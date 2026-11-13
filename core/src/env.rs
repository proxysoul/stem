use std::path::{Path, PathBuf};

pub fn stem_home() -> PathBuf {
    std::env::var_os("STEM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".stem"))
}

const PROJECT_HOME_ID: usize = 12;
const PROVIDER_KEY_ENDING: &str = "_API_KEY";
const NEVER_FROM_A_PROJECT: &[&str] = &["STEM_", "GIT_", "LD_", "DYLD_"];

pub fn project_home(root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let id: String = Sha256::digest(real.to_string_lossy().as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let name: String = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(40).collect();
    stem_home().join("projects").join(format!("{name}-{}", &id[..PROJECT_HOME_ID]))
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
