//! Provider abstraction for `xa`.
//!
//! Every provider is just an OpenAI-compatible chat-completions endpoint with a
//! custom base URL, API key and model. This lets you point `xa` at anything
//! (OpenAI, OpenRouter, vLLM, llama.cpp, Ollama's `/v1`, a local gateway, ...)
//! without any built-in/hardcoded provider URLs.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dirs::config_dir;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

/// A single configurable chat provider.
#[derive(Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    /// Base URL of the chat-completions API, e.g. `https://api.openai.com/v1`.
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    /// Reserved for future provider kinds (`openai`, `anthropic`, ...).
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "openai".to_string()
}

impl Default for Provider {
    fn default() -> Self {
        Provider {
            name: "default".into(),
            endpoint: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: "gpt-4o-mini".into(),
            kind: "openai".into(),
        }
    }
}

impl Provider {
    /// Full `chat/completions` URL for this provider.
    pub fn chat_url(&self) -> String {
        let e = self.endpoint.trim_end_matches('/');
        format!("{e}/chat/completions")
    }
}

/// On-disk collection of providers, kept in `config_dir()/xa/providers.toml`.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub providers: HashMap<String, Provider>,
}

impl ProvidersConfig {
    pub fn path() -> Option<std::path::PathBuf> {
        config_dir().map(|d| d.join("xa").join("providers.toml"))
    }

    pub fn load() -> ProvidersConfig {
        if let Some(p) = Self::path() {
            if p.exists() {
                if let Ok(s) = fs::read_to_string(&p) {
                    if let Ok(c) = toml::from_str(&s) {
                        return c;
                    }
                }
            }
        }
        ProvidersConfig::default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = Self::path()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config dir"))?;
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let s = toml::to_string(self).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(&p, s)
    }

    pub fn active_provider(&self) -> Option<Provider> {
        self.providers.get(&self.active).cloned()
    }

    pub fn list(&self) -> Vec<&Provider> {
        self.providers.values().collect()
    }

    /// Insert or replace a provider and make it the active one.
    pub fn upsert(&mut self, p: Provider) {
        self.active = p.name.clone();
        self.providers.insert(p.name.clone(), p);
    }
}

/// A single tool invocation produced by the model.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolCallRepr {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments string (as emitted by OpenAI-compatible APIs).
    pub arguments: String,
}

/// A chat message. `tool_calls` is set on assistant messages that invoke
/// tools; `tool_call_id` is set on tool-result (role `"tool"`) messages.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRepr>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Events emitted while a completion streams (or tools execute).
#[derive(Clone, Debug)]
pub enum StreamEvent {
    Delta(String),
    /// A tool call is about to be executed (for rendering in the TUI).
    ToolCall { name: String, arguments: String },
    /// A tool call finished (for rendering in the TUI).
    ToolResult {
        name: String,
        output: String,
        is_error: bool,
        /// A colorful-free unified `git diff` when the tool edited/added a file.
        diff: Option<String>,
    },
    Done,
    Error(String),
    /// Internal: marks the assistant cell at `0` as no longer streaming.
    InternalAssistIdx(u32),
}

/// Resolve the provider to use. If a provider has been configured (via
/// `xa login` or `/login`) we always honor it — even when its key is empty
/// (e.g. a local gateway with no auth). Only when *nothing* is configured
/// do we fall back to the legacy `config.toml` single-provider setup.
pub async fn load_active_provider() -> Provider {
    let pc = ProvidersConfig::load();
    if !pc.providers.is_empty() {
        if let Some(p) = pc.active_provider() {
            return p;
        }
    }
    if let Ok(c) = crate::config::load_config().await {
        if !c.api_key.is_empty() {
            return Provider {
                name: "default".into(),
                endpoint: c.base_url,
                api_key: c.api_key,
                model: c.default_model.unwrap_or_default(),
                kind: "openai".into(),
            };
        }
    }
    Provider::default()
}

/// A built-in provider preset. Built-ins only need an API key; the base URL
/// is fixed. Pick `custom` to supply your own endpoint.
#[derive(Clone)]
pub struct ProviderPreset {
    pub name: &'static str,
    pub base_url: &'static str,
    pub note: Option<&'static str>,
}

/// Curated built-in provider presets (all OpenAI-compatible chat endpoints).
pub fn builtin_presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            name: "openai",
            base_url: "https://api.openai.com/v1",
            note: None,
        },
        ProviderPreset {
            name: "anthropic",
            base_url: "https://api.anthropic.com/v1",
            note: Some("needs an OpenAI-compatible gateway for chat"),
        },
        ProviderPreset {
            name: "alibaba",
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            note: None,
        },
        ProviderPreset {
            name: "openrouter",
            base_url: "https://openrouter.ai/api/v1",
            note: None,
        },
        ProviderPreset {
            name: "deepseek",
            base_url: "https://api.deepseek.com/v1",
            note: None,
        },
        ProviderPreset {
            name: "groq",
            base_url: "https://api.groq.com/openai/v1",
            note: None,
        },
        ProviderPreset {
            name: "ollama",
            base_url: "http://localhost:11434/v1",
            note: Some("local, no key needed"),
        },
    ]
}

/// Fetch the model list from an OpenAI-compatible `/models` endpoint.
/// Returns an error string (not a panic) when the base URL or key is invalid.
pub async fn fetch_models(endpoint: &str, api_key: &str) -> Result<Vec<String>, String> {
    let base = endpoint.trim_end_matches('/');
    let models_url = format!("{base}/models");
    let client = reqwest::Client::new();
    let mut req = client.get(&models_url);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let res = req.send().await.map_err(|e| format!("request failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!(
            "HTTP {} (base URL or API key may be invalid)",
            res.status()
        ));
    }
    let body: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;
    let models: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(models)
}

/// Drive an interactive provider setup. `ask` is a blocking prompt that prints
/// `question` and returns the trimmed answer. Used by both `xa login` (stdin)
/// and the in-TUI `/login` (paused terminal read).
pub async fn interactive_configure(mut ask: impl FnMut(&str) -> String) -> Provider {
    let presets = builtin_presets();
    println!("Select a provider:");
    for (i, p) in presets.iter().enumerate() {
        let note = p.note.map(|n| format!("  ({n})")).unwrap_or_default();
        println!("  {}. {}  {}{}", i + 1, p.name, p.base_url, note);
    }
    println!("  {}. custom", presets.len() + 1);

    let choice = ask("provider # (or name): ").trim().to_string();
    let (name, endpoint) = if let Ok(n) = choice.parse::<usize>() {
        if n >= 1 && n <= presets.len() {
            (presets[n - 1].name.to_string(), presets[n - 1].base_url.to_string())
        } else if n == presets.len() + 1 {
            let name = ask("custom provider name: ").trim().to_string();
            let url = ask("base URL (e.g. https://api.openai.com/v1): ").trim().to_string();
            (name, url)
        } else {
            ("default".to_string(), "https://api.openai.com/v1".to_string())
        }
    } else if let Some(p) = presets.iter().find(|p| p.name == choice.to_lowercase()) {
        (p.name.to_string(), p.base_url.to_string())
    } else {
        let url = ask("base URL (e.g. https://api.openai.com/v1): ").trim().to_string();
        (choice, url)
    };

    let api_key = ask("api key (optional, Enter to skip): ").trim().to_string();

    let model = match fetch_models(&endpoint, &api_key).await {
        Ok(models) if !models.is_empty() => {
            println!("Available models:");
            for (i, m) in models.iter().enumerate() {
                println!("  {}. {}", i + 1, m);
            }
            println!("  {}. enter a custom model name", models.len() + 1);
            let sel = ask("select model #: ").trim().to_string();
            if let Ok(n) = sel.parse::<usize>() {
                if n >= 1 && n <= models.len() {
                    models[n - 1].clone()
                } else if n == models.len() + 1 {
                    ask("model name: ").trim().to_string()
                } else {
                    models[0].clone()
                }
            } else {
                models[0].clone()
            }
        }
        Ok(_) => {
            eprintln!("No models returned.");
            ask("model name (manual): ").trim().to_string()
        }
        Err(e) => {
            eprintln!("Could not retrieve models: {e}");
            eprintln!("The base URL or API key may be invalid.");
            ask("model name (manual): ").trim().to_string()
        }
    };

    Provider {
        name,
        endpoint,
        api_key,
        model,
        kind: "openai".into(),
    }
}

/// Serialize our `ChatMessage` history into OpenAI-compatible `messages`,
/// preserving tool calls (`tool_calls`) and tool results (`tool_call_id`).
fn messages_to_json(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| {
            let mut o = serde_json::Map::new();
            match m.role.as_str() {
                "tool" => {
                    o.insert("role".into(), "tool".into());
                    if let Some(id) = &m.tool_call_id {
                        o.insert("tool_call_id".into(), id.clone().into());
                    }
                    o.insert("content".into(), m.content.clone().into());
                }
                "assistant" => {
                    o.insert("role".into(), "assistant".into());
                    o.insert("content".into(), m.content.clone().into());
                    if let Some(tcs) = &m.tool_calls {
                        let arr = tcs
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": { "name": tc.name, "arguments": tc.arguments }
                                })
                            })
                            .collect::<Vec<_>>();
                        o.insert("tool_calls".into(), serde_json::Value::Array(arr));
                    }
                }
                _ => {
                    o.insert("role".into(), m.role.clone().into());
                    o.insert("content".into(), m.content.clone().into());
                }
            }
            serde_json::Value::Object(o)
        })
        .collect()
}

/// Run a full agentic turn loop against `provider`.
///
/// Streams text deltas; when the model emits tool calls, executes them
/// locally (`tools`), feeds the results back, and loops until a turn
/// yields no tool calls (mirroring pi_agent_rust's agent loop).
pub async fn run_conversation(
    provider: &Provider,
    history: std::sync::Arc<std::sync::Mutex<Vec<ChatMessage>>>,
    tx: mpsc::Sender<StreamEvent>,
    tools: &[std::sync::Arc<dyn crate::tools::Tool>],
    cancel: Arc<AtomicBool>,
) {
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = tx.send(StreamEvent::Done).await;
            return;
        }
        // Clone the current history so no mutex guard is held across an await.
        let mut snapshot = history.lock().unwrap().clone();
        // Inject a minimal system prompt every turn so the model knows its
        // identity, environment and current time — even on a resumed session
        // or after `/new`. Tools are already declared via the API `tools`
        // array, so we keep this tiny. Prepended to the *snapshot* only, so it
        // never accumulates duplicates inside `history`.
        let cwd = env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let system_content = format!(
            "You are xa, a coding agent. Current time: {now}. Working directory: {cwd}."
        );
        if snapshot.first().map(|m| m.role.as_str()) != Some("system") {
            snapshot.insert(
                0,
                ChatMessage {
                    role: "system".into(),
                    content: system_content,
                    ..Default::default()
                },
            );
        }
        match stream_completion(provider, &snapshot, tools, &tx, cancel.clone()).await {
            Ok((text, calls)) => {
                if cancel.load(Ordering::SeqCst) {
                    let _ = tx.send(StreamEvent::Done).await;
                    return;
                }
                if calls.is_empty() {
                    // Final answer (text already streamed) — signal completion.
                    let _ = tx.send(StreamEvent::Done).await;
                    return;
                }
                // Record the assistant message (with its tool calls) in history.
                history.lock().unwrap().push(ChatMessage {
                    role: "assistant".into(),
                    content: text,
                    tool_calls: Some(calls.clone()),
                    ..Default::default()
                });
                // Execute each tool call and append the result message.
                for tc in &calls {
                    if cancel.load(Ordering::SeqCst) {
                        let _ = tx.send(StreamEvent::Done).await;
                        return;
                    }
                    let _ = tx
                        .send(StreamEvent::ToolCall {
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        })
                        .await;
                    let args: serde_json::Value =
                        serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null);
                    let path = args
                        .get("path")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let is_file_mut = tc.name == "edit" || tc.name == "write";
                    // Snapshot the file *before* the tool runs so we can show a
                    // minimal diff of exactly what changed (works whether or not
                    // the file is tracked by git).
                    let before = if is_file_mut {
                        path.as_deref().and_then(|p| std::fs::read_to_string(p).ok())
                    } else {
                        None
                    };
                    let (mut output, is_error) = match crate::tools::call_tool(&tc.name, args, tools).await {
                        Ok(o) => (o, false),
                        Err(e) => (e, true),
                    };
                    // Cap what we feed back to the model (and the TUI) so a
                    // chatty tool (grep/read/bash) can't blow the context window.
                    output = cap_tool_output(output);
                    // For file-mutating tools, capture a minimal diff so the user
                    // sees exactly what changed — not the whole file.
                    let diff = if is_file_mut && !is_error {
                        path.as_deref().and_then(|p| {
                            std::fs::read_to_string(p).ok().and_then(|after| {
                                crate::tools::unified_diff(p, before.as_deref().unwrap_or(""), &after)
                            })
                        })
                    } else {
                        None
                    };
                    let _ = tx
                        .send(StreamEvent::ToolResult {
                            name: tc.name.clone(),
                            output: output.clone(),
                            is_error,
                            diff,
                        })
                        .await;
                    history.lock().unwrap().push(ChatMessage {
                        role: "tool".into(),
                        content: output,
                        tool_call_id: Some(tc.id.clone()),
                        ..Default::default()
                    });
                }
                // Loop: let the model continue with the tool results.
            }
            Err(e) => {
                let _ = tx.send(StreamEvent::Error(e)).await;
                return;
            }
        }
    }
}

/// Stream one completion from `provider`. Forwards text deltas to `tx` and
/// returns the accumulated `(text, tool_calls)` for the turn. Uses raw SSE
/// over reqwest so any custom OpenAI-compatible endpoint works.
async fn stream_completion(
    provider: &Provider,
    messages: &[ChatMessage],
    tools: &[std::sync::Arc<dyn crate::tools::Tool>],
    tx: &mpsc::Sender<StreamEvent>,
    cancel: Arc<AtomicBool>,
) -> Result<(String, Vec<ToolCallRepr>), String> {
    let client = reqwest::Client::new();

    let mut body = serde_json::json!({
        "model": provider.model,
        "stream": true,
        "messages": messages_to_json(messages),
    });
    if !tools.is_empty() {
        let tools_json = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect::<Vec<_>>();
        body["tools"] = serde_json::Value::Array(tools_json);
        body["tool_choice"] = "auto".into();
    }

    let res = client
        .post(provider.chat_url())
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !res.status().is_success() {
        let status = res.status();
        let txt = res.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {txt}"));
    }

    let mut stream = res.bytes_stream();
    let mut buf = String::new();
    let mut text = String::new();
    // Tool calls accumulate per `index` (OpenAI streams them by index).
    let mut calls: Vec<ToolCallRepr> = Vec::new();

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::SeqCst) {
            return Ok((text, prune_calls(calls)));
        }
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(nl) = buf.find('\n') {
            let mut line: String = buf.drain(..=nl).collect();
            line.truncate(line.trim_end().len());
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line == "data: [DONE]" {
                return Ok((text, prune_calls(calls)));
            }
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.trim();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) {
                    if let Some(err) = v.get("error") {
                        return Err(format!("api error: {err}"));
                    }
                    if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                        if let Some(c) = choices.first() {
                            // Text delta.
                            if let Some(d) = c.get("delta").and_then(|d| d.get("content")).and_then(|x| x.as_str()) {
                                if !d.is_empty() {
                                    text.push_str(d);
                                    if tx.send(StreamEvent::Delta(d.to_string())).await.is_err() {
                                        return Ok((text, prune_calls(calls)));
                                    }
                                }
                            }
                            // Tool-call deltas (may arrive incrementally).
                            if let Some(tcs) = c.get("delta").and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array()) {
                                for tc in tcs {
                                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                                    if calls.len() <= idx {
                                        calls.resize(idx + 1, ToolCallRepr::default());
                                    }
                                    if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                                        if calls[idx].id.is_empty() {
                                            calls[idx].id = id.to_string();
                                        }
                                    }
                                    if let Some(f) = tc.get("function") {
                                        if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                                            if calls[idx].name.is_empty() {
                                                calls[idx].name = n.to_string();
                                            }
                                        }
                                        if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                                            calls[idx].arguments.push_str(a);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok((text, prune_calls(calls)))
}

/// Drop incomplete tool calls (those without a name).
fn prune_calls(mut calls: Vec<ToolCallRepr>) -> Vec<ToolCallRepr> {
    calls.retain(|c| !c.name.is_empty());
    calls
}

/// Hard cap on tool output returned to the model. Keeps grep/read/bash from
/// flooding the conversation history with thousands of lines. The TUI also
/// receives the capped text, so it never has to render a giant blob.
const MAX_TOOL_OUTPUT_CHARS: usize = 4000;

fn cap_tool_output(out: String) -> String {
    if out.len() <= MAX_TOOL_OUTPUT_CHARS {
        return out;
    }
    let lines = out.lines().count();
    let mut end = MAX_TOOL_OUTPUT_CHARS;
    while end > 0 && !out.is_char_boundary(end) {
        end -= 1;
    }
    let cut = out[..end].lines().count();
    let truncated = out[..end].to_string();
    format!(
        "{truncated}\n…(output truncated: {cut}/{lines} lines, {} of {} chars shown)",
        truncated.len(),
        out.len()
    )
}
