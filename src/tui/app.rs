//! The interactive TUI application state and event loop logic.

#[allow(unused_imports)]
use std::io::{self, Stdout, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dirs;
use crossterm::{
    event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind},
    terminal,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
    Terminal,
};
use tui_textarea::{Input, Key, TextArea};
use tokio::sync::mpsc;

use crate::agent::{self, Provider, StreamEvent};
use crate::session::{self, Session};

use crate::tui::cells::{SystemCell, ThinkingCell, ToolCallCell, ToolStatus, UserCell};
use crate::tui::render::RenderContext;
use crate::tui::shimmer::shimmer_phase;
use crate::tui::slash::{fuzzy_subseq, SlashCommand, SLASH_COMMANDS};

/// Verbose command reference, shown only on `/help`.
pub const HELP_TEXT: &str = r#"
## xa — commands

- `/login [name]` — add or update a provider (custom endpoint, key, model)
- `/models [name]` — switch active provider, or set model on active provider
- `/clear` — clear the conversation
- `/help` — show this help
- `/exit` — quit

Keys: `Enter` send · `Shift+Enter` newline · `PageUp/PageDown` scroll ·
type `/` for the command menu · `Ctrl-C` quit.
"#;

/// Short codex-style tip shown when a fresh session opens.
pub const WELCOME_TEXT: &str = r#"
Tip: type `/` for the command menu, or just start chatting.
     `/login [name]` to add a provider · `/models` to switch · `/help` for all commands.
"#;

enum Pending {
    None,
    Login(String),
    Model(String),
    Save,
}

/// Events multiplexed into the single TUI loop.
enum AppEvent {
    Terminal(crossterm::event::Event),
    Stream(StreamEvent),
}

pub struct App {
    provider: Provider,
    cells: Vec<Box<dyn crate::tui::cells::HistoryCell>>,
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
        app
    }

    fn push_cell(&mut self, cell: Box<dyn crate::tui::cells::HistoryCell>) {
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
                self.system_msg(WELCOME_TEXT);
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

    fn input_is_empty(&self) -> bool {
        self.input.lines().iter().all(|l| l.is_empty())
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            // If the composer is empty, Ctrl-C quits immediately. Otherwise the
            // first press clears the input and a second within 1s quits.
            let has_text = self.input.lines().iter().any(|l| !l.trim().is_empty());
            if !has_text {
                return Ok(true);
            }
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
            self.status = "press Ctrl-C again to exit".into();
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
            KeyCode::Up if self.input_is_empty() => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down if self.input_is_empty() => {
                self.scroll = self.scroll.saturating_add(1);
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

        let mut pc = agent::ProvidersConfig::load();
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
        let mut pc = agent::ProvidersConfig::load();
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
        self.system_msg(WELCOME_TEXT);
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

    /// Render the codex-style banner box pinned at the top of the view.
    fn draw_header(&self, f: &mut ratatui::Frame, area: Rect) {
        let accent = Color::Rgb(120, 180, 255);
        let dim = Color::Rgb(150, 150, 170);
        let hint = Color::Rgb(90, 90, 110);
        let cwd = std::env::current_dir()
            .map(|p| {
                let s = p.display().to_string();
                if let Some(home) = dirs::home_dir() {
                    if let Ok(rest) = p.strip_prefix(&home) {
                        return format!("~{}", rest.display());
                    }
                }
                s
            })
            .unwrap_or_else(|_| ".".to_string());

        let version = env!("CARGO_PKG_VERSION");
        let model = if self.provider.model.is_empty() {
            "<unset>".to_string()
        } else {
            self.provider.model.clone()
        };

        // A bordered box around the header. Rendering via a widget guarantees
        // the content is clipped to the inner area and can never bleed onto
        // neighbouring rows.
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(70, 70, 90)));

        let lines = vec![
            Line::from(vec![
                Span::styled(">_ ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
                Span::styled("xa", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(" (v{version})"),
                    Style::default().fg(Color::Rgb(150, 150, 170)),
                ),
            ]),
            Line::from(vec![
                Span::styled(" model:    ", Style::default().fg(dim)),
                Span::styled(model, Style::default().fg(Color::Cyan)),
                Span::styled("     /models to change", Style::default().fg(hint)),
            ]),
            Line::from(vec![
                Span::styled(" directory:", Style::default().fg(dim)),
                Span::styled(
                    format!("  {cwd}"),
                    Style::default().fg(Color::Rgb(190, 190, 190)),
                ),
            ]),
        ];

        let paragraph = Paragraph::new(lines)
            .block(block)
            .alignment(ratatui::layout::Alignment::Left);
        f.render_widget(paragraph, area);
    }

    /// Render the short codex-style tip line beneath the header box.
    fn draw_tip(&self, f: &mut ratatui::Frame, area: Rect) {
        let tip = Line::from(vec![
            Span::styled("Tip: ", Style::default().fg(Color::Rgb(150, 150, 170))),
            Span::styled(
                "type `/` for the command menu, or just start chatting.",
                Style::default().fg(Color::Rgb(190, 190, 190)),
            ),
        ]);
        f.render_widget(Paragraph::new(tip), area);
    }

    fn total_height(&self, width: u16) -> u16 {
        self.cells.iter().map(|c| c.desired_height(width)).sum()
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        self.dirty = false;
        let area = f.area();
        let input_lines = self.input.lines().len().max(1) as u16;
        // One blank padding row above and below the text; grows with content.
        let input_h = (input_lines + 2).clamp(3, 14);

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

        // Fixed codex-style header: a bordered box (banner + model + directory)
        // on top, a one-line tip beneath it, then the scrollable transcript.
        // Everything is laid out with real Layout splits and rendered via
        // widgets, so regions can never overlap or bleed into each other.
        let view_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5), // bordered header box (3 lines + borders)
                Constraint::Length(2), // tip line (text + spacing)
                Constraint::Min(0),    // scrollable transcript
            ])
            .split(view);
        let header_area = view_chunks[0];
        let tip_area = view_chunks[1];
        let transcript = view_chunks[2];
        self.draw_header(f, header_area);
        self.draw_tip(f, tip_area);

        // Virtual scroll: compute per-cell heights, clip to viewport.
        let total = self.total_height(transcript.width);
        let view_h = transcript.height;
        let scroll = if self.scroll == u16::MAX {
            total.saturating_sub(view_h)
        } else {
            self.scroll.min(total.saturating_sub(1))
        };
        self.scroll = scroll; // reconcile sentinel

        let mut y: i32 = -(scroll as i32);
        for c in self.cells.iter() {
            let h = c.desired_height(transcript.width) as i32;
            if y + h > 0 && y < view_h as i32 {
                let top = y.max(0) as u16;
                let bottom = (y + h).min(view_h as i32) as u16;
                if bottom > top {
                    let cell_area = Rect {
                        x: transcript.left(),
                        y: transcript.top() + top,
                        width: transcript.width,
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

        // Input composer: borderless, pure grey background (DESIGN §4), with a
        // one-row pad top/bottom and left/right so the text sits vertically
        // centred inside the grey block.
        let input_block = Block::default()
            .borders(Borders::NONE)
            .padding(Padding::new(1, 1, 1, 1))
            .style(Style::default().bg(Color::Rgb(40, 40, 46)));
        self.input.set_block(input_block);
        // Visible block cursor.
        self.input
            .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        self.input
            .set_cursor_line_style(Style::default().bg(Color::Rgb(40, 40, 46)));
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
    crossterm::execute!(terminal.backend_mut(), crossterm::cursor::Show)?;
    write!(terminal.backend_mut(), "\r\n\x1b[33m{prompt}\x1b[0m ")?;
    terminal.backend_mut().flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    terminal::enable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), crossterm::cursor::Hide)?;
    Ok(s.trim().to_string())
}

/// Launch the interactive TUI with the given provider.
pub async fn run(
    provider: Provider,
    session: Session,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Duration;

    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
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
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(true) => match crossterm::event::read() {
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

    // A provider is "configured" only if it was actually persisted via
    // `xa login` / `/login` (providers.toml). A customized provider may legitimately
    // have an empty key (e.g. a local gateway with no auth), so we must not key
    // the check off `api_key.is_empty()`.
    let configured = agent::ProvidersConfig::load()
        .active_provider()
        .is_some();
    if !configured {
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
                    AppEvent::Terminal(crossterm::event::Event::Key(k)) => {
                        if app.handle_key(k)? {
                            break;
                        }
                    }
                    AppEvent::Terminal(crossterm::event::Event::Mouse(m)) => app.handle_mouse(m),
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
    crossterm::execute!(
        terminal.backend_mut(),
        terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}
