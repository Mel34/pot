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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};

use crate::cookbook::Recipe;

#[derive(Clone, Copy)]
enum MenuItem {
    Recipe(usize),
    Patch(usize, usize),
}

pub fn run(mut recipes: Vec<Recipe>) -> io::Result<()> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut expanded = vec![false; recipes.len()];
    let mut selected = 0;

    loop {
        let mut menu_items = Vec::new();

        for (recipe_index, recipe) in recipes.iter().enumerate() {
            menu_items.push(MenuItem::Recipe(recipe_index));

            if expanded[recipe_index] {
                for patch_index in 0..recipe.patches.len() {
                    menu_items.push(MenuItem::Patch(recipe_index, patch_index));
                }
            }
        }

        if selected >= menu_items.len() {
            selected = menu_items.len().saturating_sub(1);
        }

        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(area);

            let block = Block::default()
                .title(" POT — The Cookbook ")
                .borders(Borders::ALL);

            let items: Vec<ListItem> = menu_items
                .iter()
                .map(|item| match item {
                    MenuItem::Recipe(recipe_index) => {
                        let recipe = &recipes[*recipe_index];

                        let marker = if expanded[*recipe_index] {
                            "▼"
                        } else {
                            "▶"
                        };

                        ListItem::new(format!(
                            "{marker} {}  ({})",
                            recipe.name,
                            recipe.patches.len()
                        ))
                    }

                    MenuItem::Patch(recipe_index, patch_index) => {
                        let patch = &recipes[*recipe_index].patches[*patch_index];

                        let name = patch
                            .path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("<invalid filename>");

                        let marker = if patch.selected {
                            "[✓]"
                        } else {
                            "[ ]"
                        };

                        ListItem::new(format!("    {marker} {name}"))
                    }
                })
                .collect();

            let list = List::new(items)
                .block(block)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

            let mut state = ListState::default();
            state.select(Some(selected));

            frame.render_stateful_widget(list, chunks[0], &mut state);

            let help = Paragraph::new(
                "↑↓ Navigate    Enter Expand    Space Select    q Quit",
            );

            frame.render_widget(help, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,

                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                    }

                    KeyCode::Down => {
                        if selected + 1 < menu_items.len() {
                            selected += 1;
                        }
                    }

                    KeyCode::Enter => {
                        if let Some(MenuItem::Recipe(recipe_index)) =
                            menu_items.get(selected)
                        {
                            expanded[*recipe_index] = !expanded[*recipe_index];

                            if !expanded[*recipe_index] {
                                selected = selected.saturating_sub(1);
                            }
                        }
                    }

                    KeyCode::Char(' ') => {
                        if let Some(MenuItem::Patch(recipe_index, patch_index)) =
                            menu_items.get(selected)
                        {
                            let recipe = &mut recipes[*recipe_index];
                            let patch = &mut recipe.patches[*patch_index];

                            patch.selected = !patch.selected;

                            recipe.save_selection()?;
                        }
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
