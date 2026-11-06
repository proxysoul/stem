use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const PROVIDER: &str = "claude-native";
pub const ALIAS: &str = "claude-n";
pub const BASE_URL: &str = "https://api.anthropic.com/v1";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
const BOOTSTRAP_URL: &str = "https://api.anthropic.com/api/claude_cli/bootstrap";
const BOOTSTRAP_MODEL: &str = "claude-opus-4-8";
const CALLBACK_PORT: u16 = 54545;
const CALLBACK_PATH: &str = "/callback";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const EXPIRY_MARGIN_MS: u64 = 5 * 60 * 1000;

const CLAUDE_CODE_VERSION: &str = "2.1.312";
const SDK_VERSION: &str = "0.128.0";
const SYSTEM_INSTRUCTION: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";
const BILLING_PREFIX: &str = "x-anthropic-billing-header:";
const CCH_PLACEHOLDER: &str = "cch=00000";
const CCH_SEED: u64 = 0x4d659218e32a3268;
const CCH_SEARCH_WINDOW: usize = 150;
const DEVICE_ID_INSTALL_DOMAIN: &str = "stem-claude-device-id-v1:";
const DEVICE_ID_ACCOUNT_DOMAIN: &str = "stem-claude-device-id-v2";
const GLOBAL_CACHE_SCOPE: &str = "global";

const OAUTH_BETA: &str = "oauth-2025-04-20";
const STRUCTURED_OUTPUTS_BETA: &str = "structured-outputs-2025-12-15";
const EFFORT_BETA: &str = "effort-2025-11-24";
const UTILITY_BETAS: [&str; 6] = [
    OAUTH_BETA,
    "interleaved-thinking-2025-05-14",
    "thinking-token-count-2026-05-13",
    "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05",
    STRUCTURED_OUTPUTS_BETA,
];
const AGENT_BETAS: [&str; 7] = [
    "claude-code-20250219",
    OAUTH_BETA,
    "interleaved-thinking-2025-05-14",
    "thinking-token-count-2026-05-13",
    "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05",
    "advanced-tool-use-2025-11-20",
];

static LEARNED_VERSION: Mutex<Option<Option<String>>> = Mutex::new(None);
static REFRESHING: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Clone)]
struct Credentials {
    access: String,
    refresh: String,
    expires: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authorized_at: Option<u64>,
}

#[derive(Default)]
struct Identity {
    account_id: Option<String>,
    email: Option<String>,
    org_id: Option<String>,
    org_name: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn auth_file() -> PathBuf {
    crate::env::stem_home().join("auth").join("claude.json")
}

pub fn auth_path() -> String {
    auth_file().display().to_string()
}

fn load() -> Option<Credentials> {
    serde_json::from_str(&std::fs::read_to_string(auth_file()).ok()?).ok()
}

pub fn signed_in() -> bool {
    load().is_some()
}

fn write_private(file: &PathBuf, text: &str) -> Result<(), String> {
    let dir = file.parent().unwrap_or(file);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = file.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options.open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    out.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
}

fn save(credentials: &Credentials) -> Result<(), String> {
    let text = serde_json::to_string_pretty(credentials).map_err(|e| e.to_string())?;
    write_private(&auth_file(), &text).map_err(|e| format!("Claude credentials were not saved: {e}"))
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("system random source");
    buf
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for p in parts {
        hash.update(p.as_bytes());
    }
    hex(&hash.finalize())
}

fn uuid_v4() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn install_id() -> String {
    let file = crate::env::stem_home().join("install-id");
    if let Ok(id) = std::fs::read_to_string(&file)
        && !id.trim().is_empty()
    {
        return id.trim().to_string();
    }
    let id = uuid_v4();
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(&file));
    let _ = std::fs::write(&file, &id);
    id
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%').then(|| s.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], escaped) {
            (_, Some(v)) => {
                out.push(v);
                i += 2;
            }
            (b'+', None) => out.push(b' '),
            (b, None) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| percent_decode(v))
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn post_json(url: &str, body: Value, headers: &[(&str, String)]) -> Result<Value, String> {
    let mut request = http().post(url).header("Content-Type", "application/json");
    for (k, v) in headers {
        request = request.header(*k, v);
    }
    let response = request.send(body.to_string().as_bytes()).map_err(|e| format!("HTTP request failed. url={url}; details={e}"))?;
    let status = response.status().as_u16();
    let text = response.into_body().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP request failed. status={status}; url={url}; body={text}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("Anthropic returned invalid JSON. url={url}; body={text}; details={e}"))
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn identity_from_token(data: &Value) -> Identity {
    Identity {
        account_id: non_empty(&data["account"]["uuid"]),
        email: non_empty(&data["account"]["email_address"]),
        org_id: non_empty(&data["organization"]["uuid"]),
        org_name: non_empty(&data["organization"]["name"]),
    }
}

fn bootstrap_identity(access: &str) -> Result<Identity, String> {
    let url = format!("{BOOTSTRAP_URL}?entrypoint=cli&model={}", percent_encode(BOOTSTRAP_MODEL));
    let response = http()
        .get(&url)
        .header("Accept", "application/json, text/plain, */*")
        .header("Authorization", &format!("Bearer {access}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", &format!("claude-code/{}", version()))
        .header("anthropic-beta", OAUTH_BETA)
        .call()
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!("bootstrap HTTP {}", response.status().as_u16()));
    }
    let data: Value = serde_json::from_str(&response.into_body().read_to_string().unwrap_or_default()).map_err(|e| e.to_string())?;
    let a = &data["oauth_account"];
    Ok(Identity {
        account_id: non_empty(&a["account_uuid"]),
        email: non_empty(&a["account_email"]),
        org_id: non_empty(&a["organization_uuid"]),
        org_name: non_empty(&a["organization_name"]),
    })
}

fn resolve_identity(data: &Value, include_org: bool) -> Identity {
    let identity = identity_from_token(data);
    let org_satisfied = !include_org || identity.org_id.is_some();
    if identity.account_id.is_some() && identity.email.is_some() && org_satisfied {
        return identity;
    }
    match bootstrap_identity(data["access_token"].as_str().unwrap_or("")) {
        Ok(b) => Identity {
            account_id: identity.account_id.or(b.account_id),
            email: identity.email.or(b.email),
            org_id: identity.org_id.or(b.org_id),
            org_name: identity.org_name.or(b.org_name),
        },
        Err(_) => identity,
    }
}

fn expires_at(data: &Value) -> u64 {
    (now_ms() + data["expires_in"].as_u64().unwrap_or(0) * 1000).saturating_sub(EXPIRY_MARGIN_MS)
}

const IPV4_LOOPBACK: &str = "127.0.0.1";
const IPV6_LOOPBACK: &str = "::1";
const IPV6_COMPANION_ATTEMPTS: usize = 4;
const LAUNCH_PATH: &str = "/launch";
const PROVIDER_NAME: &str = "Claude Pro/Max";

fn bind_pair(port: u16) -> std::io::Result<Vec<TcpListener>> {
    let mut attempt = 0;
    loop {
        let primary = TcpListener::bind((IPV4_LOOPBACK, port))?;
        let bound = primary.local_addr()?.port();
        match TcpListener::bind((IPV6_LOOPBACK, bound)) {
            Ok(companion) => return Ok(vec![primary, companion]),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if port != 0 || attempt >= IPV6_COMPANION_ATTEMPTS {
                    return Err(e);
                }
                attempt += 1;
            }
            Err(_) => return Ok(vec![primary]),
        }
    }
}

fn bind_callback() -> Result<(Vec<TcpListener>, u16), String> {
    let listeners = match bind_pair(CALLBACK_PORT) {
        Ok(l) => l,
        Err(_) => {
            let l = bind_pair(0).map_err(|e| format!("could not open a local port for the {PROVIDER_NAME} sign-in callback: {e}"))?;
            let actual = l[0].local_addr().map_err(|e| e.to_string())?.port();
            eprintln!("Preferred port {CALLBACK_PORT} unavailable, using port {actual}");
            l
        }
    };
    let port = listeners[0].local_addr().map_err(|e| e.to_string())?.port();
    for l in &listeners {
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
    }
    Ok((listeners, port))
}

fn open_browser(url: &str) -> bool {
    let program = if cfg!(target_os = "macos") { "open" } else if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success() || cfg!(windows))
}

fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn callback_page(received: bool, detail: Option<&str>) -> String {
    let close = r#"<button type="button" id="close">Close tab</button>
  <script>(function(){
  var b=document.getElementById('close');
  if(!b)return;
  b.addEventListener('click',function(){
    window.close();
    setTimeout(function(){
      b.disabled=true;
      b.textContent="Your browser kept this tab";
      var n=document.getElementById('hint');
      if(n)n.textContent="Close it yourself and head back to Stem.";
    },180);
  });
})();</script>"#;
    let body = if received {
        format!("<h1>{} authorization received</h1>\n  <p class=\"lede\" id=\"hint\">Stem is finishing the sign-in. Check Stem for the result; this tab has done its job.</p>\n  {close}", escape_html(PROVIDER_NAME))
    } else {
        format!(
            "<h1>Sign-in to {} failed</h1>\n  <pre>{}</pre>\n  <p class=\"lede\" id=\"hint\">Nothing was stored. Close this tab and try again from Stem.</p>\n  {close}",
            escape_html(PROVIDER_NAME),
            escape_html(detail.unwrap_or("Unknown error"))
        )
    };
    format!(
        r#"<!doctype html><html lang="en" dir="ltr"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Stem: {title}</title>
<style>
  :root{{--brand-primary:#a6dcf0;--brand-secondary:#eca3c4;--ink:#e8e6f0;--dim:#9c98ae}}
  *{{box-sizing:border-box}}
  body{{margin:0;display:grid;place-items:center;min-height:100vh;background:#0d0b14;color:var(--ink);
    font:15px/1.6 -apple-system,BlinkMacSystemFont,system-ui,"Segoe UI",sans-serif;
    background-image:radial-gradient(60% 45% at 50% 0%,rgb(166 220 240/.10),transparent 70%)}}
  .card{{width:min(92vw,430px);padding:44px 40px 36px;border-radius:22px;text-align:center;
    background:linear-gradient(180deg,#191525,#141020);border:1px solid rgb(255 255 255/.07);
    box-shadow:0 30px 90px rgb(0 0 0/.55)}}
  h1{{font-size:20px;line-height:1.3;margin:0 0 8px;letter-spacing:-.01em}}
  p{{margin:0;color:var(--dim);font-size:14px}}
  .lede{{margin-bottom:22px}}
  pre{{white-space:pre-wrap;word-break:break-word;text-align:left;margin:0 0 20px;
    background:#0d0b14;border:1px solid rgb(236 163 196/.18);border-radius:12px;
    padding:12px 14px;color:var(--brand-secondary);font-size:12.5px;line-height:1.5}}
  button{{font:inherit;font-size:14px;font-weight:550;cursor:pointer;padding:9px 20px;border-radius:11px;
    color:#0d0b14;background:var(--brand-primary);border:0;transition:transform .12s ease,filter .12s ease}}
  button:hover:not(:disabled){{filter:brightness(1.07);transform:translateY(-1px)}}
  button:disabled{{cursor:default;color:var(--dim);background:rgb(255 255 255/.06)}}
  @media (prefers-reduced-motion:reduce){{button{{transition:none}}}}
</style></head><body><div class="card">
  {body}
</div></body></html>"#,
        title = escape_html(PROVIDER_NAME)
    )
}

fn respond(stream: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &str) {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    let _ = write!(stream, "{head}\r\n{body}");
    let _ = stream.flush();
}

fn is_loopback_host(host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let name = host.rsplit_once(':').filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())).map_or(host, |(name, _)| name);
    ["localhost", "127.0.0.1", "[::1]"].iter().any(|h| h.eq_ignore_ascii_case(name))
}

struct Callback {
    listeners: Vec<TcpListener>,
    expected_state: String,
    auth_url: String,
    settled: bool,
}

impl Callback {
    fn poll(&mut self) -> Option<Result<String, String>> {
        for i in 0..self.listeners.len() {
            let Ok((stream, _)) = self.listeners[i].accept() else { continue };
            if let Some(outcome) = self.handle(stream) {
                return Some(outcome);
            }
        }
        None
    }

    fn handle(&mut self, mut stream: TcpStream) -> Option<Result<String, String>> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).ok()?;
        let mut host = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.trim().eq_ignore_ascii_case("host")
            {
                host = Some(v.trim().to_string());
            }
        }
        if !is_loopback_host(host.as_deref()) {
            respond(&mut stream, "403 Forbidden", &[], "Forbidden");
            return None;
        }
        let target = request_line.split_whitespace().nth(1).unwrap_or("");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != CALLBACK_PATH {
            if path == LAUNCH_PATH {
                if self.auth_url.is_empty() {
                    respond(&mut stream, "503 Service Unavailable", &[], "OAuth launch URL is no longer active");
                } else {
                    respond(&mut stream, "302 Found", &[("Location", &self.auth_url)], "");
                }
                return None;
            }
            respond(&mut stream, "404 Not Found", &[], "Not Found");
            return None;
        }
        let code = query_param(query, "code").filter(|c| !c.is_empty());
        let state = query_param(query, "state").unwrap_or_default();
        let error = query_param(query, "error").unwrap_or_default();
        let description = query_param(query, "error_description").unwrap_or_else(|| error.clone());
        let result = if !error.is_empty() {
            Err(format!("Authorization failed: {description}"))
        } else if let Some(code) = code {
            if !self.expected_state.is_empty() && state != self.expected_state { Err("State mismatch — possible CSRF attack".to_string()) } else { Ok(code) }
        } else {
            Err("Missing authorization code".to_string())
        };
        let html = [("Content-Type", "text/html; charset=utf-8"), ("Cache-Control", "no-store")];
        match result {
            Ok(found) => {
                if self.settled {
                    respond(&mut stream, "400 Bad Request", &[], "This authorization was already completed.");
                    return None;
                }
                self.settled = true;
                respond(&mut stream, "200 OK", &html, &callback_page(true, None));
                Some(Ok(found))
            }
            Err(message) => {
                respond(&mut stream, "400 Bad Request", &html, &callback_page(false, Some(&message)));
                (!error.is_empty() && (self.expected_state.is_empty() || state == self.expected_state)).then_some(Err(message))
            }
        }
    }
}

fn parse_callback_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    if let Some((_, query)) = value.split_once("://").and_then(|(_, rest)| rest.split_once('?')) {
        let query = query.split('#').next().unwrap_or("");
        return (query_param(query, "code"), query_param(query, "state"));
    }
    if value.contains("code=") {
        let query = value.trim_start_matches(['?', '#']);
        return (query_param(query, "code"), query_param(query, "state"));
    }
    let mut parts = value.splitn(2, '#');
    (parts.next().map(str::to_string), parts.next().map(str::to_string))
}

struct RawMode;

impl RawMode {
    fn enable() -> Option<RawMode> {
        crossterm::terminal::enable_raw_mode().ok().map(|_| RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

enum Typed {
    Waiting,
    Line(String),
    Cancelled,
}

fn read_typed(buffer: &mut String) -> Typed {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, poll, read};
    while poll(Duration::from_millis(0)).unwrap_or(false) {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Char('c') | KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => return Typed::Cancelled,
                KeyCode::Esc => return Typed::Cancelled,
                KeyCode::Enter => {
                    eprint!("\r\n");
                    return Typed::Line(std::mem::take(buffer));
                }
                KeyCode::Backspace => {
                    if buffer.pop().is_some() {
                        eprint!("\x08 \x08");
                    }
                }
                KeyCode::Char(c) => {
                    buffer.push(c);
                    eprint!("{c}");
                }
                _ => {}
            },
            Ok(Event::Paste(text)) => {
                let text = text.trim().to_string();
                eprint!("{text}");
                buffer.push_str(&text);
            }
            _ => {}
        }
        let _ = std::io::stderr().flush();
    }
    Typed::Waiting
}

fn wait_for_callback(callback: &mut Callback) -> Result<String, String> {
    use std::io::IsTerminal;
    let started = Instant::now();
    let mut raw = (std::io::stdin().is_terminal() && std::io::stderr().is_terminal()).then(RawMode::enable).flatten();
    let mut typed = String::new();
    if raw.is_some() {
        eprint!("Paste the redirect URL, or just the code: ");
        let _ = std::io::stderr().flush();
    }
    loop {
        if started.elapsed() > CALLBACK_TIMEOUT {
            drop(raw.take());
            eprintln!();
            return Err(format!("{PROVIDER_NAME} sign-in timed out after 5 minutes"));
        }
        if let Some(outcome) = callback.poll() {
            if raw.take().is_some() {
                eprintln!();
            }
            return outcome;
        }
        if raw.is_some() {
            match read_typed(&mut typed) {
                Typed::Waiting => {}
                Typed::Cancelled => {
                    drop(raw.take());
                    eprintln!();
                    return Err("Sign-in cancelled.".into());
                }
                Typed::Line(line) if line.trim().is_empty() => drop(raw.take()),
                Typed::Line(line) => match parse_callback_input(&line) {
                    (Some(code), state) if !code.is_empty() && (callback.expected_state.is_empty() || state.as_deref() == Some(callback.expected_state.as_str())) => {
                        drop(raw.take());
                        return Ok(code);
                    }
                    _ => {
                        eprint!("Paste the redirect URL, or just the code: ");
                        let _ = std::io::stderr().flush();
                    }
                },
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn exchange_token(code: &str, state: &str, redirect_uri: &str, verifier: &str) -> Result<Credentials, String> {
    let (exchange_code, exchange_state) = match code.split_once('#') {
        Some((c, s)) if !s.is_empty() => (c, s),
        Some((c, _)) => (c, state),
        None => (code, state),
    };
    let data = post_json(
        TOKEN_URL,
        json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": exchange_code,
            "state": exchange_state,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        }),
        &[],
    )
    .map_err(|e| format!("Token exchange request failed. redirect_uri={redirect_uri}; details={e}"))?;
    let identity = resolve_identity(&data, true);
    Ok(Credentials {
        access: data["access_token"].as_str().unwrap_or("").to_string(),
        refresh: data["refresh_token"].as_str().unwrap_or("").to_string(),
        expires: expires_at(&data),
        account_id: identity.account_id,
        email: identity.email,
        org_id: identity.org_id,
        org_name: identity.org_name,
        authorized_at: Some(now_ms()),
    })
}

pub fn login() -> Result<Option<String>, String> {
    let (listeners, port) = bind_callback()?;
    let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
    let launch_url = format!("http://localhost:{port}{LAUNCH_PATH}");
    let state = hex(&random_bytes(16));
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes(96));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let params = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", &redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
    ];
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect();
    let auth_url = format!("{AUTHORIZE_URL}?{}", query.join("&"));
    let mut callback = Callback { listeners, expected_state: state.clone(), auth_url, settled: false };
    eprintln!("Go to: {launch_url}");
    eprintln!("Complete the login in your browser. If the browser cannot reach this machine, paste the code the authorization page shows (code#state) or the full redirect URL here.");
    eprintln!("{}", if open_browser(&launch_url) { "Browser opened. Approve the connection, then return here." } else { "Could not open a browser automatically. Open the URL above manually." });
    eprintln!("Waiting for browser authentication…");
    let found = wait_for_callback(&mut callback);
    callback.auth_url.clear();
    let found = found?;
    eprintln!("Exchanging authorization code for tokens…");
    let credentials = exchange_token(&found, &state, &redirect_uri, &verifier)?;
    save(&credentials)?;
    Ok(credentials.email)
}

fn refresh(credentials: &Credentials) -> Result<Credentials, String> {
    let data = post_json(
        TOKEN_URL,
        json!({"grant_type": "refresh_token", "client_id": CLIENT_ID, "refresh_token": credentials.refresh}),
        &[("anthropic-beta", OAUTH_BETA.into()), ("User-Agent", format!("anthropic-sdk-typescript/{SDK_VERSION} userOAuthProvider"))],
    )
    .map_err(|e| format!("Anthropic token refresh request failed. details={e}. Run `stem login {PROVIDER}` again."))?;
    let identity = resolve_identity(&data, false);
    Ok(Credentials {
        access: data["access_token"].as_str().unwrap_or("").to_string(),
        refresh: data["refresh_token"].as_str().filter(|r| !r.is_empty()).unwrap_or(&credentials.refresh).to_string(),
        expires: expires_at(&data),
        account_id: identity.account_id.or_else(|| credentials.account_id.clone()),
        email: identity.email.or_else(|| credentials.email.clone()),
        org_id: credentials.org_id.clone(),
        org_name: credentials.org_name.clone(),
        authorized_at: credentials.authorized_at,
    })
}

fn access_token(force_refresh: bool) -> Result<Credentials, String> {
    let _guard = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    let credentials = load().ok_or_else(|| format!("not signed in to Claude. Run `stem login {PROVIDER}` (or /login {PROVIDER})."))?;
    if !force_refresh && now_ms() < credentials.expires {
        return Ok(credentials);
    }
    let fresh = refresh(&credentials)?;
    save(&fresh)?;
    Ok(fresh)
}

fn version_file() -> PathBuf {
    crate::env::stem_home().join("cache").join("claude-code-version.json")
}

fn valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (2..=4).contains(&parts.len()) && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let nums = |v: &str| v.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    nums(a).cmp(&nums(b))
}

fn learned_version() -> Option<String> {
    let mut learned = LEARNED_VERSION.lock().unwrap_or_else(|e| e.into_inner());
    learned
        .get_or_insert_with(|| {
            let raw: Value = serde_json::from_str(&std::fs::read_to_string(version_file()).ok()?).ok()?;
            raw["version"].as_str().filter(|v| valid_version(v)).map(str::to_string)
        })
        .clone()
}

fn version() -> String {
    match learned_version() {
        Some(v) if compare_versions(&v, CLAUDE_CODE_VERSION).is_gt() => v,
        _ => CLAUDE_CODE_VERSION.to_string(),
    }
}

fn client_version_gate(body: &str) -> Option<String> {
    let text: String = body.chars().take(4000).collect();
    let lower = text.to_lowercase();
    if !text.contains("claude_code_version_too_old") && !lower.contains("does not support this model") {
        return None;
    }
    let re = regex::Regex::new(r"(?i)version\s+(\d+(?:\.\d+){1,3})\s+or\s+newer").ok()?;
    re.captures(&text).map(|c| c[1].to_string())
}

pub fn learn_version(body: &str) -> bool {
    let Some(v) = client_version_gate(body) else { return false };
    if !valid_version(&v) || compare_versions(&v, &version()).is_le() {
        return false;
    }
    *LEARNED_VERSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(Some(v.clone()));
    let _ = std::fs::create_dir_all(crate::env::stem_home().join("cache"));
    let _ = std::fs::write(version_file(), json!({"version": v, "learnedAt": now_ms()}).to_string());
    true
}

fn user_agent() -> String {
    format!("claude-cli/{} (external, sdk-cli)", version())
}

fn stainless_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".into(),
        "aarch64" => "arm64".into(),
        "x86" => "x86".into(),
        other => format!("other::{other}"),
    }
}

fn stainless_os() -> String {
    match std::env::consts::OS {
        "macos" => "MacOS".into(),
        "ios" => "iOS".into(),
        "android" => "Android".into(),
        "windows" => "Windows".into(),
        "freebsd" => "FreeBSD".into(),
        "openbsd" => "OpenBSD".into(),
        "linux" => "Linux".into(),
        other => format!("Other:{other}"),
    }
}

fn stainless_headers() -> [(&'static str, String); 8] {
    [
        ("X-Stainless-Arch", stainless_arch()),
        ("X-Stainless-Lang", "js".into()),
        ("X-Stainless-OS", stainless_os()),
        ("X-Stainless-Package-Version", SDK_VERSION.into()),
        ("X-Stainless-Retry-Count", "0".into()),
        ("X-Stainless-Runtime", "node".into()),
        ("X-Stainless-Runtime-Version", "v26.12.1".into()),
        ("X-Stainless-Timeout", "600".into()),
    ]
}

fn betas(agent_request: bool, thinking_request: bool) -> String {
    let mut list: Vec<&str> = if agent_request { AGENT_BETAS.to_vec() } else { UTILITY_BETAS.to_vec() };
    if agent_request && thinking_request {
        list.push(EFFORT_BETA);
    }
    list.join(",")
}

fn billing_header(first_user_text: &str) -> String {
    let v = version();
    let chars: Vec<char> = first_user_text.chars().collect();
    let k: String = [4, 7, 20].iter().map(|&i| chars.get(i).copied().unwrap_or('0')).collect();
    let suffix = &sha256_hex(&["59cf53e54c78", &k, &v])[..3];
    format!("{BILLING_PREFIX} cc_version={v}.{suffix}; cc_entrypoint=sdk-cli; {CCH_PLACEHOLDER};")
}

fn first_user_text(messages: &Value) -> String {
    let Some(m) = messages.as_array().and_then(|ms| ms.iter().find(|m| m["role"] == "user")) else { return String::new() };
    if let Some(s) = m["content"].as_str() {
        return s.to_string();
    }
    m["content"].as_array().and_then(|parts| parts.iter().find(|p| p["type"] == "text")).and_then(|p| p["text"].as_str()).unwrap_or("").to_string()
}

fn scope_shared_instructions(mut blocks: Vec<Value>) -> Vec<Value> {
    if blocks.len() > 1
        && let Some(mark) = blocks[0].get_mut("cache_control").and_then(Value::as_object_mut)
        && !mark.contains_key("scope")
    {
        mark.insert("scope".into(), GLOBAL_CACHE_SCOPE.into());
    }
    blocks
}

fn patch_cch(body: &mut [u8]) {
    let Some(marker) = find(body, BILLING_PREFIX.as_bytes(), 0) else { return };
    let from = marker + BILLING_PREFIX.len();
    let Some(at) = find(body, CCH_PLACEHOLDER.as_bytes(), from).filter(|at| at - from <= CCH_SEARCH_WINDOW) else { return };
    let cch = format!("{:05x}", xxhash_rust::xxh64::xxh64(body, CCH_SEED) & 0xfffff);
    body[at + 4..at + 9].copy_from_slice(cch.as_bytes());
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

pub struct Link {
    session_id: String,
    device: Mutex<Option<String>>,
}

impl Default for Link {
    fn default() -> Link {
        Link { session_id: uuid_v4(), device: Mutex::new(None) }
    }
}

pub struct Prepared {
    pub headers: Vec<(&'static str, String)>,
    pub payload: Vec<u8>,
}

impl Link {
    fn user_id(&self, account_id: Option<&str>) -> String {
        let mut device = self.device.lock().unwrap_or_else(|e| e.into_inner());
        let install = device.get_or_insert_with(install_id).clone();
        let device_id = match account_id.filter(|a| !a.is_empty()) {
            Some(a) => sha256_hex(&[DEVICE_ID_ACCOUNT_DOMAIN, "\0", &install, "\0", a]),
            None => sha256_hex(&[DEVICE_ID_INSTALL_DOMAIN, &install]),
        };
        let mut user = format!("{{\"device_id\":\"{device_id}\",\"session_id\":\"{}\"", self.session_id);
        if let Some(a) = account_id.filter(|a| !a.is_empty()) {
            user.push_str(&format!(",\"account_uuid\":{}", Value::from(a)));
        }
        user.push('}');
        user
    }

    pub fn prepare(&self, body: &Value, force_refresh: bool) -> Result<Prepared, String> {
        let credentials = access_token(force_refresh)?;
        let agent_request = body["tools"].as_array().is_some_and(|t| !t.is_empty());
        let thinking_request = !body["thinking"].is_null() || body["effort"].is_string() || body["output_config"]["effort"].is_string();
        let existing: Vec<Value> = match &body["system"] {
            Value::String(s) if !s.is_empty() => vec![json!({"type": "text", "text": s})],
            Value::Array(a) => a.iter().filter(|b| b["type"] == "text").cloned().collect(),
            _ => Vec::new(),
        };
        let mut sent = body.clone();
        if !existing.first().and_then(|b| b["text"].as_str()).is_some_and(|t| t.starts_with(BILLING_PREFIX)) {
            let mut system = vec![json!({"type": "text", "text": billing_header(&first_user_text(&body["messages"]))}), json!({"type": "text", "text": SYSTEM_INSTRUCTION})];
            system.extend(scope_shared_instructions(existing));
            sent["system"] = Value::Array(system);
            sent["metadata"]["user_id"] = Value::from(self.user_id(credentials.account_id.as_deref()));
        }
        let mut payload = sent.to_string().into_bytes();
        patch_cch(&mut payload);
        let mut headers: Vec<(&'static str, String)> = vec![
            ("Accept", "application/json".into()),
            ("Content-Type", "application/json".into()),
            ("User-Agent", user_agent()),
        ];
        headers.extend(stainless_headers());
        headers.extend([
            ("anthropic-beta", betas(agent_request, thinking_request)),
            ("anthropic-dangerous-direct-browser-access", "true".into()),
            ("anthropic-version", "2023-06-01".into()),
            ("Authorization", format!("Bearer {}", credentials.access)),
            ("x-app", "cli".into()),
            ("x-client-request-id", uuid_v4()),
            ("X-Claude-Code-Session-Id", self.session_id.clone()),
        ]);
        Ok(Prepared { headers, payload })
    }
}

pub fn messages_url() -> String {
    format!("{BASE_URL}/messages?beta=true")
}
