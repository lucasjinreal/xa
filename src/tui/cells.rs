//! HistoryCell (DESIGN.md §3).
//!
//! One self-contained transcript entry. Each cell knows its own height at a
//! given width and how to render itself into a clipped rectangle, following the
//! DESIGN.md "HistoryCell" pattern rather than one giant scrollable paragraph.
//!
//! Scrolling is *unified*: the transcript is one continuous column of rows and
//! every cell is flattened into a list of rows. When the viewport is scrolled
//! into the middle of a cell, `render` receives a `skip` count telling it how
//! many of that cell's own leading rows are above the viewport, so the visible
//! slice continues seamlessly from the cell above instead of restarting at the
//! cell's first row.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;
use ratatui_markdown::{
    markdown::{MarkdownBlock, MarkdownRenderer},
    theme::ThemeConfig,
};

use crate::tui::render::RenderContext;
use crate::tui::shimmer::shimmer_spans;
use crate::tui::theme;

/// A single drawable row inside a cell. `x`/`w` are offsets relative to the
/// cell's left edge (which is always the transcript's left edge), so the same
/// row can be drawn no matter where the cell is positioned in the viewport.
pub struct Row {
    pub x: u16,
    pub w: u16,
    pub line: Line<'static>,
}

impl Row {
    fn blank(width: u16) -> Self {
        Row {
            x: 0,
            w: width,
            line: Line::default(),
        }
    }
}

/// Soft-wrap `text` to `max_w` display columns, preserving every character
/// (wrapping only inserts line breaks). Wide/CJK glyphs count as 2 columns.
fn wrap_text(text: &str, max_w: usize) -> Vec<String> {
    let max_w = max_w.max(1);
    let mut out: Vec<String> = Vec::new();
    for logical in text.lines() {
        let chars: Vec<char> = logical.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut i = 0;
        while i < chars.len() {
            let mut col = 0usize;
            let mut j = i;
            while j < chars.len() {
                let w = UnicodeWidthStr::width(chars[j].encode_utf8(&mut [0; 4]));
                if col + w > max_w && j > i {
                    break;
                }
                col += w;
                j += 1;
            }
            if j == i {
                j = i + 1; // a single glyph wider than the box
            }
            out.push(chars[i..j].iter().collect());
            i = j;
        }
    }
    out
}

/// Normalize raw model text before markdown parse.
///
/// - `\r\n` / bare `\r` → `\n`
/// - collapse 3+ consecutive newlines to a double newline (one blank line)
fn normalize_md_source(text: &str) -> String {
    let mut s = text.replace("\r\n", "\n").replace('\r', "\n");
    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    s
}

/// True when `s` is non-empty and only ASCII punctuation (e.g. `"?"`, `"..."`).
fn only_ascii_punct(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_punctuation())
}

/// `ratatui-markdown` keeps every source line of a paragraph as a separate
/// render line, so a single soft newline becomes a hard visual break. CommonMark
/// treats those as soft breaks (a space). Join multi-line paragraphs (and
/// nested blockquote paragraphs) before wrapping.
///
/// Hard breaks (two trailing spaces before the newline) are preserved as `\n`.
/// A next line that is only punctuation is glued without a space (`today` +
/// `?` → `today?`) so model soft-wraps don't leave a lone `?` on the next row.
fn join_paragraph_soft_breaks(blocks: &mut [MarkdownBlock]) {
    for block in blocks {
        match block {
            MarkdownBlock::Paragraph(lines) if lines.len() > 1 => {
                let mut joined = String::new();
                for (i, raw) in lines.iter().enumerate() {
                    if i == 0 {
                        joined.push_str(raw.trim_end());
                        continue;
                    }
                    let prev_hard = lines[i - 1].ends_with("  ");
                    let piece = raw.trim_start();
                    if piece.is_empty() {
                        continue;
                    }
                    if prev_hard {
                        joined.push('\n');
                        joined.push_str(piece.trim_end());
                    } else if only_ascii_punct(piece) {
                        // glue punctuation onto the previous word
                        joined.push_str(piece.trim_end());
                    } else {
                        if !joined.is_empty()
                            && !joined.ends_with(|c: char| c.is_whitespace())
                        {
                            joined.push(' ');
                        }
                        joined.push_str(piece.trim_end());
                    }
                }
                *lines = vec![joined];
            }
            MarkdownBlock::Blockquote { children, .. } => {
                join_paragraph_soft_breaks(children);
            }
            _ => {}
        }
    }
}

fn line_is_blank(l: &Line<'_>) -> bool {
    l.spans.iter().all(|s| s.content.trim().is_empty())
}

/// Keep at most one blank line between content blocks; drop leading/trailing.
fn compact_md_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut prev_blank = true; // suppress leading blanks
    for line in lines {
        let blank = line_is_blank(&line);
        if blank {
            if !prev_blank {
                out.push(Line::default());
                prev_blank = true;
            }
        } else {
            out.push(line);
            prev_blank = false;
        }
    }
    while out.last().is_some_and(line_is_blank) {
        out.pop();
    }
    out
}

/// Merge a paragraph that is only punctuation into the previous paragraph.
/// Fixes model output like `"…today\n\n?"` which otherwise renders as a lone
/// `?` row after a blank line.
fn merge_orphan_punct_paragraphs(blocks: &mut Vec<MarkdownBlock>) {
    let mut i = 1;
    while i < blocks.len() {
        let is_orphan = matches!(
            &blocks[i],
            MarkdownBlock::Paragraph(lines)
                if lines.len() == 1 && only_ascii_punct(&lines[0])
        );
        if !is_orphan {
            i += 1;
            continue;
        }
        // Prefer merging into the nearest previous paragraph, skipping a
        // single BlankLine in between.
        let mut target = None;
        if matches!(&blocks[i - 1], MarkdownBlock::Paragraph(_)) {
            target = Some(i - 1);
        } else if i >= 2
            && matches!(&blocks[i - 1], MarkdownBlock::BlankLine)
            && matches!(&blocks[i - 2], MarkdownBlock::Paragraph(_))
        {
            target = Some(i - 2);
        }
        if let Some(t) = target {
            let punct = match blocks.remove(i) {
                MarkdownBlock::Paragraph(lines) => lines.into_iter().next().unwrap_or_default(),
                _ => unreachable!(),
            };
            // Drop the blank separator if we skipped over it.
            if t + 1 < blocks.len() && matches!(&blocks[t + 1], MarkdownBlock::BlankLine) {
                blocks.remove(t + 1);
            }
            if let MarkdownBlock::Paragraph(lines) = &mut blocks[t] {
                if let Some(last) = lines.last_mut() {
                    last.push_str(punct.trim());
                } else {
                    lines.push(punct);
                }
            }
            // stay at i (now points at whatever followed the orphan)
            continue;
        }
        i += 1;
    }
}

/// Parse + render markdown into styled lines at `max_width`, with soft-break
/// joining and blank-line compaction so assistant text doesn't fracture into
/// odd one-word / lone-punctuation rows.
fn render_markdown(text: &str, max_width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let src = normalize_md_source(text);
    let renderer = MarkdownRenderer::new(max_width.max(1));
    let mut blocks = renderer.parse(&src);
    join_paragraph_soft_breaks(&mut blocks);
    merge_orphan_punct_paragraphs(&mut blocks);
    let lines = renderer.render(&blocks, &ThemeConfig::default());
    compact_md_lines(lines)
}

/// One self-contained transcript entry.
pub trait HistoryCell {
    /// Height in rows this cell needs at `width`.
    fn desired_height(&self, width: u16) -> u16;
    /// Render into `area` (the visible slice of this cell). `skip` is the
    /// number of this cell's own leading rows that are above the viewport and
    /// therefore must not be drawn — they belong to the scroll, not the cell.
    fn render(&self, area: Rect, skip: u16, buf: &mut Buffer, ctx: &RenderContext);
    /// For downcasting concrete cell types (needed for in-place mutation).
    fn as_any(&self) -> &dyn std::any::Any;
    /// Mutable downcast accessor.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    /// Optional full-width background fill for the cell's visible slice.
    fn bg(&self) -> Option<Color> {
        None
    }
}

/// Draw a flat list of rows into `area`, honouring `skip` and clipping to the
/// bottom of the area. Shared by every cell so unified scrolling behaves
/// identically everywhere.
fn paint_rows(rows: &[Row], area: Rect, skip: u16, buf: &mut Buffer) {
    let mut y = area.top();
    for row in rows.iter().skip(skip as usize) {
        if y >= area.bottom() {
            break;
        }
        buf.set_line(area.left() + row.x, y, &row.line, row.w);
        y += 1;
    }
}

// ---- System / Note cell ----------------------------------------------------

pub struct SystemCell {
    pub content: String,
}

impl SystemCell {
    fn build(&self, width: u16, _ctx: Option<&RenderContext>) -> Vec<Row> {
        let styled = render_markdown(&self.content, width as usize);
        let mut rows: Vec<Row> = styled
            .into_iter()
            .map(|line| Row {
                x: 0,
                w: width,
                line,
            })
            .collect();
        rows.push(Row::blank(width)); // trailing separator
        rows
    }
}

impl HistoryCell for SystemCell {
    fn desired_height(&self, width: u16) -> u16 {
        self.build(width, None).len() as u16
    }
    fn render(&self, area: Rect, skip: u16, buf: &mut Buffer, ctx: &RenderContext) {
        let rows = self.build(area.width, Some(ctx));
        paint_rows(&rows, area, skip, buf);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// ---- User cell -------------------------------------------------------------

/// Leading prompt for user messages and the input bar (U+276F HEAVY
/// RIGHT-POINTING ANGLE). Display width is 1; with a trailing space the
/// full lead occupies [`USER_LEAD_COLS`] columns.
pub const USER_PROMPT: &str = "❯";
/// Columns reserved for `❯ ` before user text (glyph + space).
pub const USER_LEAD_COLS: u16 = 2;

pub struct UserCell {
    pub content: String,
}

impl UserCell {
    fn build(&self, width: u16, _ctx: Option<&RenderContext>) -> Vec<Row> {
        // Layout: [1 col margin][❯ ][text…][1 col margin]
        let left_margin = 1u16;
        let right_margin = 1u16;
        let text_x = left_margin + USER_LEAD_COLS;
        let avail = (width
            .saturating_sub(left_margin + USER_LEAD_COLS + right_margin))
        .max(1);
        let mut wrapped = wrap_text(&self.content, avail as usize);
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }

        // Grey lead on dim gray cell fill; text stays readable white-grey.
        let prompt_style = Style::default()
            .fg(theme::USER_LEAD)
            .bg(theme::USER_BG)
            .add_modifier(Modifier::BOLD);
        let text_style = Style::default().fg(theme::TEXT).bg(theme::USER_BG);

        let mut rows = vec![Row::blank(width)]; // top padding
        for (i, l) in wrapped.into_iter().enumerate() {
            if i == 0 {
                // First line carries the ❯ lead; wrapped lines hang under the text.
                rows.push(Row {
                    x: left_margin,
                    w: USER_LEAD_COLS + avail,
                    line: Line::from(vec![
                        Span::styled(format!("{USER_PROMPT} "), prompt_style),
                        Span::styled(l, text_style),
                    ]),
                });
            } else {
                rows.push(Row {
                    x: text_x,
                    w: avail,
                    line: Line::from(Span::styled(l, text_style)),
                });
            }
        }
        rows.push(Row::blank(width)); // bottom padding
        rows
    }
}

impl HistoryCell for UserCell {
    fn desired_height(&self, width: u16) -> u16 {
        self.build(width, None).len() as u16
    }
    fn render(&self, area: Rect, skip: u16, buf: &mut Buffer, ctx: &RenderContext) {
        let rows = self.build(area.width, Some(ctx));
        paint_rows(&rows, area, skip, buf);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn bg(&self) -> Option<Color> {
        // True neutral gray (R=G=B) — previous slate had a blue cast.
        Some(theme::USER_BG)
    }
}

// ---- Tool call card (DESIGN.md §7) -----------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum ToolStatus {
    Running,
    Success,
    Failed,
}

/// Left indent of thinking content: text blocks and tool-call headers align
/// here, so the whole assistant turn reads as one indented column.
const THINK_INDENT: u16 = 2;
/// Extra indent for content nested *under* a tool card (diff / read meta).
const TOOL_BODY_INDENT: u16 = 4;

pub struct ToolCallCell {
    pub tool_name: String,
    pub args_preview: String,
    pub status: ToolStatus,
    pub output: Option<String>,
    /// A unified `git diff` (no color codes) for file-mutating tools.
    pub diff: Option<String>,
    pub expanded: bool,
    /// Target path for read/edit/write tools (for the `← Edit` / `→ Read`
    /// summary headers).
    pub path: Option<String>,
    /// Requested `offset`/`limit` for read tools (shown in the `→ Read` header).
    pub read_offset: Option<usize>,
    pub read_limit: Option<usize>,
}

impl ToolCallCell {
    pub fn header_line(&self, ctx: Option<&RenderContext>) -> Line<'static> {
        let (icon, color) = match self.status {
            ToolStatus::Running => ("▸", theme::ACCENT),
            ToolStatus::Success => ("✓", theme::SUCCESS),
            ToolStatus::Failed => ("✗", theme::ERROR),
        };
        let mut spans = vec![Span::styled(
            format!(" {icon} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )];
        let summary = format!("{}  {}", self.tool_name, self.args_preview);
        match ctx {
            Some(c) if self.status == ToolStatus::Running => {
                let mut s = shimmer_spans(&summary, color, c.shimmer_phase);
                spans.append(&mut s);
            }
            _ => spans.push(Span::styled(summary, Style::default().fg(color))),
        }
        let toggle = if self.output.is_some() || self.diff.is_some() {
            let hint = if self.expanded {
                "  ▾".to_string()
            } else {
                // Collapsed: hide the bulk, show only a one-line summary so
                // the user knows what's there without dumping content
                // (DESIGN.md §7: long output is truncated + "see more", not
                // shown by default). grep/bash/... get a count; edits get a
                // change summary.
                let n = if let Some(d) = self.diff.as_deref() {
                    format!("  ▸ {}", diff_change_summary(d))
                } else if let Some(o) = self.output.as_deref() {
                    let c = o.lines().count();
                    if self.tool_name == "grep" {
                        format!("  ▸ {c} matches")
                    } else {
                        format!("  ▸ {c} lines")
                    }
                } else {
                    "  ▸".to_string()
                };
                n
            };
            hint
        } else {
            "".to_string()
        };
        if !toggle.is_empty() {
            spans.push(Span::styled(
                toggle.to_string(),
                Style::default().fg(theme::TEXT_DIM),
            ));
        }
        Line::from(spans)
    }

    fn build(&self, width: u16, ctx: Option<&RenderContext>) -> Vec<Row> {
        let mut rows = vec![Row {
            x: THINK_INDENT,
            w: width.saturating_sub(THINK_INDENT),
            line: self.header_line(ctx),
        }];
        if self.expanded {
            let x = TOOL_BODY_INDENT;
            let w = width.saturating_sub(TOOL_BODY_INDENT);
            match self.tool_name.as_str() {
                "read" => {
                    // Reads don't dump the file; show a compact `→ Read` header
                    // with the requested window instead of raw content.
                    if let Some(p) = self.path.as_deref() {
                        rows.push(Row {
                            x,
                            w,
                            line: Line::from(Span::styled(
                                format!("→ Read {}", p),
                                Style::default()
                                    .fg(theme::ACCENT)
                                    .add_modifier(Modifier::BOLD),
                            )),
                        });
                    }
                    if self.read_offset.is_some() || self.read_limit.is_some() {
                        let off = self.read_offset.map(|n| n as i64).unwrap_or(1);
                        let lim = self.read_limit.map(|n| n as i64).unwrap_or(-1);
                        rows.push(Row {
                            x,
                            w,
                            line: Line::from(Span::styled(
                                format!("  [offset={}, limit={}]", off, lim),
                                Style::default().fg(theme::TEXT_DIM),
                            )),
                        });
                    }
                }
                _ => {
                    // Prefer a colorful, line-numbered edit view for
                    // file-mutating tools (DESIGN.md §7: "each edit/add prints
                    // git diff colorful").
                    if let Some(diff) = self.diff.as_deref() {
                        let label = if diff_is_new_file(diff) {
                            "← New file"
                        } else {
                            "← Edit"
                        };
                        let mut shown = build_diff_rows(diff, x, w, label);
                        let limit = 60;
                        if shown.len() > limit {
                            shown.truncate(limit);
                            shown.push(Row {
                                x,
                                w,
                                line: Line::from(Span::styled(
                                    "…(truncated)".to_string(),
                                    Style::default().fg(theme::TEXT_DIM),
                                )),
                            });
                        }
                        rows.append(&mut shown);
                    } else if let Some(out) = self.output.as_deref() {
                        let all: Vec<&str> = out.lines().collect();
                        // Keep the expanded preview tiny: the bulk is hidden by
                        // default, this is just a peek. Content is also capped
                        // upstream before it ever reaches the TUI.
                        let limit = 5;
                        let shown: String = if all.len() > limit {
                            format!("{}\n…({} more lines, expand to view)", all[..limit].join("\n"), all.len() - limit)
                        } else {
                            out.to_string()
                        };
                        let color = if self.status == ToolStatus::Failed {
                            theme::ERROR
                        } else {
                            theme::TEXT_DIM
                        };
                        for l in shown.lines() {
                            rows.push(Row {
                                x,
                                w,
                                line: Line::from(Span::styled(
                                    l.to_string(),
                                    Style::default().fg(color),
                                )),
                            });
                        }
                    }
                }
            }
        }
        rows
    }
}

/// Parse a unified `git diff` into a compact, line-numbered edit view:
/// a `<header_label> <path>` header followed by each changed line prefixed
/// with its line number and a `-`/`+` marker. `header_label` is "← Edit" for
/// an edit to a tracked file or "← New file" for an untracked/new file, so a
/// full-file addition is never misread as a "full rewrite" of an existing one.
fn build_diff_rows(diff: &str, x: u16, w: u16, header_label: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut old_ln: usize = 0;
    let mut new_ln: usize = 0;
    for raw in diff.lines() {
        if raw.starts_with("diff --git")
            || raw.starts_with("index ")
            || raw.starts_with("new file mode")
            || raw.starts_with("old mode")
            || raw.starts_with("deleted file mode")
            || raw.starts_with("Binary files")
            || raw.starts_with("--- ")
        {
            continue;
        }
        if raw.starts_with("+++ ") {
            let p = raw[4..].trim_start();
            let p = p.strip_prefix("b/").unwrap_or(p);
            let p = p.strip_prefix("./").unwrap_or(p);
            rows.push(Row {
                x,
                w,
                line: Line::from(Span::styled(
                    format!("{} {}", header_label, p),
                    Style::default()
                        .fg(theme::WARNING)
                        .add_modifier(Modifier::BOLD),
                )),
            });
            continue;
        }
        if raw.starts_with("@@") {
            if let Some((old, new)) = parse_hunk_header(raw) {
                old_ln = old;
                new_ln = new;
            }
            continue;
        }
        let (sign, content, ln) = match raw.chars().next() {
            Some('-') => ("-", &raw[1..], old_ln),
            Some('+') => ("+", &raw[1..], new_ln),
            Some(' ') => (" ", &raw[1..], new_ln),
            _ => ("", raw, new_ln),
        };
        match raw.chars().next() {
            Some('-') => old_ln += 1,
            Some('+') => new_ln += 1,
            Some(' ') => {
                old_ln += 1;
                new_ln += 1;
            }
            _ => {}
        }
        let color = match sign {
            "-" => theme::DIFF_DEL,
            "+" => theme::DIFF_ADD,
            _ => theme::DIFF_META,
        };
        rows.push(Row {
            x,
            w,
            line: Line::from(vec![
                Span::styled(
                    format!("{:>4} ", ln),
                    Style::default().fg(theme::TEXT_HINT),
                ),
                Span::styled(format!("{} ", sign), Style::default().fg(color)),
                Span::styled(content.to_string(), Style::default().fg(color)),
            ]),
        });
    }
    rows
}

/// Parse the line numbers out of a hunk header like `@@ -204,7 +204,7 @@`.
fn parse_hunk_header(h: &str) -> Option<(usize, usize)> {
    let mut parts = h.split_whitespace();
    let _ = parts.next(); // "@@"
    let old = parts.next()?;
    let new = parts.next()?;
    let old_n = old
        .trim_start_matches('-')
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new_n = new
        .trim_start_matches('+')
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old_n, new_n))
}

/// True when the diff is the *creation* of a new/untracked file rather than an
/// edit to an existing one. `git diff --no-index /dev/null <path>` (xa's
/// fallback for untracked files) emits `--- /dev/null`, and a tracked new file
/// emits `new file mode`. In both cases the entire file shows as `+`, which is
/// expected — we just mustn't label it "Edit" (that would imply a full rewrite
/// of an existing file).
fn diff_is_new_file(diff: &str) -> bool {
    diff.contains("--- /dev/null") || diff.contains("new file mode")
}

/// Short summary of how many lines changed, for the collapsed tool header
/// (e.g. "3 changed"). Counts added/removed lines; context lines are ignored.
fn diff_change_summary(diff: &str) -> String {
    let added = diff.lines().filter(|l| l.starts_with('+')).count();
    let removed = diff.lines().filter(|l| l.starts_with('-')).count();
    if diff_is_new_file(diff) {
        format!("{added} new lines")
    } else if removed == 0 {
        format!("{added} added")
    } else {
        format!("{added}↑ / {removed}↓")
    }
}

// ---- Thinking (interleaved: assistant text + tool-call cards) -------------

/// One ordered entry in a thinking block. Interleaving mirrors the real model
/// turn — text, then a tool call, then more text, then another tool call — so
/// the transcript reads naturally instead of putting every tool at the top.
pub enum ThinkBlock {
    Text(String),
    Tool(ToolCallCell),
}

impl ThinkBlock {
    fn as_tool_mut(&mut self) -> Option<&mut ToolCallCell> {
        match self {
            ThinkBlock::Tool(t) => Some(t),
            _ => None,
        }
    }
}

pub struct ThinkingCell {
    pub blocks: Vec<ThinkBlock>,
    pub streaming: bool,
}

impl ThinkingCell {
    pub fn new() -> Self {
        ThinkingCell {
            blocks: Vec::new(),
            streaming: true,
        }
    }

    /// Append streamed text, merging into the previous text block when the
    /// model is still emitting the same paragraph.
    pub fn add_text(&mut self, s: &str) {
        if let Some(ThinkBlock::Text(last)) = self.blocks.last_mut() {
            last.push_str(s);
        } else {
            self.blocks.push(ThinkBlock::Text(s.to_string()));
        }
    }

    /// Add a freshly-started tool-call card.
    pub fn add_tool(
        &mut self,
        name: &str,
        preview: &str,
        path: Option<String>,
        read_offset: Option<usize>,
        read_limit: Option<usize>,
    ) {
        self.blocks.push(ThinkBlock::Tool(ToolCallCell {
            tool_name: name.to_string(),
            args_preview: preview.to_string(),
            status: ToolStatus::Running,
            output: None,
            diff: None,
            expanded: false,
            path,
            read_offset,
            read_limit,
        }));
    }

    /// Mark the most recent still-running tool card as finished.
    pub fn finish_tool(&mut self, output: Option<String>, is_error: bool, diff: Option<String>) {
        for b in self.blocks.iter_mut().rev() {
            if let Some(t) = b.as_tool_mut() {
                if t.status == ToolStatus::Running {
                    t.status = if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Success
                    };
                    t.output = output;
                    t.diff = diff;
                    // Auto-expand on failure, when we have a diff to show, or for
                    // reads (so the `→ Read` header is visible).
                    t.expanded = is_error || t.diff.is_some() || t.tool_name == "read";
                    return;
                }
            }
        }
    }

    /// Concatenated assistant text (for session persistence).
    pub fn answer_text(&self) -> String {
        let mut s = String::new();
        for b in &self.blocks {
            if let ThinkBlock::Text(t) = b {
                s.push_str(t);
            }
        }
        s
    }

    fn build(&self, width: u16, ctx: Option<&RenderContext>) -> Vec<Row> {
        // Waiting / Thinking / Responding live in the activity strip above the
        // input bar (Claude Code style) — not inside the transcript cell.
        if self.blocks.is_empty() {
            return Vec::new();
        }

        let indent = THINK_INDENT;
        let text_w = width.saturating_sub(indent).max(1) as usize;
        let mut rows = vec![Row::blank(width)]; // top padding
        for b in &self.blocks {
            match b {
                ThinkBlock::Tool(t) => rows.extend(t.build(width, ctx)),
                ThinkBlock::Text(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    for line in render_markdown(text, text_w) {
                        rows.push(Row {
                            x: indent,
                            w: width.saturating_sub(indent),
                            line,
                        });
                    }
                }
            }
        }
        // Streaming cursor: append to the last text row so it sits at the end
        // of the answer (not on its own row, which looked like a stray glyph /
        // broken line break after the message).
        if self.streaming {
            let tail_is_tool = matches!(self.blocks.last(), Some(ThinkBlock::Tool(_)));
            if !tail_is_tool {
                let cursor = Span::styled("█", Style::default().fg(theme::ACCENT));
                let mut glued = false;
                if let Some(last) = rows.last_mut() {
                    if last.x == indent && !line_is_blank(&last.line) {
                        last.line.spans.push(cursor.clone());
                        glued = true;
                    }
                }
                if !glued {
                    rows.push(Row {
                        x: indent,
                        w: width.saturating_sub(indent),
                        line: Line::from(cursor),
                    });
                }
            }
        }
        rows.push(Row::blank(width)); // bottom padding
        rows
    }
}

impl HistoryCell for ThinkingCell {
    fn desired_height(&self, width: u16) -> u16 {
        self.build(width, None).len() as u16
    }
    fn render(&self, area: Rect, skip: u16, buf: &mut Buffer, ctx: &RenderContext) {
        let rows = self.build(area.width, Some(ctx));
        paint_rows(&rows, area, skip, buf);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod markdown_layout_tests {
    use super::*;

    fn line_text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn soft_break_joins_mid_sentence() {
        let lines = render_markdown("Hi! How can I help you\ntoday?", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "Hi! How can I help you today?");
    }

    #[test]
    fn soft_break_glues_lone_question_mark() {
        let lines = render_markdown("Hi! How can I help you today\n?", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "Hi! How can I help you today?");
    }

    #[test]
    fn double_newline_orphan_punct_merges() {
        let lines = render_markdown("Hi! How can I help you today\n\n?", 80);
        assert_eq!(
            lines.len(),
            1,
            "got: {:?}",
            lines.iter().map(line_text).collect::<Vec<_>>()
        );
        assert_eq!(line_text(&lines[0]), "Hi! How can I help you today?");
    }

    #[test]
    fn plain_greeting_stays_one_line() {
        let lines = render_markdown(
            "Hi! I'm xa, a coding agent. How can I help you today?",
            80,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(
            line_text(&lines[0]),
            "Hi! I'm xa, a coding agent. How can I help you today?"
        );
    }

    #[test]
    fn paragraph_spacing_kept_once() {
        let lines = render_markdown("Hello.\n\nWorld.", 80);
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "Hello.");
        assert!(line_is_blank(&lines[1]));
        assert_eq!(line_text(&lines[2]), "World.");
    }

    #[test]
    fn thinking_cell_inline_cursor() {
        let mut tc = ThinkingCell::new();
        tc.add_text("Hi! How can I help you today?");
        tc.streaming = true;
        let rows = tc.build(80, None);
        let content: Vec<_> = rows
            .iter()
            .filter(|r| !line_is_blank(&r.line))
            .map(|r| line_text(&r.line))
            .collect();
        assert_eq!(content.len(), 1);
        assert!(
            content[0].ends_with('█'),
            "cursor should be inline, got {:?}",
            content[0]
        );
        assert!(content[0].contains("today?"));
    }
}
