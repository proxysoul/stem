use crate::servers::Lang;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Project {
    pub root: PathBuf,
    pub runner: String,
    pub commands: Vec<(String, String)>,
    pub scripts: Vec<String>,
    pub languages: Vec<(Lang, usize)>,
    pub roots: Vec<(Lang, PathBuf, usize)>,
    pub branch: Option<String>,
    pub top: Vec<String>,
}

impl Project {
    pub fn detect(root: &Path) -> Project {
        let mut p = Project {
            root: root.to_path_buf(),
            ..Default::default()
        };
        let has = |f: &str| root.join(f).exists();
        if has("package.json") {
            let pkg: Value = std::fs::read_to_string(root.join("package.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let pm = if has("bun.lock") || has("bun.lockb") {
                "bun"
            } else if has("pnpm-lock.yaml") {
                "pnpm"
            } else if has("yarn.lock") {
                "yarn"
            } else {
                "npm"
            };
            p.runner = pm.into();
            let scripts: Vec<String> = pkg["scripts"].as_object().map(|s| s.keys().cloned().collect()).unwrap_or_default();
            for key in ["test", "build", "lint", "typecheck", "check", "format", "dev"] {
                if scripts.iter().any(|s| s == key) {
                    p.commands.push((key.into(), format!("{pm} run {key}")));
                }
            }
            if !p.commands.iter().any(|(k, _)| k == "test") && pm == "bun" {
                p.commands.push(("test".into(), "bun test".into()));
            }
            if !p.commands.iter().any(|(k, _)| k == "typecheck") && has("tsconfig.json") {
                let tsc = if pm == "bun" {
                    "bunx tsc --noEmit"
                } else if pm == "pnpm" {
                    "pnpm exec tsc --noEmit"
                } else {
                    "npx tsc --noEmit"
                };
                p.commands.push(("typecheck".into(), tsc.into()));
            }
            p.scripts = scripts;
        } else if has("Cargo.toml") {
            p.runner = "cargo".into();
            for (k, c) in [
                ("test", "cargo test"),
                ("build", "cargo build"),
                ("typecheck", "cargo check"),
                ("lint", "cargo clippy"),
                ("format", "cargo fmt"),
            ] {
                p.commands.push((k.into(), c.into()));
            }
        } else if has("go.mod") {
            p.runner = "go".into();
            for (k, c) in [("test", "go test ./..."), ("build", "go build ./..."), ("lint", "go vet ./...")] {
                p.commands.push((k.into(), c.into()));
            }
        } else if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
            let prefix = if has("uv.lock") {
                "uv run "
            } else if has("poetry.lock") {
                "poetry run "
            } else {
                "python -m "
            };
            p.runner = prefix.split_whitespace().next().unwrap_or("python").into();
            p.commands.push(("test".into(), format!("{prefix}pytest")));
        } else if has("Makefile") {
            p.runner = "make".into();
            p.commands.push(("test".into(), "make test".into()));
            p.commands.push(("build".into(), "make".into()));
        }
        (p.languages, p.roots) = count_languages(root);
        p.branch = std::fs::read_to_string(root.join(".git/HEAD"))
            .ok()
            .and_then(|h| h.trim().strip_prefix("ref: refs/heads/").map(str::to_string));
        let mut top: Vec<String> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.file_type().is_ok_and(|t| t.is_dir()) { format!("{name}/") } else { name }
            })
            .filter(|n| !matches!(n.as_str(), ".git/" | "node_modules/" | "target/" | ".stem/" | ".DS_Store"))
            .collect();
        top.sort();
        top.truncate(60);
        p.top = top;
        p
    }

    pub fn rel_root(&self, root: &Path) -> String {
        match root.strip_prefix(&self.root) {
            Ok(p) if p.as_os_str().is_empty() => ".".into(),
            Ok(p) => p.display().to_string(),
            Err(_) => root.display().to_string(),
        }
    }

    pub fn command(&self, key: &str) -> Option<&str> {
        self.commands.iter().find(|(k, _)| k == key).map(|(_, c)| c.as_str())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "root": self.root.display().to_string(),
            "runner": self.runner,
            "commands": self.commands.iter().map(|(k, c)| (k.clone(), json!(c))).collect::<serde_json::Map<_, _>>(),
            "scripts": self.scripts,
            "languages": self.languages.iter().map(|(l, n)| json!({"language": l.label(), "files": n})).collect::<Vec<_>>(),
            "roots": self.roots.iter().take(12).map(|(l, r, n)| json!({"language": l.label(), "root": self.rel_root(r), "files": n})).collect::<Vec<_>>(),
            "branch": self.branch,
        })
    }

    pub fn rewrite(&self, cmd: &str) -> Option<String> {
        let trimmed = cmd.trim_start();
        let (tool, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let runner = self.runner.as_str();
        if !matches!(runner, "bun" | "pnpm" | "yarn") || !matches!(tool, "npm" | "npx") {
            return None;
        }
        let rest = rest.trim();
        let new = match (tool, runner) {
            ("npx", "bun") => format!("bunx {rest}"),
            ("npx", "pnpm") => format!("pnpm exec {rest}"),
            ("npx", _) => format!("yarn dlx {rest}"),
            ("npm", _) if rest == "test" || rest.starts_with("test ") || rest == "t" => {
                self.command("test").map(str::to_string).unwrap_or(format!("{runner} test"))
            }
            ("npm", _) if rest.starts_with("run ") => format!("{runner} {rest}"),
            ("npm", _) if rest == "install" || rest == "i" || rest == "ci" => format!("{runner} install"),
            ("npm", _) if rest.starts_with("install ") || rest.starts_with("i ") => {
                format!("{runner} add {}", rest.split_once(' ').map(|(_, r)| r).unwrap_or(""))
            }
            _ => return None,
        };
        Some(new)
    }
}

fn count_languages(root: &Path) -> (Vec<(Lang, usize)>, Vec<(Lang, PathBuf, usize)>) {
    let mut counts: HashMap<Lang, usize> = HashMap::new();
    let mut by_root: HashMap<(Lang, PathBuf), usize> = HashMap::new();
    let mut dir_roots: HashMap<(Lang, PathBuf), PathBuf> = HashMap::new();
    for path in crate::host::walk(root).take(100_000) {
        let Some(lang) = Lang::of(&path) else { continue };
        *counts.entry(lang).or_default() += 1;
        let dir = path.parent().unwrap_or(root).to_path_buf();
        let space = dir_roots.entry((lang, dir)).or_insert_with(|| crate::servers::root_for(lang, &path, root)).clone();
        *by_root.entry((lang, space)).or_default() += 1;
    }
    let mut v: Vec<(Lang, usize)> = counts.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mut roots: Vec<(Lang, PathBuf, usize)> = by_root.into_iter().map(|((l, r), n)| (l, r, n)).collect();
    roots.sort_by_key(|x| std::cmp::Reverse(x.2));
    (v, roots)
}
