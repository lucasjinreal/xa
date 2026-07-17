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
    let sessions = session::list_summaries();
    if sessions.is_empty() {
        println!("No saved sessions yet.");
        return Ok(None);
    }

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;
    terminal::enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_picker(&mut terminal, &sessions);

    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::cursor::Show)?;
    terminal.show_cursor()?;
    result
}

fn run_picker(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    sessions: &[SessionSummary],
) -> io::Result<Option<String>> {
    let mut selected = 0usize;
    loop {
        terminal.draw(|frame| draw(frame, sessions, selected))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(sessions.len() - 1),
            KeyCode::Home => selected = 0,
            KeyCode::End => selected = sessions.len() - 1,
            KeyCode::Enter => return Ok(Some(sessions[selected].id.clone())),
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            _ => {}
        }
    }
}

fn draw(frame: &mut ratatui::Frame, sessions: &[SessionSummary], selected: usize) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(theme::BG)), area);
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
            Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Select a previous conversation",
            Style::default().fg(theme::TEXT_DIM),
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
                    .fg(if active { theme::ACCENT } else { theme::TEXT_DIM })
                    .bg(if active { theme::SELECT_BG } else { theme::BG }),
            ),
            Span::styled(
                title.to_string(),
                Style::default()
                    .fg(theme::TEXT)
                    .bg(if active { theme::SELECT_BG } else { theme::BG })
                    .add_modifier(if active { Modifier::BOLD } else { Modifier::empty() }),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), sections[1]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑↓ navigate  ·  Enter resume  ·  Esc cancel",
            Style::default().fg(theme::TEXT_DIM),
        ))),
        sections[2],
    );
}
