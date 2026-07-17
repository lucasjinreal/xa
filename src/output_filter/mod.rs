//! Extensible command-output middleware for tool results.
//!
//! Filters are deliberately loss-aware: command-specific filters retain errors,
//! failures, summaries, paths, and structural metadata; a final cap only limits
//! what is ultimately added to the model context.

mod filters;

use serde::{Deserialize, Serialize};

const MAX_CONTEXT_CHARS: usize = 4_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolOutputStats {
    pub tool: String,
    /// A compact command signature (never arbitrary argument values) for
    /// aggregate reporting without persisting secrets from shell commands.
    #[serde(default)]
    pub command: String,
    pub filter: String,
    pub raw_bytes: usize,
    pub returned_bytes: usize,
    pub raw_lines: usize,
    pub returned_lines: usize,
    pub estimated_tokens_saved: usize,
}

pub struct ProcessedOutput {
    pub output: String,
    pub stats: ToolOutputStats,
}

/// Process a tool result before it is shown to the model. Shell commands use
/// the registry; direct tools only receive the universal context cap.
pub fn process(tool: &str, command: Option<&str>, raw: String) -> ProcessedOutput {
    let (filtered, filter) = match (tool, command) {
        ("bash", Some(command)) => filters::process(command, &raw),
        _ => (raw.clone(), "context-cap"),
    };
    let output = cap_context(filtered);
    let raw_bytes = raw.len();
    let returned_bytes = output.len();
    ProcessedOutput {
        stats: ToolOutputStats {
            tool: tool.to_string(),
            command: command_signature(tool, command),
            filter: filter.to_string(),
            raw_bytes,
            returned_bytes,
            raw_lines: raw.lines().count(),
            returned_lines: output.lines().count(),
            estimated_tokens_saved: estimate_tokens(raw_bytes)
                .saturating_sub(estimate_tokens(returned_bytes)),
        },
        output,
    }
}

fn command_signature(tool: &str, command: Option<&str>) -> String {
    let Some(command) = command else { return tool.to_string() };
    let mut parts = Vec::new();
    for part in command.split_whitespace().take(3) {
        let sensitive = part.contains('=')
            || part.to_ascii_lowercase().contains("token")
            || part.to_ascii_lowercase().contains("secret")
            || part.to_ascii_lowercase().contains("password");
        parts.push(if sensitive { "<arg>" } else { part });
    }
    if parts.is_empty() { tool.to_string() } else { parts.join(" ") }
}

fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

fn cap_context(output: String) -> String {
    if output.len() <= MAX_CONTEXT_CHARS {
        return output;
    }
    let shown = filters::common::truncate_chars(&output, MAX_CONTEXT_CHARS);
    format!(
        "{shown}\n[… output capped for model context: {} of {} chars shown]",
        shown.len(),
        output.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_diff_keeps_changes_and_accounts_for_savings() {
        let raw = "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+new\n unchanged context that should not enter the model\n unchanged context that should not enter the model\n";
        let result = process("bash", Some("git diff"), raw.into());
        assert_eq!(result.stats.filter, "git-diff");
        assert!(result.output.contains("+new"));
        assert!(!result.output.contains(" unchanged"));
        assert!(result.stats.estimated_tokens_saved > 0);
    }

    #[test]
    fn test_filter_preserves_failures_and_summary() {
        let raw = "collecting…\ntest_one PASSED\ntest_two FAILED\nAssertionError: expected 2\n=== 1 failed, 1 passed in 1.2s ===\n";
        let result = process("bash", Some("pytest -q"), raw.into());
        assert_eq!(result.stats.filter, "python");
        assert!(result.output.contains("FAILED"));
        assert!(result.output.contains("AssertionError"));
        assert!(result.output.contains("1 failed"));
    }

    #[test]
    fn context_cap_is_included_in_savings_accounting() {
        let result = process("read", None, "x".repeat(MAX_CONTEXT_CHARS * 2));
        assert_eq!(result.stats.filter, "context-cap");
        assert!(result.stats.returned_bytes < result.stats.raw_bytes);
    }
}
