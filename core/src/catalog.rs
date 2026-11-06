use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

const CATALOG_URL: &str = "https://models.dev/api.json";
const REVALIDATE_AFTER: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Protocol {
    Anthropic,
    OpenAiChat,
    ClaudeCode,
    ClaudeNative,
}

#[derive(Clone, Debug, Default)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reasoning {
    None,
    Effort(Vec<String>),
    Budget { min: u64, max: Option<u64> },
    Always,
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context: u64,
    pub output: u64,
    pub cost: Cost,
    pub tool_call: bool,
    pub reasoning: Reasoning,
    pub release_date: String,
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub provider_id: String,
    pub provider_name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub key: String,
    pub key_source: String,
    pub gateway: bool,
    pub model: ModelInfo,
    pub why: String,
}

pub struct Catalog {
    index: Value,
    loaded: RefCell<HashMap<String, Rc<Value>>>,
    pub source: String,
}

fn cache_dir() -> PathBuf {
    crate::env::stem_home().join("cache").join("models.dev")
}

impl Catalog {
    pub fn load() -> Result<Catalog, String> {
        let dir = cache_dir();
        let index_path = dir.join("index.json");
        let etag_path = dir.join("etag");
        let has_index = index_path.is_file();
        let fresh = std::fs::metadata(&index_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < REVALIDATE_AFTER);
        if has_index && fresh {
            return Self::from_index(&index_path, "models.dev (cached)");
        }
        let etag = std::fs::read_to_string(&etag_path).ok().filter(|_| has_index);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        let mut request = agent.get(CATALOG_URL);
        if let Some(tag) = &etag {
            request = request.header("If-None-Match", tag.trim());
        }
        match request.call() {
            Ok(response) if response.status() == 304 => {
                let _ = std::fs::File::options()
                    .write(true)
                    .open(&index_path)
                    .and_then(|f| f.set_modified(SystemTime::now()));
                Self::from_index(&index_path, "models.dev (unchanged)")
            }
            Ok(mut response) if response.status() == 200 => {
                let new_etag = response.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
                let body = response
                    .body_mut()
                    .with_config()
                    .limit(64 * 1024 * 1024)
                    .read_to_vec()
                    .map_err(|e| format!("reading models.dev: {e}"))?;
                let full: Value = serde_json::from_slice(&body).map_err(|e| format!("models.dev returned invalid JSON: {e}"))?;
                drop(body);
                let index = split(&full, &dir)?;
                if let Some(tag) = new_etag {
                    let _ = std::fs::write(&etag_path, tag);
                }
                Ok(Catalog {
                    index,
                    loaded: RefCell::default(),
                    source: "models.dev (fetched)".into(),
                })
            }
            other if has_index => {
                let _ = other;
                Self::from_index(&index_path, "models.dev (offline copy)")
            }
            other => Err(format!(
                "cannot reach {CATALOG_URL} and no cached copy exists: {}",
                match other {
                    Ok(r) => format!("HTTP {}", r.status()),
                    Err(e) => e.to_string(),
                }
            )),
        }
    }

    fn from_index(path: &std::path::Path, source: &str) -> Result<Catalog, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let index: Value = serde_json::from_str(&text).map_err(|e| format!("broken catalog cache, delete {}: {e}", path.display()))?;
        Ok(Catalog {
            index,
            loaded: RefCell::default(),
            source: source.into(),
        })
    }

    fn providers(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.index.as_object().into_iter().flatten()
    }

    fn full(&self, id: &str) -> Option<Rc<Value>> {
        if let Some(v) = self.loaded.borrow().get(id) {
            return Some(v.clone());
        }
        let value = Rc::new(read_provider(id)?);
        self.loaded.borrow_mut().insert(id.to_string(), value.clone());
        Some(value)
    }

    pub fn resolve(&self, spec: Option<&str>) -> Result<Resolved, String> {
        let spec = spec
            .map(str::to_string)
            .or_else(|| std::env::var("STEM_MODEL").ok())
            .filter(|s| !s.trim().is_empty());
        match spec {
            Some(spec) => self.resolve_spec(spec.trim()),
            None => self.resolve_from_keys(),
        }
    }

    fn resolve_spec(&self, spec: &str) -> Result<Resolved, String> {
        if spec == crate::claude_native::PROVIDER || spec == crate::claude_native::ALIAS {
            return self.claude_native(None, format!("chosen: {spec}"));
        }
        if let Some((provider_id, model_id)) = spec.split_once('/')
            && (provider_id == crate::claude_native::PROVIDER || provider_id == crate::claude_native::ALIAS) {
                return self.claude_native(Some(model_id), format!("chosen: {spec}"));
            }
        if spec == crate::claude_code::PROVIDER {
            return self.claude_code(None, format!("chosen: {spec}"));
        }
        if let Some((provider_id, model_id)) = spec.split_once('/')
            && provider_id == crate::claude_code::PROVIDER {
                return self.claude_code(Some(model_id), format!("chosen: {spec}"));
            }
        if let Some((provider_id, model_id)) = spec.split_once('/')
            && let Some(entry) = self.index.get(provider_id) {
                if has_model(entry, model_id) {
                    return self.build(provider_id, model_id, format!("chosen: {spec}"));
                }
                return Err(unknown_model(provider_id, entry, model_id));
            }
        let mut candidates: Vec<(&String, &Value)> = self
            .providers()
            .filter(|(_, p)| has_model(p, spec) && key_for(p).is_some() && protocol_of(p).is_some())
            .collect();
        candidates.sort_by_key(|(_, p)| p["gateway"].as_bool().unwrap_or(false));
        match candidates.first() {
            Some((id, _)) => self.build(id, spec, format!("{spec} through {id}, the provider with a key set")),
            None => {
                let anywhere: Vec<&String> = self.providers().filter(|(_, p)| has_model(p, spec)).map(|(id, _)| id).take(8).collect();
                if anywhere.is_empty() {
                    Err(format!("no provider on models.dev lists a model called {spec}. Try `stem models {spec}`."))
                } else {
                    Err(format!(
                        "{spec} exists on {} but none of them has an API key set. Set the key, or use provider/model.",
                        anywhere.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    ))
                }
            }
        }
    }

    fn resolve_from_keys(&self) -> Result<Resolved, String> {
        let mut with_keys: Vec<(&String, &Value)> = self.providers().filter(|(_, p)| key_for(p).is_some() && protocol_of(p).is_some()).collect();
        with_keys.sort_by_key(|(id, p)| (p["gateway"].as_bool().unwrap_or(false), *id != "anthropic"));
        let Some((id, entry)) = with_keys.first() else {
            if crate::claude_native::signed_in() {
                return self.claude_native(None, "no API key set; signed in to Claude, so your Claude plan".into());
            }
            if crate::claude_code::binary().is_some() {
                return self.claude_code(None, "no API key set; Claude Code is installed, so your Claude plan".into());
            }
            return Err("no API key found. Put one in ~/.stem/.env, for example ANTHROPIC_API_KEY=..., or pass --model provider/model.".into());
        };
        let provider = self.full(id).ok_or_else(|| format!("no cached data for {id}"))?;
        let model_id = default_model(&provider).ok_or_else(|| format!("{id} lists no model with tool calling"))?;
        let env_name = key_for(entry).map(|(name, _)| name).unwrap_or_default();
        self.build(id, &model_id, format!("{env_name} is set; newest coding model on {id}"))
    }

    fn build(&self, provider_id: &str, model_id: &str, why: String) -> Result<Resolved, String> {
        let provider = self.full(provider_id).ok_or_else(|| format!("no cached data for {provider_id}"))?;
        let protocol = protocol_of(&provider).ok_or_else(|| {
            format!(
                "{provider_id} speaks {}, which Stem does not support yet. Supported: Anthropic and OpenAI-compatible providers and gateways.",
                provider["npm"].as_str().unwrap_or("an unknown API")
            )
        })?;
        let (key_source, key) = match std::env::var("STEM_API_KEY") {
            Ok(k) if !k.is_empty() => ("STEM_API_KEY".to_string(), k),
            _ => key_for(&provider).ok_or_else(|| {
                format!(
                    "{provider_id} needs one of {} to be set (in ~/.stem/.env or the shell)",
                    env_names(&provider).join(", ")
                )
            })?,
        };
        let raw = &provider["models"][model_id];
        let mut model = model_info(raw);
        if let Some((origin, id)) = raw["canonical_model_id"].as_str().and_then(|c| c.split_once('/'))
            && let Some(origin_provider) = self.full(origin)
                && let Some(origin_model) = origin_provider["models"].get(id) {
                    fill_missing(&mut model, &model_info(origin_model));
                }
        let base_url = provider["api"]
            .as_str()
            .map(str::to_string)
            .or_else(|| default_base(provider_id, protocol))
            .ok_or_else(|| format!("models.dev gives no API address for {provider_id}"))?;
        Ok(Resolved {
            provider_id: provider_id.to_string(),
            provider_name: provider["name"].as_str().unwrap_or(provider_id).to_string(),
            protocol,
            base_url: base_url.trim_end_matches('/').to_string(),
            key,
            key_source,
            gateway: is_gateway(&provider),
            model,
            why,
        })
    }

    fn claude_code(&self, model_id: Option<&str>, why: String) -> Result<Resolved, String> {
        let provider = self.full("anthropic").ok_or("models.dev has no data for anthropic")?;
        let model_id = match model_id {
            Some(id) => id.to_string(),
            None => default_model(&provider).ok_or("anthropic lists no coding model")?,
        };
        let raw = provider["models"].get(&model_id).ok_or_else(|| unknown_model("anthropic", &self.index["anthropic"], &model_id))?;
        let bin = crate::claude_code::binary().ok_or("claude-code needs the claude command: install Claude Code, then run `stem login claude`")?;
        Ok(Resolved {
            provider_id: crate::claude_code::PROVIDER.into(),
            provider_name: "Claude Code".into(),
            protocol: Protocol::ClaudeCode,
            base_url: bin.display().to_string(),
            key: String::new(),
            key_source: "Claude Code sign-in".into(),
            gateway: false,
            model: plan_model(raw),
            why,
        })
    }

    fn claude_native(&self, model_id: Option<&str>, why: String) -> Result<Resolved, String> {
        let provider = self.full("anthropic").ok_or("models.dev has no data for anthropic")?;
        let model_id = match model_id {
            Some(id) => id.to_string(),
            None => default_model(&provider).ok_or("anthropic lists no coding model")?,
        };
        let raw = provider["models"].get(&model_id).ok_or_else(|| unknown_model("anthropic", &self.index["anthropic"], &model_id))?;
        if !crate::claude_native::signed_in() {
            return Err(format!("not signed in to Claude: run `stem login {}`", crate::claude_native::PROVIDER));
        }
        Ok(Resolved {
            provider_id: crate::claude_native::PROVIDER.into(),
            provider_name: "Claude Pro/Max".into(),
            protocol: Protocol::ClaudeNative,
            base_url: crate::claude_native::BASE_URL.into(),
            key: String::new(),
            key_source: format!("Claude sign-in ({})", crate::claude_native::auth_path()),
            gateway: false,
            model: native_model(raw),
            why,
        })
    }

    pub fn cheapest_sibling(&self, resolved: &Resolved) -> Option<ModelInfo> {
        let native = resolved.protocol == Protocol::ClaudeNative;
        let plan = native || resolved.protocol == Protocol::ClaudeCode;
        let provider = self.full(if plan { "anthropic" } else { &resolved.provider_id })?;
        if plan {
            let newest = newest_release(&provider);
            return provider["models"]
                .as_object()?
                .values()
                .filter(|m| usable(m) && release_within(m, &newest, 240) && m["cost"]["input"].as_f64().unwrap_or(0.0) > 0.0)
                .min_by(|a, b| a["cost"]["input"].as_f64().partial_cmp(&b["cost"]["input"].as_f64()).unwrap_or(std::cmp::Ordering::Equal).then(b["release_date"].as_str().cmp(&a["release_date"].as_str())))
                .map(if native { native_model } else { plan_model });
        }
        let newest = newest_release(&provider);
        provider["models"]
            .as_object()?
            .values()
            .filter(|m| usable(m) && release_within(m, &newest, 240))
            .map(model_info)
            .filter(|m| m.cost.input > 0.0)
            .min_by(|a, b| {
                a.cost
                    .input
                    .partial_cmp(&b.cost.input)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.release_date.cmp(&a.release_date))
            })
    }

    pub fn list(&self, filter: Option<&str>) -> Vec<(String, ModelInfo, bool)> {
        let filter = filter.map(str::to_lowercase);
        let mut rows = Vec::new();
        for (pid, entry) in self.providers() {
            let has_key = key_for(entry).is_some();
            if filter.is_none() && !has_key {
                continue;
            }
            let wanted = |mid: &str| filter.as_ref().is_none_or(|f| format!("{pid}/{mid}").to_lowercase().contains(f));
            if !entry["models"].as_array().into_iter().flatten().any(|m| m.as_str().is_some_and(wanted)) {
                continue;
            }
            let Some(provider) = read_provider(pid) else { continue };
            for (mid, raw) in provider["models"].as_object().into_iter().flatten() {
                if wanted(mid) && usable(raw) {
                    rows.push((format!("{pid}/{mid}"), model_info(raw), has_key));
                }
            }
        }
        if crate::claude_native::signed_in()
            && let Some(provider) = read_provider("anthropic") {
                for (mid, raw) in provider["models"].as_object().into_iter().flatten() {
                    let id = format!("{}/{mid}", crate::claude_native::PROVIDER);
                    if usable(raw) && filter.as_ref().is_none_or(|f| id.to_lowercase().contains(f)) {
                        rows.push((id, native_model(raw), true));
                    }
                }
            }
        if crate::claude_code::binary().is_some()
            && let Some(provider) = read_provider("anthropic") {
                for (mid, raw) in provider["models"].as_object().into_iter().flatten() {
                    let id = format!("{}/{mid}", crate::claude_code::PROVIDER);
                    if usable(raw) && filter.as_ref().is_none_or(|f| id.to_lowercase().contains(f)) {
                        rows.push((id, plan_model(raw), true));
                    }
                }
            }
        rows.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.release_date.cmp(&a.1.release_date)));
        rows
    }
}

fn read_provider(id: &str) -> Option<Value> {
    let text = std::fs::read_to_string(cache_dir().join("providers").join(format!("{}.json", safe_name(id)))).ok()?;
    serde_json::from_str(&text).ok()
}

fn safe_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn split(full: &Value, dir: &std::path::Path) -> Result<Value, String> {
    let providers_dir = dir.join("providers");
    let _ = std::fs::remove_dir_all(&providers_dir);
    std::fs::create_dir_all(&providers_dir).map_err(|e| format!("{}: {e}", providers_dir.display()))?;
    let mut index = serde_json::Map::new();
    for (id, provider) in full.as_object().into_iter().flatten() {
        let ids: Vec<Value> = provider["models"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, _)| Value::String(k.clone()))
            .collect();
        index.insert(
            id.clone(),
            serde_json::json!({"name": provider["name"], "env": provider["env"], "npm": provider["npm"], "api": provider["api"], "gateway": is_gateway(provider), "models": ids}),
        );
        std::fs::write(providers_dir.join(format!("{}.json", safe_name(id))), provider.to_string()).map_err(|e| e.to_string())?;
    }
    let index = Value::Object(index);
    std::fs::write(dir.join("index.json"), index.to_string()).map_err(|e| e.to_string())?;
    Ok(index)
}

fn has_model(entry: &Value, id: &str) -> bool {
    entry["models"].as_array().is_some_and(|m| m.iter().any(|v| v == id))
}

fn unknown_model(provider_id: &str, entry: &Value, model_id: &str) -> String {
    let mut close: Vec<&str> = entry["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .filter(|k| k.contains(model_id) || model_id.contains(k) || k.split('-').any(|part| model_id.contains(part) && part.len() > 3))
        .collect();
    close.truncate(8);
    format!(
        "{provider_id} has no model {model_id}. Close: {}",
        if close.is_empty() {
            "none; run `stem models` to list".to_string()
        } else {
            close.join(", ")
        }
    )
}

fn env_names(provider: &Value) -> Vec<String> {
    provider["env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn key_for(provider: &Value) -> Option<(String, String)> {
    env_names(provider)
        .into_iter()
        .find_map(|name| std::env::var(&name).ok().filter(|v| !v.is_empty()).map(|v| (name, v)))
}

fn protocol_of(provider: &Value) -> Option<Protocol> {
    match provider["npm"].as_str()? {
        "@ai-sdk/anthropic" => Some(Protocol::Anthropic),
        "@ai-sdk/openai-compatible" | "@ai-sdk/openai" | "@openrouter/ai-sdk-provider" => Some(Protocol::OpenAiChat),
        _ if provider["api"].is_string() => Some(Protocol::OpenAiChat),
        _ => None,
    }
}

fn default_base(provider_id: &str, protocol: Protocol) -> Option<String> {
    match (provider_id, protocol) {
        (_, Protocol::Anthropic) => Some("https://api.anthropic.com/v1".into()),
        ("openai", _) => Some("https://api.openai.com/v1".into()),
        _ => None,
    }
}

fn is_gateway(provider: &Value) -> bool {
    provider["models"].as_object().is_some_and(|models| {
        let routed = models
            .iter()
            .filter(|(id, m)| id.contains('/') || m["canonical_model_id"].as_str().is_some_and(|c| !c.ends_with(&format!("/{id}"))))
            .count();
        routed * 2 > models.len()
    })
}

fn usable(raw: &Value) -> bool {
    raw["tool_call"].as_bool().unwrap_or(false)
        && raw["status"].as_str() != Some("deprecated")
        && raw["modalities"]["output"].as_array().is_none_or(|o| o.iter().any(|v| v == "text"))
}

fn newest_release(provider: &Value) -> String {
    provider["models"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, m)| m["release_date"].as_str())
        .max()
        .unwrap_or("")
        .to_string()
}

fn days(date: &str) -> i64 {
    let parts: Vec<i64> = date.split('-').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [y, m, d, ..] => y * 365 + m * 30 + d,
        [y, m] => y * 365 + m * 30,
        _ => 0,
    }
}

fn release_within(raw: &Value, newest: &str, window: i64) -> bool {
    days(newest) - days(raw["release_date"].as_str().unwrap_or("")) <= window
}

fn default_model(provider: &Value) -> Option<String> {
    let models = provider["models"].as_object()?;
    let coding = |m: &Value| m["description"].as_str().is_some_and(|d| d.to_lowercase().contains("coding"));
    let pick = |want_coding: bool| {
        models
            .iter()
            .filter(|(id, m)| usable(m) && !id.contains("latest") && (!want_coding || coding(m)))
            .max_by(|(ia, a), (ib, b)| a["release_date"].as_str().cmp(&b["release_date"].as_str()).then(ib.len().cmp(&ia.len())))
            .map(|(id, _)| id.clone())
    };
    pick(true).or_else(|| pick(false))
}

fn model_info(raw: &Value) -> ModelInfo {
    let f = |v: &Value| v.as_f64().unwrap_or(0.0);
    let options = raw["reasoning_options"].as_array().cloned().unwrap_or_default();
    let effort = options.iter().find(|o| o["type"] == "effort").map(|o| {
        o["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    });
    let budget = options.iter().find(|o| o["type"] == "budget_tokens");
    let reasoning = match (raw["reasoning"].as_bool().unwrap_or(false), effort, budget) {
        (false, _, _) => Reasoning::None,
        (true, Some(values), _) => Reasoning::Effort(values),
        (true, None, Some(b)) => Reasoning::Budget {
            min: b["min"].as_u64().unwrap_or(1024),
            max: b["max"].as_u64(),
        },
        (true, None, None) => Reasoning::Always,
    };
    ModelInfo {
        id: raw["id"].as_str().unwrap_or("").to_string(),
        name: raw["name"].as_str().unwrap_or("").to_string(),
        context: raw["limit"]["context"].as_u64().unwrap_or(0),
        output: raw["limit"]["output"].as_u64().unwrap_or(0),
        cost: Cost {
            input: f(&raw["cost"]["input"]),
            output: f(&raw["cost"]["output"]),
            cache_read: f(&raw["cost"]["cache_read"]),
            cache_write: f(&raw["cost"]["cache_write"]),
        },
        tool_call: raw["tool_call"].as_bool().unwrap_or(false),
        reasoning,
        release_date: raw["release_date"].as_str().unwrap_or("").to_string(),
    }
}

fn native_model(raw: &Value) -> ModelInfo {
    ModelInfo { cost: Cost::default(), ..model_info(raw) }
}

fn plan_model(raw: &Value) -> ModelInfo {
    ModelInfo { tool_call: false, cost: Cost::default(), ..model_info(raw) }
}

fn fill_missing(model: &mut ModelInfo, origin: &ModelInfo) {
    if model.context == 0 {
        model.context = origin.context;
    }
    if model.output == 0 {
        model.output = origin.output;
    }
    if model.cost.input == 0.0 && model.cost.output == 0.0 {
        model.cost = origin.cost.clone();
    }
    if model.reasoning == Reasoning::None {
        model.reasoning = origin.reasoning.clone();
    }
}
