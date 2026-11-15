use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum Calls {
    One(String),
    Many(Vec<String>),
}

impl Calls {
    fn matches(&self, call: &str) -> bool {
        match self {
            Calls::One(c) => c == "*" || c == call,
            Calls::Many(cs) => cs.iter().any(|c| c == "*" || c == call),
        }
    }
}

#[derive(Deserialize, Clone)]
struct Match {
    call: Calls,
    #[serde(default)]
    pattern: Option<String>,
}

#[derive(Deserialize, Clone)]
struct Rewrite {
    from: String,
    to: String,
}

#[derive(Deserialize, Clone)]
struct Redirect {
    call: String,
    #[serde(default)]
    args: Value,
}

#[derive(Deserialize, Clone)]
struct RawRule {
    name: String,
    #[serde(rename = "match")]
    when: Match,
    action: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    rewrite: Option<Rewrite>,
    #[serde(default)]
    redirect: Option<Redirect>,
    #[serde(default)]
    note: Option<String>,
}

struct Rule {
    raw: RawRule,
    pattern: Option<Regex>,
    rewrite_regex: Option<Regex>,
    source: &'static str,
}

pub enum Outcome {
    Run { name: String, args: Value, note: Option<String> },
    Deny(String),
}

pub struct Rules {
    rules: Vec<Rule>,
    pub problems: Vec<String>,
}

fn subject_key(call: &str) -> Option<&'static str> {
    match call {
        "sh" => Some("cmd"),
        "read" | "write" | "edit" | "remove" | "outline" | "diagnostics" => Some("path"),
        "grep" => Some("pattern"),
        "test" => Some("filter"),
        "run" => Some("script"),
        "sql" => Some("query"),
        _ => None,
    }
}

fn subject(call: &str, args: &Value) -> String {
    if call == "remove" {
        if let Some(list) = args["paths"].as_array() {
            return list.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" ");
        }
    }
    subject_key(call).and_then(|k| args[k].as_str()).unwrap_or("").to_string()
}

fn fill(value: &Value, caps: &regex::Captures) -> Value {
    match value {
        Value::String(s) => {
            let mut out = String::new();
            caps.expand(s, &mut out);
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, caps)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), fill(v, caps))).collect()),
        other => other.clone(),
    }
}

const SHELL_PATTERN: &str = r#"(?:"([^"]+)"|'([^']+)'|([^\s'"|;&<>$`*?-][^\s|;&<>`]*))"#;
const SHELL_PATH: &str = r#"([^\s'"|;&<>$`*?\[-][^\s|;&<>`*?\[]*)"#;

fn built_in() -> Vec<Rule> {
    let search = |name: &str, flags: &str, ignore_case: bool| RawRule {
        name: name.to_string(),
        when: Match { call: Calls::One("sh".into()), pattern: Some(format!(r"^\s*(?:git\s+grep|grep|rg)({flags})\s+{SHELL_PATTERN}(?:\s+{SHELL_PATH})?\s*$")) },
        action: "redirect".into(),
        reason: None,
        rewrite: None,
        redirect: Some(Redirect { call: "grep".into(), args: json!({"pattern": "$2$3$4", "path": "$5", "ignoreCase": ignore_case}) }),
        note: Some("ran $.grep instead of a shell search: it skips ignored files and returns rows".into()),
    };
    let read = RawRule {
        name: "shell-read".into(),
        when: Match { call: Calls::One("sh".into()), pattern: Some(format!(r"^\s*cat\s+{SHELL_PATH}\s*$")) },
        action: "redirect".into(),
        reason: None,
        rewrite: None,
        redirect: Some(Redirect { call: "read".into(), args: json!({"path": "$1"}) }),
        note: Some("ran $.read instead of cat".into()),
    };
    [
        search("shell-search", r"(?:\s+-[rRnHsE]+)*\s+-[rRnHsE]*i[rRnHisE]*(?:\s+-[rRnHisE]+)*", true),
        search("shell-search", r"(?:\s+-[rRnHsE]+)*", false),
        read,
    ]
    .into_iter()
    .filter_map(|raw| {
        let pattern = Regex::new(raw.when.pattern.as_deref().unwrap_or_default()).ok()?;
        Some(Rule { raw, pattern: Some(pattern), rewrite_regex: None, source: "built-in" })
    })
    .collect()
}

const DENY_REASON_CHARS: usize = 200;
const DENY_NAME_CHARS: usize = 60;

fn project_may(rule: &RawRule) -> bool {
    rule.action == "deny"
}

impl Rules {
    pub fn load(root: &Path) -> Rules {
        let mut rules = Vec::new();
        let mut problems = Vec::new();
        let sources: [(Option<std::path::PathBuf>, &'static str); 2] = [(Some(root.join(".stem").join("rules.json")), "project"), (Some(crate::env::stem_home().join("rules.json")), "user")];
        for (path, source) in sources {
            let Some(path) = path else { continue };
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let raw: Vec<RawRule> = match serde_json::from_str::<Value>(&text).map(|v| v.get("rules").cloned().unwrap_or(v)).and_then(serde_json::from_value) {
                Ok(r) => r,
                Err(e) => {
                    problems.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            for r in raw {
                if !matches!(r.action.as_str(), "allow" | "deny" | "rewrite" | "redirect") {
                    problems.push(format!("{}: rule {} has unknown action {}", path.display(), r.name, r.action));
                    continue;
                }
                if source == "project" && r.action == "allow" {
                    problems.push(format!("{}: rule {} ignored: a project's own rules cannot allow; put allow rules in ~/.stem/rules.json", path.display(), r.name));
                    continue;
                }
                if source == "project" && !project_may(&r) {
                    problems.push(format!("{}: rule {} ignored: a project's own rules can only deny a call; put other rules in ~/.stem/rules.json", path.display(), r.name));
                    continue;
                }
                if (r.action == "rewrite" && r.rewrite.is_none()) || (r.action == "redirect" && r.redirect.is_none()) {
                    problems.push(format!("{}: rule {} needs a {} field", path.display(), r.name, r.action));
                    continue;
                }
                let pattern = match r.when.pattern.as_deref().map(Regex::new).transpose() {
                    Ok(p) => p,
                    Err(e) => {
                        problems.push(format!("{}: rule {} pattern: {e}", path.display(), r.name));
                        continue;
                    }
                };
                let rewrite_regex = if r.action == "rewrite" {
                    if let Some(rw) = &r.rewrite {
                        match Regex::new(&rw.from) {
                            Ok(re) => Some(re),
                            Err(e) => {
                                problems.push(format!("{}: rule {} rewrite pattern: {e}", path.display(), r.name));
                                continue;
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                rules.push(Rule { raw: r, pattern, rewrite_regex, source });
            }
        }
        rules.extend(built_in());
        Rules { rules, problems }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn apply(&self, call: &str, args: &Value) -> (Outcome, Option<Value>) {
        let text = subject(call, args);
        for rule in &self.rules {
            if !rule.raw.when.call.matches(call) {
                continue;
            }
            let caps = match &rule.pattern {
                Some(p) => match p.captures(&text) {
                    Some(c) => Some(c),
                    None => continue,
                },
                None => None,
            };
            let hit = json!({"rule": rule.raw.name, "source": rule.source, "call": call, "action": rule.raw.action});
            let note = rule.raw.note.clone();
            let outcome = match rule.raw.action.as_str() {
                "deny" => {
                    let reason: String = rule.raw.reason.clone().unwrap_or_else(|| "not allowed here".into()).chars().take(DENY_REASON_CHARS).collect();
                    let whose = if rule.source == "project" { "this project's rule" } else { "rule" };
                    let name: String = rule.raw.name.chars().take(DENY_NAME_CHARS).collect();
                    Outcome::Deny(format!("refused by {whose} {name}: {reason}"))
                }
                "rewrite" => {
                    let rw = rule.raw.rewrite.as_ref().expect("checked at load");
                    let mut next = args.clone();
                    if let (Some(key), Some(from)) = (subject_key(call), &rule.rewrite_regex) {
                        next[key] = json!(from.replace_all(&text, rw.to.as_str()).into_owned());
                    }
                    let shown = format!("rule {} changed this call", rule.raw.name);
                    Outcome::Run { name: call.to_string(), args: next, note: Some(note.unwrap_or(shown)) }
                }
                "redirect" => {
                    let rd = rule.raw.redirect.as_ref().expect("checked at load");
                    let filled = match &caps {
                        Some(c) => fill(&rd.args, c),
                        None => rd.args.clone(),
                    };
                    let shown = format!("rule {} ran $.{} instead of $.{call}", rule.raw.name, rd.call);
                    Outcome::Run { name: rd.call.clone(), args: filled, note: Some(note.unwrap_or(shown)) }
                }
                _ => Outcome::Run { name: call.to_string(), args: args.clone(), note },
            };
            return (outcome, Some(hit));
        }
        (Outcome::Run { name: call.to_string(), args: args.clone(), note: None }, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calls_matches_zero_alloc() {
        let one = Calls::One("sh".into());
        assert!(one.matches("sh"));
        assert!(!one.matches("read"));

        let star = Calls::One("*".into());
        assert!(star.matches("sh"));
        assert!(star.matches("anything"));

        let many = Calls::Many(vec!["read".into(), "write".into()]);
        assert!(many.matches("read"));
        assert!(many.matches("write"));
        assert!(!many.matches("sh"));

        let many_star = Calls::Many(vec!["read".into(), "*".into()]);
        assert!(many_star.matches("random"));
    }

    #[test]
    fn test_builtin_redirect() {
        let rules = Rules { rules: built_in(), problems: Vec::new() };
        let (outcome, hit) = rules.apply("sh", &json!({"cmd": "cat README.md"}));
        assert!(hit.is_some());
        match outcome {
            Outcome::Run { name, args, .. } => {
                assert_eq!(name, "read");
                assert_eq!(args["path"], "README.md");
            }
            Outcome::Deny(_) => panic!("expected redirect, got deny"),
        }
    }

    #[test]
    fn test_precompiled_rewrite_rule() {
        let rule = Rule {
            raw: RawRule {
                name: "rewrite-test".into(),
                when: Match { call: Calls::One("sh".into()), pattern: None },
                action: "rewrite".into(),
                reason: None,
                rewrite: Some(Rewrite { from: r"foo(\d+)".into(), to: r"bar$1".into() }),
                redirect: None,
                note: None,
            },
            pattern: None,
            rewrite_regex: Some(Regex::new(r"foo(\d+)").unwrap()),
            source: "user",
        };
        let rules = Rules { rules: vec![rule], problems: Vec::new() };
        let (outcome, hit) = rules.apply("sh", &json!({"cmd": "echo foo123"}));
        assert!(hit.is_some());
        match outcome {
            Outcome::Run { name, args, .. } => {
                assert_eq!(name, "sh");
                assert_eq!(args["cmd"], "echo bar123");
            }
            Outcome::Deny(_) => panic!("expected rewrite run"),
        }
    }
}
