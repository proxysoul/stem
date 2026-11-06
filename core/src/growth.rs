use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const CORE: &[&str] = &[
    "read", "write", "edit", "remove", "files", "grep", "sh", "project", "test", "run", "defs", "refs", "callers", "impls", "typeAt", "hover", "rename",
    "symbols", "outline", "diagnostics", "ask", "sql", "plan", "done", "save", "tools", "forget",
];
const OFFER_AT: u64 = 3;
const UNUSED_DAYS: u128 = 21;

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
}

pub struct Growth {
    dir: PathBuf,
    pub enabled: bool,
    pub tools: Vec<Tool>,
    shapes: HashMap<String, (u64, String)>,
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

pub fn shape_of(code: &str) -> String {
    let mut out = String::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' | '`' => {
                let quote = c;
                while let Some(n) = chars.next() {
                    if n == '\\' {
                        chars.next();
                    } else if n == quote {
                        break;
                    }
                }
                out.push('S');
            }
            c if c.is_ascii_digit() => {
                while chars.peek().is_some_and(|n| n.is_ascii_digit() || *n == '.') {
                    chars.next();
                }
                out.push('N');
            }
            c if c.is_whitespace() => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

impl Growth {
    pub fn open(root: &Path) -> Growth {
        let dir = root.join(".stem").join("tools");
        let enabled = std::env::var("STEM_TOOLS").map(|v| v != "off" && v != "0").unwrap_or(true);
        let mut tools: Vec<Tool> = Vec::new();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.path().extension().is_some_and(|e| e == "json") && entry.file_name() != "shapes.json" {
                if let Some(t) = std::fs::read_to_string(entry.path()).ok().and_then(|t| serde_json::from_str::<Tool>(&t).ok()) {
                    tools.push(t);
                }
            }
        }
        let shapes = std::fs::read_to_string(dir.join("shapes.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let mut g = Growth { dir, enabled, tools, shapes, events: Vec::new() };
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

    pub fn alive(&self) -> impl Iterator<Item = &Tool> {
        self.tools.iter().filter(|t| t.dropped.is_none())
    }

    pub fn generation(&self) -> String {
        let mut parts: Vec<String> = self.alive().map(|t| format!("{}:{}", t.name, t.hash)).collect();
        parts.sort();
        if parts.is_empty() { "g0".into() } else { format!("g-{}", &hash_of(&parts.join(","))[..8]) }
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
        if check.trim().is_empty() {
            return Err("pass {check: (fn) => ...}: a self-check that calls the function and returns true when the result is right".into());
        }
        let effects = effects_of(&code);
        let hash = hash_of(&code);
        let now = crate::events::now_ms();
        let replaced = self.tools.iter().position(|t| t.name == name);
        let (uses, born) = replaced.map(|i| (self.tools[i].uses, self.tools[i].born)).unwrap_or((0, now));
        let tool = Tool { name: name.into(), doc, code, check, effects: effects.clone(), hash: hash.clone(), born, last_used: now, uses, fails: 0, dropped: None };
        self.persist(&tool);
        match replaced {
            Some(i) => self.tools[i] = tool,
            None => self.tools.push(tool),
        }
        self.events.push(("tool_born".into(), json!({"name": name, "hash": hash, "effects": effects, "replaced": replaced.is_some()})));
        Ok(json!({"saved": name, "effects": effects, "generation": self.generation(), "note": format!("$.{name} works now in this session and is listed in every future session")}))
    }

    pub fn used(&mut self, name: &str, ok: bool) {
        let Some(t) = self.tools.iter_mut().find(|t| t.name == name) else { return };
        t.last_used = crate::events::now_ms();
        if ok {
            t.uses += 1;
        } else {
            t.fails += 1;
        }
        let tool = t.clone();
        self.persist(&tool);
        self.events.push(("tool_used".into(), json!({"name": name, "ok": ok})));
        if !ok && tool.fails >= 3 && tool.fails * 2 > tool.uses {
            self.drop_tool(name, &format!("failed {} of {} uses", tool.fails, tool.fails + tool.uses));
        }
    }

    pub fn drop_tool(&mut self, name: &str, reason: &str) {
        let Some(t) = self.tools.iter_mut().find(|t| t.name == name) else { return };
        t.dropped = Some(reason.to_string());
        let tool = t.clone();
        self.persist(&tool);
        self.events.push(("tool_dropped".into(), json!({"name": name, "reason": reason})));
    }

    pub fn checked(&mut self, name: &str, ok: bool, detail: &str) {
        self.events.push(("tool_checked".into(), json!({"name": name, "ok": ok, "detail": detail})));
        if !ok {
            self.drop_tool(name, &format!("self-check failed: {detail}"));
        }
    }

    pub fn observe(&mut self, code: &str) -> Option<String> {
        if !self.enabled || code.len() < 60 || code.contains("$.save(") {
            return None;
        }
        let shape = shape_of(code);
        let key = hash_of(&shape);
        let entry = self.shapes.entry(key).or_insert((0, code.chars().take(300).collect()));
        entry.0 += 1;
        let count = entry.0;
        let _ = std::fs::create_dir_all(&self.dir);
        if self.shapes.len() > 2000 {
            self.shapes.retain(|_, v| v.0 > 1);
        }
        if let Ok(text) = serde_json::to_string(&self.shapes) {
            let _ = std::fs::write(self.dir.join("shapes.json"), text);
        }
        (count == OFFER_AT || (count > OFFER_AT && count % 5 == 0)).then(|| {
            format!("[stem: this project has run a script of this same shape {count} times. If it will come back, save it once as a function: $.save(\"name\", (args) => {{ ... }}, {{doc: \"what it returns\", check: (f) => /* call f and return true if right */}})]")
        })
    }

    pub fn prompt_lines(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let mut alive: Vec<&Tool> = self.alive().collect();
        alive.sort_by(|a, b| b.uses.cmp(&a.uses).then(b.last_used.cmp(&a.last_used)));
        alive
            .iter()
            .take(12)
            .map(|t| {
                let params = t.code.split("=>").next().unwrap_or("").trim().trim_start_matches("async").trim();
                format!(
                    "- $.{}{}: {} (used {}×{})",
                    t.name,
                    if params.starts_with('(') { params.to_string() } else { format!("({params})") },
                    t.doc,
                    t.uses,
                    if t.effects.is_empty() { String::new() } else { format!(", {}", t.effects.join(", ")) }
                )
            })
            .collect()
    }

    pub fn table(&self) -> Vec<String> {
        let mut rows: Vec<&Tool> = self.tools.iter().collect();
        rows.sort_by(|a, b| a.dropped.is_some().cmp(&b.dropped.is_some()).then(b.uses.cmp(&a.uses)));
        rows.iter()
            .map(|t| {
                let age = crate::events::now_ms().saturating_sub(t.born) / 86_400_000;
                match &t.dropped {
                    None => format!("● $.{:<24} used {:>3}× · failed {:>2}× · {} d · {}", t.name, t.uses, t.fails, age, t.doc),
                    Some(r) => format!("○ $.{:<24} dropped: {r}", t.name),
                }
            })
            .collect()
    }
}
