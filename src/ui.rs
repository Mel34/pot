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
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::cookbook::{PatchStatus, Recipe};
use crate::prepare::{self, PrepState};
use crate::proc::Chunk;
use crate::term::TerminalGuard;
use crate::worker::{CookReq, Op, PrepareReq, VerifyReq, WorkerCommand, WorkerEvent};

#[derive(Clone, Copy)]
enum MenuItem {
    Recipe(usize),
    Patch(usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerStatus {
    Preparing,
    Resetting,
    Cooking,
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
        let right_part: String = chars.iter().skip(chars.len().saturating_sub(right)).collect();

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

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .split(vertical[1]);

    horizontal[1]
}

pub fn first_start(title: &str, message: &str, hostname: &str) -> io::Result<bool> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut selected = true;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let outer = Block::default()
                .title(" POT — First Start Setup ")
                .borders(Borders::ALL);

            frame.render_widget(outer, area);

            let dialog_area = centered_rect(48, 9, area);

            let dialog = Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL);

            let inner = dialog.inner(dialog_area);
            frame.render_widget(dialog, dialog_area);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(2),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(inner);

            let message = Paragraph::new(message)
                .alignment(ratatui::layout::Alignment::Center);

            let repository = Paragraph::new(hostname)
                .style(Style::default().add_modifier(Modifier::BOLD))
                .alignment(ratatui::layout::Alignment::Center);

            let buttons = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Fill(1),
                    Constraint::Length(7),
                    Constraint::Length(3),
                    Constraint::Length(7),
                    Constraint::Fill(1),
                ])
                .split(chunks[4]);

            let yes = if selected {
                Paragraph::new(Span::styled(
                    "[ YES ]",
                    Style::default().add_modifier(Modifier::REVERSED),
                ))
            } else {
                Paragraph::new("[ YES ]")
            };

            let no = if !selected {
                Paragraph::new(Span::styled(
                    "[ NO ]",
                    Style::default().add_modifier(Modifier::REVERSED),
                ))
            } else {
                Paragraph::new("[ NO ]")
            };

            frame.render_widget(message, chunks[1]);
            frame.render_widget(repository, chunks[2]);
            frame.render_widget(yes, buttons[1]);
            frame.render_widget(no, buttons[3]);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Left | KeyCode::Right => {
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

fn draw_log(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    recipe_name: &str,
    logs: &[String],
    scroll: usize,
    auto_scroll: bool,
    is_active: bool,
) -> io::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        let scroll_status = if auto_scroll { " [AUTO-SCROLL] " } else { " [SCROLLED] " };
        let title = format!(" POT — Kitchen Logs: {recipe_name}{scroll_status}");

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL);

        let inner_height = chunks[0].height.saturating_sub(2) as usize;
        let total_lines = logs.len();

        let start_line = if auto_scroll {
            total_lines.saturating_sub(inner_height)
        } else {
            scroll.min(total_lines.saturating_sub(1))
        };

        let visible_lines: Vec<Line> = logs
            .iter()
            .skip(start_line)
            .take(inner_height)
            .map(|l| {
                if l.starts_with("==>") {
                    Line::from(Span::styled(l, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)))
                } else if l.starts_with("  ->") {
                    Line::from(Span::styled(l, Style::default().fg(Color::Cyan)))
                } else if l.contains("error") || l.contains("failed") || l.contains("FAILED") {
                    Line::from(Span::styled(l, Style::default().fg(Color::Red)))
                } else {
                    Line::from(Span::raw(l))
                }
            })
            .collect();

        let paragraph = Paragraph::new(visible_lines)
            .block(block)
            .wrap(Wrap { trim: false });

        frame.render_widget(Clear, chunks[0]);
        frame.render_widget(paragraph, chunks[0]);

        let help_text = if is_active {
            "[Esc/l] Back to Cookbook    [↑↓/PgUp/PgDn] Scroll    [a] Abort Build"
        } else {
            "[Esc/l] Back to Cookbook    [↑↓/PgUp/PgDn] Scroll"
        };

        frame.render_widget(Paragraph::new(help_text), chunks[1]);
    })?;

    Ok(())
}

fn draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    recipes: &[Recipe],
    cache_dir: &Path,
    expanded: &[bool],
    worker_status: &[Option<WorkerStatus>],
    cook_status: &[Option<String>],
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

        let cooking_recipe = worker_status
            .iter()
            .position(|status| matches!(status, Some(WorkerStatus::Cooking)));

        for item in menu_items {
            match item {
                MenuItem::Recipe(recipe_index) => {
                    let recipe = &recipes[*recipe_index];

                    let marker = if expanded[*recipe_index] {
                        "▾"
                    } else {
                        "▸"
                    };

                    let is_prepared = prepare::state(cache_dir, &recipe.name) == PrepState::Ready;

                    let status = match worker_status[*recipe_index] {
                        Some(WorkerStatus::Preparing) => {
                            "[preparing sources...]".to_owned()
                        }
                        Some(WorkerStatus::Resetting) => {
                            "[resetting source...]".to_owned()
                        }
                        Some(WorkerStatus::Cooking) => cook_status[*recipe_index]
                            .as_deref()
                            .map(|status| format!("[{status}]"))
                            .unwrap_or_else(|| "[cooking]".to_owned()),
                        None if cooking_recipe.is_some() => {
                            "[please wait, cook in progress]".to_owned()
                        }
                        None if cook_status[*recipe_index].is_some() => {
                            cook_status[*recipe_index]
                                .as_deref()
                                .unwrap_or("")
                                .to_owned()
                        }
                        None if is_prepared => "[ready to cook]".to_owned(),
                        None => String::new(),
                    };

                    let prefix = format!(
                        "{marker} ({})     ",
                        recipe.patches.len()
                    );

                    let status_width = Line::raw(&status).width();

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
                    let name = patch.file_name();

                    let marker = if patch.selected { "[✓]" } else { "[ ]" };

                    let status = match patch.status {
                        PatchStatus::Unknown => "?",
                        PatchStatus::Checking => "⟳",
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

        frame.render_widget(Clear, chunks[0]);
        frame.render_stateful_widget(list, chunks[0], &mut state);

        let help = match menu_items.get(selected) {
            Some(MenuItem::Recipe(recipe_index)) => {
                let is_busy = matches!(
                    worker_status[*recipe_index],
                    Some(WorkerStatus::Cooking) | Some(WorkerStatus::Preparing) | Some(WorkerStatus::Resetting)
                );

                if is_busy {
                    Paragraph::new(
                        "[↑↓] Navigate    [a] Abort    [l/Enter] Live Log    [Space] Select    [q] Quit",
                    )
                } else if prepare::state(cache_dir, &recipes[*recipe_index].name) == PrepState::Ready {
                    Paragraph::new(
                        "[↑↓] Navigate    [Enter] Cook    [r] Reset    [l] Logs    [Space] Select    [q] Quit",
                    )
                } else {
                    Paragraph::new(
                        "[↑↓] Navigate    [Enter] Prepare  [l] Logs    [Space] Select    [q] Quit",
                    )
                }
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
    env: crate::worker::Env,
    _log: bool,
    _cookbook_dir: &Path,
) -> io::Result<()> {
    let worker = crate::worker::start(env.clone())?;
    crate::sudo::spawn_keepalive(|| {});
    let mut guard = TerminalGuard::enter()?;

    let mut expanded = vec![false; recipes.len()];
    let mut worker_status = vec![None; recipes.len()];
    let mut cook_status = vec![None; recipes.len()];
    let mut log_buffers: Vec<Vec<String>> = vec![Vec::new(); recipes.len()];
    let mut viewing_log: Option<usize> = None;
    let mut log_scroll: usize = 0;
    let mut auto_scroll = true;
    let mut selected = 0;

    loop {
        while let Ok(event) = worker.events.try_recv() {
            match event {
                WorkerEvent::Started { recipe_index, op } => {
                    worker_status[recipe_index] = match op {
                        Op::Prepare => Some(WorkerStatus::Preparing),
                        Op::Reset => Some(WorkerStatus::Resetting),
                        Op::Cook => Some(WorkerStatus::Cooking),
                    };
                }

                WorkerEvent::Phase { recipe_index, text } => {
                    cook_status[recipe_index] = Some(text);
                }

                WorkerEvent::Output { recipe_index, chunk } => {
                    if let Some(buf) = log_buffers.get_mut(recipe_index) {
                        match chunk {
                            Chunk::Line(line) => buf.push(line),
                            Chunk::Progress(prog) => {
                                if let Some(last) = buf.last_mut() {
                                    *last = prog;
                                } else {
                                    buf.push(prog);
                                }
                            }
                        }
                    }
                }

                WorkerEvent::Prepared {
                    recipe_index,
                    result,
                } => {
                    worker_status[recipe_index] = None;
                    match result {
                        Ok(()) => {
                            cook_status[recipe_index] = Some("[ready to cook]".to_owned());
                        }
                        Err(error) => {
                            cook_status[recipe_index] = Some(format!("[prepare failed: {error}]"));
                        }
                    }
                }

                WorkerEvent::Verified {
                    recipe_index,
                    patch_index,
                    status,
                    detail: _,
                } => {
                    if let Some(recipe) = recipes.get_mut(recipe_index) {
                        if let Some(patch) = recipe.patches.get_mut(patch_index) {
                            patch.status = status;
                        }
                    }
                }

                WorkerEvent::Cooked {
                    recipe_index,
                    result,
                    overlay: _,
                } => {
                    match result {
                        Ok(()) => {
                            worker_status[recipe_index] = None;
                            cook_status[recipe_index] = Some("[cooked successfully]".to_owned());
                        }
                        Err(crate::worker::JobError::Failed(ref message)) if message.contains("already being cooked") => {}
                        Err(error) => {
                            worker_status[recipe_index] = None;
                            cook_status[recipe_index] = Some(format!("[cook failed: {error}]"));
                        }
                    }
                }

                WorkerEvent::Notice(_) => {}

                WorkerEvent::Stopped => break,
            }
        }

        if let Some(recipe_index) = viewing_log {
            let is_active = matches!(
                worker_status.get(recipe_index).and_then(|s| *s),
                Some(WorkerStatus::Cooking) | Some(WorkerStatus::Preparing) | Some(WorkerStatus::Resetting)
            );

            draw_log(
                guard.terminal(),
                &recipes[recipe_index].name,
                &log_buffers[recipe_index],
                log_scroll,
                auto_scroll,
                is_active,
            )?;
        } else {
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
                guard.terminal(),
                &recipes,
                &env.cache_dir,
                &expanded,
                &worker_status,
                &cook_status,
                selected,
                &menu_items,
            )?;
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if let Some(recipe_index) = viewing_log {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('l') => {
                            viewing_log = None;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            auto_scroll = false;
                            log_scroll = log_scroll.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let total = log_buffers[recipe_index].len();
                            log_scroll = (log_scroll + 1).min(total.saturating_sub(1));
                            if log_scroll >= total.saturating_sub(1) {
                                auto_scroll = true;
                            }
                        }
                        KeyCode::PageUp => {
                            auto_scroll = false;
                            log_scroll = log_scroll.saturating_sub(15);
                        }
                        KeyCode::PageDown => {
                            let total = log_buffers[recipe_index].len();
                            log_scroll = (log_scroll + 15).min(total.saturating_sub(1));
                            if log_scroll >= total.saturating_sub(1) {
                                auto_scroll = true;
                            }
                        }
                        KeyCode::Char('a') => {
                            let _ = worker.send(WorkerCommand::Abort { recipe_index });
                        }
                        _ => {}
                    }
                } else {
                    let mut menu_items = Vec::new();
                    for (recipe_index, recipe) in recipes.iter().enumerate() {
                        menu_items.push(MenuItem::Recipe(recipe_index));
                        if expanded[recipe_index] {
                            for patch_index in 0..recipe.patches.len() {
                                menu_items.push(MenuItem::Patch(recipe_index, patch_index));
                            }
                        }
                    }

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
                            if let Some(MenuItem::Recipe(recipe_index)) = menu_items.get(selected) {
                                expanded[*recipe_index] = false;
                            }
                        }

                        KeyCode::Right => {
                            if let Some(MenuItem::Recipe(recipe_index)) = menu_items.get(selected) {
                                expanded[*recipe_index] = true;
                            }
                        }

                        KeyCode::Char('l') => {
                            if let Some(MenuItem::Recipe(recipe_index)) = menu_items.get(selected) {
                                viewing_log = Some(*recipe_index);
                                auto_scroll = true;
                            }
                        }

                        KeyCode::Enter => match menu_items.get(selected) {
                            Some(&MenuItem::Recipe(recipe_index)) => {
                                if worker_status[recipe_index] == Some(WorkerStatus::Cooking) {
                                    viewing_log = Some(recipe_index);
                                    auto_scroll = true;
                                    continue;
                                }

                                if worker_status.iter().any(|s| matches!(s, Some(WorkerStatus::Cooking))) {
                                    continue;
                                }

                                let recipe = &recipes[recipe_index];
                                let is_prepared = prepare::state(&env.cache_dir, &recipe.name) == PrepState::Ready;

                                if is_prepared {
                                    if !crate::sudo::credentials_valid() {
                                        guard.suspend()?;
                                        let auth_result = crate::sudo::authenticate();
                                        guard.resume()?;
                                        if let Err(error) = auth_result {
                                            cook_status[recipe_index] = Some(format!("[sudo: {error}]"));
                                            continue;
                                        }
                                    }

                                    let patches = recipe
                                        .patches
                                        .iter()
                                        .filter(|patch| patch.selected)
                                        .map(|patch| patch.path.clone())
                                        .collect();

                                    worker
                                        .send(WorkerCommand::Cook(CookReq {
                                            recipe_index,
                                            recipe_name: recipe.name.clone(),
                                            patches,
                                        }))
                                        .map_err(|_| io::Error::other("worker send failed"))?;
                                } else {
                                    let verify = recipe
                                        .patches
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, patch)| patch.selected)
                                        .map(|(patch_index, patch)| VerifyReq {
                                            recipe_index,
                                            recipe_name: recipe.name.clone(),
                                            patch_index,
                                            patch_path: patch.path.clone(),
                                        })
                                        .collect();

                                    worker
                                        .send(WorkerCommand::Prepare {
                                            request: PrepareReq {
                                                recipe_index,
                                                recipe_name: recipe.name.clone(),
                                                reset: false,
                                            },
                                            verify,
                                        })
                                        .map_err(|_| io::Error::other("worker send failed"))?;
                                }
                            }

                            Some(&MenuItem::Patch(recipe_index, patch_index)) => {
                                recipes[recipe_index].patches[patch_index].status = PatchStatus::Checking;
                                let recipe = &recipes[recipe_index];
                                let patch = &recipe.patches[patch_index];

                                worker
                                    .send(WorkerCommand::Verify(VerifyReq {
                                        recipe_index,
                                        recipe_name: recipe.name.clone(),
                                        patch_index,
                                        patch_path: patch.path.clone(),
                                    }))
                                    .map_err(|_| io::Error::other("worker send failed"))?;
                            }

                            None => {}
                        },

                        KeyCode::Char('r') => {
                            if let Some(&MenuItem::Recipe(recipe_index)) = menu_items.get(selected) {
                                let recipe = &recipes[recipe_index];
                                let verify = recipe
                                    .patches
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, patch)| patch.selected)
                                    .map(|(patch_index, patch)| VerifyReq {
                                        recipe_index,
                                        recipe_name: recipe.name.clone(),
                                        patch_index,
                                        patch_path: patch.path.clone(),
                                    })
                                    .collect();

                                worker
                                    .send(WorkerCommand::Prepare {
                                        request: PrepareReq {
                                            recipe_index,
                                            recipe_name: recipe.name.clone(),
                                            reset: true,
                                        },
                                        verify,
                                    })
                                    .map_err(|_| io::Error::other("worker send failed"))?;
                            }
                        }

                        KeyCode::Char('a') => {
                            if let Some(&MenuItem::Recipe(recipe_index)) = menu_items.get(selected) {
                                worker
                                    .send(WorkerCommand::Abort { recipe_index })
                                    .map_err(|_| io::Error::other("worker send failed"))?;
                            }
                        }

                        KeyCode::Char(' ') => {
                            if let Some(&MenuItem::Patch(recipe_index, patch_index)) = menu_items.get(selected) {
                                let recipe = &mut recipes[recipe_index];
                                let patch = &mut recipe.patches[patch_index];

                                patch.selected = !patch.selected;
                                recipe.save_selection()?;
                            }
                        }

                        _ => {}
                    }
                }
            }
        }
    }

    let _ = worker.send(WorkerCommand::Shutdown);
    Ok(())
}
