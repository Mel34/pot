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
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
    Terminal,
};

use crate::cookbook::Recipe;

pub fn run(recipes: Vec<Recipe>) -> io::Result<()> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut selected = 0;
    let mut expanded = false;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let block = Block::default()
                .title(" POT — The Cookbook ")
                .borders(Borders::ALL);

            let mut items = Vec::new();

            for (index, recipe) in recipes.iter().enumerate() {
                let marker = if index == selected && expanded {
                    "▼"
                } else {
                    "▶"
                };

                items.push(ListItem::new(format!(
                    "{marker} {}  ({})",
                    recipe.name,
                    recipe.patches.len()
                )));

                if index == selected && expanded {
                    for patch in &recipe.patches {
                        let name = patch
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("<invalid filename>");

                        items.push(ListItem::new(format!("    [ ] {name}")));
                    }
                }
            }

            let list = List::new(items)
                .block(block)
                .highlight_style(Style::default().add_modifier(Modifier::BOLD));

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(2),
                ])
                .split(area);

            let mut state = ListState::default();
            state.select(Some(selected));

            frame.render_stateful_widget(list, chunks[0], &mut state);

            let help = ListItem::new("↑↓ Navigate    Enter Expand    q Quit");
            frame.render_widget(
                List::new(vec![help]),
                chunks[1],
            );
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,

                    KeyCode::Up => {
                        if selected > 0 {
                            selected -= 1;
                            expanded = false;
                        }
                    }

                    KeyCode::Down => {
                        if selected + 1 < recipes.len() {
                            selected += 1;
                            expanded = false;
                        }
                    }

                    KeyCode::Enter => {
                        expanded = !expanded;
                    }

                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
