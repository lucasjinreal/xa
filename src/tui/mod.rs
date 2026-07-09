//! Codex-like interactive TUI for `xa`.
//!
//! Built on ratatui + crossterm with an Elm-ish event loop. Agent output is
//! rendered as a sequence of independent [`HistoryCell`]s (user messages,
//! assistant markdown, tool-call cards, errors, system notes) with simple
//! virtual scrolling. The transcript follows the DESIGN.md "HistoryCell"
//! pattern rather than one giant scrollable paragraph.
//!
//! Slash commands (`/login`, `/models`, `/clear`, `/help`, `/exit`, ...) are
//! handled via a floating popup overlay driven by a fuzzy subsequence filter.

#[allow(unused_imports)]
use std::io::{self, Stdout, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind},
    execute, terminal,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use ratatui_markdown::{markdown::MarkdownRenderer, theme::ThemeConfig};
use tui_textarea::{Input, Key, TextArea};
use tokio::sync::mpsc;

use crate::agent::{self, Provider, ProvidersConfig, StreamEvent};
use crate::session::{self, Session};

// ===========================================================================
// Shimmer (DESIGN.md §8)
// ===========================================================================

/// Blend two 4-bit/truecolor `Color`s by `t` in [0,1] (1 => `to`).
fn blend(base: Color, to: Color, t: f32) -> Color {
    let (br, bg, bb) = to_rgb(base);
    let (tr, tg, tb) = to_rgb(to);
    let r = (br as f32 + (tr as f32 - br as f32) * t) as u8;
    let g = (bg as f32 + (tg as f32 - bg as f32) * t) as u8;
    let b = (bb as f32 + (tb as f32 - bb as f32) * t) as u8;
    Color::Rgb(r, g, b)
}

fn to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::White => (255, 255, 255),
        Color::Gray => (190, 190, 190),
        Color::DarkGray => (110, 110, 110),
        Color::Black => (0, 0, 0),
        Color::Yellow => (255, 215, 0),
        Color::Green => (0, 200, 0),
        Color::Red => (220, 50, 50),
        Color::Cyan => (0, 200, 200),
        Color::Magenta => (200, 0, 200),
        Color::Blue => (50, 50, 220),
        other => {
            // Fall back to ANSI approximation for named colors we don't list.
            let (r, g, b) = ansi_approx(other);
            (r, g, b)
        }
    }
}

fn ansi_approx(c: Color) -> (u8, u8, u8) {
    // crude: only handle a few extras, default gray
    match c {
        Color::Reset => (200, 200, 200),
        _ => (180, 180, 180),
    }
}

/// Render `text` as a moving shimmer highlight. `phase` is in [0,1); the
/// highlight band sweeps left→right once per `period`.
fn shimmer_spans(text: &str, base: Color, phase: f32) -> Vec<Span<'static>> {
    let len = text.chars().count().max(1) as f32;
    let mut out = Vec::with_capacity(text.chars().count());
    for (i, ch) in text.chars().enumerate() {
        let pos = i as f32 / len;
        let dist = (pos - phase).abs().min(1.0 - (pos - phase).abs());
        let highlight = (1.0 - dist * 4.0).clamp(0.0, 1.0);
        let color = blend(base, Color::White, highlight);
        out.push(Span::styled(ch.to_string(), Style::default().fg(color)));
    }
    out
}

/// Current shimmer phase given a start instant and period.
fn shimmer_phase(start: Instant, period: f32) -> f32 {
    (start.elapsed().as_secs_f32() / period).rem_euclid(1.0)
}

// ===========================================================================
// HistoryCell (DESIGN.md §3)
// ===========================================================================

struct RenderContext {
    shimmer_phase: f32,
}

/// One self-contained transcript entry.
trait HistoryCell {
    /// Height in rows this cell needs at `width`.
    fn desired_height(&self, width: u16) -> u16;
    /// Render into `area` (caller guarantees height >= desired_height).
    fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer, ctx: &RenderContext);
    /// For downcasting concrete cell types (needed for in-place mutation).
    fn as_any(&self) -> &dyn std::any::Any;
    /// Mutable downcast accessor.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// ---- System / Note cell ----------------------------------------------------

struct SystemCell {
    content: String,
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
    fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer, _ctx: &RenderContext) {
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

struct UserCell {
    content: String,
}

impl HistoryCell for UserCell {
    fn desired_height(&self, width: u16) -> u16 {
        let w = (width.saturating_sub(4)).max(1) as usize;
        let mut lines = 2u16; // header + blank
        for l in self.content.lines() {
            lines += (l.chars().count() / w.max(1) + 1) as u16;
        }
        lines
    }
    fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer, _ctx: &RenderContext) {
        let header = Line::from(Span::styled(
            "✦ You",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
        buf.set_line(area.left(), area.top(), &header, area.width);
        let mut y = area.top() + 2;
        for l in self.content.lines() {
            if y >= area.bottom() {
                break;
            }
            buf.set_line(area.left() + 2, y, &Line::from(l.to_string()), area.width - 2);
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
enum ToolStatus {
    Running,
    Success,
    Failed,
}

struct ToolCallCell {
    tool_name: String,
    args_preview: String,
    status: ToolStatus,
    output: Option<String>,
    expanded: bool,
}

impl ToolCallCell {
    fn header_line(&self, ctx: &RenderContext) -> Line<'static> {
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
    fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer, ctx: &RenderContext) {
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

struct ThinkingCell {
    phrase: String,
    tools: Vec<ToolCallCell>,
    answer: String,
    streaming: bool,
}

impl ThinkingCell {
    fn new() -> Self {
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
        let renderer = MarkdownRenderer::new(width.saturating_sub(4) as usize);
        let blocks = renderer.parse(&self.answer);
        renderer.render(&blocks, &ThemeConfig::default())
    }
}

impl HistoryCell for ThinkingCell {
    fn desired_height(&self, width: u16) -> u16 {
        let mut h = 1u16; // phrase header
        for t in &self.tools {
            h += t.desired_height(width);
        }
        let ans = self.rendered_lines(width).len() as u16;
        h += ans;
        if self.streaming || ans == 0 {
            h = h.max(3);
        } else {
            h = h.max(2);
        }
        h
    }
    fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer, ctx: &RenderContext) {
        // Single phrase header (no per-tool "Thinking", no "Assistant" title).
        let label = if self.streaming && self.answer.is_empty() && self.tools.is_empty() {
            Line::from(shimmer_spans(
                &format!("{}…", self.phrase),
                Color::White,
                ctx.shimmer_phase,
            ))
        } else {
            Line::from(Span::styled(
                format!("{}", self.phrase),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ))
        };
        buf.set_line(area.left(), area.top(), &label, area.width);
        let mut y: i32 = area.top() as i32 + 1;
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

// ===========================================================================
// Slash command popup (DESIGN.md §5)
// ===========================================================================

struct SlashCommand {
    name: &'static str,
    desc: &'static str,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { name: "/login", desc: "add or update a provider" },
    SlashCommand { name: "/models", desc: "switch provider / set model" },
    SlashCommand { name: "/clear", desc: "clear the conversation" },
    SlashCommand { name: "/help", desc: "show help" },
    SlashCommand { name: "/exit", desc: "quit" },
    SlashCommand { name: "/sessions", desc: "list saved sessions" },
    SlashCommand { name: "/tools", desc: "list available tools" },
    SlashCommand { name: "/save", desc: "save the current session" },
    SlashCommand { name: "/new", desc: "start a new session" },
];

/// Subsequence fuzzy match: every char of `query` appears in order in `text`.
fn fuzzy_subseq(query: &str, text: &str) -> bool {
    let mut it = text.chars();
    for q in query.chars() {
        let q = q.to_ascii_lowercase();
        loop {
            match it.next() {
                Some(c) if c.to_ascii_lowercase() == q => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

// ===========================================================================
// App
// ===========================================================================

enum Pending {
    None,
    Login(String),
    Model(String),
    Save,
}

/// Events multiplexed into the single TUI loop.
enum AppEvent {
    Terminal(Event),
    Stream(StreamEvent),
}

struct App {
    provider: Provider,
    cells: Vec<Box<dyn HistoryCell>>,
    input: TextArea<'static>,
    scroll: u16,
    auto_scroll: bool,
    streaming: bool,
    should_quit: bool,
    status: String,
    pending: Pending,
    event_tx: mpsc::Sender<AppEvent>,
    reader_paused: Arc<AtomicBool>,
    session: Session,
    history: Vec<String>,
    history_idx: Option<usize>,
    agent_history: std::sync::Arc<std::sync::Mutex<Vec<agent::ChatMessage>>>,
    shimmer_start: Instant,
    dirty: bool,
    /// Active tool-call card index (during a streaming tool run) for grouping.
    active_think: Option<usize>,
    /// Timestamp of the last Ctrl-C press (for double-press-to-quit).
    last_ctrl_c: Option<Instant>,
    slash_mode: bool,
    slash_query: String,
    slash_selected: usize,
    queued_inputs: std::collections::VecDeque<String>,
}

impl App {
    fn new(
        provider: Provider,
        event_tx: mpsc::Sender<AppEvent>,
        reader_paused: Arc<AtomicBool>,
        session: Session,
    ) -> Self {
        let mut app = App {
            provider,
            cells: Vec::new(),
            input: TextArea::default(),
            scroll: 0,
            auto_scroll: true,
            streaming: false,
            should_quit: false,
            status: "ready".into(),
            pending: Pending::None,
            event_tx,
            reader_paused,
            session,
            history: Vec::new(),
            history_idx: None,
            agent_history: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            shimmer_start: Instant::now(),
            dirty: true,
            active_think: None,
            last_ctrl_c: None,
            slash_mode: false,
            slash_query: String::new(),
            slash_selected: 0,
            queued_inputs: std::collections::VecDeque::new(),
        };
        app.system_msg(HELP_TEXT);
        app
    }

    fn push_cell(&mut self, cell: Box<dyn HistoryCell>) {
        self.cells.push(cell);
        self.dirty = true;
        if self.auto_scroll {
            self.scroll = u16::MAX; // sentinel: follow bottom
        }
    }

    fn system_msg(&mut self, content: impl Into<String>) {
        self.push_cell(Box::new(SystemCell {
            content: content.into(),
        }));
    }

    /// Rebuild the persisted session from the current messages and save it.
    fn sync_session(&mut self) {
        self.session.provider = self.provider.name.clone();
        self.session.model = self.provider.model.clone();
        let mut msgs = Vec::new();
        for c in self.cells.iter() {
            if let Some(u) = c.as_any().downcast_ref::<UserCell>() {
                msgs.push(session::StoredMessage {
                    role: "user".into(),
                    content: u.content.clone(),
                });
            } else if let Some(tc) = c.as_any().downcast_ref::<ThinkingCell>() {
                if !tc.answer.trim().is_empty() {
                    msgs.push(session::StoredMessage {
                        role: "assistant".into(),
                        content: tc.answer.clone(),
                    });
                }
            }
        }
        self.session.messages = msgs;
        self.session.touch();
        let _ = session::save(&self.session);
    }

    fn submit(&mut self, text: String) {
        if text.starts_with('/') {
            self.handle_slash(&text);
            self.input = TextArea::default();
            return;
        }

        self.push_cell(Box::new(UserCell { content: text.clone() }));
        self.input = TextArea::default();
        self.scroll = u16::MAX;
        self.auto_scroll = true;

        {
            let mut h = self.agent_history.lock().unwrap();
            h.push(agent::ChatMessage {
                role: "user".into(),
                content: text.clone(),
                ..Default::default()
            });
        }

        let think_idx = self.cells.len();
        self.push_cell(Box::new(ThinkingCell::new()));
        self.active_think = Some(think_idx);
        self.sync_session();
        self.streaming = true;
        self.status = format!("streaming · {}", self.provider.model);
        self.dirty = true;

        let provider = self.provider.clone();
        let event_tx = self.event_tx.clone();
        let agent_hist = self.agent_history.clone();
        let assistant_idx = think_idx as u32;
        tokio::spawn(async move {
            let (stx, mut srx) = mpsc::channel::<StreamEvent>(64);
            let fwd = event_tx.clone();
            tokio::spawn(async move {
                while let Some(se) = srx.recv().await {
                    if fwd.send(AppEvent::Stream(se)).await.is_err() {
                        break;
                    }
                }
            });
            let tools = crate::tools::all_tools();
            agent::run_conversation(&provider, agent_hist, stx, &tools).await;
            let _ = event_tx.send(AppEvent::Stream(StreamEvent::Done)).await;
            let _ = event_tx.send(AppEvent::Stream(StreamEvent::InternalAssistIdx(assistant_idx))).await;
        });
    }

    fn handle_stream(&mut self, se: StreamEvent) {
        match se {
            StreamEvent::Delta(s) => {
                // Append to the active thinking cell's answer.
                if let Some(i) = self.active_think {
                    if let Some(tc) = self.cells[i].as_any_mut().downcast_mut::<ThinkingCell>() {
                        tc.answer.push_str(&s);
                        tc.streaming = true;
                    }
                } else {
                    let mut tc = ThinkingCell::new();
                    tc.answer.push_str(&s);
                    tc.streaming = true;
                    self.cells.push(Box::new(tc));
                    self.active_think = Some(self.cells.len() - 1);
                }
                self.status = format!("streaming · {}", self.provider.model);
                self.dirty = true;
            }
            StreamEvent::Done => {
                self.streaming = false;
                self.status = "ready".into();
                if let Some(i) = self.active_think {
                    if let Some(tc) = self.cells[i].as_any_mut().downcast_mut::<ThinkingCell>() {
                        tc.streaming = false;
                    }
                }
                self.active_think = None;
                self.sync_session();
                // flush queued inputs
                if let Some(next) = self.queued_inputs.pop_front() {
                    self.submit(next);
                }
                self.dirty = true;
            }
            StreamEvent::Error(e) => {
                self.streaming = false;
                self.status = "error".into();
                if let Some(i) = self.active_think {
                    if let Some(tc) = self.cells[i].as_any_mut().downcast_mut::<ThinkingCell>() {
                        tc.answer
                            .push_str(&format!("\n\n**error:** {e}"));
                        tc.streaming = false;
                    }
                }
                self.active_think = None;
                self.dirty = true;
            }
            StreamEvent::ToolCall { name, arguments } => {
                let preview = args_preview(&arguments);
                // Ensure an active thinking cell exists, then add the tool.
                let idx = if let Some(i) = self.active_think {
                    i
                } else {
                    self.cells.push(Box::new(ThinkingCell::new()));
                    let i = self.cells.len() - 1;
                    self.active_think = Some(i);
                    i
                };
                if let Some(tc) = self.cells[idx].as_any_mut().downcast_mut::<ThinkingCell>() {
                    tc.tools.push(ToolCallCell {
                        tool_name: name.clone(),
                        args_preview: preview,
                        status: ToolStatus::Running,
                        output: None,
                        expanded: false,
                    });
                }
                self.dirty = true;
            }
            StreamEvent::ToolResult {
                name,
                output,
                is_error,
            } => {
                // Update the most recent running tool card inside the active thinking cell.
                let mut updated = false;
                if let Some(i) = self.active_think {
                    if let Some(tc) = self.cells[i].as_any_mut().downcast_mut::<ThinkingCell>() {
                        for t in tc.tools.iter_mut().rev() {
                            if t.status == ToolStatus::Running {
                                t.status = if is_error {
                                    ToolStatus::Failed
                                } else {
                                    ToolStatus::Success
                                };
                                t.output = Some(output.clone());
                                t.expanded = is_error; // auto-expand on failure
                                updated = true;
                                break;
                            }
                        }
                    }
                }
                if !updated {
                    let mut tc = ThinkingCell::new();
                    tc.tools.push(ToolCallCell {
                        tool_name: name,
                        args_preview: String::new(),
                        status: if is_error {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Success
                        },
                        output: Some(output),
                        expanded: is_error,
                    });
                    self.cells.push(Box::new(tc));
                    self.active_think = Some(self.cells.len() - 1);
                }
                self.dirty = true;
            }
            StreamEvent::InternalAssistIdx(idx) => {
                if let Some(c) = self.cells.get_mut(idx as usize) {
                    if let Some(tc) = c.as_any_mut().downcast_mut::<ThinkingCell>() {
                        tc.streaming = false;
                    }
                }
                self.dirty = true;
            }
        }
    }

    fn handle_slash(&mut self, raw: &str) {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        let cmd = parts.first().copied().unwrap_or("");
        let arg = parts.get(1).copied().unwrap_or("");
        match cmd {
            "/exit" | "/quit" | "/q" => self.should_quit = true,
            "/clear" => {
                self.cells.clear();
                self.system_msg(HELP_TEXT);
            }
            "/help" | "/?" => self.system_msg(HELP_TEXT),
            "/login" => self.pending = Pending::Login(arg.to_string()),
            "/models" => self.pending = Pending::Model(arg.to_string()),
            "/sessions" | "/sess" | "/history" => self.list_sessions(),
            "/tools" => self.list_tools(),
            "/save" => {
                if arg.is_empty() {
                    self.pending = Pending::Save;
                } else {
                    self.session.title = arg.to_string();
                    self.sync_session();
                    self.system_msg(format!("session saved as `{}` ({})", self.session.title, self.session.id));
                }
            }
            "/new" => self.new_session(),
            other => self.system_msg(format!("unknown command: `{other}` (try /help)")),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            // Ctrl-C once: clear the input. Twice within 1s: quit.
            let now = Instant::now();
            let double = self
                .last_ctrl_c
                .map(|t| now.duration_since(t).as_secs_f32() < 1.0)
                .unwrap_or(false);
            if double {
                return Ok(true);
            }
            self.last_ctrl_c = Some(now);
            self.input = TextArea::default();
            self.status = "ready".into();
            self.dirty = true;
            return Ok(false);
        }
        if self.slash_mode {
            return self.handle_slash_key(key);
        }
        match key.code {
            KeyCode::PageUp => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(5);
                self.dirty = true;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(5);
                self.dirty = true;
            }
            KeyCode::Up if self.input.lines().len() <= 1 => self.recall_history(-1),
            KeyCode::Down if self.input.lines().len() <= 1 => self.recall_history(1),
            KeyCode::Char('/') if self.input.lines().len() <= 1
                && self.input.lines().first().map(|l| l.is_empty()).unwrap_or(true) =>
            {
                self.slash_mode = true;
                self.slash_query.clear();
                self.slash_selected = 0;
                self.dirty = true;
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                if self.streaming {
                    // Tab-queue: buffer the input for the next turn (DESIGN §4).
                    let text = self.input.lines().join("\n").trim().to_string();
                    if !text.is_empty() {
                        self.queued_inputs.push_back(text);
                        self.input = TextArea::default();
                        self.status = format!("queued · {} pending", self.queued_inputs.len());
                        self.dirty = true;
                    }
                    return Ok(false);
                }
                let text = self.input.lines().join("\n").trim().to_string();
                if !text.is_empty() {
                    self.history.push(text.clone());
                    self.history_idx = None;
                    self.submit(text);
                }
            }
            KeyCode::Tab if self.streaming => {
                let text = self.input.lines().join("\n").trim().to_string();
                if !text.is_empty() {
                    self.queued_inputs.push_back(text);
                    self.input = TextArea::default();
                    self.status = format!("queued · {} pending", self.queued_inputs.len());
                    self.dirty = true;
                }
            }
            _ => {
                self.history_idx = None;
                if let Some(input) = map_key(key) {
                    self.input.input(input);
                    self.dirty = true;
                }
            }
        }
        Ok(false)
    }

    fn handle_slash_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.slash_mode = false;
                self.dirty = true;
            }
            KeyCode::Char(c) => {
                self.slash_query.push(c);
                self.slash_selected = 0;
                self.dirty = true;
            }
            KeyCode::Backspace => {
                self.slash_query.pop();
                self.slash_selected = 0;
                if self.slash_query.is_empty() {
                    self.slash_mode = false;
                }
                self.dirty = true;
            }
            KeyCode::Up => {
                if self.slash_selected > 0 {
                    self.slash_selected -= 1;
                }
                self.dirty = true;
            }
            KeyCode::Down => {
                let n = self.filtered_slash().len();
                if n > 0 && self.slash_selected + 1 < n {
                    self.slash_selected += 1;
                }
                self.dirty = true;
            }
            KeyCode::Enter => {
                let filtered = self.filtered_slash();
                if let Some(cmd) = filtered.get(self.slash_selected).map(|c| c.name.to_string()) {
                    self.slash_mode = false;
                    self.slash_query.clear();
                    self.submit(cmd);
                }
                self.dirty = true;
            }
            _ => {}
        }
        Ok(false)
    }

    fn filtered_slash(&self) -> Vec<&'static SlashCommand> {
        if self.slash_query.is_empty() {
            SLASH_COMMANDS.iter().collect()
        } else {
            SLASH_COMMANDS
                .iter()
                .filter(|c| fuzzy_subseq(&self.slash_query, c.name))
                .collect()
        }
    }

    /// Recall a previous submitted input (codex-like Up/Down on a single line).
    fn recall_history(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        let len = self.history.len() as i32;
        let idx = match self.history_idx {
            Some(i) => {
                let n = i as i32 + dir;
                if n < 0 {
                    0
                } else if n >= len {
                    (len - 1) as usize
                } else {
                    n as usize
                }
            }
            None => {
                if dir < 0 {
                    (len - 1) as usize
                } else {
                    0
                }
            }
        };
        self.history_idx = Some(idx);
        let text = self.history[idx].clone();
        self.input = TextArea::new(vec![text]);
        self.input.input(Input { key: Key::End, ctrl: false, alt: false, shift: false });
        self.dirty = true;
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollUp => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(3);
                self.dirty = true;
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(3);
                self.dirty = true;
            }
            _ => {}
        }
    }

    async fn do_pending(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<()> {
        self.reader_paused.store(true, Ordering::SeqCst);
        let result = match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::None => Ok(()),
            Pending::Login(name) => self.login(terminal, name).await,
            Pending::Model(arg) => self.switch_model(terminal, arg),
            Pending::Save => {
                let title = read_line_paused(terminal, "session title:")?;
                if !title.is_empty() {
                    self.session.title = title;
                }
                self.sync_session();
                self.system_msg(format!(
                    "session saved as `{}` ({})",
                    self.session.title, self.session.id
                ));
                Ok(())
            }
        };
        self.reader_paused.store(false, Ordering::SeqCst);
        self.dirty = true;
        result
    }

    async fn login(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        _name_arg: String,
    ) -> io::Result<()> {
        let provider = agent::interactive_configure(|q| {
            read_line_paused(terminal, q).unwrap_or_default()
        })
        .await;

        let mut pc = ProvidersConfig::load();
        pc.upsert(provider.clone());
        match pc.save() {
            Ok(_) => self.system_msg(format!("logged in as provider `{}`", provider.name)),
            Err(e) => self.system_msg(format!("save failed: {e}")),
        }
        self.provider = provider;
        self.dirty = true;
        Ok(())
    }

    fn switch_model(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        arg: String,
    ) -> io::Result<()> {
        let mut pc = ProvidersConfig::load();
        if arg.is_empty() {
            let names: Vec<String> = pc.list().iter().map(|p| p.name.clone()).collect();
            self.system_msg(format!("providers: {}", names.join(", ")));
            let choice = read_line_paused(terminal, "switch to provider:")?;
            if let Some(p) = pc.providers.get(&choice) {
                pc.active = choice.clone();
                pc.save().ok();
                self.provider = p.clone();
                self.system_msg(format!("active provider: {}", choice));
            } else {
                self.system_msg("unknown provider".to_string());
            }
        } else if let Some(p) = pc.providers.get(&arg) {
            pc.active = arg.clone();
            pc.save().ok();
            self.provider = p.clone();
            self.system_msg(format!("active provider: {}", arg));
        } else {
            self.provider.model = arg.clone();
            pc.upsert(self.provider.clone());
            pc.save().ok();
            self.system_msg(format!("model set to `{}`", arg));
        }
        self.dirty = true;
        Ok(())
    }

    fn list_sessions(&mut self) {
        let mut out = String::from("## Saved sessions\n\n");
        let sessions = session::list();
        if sessions.is_empty() {
            out.push_str("_No saved sessions yet. Use `/save [title]` to store the current conversation._");
        } else {
            for s in &sessions {
                out.push_str(&format!(
                    "- **{}** — `{}` · model `{}` · {} msgs\n",
                    s.title,
                    s.id,
                    s.model,
                    s.messages.len()
                ));
            }
            out.push_str("\nResume with: `xa chat --session <id>`");
        }
        self.system_msg(out);
    }

    fn new_session(&mut self) {
        self.session = Session::new(&self.provider.name, &self.provider.model);
        self.cells.clear();
        self.agent_history.lock().unwrap().clear();
        self.system_msg(HELP_TEXT);
        self.system_msg(format!("started a new session `{}`", self.session.id));
        self.dirty = true;
    }

    fn list_tools(&mut self) {
        let mut s = String::from("## Available tools\n\n");
        for t in crate::tools::all_tools() {
            s.push_str(&format!("- **{}**: {}\n", t.name(), t.description()));
        }
        self.system_msg(s);
    }

    fn total_height(&self, width: u16) -> u16 {
        self.cells.iter().map(|c| c.desired_height(width)).sum()
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        self.dirty = false;
        let area = f.area();
        let input_lines = self.input.lines().len().max(1) as u16;
        let input_h = (input_lines + 2).clamp(3, 12);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(input_h),
                Constraint::Length(1),
            ])
            .split(area);

        let view = chunks[0];
        let ctx = RenderContext {
            shimmer_phase: shimmer_phase(self.shimmer_start, 1.8),
        };

        // Virtual scroll: compute per-cell heights, clip to viewport.
        let total = self.total_height(view.width);
        let view_h = view.height;
        let scroll = if self.scroll == u16::MAX {
            total.saturating_sub(view_h)
        } else {
            self.scroll.min(total.saturating_sub(1))
        };
        self.scroll = scroll; // reconcile sentinel

        let mut y: i32 = -(scroll as i32);
        for c in self.cells.iter() {
            let h = c.desired_height(view.width) as i32;
            if y + h > 0 && y < view_h as i32 {
                let top = y.max(0) as u16;
                let bottom = (y + h).min(view_h as i32) as u16;
                if bottom > top {
                    let cell_area = Rect {
                        x: view.left(),
                        y: top,
                        width: view.width,
                        height: bottom - top,
                    };
                    c.render(cell_area, f.buffer_mut(), &ctx);
                }
            }
            y += h;
            if y >= view_h as i32 {
                break;
            }
        }

        // Input composer with stateful border (DESIGN §4).
        let border_color = if self.streaming {
            Color::Cyan
        } else {
            Color::Rgb(90, 90, 120)
        };
        let title = if self.streaming {
            " Input · streaming (Esc to interrupt) "
        } else if self.input.lines().first().map(|l| l.is_empty()).unwrap_or(true) {
            " Input · Enter send · Shift+Enter newline · / for commands "
        } else {
            " Input "
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color)));
        self.input.set_block(input_block);
        // Placeholder hint when empty and not streaming.
        if self.input.lines().first().map(|l| l.is_empty()).unwrap_or(true) && !self.streaming {
            self.input
                .set_placeholder_text("Type a message, or / for commands…");
        }
        f.render_widget(&self.input, chunks[1]);

        // Status line + model hint (DESIGN §6).
        let queued = if self.queued_inputs.is_empty() {
            String::new()
        } else {
            format!(" · {} queued", self.queued_inputs.len())
        };
        let status_line = Line::from(vec![
            Span::styled(
                format!(" {} ", self.status),
                Style::default().fg(if self.streaming {
                    Color::Cyan
                } else if self.status == "error" {
                    Color::Red
                } else {
                    Color::Rgb(150, 150, 150)
                }),
            ),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(150, 150, 150))),
            Span::styled(
                format!(" {}", self.provider.model),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(150, 150, 150))),
            Span::styled(
                format!(" {}", self.provider.name),
                Style::default().fg(Color::Rgb(150, 150, 150)),
            ),
            Span::styled(queued, Style::default().fg(Color::Yellow)),
        ]);
        f.render_widget(Paragraph::new(status_line), chunks[2]);

        // Slash popup overlay (DESIGN §5).
        if self.slash_mode {
            let filtered = self.filtered_slash();
            let popup_h = (filtered.len() as u16 + 2).clamp(3, 12);
            let popup_w = 46.min(chunks[1].width);
            let popup_area = Rect {
                x: chunks[1].left(),
                y: (chunks[1].top().saturating_sub(popup_h)).max(0),
                width: popup_w,
                height: popup_h,
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(Span::styled(
                    format!(" /{:<1$} ", self.slash_query, 10),
                    Style::default().fg(Color::Magenta),
                ));
            let inner = block.inner(popup_area);
            f.render_widget(block, popup_area);
            let mut y = inner.top();
            for (i, cmd) in filtered.iter().enumerate() {
                if y >= inner.bottom() {
                    break;
                }
                let sel = i == self.slash_selected;
                let style = if sel {
                    Style::default().fg(Color::Black).bg(Color::Magenta)
                } else {
                    Style::default().fg(Color::Rgb(190, 190, 190))
                };
                let line = Line::from(vec![
                    Span::styled(format!(" {:<10}", cmd.name), style),
                    Span::styled(format!(" {:<28}", cmd.desc), style),
                ]);
                f.render_widget(Paragraph::new(line), Rect {
                    x: inner.left(),
                    y,
                    width: inner.width,
                    height: 1,
                });
                y += 1;
            }
        }
    }
}

fn args_preview(arguments: &str) -> String {
    // Try to pull the first interesting arg value for a one-line summary.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments) {
        if let serde_json::Value::Object(map) = v {
            if let Some((_, val)) = map.iter().next() {
                let s = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                return format!("({})", s.chars().take(40).collect::<String>());
            }
        }
    }
    String::new()
}

/// Map a crossterm key event into tui-textarea's backend-agnostic `Input`.
fn map_key(key: KeyEvent) -> Option<Input> {
    use KeyModifiers as M;
    let k = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Tab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Esc => Key::Esc,
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    };
    Some(Input {
        key: k,
        ctrl: key.modifiers.contains(M::CONTROL),
        alt: key.modifiers.contains(M::ALT),
        shift: key.modifiers.contains(M::SHIFT),
    })
}

/// Temporarily drop raw mode to read a single line from the user.
fn read_line_paused(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    prompt: &str,
) -> io::Result<String> {
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), crossterm::cursor::Show)?;
    write!(terminal.backend_mut(), "\r\n\x1b[33m{prompt}\x1b[0m ")?;
    terminal.backend_mut().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    terminal::enable_raw_mode()?;
    execute!(terminal.backend_mut(), crossterm::cursor::Hide)?;
    Ok(s.trim().to_string())
}

/// Launch the interactive TUI with the given provider.
pub async fn run(
    provider: Provider,
    session: Session,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        crossterm::cursor::Hide,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal::enable_raw_mode()?;

    let (tx_event, mut rx_event) = mpsc::channel::<AppEvent>(128);
    let reader_paused = Arc::new(AtomicBool::new(false));

    {
        let tx = tx_event.clone();
        let paused = reader_paused.clone();
        tokio::task::spawn_blocking(move || loop {
            if paused.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(e) => {
                        if tx.blocking_send(AppEvent::Terminal(e)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => continue,
                Err(_) => break,
            }
        });
    }

    let mut app = App::new(provider, tx_event, reader_paused, session);

    if app.provider.api_key.is_empty() {
        app.system_msg(
            "_No provider configured._ Run `/login [name]` to add one (custom endpoint + key + model).",
        );
    }

    if !app.session.messages.is_empty() {
        let resumed = app.session.messages.clone();
        app.system_msg(format!("resumed session `{}`", app.session.id));
        for m in &resumed {
            match m.role.as_str() {
                "user" => app.push_cell(Box::new(UserCell { content: m.content.clone() })),
                "assistant" => {
                    let mut tc = ThinkingCell::new();
                    tc.answer = m.content.clone();
                    tc.streaming = false;
                    app.push_cell(Box::new(tc));
                }
                _ => {}
            }
        }
        let hist: Vec<agent::ChatMessage> = app
            .session
            .messages
            .iter()
            .map(|m| agent::ChatMessage {
                role: match m.role.as_str() {
                    "user" => "user",
                    _ => "assistant",
                }
                .to_string(),
                content: m.content.clone(),
                ..Default::default()
            })
            .collect();
        *app.agent_history.lock().unwrap() = hist;
    }

    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        if app.dirty {
            terminal.draw(|f| app.draw(f))?;
        }

        tokio::select! {
            Some(ev) = rx_event.recv() => {
                match ev {
                    AppEvent::Terminal(Event::Key(k)) => {
                        if app.handle_key(k)? {
                            break;
                        }
                    }
                    AppEvent::Terminal(Event::Mouse(m)) => app.handle_mouse(m),
                    AppEvent::Terminal(_) => {}
                    AppEvent::Stream(se) => app.handle_stream(se),
                }
            }
            _ = tick.tick() => {
                // Only redraw when something is animating.
                if app.streaming || app.slash_mode {
                    app.dirty = true;
                }
            }
        }

        if !matches!(app.pending, Pending::None) {
            app.do_pending(&mut terminal).await?;
        }

        if app.should_quit {
            break;
        }
    }

    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

const HELP_TEXT: &str = r#"
## xa — commands

- `/login [name]` — add or update a provider (custom endpoint, key, model)
- `/models [name]` — switch active provider, or set model on active provider
- `/clear` — clear the conversation
- `/help` — show this help
- `/exit` — quit

Keys: `Enter` send · `Shift+Enter` newline · `PageUp/PageDown` scroll ·
type `/` for the command menu · `Ctrl-C` quit.
"#;
