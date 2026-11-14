use crate::servers::Lang;
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};
use tree_sitter_tags::{TagsConfiguration, TagsContext};

const SCHEMA: &str = "
create table if not exists files (path text primary key, lang text, size integer, mtime integer, lines integer);
create table if not exists symbols (file text, name text, kind text, line integer, end_line integer, doc text);
create table if not exists refs (file text, name text, kind text, line integer);
create table if not exists commits (hash text primary key, date text, author text, subject text);
create table if not exists commit_files (hash text, path text);
create table if not exists cochange (a text, b text, n integer);
create table if not exists meta (key text primary key, value text);
create index if not exists symbols_name on symbols(name);
create index if not exists symbols_file on symbols(file);
create index if not exists refs_name on refs(name);
create index if not exists refs_file on refs(file);
create index if not exists commit_files_path on commit_files(path);
create index if not exists cochange_a on cochange(a);
";

pub const TABLES: &str = "files(path, lang, size, mtime, lines) · symbols(file, name, kind, line, end_line, doc): definitions · refs(file, name, kind, line): calls and type uses by name · commits(hash, date, author, subject) · commit_files(hash, path) · cochange(a, b, n): files changed together n times";

pub struct Facts {
    db: Connection,
    root: PathBuf,
    pub fresh_at: Option<Instant>,
}

struct Parsed {
    path: String,
    lang: &'static str,
    size: i64,
    mtime: i64,
    lines: i64,
    defs: Vec<(String, String, i64, i64, Option<String>)>,
    refs: Vec<(String, String, i64)>,
}

fn configs() -> Vec<(Lang, &'static str, TagsConfiguration)> {
    let ts_query = format!("{}\n{}", tree_sitter_javascript::TAGS_QUERY, tree_sitter_typescript::TAGS_QUERY);
    let mut out = Vec::new();
    let mut add = |lang: Lang, name: &'static str, language: tree_sitter::Language, tags: &str, locals: &str| {
        if let Ok(c) = TagsConfiguration::new(language, tags, locals) {
            out.push((lang, name, c));
        }
    };
    add(Lang::TypeScript, "typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), &ts_query, tree_sitter_typescript::LOCALS_QUERY);
    add(Lang::TypeScript, "tsx", tree_sitter_typescript::LANGUAGE_TSX.into(), &ts_query, tree_sitter_typescript::LOCALS_QUERY);
    add(Lang::TypeScript, "javascript", tree_sitter_javascript::LANGUAGE.into(), tree_sitter_javascript::TAGS_QUERY, tree_sitter_javascript::LOCALS_QUERY);
    add(Lang::Rust, "rust", tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::TAGS_QUERY, "");
    add(Lang::Python, "python", tree_sitter_python::LANGUAGE.into(), tree_sitter_python::TAGS_QUERY, "");
    add(Lang::Go, "go", tree_sitter_go::LANGUAGE.into(), tree_sitter_go::TAGS_QUERY, "");
    out
}

fn grammar_for(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rs" => "rust",
        "py" | "pyi" => "python",
        "go" => "go",
        _ => return None,
    })
}

fn line_of(starts: &[usize], offset: usize) -> i64 {
    (starts.partition_point(|s| *s <= offset)) as i64
}

fn parse(path: &Path, rel: String, grammar: &'static str, configs: &[(Lang, &'static str, TagsConfiguration)], ctx: &mut TagsContext, size: i64, mtime: i64) -> Parsed {
    let mut parsed = Parsed { path: rel, lang: grammar, size, mtime, lines: 0, defs: Vec::new(), refs: Vec::new() };
    let Ok(source) = std::fs::read(path) else { return parsed };
    let starts: Vec<usize> = std::iter::once(0).chain(source.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i + 1)).collect();
    parsed.lines = starts.len() as i64;
    if source.len() > 2_000_000 {
        return parsed;
    }
    let Some((_, _, config)) = configs.iter().find(|(_, g, _)| *g == grammar) else { return parsed };
    let Ok((tags, _)) = ctx.generate_tags(config, &source, None) else { return parsed };
    for tag in tags.flatten() {
        let name = String::from_utf8_lossy(&source[tag.name_range.clone()]).into_owned();
        let kind = config.syntax_type_name(tag.syntax_type_id).to_string();
        let line = line_of(&starts, tag.name_range.start);
        if tag.is_definition {
            parsed.defs.push((name, kind, line, line_of(&starts, tag.range.end.saturating_sub(1).max(tag.range.start)), tag.docs.map(|d| d.chars().take(400).collect())));
        } else {
            parsed.refs.push((name, kind, line));
        }
    }
    parsed
}

impl Facts {
    pub fn open(root: &Path) -> Result<Facts, String> {
        let dir = crate::env::project_home(root);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let db = Connection::open(dir.join("facts.db")).map_err(|e| format!("fact store: {e}"))?;
        db.execute_batch("pragma journal_mode = wal; pragma synchronous = normal;").map_err(|e| e.to_string())?;
        db.execute_batch(SCHEMA).map_err(|e| format!("fact store schema: {e}"))?;
        Ok(Facts { db, root: root.to_path_buf(), fresh_at: None })
    }

    pub fn refresh(&mut self) -> Result<Value, String> {
        if self.fresh_at.is_some_and(|t| t.elapsed().as_secs() < 2) {
            return Ok(json!({"changed": 0}));
        }
        let started = Instant::now();
        let mut known: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut stmt = self.db.prepare("select path, size, mtime from files").map_err(|e| e.to_string())?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))).map_err(|e| e.to_string())?;
            for row in rows.flatten() {
                known.insert(row.0, (row.1, row.2));
            }
        }
        let mut todo: Vec<(PathBuf, String, &'static str, i64, i64)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for path in crate::host::walk(&self.root).take(200_000) {
            let Some(grammar) = grammar_for(&path) else { continue };
            let rel = path.strip_prefix(&self.root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let size = meta.len() as i64;
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
            seen.insert(rel.clone());
            if known.get(&rel) != Some(&(size, mtime)) {
                todo.push((path, rel, grammar, size, mtime));
            }
        }
        let removed: Vec<String> = known.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(12);
        let chunk = todo.len().div_ceil(threads).max(1);
        let parsed: Vec<Parsed> = std::thread::scope(|scope| {
            let handles: Vec<_> = todo
                .chunks(chunk)
                .map(|part| {
                    scope.spawn(move || {
                        let configs = configs();
                        let mut ctx = TagsContext::new();
                        part.iter().map(|(p, rel, g, size, mtime)| parse(p, rel.clone(), g, &configs, &mut ctx, *size, *mtime)).collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        let tx = self.db.transaction().map_err(|e| e.to_string())?;
        {
            let mut del_files = tx.prepare("delete from files where path = ?1").map_err(|e| e.to_string())?;
            let mut del_symbols = tx.prepare("delete from symbols where file = ?1").map_err(|e| e.to_string())?;
            let mut del_refs = tx.prepare("delete from refs where file = ?1").map_err(|e| e.to_string())?;
            for path in removed.iter().chain(parsed.iter().map(|p| &p.path)) {
                del_files.execute([path]).map_err(|e| e.to_string())?;
                del_symbols.execute([path]).map_err(|e| e.to_string())?;
                del_refs.execute([path]).map_err(|e| e.to_string())?;
            }
            let mut ins_file = tx.prepare("insert into files values (?1, ?2, ?3, ?4, ?5)").map_err(|e| e.to_string())?;
            let mut ins_sym = tx.prepare("insert into symbols values (?1, ?2, ?3, ?4, ?5, ?6)").map_err(|e| e.to_string())?;
            let mut ins_ref = tx.prepare("insert into refs values (?1, ?2, ?3, ?4)").map_err(|e| e.to_string())?;
            for p in &parsed {
                ins_file.execute(params![p.path, p.lang, p.size, p.mtime, p.lines]).map_err(|e| e.to_string())?;
                for (name, kind, line, end, doc) in &p.defs {
                    ins_sym.execute(params![p.path, name, kind, line, end, doc]).map_err(|e| e.to_string())?;
                }
                for (name, kind, line) in &p.refs {
                    ins_ref.execute(params![p.path, name, kind, line]).map_err(|e| e.to_string())?;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        let history = self.refresh_history().unwrap_or_else(|e| json!({"error": e}));
        self.fresh_at = Some(Instant::now());
        Ok(json!({"parsed": parsed.len(), "removed": removed.len(), "ms": started.elapsed().as_millis() as u64, "history": history}))
    }

    fn refresh_history(&mut self) -> Result<Value, String> {
        let head = crate::sys::command("git").args(["rev-parse", "HEAD"]).current_dir(&self.root).output().map_err(|e| e.to_string())?;
        if !head.status.success() {
            return Ok(json!("no git"));
        }
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let stored: Option<String> = self.db.query_row("select value from meta where key = 'head'", [], |r| r.get(0)).ok();
        if stored.as_deref() == Some(head.as_str()) {
            return Ok(json!("current"));
        }
        let log = crate::sys::command("git")
            .args(["log", "-n", "3000", "--no-merges", "--name-only", "--format=%x1e%H%x1f%cs%x1f%an%x1f%s"])
            .current_dir(&self.root)
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&log.stdout);
        let tx = self.db.transaction().map_err(|e| e.to_string())?;
        tx.execute_batch("delete from commits; delete from commit_files; delete from cochange;").map_err(|e| e.to_string())?;
        let mut pairs: HashMap<(String, String), i64> = HashMap::new();
        let mut count = 0;
        {
            let mut ins_c = tx.prepare("insert or ignore into commits values (?1, ?2, ?3, ?4)").map_err(|e| e.to_string())?;
            let mut ins_f = tx.prepare("insert into commit_files values (?1, ?2)").map_err(|e| e.to_string())?;
            for record in text.split('\x1e').filter(|r| !r.trim().is_empty()) {
                let mut lines = record.lines();
                let header: Vec<&str> = lines.next().unwrap_or("").split('\x1f').collect();
                if header.len() < 4 {
                    continue;
                }
                let files: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
                ins_c.execute(params![header[0], header[1], header[2], header[3]]).map_err(|e| e.to_string())?;
                for f in &files {
                    ins_f.execute(params![header[0], f]).map_err(|e| e.to_string())?;
                }
                if files.len() <= 24 {
                    for a in &files {
                        for b in &files {
                            if a != b {
                                *pairs.entry((a.to_string(), b.to_string())).or_default() += 1;
                            }
                        }
                    }
                }
                count += 1;
            }
            let mut ins_p = tx.prepare("insert into cochange values (?1, ?2, ?3)").map_err(|e| e.to_string())?;
            for ((a, b), n) in pairs.iter().filter(|(_, n)| **n >= 2) {
                ins_p.execute(params![a, b, n]).map_err(|e| e.to_string())?;
            }
            tx.execute("insert or replace into meta values ('head', ?1)", [&head]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(json!({"commits": count}))
    }

    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Value, String> {
        let mut stmt = self.db.prepare(sql).map_err(|e| format!("$.sql: {e}. Tables: {TABLES}"))?;
        if !stmt.readonly() {
            return Err("$.sql only reads; use $.edit, $.write and the other actions to change things".into());
        }
        let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
        let bound: Vec<rusqlite::types::Value> = params
            .iter()
            .map(|p| match p {
                Value::Null => rusqlite::types::Value::Null,
                Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
                Value::Number(n) => n.as_i64().map(rusqlite::types::Value::Integer).unwrap_or_else(|| rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))),
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                other => rusqlite::types::Value::Text(other.to_string()),
            })
            .collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(bound.iter())).map_err(|e| format!("$.sql: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| format!("$.sql: {e}"))? {
            if out.len() >= 5000 {
                out.push(json!({"…": "cut at 5000 rows; add a limit or a narrower where"}));
                break;
            }
            let mut obj = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let v = match row.get_ref(i).map_err(|e| e.to_string())? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(f) => json!(f),
                    ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    ValueRef::Blob(b) => json!(format!("<{} bytes>", b.len())),
                };
                obj.insert(name.clone(), v);
            }
            out.push(Value::Object(obj));
        }
        Ok(Value::Array(out))
    }
}
