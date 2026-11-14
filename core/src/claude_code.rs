use crate::catalog::Resolved;
use crate::llm::{Block, Msg, Reply, Role, Stop, Usage};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PROVIDER: &str = "claude-code";
const KEPT_SESSIONS: usize = 3;
const HIDDEN_ENV: [&str; 4] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_OAUTH_TOKEN"];

pub fn binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("STEM_CLAUDE").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let home = dirs::home_dir().unwrap_or_default();
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).map(|d| d.join("claude")).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain([home.join(".local/bin/claude"), home.join(".claude/local/claude")])
        .find(|p| p.is_file())
}

pub fn login() -> Result<(), String> {
    let bin = binary().ok_or("the claude command is not installed. Install Claude Code first: https://claude.com/claude-code")?;
    let status = Command::new(&bin).args(["auth", "login", "--claudeai"]).status().map_err(|e| format!("starting claude: {e}"))?;
    if !status.success() {
        return Err("Claude sign-in did not finish".into());
    }
    let out = Command::new(&bin).args(["auth", "status"]).output().map_err(|e| format!("claude auth status: {e}"))?;
    let info: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    if info["loggedIn"].as_bool() == Some(true) { Ok(()) } else { Err("Claude Code still reports no sign-in".into()) }
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    system: String,
    effort: Option<String>,
    synced: usize,
    fingerprint: u64,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
pub struct Link {
    sessions: RefCell<Vec<Session>>,
}

fn fingerprint(msgs: &[Msg]) -> u64 {
    let mut h = DefaultHasher::new();
    format!("{msgs:?}").hash(&mut h);
    h.finish()
}

fn block_text(b: &Block) -> Option<String> {
    match b {
        Block::Text(t) => Some(t.clone()),
        Block::ToolUse { input, .. } => Some(format!("```js\n{}\n```", input["code"].as_str().unwrap_or(""))),
        Block::ToolResult { content, is_error, .. } => Some(format!("{}{content}", if *is_error { "Script failed:\n" } else { "Script result:\n" })),
        Block::Thinking { .. } | Block::RedactedThinking(_) | Block::Reasoning(_) => None,
    }
}

fn transcript(msgs: &[Msg]) -> String {
    msgs.iter()
        .map(|m| {
            let who = if m.role == Role::User { "User" } else { "Assistant" };
            format!("{who}:\n{}", m.blocks.iter().filter_map(block_text).collect::<Vec<_>>().join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn user_line(texts: Vec<String>) -> String {
    let content: Vec<Value> = texts.into_iter().filter(|t| !t.is_empty()).map(|t| json!({"type": "text", "text": t})).collect();
    json!({"type": "user", "message": {"role": "user", "content": content}, "parent_tool_use_id": null}).to_string()
}

fn spawn(resolved: &Resolved, system: &str, effort: Option<&str>, max_output: u64) -> Result<Session, String> {
    let bin = binary().ok_or("the claude command is not installed. Install Claude Code and run `claude` once to sign in.")?;
    let home = crate::env::stem_home().join(PROVIDER);
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    let mut cmd = crate::sys::command(bin);
    cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .args(["--model", &resolved.model.id])
        .args(["--tools", "", "--strict-mcp-config", "--setting-sources", "", "--disable-slash-commands", "--no-session-persistence"])
        .args(["--permission-prompts", "none", "--system-prompt", system]);
    if let Some(e) = effort {
        cmd.args(["--effort", e]);
    }
    for name in HIDDEN_ENV {
        cmd.env_remove(name);
    }
    cmd.env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", max_output.to_string())
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS", "1")
        .env("CLAUDE_CODE_DISABLE_ATTACHMENTS", "1")
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("starting claude: {e}"))?;
    let stdin = child.stdin.take().ok_or("claude has no stdin")?;
    let stdout = child.stdout.take().ok_or("claude has no stdout")?;
    let err_pipe = child.stderr.take().ok_or("claude has no stderr")?;
    let (tx, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let stderr = Arc::new(Mutex::new(String::new()));
    let sink = stderr.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(err_pipe).lines().map_while(Result::ok) {
            if let Ok(mut s) = sink.lock() {
                s.push_str(&line);
                s.push('\n');
            }
        }
    });
    Ok(Session { child, stdin, lines, stderr, system: system.to_string(), effort: effort.map(str::to_string), synced: 0, fingerprint: fingerprint(&[]) })
}

fn stop_of(reason: &str) -> Stop {
    match reason {
        "end_turn" | "stop_sequence" | "" => Stop::End,
        "max_tokens" => Stop::MaxTokens,
        other => Stop::Other(other.to_string()),
    }
}

fn usage_of(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Usage { input: n("input_tokens"), output: n("output_tokens"), cache_read: n("cache_read_input_tokens"), cache_write: n("cache_creation_input_tokens") }
}

fn read_reply(s: &mut Session, cancel: &Arc<AtomicBool>) -> Result<Reply, String> {
    let mut blocks = Vec::new();
    let mut usage = Usage::default();
    let mut stop = Stop::End;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let line = match s.lines.recv_timeout(Duration::from_millis(100)) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                let err = s.stderr.lock().map(|e| e.trim().to_string()).unwrap_or_default();
                return Err(format!("claude stopped without an answer{}", if err.is_empty() { String::new() } else { format!(": {err}") }));
            }
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
        match event["type"].as_str() {
            Some("assistant") if event["parent_tool_use_id"].is_null() => {
                let m = &event["message"];
                for b in m["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("text") => blocks.push(Block::Text(b["text"].as_str().unwrap_or("").to_string())),
                        Some("thinking") => blocks.push(Block::Thinking { text: b["thinking"].as_str().unwrap_or("").to_string(), signature: b["signature"].as_str().unwrap_or("").to_string() }),
                        _ => {}
                    }
                }
                usage = usage_of(&m["usage"]);
                if let Some(r) = m["stop_reason"].as_str() {
                    stop = stop_of(r);
                }
            }
            Some("result") => {
                if event["is_error"].as_bool().unwrap_or(false) {
                    let status = event["api_error_status"].as_u64().map(|c| format!(" (HTTP {c})")).unwrap_or_default();
                    return Err(format!("Claude Code: {}{status}", event["result"].as_str().unwrap_or("unknown error")));
                }
                if let Some(r) = event["stop_reason"].as_str() {
                    stop = stop_of(r);
                }
                return Ok(Reply { blocks, stop, usage });
            }
            _ => {}
        }
    }
}

impl Link {
    pub fn complete(&self, resolved: &Resolved, effort: Option<&str>, max_output: u64, system: &str, msgs: &[Msg], cancel: &Arc<AtomicBool>) -> Result<Reply, String> {
        let mut sessions = self.sessions.borrow_mut();
        let resumable = sessions.iter().position(|s| {
            s.system == system
                && s.effort.as_deref() == effort
                && s.synced > 0
                && s.synced < msgs.len()
                && msgs[s.synced..].iter().all(|m| m.role == Role::User)
                && fingerprint(&msgs[..s.synced]) == s.fingerprint
        });
        let (mut session, line) = match resumable {
            Some(i) => {
                let s = sessions.remove(i);
                let texts = msgs[s.synced..].iter().flat_map(|m| m.blocks.iter().filter_map(block_text)).collect();
                (s, user_line(texts))
            }
            None => {
                let tail = msgs.iter().rposition(|m| m.role != Role::User).map_or(0, |i| i + 1);
                let mut texts = Vec::new();
                if tail > 0 {
                    texts.push(format!("Earlier in this conversation:\n<transcript>\n{}\n</transcript>", transcript(&msgs[..tail])));
                }
                texts.extend(msgs[tail..].iter().flat_map(|m| m.blocks.iter().filter_map(block_text)));
                (spawn(resolved, system, effort, max_output)?, user_line(texts))
            }
        };
        writeln!(session.stdin, "{line}").and_then(|_| session.stdin.flush()).map_err(|e| format!("writing to claude: {e}"))?;
        let reply = read_reply(&mut session, cancel)?;
        let mut seen = msgs.to_vec();
        seen.push(Msg { role: Role::Assistant, blocks: reply.blocks.clone() });
        session.synced = seen.len();
        session.fingerprint = fingerprint(&seen);
        sessions.push(session);
        if sessions.len() > KEPT_SESSIONS {
            sessions.remove(0);
        }
        Ok(reply)
    }
}
