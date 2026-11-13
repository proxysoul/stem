use crate::catalog::Resolved;
use crate::llm::{Block, Msg, Reply, Stop, Tool, Usage};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub struct Graft {
    pub id: String,
    pub path: PathBuf,
    pub entry: Value,
}

pub static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn dirs() -> Vec<PathBuf> {
    vec![crate::env::stem_home().join("grafts")]
}

pub fn load() -> (Vec<Graft>, Vec<String>) {
    let mut grafts: Vec<Graft> = Vec::new();
    let mut problems = Vec::new();
    for dir in dirs() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = read.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        for path in files {
            let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string())).and_then(|v| normalize(&id, &path, v)) {
                Ok(entry) => {
                    grafts.retain(|g| g.id != id);
                    grafts.push(Graft { id, path, entry });
                }
                Err(e) => problems.push(format!("{}: {e}", path.display())),
            }
        }
    }
    (grafts, problems)
}

fn normalize(id: &str, path: &Path, mut v: Value) -> Result<Value, String> {
    let protocol = v["protocol"].as_str().unwrap_or("chat").to_string();
    if !matches!(protocol.as_str(), "anthropic" | "chat" | "responses" | "command") {
        return Err(format!("protocol is one of anthropic, chat, responses, command; got {protocol}"));
    }
    if protocol == "command" && command_of(&v, path).is_none() {
        return Err("a command graft needs \"command\": [\"program\", \"arg\", ...]".into());
    }
    if protocol != "command" && !v["api"].is_string() {
        return Err(format!("a {protocol} graft needs \"api\": \"https://.../v1\""));
    }
    let models: serde_json::Map<String, Value> = match &v["models"] {
        Value::Array(ids) => ids.iter().filter_map(|m| m.as_str()).map(|m| (m.to_string(), json!({"id": m, "tool_call": true}))).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(k, m)| {
                let mut m = m.clone();
                if m["id"].is_null() {
                    m["id"] = json!(k);
                }
                if m["tool_call"].is_null() {
                    m["tool_call"] = json!(true);
                }
                (k.clone(), m)
            })
            .collect(),
        _ => return Err("\"models\" lists the model ids, as an array or an object".into()),
    };
    if models.is_empty() {
        return Err("\"models\" is empty".into());
    }
    v["models"] = Value::Object(models);
    if v["name"].is_null() {
        v["name"] = json!(id);
    }
    if v["env"].is_null() {
        v["env"] = json!([]);
    }
    v["protocol"] = json!(protocol);
    v["graft"] = json!(path.display().to_string());
    Ok(v)
}

fn command_of(v: &Value, path: &Path) -> Option<Vec<String>> {
    let parts: Vec<String> = match &v["command"] {
        Value::String(s) => s.split_whitespace().map(str::to_string).collect(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => return None,
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let parts: Vec<String> = parts
        .into_iter()
        .map(|p| if p.starts_with("./") || p.starts_with("../") { dir.join(&p).display().to_string() } else { p })
        .collect();
    (!parts.is_empty()).then_some(parts)
}

pub fn complete(resolved: &Resolved, effort: Option<&str>, max_output: u64, system: &str, msgs: &[Msg], tools: &[Tool], cancel: &Arc<AtomicBool>) -> Result<Reply, String> {
    let path = PathBuf::from(&resolved.base_url);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let parts = command_of(&v, &path).ok_or_else(|| format!("{} has no command", path.display()))?;
    let request = json!({
        "model": resolved.model.id,
        "system": system,
        "messages": crate::llm::anthropic_messages(msgs),
        "tools": tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema})).collect::<Vec<_>>(),
        "max_tokens": max_output,
        "effort": effort,
    });
    let mut child = Command::new(&parts[0])
        .args(&parts[1..])
        .env("STEM_GRAFT", &resolved.provider_id)
        .env("STEM_GRAFT_DIR", path.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("graft {} could not start {}: {e}", resolved.provider_id, parts[0]))?;
    let body = request.to_string();
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(body.as_bytes());
    });
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    let out_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let _ = writer.join();
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    let last = out.lines().rev().find(|l| l.trim_start().starts_with('{'));
    let reply: Value = match last.and_then(|l| serde_json::from_str(l).ok()) {
        Some(v) => v,
        None => {
            let tail: String = err.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
            return Err(format!("graft {} ({}) gave no JSON reply: {}", resolved.provider_id, status, tail.trim()));
        }
    };
    if let Some(e) = reply.get("error").filter(|e| !e.is_null()) {
        return Err(format!("graft {}: {}", resolved.provider_id, e.as_str().map(str::to_string).unwrap_or_else(|| e["message"].as_str().unwrap_or("error").to_string())));
    }
    let blocks = crate::llm::anthropic_content(&reply["content"]);
    let u = &reply["usage"];
    let usage = Usage {
        input: u["input_tokens"].as_u64().unwrap_or(0),
        output: u["output_tokens"].as_u64().unwrap_or(0),
        cache_read: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
        cache_write: u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
    };
    let has_tools = blocks.iter().any(|b| matches!(b, Block::ToolUse { .. }));
    let stop = match reply["stop_reason"].as_str() {
        Some("max_tokens") => Stop::MaxTokens,
        _ if has_tools => Stop::ToolUse,
        _ => Stop::End,
    };
    Ok(Reply { blocks, stop, usage })
}
