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
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::cookbook::{PatchStatus, Recipe};
use crate::{kitchen, package, prepare};

#[derive(Clone, Copy)]
enum MenuItem {
    Recipe(usize),
    Patch(usize, usize),
}

#[derive(Clone, Copy)]
enum WorkerStatus {
    Preparing,
    Resetting,
}

fn middle_truncate(name: &str, max_width: usize) -> String {
    let line = Line::raw(name);

    if line.width() <= max_width {
        return name.to_owned();
    }

    if max_width <= 1 {
        return "…".to_owned();
    }

    let chars: Vec<char> = name.chars().collect();
    let mut left = (max_width - 1) / 2;
    let mut right = max_width - 1 - left;

    loop {
        let left_part: String = chars.iter().take(left).collect();
        let right_part: String = chars
            .iter()
            .rev()
            .take(right)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        let result = format!("{left_part}…{right_part}");

        if Line::raw(&result).width() <= max_width {
            return result;
        }

        if right > 0 {
            right -= 1;
        } else if left > 0 {
            left -= 1;
        } else {
            return "…".to_owned();
        }
    }
}

fn draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    recipes: &[Recipe],
    cache_dir: &Path,
    expanded: &[bool],
    worker_status: &[Option<WorkerStatus>],
    selected: usize,
    menu_items: &[MenuItem],
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

        let content_width = chunks[0].width.saturating_sub(2) as usize;

        let header = ListItem::new("Patches   Recipe");
        let separator = ListItem::new("─".repeat(content_width));

        let mut items = vec![header, separator];

        for item in menu_items {
            match item {
                MenuItem::Recipe(recipe_index) => {
                    let recipe = &recipes[*recipe_index];

                    let marker = if expanded[*recipe_index] {
                        "▼"
                    } else {
                        "▶"
                    };

                    let is_prepared = prepare::is_prepared(cache_dir, &recipe.name);

                    let status = match worker_status[*recipe_index] {
                        Some(WorkerStatus::Preparing) => "[preparing sources...]",
                        Some(WorkerStatus::Resetting) => "[resetting source...]",
                        None if is_prepared => "[ready to cook]",
                        None => "",
                    };

                    let prefix = format!(
                        "{marker} ({})     ",
                        recipe.patches.len()
                    );

                    let status_width = Line::raw(status).width();

                    let name_width = content_width
                        .saturating_sub(Line::raw(&prefix).width())
                        .saturating_sub(status_width)
                        .saturating_sub(if status.is_empty() { 0 } else { 2 });

                    let name = middle_truncate(&recipe.name, name_width);

                    let used_width =
                        Line::raw(&format!("{prefix}{name}")).width();

                    let gap = content_width
                        .saturating_sub(used_width)
                        .saturating_sub(status_width);

                    let filler = " ".repeat(gap);

                    items.push(ListItem::new(Line::from(vec![
                        Span::raw(format!("{prefix}{name}{filler}")),
                        Span::raw(status),
                    ])));
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

                    items.push(ListItem::new(format!(
                        "    {marker} {name}  {status}"
                    )));
                }
            }
        }

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        let mut state = ListState::default();

        if !menu_items.is_empty() {
            state.select(Some(selected + 2));
        }

        frame.render_stateful_widget(list, chunks[0], &mut state);

        let help = match menu_items.get(selected) {
            Some(MenuItem::Recipe(recipe_index))
                if prepare::is_prepared(cache_dir, &recipes[*recipe_index].name) =>
            {
                Paragraph::new(
                    "[↑↓] Navigate    [Enter] Cook    [r] Reset    [Space] Select    [q] Quit",
                )
            }
            Some(MenuItem::Recipe(_)) => {
                Paragraph::new("[↑↓] Navigate    [Enter] Prepare  [Space] Select    [q] Quit")
            }
            Some(MenuItem::Patch(_, _)) => {
                Paragraph::new("[↑↓] Navigate    [Enter] Verify   [Space] Select    [q] Quit")
            }
            None => Paragraph::new(""),
        };

        frame.render_widget(help, chunks[1]);
    })?;

    Ok(())
}

pub fn run(
    mut recipes: Vec<Recipe>,
    cache_dir: &Path,
    log: bool,
    repository_name: &str,
) -> io::Result<()> {
    let (worker_tx, worker_rx) = crate::worker::start();

    worker_rx
        .recv()
        .map_err(|_| io::Error::other("worker thread exited"))?;

    enable_raw_mode()?;

    let mut stdout = io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut expanded = vec![false; recipes.len()];
    let mut worker_status = vec![None; recipes.len()];
    let mut selected = 0;

    loop {
        while let Ok(event) = worker_rx.try_recv() {
            match event {
                crate::worker::WorkerEvent::Preparing { recipe_index } => {
                    worker_status[recipe_index] = Some(WorkerStatus::Preparing);
                }

                crate::worker::WorkerEvent::Resetting { recipe_index } => {
                    worker_status[recipe_index] = Some(WorkerStatus::Resetting);
                }

                crate::worker::WorkerEvent::Prepared {
                    recipe_index,
                    work_dir,
                    output: _,
                    success,
                } => {
                    worker_status[recipe_index] = None;

                    if success {
                        recipes[recipe_index].check_patches(&work_dir.join("src"));
                    }
                }

                crate::worker::WorkerEvent::Verified {
                    recipe_index,
                    patch_index,
                    status,
                } => {
                    recipes[recipe_index].patches[patch_index].status = status;
                }

                crate::worker::WorkerEvent::Ready
                | crate::worker::WorkerEvent::Stopped
                | crate::worker::WorkerEvent::Cooked { .. } => {}
            }
        }

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
            cache_dir,
            &expanded,
            &worker_status,
            selected,
            &menu_items,
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

                    KeyCode::Left => {
                        if let Some(MenuItem::Recipe(recipe_index)) =
                            menu_items.get(selected)
                        {
                            expanded[*recipe_index] = false;
                        }
                    }

                    KeyCode::Right => {
                        if let Some(MenuItem::Recipe(recipe_index)) =
                            menu_items.get(selected)
                        {
                            expanded[*recipe_index] = true;
                        }
                    }

                    KeyCode::Enter => {
                        match menu_items.get(selected) {
                            Some(MenuItem::Recipe(recipe_index)) => {
                                let recipe = &recipes[*recipe_index];

                                if prepare::is_prepared(cache_dir, &recipe.name) {
                                    kitchen::run(
                                        &mut terminal,
                                        &worker_tx,
                                        &worker_rx,
                                        recipe,
                                        cache_dir,
                                        repository_name,
                                        log,
                                    )?;
                                } else {
                                    let (package_name, version) =
                                        package::resolve(&recipe.name);

                                    worker_tx
                                        .send(crate::worker::WorkerCommand::Prepare {
                                            recipe_index: *recipe_index,
                                            cache_dir: cache_dir.to_path_buf(),
                                            package: package_name,
                                            version,
                                            log,
                                        })
                                        .map_err(|_| {
                                            io::Error::other("worker thread exited")
                                        })?;
                                }
                            }

                            Some(MenuItem::Patch(recipe_index, patch_index)) => {
                                let recipe = &recipes[*recipe_index];

                                if prepare::is_prepared(cache_dir, &recipe.name) {
                                    let patch_path =
                                        recipe.patches[*patch_index].path.clone();

                                    let source_dir =
                                        cache_dir.join("work").join(&recipe.name).join("src");

                                    worker_tx
                                        .send(crate::worker::WorkerCommand::Verify {
                                            recipe_index: *recipe_index,
                                            patch_index: *patch_index,
                                            patch_path,
                                            source_dir,
                                        })
                                        .map_err(|_| {
                                            io::Error::other("worker thread exited")
                                        })?;
                                } else {
                                    let (package_name, version) =
                                        package::resolve(&recipe.name);

                                    let work_dir = prepare::prepare_sources(
                                        cache_dir,
                                        &package_name,
                                        &version,
                                        log,
                                    ).work_dir;

                                    let patch_path =
                                        recipe.patches[*patch_index].path.clone();

                                    worker_tx
                                        .send(crate::worker::WorkerCommand::Verify {
                                            recipe_index: *recipe_index,
                                            patch_index: *patch_index,
                                            patch_path,
                                            source_dir: work_dir.join("src"),
                                        })
                                        .map_err(|_| {
                                            io::Error::other("worker thread exited")
                                        })?;
                                }
                            }

                            None => {}
                        }
                    }

                    KeyCode::Char('r') => {
                        if let Some(MenuItem::Recipe(recipe_index)) =
                            menu_items.get(selected)
                        {
                            let recipe = &recipes[*recipe_index];

                            if prepare::is_prepared(cache_dir, &recipe.name) {
                                let (package_name, version) =
                                    package::resolve(&recipe.name);

                                worker_tx
                                    .send(crate::worker::WorkerCommand::Reset {
                                        recipe_index: *recipe_index,
                                        cache_dir: cache_dir.to_path_buf(),
                                        package: package_name,
                                        version,
                                        log,
                                    })
                                    .map_err(|_| {
                                        io::Error::other("worker thread exited")
                                    })?;
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

    worker_tx.send(crate::worker::WorkerCommand::Shutdown).ok();

    Ok(())
}
