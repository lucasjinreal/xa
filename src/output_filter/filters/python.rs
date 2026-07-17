//! Python-oriented filters adapted from RTK's pytest, ruff, mypy, pip, and uv handlers.

use serde_json::Value;

use super::common::{strip_ansi, truncate_chars, words};

const MAX_DIAGNOSTICS: usize = 80;

pub fn matches(command: &str) -> bool {
    let words = words(command);
    words.iter().any(|word| matches!(*word, "pytest" | "ruff" | "mypy" | "black" | "pip" | "uv"))
        || (words.iter().any(|word| *word == "python" || *word == "python3")
            && (command.contains("pytest") || command.contains("ruff") || command.contains("mypy")))
}

pub fn filter(raw: &str) -> String {
    // Ruff's JSON diagnostics carry precise file/rule/location information.
    // Handle them before general text parsing.
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        if value.is_array() {
            return filter_ruff_json(&value).unwrap_or_else(|| filter_text(raw));
        }
    }
    filter_text(raw)
}

fn filter_text(raw: &str) -> String {
    let cleaned = strip_ansi(raw);
    if cleaned.contains("Traceback (most recent call last):") {
        return filter_traceback(&cleaned);
    }
    if cleaned.lines().any(|line| line.contains(": error:")) {
        return filter_mypy(&cleaned);
    }
    if cleaned.contains("==== FAILURES ====") || cleaned.contains(" failed") || cleaned.contains(" FAILED") {
        return filter_pytest(&cleaned);
    }
    if cleaned.lines().any(|line| line.contains("would reformat") || line.contains("reformatted ")) {
        return filter_formatter(&cleaned);
    }
    if cleaned.contains("Successfully installed") || cleaned.contains("Requirement already satisfied") {
        return super::system::filter_package_manager(&cleaned);
    }
    super::default::filter(&cleaned)
}

fn filter_pytest(raw: &str) -> String {
    let mut output = String::new();
    let mut in_failures = false;
    let mut kept = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim_end();
        if trimmed.contains("==== FAILURES ====") { in_failures = true; }
        let important = in_failures
            || trimmed.contains("FAILED") || trimmed.contains("ERROR")
            || trimmed.starts_with("E   ") || trimmed.starts_with("E ")
            || trimmed.contains("AssertionError") || trimmed.starts_with("===");
        if !important { continue; }
        if kept == MAX_DIAGNOSTICS {
            output.push_str("[… additional pytest diagnostics omitted]\n");
            break;
        }
        output.push_str(&truncate_chars(trimmed, 260));
        output.push('\n');
        kept += 1;
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}

fn filter_mypy(raw: &str) -> String {
    let mut output = String::new();
    let mut errors = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim();
        let diagnostic = trimmed.contains(": error:") || trimmed.contains(": note:");
        if diagnostic {
            if errors == MAX_DIAGNOSTICS {
                output.push_str("[… additional mypy diagnostics omitted]\n");
                break;
            }
            output.push_str(&truncate_chars(trimmed, 300));
            output.push('\n');
            if trimmed.contains(": error:") { errors += 1; }
        } else if trimmed.starts_with("Found ") || trimmed.contains("Success: no issues found") {
            output.push_str(trimmed);
            output.push('\n');
        }
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}

fn filter_traceback(raw: &str) -> String {
    let mut output = String::new();
    let mut in_traceback = false;
    let mut kept = 0usize;
    for line in raw.lines() {
        if line.contains("Traceback (most recent call last):") { in_traceback = true; }
        if !in_traceback { continue; }
        if kept == 60 {
            output.push_str("[… additional traceback frames omitted]\n");
            break;
        }
        output.push_str(&truncate_chars(line.trim_end(), 300));
        output.push('\n');
        kept += 1;
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}

fn filter_formatter(raw: &str) -> String {
    let mut output = String::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.contains("would reformat") || trimmed.contains("reformatted ")
            || trimmed.contains("left unchanged") || trimmed.contains("All done!")
            || trimmed.contains("files would be") {
            output.push_str(&truncate_chars(trimmed, 260));
            output.push('\n');
        }
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}

fn filter_ruff_json(value: &Value) -> Option<String> {
    let issues = value.as_array()?;
    if issues.is_empty() { return Some("ruff: no issues found\n".to_string()); }
    let mut output = format!("ruff: {} issues\n", issues.len());
    for issue in issues.iter().take(MAX_DIAGNOSTICS) {
        let filename = issue.get("filename")?.as_str()?;
        let row = issue.get("location")?.get("row")?.as_u64()?;
        let column = issue.get("location")?.get("column")?.as_u64()?;
        let code = issue.get("code").and_then(Value::as_str).unwrap_or("?");
        let message = issue.get("message").and_then(Value::as_str).unwrap_or("diagnostic");
        output.push_str(&format!("{filename}:{row}:{column}: {code} {message}\n"));
    }
    if issues.len() > MAX_DIAGNOSTICS {
        output.push_str(&format!("[… {} additional ruff issues omitted]\n", issues.len() - MAX_DIAGNOSTICS));
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mypy_filter_keeps_error_locations_and_summary() {
        let output = filter_text("Checking 10 files\nsrc/a.py:4: error: Bad type  [arg-type]\nFound 1 error in 1 file\n");
        assert!(output.contains("src/a.py:4: error"));
        assert!(output.contains("Found 1 error"));
        assert!(!output.contains("Checking"));
    }

    #[test]
    fn ruff_json_filter_keeps_actionable_fields() {
        let value: Value = serde_json::from_str(r#"[{"filename":"a.py","location":{"row":2,"column":4},"code":"F401","message":"unused import"}]"#).unwrap();
        let output = filter_ruff_json(&value).unwrap();
        assert!(output.contains("a.py:2:4: F401 unused import"));
    }
}
