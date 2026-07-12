//! Built-in agent tools for `xa`, modeled on pi_agent_rust's local tool
//! execution: each tool implements [`Tool`] (name/description/JSON-schema
//! `parameters`/`execute`) and is exposed to the model via OpenAI-compatible
//! `tools`. The [`crate::agent::run_conversation`] loop executes them
//! locally and feeds results back (role `tool`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use similar::TextDiff;

use serde_json::Value;

/// A local agent tool.
pub trait Tool: Send + Sync {
    /// Tool name (used by the model to invoke it).
    fn name(&self) -> &str;
    /// Human description shown to the model.
    fn description(&self) -> &str;
    /// JSON Schema object describing the tool's `parameters`.
    fn parameters(&self) -> Value;
    /// Execute the tool with parsed arguments; returns text output.
    fn execute(&self, args: Value) -> Result<String, String>;
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing argument `{key}`"))
}

fn arg_opt(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn read_file_capped(path: &Path, max_bytes: usize) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if data.len() > max_bytes {
        return Err(format!(
            "file too large: {} bytes (limit {max_bytes})",
            data.len()
        ));
    }
    Ok(String::from_utf8_lossy(&data).to_string())
}

// ===========================================================================
// bash
// ===========================================================================

pub struct BashTool;
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn description(&self) -> &str {
        "Run a shell command via `sh -c` and return its combined stdout/stderr. Use for git, builds, tests, and filesystem operations."
    }
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "shell command to run" }
            },
            "required": ["command"]
        })
    }
    fn execute(&self, args: Value) -> Result<String, String> {
        let cmd = arg_str(&args, "command")?;
        let out = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .map_err(|e| format!("failed to run command: {e}"))?;
        let mut s = String::new();
        s.push_str(&String::from_utf8_lossy(&out.stdout));
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.trim().is_empty() {
            s.push_str("\n[stderr]\n");
            s.push_str(&err);
        }
        s.push_str(&format!("\n[exit code {}]", out.status.code().unwrap_or(-1)));
        Ok(s)
    }
}

// ===========================================================================
// read
// ===========================================================================

pub struct ReadTool;
impl Tool for ReadTool {
    fn name(&self) -> &str { "read" }
    fn description(&self) -> &str {
        "Read a UTF-8 text file and return its contents."
    }
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "path to the file" },
                "offset": { "type": "integer", "description": "1-based line to start reading from (optional)" },
                "limit": { "type": "integer", "description": "max number of lines to read (optional)" }
            },
            "required": ["path"]
        })
    }
    fn execute(&self, args: Value) -> Result<String, String> {
        let path = arg_str(&args, "path")?;
        let text = read_file_capped(Path::new(path), 200 * 1024)?;
        let offset = args.get("offset").and_then(|n| n.as_u64()).map(|n| n as usize);
        let limit = args.get("limit").and_then(|n| n.as_u64()).map(|n| n as usize);
        if offset.is_none() && limit.is_none() {
            return Ok(text);
        }
        let start = offset.map(|n| n.saturating_sub(1)).unwrap_or(0);
        let lines: Vec<&str> = text.lines().collect();
        let mut end = lines.len();
        if let Some(lim) = limit {
            end = (start + lim).min(lines.len());
        }
        Ok(lines
            .get(start..end)
            .map(|s| s.join("\n"))
            .unwrap_or_default())
    }
}

// ===========================================================================
// write
// ===========================================================================

pub struct WriteTool;
impl Tool for WriteTool {
    fn name(&self) -> &str { "write" }
    fn description(&self) -> &str {
        "Write text content to a file, creating parent directories as needed. Prefer `edit` for modifying existing files — only use `write` to create a brand-new file or replace an entire file on purpose. NEVER use `write` for a small change to an existing file."
    }
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "path to write" },
                "content": { "type": "string", "description": "content to write" }
            },
            "required": ["path", "content"]
        })
    }
    fn execute(&self, args: Value) -> Result<String, String> {
        let path = arg_str(&args, "path")?;
        let content = arg_str(&args, "content")?;
        let p = PathBuf::from(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&p, content)
            .map_err(|e| format!("cannot write {}: {e}", p.display()))?;
        Ok(format!("wrote {} bytes to {}", content.len(), p.display()))
    }
}

// ===========================================================================
// edit
// ===========================================================================

pub struct EditTool;
impl Tool for EditTool {
    fn name(&self) -> &str { "edit" }
    fn description(&self) -> &str {
        "Make a small, surgical change: replace the first occurrence of `old` with `new` in a file and write it back. Use this for edits to existing files — keep `old`/`new` to just the lines that change. Use `write` only to create a new file."
    }
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "path to edit" },
                "old": { "type": "string", "description": "text to replace" },
                "new": { "type": "string", "description": "replacement text" }
            },
            "required": ["path", "old", "new"]
        })
    }
    fn execute(&self, args: Value) -> Result<String, String> {
        let path = arg_str(&args, "path")?;
        let old = arg_str(&args, "old")?;
        let new = arg_str(&args, "new")?;
        let p = Path::new(path);
        let text = read_file_capped(p, 200 * 1024)?;
        if let Some(pos) = text.find(old) {
            let mut out = String::with_capacity(text.len());
            out.push_str(&text[..pos]);
            out.push_str(new);
            out.push_str(&text[pos + old.len()..]);
            std::fs::write(p, &out)
                .map_err(|e| format!("cannot write {}: {e}", p.display()))?;
            Ok(format!("edited {} ({} -> {} bytes)", p.display(), text.len(), out.len()))
        } else {
            Err(format!("`old` string not found in {}", p.display()))
        }
    }
}

// ===========================================================================
// glob
// ===========================================================================

fn glob_to_regex(pattern: &str) -> Result<regex::Regex, String> {
    let mut re = String::from("^");
    for c in pattern.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push_str("."),
            c if ".+()|{}^$[]\\".contains(c) => {
                re.push('\\');
                re.push(c);
            }
            c => re.push(c),
        }
    }
    re.push('$');
    regex::Regex::new(&re).map_err(|e| format!("bad glob: {e}"))
}

pub struct GlobTool;
impl Tool for GlobTool {
    fn name(&self) -> &str { "glob" }
    fn description(&self) -> &str {
        "List files matching a glob pattern (e.g. `**/*.rs`) under the current directory."
    }
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "glob pattern" }
            },
            "required": ["pattern"]
        })
    }
    fn execute(&self, args: Value) -> Result<String, String> {
        let pattern = arg_str(&args, "pattern")?;
        let re = glob_to_regex(pattern)?;
        let mut found: Vec<String> = Vec::new();
        let mut visited = 0usize;
        let mut stack = vec![PathBuf::from(".")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                if found.len() >= 2000 || visited >= 50000 {
                    break;
                }
                visited += 1;
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    stack.push(p);
                } else if ft.is_file() {
                    if let Some(s) = p.to_str() {
                        if re.is_match(s) {
                            found.push(s.to_string());
                        }
                    }
                }
            }
        }
        if found.is_empty() {
            Ok("(no matches)".into())
        } else {
            found.sort();
            Ok(found.join("\n"))
        }
    }
}

// ===========================================================================
// grep
// ===========================================================================

pub struct GrepTool;
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }
    fn description(&self) -> &str {
        "Search file contents for a substring (case-insensitive), returning matching `file:line` entries."
    }
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "substring to search" },
                "path": { "type": "string", "description": "directory or file to search (default: .)" }
            },
            "required": ["pattern"]
        })
    }
    fn execute(&self, args: Value) -> Result<String, String> {
        let pattern = arg_str(&args, "pattern")?.to_lowercase();
        let root = arg_opt(&args, "path").unwrap_or_else(|| ".".into());
        let mut hits: Vec<String> = Vec::new();
        let mut visited = 0usize;
        let mut stack = vec![PathBuf::from(&root)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                if hits.len() >= 2000 || visited >= 50000 {
                    break;
                }
                visited += 1;
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    stack.push(p);
                } else if ft.is_file() {
                    if let Ok(text) = read_file_capped(&p, 2 * 1024 * 1024) {
                        for (i, line) in text.lines().enumerate() {
                            if line.to_lowercase().contains(&pattern) {
                                hits.push(format!("{}:{}: {}", p.display(), i + 1, line.trim()));
                                if hits.len() >= 2000 {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
        if hits.is_empty() {
            Ok("(no matches)".into())
        } else {
            Ok(hits.join("\n"))
        }
    }
}

/// Produce a minimal, colorful-free unified diff between the *before* and
/// *after* contents of `path` so the TUI can render exactly what an
/// `edit`/`write` tool changed.
///
/// Unlike the previous git-based approach, this is purely content based: it
/// works whether or not the file is tracked by git, and — crucially — edits to
/// an untracked file show only the changed hunk(s) instead of the entire file
/// being printed as a wall of `+` lines. New/empty `before` content is treated
/// as a brand-new file and emits a `--- /dev/null` header so the TUI still
/// labels it "← New file".
pub fn unified_diff(path: &str, before: &str, after: &str) -> Option<String> {
    if before == after {
        return None;
    }

    if before.is_empty() {
        // Brand-new file: every line is an addition.
        let n = after.lines().count();
        let mut s = String::new();
        s.push_str(&format!("diff --git a/{p} b/{p}\n", p = path));
        s.push_str("new file mode 100644\n");
        s.push_str("--- /dev/null\n");
        s.push_str(&format!("+++ b/{p}\n", p = path));
        s.push_str(&format!("@@ -0,0 +1,{n} @@\n"));
        for line in after.lines() {
            s.push('+');
            s.push_str(line);
            s.push('\n');
        }
        return Some(s);
    }

    let diff = TextDiff::from_lines(before, after);
    let old_header = format!("a/{path}");
    let new_header = format!("b/{path}");
    let s = diff
        .unified_diff()
        .context_radius(3)
        .header(&old_header, &new_header)
        .to_string();
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

/// All built-in tools.
pub fn all_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(BashTool),
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(GlobTool),
        Arc::new(GrepTool),
    ]
}

/// Find a tool by name.
pub fn find_tool<'a>(name: &str, tools: &'a [Arc<dyn Tool>]) -> Option<&'a Arc<dyn Tool>> {
    tools.iter().find(|t| t.name() == name)
}

/// Execute a tool by name with a JSON `args` object. Bounded by a 120s timeout
/// and run on a blocking thread so it never stalls the async runtime.
pub async fn call_tool(
    name: &str,
    args: Value,
    tools: &[Arc<dyn Tool>],
) -> Result<String, String> {
    let tool = find_tool(name, tools)
        .ok_or_else(|| format!("unknown tool: {name}"))?
        .clone();
    match tokio::time::timeout(Duration::from_secs(120), tokio::task::spawn_blocking(move || tool.execute(args))).await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("tool panicked: {e}")),
        Err(_) => Err("tool timed out after 120s".into()),
    }
}
