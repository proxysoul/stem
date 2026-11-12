use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const LOOKING: &[&str] = &[
    "ls", "cat", "sed", "grep", "rg", "head", "tail", "wc", "find", "echo", "cd", "pwd", "stat", "date", "sleep", "ps", "pgrep", "lsof", "which", "file", "du", "df", "mkdir", "cp", "mv", "rm", "touch", "printf", "curl", "open", "kill", "pkill", "for", "while", "if",
    "test", "[", "export", "source", "true", "false", "diff", "tree", "less", "more", "awk", "sort", "uniq", "cut", "tr", "xargs", "env", "set", "git", "gh", "chmod", "ln", "tar", "unzip", "jq", "tee", "time", "nohup", "wait", "read", "exit", "return",
];
const INLINE_FLAGS: &[&str] = &["-e", "-c", "-p", "--eval", "--print"];
const SHELL_WORDS: &[&str] = &["do", "done", "then", "else", "elif", "fi", "esac", "case", "in", "function", "{", "}", "(", ")"];
const READS_ONLY: &[&str] = &[
    "ls", "cat", "sed", "grep", "rg", "head", "tail", "wc", "find", "echo", "cd", "pwd", "stat", "date", "sleep", "ps", "pgrep", "lsof", "which", "file", "du", "df", "printf", "test", "[", "true", "false", "diff", "tree", "less", "more", "awk", "sort", "uniq",
    "cut", "tr", "jq", "env", "time", "wait", "read", "for", "while", "if", "export", "set",
];
const GIT_READS: &[&str] = &["grep", "log", "show", "diff", "status", "blame", "ls-files", "rev-parse", "check-ignore", "describe", "count-objects", "ls-remote", "cat-file", "shortlog", "reflog"];
const GH_READS: &[&str] = &["view", "list", "status", "diff", "checks", "watch"];

fn parts_of(cmd: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == ';' || c == '\n' || (c == '&' && chars.get(i + 1) == Some(&'&')) || (c == '|' && chars.get(i + 1) == Some(&'|')) => {
                parts.push(std::mem::take(&mut cur));
                i += if c == '&' || c == '|' { 2 } else { 1 };
                continue;
            }
            None => {}
        }
        cur.push(c);
        i += 1;
    }
    parts.push(cur);
    if quote.is_some() {
        return Vec::new();
    }
    parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

fn first_words(part: &str) -> Vec<&str> {
    let mut words: Vec<&str> = part.split_whitespace().collect();
    while words.first().is_some_and(|w| w.contains('=') && !w.starts_with('-')) {
        words.remove(0);
    }
    if words.first() == Some(&"timeout") && words.len() > 2 {
        words.drain(..2);
    }
    words
}

pub fn changes_things(cmd: &str) -> bool {
    let parts = parts_of(cmd);
    parts.is_empty()
        || parts.iter().any(|part| {
            let words = first_words(cut_at(part, &[" | "]));
            let wrote = part.contains(" > ") || part.contains(" >> ") || words.contains(&"-i");
            match words.first().copied() {
                None => false,
                Some("git") => wrote || !words.get(1).is_some_and(|sub| GIT_READS.contains(sub)),
                Some("gh") => wrote || !words.get(2).is_some_and(|verb| GH_READS.contains(verb)),
                Some(first) => wrote || !(READS_ONLY.contains(&first) || SHELL_WORDS.contains(&first)),
            }
        })
}
const MAX_COMMAND: usize = 120;
const MAX_AREAS: usize = 80;
const MAX_CHECKS: usize = 12;
const HABIT_MIN_RUNS: u32 = 3;
const HABIT_MIN_SHARE: f64 = 0.3;
const HABITS_SHOWN: usize = 3;
const PLACEHOLDER: &str = "<file>";

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Check {
    pub runs: u32,
    pub passed: u32,
    pub last: u64,
    #[serde(default)]
    pub before: u32,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Area {
    pub turns: u32,
    pub checks: BTreeMap<String, Check>,
}

pub type Habits = BTreeMap<String, Area>;

pub fn area(path: &str) -> String {
    let parts: Vec<&str> = path.trim_start_matches("./").split('/').filter(|p| !p.is_empty()).collect();
    let keep = parts.len().saturating_sub(1).min(2);
    if keep == 0 { ".".to_string() } else { parts[..keep].join("/") }
}

fn cut_at<'a>(text: &'a str, marks: &[&str]) -> &'a str {
    marks.iter().filter_map(|m| text.find(m)).min().map(|at| &text[..at]).unwrap_or(text)
}

fn one_check(part: &str, edited: &[String]) -> Option<String> {
    let part = cut_at(part, &[" | ", " 2>", " >", " <"]).trim();
    let mut words = first_words(part);
    if words.first() == Some(&"bun") && words.get(1) == Some(&"x") {
        words.drain(..2);
        words.insert(0, "bunx");
    }
    let first = words.first()?;
    let command_like = first.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c));
    if !command_like || SHELL_WORDS.contains(first) {
        return None;
    }
    let scratch = |w: &str| w.starts_with("/tmp") || w.starts_with("/private/") || w.starts_with('~') || w.starts_with('$') || w.contains("/tmp/");
    if LOOKING.contains(first) || words.iter().any(|w| INLINE_FLAGS.contains(w) || scratch(w)) {
        return None;
    }
    let named = |w: &str| {
        let w = w.trim_start_matches("./");
        (w.contains('.') || w.contains('/')) && edited.iter().any(|e| e == w || e.ends_with(&format!("/{w}")))
    };
    let mut shown: Vec<&str> = Vec::new();
    for w in words {
        let next = if named(w) { PLACEHOLDER } else { w };
        if !(next == PLACEHOLDER && shown.last() == Some(&PLACEHOLDER)) {
            shown.push(next);
        }
    }
    let command = shown.join(" ");
    (command.len() <= MAX_COMMAND && !command.contains(['`', '\n'])).then_some(command)
}

fn still_a_command(check: &str) -> bool {
    let body = check.split_once(" && ").filter(|(lead, _)| lead.starts_with("cd ")).map(|(_, rest)| rest).unwrap_or(check);
    let words: Vec<&str> = body.split_whitespace().collect();
    let Some(first) = words.first() else { return false };
    let spelled = first.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c));
    spelled && !SHELL_WORDS.contains(first) && !LOOKING.contains(first) && !(*first == "bun" && words.get(1) == Some(&"x"))
}

pub fn checks_in(cmd: &str, edited: &[String]) -> Vec<String> {
    if cmd.contains("${") || cmd.contains("$(") {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut place: Option<String> = None;
    for part in parts_of(cmd) {
        let part = part.as_str();
        if let Some(dir) = part.strip_prefix("cd ") {
            place = Some(dir.trim().to_string()).filter(|d| !d.starts_with('/') && !d.starts_with('~') && !d.starts_with('$'));
            continue;
        }
        let Some(check) = one_check(part, edited) else { continue };
        let full = match &place {
            Some(dir) => format!("cd {dir} && {check}"),
            None => check,
        };
        if full.len() <= MAX_COMMAND && !out.contains(&full) {
            out.push(full);
        }
    }
    out
}

pub fn learn(habits: &mut Habits, changed: &[String], ran: &[(String, Option<bool>)], ran_before: &[String], session: u64) {
    let mut early: Vec<String> = Vec::new();
    for cmd in ran_before {
        for check in checks_in(cmd, changed) {
            if !early.contains(&check) {
                early.push(check);
            }
        }
    }
    let mut areas: Vec<String> = changed.iter().map(|f| area(f)).collect();
    areas.sort();
    areas.dedup();
    let mut checks: Vec<(String, bool)> = Vec::new();
    for (cmd, ok) in ran {
        for check in checks_in(cmd, changed) {
            match checks.iter_mut().find(|(c, _)| *c == check) {
                Some(seen) => seen.1 |= *ok == Some(true),
                None => checks.push((check, *ok == Some(true))),
            }
        }
    }
    for name in areas {
        let entry = habits.entry(name).or_default();
        entry.checks.retain(|cmd, _| still_a_command(cmd));
        entry.turns += 1;
        for (check, passed) in &checks {
            let c = entry.checks.entry(check.clone()).or_default();
            c.runs += 1;
            c.passed += u32::from(*passed);
            c.last = session;
        }
        for check in &early {
            if let Some(c) = entry.checks.get_mut(check) {
                c.before += 1;
            }
        }
        while entry.checks.len() > MAX_CHECKS {
            let Some(weakest) = entry.checks.iter().min_by_key(|(_, c)| (c.runs, c.last)).map(|(k, _)| k.clone()) else { break };
            entry.checks.remove(&weakest);
        }
    }
    while habits.len() > MAX_AREAS {
        let Some(quietest) = habits.iter().min_by_key(|(_, a)| a.turns).map(|(k, _)| k.clone()) else { break };
        habits.remove(&quietest);
    }
}

pub fn usual(area: &Area) -> Vec<(&String, &Check)> {
    let mut found: Vec<(&String, &Check)> = area.checks.iter().filter(|(_, c)| c.runs >= HABIT_MIN_RUNS && c.before * 2 <= c.runs && f64::from(c.runs) / f64::from(area.turns.max(1)) >= HABIT_MIN_SHARE).collect();
    found.sort_by(|a, b| b.1.runs.cmp(&a.1.runs).then(b.1.last.cmp(&a.1.last)));
    found.truncate(HABITS_SHOWN);
    found
}

pub fn note(name: &str, area: &Area) -> Option<String> {
    let found = usual(area);
    if found.is_empty() {
        return None;
    }
    let list = found.iter().map(|(cmd, c)| format!("`{cmd}` ({} of {} turns)", c.runs, area.turns)).collect::<Vec<_>>().join(", ");
    Some(format!("after changes in {name}, earlier turns in this project checked their work with: {list}. Run the ones that fit this change before you answer."))
}

pub fn summary(habits: &Habits) -> Value {
    let mut areas: Vec<(&String, &Area)> = habits.iter().filter(|(_, a)| !usual(a).is_empty()).collect();
    areas.sort_by(|a, b| b.1.turns.cmp(&a.1.turns));
    json!(areas.iter().take(8).map(|(name, a)| json!({"area": name, "turns": a.turns, "checks": usual(a).iter().map(|(cmd, c)| json!({"cmd": cmd, "runs": c.runs, "passed": c.passed})).collect::<Vec<_>>()})).collect::<Vec<_>>())
}
