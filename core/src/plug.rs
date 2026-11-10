use crate::hook::{Begin, Reported, begin_with, cap, continue_task, end_with, usage_only};
use crate::learn::Learn;
use crate::rules::{Outcome, Rules};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub const PROTOCOL: u64 = 1;
const DEFAULT_DECIDE_MS: u64 = 2_500;
const METHODS: &[&str] = &["hello", "learn.import", "turn.start", "tool.result", "rule.check", "turn.end", "feedback", "status", "shutdown"];

fn send(message: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

struct Link {
    lines: Receiver<String>,
    queued: VecDeque<String>,
    next: u64,
}

impl Link {
    fn take(&mut self) -> Option<String> {
        self.queued.pop_front().or_else(|| self.lines.recv().ok())
    }

    fn call(&mut self, method: &str, params: Value, wait: Duration) -> Option<Value> {
        self.next += 1;
        let id = format!("stem-{}", self.next);
        send(&json!({"id": id, "method": method, "params": params}));
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let line = self.lines.recv_timeout(left).ok()?;
            let reply = serde_json::from_str::<Value>(&line).ok().filter(|v| v["id"].as_str() == Some(id.as_str()) && v.get("method").is_none());
            match reply {
                Some(v) => return v.get("result").cloned(),
                None => self.queued.push_back(line),
            }
        }
    }
}

#[derive(Default)]
struct Seen {
    edited: bool,
    checks: Vec<(String, Option<bool>)>,
    told: HashSet<String>,
    kinds: Vec<String>,
    offered: Vec<String>,
    undone_told: HashSet<String>,
    noted_regress: Vec<(String, String, String)>,
}

impl Seen {
    fn fired(&mut self, kind: &str) {
        if !self.kinds.iter().any(|k| k == kind) {
            self.kinds.push(kind.to_string());
        }
    }
}

struct Server {
    root: PathBuf,
    link: Rc<RefCell<Link>>,
    decide_ms: Option<u64>,
    reports_edits: bool,
    seen: HashMap<String, Seen>,
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
}

const ORIGIN_USER: &str = "user";
const ORIGIN_ENGINE: &str = "engine";

fn origin(params: &Value) -> &str {
    params["origin"].as_str().unwrap_or(ORIGIN_USER)
}

impl Server {
    fn session(params: &Value) -> Result<String, String> {
        params["session"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_string).ok_or_else(|| "missing session".into())
    }

    fn decider(&self) -> Option<crate::learn::Decider> {
        let wait = Duration::from_millis(self.decide_ms?);
        let link = self.link.clone();
        Some(Box::new(move |asked: &Value| link.borrow_mut().call("decide", asked.clone(), wait)))
    }

    fn handle(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "hello" => {
                let offered = strings(&params["capabilities"]).unwrap_or_default();
                self.decide_ms = offered.iter().any(|c| c == "decide").then(|| params["decideMs"].as_u64().unwrap_or(DEFAULT_DECIDE_MS));
                self.reports_edits = offered.iter().any(|c| c == "edits");
                Ok(json!({"protocol": PROTOCOL, "stem": env!("CARGO_PKG_VERSION"), "root": self.root, "methods": METHODS, "callbacks": if self.decide_ms.is_some() { json!(["decide"]) } else { json!([]) }}))
            }
            "turn.start" => {
                let session = Self::session(params)?;
                let prompt = params["prompt"].as_str().unwrap_or("").trim();
                if prompt.is_empty() {
                    return Ok(json!({"note": null, "kinds": [], "skipped": "empty prompt"}));
                }
                crate::events::ensure_ignored(&self.root.join(".stem"));
                if origin(params) != ORIGIN_USER {
                    let continues = origin(params) == ORIGIN_ENGINE && continue_task(&self.root, &session, self.reports_edits);
                    if continues {
                        self.seen.insert(session.clone(), Seen::default());
                        return Ok(json!({"note": null, "kinds": [], "continues": true}));
                    }
                    return Ok(json!({"note": null, "kinds": [], "skipped": "only the user's own requests start or steer a task"}));
                }
                let begin = Begin { decide: self.decider(), without_snapshot: self.reports_edits, task: params["task"].as_str().filter(|t| !t.trim().is_empty()) };
                let started = begin_with(&self.root, &session, prompt, begin);
                let mut seen = Seen::default();
                seen.undone_told.extend(started.noted_regress.iter().map(|(file, _, _)| file.clone()));
                self.seen.insert(session.clone(), seen);
                let mut result = json!({"note": started.note.map(|n| cap(&n)), "kinds": started.kinds});
                if let Some(answer) = started.answer {
                    result["answer"] = answer;
                }
                if let Some(familiarity) = started.familiarity {
                    result["familiarity"] = familiarity;
                }
                Ok(result)
            }
            "tool.result" => {
                let call = params["call"].as_str().unwrap_or("");
                let command = params["args"]["cmd"].as_str().unwrap_or("");
                if let Some(seen) = params["session"].as_str().and_then(|s| self.seen.get_mut(s)) {
                    match call {
                        "edit" | "write" => seen.edited = true,
                        "sh" if seen.edited && !command.is_empty() => seen.checks.push((command.to_string(), params["result"]["code"].as_i64().map(|c| c == 0))),
                        _ => {}
                    }
                }
                if call != "test" && !(call == "sh" && crate::host::looks_like_test(command)) {
                    return Ok(json!({}));
                }
                let mut out = match &params["result"] {
                    Value::String(s) => json!({"out": s}),
                    Value::Object(_) => params["result"].clone(),
                    _ => json!({}),
                };
                if params["clean"].as_bool() == Some(true) {
                    let command = params["args"]["cmd"].as_str().or(params["args"]["filter"].as_str()).unwrap_or("");
                    let text = format!("{}\n{}", out["out"].as_str().unwrap_or(""), out["err"].as_str().unwrap_or(""));
                    return Ok(Learn::open(&self.root).saw_clean_run(command, &text));
                }
                let before: Vec<String> = out.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                Learn::open(&self.root).annotate_test(&mut out);
                let added: serde_json::Map<String, Value> = out.as_object().map(|o| o.iter().filter(|(k, _)| !before.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
                if added.contains_key("knownFailures") {
                    if let Some(seen) = params["session"].as_str().and_then(|s| self.seen.get_mut(s)) {
                        seen.fired("known");
                    }
                }
                Ok(Value::Object(added))
            }
            "rule.check" => {
                let call = params["call"].as_str().ok_or("missing call")?;
                let (outcome, rule) = Rules::load(&self.root).apply(call, &params["args"]);
                let mut learned: Vec<String> = Vec::new();
                if let (Outcome::Run { name, args, .. }, Some(seen)) = (&outcome, params["session"].as_str().and_then(|s| self.seen.get_mut(s))) {
                    if let Some(path) = args["path"].as_str().filter(|_| matches!(name.as_str(), "read" | "edit" | "write")) {
                        let mut learn = Learn::open(&self.root);
                        learn.regress_told = std::mem::take(&mut seen.undone_told);
                        if let Some(note) = learn.undone_at(path).filter(|_| name != "write") {
                            seen.fired("regress");
                            seen.noted_regress.extend(learn.noted_regress.iter().cloned());
                            learned.push(note);
                        }
                        if name != "read" {
                            if let Some(note) = learn.lessons_at(path) {
                                seen.fired("lessons");
                                learned.push(note);
                            }
                        }
                        seen.undone_told = std::mem::take(&mut learn.regress_told);
                        if name != "read" {
                            seen.edited = true;
                            learn.habits_told = std::mem::take(&mut seen.told);
                            if let Some(note) = learn.habits_for(path) {
                                seen.fired("habits");
                                seen.offered.extend(learn.habits_offered.iter().cloned());
                                learned.push(note);
                            }
                            seen.told = learn.habits_told;
                        }
                    }
                }
                Ok(match outcome {
                    Outcome::Run { name, args, note } => {
                        let notes: Vec<String> = note.into_iter().chain(learned).collect();
                        json!({"action": if rule.is_some() { "redirect" } else { "run" }, "call": name, "args": args, "note": (!notes.is_empty()).then(|| notes.join("\n")), "rule": rule})
                    }
                    Outcome::Deny(reason) => json!({"action": "deny", "reason": reason, "rule": rule}),
                })
            }
            "turn.end" => {
                let session = Self::session(params)?;
                let from = origin(params);
                let finished = params["finished"].as_bool().unwrap_or(true);
                let seen = self.seen.remove(&session).unwrap_or_default();
                let unlearned = |root: &Path| json!({"skipped": "only the user's own requests and the app's continuations of them are learned from", "usageStored": usage_only(root, &session, from, finished, params["shown"].as_bool().unwrap_or(true), &params["usage"])});
                if from != ORIGIN_USER && from != ORIGIN_ENGINE {
                    return Ok(unlearned(&self.root));
                }
                let edits: Option<Vec<(String, String, String)>> = params["edits"].as_array().map(|a| {
                    a.iter().filter_map(|e| Some((e["file"].as_str()?.to_string(), e["before"].as_str().unwrap_or("").to_string(), e["after"].as_str().unwrap_or("").to_string()))).collect()
                });
                let touched = strings(&params["touched"]);
                let reported = Reported {
                    edits: if self.reports_edits { Some(edits.unwrap_or_default()) } else { edits },
                    touched: if self.reports_edits { Some(touched.unwrap_or_default()) } else { touched },
                    tests: strings(&params["tests"]).unwrap_or_default(),
                    usage: params["usage"].clone(),
                    checks: seen.checks,
                    kinds: seen.kinds,
                    offered_checks: seen.offered,
                    noted_regress: seen.noted_regress,
                    shown: params["shown"].as_bool(),
                    origin: Some(from.to_string()),
                };
                let ended = end_with(&self.root, &session, params["answer"].as_str().unwrap_or(""), finished, reported);
                Ok(if ended.get("skipped").is_some() && from == ORIGIN_ENGINE { unlearned(&self.root) } else { ended })
            }
            "learn.import" => {
                let format = params["format"].as_str().unwrap_or("");
                if format != "empryo-stem-1" {
                    return Err(format!("unknown format {format}; this Stem imports: empryo-stem-1"));
                }
                let path = params["path"].as_str().ok_or("missing path")?;
                Learn::open(&self.root).import_empryo(Path::new(path))
            }
            "feedback" => Ok(Learn::open(&self.root).add_feedback(params["text"].as_str().unwrap_or(""))),
            "status" => {
                let mut summary = Learn::open(&self.root).summary();
                summary["usage"] = crate::usage::summary(&self.root);
                Ok(summary)
            }
            other => Err(format!("unknown method {other}; this Stem speaks: {}", METHODS.join(", "))),
        }
    }
}

pub fn run(root: &Path) -> i32 {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let link = Rc::new(RefCell::new(Link { lines: rx, queued: VecDeque::new(), next: 0 }));
    let mut server = Server { root: root.to_path_buf(), link: link.clone(), decide_ms: None, reports_edits: false, seen: HashMap::new() };
    loop {
        let Some(line) = link.borrow_mut().take() else { return 0 };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(&json!({"id": null, "error": {"code": "parse", "message": e.to_string()}}));
                continue;
            }
        };
        let Some(method) = message["method"].as_str() else { continue };
        let id = message["id"].clone();
        if method == "shutdown" {
            send(&json!({"id": id, "result": {}}));
            return 0;
        }
        let reply = match server.handle(method, &message["params"]) {
            Ok(result) => json!({"id": id, "result": result}),
            Err(e) => json!({"id": id, "error": {"code": "failed", "message": e}}),
        };
        if !id.is_null() {
            send(&reply);
        }
    }
}
