//! Compact session picker for `xa resume`.

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

use crate::session::{self, SessionSummary};
use crate::tui::theme;

/// Open the `xa resume` picker and return the selected session id.
pub fn pick_session() -> io::Result<Option<String>> {
    let _crash_guard = super::crash::TuiGuard::enter();
    let result = pick_session_inner();
    if let Err(error) = &result {
        super::crash::report_error(error);
    }
    result
}

fn pick_session_inner() -> io::Result<Option<String>> {
    let mut sessions = session::list_summaries();
    if sessions.is_empty() {
        println!("No saved sessions yet.");
        return Ok(None);
    }

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;
    terminal::enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_picker(&mut terminal, &mut sessions);

    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::cursor::Show)?;
    terminal.show_cursor()?;
    result
}

fn run_picker(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    sessions: &mut Vec<SessionSummary>,
) -> io::Result<Option<String>> {
    let mut selected = 0usize;
    let mut confirming_delete = false;
    loop {
        terminal.draw(|frame| draw(frame, sessions, selected, confirming_delete))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if confirming_delete {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    session::delete(&sessions[selected].id)?;
                    sessions.remove(selected);
                    if sessions.is_empty() {
                        return Ok(None);
                    }
                    selected = selected.min(sessions.len() - 1);
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
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(sessions.len() - 1),
            KeyCode::Home => selected = 0,
            KeyCode::End => selected = sessions.len() - 1,
            KeyCode::Enter => return Ok(Some(sessions[selected].id.clone())),
            KeyCode::Char('d') => confirming_delete = true,
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            _ => {}
        }
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    sessions: &[SessionSummary],
    selected: usize,
    confirming_delete: bool,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(theme::t().bg)), area);
    let rows = area.height.saturating_sub(7).max(1) as usize;
    let start = selected.saturating_sub(rows.saturating_sub(1));
    let end = (start + rows).min(sessions.len());
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Length(2), Constraint::Min(1), Constraint::Length(2)])
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
        lines.push(Line::from(vec![
            Span::styled(
                format!("{prefix} {:<9}", session::relative_time(summary.updated)),
                Style::default()
                    .fg(if active { theme::t().accent } else { theme::t().text_dim })
                    .bg(if active { theme::t().select_bg } else { theme::t().bg }),
            ),
            Span::styled(
                title.to_string(),
                Style::default()
                    .fg(theme::t().text)
                    .bg(if active { theme::t().select_bg } else { theme::t().bg })
                    .add_modifier(if active { Modifier::BOLD } else { Modifier::empty() }),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), sections[1]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ navigate  ·  Enter resume  ·  d delete  ·  Esc cancel",
            Style::default().fg(theme::t().text_dim),
        ))),
        sections[2],
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
