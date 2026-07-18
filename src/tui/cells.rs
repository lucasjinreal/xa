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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use ratatui_markdown::{
    highlight::{segments_to_lines, CodeHighlighter, TreeSitterHighlighter},
    markdown::{MarkdownBlock, MarkdownRenderer},
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
    std::sync::LazyLock::new(|| {
        Arc::new(
            TreeSitterHighlighter::new().with_code_colors(theme::code_syntax_colors()),
        )
    });

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
/// Code-block rows (marked with the theme code-block background) are never
/// treated as collapsible blanks — they form a solid code background bar.
fn compact_md_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut prev_blank = true; // suppress leading blanks
    for line in lines {
        if is_code_line(&line) {
            out.push(line);
            prev_blank = false;
            continue;
        }
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
    while out.last().is_some_and(|l| line_is_blank(l) && !is_code_line(l)) {
        out.pop();
    }
    // Drop leading non-code blanks only.
    while out
        .first()
        .is_some_and(|l| line_is_blank(l) && !is_code_line(l))
    {
        out.remove(0);
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

/// Fenced code background geometry: a two-column left margin, a four-column
/// right margin, and no inner left padding.
const CODE_BLOCK_LEFT_MARGIN: u16 = 2;
const CODE_BLOCK_RIGHT_MARGIN: u16 = 4;

/// Custom markdown hook for fenced code + content-sized tables.
///
/// Code layout (Grok/Codex minimal):
/// - vertical padding = 0 (no empty bg rows inside the block)
/// - background starts after a 2-column left margin
/// - left padding = 0; background ends before a 4-column right margin
/// - no border / title / footer
/// - surrounding top/bottom margin of 1 blank line is applied when the AI
///   cell flushes the fence into the transcript
///
/// Tables size to their content instead of stretching to the terminal width;
/// columns only shrink (and wrap) when the natural width exceeds `max_width`.
struct XaRenderHooks {
    max_width: usize,
}

// Box-drawing chars for markdown tables (same set as ratatui-markdown).
const T_HLINE: &str = "─";
const T_VLINE: &str = "│";
const T_TL: &str = "┌";
const T_TR: &str = "┐";
const T_BL: &str = "└";
const T_BR: &str = "┘";
const T_TM: &str = "┬";
const T_BM: &str = "┴";
const T_ML: &str = "├";
const T_MR: &str = "┤";
const T_X: &str = "┼";
/// Spaces around cell text inside a column (`│ text │` → 1 each side).
const TABLE_CELL_PAD: usize = 1;

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Word-wrap plain text to `max_w` display columns. Empty → one empty line.
fn wrap_cell_text(text: &str, max_w: usize) -> Vec<String> {
    if max_w == 0 {
        return vec![String::new()];
    }
    let text = text.trim();
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;

    for word in text.split_whitespace() {
        let ww = display_width(word);
        if cur.is_empty() {
            if ww <= max_w {
                cur = word.to_string();
                cur_w = ww;
            } else {
                // Hard-break overlong tokens.
                let mut chunk = String::new();
                let mut cw = 0usize;
                for ch in word.chars() {
                    let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if cw + ch_w > max_w && !chunk.is_empty() {
                        lines.push(std::mem::take(&mut chunk));
                        cw = 0;
                    }
                    chunk.push(ch);
                    cw += ch_w;
                }
                cur = chunk;
                cur_w = cw;
            }
            continue;
        }
        if cur_w + 1 + ww <= max_w {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        } else {
            lines.push(std::mem::take(&mut cur));
            if ww <= max_w {
                cur = word.to_string();
                cur_w = ww;
            } else {
                let mut chunk = String::new();
                let mut cw = 0usize;
                for ch in word.chars() {
                    let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if cw + ch_w > max_w && !chunk.is_empty() {
                        lines.push(std::mem::take(&mut chunk));
                        cw = 0;
                    }
                    chunk.push(ch);
                    cw += ch_w;
                }
                cur = chunk;
                cur_w = cw;
            }
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

fn table_hline(col_widths: &[usize], left: &str, mid: &str, right: &str) -> String {
    let mut s = String::from(left);
    for (i, w) in col_widths.iter().enumerate() {
        if i > 0 {
            s.push_str(mid);
        }
        s.push_str(&T_HLINE.repeat(*w));
    }
    s.push_str(right);
    s
}

/// Content-sized markdown table: columns hug content; only shrink when the
/// natural width would exceed `max_width`.
fn render_content_sized_table(
    headers: &[String],
    rows: &[Vec<String>],
    max_width: usize,
) -> Vec<Line<'static>> {
    let col_count = headers
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if col_count == 0 {
        return Vec::new();
    }

    let pad2 = TABLE_CELL_PAD * 2;
    // Natural content width per column (no forced expansion).
    let mut natural: Vec<usize> = (0..col_count)
        .map(|c| {
            let hw = headers
                .get(c)
                .map(|h| display_width(h.trim()))
                .unwrap_or(0);
            let rw = rows
                .iter()
                .filter_map(|r| r.get(c))
                .map(|cell| display_width(cell.trim()))
                .max()
                .unwrap_or(0);
            hw.max(rw).max(1)
        })
        .collect();

    // Column outer widths include left+right cell padding.
    let mut col_widths: Vec<usize> = natural.iter().map(|n| n + pad2).collect();

    // Borders: one VLINE between cols + left/right → col_count + 1 chars.
    let border_overhead = col_count + 1;
    let natural_total: usize = col_widths.iter().sum::<usize>() + border_overhead;
    let available = max_width.max(border_overhead + col_count * (pad2 + 1));

    // Only shrink when table is wider than the viewport — never grow to fill it.
    if natural_total > available {
        let content_budget = available.saturating_sub(border_overhead + col_count * pad2);
        let natural_sum: usize = natural.iter().sum::<usize>().max(1);
        let mut allocated: Vec<usize> = natural
            .iter()
            .map(|n| ((*n as u64 * content_budget as u64) / natural_sum as u64).max(1) as usize)
            .collect();
        // Fix rounding so we don't exceed budget.
        let mut used: usize = allocated.iter().sum();
        while used > content_budget {
            if let Some((i, _)) = allocated
                .iter()
                .enumerate()
                .filter(|(_, w)| **w > 1)
                .max_by_key(|(_, w)| *w)
            {
                allocated[i] -= 1;
                used -= 1;
            } else {
                break;
            }
        }
        while used < content_budget {
            if let Some((i, _)) = natural
                .iter()
                .enumerate()
                .filter(|(i, n)| allocated[*i] < **n)
                .max_by_key(|(_, n)| *n)
            {
                allocated[i] += 1;
                used += 1;
            } else {
                break;
            }
        }
        natural = allocated;
        col_widths = natural.iter().map(|n| n + pad2).collect();
    }

    let border = Style::default().fg(theme::t().md_muted);
    let header_style = Style::default()
        .fg(theme::t().md_text)
        .add_modifier(Modifier::BOLD);
    let cell_style = Style::default().fg(theme::t().md_text);

    // Pre-wrap every cell to its content width.
    let header_wrapped: Vec<Vec<String>> = (0..col_count)
        .map(|c| {
            let text = headers.get(c).map(|s| s.as_str()).unwrap_or("");
            wrap_cell_text(text, natural[c])
        })
        .collect();
    let rows_wrapped: Vec<Vec<Vec<String>>> = rows
        .iter()
        .map(|row| {
            (0..col_count)
                .map(|c| {
                    let text = row.get(c).map(|s| s.as_str()).unwrap_or("");
                    wrap_cell_text(text, natural[c])
                })
                .collect()
        })
        .collect();

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        table_hline(&col_widths, T_TL, T_TM, T_TR),
        border,
    )));

    let push_row = |lines: &mut Vec<Line<'static>>,
                    cells: &[Vec<String>],
                    style: Style| {
        let height = cells.iter().map(|c| c.len().max(1)).max().unwrap_or(1);
        for li in 0..height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::styled(T_VLINE.to_string(), border));
            for c in 0..col_count {
                let text = cells
                    .get(c)
                    .and_then(|lines| lines.get(li))
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let tw = display_width(text);
                let inner = natural[c];
                let pad_right = inner.saturating_sub(tw);
                let mut cell = String::new();
                cell.push_str(&" ".repeat(TABLE_CELL_PAD));
                cell.push_str(text);
                cell.push_str(&" ".repeat(pad_right + TABLE_CELL_PAD));
                // Guard: ensure we fill the column outer width.
                let target = col_widths[c];
                let cw = display_width(&cell);
                if cw < target {
                    cell.push_str(&" ".repeat(target - cw));
                }
                spans.push(Span::styled(cell, style));
                spans.push(Span::styled(T_VLINE.to_string(), border));
            }
            lines.push(Line::from(spans));
        }
    };

    push_row(&mut lines, &header_wrapped, header_style);
    lines.push(Line::from(Span::styled(
        table_hline(&col_widths, T_ML, T_X, T_MR),
        border,
    )));

    for (ri, cells) in rows_wrapped.iter().enumerate() {
        push_row(&mut lines, cells, cell_style);
        let is_last = ri + 1 == rows_wrapped.len();
        if is_last {
            lines.push(Line::from(Span::styled(
                table_hline(&col_widths, T_BL, T_BM, T_BR),
                border,
            )));
        } else {
            lines.push(Line::from(Span::styled(
                table_hline(&col_widths, T_ML, T_X, T_MR),
                border,
            )));
        }
    }

    // Header-only table (no data rows): still close with bottom border.
    if rows_wrapped.is_empty() {
        lines.push(Line::from(Span::styled(
            table_hline(&col_widths, T_BL, T_BM, T_BR),
            border,
        )));
    }

    lines
}

/// Drop trailing empty lines from fence body (models often leave a blank
/// before the closing ```).
fn trim_code_fence_body(content: &str) -> &str {
    content.trim_end_matches(['\n', '\r'])
}

fn code_bg_style() -> Style {
    Style::default().fg(theme::t().code_text).bg(theme::t().code_bg)
}

/// True when a foreground is pure white (or unset → terminal white).
fn is_harsh_white_fg(fg: Option<Color>) -> bool {
    match fg {
        None => true,
        Some(Color::White) | Some(Color::Reset) => true,
        Some(Color::Rgb(r, g, b)) if r >= 240 && g >= 240 && b >= 240 => true,
        _ => false,
    }
}

/// Ensure code spans never paint harsh white; plain tokens use soft `CODE_TEXT`.
fn paint_code_span(style: Style) -> Style {
    let with_fg = if is_harsh_white_fg(style.fg) {
        style.fg(theme::t().code_text)
    } else {
        style
    };
    with_fg.bg(theme::t().code_bg)
}

/// Mark every span with the code-block background so ThinkingCell can detect
/// and expand the bar to full terminal width.
fn apply_code_bg(mut line: Line<'static>) -> Line<'static> {
    for span in &mut line.spans {
        span.style = paint_code_span(span.style);
    }
    if line.spans.is_empty() {
        // Keep a single space so the row is still classified as a code bar
        // (and can be padded to full width later).
        line.spans
            .push(Span::styled(" ", code_bg_style()));
    }
    line
}

fn is_code_line(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .any(|s| s.style.bg == Some(theme::t().code_bg))
}

/// Code body row: code spans + trailing background fill, between two-column
/// outer margins. No extra vertical padding rows.
fn code_body_row(line: Line<'static>, width: u16) -> Row {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for mut span in line.spans {
        // Keep indent/spaces that are part of the source line; only drop fully
        // empty markers (no content at all).
        if span.content.is_empty() {
            continue;
        }
        span.style = paint_code_span(span.style);
        spans.push(span);
    }
    if spans.is_empty() {
        spans.push(Span::styled(" ", code_bg_style()));
    }
    let used: usize = spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let w = width
        .saturating_sub(CODE_BLOCK_LEFT_MARGIN + CODE_BLOCK_RIGHT_MARGIN)
        .max(1) as usize;
    if used < w {
        spans.push(Span::styled(" ".repeat(w - used), code_bg_style()));
    }
    Row::new(CODE_BLOCK_LEFT_MARGIN, w as u16, Line::from(spans))
}

/// Emit a fenced code block body (no internal vertical padding). Surrounding
/// 1-line margins are plain blanks (not code-bg), injected only when needed.
fn push_code_block_rows(code_lines: Vec<Line<'static>>, width: u16, rows: &mut Vec<Row>) {
    let body: Vec<Line<'static>> = code_lines
        .into_iter()
        .filter(|l| !line_is_blank(l))
        .collect();
    if body.is_empty() {
        return;
    }

    // Top margin = 1 blank line between previous content and the code bar.
    if rows
        .last()
        .is_some_and(|r| !line_is_blank(&r.line))
    {
        rows.push(Row::blank(width));
    }
    for line in body {
        rows.push(code_body_row(line, width));
    }
    // Bottom margin = 1 blank line after the code bar.
    rows.push(Row::blank(width));
}

fn styled_heading(text: &str, color: Color, mods: Modifier) -> Line<'static> {
    Line::from(Span::styled(
        text.replace('\t', "    "),
        Style::default().fg(color).add_modifier(mods),
    ))
}

/// Inline `` `code` `` is foreground-only and bold. Fenced code keeps its
/// full-row background; links may share the same color but stay underlined.
fn style_inline_code_spans(lines: &mut [Line<'static>]) {
    let cyan = theme::t().md_inline_code;
    for line in lines.iter_mut() {
        if is_code_line(line) {
            continue;
        }
        for span in &mut line.spans {
            if span.style.fg == Some(cyan)
                && !span.style.add_modifier.contains(Modifier::UNDERLINED)
            {
                span.style = span.style.add_modifier(Modifier::BOLD);
                span.style.bg = None;
            }
        }
    }
}

impl ratatui_markdown::markdown::RenderHooks for XaRenderHooks {
    fn heading1(&self, text: &str) -> Option<Line<'static>> {
        Some(styled_heading(text, theme::t().md_heading1, Modifier::BOLD))
    }

    fn heading2(&self, text: &str) -> Option<Line<'static>> {
        Some(styled_heading(text, theme::t().md_heading2, Modifier::BOLD))
    }

    fn heading3(&self, text: &str) -> Option<Line<'static>> {
        Some(styled_heading(text, theme::t().md_heading3, Modifier::BOLD))
    }

    fn table(&self, headers: &[String], rows: &[Vec<String>]) -> Option<Vec<Line<'static>>> {
        Some(render_content_sized_table(headers, rows, self.max_width))
    }

    /// Standalone `` `code` `` blocks (rare) — cyan + bold, no backticks.
    fn inline_code(&self, code: &str) -> Option<Line<'static>> {
        let style = Style::default()
            .fg(theme::t().md_inline_code)
            .add_modifier(Modifier::BOLD);
        Some(Line::from(Span::styled(code.replace('\t', "    "), style)))
    }

    /// No language title / chrome on fenced blocks.
    fn code_block_header(&self, _lang: &str) -> Option<Line<'static>> {
        Some(Line::default())
    }

    fn code_block_footer(&self, _lang: &str, _content_line_count: usize) -> Option<Line<'static>> {
        Some(Line::default())
    }

    fn render_code_block(&self, lang: &str, content: &str) -> Option<Vec<Line<'static>>> {
        let content = trim_code_fence_body(content);
        let segments = TS_HIGHLIGHTER.highlight(lang, content);
        let mut lines: Vec<Line<'static>> = Vec::new();
        // Body only — vertical padding = 0; margin applied at flush time.

        if segments.is_empty() {
            let code_style = code_bg_style();
            if content.is_empty() {
                lines.push(apply_code_bg(Line::from(Span::styled(" ", code_style))));
            } else {
                for line in content.split('\n') {
                    lines.push(apply_code_bg(Line::from(Span::styled(
                        line.to_string(),
                        code_style,
                    ))));
                }
            }
        } else {
            let hl = segments_to_lines(
                content,
                &segments,
                "",
                code_bg_style(),
                self.max_width,
            );
            for line in hl {
                lines.push(apply_code_bg(line));
            }
        }

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
    let theme = theme::markdown_theme();
    let mut lines = compact_md_lines(renderer.render(&blocks, &theme));
    style_inline_code_spans(&mut lines);
    lines
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
            .fg(theme::t().user_lead)
            .bg(theme::t().user_bg)
            .add_modifier(Modifier::BOLD);
        let text_style = Style::default().fg(theme::t().text).bg(theme::t().user_bg);

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
        Some(theme::t().user_bg)
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
/// Right-side breathing room for AI markdown text (not used by full-width
/// code bars, which paint edge-to-edge).
const THINK_RIGHT_PAD: u16 = 1;
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
    /// Text shown inside the tool header parentheses, e.g. `Read(path:10-50)`.
    fn header_arg_text(&self) -> String {
        match self.tool_name.as_str() {
            "edit" | "write" => self
                .path
                .as_deref()
                .unwrap_or(self.args_preview.as_str())
                .to_string(),
            "read" => {
                let path = self
                    .path
                    .as_deref()
                    .unwrap_or(self.args_preview.as_str());
                match (self.read_offset, self.read_limit) {
                    (None, None) => path.to_string(),
                    // offset is 1-based start line; limit is max lines to read.
                    (Some(off), None) => format!("{path}:{off}-"),
                    (None, Some(lim)) => format!("{path}:1-{lim}"),
                    (Some(off), Some(lim)) => {
                        let end = off.saturating_add(lim).saturating_sub(1);
                        format!("{path}:{off}-{end}")
                    }
                }
            }
            _ => self.args_preview.clone(),
        }
    }

    pub fn header_line(&self, ctx: Option<&RenderContext>) -> Line<'static> {
        let (icon, color) = match self.status {
            ToolStatus::Running => ("▸", theme::t().accent),
            ToolStatus::Success => ("▪", theme::t().text),
            ToolStatus::Failed => ("▪", theme::t().error),
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
            Style::default().fg(theme::t().text).add_modifier(Modifier::BOLD),
        ));
        
        // edit/write: file path. read: path + line window. else: first-arg preview.
        let display_text = self.header_arg_text();
        
        let max_display_len = 60;
        let truncated = display_text.chars().count() > max_display_len;
        let args_display = if truncated {
            let trimmed: String = display_text.chars().take(max_display_len).collect();
            format!("{trimmed}...)")
        } else {
            format!("{display_text})")
        };
        
        let args_style = if truncated {
            Style::default().fg(theme::t().text_hint) // Even dimmer for trimmed
        } else {
            Style::default().fg(theme::t().text_dim) // Dimmer white for normal args
        };
        
        match ctx {
            Some(c) if self.status == ToolStatus::Running => {
                let mut s = shimmer_spans(&args_display, color, c.shimmer_phase);
                spans.append(&mut s);
            }
            _ => spans.push(Span::styled(args_display, args_style)),
        }

        if matches!(self.tool_name.as_str(), "edit" | "write") {
            if let Some(diff) = self.diff.as_deref() {
                let (added, removed) = diff_change_counts(diff);
                spans.push(Span::styled(
                    format!(" (+{added} -{removed})"),
                    Style::default().fg(theme::t().text_hint),
                ));
            }
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
                            Span::styled("└ ", Style::default().fg(theme::t().text_dim)),
                            Span::styled(
                                format!("Read {} lines", line_count),
                                Style::default().fg(theme::t().text_dim),
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
                    const MAX_DIFF_ROWS: usize = 200;
                    const DIFF_HEAD_ROWS: usize = MAX_DIFF_ROWS / 2;
                    if shown.len() > MAX_DIFF_ROWS {
                        let omitted = shown.len() - MAX_DIFF_ROWS;
                        let mut tail = shown.split_off(shown.len() - DIFF_HEAD_ROWS);
                        shown.truncate(DIFF_HEAD_ROWS);
                        rows.append(&mut shown);
                        rows.push(Row::new(
                            x,
                            w,
                            Line::from(vec![
                                Span::styled("  ", Style::default().fg(theme::t().text_dim)),
                                Span::styled(
                                    format!("… {omitted} diff lines omitted …"),
                                    Style::default().fg(theme::t().text_hint),
                                ),
                            ]),
                        ));
                        rows.append(&mut tail);
                    } else {
                        rows.append(&mut shown);
                    }
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
                                Span::styled(prefix, Style::default().fg(theme::t().text_dim)),
                                Span::styled(
                                    line.to_string(),
                                    Style::default().fg(theme::t().text_dim),
                                ),
                            ]),
                        ));
                    }
                    
                    if lines.len() > max_lines {
                        rows.push(Row::new(
                            x,
                            w,
                            Line::from(vec![
                                Span::styled("  ", Style::default().fg(theme::t().text_dim)),
                                Span::styled(
                                    format!("… +{} lines", lines.len() - max_lines),
                                    Style::default().fg(theme::t().text_hint),
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
                            Span::styled("└ ", Style::default().fg(theme::t().text_dim)),
                            Span::styled(
                                summary,
                                Style::default().fg(theme::t().text_dim),
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
            "-" => (theme::t().diff_del, theme::t().diff_del_bg),
            "+" => (theme::t().diff_add, theme::t().diff_add_bg),
            _ => (theme::t().diff_meta, Color::Reset),
        };
        let ln_style = if sign == " " || sign == "" {
            Style::default().fg(theme::t().text_hint)
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
                Style::default().fg(theme::t().diff_meta)
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
            Style::default().fg(theme::t().code_text).bg(bg)
        } else {
            Style::default().fg(theme::t().code_text)
        };
        return vec![Span::styled(code.to_string(), style)];
    }
    let mut spans = Vec::new();
    for seg in &segments {
        // tree-sitter returns byte offsets; use bytes for slicing.
        let end = seg.end.min(code.len());
        let start = seg.start.min(end);
        let text = code[start..end].to_string();
        if text.is_empty() {
            continue;
        }
        let mut style = if is_harsh_white_fg(seg.style.fg) {
            seg.style.fg(theme::t().code_text)
        } else {
            seg.style
        };
        if let Some(bg) = bg_override {
            style = style.bg(bg);
        }
        spans.push(Span::styled(text, style));
    }
    if spans.is_empty() {
        let style = if let Some(bg) = bg_override {
            Style::default().fg(theme::t().diff_meta).bg(bg)
        } else {
            Style::default().fg(theme::t().diff_meta)
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

/// Count actual changed lines, excluding unified-diff file headers.
fn diff_change_counts(diff: &str) -> (usize, usize) {
    let added = diff
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count();
    let removed = diff
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count();
    (added, removed)
}

/// Short summary of how many lines changed, for collapsed tool output.
fn diff_change_summary(diff: &str) -> String {
    let (added, removed) = diff_change_counts(diff);
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
        // Left indent + 1-col right pad for normal markdown text.
        let text_w = width
            .saturating_sub(indent.saturating_add(THINK_RIGHT_PAD))
            .max(1) as usize;
        let text_row_w = width.saturating_sub(indent.saturating_add(THINK_RIGHT_PAD));
        let mut rows = vec![Row::blank(width)]; // top padding
        for b in &self.blocks {
            match b {
                ThinkBlock::Tool(t) => rows.extend(t.build(width, ctx)),
                ThinkBlock::Text(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    let md_lines = render_markdown(text, text_w);
                    // Group consecutive code lines so each fence becomes one
                    // code background bar with a 1-line plain margin above/below.
                    let mut code_buf: Vec<Line<'static>> = Vec::new();
                    let flush_code = |buf: &mut Vec<Line<'static>>, rows: &mut Vec<Row>| {
                        if !buf.is_empty() {
                            let chunk = std::mem::take(buf);
                            push_code_block_rows(chunk, width, rows);
                        }
                    };
                    for line in md_lines {
                        if is_code_line(&line) {
                            code_buf.push(line);
                        } else {
                            flush_code(&mut code_buf, &mut rows);
                            // Collapse a blank that would double the post-code margin.
                            if line_is_blank(&line)
                                && rows.last().is_some_and(|r| line_is_blank(&r.line))
                            {
                                continue;
                            }
                            rows.push(Row::new(indent, text_row_w, line));
                        }
                    }
                    flush_code(&mut code_buf, &mut rows);
                }
            }
        }
        // Streaming cursor: append to the last text row so it sits at the end
        // of the answer (not on its own row, which looked like a stray glyph /
        // broken line break after the message). Skip full-width code bars.
        if self.streaming {
            let tail_is_tool = matches!(self.blocks.last(), Some(ThinkBlock::Tool(_)));
            if !tail_is_tool {
                let cursor = Span::styled("█", Style::default().fg(theme::t().accent));
                let mut glued = false;
                if let Some(last) = rows.last_mut() {
                    if last.x == indent
                        && !line_is_blank(&last.line)
                        && !is_code_line(&last.line)
                    {
                        last.line.spans.push(cursor.clone());
                        glued = true;
                    }
                }
                if !glued {
                    rows.push(Row::new(indent, text_row_w, Line::from(cursor)));
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
    fn diff_change_counts_ignore_file_headers() {
        let diff = "--- a/example.rs\n+++ b/example.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n+added\n";
        assert_eq!(diff_change_counts(diff), (2, 1));
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
        // Body only (vertical padding = 0), no inner left padding.
        let lines = render_markdown(
            "```bash\npython -m magnus.training.train_sarm --resume /tmp/checkpoint.pt\n```",
            100,
        );
        assert_eq!(lines.len(), 1, "body only, no vertical pads");
        assert!(is_code_line(&lines[0]));
        assert_eq!(
            line_text(&lines[0]).trim_end(),
            "python -m magnus.training.train_sarm --resume /tmp/checkpoint.pt"
        );
    }

    #[test]
    fn fenced_code_has_margin_between_paragraphs() {
        let lines = render_markdown("Before.\n\n```text\ncode line\n```\n\nAfter.", 100);
        let texts: Vec<_> = lines.iter().map(|l| line_text(l).trim_end().to_string()).collect();
        assert!(texts.iter().any(|t| t == "Before."));
        assert!(texts.iter().any(|t| t == "code line"));
        assert!(texts.iter().any(|t| t == "After."));
        let code_idx = texts.iter().position(|t| t == "code line").unwrap();
        assert!(is_code_line(&lines[code_idx]));
        // Neighbors are plain blanks (margin), not code-bg pads.
        assert!(line_is_blank(&lines[code_idx - 1]));
        assert!(!is_code_line(&lines[code_idx - 1]));
        assert!(line_is_blank(&lines[code_idx + 1]));
        assert!(!is_code_line(&lines[code_idx + 1]));
    }

    #[test]
    fn trailing_blank_code_lines_are_removed() {
        let lines = render_markdown(
            "```text\nUsage: `python -m magnus.training.train_sarm`\n\n\n```",
            100,
        );
        // Body only (inner trailing fence blanks trimmed by render path)
        assert_eq!(lines.len(), 1);
        assert_eq!(
            line_text(&lines[0]).trim_end(),
            "Usage: `python -m magnus.training.train_sarm`"
        );
        assert!(lines.iter().all(is_code_line));
    }

    #[test]
    fn thinking_cell_code_bar_has_two_column_outer_margins() {
        let mut tc = ThinkingCell::new();
        tc.add_text("```text\nhello\n```");
        tc.streaming = false;
        let width = 40u16;
        let rows = tc.build(width, None);
        let code_rows: Vec<_> = rows
            .iter()
            .filter(|r| is_code_line(&r.line))
            .collect();
        // Body only — no code-bg vertical pads.
        assert_eq!(code_rows.len(), 1, "expected body only");
        let r = code_rows[0];
        assert_eq!(r.x, CODE_BLOCK_LEFT_MARGIN, "code bar has a 2-column left margin");
        assert_eq!(
            r.w,
            width - CODE_BLOCK_LEFT_MARGIN - CODE_BLOCK_RIGHT_MARGIN,
            "code bar leaves a 4-column right margin"
        );
        assert_eq!(
            line_display_width_for_test(&r.line),
            r.w as usize,
            "line is padded with CODE_BG spaces to the row width"
        );
        assert!(line_text(&r.line).starts_with("hello"));
        assert!(line_text(&r.line).ends_with("  "));
    }

    #[test]
    fn thinking_cell_code_margin_around_multiline_body() {
        let mut tc = ThinkingCell::new();
        tc.add_text("Before.\n\n```text\nline1\nline2\n```\n\nAfter.");
        tc.streaming = false;
        let rows = tc.build(40, None);
        let code_rows: Vec<_> = rows.iter().filter(|r| is_code_line(&r.line)).collect();
        assert_eq!(code_rows.len(), 2); // line1 + line2, no pads
        assert!(line_text(&code_rows[0].line).contains("line1"));
        assert!(line_text(&code_rows[1].line).contains("line2"));

        // Find indices of code rows in full layout; neighbors should be blank margins.
        let idxs: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| is_code_line(&r.line))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(idxs.len(), 2);
        let first = idxs[0];
        let last = idxs[1];
        assert!(line_is_blank(&rows[first - 1].line));
        assert!(!is_code_line(&rows[first - 1].line));
        assert!(line_is_blank(&rows[last + 1].line));
        assert!(!is_code_line(&rows[last + 1].line));
    }

    fn line_display_width_for_test(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum()
    }

    #[test]
    fn table_hugs_content_not_terminal_width() {
        // Small table on a wide viewport must not stretch to full width.
        let md = "\
| A | B |
| --- | --- |
| x | y |
";
        let width = 100;
        let lines = render_markdown(md, width);
        assert!(
            !lines.is_empty(),
            "expected table lines, got: {:?}",
            lines.iter().map(line_text).collect::<Vec<_>>()
        );
        let top = line_text(&lines[0]);
        assert!(
            top.starts_with('┌') && top.ends_with('┐'),
            "top border: {top:?}"
        );
        let table_w = display_width(&top);
        // Natural: │ A │ B │ → roughly a dozen columns, far under 100.
        assert!(
            table_w < 30,
            "table should hug content (got width {table_w}, text {top:?})"
        );
        assert!(
            table_w < width,
            "table must not expand to full terminal width"
        );
        // Header row should contain the cells.
        let joined: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(joined.contains('A') && joined.contains('B'));
        assert!(joined.contains('x') && joined.contains('y'));
    }

    #[test]
    fn table_wraps_when_wider_than_viewport() {
        let md = "\
| LongHeaderOne | LongHeaderTwo |
| --- | --- |
| some fairly long cell value here | another long cell value too |
";
        let lines = render_markdown(md, 28);
        let top = lines
            .iter()
            .map(line_text)
            .find(|t| t.starts_with('┌'))
            .expect("top border");
        let table_w = display_width(&top);
        assert!(
            table_w <= 28,
            "wide table must shrink to viewport (got {table_w}): {top:?}"
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
