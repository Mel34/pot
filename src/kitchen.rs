use std::io;

use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
};

use crate::cookbook::Recipe;

pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    recipe: &Recipe,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);

            let block = Block::default()
                .title(format!(" POT — The Kitchen — {} ", recipe.name))
                .borders(Borders::ALL);

            let body = Paragraph::new("Kitchen ready. Build output will appear here.")
                .block(block);

            frame.render_widget(body, chunks[0]);

            frame.render_widget(
                Paragraph::new("[Esc/q] Return to Cookbook"),
                chunks[1],
            );
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}
