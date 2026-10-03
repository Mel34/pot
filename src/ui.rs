use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    widgets::{Block, Borders, List, ListItem},
    Terminal,
};

use crate::cookbook::Recipe;

pub fn run(recipes: Vec<Recipe>) -> io::Result<()> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let block = Block::default()
                .title(" POT — The Cookbook ")
                .borders(Borders::ALL);

            let items: Vec<ListItem> = recipes
                .iter()
                .map(|recipe| {
                    let label = format!("{}  ({})", recipe.name, recipe.patches.len());
                    ListItem::new(label)
                })
                .collect();

            let list = List::new(items).block(block);

            frame.render_widget(list, area);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
