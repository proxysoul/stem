use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl: Option<String>,
    #[serde(default)]
    pub yolo: bool,
}

fn path() -> PathBuf {
    crate::env::stem_home().join("config.json")
}

impl Config {
    pub fn load() -> Config {
        std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn update(change: impl FnOnce(&mut Config)) {
        let mut config = Config::load();
        change(&mut config);
        let file = path();
        let _ = std::fs::create_dir_all(file.parent().unwrap_or(&file));
        let tmp = file.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string_pretty(&config) {
            if std::fs::write(&tmp, text + "\n").is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        }
    }
}
