use std::io;
use std::path::Path;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::cookbook::{PatchStatus, Recipe};
use crate::{package, prepare};

#[derive(Clone, Copy)]
enum MenuItem {
    Recipe(usize),
    Patch(usize, usize),
}

fn draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    recipes: &[Recipe],
    expanded: &[bool],
    selected: usize,
    menu_items: &[MenuItem],
    busy_message: Option<&str>,
) -> io::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
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

                    let marker = if patch.selected { "[✓]" } else { "[ ]" };

                    let status = match patch.status {
                        PatchStatus::Unknown => "?",
                        PatchStatus::Clean => "✓",
                        PatchStatus::Failed => "✗",
                    };

                    ListItem::new(format!("    {marker} {name}  {status}"))
                }
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        let mut state = ListState::default();
        state.select(Some(selected));

        frame.render_stateful_widget(list, chunks[0], &mut state);

        let help = Paragraph::new("↑↓ Navigate    Enter Act    Space Select    q Quit");

        frame.render_widget(help, chunks[1]);

        if let Some(message) = busy_message {
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(40),
                    Constraint::Length(5),
                    Constraint::Percentage(60),
                ])
                .split(area);

            let horizontal = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(40),
                    Constraint::Percentage(30),
                ])
                .split(vertical[1]);

            let popup_area = horizontal[1];

            frame.render_widget(Clear, popup_area);

            let popup = Paragraph::new(format!("{message}\n\nPlease wait"))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));

            frame.render_widget(popup, popup_area);
        }
    })?;

    Ok(())
}

pub fn run(mut recipes: Vec<Recipe>, cache_dir: &Path, verbose: bool) -> io::Result<()> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let expanded = vec![true; recipes.len()];
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

        draw(
            &mut terminal,
            &recipes,
            &expanded,
            selected,
            &menu_items,
            None,
        )?;

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
                        if let Some(MenuItem::Recipe(recipe_index)) = menu_items.get(selected) {
                            let (package_name, version) = {
                                let recipe = &recipes[*recipe_index];
                                package::resolve(&recipe.name)
                            };

                            draw(
                                &mut terminal,
                                &recipes,
                                &expanded,
                                selected,
                                &menu_items,
                                Some("Preparing sources…"),
                            )?;

                            let work_dir = prepare::prepare_sources(
                                cache_dir,
                                &package_name,
                                &version,
                                verbose,
                            );

                            recipes[*recipe_index].check_patches(&work_dir.join("src"));
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
