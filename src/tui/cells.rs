//! HistoryCell (DESIGN.md §3).
//!
//! One self-contained transcript entry. Each cell knows its own height at a
//! given width and how to render itself into a clipped rectangle, following the
//! DESIGN.md "HistoryCell" pattern rather than one giant scrollable paragraph.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui_markdown::{markdown::MarkdownRenderer, theme::ThemeConfig};

use crate::tui::render::RenderContext;
use crate::tui::shimmer::shimmer_spans;

/// One self-contained transcript entry.
pub trait HistoryCell {
    /// Height in rows this cell needs at `width`.
    fn desired_height(&self, width: u16) -> u16;
    /// Render into `area` (caller guarantees height >= desired_height).
    fn render(&self, area: Rect, buf: &mut Buffer, ctx: &RenderContext);
    /// For downcasting concrete cell types (needed for in-place mutation).
    fn as_any(&self) -> &dyn std::any::Any;
    /// Mutable downcast accessor.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// ---- System / Note cell ----------------------------------------------------

pub struct SystemCell {
    pub content: String,
}

impl HistoryCell for SystemCell {
    fn desired_height(&self, width: u16) -> u16 {
        let w = (width.saturating_sub(4)).max(1) as usize;
        let mut lines = 1u16;
        for l in self.content.lines() {
            lines += (l.chars().count() / w.max(1) + 1) as u16;
        }
        lines + 1
    }
    fn render(&self, area: Rect, buf: &mut Buffer, _ctx: &RenderContext) {
        let renderer = MarkdownRenderer::new(area.width as usize);
        let blocks = renderer.parse(&self.content);
        let styled = renderer.render(&blocks, &ThemeConfig::default());
        let mut y = area.top();
        for line in styled {
            if y >= area.bottom() {
                break;
            }
            buf.set_line(area.left(), y, &line, area.width);
            y += 1;
        }
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

impl HistoryCell for UserCell {
    fn desired_height(&self, width: u16) -> u16 {
        let w = (width.saturating_sub(4)).max(1) as usize;
        let mut lines = 0u16;
        for l in self.content.lines() {
            lines += (l.chars().count() / w.max(1) + 1) as u16;
        }
        lines.max(1) + 2 // one blank row of padding top and bottom
    }
    fn render(&self, area: Rect, buf: &mut Buffer, _ctx: &RenderContext) {
        let bg = Color::Rgb(45, 45, 52);
        // Grey block spanning the full width to distinguish user turns.
        buf.set_style(area, Style::default().bg(bg));
        let mut y = area.top() + 1; // top padding row
        for l in self.content.lines() {
            if y >= area.bottom() {
                break;
            }
            buf.set_line(
                area.left() + 2,
                y,
                &Line::from(Span::styled(l.to_string(), Style::default().bg(bg))),
                area.width.saturating_sub(2),
            );
            y += 1;
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
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
    pub fn header_line(&self, ctx: &RenderContext) -> Line<'static> {
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
        if self.status == ToolStatus::Running {
            let mut s = shimmer_spans(&summary, color, ctx.shimmer_phase);
            spans.append(&mut s);
        } else {
            spans.push(Span::styled(
                summary,
                Style::default().fg(color),
            ));
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
}

impl HistoryCell for ToolCallCell {
    fn desired_height(&self, _width: u16) -> u16 {
        1 + if self.expanded {
            let out = self.output.as_deref().unwrap_or("");
            let capped = out.len();
            let rows = capped / 200 + out.lines().count().min(20) as usize + 1;
            rows as u16
        } else {
            0
        }
    }
    fn render(&self, area: Rect, buf: &mut Buffer, ctx: &RenderContext) {
        buf.set_line(area.left(), area.top(), &self.header_line(ctx), area.width);
        if self.expanded {
            let out = self.output.as_deref().unwrap_or("");
            let shown: String = if out.lines().count() > 20 {
                out.lines().take(20).collect::<Vec<_>>().join("\n") + "\n…(truncated)"
            } else {
                out.to_string()
            };
            let mut y = area.top() + 1;
            for l in shown.lines() {
                if y >= area.bottom() {
                    break;
                }
                let color = if self.status == ToolStatus::Failed {
                    Color::Rgb(220, 120, 120)
                } else {
                    Color::Rgb(150, 150, 150)
                };
                buf.set_line(
                    area.left() + 4,
                    y,
                    &Line::from(Span::styled(l.to_string(), Style::default().fg(color))),
                    area.width - 4,
                );
                y += 1;
            }
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
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
}

impl HistoryCell for ThinkingCell {
    fn desired_height(&self, width: u16) -> u16 {
        // The "Thinking…" phrase is a transient indicator: it only occupies a
        // row while we're still waiting (no answer, no tools yet). Once content
        // starts arriving it disappears and doesn't persist in the transcript.
        let indicator = self.streaming && self.answer.is_empty() && self.tools.is_empty();
        if indicator {
            // One blank padding row (matching text) + the shimmer line.
            return 2;
        }
        let mut h = 0u16;
        for t in &self.tools {
            h += t.desired_height(width);
        }
        h += self.rendered_lines(width).len() as u16;
        // One blank padding row above and below the content.
        h + 2
    }
    fn render(&self, area: Rect, buf: &mut Buffer, ctx: &RenderContext) {
        let indicator = self.streaming && self.answer.is_empty() && self.tools.is_empty();
        // Transient shimmer indicator, shown only while waiting for the first
        // token. It is not persisted once the answer or tools appear.
        if indicator {
            // Same left offset and top padding as the rendered answer text.
            let label = Line::from(shimmer_spans(
                &format!("{}…", self.phrase),
                Color::Rgb(150, 150, 160),
                ctx.shimmer_phase,
            ));
            buf.set_line(area.left() + 2, area.top() + 1, &label, area.width - 2);
            return;
        }
        let mut y: i32 = area.top() as i32 + 1; // top padding row
        let bottom = area.bottom() as i32;
        for t in &self.tools {
            let th = t.desired_height(area.width) as i32;
            if y < bottom {
                let vis = (bottom - y).min(th).max(1) as u16;
                let cell_area = Rect {
                    x: area.left() + 2,
                    y: y as u16,
                    width: area.width.saturating_sub(2),
                    height: vis,
                };
                t.render(cell_area, buf, ctx);
            }
            y += th;
        }
        // Answer rendered directly, no title.
        if !self.answer.is_empty() {
            let lines = self.rendered_lines(area.width);
            for line in lines {
                if y >= bottom {
                    break;
                }
                buf.set_line(area.left() + 2, y as u16, &line, area.width - 2);
                y += 1;
            }
        } else if self.streaming && y < bottom {
            buf.set_line(
                area.left() + 2,
                y as u16,
                &Line::from(Span::styled("▍", Style::default().fg(Color::White))),
                area.width - 2,
            );
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
