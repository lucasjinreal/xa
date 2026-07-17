//! Minimal session persistence for `xa`, inspired by pi_agent_rust's session
//! model (one file per session + metadata for fast listing) but kept simple:
//! each session is a single JSON file under `config_dir()/xa/sessions`.

use std::fs;
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

/// List all sessions, newest updated first.
pub fn list() -> Vec<Session> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                if let Ok(s) = fs::read_to_string(&p) {
                    if let Ok(sess) = serde_json::from_str::<Session>(&s) {
                        out.push(sess);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
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

/// Remove a session by id.
pub fn remove(id: &str) -> std::io::Result<()> {
    let p = path_for(id);
    if p.exists() {
        fs::remove_file(p)?;
    }
    Ok(())
}

impl Session {
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
        }
    }

    /// Touch the `updated` timestamp.
    pub fn touch(&mut self) {
        self.updated = chrono::Utc::now().timestamp_millis();
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
}
