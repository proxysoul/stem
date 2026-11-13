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
    crate::env::project_home(root).join("learn").join("pending").join(format!("{}.json", if name.is_empty() { "default".into() } else { name }))
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
    pub checks_before: Vec<String>,
    pub kinds: Vec<String>,
    pub withheld: Vec<String>,
    pub acted: bool,
    pub rule_hits: usize,
    pub offered_checks: Vec<String>,
    pub noted_regress: Vec<(String, String, String)>,
    pub shown: Option<bool>,
    pub origin: Option<String>,
}

#[derive(Default)]
pub struct Started {
    pub note: Option<String>,
    pub kinds: Vec<String>,
    pub withheld: Vec<String>,
    pub answer: Option<Value>,
    pub familiarity: Option<Value>,
    pub noted_regress: Vec<(String, String, String)>,
}

#[derive(Default)]
pub struct Begin<'a> {
    pub decide: Option<crate::learn::Decider>,
    pub without_snapshot: bool,
    pub task: Option<&'a str>,
    pub show_all: bool,
}

pub fn shows_everything() -> bool {
    std::env::var("STEM_SHOW").is_ok_and(|v| v == "all")
}

const ORIGIN_USER: &str = "user";
const ANSWER_FOLLOWED_STEPS: u64 = 2;

fn working_tree(root: &Path, skip: bool) -> (Option<String>, Vec<String>) {
    if skip {
        return (None, Vec::new());
    }
    std::thread::scope(|scope| {
        let base = scope.spawn(|| snapshot(root));
        let others = untracked(root);
        (base.join().unwrap_or(None), others)
    })
}

fn write_pending(root: &Path, session: &str, pending: &Value) {
    let path = pending_path(root, session);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, pending.to_string());
}

fn fix_triples(list: &Value) -> Vec<(String, String, String)> {
    list.as_array().into_iter().flatten().filter_map(|f| Some((f[0].as_str()?.to_string(), f[1].as_str()?.to_string(), f[2].as_str()?.to_string()))).collect()
}

pub fn usage_only(root: &Path, session: &str, origin: &str, finished: bool, shown: bool, usage: &Value) -> bool {
    let mut learn = Learn::open(root);
    learn.session = Some(session.to_string());
    crate::usage::record(root, &crate::usage::Turn { session, task: learn.task_id(), origin, kinds: &[], withheld: &[], followed: &[], shown, finished, usage, rule_hits: 0 })
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
    let known: Vec<String> = p["untracked"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let (edits, touched) = match (reported.edits, reported.touched, p["base"].as_str()) {
        (Some(edits), Some(touched), _) => (edits, touched),
        (edits, touched, Some(base)) => {
            let (seen_edits, seen_touched) = changes_since(root, base, &known);
            (edits.unwrap_or(seen_edits), touched.unwrap_or(seen_touched))
        }
        (edits, touched, None) => (edits.unwrap_or_default(), touched.unwrap_or_default()),
    };
    let mut kinds: Vec<String> = p["kinds"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
    for kind in &reported.kinds {
        if !kinds.contains(kind) {
            kinds.push(kind.clone());
        }
    }
    let mut withheld: Vec<String> = p["withheld"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
    for kind in &reported.withheld {
        if !withheld.contains(kind) && !kinds.contains(kind) {
            withheld.push(kind.clone());
        }
    }
    let has = |kind: &str| kinds.iter().chain(&withheld).any(|k| k == kind);
    let mut regress = fix_triples(&p["regress"]);
    regress.extend(reported.noted_regress.iter().cloned());
    let reapplied = !regress.is_empty() && regress.iter().all(|(file, before, after)| std::fs::read_to_string(root.join(file)).is_ok_and(|now| crate::learn::applied(&now, before, after)));
    let wrote_familiar = p["familiar"].as_array().into_iter().flatten().filter_map(Value::as_str).any(|f| touched.iter().any(|t| t == f));
    let ran_usual = reported.checks.iter().any(|(cmd, _)| crate::habits::checks_in(cmd, &touched).iter().any(|c| reported.offered_checks.contains(c)));
    let cites_same = p["answered"].as_array().into_iter().flatten().filter_map(Value::as_str).any(|f| answer.contains(f));
    let answered_directly = finished && touched.is_empty() && (cites_same || reported.usage["steps"].as_u64().is_some_and(|s| s <= ANSWER_FOLLOWED_STEPS));
    let reread_nothing = finished && reported.usage["repeatReads"].as_u64() == Some(0);
    let resumed: Vec<&str> = p["resumed"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let went_on = if resumed.is_empty() { finished } else { resumed.iter().any(|f| touched.iter().any(|t| t == f)) };
    let outcomes = [("familiar", wrote_familiar), ("regress", reapplied), ("habits", ran_usual), ("answer", answered_directly), ("resume", went_on), ("trail", reread_nothing)];
    let followed: Vec<String> = outcomes.iter().filter(|(kind, did)| *did && has(kind)).map(|(kind, _)| kind.to_string()).collect();
    let mut summary = json!({"request": prompt.chars().take(120).collect::<String>(), "edits": edits.len(), "files": touched, "kinds": kinds, "withheld": withheld, "followed": followed});
    let mut learn = Learn::open(root);
    learn.session = Some(session.to_string());
    learn.continues = p["continues"].as_bool().unwrap_or(false);
    learn.task_hint = p["task"].as_str().map(str::to_string);
    learn.ran_tests(&reported.tests);
    learn.checks_run = reported.checks;
    learn.checks_before = reported.checks_before;
    learn.acted = reported.acted || reported.origin.as_deref().is_some_and(|o| o != ORIGIN_USER);
    learn.external(prompt, answer, edits, touched, finished, reported.usage["steps"].as_u64().unwrap_or(0) as usize);
    let origin = reported.origin.as_deref().unwrap_or(ORIGIN_USER);
    let turn = crate::usage::Turn { session, task: learn.task_id(), origin, kinds: &kinds, withheld: &withheld, followed: &followed, shown: reported.shown.unwrap_or(true), finished, usage: &reported.usage, rule_hits: reported.rule_hits };
    summary["usageStored"] = json!(crate::usage::record(root, &turn));
    summary
}

pub fn begin(root: &Path, session: &str, prompt: &str) -> Option<String> {
    begin_with(root, session, prompt, Begin::default()).note
}

pub fn continue_task(root: &Path, session: &str, without_snapshot: bool, hint: Option<&str>) -> bool {
    let mut learn = Learn::open(root);
    learn.session = Some(session.to_string());
    let known = learn.current_task();
    let Some(task) = known.clone().or_else(|| hint.map(str::to_string).filter(|h| !h.trim().is_empty())) else { return false };
    if read_pending(root, session).is_some() {
        end(root, session, "", false);
    }
    let (base, others) = working_tree(root, without_snapshot);
    write_pending(root, session, &json!({"prompt": task, "continues": known.is_some(), "task": hint, "base": base, "untracked": others, "at": now(), "kinds": []}));
    true
}

pub fn begin_with(root: &Path, session: &str, prompt: &str, options: Begin) -> Started {
    if let Some(p) = read_pending(root, session) {
        let same = p["prompt"].as_str() == Some(prompt) && now().saturating_sub(p["at"].as_u64().unwrap_or(0)) < REUSE_SECS;
        if same {
            return Started::default();
        }
        end(root, session, "", false);
    }
    if let Ok(entries) = std::fs::read_dir(crate::env::project_home(root).join("learn").join("pending")) {
        for entry in entries.flatten() {
            let old = entry.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 24 * 3600);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let mut learn = Learn::open(root);
    learn.host = true;
    learn.decide = options.decide;
    learn.session = Some(session.to_string());
    learn.task_hint = options.task.map(str::to_string);
    let records = crate::usage::records(root);
    let show_all = options.show_all || shows_everything();
    let (shown, held): (Vec<_>, Vec<_>) = learn.brief(prompt).into_iter().partition(|(kind, _)| show_all || crate::usage::shows(&records, kind));
    let note = (!shown.is_empty()).then(|| shown.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n"));
    let kinds: Vec<String> = shown.iter().map(|(kind, _)| kind.to_string()).collect();
    let withheld: Vec<String> = held.iter().map(|(kind, _)| kind.to_string()).collect();
    let answer_shown = kinds.iter().any(|k| k == "answer");
    let (base, others) = working_tree(root, options.without_snapshot);
    write_pending(
        root,
        session,
        &json!({"prompt": prompt, "base": base, "untracked": others, "at": now(), "note": note, "kinds": kinds, "withheld": withheld, "familiar": learn.noted_familiar, "resumed": learn.noted_resume, "regress": learn.noted_regress, "answered": learn.offered.as_ref().map(|(a, _)| a.files.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>()), "task": options.task}),
    );
    let answer = learn.offered.as_ref().filter(|_| answer_shown).map(|(a, sure)| json!({"text": a.answer, "request": a.request, "files": a.files.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), "sure": sure}));
    let familiarity = learn.familiarity.map(|score| json!({"score": (score * 100.0).round() / 100.0, "priorSteps": learn.prior_steps()}));
    Started { note, kinds, withheld, answer, familiarity, noted_regress: learn.noted_regress.clone() }
}

fn log(root: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::env::project_home(root).join("hooks.log")) {
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
