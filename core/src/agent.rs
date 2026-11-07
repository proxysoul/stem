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
const FAMILIAR_EFFORT: &str = "low";

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
    ejected: Vec<(u64, String)>,
    compacted: bool,
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
    pub fn flush_learned(&mut self) {
        let learned: Vec<(String, Value)> = self.host.borrow_mut().learn.events.drain(..).collect();
        for (kind, fields) in learned {
            self.log.event(&kind, fields);
        }
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
        {
            let text = if outcome.ok { outcome.value.to_string() } else { outcome.error.clone().unwrap_or_default() };
            self.host.borrow_mut().learn.last_result = text.chars().take(4000).collect();
        }
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
                   "calls": calls, "steps": steps.iter().take(40).collect::<Vec<_>>(), "touched_bytes": touched, "return_bytes": full, "error": outcome.error, "timed_out": outcome.timed_out}),
        );
        let notes: Vec<String> = self.host.borrow_mut().growth.notes.drain(..).collect();
        self.flush_learned();
        for note in notes {
            text.push_str(&format!("\n{note}"));
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

impl Session {
    pub fn new(client: Client, ask: Option<Client>, root: std::path::PathBuf, cancel: Arc<AtomicBool>, yolo: Arc<AtomicBool>, approver: Option<Approver>) -> Result<Session, String> {
        let client = Rc::new(client);
        let mut engine = Engine::new(root.clone(), cancel.clone(), ask, Board::new(), yolo, approver)?;
        engine.host.borrow_mut().growth.new_session();
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
            ejected: Vec::new(),
            compacted: false,
        })
    }

    pub fn clear(&mut self) {
        self.msgs.clear();
        self.steps.clear();
        self.ejected.clear();
        self.last_context = 0;
        self.compacted = false;
    }

    fn ratio(&self) -> f64 {
        self.last_context as f64 / self.limit() as f64
    }

    fn limit(&self) -> u64 {
        std::env::var("STEM_CONTEXT_LIMIT").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(self.client.resolved.model.context).max(1)
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

    fn publish_context(&self) {
        let sizes = self.step_sizes();
        let out_ids: Vec<u64> = self.ejected.iter().map(|(n, _)| *n).collect();
        let mut steps: Vec<Value> = sizes
            .iter()
            .map(|(n, chars)| {
                json!({"id": n, "tokens": chars / 4, "what": self.step_what(*n), "state": "in"})
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
        let context = self.limit();
        self.engine.host.borrow_mut().context = json!({
            "used": self.last_context,
            "limit": context,
            "percent": self.last_context * 100 / context,
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

    fn compaction_request(&self, why: &str, keep: Option<u64>) -> String {
        let context = self.limit();
        let list: Vec<String> = self
            .step_sizes()
            .iter()
            .filter(|(n, _)| Some(*n) != keep)
            .map(|(n, chars)| format!("#{n} · {} tokens · {}", chars / 4, self.step_what(*n)))
            .collect();
        format!(
            "[stem: compaction ({why}). The context is {}% full ({} of {} tokens). Choose finished steps to eject: each moves to a file and $.recall(id) brings it back later. Keep what the current work still needs. Add a summary only for steps whose facts you may need again; leave it out otherwise. Reply with JSON only, no tool call:\n{{\"eject\": [{{\"id\": 3, \"summary\": \"optional\"}}, {{\"id\": 4}}]}}\nSteps:\n{}]",
            self.last_context * 100 / context,
            self.last_context,
            context,
            list.join("\n")
        )
    }

    fn compact(&mut self, why: &str, keep: Option<u64>) -> (Usage, String) {
        let mut usage = Usage::default();
        if self.step_sizes().iter().filter(|(n, _)| Some(*n) != keep).count() == 0 {
            return (usage, "nothing to eject".into());
        }
        let percent = self.last_context * 100 / self.limit();
        self.engine.board.banner(Some(format!("compacting · {percent}% full · choosing what to eject")));
        let request = self.compaction_request(why, keep);
        let joined = match self.msgs.last_mut() {
            Some(last) if last.role == Role::User => {
                last.blocks.push(Block::Text(request));
                true
            }
            _ => {
                self.msgs.push(Msg { role: Role::User, blocks: vec![Block::Text(request)] });
                self.steps.push(None);
                false
            }
        };
        let reply = self.client.complete(&self.system, &self.msgs, &self.tools, &self.cancel);
        if joined {
            if let Some(last) = self.msgs.last_mut() {
                last.blocks.pop();
            }
        } else {
            self.msgs.pop();
            self.steps.pop();
        }
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                self.engine.board.banner(None);
                self.engine.log.event("compact_failed", json!({"error": e}));
                let line = format!("compaction failed: {e}");
                self.engine.board.notice(&line);
                return (usage, line);
            }
        };
        usage.add(reply.usage);
        let text: String = reply.blocks.iter().filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join("");
        let picked: Vec<(u64, Option<String>)> = text
            .find('{')
            .zip(text.rfind('}'))
            .and_then(|(a, b)| serde_json::from_str::<Value>(&text[a..=b]).ok())
            .map(|v| {
                v["eject"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| {
                        let id = e["id"].as_u64().or_else(|| e.as_u64()).or_else(|| e["id"].as_str().and_then(|s| s.trim_start_matches('#').parse().ok()))?;
                        Some((id, e["summary"].as_str().map(str::to_string).filter(|s| !s.trim().is_empty())))
                    })
                    .filter(|(id, _)| Some(*id) != keep)
                    .collect()
            })
            .unwrap_or_default();
        self.engine.log.event("compact", json!({"why": why, "picked": picked.iter().map(|(i, _)| i).collect::<Vec<_>>(), "reply_chars": text.len()}));
        if picked.is_empty() {
            self.engine.board.banner(None);
            let line = format!("compaction · kept all {} steps · {percent}% full · forced eject at 95%", self.step_sizes().len());
            self.engine.board.notice(&line);
            return (usage, line);
        }
        (usage, self.eject(&picked))
    }

    fn eject(&mut self, picked: &[(u64, Option<String>)]) -> String {
        let ids: Vec<u64> = picked.iter().map(|(n, _)| *n).collect();
        let chosen: Vec<usize> = (0..self.msgs.len()).filter(|&i| self.steps[i].is_some_and(|n| ids.contains(&n))).collect();
        if chosen.is_empty() {
            self.engine.board.banner(None);
            return format!("none of {ids:?} is in the context");
        }
        let present: Vec<u64> = ids.iter().copied().filter(|n| chosen.iter().any(|&i| self.steps[i] == Some(*n))).collect();
        let labels: Vec<String> = present.iter().map(|n| format!("#{n}")).collect();
        self.engine.board.banner(Some(format!("ejecting {} → file", labels.join(" "))));
        let dir = self.offload_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.engine.board.banner(None);
            let line = format!("eject skipped: cannot create {} ({e}); every step stays in the context", dir.display());
            self.engine.board.notice(&line);
            self.engine.log.event("eject_failed", json!({"ids": ids, "error": e.to_string()}));
            return line;
        }
        let mut lines: Vec<String> = Vec::new();
        let mut written: Vec<u64> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        let mut chars = 0;
        for (n, summary) in picked {
            let mut dump = String::new();
            for i in chosen.iter().copied().filter(|&i| self.steps[i] == Some(*n)) {
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
            if let Err(e) = std::fs::write(dir.join(format!("{n}.md")), &dump) {
                failed.push(format!("#{n} ({e})"));
                continue;
            }
            written.push(*n);
            chars += dump.len();
            let what = self.step_what(*n);
            lines.push(match summary {
                Some(s) => format!("#{n} {what}: {}", s.trim()),
                None => format!("#{n} {what}"),
            });
            self.ejected.push((*n, what));
        }
        let removed: Vec<usize> = chosen.iter().copied().filter(|&i| self.steps[i].is_some_and(|n| written.contains(&n))).collect();
        if removed.is_empty() {
            self.engine.board.banner(None);
            let line = format!("eject skipped: nothing could be saved to {}{}; every step stays in the context", dir.display(), if failed.is_empty() { String::new() } else { format!(" ({})", failed.join(", ")) });
            self.engine.board.notice(&line);
            self.engine.log.event("eject_failed", json!({"ids": ids, "failed": failed}));
            return line;
        }
        let labels: Vec<String> = written.iter().map(|n| format!("#{n}")).collect();
        let first = removed[0];
        for &i in removed.iter().rev() {
            self.msgs.remove(i);
            self.steps.remove(i);
        }
        let note = format!("[stem: ejected to files, $.recall(id) brings one back:\n{}]", lines.join("\n"));
        match first.checked_sub(1).and_then(|i| self.msgs.get_mut(i)) {
            Some(prev) if prev.role == Role::User => prev.blocks.push(Block::Text(note)),
            _ => {
                let at = first.min(self.msgs.len());
                self.msgs.insert(at, Msg { role: Role::User, blocks: vec![Block::Text(note)] });
                self.steps.insert(at, None);
            }
        }
        let before = self.last_context * 100 / self.limit();
        self.last_context = self.last_context.saturating_sub(chars as u64 / 4);
        let after = self.last_context * 100 / self.limit();
        self.engine.board.context(after);
        self.engine.board.ejected(lines.len(), chars / 4);
        self.engine.board.banner(None);
        let rel = dir.strip_prefix(&self.engine.host.borrow().root).map(|p| p.display().to_string()).unwrap_or_default();
        let line = format!("ejected {} · {} tokens · {before}% → {after}% · {rel} · recall with $.recall(id)", labels.join(" "), crate::ui::tokens(chars / 4));
        self.engine.board.notice(&line);
        self.engine.log.event("eject", json!({"ids": written, "chars": chars, "failed": failed, "summaries": picked.iter().filter(|(n, s)| s.is_some() && written.contains(n)).count()}));
        line
    }

    pub fn compact_now(&mut self) -> (String, f64) {
        let (u, line) = self.compact("the user asked to compact", None);
        let cost = cost_of(&self.client.resolved.model.cost, u);
        self.usage.add(u);
        self.cost += cost;
        (line, cost)
    }

    fn settle_judge(&mut self, j: JudgeRun, reply: &Result<crate::llm::Reply, String>, turn_usage: &mut Usage, turn_cost: &mut f64, started: Instant) -> Option<String> {
        j.done.store(true, Ordering::SeqCst);
        let outcome = j.handle.join().ok().and_then(|r| r.ok());
        let (pick, usage) = outcome?;
        let cost = cost_of(&j.price, usage);
        self.engine.host.borrow_mut().add_ask_usage(usage, cost);
        self.engine.log.event("learn_used", json!({"what": "answer_judge", "pick": pick}));
        let chosen = (pick >= 1).then(|| j.candidates.get(pick - 1).cloned()).flatten()?;
        if let Ok(r) = reply {
            let step_cost = cost_of(&self.client.resolved.model.cost, r.usage);
            turn_usage.add(r.usage);
            *turn_cost += step_cost;
            self.engine.log.event(
                "model",
                json!({"in": r.usage.input, "out": r.usage.output, "cache_read": r.usage.cache_read, "cache_write": r.usage.cache_write,
                       "cost": step_cost, "ms": started.elapsed().as_millis() as u64, "stop": format!("{:?}", r.stop), "discarded": true}),
            );
        }
        let note = self.engine.host.borrow_mut().learn.answer_note(&chosen);
        self.engine.flush_learned();
        Some(note)
    }

    pub fn turn(&mut self, prompt: &str) -> TurnReport {
        let started = Instant::now();
        self.cancel.store(false, Ordering::SeqCst);
        self.engine.board.start();
        let ask_cost_before = self.engine.host.borrow().ask_cost;
        let start = self.engine.host.borrow_mut().start_note(prompt);
        self.engine.flush_learned();
        let mut judge = start.judge.map(|j| JudgeRun::spawn(j, self.cancel.clone()));
        let restore_effort = if start.familiar && crate::learn::on("effort") && self.client.efforts().iter().any(|e| e == FAMILIAR_EFFORT) {
            let before = self.client.effort();
            let _ = self.client.set_effort(FAMILIAR_EFFORT);
            self.engine.log.event("learn_used", json!({"what": "effort"}));
            before
        } else {
            None
        };
        let text = match start.note {
            Some(n) => format!("{prompt}\n\n{n}"),
            None => prompt.to_string(),
        };
        match self.msgs.last_mut() {
            Some(last) if last.role == Role::User => last.blocks.push(Block::Text(text)),
            _ => {
                self.msgs.push(Msg { role: Role::User, blocks: vec![Block::Text(text)] });
                self.steps.push(None);
            }
        }
        let mut turn_usage = Usage::default();
        let mut turn_cost = 0.0;
        let mut scripts = 0;
        let mut steps = 0;
        let mut answer = String::new();
        let mut error = None;
        self.engine.host.borrow_mut().plan.clear();
        self.engine.board.plan(Vec::new());
        self.engine.log.event("prompt", json!({"chars": prompt.len()}));
        let max_steps = std::env::var("STEM_MAX_STEPS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(MAX_STEPS);
        while steps < max_steps {
            steps += 1;
            if self.cancel.load(Ordering::SeqCst) {
                error = Some("stopped".into());
                break;
            }
            if self.ratio() < 0.7 {
                self.compacted = false;
            }
            let newest = self.steps.iter().rev().flatten().next().copied();
            if self.ratio() >= 0.8 && !self.compacted {
                self.compacted = true;
                let (u, _) = self.compact("the context passed 80%", newest);
                turn_usage.add(u);
                turn_cost += cost_of(&self.client.resolved.model.cost, u);
            }
            if self.ratio() >= 0.95 {
                let sizes = self.step_sizes();
                let total: usize = sizes.iter().map(|(_, c)| c).sum();
                let mut acc = 0;
                let oldest: Vec<(u64, Option<String>)> = sizes
                    .iter()
                    .filter(|(n, _)| Some(*n) != newest)
                    .take_while(|(_, c)| {
                        acc += c;
                        acc < total / 2
                    })
                    .map(|(n, _)| (*n, None))
                    .collect();
                if !oldest.is_empty() {
                    let percent = self.last_context * 100 / self.limit();
                    self.engine.board.banner(Some(format!("context {percent}% · ejecting the oldest steps")));
                    self.eject(&oldest);
                }
            }
            if let Some(full) = self.context_full() {
                error = Some(full);
                break;
            }
            self.engine.board.phase("thinking");
            let step_started = Instant::now();
            let flag = judge.as_ref().map(|j| j.first_cancel.clone()).unwrap_or_else(|| self.cancel.clone());
            let reply = self.client.complete(&self.system, &self.msgs, &self.tools, &flag);
            if let Some(j) = judge.take() {
                if let Some(note) = self.settle_judge(j, &reply, &mut turn_usage, &mut turn_cost, step_started) {
                    if let Some(last) = self.msgs.last_mut() {
                        last.blocks.push(Block::Text(note));
                    }
                    if !self.cancel.load(Ordering::SeqCst) {
                        continue;
                    }
                }
            }
            let reply = match reply {
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
            self.engine.board.context(self.last_context * 100 / self.limit());
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
                            self.engine.run_script(code)
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
                content.insert_str(0, &format!("[#{n}] "));
            }
            self.msgs.push(Msg {
                role: Role::User,
                blocks: results,
            });
            self.steps.push(step_id);
        }
        let open: Vec<String> = self.engine.host.borrow().plan.iter().filter(|(_, d)| !*d).map(|(n, _)| n.clone()).collect();
        if !open.is_empty() && !answer.is_empty() && !answer.contains("Not done:") {
            answer.push_str("\nNot done:");
            for step in &open {
                answer.push_str(&format!("\n- {step}: planned, not marked done"));
            }
        }
        if steps >= max_steps && answer.is_empty() && error.is_none() {
            error = Some(format!("stopped after {max_steps} model steps without a final answer"));
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
        let finished = error.is_none() && !answer.is_empty();
        self.engine.host.borrow_mut().finish_learning(prompt, finished, steps, &answer);
        self.engine.flush_learned();
        if let Some(e) = restore_effort {
            let _ = self.client.set_effort(&e);
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
- Chain as many `$` calls as you need in one script and use each result in the next: the return shape of every call is listed below, so you know what comes back before you run it.
- A failed `$` call throws an Error. Catch it with try/catch to handle it and go on; if you don't, the script stops there and you are told which step failed. Edits made before the failure are kept.

$ functions
- $.read(path, from?, to?) → file text (from/to are 1-based line numbers)
- $.files(glob?) → paths, git-ignored files skipped
- $.grep(regex, {path?, glob?, max?, ignoreCase?, literal?}) → [{file, line, text}]
- $.edit(path, old, new) → {path, replaced, lines, errors}: exact text replace; fails if old is missing or not unique (shows the closest text). Also $.edit(path, [{old, new}, ...]), $.edit(path, regex, replacement), $.edit(path, text => newText). Pass {old, new, all: true} to replace every copy.
- $.write(path, text) → {path, lines, created, errors}: create or overwrite a file; scratch files (repro scripts) go in /tmp and never need approval
- Edit and write results include `errors`: the language server's errors in that file right after the change.
- $.test(filter?) → {passed, code, summary, out, err} with the project's own test command; pass test file paths to run only those
- $.testsFor(files) → test files that cover those source files (same name, or importing them)
- $.sh(cmd, {timeout?, cwd?}) → {code, out, err}; for git, installs and anything else
- $.plan(["step", ...]) → the plan for this request, 2 to 7 short steps the user can follow; call it first for any task with more than one step. $.done(step?) marks one done (no argument: the next).
- Destructive steps (deleting, git reset, clean, push, history rewrites, publishing) wait for the user's approval. If the user declines, the call fails with that answer: do not retry it another way.
"#;

pub const MORE_DOC: &str = r#"- $.rename(nameOrLocation, newName) → {files: [{file, edits}], errors}: symbol-aware rename across the project
- $.defs(name), $.refs(name), $.callers(name), $.impls(name) → [{file, line, text}] (callers also give {caller}). Wherever a name is accepted you can pass {file, line, name} to pick one exact place; if a name matches several symbols, results say which one with `of`.
- $.typeAt(name) → type and docs
- $.symbols(query) → [{file, line, text, kind}]
- $.outline(path) → the file's structure
- $.diagnostics(path?) → [{file, line, text, severity}]
- $.sql(query, ...params) → rows as objects, read-only. Tables: files(path, lang, size, mtime, lines) · symbols(file, name, kind, line, end_line, doc) definitions · refs(file, name, kind, line) calls and type uses by name · commits(hash, date, author, subject) · commit_files(hash, path) · cochange(a, b, n) files changed together n times. Use ? placeholders for values.
- $.run(name) → runs a project command or package script (build, lint, typecheck, ...)
- $.project() → runner, commands, languages, branch
- $.remove(pathOrPaths) → {removed: [paths]}: delete files or folders inside the project (asks the user first)
- $.ask(prompt, input?) → a small, cheap model's text answer, for summarizing or classifying large text inside a script
- $.save(name, (args) => { ... }, {doc: "one line: what it returns", check: (f) => f(...) === expected, anchors?: ["names it depends on"]}) → saves a function made of $ calls; it becomes $.name now and in every later session. Build area functions on queries ($.grep, $.files, $.refs), not fixed path lists, so they keep up with new code. Saving under an existing name makes a new version; if it keeps failing, the old one comes back. A function whose anchors leave the code, or that suddenly returns nothing, is flagged stale when used.
- $.tools() → saved functions, most used first: [{name, doc, uses, version, anchors, stale}]
- $.forget(name, reason) → drop a saved function that is wrong or no longer useful.
- $.context() → {used, limit, percent, cacheWarmSeconds, steps: [{id, tokens, what, state: in | out}], outCount}; each script result starts with its step id, like [#7]
- $.recall(id) → the full script and result of an ejected step, as this script's result. Ejecting happens only at compaction: when the context passes 80%, or the user runs /compact, Stem asks you which steps to eject.
"#;

pub fn help(name: Option<&str>) -> String {
    let lines: Vec<&str> = MORE_DOC.lines().filter(|l| name.is_none_or(|n| l.contains(&format!("$.{}(", n.trim_start_matches("$."))))).collect();
    if lines.is_empty() { MORE_DOC.to_string() } else { lines.join("\n") }
}

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

{doc}{more}
Rules
- Read before you change. Keep changes minimal and in the style of the code around them.
- Use $.rename for renames, never text replace.
- How to check a change is your call, or the user's when they say: `errors` in edit results, a test run, a quick script in /tmp, or nothing. Say in the answer what you checked.
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
- the checks you ran, if any, and their result (for example "bun test: 5 pass, 0 fail")
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

{learned}{grown}Project
- Root: {root}
- Branch: {branch}
- Languages: {languages}
- Commands: {commands}
- Top level: {top}
"#,
        doc = SCRIPTS_DOC,
        more = MORE_DOC,
        learned = {
            let lines = host.learn.prompt_lines();
            if lines.is_empty() { String::new() } else { format!("Learned in this project (checked against the code, not guesses)\n{}\n\n", lines.join("\n")) }
        },
        grown = {
            let lines = host.growth.prompt_lines();
            if lines.is_empty() { String::new() } else { format!("Saved functions in this project, checked and kept current. When a task touches one of these areas, call its function first instead of searching again.\n{}\n\n", lines.join("\n")) }
        },
        runner = if p.runner.is_empty() { String::new() } else { format!(" ({})", p.runner) },
        root = p.root.display(),
        branch = p.branch.clone().unwrap_or_else(|| "none".into()),
        languages = if languages.is_empty() { "none detected".into() } else { languages.join(", ") },
        commands = if commands.is_empty() { "none detected".into() } else { commands.join(", ") },
        top = p.top.join(" "),
    )
}

struct JudgeRun {
    candidates: Vec<crate::learn::Answer>,
    handle: std::thread::JoinHandle<Result<(usize, Usage), String>>,
    first_cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    price: crate::catalog::Cost,
}

impl JudgeRun {
    fn spawn(j: crate::host::PendingJudge, user_cancel: Arc<AtomicBool>) -> JudgeRun {
        let first_cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let price = j.resolved.model.cost.clone();
        let count = j.candidates.len();
        let (hit, finished, watch) = (first_cancel.clone(), done.clone(), user_cancel.clone());
        let (relay, stop) = (first_cancel.clone(), done.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if watch.load(Ordering::SeqCst) {
                    relay.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let resolved = j.resolved;
        let prompt = j.prompt;
        let handle = std::thread::spawn(move || {
            let client = Client::new(resolved);
            client.set_plain(true);
            let reply = client.complete("Answer exactly what is asked, briefly.", &[Msg { role: Role::User, blocks: vec![Block::Text(prompt)] }], &[], &user_cancel)?;
            let text: String = reply.blocks.iter().filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None }).collect();
            let pick: usize = text.trim().chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
            let pick = if pick <= count { pick } else { 0 };
            if pick >= 1 && !finished.load(Ordering::SeqCst) {
                hit.store(true, Ordering::SeqCst);
            }
            Ok((pick, reply.usage))
        });
        JudgeRun { candidates: j.candidates, handle, first_cancel, done, price }
    }
}
