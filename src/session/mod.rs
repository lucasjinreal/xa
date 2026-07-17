//! Minimal session persistence for `xa`, inspired by pi_agent_rust's session
//! model (one file per session + metadata for fast listing) but kept simple:
//! each session is a single JSON file under `config_dir()/xa/sessions`.

use std::fs;
use std::collections::BTreeMap;
use std::path::PathBuf;

use dirs::config_dir;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredMessage {
    pub role: String, // "user" | "assistant" | "system" | "tool"
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<StoredToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub provider: String,
    pub model: String,
    pub created: i64,
    pub updated: i64,
    pub messages: Vec<StoredMessage>,
    /// Per-tool output reductions. Kept with the session for an auditable
    /// account of what was removed before sending tool output to the LLM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_filter_calls: Vec<crate::output_filter::ToolOutputStats>,
    #[serde(default, skip_serializing_if = "ApiTokenUsage::is_empty")]
    pub api_token_usage: ApiTokenUsage,
}

/// Lightweight metadata used by session lists and the resume picker. Reading
/// this avoids loading every saved conversation into memory just to display it.
#[derive(Deserialize, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub updated: i64,
}

/// Session data used by `xa gain`; message bodies are intentionally omitted.
#[derive(Deserialize)]
pub struct GainSessionRecord {
    pub updated: i64,
    #[serde(default)]
    pub output_filter_calls: Vec<crate::output_filter::ToolOutputStats>,
    #[serde(default)]
    pub api_token_usage: ApiTokenUsage,
}

fn sessions_dir() -> PathBuf {
    config_dir()
        .map(|d| d.join("xa").join("sessions"))
        .unwrap_or_else(|| PathBuf::from(".xa/sessions"))
}

fn path_for(id: &str) -> PathBuf {
    sessions_dir().join(format!("{id}.json"))
}

/// Generate a short, collision-resistant session id.
pub fn new_id() -> String {
    let millis = chrono::Utc::now().timestamp_millis();
    let pid = std::process::id();
    format!("{millis:x}-{pid:x}")
}

/// List session metadata newest first, without deserializing message bodies.
pub fn list_summaries() -> Vec<SessionSummary> {
    let dir = sessions_dir();
    let mut out: Vec<SessionSummary> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(file) = fs::File::open(path) {
                    if let Ok(summary) = serde_json::from_reader(file) {
                        out.push(summary);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
}

/// Read only aggregate fields for historical reporting, never session messages.
pub fn gain_records() -> Vec<GainSessionRecord> {
    let mut records = Vec::new();
    if let Ok(entries) = fs::read_dir(sessions_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(file) = fs::File::open(path) {
                    if let Ok(record) = serde_json::from_reader(file) {
                        records.push(record);
                    }
                }
            }
        }
    }
    records
}

/// Human-readable elapsed time for session metadata, e.g. `9m ago`.
pub fn relative_time(timestamp_ms: i64) -> String {
    let elapsed = (chrono::Utc::now().timestamp_millis() - timestamp_ms).max(0) / 1000;
    match elapsed {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        86_400..=604_799 => format!("{}d ago", elapsed / 86_400),
        _ => format!("{}w ago", elapsed / 604_800),
    }
}

/// Persist a session (creating the directory if needed).
pub fn save(session: &Session) -> std::io::Result<()> {
    let dir = sessions_dir();
    fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(session)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(path_for(&session.id), json)
}

/// Load a session by id.
pub fn load(id: &str) -> Option<Session> {
    fs::read_to_string(path_for(id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(|mut session: Session| {
            session.remove_legacy_resume_duplicates();
            session
        })
}

/// Permanently remove a saved session by id.
pub fn delete(id: &str) -> std::io::Result<()> {
    fs::remove_file(path_for(id))
}

impl Session {
    /// A session becomes persistent only once the user has actually spoken.
    pub fn has_user_message(&self) -> bool {
        self.messages.iter().any(|message| message.role == "user")
    }

    pub fn record_output_filter_call(&mut self, stats: crate::output_filter::ToolOutputStats) {
        self.output_filter_calls.push(stats);
    }

    pub fn record_api_token_usage(&mut self, prompt: u32, completion: u32, total: u32) {
        self.api_token_usage.requests += 1;
        self.api_token_usage.prompt_tokens += u64::from(prompt);
        self.api_token_usage.completion_tokens += u64::from(completion);
        self.api_token_usage.total_tokens += u64::from(total);
    }

    pub fn output_savings(&self) -> OutputSavings {
        let mut totals = OutputSavings::default();
        for call in &self.output_filter_calls {
            totals.calls += 1;
            totals.raw_bytes += call.raw_bytes;
            totals.returned_bytes += call.returned_bytes;
            totals.estimated_tokens_saved += call.estimated_tokens_saved;
        }
        totals
    }

    /// Group persisted call statistics for the exit report. Sorting by bytes
    /// saved makes the highest-impact filters immediately visible.
    pub fn output_savings_by_filter(&self) -> Vec<OutputFilterSavings> {
        let mut grouped: BTreeMap<String, OutputFilterSavings> = BTreeMap::new();
        for call in &self.output_filter_calls {
            let label = format!("{}/{}", call.tool, call.filter);
            let entry = grouped.entry(label.clone()).or_insert_with(|| OutputFilterSavings {
                label,
                ..Default::default()
            });
            entry.calls += 1;
            entry.raw_bytes += call.raw_bytes;
            entry.returned_bytes += call.returned_bytes;
            entry.estimated_tokens_saved += call.estimated_tokens_saved;
        }
        let mut rows: Vec<_> = grouped.into_values().collect();
        rows.sort_by(|a, b| {
            b.bytes_saved()
                .cmp(&a.bytes_saved())
                .then_with(|| a.label.cmp(&b.label))
        });
        rows
    }
    /// Remove duplicate assistant entries written by xa versions that saved a
    /// tool-call response both with its tool calls and as a second plain reply.
    /// Those duplicates accumulated on every resume and could make a session
    /// consume unbounded memory when restored.
    fn remove_legacy_resume_duplicates(&mut self) {
        let mut cleaned = Vec::with_capacity(self.messages.len());
        let mut i = 0;
        while i < self.messages.len() {
            let message = &self.messages[i];
            cleaned.push(message.clone());
            i += 1;

            if message.role != "assistant" || message.tool_calls.is_none() {
                continue;
            }

            while i < self.messages.len() && self.messages[i].role == "tool" {
                cleaned.push(self.messages[i].clone());
                i += 1;
            }
            while i < self.messages.len()
                && self.messages[i].role == "assistant"
                && self.messages[i].tool_calls.is_none()
                && self.messages[i].content == message.content
            {
                i += 1;
            }
        }
        self.messages = cleaned;
    }

    /// Create a fresh, empty session bound to a provider/model.
    pub fn new(provider: &str, model: &str) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Session {
            id: new_id(),
            title: "untitled".to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            created: now,
            updated: now,
            messages: Vec::new(),
            output_filter_calls: Vec::new(),
            api_token_usage: ApiTokenUsage::default(),
        }
    }

    /// Touch the `updated` timestamp.
    pub fn touch(&mut self) {
        self.updated = chrono::Utc::now().timestamp_millis();
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct ApiTokenUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl ApiTokenUsage {
    fn is_empty(&self) -> bool {
        self.requests == 0 && self.total_tokens == 0
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OutputSavings {
    pub calls: usize,
    pub raw_bytes: usize,
    pub returned_bytes: usize,
    pub estimated_tokens_saved: usize,
}

impl OutputSavings {
    pub fn bytes_saved(self) -> usize {
        self.raw_bytes.saturating_sub(self.returned_bytes)
    }

    pub fn savings_percent(self) -> f64 {
        if self.raw_bytes == 0 {
            0.0
        } else {
            self.bytes_saved() as f64 * 100.0 / self.raw_bytes as f64
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OutputFilterSavings {
    pub label: String,
    pub calls: usize,
    pub raw_bytes: usize,
    pub returned_bytes: usize,
    pub estimated_tokens_saved: usize,
}

impl OutputFilterSavings {
    pub fn bytes_saved(&self) -> usize {
        self.raw_bytes.saturating_sub(self.returned_bytes)
    }

    pub fn savings_percent(&self) -> f64 {
        if self.raw_bytes == 0 {
            0.0
        } else {
            self.bytes_saved() as f64 * 100.0 / self.raw_bytes as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> StoredMessage {
        StoredMessage {
            role: role.into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn removes_all_legacy_tool_turn_duplicates() {
        let mut session = Session::new("test", "test");
        let mut call = message("assistant", "done");
        call.tool_calls = Some(vec![StoredToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }]);
        session.messages = vec![
            call,
            message("tool", "file contents"),
            message("assistant", "done"),
            message("assistant", "done"),
            message("user", "next"),
        ];

        session.remove_legacy_resume_duplicates();

        assert_eq!(session.messages.len(), 3);
        assert_eq!(session.messages[0].role, "assistant");
        assert_eq!(session.messages[1].role, "tool");
        assert_eq!(session.messages[2].role, "user");
    }

    #[test]
    fn empty_sessions_are_not_conversations() {
        let mut session = Session::new("test", "test");
        assert!(!session.has_user_message());
        session.messages.push(message("assistant", "welcome"));
        assert!(!session.has_user_message());
        session.messages.push(message("user", "hello"));
        assert!(session.has_user_message());
    }

    #[test]
    fn aggregates_persisted_output_savings() {
        let mut session = Session::new("test", "test");
        session.record_output_filter_call(crate::output_filter::ToolOutputStats {
            raw_bytes: 400,
            returned_bytes: 80,
            estimated_tokens_saved: 80,
            ..Default::default()
        });
        let totals = session.output_savings();
        assert_eq!(totals.calls, 1);
        assert_eq!(totals.bytes_saved(), 320);
        assert_eq!(totals.estimated_tokens_saved, 80);
    }

    #[test]
    fn groups_output_savings_by_tool_and_filter() {
        let mut session = Session::new("test", "test");
        for (filter, raw_bytes, returned_bytes) in [("cargo", 800, 200), ("cargo", 400, 100), ("default", 300, 250)] {
            session.record_output_filter_call(crate::output_filter::ToolOutputStats {
                tool: "bash".into(),
                filter: filter.into(),
                raw_bytes,
                returned_bytes,
                ..Default::default()
            });
        }
        let rows = session.output_savings_by_filter();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "bash/cargo");
        assert_eq!(rows[0].calls, 2);
        assert_eq!(rows[0].bytes_saved(), 900);
        assert!((rows[0].savings_percent() - 75.0).abs() < f64::EPSILON);
    }
}
