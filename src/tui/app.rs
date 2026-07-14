//! The interactive TUI application state and event loop logic.

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
use tui_textarea::{CursorMove, Input, Key, TextArea};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

use crate::agent::{self, Provider, StreamEvent};
use crate::session::{self, Session};

use crate::tui::cells::{SystemCell, ThinkingCell, UserCell, USER_LEAD_COLS, USER_PROMPT};
use crate::tui::render::RenderContext;
use crate::tui::shimmer::{shimmer_phase, shimmer_spans_to};
use crate::tui::slash::{fuzzy_subseq, SlashCommand, SLASH_COMMANDS};
use crate::tui::theme;
use crate::tui::think::{StreamPhase, ThinkFilter};
use crate::tui::wizard::{Wizard, WizardAction};

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
    Save,
}

/// Events multiplexed into the single TUI loop.
enum AppEvent {
    Terminal(crossterm::event::Event),
    Stream(StreamEvent),
    /// Result of an async model-list fetch kicked off by the setup wizard.
    Wizard(Result<Vec<String>, String>),
}

pub struct App {
    provider: Provider,
    cells: Vec<Box<dyn crate::tui::cells::HistoryCell>>,
    input: TextArea<'static>,
    scroll: u16,
    auto_scroll: bool,
    streaming: bool,
    should_quit: bool,
    /// Footer / transient messages (queue, ctrl-c hint). Activity labels live
    /// in [`stream_phase`] above the input bar.
    status: String,
    /// Claude-Code-style activity above the input (Waiting / Thinking / …).
    stream_phase: StreamPhase,
    /// When the current [`stream_phase`] began (timer restarts on phase change).
    phase_started: Instant,
    /// Strips `<think>`…`</think>` from streamed text and drives Thinking phase.
    think_filter: ThinkFilter,
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
    /// Width (columns) of the input composer area from the last frame, used to
    /// soft-wrap the typed text so it never overflows the box.
    input_area_width: u16,
    /// Active provider/model setup wizard (codex-style modal). While set,
    /// all keys are routed to it and it is drawn as a centered overlay.
    wizard: Option<Wizard>,
}

impl App {
    fn new(
        provider: Provider,
        event_tx: mpsc::Sender<AppEvent>,
        reader_paused: Arc<AtomicBool>,
        session: Session,
    ) -> Self {
        App {
            provider,
            cells: Vec::new(),
            input: TextArea::default(),
            scroll: 0,
            auto_scroll: true,
            streaming: false,
            should_quit: false,
            status: String::new(),
            stream_phase: StreamPhase::Idle,
            phase_started: Instant::now(),
            think_filter: ThinkFilter::new(),
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
            input_area_width: 80,
            wizard: None,
        }
    }

    fn push_cell(&mut self, cell: Box<dyn crate::tui::cells::HistoryCell>) {
        self.cells.push(cell);
        self.dirty = true;
        if self.auto_scroll {
            self.scroll = u16::MAX; // sentinel: follow bottom
        }
    }

    /// Update activity phase; restarts the elapsed timer only when it changes.
    fn set_stream_phase(&mut self, phase: StreamPhase) {
        if self.stream_phase != phase {
            self.stream_phase = phase;
            self.phase_started = Instant::now();
            self.dirty = true;
        }
    }

    /// Elapsed time in the current status, e.g. `0.4s` / `12.3s` / `1m05s`.
    fn phase_elapsed_label(&self) -> String {
        let secs = self.phase_started.elapsed().as_secs_f32();
        if secs < 60.0 {
            format!("{secs:.1}s")
        } else {
            let m = (secs as u32) / 60;
            let s = (secs as u32) % 60;
            format!("{m}m{s:02}s")
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
                let answer = tc.answer_text();
                if !answer.trim().is_empty() {
                    msgs.push(session::StoredMessage {
                        role: "assistant".into(),
                        content: answer,
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
        self.think_filter.reset();
        self.set_stream_phase(StreamPhase::Waiting);
        self.status.clear();
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
                // Strip `<think>`…`</think>` from the transcript; drive phase.
                let (visible, inside_think) = self.think_filter.feed(&s);
                let next = if inside_think {
                    StreamPhase::Thinking
                } else if self.think_filter.saw_visible || !visible.is_empty() {
                    StreamPhase::Responding
                } else {
                    StreamPhase::Waiting
                };
                self.set_stream_phase(next);

                if !visible.is_empty() {
                    if let Some(i) = self.active_think {
                        if let Some(tc) =
                            self.cells[i].as_any_mut().downcast_mut::<ThinkingCell>()
                        {
                            tc.add_text(&visible);
                            tc.streaming = true;
                        }
                    } else {
                        let mut tc = ThinkingCell::new();
                        tc.add_text(&visible);
                        tc.streaming = true;
                        self.cells.push(Box::new(tc));
                        self.active_think = Some(self.cells.len() - 1);
                    }
                }
                self.dirty = true;
            }
            StreamEvent::Done => {
                // Flush any held partial tag text outside a think block.
                let tail = self.think_filter.finish();
                if !tail.is_empty() {
                    if let Some(i) = self.active_think {
                        if let Some(tc) =
                            self.cells[i].as_any_mut().downcast_mut::<ThinkingCell>()
                        {
                            tc.add_text(&tail);
                        }
                    }
                }
                self.streaming = false;
                self.set_stream_phase(StreamPhase::Idle);
                self.status.clear();
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
                self.set_stream_phase(StreamPhase::Error);
                self.status.clear();
                // Surface the error directly inside the AI response cell so the
                // user can see what went wrong, rather than only the status bar.
                let idx = if let Some(i) = self.active_think {
                    i
                } else {
                    self.cells.push(Box::new(ThinkingCell::new()));
                    let i = self.cells.len() - 1;
                    self.active_think = Some(i);
                    i
                };
                if let Some(tc) = self.cells[idx].as_any_mut().downcast_mut::<ThinkingCell>() {
                    tc.add_text(&format!("\n\n**error:** {e}"));
                    tc.streaming = false;
                }
                self.active_think = None;
                self.dirty = true;
            }
            StreamEvent::ToolCall { name, arguments } => {
                let preview = args_preview(&arguments);
                // Pull path / read window out of the args for the `← Edit` /
                // `→ Read` summaries.
                let (path, read_offset, read_limit) = tool_path_window(&arguments);
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
                    tc.add_tool(&name, &preview, path, read_offset, read_limit);
                }
                self.set_stream_phase(StreamPhase::RunningTool);
                self.dirty = true;
            }
            StreamEvent::ToolResult {
                name,
                output,
                is_error,
                diff,
            } => {
                // Update the most recent running tool card inside the active
                // thinking cell.
                let mut updated = false;
                if let Some(i) = self.active_think {
                    if let Some(tc) = self.cells[i].as_any_mut().downcast_mut::<ThinkingCell>() {
                        tc.finish_tool(Some(output.clone()), is_error, diff.clone());
                        updated = true;
                    }
                }
                if !updated {
                    let mut tc = ThinkingCell::new();
                    tc.add_tool(&name, "", None, None, None);
                    tc.finish_tool(Some(output), is_error, diff);
                    self.cells.push(Box::new(tc));
                    self.active_think = Some(self.cells.len() - 1);
                }
                // After a tool, wait for the next model tokens.
                if self.streaming {
                    let next = if self.think_filter.inside_think() {
                        StreamPhase::Thinking
                    } else if self.think_filter.saw_visible {
                        StreamPhase::Responding
                    } else {
                        StreamPhase::Waiting
                    };
                    self.set_stream_phase(next);
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
            "/login" => {
                let arg = if arg.is_empty() { None } else { Some(arg.to_string()) };
                self.wizard = Some(Wizard::new_login(arg.as_deref()));
                self.dirty = true;
            }
            "/models" => {
                let arg = if arg.is_empty() { None } else { Some(arg.to_string()) };
                self.wizard = Some(Wizard::new_models(arg.as_deref()));
                self.dirty = true;
            }
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

    /// Recover the logical input text. Soft-wrap line breaks inserted by
    /// [`Self::reflow_input`] carry no characters, so concatenating the lines
    /// losslessly reproduces what the user typed (paste newlines are folded).
    fn input_text(&self) -> String {
        self.input.lines().concat().trim().to_string()
    }

    /// Display width of a single character, treating wide/CJK glyphs as 2 cols.
    fn cw(c: char) -> usize {
        UnicodeWidthChar::width(c).unwrap_or(0)
    }

    /// Build a display-only, soft-wrapped copy of the input composer.
    ///
    /// `self.input` always holds the *logical* text (real newlines only, from
    /// Shift-Enter); wrapping is applied purely for rendering so the editable
    /// buffer is never mutated and never compounds broken line breaks across
    /// frames. The cursor is mapped onto the wrapped layout so it tracks the
    /// logical position correctly even across real-newline boundaries.
    fn wrapped_input(&self) -> TextArea<'static> {
        // Horizontal pad: left margin (1) + ❯ lead + right pad (1).
        let h_pad = 1u16 + USER_LEAD_COLS + 1;
        let max_w = (self.input_area_width.saturating_sub(h_pad)).max(1) as usize;
        let (cur_row, cur_col) = self.input.cursor();
        let lines: Vec<String> = self.input.lines().iter().map(|s| s.to_string()).collect();

        // Linear cursor index over the logical text (real newlines count as 1).
        let mut cursor_idx: usize = 0;
        for (i, l) in lines.iter().enumerate() {
            if i < cur_row {
                cursor_idx += l.chars().count() + 1; // +1 for the real newline
            } else {
                cursor_idx += cur_col;
                break;
            }
        }

        let mut display: Vec<String> = Vec::new();
        let mut disp_start: Vec<usize> = Vec::new();
        let mut disp_len: Vec<usize> = Vec::new();
        let mut global: usize = 0;

        for line in &lines {
            let chars: Vec<char> = line.chars().collect();
            let mut i = 0;
            let mut produced = false;
            while i < chars.len() {
                let mut col = 0usize;
                let mut j = i;
                while j < chars.len() {
                    let w = Self::cw(chars[j]);
                    if col + w > max_w && j > i {
                        break;
                    }
                    col += w;
                    j += 1;
                }
                if j == i {
                    // A single glyph wider than the box: keep it on its own line.
                    j = i + 1;
                }
                let seg: String = chars[i..j].iter().collect();
                disp_start.push(global);
                disp_len.push(seg.chars().count());
                display.push(seg);
                global += j - i;
                i = j;
                produced = true;
            }
            if !produced {
                // Preserve an empty logical line as one empty display line.
                disp_start.push(global);
                disp_len.push(0);
                display.push(String::new());
            }
            global += 1; // the real newline boundary between logical lines
        }

        // Map the linear cursor index onto a (row, col) in the wrapped output.
        let (mut target_row, mut target_col) = (0usize, 0usize);
        let mut found = false;
        for idx in 0..display.len() {
            let start = disp_start[idx];
            let len = disp_len[idx];
            if cursor_idx <= start + len {
                target_row = idx;
                target_col = cursor_idx.saturating_sub(start);
                found = true;
                break;
            }
        }
        if !found {
            if let Some(last) = display.len().checked_sub(1) {
                target_row = last;
                target_col = disp_len[last];
            }
        }

        let mut ta = TextArea::new(display);
        ta.move_cursor(CursorMove::Jump(target_row as u16, target_col as u16));
        ta
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        // Dismiss a sticky error activity line on the next keystroke.
        if self.stream_phase == StreamPhase::Error && !self.streaming {
            self.set_stream_phase(StreamPhase::Idle);
        }

        // While the setup wizard is open it owns the keyboard entirely.
        if let Some(w) = &mut self.wizard {
            let action = w.handle_key(key);
            match action {
                WizardAction::None => {}
                WizardAction::Cancel => {
                    self.wizard = None;
                    self.system_msg("provider setup cancelled");
                }
                WizardAction::StartFetch { endpoint, api_key } => {
                    self.start_wizard_fetch(endpoint, api_key);
                }
                WizardAction::Done(provider) => {
                    self.finish_wizard(provider);
                }
            }
            self.dirty = true;
            return Ok(false);
        }
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
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_add(5);
                self.dirty = true;
            }
            KeyCode::Up if self.input_is_empty() => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down if self.input_is_empty() => {
                self.auto_scroll = false;
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
                    let text = self.input_text();
                    if !text.is_empty() {
                        self.queued_inputs.push_back(text);
                        self.input = TextArea::default();
                        self.status = format!("queued · {} pending", self.queued_inputs.len());
                        self.dirty = true;
                    }
                    return Ok(false);
                }
                let text = self.input_text();
                if !text.is_empty() {
                    self.history.push(text.clone());
                    self.history_idx = None;
                    self.submit(text);
                }
            }
            KeyCode::Tab if self.streaming => {
                let text = self.input_text();
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
                self.auto_scroll = false;
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

    /// Kick off the async model-list fetch requested by the setup wizard.
    /// The result comes back through [`AppEvent::Wizard`] in the main loop.
    fn start_wizard_fetch(&mut self, endpoint: String, api_key: String) {
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let res = agent::fetch_models(&endpoint, &api_key).await;
            let _ = tx.send(AppEvent::Wizard(res)).await;
        });
    }

    /// Persist the provider the wizard produced and make it active.
    fn finish_wizard(&mut self, provider: agent::Provider) {
        let mut pc = agent::ProvidersConfig::load();
        pc.upsert(provider.clone());
        match pc.save() {
            Ok(_) => self.system_msg(format!(
                "provider `{}` ready · model `{}`",
                provider.name, provider.model
            )),
            Err(e) => self.system_msg(format!("save failed: {e}")),
        }
        self.provider = provider;
        self.wizard = None;
        self.dirty = true;
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

    /// Render the header: ASCII art logo on the left, session info box on the right.
    fn draw_header(&self, f: &mut ratatui::Frame, area: Rect) {
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

        if area.width < 60 || area.height < 9 {
            let fallback = Line::from(vec![
                Span::styled("❯ ", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled("xa", Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" (v{version})"), Style::default().fg(theme::TEXT_DIM)),
            ]);
            f.render_widget(Paragraph::new(fallback), area);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(28),
                Constraint::Length(42),
                Constraint::Min(0),
            ])
            .split(area);

        let version_line = format!("v{}", version);
        let version_line_padded = format!("  {}", version_line);
        let logo_lines = vec![
            "  ██╗  ██╗ █████╗",
            "  ╚██╗██╔╝██╔══██╗",
            "   ╚███╔╝ ███████║",
            "   ██╔██╗ ██╔══██║",
            "  ██╔╝ ██╗██║  ██║",
            "  ╚═╝  ╚═╝╚═╝  ╚═╝",
            "",
            "  XA Code Agent",
            &version_line_padded,
        ];

        let logo_spans: Vec<Line> = logo_lines.iter().map(|line| {
            if line.is_empty() {
                Line::from("")
            } else if line.contains("██") || line.contains("╚") || line.contains("╔") || line.contains("╝") || line.contains("╗") || line.contains("═") {
                Line::from(Span::styled(*line, Style::default().fg(theme::ACCENT)))
            } else if line.contains("XA Code Agent") {
                Line::from(Span::styled(*line, Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD)))
            } else {
                Line::from(Span::styled(*line, Style::default().fg(theme::TEXT_DIM)))
            }
        }).collect();

        let logo = Paragraph::new(logo_spans);
        f.render_widget(logo, chunks[0]);

        let box_w = 42usize;
        let cw = box_w.saturating_sub(2);

        let truncate = |s: &str, max: usize| -> String {
            let sw = unicode_width::UnicodeWidthStr::width(s);
            if sw <= max {
                s.to_string()
            } else {
                let mut t = String::new();
                let mut w = 0;
                for ch in s.chars() {
                    let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if w + ch_w + 3 > max {
                        t.push_str("...");
                        break;
                    }
                    t.push(ch);
                    w += ch_w;
                }
                t
            }
        };

        let border_style = Style::default().fg(theme::BORDER);
        let label_style = Style::default().fg(theme::TEXT_DIM);
        let value_style = Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD);

        let max_val_w = cw.saturating_sub(14);
        let model_display = truncate(&model, max_val_w);
        let cwd_display = truncate(&cwd, max_val_w);

        let keys = ["Model", "Workspace", "Permission", "Context"];
        let max_key_w = keys.iter().map(|k| unicode_width::UnicodeWidthStr::width(*k)).max().unwrap_or(0);

        let kv_line = |key: &str, value: &str| -> Line<'static> {
            let key_w = unicode_width::UnicodeWidthStr::width(key);
            let val_w = unicode_width::UnicodeWidthStr::width(value);
            let key_pad = max_key_w.saturating_sub(key_w);
            let gap = 2;
            let content_w = 1 + max_key_w + gap + val_w;
            let line_pad = cw.saturating_sub(content_w);
            Line::from(vec![
                Span::styled("│", border_style),
                Span::styled(" ", border_style),
                Span::styled(key.to_string(), label_style),
                Span::styled(" ".repeat(key_pad + gap), border_style),
                Span::styled(value.to_string(), value_style),
                Span::styled(format!("{}│", " ".repeat(line_pad)), border_style),
            ])
        };
        let blank_line = || -> Line<'static> {
            Line::from(vec![
                Span::styled("│", border_style),
                Span::styled(" ".repeat(cw), border_style),
                Span::styled("│", border_style),
            ])
        };

        let title = "─ Session ";
        let title_w = unicode_width::UnicodeWidthStr::width(title);
        let top_fill = cw.saturating_sub(title_w);
        let top_border = format!("╭{}{}╮", title, "─".repeat(top_fill));

        let session_lines: Vec<Line> = vec![
            Line::from(vec![Span::styled(top_border, border_style)]),
            kv_line("Model", &model_display),
            kv_line("Workspace", &cwd_display),
            kv_line("Permission", "Auto"),
            kv_line("Context", "128k tokens"),
            Line::from(vec![Span::styled(format!("╰{}╯", "─".repeat(cw)), border_style)]),
        ];

        let session = Paragraph::new(session_lines);
        f.render_widget(session, chunks[1]);
    }

    /// Render the short codex-style tip line beneath the header box.
    fn draw_tip(&self, f: &mut ratatui::Frame, area: Rect) {
        let tip = Line::from(vec![
            Span::styled("Tip: ", Style::default().fg(theme::TEXT_DIM)),
            Span::styled(
                "type `/` for the command menu, or just start chatting.",
                Style::default().fg(theme::TEXT),
            ),
        ]);
        f.render_widget(Paragraph::new(tip), area);
    }

    /// Claude-Code-style activity row above the input: Waiting / Thinking / …
    /// with an elapsed timer that restarts whenever the status changes.
    fn draw_activity(&self, f: &mut ratatui::Frame, area: Rect, phase: f32) {
        if area.height == 0 {
            return;
        }
        let label = if let Some(l) = self.stream_phase.label() {
            l.to_string()
        } else if !self.status.is_empty() {
            self.status.clone()
        } else {
            return;
        };

        let base = match self.stream_phase {
            StreamPhase::Error => theme::ERROR,
            StreamPhase::Thinking => theme::TEXT_DIM,
            StreamPhase::Responding => theme::TEXT_DIM,
            StreamPhase::RunningTool => theme::WARNING,
            StreamPhase::Waiting => theme::TEXT_DIM,
            StreamPhase::Idle => theme::TEXT_DIM,
        };

        let spinner_frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let elapsed_ms = self.shimmer_start.elapsed().as_millis() as f32;
        let spinner_idx = ((elapsed_ms / 80.0) as usize) % spinner_frames.len();
        let spinner = spinner_frames[spinner_idx];

        let mut spans = vec![Span::styled("  ", Style::default().fg(theme::TEXT_DIM))];
        if self.stream_phase.is_active() && self.stream_phase != StreamPhase::Error {
            spans.push(Span::styled(
                format!("{} ", spinner),
                Style::default().fg(theme::TEXT),
            ));
            spans.extend(shimmer_spans_to(
                &label,
                base,
                Color::White,
                phase,
            ));
        } else {
            spans.push(Span::styled(label, Style::default().fg(base)));
        }
        if self.stream_phase.is_active() {
            spans.push(Span::styled(
                format!("  {}", self.phase_elapsed_label()),
                Style::default().fg(theme::TEXT_DIM),
            ));
        }
        if self.streaming {
            spans.push(Span::styled(
                "  · esc to interrupt",
                Style::default().fg(theme::TEXT_DIM),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn total_height(&self, width: u16) -> u16 {
        self.cells.iter().map(|c| c.desired_height(width)).sum()
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        self.dirty = false;
        let area = f.area();

        // Activity strip sits *above* the input (Claude Code): Waiting /
        // Thinking / Responding. Footer under the input keeps model meta.
        let show_activity = self.stream_phase.is_active() || !self.status.is_empty();
        let activity_h: u16 = if show_activity { 1 } else { 0 };

        // Pre-compute the layout with a provisional height so we know the input
        // width available for soft-wrapping.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),  // padding above activity
                Constraint::Length(activity_h),
                Constraint::Length(1),  // padding below activity
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);
        self.input_area_width = chunks[4].width;

        // Build the soft-wrapped composer copy now that we know the width, so
        // the input block can grow vertically with the wrapped line count.
        let wrapped = self.wrapped_input();
        let input_lines = wrapped.lines().len().max(1) as u16;
        // One blank padding row above and below the text; grows with content.
        let input_h = (input_lines + 2).clamp(3, 14);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),  // padding above activity
                Constraint::Length(activity_h),
                Constraint::Length(1),  // padding below activity
                Constraint::Length(input_h),
                Constraint::Length(1),
            ])
            .split(area);
        self.input_area_width = chunks[4].width;

        let view = chunks[0];
        let activity_area = chunks[2];
        let input_area = chunks[4];
        let footer_area = chunks[5];
        let ctx = RenderContext {
            shimmer_phase: shimmer_phase(self.shimmer_start, 1.8),
        };

        // The header banner + tip are part of the scroll, not pinned above it:
        // while the view is pinned to the very top they are shown, but once the
        // user scrolls down into the transcript they scroll away and the full
        // height is handed to the conversation.
        const HEADER_H: u16 = 9;
        const TIP_H: u16 = 2;
        const PRE: u16 = HEADER_H + TIP_H;

        let total_cells = self.total_height(view.width);

        // First pass assumes the banner is shown to decide whether we're at the
        // top, then recompute the true scroll range for the chosen layout.
        let usable_with_header = view.height.saturating_sub(PRE);
        let max_with_header = total_cells.saturating_sub(usable_with_header);
        let scroll_candidate = if self.auto_scroll {
            max_with_header
        } else {
            self.scroll.min(max_with_header)
        };
        let show_header = scroll_candidate == 0;

        let (transcript, header_area, tip_area) = if show_header {
            (
                Rect {
                    x: view.left(),
                    y: view.top() + PRE,
                    width: view.width,
                    height: usable_with_header,
                },
                Rect {
                    x: view.left(),
                    y: view.top(),
                    width: view.width,
                    height: HEADER_H,
                },
                Rect {
                    x: view.left(),
                    y: view.top() + HEADER_H,
                    width: view.width,
                    height: TIP_H,
                },
            )
        } else {
            (view, Rect::default(), Rect::default())
        };

        if show_header {
            self.draw_header(f, header_area);
            self.draw_tip(f, tip_area);
        }

        // Virtual scroll: compute per-cell heights, clip to viewport.
        let total = total_cells;
        let view_h = transcript.height;
        let max_scroll = total.saturating_sub(view_h);
        // One blank row of breathing room between the last line (thinking
        // indicator, streaming output, …) and the input composer, so content
        // never butts right up against the input bar at the bottom (DESIGN §4).
        const BOTTOM_PAD: u16 = 1;
        // When the transcript actually scrolls, allow scrolling one extra row
        // past the end so that blank row is always reserved at the bottom.
        let pad = if max_scroll > 0 { BOTTOM_PAD } else { 0 };
        // Auto-scroll keeps the newest content pinned to the bottom, so the
        // streaming output stays visible as it grows. Any manual scroll turns
        // it off (below) and we then respect the explicit offset.
        let scroll = if self.auto_scroll {
            max_scroll + pad
        } else {
            self.scroll.min(max_scroll + pad)
        };
        self.scroll = scroll; // reconcile

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
                    // How many of this cell's own leading rows sit above the
                    // viewport. Passing `skip` keeps scrolling unified: the
                    // visible slice continues seamlessly from the cell above
                    // instead of each cell restarting at its own first row.
                    let skip = (-y).max(0) as u16;
                    if let Some(bg) = c.bg() {
                        f.buffer_mut().set_style(cell_area, Style::default().bg(bg));
                    }
                    c.render(cell_area, skip, f.buffer_mut(), &ctx);
                }
            }
            y += h;
            if y >= view_h as i32 {
                break;
            }
        }

        // Activity strip directly above the input (Claude Code style).
        self.draw_activity(f, activity_area, ctx.shimmer_phase);

        // Input composer: borderless grey block (DESIGN §4) with a `❯ ` lead
        // matching UserCell. Pad left enough for the icon so typed text lines
        // up under (and with) the transcript user messages.
        let input_bg = theme::INPUT_BG;
        let input_block = Block::default()
            .borders(Borders::NONE)
            .padding(Padding::new(1 + USER_LEAD_COLS, 1, 1, 1))
            .style(Style::default().bg(input_bg));
        let mut wrapped = wrapped;
        wrapped.set_block(input_block);
        // Visible block cursor.
        wrapped.set_cursor_style(
            Style::default()
                .fg(theme::BG)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        );
        wrapped.set_cursor_line_style(Style::default().bg(input_bg));
        // Placeholder hint when empty and not streaming.
        if self.input.lines().first().map(|l| l.is_empty()).unwrap_or(true) && !self.streaming {
            wrapped.set_placeholder_text("Type a message, or / for commands…");
            wrapped.set_placeholder_style(Style::default().fg(theme::TEXT_DIM).bg(input_bg));
        }
        // Render the soft-wrapped copy (already built above) so long input
        // grows vertically instead of overflowing; the editable buffer itself
        // is never mutated.
        f.render_widget(&wrapped, input_area);

        // Paint the shared `❯` lead in the left pad of the first content row
        // (same glyph + style family as UserCell).
        if input_area.height > 2 && input_area.width > 1 + USER_LEAD_COLS {
            let lead_style = Style::default()
                .fg(theme::INPUT_LEAD)
                .bg(input_bg)
                .add_modifier(Modifier::BOLD);
            f.buffer_mut().set_stringn(
                input_area.left() + 1,
                input_area.top() + 1,
                USER_PROMPT,
                USER_PROMPT.chars().count(),
                lead_style,
            );
        }

        // Footer: dim grey throughout, segments split with • .
        let mut footer_spans = vec![
            Span::styled(
                format!(" {}", self.provider.model),
                Style::default().fg(theme::FOOTER),
            ),
            Span::styled(" • ", Style::default().fg(theme::FOOTER)),
            Span::styled(
                self.provider.name.clone(),
                Style::default().fg(theme::FOOTER),
            ),
        ];
        if !self.queued_inputs.is_empty() {
            footer_spans.push(Span::styled(" • ", Style::default().fg(theme::FOOTER)));
            footer_spans.push(Span::styled(
                format!("{} queued", self.queued_inputs.len()),
                Style::default().fg(theme::FOOTER),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(footer_spans)), footer_area);

        // Slash popup overlay (DESIGN §5).
        if self.slash_mode {
            let filtered = self.filtered_slash();
            let popup_h = (filtered.len() as u16 + 2).clamp(3, 12);
            let popup_w = 46.min(input_area.width);
            let popup_area = Rect {
                x: input_area.left(),
                y: (input_area.top().saturating_sub(popup_h)).max(0),
                width: popup_w,
                height: popup_h,
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::ACCENT))
                .title(Span::styled(
                    format!(" /{:<1$} ", self.slash_query, 10),
                    Style::default().fg(theme::ACCENT),
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
                    Style::default()
                        .fg(theme::TEXT)
                        .bg(theme::SELECT_BG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT_DIM)
                };
                let line = Line::from(vec![
                    Span::styled(format!(" {:<10}", cmd.name), style),
                    Span::styled(format!(" {:<28}", cmd.desc), style),
                ]);
                f.render_widget(
                    Paragraph::new(line),
                    Rect {
                        x: inner.left(),
                        y,
                        width: inner.width,
                        height: 1,
                    },
                );
                y += 1;
            }
        }

        // Setup wizard overlay (codex-style provider/model selection modal).
        // Drawn last so it sits on top of everything.
        if let Some(wizard) = &self.wizard {
            wizard.draw(f, area);
        }
    }
}

/// Extract the file `path` and optional read `offset`/`limit` from a tool's
/// JSON arguments, for the `← Edit` / `→ Read` card summaries.
fn tool_path_window(arguments: &str) -> (Option<String>, Option<usize>, Option<usize>) {
    let v: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(_) => return (None, None, None),
    };
    let map = match v.as_object() {
        Some(m) => m,
        None => return (None, None, None),
    };
    let path = map
        .get("path")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let num = |k: &str| map.get(k).and_then(|n| n.as_u64()).map(|n| n as usize);
    (path, num("offset"), num("limit"))
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
                    // Strip any persisted think tags so resume matches live turns.
                    let mut filter = ThinkFilter::new();
                    let (visible, _) = filter.feed(&m.content);
                    let tail = filter.finish();
                    let mut text = visible;
                    text.push_str(&tail);
                    let mut tc = ThinkingCell::new();
                    if !text.is_empty() {
                        tc.add_text(&text);
                    }
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
                    AppEvent::Wizard(res) => {
                        if let Some(w) = &mut app.wizard {
                            w.on_fetch_result(res);
                        }
                        app.dirty = true;
                    }
                }
            }
            _ = tick.tick() => {
                // Only redraw when something is animating.
                if app.streaming || app.slash_mode || app.wizard.is_some() {
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
