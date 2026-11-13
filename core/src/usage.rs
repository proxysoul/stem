use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FIELDS: &[&str] = &["model", "effort", "inputTokens", "outputTokens", "cacheRead", "cacheWrite", "cost", "steps", "toolOutputChars", "repeatReads", "durationMs"];
const MAX_BYTES: u64 = 4_000_000;
const KEEP_LINES: usize = 4_000;
const FOLLOWABLE: &[&str] = &["familiar", "regress", "answer", "habits", "resume", "trail"];
const UNNOTED: &str = "none";
pub const EARNING: &[&str] = &["familiar", "regress", "answer", "habits", "resume"];
const SHOWN_UNTIL_JUDGED: &[&str] = &["regress", "answer", "resume"];
const PROVEN_MIN_TURNS: usize = 10;
const PROVEN_MIN_RATE: f64 = 0.5;
const JUDGED_ON_LAST: usize = 30;
const ALWAYS_SHOWN: &[&str] = &["trail"];

#[derive(Default, Clone, Copy)]
pub struct Record {
    pub turns: usize,
    pub followed: usize,
}

pub type Records = BTreeMap<String, Record>;

fn rows(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(path(root)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn strings(list: &Value) -> Vec<String> {
    list.as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn tally(rows: &[Value]) -> Records {
    let mut records = Records::new();
    for row in rows.iter().rev() {
        let followed = strings(&row["followed"]);
        for kind in kinds_of(row).into_iter().chain(strings(&row["withheld"])) {
            let record = records.entry(kind.clone()).or_default();
            if record.turns >= JUDGED_ON_LAST {
                continue;
            }
            record.turns += 1;
            record.followed += usize::from(followed.contains(&kind));
        }
    }
    records
}

pub fn records(root: &Path) -> Records {
    tally(&rows(root))
}

pub fn proven(records: &Records, kind: &str) -> bool {
    if !EARNING.contains(&kind) {
        return true;
    }
    let record = records.get(kind).copied().unwrap_or_default();
    if record.turns < PROVEN_MIN_TURNS {
        return SHOWN_UNTIL_JUDGED.contains(&kind);
    }
    record.followed as f64 / record.turns as f64 >= PROVEN_MIN_RATE
}

fn state(records: &Records, kind: &str) -> &'static str {
    let judged = records.get(kind).is_some_and(|r| r.turns >= PROVEN_MIN_TURNS);
    match (judged, proven(records, kind)) {
        (true, true) => "proven",
        (false, true) => "shown until judged",
        (true, false) => "withheld",
        (false, false) => "probation",
    }
}

pub fn shows(records: &Records, kind: &str) -> bool {
    match kind {
        "lessons" => proven(records, "familiar"),
        other => proven(records, other),
    }
}

pub struct Turn<'a> {
    pub session: &'a str,
    pub task: Option<u64>,
    pub origin: &'a str,
    pub kinds: &'a [String],
    pub withheld: &'a [String],
    pub followed: &'a [String],
    pub shown: bool,
    pub finished: bool,
    pub usage: &'a Value,
    pub rule_hits: usize,
}

fn path(root: &Path) -> PathBuf {
    crate::env::project_home(root).join("learn").join("usage.jsonl")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn record(root: &Path, turn: &Turn) -> bool {
    use std::io::Write;
    let mut row = Map::new();
    row.insert("at".into(), json!(now_ms()));
    row.insert("session".into(), json!(turn.session));
    row.insert("task".into(), json!(turn.task));
    row.insert("origin".into(), json!(turn.origin));
    row.insert("kinds".into(), json!(turn.kinds));
    row.insert("withheld".into(), json!(turn.withheld));
    row.insert("followed".into(), json!(turn.followed));
    row.insert("shown".into(), json!(turn.shown));
    row.insert("finished".into(), json!(turn.finished));
    row.insert("ruleHits".into(), json!(turn.rule_hits));
    for field in FIELDS {
        if let Some(v) = turn.usage.get(*field).filter(|v| v.is_number() || v.is_string()) {
            row.insert((*field).into(), v.clone());
        }
    }
    let file = path(root);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > MAX_BYTES) {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let lines: Vec<&str> = text.lines().collect();
            let _ = std::fs::write(&file, format!("{}\n", lines[lines.len().saturating_sub(KEEP_LINES)..].join("\n")));
        }
    }
    std::fs::OpenOptions::new().create(true).append(true).open(&file).is_ok_and(|mut f| writeln!(f, "{}", Value::Object(row)).is_ok())
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 { values[mid] } else { (values[mid - 1] + values[mid]) / 2.0 })
}

fn fresh_tokens(row: &Value) -> Option<f64> {
    Some((row["inputTokens"].as_f64()? - row["cacheRead"].as_f64().unwrap_or(0.0)).max(0.0) + row["outputTokens"].as_f64().unwrap_or(0.0))
}

fn kinds_of(row: &Value) -> Vec<String> {
    row["kinds"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn shown(row: &Value) -> bool {
    row["shown"].as_bool().unwrap_or(true)
}

fn turn_stats(kind: &str, rows: &[&Value]) -> Value {
    let over = |pick: &dyn Fn(&Value) -> Option<f64>| median(rows.iter().filter_map(|r| pick(r)).collect());
    let mut stats = json!({
        "turns": rows.len(),
        "medianCost": over(&|r| r["cost"].as_f64()),
        "medianTokens": over(&|r| Some(r["inputTokens"].as_f64()? + r["outputTokens"].as_f64().unwrap_or(0.0))),
        "medianFreshTokens": over(&fresh_tokens),
        "medianSteps": over(&|r| r["steps"].as_f64()),
        "medianRepeatReads": over(&|r| r["repeatReads"].as_f64()),
        "ruleHits": rows.iter().filter_map(|r| r["ruleHits"].as_u64()).sum::<u64>(),
    });
    if FOLLOWABLE.contains(&kind) {
        stats["followed"] = json!(rows.iter().filter(|r| r["followed"].as_array().is_some_and(|f| f.iter().any(|k| k.as_str() == Some(kind)))).count());
    }
    stats
}

fn by_kind<'a>(rows: impl Iterator<Item = &'a Value>, kinds: &dyn Fn(&Value) -> Vec<String>) -> Value {
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for row in rows {
        for kind in kinds(row) {
            groups.entry(kind).or_default().push(row);
        }
    }
    Value::Object(groups.iter().map(|(kind, rows)| (kind.clone(), turn_stats(kind, rows))).collect())
}

struct Task {
    cost: Option<f64>,
    turns: usize,
    steps: f64,
    shown: Vec<String>,
    withheld: Vec<String>,
}

fn task_stats(tasks: &[&Task]) -> Value {
    json!({
        "tasks": tasks.len(),
        "medianCost": median(tasks.iter().filter_map(|t| t.cost).collect()),
        "medianTurns": median(tasks.iter().map(|t| t.turns as f64).collect()),
        "medianSteps": median(tasks.iter().map(|t| t.steps).collect()),
    })
}

fn tasks_by(tasks: &[Task], kinds: &dyn Fn(&Task) -> Vec<String>) -> Value {
    let mut groups: BTreeMap<String, Vec<&Task>> = BTreeMap::new();
    for task in tasks {
        for kind in kinds(task) {
            groups.entry(kind).or_default().push(task);
        }
    }
    Value::Object(groups.iter().map(|(kind, tasks)| (kind.clone(), task_stats(tasks))).collect())
}

pub fn summary(root: &Path) -> Value {
    let rows = rows(root);
    let held_back = |row: &Value| strings(&row["withheld"]);
    let noted = |row: &Value| if kinds_of(row).is_empty() && held_back(row).is_empty() { vec![UNNOTED.to_string()] } else if shown(row) { kinds_of(row) } else { Vec::new() };
    let withheld = |row: &Value| if shown(row) { held_back(row) } else { kinds_of(row).into_iter().chain(held_back(row)).collect() };
    let records = tally(&rows);
    let earning: Map<String, Value> = EARNING
        .iter()
        .map(|kind| {
            let record = records.get(*kind).copied().unwrap_or_default();
            ((*kind).to_string(), json!({"turns": record.turns, "followed": record.followed, "state": state(&records, kind), "needsTurns": PROVEN_MIN_TURNS, "needsRate": PROVEN_MIN_RATE}))
        })
        .chain(ALWAYS_SHOWN.iter().map(|kind| {
            let record = records.get(*kind).copied().unwrap_or_default();
            ((*kind).to_string(), json!({"turns": record.turns, "followed": record.followed, "state": "always shown"}))
        }))
        .collect();

    let unassigned: Vec<&Value> = rows.iter().filter(|r| r["task"].as_u64().is_none()).collect();
    let mut grouped: BTreeMap<(String, u64), Task> = BTreeMap::new();
    for row in &rows {
        let (Some(session), Some(id)) = (row["session"].as_str(), row["task"].as_u64()) else { continue };
        let task = grouped.entry((session.to_string(), id)).or_insert(Task { cost: None, turns: 0, steps: 0.0, shown: Vec::new(), withheld: Vec::new() });
        task.turns += 1;
        task.steps += row["steps"].as_f64().unwrap_or(0.0);
        if let Some(cost) = row["cost"].as_f64() {
            task.cost = Some(task.cost.unwrap_or(0.0) + cost);
        }
        for kind in noted(row).into_iter().filter(|k| k != UNNOTED) {
            if !task.shown.contains(&kind) {
                task.shown.push(kind);
            }
        }
        for kind in withheld(row) {
            if !task.withheld.contains(&kind) {
                task.withheld.push(kind);
            }
        }
    }
    let tasks: Vec<Task> = grouped.into_values().collect();
    json!({
        "turns": rows.len(),
        "byNote": by_kind(rows.iter(), &noted),
        "notShown": by_kind(rows.iter(), &withheld),
        "kinds": earning,
        "tasks": {
            "count": tasks.len(),
            "unassigned": {"turns": unassigned.len(), "cost": unassigned.iter().filter_map(|r| r["cost"].as_f64()).sum::<f64>()},
            "byNote": tasks_by(&tasks, &|t| if t.shown.is_empty() && t.withheld.is_empty() { vec![UNNOTED.to_string()] } else { t.shown.clone() }),
            "notShown": tasks_by(&tasks, &|t| t.withheld.iter().filter(|k| !t.shown.contains(k)).cloned().collect()),
        },
    })
}
