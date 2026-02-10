use crate::config::Config;
use crate::llm::process_with_llm;
use chrono::Utc;
use dirs::config_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct StoreConfig {
    pub entries: Vec<StoreEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct StoreEntry {
    pub id: u64,
    pub tag: String,
    pub note: String,
    pub secret: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
struct TagResponse {
    tag: String,
    reason: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SearchResponse {
    found: bool,
    id: Option<u64>,
    reason: Option<String>,
}

pub async fn add_secret_with_tag(
    config: &Config,
    secret: &str,
    note: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let secret = secret.trim();
    let note = note.trim();

    if secret.is_empty() {
        eprintln!("Error: secret cannot be empty.");
        return Ok(());
    }

    if note.is_empty() {
        eprintln!("Error: note/description cannot be empty.");
        return Ok(());
    }

    let mut store = load_store()?;

    let existing_tags: HashSet<String> = store
        .entries
        .iter()
        .map(|e| e.tag.to_lowercase())
        .collect();

    let prompt = build_tag_prompt(note, &existing_tags);
    let llm_response = process_with_llm(config, &prompt, false).await?;
    let mut tag = match parse_json::<TagResponse>(&llm_response) {
        Some(parsed) => parsed.tag,
        None => fallback_tag(note),
    };

    tag = sanitize_tag(&tag);
    if tag.is_empty() {
        tag = fallback_tag(note);
    }

    tag = ensure_unique_tag(&tag, &existing_tags);

    let entry = StoreEntry {
        id: Utc::now().timestamp_millis() as u64,
        tag: tag.clone(),
        note: note.to_string(),
        secret: secret.to_string(),
        created_at: Utc::now().to_rfc3339(),
    };

    store.entries.push(entry);
    save_store(&store)?;

    println!("Added secret with tag: {}", tag);
    Ok(())
}

pub async fn search_secret(
    config: &Config,
    query: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let query = query.trim();
    if query.is_empty() {
        eprintln!("Error: query cannot be empty.");
        return Ok(());
    }

    let store = load_store()?;
    if store.entries.is_empty() {
        println!("No found such thing.");
        return Ok(());
    }

    let masked_entries = build_masked_entries(&store.entries);
    let prompt = build_search_prompt(query, &masked_entries);
    let llm_response = process_with_llm(config, &prompt, false).await?;
    let parsed = parse_json::<SearchResponse>(&llm_response);

    if let Some(result) = parsed {
        if result.found {
            if let Some(id) = result.id {
                if let Some(entry) = store.entries.iter().find(|e| e.id == id) {
                    println!("{}", entry.secret);
                    return Ok(());
                }
            }
        }
    }

    println!("No found such thing.");
    Ok(())
}

fn load_store() -> Result<StoreConfig, Box<dyn std::error::Error>> {
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");
    let store_file = config_dir.join("stores.toml");

    if !store_file.exists() {
        return Ok(StoreConfig::default());
    }

    let content = fs::read_to_string(&store_file)?;
    match toml::from_str(&content) {
        Ok(parsed) => Ok(parsed),
        Err(_) => {
            let backup_path = store_file.with_extension("toml.backup");
            fs::rename(&store_file, &backup_path)?;
            eprintln!(
                "Warning: Corrupted stores.toml detected. Backed up to {:?} and created a new one.",
                backup_path
            );
            Ok(StoreConfig::default())
        }
    }
}

fn save_store(store: &StoreConfig) -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");
    fs::create_dir_all(&config_dir)?;
    let store_file = config_dir.join("stores.toml");
    let content = toml::to_string(store)?;
    fs::write(&store_file, content)?;
    Ok(())
}

fn build_tag_prompt(note: &str, existing_tags: &HashSet<String>) -> String {
    let mut existing: Vec<String> = existing_tags.iter().cloned().collect();
    existing.sort();

    format!(
        "You generate short, memorable tags for secret notes.\n\nRules:\n- Return JSON only.\n- JSON schema: {{\"tag\": string, \"reason\": string}}.\n- tag must be 2-4 words max, lowercase, use hyphens instead of spaces.\n- tag must not include any sensitive data (only use the note).\n- tag must not duplicate existing tags.\n\nExisting tags: {:?}\n\nNote: {}\n\nReturn JSON only.",
        existing,
        note
    )
}

fn build_search_prompt(query: &str, masked_entries: &[MaskedEntry]) -> String {
    let entries_json = serde_json::to_string_pretty(masked_entries).unwrap_or_else(|_| "[]".to_string());
    format!(
        "You are a secret locator. Given a user query and a list of entries, find the best matching entry.\n\nRules:\n- Return JSON only.\n- JSON schema: {{\"found\": boolean, \"id\": number|null, \"reason\": string}}.\n- If nothing matches well, set found=false and id=null.\n- Do not invent ids.\n\nEntries (secret is placeholder only):\n{}\n\nQuery: {}\n\nReturn JSON only.",
        entries_json,
        query
    )
}

fn fallback_tag(note: &str) -> String {
    let words: Vec<String> = note
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .take(4)
        .map(|w| w.to_lowercase())
        .collect();
    if words.is_empty() {
        "untagged".to_string()
    } else {
        words.join("-")
    }
}

fn sanitize_tag(tag: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;

    for ch in tag.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if ch == '-' || ch.is_whitespace() {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
    }

    while out.starts_with('-') {
        out.remove(0);
    }
    while out.ends_with('-') {
        out.pop();
    }

    out
}

fn ensure_unique_tag(tag: &str, existing_tags: &HashSet<String>) -> String {
    if !existing_tags.contains(&tag.to_lowercase()) {
        return tag.to_string();
    }

    for i in 2..=99 {
        let candidate = format!("{}-{}", tag, i);
        if !existing_tags.contains(&candidate.to_lowercase()) {
            return candidate;
        }
    }

    format!("{}-{}", tag, Utc::now().timestamp_millis())
}

fn parse_json<T: for<'de> Deserialize<'de>>(input: &str) -> Option<T> {
    if let Ok(parsed) = serde_json::from_str::<T>(input) {
        return Some(parsed);
    }

    let start = input.find('{')?;
    let end = input.rfind('}')?;
    if start >= end {
        return None;
    }

    let slice = &input[start..=end];
    serde_json::from_str::<T>(slice).ok()
}

#[derive(Serialize)]
struct MaskedEntry {
    id: u64,
    tag: String,
    note: String,
    created_at: String,
    secret_placeholder: String,
}

fn build_masked_entries(entries: &[StoreEntry]) -> Vec<MaskedEntry> {
    entries
        .iter()
        .map(|e| MaskedEntry {
            id: e.id,
            tag: e.tag.clone(),
            note: e.note.clone(),
            created_at: e.created_at.clone(),
            secret_placeholder: format!("SECRET_{}", e.id),
        })
        .collect()
}
