use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

const KNOWN_AFTER: u32 = 2;
const MAX_KNOWN_EXTRA: usize = 0;
const LESSON_CHARS: usize = 600;
const MAX_LESSONS: usize = 40;
const LESSON_MIN_WORDS: usize = 3;
const RECENT: usize = 20;
const ANSWER_CHARS: usize = 800;
const MAX_ANSWERS: usize = 60;
const ANSWER_CANDIDATES: usize = 3;
const ANSWER_MIN_SHARE: f64 = 0.22;
pub const ANSWER_SURE_SHARE: f64 = 0.4;
const QUESTION_WORDS: &[&str] = &["question", "answer", "function", "name", "path", "change", "changes", "which", "where", "what", "code", "does", "done", "used", "using", "work", "works"];
const KNOWN_TAIL: usize = 12;
const RESUME_RESULT: usize = 800;
const RESUME_MIN_WORDS: usize = 3;
const FIX_CHARS: usize = 400;
const REGRESS_MIN_WORDS: usize = 2;
const RESUME_MAX_SECS: u64 = 24 * 3600;
const FIXES_PER_TASK: usize = 40;
const SUBSTANTIVE_WORDS: usize = 2;
const TRAIL_FILES_MAX: usize = 10;
const HUB_SHARE: f64 = 0.25;
const HUB_MIN_WORDS: usize = 40;
const HUB_MIN_FILES: usize = 30;
const FOLLOW_UP: &[&str] = &["continue", "finish", "left", "rest", "next", "alright", "okay", "update", "keep", "going", "more", "whats", "what", "carry", "proceed", "anything", "else", "done", "sure", "please", "thanks"];

fn substantive(prompt: &str) -> bool {
    words(prompt).iter().filter(|w| !FOLLOW_UP.contains(&w.as_str())).count() >= SUBSTANTIVE_WORDS
}

fn trail_target(path: &str) -> bool {
    let lower = path.to_lowercase();
    let name = Path::new(&lower).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let generated = lower.contains(".generated.") || ["cargo.lock", "bun.lock", "bun.lockb", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "go.sum", "poetry.lock"].contains(&name.as_str());
    let locale = lower.ends_with(".json") && lower.split('/').any(|p| p == "locales" || p == "i18n" || p == "lang");
    let fixture = lower.split('/').any(|p| ["fixtures", "__fixtures__", "__snapshots__", "testdata"].contains(&p));
    !generated && !locale && !fixture
}

fn learnable(path: &str) -> bool {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let secret = [".env", ".pem", ".key", ".p12", ".pfx", "id_rsa", "id_ed25519", ".npmrc", ".netrc"].iter().any(|s| name == s.trim_start_matches('.') || name.ends_with(s) || name.starts_with(".env"));
    let scratch = ["tmp-", "temp-", "scratch-"].iter().any(|p| name.starts_with(p));
    let hidden = path.split('/').any(|part| part.starts_with('.') && part.len() > 1);
    !secret && !scratch && !hidden
}
const TRAIL_HALF_LIFE: f64 = 60.0;
const LESSON_MAX_SESSIONS: u64 = 100;

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn undone_in_a_commit(root: &Path, file: &str, after: &str) -> bool {
    std::process::Command::new("git")
        .args(["log", "--format=%H", "-S", after, "--", file])
        .current_dir(root)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count() >= 2)
        .unwrap_or(false)
}
const MAX_FIXES: usize = 200;
const SAMPLE_CHECKED: usize = 4;
const SAMPLE_TOTAL: usize = 10;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Failure {
    pub test: String,
    pub file: String,
    pub seen: u32,
    pub clean: bool,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Lesson {
    pub text: String,
    pub files: Vec<String>,
    pub words: Vec<String>,
    #[serde(default)]
    pub hashes: Vec<(String, String)>,
    #[serde(default)]
    pub session: u64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Fix {
    pub file: String,
    pub before: String,
    pub after: String,
    pub request: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Resume {
    pub request: String,
    pub steps: usize,
    pub edits: Vec<(String, String, String)>,
    pub scratch: Vec<String>,
    pub last_result: String,
    #[serde(default)]
    pub at: u64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Answer {
    pub words: Vec<String>,
    pub request: String,
    pub answer: String,
    pub files: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    #[serde(default)]
    test_command: Option<String>,
    #[serde(default)]
    failures: BTreeMap<String, Failure>,
    #[serde(default)]
    sessions: u64,
    #[serde(default)]
    recent: Vec<String>,
    #[serde(default)]
    trails: BTreeMap<String, BTreeMap<String, u32>>,
    #[serde(default)]
    tested: BTreeMap<String, u32>,
    #[serde(default)]
    lessons: Vec<Lesson>,
    #[serde(default)]
    fixes: Vec<Fix>,
    #[serde(default)]
    resume: Option<Resume>,
    #[serde(default)]
    answers: Vec<Answer>,
    #[serde(default)]
    last_changed: Vec<String>,
    #[serde(default)]
    last_prompt: String,
    #[serde(default)]
    changed_at: BTreeMap<String, u64>,
    #[serde(default)]
    task: String,
}

pub struct Warm {
    pub note: String,
    pub files: Vec<String>,
    pub score: f64,
}

pub struct Learn {
    dir: PathBuf,
    root: PathBuf,
    pub enabled: bool,
    state: State,
    touched: HashSet<String>,
    tests_run: Vec<String>,
    edits: Vec<(String, String, String)>,
    scratch: Vec<String>,
    pub last_result: String,
    pub events: Vec<(String, Value)>,
    pub preview: bool,
    pub host: bool,
    pub decide: Option<Decider>,
}

pub type Decider = Box<dyn FnMut(&Value) -> Option<Value>>;
pub const DECIDE_MIN: f64 = 0.6;
const DECIDE_FIX_OPTIONS: usize = 8;

struct StateLock(std::fs::File);

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn is_test_file(p: &str) -> bool {
    p.contains(".test.") || p.contains(".spec.") || p.contains("/__tests__/") || p.contains("_test.") || p.starts_with("tests/") || p.contains("/tests/")
}

pub fn failing_tests(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let plain = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap().replace_all(output, "").into_owned();
    for line in plain.lines() {
        let l = line.trim();
        let name = if let Some(rest) = l.strip_prefix("(fail) ") {
            Some(rest.split(" [").next().unwrap_or(rest).trim().to_string())
        } else if let Some(rest) = l.strip_prefix("FAIL ") {
            rest.split_once(" > ").map(|(_, n)| n.trim().to_string())
        } else if let Some(rest) = l.strip_prefix("× ").or_else(|| l.strip_prefix("✕ ")) {
            Some(rest.trim_end_matches(|c: char| c.is_ascii_digit() || c == 'm' || c == 's' || c == ' ').trim().to_string())
        } else if let Some(rest) = l.strip_prefix("--- FAIL: ") {
            Some(rest.split(' ').next().unwrap_or(rest).to_string())
        } else if l.starts_with("FAILED ") {
            Some(l[7..].split(" - ").next().unwrap_or("").trim().to_string())
        } else {
            None
        };
        if let Some(n) = name.filter(|n| !n.is_empty() && n.len() < 300) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

pub fn on(feature: &str) -> bool {
    !std::env::var("STEM_LEARN_OFF").is_ok_and(|v| v.split(',').any(|f| f.trim() == feature))
}

const STOP: &[&str] = &["this", "that", "with", "from", "when", "then", "should", "instead", "into", "only", "like", "gives", "give", "does", "doesn", "returns", "return", "value", "values", "request", "header", "string", "first", "last", "there", "their", "them", "have", "will", "would", "could", "because", "after", "before", "fix", "root", "cause", "source", "test", "tests", "files", "file", "edit", "report"];

fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<char> = raw.chars().collect();
        let mut parts: Vec<String> = Vec::new();
        let mut cur = String::new();
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() && !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            cur.push(*c);
        }
        parts.push(cur);
        for p in parts {
            let w = singular(&p.to_lowercase());
            if w.chars().count() >= 4 && w.chars().any(|c| c.is_alphabetic()) && !STOP.contains(&w.as_str()) && !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out
}

#[derive(PartialEq, Clone, Copy)]
enum EditState {
    Present,
    Undone,
    Unclear,
}

fn edit_state(now: &str, old: &str, new: &str) -> EditState {
    let has_new = !new.is_empty() && now.contains(new);
    let has_old = !old.is_empty() && now.contains(old);
    if new.contains(old) {
        if has_new {
            EditState::Present
        } else if has_old {
            EditState::Undone
        } else {
            EditState::Unclear
        }
    } else if old.contains(new) {
        if has_old {
            EditState::Undone
        } else if new.is_empty() || has_new {
            EditState::Present
        } else {
            EditState::Unclear
        }
    } else {
        match (has_new, has_old) {
            (true, false) => EditState::Present,
            (false, true) => EditState::Undone,
            _ => EditState::Unclear,
        }
    }
}

fn singular(w: &str) -> String {
    let n = w.chars().count();
    if n >= 6 && w.ends_with("ies") {
        return format!("{}y", &w[..w.len() - 3]);
    }
    if n >= 6 && ["sses", "shes", "ches", "xes", "zes"].iter().any(|e| w.ends_with(e)) {
        return w[..w.len() - 2].to_string();
    }
    if n >= 5 && w.ends_with('s') && !w.ends_with("ss") {
        return w[..w.len() - 1].to_string();
    }
    w.to_string()
}

fn terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<char> = raw.chars().collect();
        let mut parts: Vec<String> = Vec::new();
        let mut cur = String::new();
        for (i, c) in chars.iter().enumerate() {
            let boundary = i > 0 && c.is_uppercase() && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
            if boundary && !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            cur.push(*c);
        }
        parts.push(cur);
        for p in parts {
            let w = singular(&p.to_lowercase());
            if w.chars().count() >= 3 && w.chars().any(|c| c.is_alphabetic()) && !STOP.contains(&w.as_str()) && !QUESTION_WORDS.contains(&w.as_str()) && !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out
}

impl Learn {
    pub fn open(root: &Path) -> Learn {
        let dir = root.join(".stem").join("learn");
        let enabled = std::env::var("STEM_LEARN").map(|v| v != "off" && v != "0").unwrap_or(true);
        let state: State = if enabled { std::fs::read_to_string(dir.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() } else { State::default() };
        Learn { dir, root: root.to_path_buf(), enabled, state, touched: HashSet::new(), tests_run: Vec::new(), edits: Vec::new(), scratch: Vec::new(), last_result: String::new(), events: Vec::new(), preview: false, host: false, decide: None }
    }

    fn lock(&self) -> Option<StateLock> {
        if !self.enabled {
            return None;
        }
        std::fs::create_dir_all(&self.dir).ok()?;
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.dir.join("state.lock")).ok()?;
        file.lock().ok()?;
        Some(StateLock(file))
    }

    fn reload(&mut self) {
        if !self.enabled {
            return;
        }
        if let Some(state) = std::fs::read_to_string(self.dir.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            self.state = state;
        }
    }

    fn save(&self) {
        if !self.enabled {
            return;
        }
        let _ = std::fs::create_dir_all(&self.dir);
        crate::events::ensure_ignored(&self.root.join(".stem"));
        if let Ok(t) = serde_json::to_string_pretty(&self.state) {
            let tmp = self.dir.join(format!("state.json.{}", std::process::id()));
            if std::fs::write(&tmp, t).is_ok() && std::fs::rename(&tmp, self.dir.join("state.json")).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    pub fn test_command(&self) -> Option<&str> {
        self.state.test_command.as_deref().filter(|_| self.enabled && on("testcmd"))
    }

    fn record_clean_failures(&mut self, file: &str, failing: &[String]) {
        for t in failing {
            let f = self.state.failures.entry(t.clone()).or_insert_with(|| Failure { test: t.clone(), file: file.to_string(), seen: 0, clean: true });
            f.seen = f.seen.max(KNOWN_AFTER);
            f.clean = true;
        }
        self.save();
    }

    pub fn observe(&mut self, call: &str, a: &Value, result: &Result<Value, String>) {
        if !self.enabled || result.is_err() {
            return;
        }
        let path = a["path"].as_str().map(|p| p.trim_start_matches("./").to_string());
        match (call, path) {
            ("write", Some(p)) if p.starts_with("/tmp/") || p.starts_with("/private/tmp/") => {
                if !self.scratch.contains(&p) {
                    self.scratch.push(p);
                }
            }
            ("edit" | "write" | "rename" | "remove", Some(p)) if !p.starts_with("/tmp/") && !p.starts_with("/private/tmp/") => {
                if call == "edit" {
                    for e in a["edits"].as_array().into_iter().flatten() {
                        if let (Some(old), Some(new)) = (e["old"].as_str(), e["new"].as_str()) {
                            if old.len() <= FIX_CHARS && new.len() <= FIX_CHARS && old.trim() != new.trim() {
                                self.edits.push((p.clone(), old.to_string(), new.to_string()));
                            }
                        }
                    }
                }
                self.touched.insert(p);
            }
            _ => {}
        }
        let args: Vec<&str> = match call {
            "test" => a["filter"].as_str().unwrap_or("").split_whitespace().collect(),
            "sh" => a["cmd"].as_str().unwrap_or("").split_whitespace().collect(),
            _ => Vec::new(),
        };
        for t in args.into_iter().filter(|t| is_test_file(t) && self.root.join(t).is_file()) {
            if !self.tests_run.iter().any(|x| x == t) {
                self.tests_run.push(t.to_string());
            }
        }
    }

    pub fn annotate_test(&mut self, out: &mut Value) {
        if !self.enabled || !on("known") {
            return;
        }
        let text = format!("{}\n{}", out["out"].as_str().unwrap_or(""), out["err"].as_str().unwrap_or(""));
        let failing = failing_tests(&text);
        if failing.is_empty() {
            return;
        }
        let mut known = Vec::new();
        let mut fresh = Vec::new();
        for t in &failing {
            let hit = self.state.failures.values().find(|f| &f.test == t && f.clean && f.seen >= KNOWN_AFTER);
            match hit {
                Some(_) => known.push(t.clone()),
                None => fresh.push(t.clone()),
            }
        }
        if !known.is_empty() {
            out["knownFailures"] = json!(known);
            out["newFailures"] = json!(fresh);
            out["passedIgnoringKnown"] = json!(fresh.is_empty());
            out["note"] = json!(format!("{} failing test(s) also fail on a clean checkout of this project, before any change; they are not caused by your edit", known.len()));
            if on("plainknown") {
                let banner = if fresh.is_empty() {
                    format!("[stem: every failing test here also fails on a clean checkout, before any change, so your change broke nothing. Pre-existing failures: {}]", known.join("; "))
                } else {
                    format!("[stem: {} of the failures also fail on a clean checkout (pre-existing: {}). New failures: {}]", known.len(), known.join("; "), fresh.join("; "))
                };
                let tail: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                let kept = if fresh.is_empty() { tail[tail.len().saturating_sub(KNOWN_TAIL)..].join("\n") } else { out["out"].as_str().unwrap_or("").to_string() };
                out["out"] = json!(format!("{banner}\n{kept}"));
                if fresh.is_empty() {
                    out["err"] = json!("");
                }
            }
            self.events.push(("learn_used".into(), json!({"what": "known_failures", "n": known.len()})));
        }
    }

    pub fn add_feedback(&mut self, text: &str) -> Value {
        let text: String = text.trim().chars().take(LESSON_CHARS).collect();
        if text.is_empty() {
            return json!({"skipped": "empty feedback"});
        }
        let _lock = self.lock();
        self.reload();
        let mut ws = words(&self.state.last_prompt);
        for w in words(&text) {
            if !ws.contains(&w) {
                ws.push(w);
            }
        }
        let rejected: String = self.state.last_prompt.chars().take(160).collect();
        self.state.fixes.retain(|f| f.request != rejected);
        let hashes: Vec<(String, String)> = self.state.last_changed.iter().map(|f| (f.clone(), content_hash(&self.root.join(f)))).collect();
        let lesson = Lesson { text: text.clone(), files: self.state.last_changed.clone(), words: ws, hashes, session: self.state.sessions };
        self.state.lessons.push(lesson);
        let n = self.state.lessons.len();
        if n > MAX_LESSONS {
            self.state.lessons.drain(..n - MAX_LESSONS);
        }
        self.save();
        json!({"lesson": text, "files": self.state.last_changed})
    }

    fn lesson_changed(&self, l: &Lesson) -> bool {
        l.hashes.iter().any(|(f, h)| content_hash(&self.root.join(f)) != *h)
    }

    pub fn lessons_for(&self, prompt: &str, files: &[String]) -> Option<String> {
        if !self.enabled || !on("lessons") || self.state.lessons.is_empty() {
            return None;
        }
        let ws = words(prompt);
        let mut picked: Vec<&Lesson> = self
            .state
            .lessons
            .iter()
            .rev()
            .filter(|l| l.files.iter().all(|f| self.root.join(f).is_file()))
            .filter(|l| !(self.state.sessions.saturating_sub(l.session) > LESSON_MAX_SESSIONS && l.session > 0 && self.lesson_changed(l)))
            .filter(|l| l.files.iter().any(|f| files.contains(f)) || l.words.iter().filter(|w| ws.contains(w)).count() >= LESSON_MIN_WORDS)
            .collect();
        picked.truncate(2);
        if picked.is_empty() {
            return None;
        }
        Some(format!(
            "[stem: feedback on earlier fixes here, which those fixes failed: {}]",
            picked
                .iter()
                .map(|l| format!(
                    "in {}{}: {}",
                    if l.files.is_empty() { "this project".to_string() } else { l.files.join(", ") },
                    if self.lesson_changed(l) { " (that code has changed since)" } else { "" },
                    l.text
                ))
                .collect::<Vec<_>>()
                .join(" | ")
        ))
    }

    pub fn answer_candidates(&self, prompt: &str) -> Vec<(bool, Answer)> {
        if !self.enabled || !on("answers") || self.state.answers.is_empty() {
            return Vec::new();
        }
        let valid: Vec<&Answer> = self.state.answers.iter().filter(|a| a.files.iter().all(|(f, h)| content_hash(&self.root.join(f)) == *h)).collect();
        let total = valid.len() as f64;
        let share = |asked: &[String], docs: &[Vec<String>]| -> Vec<f64> {
            let df = |w: &String| docs.iter().filter(|d| d.contains(w)).count().max(1) as f64;
            let weight = |w: &String| ((total + 1.0) / df(w)).ln();
            let own: f64 = asked.iter().map(weight).sum::<f64>().max(0.5);
            docs.iter().map(|d| asked.iter().filter(|w| d.contains(w)).map(weight).sum::<f64>() / own).collect()
        };
        let asked_words: Vec<String> = words(prompt).into_iter().filter(|w| !QUESTION_WORDS.contains(&w.as_str())).collect();
        let sure = share(&asked_words, &valid.iter().map(|a| a.words.clone()).collect::<Vec<_>>());
        let near = share(&terms(prompt), &valid.iter().map(|a| terms(&format!("{} {}", a.request, a.answer))).collect::<Vec<_>>());
        let mut scored: Vec<(f64, bool, &Answer)> = valid
            .iter()
            .enumerate()
            .filter(|(i, _)| near[*i] >= ANSWER_MIN_SHARE || sure[*i] >= ANSWER_SURE_SHARE)
            .map(|(i, a)| (near[i].max(sure[i]), sure[i] >= ANSWER_SURE_SHARE, *a))
            .collect();
        scored.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(ANSWER_CANDIDATES).map(|(_, sure, a)| (sure, a.clone())).collect()
    }

    fn ask(&mut self, kind: &str, question: &str, request: &str, options: &[String]) -> Option<usize> {
        let decide = self.decide.as_mut()?;
        if options.is_empty() {
            return None;
        }
        let ids: Vec<String> = (0..options.len()).map(|i| format!("{}{i}", &kind[..1])).collect();
        let asked = json!({
            "kind": kind,
            "question": question,
            "request": request,
            "options": ids.iter().zip(options).map(|(id, text)| json!({"id": id, "text": text})).collect::<Vec<_>>(),
        });
        let reply = decide(&asked)?;
        let confidence = reply["confidence"].as_f64().unwrap_or(0.0);
        let at = ids.iter().position(|id| reply["choice"].as_str() == Some(id.as_str()))?;
        self.events.push(("learn_decided".into(), json!({"kind": kind, "confidence": confidence, "used": confidence >= DECIDE_MIN})));
        (confidence >= DECIDE_MIN).then_some(at)
    }

    fn decided_answer(&mut self, prompt: &str, candidates: &[(bool, Answer)]) -> Option<Answer> {
        let options: Vec<String> = candidates.iter().map(|(_, a)| a.request.trim().chars().take(200).collect()).collect();
        let at = self.ask("answer", "Which earlier question, if any, asks exactly the same thing as this request?", prompt, &options)?;
        candidates.get(at).map(|(_, a)| a.clone())
    }

    pub fn ran_tests(&mut self, files: &[String]) {
        for t in files.iter().filter(|t| is_test_file(t)) {
            if !self.tests_run.contains(t) {
                self.tests_run.push(t.clone());
            }
        }
    }

    pub fn import_empryo(&mut self, path: &Path) -> Result<Value, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let old: Value = serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
        if old["version"].as_u64() != Some(1) {
            return Err(format!("{} is not an Empryo Stem store of version 1", path.display()));
        }
        let sha1_of = |file: &str| -> String {
            use sha1::Digest;
            std::fs::read(self.root.join(file)).map(|b| sha1::Sha1::digest(&b).iter().take(8).map(|x| format!("{x:02x}")).collect::<String>()).unwrap_or_default()
        };
        let carry = |pairs: &Value| -> Option<Vec<(String, String)>> {
            pairs.as_array()?.iter().map(|p| {
                let file = p[0].as_str()?;
                let same = !sha1_of(file).is_empty() && p[1].as_str() == Some(sha1_of(file).as_str());
                Some((file.to_string(), if same { content_hash(&self.root.join(file)) } else { "changed before import".to_string() }))
            }).collect()
        };
        let fixes: Vec<Fix> = old["fixes"].as_array().into_iter().flatten().filter_map(|f| {
            let fix = Fix { file: f["file"].as_str()?.to_string(), before: f["before"].as_str()?.to_string(), after: f["after"].as_str()?.to_string(), request: f["request"].as_str()?.chars().take(160).collect() };
            let now = std::fs::read_to_string(self.root.join(&fix.file)).unwrap_or_default();
            (learnable(&fix.file) && !fix.before.trim().is_empty() && substantive(&fix.request) && edit_state(&now, &fix.before, &fix.after) == EditState::Present).then_some(fix)
        }).collect();
        let answers: Vec<Answer> = old["answers"].as_array().into_iter().flatten().filter_map(|a| {
            let request = a["request"].as_str()?;
            let files = carry(&a["files"])?;
            (substantive(request) && !files.is_empty() && files.iter().all(|(f, h)| learnable(f) && *h == content_hash(&self.root.join(f)))).then(|| Answer { words: words(request), request: request.to_string(), answer: a["answer"].as_str().unwrap_or("").chars().take(ANSWER_CHARS).collect(), files })
        }).collect();
        let lessons: Vec<Lesson> = old["lessons"].as_array().into_iter().flatten().filter_map(|l| {
            let text = l["text"].as_str()?.trim().to_string();
            let files: Vec<String> = l["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect()).unwrap_or_default();
            (!text.is_empty()).then(|| Lesson { words: words(&format!("{text} {}", l["words"].as_array().map(|w| w.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default())), hashes: carry(&l["hashes"]).unwrap_or_default(), files, text, session: 0 })
        }).collect();
        let failures: Vec<Failure> = old["failures"].as_object().into_iter().flatten().filter_map(|(name, f)| {
            let seen = f["seen"].as_u64()? as u32;
            (seen >= KNOWN_AFTER).then(|| Failure { test: f["test"].as_str().unwrap_or(name).to_string(), file: f["command"].as_str().unwrap_or("").to_string(), seen, clean: true })
        }).collect();
        let _lock = self.lock();
        self.reload();
        let mut added = (0usize, 0usize, 0usize, 0usize);
        for fix in fixes {
            if !self.state.fixes.iter().any(|f| f.file == fix.file && f.before == fix.before) {
                self.state.fixes.insert(0, fix);
                added.0 += 1;
            }
        }
        self.state.fixes.truncate(MAX_FIXES);
        for answer in answers {
            if !self.state.answers.iter().any(|a| a.request == answer.request) {
                self.state.answers.insert(0, answer);
                added.1 += 1;
            }
        }
        self.state.answers.truncate(MAX_ANSWERS);
        for lesson in lessons {
            if !self.state.lessons.iter().any(|l| l.text == lesson.text) {
                self.state.lessons.insert(0, lesson);
                added.2 += 1;
            }
        }
        self.state.lessons.truncate(MAX_LESSONS);
        for failure in failures {
            if !self.state.failures.contains_key(&failure.test) {
                added.3 += 1;
            }
            let entry = self.state.failures.entry(failure.test.clone()).or_insert_with(|| failure.clone());
            entry.seen = entry.seen.max(failure.seen);
            entry.clean = true;
        }
        self.save();
        Ok(json!({"fixes": added.0, "answers": added.1, "lessons": added.2, "knownFailures": added.3}))
    }

    pub fn summary(&self) -> Value {
        json!({
            "sessions": self.state.sessions,
            "fixes": self.state.fixes.len(),
            "answers": self.state.answers.len(),
            "lessons": self.state.lessons.len(),
            "knownFailures": self.state.failures.values().filter(|f| f.clean && f.seen >= KNOWN_AFTER).count(),
            "familiarWords": self.state.trails.len(),
            "testCommand": self.state.test_command,
            "resume": self.state.resume.is_some(),
        })
    }

    pub fn answer_note(&mut self, chosen: &Answer) -> String {
        self.events.push(("learn_used".into(), json!({"what": "answers"})));
        format!(
            "[stem: an earlier answer to the same question. The files it cites have not changed since ({}), so it still holds. Asked: {}\nAnswer:\n{}\nAnswer from it directly, in a final answer without a script; read code only if the new question asks for something it does not cover.]",
            chosen.files.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(", "),
            chosen.request.trim(),
            chosen.answer.trim()
        )
    }

    pub fn resume_note(&mut self, prompt: &str) -> Option<String> {
        if !self.enabled || !on("resume") {
            return None;
        }
        let r = self.state.resume.clone()?;
        if r.at > 0 && unix_now().saturating_sub(r.at) > RESUME_MAX_SECS {
            let _lock = self.lock();
            self.reload();
            if self.state.resume.as_ref().is_some_and(|now| now.at == r.at) {
                self.state.resume = None;
                self.save();
            }
            return None;
        }
        let asked = words(prompt);
        let shared = words(&r.request).iter().filter(|w| asked.contains(w)).count();
        if shared < RESUME_MIN_WORDS {
            return None;
        }
        self.events.push(("learn_used".into(), json!({"what": "resume"})));
        let mut present: Vec<String> = Vec::new();
        let mut undone: Vec<String> = Vec::new();
        let mut moved: Vec<String> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        for (i, (f, old, new)) in r.edits.iter().enumerate() {
            let superseded = !new.is_empty() && r.edits[i + 1..].iter().any(|(g, later_old, _)| g == f && later_old.contains(new.as_str()));
            if superseded {
                continue;
            }
            let Ok(now) = std::fs::read_to_string(self.root.join(f)) else {
                if !gone.contains(f) {
                    gone.push(f.clone());
                }
                continue;
            };
            let shown = format!("in {f}\n```\n{}\n```\nbecame\n```\n{}\n```", old.trim_end(), new.trim_end());
            match edit_state(&now, old, new) {
                EditState::Present => present.push(shown),
                EditState::Undone => undone.push(shown),
                EditState::Unclear => {
                    if !moved.contains(f) {
                        moved.push(f.clone())
                    }
                }
            }
        }
        let mut lines = vec![format!(
            "[stem: an earlier session stopped after {} steps without a final answer. It was asked:\n```\n{}\n```\nIf you are asked to do the same work, continue from where it stopped instead of starting over.",
            r.steps,
            r.request.trim()
        )];
        if r.edits.is_empty() {
            lines.push("It made no edits.".into());
        }
        if !present.is_empty() {
            lines.push(format!("Its edits still in the files: {}", present.join("; ")));
        }
        if !undone.is_empty() {
            lines.push(format!("Its edits no longer in the files (the code before them is back): {}", undone.join("; ")));
        }
        if !moved.is_empty() {
            lines.push(format!("Code it edited has changed since, so read it again before relying on those edits: {}.", moved.join(", ")));
        }
        if !gone.is_empty() {
            lines.push(format!("Files it edited no longer exist: {}.", gone.join(", ")));
        }
        let scratch: Vec<&String> = r.scratch.iter().filter(|p| self.root.join(p.as_str()).is_file()).collect();
        if !scratch.is_empty() {
            lines.push(format!("Its scratch scripts still exist: {}.", scratch.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")));
        }
        lines.push(if r.last_result.trim().is_empty() {
            "Its last script returned nothing.]".to_string()
        } else {
            format!("Its last script returned:\n```\n{}\n```]", r.last_result.trim())
        });
        Some(lines.join("\n"))
    }

    pub fn regressions(&mut self, prompt: &str) -> Option<String> {
        if !self.enabled || !on("regress") || self.state.fixes.is_empty() {
            return None;
        }
        let asked = words(prompt);
        let mut groups: Vec<(String, String, Vec<Fix>)> = Vec::new();
        for f in self.state.fixes.iter().rev() {
            match groups.iter_mut().find(|(r, file, _)| *r == f.request && *file == f.file) {
                Some(g) => g.2.push(f.clone()),
                None => groups.push((f.request.clone(), f.file.clone(), vec![f.clone()])),
            }
        }
        let requests: Vec<Vec<String>> = groups.iter().map(|(r, _, _)| words(r)).collect();
        let mut texts: BTreeMap<String, String> = BTreeMap::new();
        let mut found: Vec<(usize, bool, String, String, Vec<Fix>)> = Vec::new();
        let mut unsure: Vec<(String, String, Vec<Fix>)> = Vec::new();
        let prune = |fixes: &[Fix]| -> Vec<Fix> { fixes.iter().filter(|f| !fixes.iter().any(|g| g.before.len() > f.before.len() && g.before.contains(f.before.as_str()) && g.after.contains(f.after.as_str()))).cloned().collect() };
        for (request, file, fixes) in groups {
            let now = texts.entry(file.clone()).or_insert_with(|| std::fs::read_to_string(self.root.join(&file)).unwrap_or_default());
            if now.is_empty() || !fixes.iter().all(|f| edit_state(now, &f.before, &f.after) == EditState::Undone) {
                continue;
            }
            let theirs = words(&request);
            let shared = theirs.iter().filter(|w| asked.contains(w)).count();
            let distinctive = theirs.iter().filter(|w| asked.contains(w)).any(|w| requests.iter().filter(|r| r.contains(w)).count() <= 1);
            let names_file = words(&file).iter().any(|w| asked.contains(w));
            if shared < REGRESS_MIN_WORDS && !distinctive && !names_file {
                unsure.push((request, file, prune(&fixes)));
                continue;
            }
            let strong = shared * 10 >= theirs.len() * 6;
            found.push((shared, strong, request, file, prune(&fixes)));
        }
        if found.is_empty() && !unsure.is_empty() {
            unsure.truncate(DECIDE_FIX_OPTIONS);
            let options: Vec<String> = unsure.iter().map(|(r, file, _)| format!("{} ({file})", r.trim().chars().take(160).collect::<String>())).collect();
            if let Some(at) = self.ask("fix", "Which earlier fix, if any, is the same bug this request is about? Its code has been undone in the file.", prompt, &options) {
                let (request, file, kept) = unsure.swap_remove(at);
                found.push((0, false, request, file, kept));
            }
        }
        if found.is_empty() {
            return None;
        }
        found.sort_by(|a, b| b.0.cmp(&a.0));
        found.truncate(2);
        let changes = |kept: &[Fix]| kept.iter().rev().take(3).map(|f| format!("```\n{}\n```\nto\n```\n{}\n```", f.before.trim_end(), f.after.trim_end())).collect::<Vec<_>>().join("\nand ");
        let (_, strong, request, file, kept) = &found[0];
        let deliberate = kept.iter().any(|f| undone_in_a_commit(&self.root, file, &f.after));
        if *strong && on("restore") && !deliberate && !self.host {
            let mut now = std::fs::read_to_string(self.root.join(file)).unwrap_or_default();
            let mut applied: Vec<&Fix> = Vec::new();
            for f in kept.iter().rev() {
                if let Some(at) = now.find(f.before.as_str()) {
                    now.replace_range(at..at + f.before.len(), &f.after);
                    applied.push(f);
                }
            }
            if self.preview && applied.len() == kept.len() {
                return Some(format!("[stem: this request matches an earlier fix that was undone since, for the request: {}\nWhen the task starts, Stem re-applies that fix to {file}: it changed {}]", request.trim(), changes(kept)));
            }
            if applied.len() == kept.len() && std::fs::write(self.root.join(file), &now).is_ok() {
                for f in applied {
                    self.edits.push((file.clone(), f.before.clone(), f.after.clone()));
                }
                self.touched.insert(file.clone());
                self.events.push(("learn_used".into(), json!({"what": "restore", "file": file})));
                return Some(format!(
                    "[stem: this request matches an earlier fix that was undone since, for the request: {}\nStem has re-applied that fix to {file}: it changed {}\nCheck it against the report as you see fit, then answer. Change it only if it does not fix what the report describes.]",
                    request.trim(),
                    changes(kept)
                ));
            }
        }
        self.events.push(("learn_used".into(), json!({"what": "regress", "n": found.len(), "deliberate": deliberate})));
        let lead = if deliberate {
            "[stem: an earlier fix was removed again in a later commit, so it may have been undone on purpose. If this report asks for that behaviour back, re-apply it and answer; otherwise leave it.]"
        } else {
            "[stem: an earlier fix was undone. If this report matches its request, re-apply that fix and answer; search further only if the report asks for something else.]"
        };
        Some(format!(
            "{lead}\n{}",
            found
                .iter()
                .map(|(_, _, request, file, kept)| {
                    let now = texts.get(file).cloned().unwrap_or_default();
                    let line = kept.iter().filter_map(|f| now.find(f.before.as_str())).min().map(|at| now[..at].matches('\n').count() + 1).unwrap_or(1);
                    format!("{file}:{line} again has the code an earlier fix replaced, for the request: {}\nThat fix changed {}", request.trim(), changes(kept))
                })
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    pub fn warm(&mut self, prompt: &str) -> Option<Warm> {
        if !self.enabled || !on("warm") || self.state.trails.is_empty() {
            return None;
        }
        let files_total = self.state.trails.values().flat_map(|m| m.keys()).collect::<HashSet<_>>().len().max(1) as f64;
        let mut score: BTreeMap<String, (f64, Vec<String>)> = BTreeMap::new();
        let mut by_word: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
        for (key, files) in &self.state.trails {
            let merged = by_word.entry(words(key).into_iter().next().unwrap_or_else(|| key.clone())).or_default();
            for (f, n) in files {
                *merged.entry(f.clone()).or_default() += n;
            }
        }
        let mut linked: BTreeMap<String, usize> = BTreeMap::new();
        for files in by_word.values() {
            for f in files.keys() {
                *linked.entry(f.clone()).or_default() += 1;
            }
        }
        let word_total = by_word.len();
        let hub = |f: &str| word_total >= HUB_MIN_WORDS && linked.len() >= HUB_MIN_FILES && *linked.get(f).unwrap_or(&0) as f64 / word_total as f64 > HUB_SHARE;
        let asked = words(prompt);
        for w in asked.clone() {
            let Some(m) = by_word.get(&w) else { continue };
            let idf = (files_total / m.len() as f64).ln() + 0.1;
            for (f, n) in m {
                if !self.root.join(f).is_file() || !trail_target(f) || hub(f) {
                    continue;
                }
                let age = self.state.sessions.saturating_sub(*self.state.changed_at.get(f).unwrap_or(&self.state.sessions)) as f64;
                let e = score.entry(f.clone()).or_insert((0.0, Vec::new()));
                e.0 += idf * (*n as f64).sqrt() * 0.5f64.powf(age / TRAIL_HALF_LIFE);
                if e.1.len() < 4 {
                    e.1.push(w.clone());
                }
            }
        }
        for (f, (s, _)) in score.iter_mut() {
            let hits = words(f).iter().filter(|w| asked.contains(w)).count();
            *s *= 1.0 + hits as f64;
        }
        let mut ranked: Vec<(String, (f64, Vec<String>))> = score.into_iter().filter(|(_, (s, _))| *s >= 1.0).collect();
        ranked.sort_by(|a, b| b.1.0.partial_cmp(&a.1.0).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(3);
        if ranked.is_empty() {
            return None;
        }
        self.events.push(("learn_used".into(), json!({"what": "warm", "files": ranked.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>()})));
        let score = ranked[0].1.0;
        Some(Warm {
            note: format!(
                "[stem: earlier work in this project with similar words changed: {}. Start there if it fits; search if not. Read the whole function you change and the code around it before editing, so the change follows its order and conventions.]",
                ranked.iter().map(|(f, (_, w))| format!("{f} ({})", w.join(", "))).collect::<Vec<_>>().join("; ")
            ),
            files: ranked.into_iter().map(|(f, _)| f).collect(),
            score,
        })
    }

    pub fn prompt_lines(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let mut lines = Vec::new();
        if let Some(c) = self.test_command() {
            lines.push(format!("- $.test(files) runs `{c}` here, which a clean checkout passes."));
        }
        let known = self.state.failures.values().filter(|f| f.seen >= KNOWN_AFTER).count();
        if known > 0 && on("known") {
            lines.push(format!("- {known} test(s) fail on a clean checkout; $.test marks them in knownFailures, so judge your change by newFailures."));
        }
        lines
    }

    pub fn external(&mut self, prompt: &str, answer: &str, edits: Vec<(String, String, String)>, touched: Vec<String>, finished: bool) {
        self.edits = edits;
        self.touched = touched.into_iter().collect();
        self.finish(prompt, finished, 0, answer);
    }

    pub fn task_request(&self, prompt: &str) -> String {
        if substantive(prompt) || self.state.task.is_empty() {
            prompt.to_string()
        } else {
            format!("{}\n{prompt}", self.state.task)
        }
    }

    pub fn brief(&mut self, prompt: &str) -> Option<String> {
        let asks_new = substantive(prompt);
        let task = self.task_request(prompt);
        let prompt = task.as_str();
        let warm = self.warm(prompt);
        let files: Vec<String> = warm.as_ref().map(|w| w.files.clone()).unwrap_or_default();
        let mut parts: Vec<String> = Vec::new();
        if let Some(r) = self.resume_note(prompt) {
            parts.push(r);
        }
        if asks_new {
            let candidates = self.answer_candidates(prompt);
            let chosen = match candidates.first() {
                Some((true, a)) => Some(a.clone()),
                _ => self.decided_answer(prompt, &candidates),
            };
            if let Some(a) = chosen {
                let note = self.answer_note(&a);
                parts.push(note);
            }
            if let Some(r) = self.regressions(prompt) {
                parts.push(r);
            }
        }
        if let Some(l) = self.lessons_for(prompt, &files) {
            self.events.push(("learn_used".into(), json!({"what": "lessons"})));
            parts.push(l);
        }
        if let Some(w) = warm {
            parts.push(w.note);
        }
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    pub fn finish(&mut self, prompt: &str, finished: bool, steps: usize, answer: &str) {
        if !self.enabled {
            return;
        }
        let _lock = self.lock();
        self.reload();
        let task = self.task_request(prompt);
        let asks_new = substantive(prompt);
        if asks_new {
            self.state.task = prompt.chars().take(600).collect();
        }
        let prompt = task.as_str();
        self.state.sessions += 1;
        self.state.resume = (!finished && on("resume")).then(|| Resume {
            request: prompt.chars().take(600).collect(),
            steps,
            edits: self.edits.clone(),
            scratch: self.scratch.iter().filter(|p| Path::new(p).is_file()).cloned().collect(),
            last_result: self.last_result.chars().rev().take(RESUME_RESULT).collect::<Vec<_>>().into_iter().rev().collect(),
            at: unix_now(),
        });
        let changed: Vec<String> = self.touched.iter().filter(|t| !is_test_file(t) && learnable(t)).cloned().collect();
        if asks_new && finished && changed.is_empty() && on("answers") && !answer.trim().is_empty() {
            let cited: Vec<(String, String)> = regex::Regex::new(r"[A-Za-z0-9_./-]+\.[A-Za-z0-9]+")
                .unwrap()
                .find_iter(answer)
                .map(|m| m.as_str().trim_start_matches("./").to_string())
                .filter(|p| self.root.join(p).is_file() && learnable(p))
                .fold(Vec::new(), |mut v, p| {
                    if !v.iter().any(|(x, _)| *x == p) {
                        let hash = content_hash(&self.root.join(&p));
                        v.push((p, hash));
                    }
                    v
                });
            if !cited.is_empty() {
                self.state.answers.push(Answer { words: words(prompt), request: prompt.chars().take(300).collect(), answer: answer.chars().take(ANSWER_CHARS).collect(), files: cited });
                let n = self.state.answers.len();
                if n > MAX_ANSWERS {
                    self.state.answers.drain(..n - MAX_ANSWERS);
                }
            }
        }
        self.state.last_changed = changed.clone();
        self.state.last_prompt = prompt.chars().take(2000).collect();
        if !changed.is_empty() {
            for f in &changed {
                self.state.changed_at.insert(f.clone(), self.state.sessions);
            }
            let asked = words(prompt);
            let targets: Vec<&String> = changed.iter().filter(|f| trail_target(f)).collect();
            let targets: Vec<&String> = if targets.len() > TRAIL_FILES_MAX { targets.into_iter().filter(|f| words(f).iter().any(|w| asked.contains(w))).collect() } else { targets };
            for w in asked.into_iter().filter(|w| !FOLLOW_UP.contains(&w.as_str()) && !QUESTION_WORDS.contains(&w.as_str())).take(40) {
                let entry = self.state.trails.entry(w).or_default();
                for f in &targets {
                    *entry.entry((*f).clone()).or_default() += 1;
                }
            }
            for t in &self.tests_run {
                *self.state.tested.entry(t.clone()).or_default() += 1;
            }
            if on("regress") {
                for (file, before, after) in self.edits.iter().filter(|(f, before, _)| learnable(f) && !before.trim().is_empty()).take(FIXES_PER_TASK) {
                    let now = std::fs::read_to_string(self.root.join(file)).unwrap_or_default();
                    if edit_state(&now, before, after) == EditState::Present {
                        self.state.fixes.retain(|f| !(f.file == *file && f.before == *before));
                        self.state.fixes.push(Fix { file: file.clone(), before: before.clone(), after: after.clone(), request: prompt.chars().take(160).collect() });
                    }
                }
                let n = self.state.fixes.len();
                if n > MAX_FIXES {
                    self.state.fixes.drain(..n - MAX_FIXES);
                }
            }
        }
        for t in &self.touched {
            if !self.state.recent.contains(t) {
                self.state.recent.push(t.clone());
            }
        }
        let n = self.state.recent.len();
        if n > RECENT {
            self.state.recent.drain(..n - RECENT);
        }
        self.save();
    }

    pub fn recent(&self) -> Vec<String> {
        self.state.recent.clone()
    }
}

pub fn calibrate(root: &Path) -> Value {
    let dirty = std::process::Command::new("git").args(["status", "--porcelain", "--untracked-files=no"]).current_dir(root).output().map(|o| !o.stdout.is_empty()).unwrap_or(false);
    if dirty {
        return json!({"skipped": "uncommitted changes: test results would not describe a clean checkout"});
    }
    let mut learn = Learn::open(root);
    let project = crate::project::Project::detect(root);
    let script = std::fs::read_to_string(root.join("package.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["scripts"]["test"].as_str().map(str::to_string)).unwrap_or_default();
    let exec = if project.runner == "bun" { "bunx" } else { "npx" };
    let mut candidates: Vec<String> = Vec::new();
    if project.runner == "bun" {
        candidates.push("bun test <files>".into());
    }
    if script.contains("vitest") {
        candidates.push(format!("{exec} vitest run <files> --coverage.enabled=false"));
    }
    if script.contains("jest") {
        candidates.push(format!("{exec} jest <files>"));
    }
    if candidates.is_empty() {
        return json!({"skipped": "no test runner found"});
    }
    let mut sample: Vec<String> = Vec::new();
    let mut checked: Vec<(&String, u32)> = learn.state.tested.iter().map(|(t, n)| (t, *n)).collect();
    checked.sort_by(|a, b| b.1.cmp(&a.1));
    for (t, _) in checked {
        if root.join(t).is_file() && !sample.contains(t) && sample.len() < SAMPLE_CHECKED {
            sample.push(t.clone());
        }
    }
    for m in learn.recent().iter().rev() {
        let p = root.join(m);
        let (Some(dir), Some(stem)) = (p.parent(), p.file_stem().and_then(|s| s.to_str())) else { continue };
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{stem}.")) && is_test_file(&name) {
                let rel = entry.path().strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
                if !sample.contains(&rel) {
                    sample.push(rel);
                }
            }
        }
        if sample.len() >= SAMPLE_TOTAL {
            break;
        }
    }
    if sample.is_empty() {
        return json!({"skipped": "no recently touched code with tests yet"});
    }
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut results: Vec<(String, Vec<String>, u128)> = Vec::new();
    let mut tried = Vec::new();
    for c in &candidates {
        let cmd = c.replace("<files>", &sample.join(" "));
        let started = std::time::Instant::now();
        let Ok(out) = crate::host::run_command(&cmd, root, std::time::Duration::from_secs(180), &cancel) else { continue };
        let ms = started.elapsed().as_millis();
        let text = format!("{}\n{}", out["out"].as_str().unwrap_or(""), out["err"].as_str().unwrap_or(""));
        let failing = failing_tests(&text);
        let code = out["code"].as_i64().unwrap_or(-1);
        let passed = regex::Regex::new(r"(?i)(\d+)\s+pass|Tests\s+(\d+)\s+passed|(\d+)\s+passed").unwrap().captures_iter(&text).filter_map(|c| c.iter().skip(1).flatten().next().and_then(|m| m.as_str().parse::<u64>().ok())).max().unwrap_or(0);
        let ran = passed > 0 && (code == 0 || !failing.is_empty());
        tried.push(json!({"cmd": c, "code": code, "failing": failing.len(), "passed": passed, "ran": ran, "ms": ms}));
        if ran {
            results.push((c.clone(), failing, ms));
        }
    }
    let fewest = results.iter().map(|(_, f, _)| f.len()).min().unwrap_or(0);
    let pick = results.iter().filter(|(_, f, _)| f.len() <= fewest + MAX_KNOWN_EXTRA).min_by_key(|(_, _, ms)| *ms).cloned();
    let Some((cmd, failing, _)) = pick else { return json!({"tried": tried}) };
    let n = failing.len();
    let _lock = learn.lock();
    learn.reload();
    if candidates.len() > 1 {
        learn.state.test_command = Some(cmd.clone());
    }
    learn.record_clean_failures(&sample.join(" "), &failing);
    learn.save();
    json!({"testCommand": cmd, "cleanFailures": n, "sample": sample, "tried": tried})
}

fn content_hash(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::read(path).unwrap_or_default().hash(&mut h);
    format!("{:016x}", h.finish())
}
