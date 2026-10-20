use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

use crate::theme::*;

pub fn draw_dialog_box(f: &mut Frame, width: u16, height: u16) -> Rect {
    let screen = f.area();
    let w = width.min(screen.width.saturating_sub(4));
    let h = height.min(screen.height.saturating_sub(2));

    let x = (screen.width.saturating_sub(w)) / 2;
    let y = (screen.height.saturating_sub(h)) / 2;

    let shadow_rect = Rect {
        x: (x + 2).min(screen.width - 1),
        y: (y + 1).min(screen.height - 1),
        width: w.min(screen.width.saturating_sub(x + 2)),
        height: h.min(screen.height.saturating_sub(y + 1)),
    };
    f.render_widget(
        Block::default().style(Style::default().bg(NC_BLACK)),
        shadow_rect,
    );

    let dialog_rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, dialog_rect);
    dialog_rect
}

pub fn draw_help_dialog(f: &mut Frame) {
    let dialog_rect = draw_dialog_box(f, 76, 21);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(NC_WHITE))
        .style(Style::default().bg(NC_BLUE).fg(NC_WHITE))
        .title(Span::styled(
            " [ POT — Help & Shortcuts ] ",
            Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD),
        ));

    let help_entries = [
        (
            "Tab / ← / →",
            "Switch focus between Left (Recipes) and Right pane",
        ),
        ("↑ / ↓ / j / k", "Move cursor up / down in active pane"),
        (
            "Home / End",
            "Jump to first / last item (or log top / bottom)",
        ),
        ("PgUp / PgDn", "Move cursor or scroll log by 10/15 rows"),
        (
            "Space",
            "Toggle patch selection [✓] (or toggle log auto-scroll)",
        ),
        (
            "Enter",
            "Left: Prepare → Cook → Install | Right: Verify patch",
        ),
        ("F1", "Show / hide this Help table"),
        ("F2", "Cook recipe (Left) / Verify all patches (Right)"),
        ("F3", "Toggle Right pane: Patches ⇄ Live Build Log"),
        ("F4", "Reset prepared sources for selected recipe"),
        (
            "F5",
            "Refresh databases, then check recipes for available updates",
        ),
        (
            "F8 / a",
            "Abort active preparation, compilation, or install",
        ),
        ("F10 / q", "Quit POT"),
    ];

    let mut lines = vec![Line::from("")];
    for (key, desc) in help_entries {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<15}", key),
                Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD),
            ),
            Span::styled(": ", Style::default().fg(NC_CYAN)),
            Span::styled(desc, Style::default().fg(NC_WHITE)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("                        "),
        Span::styled(
            "[ Press ESC or F1 to close ]",
            Style::default().fg(NC_CYAN).add_modifier(Modifier::BOLD),
        ),
    ]));

    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, dialog_rect);
}

pub fn draw_message_dialog(f: &mut Frame, title: &str, msg: &str) {
    let screen = f.area();
    let width = 68.min(screen.width.saturating_sub(4));
    let height = 12.min(screen.height.saturating_sub(2));

    let dialog_rect = draw_dialog_box(f, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Red))
        .style(Style::default().bg(NC_BLUE).fg(NC_WHITE))
        .title(Span::styled(
            format!(" [ {title} ] "),
            Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD),
        ));

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(msg, Style::default().fg(NC_WHITE))),
        Line::from(""),
        Line::from(Span::styled(
            "[ Press ESC or Enter to close ]",
            Style::default()
                .bg(NC_CYAN)
                .fg(NC_BLACK)
                .add_modifier(Modifier::BOLD),
        )),
    ];

    let p = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });

    f.render_widget(p, dialog_rect);
}

pub fn first_start(title: &str, message: &str, hostname: &str) -> io::Result<bool> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut selected = true;

    loop {
        terminal.draw(|f| {
            let area = f.area();
            f.render_widget(Block::default().style(Style::default().bg(NC_BLUE)), area);

            let dialog_rect = draw_dialog_box(f, 54, 12);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(NC_WHITE))
                .style(Style::default().bg(NC_BLUE).fg(NC_WHITE))
                .title(Span::styled(
                    format!(" [ {title} ] "),
                    Style::default().fg(NC_YELLOW).add_modifier(Modifier::BOLD),
                ));

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(2),
                    Constraint::Length(1),
                    Constraint::Length(2),
                    Constraint::Length(1),
                ])
                .split(block.inner(dialog_rect));

            f.render_widget(block, dialog_rect);

            let msg_p = Paragraph::new(message)
                .style(Style::default().fg(NC_WHITE))
                .alignment(Alignment::Center);

            let host_p = Paragraph::new(hostname)
                .style(Style::default().fg(NC_CYAN).add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center);

            let btn_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Fill(1),
                    Constraint::Length(10),
                    Constraint::Length(4),
                    Constraint::Length(10),
                    Constraint::Fill(1),
                ])
                .split(chunks[3]);

            let yes_style = if selected {
                Style::default()
                    .bg(NC_CYAN)
                    .fg(NC_BLACK)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(NC_GRAY).fg(NC_BLACK)
            };

            let no_style = if !selected {
                Style::default()
                    .bg(NC_CYAN)
                    .fg(NC_BLACK)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(NC_GRAY).fg(NC_BLACK)
            };

            f.render_widget(msg_p, chunks[1]);
            f.render_widget(host_p, chunks[2]);
            f.render_widget(
                Paragraph::new("  YES  ")
                    .style(yes_style)
                    .alignment(Alignment::Center),
                btn_chunks[1],
            );
            f.render_widget(
                Paragraph::new("  NO   ")
                    .style(no_style)
                    .alignment(Alignment::Center),
                btn_chunks[3],
            );
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                        selected = !selected;
                    }
                    KeyCode::Enter => {
                        disable_raw_mode()?;
                        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                        terminal.show_cursor()?;
                        return Ok(selected);
                    }
                    KeyCode::Esc => {
                        disable_raw_mode()?;
                        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                        terminal.show_cursor()?;
                        return Ok(false);
                    }
                    _ => {}
                }
            }
        }
    }
}
