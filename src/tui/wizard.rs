//! In-TUI provider / model setup wizard (codex-style interactive flow).
//!
//! Replaces the old `read_line_paused` line prompt with a clean modal overlay:
//!   1. Select a provider (built-in presets, existing providers, or custom)
//!   2. Optionally enter an API key
//!   3. Models are auto-queried from the endpoint (spinner while loading)
//!   4. Select a model from the returned list (or type a custom one)
//!
//! Navigation is arrow-key driven; Esc steps back / cancels. The async model
//! fetch is owned by `App` (via [`WizardAction::StartFetch`]) so the TUI never
//! blocks.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::agent::{builtin_presets, Provider, ProviderPreset, ProvidersConfig};
use crate::tui::theme;

/// Accent + dim palette — gray + orange (shared with the rest of the TUI).
const ACCENT: Color = theme::ACCENT;
const DIM: Color = theme::TEXT_DIM;
const PLAIN: Color = theme::TEXT;
const SELECT_BG: Color = theme::SELECT_BG;
const BORDER: Color = theme::BORDER;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Where the wizard collects its source list from.
#[derive(Clone)]
enum Source {
    Existing(Provider),
    Preset(ProviderPreset),
    Custom,
}

impl Source {
    fn label(&self) -> String {
        match self {
            Source::Existing(p) => p.name.clone(),
            Source::Preset(p) => p.name.to_string(),
            Source::Custom => "custom".to_string(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Provider,
    CustomName,
    CustomUrl,
    ApiKey,
    Fetching,
    Model,
    CustomModel,
}

/// Result of feeding a key to the wizard. `App` acts on these.
pub enum WizardAction {
    /// Nothing happened (a navigation key with no effect, etc).
    None,
    /// Begin the async model fetch for `endpoint` + `api_key`.
    StartFetch { endpoint: String, api_key: String },
    /// Wizard finished: configure and persist `Provider`.
    Done(Provider),
    /// User cancelled (Esc at the first step).
    Cancel,
}

pub struct Wizard {
    mode: WizardMode,
    step: Step,
    sources: Vec<Source>,
    source_idx: usize,
    /// Draft provider being built up across steps.
    draft: Provider,
    /// True when the chosen source came through the custom name/url path.
    came_from_custom: bool,
    /// Text-buffer step content (api key / custom name / url / model).
    text: String,
    text_label: String,
    /// Fetched models and any fetch error.
    models: Vec<String>,
    model_err: Option<String>,
    model_idx: usize,
    fetching: bool,
    created: Instant,
    message: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum WizardMode {
    Login,
    Models,
}

impl Wizard {
    fn build_sources(include_existing: bool) -> Vec<Source> {
        let mut v = Vec::new();
        if include_existing {
            let cfg = ProvidersConfig::load();
            for p in cfg.list() {
                v.push(Source::Existing(p.clone()));
            }
        }
        for p in builtin_presets() {
            v.push(Source::Preset(p));
        }
        v.push(Source::Custom);
        v
    }

    fn new(mode: WizardMode, preselect: Option<&str>) -> Self {
        // Login mode still shows existing providers so you can update them.
        let sources = Self::build_sources(true);
        let mut w = Wizard {
            mode,
            step: Step::Provider,
            sources,
            source_idx: 0,
            draft: Provider::default(),
            came_from_custom: false,
            text: String::new(),
            text_label: String::new(),
            models: Vec::new(),
            model_err: None,
            model_idx: 0,
            fetching: false,
            created: Instant::now(),
            message: None,
        };
        if let Some(name) = preselect {
            if let Some(i) = w.sources.iter().position(|s| s.label() == name) {
                w.source_idx = i;
            }
        }
        w
    }

    pub fn new_login(preselect: Option<&str>) -> Self {
        Self::new(WizardMode::Login, preselect)
    }

    pub fn new_models(preselect: Option<&str>) -> Self {
        Self::new(WizardMode::Models, preselect)
    }

    /// True while the async model fetch is in flight (drives the spinner).
    pub fn is_fetching(&self) -> bool {
        self.fetching
    }

    /// Run the wizard as a standalone interactive terminal (used by the
    /// `xa login` / `xa models` CLI commands). Mirrors the in-app TUI flow
    /// but owns its own alternate screen + event loop. Returns the configured
    /// provider once persisted, or `None` if the user cancelled.
    pub async fn run_standalone(
        mode: WizardMode,
        preselect: Option<&str>,
    ) -> Result<Option<Provider>, Box<dyn std::error::Error>> {
        use std::io::stdout;
        use std::time::Duration;

        use crossterm::{
            cursor::{Hide, Show},
            event::{self, Event},
            execute,
            terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
        };
        use ratatui::{backend::CrosstermBackend, Terminal};
        use tokio::sync::mpsc;

        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal::enable_raw_mode()?;

        // Keyboard reader (mirrors `app.rs` `run`).
        let (tx, mut rx) = mpsc::channel::<Event>(64);
        tokio::task::spawn_blocking(move || loop {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(e) => {
                        if tx.blocking_send(e).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => continue,
                Err(_) => break,
            }
        });

        let mut wizard = match mode {
            WizardMode::Login => Wizard::new_login(preselect),
            WizardMode::Models => Wizard::new_models(preselect),
        };
        let mut result: Option<Provider> = None;
        let mut dirty = true;
        let mut fetch_rx: Option<mpsc::Receiver<Result<Vec<String>, String>>> = None;

        loop {
            if dirty {
                terminal.draw(|f| wizard.draw(f, f.area())).ok();
                dirty = false;
            }
            tokio::select! {
                Some(ev) = rx.recv() => {
                    if let Event::Key(k) = ev {
                        match wizard.handle_key(k) {
                            WizardAction::None => {}
                            WizardAction::Cancel => break,
                            WizardAction::Done(p) => {
                                result = Some(p);
                                break;
                            }
                            WizardAction::StartFetch { endpoint, api_key } => {
                                let (ftx, frx) = mpsc::channel(4);
                                fetch_rx = Some(frx);
                                tokio::spawn(async move {
                                    let r = crate::agent::fetch_models(&endpoint, &api_key).await;
                                    let _ = ftx.send(r).await;
                                });
                            }
                        }
                        // Always redraw after a keypress so moved selection /
                        // typed text is reflected (arrow keys return `None`).
                        dirty = true;
                    }
                }
                Some(fr) = async { fetch_rx.as_mut().unwrap().recv().await },
                    if fetch_rx.is_some() =>
                {
                    wizard.on_fetch_result(fr);
                    fetch_rx = None;
                    dirty = true;
                }
            }
            // Keep the spinner animating while a fetch is in flight.
            if wizard.is_fetching() {
                dirty = true;
            }
        }

        terminal::disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
        terminal.show_cursor()?;

        if let Some(p) = &result {
            let mut pc = ProvidersConfig::load();
            pc.upsert(p.clone());
            pc.save()?;
        }
        Ok(result)
    }

    /// Apply a finished async model fetch.
    pub fn on_fetch_result(&mut self, res: Result<Vec<String>, String>) {
        self.fetching = false;
        match res {
            Ok(models) if !models.is_empty() => {
                self.models = models;
                self.model_idx = 0;
                self.message = None;
                self.step = Step::Model;
            }
            Ok(_) => {
                self.models = Vec::new();
                self.model_err = Some("No models returned by this endpoint.".into());
                self.step = Step::Model;
            }
            Err(e) => {
                self.models = Vec::new();
                self.model_err = Some(e);
                self.step = Step::Model;
            }
        }
    }

    fn title(&self) -> String {
        match (self.mode, self.step) {
            (_, Step::Provider) => " Select a provider ".into(),
            (_, Step::CustomName) | (_, Step::CustomUrl) => " Custom provider ".into(),
            (_, Step::ApiKey) => " API key ".into(),
            (_, Step::Fetching) => " Loading models ".into(),
            (_, Step::Model) => " Select a model ".into(),
            (_, Step::CustomModel) => " Custom model ".into(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> WizardAction {
        match self.step {
            Step::Provider => self.key_provider(key),
            Step::CustomName | Step::CustomUrl | Step::ApiKey | Step::CustomModel => {
                self.key_text(key)
            }
            Step::Model => self.key_model(key),
            Step::Fetching => {
                if key.code == KeyCode::Esc {
                    return WizardAction::Cancel;
                }
                WizardAction::None
            }
        }
    }

    fn key_provider(&mut self, key: KeyEvent) -> WizardAction {
        match key.code {
            KeyCode::Up => {
                if self.source_idx > 0 {
                    self.source_idx -= 1;
                }
                WizardAction::None
            }
            KeyCode::Down => {
                if self.source_idx + 1 < self.sources.len() {
                    self.source_idx += 1;
                }
                WizardAction::None
            }
            KeyCode::Enter => {
                let src = self.sources[self.source_idx].clone();
                match src {
                    Source::Existing(p) => {
                        self.draft = p.clone();
                        self.came_from_custom = false;
                        self.text = p.api_key.clone();
                        self.text_label = "API key (optional):".into();
                        self.step = Step::ApiKey;
                    }
                    Source::Preset(p) => {
                        self.draft = Provider {
                            name: p.name.to_string(),
                            endpoint: p.base_url.to_string(),
                            ..Default::default()
                        };
                        self.came_from_custom = false;
                        self.text.clear();
                        self.text_label = "API key (optional):".into();
                        self.step = Step::ApiKey;
                    }
                    Source::Custom => {
                        self.came_from_custom = true;
                        self.text.clear();
                        self.text_label = "Provider name:".into();
                        self.step = Step::CustomName;
                    }
                }
                WizardAction::None
            }
            KeyCode::Esc => WizardAction::Cancel,
            _ => WizardAction::None,
        }
    }

    fn key_text(&mut self, key: KeyEvent) -> WizardAction {
        match key.code {
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return WizardAction::None;
                }
                self.text.push(c);
                WizardAction::None
            }
            KeyCode::Backspace => {
                self.text.pop();
                WizardAction::None
            }
            KeyCode::Enter => {
                let val = self.text.trim().to_string();
                match self.step {
                    Step::CustomName => {
                        if val.is_empty() {
                            return WizardAction::None;
                        }
                        self.draft.name = val;
                        self.text.clear();
                        self.text_label = "Base URL (e.g. https://api.openai.com/v1):".into();
                        self.step = Step::CustomUrl;
                    }
                    Step::CustomUrl => {
                        if val.is_empty() {
                            return WizardAction::None;
                        }
                        self.draft.endpoint = val;
                        self.text.clear();
                        self.text_label = "API key (optional):".into();
                        self.step = Step::ApiKey;
                    }
                    Step::ApiKey => {
                        self.draft.api_key = val;
                        self.fetching = true;
                        self.step = Step::Fetching;
                        return WizardAction::StartFetch {
                            endpoint: self.draft.endpoint.clone(),
                            api_key: self.draft.api_key.clone(),
                        };
                    }
                    Step::CustomModel => {
                        if val.is_empty() {
                            return WizardAction::None;
                        }
                        self.draft.model = val;
                        return self.finish();
                    }
                    _ => {}
                }
                WizardAction::None
            }
            KeyCode::Esc => {
                // Step back one screen.
                self.step = match self.step {
                    Step::CustomName => Step::Provider,
                    Step::CustomUrl => Step::CustomName,
                    Step::ApiKey => {
                        if self.came_from_custom {
                            Step::CustomUrl
                        } else {
                            Step::Provider
                        }
                    }
                    Step::CustomModel => Step::Model,
                    _ => Step::Provider,
                };
                WizardAction::None
            }
            _ => WizardAction::None,
        }
    }

    fn key_model(&mut self, key: KeyEvent) -> WizardAction {
        let total = self.models.len() + 1; // + custom entry
        match key.code {
            KeyCode::Up => {
                if self.model_idx > 0 {
                    self.model_idx -= 1;
                }
                WizardAction::None
            }
            KeyCode::Down => {
                if self.model_idx + 1 < total {
                    self.model_idx += 1;
                }
                WizardAction::None
            }
            KeyCode::Enter => {
                if self.model_idx < self.models.len() {
                    self.draft.model = self.models[self.model_idx].clone();
                    self.finish()
                } else {
                    // custom model entry
                    self.text.clear();
                    self.text_label = "Model name:".into();
                    self.step = Step::CustomModel;
                    WizardAction::None
                }
            }
            KeyCode::Esc => {
                self.step = Step::ApiKey;
                WizardAction::None
            }
            _ => WizardAction::None,
        }
    }

    fn finish(&self) -> WizardAction {
        let mut p = self.draft.clone();
        if p.name.is_empty() {
            p.name = "default".into();
        }
        WizardAction::Done(p)
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let width = area.width.min(66).max(40) as u16;
        let height = area.height.min(22).max(10) as u16;
        let x = area.width.saturating_sub(width) / 2;
        let y = area.height.saturating_sub(height) / 2;
        let popup = Rect {
            x,
            y,
            width,
            height,
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(
                self.title(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(popup);
        f.render_widget(block, popup);

        // Lay out [content area] over [footer hint].
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let body = chunks[0];

        match self.step {
            Step::Provider => self.draw_provider(f, body),
            Step::CustomName | Step::CustomUrl | Step::ApiKey | Step::CustomModel => {
                self.draw_text(f, body)
            }
            Step::Fetching => self.draw_fetching(f, body),
            Step::Model => self.draw_model(f, body),
        }

        let hint = Line::from(vec![
            Span::styled("↑/↓", Style::default().fg(ACCENT)),
            Span::styled(" navigate  ", Style::default().fg(DIM)),
            Span::styled("Enter", Style::default().fg(ACCENT)),
            Span::styled(" select  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(ACCENT)),
            Span::styled(" back", Style::default().fg(DIM)),
        ]);
        f.render_widget(
            Paragraph::new(hint).alignment(Alignment::Center),
            chunks[1],
        );
    }

    fn draw_provider(&self, f: &mut Frame, area: Rect) {
        let mut y = area.top();
        for (i, src) in self.sources.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let sel = i == self.source_idx;
            let mut spans = Vec::new();
            if sel {
                spans.push(Span::styled(
                    " › ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled("   ", Style::default()));
            }
            let label = src.label();
            spans.push(Span::styled(
                format!("{:<12}", label),
                Style::default()
                    .fg(if sel { Color::White } else { ACCENT })
                    .add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }),
            ));
            let detail = match src {
                Source::Existing(p) => format!("{}  (current)", p.endpoint),
                Source::Preset(p) => match p.note {
                    Some(n) => format!("{:<52}  ({})", p.base_url, n),
                    None => format!("{:<52}", p.base_url),
                },
                Source::Custom => "your own OpenAI-compatible endpoint".into(),
            };
            spans.push(Span::styled(
                detail,
                Style::default().fg(if sel { PLAIN } else { DIM }),
            ));
            let line = Line::from(spans);
            let style = if sel {
                Style::default().bg(SELECT_BG)
            } else {
                Style::default()
            };
            f.render_widget(
                Paragraph::new(line).style(style),
                Rect {
                    x: area.left(),
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y += 1;
        }
    }

    fn draw_text(&self, f: &mut Frame, area: Rect) {
        let y = area.top();
        // Label line.
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                self.text_label.clone(),
                Style::default().fg(DIM),
            )])),
            Rect {
                x: area.left(),
                y,
                width: area.width,
                height: 1,
            },
        );
        // Input line (echo dots for api key, plain otherwise).
        let shown = if self.step == Step::ApiKey && !self.text.is_empty() {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        };
        let input_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                format!("{}{}", shown, "_"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        if y + 1 < area.bottom() {
            f.render_widget(
                Paragraph::new(input_line),
                Rect {
                    x: area.left(),
                    y: y + 1,
                    width: area.width,
                    height: 1,
                },
            );
        }
    }

    fn draw_fetching(&self, f: &mut Frame, area: Rect) {
        let elapsed = self.created.elapsed().as_millis() as usize;
        let frame = SPINNER[(elapsed / 100) % SPINNER.len()];
        let line = Line::from(vec![
            Span::styled(
                format!(" {frame} "),
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("Querying models at {}", self.draft.endpoint),
                Style::default().fg(PLAIN),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line),
            Rect {
                x: area.left(),
                y: area.top(),
                width: area.width,
                height: 1,
            },
        );
    }

    fn draw_model(&self, f: &mut Frame, area: Rect) {
        let mut y = area.top();
        if let Some(err) = &self.model_err {
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    format!(" ! {err}"),
                    Style::default().fg(Color::Rgb(255, 180, 120)),
                )])),
                Rect {
                    x: area.left(),
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y += 1;
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    "   Enter a model manually, or Esc to go back.",
                    Style::default().fg(DIM),
                )])),
                Rect {
                    x: area.left(),
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y += 1;
        }
        for (i, m) in self.models.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let sel = i == self.model_idx;
            let mut spans = Vec::new();
            spans.push(Span::styled(
                if sel { " › " } else { "   " },
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                m.clone(),
                Style::default()
                    .fg(if sel { Color::White } else { PLAIN })
                    .add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }),
            ));
            let style = if sel {
                Style::default().bg(SELECT_BG)
            } else {
                Style::default()
            };
            f.render_widget(
                Paragraph::new(Line::from(spans)).style(style),
                Rect {
                    x: area.left(),
                    y,
                    width: area.width,
                    height: 1,
                },
            );
            y += 1;
        }
        // Custom model entry.
        if y < area.bottom() {
            let sel = self.model_idx == self.models.len();
            let style = if sel {
                Style::default().bg(SELECT_BG)
            } else {
                Style::default()
            };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if sel { " › " } else { "   " },
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "custom… (enter a model name)",
                        Style::default().fg(if sel { Color::White } else { DIM }),
                    ),
                ]))
                .style(style),
                Rect {
                    x: area.left(),
                    y,
                    width: area.width,
                    height: 1,
                },
            );
        }
    }
}
