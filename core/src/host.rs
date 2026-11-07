use crate::llm::{Block, Client, Msg, Role, Usage};
use crate::lsp::{Server, path_of};
use crate::project::Project;
use crate::servers::{self, Lang};
use crate::ui::{Board, ServerState};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SH_OUTPUT_LIMIT: usize = 2_000_000;
const GREP_DEFAULT_MAX: usize = 300;
const FAMILIAR_SCORE: f64 = 3.0;
const DIAGNOSTIC_WAIT: Duration = Duration::from_millis(2500);

pub struct PendingJudge {
    pub candidates: Vec<crate::learn::Answer>,
    pub prompt: String,
    pub resolved: crate::catalog::Resolved,
}

pub struct StartNote {
    pub note: Option<String>,
    pub familiar: bool,
    pub judge: Option<PendingJudge>,
}

pub struct CallRecord {
    pub name: String,
    pub ok: bool,
    pub target: Option<String>,
}

pub struct ServerEvent {
    pub lang: &'static str,
    pub root: String,
    pub name: String,
    pub source: String,
    pub ms: u128,
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Approval {
    Once,
    Always,
    Deny,
}

pub type Approver = Box<dyn FnMut(&str, &str) -> Approval>;

pub struct Host {
    pub root: PathBuf,
    pub yolo: Arc<AtomicBool>,
    pub approver: Option<Approver>,
    pub project: Project,
    servers: HashMap<(Lang, PathBuf), Server>,
    failed: HashMap<(Lang, PathBuf), String>,
    pub calls: Vec<CallRecord>,
    pub touched: usize,
    pub server_events: Vec<ServerEvent>,
    pub ask_client: Option<Rc<Client>>,
    pub ask_usage: Usage,
    pub ask_cost: f64,
    pub plan: Vec<(String, bool)>,
    pub context: Value,
    pub offload_dir: Option<PathBuf>,
    facts: Option<crate::facts::Facts>,
    pub growth: crate::growth::Growth,
    pub learn: crate::learn::Learn,
    board: Board,
    cancel: Arc<AtomicBool>,
    lsp_enabled: bool,
}

type HostResult = Result<Value, String>;

impl Host {
    pub fn new(root: PathBuf, board: Board, cancel: Arc<AtomicBool>, ask_client: Option<Rc<Client>>, yolo: Arc<AtomicBool>, approver: Option<Approver>) -> Host {
        let project = Project::detect(&root);
        let growth = crate::growth::Growth::open(&root);
        let learn = crate::learn::Learn::open(&root);
        Host {
            root,
            yolo,
            approver,
            project,
            servers: HashMap::new(),
            failed: HashMap::new(),
            calls: Vec::new(),
            touched: 0,
            server_events: Vec::new(),
            ask_client,
            ask_usage: Usage::default(),
            ask_cost: 0.0,
            plan: Vec::new(),
            context: Value::Null,
            offload_dir: None,
            facts: None,
            growth,
            learn,
            board,
            cancel,
            lsp_enabled: std::env::var("STEM_LSP").map(|v| v != "off" && v != "0").unwrap_or(true),
        }
    }

    pub fn dispatch(&mut self, name: &str, args: &str) -> String {
        let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
        if let Some(reply) = self.internal(name, &args) {
            return reply.to_string();
        }
        let target = args["path"]
            .as_str()
            .or(args["file"].as_str())
            .or(args["name"].as_str())
            .or(args["cmd"].as_str())
            .map(|s| s.chars().take(80).collect());
        let result = self.call(name, &args);
        self.learn.observe(name, &args, &result);
        let reply = match &result {
            Ok(v) => json!({"ok": true, "value": v}),
            Err(e) => json!({"ok": false, "error": e}),
        };
        let text = reply.to_string();
        self.touched += text.len();
        self.board.piece(name, target.as_deref(), text.len(), result.is_ok());
        self.calls.push(CallRecord {
            name: name.to_string(),
            ok: result.is_ok(),
            target,
        });
        text
    }

    fn internal(&mut self, name: &str, a: &Value) -> Option<Value> {
        match name {
            "tools" => Some(json!({"ok": true, "value": self.growth.loadable()})),
            "tool_used" => {
                let name = a["name"].as_str().unwrap_or("");
                let ok = a["ok"].as_bool().unwrap_or(false);
                let missing = if ok { self.missing_anchors(&self.growth.anchors(name)) } else { Vec::new() };
                let note = self.growth.used(name, ok, a["size"].as_u64(), a["chars"].as_u64(), &missing);
                self.board.piece("tool", Some(name), 200, ok);
                Some(json!({"ok": true, "value": note}))
            }
            "tool_checked" => {
                let note = self.growth.checked(a["name"].as_str().unwrap_or(""), a["ok"].as_bool().unwrap_or(false), a["detail"].as_str().unwrap_or(""));
                Some(json!({"ok": true, "value": note}))
            }
            _ => None,
        }
    }

    fn call(&mut self, name: &str, a: &Value) -> HostResult {
        match name {
            "read" => self.read(a),
            "write" => self.write(a),
            "remove" => self.remove(a),
            "edit" => self.edit(a),
            "files" => self.files(a),
            "grep" => self.grep(a),
            "sh" => self.sh(a),
            "project" => {
                let mut p = self.project.to_json();
                p["intelligence"] = json!(self.intelligence());
                Ok(p)
            }
            "sql" => self.sql(a),
            "save" => {
                let mut a = a.clone();
                let given: Vec<String> = a["anchors"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
                let wanted = if given.is_empty() { crate::growth::anchors_of(a["code"].as_str().unwrap_or("")) } else { given };
                let missing = self.missing_anchors(&wanted);
                let kept: Vec<String> = wanted.into_iter().filter(|w| !missing.contains(w)).collect();
                a["anchors"] = json!(kept);
                let mut saved = self.growth.save(&a)?;
                if !missing.is_empty() {
                    saved["anchorsNotInCode"] = json!(missing);
                }
                Ok(saved)
            }
            "forget" => {
                let name = a["name"].as_str().ok_or("missing name")?;
                self.growth.drop_tool(name, a["reason"].as_str().unwrap_or("forgotten on request"));
                Ok(json!({"dropped": name}))
            }
            "plan" => self.set_plan(a),
            "eject" => Err("ejecting happens only at compaction: Stem asks you which steps to eject when the context passes 80%, or when the user runs /compact. Keep working.".into()),
            "context" => Ok(self.context.clone()),
            "help" => Ok(Value::String(crate::agent::help(a["name"].as_str()))),
            "toolList" => Ok(self.growth.list()),
            "recall" => {
                let id = a["id"].as_u64().or_else(|| a["id"].as_str().and_then(|s| s.trim().trim_start_matches('#').parse().ok())).ok_or("$.recall(id) needs a step id")?;
                let dir = self.offload_dir.clone().ok_or("nothing was ejected in this session")?;
                std::fs::read_to_string(dir.join(format!("{id}.md"))).map(Value::String).map_err(|_| format!("step #{id} was not ejected; $.context() shows which ones are out"))
            }
            "done" => self.mark_done(a),
            "test" => self.test(a),
            "testsFor" => self.tests_for(a),
            "run" => self.run_script(a),
            "defs" => self.locations("textDocument/definition", a),
            "impls" => self.locations("textDocument/implementation", a),
            "refs" => self.refs(a),
            "callers" => self.callers(a),
            "hover" => self.hover(a),
            "rename" => self.rename(a),
            "symbols" => self.symbols(a),
            "outline" => self.outline(a),
            "diagnostics" => self.diagnostics(a),
            "ask" => self.ask(a),
            other => Err(format!(
                "$.{other} does not exist. Available: read, write, edit, remove, files, grep, sh, project, test, run, defs, refs, callers, impls, typeAt, rename, symbols, outline, diagnostics, ask"
            )),
        }
    }

    pub fn finish_learning(&mut self, prompt: &str, finished: bool, steps: usize, answer: &str) {
        self.learn.finish(prompt, finished, steps, answer);
    }

    pub fn start_note(&mut self, prompt: &str) -> StartNote {
        let warm = self.learn.warm(prompt);
        let files: Vec<String> = warm.as_ref().map(|w| w.files.clone()).unwrap_or_default();
        let score = warm.as_ref().map(|w| w.score);
        let mut parts: Vec<String> = Vec::new();
        if let Some(r) = self.learn.resume_note(prompt) {
            parts.push(r);
        }
        let (same, judge) = self.same_question(prompt);
        if let Some(a) = same {
            let note = self.learn.answer_note(&a);
            parts.push(note);
        }
        if let Some(r) = self.learn.regressions(prompt) {
            parts.push(r);
        }
        if let Some(l) = self.learn.lessons_for(prompt, &files) {
            self.learn.events.push(("learn_used".into(), json!({"what": "lessons"})));
            parts.push(l);
        }
        if let Some(w) = warm {
            parts.push(w.note);
        }
        StartNote { note: (!parts.is_empty()).then(|| parts.join("\n")), familiar: score.is_some_and(|s| s >= FAMILIAR_SCORE), judge }
    }

    fn resolve(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        let joined = if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) };
        normalize(&joined)
    }

    fn inside(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }

    fn scratch(&self, path: &Path) -> bool {
        let temp = std::env::temp_dir();
        let real = std::fs::canonicalize(&temp).unwrap_or(temp.clone());
        [temp, real, PathBuf::from("/tmp"), PathBuf::from("/private/tmp")].iter().any(|t| path.starts_with(t))
    }

    pub fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.display().to_string())
    }

    fn need_path(&self, a: &Value) -> Result<PathBuf, String> {
        let raw = a["path"].as_str().or(a["file"].as_str()).ok_or("missing path")?;
        Ok(self.resolve(raw))
    }

    fn missing_file(&self, path: &Path) -> String {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let similar: Vec<String> = walk(&self.root)
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == name))
            .take(5)
            .map(|p| self.rel(&p))
            .collect();
        if similar.is_empty() {
            format!("{} does not exist", self.rel(path))
        } else {
            format!("{} does not exist. Files with that name: {}", self.rel(path), similar.join(", "))
        }
    }

    fn read(&mut self, a: &Value) -> HostResult {
        let path = self.need_path(a)?;
        if path.is_dir() {
            let mut entries: Vec<String> = std::fs::read_dir(&path)
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if e.path().is_dir() { format!("{n}/") } else { n }
                })
                .collect();
            entries.sort();
            return Err(format!(
                "{} is a folder. It contains: {}. Use $.files(glob) to list files.",
                self.rel(&path),
                entries.join(", ")
            ));
        }
        let text = std::fs::read(&path).map_err(|_| self.missing_file(&path))?;
        let text = String::from_utf8_lossy(&text).into_owned();
        let from = a["from"].as_u64();
        let to = a["to"].as_u64();
        if from.is_none() && to.is_none() {
            return Ok(json!(text));
        }
        let lines: Vec<&str> = text.lines().collect();
        let start = from.unwrap_or(1).max(1) as usize;
        let end = (to.unwrap_or(lines.len() as u64) as usize).min(lines.len());
        Ok(json!(lines.get(start - 1..end).map(|s| s.join("\n")).unwrap_or_default()))
    }

    fn write(&mut self, a: &Value) -> HostResult {
        let path = self.need_path(a)?;
        if !self.inside(&path) && !self.scratch(&path) {
            return Err(format!("refusing to write outside the project: {}. Scratch files can go in {}", path.display(), std::env::temp_dir().display()));
        }
        let text = a["text"].as_str().ok_or("missing text")?;
        let existed = path.exists();
        let before = if self.scratch(&path) { None } else { self.baseline(&path) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, text).map_err(|e| format!("writing {}: {e}", self.rel(&path)))?;
        let mut out = json!({"path": self.rel(&path), "lines": text.lines().count(), "created": !existed});
        if let Some(n) = a["replaced"].as_u64() {
            out["replaced"] = json!(n);
        }
        if !self.scratch(&path) {
            self.attach_errors(&path, text, &mut out, before);
        }
        Ok(out)
    }

    fn edit(&mut self, a: &Value) -> HostResult {
        let path = self.need_path(a)?;
        let scratch = self.scratch(&path);
        if !self.inside(&path) && !scratch {
            return Err(format!("refusing to edit outside the project: {}", path.display()));
        }
        let mut text = std::fs::read_to_string(&path).map_err(|_| self.missing_file(&path))?;
        let edits = a["edits"].as_array().ok_or("missing edits")?;
        let mut changed_lines = Vec::new();
        let mut replaced = 0;
        for (i, e) in edits.iter().enumerate() {
            let old = e["old"].as_str().or(e["from"].as_str()).ok_or(format!("edit {} has no old text", i + 1))?;
            let new = e["new"].as_str().or(e["to"].as_str()).unwrap_or("");
            if old.is_empty() {
                return Err(format!("edit {} has empty old text; use $.write to create or replace a whole file", i + 1));
            }
            let all = e["all"].as_bool().unwrap_or(false);
            let positions: Vec<usize> = text.match_indices(old).map(|(p, _)| p).collect();
            let line_of = |pos: usize, t: &str| t[..pos].matches('\n').count() + 1;
            match positions.len() {
                0 => {
                    return Err(format!(
                        "edit {} of {}: the old text was not found in {}. {}Nothing was changed.",
                        i + 1,
                        edits.len(),
                        self.rel(&path),
                        nearest(&text, old)
                    ));
                }
                n if n > 1 && !all => {
                    let lines: Vec<String> = positions.iter().take(10).map(|p| line_of(*p, &text).to_string()).collect();
                    return Err(format!(
                        "edit {} of {}: the old text matches {n} places in {} (lines {}). Add surrounding lines to make it unique, or pass {{old, new, all: true}}. Nothing was changed.",
                        i + 1,
                        edits.len(),
                        self.rel(&path),
                        lines.join(", ")
                    ));
                }
                _ => {}
            }
            for p in &positions {
                changed_lines.push(line_of(*p, &text));
            }
            replaced += positions.len();
            text = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        }
        let before = if scratch { None } else { self.baseline(&path) };
        std::fs::write(&path, &text).map_err(|e| e.to_string())?;
        changed_lines.sort();
        changed_lines.dedup();
        let mut out = json!({"path": self.rel(&path), "replaced": replaced, "lines": changed_lines});
        if !scratch {
            self.attach_errors(&path, &text, &mut out, before);
        }
        Ok(out)
    }

    fn error_lines(items: &[Value]) -> Vec<(u64, String)> {
        items
            .iter()
            .filter(|d| d["severity"].as_u64().unwrap_or(1) == 1)
            .map(|d| (d["range"]["start"]["line"].as_u64().unwrap_or(0) + 1, d["message"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string()))
            .collect()
    }

    fn baseline(&mut self, path: &Path) -> Option<Vec<String>> {
        let lang = Lang::of(path)?;
        if !path.exists() {
            return Some(Vec::new());
        }
        let root = self.root_of(lang, path);
        let server = self.server(lang, &root)?;
        server.open(path).ok()?;
        let items = server.diagnostics(path, None, DIAGNOSTIC_WAIT);
        Some(Self::error_lines(&items).into_iter().map(|(_, m)| m).collect())
    }

    fn attach_errors(&mut self, path: &Path, text: &str, out: &mut Value, before: Option<Vec<String>>) {
        let Some(lang) = Lang::of(path) else { return };
        let root = self.root_of(lang, path);
        let Some(server) = self.server(lang, &root) else { return };
        let Ok(seq) = server.changed(path, text) else { return };
        let items = server.diagnostics(path, Some(seq), DIAGNOSTIC_WAIT);
        let mut old = before.unwrap_or_default();
        let mut fresh = Vec::new();
        for (line, message) in Self::error_lines(&items) {
            match old.iter().position(|m| *m == message) {
                Some(i) => {
                    old.remove(i);
                }
                None => fresh.push(format!("line {line}: {message}")),
            }
        }
        let existing = Self::error_lines(&items).len() - fresh.len();
        fresh.truncate(12);
        out["errors"] = json!(fresh);
        if existing > 0 {
            out["errorsBefore"] = json!(format!("{existing} errors in this file were already there before the change"));
        }
    }

    fn files(&mut self, a: &Value) -> HostResult {
        let base = a["path"].as_str().map(|p| self.resolve(p)).unwrap_or(self.root.clone());
        let glob = a["glob"].as_str().filter(|g| !g.is_empty());
        let matcher = match glob {
            Some(g) => {
                let pattern = if g.contains('/') || g.starts_with("**") {
                    g.to_string()
                } else {
                    format!("**/{g}")
                };
                Some(globset::Glob::new(&pattern).map_err(|e| format!("bad glob {g}: {e}"))?.compile_matcher())
            }
            None => None,
        };
        let max = a["max"].as_u64().unwrap_or(5000) as usize;
        let mut out: Vec<String> = Vec::new();
        let mut total = 0;
        for path in walk(&base) {
            let rel = self.rel(&path);
            if matcher.as_ref().is_none_or(|m| m.is_match(&rel)) {
                total += 1;
                if out.len() < max {
                    out.push(rel);
                }
            }
        }
        out.sort();
        if total > out.len() {
            out.push(format!("… {} more files not listed; narrow the glob or pass {{max}}", total - out.len()));
        }
        Ok(json!(out))
    }

    fn grep(&mut self, a: &Value) -> HostResult {
        let pattern = a["pattern"].as_str().ok_or("missing pattern")?;
        let ignore_case = a["ignoreCase"].as_bool().unwrap_or(false);
        let literal = a["literal"].as_bool().unwrap_or(false);
        let source = if literal { regex::escape(pattern) } else { pattern.to_string() };
        let re = regex::RegexBuilder::new(&source)
            .case_insensitive(ignore_case)
            .build()
            .or_else(|_| regex::RegexBuilder::new(&regex::escape(pattern)).case_insensitive(ignore_case).build())
            .map_err(|e| e.to_string())?;
        let base = a["path"].as_str().map(|p| self.resolve(p)).unwrap_or(self.root.clone());
        let matcher = match a["glob"].as_str() {
            Some(g) => Some(
                globset::Glob::new(&if g.contains('/') { g.to_string() } else { format!("**/{g}") })
                    .map_err(|e| e.to_string())?
                    .compile_matcher(),
            ),
            None => None,
        };
        let max = a["max"].as_u64().map(|m| m as usize).unwrap_or(GREP_DEFAULT_MAX);
        let mut rows = Vec::new();
        let mut total = 0usize;
        let files: Box<dyn Iterator<Item = PathBuf>> = if base.is_file() {
            Box::new(std::iter::once(base.clone()))
        } else {
            Box::new(walk(&base))
        };
        for path in files {
            let rel = self.rel(&path);
            if matcher.as_ref().is_some_and(|m| !m.is_match(&rel)) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if bytes.len() > 4_000_000 || bytes.iter().take(4096).any(|b| *b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    total += 1;
                    if rows.len() < max {
                        let shown: String = line.trim().chars().take(240).collect();
                        rows.push(json!({"file": rel, "line": i + 1, "text": shown}));
                    }
                }
            }
            if self.cancel.load(Ordering::Relaxed) {
                break;
            }
        }
        if total > rows.len() {
            rows.push(json!({"file": "…", "line": 0, "text": format!("{} more matches not shown (of {total}); narrow the pattern or pass {{max}}", total - rows.len())}));
        }
        Ok(json!(rows))
    }

    fn missing_anchors(&mut self, anchors: &[String]) -> Vec<String> {
        if anchors.is_empty() {
            return Vec::new();
        }
        if self.facts.is_none() {
            match crate::facts::Facts::open(&self.root) {
                Ok(f) => self.facts = Some(f),
                Err(_) => return Vec::new(),
            }
        }
        let facts = self.facts.as_mut().unwrap();
        if facts.refresh().is_err() {
            return Vec::new();
        }
        anchors
            .iter()
            .filter(|anchor| {
                let like = json!(format!("%{anchor}%"));
                let found = facts
                    .query("select 1 from symbols where name like ?1 union all select 1 from refs where name like ?1 union all select 1 from files where path like ?1 limit 1", &[like])
                    .ok()
                    .and_then(|v| v.as_array().map(|a| !a.is_empty()))
                    .unwrap_or(true);
                !found
            })
            .cloned()
            .collect()
    }

    fn sql(&mut self, a: &Value) -> HostResult {
        let query = a["query"].as_str().ok_or("$.sql(query, ...params) needs a query")?;
        if self.facts.is_none() {
            self.board.phase("indexing code facts");
            self.facts = Some(crate::facts::Facts::open(&self.root)?);
        }
        let facts = self.facts.as_mut().unwrap();
        facts.refresh()?;
        let params = a["params"].as_array().cloned().unwrap_or_default();
        facts.query(query, &params)
    }

    fn plan_view(&self) -> Value {
        let done = self.plan.iter().filter(|(_, d)| *d).count();
        json!({"done": done, "total": self.plan.len(), "steps": self.plan.iter().map(|(n, d)| json!({"step": n, "done": d})).collect::<Vec<_>>()})
    }

    fn set_plan(&mut self, a: &Value) -> HostResult {
        let steps: Vec<String> = a["steps"].as_array().into_iter().flatten().filter_map(|s| s.as_str().or(s["step"].as_str()).map(str::to_string)).collect();
        if steps.is_empty() {
            return Err("$.plan takes a list of short step names".into());
        }
        let old = std::mem::take(&mut self.plan);
        self.plan = steps.into_iter().map(|s| {
            let done = old.iter().any(|(n, d)| *d && *n == s);
            (s, done)
        }).collect();
        self.board.plan(self.plan.clone());
        Ok(self.plan_view())
    }

    fn mark_done(&mut self, a: &Value) -> HostResult {
        if self.plan.is_empty() {
            return Err("there is no plan yet; call $.plan([...]) first".into());
        }
        let items: Vec<Value> = match &a["which"] {
            Value::Array(v) => v.clone(),
            Value::Null => {
                let next = self.plan.iter().position(|(_, d)| !*d).map(|i| i + 1).unwrap_or(self.plan.len());
                vec![json!(next)]
            }
            v => vec![v.clone()],
        };
        for item in items {
            let index = match &item {
                Value::Number(n) => n.as_u64().map(|n| n as usize).filter(|n| *n >= 1 && *n <= self.plan.len()).map(|n| n - 1),
                Value::String(s) => self.plan.iter().position(|(n, _)| n == s).or_else(|| self.plan.iter().position(|(n, _)| n.to_lowercase().contains(&s.to_lowercase()))),
                _ => None,
            };
            match index {
                Some(i) => self.plan[i].1 = true,
                None => return Err(format!("no step {item} in the plan: {}", self.plan.iter().enumerate().map(|(i, (n, _))| format!("{} {n}", i + 1)).collect::<Vec<_>>().join(", "))),
            }
        }
        self.board.plan(self.plan.clone());
        Ok(self.plan_view())
    }

    fn approve(&mut self, what: &str, reason: &str) -> Result<(), String> {
        if self.yolo.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(approver) = self.approver.as_mut() else {
            return Err(format!("not allowed without approval: {what} ({reason}). Nobody is at the keyboard to approve it; the user can rerun with --yolo. Continue without it, and say so in your answer."));
        };
        self.board.pause();
        let answer = approver(what, reason);
        self.board.resume();
        match answer {
            Approval::Once => Ok(()),
            Approval::Always => {
                self.yolo.store(true, Ordering::SeqCst);
                Ok(())
            }
            Approval::Deny => Err(format!("the user declined: {what} ({reason}). Do not try it another way; continue without it and say so in your answer.")),
        }
    }

    fn remove(&mut self, a: &Value) -> HostResult {
        let raw: Vec<String> = match &a["paths"] {
            Value::Array(items) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Value::String(s) => vec![s.clone()],
            _ => a["path"].as_str().map(|s| vec![s.to_string()]).unwrap_or_default(),
        };
        if raw.is_empty() {
            return Err("pass a path or a list of paths".into());
        }
        let paths: Vec<PathBuf> = raw.iter().map(|r| self.resolve(r)).collect();
        if let Some(outside) = paths.iter().find(|p| (!self.inside(p) && !self.scratch(p)) || **p == self.root) {
            return Err(format!("refusing to delete {}: only files and folders inside the project can be removed", outside.display()));
        }
        let existing: Vec<&PathBuf> = paths.iter().filter(|p| p.exists()).collect();
        if existing.is_empty() {
            return Ok(json!({"removed": [], "missing": raw}));
        }
        let names: Vec<String> = existing.iter().map(|p| if p.is_dir() { format!("{}/", self.rel(p)) } else { self.rel(p) }).collect();
        let shown = if names.len() > 6 { format!("{} and {} more", names[..6].join(", "), names.len() - 6) } else { names.join(", ") };
        if !existing.iter().all(|p| self.scratch(p) && !self.inside(p)) {
            self.approve(&format!("delete {shown}"), "deleting cannot be undone from Stem")?;
        }
        let mut removed = Vec::new();
        for p in existing {
            let result = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
            result.map_err(|e| format!("deleting {}: {e}", self.rel(p)))?;
            removed.push(self.rel(p));
        }
        Ok(json!({"removed": removed}))
    }

    fn sh(&mut self, a: &Value) -> HostResult {
        let original = a["cmd"].as_str().ok_or("missing cmd")?;
        let rewritten = self.project.rewrite(original);
        let cmd = rewritten.clone().unwrap_or_else(|| original.to_string());
        if let Some(reason) = crate::guard::destructive(&cmd) {
            self.approve(&cmd, &reason)?;
        }
        let timeout = Duration::from_secs(a["timeout"].as_u64().unwrap_or(300));
        let cwd = a["cwd"].as_str().map(|c| self.resolve(c)).unwrap_or(self.root.clone());
        let mut out = run_command(&cmd, &cwd, timeout, &self.cancel)?;
        if looks_like_test(&cmd) {
            self.learn.annotate_test(&mut out);
        }
        if let Some(r) = rewritten {
            out["note"] = json!(format!("this repo uses {}: ran `{r}` instead of `{original}`", self.project.runner));
        }
        Ok(out)
    }

    fn test(&mut self, a: &Value) -> HostResult {
        let base = self
            .project
            .command("test")
            .ok_or("no test command found for this project; use $.sh")?
            .to_string();
        let filter = a["filter"].as_str().map(str::trim).filter(|f| !f.is_empty());
        let fan_out = base.contains("--filter") || base.contains("--workspaces") || base.contains("turbo ") || base.contains("nx ") || base.contains("lerna ");
        let paths: Vec<&str> = filter.map(|f| f.split_whitespace().collect()).unwrap_or_default();
        let quoted = paths.iter().map(|p| shell_quote(p)).collect::<Vec<_>>().join(" ");
        let all_paths = !paths.is_empty() && paths.iter().all(|p| self.resolve(p).exists());
        let learned = self.learn.test_command().map(str::to_string);
        let cmd = match filter {
            Some(_) if all_paths && learned.is_some() => learned.unwrap_or_default().replace("<files>", &quoted),
            Some(_) if all_paths && self.project.runner == "bun" => format!("bun test {quoted}"),
            Some(_) if all_paths && self.project.runner == "cargo" => format!("cargo test {}", paths.iter().filter_map(|p| Path::new(p).file_stem().and_then(|s| s.to_str())).map(shell_quote).collect::<Vec<_>>().join(" ")),
            Some(_) if base.starts_with("npm run") || base.starts_with("pnpm run") || base.starts_with("yarn run") => format!("{base} -- {quoted}"),
            Some(_) => format!("{base} {quoted}"),
            None if fan_out => {
                return Err(format!(
                    "this repo's test command runs every package (`{base}`), which takes many minutes. Pass the test files or folders the change affects: $.test(\"path/to/file.test.ts\")"
                ));
            }
            None => base,
        };
        if let Some(reason) = crate::guard::destructive(&cmd) {
            self.approve(&cmd, &reason)?;
        }
        let timeout = Duration::from_secs(a["timeout"].as_u64().unwrap_or(150));
        let mut out = run_command(&cmd, &self.root, timeout, &self.cancel)?;
        let code = out["code"].as_i64().unwrap_or(-1);
        out["passed"] = json!(code == 0);
        out["cmd"] = json!(cmd);
        self.learn.annotate_test(&mut out);
        let all = format!("{}\n{}", out["out"].as_str().unwrap_or(""), out["err"].as_str().unwrap_or(""));
        let tail: Vec<&str> = all.lines().filter(|l| !l.trim().is_empty()).collect();
        out["summary"] = json!(tail[tail.len().saturating_sub(30)..].join("\n"));
        Ok(out)
    }

    fn tests_for(&mut self, a: &Value) -> HostResult {
        let files: Vec<String> = match &a["files"] {
            Value::Array(v) => v.iter().filter_map(|x| x.as_str().or(x["file"].as_str()).map(str::to_string)).collect(),
            Value::String(s) => vec![s.clone()],
            _ => Vec::new(),
        };
        if files.is_empty() {
            return Err("$.testsFor(files) needs a file or a list of files".into());
        }
        let mut found: Vec<String> = Vec::new();
        let is_test = |p: &str| p.contains(".test.") || p.contains(".spec.") || p.contains("/__tests__/") || p.contains("_test.") || p.starts_with("tests/") || p.contains("/tests/");
        for f in &files {
            let path = self.resolve(f);
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(|s| s.split('.').next().unwrap_or(s).to_string()) else { continue };
            let dir = path.parent().map(|d| d.to_path_buf()).unwrap_or_default();
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(&format!("{stem}.")) && is_test(&name) || name.starts_with(&format!("{stem}_test")) || name.starts_with(&format!("test_{stem}")) {
                    found.push(self.rel(&entry.path()));
                }
            }
            let pattern = format!(r#"(from|import|require()s*["'][^"']*{}(.[jt]sx?)?["']|use .*{}|import .*{}"#, regex::escape(&stem), regex::escape(&stem), regex::escape(&stem));
            let hits = self.grep(&json!({"pattern": pattern, "max": 400}))?;
            for h in hits.as_array().into_iter().flatten() {
                if let Some(file) = h["file"].as_str() {
                    if is_test(file) && !found.iter().any(|x| x == file) {
                        found.push(file.to_string());
                    }
                }
            }
        }
        let sources: Vec<String> = files.iter().map(|f| self.rel(&self.resolve(f))).collect();
        let near = |t: &str| -> usize {
            sources
                .iter()
                .map(|s| {
                    let a: Vec<&str> = s.split('/').collect();
                    let b: Vec<&str> = t.split('/').collect();
                    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
                    a.len() + b.len() - 2 * common
                })
                .min()
                .unwrap_or(99)
        };
        found.sort_by_key(|t| (t.contains("/tests/"), near(t), t.clone()));
        found.dedup();
        found.truncate(8);
        Ok(json!(found))
    }

    fn run_script(&mut self, a: &Value) -> HostResult {
        let asked = a["script"].as_str().ok_or("missing script")?;
        let script = match asked {
            "check" | "tsc" | "types" | "type-check" if self.project.command(asked).is_none() => "typecheck",
            "tests" | "spec" if self.project.command(asked).is_none() => "test",
            "fmt" | "prettier" if self.project.command(asked).is_none() => "format",
            other => other,
        };
        let cmd = match self.project.command(script) {
            Some(c) => c.to_string(),
            None if self.project.scripts.iter().any(|s| s == script) => format!("{} run {script}", self.project.runner),
            None => {
                return Err(format!(
                    "no `{script}` command in this project. Known: {}",
                    self.project
                        .commands
                        .iter()
                        .map(|(k, _)| k.as_str())
                        .chain(self.project.scripts.iter().map(|s| s.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        };
        let timeout = Duration::from_secs(a["timeout"].as_u64().unwrap_or(900));
        let mut out = run_command(&cmd, &self.root, timeout, &self.cancel)?;
        out["cmd"] = json!(cmd);
        Ok(out)
    }

    pub fn root_of(&self, lang: Lang, path: &Path) -> PathBuf {
        servers::root_for(lang, path, &self.root)
    }

    fn label(&self, root: &Path, name: &str) -> String {
        let at = self.project.rel_root(root);
        if at == "." { name.to_string() } else { format!("{name} {at}") }
    }

    fn server(&mut self, lang: Lang, root: &Path) -> Option<&mut Server> {
        let key = (lang, root.to_path_buf());
        if !self.lsp_enabled || self.failed.contains_key(&key) {
            return None;
        }
        if self.servers.get(&key).is_some_and(|s| !s.alive()) {
            self.servers.remove(&key);
        }
        if !self.servers.contains_key(&key) {
            self.board.phase(&format!("starting {} intelligence", lang.label()));
            let started = Instant::now();
            let pending = self.label(root, lang.label());
            self.board.server(&pending, ServerState::Starting);
            let result = servers::find(lang, root).and_then(|spec| Server::start(spec, root));
            match result {
                Ok(server) => {
                    self.board.server(&pending, ServerState::Ready);
                    self.board.rename_server(&pending, &self.label(root, &server.name));
                    self.server_events.push(ServerEvent {
                        lang: lang.label(),
                        name: server.name.clone(),
                        source: server.source.clone(),
                        root: self.project.rel_root(root),
                        ms: started.elapsed().as_millis(),
                        error: None,
                    });
                    self.servers.insert(key.clone(), server);
                }
                Err(e) => {
                    self.board.server(&pending, ServerState::Failed);
                    self.server_events.push(ServerEvent {
                        lang: lang.label(),
                        name: String::new(),
                        source: String::new(),
                        root: self.project.rel_root(root),
                        ms: started.elapsed().as_millis(),
                        error: Some(e.clone()),
                    });
                    self.failed.insert(key, e);
                    return None;
                }
            }
        }
        self.servers.get_mut(&key)
    }

    fn server_for(&mut self, lang: Lang, root: &Path) -> Result<&mut Server, String> {
        if !self.lsp_enabled {
            return Err("code intelligence is off (STEM_LSP=off)".into());
        }
        let key = (lang, root.to_path_buf());
        if self.server(lang, root).is_none() {
            return Err(format!("no {} language server for {}: {}", lang.label(), self.project.rel_root(root), self.failed.get(&key).cloned().unwrap_or_default()));
        }
        let server = self.servers.get_mut(&key).unwrap();
        server.wait_ready(Duration::from_secs(90));
        Ok(server)
    }

    fn server_at(&mut self, lang: Lang, path: &Path) -> Result<&mut Server, String> {
        let root = self.root_of(lang, path);
        self.server_for(lang, &root)
    }

    pub fn intelligence(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for (lang, root, files) in self.project.roots.iter().take(12) {
            let key = (*lang, root.clone());
            let state = match (self.servers.get(&key), self.failed.get(&key)) {
                (Some(s), _) => json!({"server": s.name, "source": s.source, "ready_ms": s.start_ms as u64}),
                (None, Some(e)) => json!({"error": e}),
                (None, None) => json!({"state": "starts on first use"}),
            };
            out.push(json!({"language": lang.label(), "root": self.project.rel_root(root), "files": files, "intelligence": state}));
        }
        out
    }

    fn targets(&mut self, a: &Value) -> Result<Vec<(Lang, PathBuf, Value)>, String> {
        if let Some(file) = a["file"].as_str().or(a["path"].as_str()) {
            let path = self.resolve(file);
            let lang = Lang::of(&path).ok_or_else(|| format!("no language server handles {}", self.rel(&path)))?;
            let text = std::fs::read_to_string(&path).map_err(|_| self.missing_file(&path))?;
            let line_no = a["line"].as_u64().unwrap_or(1).max(1) as usize;
            let line = text.lines().nth(line_no - 1).unwrap_or("");
            let char_col = match (a["col"].as_u64(), a["name"].as_str()) {
                (Some(c), _) => c.saturating_sub(1) as usize,
                (None, Some(name)) => find_word(line, name).unwrap_or(0),
                (None, None) => line.chars().take_while(|c| c.is_whitespace()).count(),
            };
            let character = utf16_col(line, char_col);
            return Ok(vec![(lang, path, json!({"line": line_no - 1, "character": character}))]);
        }
        let name = a["name"].as_str().ok_or("pass a symbol name, or {file, line, col}")?;
        let short = name.rsplit(['.', ':']).next().unwrap_or(name);
        let declared = self.grep(&json!({"pattern": format!(r"\b(class|def|function|fn|struct|enum|trait|interface|type|const|let|var|func|module|impl)\s+{}\b", regex::escape(short)), "max": 50}))?;
        let words = self.grep(&json!({"pattern": format!(r"\b{}\b", regex::escape(short)), "max": 200}))?;
        let hits: Vec<(PathBuf, usize, String)> = declared
            .as_array()
            .into_iter()
            .flatten()
            .chain(words.as_array().into_iter().flatten())
            .filter(|h| h["line"].as_u64().is_some_and(|l| l > 0))
            .map(|h| {
                (
                    self.resolve(h["file"].as_str().unwrap_or("")),
                    h["line"].as_u64().unwrap_or(1) as usize - 1,
                    h["text"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let root = self.root.clone();
        let mut spaces: Vec<(Lang, PathBuf, usize)> = Vec::new();
        for (path, _, _) in &hits {
            let Some(lang) = Lang::of(path) else { continue };
            let space = self.root_of(lang, path);
            match spaces.iter_mut().find(|(l, r, _)| *l == lang && *r == space) {
                Some(entry) => entry.2 += 1,
                None => spaces.push((lang, space, 1)),
            }
        }
        spaces.sort_by_key(|x| std::cmp::Reverse(x.2));
        let mut found = Vec::new();
        for (lang, space, _) in spaces {
            let lang_hits: Vec<&(PathBuf, usize, String)> = hits.iter().filter(|(p, _, _)| Lang::of(p) == Some(lang) && servers::root_for(lang, p, &root) == space).collect();
            if lang_hits.is_empty() {
                continue;
            }
            let Ok(server) = self.server_for(lang, &space) else { continue };
            let mut opened = std::collections::HashSet::new();
            for (path, _, _) in &lang_hits {
                if opened.len() >= 4 {
                    break;
                }
                if opened.insert(path.clone()) {
                    let _ = server.open(path);
                }
            }
            let result = server
                .request("workspace/symbol", json!({"query": short}), Duration::from_secs(30))
                .unwrap_or(Value::Null);
            for s in result.as_array().into_iter().flatten() {
                let symbol_name = s["name"].as_str().unwrap_or("");
                let bare = symbol_name.split('(').next().unwrap_or(symbol_name);
                if bare != short {
                    continue;
                }
                if name != short {
                    let wanted = name[..name.len() - short.len()].trim_end_matches(['.', ':']);
                    let wanted = wanted.rsplit(['.', ':']).next().unwrap_or(wanted);
                    let container = s["containerName"].as_str().unwrap_or("");
                    let path = path_of(s["location"]["uri"].as_str().unwrap_or(""));
                    let line = s["location"]["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
                    let inside = if container.is_empty() {
                        enclosing_is(&path, line, wanted)
                    } else {
                        container.rsplit(['.', ':']).next() == Some(wanted) || container.contains(wanted)
                    };
                    if !inside {
                        continue;
                    }
                }
                let Some(uri) = s["location"]["uri"].as_str() else { continue };
                let path = path_of(uri);
                if !path.starts_with(&root) || path.components().any(|c| c.as_os_str() == "node_modules") {
                    continue;
                }
                let range = &s["location"]["range"];
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let start = range["start"]["line"].as_u64().unwrap_or(0) as usize;
                let lines: Vec<&str> = text.lines().collect();
                let line_no = (start..(start + 8).min(lines.len()))
                    .find(|&n| find_word(lines[n], short).is_some())
                    .unwrap_or(start);
                let line = lines.get(line_no).copied().unwrap_or("");
                let col = find_word(line, short)
                    .map(|c| utf16_col(line, c))
                    .unwrap_or(range["start"]["character"].as_u64().unwrap_or(0) as usize);
                found.push((lang, path, json!({"line": line_no, "character": col})));
            }
            if found.is_empty() {
                let mut candidates: Vec<&(PathBuf, usize, String)> =
                    lang_hits.iter().copied().filter(|(_, _, text)| looks_like_declaration(text, short)).collect();
                if candidates.is_empty() {
                    candidates = lang_hits.iter().copied().take(3).collect();
                }
                let mut seen = std::collections::HashSet::new();
                for (path, line, _) in candidates.into_iter().take(12) {
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let line_text = text.lines().nth(*line).unwrap_or("");
                    let (Some(col), Ok(uri)) = (find_word(line_text, short), server.open(path)) else {
                        continue;
                    };
                    let pos = json!({"line": line, "character": utf16_col(line_text, col)});
                    let Ok(def) = server.request(
                        "textDocument/definition",
                        json!({"textDocument": {"uri": uri}, "position": pos}),
                        Duration::from_secs(30),
                    ) else {
                        continue;
                    };
                    let first = if def.is_array() { def[0].clone() } else { def };
                    let Some(target) = first["uri"].as_str().or(first["targetUri"].as_str()).map(path_of) else {
                        continue;
                    };
                    if !target.starts_with(&root) || target.components().any(|c| c.as_os_str() == "node_modules") {
                        continue;
                    }
                    let range = if first["range"].is_object() {
                        first["range"].clone()
                    } else {
                        first["targetSelectionRange"].clone()
                    };
                    let def_line = range["start"]["line"].as_u64().unwrap_or(0) as usize;
                    if !seen.insert((target.clone(), def_line)) {
                        continue;
                    }
                    let def_text = std::fs::read_to_string(&target).unwrap_or_default();
                    let l = def_text.lines().nth(def_line).unwrap_or("");
                    let c = find_word(l, short)
                        .map(|c| utf16_col(l, c))
                        .unwrap_or(range["start"]["character"].as_u64().unwrap_or(0) as usize);
                    found.push((lang, target, json!({"line": def_line, "character": c})));
                }
            }
            if !found.is_empty() {
                break;
            }
        }
        if found.is_empty() {
            let hint: Vec<String> = hits.iter().take(5).map(|(p, l, _)| format!("{}:{}", self.rel(p), l + 1)).collect();
            return Err(if hint.is_empty() {
                format!("no symbol named {name} in this project")
            } else {
                format!(
                    "no declared symbol named {name}; text matches at {}. Pass {{file, line, name}} to ask about one of them.",
                    hint.join(", ")
                )
            });
        }
        found.truncate(8);
        Ok(found)
    }

    fn location_rows(&self, result: &Value) -> Vec<Value> {
        let items: Vec<Value> = match result {
            Value::Array(a) => a.clone(),
            Value::Object(_) => vec![result.clone()],
            _ => vec![],
        };
        let mut rows: Vec<Value> = Vec::new();
        let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for loc in items {
            let uri = loc["uri"].as_str().or(loc["targetUri"].as_str()).unwrap_or("");
            let range = if loc["range"].is_object() {
                &loc["range"]
            } else {
                &loc["targetSelectionRange"]
            };
            let path = path_of(uri);
            let line = range["start"]["line"].as_u64().unwrap_or(0) as usize;
            let lines = cache.entry(path.clone()).or_insert_with(|| {
                std::fs::read_to_string(&path)
                    .map(|t| t.lines().map(str::to_string).collect())
                    .unwrap_or_default()
            });
            let text: String = lines.get(line).map(|l| l.trim().chars().take(240).collect()).unwrap_or_default();
            rows.push(json!({"file": self.rel(&path), "line": line + 1, "text": text}));
        }
        rows
    }

    fn locations(&mut self, method: &str, a: &Value) -> HostResult {
        let targets = self.targets(a)?;
        let mut rows = Vec::new();
        for (lang, path, pos) in targets {
            let server = self.server_at(lang, &path)?;
            let uri = server.open(&path)?;
            let result = server.request(method, json!({"textDocument": {"uri": uri}, "position": pos}), Duration::from_secs(60))?;
            let found = self.location_rows(&result);
            if found.is_empty() && method == "textDocument/definition" {
                let line = pos["line"].as_u64().unwrap_or(0) as usize;
                let text = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| t.lines().nth(line).map(|l| l.trim().chars().take(240).collect::<String>()))
                    .unwrap_or_default();
                rows.push(json!({"file": self.rel(&path), "line": line + 1, "text": text}));
            }
            rows.extend(found);
        }
        dedup(&mut rows);
        Ok(json!(rows))
    }

    fn refs(&mut self, a: &Value) -> HostResult {
        let targets = self.targets(a)?;
        let several = targets.len() > 1;
        let mut rows = Vec::new();
        for (lang, path, pos) in targets {
            let of = format!("{}:{}", self.rel(&path), pos["line"].as_u64().unwrap_or(0) + 1);
            let server = self.server_at(lang, &path)?;
            let uri = server.open(&path)?;
            let result = server.request(
                "textDocument/references",
                json!({"textDocument": {"uri": uri}, "position": pos, "context": {"includeDeclaration": true}}),
                Duration::from_secs(90),
            )?;
            for mut row in self.location_rows(&result) {
                if several {
                    row["of"] = json!(of);
                }
                rows.push(row);
            }
        }
        dedup(&mut rows);
        Ok(json!(rows))
    }

    fn callers(&mut self, a: &Value) -> HostResult {
        let targets = self.targets(a)?;
        let mut raw: Vec<(PathBuf, usize, Value)> = Vec::new();
        for (lang, path, pos) in targets {
            let server = self.server_at(lang, &path)?;
            let uri = server.open(&path)?;
            let items = server.request(
                "textDocument/prepareCallHierarchy",
                json!({"textDocument": {"uri": uri}, "position": pos}),
                Duration::from_secs(60),
            )?;
            for item in items.as_array().into_iter().flatten() {
                let calls = server.request("callHierarchy/incomingCalls", json!({"item": item}), Duration::from_secs(60))?;
                for call in calls.as_array().into_iter().flatten() {
                    let from = &call["from"];
                    let file = path_of(from["uri"].as_str().unwrap_or(""));
                    for range in call["fromRanges"].as_array().into_iter().flatten() {
                        raw.push((file.clone(), range["start"]["line"].as_u64().unwrap_or(0) as usize, from["name"].clone()));
                    }
                }
            }
        }
        let mut rows = Vec::new();
        for (file, line, caller) in raw {
            let text = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| t.lines().nth(line).map(|l| l.trim().chars().take(240).collect::<String>()))
                .unwrap_or_default();
            rows.push(json!({"file": self.rel(&file), "line": line + 1, "text": text, "caller": caller.as_str().map(|c| if Path::new(c).is_absolute() { self.rel(Path::new(c)) } else { c.to_string() })}));
        }
        dedup(&mut rows);
        Ok(json!(rows))
    }

    fn hover(&mut self, a: &Value) -> HostResult {
        let targets = self.targets(a)?;
        let mut out = Vec::new();
        for (lang, path, pos) in targets {
            let rel = self.rel(&path);
            let server = self.server_at(lang, &path)?;
            let uri = server.open(&path)?;
            let result = server.request(
                "textDocument/hover",
                json!({"textDocument": {"uri": uri}, "position": pos}),
                Duration::from_secs(30),
            )?;
            let text = hover_text(&result["contents"]);
            out.push(format!(
                "{rel}:{}: {}",
                pos["line"].as_u64().unwrap_or(0) + 1,
                text.chars().take(2000).collect::<String>()
            ));
        }
        Ok(json!(out.join("\n\n")))
    }

    fn rename(&mut self, a: &Value) -> HostResult {
        let to = a["to"].as_str().ok_or("missing new name")?;
        let targets = self.targets(a)?;
        if targets.len() > 1 {
            let places: Vec<String> = targets
                .iter()
                .map(|(_, p, pos)| format!("{}:{}", self.rel(p), pos["line"].as_u64().unwrap_or(0) + 1))
                .collect();
            return Err(format!(
                "{} names {} different symbols ({}). Pass {{file, line, name}} to pick one. Nothing was changed.",
                a["name"].as_str().unwrap_or("this"),
                targets.len(),
                places.join(", ")
            ));
        }
        let (lang, path, pos) = targets.into_iter().next().unwrap();
        let root = self.root.clone();
        let space = self.root_of(lang, &path);
        let server = self.server_for(lang, &space)?;
        let uri = server.open(&path)?;
        let edit = server.request(
            "textDocument/rename",
            json!({"textDocument": {"uri": uri}, "position": pos, "newName": to}),
            Duration::from_secs(90),
        )?;
        let mut per_file: HashMap<PathBuf, Vec<Value>> = HashMap::new();
        if let Some(changes) = edit["changes"].as_object() {
            for (u, edits) in changes {
                per_file.entry(path_of(u)).or_default().extend(edits.as_array().cloned().unwrap_or_default());
            }
        }
        for change in edit["documentChanges"].as_array().into_iter().flatten() {
            if let Some(u) = change["textDocument"]["uri"].as_str() {
                per_file
                    .entry(path_of(u))
                    .or_default()
                    .extend(change["edits"].as_array().cloned().unwrap_or_default());
            } else if change["kind"].is_string() {
                return Err("this rename would create, move or delete files; Stem does not apply that yet. Nothing was changed.".into());
            }
        }
        if per_file.is_empty() {
            return Err("the language server found nothing to rename at that place. Nothing was changed.".into());
        }
        let outside: Vec<String> = per_file
            .keys()
            .filter(|p| !normalize(p).starts_with(&root))
            .map(|p| p.display().to_string())
            .collect();
        let total: usize = per_file.values().map(|v| v.len()).sum();
        if !outside.is_empty() {
            return Err(format!(
                "refusing: this rename would edit {total} places in {} files, {} of them outside the project (for example {}). It probably renames a library or trait member. Nothing was changed.",
                per_file.len(),
                outside.len(),
                outside.first().cloned().unwrap_or_default()
            ));
        }
        if a["dryRun"].as_bool().unwrap_or(false) {
            let files: Vec<String> = per_file.keys().map(|p| self.rel(p)).collect();
            return Ok(json!({"wouldEdit": total, "files": files}));
        }
        let before = self.baseline(&path);
        let mut files = Vec::new();
        for (file, mut edits) in per_file {
            let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
            edits.sort_by(|x, y| {
                let k = |e: &Value| {
                    (
                        e["range"]["start"]["line"].as_u64().unwrap_or(0),
                        e["range"]["start"]["character"].as_u64().unwrap_or(0),
                    )
                };
                k(y).cmp(&k(x))
            });
            let mut new_text = text.clone();
            for e in &edits {
                let start = offset(&new_text, &e["range"]["start"]);
                let end = offset(&new_text, &e["range"]["end"]);
                if start > end || end > new_text.len() {
                    return Err(format!(
                        "the language server returned an edit outside {}. Nothing more was changed.",
                        self.rel(&file)
                    ));
                }
                new_text.replace_range(start..end, e["newText"].as_str().unwrap_or(""));
            }
            std::fs::write(&file, &new_text).map_err(|e| e.to_string())?;
            if let Some(server) = self.servers.get_mut(&(lang, space.clone())) {
                let _ = server.changed(&file, &new_text);
            }
            files.push(json!({"file": self.rel(&file), "edits": edits.len()}));
        }
        let mut out = json!({"renamed": total, "to": to, "files": files});
        let first = path.clone();
        let text = std::fs::read_to_string(&first).unwrap_or_default();
        self.attach_errors(&first, &text, &mut out, before);
        Ok(out)
    }

    fn symbols(&mut self, a: &Value) -> HostResult {
        let query = a["query"].as_str().unwrap_or("");
        let spaces: Vec<(Lang, PathBuf)> = self.project.roots.iter().take(8).map(|(l, r, _)| (*l, r.clone())).collect();
        let mut rows = Vec::new();
        let mut errors = Vec::new();
        for (lang, space) in spaces {
            let server = match self.server_for(lang, &space) {
                Ok(s) => s,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let Ok(result) = server.request("workspace/symbol", json!({"query": query}), Duration::from_secs(30)) else {
                continue;
            };
            for s in result.as_array().into_iter().flatten().take(200) {
                let path = path_of(s["location"]["uri"].as_str().unwrap_or(""));
                if !path.starts_with(&self.root) || path.components().any(|c| c.as_os_str() == "node_modules") {
                    continue;
                }
                rows.push(json!({
                    "file": self.rel(&path),
                    "line": s["location"]["range"]["start"]["line"].as_u64().unwrap_or(0) + 1,
                    "text": s["name"],
                    "kind": symbol_kind(s["kind"].as_u64().unwrap_or(0)),
                }));
            }
        }
        if rows.is_empty() && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(json!(rows))
    }

    fn outline(&mut self, a: &Value) -> HostResult {
        let path = self.need_path(a)?;
        let lang = Lang::of(&path).ok_or_else(|| format!("no language server handles {}", self.rel(&path)))?;
        let server = self.server_at(lang, &path)?;
        let uri = server.open(&path)?;
        let result = server.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri}}), Duration::from_secs(30))?;
        fn walk_symbols(items: &Value, depth: usize, out: &mut Vec<String>) {
            for s in items.as_array().into_iter().flatten() {
                let range = if s["range"].is_object() { &s["range"] } else { &s["location"]["range"] };
                out.push(format!(
                    "{}{} {} :{}-{}",
                    "  ".repeat(depth),
                    symbol_kind(s["kind"].as_u64().unwrap_or(0)),
                    s["name"].as_str().unwrap_or(""),
                    range["start"]["line"].as_u64().unwrap_or(0) + 1,
                    range["end"]["line"].as_u64().unwrap_or(0) + 1
                ));
                if depth < 2 {
                    walk_symbols(&s["children"], depth + 1, out);
                }
            }
        }
        let mut lines = Vec::new();
        walk_symbols(&result, 0, &mut lines);
        Ok(json!(lines.join("\n")))
    }

    fn diagnostics(&mut self, a: &Value) -> HostResult {
        let row = |file: String, d: &Value| {
            json!({
                "file": file,
                "line": d["range"]["start"]["line"].as_u64().unwrap_or(0) + 1,
                "text": d["message"].as_str().unwrap_or("").lines().next().unwrap_or(""),
                "severity": match d["severity"].as_u64().unwrap_or(1) { 1 => "error", 2 => "warning", 3 => "info", _ => "hint" },
            })
        };
        if let Some(raw) = a["path"].as_str() {
            let path = self.resolve(raw);
            let lang = Lang::of(&path).ok_or_else(|| format!("no language server handles {}", self.rel(&path)))?;
            let rel = self.rel(&path);
            let server = self.server_at(lang, &path)?;
            server.open(&path)?;
            let items = server.diagnostics(&path, None, Duration::from_secs(10));
            return Ok(json!(items.iter().map(|d| row(rel.clone(), d)).collect::<Vec<_>>()));
        }
        let mut rows = Vec::new();
        for server in self.servers.values() {
            for (file, items) in server.all_diagnostics() {
                let rel = file
                    .strip_prefix(server.root())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| file.display().to_string());
                rows.extend(items.iter().map(|d| row(rel.clone(), d)));
            }
        }
        Ok(json!(rows))
    }

    fn same_question(&mut self, prompt: &str) -> (Option<crate::learn::Answer>, Option<PendingJudge>) {
        let candidates = self.learn.answer_candidates(prompt);
        let Some((sure, top)) = candidates.first().cloned() else {
            return (None, None);
        };
        if sure {
            self.learn.events.push(("learn_used".into(), json!({"what": "answer_sure"})));
            return (Some(top), None);
        }
        let Some(client) = self.ask_client.as_ref() else {
            return (None, None);
        };
        let list: Vec<String> = candidates
            .iter()
            .enumerate()
            .map(|(i, (_, c))| format!("{}. Asked: {}\nAnswer: {}", i + 1, c.request.trim(), c.answer.trim()))
            .collect();
        let judge = format!(
            "New question:\n{}\n\nEarlier questions in this project, each with the answer it got (the code they cite is unchanged since):\n{}\n\nDoes one earlier answer fully answer the new question, naming the code the new question asks about? Reply with its number, or 0 if none does. Reply with the number only.",
            prompt.chars().take(1500).collect::<String>(),
            list.join("\n\n")
        );
        (None, Some(PendingJudge { candidates: candidates.into_iter().map(|(_, a)| a).collect(), prompt: judge, resolved: client.resolved.clone() }))
    }

    pub fn add_ask_usage(&mut self, usage: Usage, cost: f64) {
        self.ask_usage.add(usage);
        self.ask_cost += cost;
    }

    fn ask(&mut self, a: &Value) -> HostResult {
        let client = self.ask_client.clone().ok_or("no small model is available for $.ask with this provider")?;
        let prompt = a["prompt"].as_str().unwrap_or("");
        let content = match a["input"].as_str() {
            Some(input) => format!("{prompt}\n\n<input>\n{input}\n</input>"),
            None => prompt.to_string(),
        };
        let reply = client.complete(
            "Answer exactly what is asked, briefly. If asked for JSON, reply with JSON only.",
            &[Msg {
                role: Role::User,
                blocks: vec![Block::Text(content)],
            }],
            &[],
            &self.cancel,
        )?;
        self.ask_usage.add(reply.usage);
        self.ask_cost += crate::agent::cost_of(&client.resolved.model.cost, reply.usage);
        let text: String = reply
            .blocks
            .iter()
            .filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None })
            .collect();
        if reply.stop == crate::llm::Stop::MaxTokens {
            return Err(format!("the small model's answer was cut off at {} tokens; ask for less", client.max_output));
        }
        if text.trim().is_empty() {
            return Err("the small model returned an empty answer".into());
        }
        Ok(json!(text))
    }
}

fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c)) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

pub fn run_command(cmd: &str, cwd: &Path, timeout: Duration, cancel: &Arc<AtomicBool>) -> HostResult {
    let started = Instant::now();
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    let mut child = command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CI", "1")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .spawn()
        .map_err(|e| format!("cannot run `{cmd}`: {e}"))?;
    let collect = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 65536];
            let mut total = 0usize;
            while let Ok(n) = r.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                total += n;
                if buf.len() < SH_OUTPUT_LIMIT {
                    buf.extend_from_slice(&chunk[..n.min(SH_OUTPUT_LIMIT - buf.len())]);
                }
            }
            (buf, total)
        })
    };
    let out_handle = collect(Box::new(child.stdout.take().unwrap()));
    let err_handle = collect(Box::new(child.stderr.take().unwrap()));
    let mut killed = None;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if cancel.load(Ordering::Relaxed) {
            killed = Some("cancelled");
        } else if started.elapsed() > timeout {
            killed = Some("timed out");
        }
        if killed.is_some() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (out, out_total) = out_handle.join().unwrap_or_default();
    let (err, err_total) = err_handle.join().unwrap_or_default();
    let text = |buf: Vec<u8>, total: usize| {
        let mut s = strip_ansi(&String::from_utf8_lossy(&buf));
        if total > buf.len() {
            s.push_str(&format!("\n[cut: kept the first {} of {} bytes]", buf.len(), total));
        }
        s
    };
    let mut result = json!({
        "code": status.and_then(|s| s.code()).unwrap_or(-1),
        "out": text(out, out_total),
        "err": text(err, err_total),
        "ms": started.elapsed().as_millis() as u64,
    });
    if let Some(reason) = killed {
        result["killed"] = json!(format!("{reason} after {} s", started.elapsed().as_secs()));
    }
    Ok(result)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub fn walk(base: &Path) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(base)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .require_git(false)
        .filter_entry(|e| !matches!(e.file_name().to_str(), Some(".git" | "node_modules" | ".stem")))
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn nearest(text: &str, old: &str) -> String {
    let first = old.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if first.is_empty() {
        return String::new();
    }
    let words: Vec<&str> = first.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| w.len() > 2).collect();
    let best = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i, l, words.iter().filter(|w| l.contains(*w)).count()))
        .filter(|(_, _, score)| *score > 0)
        .max_by_key(|(_, _, score)| *score);
    match best {
        Some((i, line, _)) => {
            let lines: Vec<&str> = text.lines().collect();
            let from = i.saturating_sub(1);
            let to = (i + 3).min(lines.len());
            let snippet: Vec<String> = (from..to).map(|n| format!("{:>5}| {}", n + 1, lines[n])).collect();
            let _ = line;
            format!("Closest text in the file:\n{}\n", snippet.join("\n"))
        }
        None => String::new(),
    }
}

fn enclosing_is(path: &Path, line: usize, container: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let lines: Vec<&str> = text.lines().collect();
    let Some(own) = lines.get(line) else { return false };
    let mut indent = own.len() - own.trim_start().len();
    for l in lines[..line].iter().rev() {
        if l.trim().is_empty() {
            continue;
        }
        let i = l.len() - l.trim_start().len();
        if i < indent {
            if find_word(l, container).is_some()
                && ["class", "impl", "struct", "trait", "interface", "object", "namespace", "module", "enum"]
                    .iter()
                    .any(|k| l.contains(k))
            {
                return true;
            }
            indent = i;
            if i == 0 {
                return false;
            }
        }
    }
    false
}

fn looks_like_declaration(line: &str, name: &str) -> bool {
    let t = line.trim_start();
    [
        "fn ",
        "pub fn ",
        "function ",
        "export function ",
        "export const ",
        "const ",
        "let ",
        "class ",
        "export class ",
        "interface ",
        "export interface ",
        "type ",
        "export type ",
        "def ",
        "async def ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "impl ",
        "func ",
        "export default function ",
    ]
    .iter()
    .any(|k| t.starts_with(k) && t[k.len()..].trim_start().starts_with(name))
}

fn find_word(line: &str, word: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let target: Vec<char> = word.chars().collect();
    if target.is_empty() {
        return None;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    (0..chars.len().saturating_sub(target.len() - 1))
        .find(|&i| chars[i..i + target.len()] == target[..] && (i == 0 || !is_ident(chars[i - 1])) && chars.get(i + target.len()).is_none_or(|c| !is_ident(*c)))
}

fn utf16_col(line: &str, char_col: usize) -> usize {
    line.chars().take(char_col).map(|c| c.len_utf16()).sum()
}

fn offset(text: &str, pos: &Value) -> usize {
    let line = pos["line"].as_u64().unwrap_or(0) as usize;
    let character = pos["character"].as_u64().unwrap_or(0) as usize;
    let mut start = 0;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == line {
            let mut units = 0;
            for (b, c) in l.char_indices() {
                if units >= character {
                    return start + b;
                }
                units += c.len_utf16();
            }
            return start + l.trim_end_matches('\n').len();
        }
        start += l.len();
    }
    text.len()
}

fn hover_text(contents: &Value) -> String {
    match contents {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(hover_text).collect::<Vec<_>>().join("\n"),
        Value::Object(o) => o.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    }
    .replace("```typescript\n", "")
    .replace("```rust\n", "")
    .replace("```python\n", "")
    .replace("```\n", "")
    .replace("```", "")
    .trim()
    .to_string()
}

fn symbol_kind(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        22 => "member",
        23 => "struct",
        26 => "type",
        _ => "symbol",
    }
}

fn dedup(rows: &mut Vec<Value>) {
    let mut seen = std::collections::HashSet::new();
    rows.retain(|r| seen.insert(format!("{}:{}:{}", r["file"], r["line"], r["text"])));
}

fn looks_like_test(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    ["vitest", "jest", "bun test", "pytest", "cargo test", "go test", "npm test", "npm run test", "bun run test", "pnpm test", "yarn test", "mocha", "ava"].iter().any(|k| c.contains(k))
}
