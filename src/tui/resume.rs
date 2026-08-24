//! Compact session picker for `xa resume`.
//!
//! Default view shows sessions from the current workspace directory
//! (matching Codex-like behavior). A `Global` tab lets the user see all
//! sessions regardless of workspace.

use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Terminal,
};

use crate::session::{self, SessionSummary, normalize_workspace};
use crate::tui::theme;

/// Tabs available in the session picker.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    /// Sessions whose saved workspace matches the current dir.
    Workspace,
    /// All saved sessions.
    Global,
}

/// Open the `xa resume` picker and return the selected session id.
///
/// `workspace` is the current working directory; sessions whose saved
/// workspace matches it are shown in the default (workspace-local) tab.
pub fn pick_session(workspace: &str) -> io::Result<Option<String>> {
    let _crash_guard = super::crash::TuiGuard::enter();
    let result = pick_session_inner(workspace);
    if let Err(error) = &result {
        super::crash::report_error(error);
    }
    result
}

fn pick_session_inner(workspace: &str) -> io::Result<Option<String>> {
    let all: Vec<SessionSummary> = session::list_summaries();
    if all.is_empty() {
        println!("No saved sessions yet.");
        return Ok(None);
    }

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;
    terminal::enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_picker(&mut terminal, workspace, all);

    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::cursor::Show)?;
    terminal.show_cursor()?;
    result
}

fn run_picker(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    workspace: &str,
    all: Vec<SessionSummary>,
) -> io::Result<Option<String>> {
    // All sessions read from disk once at startup. We keep them in an Arc<Mutex>
    // so deletions can be applied without re-reading, while still keeping the
    // terminal alive for the whole picker.
    use std::sync::{Arc, Mutex};
    let all = Arc::new(Mutex::new(all));
    let mut selected = 0usize;
    let mut tab = Tab::Workspace;
    let mut confirming_delete = false;

    loop {
        let all_c = all.lock().unwrap();
        let workspace_sessions: Vec<SessionSummary> = all_c
            .iter()
            .filter(|s| s.workspace.as_deref() == Some(workspace))
            .cloned()
            .collect();
        let sessions = match tab {
            Tab::Workspace => &workspace_sessions,
            Tab::Global => &*all_c,
        };

        terminal.draw(|frame| draw(frame, sessions, selected, tab, confirming_delete))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if confirming_delete {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let id = sessions[selected].id.clone();
                    drop(all_c);
                    session::delete(&id)?;
                    if all.lock().unwrap().is_empty() {
                        return Ok(None);
                    }
                    let len = all.lock().unwrap().len();
                    selected = selected.min(len - 1);
                    confirming_delete = false;
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    confirming_delete = false;
                }
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(sessions.len().saturating_sub(1))
            }
            KeyCode::Home => selected = 0,
            KeyCode::End => selected = sessions.len().saturating_sub(1),
            KeyCode::Enter => return Ok(Some(sessions[selected].id.clone())),
            KeyCode::Char('d') => confirming_delete = true,
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            // Left/Right arrow to switch tabs
            KeyCode::Left => {
                tab = Tab::Workspace;
                selected = 0;
            }
            KeyCode::Right => {
                tab = Tab::Global;
                selected = 0;
            }
            _ => {}
        }
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    sessions: &[SessionSummary],
    selected: usize,
    tab: Tab,
    confirming_delete: bool,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(theme::t().bg)), area);
    let rows = area.height.saturating_sub(8).max(1) as usize;
    let start = selected.saturating_sub(rows.saturating_sub(1));
    let end = (start + rows).min(sessions.len());
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(2),   // heading
            Constraint::Length(1),   // tab bar
            Constraint::Min(1),      // session list
            Constraint::Length(2),   // footer
        ])
        .split(area);

    let heading = Paragraph::new(vec![
        Line::from(Span::styled(
            "Resume a session",
            Style::default().fg(theme::t().text).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Select a previous conversation",
            Style::default().fg(theme::t().text_dim),
        )),
    ]);
    frame.render_widget(heading, sections[0]);

    // Tab bar: [Workspace] / [Global]
    let ws_active = tab == Tab::Workspace;
    let gl_active = tab == Tab::Global;
    let ws_label = if ws_active { "Workspace" } else { "Workspace" };
    let gl_label = if gl_active { "Global" } else { "Global" };
    let tab_text = Line::from(vec![
        Span::styled(
            format!("{} ", ws_label),
            Style::default().fg(if ws_active { theme::t().accent } else { theme::t().text_dim }),
        ),
        Span::styled(
            gl_label.to_string(),
            Style::default().fg(if gl_active { theme::t().accent } else { theme::t().text_dim }),
        ),
    ]);
    frame.render_widget(Paragraph::new(tab_text), sections[1]);

    let current_ws = std::env::current_dir()
        .map(|p| normalize_workspace(&p))
        .unwrap_or_default();
    let mut lines = Vec::with_capacity(end - start);
    for (index, summary) in sessions[start..end].iter().enumerate() {
        let index = start + index;
        let active = index == selected;
        let title = if summary.title == "untitled" {
            "Untitled session"
        } else {
            &summary.title
        };
        let prefix = if active { "›" } else { " " };
        // Show a small tag for sessions from a different workspace.
        let ws_tag = if let Some(ws) = &summary.workspace {
            if ws != &current_ws {
                format!(" · other")
            } else {
                String::new()
            }
        } else {
            " · no workspace".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{prefix} {:<9}", session::relative_time(summary.updated)),
                Style::default()
                    .fg(if active { theme::t().accent } else { theme::t().text_dim })
                    .bg(if active { theme::t().select_bg } else { theme::t().bg }),
            ),
            Span::styled(
                format!("{}{}", title, ws_tag),
                Style::default()
                    .fg(theme::t().text)
                    .bg(if active { theme::t().select_bg } else { theme::t().bg })
                    .add_modifier(if active { Modifier::BOLD } else { Modifier::empty() }),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), sections[2]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ navigate  ·  Enter resume  ·  d delete  ·  Esc cancel",
            Style::default().fg(theme::t().text_dim),
        ))),
        sections[3],
    );
    if confirming_delete {
        draw_delete_confirmation(frame, &sessions[selected]);
    }
}

fn draw_delete_confirmation(frame: &mut ratatui::Frame, session: &SessionSummary) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(64).max(1);
    let height = area.height.min(7).max(1);
    let dialog = ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let title = if session.title == "untitled" {
        "Untitled session"
    } else {
        &session.title
    };
    let text = format!(
        "Delete this session permanently?\n\n{title}\n\nEnter / y: delete   Esc / n: cancel"
    );
    frame.render_widget(Clear, dialog);
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(theme::t().text).bg(theme::t().bg))
            .block(
                Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .title(" Delete session ")
                    .style(Style::default().fg(theme::t().accent).bg(theme::t().bg)),
            ),
        dialog,
    );
}
