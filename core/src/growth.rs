use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const CORE: &[&str] = &[
    "read", "write", "edit", "remove", "files", "grep", "sh", "project", "testsFor", "test", "run", "defs", "refs", "callers", "impls", "typeAt", "hover", "rename",
    "symbols", "outline", "diagnostics", "sql", "eject", "context", "help", "recall", "plan", "done", "ask", "save", "tools", "forget", "references", "definition",
    "find", "exec", "readFile", "writeFile", "delete", "rm",
];
const UNUSED_DAYS: u128 = 21;
const SHOWN: usize = 5;
const IDLE_SESSIONS: u64 = 5;
const BIG_RESULT: u64 = 3000;


#[derive(Serialize, Deserialize, Clone)]
pub struct Tool {
    pub name: String,
    pub doc: String,
    pub code: String,
    pub check: String,
    pub effects: Vec<String>,
    pub hash: String,
    pub born: u128,
    pub last_used: u128,
    pub uses: u64,
    pub fails: u64,
    #[serde(default)]
    pub dropped: Option<String>,
    #[serde(default = "first_version")]
    pub version: u32,
    #[serde(default)]
    pub anchors: Vec<String>,
    #[serde(default)]
    pub sizes: Vec<u64>,
    #[serde(default)]
    pub stale: Option<String>,
    #[serde(default)]
    pub stale_uses: u32,
    #[serde(default)]
    pub born_session: u64,
    #[serde(default)]
    pub last_session: u64,
    #[serde(default)]
    pub previous: Option<Box<Tool>>,
}

fn first_version() -> u32 {
    1
}

pub struct Growth {
    dir: PathBuf,
    pub enabled: bool,
    pub tools: Vec<Tool>,
    noted: Vec<String>,
    pub session_no: u64,
    pub notes: Vec<String>,
    pub events: Vec<(String, Value)>,
}

fn hash_of(text: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn effects_of(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (pattern, effect) in [
        ("$.edit(", "edits files"),
        ("$.write(", "writes files"),
        ("$.remove(", "deletes files"),
        ("$.rename(", "renames symbols"),
        ("$.sh(", "runs commands"),
        ("$.run(", "runs commands"),
        ("$.test(", "runs tests"),
        ("$.ask(", "calls a model"),
    ] {
        if code.contains(pattern) && !out.contains(&effect.to_string()) {
            out.push(effect.to_string());
        }
    }
    out
}

impl Growth {
    pub fn open(root: &Path) -> Growth {
        let dir = root.join(".stem").join("tools");
        let enabled = std::env::var("STEM_TOOLS").map(|v| v != "off" && v != "0").unwrap_or(true);
        let mut tools: Vec<Tool> = Vec::new();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.path().extension().is_some_and(|e| e == "json") && entry.file_name() != "explored.json" && entry.file_name() != "shapes.json" {
                if let Some(t) = std::fs::read_to_string(entry.path()).ok().and_then(|t| serde_json::from_str::<Tool>(&t).ok()) {
                    tools.push(t);
                }
            }
        }
        let _ = std::fs::remove_file(dir.join("shapes.json"));
        let _ = std::fs::remove_file(dir.join("explored.json"));
        let session_no = std::fs::read_to_string(dir.join("sessions")).ok().and_then(|t| t.trim().parse::<u64>().ok()).unwrap_or(0);
        let mut g = Growth { dir, enabled, tools, noted: Vec::new(), session_no, notes: Vec::new(), events: Vec::new() };
        let now = crate::events::now_ms();
        let stale: Vec<String> = g
            .alive()
            .filter(|t| t.uses < 3 && now.saturating_sub(t.last_used.max(t.born)) > UNUSED_DAYS * 24 * 3600 * 1000)
            .map(|t| t.name.clone())
            .collect();
        for name in stale {
            g.drop_tool(&name, &format!("unused for {UNUSED_DAYS} days"));
        }
        g
    }

    pub fn new_session(&mut self) {
        if !self.enabled || !self.dir.is_dir() {
            return;
        }
        let Ok(lock) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.dir.join("sessions.lock")) else {
            return;
        };
        if lock.lock().is_err() {
            return;
        }
        let stored = std::fs::read_to_string(self.dir.join("sessions")).ok().and_then(|t| t.trim().parse::<u64>().ok()).unwrap_or(0);
        self.session_no = stored + 1;
        let _ = std::fs::write(self.dir.join("sessions"), self.session_no.to_string());
        let _ = lock.unlock();
        let session_no = self.session_no;
        let idle: Vec<(String, u64)> = self
            .alive()
            .filter(|t| t.born_session > 0 && t.uses < 2 && session_no.saturating_sub(t.last_session.max(t.born_session)) > IDLE_SESSIONS)
            .map(|t| (t.name.clone(), session_no - t.last_session.max(t.born_session)))
            .collect();
        for (name, n) in idle {
            self.drop_tool(&name, &format!("not used in the {n} sessions since it was saved"));
        }
    }

    pub fn alive(&self) -> impl Iterator<Item = &Tool> {
        self.tools.iter().filter(|t| t.dropped.is_none())
    }

    pub fn generation(&self) -> String {
        let mut parts: Vec<String> = self.alive().map(|t| format!("{}:{}", t.name, t.hash)).collect();
        parts.sort();
        if parts.is_empty() { "g0".into() } else { format!("g-{}", &hash_of(&parts.join(","))[..8]) }
    }

    fn current_session(&mut self) -> u64 {
        if let Some(n) = std::fs::read_to_string(self.dir.join("sessions")).ok().and_then(|t| t.trim().parse::<u64>().ok()) {
            self.session_no = n;
        }
        self.session_no.max(1)
    }

    fn on_disk(&self, name: &str) -> Option<Tool> {
        std::fs::read_to_string(self.dir.join(format!("{name}.json"))).ok().and_then(|t| serde_json::from_str(&t).ok())
    }

    fn persist(&self, tool: &Tool) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(text) = serde_json::to_string_pretty(tool) {
            let _ = std::fs::write(self.dir.join(format!("{}.json", tool.name)), text);
        }
    }

    pub fn loadable(&self) -> Value {
        if !self.enabled {
            return json!([]);
        }
        json!(self.alive().map(|t| json!({"name": t.name, "code": t.code, "check": t.check, "effects": t.effects})).collect::<Vec<_>>())
    }

    pub fn list(&self) -> Value {
        let mut alive: Vec<&Tool> = self.alive().collect();
        alive.sort_by(|a, b| rank(b).cmp(&rank(a)));
        json!(alive.iter().map(|t| json!({"name": t.name, "doc": t.doc, "uses": t.uses, "version": t.version, "anchors": t.anchors, "stale": t.stale})).collect::<Vec<_>>())
    }

    pub fn save(&mut self, a: &Value) -> Result<Value, String> {
        if !self.enabled {
            return Err("saving functions is off for this run (STEM_TOOLS=off)".into());
        }
        let name = a["name"].as_str().ok_or("$.save(name, fn, {doc, check}) needs a name")?;
        if !name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("{name} is not a valid name: use camelCase letters and digits"));
        }
        if CORE.contains(&name) {
            return Err(format!("$.{name} is a built-in; pick another name"));
        }
        let code = a["code"].as_str().ok_or("missing function source")?.to_string();
        let doc = a["doc"].as_str().filter(|d| !d.trim().is_empty()).ok_or("pass {doc: \"one line: what it returns\"}")?.to_string();
        let check = a["check"].as_str().unwrap_or("").to_string();
        let anchors: Vec<String> = a["anchors"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        if check.trim().is_empty() {
            return Err("pass {check: (fn) => ...}: a self-check that calls the function and returns true when the result is right".into());
        }
        let session = self.current_session();
        let effects = effects_of(&code);
        let hash = hash_of(&code);
        let now = crate::events::now_ms();
        let replaced = self.tools.iter().position(|t| t.name == name);
        let (uses, born, version, previous) = match replaced {
            Some(i) => {
                let old = &self.tools[i];
                let mut kept = old.clone();
                kept.previous = None;
                (old.uses, old.born, old.version + 1, (old.dropped.is_none() && old.stale.is_none()).then(|| Box::new(kept)))
            }
            None => (0, now, 1, None),
        };
        let tool = Tool { name: name.into(), doc, code, check, effects: effects.clone(), hash: hash.clone(), born, last_used: now, uses, fails: 0, dropped: None, version, anchors: anchors.clone(), sizes: Vec::new(), stale: None, stale_uses: 0, born_session: session, last_session: session, previous };
        self.persist(&tool);
        match replaced {
            Some(i) => self.tools[i] = tool,
            None => self.tools.push(tool),
        }
        self.events.push(("tool_born".into(), json!({"name": name, "hash": hash, "effects": effects, "version": version, "anchors": anchors})));
        Ok(json!({"saved": name, "version": version, "anchors": anchors, "effects": effects, "generation": self.generation(), "note": format!("$.{name} works now and in every later session. It is marked stale if its anchors leave the code or it starts returning nothing.")}))
    }

    pub fn used(&mut self, name: &str, ok: bool, size: Option<u64>, chars: Option<u64>, missing: &[String]) -> Option<String> {
        let session_no = self.current_session();
        let i = self.tools.iter().position(|t| t.name == name)?;
        if let Some(disk) = self.on_disk(name) {
            let elsewhere = disk.dropped.is_some() || disk.version != self.tools[i].version || disk.hash != self.tools[i].hash;
            self.tools[i] = disk;
            if elsewhere {
                return None;
            }
        }
        let t = &mut self.tools[i];
        t.last_used = crate::events::now_ms();
        t.last_session = session_no;
        if ok {
            t.uses += 1;
        } else {
            t.fails += 1;
        }
        let mut note = None;
        if ok {
            let usual = median(&t.sizes);
            let reason = if !missing.is_empty() {
                Some(format!("{} {} no longer in the code", missing.join(", "), if missing.len() == 1 { "is" } else { "are" }))
            } else if size == Some(0) && usual >= 2 {
                Some(format!("returned nothing; it used to return about {usual}"))
            } else {
                None
            };
            if let Some(s) = size {
                t.sizes.push(s);
                if t.sizes.len() > 8 {
                    t.sizes.remove(0);
                }
            }
            match reason {
                Some(r) => {
                    if t.stale.is_none() {
                        self.events.push(("tool_stale".into(), json!({"name": name, "reason": r})));
                    }
                    t.stale = Some(r.clone());
                    note = (!self.noted.iter().any(|n| n == name)).then(|| format!("[stem: $.{name} may be stale: {r}. If the feature moved, save a fixed version under the same name; if it is gone, $.forget(\"{name}\", why).]"));
                }
                None => t.stale = None,
            }
        }
        if t.stale.is_some() {
            t.stale_uses += 1;
        } else {
            t.stale_uses = 0;
        }
        if note.is_none() && chars.is_some_and(|c| c > BIG_RESULT) && !self.noted.iter().any(|n| n == name) {
            note = Some(format!("[stem: $.{name} returned {} characters, more than a task needs to start. Save a trimmed version under the same name: fewer lines, or narrow by topic.]", chars.unwrap_or(0)));
        }
        let tool = t.clone();
        if note.is_some() {
            self.noted.push(name.to_string());
        }
        self.persist(&tool);
        self.events.push(("tool_used".into(), json!({"name": name, "ok": ok, "size": size, "version": tool.version})));
        if tool.stale_uses >= 5 {
            self.drop_tool(name, &format!("stale for {} uses and never fixed: {}", tool.stale_uses, tool.stale.clone().unwrap_or_default()));
        }
        if !ok && tool.fails >= 3 && tool.fails * 2 > tool.uses {
            match tool.previous.clone() {
                Some(prev) => self.restore(name, *prev),
                None => self.drop_tool(name, &format!("failed {} of {} uses", tool.fails, tool.fails + tool.uses)),
            }
        }
        note
    }

    fn restore(&mut self, name: &str, prev: Tool) {
        let Some(i) = self.tools.iter().position(|t| t.name == name) else { return };
        let failed = self.tools[i].version;
        self.tools[i] = prev;
        let tool = self.tools[i].clone();
        self.persist(&tool);
        self.events.push(("tool_restored".into(), json!({"name": name, "from": failed, "to": tool.version})));
        self.notes.push(format!("[stem: $.{name} version {failed} kept failing, so version {} is back.]", tool.version));
    }

    pub fn anchors(&self, name: &str) -> Vec<String> {
        self.alive().find(|t| t.name == name).map(|t| t.anchors.clone()).unwrap_or_default()
    }

    pub fn drop_tool(&mut self, name: &str, reason: &str) {
        let disk = self.on_disk(name);
        let Some(t) = self.tools.iter_mut().find(|t| t.name == name) else { return };
        if let Some(d) = disk.filter(|d| d.hash != t.hash || d.dropped.is_some()) {
            *t = d;
            return;
        }
        t.dropped = Some(reason.to_string());
        let tool = t.clone();
        self.persist(&tool);
        self.events.push(("tool_dropped".into(), json!({"name": name, "reason": reason})));
    }

    pub fn checked(&mut self, name: &str, ok: bool, detail: &str) -> Option<String> {
        self.events.push(("tool_checked".into(), json!({"name": name, "ok": ok, "detail": detail})));
        if ok {
            return None;
        }
        let t = self.tools.iter_mut().find(|t| t.name == name)?;
        if detail.starts_with("does not load") {
            let reason = detail.to_string();
            self.drop_tool(name, &reason);
            return None;
        }
        let reason = format!("its self-check fails now ({detail})");
        if t.stale.is_none() {
            self.events.push(("tool_stale".into(), json!({"name": name, "reason": reason})));
        }
        t.stale = Some(reason.clone());
        let tool = t.clone();
        self.persist(&tool);
        self.noted.push(name.to_string());
        Some(format!("[stem: $.{name} may be stale: {reason}. Its result may be incomplete. If the feature moved, save a fixed version under the same name; if it is gone, $.forget(\"{name}\", why).]"))
    }

    pub fn prompt_lines(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let mut alive: Vec<&Tool> = self.alive().collect();
        alive.sort_by(|a, b| rank(b).cmp(&rank(a)));
        let more = alive.len().saturating_sub(SHOWN);
        let mut shown: Vec<&Tool> = alive.into_iter().take(SHOWN).collect();
        shown.sort_by(|a, b| a.name.cmp(&b.name));
        let mut lines: Vec<String> = shown
            .iter()
            .map(|t| {
                let params = t.code.split("=>").next().unwrap_or("").trim().trim_start_matches("async").trim();
                let area: Vec<&str> = t.anchors.iter().filter(|a| a.contains('/')).map(String::as_str).take(2).collect();
                format!(
                    "- $.{}{}: {}{}{}{}",
                    t.name,
                    if params.starts_with('(') { params.to_string() } else { format!("({params})") },
                    t.doc,
                    if area.is_empty() { String::new() } else { format!(" Area: {}.", area.join(", ")) },
                    if t.effects.is_empty() { String::new() } else { format!(" ({})", t.effects.join(", ")) },
                    if t.stale.is_some() { " (may be stale)" } else { "" }
                )
            })
            .collect();
        if more > 0 {
            lines.push(format!("- {more} more: $.tools() lists them"));
        }
        lines
    }

    pub fn table(&self) -> Vec<String> {
        let mut rows: Vec<&Tool> = self.tools.iter().collect();
        rows.sort_by(|a, b| a.dropped.is_some().cmp(&b.dropped.is_some()).then(b.uses.cmp(&a.uses)));
        rows.iter()
            .map(|t| {
                let age = crate::events::now_ms().saturating_sub(t.born) / 86_400_000;
                match &t.dropped {
                    None => format!("{} $.{:<24} v{} · used {:>3}× · failed {:>2}× · {} d · {}{}", if t.stale.is_some() { "◐" } else { "●" }, t.name, t.version, t.uses, t.fails, age, t.doc, t.stale.as_ref().map(|s| format!(" · stale: {s}")).unwrap_or_default()),
                    Some(r) => format!("○ $.{:<24} dropped: {r}", t.name),
                }
            })
            .collect()
    }
}

fn rank(t: &Tool) -> (bool, u64, u128) {
    (t.stale.is_none(), t.uses.saturating_sub(t.fails), t.last_used)
}

fn median(sizes: &[u64]) -> u64 {
    let mut s = sizes.to_vec();
    s.sort();
    s.get(s.len() / 2).copied().unwrap_or(0)
}

pub fn anchors_of(code: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' && c != '\'' && c != '`' {
            continue;
        }
        let mut lit = String::new();
        while let Some(n) = chars.next() {
            if n == '\\' {
                chars.next();
            } else if n == c {
                break;
            } else {
                lit.push(n);
            }
        }
        for word in lit.split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '/' || ch == '.')) {
            let word = word.trim_matches(|ch: char| ch == '.' || ch == '/' || ch == '-');
            let has_letters = word.chars().filter(|ch| ch.is_alphabetic()).count() >= 4;
            if has_letters && !word.contains('*') && !out.iter().any(|w| w == word) && out.len() < 6 {
                out.push(word.to_string());
            }
        }
    }
    out
}
