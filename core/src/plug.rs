use crate::hook::{Reported, begin_with, cap, end_with};
use crate::learn::Learn;
use crate::rules::{Outcome, Rules};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub const PROTOCOL: u64 = 1;
const DEFAULT_DECIDE_MS: u64 = 2_500;
const METHODS: &[&str] = &["hello", "turn.start", "tool.result", "rule.check", "turn.end", "feedback", "status", "shutdown"];

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

struct Server {
    root: PathBuf,
    link: Rc<RefCell<Link>>,
    decide_ms: Option<u64>,
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
}

fn from_user(params: &Value) -> bool {
    params["origin"].as_str().is_none_or(|o| o == "user")
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
                Ok(json!({"protocol": PROTOCOL, "stem": env!("CARGO_PKG_VERSION"), "root": self.root, "methods": METHODS, "callbacks": if self.decide_ms.is_some() { json!(["decide"]) } else { json!([]) }}))
            }
            "turn.start" => {
                let session = Self::session(params)?;
                let prompt = params["prompt"].as_str().unwrap_or("").trim();
                if prompt.is_empty() || !from_user(params) {
                    return Ok(json!({"note": null, "skipped": if prompt.is_empty() { "empty prompt" } else { "only the user's own requests start or steer a task" }}));
                }
                crate::events::ensure_ignored(&self.root.join(".stem"));
                let note = begin_with(&self.root, &session, prompt, self.decider()).map(|n| cap(&n));
                Ok(json!({"note": note}))
            }
            "tool.result" => {
                let call = params["call"].as_str().unwrap_or("");
                let command = params["args"]["cmd"].as_str().unwrap_or("");
                if call != "test" && !(call == "sh" && crate::host::looks_like_test(command)) {
                    return Ok(json!({}));
                }
                let mut out = match &params["result"] {
                    Value::String(s) => json!({"out": s}),
                    Value::Object(_) => params["result"].clone(),
                    _ => json!({}),
                };
                let before: Vec<String> = out.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                Learn::open(&self.root).annotate_test(&mut out);
                let added: serde_json::Map<String, Value> = out.as_object().map(|o| o.iter().filter(|(k, _)| !before.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
                Ok(Value::Object(added))
            }
            "rule.check" => {
                let call = params["call"].as_str().ok_or("missing call")?;
                let (outcome, rule) = Rules::load(&self.root).apply(call, &params["args"]);
                Ok(match outcome {
                    Outcome::Run { name, args, note } => json!({"action": if rule.is_some() { "redirect" } else { "run" }, "call": name, "args": args, "note": note, "rule": rule}),
                    Outcome::Deny(reason) => json!({"action": "deny", "reason": reason, "rule": rule}),
                })
            }
            "turn.end" => {
                let session = Self::session(params)?;
                if !from_user(params) {
                    return Ok(json!({"skipped": "only the user's own requests are learned from"}));
                }
                let edits = params["edits"].as_array().map(|a| {
                    a.iter().filter_map(|e| Some((e["file"].as_str()?.to_string(), e["before"].as_str().unwrap_or("").to_string(), e["after"].as_str().unwrap_or("").to_string()))).collect()
                });
                let reported = Reported { edits, touched: strings(&params["touched"]), tests: strings(&params["tests"]).unwrap_or_default() };
                let answer = params["answer"].as_str().unwrap_or("");
                Ok(end_with(&self.root, &session, answer, params["finished"].as_bool().unwrap_or(true), reported))
            }
            "feedback" => Ok(Learn::open(&self.root).add_feedback(params["text"].as_str().unwrap_or(""))),
            "status" => Ok(Learn::open(&self.root).summary()),
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
    let mut server = Server { root: root.to_path_buf(), link: link.clone(), decide_ms: None };
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
