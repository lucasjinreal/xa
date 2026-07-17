use super::common::has_word;

const MAX_FAILURE_LINES: usize = 100;

pub fn matches(command: &str) -> bool {
    ["pytest", "jest", "vitest", "mocha", "phpunit", "go", "npm", "pnpm", "yarn"]
        .iter()
        .any(|word| has_word(command, word))
        && (command.contains("test") || command.contains("jest") || command.contains("vitest"))
}

pub fn filter(raw: &str) -> String {
    let mut output = String::new();
    let mut kept = 0usize;
    for line in raw.lines() {
        let trimmed = line.trim();
        let important = trimmed.contains("FAILED") || trimmed.contains("FAIL")
            || trimmed.contains("Error") || trimmed.contains("error:")
            || trimmed.contains("panic") || trimmed.contains("Assertion")
            || trimmed.contains("expected") || trimmed.contains("actual")
            || trimmed.starts_with("test result:") || trimmed.starts_with("Ran ")
            || trimmed.contains(" passed") || trimmed.contains(" failed")
            || trimmed.starts_with("===");
        if !important { continue; }
        if kept == MAX_FAILURE_LINES {
            output.push_str("[… additional test diagnostics omitted]\n");
            break;
        }
        output.push_str(trimmed);
        output.push('\n');
        kept += 1;
    }
    if output.is_empty() { super::default::filter(raw) } else { output }
}
