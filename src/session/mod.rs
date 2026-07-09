//! Minimal session persistence for `xa`, inspired by pi_agent_rust's session
//! model (one file per session + metadata for fast listing) but kept simple:
//! each session is a single JSON file under `config_dir()/xa/sessions`.

use std::fs;
use std::path::PathBuf;

use dirs::config_dir;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredMessage {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
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
