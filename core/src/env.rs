use std::path::{Path, PathBuf};

pub fn stem_home() -> PathBuf {
    std::env::var_os("STEM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".stem"))
}

pub fn load_dotenv(project: &Path) -> Vec<PathBuf> {
    let mut loaded = Vec::new();
    for file in [project.join(".env"), stem_home().join(".env")] {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for (key, value) in parse(&text) {
            if std::env::var_os(&key).is_none() {
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
