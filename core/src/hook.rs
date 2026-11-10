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

#[derive(Default)]
pub struct Reported {
    pub edits: Option<Vec<(String, String, String)>>,
    pub touched: Option<Vec<String>>,
    pub tests: Vec<String>,
    pub usage: Value,
    pub checks: Vec<(String, Option<bool>)>,
    pub kinds: Vec<String>,
}

#[derive(Default)]
pub struct Started {
    pub note: Option<String>,
    pub kinds: Vec<String>,
    pub answer: Option<Value>,
    pub familiarity: Option<Value>,
}

const USAGE_FIELDS: &[&str] = &["model", "effort", "inputTokens", "outputTokens", "cacheRead", "cacheWrite", "cost", "steps", "toolOutputChars", "repeatReads", "durationMs"];
const USAGE_MAX_BYTES: u64 = 4_000_000;
const USAGE_KEEP_LINES: usize = 4_000;

fn usage_path(root: &Path) -> PathBuf {
    root.join(".stem").join("learn").join("usage.jsonl")
}

fn record_usage(root: &Path, session: &str, kinds: &Value, finished: bool, usage: &Value) {
    use std::io::Write;
    let Some(given) = usage.as_object() else { return };
    let mut row = serde_json::Map::new();
    row.insert("at".into(), json!(now()));
    row.insert("session".into(), json!(session));
    row.insert("kinds".into(), if kinds.is_array() { kinds.clone() } else { json!([]) });
    row.insert("finished".into(), json!(finished));
    for field in USAGE_FIELDS {
        if let Some(v) = given.get(*field).filter(|v| v.is_number() || v.is_string()) {
            row.insert((*field).into(), v.clone());
        }
    }
    let path = usage_path(root);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > USAGE_MAX_BYTES) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let lines: Vec<&str> = text.lines().collect();
            let _ = std::fs::write(&path, format!("{}\n", lines[lines.len().saturating_sub(USAGE_KEEP_LINES)..].join("\n")));
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", Value::Object(row));
    }
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(values[values.len() / 2])
}

pub fn usage_summary(root: &Path) -> Value {
    let rows: Vec<Value> = std::fs::read_to_string(usage_path(root)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut groups: std::collections::BTreeMap<String, Vec<&Value>> = std::collections::BTreeMap::new();
    for row in &rows {
        let kinds: Vec<&str> = row["kinds"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if kinds.is_empty() {
            groups.entry("none".into()).or_default().push(row);
        }
        for kind in kinds {
            groups.entry(kind.to_string()).or_default().push(row);
        }
    }
    let number = |rows: &[&Value], pick: &dyn Fn(&Value) -> Option<f64>| median(rows.iter().filter_map(|r| pick(r)).collect());
    let by_note: serde_json::Map<String, Value> = groups
        .iter()
        .map(|(kind, rows)| {
            let stats = json!({
                "turns": rows.len(),
                "medianCost": number(rows, &|r| r["cost"].as_f64()),
                "medianTokens": number(rows, &|r| Some(r["inputTokens"].as_f64()? + r["outputTokens"].as_f64().unwrap_or(0.0))),
                "medianSteps": number(rows, &|r| r["steps"].as_f64()),
                "medianRepeatReads": number(rows, &|r| r["repeatReads"].as_f64()),
            });
            (kind.clone(), stats)
        })
        .collect();
    json!({"turns": rows.len(), "byNote": by_note})
}

pub fn end(root: &Path, session: &str, answer: &str, finished: bool) -> Value {
    end_with(root, session, answer, finished, Reported::default())
}

pub fn end_with(root: &Path, session: &str, answer: &str, finished: bool, reported: Reported) -> Value {
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
    let (edits, touched) = match (reported.edits, reported.touched) {
        (Some(edits), Some(touched)) => (edits, touched),
        (edits, touched) => {
            let (seen_edits, seen_touched) = changes_since(root, base, &known);
            (edits.unwrap_or(seen_edits), touched.unwrap_or(seen_touched))
        }
    };
    let summary = json!({"request": prompt.chars().take(120).collect::<String>(), "edits": edits.len(), "files": touched});
    let mut kinds: Vec<Value> = p["kinds"].as_array().cloned().unwrap_or_default();
    kinds.extend(reported.kinds.iter().map(|k| json!(k)).filter(|k| !kinds.contains(k)).collect::<Vec<_>>());
    record_usage(root, session, &json!(kinds), finished, &reported.usage);
    let mut learn = Learn::open(root);
    learn.session = Some(session.to_string());
    learn.ran_tests(&reported.tests);
    learn.checks_run = reported.checks;
    learn.external(prompt, answer, edits, touched, finished, reported.usage["steps"].as_u64().unwrap_or(0) as usize);
    summary
}

pub fn begin(root: &Path, session: &str, prompt: &str) -> Option<String> {
    begin_with(root, session, prompt, None).note
}

pub fn begin_with(root: &Path, session: &str, prompt: &str, decide: Option<crate::learn::Decider>) -> Started {
    if let Some(p) = read_pending(root, session) {
        let same = p["prompt"].as_str() == Some(prompt) && now().saturating_sub(p["at"].as_u64().unwrap_or(0)) < REUSE_SECS;
        if same {
            return Started::default();
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
    learn.decide = decide;
    learn.session = Some(session.to_string());
    let note = learn.brief(prompt);
    let kinds = learn.note_kinds();
    let pending = json!({"prompt": prompt, "base": snapshot(root), "untracked": untracked(root), "at": now(), "note": note, "kinds": kinds});
    let path = pending_path(root, session);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, pending.to_string());
    let answer = learn.offered.as_ref().map(|(a, sure)| json!({"text": a.answer, "request": a.request, "files": a.files.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), "sure": sure}));
    let familiarity = learn.familiarity.map(|score| json!({"score": (score * 100.0).round() / 100.0, "priorSteps": learn.prior_steps()}));
    Started { note, kinds, answer, familiarity }
}

fn log(root: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(root.join(".stem").join("hooks.log")) {
        let _ = writeln!(f, "{} {line}", now());
    }
}

const NOTE_CHARS: usize = 16_000;

pub fn cap(note: &str) -> String {
    if note.len() <= NOTE_CHARS {
        return note.to_string();
    }
    let mut end = NOTE_CHARS;
    while !note.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[stem: note cut at {NOTE_CHARS} characters]", &note[..end])
}

pub fn run(event: &str) -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let input: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let cwd = input["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let root = project_root(&cwd);
    let base = input["session_id"].as_str().or(input["sessionID"].as_str()).filter(|s| !s.trim().is_empty()).unwrap_or("default");
    let session = match input["tab_id"].as_str().filter(|t| !t.trim().is_empty()) {
        Some(tab) => format!("{base}:{tab}"),
        None => base.to_string(),
    };
    match event {
        "prompt" | "UserPromptSubmit" => {
            let prompt = input["prompt"].as_str().unwrap_or("").trim();
            if prompt.is_empty() {
                return 0;
            }
            if let Some(note) = begin(&root, &session, prompt).map(|n| cap(&n)) {
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
