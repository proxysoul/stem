use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Log {
    file: Option<BufWriter<File>>,
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
        let file = OpenOptions::new().create(true).append(true).open(&path).ok().map(BufWriter::new);
        Log { file, path }
    }

    pub fn event(&mut self, kind: &str, mut fields: Value) {
        if let Some(obj) = fields.as_object_mut() {
            obj.insert("k".into(), json!(kind));
            obj.insert("ts".into(), json!(now_ms() as u64));
        }
        if let Some(f) = &mut self.file {
            let _ = serde_json::to_writer(&mut *f, &fields);
            let _ = f.write_all(b"\n");
        }
    }

    pub fn flush(&mut self) {
        if let Some(f) = &mut self.file {
            let _ = f.flush();
        }
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_buffered_event_writing() {
        let tmp = std::env::temp_dir().join(format!("stem_test_log_{}", now_ms()));
        let _ = std::fs::create_dir_all(&tmp);
        {
            let mut log = Log::open(&tmp);
            log.event("test_event", json!({"hello": "world", "count": 42}));
            log.event("second_event", json!({"ok": true}));
            // Drop will flush the BufWriter
        }
        let sessions = sessions_dir(&tmp);
        let entries: Vec<_> = std::fs::read_dir(&sessions).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1);
        let content = std::fs::read_to_string(entries[0].path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let v1: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v1["k"], "test_event");
        assert_eq!(v1["hello"], "world");
        assert_eq!(v1["count"], 42);
        assert!(v1["ts"].as_u64().is_some());
        let v2: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v2["k"], "second_event");
        assert_eq!(v2["ok"], true);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
