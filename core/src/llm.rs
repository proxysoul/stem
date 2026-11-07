use crate::catalog::{Protocol, Reasoning, Resolved};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum Block {
    Text(String),
    Thinking { text: String, signature: String },
    RedactedThinking(String),
    Reasoning(Value),
    ToolUse { id: String, name: String, input: Value },
    ToolResult { id: String, content: String, is_error: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub role: Role,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    End,
    ToolUse,
    MaxTokens,
    Other(String),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

#[derive(Debug)]
pub struct Reply {
    pub blocks: Vec<Block>,
    pub stop: Stop,
    pub usage: Usage,
}

pub struct Tool {
    pub name: &'static str,
    pub description: String,
    pub schema: Value,
}

pub struct Client {
    pub resolved: Resolved,
    agent: ureq::Agent,
    effort: RefCell<Option<String>>,
    pub max_output: u64,
    claude: crate::claude_code::Link,
    native: crate::claude_native::Link,
    long_cache: Cell<bool>,
    plain: Cell<bool>,
}

pub const CACHE_TTLS: [&str; 2] = ["5m", "1h"];

fn saved_long_cache() -> bool {
    match std::env::var("STEM_CACHE_TTL") {
        Ok(v) if !v.is_empty() => v == "1h",
        _ => crate::config::Config::load().cache_ttl.as_deref() == Some("1h"),
    }
}

const RETRIES: u32 = 4;

fn efforts_of(reasoning: &Reasoning) -> Vec<String> {
    match reasoning {
        Reasoning::Effort(values) => values.clone(),
        Reasoning::Budget { .. } => ["off", "low", "medium", "high", "max"].iter().map(|s| s.to_string()).collect(),
        _ => Vec::new(),
    }
}

impl Client {
    pub fn new(resolved: Resolved) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .build()
            .into();
        let choices = efforts_of(&resolved.model.reasoning);
        let wanted = std::env::var("STEM_EFFORT").unwrap_or_else(|_| "high".into());
        let effort = choices
            .iter()
            .find(|v| **v == wanted)
            .cloned()
            .or_else(|| choices.iter().find(|v| *v == "medium").cloned())
            .or_else(|| choices.first().cloned());
        let max_output = std::env::var("STEM_MAX_OUTPUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(if resolved.model.output == 0 {
                16_000
            } else {
                resolved.model.output.min(64_000)
            });
        Client {
            resolved,
            agent,
            effort: RefCell::new(effort),
            max_output,
            claude: Default::default(),
            native: Default::default(),
            long_cache: Cell::new(saved_long_cache()),
            plain: Cell::new(false),
        }
    }

    pub fn cache_ttl(&self) -> Option<Duration> {
        let claude = self.resolved.model.id.contains("claude");
        match self.resolved.protocol {
            Protocol::Anthropic | Protocol::ClaudeNative => Some(Duration::from_secs(if self.long_cache.get() { 3600 } else { 300 })),
            Protocol::ClaudeCode => Some(Duration::from_secs(300)),
            Protocol::OpenAiChat if claude => Some(Duration::from_secs(300)),
            Protocol::OpenAiChat | Protocol::OpenAiResponses if self.resolved.provider_id == "openai" || self.resolved.model.id.starts_with("gpt-") => Some(Duration::from_secs(600)),
            _ => None,
        }
    }

    fn cache_marker(&self) -> Value {
        if self.long_cache.get() { json!({"type": "ephemeral", "ttl": "1h"}) } else { json!({"type": "ephemeral"}) }
    }

    pub fn set_cache_ttl(&self, wanted: &str) -> Result<&'static str, String> {
        let long = match wanted.trim().to_lowercase().as_str() {
            "5m" | "5" | "5min" | "300" | "300s" => false,
            "1h" | "60m" | "60" | "3600" | "3600s" => true,
            _ => return Err(format!("cache TTL is one of: {}", CACHE_TTLS.join(", "))),
        };
        if !matches!(self.resolved.protocol, Protocol::Anthropic | Protocol::ClaudeNative) {
            return Err(format!("{} sets its own cache TTL; /cache works for Anthropic models (API key or {})", self.resolved.provider_name, crate::claude_native::PROVIDER));
        }
        self.long_cache.set(long);
        Ok(if long { "1h" } else { "5m" })
    }

    pub fn cache_ttl_choice(&self) -> &'static str {
        if self.long_cache.get() { "1h" } else { "5m" }
    }

    pub fn set_plain(&self, on: bool) {
        self.plain.set(on);
    }

    pub fn efforts(&self) -> Vec<String> {
        efforts_of(&self.resolved.model.reasoning)
    }

    pub fn effort(&self) -> Option<String> {
        self.effort.borrow().clone()
    }

    pub fn set_effort(&self, wanted: &str) -> Result<String, String> {
        let choices = self.efforts();
        if choices.is_empty() {
            return Err(format!("{} has no reasoning effort setting", self.resolved.model.id));
        }
        let found = choices.iter().find(|c| c.eq_ignore_ascii_case(wanted)).cloned().ok_or_else(|| format!("{} takes one of: {}", self.resolved.model.id, choices.join(", ")))?;
        *self.effort.borrow_mut() = Some(found.clone());
        Ok(found)
    }

    pub fn complete(&self, system: &str, msgs: &[Msg], tools: &[Tool], cancel: &Arc<AtomicBool>) -> Result<Reply, String> {
        let (url, body) = match self.resolved.protocol {
            Protocol::ClaudeCode => return self.claude.complete(&self.resolved, self.effort().as_deref(), self.max_output, system, msgs, cancel),
            Protocol::Anthropic => (format!("{}/messages", self.resolved.base_url), self.anthropic_body(system, msgs, tools)),
            Protocol::ClaudeNative => (crate::claude_native::messages_url(), self.anthropic_body(system, msgs, tools)),
            Protocol::OpenAiChat => (format!("{}/chat/completions", self.resolved.base_url), self.openai_body(system, msgs, tools)),
            Protocol::OpenAiResponses => (format!("{}/responses", self.resolved.base_url), self.responses_body(system, msgs, tools)),
            Protocol::Command => return crate::graft::complete(&self.resolved, self.effort().as_deref(), self.max_output, system, msgs, tools, cancel),
        };
        let mut last_error = String::new();
        let mut refreshed = false;
        let mut force_refresh = false;
        for attempt in 0..=RETRIES {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(1000 * 2u64.pow(attempt - 1)));
            }
            let mut request = self.agent.post(&url).header("content-type", "application/json");
            let mut payload = None;
            request = match self.resolved.protocol {
                Protocol::ClaudeNative => {
                    let prepared = self.native.prepare(&body, std::mem::take(&mut force_refresh))?;
                    payload = Some(prepared.payload);
                    prepared.headers.into_iter().fold(request, |r, (k, v)| r.header(k, v))
                }
                Protocol::Anthropic if self.resolved.provider_id == "anthropic" => {
                    request.header("x-api-key", &self.resolved.key).header("anthropic-version", "2023-06-01")
                }
                Protocol::Anthropic => request
                    .header("authorization", &format!("Bearer {}", self.resolved.key))
                    .header("anthropic-version", "2023-06-01"),
                _ if self.resolved.key.is_empty() => request,
                _ => request.header("authorization", &format!("Bearer {}", self.resolved.key)),
            };
            let payload = payload.unwrap_or_else(|| body.to_string().into_bytes());
            let response = match request.send(&payload[..]) {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("network error talking to {}: {e}", self.resolved.provider_name);
                    continue;
                }
            };
            let status = response.status().as_u16();
            if status != 200 {
                let text = response.into_body().read_to_string().unwrap_or_default();
                last_error = format!("{} returned HTTP {status}: {}", self.resolved.provider_name, error_message(&text));
                if self.resolved.protocol == Protocol::ClaudeNative {
                    if status == 401 && !refreshed {
                        refreshed = true;
                        force_refresh = true;
                        continue;
                    }
                    if status == 400 && crate::claude_native::learn_version(&text) {
                        continue;
                    }
                }
                if status == 429 || status == 408 || status == 409 || status >= 500 {
                    continue;
                }
                return Err(last_error);
            }
            let reader = BufReader::new(response.into_body().into_reader());
            let result = match self.resolved.protocol {
                Protocol::Anthropic | Protocol::ClaudeNative => read_anthropic(reader, cancel),
                Protocol::OpenAiResponses => read_responses(reader, cancel),
                _ => read_openai(reader, cancel),
            };
            match result {
                Ok(reply) => return Ok(reply),
                Err(StreamError::Cancelled) => return Err("cancelled".into()),
                Err(StreamError::Retry(e)) => last_error = e,
                Err(StreamError::Fatal(e)) => return Err(e),
            }
        }
        Err(format!("{last_error} (gave up after {} attempts)", RETRIES + 1))
    }

    fn anthropic_body(&self, system: &str, msgs: &[Msg], tools: &[Tool]) -> Value {
        let mut messages: Vec<Value> = msgs.iter().map(anthropic_msg).collect();
        if let Some(last) = messages.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|c| c.last_mut()) {
            last["cache_control"] = self.cache_marker();
        }
        let mut tool_defs: Vec<Value> = tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema}))
            .collect();
        if let Some(last) = tool_defs.last_mut() {
            last["cache_control"] = self.cache_marker();
        }
        let mut body = json!({
            "model": self.resolved.model.id,
            "max_tokens": self.max_output,
            "stream": true,
            "system": [{"type": "text", "text": system, "cache_control": self.cache_marker()}],
            "messages": messages,
        });
        if !tool_defs.is_empty() {
            body["tools"] = Value::Array(tool_defs);
        }
        match if self.plain.get() { &Reasoning::None } else { &self.resolved.model.reasoning } {
            Reasoning::Effort(_) => {
                body["thinking"] = json!({"type": "adaptive"});
                if let Some(effort) = self.effort() {
                    body["output_config"] = json!({"effort": effort});
                }
            }
            Reasoning::Budget { min, max } => {
                let share = match self.effort().as_deref() {
                    Some("off") => 0,
                    Some("low") => 16,
                    Some("medium") => 8,
                    Some("max") => 2,
                    _ => 4,
                };
                if share > 0 {
                    let budget = (self.max_output / share).max(*min).min(max.unwrap_or(u64::MAX));
                    if budget < self.max_output {
                        body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
                    }
                }
            }
            _ => {}
        }
        body
    }

    fn openai_body(&self, system: &str, msgs: &[Msg], tools: &[Tool]) -> Value {
        let mut messages = vec![json!({"role": "system", "content": system})];
        let names: std::collections::HashMap<String, String> = msgs
            .iter()
            .flat_map(|m| m.blocks.iter())
            .filter_map(|b| {
                if let Block::ToolUse { id, name, .. } = b {
                    Some((id.clone(), name.clone()))
                } else {
                    None
                }
            })
            .collect();
        for msg in msgs {
            for mut m in openai_msgs(msg) {
                if m["role"] == "tool"
                    && let Some(name) = m["tool_call_id"].as_str().and_then(|id| names.get(id)) {
                        m["name"] = json!(name);
                    }
                messages.push(m);
            }
        }
        let mut body = json!({
            "model": self.resolved.model.id,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
            "max_tokens": self.max_output,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(
                tools
                    .iter()
                    .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
                    .collect(),
            );
        }
        if let (Reasoning::Effort(_), Some(effort)) = (&self.resolved.model.reasoning, self.effort()) {
            body["reasoning_effort"] = json!(effort);
        }
        if self.resolved.provider_id == "openai" {
            body["prompt_cache_key"] = json!(cache_key(system));
        }
        body
    }

    fn responses_body(&self, system: &str, msgs: &[Msg], tools: &[Tool]) -> Value {
        let input: Vec<Value> = msgs.iter().flat_map(responses_items).collect();
        let mut body = json!({
            "model": self.resolved.model.id,
            "instructions": system,
            "input": input,
            "stream": true,
            "store": false,
            "max_output_tokens": self.max_output,
            "prompt_cache_key": cache_key(system),
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(
                tools
                    .iter()
                    .map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.schema, "strict": false}))
                    .collect(),
            );
        }
        if self.resolved.model.reasoning != Reasoning::None {
            body["include"] = json!(["reasoning.encrypted_content"]);
            if let (Reasoning::Effort(_), Some(effort)) = (&self.resolved.model.reasoning, self.effort()) {
                body["reasoning"] = json!({"effort": effort});
            }
        }
        body
    }
}

fn cache_key(system: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(system, &mut h);
    format!("stem-{:016x}", std::hash::Hasher::finish(&h))
}

fn responses_items(msg: &Msg) -> Vec<Value> {
    let mut out = Vec::new();
    let mut text = Vec::new();
    for b in &msg.blocks {
        match (msg.role, b) {
            (Role::User, Block::ToolResult { id, content, .. }) => out.push(json!({"type": "function_call_output", "call_id": id, "output": content})),
            (Role::User, Block::Text(t)) => text.push(json!({"type": "input_text", "text": t})),
            (Role::Assistant, Block::Reasoning(item)) => out.push(item.clone()),
            (Role::Assistant, Block::Text(t)) if !t.is_empty() => out.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": t}]})),
            (Role::Assistant, Block::ToolUse { id, name, input }) => out.push(json!({"type": "function_call", "call_id": id, "name": name, "arguments": input.to_string()})),
            _ => {}
        }
    }
    if !text.is_empty() {
        out.push(json!({"role": "user", "content": text}));
    }
    out
}

pub fn anthropic_content(content: &Value) -> Vec<Block> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| match b["type"].as_str()? {
            "text" => Some(Block::Text(b["text"].as_str().unwrap_or("").into())),
            "tool_use" => Some(Block::ToolUse {
                id: b["id"].as_str().unwrap_or("").into(),
                name: b["name"].as_str().unwrap_or("").into(),
                input: b["input"].clone(),
            }),
            _ => None,
        })
        .collect()
}

pub fn anthropic_messages(msgs: &[Msg]) -> Vec<Value> {
    msgs.iter().map(anthropic_msg).collect()
}

fn error_message(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or(v["message"].as_str()).map(str::to_string))
        .unwrap_or_else(|| text.chars().take(400).collect())
}

fn anthropic_msg(msg: &Msg) -> Value {
    let content: Vec<Value> = msg
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text(t) => Some(json!({"type": "text", "text": t})),
            Block::Thinking { text, signature } => Some(json!({"type": "thinking", "thinking": text, "signature": signature})),
            Block::RedactedThinking(data) => Some(json!({"type": "redacted_thinking", "data": data})),
            Block::Reasoning(_) => None,
            Block::ToolUse { id, name, input } => Some(json!({"type": "tool_use", "id": id, "name": name, "input": input})),
            Block::ToolResult { id, content, is_error } => Some(json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error})),
        })
        .collect();
    json!({"role": if msg.role == Role::User { "user" } else { "assistant" }, "content": content})
}

fn openai_msgs(msg: &Msg) -> Vec<Value> {
    match msg.role {
        Role::Assistant => {
            let text: String = msg
                .blocks
                .iter()
                .filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None })
                .collect::<Vec<_>>()
                .join("\n");
            let calls: Vec<Value> = msg
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => {
                        Some(json!({"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}))
                    }
                    _ => None,
                })
                .collect();
            let mut m = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
            if !calls.is_empty() {
                m["tool_calls"] = Value::Array(calls);
            }
            vec![m]
        }
        Role::User => {
            let mut out = Vec::new();
            let mut text = Vec::new();
            for b in &msg.blocks {
                match b {
                    Block::ToolResult { id, content, .. } => out.push(json!({"role": "tool", "tool_call_id": id, "content": content})),
                    Block::Text(t) => text.push(t.clone()),
                    _ => {}
                }
            }
            if !text.is_empty() {
                out.push(json!({"role": "user", "content": text.join("\n")}));
            }
            out
        }
    }
}

enum StreamError {
    Cancelled,
    Retry(String),
    Fatal(String),
}

fn sse_events<R: BufRead>(reader: R, cancel: &Arc<AtomicBool>, mut on: impl FnMut(&str, Value) -> Result<bool, StreamError>) -> Result<(), StreamError> {
    let mut event = String::new();
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            return Err(StreamError::Cancelled);
        }
        let line = line.map_err(|e| StreamError::Retry(format!("stream broke: {e}")))?;
        if let Some(name) = line.strip_prefix("event:") {
            event = name.trim().to_string();
        } else if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(());
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
            if !on(&event, value)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn read_anthropic<R: BufRead>(reader: R, cancel: &Arc<AtomicBool>) -> Result<Reply, StreamError> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut partial_json: Vec<String> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = None;
    sse_events(reader, cancel, |_, v| {
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                usage.input = u["input_tokens"].as_u64().unwrap_or(0);
                usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                usage.output = u["output_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let b = &v["content_block"];
                blocks.push(match b["type"].as_str().unwrap_or("") {
                    "tool_use" => Block::ToolUse {
                        id: b["id"].as_str().unwrap_or("").into(),
                        name: b["name"].as_str().unwrap_or("").into(),
                        input: Value::Null,
                    },
                    "thinking" => Block::Thinking {
                        text: String::new(),
                        signature: String::new(),
                    },
                    "redacted_thinking" => Block::RedactedThinking(b["data"].as_str().unwrap_or("").into()),
                    _ => Block::Text(String::new()),
                });
                partial_json.push(String::new());
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let (Some(block), Some(json_buf)) = (blocks.last_mut(), partial_json.last_mut()) else {
                    return Ok(true);
                };
                match (d["type"].as_str().unwrap_or(""), block) {
                    ("text_delta", Block::Text(t)) => t.push_str(d["text"].as_str().unwrap_or("")),
                    ("thinking_delta", Block::Thinking { text, .. }) => text.push_str(d["thinking"].as_str().unwrap_or("")),
                    ("signature_delta", Block::Thinking { signature, .. }) => signature.push_str(d["signature"].as_str().unwrap_or("")),
                    ("input_json_delta", Block::ToolUse { .. }) => json_buf.push_str(d["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(Block::ToolUse { input, .. }), Some(buf)) = (blocks.last_mut(), partial_json.last()) {
                    *input = if buf.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(buf).unwrap_or_else(|_| json!({"__unparsed": buf}))
                    };
                }
            }
            "message_delta" => {
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    stop = Some(match reason {
                        "end_turn" | "stop_sequence" => Stop::End,
                        "tool_use" => Stop::ToolUse,
                        "max_tokens" => Stop::MaxTokens,
                        other => Stop::Other(other.into()),
                    });
                }
                if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                    usage.output = o;
                }
            }
            "message_stop" => return Ok(false),
            "error" => {
                let kind = v["error"]["type"].as_str().unwrap_or("");
                let message = v["error"]["message"].as_str().unwrap_or("stream error").to_string();
                return Err(if kind == "overloaded_error" || kind == "api_error" {
                    StreamError::Retry(message)
                } else {
                    StreamError::Fatal(message)
                });
            }
            _ => {}
        }
        Ok(true)
    })?;
    let stop = stop.ok_or_else(|| StreamError::Retry("the stream ended before the model finished".into()))?;
    Ok(Reply { blocks, stop, usage })
}

fn read_openai<R: BufRead>(reader: R, cancel: &Arc<AtomicBool>) -> Result<Reply, StreamError> {
    let mut text = String::new();
    let mut calls: Vec<(String, String, String)> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = None;
    sse_events(reader, cancel, |_, v| {
        if let Some(err) = v.get("error") {
            return Err(StreamError::Fatal(err["message"].as_str().unwrap_or("stream error").into()));
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let cached = u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
            usage.input = u["prompt_tokens"].as_u64().unwrap_or(0).saturating_sub(cached);
            usage.cache_read = cached;
            usage.output = u["completion_tokens"].as_u64().unwrap_or(0);
        }
        let Some(choice) = v["choices"].get(0) else { return Ok(true) };
        let delta = &choice["delta"];
        if let Some(t) = delta["content"].as_str() {
            text.push_str(t);
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = match call["index"].as_u64() {
                Some(i) => i as usize,
                None if call["id"].as_str().is_some_and(|id| !calls.iter().any(|c| c.0 == id)) => calls.len(),
                None => calls.len().saturating_sub(1),
            };
            while calls.len() <= index {
                calls.push(Default::default());
            }
            let slot = &mut calls[index];
            if let Some(id) = call["id"].as_str() {
                slot.0 = id.into();
            }
            if let Some(name) = call["function"]["name"].as_str() {
                slot.1.push_str(name);
            }
            if let Some(args) = call["function"]["arguments"].as_str() {
                slot.2.push_str(args);
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            stop = Some(match reason {
                "stop" => Stop::End,
                "tool_calls" | "function_call" => Stop::ToolUse,
                "length" => Stop::MaxTokens,
                other => Stop::Other(other.into()),
            });
        }
        Ok(true)
    })?;
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(Block::Text(text));
    }
    for (i, (id, name, args)) in calls.into_iter().enumerate().filter(|(_, c)| !c.1.is_empty() || !c.2.is_empty()) {
        let input = if args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&args).unwrap_or_else(|_| json!({"__unparsed": args}))
        };
        blocks.push(Block::ToolUse {
            id: if id.is_empty() { format!("call_{i}") } else { id },
            name,
            input,
        });
    }
    let stop = match stop {
        Some(Stop::End) if blocks.iter().any(|b| matches!(b, Block::ToolUse { .. })) => Stop::ToolUse,
        Some(s) => s,
        None => return Err(StreamError::Retry("the stream ended before the model finished".into())),
    };
    Ok(Reply { blocks, stop, usage })
}

fn read_responses<R: BufRead>(reader: R, cancel: &Arc<AtomicBool>) -> Result<Reply, StreamError> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = None;
    sse_events(reader, cancel, |_, v| {
        match v["type"].as_str().unwrap_or("") {
            "response.output_item.done" => {
                let item = &v["item"];
                match item["type"].as_str().unwrap_or("") {
                    "message" => {
                        let text: String = item["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect();
                        if !text.is_empty() {
                            blocks.push(Block::Text(text));
                        }
                    }
                    "function_call" => {
                        let args = item["arguments"].as_str().unwrap_or("");
                        blocks.push(Block::ToolUse {
                            id: item["call_id"].as_str().unwrap_or("").into(),
                            name: item["name"].as_str().unwrap_or("").into(),
                            input: if args.trim().is_empty() { json!({}) } else { serde_json::from_str(args).unwrap_or_else(|_| json!({"__unparsed": args})) },
                        });
                    }
                    "reasoning" if item["encrypted_content"].is_string() => {
                        let mut kept = item.clone();
                        if let Some(o) = kept.as_object_mut() {
                            o.remove("status");
                        }
                        blocks.push(Block::Reasoning(kept));
                    }
                    _ => {}
                }
            }
            kind @ ("response.completed" | "response.incomplete") => {
                let r = &v["response"];
                let u = &r["usage"];
                let cached = u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
                usage.input = u["input_tokens"].as_u64().unwrap_or(0).saturating_sub(cached);
                usage.cache_read = cached;
                usage.output = u["output_tokens"].as_u64().unwrap_or(0);
                stop = Some(match (kind, r["incomplete_details"]["reason"].as_str()) {
                    ("response.incomplete", Some("max_output_tokens")) => Stop::MaxTokens,
                    ("response.incomplete", reason) => Stop::Other(reason.unwrap_or("incomplete").into()),
                    _ if blocks.iter().any(|b| matches!(b, Block::ToolUse { .. })) => Stop::ToolUse,
                    _ => Stop::End,
                });
                return Ok(false);
            }
            "response.failed" | "error" => {
                let e = if v["response"]["error"].is_object() { &v["response"]["error"] } else if v["error"].is_object() { &v["error"] } else { &v };
                let message = e["message"].as_str().unwrap_or("stream error").to_string();
                let code = e["code"].as_str().unwrap_or("");
                return Err(if code.contains("server") || code.contains("rate") || code.contains("overload") { StreamError::Retry(message) } else { StreamError::Fatal(message) });
            }
            _ => {}
        }
        Ok(true)
    })?;
    let stop = stop.ok_or_else(|| StreamError::Retry("the stream ended before the model finished".into()))?;
    Ok(Reply { blocks, stop, usage })
}
