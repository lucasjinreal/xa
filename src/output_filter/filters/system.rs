//! Conservative system-command filters inspired by RTK's `cmds/system`.

use std::collections::HashSet;

use serde_json::Value;

use super::common::{has_word, strip_ansi, truncate_chars};

const MAX_LISTING_LINES: usize = 120;
const MAX_LOG_LINES: usize = 100;

pub fn is_package_manager(command: &str) -> bool {
    ["npm", "pnpm", "yarn", "bun", "pip", "poetry", "uv"]
        .iter()
        .any(|word| has_word(command, word))
}

pub fn is_file_listing(command: &str) -> bool {
    ["ls", "find", "tree", "rg", "grep"]
        .iter()
        .any(|word| has_word(command, word))
}

pub fn is_environment(command: &str) -> bool {
    has_word(command, "env") || has_word(command, "printenv")
}

pub fn is_json_command(command: &str) -> bool {
    has_word(command, "jq") || command.contains("--json") || command.contains("json")
}

pub fn is_log_command(command: &str) -> bool {
    has_word(command, "journalctl") || has_word(command, "dmesg")
        || command.contains(" tail ") || command.starts_with("tail ")
}

pub fn filter_package_manager(raw: &str) -> String {
    let mut output = String::new();
    for line in strip_ansi(raw).lines() {
        let trimmed = line.trim();
        let important = trimmed.starts_with("error") || trimmed.starts_with("ERR!")
            || trimmed.starts_with("WARN") || trimmed.starts_with("warning")
            || trimmed.contains("vulnerabilit") || trimmed.contains("added ")
            || trimmed.contains("removed ") || trimmed.contains("changed ")
            || trimmed.contains("up to date") || trimmed.contains("Successfully installed")
            || trimmed.contains("Requirement already satisfied") || trimmed.contains("Packages:");
        if important {
            output.push_str(trimmed);
            output.push('\n');
        }
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}

pub fn filter_file_listing(raw: &str) -> String {
    let cleaned = strip_ansi(raw);
    let mut output = String::new();
    let mut shown = 0usize;
    let mut skipped = 0usize;
    for line in cleaned.lines() {
        if line.trim().is_empty() { continue; }
        if shown == MAX_LISTING_LINES {
            skipped += 1;
            continue;
        }
        output.push_str(&truncate_chars(line, 240));
        output.push('\n');
        shown += 1;
    }
    if skipped > 0 {
        output.push_str(&format!("[… {} additional paths omitted]\n", skipped));
    }
    output
}

pub fn filter_environment(raw: &str) -> String {
    let mut output = String::new();
    let mut seen = HashSet::new();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        if !seen.insert(key) { continue; }
        let sensitive = ["TOKEN", "SECRET", "PASSWORD", "API_KEY", "PRIVATE_KEY"]
            .iter()
            .any(|needle| key.to_ascii_uppercase().contains(needle));
        if sensitive {
            output.push_str(&format!("{key}=<redacted>\n"));
        } else if is_interesting_environment_key(key) {
            output.push_str(&format!("{key}={}\n", truncate_chars(value, 120)));
        }
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}

pub fn filter_json(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return super::default::filter(raw);
    };
    summarize_json(&value, 0)
}

pub fn filter_logs(raw: &str) -> String {
    let mut output = String::new();
    let mut previous = "";
    let mut repeats = 0usize;
    let mut shown = 0usize;
    for line in strip_ansi(raw).lines() {
        let normalized = line.trim();
        if normalized == previous {
            repeats += 1;
            continue;
        }
        if repeats > 0 {
            output.push_str(&format!("[previous line repeated {} times]\n", repeats));
            repeats = 0;
        }
        if shown == MAX_LOG_LINES {
            output.push_str("[… additional log lines omitted]\n");
            break;
        }
        output.push_str(&truncate_chars(normalized, 240));
        output.push('\n');
        previous = normalized;
        shown += 1;
    }
    if repeats > 0 { output.push_str(&format!("[previous line repeated {} times]\n", repeats)); }
    output
}

fn is_interesting_environment_key(key: &str) -> bool {
    key == "PATH" || key == "HOME" || key == "SHELL" || key == "PWD"
        || key.starts_with("CARGO_") || key.starts_with("RUST")
        || key.starts_with("PYTHON") || key.starts_with("NODE_")
        || key.starts_with("JAVA_") || key.starts_with("GO")
        || key.starts_with("CI") || key.starts_with("TERM")
}

fn summarize_json(value: &Value, depth: usize) -> String {
    if depth >= 3 { return "<…>".to_string(); }
    match value {
        Value::Object(map) => {
            let mut fields: Vec<_> = map.iter().collect();
            fields.sort_by_key(|(key, _)| *key);
            let parts: Vec<_> = fields.into_iter().take(30).map(|(key, value)| {
                format!("{key}: {}", summarize_json(value, depth + 1))
            }).collect();
            let suffix = if map.len() > 30 { ", …" } else { "" };
            format!("{{ {}{} }}", parts.join(", "), suffix)
        }
        Value::Array(values) => {
            let samples: Vec<_> = values.iter().take(5).map(|value| summarize_json(value, depth + 1)).collect();
            let suffix = if values.len() > 5 { ", …" } else { "" };
            format!("[{} items: {}{}]", values.len(), samples.join(", "), suffix)
        }
        Value::String(text) => format!("\"{}\"", truncate_chars(text, 120)),
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_filter_redacts_secrets() {
        let output = filter_environment("PATH=/bin\nAPI_TOKEN=secret\nHOME=/tmp\n");
        assert!(output.contains("API_TOKEN=<redacted>"));
        assert!(!output.contains("secret"));
    }

    #[test]
    fn json_filter_keeps_shape_not_large_values() {
        let output = filter_json(r#"{"items":[{"name":"a"},{"name":"b"}],"payload":"abcdefghijklmnopqrstuvwxyz"}"#);
        assert!(output.contains("items: [2 items"));
        assert!(output.contains("payload"));
    }
}
