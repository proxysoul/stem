use crate::catalog::Cost;
use crate::events::Log;
use crate::host::{Approver, Host};
use crate::llm::{Block, Client, Msg, Role, Stop, Tool, Usage};
use crate::sandbox::Sandbox;
use crate::ui::Board;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const RESULT_LIMIT: usize = 24_000;
const MAX_STEPS: usize = 120;

pub fn cache_percent(u: Usage) -> u64 {
    let total = u.input + u.cache_read + u.cache_write;
    if total == 0 { 0 } else { u.cache_read * 100 / total }
}

pub fn cost_of(cost: &Cost, u: Usage) -> f64 {
    (u.input as f64 * cost.input + u.output as f64 * cost.output + u.cache_read as f64 * cost.cache_read + u.cache_write as f64 * cost.cache_write)
        / 1_000_000.0
}

pub struct Engine {
    pub host: Rc<RefCell<Host>>,
    sandbox: Sandbox,
    pub log: Log,
    pub board: Board,
}

pub struct Session {
    pub client: Rc<Client>,
    pub engine: Engine,
    pub msgs: Vec<Msg>,
    system: String,
    pub cancel: Arc<AtomicBool>,
    pub usage: Usage,
    pub cost: f64,
    tools: Vec<Tool>,
    last_context: u64,
    pub cache_at: Option<Instant>,
    steps: Vec<Option<u64>>,
    next_step: u64,
    leaving: Vec<(Vec<u64>, Option<String>)>,
    ejected: Vec<(u64, String)>,
    warned: u8,
    summarizer: Rc<Client>,
}

pub struct TurnReport {
    pub answer: String,
    pub scripts: usize,
    pub usage: Usage,
    pub cost: f64,
    pub elapsed: Duration,
    pub error: Option<String>,
}

impl Engine {
    pub fn step_flags(&self) -> (bool, bool) {
        let host = self.host.borrow();
        let scratch = |t: &Option<String>| t.as_deref().is_some_and(|p| p.starts_with("/tmp") || p.starts_with("/private/tmp") || p.starts_with(&*std::env::temp_dir().to_string_lossy()));
        let changed = host.calls.iter().any(|c| c.ok && matches!(c.name.as_str(), "edit" | "write" | "rename" | "remove") && !scratch(&c.target));
        let ran = host.calls.iter().any(|c| matches!(c.name.as_str(), "test" | "run" | "sh"));
        (changed, ran)
    }

    pub fn ran_repro(&self) -> bool {
        self.host.borrow().calls.iter().any(|c| c.name == "sh" && c.target.as_deref().is_some_and(|t| t.contains("/tmp/") || t.contains("bun -e") || t.contains("node -e") || t.contains("python -c") || t.contains("repro")))
    }

    pub fn ran_last(&self) -> bool {
        let host = self.host.borrow();
        let scratch = |t: &Option<String>| t.as_deref().is_some_and(|p| p.starts_with("/tmp") || p.starts_with("/private/tmp"));
        let last_change = host.calls.iter().rposition(|c| c.ok && matches!(c.name.as_str(), "edit" | "write" | "rename" | "remove") && !scratch(&c.target));
        let last_run = host.calls.iter().rposition(|c| matches!(c.name.as_str(), "test" | "run" | "sh"));
        match (last_change, last_run) {
            (Some(c), Some(r)) => r > c,
            (None, Some(_)) => true,
            _ => false,
        }
    }

    pub fn new(root: std::path::PathBuf, cancel: Arc<AtomicBool>, ask: Option<Client>, board: Board, yolo: Arc<AtomicBool>, approver: Option<Approver>) -> Result<Engine, String> {
        let host = Rc::new(RefCell::new(Host::new(root.clone(), board.clone(), cancel.clone(), ask.map(Rc::new), yolo, approver)));
        let bridge = host.clone();
        let sandbox = Sandbox::new(move |name, args| bridge.borrow_mut().dispatch(&name, &args), cancel)?;
        Ok(Engine {
            host,
            sandbox,
            log: Log::open(&root),
            board,
        })
    }
    pub fn run_script(&mut self, code: &str) -> (String, bool) {
        self.board.script();
        let started = Instant::now();
        {
            let mut h = self.host.borrow_mut();
            h.calls.clear();
            h.touched = 0;
        }
        let limit = Duration::from_secs(std::env::var("STEM_SCRIPT_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(1800));
        let outcome = self.sandbox.run(code, limit);
        let (calls, touched) = {
            let h = self.host.borrow();
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for c in &h.calls {
                *counts.entry(c.name.clone()).or_default() += 1;
            }
            (counts, h.touched)
        };
        let steps: Vec<String> = self
            .host
            .borrow()
            .calls
            .iter()
            .map(|c| {
                format!(
                    "{}{} {}",
                    c.name,
                    c.target.as_ref().map(|t| format!("({t})")).unwrap_or_default(),
                    if c.ok { "ok" } else { "failed" }
                )
            })
            .collect();
        let mut text = if outcome.ok {
            match &outcome.value {
                Value::String(s) => s.clone(),
                Value::Null => "null".to_string(),
                v => serde_json::to_string_pretty(v).unwrap_or_default(),
            }
        } else {
            let mut t = String::from("Error");
            if let Some(line) = outcome.line {
                t.push_str(&format!(" at line {line}"));
            }
            if let Some(call) = &outcome.failed_call {
                t.push_str(&format!(" in $.{call}"));
            }
            t.push_str(&format!(": {}", outcome.error.clone().unwrap_or_default()));
            let changes: Vec<&String> = steps
                .iter()
                .filter(|s| (s.starts_with("edit") || s.starts_with("write") || s.starts_with("rename")) && s.ends_with("ok"))
                .collect();
            if !steps.is_empty() {
                t.push_str(&format!("\nSteps that ran: {}", steps.join(", ")));
            }
            if !changes.is_empty() {
                t.push_str("\nChanges made before the error were kept.");
            }
            t
        };
        if !outcome.log.is_empty() {
            text.push_str(&format!("\n[console]\n{}", outcome.log.join("\n")));
        }
        let full = text.len();
        if full > RESULT_LIMIT {
            let mut cut = RESULT_LIMIT;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str(&format!(
                "\n[cut: showing {RESULT_LIMIT} of {full} characters. Return a smaller summary, or slice the data in the script.]"
            ));
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        code.hash(&mut hasher);
        self.log.event(
            "script",
            json!({"hash": format!("{:016x}", hasher.finish()), "lines": code.lines().count(), "ok": outcome.ok, "ms": started.elapsed().as_millis() as u64,
                   "calls": calls, "touched_bytes": touched, "return_bytes": full, "error": outcome.error, "timed_out": outcome.timed_out}),
        );
        if let Some(hint) = self.host.borrow_mut().growth.observe(code) {
            text.push_str(&format!("\n{hint}"));
        }
        let grown: Vec<(String, Value)> = self.host.borrow_mut().growth.events.drain(..).collect();
        for (kind, fields) in grown {
            self.log.event(&kind, fields);
        }
        let servers: Vec<Value> = self
            .host
            .borrow_mut()
            .server_events
            .drain(..)
            .map(|e| json!({"lang": e.lang, "root": e.root, "server": e.name, "source": e.source, "ms": e.ms as u64, "error": e.error}))
            .collect();
        for e in servers {
            self.log.event("server", e);
        }
        (text, !outcome.ok)
    }
}

fn imperative(prompt: &str) -> bool {
    let first = prompt.trim().trim_start_matches(|c: char| !c.is_alphanumeric()).split_whitespace().find(|w| !matches!(w.to_lowercase().as_str(), "please" | "can" | "you" | "could" | "now"));
    first.is_some_and(|w| {
        matches!(
            w.to_lowercase().trim_end_matches(',').as_ref(),
            "fix" | "add" | "implement" | "make" | "rename" | "remove" | "delete" | "update" | "change" | "refactor" | "write" | "create" | "move" | "replace" | "stop" | "support" | "handle" | "ensure" | "migrate" | "port" | "build"
        )
    })
}

impl Session {
    pub fn new(client: Client, ask: Option<Client>, root: std::path::PathBuf, cancel: Arc<AtomicBool>, yolo: Arc<AtomicBool>, approver: Option<Approver>) -> Result<Session, String> {
        let client = Rc::new(client);
        let summarizer = match std::env::var("STEM_SUMMARIZER").ok().filter(|s| !s.is_empty()) {
            Some(spec) => crate::catalog::Catalog::load().and_then(|c| c.resolve(Some(&spec))).map(|r| Rc::new(Client::new(r))).unwrap_or_else(|_| client.clone()),
            None => client.clone(),
        };
        let mut engine = Engine::new(root.clone(), cancel.clone(), ask, Board::new(), yolo, approver)?;
        let system = system_prompt(&engine.host.borrow(), &client);
        let r = &client.resolved;
        engine.log.event(
            "session",
            json!({
                "provider": r.provider_id, "model": r.model.id, "protocol": format!("{:?}", r.protocol), "gateway": r.gateway,
                "key_source": r.key_source, "why": r.why, "effort": client.effort(), "max_output": client.max_output,
                "context": r.model.context, "price": {"input": r.model.cost.input, "output": r.model.cost.output, "cache_read": r.model.cost.cache_read, "cache_write": r.model.cost.cache_write},
                "root": root.display().to_string(), "system_chars": system.len(), "version": env!("CARGO_PKG_VERSION"),
            }),
        );
        let tools = if r.model.tool_call {
            vec![Tool {
                name: "js",
                description: "Run one JavaScript block in Stem's sandbox. The block is the body of an async function: use `return` to send back a small result. Only the return value (and console.log lines) comes back.".into(),
                schema: json!({"type": "object", "properties": {"code": {"type": "string", "description": "JavaScript function body using $"}}, "required": ["code"]}),
            }]
        } else {
            Vec::new()
        };
        Ok(Session {
            client,
            engine,
            msgs: Vec::new(),
            system,
            cancel,
            usage: Usage::default(),
            cost: 0.0,
            tools,
            last_context: 0,
            cache_at: None,
            steps: Vec::new(),
            next_step: 0,
            leaving: Vec::new(),
            ejected: Vec::new(),
            warned: 0,
            summarizer,
        })
    }

    pub fn clear(&mut self) {
        self.msgs.clear();
        self.steps.clear();
        self.leaving.clear();
        self.ejected.clear();
        self.last_context = 0;
        self.warned = 0;
    }

    fn ratio(&self) -> f64 {
        let context = self.client.resolved.model.context;
        if context == 0 { 0.0 } else { self.last_context as f64 / context as f64 }
    }

    fn step_sizes(&self) -> Vec<(u64, usize)> {
        let mut sizes: Vec<(u64, usize)> = Vec::new();
        for (i, step) in self.steps.iter().enumerate() {
            let Some(n) = step else { continue };
            let chars: usize = self.msgs[i]
                .blocks
                .iter()
                .map(|b| match b {
                    Block::Text(t) => t.len(),
                    Block::ToolUse { input, .. } => input.to_string().len(),
                    Block::ToolResult { content, .. } => content.len(),
                    _ => 0,
                })
                .sum();
            match sizes.iter_mut().find(|(s, _)| s == n) {
                Some(entry) => entry.1 += chars,
                None => sizes.push((*n, chars)),
            }
        }
        sizes
    }

    fn offload_dir(&self) -> std::path::PathBuf {
        let session = self.engine.log.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.engine.host.borrow().root.join(".stem").join("offload").join(session)
    }

    fn cache_cold(&self) -> bool {
        match (self.cache_at, self.client.cache_ttl()) {
            (Some(at), Some(ttl)) => at.elapsed() >= ttl,
            (None, _) => true,
            (Some(_), None) => false,
        }
    }

    fn publish_context(&self) {
        let sizes = self.step_sizes();
        let out_ids: Vec<u64> = self.ejected.iter().map(|(n, _)| *n).collect();
        let leaving: Vec<u64> = self.leaving.iter().flat_map(|(ids, _)| ids.iter().copied()).collect();
        let mut steps: Vec<Value> = sizes
            .iter()
            .map(|(n, chars)| {
                json!({"id": n, "tokens": chars / 4, "what": self.step_what(*n), "state": if leaving.contains(n) { "leaving" } else { "in" }})
            })
            .collect();
        for (n, what) in &self.ejected {
            steps.push(json!({"id": n, "tokens": 0, "what": what, "state": "out"}));
        }
        steps.sort_by_key(|v| v["id"].as_u64().unwrap_or(0));
        let warm = match (self.cache_at, self.client.cache_ttl()) {
            (Some(at), Some(ttl)) => Some(ttl.saturating_sub(at.elapsed()).as_secs()),
            _ => None,
        };
        let context = self.client.resolved.model.context;
        self.engine.host.borrow_mut().context = json!({
            "used": self.last_context,
            "limit": context,
            "percent": if context == 0 { 0 } else { self.last_context * 100 / context },
            "cacheWarmSeconds": warm,
            "steps": steps,
            "outCount": out_ids.len(),
        });
        self.engine.host.borrow_mut().offload_dir = Some(self.offload_dir());
    }

    fn step_what(&self, n: u64) -> String {
        for (m, s) in self.msgs.iter().zip(&self.steps) {
            if *s != Some(n) || m.role != Role::Assistant {
                continue;
            }
            for b in &m.blocks {
                if let Block::ToolUse { input, .. } = b {
                    let code = input["code"].as_str().unwrap_or("");
                    let mut names: Vec<String> = Vec::new();
                    for (at, _) in code.match_indices("$.") {
                        let name: String = code[at + 2..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                        let arg: String = code[at + 2 + name.len()..].trim_start_matches('(').chars().take_while(|c| *c != ')' && *c != ',' && *c != '\n').take(40).collect();
                        let item = format!("{name}{}", if arg.is_empty() { String::new() } else { format!(" {}", arg.trim_matches(|c| c == '"' || c == '\'' || c == '`')) });
                        if !name.is_empty() && !names.contains(&item) && names.len() < 4 {
                            names.push(item);
                        }
                    }
                    return names.join(", ");
                }
            }
        }
        String::new()
    }

    fn apply_ejects(&mut self, keep: Option<u64>, force: bool) -> Usage {
        let mut usage = Usage::default();
        if self.leaving.is_empty() || !(force || self.cache_cold() || self.ratio() >= 0.8) {
            return usage;
        }
        let queued = std::mem::take(&mut self.leaving);
        for (ids, summary) in queued {
            let ids: Vec<u64> = ids.into_iter().filter(|n| Some(*n) != keep).collect();
            match self.eject(&ids, summary) {
                Ok(u) => usage.add(u),
                Err(e) => self.engine.log.event("eject_failed", json!({"error": e, "ids": ids})),
            }
        }
        usage
    }

    fn eject(&mut self, ids: &[u64], summary: Option<String>) -> Result<Usage, String> {
        let chosen: Vec<usize> = (0..self.msgs.len()).filter(|&i| self.steps[i].is_some_and(|n| ids.contains(&n))).collect();
        if chosen.is_empty() {
            return Err(format!("none of the steps {ids:?} is still in the context"));
        }
        let dir = self.offload_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut taken: Vec<u64> = Vec::new();
        let mut all = String::new();
        let mut chars = 0;
        for &n in ids {
            let mut dump = String::new();
            for i in chosen.iter().copied().filter(|&i| self.steps[i] == Some(n)) {
                for b in &self.msgs[i].blocks {
                    match b {
                        Block::ToolUse { input, .. } => dump.push_str(&format!("## #{n} script\n```js\n{}\n```\n", input["code"].as_str().unwrap_or(""))),
                        Block::ToolResult { content, .. } => dump.push_str(&format!("## #{n} result\n{content}\n")),
                        Block::Text(t) if !t.trim().is_empty() => dump.push_str(&format!("{t}\n")),
                        _ => {}
                    }
                }
            }
            if dump.is_empty() {
                continue;
            }
            std::fs::write(dir.join(format!("{n}.md")), &dump).map_err(|e| e.to_string())?;
            chars += dump.len();
            all.push_str(&dump);
            let what = self.step_what(n);
            self.ejected.push((n, what));
            taken.push(n);
        }
        let mut usage = Usage::default();
        let summary = match summary.filter(|s| !s.trim().is_empty()) {
            Some(s) => s,
            None => {
                let excerpt: String = all.chars().take(200_000).collect();
                let reply = self.summarizer.complete(
                    "You compress your own past work so you can keep going with less context. Write at most 8 short lines: facts learned (paths, line numbers, names, commands), changes made, what failed, what is still open. No prose.",
                    &[Msg { role: Role::User, blocks: vec![Block::Text(excerpt)] }],
                    &[],
                    &self.cancel,
                )?;
                usage = reply.usage;
                reply.blocks.iter().filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join("\n")
            }
        };
        let first = chosen[0];
        for &i in chosen.iter().rev() {
            self.msgs.remove(i);
            self.steps.remove(i);
        }
        let labels: Vec<String> = taken.iter().map(|n| format!("#{n}")).collect();
        let note = format!(
            "[stem: {} moved out of the context ({}k tokens); $.recall(id) brings one back. Summary:\n{}]",
            labels.join(" "),
            chars / 4000,
            summary.trim()
        );
        match first.checked_sub(1).and_then(|i| self.msgs.get_mut(i)) {
            Some(prev) if prev.role == Role::User => prev.blocks.push(Block::Text(note)),
            _ => {
                let at = first.min(self.msgs.len());
                self.msgs.insert(at, Msg { role: Role::User, blocks: vec![Block::Text(note)] });
                self.steps.insert(at, None);
            }
        }
        self.last_context = self.last_context.saturating_sub(chars as u64 / 4);
        self.engine.log.event("eject", json!({"ids": taken, "chars": chars, "summary_chars": summary.len(), "cold": self.cache_cold()}));
        Ok(usage)
    }

    pub fn turn(&mut self, prompt: &str) -> TurnReport {
        let started = Instant::now();
        self.cancel.store(false, Ordering::SeqCst);
        self.engine.board.start();
        match self.msgs.last_mut() {
            Some(last) if last.role == Role::User => last.blocks.push(Block::Text(prompt.to_string())),
            _ => {
                self.msgs.push(Msg { role: Role::User, blocks: vec![Block::Text(prompt.to_string())] });
                self.steps.push(None);
            }
        }
        let mut turn_usage = Usage::default();
        let mut turn_cost = 0.0;
        let mut scripts = 0;
        let mut steps = 0;
        let mut answer = String::new();
        let mut error = None;
        let (mut dirty, mut changed_any, mut ran_any, mut repro_any, mut nudges) = (false, false, false, false, 0usize);
        let mut given: Vec<&str> = Vec::new();
        let ask_cost_before = self.engine.host.borrow().ask_cost;
        self.engine.host.borrow_mut().plan.clear();
        self.engine.board.plan(Vec::new());
        self.engine.log.event("prompt", json!({"chars": prompt.len()}));
        while steps < MAX_STEPS {
            steps += 1;
            if self.cancel.load(Ordering::SeqCst) {
                error = Some("stopped".into());
                break;
            }
            if self.ratio() >= 0.95 && self.leaving.is_empty() {
                let sizes = self.step_sizes();
                if sizes.len() > 3 {
                    let total: usize = sizes.iter().map(|(_, c)| c).sum();
                    let mut acc = 0;
                    let oldest: Vec<u64> = sizes
                        .iter()
                        .take(sizes.len() - 2)
                        .take_while(|(_, c)| {
                            acc += c;
                            acc < total / 2
                        })
                        .map(|(n, _)| *n)
                        .collect();
                    let oldest = if oldest.is_empty() { vec![sizes[0].0] } else { oldest };
                    self.leaving.push((oldest, None));
                }
            }
            let u = self.apply_ejects(None, self.ratio() >= 0.95);
            turn_usage.add(u);
            turn_cost += cost_of(&self.summarizer.resolved.model.cost, u);
            if let Some(full) = self.context_full() {
                error = Some(full);
                break;
            }
            self.engine.board.phase("thinking");
            let step_started = Instant::now();
            let reply = match self.client.complete(&self.system, &self.msgs, &self.tools, &self.cancel) {
                Ok(r) => r,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            };
            let step_cost = cost_of(&self.client.resolved.model.cost, reply.usage);
            turn_usage.add(reply.usage);
            turn_cost += step_cost;
            self.last_context = reply.usage.input + reply.usage.cache_read + reply.usage.cache_write;
            if reply.usage.cache_read + reply.usage.cache_write > 0 {
                self.cache_at = Some(Instant::now());
            }
            self.engine.board.cache(cache_percent(turn_usage), self.cache_at.zip(self.client.cache_ttl()));
            self.engine.log.event(
                "model",
                json!({"in": reply.usage.input, "out": reply.usage.output, "cache_read": reply.usage.cache_read, "cache_write": reply.usage.cache_write,
                       "cost": step_cost, "ms": step_started.elapsed().as_millis() as u64, "stop": format!("{:?}", reply.stop)}),
            );
            let text: String = reply
                .blocks
                .iter()
                .filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None })
                .collect::<Vec<_>>()
                .join("\n");
            let calls: Vec<(String, String, Value)> = reply
                .blocks
                .iter()
                .filter_map(|b| {
                    if let Block::ToolUse { id, name, input } = b {
                        Some((id.clone(), name.clone(), input.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let fenced = if self.tools.is_empty() { fenced_js(&text) } else { Vec::new() };
            let step_id = (!calls.is_empty() || !fenced.is_empty()).then(|| {
                self.next_step += 1;
                self.next_step
            });
            self.msgs.push(Msg {
                role: Role::Assistant,
                blocks: reply.blocks.clone(),
            });
            self.steps.push(step_id);
            if calls.is_empty() && fenced.is_empty() && reply.stop != Stop::MaxTokens && !text.trim().is_empty() && nudges < 2 {
                let bug = { let p = prompt.to_lowercase(); p.contains("bug") || p.starts_with("fix") || p.contains(" fix ") };
                let nudge = if dirty && !given.contains(&"dirty") {
                    given.push("dirty");
                    Some("[stem] Before the final answer: files changed since anything last ran. Prove the change: reproduce the exact trigger from the request with a short script ($.write('/tmp/repro.ts', ...) then $.sh('bun /tmp/repro.ts')) and show the right result, then run the tests that cover it ($.testsFor(files), then $.test(those files)). Fix what fails, then answer.")
                } else if bug && changed_any && !repro_any && !given.contains(&"repro") {
                    given.push("repro");
                    Some("[stem] Before the final answer: this is a bug fix, and no reproduction of the bug was run. Existing tests passed before the fix too, so they do not prove it. Write a short script in /tmp that triggers exactly what the report describes, run it with $.sh, and show the right result now. If it still shows the bug, fix it. Then answer.")
                } else if !changed_any && !ran_any && imperative(prompt) && !given.contains(&"nothing") {
                    given.push("nothing");
                    Some("[stem] Before the final answer: nothing was changed and nothing was run. If you think it already works, prove it: reproduce the exact trigger from the request with a short script and show the right result. If it does not work, make the fix. Then answer.")
                } else {
                    None
                };
                if let Some(nudge) = nudge {
                    nudges += 1;
                    self.engine.log.event("nudge", json!({"dirty": dirty, "changed": changed_any, "ran": ran_any}));
                    self.msgs.push(Msg { role: Role::User, blocks: vec![Block::Text(nudge.to_string())] });
                    self.steps.push(None);
                    continue;
                }
            }
            if calls.is_empty() && fenced.is_empty() {
                if reply.stop == Stop::MaxTokens {
                    answer = format!("{text}\n\n[cut off: the reply hit the {} token output limit]", self.client.max_output);
                } else if text.trim().is_empty() {
                    error = Some(match reply.stop {
                        Stop::MaxTokens => format!("empty reply: the model used its whole {} token budget on reasoning", self.client.max_output),
                        other => format!("empty reply from the model (stop: {other:?})"),
                    });
                } else {
                    answer = text;
                }
                break;
            }
            let mut results = Vec::new();
            for (id, name, input) in calls {
                let (content, is_error) = if name == "js" {
                    match input["code"].as_str() {
                        Some(code) => {
                            scripts += 1;
                            self.publish_context();
                            let result = self.engine.run_script(code);
                            let (changed, ran) = self.engine.step_flags();
                            changed_any |= changed;
                            ran_any |= ran;
                            repro_any |= self.engine.ran_repro();
                            dirty = (dirty || changed) && !self.engine.ran_last();
                            result
                        }
                        None => ("the js tool needs {\"code\": \"...\"}; your call had no code string".to_string(), true),
                    }
                } else {
                    (format!("there is no tool named {name}; the only tool is js"), true)
                };
                results.push(Block::ToolResult { id, content, is_error });
            }
            for code in fenced {
                scripts += 1;
                let (content, is_error) = self.engine.run_script(&code);
                results.push(Block::Text(format!(
                    "{}{content}",
                    if is_error { "Script failed:\n" } else { "Script result:\n" }
                )));
            }
            if let (Some(n), Some(Block::ToolResult { content, .. })) = (step_id, results.first_mut()) {
                content.push_str(&format!("\n[#{n}]"));
            }
            let ratio = self.ratio();
            let level = if ratio >= 0.9 { 2 } else if ratio >= 0.8 { 1 } else { 0 };
            if level > self.warned {
                self.warned = level;
                results.push(Block::Text(format!(
                    "[stem: the context is {}% full ({} of {} tokens). $.context() lists every step with its id and size; $.eject([ids], summary) moves finished ones out to a file and leaves your summary.]",
                    (ratio * 100.0) as u64,
                    self.last_context,
                    self.client.resolved.model.context
                )));
            }
            self.msgs.push(Msg {
                role: Role::User,
                blocks: results,
            });
            self.steps.push(step_id);
            let asked: Vec<(Vec<u64>, Option<String>)> = self.engine.host.borrow_mut().eject.drain(..).collect();
            self.leaving.extend(asked);
            let u = self.apply_ejects(step_id, false);
            turn_usage.add(u);
            turn_cost += cost_of(&self.summarizer.resolved.model.cost, u);
        }
        let open: Vec<String> = self.engine.host.borrow().plan.iter().filter(|(_, d)| !*d).map(|(n, _)| n.clone()).collect();
        if !open.is_empty() && !answer.is_empty() && !answer.contains("Not done:") {
            answer.push_str("\nNot done:");
            for step in &open {
                answer.push_str(&format!("\n- {step}: planned, not marked done"));
            }
        }
        if steps >= MAX_STEPS && answer.is_empty() && error.is_none() {
            error = Some(format!("stopped after {MAX_STEPS} model steps without a final answer"));
        }
        if self
            .msgs
            .last()
            .is_some_and(|m| m.role == Role::User && m.blocks.iter().any(|b| matches!(b, Block::ToolResult { .. })))
        {
            self.msgs.push(Msg {
                role: Role::Assistant,
                blocks: vec![Block::Text(format!("(stopped: {})", error.clone().unwrap_or_default()))],
            });
            self.steps.push(None);
        }
        let ask_cost = self.engine.host.borrow().ask_cost - ask_cost_before;
        turn_cost += ask_cost;
        self.usage.add(turn_usage);
        self.cost += turn_cost;
        let pieces = self.engine.board.stop();
        let elapsed = started.elapsed();
        self.engine.log.event(
            "turn",
            json!({"steps": steps, "scripts": scripts, "calls": pieces, "generation": self.engine.host.borrow().growth.generation(), "in": turn_usage.input, "out": turn_usage.output, "cache_read": turn_usage.cache_read,
                   "cache_write": turn_usage.cache_write, "cost": turn_cost, "ask_cost": ask_cost, "ms": elapsed.as_millis() as u64, "error": error, "answer_chars": answer.len()}),
        );
        TurnReport {
            answer,
            scripts,
            usage: turn_usage,
            cost: turn_cost,
            elapsed,
            error,
        }
    }

    fn context_full(&self) -> Option<String> {
        let context = self.client.resolved.model.context;
        (context > 0 && self.last_context >= context * 99 / 100).then(|| {
            format!(
                "this conversation fills {}% of {}'s {} token context. Stem never rewrites earlier messages, so the cache stays valid; start a new conversation with /new.",
                self.last_context * 100 / context,
                self.client.resolved.model.id,
                context
            )
        })
    }
}

fn fenced_js(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let Some(newline) = after.find('\n') else { break };
        let lang = after[..newline].trim();
        let body = &after[newline + 1..];
        let Some(end) = body.find("```") else { break };
        if matches!(lang, "js" | "javascript" | "ts" | "typescript") {
            out.push(body[..end].to_string());
        }
        rest = &body[end + 3..];
    }
    out
}

pub const SCRIPTS_DOC: &str = r#"How scripts work
- The block runs in a sandbox with one object, `$`. Every `$` call returns its value directly; `await` is allowed but not needed.
- Only the script's return value (and console.log lines) comes back to you. File contents, search hits and logs stay inside the script, so read, filter and decide in code and return a short summary.
- Do the whole job in one script when you can: gather, change, check, return. Variables do not survive between scripts; put anything you want to keep on `state`.
- If a `$` call fails, the script stops there and you are told which step failed. Edits made before the failure are kept.

$ functions
Look
- $.read(path, from?, to?) → file text (from/to are 1-based line numbers)
- $.files(glob?) → paths, git-ignored files skipped
- $.grep(regex, {path?, glob?, max?, ignoreCase?, literal?}) → [{file, line, text}]
- $.project() → runner, commands, languages, branch
Change
- $.edit(path, old, new) → exact text replace; fails if old is missing or not unique (shows the closest text). Also $.edit(path, [{old, new}, ...]), $.edit(path, regex, replacement), $.edit(path, text => newText). Pass {old, new, all: true} to replace every copy.
- $.write(path, text) → create or overwrite a file; scratch files (repro scripts) go in /tmp and never need approval
- $.remove(pathOrPaths) → delete files or folders inside the project (asks the user first)
- $.rename(nameOrLocation, newName) → symbol-aware rename across the project
- Edit, write and rename results include `errors`: the language server's errors in that file right after the change.
Run
- $.test(filter?) → {passed, code, summary, out, err} with the project's own test command; pass test file paths to run only those
- $.testsFor(files) → test files that cover those source files (same name, or importing them)
- $.run(name) → runs a project command or package script (build, lint, typecheck, ...)
- $.sh(cmd, {timeout?, cwd?}) → {code, out, err}; for git, installs and anything else
- Destructive steps (deleting, git reset, clean, push, history rewrites, publishing) wait for the user's approval. If the user declines, the call fails with that answer: do not retry it another way.
Ask the fact store (SQLite, kept current for every file)
- $.sql(query, ...params) → rows as objects, read-only. Tables: files(path, lang, size, mtime, lines) · symbols(file, name, kind, line, end_line, doc) definitions · refs(file, name, kind, line) calls and type uses by name · commits(hash, date, author, subject) · commit_files(hash, path) · cochange(a, b, n) files changed together n times. Use ? placeholders for values.
Know (language servers, started automatically)
- $.defs(name), $.refs(name), $.callers(name), $.impls(name) → [{file, line, text}]
- $.typeAt(name) → type and docs
- $.symbols(query) → [{file, line, text, kind}];  $.outline(path) → the file's structure
- $.diagnostics(path?) → [{file, line, text, severity}]
- Wherever a name is accepted you can pass {file, line, name} instead, to pick one exact place. If a name matches several symbols, results say which one with `of`.
Grow (functions saved in this project, kept across sessions)
- $.save(name, (args) => { ... }, {doc: "one line: what it returns", check: (f) => f(...) === expected}) → saves a function made of $ calls; it becomes $.name now and in every later session. The check runs before saving and again when it is first used in a session; a failing one is dropped. Save only what this project will need again.
- $.forget(name, reason) → drop a saved function that is wrong or no longer useful.
Delegate
- $.ask(prompt, input?) → a small, cheap model's text answer, for summarizing or classifying large text inside a script
Context (each script result ends with its id, like [#7])
- $.context() → {used, limit, percent, cacheWarmSeconds, steps: [{id, tokens, what, state: in | leaving | out}]}
- $.eject([ids], summary?) → move finished steps out to a file; only your short summary stays (written for you if left out). To keep the prompt cache valid they leave when the cache goes cold or the context passes 80%; until then they stay.
- $.recall(id) → the full script and result of an ejected step, as this script's result
Progress
- $.plan(["step", ...]) → set the plan for this request: 2 to 7 short steps the user can follow. Call it in your first script for any task with more than one step.
- $.done(stepNumberOrName?) → mark a step done (no argument: the next one). Mark each step as soon as it is finished.
"#;

fn system_prompt(host: &Host, client: &Client) -> String {
    let p = &host.project;
    let commands: Vec<String> = p.commands.iter().map(|(k, c)| format!("{k}: `{c}`")).collect();
    let languages: Vec<String> = p.languages.iter().take(6).map(|(l, n)| format!("{} ({n} files)", l.label())).collect();
    let act = if client.resolved.model.tool_call {
        "To act, call the `js` tool with a JavaScript function body. When the task is finished, reply in plain text with no tool call: that ends your turn."
    } else {
        "To act, reply with one ```js fenced block containing a JavaScript function body; it runs and its result comes back. When the task is finished, reply in plain text with no code block: that ends your turn."
    };
    format!(
        r#"You are Stem, a coding engine working inside one project. You get things done with code, then report what you did.

{act}

{doc}
Rules
- Read before you change. Keep changes minimal and in the style of the code around them.
- Use $.rename for renames, never text replace.
- For a bug: first reproduce it, before changing anything. Write a short script in /tmp that triggers exactly what the report describes and run it with $.sh (for example `bun /tmp/repro.ts`); it must show the wrong result. If it does not, the repro is wrong, not the report: re-read the trigger and evidence and try again. Then fix the root cause, run the same script again and show the right result, then run the tests that cover the change ($.testsFor). Existing tests passing is not proof: they passed before the fix too.
- After changing code, check it: look at `errors` in edit results, then run the tests or typecheck that cover the change.
- Never discard work you did not make: no git reset, checkout of files, stash or clean. Never commit or push unless asked.
- Use the project's own runner{runner}.
- If the request is ambiguous, pick the most useful reading and say which one in your answer.

Final answer
The final answer is structured, never prose. No greeting, no filler, no explanation of your process, no offers.
First line, always: "Asked: " and the user's request rewritten as one clear, precise sentence.
For a question, then:
Answer: the direct answer in 1 to 5 short lines; cite path:line where it helps.
For a task, then:
Done:
- what you did, one line each, with the path (for example "src/cart.ts: subtotal multiplies price by qty")
- the checks you ran and their result (for example "bun test: 5 pass, 0 fail")
Not done:
- what was asked but not done, and why, one line each
Leave out "Not done:" when everything was done. Nothing else after these parts: no closing sentence, no notes, no suggestions.
Each line is short: at most about 15 words. No adjectives, no hedging.
Example for a task:
Asked: Fix the failing cart tests.
Done:
- src/cart.ts: subtotal now multiplies price by qty
- bun test: 2 pass, 0 fail
Not done:
- tsc error in cart.test.ts (missing bun types): unrelated to the bug, left as is
Example for a question:
Asked: Where is the --max-count flag parsed?
Answer: crates/core/flags/defs.rs:3913, MaxCount::update stores it in args.max_count.

{grown}Project
- Root: {root}
- Branch: {branch}
- Languages: {languages}
- Commands: {commands}
- Top level: {top}
"#,
        doc = SCRIPTS_DOC,
        grown = {
            let lines = host.growth.prompt_lines();
            if lines.is_empty() { String::new() } else { format!("Saved functions in this project (use them, they are checked)\n{}\n\n", lines.join("\n")) }
        },
        runner = if p.runner.is_empty() { String::new() } else { format!(" ({})", p.runner) },
        root = p.root.display(),
        branch = p.branch.clone().unwrap_or_else(|| "none".into()),
        languages = if languages.is_empty() { "none detected".into() } else { languages.join(", ") },
        commands = if commands.is_empty() { "none detected".into() } else { commands.join(", ") },
        top = p.top.join(" "),
    )
}
