use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Log {
    file: Option<File>,
    pub path: PathBuf,
}

pub fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

pub fn sessions_dir(root: &Path) -> PathBuf {
    crate::env::project_home(root).join("sessions")
}

impl Log {
    pub fn open(root: &Path) -> Log {
        let dir = sessions_dir(root);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.jsonl", now_ms()));
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Log { file, path }
    }

    pub fn event(&mut self, kind: &str, mut fields: Value) {
        if let Some(obj) = fields.as_object_mut() {
            obj.insert("k".into(), json!(kind));
            obj.insert("ts".into(), json!(now_ms() as u64));
        }
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{fields}");
        }
    }
}
