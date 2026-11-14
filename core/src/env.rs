use std::path::{Path, PathBuf};

pub fn stem_home() -> PathBuf {
    std::env::var_os("STEM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".stem"))
}

const PROJECT_HOME_ID: usize = 12;
const PROVIDER_KEY_ENDING: &str = "_API_KEY";
const NEVER_FROM_A_PROJECT: &[&str] = &["STEM_", "GIT_", "LD_", "DYLD_"];

pub fn project_path(root: &Path, raw: &str) -> String {
    let given = Path::new(raw);
    let relative = if given.is_absolute() {
        let real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let file = std::fs::canonicalize(given).unwrap_or_else(|_| given.to_path_buf());
        file.strip_prefix(&real).or_else(|_| given.strip_prefix(root)).map(Path::to_path_buf).unwrap_or_else(|_| given.to_path_buf())
    } else {
        given.to_path_buf()
    };
    let slashed = relative.to_string_lossy().replace('\\', "/");
    slashed.trim_start_matches("./").to_string()
}

pub fn project_home(root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let id: String = Sha256::digest(real.to_string_lossy().as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let name: String = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(40).collect();
    let home = stem_home().join("projects").join(format!("{name}-{}", &id[..PROJECT_HOME_ID]));
    if !home.exists() {
        adopt_old_store(&real, &home);
    }
    home
}

const OLD_STORE_PARTS: &[&str] = &["learn/state.json", "learn/usage.jsonl", "tools"];

fn stem_folder_is_ours(root: &Path) -> bool {
    let git = |args: &[&str]| crate::sys::command("git").args(args).current_dir(root).env_remove("GIT_DIR").env_remove("GIT_INDEX_FILE").env_remove("GIT_WORK_TREE").output().ok().filter(|o| o.status.success());
    let inside = git(&["rev-parse", "--is-inside-work-tree"]).is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
    inside && git(&["ls-files", "-z", "--", ":(icase).stem"]).is_some_and(|o| o.stdout.is_empty())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)?.flatten() {
            if entry.file_type().is_ok_and(|t| !t.is_symlink()) {
                copy_tree(&entry.path(), &to.join(entry.file_name()))?;
            }
        }
        Ok(())
    } else {
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(from, to).map(|_| ())
    }
}

fn adopt_old_store(root: &Path, home: &Path) {
    let old = root.join(".stem");
    let state = old.join("learn").join("state.json");
    if !state.is_file() || std::fs::symlink_metadata(&old).is_ok_and(|m| m.file_type().is_symlink()) || !stem_folder_is_ours(root) {
        return;
    }
    let moved: Vec<&str> = OLD_STORE_PARTS.iter().copied().filter(|part| old.join(part).exists() && copy_tree(&old.join(part), &home.join(part)).is_ok()).collect();
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _ = std::fs::write(home.join("adopted.json"), serde_json::json!({"from": old, "at": at, "parts": moved}).to_string());
}

fn provider_key(key: &str) -> bool {
    key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && !NEVER_FROM_A_PROJECT.iter().any(|p| key.starts_with(p)) && key.ends_with(PROVIDER_KEY_ENDING)
}

pub fn load_dotenv(project: &Path) -> Vec<PathBuf> {
    let mut loaded = Vec::new();
    for (file, from_project) in [(stem_home().join(".env"), false), (project.join(".env"), true)] {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for (key, value) in parse(&text) {
            if (!from_project || provider_key(&key)) && std::env::var_os(&key).is_none() {
                unsafe { std::env::set_var(key, value) };
            }
        }
        loaded.push(file);
    }
    for (canonical, alias) in [("LLMGATEWAY_API_KEY", "LLMGW_KEY")] {
        if std::env::var_os(canonical).is_none()
            && let Some(v) = std::env::var_os(alias) {
                unsafe { std::env::set_var(canonical, v) };
            }
    }
    loaded
}

fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            Some((key.trim().to_string(), value.to_string()))
        })
        .collect()
}
