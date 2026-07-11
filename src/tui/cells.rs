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
use ratatui_markdown::{markdown::MarkdownRenderer, theme::ThemeConfig};

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

use crate::tui::render::RenderContext;
use crate::tui::shimmer::shimmer_spans;

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
        let renderer = MarkdownRenderer::new(width as usize);
        let blocks = renderer.parse(&self.content);
        let styled = renderer.render(&blocks, &ThemeConfig::default());
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

pub struct UserCell {
    pub content: String,
}

impl UserCell {
    fn build(&self, width: u16, _ctx: Option<&RenderContext>) -> Vec<Row> {
        let avail = (width.saturating_sub(4)).max(1);
        let x = 2u16;
        let wrapped = wrap_text(&self.content, avail as usize);
        let mut rows = vec![Row::blank(width)]; // top padding
        for l in wrapped {
            rows.push(Row {
                x,
                w: avail,
                line: Line::from(l),
            });
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
        Some(Color::Rgb(45, 45, 52))
    }
}

// ---- Tool call card (DESIGN.md §7) -----------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum ToolStatus {
    Running,
    Success,
    Failed,
}

pub struct ToolCallCell {
    pub tool_name: String,
    pub args_preview: String,
    pub status: ToolStatus,
    pub output: Option<String>,
    pub expanded: bool,
}

impl ToolCallCell {
    pub fn header_line(&self, ctx: Option<&RenderContext>) -> Line<'static> {
        let (icon, color) = match self.status {
            ToolStatus::Running => ("▸", Color::Cyan),
            ToolStatus::Success => ("✓", Color::Green),
            ToolStatus::Failed => ("✗", Color::Red),
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
        let toggle = if self.output.is_some() {
            if self.expanded {
                "  ▾"
            } else {
                "  ▸"
            }
        } else {
            ""
        };
        if !toggle.is_empty() {
            spans.push(Span::styled(
                toggle.to_string(),
                Style::default().fg(Color::Rgb(150, 150, 150)),
            ));
        }
        Line::from(spans)
    }

    fn build(&self, width: u16, ctx: Option<&RenderContext>) -> Vec<Row> {
        let mut rows = vec![Row {
            x: 0,
            w: width,
            line: self.header_line(ctx),
        }];
        if self.expanded {
            let out = self.output.as_deref().unwrap_or("");
            let shown: String = if out.lines().count() > 20 {
                out.lines().take(20).collect::<Vec<_>>().join("\n") + "\n…(truncated)"
            } else {
                out.to_string()
            };
            let color = if self.status == ToolStatus::Failed {
                Color::Rgb(220, 120, 120)
            } else {
                Color::Rgb(150, 150, 150)
            };
            let x = 4u16;
            let w = width.saturating_sub(4);
            for l in shown.lines() {
                rows.push(Row {
                    x,
                    w,
                    line: Line::from(Span::styled(l.to_string(), Style::default().fg(color))),
                });
            }
        }
        rows
    }
}

// ---- Thinking (one block: phrase header + tool calls + answer) ------------

const THINK_PHRASES: &[&str] = &[
    "Thinking",
    "Mulling it over",
    "Reasoning",
    "Working",
    "Pondering",
    "Figuring it out",
];

pub struct ThinkingCell {
    pub phrase: String,
    pub tools: Vec<ToolCallCell>,
    pub answer: String,
    pub streaming: bool,
}

impl ThinkingCell {
    pub fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as usize;
        let phrase = THINK_PHRASES[n % THINK_PHRASES.len()].to_string();
        ThinkingCell {
            phrase,
            tools: Vec::new(),
            answer: String::new(),
            streaming: true,
        }
    }
    fn rendered_lines(&self, width: u16) -> Vec<Line<'static>> {
        // Wrap to the exact column budget used at render time (text is drawn at
        // a +2 left offset with a width of `width - 2`). Wrapping any narrower
        // pushes trailing tokens (e.g. a lone "?") onto their own line.
        let renderer = MarkdownRenderer::new(width.saturating_sub(2) as usize);
        let blocks = renderer.parse(&self.answer);
        let mut lines = renderer.render(&blocks, &ThemeConfig::default());
        let is_blank = |l: &Line<'static>| {
            l.spans.iter().all(|s| s.content.trim().is_empty())
        };
        while lines.first().map(&is_blank).unwrap_or(false) {
            lines.remove(0);
        }
        while lines.last().map(&is_blank).unwrap_or(false) {
            lines.pop();
        }
        lines
    }

    fn build(&self, width: u16, ctx: Option<&RenderContext>) -> Vec<Row> {
        // The "Thinking…" phrase is a transient indicator: it only occupies a
        // row while we're still waiting (no answer, no tools yet). Once content
        // starts arriving it disappears and doesn't persist in the transcript.
        let indicator = self.streaming && self.answer.is_empty() && self.tools.is_empty();
        if indicator {
            let line = match ctx {
                Some(c) => Line::from(shimmer_spans(
                    &format!("{}…", self.phrase),
                    Color::Rgb(150, 150, 160),
                    c.shimmer_phase,
                )),
                None => Line::from(self.phrase.clone() + "…"),
            };
            return vec![
                Row::blank(width),
                Row {
                    x: 2,
                    w: width.saturating_sub(2),
                    line,
                },
            ];
        }

        let mut rows = vec![Row::blank(width)]; // top padding
        for t in &self.tools {
            rows.extend(t.build(width, ctx));
        }
        // Answer rendered directly, no title.
        if !self.answer.is_empty() {
            for line in self.rendered_lines(width) {
                rows.push(Row {
                    x: 2,
                    w: width.saturating_sub(2),
                    line,
                });
            }
        } else if self.streaming {
            rows.push(Row {
                x: 2,
                w: width.saturating_sub(2),
                line: Line::from(Span::styled("▍", Style::default().fg(Color::White))),
            });
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
