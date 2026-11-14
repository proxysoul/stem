use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub source: String,
}

#[derive(Default)]
struct Shared {
    pending: HashMap<u64, Sender<Value>>,
    diagnostics: HashMap<String, (u64, Vec<Value>)>,
    progress: HashSet<String>,
    last_progress: Option<Instant>,
    exited: bool,
}

pub struct Server {
    pub name: String,
    pub source: String,
    pub start_ms: u128,
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Shared>>,
    next_id: AtomicU64,
    diag_seq: Arc<AtomicU64>,
    opened: HashMap<String, i32>,
    pub capabilities: Value,
    root: PathBuf,
    warmed: bool,
}

pub fn uri(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    let encoded: String = s
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '?' => "%3F".to_string(),
            c => c.to_string(),
        })
        .collect();
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

pub fn path_of(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded = percent_decode(raw);
    if cfg!(windows) {
        PathBuf::from(decoded.trim_start_matches('/'))
    } else {
        PathBuf::from(decoded)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn write_message(stdin: &Arc<Mutex<ChildStdin>>, value: &Value) -> Result<(), String> {
    let body = value.to_string();
    let mut guard = stdin.lock().map_err(|_| "language server pipe poisoned")?;
    guard
        .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .map_err(|e| format!("language server pipe closed: {e}"))?;
    guard.flush().map_err(|e| e.to_string())
}

fn read_loop(stdout: impl Read, shared: Arc<Mutex<Shared>>, stdin: Arc<Mutex<ChildStdin>>, diag_seq: Arc<AtomicU64>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let mut s = shared.lock().unwrap();
                    s.exited = true;
                    s.pending.clear();
                    return;
                }
                Ok(_) => {}
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:").or_else(|| line.strip_prefix("content-length:")) {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            shared.lock().unwrap().exited = true;
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&body) else { continue };
        let method = message["method"].as_str().unwrap_or("");
        if message.get("id").is_some() && !method.is_empty() {
            let result = match method {
                "workspace/configuration" => Value::Array(
                    message["params"]["items"]
                        .as_array()
                        .map(|items| items.iter().map(|_| Value::Null).collect())
                        .unwrap_or_default(),
                ),
                "workspace/workspaceFolders" => Value::Null,
                _ => Value::Null,
            };
            let _ = write_message(&stdin, &json!({"jsonrpc": "2.0", "id": message["id"], "result": result}));
            continue;
        }
        if let Some(id) = message["id"].as_u64() {
            if let Some(tx) = shared.lock().unwrap().pending.remove(&id) {
                let _ = tx.send(message);
            }
            continue;
        }
        match method {
            "textDocument/publishDiagnostics" => {
                let seq = diag_seq.fetch_add(1, Ordering::Relaxed) + 1;
                let uri = message["params"]["uri"].as_str().unwrap_or("").to_string();
                let items = message["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
                shared.lock().unwrap().diagnostics.insert(uri, (seq, items));
            }
            "$/progress" => {
                let token = message["params"]["token"].to_string();
                let kind = message["params"]["value"]["kind"].as_str().unwrap_or("");
                let mut s = shared.lock().unwrap();
                match kind {
                    "begin" => {
                        s.progress.insert(token);
                    }
                    "end" => {
                        s.progress.remove(&token);
                    }
                    _ => {}
                }
                s.last_progress = Some(Instant::now());
            }
            _ => {}
        }
    }
}

pub fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "rs" => "rust",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "lua" => "lua",
        "zig" => "zig",
        _ => "plaintext",
    }
}

impl Server {
    pub fn start(spec: ServerSpec, root: &Path) -> Result<Server, String> {
        let started = Instant::now();
        let mut child = crate::sys::command(&spec.command)
            .args(&spec.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting {} ({}): {e}", spec.name, spec.command.display()))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().ok_or("no stdin")?));
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let diag_seq = Arc::new(AtomicU64::new(0));
        {
            let (shared, stdin, diag_seq) = (shared.clone(), stdin.clone(), diag_seq.clone());
            std::thread::spawn(move || read_loop(stdout, shared, stdin, diag_seq));
        }
        let mut server = Server {
            name: spec.name,
            source: spec.source,
            start_ms: 0,
            child,
            stdin,
            shared,
            next_id: AtomicU64::new(1),
            diag_seq,
            opened: HashMap::new(),
            capabilities: Value::Null,
            root: root.to_path_buf(),
            warmed: false,
        };
        let root_uri = uri(root);
        let init = server.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": "root"}],
                "capabilities": {
                    "general": {"positionEncodings": ["utf-16"]},
                    "window": {"workDoneProgress": true},
                    "workspace": {"configuration": true, "workspaceFolders": true, "workspaceEdit": {"documentChanges": true}, "symbol": {}},
                    "textDocument": {
                        "synchronization": {"didSave": true},
                        "publishDiagnostics": {"relatedInformation": false},
                        "hover": {"contentFormat": ["plaintext", "markdown"]},
                        "definition": {"linkSupport": true},
                        "implementation": {"linkSupport": true},
                        "references": {},
                        "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                        "rename": {"prepareSupport": true},
                        "callHierarchy": {},
                        "diagnostic": {},
                    }
                }
            }),
            Duration::from_secs(60),
        )?;
        server.capabilities = init["capabilities"].clone();
        server.notify("initialized", json!({}))?;
        server.start_ms = started.elapsed().as_millis();
        Ok(server)
    }

    pub fn alive(&self) -> bool {
        !self.shared.lock().map(|s| s.exited).unwrap_or(true)
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        write_message(&self.stdin, &json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        {
            let mut s = self.shared.lock().unwrap();
            if s.exited {
                return Err(format!("{} has exited", self.name));
            }
            s.pending.insert(id, tx);
        }
        write_message(&self.stdin, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        match rx.recv_timeout(timeout) {
            Ok(message) => match message.get("error") {
                Some(err) if !err.is_null() => Err(format!("{} {method}: {}", self.name, err["message"].as_str().unwrap_or("error"))),
                _ => Ok(message["result"].clone()),
            },
            Err(_) => {
                self.shared.lock().unwrap().pending.remove(&id);
                let _ = self.notify("$/cancelRequest", json!({"id": id}));
                Err(format!("{} did not answer {method} within {} s", self.name, timeout.as_secs()))
            }
        }
    }

    pub fn open(&mut self, path: &Path) -> Result<String, String> {
        let u = uri(path);
        if !self.opened.contains_key(&u) {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument": {"uri": u, "languageId": language_id(path), "version": 1, "text": text}}),
            )?;
            self.opened.insert(u.clone(), 1);
        }
        Ok(u)
    }

    pub fn changed(&mut self, path: &Path, text: &str) -> Result<u64, String> {
        let u = uri(path);
        let before = self.diag_seq.load(Ordering::Relaxed);
        match self.opened.get_mut(&u) {
            Some(version) => {
                *version += 1;
                let v = *version;
                self.notify(
                    "textDocument/didChange",
                    json!({"textDocument": {"uri": u, "version": v}, "contentChanges": [{"text": text}]}),
                )?;
            }
            None => {
                self.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {"uri": u, "languageId": language_id(path), "version": 1, "text": text}}),
                )?;
                self.opened.insert(u.clone(), 1);
            }
        }
        self.notify("textDocument/didSave", json!({"textDocument": {"uri": u}}))?;
        Ok(before)
    }

    pub fn wait_ready(&mut self, max: Duration) {
        let first = !self.warmed;
        self.warmed = true;
        let started = Instant::now();
        let settle = if first { Duration::from_millis(700) } else { Duration::from_millis(150) };
        loop {
            let (busy, quiet_for) = {
                let s = self.shared.lock().unwrap();
                (!s.progress.is_empty(), s.last_progress.map(|t| t.elapsed()).unwrap_or(started.elapsed()))
            };
            if (!busy && quiet_for >= settle) || started.elapsed() >= max {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn diagnostics(&mut self, path: &Path, after: Option<u64>, wait: Duration) -> Vec<Value> {
        let u = uri(path);
        if self.capabilities.get("diagnosticProvider").is_some_and(|v| !v.is_null())
            && let Ok(report) = self.request("textDocument/diagnostic", json!({"textDocument": {"uri": u}}), wait.max(Duration::from_secs(5)))
                && let Some(items) = report["items"].as_array() {
                    return items.clone();
                }
        let started = Instant::now();
        loop {
            {
                let s = self.shared.lock().unwrap();
                if let Some((seq, items)) = s.diagnostics.get(&u)
                    && after.is_none_or(|a| *seq > a) {
                        return items.clone();
                    }
            }
            if started.elapsed() >= wait {
                return self.shared.lock().unwrap().diagnostics.get(&u).map(|(_, i)| i.clone()).unwrap_or_default();
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    pub fn all_diagnostics(&self) -> Vec<(PathBuf, Vec<Value>)> {
        self.shared
            .lock()
            .unwrap()
            .diagnostics
            .iter()
            .filter(|(_, (_, items))| !items.is_empty())
            .map(|(u, (_, items))| (path_of(u), items.clone()))
            .collect()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_millis(800));
        let _ = self.notify("exit", Value::Null);
        std::thread::sleep(Duration::from_millis(50));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
