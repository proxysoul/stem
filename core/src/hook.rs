use crate::learn::Learn;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const REUSE_SECS: u64 = 10;
const FIX_CHARS: usize = 400;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn project_root(cwd: &Path) -> PathBuf {
    git(cwd, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim())).unwrap_or_else(|| cwd.to_path_buf())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn pending_path(root: &Path, session: &str) -> PathBuf {
    let name: String = session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(80).collect();
    root.join(".stem").join("learn").join("pending").join(format!("{}.json", if name.is_empty() { "default".into() } else { name }))
}

fn untracked(root: &Path) -> Vec<String> {
    git(root, &["ls-files", "--others", "--exclude-standard"]).map(|s| s.lines().filter(|l| !l.starts_with(".stem/")).map(str::to_string).collect()).unwrap_or_default()
}

fn snapshot(root: &Path) -> Option<String> {
    let stash = git(root, &["stash", "create"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    stash.or_else(|| git(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()))
}

pub fn changes_since(root: &Path, base: &str, before_untracked: &[String]) -> (Vec<(String, String, String)>, Vec<String>) {
    let diff = git(root, &["diff", "--no-color", "--no-ext-diff", "-U2", base, "--"]).unwrap_or_default();
    let mut edits = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut file = String::new();
    let (mut before, mut after, mut open) = (String::new(), String::new(), false);
    let flush = |file: &str, before: &mut String, after: &mut String, open: &mut bool, edits: &mut Vec<(String, String, String)>| {
        if *open && !file.is_empty() && before.len() <= FIX_CHARS && after.len() <= FIX_CHARS && before.trim() != after.trim() {
            edits.push((file.to_string(), std::mem::take(before), std::mem::take(after)));
        }
        before.clear();
        after.clear();
        *open = false;
    };
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            flush(&file, &mut before, &mut after, &mut open, &mut edits);
            file = line.rsplit(" b/").next().unwrap_or("").to_string();
            if !file.starts_with(".stem/") && !touched.contains(&file) {
                touched.push(file.clone());
            }
        } else if line.starts_with("@@") {
            flush(&file, &mut before, &mut after, &mut open, &mut edits);
            open = !file.starts_with(".stem/");
        } else if open {
            if let Some(rest) = line.strip_prefix(' ') {
                before.push_str(rest);
                before.push('\n');
                after.push_str(rest);
                after.push('\n');
            } else if let Some(rest) = line.strip_prefix('-') {
                before.push_str(rest);
                before.push('\n');
            } else if let Some(rest) = line.strip_prefix('+') {
                after.push_str(rest);
                after.push('\n');
            }
        }
    }
    flush(&file, &mut before, &mut after, &mut open, &mut edits);
    for f in untracked(root) {
        if !before_untracked.contains(&f) && !touched.contains(&f) {
            touched.push(f);
        }
    }
    (edits, touched)
}

fn read_pending(root: &Path, session: &str) -> Option<Value> {
    std::fs::read_to_string(pending_path(root, session)).ok().and_then(|t| serde_json::from_str(&t).ok())
}

pub fn end(root: &Path, session: &str, answer: &str, finished: bool) -> Value {
    let candidates = if session == "default" { vec![pending_path(root, session)] } else { vec![pending_path(root, session), pending_path(root, "default")] };
    let taken = candidates.iter().find_map(|path| {
        let claim = path.with_extension(format!("taken-{}", std::process::id()));
        std::fs::rename(path, &claim).ok().map(|_| claim)
    });
    let Some(claim) = taken else {
        return json!({"skipped": "no request started in this session"});
    };
    let p = std::fs::read_to_string(&claim).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
    let _ = std::fs::remove_file(&claim);
    crate::growth::Growth::open(root).new_session();
    let prompt = p["prompt"].as_str().unwrap_or("");
    let base = p["base"].as_str().unwrap_or("HEAD");
    let known: Vec<String> = p["untracked"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let (edits, touched) = changes_since(root, base, &known);
    let summary = json!({"request": prompt.chars().take(120).collect::<String>(), "edits": edits.len(), "files": touched});
    let mut learn = Learn::open(root);
    learn.external(prompt, answer, edits, touched, finished);
    summary
}

pub fn begin(root: &Path, session: &str, prompt: &str) -> Option<String> {
    if let Some(p) = read_pending(root, session) {
        let same = p["prompt"].as_str() == Some(prompt) && now().saturating_sub(p["at"].as_u64().unwrap_or(0)) < REUSE_SECS;
        if same {
            return None;
        }
        end(root, session, "", false);
    }
    if let Ok(entries) = std::fs::read_dir(root.join(".stem").join("learn").join("pending")) {
        for entry in entries.flatten() {
            let old = entry.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 24 * 3600);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    crate::events::ensure_ignored(&root.join(".stem"));
    let mut learn = Learn::open(root);
    learn.host = true;
    let note = learn.brief(prompt);
    let pending = json!({"prompt": prompt, "base": snapshot(root), "untracked": untracked(root), "at": now(), "note": note});
    let path = pending_path(root, session);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, pending.to_string());
    note
}

fn log(root: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(root.join(".stem").join("hooks.log")) {
        let _ = writeln!(f, "{} {line}", now());
    }
}

pub fn run(event: &str) -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let input: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let cwd = input["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let root = project_root(&cwd);
    let session = input["session_id"].as_str().or(input["sessionID"].as_str()).filter(|s| !s.trim().is_empty()).unwrap_or("default").to_string();
    match event {
        "prompt" | "UserPromptSubmit" => {
            let prompt = input["prompt"].as_str().unwrap_or("").trim();
            if prompt.is_empty() {
                return 0;
            }
            if let Some(note) = begin(&root, &session, prompt) {
                println!("{}", json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": note}}));
            }
            log(&root, &format!("prompt {session}"));
        }
        "stop" | "Stop" => {
            let answer = input["last_assistant_message"].as_str().unwrap_or("");
            let summary = end(&root, &session, answer, true);
            log(&root, &format!("stop {session} {summary}"));
        }
        other => {
            eprintln!("stem hook: unknown event {other}; use prompt or stop");
            return 0;
        }
    }
    0
}
