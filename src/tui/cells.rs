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
    highlight::{segments_to_lines, CodeHighlighter, TreeSitterHighlighter},
    markdown::{MarkdownBlock, MarkdownRenderer},
    theme::ThemeConfig,
};
use std::cell::RefCell;
use std::sync::Arc;

use crate::tui::render::RenderContext;
use crate::tui::shimmer::shimmer_spans;
use crate::tui::theme;

/// Cached layout for a history cell at a given terminal width.
///
/// Without this, every scroll/redraw re-parses markdown and re-runs
/// tree-sitter highlighting for *every* cell just to measure height — which
/// made mouse-wheel scroll feel stuck and delayed key handling (e.g. Ctrl-C).
struct LayoutCache {
    width: u16,
    /// Extra key bits (streaming flag, content generation, …).
    fingerprint: u64,
    rows: Vec<Row>,
}

impl LayoutCache {
    fn matches(&self, width: u16, fingerprint: u64) -> bool {
        self.width == width && self.fingerprint == fingerprint
    }
}

/// Return cached rows or rebuild via `build`.
fn cached_rows<'a>(
    cache: &'a RefCell<Option<LayoutCache>>,
    width: u16,
    fingerprint: u64,
    build: impl FnOnce() -> Vec<Row>,
) -> std::cell::Ref<'a, Vec<Row>> {
    {
        let mut slot = cache.borrow_mut();
        let hit = slot
            .as_ref()
            .is_some_and(|c| c.matches(width, fingerprint));
        if !hit {
            *slot = Some(LayoutCache {
                width,
                fingerprint,
                rows: build(),
            });
        }
    }
    std::cell::Ref::map(cache.borrow(), |c| {
        &c.as_ref().expect("layout cache just populated").rows
    })
}

static TS_HIGHLIGHTER: std::sync::LazyLock<Arc<TreeSitterHighlighter>> =
    std::sync::LazyLock::new(|| Arc::new(TreeSitterHighlighter::new()));

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

    fn new(x: u16, w: u16, line: Line<'static>) -> Self {
        Row { x, w, line }
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

/// ratatui-markdown only supports h1–h3; h4+ get misparsed as h3 with leading
/// `#` in the text. Convert h4–h6 to bold so they render distinctly from
/// regular paragraphs without stray hash marks.
fn convert_h456_to_bold(text: &str) -> String {
    text.replace("###### ", "**")
        .replace("##### ", "**")
        .replace("#### ", "**")
}

/// Normalize raw model text before markdown parse.
///
/// - `\r\n` / bare `\r` → `\n`
/// - collapse 3+ consecutive newlines to a double newline (one blank line)
/// - convert h4–h6 headings to bold (library only supports h1–h3)
fn normalize_md_source(text: &str) -> String {
    let mut s = text.replace("\r\n", "\n").replace('\r', "\n");
    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    convert_h456_to_bold(&s)
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

/// Horizontal indent for fenced code bodies (2-space "tab").
const CODE_BLOCK_INDENT: &str = "  ";

/// Custom markdown hook for fenced code. `segments_to_lines` translates the
/// highlighter's whole-block byte offsets into each rendered line correctly;
/// the prior implementation incorrectly reused those offsets on every line.
///
/// Layout: one blank line above, one blank line below, each content line
/// prefixed with [`CODE_BLOCK_INDENT`].
struct XaRenderHooks {
    max_width: usize,
}

/// Drop trailing empty lines from fence body (models often leave a blank
/// before the closing ```).
fn trim_code_fence_body(content: &str) -> &str {
    content.trim_end_matches(['\n', '\r'])
}

impl ratatui_markdown::markdown::RenderHooks for XaRenderHooks {
    fn render_code_block(&self, lang: &str, content: &str) -> Option<Vec<Line<'static>>> {
        let content = trim_code_fence_body(content);
        let segments = TS_HIGHLIGHTER.highlight(lang, content);
        let mut lines: Vec<Line<'static>> = Vec::new();
        // 1 line top pad
        lines.push(Line::default());

        if segments.is_empty() {
            let code_style = Style::default().fg(theme::TEXT_DIM);
            if content.is_empty() {
                lines.push(Line::from(Span::raw(CODE_BLOCK_INDENT.to_string())));
            } else {
                for line in content.split('\n') {
                    lines.push(Line::from(vec![
                        Span::raw(CODE_BLOCK_INDENT.to_string()),
                        Span::styled(line.to_string(), code_style),
                    ]));
                }
            }
        } else {
            lines.extend(segments_to_lines(
                content,
                &segments,
                CODE_BLOCK_INDENT,
                Style::default(),
                self.max_width,
            ));
        }

        // 1 line bottom pad
        lines.push(Line::default());
        Some(lines)
    }
}

/// Parse + render markdown into styled lines at `max_width`, with soft-break
/// joining and blank-line compaction so assistant text doesn't fracture into
/// odd one-word / lone-punctuation rows. Code blocks are syntax-highlighted
/// via tree-sitter when a grammar is available.
fn render_markdown(text: &str, max_width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let src = normalize_md_source(text);
    let hooks = XaRenderHooks {
        max_width: max_width.max(1),
    };
    let mut renderer = MarkdownRenderer::new(max_width.max(1));
    renderer = renderer.with_render_hooks(Box::new(hooks));
    let mut blocks = renderer.parse(&src);
    join_paragraph_soft_breaks(&mut blocks);
    merge_orphan_punct_paragraphs(&mut blocks);
    let mut theme = ThemeConfig::default();
    theme.muted_text_color = Color::Rgb(160, 160, 160);
    let lines = renderer.render(&blocks, &theme);
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
    layout: RefCell<Option<LayoutCache>>,
}

impl SystemCell {
    pub fn new(content: impl Into<String>) -> Self {
        SystemCell {
            content: content.into(),
            layout: RefCell::new(None),
        }
    }

    fn build(&self, width: u16, _ctx: Option<&RenderContext>) -> Vec<Row> {
        let styled = render_markdown(&self.content, width as usize);
        let mut rows: Vec<Row> = styled
            .into_iter()
            .map(|line| Row::new(0, width, line))
            .collect();
        rows.push(Row::blank(width)); // trailing separator
        rows
    }

    fn rows(&self, width: u16) -> std::cell::Ref<'_, Vec<Row>> {
        cached_rows(&self.layout, width, 0, || self.build(width, None))
    }
}

impl HistoryCell for SystemCell {
    fn desired_height(&self, width: u16) -> u16 {
        self.rows(width).len() as u16
    }
    fn render(&self, area: Rect, skip: u16, buf: &mut Buffer, _ctx: &RenderContext) {
        paint_rows(&self.rows(area.width), area, skip, buf);
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
    layout: RefCell<Option<LayoutCache>>,
}

impl UserCell {
    pub fn new(content: impl Into<String>) -> Self {
        UserCell {
            content: content.into(),
            layout: RefCell::new(None),
        }
    }

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
                rows.push(Row::new(
                    left_margin,
                    USER_LEAD_COLS + avail,
                    Line::from(vec![
                        Span::styled(format!("{USER_PROMPT} "), prompt_style),
                        Span::styled(l, text_style),
                    ]),
                ));
            } else {
                rows.push(Row::new(
                    text_x,
                    avail,
                    Line::from(Span::styled(l, text_style)),
                ));
            }
        }
        rows.push(Row::blank(width)); // bottom padding
        rows
    }

    fn rows(&self, width: u16) -> std::cell::Ref<'_, Vec<Row>> {
        cached_rows(&self.layout, width, 0, || self.build(width, None))
    }
}

impl HistoryCell for UserCell {
    fn desired_height(&self, width: u16) -> u16 {
        self.rows(width).len() as u16
    }
    fn render(&self, area: Rect, skip: u16, buf: &mut Buffer, _ctx: &RenderContext) {
        paint_rows(&self.rows(area.width), area, skip, buf);
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
    /// OpenAI-style tool call id (e.g. `call_abc123`), needed to rebuild the
    /// agent history on session resume.
    pub tool_call_id: Option<String>,
    /// Full JSON arguments string (the preview is truncated for display).
    pub arguments: Option<String>,
}

impl ToolCallCell {
    pub fn header_line(&self, ctx: Option<&RenderContext>) -> Line<'static> {
        let (icon, color) = match self.status {
            ToolStatus::Running => ("▸", theme::ACCENT),
            ToolStatus::Success => ("▪", theme::TEXT),
            ToolStatus::Failed => ("▪", theme::ERROR),
        };
        
        let mut spans = vec![Span::styled(
            format!("{} ", icon),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )];
        
        // Capitalize tool name and make it bold white
        let tool_name_cap = if self.tool_name.len() > 0 {
            let mut chars = self.tool_name.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        } else {
            self.tool_name.clone()
        };
        
        spans.push(Span::styled(
            format!("{}(", tool_name_cap),
            Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD),
        ));
        
        // For edit/write tools, show the file path; otherwise show args preview
        let is_edit_write = self.tool_name == "edit" || self.tool_name == "write";
        let display_text = if is_edit_write {
            self.path.as_deref().unwrap_or(&self.args_preview).to_string()
        } else {
            self.args_preview.clone()
        };
        
        let max_display_len = 60;
        let args_display = if display_text.len() > max_display_len {
            let trimmed = &display_text[..max_display_len];
            format!("{}...)", trimmed)
        } else {
            format!("{})", display_text)
        };
        
        let args_style = if display_text.len() > max_display_len {
            Style::default().fg(theme::TEXT_HINT) // Even dimmer for trimmed
        } else {
            Style::default().fg(theme::TEXT_DIM) // Dimmer white for normal args
        };
        
        match ctx {
            Some(c) if self.status == ToolStatus::Running => {
                let mut s = shimmer_spans(&args_display, color, c.shimmer_phase);
                spans.append(&mut s);
            }
            _ => spans.push(Span::styled(args_display, args_style)),
        }
        
        Line::from(spans)
    }

    fn build(&self, width: u16, ctx: Option<&RenderContext>) -> Vec<Row> {
        let mut rows = vec![Row::new(
            THINK_INDENT,
            width.saturating_sub(THINK_INDENT),
            self.header_line(ctx),
        )];
        
        // Show output/summary below the header with tree prefix
        let x = THINK_INDENT + 3; // Indent for tree branch
        let w = width.saturating_sub(THINK_INDENT + 3);
        
        match self.tool_name.as_str() {
            "read" => {
                // Show "Read N lines" summary
                if let Some(_p) = self.path.as_deref() {
                    let line_count = if let Some(output) = &self.output {
                        output.lines().count()
                    } else {
                        0
                    };
                    rows.push(Row::new(
                        x,
                        w,
                        Line::from(vec![
                            Span::styled("└ ", Style::default().fg(theme::TEXT_DIM)),
                            Span::styled(
                                format!("Read {} lines", line_count),
                                Style::default().fg(theme::TEXT_DIM),
                            ),
                        ]),
                    ));
                }
            }
            "edit" | "write" => {
                // Show git diff for edits
                if let Some(diff) = self.diff.as_deref() {
                    let label = if diff_is_new_file(diff) {
                        "New file"
                    } else {
                        "Edit"
                    };
                    let lang = self.path.as_deref().and_then(lang_from_path);
                    let mut shown = build_diff_rows(diff, x, w, label, lang);
                    let limit = 10;
                    if shown.len() > limit {
                        shown.truncate(limit);
                        rows.push(Row::new(
                            x,
                            w,
                            Line::from(vec![
                                Span::styled("  ", Style::default().fg(theme::TEXT_DIM)),
                                Span::styled(
                                    format!("… +{} lines", shown.len() - limit),
                                    Style::default().fg(theme::TEXT_HINT),
                                ),
                            ]),
                        ));
                    }
                    rows.append(&mut shown);
                }
            }
            _ => {
                // For bash/grep/other tools, show output with tree prefix
                if let Some(out) = self.output.as_deref() {
                    let lines: Vec<&str> = out.lines().collect();
                    let max_lines = 3;
                    let show_lines = if lines.len() > max_lines {
                        &lines[..max_lines]
                    } else {
                        &lines
                    };
                    
                    for (i, line) in show_lines.iter().enumerate() {
                        let prefix = if i == 0 { "└ " } else { "  " };
                        rows.push(Row::new(
                            x,
                            w,
                            Line::from(vec![
                                Span::styled(prefix, Style::default().fg(theme::TEXT_DIM)),
                                Span::styled(
                                    line.to_string(),
                                    Style::default().fg(theme::TEXT_DIM),
                                ),
                            ]),
                        ));
                    }
                    
                    if lines.len() > max_lines {
                        rows.push(Row::new(
                            x,
                            w,
                            Line::from(vec![
                                Span::styled("  ", Style::default().fg(theme::TEXT_DIM)),
                                Span::styled(
                                    format!("… +{} lines", lines.len() - max_lines),
                                    Style::default().fg(theme::TEXT_HINT),
                                ),
                            ]),
                        ));
                    }
                } else if let Some(diff) = self.diff.as_deref() {
                    // For edits with diff, show change summary
                    let summary = diff_change_summary(diff);
                    rows.push(Row::new(
                        x,
                        w,
                        Line::from(vec![
                            Span::styled("└ ", Style::default().fg(theme::TEXT_DIM)),
                            Span::styled(
                                summary,
                                Style::default().fg(theme::TEXT_DIM),
                            ),
                        ]),
                    ));
                }
            }
        }
        
        rows
    }
}

/// Parse a unified `git diff` into a compact, line-numbered edit view:
/// each changed line is shown with its line number and a colored background
/// (green for additions, red for deletions) — no `+`/`-` prefix characters.
/// Code content is syntax-highlighted via tree-sitter when the language is known.
fn build_diff_rows(diff: &str, x: u16, w: u16, _header_label: &str, lang: Option<&str>) -> Vec<Row> {
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
            || raw.starts_with("+++ ")
        {
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
        let (fg, bg) = match sign {
            "-" => (theme::DIFF_DEL, theme::DIFF_DEL_BG),
            "+" => (theme::DIFF_ADD, theme::DIFF_ADD_BG),
            _ => (theme::DIFF_META, Color::Reset),
        };
        let ln_style = if sign == " " || sign == "" {
            Style::default().fg(theme::TEXT_HINT)
        } else {
            Style::default().fg(fg).bg(bg)
        };
        let ln_width = 5;
        let content_w = (w as usize).saturating_sub(ln_width);

        let mut line_spans = vec![Span::styled(format!("{:>4} ", ln), ln_style)];

        if let Some(lg) = lang {
            let bg_ov = if sign == "-" || sign == "+" { Some(bg) } else { None };
            let mut hl_spans = highlight_code_spans(content, lg, bg_ov, content_w);
            let visible_w: usize = hl_spans.iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            if visible_w < content_w {
                let pad_style = if sign == "-" || sign == "+" {
                    Style::default().bg(bg)
                } else {
                    Style::default()
                };
                hl_spans.push(Span::styled(" ".repeat(content_w - visible_w), pad_style));
            }
            line_spans.extend(hl_spans);
        } else {
            let content_style = if sign == " " || sign == "" {
                Style::default().fg(theme::DIFF_META)
            } else {
                Style::default().fg(fg).bg(bg)
            };
            let padded = if content.len() < content_w {
                format!("{:<width$}", content, width = content_w)
            } else {
                content.to_string()
            };
            line_spans.push(Span::styled(padded, content_style));
        };

        rows.push(Row::new(
            x,
            w,
            Line::from(line_spans),
        ));
    }
    rows
}

/// Map a file path to a tree-sitter language name for syntax highlighting.
fn lang_from_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "sh" | "bash" | "zsh" => "bash",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "c" | "h" => "c",
        "cpp" | "cxx" | "cc" | "hpp" => "cpp",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "sql" => "sql",
        "rb" => "ruby",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

/// Syntax-highlight a single line of code, returning styled spans.
/// If `bg_override` is set, it replaces the background on every span (used for
/// diff +/- lines that carry a green/red background).
fn highlight_code_spans(code: &str, lang: &str, bg_override: Option<Color>, _max_w: usize) -> Vec<Span<'static>> {
    let segments = TS_HIGHLIGHTER.highlight(lang, code);
    if segments.is_empty() {
        let style = if let Some(bg) = bg_override {
            Style::default().fg(theme::DIFF_META).bg(bg)
        } else {
            Style::default().fg(theme::DIFF_META)
        };
        return vec![Span::styled(code.to_string(), style)];
    }
    let mut spans = Vec::new();
    for seg in &segments {
        let text: String = code.chars().skip(seg.start).take(seg.end - seg.start).collect();
        if text.is_empty() {
            continue;
        }
        let mut style = seg.style;
        if let Some(bg) = bg_override {
            style = style.bg(bg);
        }
        spans.push(Span::styled(text, style));
    }
    if spans.is_empty() {
        let style = if let Some(bg) = bg_override {
            Style::default().fg(theme::DIFF_META).bg(bg)
        } else {
            Style::default().fg(theme::DIFF_META)
        };
        spans.push(Span::styled(code.to_string(), style));
    }
    spans
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
    /// Bumped on content mutations so the layout cache invalidates.
    layout_gen: u64,
    layout: RefCell<Option<LayoutCache>>,
}

impl ThinkingCell {
    pub fn new() -> Self {
        ThinkingCell {
            blocks: Vec::new(),
            streaming: true,
            layout_gen: 0,
            layout: RefCell::new(None),
        }
    }

    fn bump_layout(&mut self) {
        self.layout_gen = self.layout_gen.wrapping_add(1);
        *self.layout.borrow_mut() = None;
    }

    fn fingerprint(&self) -> u64 {
        // Include streaming so the cursor row appears/disappears correctly
        // even when external code toggles the flag without bump_layout.
        self.layout_gen
            .wrapping_mul(2)
            .wrapping_add(u64::from(self.streaming))
    }

    /// True when a tool header still needs the shimmer animation.
    fn has_running_tool(&self) -> bool {
        self.blocks.iter().any(|b| {
            matches!(
                b,
                ThinkBlock::Tool(t) if t.status == ToolStatus::Running
            )
        })
    }

    /// Append streamed text, merging into the previous text block when the
    /// model is still emitting the same paragraph.
    pub fn add_text(&mut self, s: &str) {
        if let Some(ThinkBlock::Text(last)) = self.blocks.last_mut() {
            last.push_str(s);
        } else {
            self.blocks.push(ThinkBlock::Text(s.to_string()));
        }
        self.bump_layout();
    }

    /// Add a freshly-started tool-call card.
    pub fn add_tool(
        &mut self,
        name: &str,
        preview: &str,
        path: Option<String>,
        read_offset: Option<usize>,
        read_limit: Option<usize>,
        tool_call_id: Option<String>,
        arguments: Option<String>,
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
            tool_call_id,
            arguments,
        }));
        self.bump_layout();
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
                    self.bump_layout();
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

    /// References to every tool-call card in order (for session persistence).
    pub fn tool_blocks(&self) -> Vec<&ToolCallCell> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                ThinkBlock::Tool(t) => Some(t),
                _ => None,
            })
            .collect()
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
                    let md_lines = render_markdown(text, text_w);
                    for line in md_lines {
                        rows.push(Row::new(indent, width.saturating_sub(indent), line));
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
                    rows.push(Row::new(
                        indent,
                        width.saturating_sub(indent),
                        Line::from(cursor),
                    ));
                }
            }
        }
        rows.push(Row::blank(width)); // bottom padding, matching the top row
        rows
    }

    fn rows(&self, width: u16, ctx: Option<&RenderContext>) -> std::cell::Ref<'_, Vec<Row>> {
        // Running tool headers shimmer every frame — skip the cache so the
        // animation keeps moving. Idle/finished cells hit the cache hard on
        // scroll, which is the expensive path we care about.
        let live = self.has_running_tool() && ctx.is_some();
        if live {
            // Force rebuild into the cache slot so subsequent height queries
            // in the same frame still benefit.
            *self.layout.borrow_mut() = None;
            return cached_rows(&self.layout, width, self.fingerprint(), || {
                self.build(width, ctx)
            });
        }
        cached_rows(&self.layout, width, self.fingerprint(), || {
            self.build(width, None)
        })
    }
}

impl HistoryCell for ThinkingCell {
    fn desired_height(&self, width: u16) -> u16 {
        self.rows(width, None).len() as u16
    }
    fn render(&self, area: Rect, skip: u16, buf: &mut Buffer, ctx: &RenderContext) {
        paint_rows(&self.rows(area.width, Some(ctx)), area, skip, buf);
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
    fn fenced_code_is_plain_and_indented() {
        // Standalone fence: compact_md_lines drops leading/trailing pads of the
        // whole output, but content still gets the 2-space indent.
        let lines = render_markdown(
            "```bash\npython -m magnus.training.train_sarm --resume /tmp/checkpoint.pt\n```",
            100,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(
            line_text(&lines[0]),
            "  python -m magnus.training.train_sarm --resume /tmp/checkpoint.pt"
        );
    }

    #[test]
    fn fenced_code_has_pad_between_paragraphs() {
        let lines = render_markdown("Before.\n\n```text\ncode line\n```\n\nAfter.", 100);
        let texts: Vec<_> = lines.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec![
                "Before.".to_string(),
                String::new(),
                "  code line".to_string(),
                String::new(),
                "After.".to_string(),
            ],
            "got: {texts:?}"
        );
    }

    #[test]
    fn trailing_blank_code_lines_are_removed() {
        let lines = render_markdown(
            "```text\nUsage: `python -m magnus.training.train_sarm`\n\n\n```",
            100,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(
            line_text(&lines[0]),
            "  Usage: `python -m magnus.training.train_sarm`"
        );
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

    #[test]
    fn thinking_cell_has_matching_top_and_bottom_padding() {
        let mut tc = ThinkingCell::new();
        tc.add_text("Done.");
        tc.streaming = false;
        let rows = tc.build(80, None);
        assert!(line_is_blank(&rows[0].line));
        assert!(line_is_blank(&rows[rows.len() - 1].line));
    }
}
