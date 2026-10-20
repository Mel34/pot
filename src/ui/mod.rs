pub mod bottom_bar;
pub mod dialogs;
pub mod panes;

pub use bottom_bar::{ActivePane, RightPaneMode};
pub use dialogs::first_start;

use std::io;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

use crate::cookbook::PatchStatus;
use crate::cookbook::Recipe;
use crate::package::VersionStatus;
use crate::prepare::{self, PrepState};
use crate::proc::Chunk;
use crate::term::TerminalGuard;
use crate::worker::{
    CheckUpdateReq, CookReq, InstallReq, Op, PrepareReq, VerifyReq, WorkerCommand, WorkerEvent,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    Preparing,
    Resetting,
    Cooking,
    Installing,
}

enum Modal {
    Help,
    Notice(String),
}

fn scan_artifact(repo_dir: &Path, recipe_name: &str) -> Option<String> {
    let entries = std::fs::read_dir(repo_dir).ok()?;
    let prefix = format!("{recipe_name}-");
    let mut matching = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if file_name.starts_with(&prefix) && file_name.ends_with(".pkg.tar.zst") {
                matching.push(file_name.to_owned());
            }
        }
    }
    matching.sort();
    matching.pop()
}

pub fn run(
    mut recipes: Vec<Recipe>,
    env: crate::worker::Env,
    _log: bool,
    _cookbook_dir: &Path,
) -> io::Result<()> {
    let worker = crate::worker::start(env.clone())?;
    let mut guard = TerminalGuard::enter()?;

    let mut active_pane = ActivePane::Left;
    let mut right_mode = RightPaneMode::Patches;
    let mut recipe_cursor: usize = 0;
    let mut patch_cursor: usize = 0;
    let mut log_scroll: usize = 0;
    let mut auto_scroll = true;
    let mut modal: Option<Modal> = None;

    let mut worker_status: Vec<Option<WorkerStatus>> = vec![None; recipes.len()];
    let mut cook_status: Vec<Option<String>> = vec![None; recipes.len()];
    let mut log_buffers: Vec<Vec<String>> = vec![Vec::new(); recipes.len()];

    let repository_dir = crate::repository::repository_dir(&env.repository_name).ok();
    let mut artifacts: Vec<Option<String>> = recipes
        .iter()
        .map(|r| {
            repository_dir
                .as_ref()
                .and_then(|d| scan_artifact(d, &r.name))
        })
        .collect();

    for (i, art) in artifacts.iter().enumerate() {
        if art.is_some() {
            cook_status[i] = Some("[cooked successfully]".to_owned());
        }
    }

    loop {
        while let Ok(event) = worker.events.try_recv() {
            match event {
                WorkerEvent::Started { recipe_index, op } => {
                    worker_status[recipe_index] = match op {
                        Op::Prepare => Some(WorkerStatus::Preparing),
                        Op::Reset => Some(WorkerStatus::Resetting),
                        Op::Cook => Some(WorkerStatus::Cooking),
                        Op::Install => Some(WorkerStatus::Installing),
                    };

                    if matches!(op, Op::Cook | Op::Install) {
                        recipe_cursor = recipe_index;
                        right_mode = RightPaneMode::BuildLog;
                        auto_scroll = true;
                    }
                }

                WorkerEvent::Phase { recipe_index, text } => {
                    cook_status[recipe_index] = Some(text);
                }

                WorkerEvent::Output {
                    recipe_index,
                    chunk,
                } => {
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

                    if auto_scroll
                        && right_mode == RightPaneMode::BuildLog
                        && recipe_cursor == recipe_index
                    {
                        if let Some(buf) = log_buffers.get(recipe_index) {
                            let total = buf.len();
                            log_scroll = total;
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
                    ..
                } => {
                    worker_status[recipe_index] = None;
                    match result {
                        Ok(()) => {
                            cook_status[recipe_index] = Some("[cooked successfully]".to_owned());
                            if let Some(ref d) = repository_dir {
                                if let Some(recipe) = recipes.get(recipe_index) {
                                    artifacts[recipe_index] = scan_artifact(d, &recipe.name);
                                }
                            }
                        }
                        Err(crate::worker::JobError::Failed(ref message))
                            if message.contains("already being cooked") => {}
                        Err(error) => {
                            cook_status[recipe_index] = Some(format!("[cook failed: {error}]"));
                        }
                    }
                }

                WorkerEvent::Installed {
                    recipe_index,
                    result,
                } => {
                    worker_status[recipe_index] = None;
                    match result {
                        Ok(()) => {
                            cook_status[recipe_index] = Some("[installed]".to_owned());
                        }
                        Err(error) => {
                            cook_status[recipe_index] = Some(format!("[install failed: {error}]"));
                        }
                    }
                }

                WorkerEvent::UpdateChecked {
                    recipe_index,
                    result,
                } => {
                    cook_status[recipe_index] = Some(match result {
                        Ok(VersionStatus::UpdateAvailable) => "[update]".to_owned(),
                        Ok(VersionStatus::Current) => "[up to date]".to_owned(),
                        Ok(VersionStatus::CachedNewer) => "[cached newer]".to_owned(),
                        Err(error) => format!("[update check failed: {error}]"),
                    });
                }

                WorkerEvent::Notice(msg) => {
                    modal = Some(Modal::Notice(msg));
                }

                WorkerEvent::Stopped => break,
            }
        }

        let enter_label = match active_pane {
            ActivePane::Left => {
                if let Some(recipe) = recipes.get(recipe_cursor) {
                    if artifacts
                        .get(recipe_cursor)
                        .and_then(|a| a.as_ref())
                        .is_some()
                        || cook_status[recipe_cursor].as_deref() == Some("[cooked successfully]")
                        || cook_status[recipe_cursor].as_deref() == Some("[installed]")
                    {
                        "Install"
                    } else if prepare::state(&env.cache_dir, &recipe.name) == PrepState::Ready {
                        "Cook"
                    } else {
                        "Prepare"
                    }
                } else {
                    "Enter"
                }
            }
            ActivePane::Right => match right_mode {
                RightPaneMode::Patches => "Verify",
                RightPaneMode::BuildLog => "Log",
            },
        };

        guard.terminal().draw(|f: &mut Frame| {
            let area = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(area);

            let pane_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[0]);

            panes::draw_panes(
                f,
                pane_chunks[0],
                pane_chunks[1],
                &recipes,
                &env.cache_dir,
                recipe_cursor,
                patch_cursor,
                active_pane,
                right_mode,
                &worker_status,
                &cook_status,
                &artifacts,
                &log_buffers,
                log_scroll,
                auto_scroll,
            );

            bottom_bar::draw_bottom_bar(f, chunks[1], active_pane, right_mode, enter_label);

            if let Some(ref m) = modal {
                match m {
                    Modal::Help => dialogs::draw_help_dialog(f),
                    Modal::Notice(msg) => dialogs::draw_message_dialog(f, "Notice", msg),
                }
            }
        })?;

        if event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if let Some(ref m) = modal {
                    match m {
                        Modal::Help => {
                            if matches!(key.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::Enter) {
                                modal = None;
                            }
                        }
                        Modal::Notice(_) => {
                            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                                modal = None;
                            }
                        }
                    }
                    continue;
                }

                match key.code {
                    KeyCode::F(1) => {
                        modal = Some(Modal::Help);
                    }
                    KeyCode::F(10) | KeyCode::Char('q') => {
                        break;
                    }
                    KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                        active_pane = match active_pane {
                            ActivePane::Left => ActivePane::Right,
                            ActivePane::Right => ActivePane::Left,
                        };
                    }
                    KeyCode::F(3) => {
                        right_mode = match right_mode {
                            RightPaneMode::Patches => RightPaneMode::BuildLog,
                            RightPaneMode::BuildLog => RightPaneMode::Patches,
                        };
                    }
                    KeyCode::Esc => {
                        if right_mode == RightPaneMode::BuildLog {
                            right_mode = RightPaneMode::Patches;
                        } else if active_pane == ActivePane::Right {
                            active_pane = ActivePane::Left;
                        }
                    }

                    _ => match active_pane {
                        ActivePane::Left => match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                recipe_cursor = recipe_cursor.saturating_sub(1);
                                patch_cursor = 0;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if recipe_cursor + 1 < recipes.len() {
                                    recipe_cursor += 1;
                                    patch_cursor = 0;
                                }
                            }
                            KeyCode::Home => {
                                recipe_cursor = 0;
                                patch_cursor = 0;
                            }
                            KeyCode::End => {
                                if !recipes.is_empty() {
                                    recipe_cursor = recipes.len() - 1;
                                    patch_cursor = 0;
                                }
                            }
                            KeyCode::PageUp => {
                                recipe_cursor = recipe_cursor.saturating_sub(10);
                                patch_cursor = 0;
                            }
                            KeyCode::PageDown => {
                                if !recipes.is_empty() {
                                    recipe_cursor = (recipe_cursor + 10).min(recipes.len() - 1);
                                    patch_cursor = 0;
                                }
                            }
                            KeyCode::Enter => {
                                if let Some(recipe) = recipes.get(recipe_cursor) {
                                    if worker_status[recipe_cursor].is_some() {
                                        right_mode = RightPaneMode::BuildLog;
                                        active_pane = ActivePane::Right;
                                        auto_scroll = true;
                                        continue;
                                    }

                                    let is_prepared = prepare::state(&env.cache_dir, &recipe.name)
                                        == PrepState::Ready;

                                    if artifacts
                                        .get(recipe_cursor)
                                        .and_then(|a| a.as_ref())
                                        .is_some()
                                        || cook_status[recipe_cursor].as_deref()
                                            == Some("[cooked successfully]")
                                        || cook_status[recipe_cursor].as_deref()
                                            == Some("[installed]")
                                    {
                                        worker
                                            .send(WorkerCommand::Install(InstallReq {
                                                recipe_index: recipe_cursor,
                                                recipe_name: recipe.name.clone(),
                                            }))
                                            .map_err(|_| io::Error::other("worker send failed"))?;
                                    } else if is_prepared {
                                        let patches = recipe
                                            .patches
                                            .iter()
                                            .filter(|patch| patch.selected)
                                            .map(|patch| patch.path.clone())
                                            .collect();

                                        worker
                                            .send(WorkerCommand::Cook(CookReq {
                                                recipe_index: recipe_cursor,
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
                                                recipe_index: recipe_cursor,
                                                recipe_name: recipe.name.clone(),
                                                patch_index,
                                                patch_path: patch.path.clone(),
                                            })
                                            .collect();

                                        worker
                                            .send(WorkerCommand::Prepare {
                                                request: PrepareReq {
                                                    recipe_index: recipe_cursor,
                                                    recipe_name: recipe.name.clone(),
                                                    reset: false,
                                                },
                                                verify,
                                            })
                                            .map_err(|_| io::Error::other("worker send failed"))?;
                                    }
                                }
                            }
                            KeyCode::F(2) | KeyCode::Char('c') => {
                                if let Some(recipe) = recipes.get(recipe_cursor) {
                                    if worker_status[recipe_cursor].is_none() {
                                        let is_prepared =
                                            prepare::state(&env.cache_dir, &recipe.name)
                                                == PrepState::Ready;
                                        if is_prepared {
                                            let patches = recipe
                                                .patches
                                                .iter()
                                                .filter(|patch| patch.selected)
                                                .map(|patch| patch.path.clone())
                                                .collect();

                                            worker
                                                .send(WorkerCommand::Cook(CookReq {
                                                    recipe_index: recipe_cursor,
                                                    recipe_name: recipe.name.clone(),
                                                    patches,
                                                }))
                                                .map_err(|_| {
                                                    io::Error::other("worker send failed")
                                                })?;
                                        }
                                    }
                                }
                            }
                            KeyCode::F(4) | KeyCode::Char('r') => {
                                if let Some(recipe) = recipes.get(recipe_cursor) {
                                    let verify = recipe
                                        .patches
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, patch)| patch.selected)
                                        .map(|(patch_index, patch)| VerifyReq {
                                            recipe_index: recipe_cursor,
                                            recipe_name: recipe.name.clone(),
                                            patch_index,
                                            patch_path: patch.path.clone(),
                                        })
                                        .collect();

                                    worker
                                        .send(WorkerCommand::Prepare {
                                            request: PrepareReq {
                                                recipe_index: recipe_cursor,
                                                recipe_name: recipe.name.clone(),
                                                reset: true,
                                            },
                                            verify,
                                        })
                                        .map_err(|_| io::Error::other("worker send failed"))?;
                                }
                            }
                            KeyCode::F(5) => {
                                let _ = crate::helper::sync_packages(&env.repository_name);
                            }
                            KeyCode::F(7) => {
                                if let Some(recipe) = recipes.get(recipe_cursor) {
                                    if worker_status[recipe_cursor].is_none() {
                                        cook_status[recipe_cursor] =
                                            Some("[checking update...]".to_owned());

                                        worker
                                            .send(WorkerCommand::CheckUpdate(CheckUpdateReq {
                                                recipe_index: recipe_cursor,
                                                recipe_name: recipe.name.clone(),
                                            }))
                                            .map_err(|_| io::Error::other("worker send failed"))?;
                                    }
                                }
                            }
                            KeyCode::F(8) | KeyCode::Char('a') => {
                                let _ = worker.send(WorkerCommand::Abort {
                                    recipe_index: recipe_cursor,
                                });
                            }
                            _ => {}
                        },

                        ActivePane::Right => match right_mode {
                            RightPaneMode::Patches => match key.code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    patch_cursor = patch_cursor.saturating_sub(1);
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if let Some(recipe) = recipes.get(recipe_cursor) {
                                        if patch_cursor + 1 < recipe.patches.len() {
                                            patch_cursor += 1;
                                        }
                                    }
                                }
                                KeyCode::Home => {
                                    patch_cursor = 0;
                                }
                                KeyCode::End => {
                                    if let Some(recipe) = recipes.get(recipe_cursor) {
                                        if !recipe.patches.is_empty() {
                                            patch_cursor = recipe.patches.len() - 1;
                                        }
                                    }
                                }
                                KeyCode::PageUp => {
                                    patch_cursor = patch_cursor.saturating_sub(10);
                                }
                                KeyCode::PageDown => {
                                    if let Some(recipe) = recipes.get(recipe_cursor) {
                                        if !recipe.patches.is_empty() {
                                            patch_cursor =
                                                (patch_cursor + 10).min(recipe.patches.len() - 1);
                                        }
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    if let Some(recipe) = recipes.get_mut(recipe_cursor) {
                                        if let Some(patch) = recipe.patches.get_mut(patch_cursor) {
                                            patch.selected = !patch.selected;
                                            let _ = recipe.save_selection();
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    if let Some(recipe) = recipes.get_mut(recipe_cursor) {
                                        if let Some(patch) = recipe.patches.get_mut(patch_cursor) {
                                            patch.status = PatchStatus::Checking;
                                            let req = VerifyReq {
                                                recipe_index: recipe_cursor,
                                                recipe_name: recipe.name.clone(),
                                                patch_index: patch_cursor,
                                                patch_path: patch.path.clone(),
                                            };
                                            let _ = worker.send(WorkerCommand::Verify(req));
                                        }
                                    }
                                }
                                KeyCode::F(2) => {
                                    if let Some(recipe) = recipes.get_mut(recipe_cursor) {
                                        for (p_idx, patch) in recipe.patches.iter_mut().enumerate()
                                        {
                                            if patch.selected {
                                                patch.status = PatchStatus::Checking;
                                                let req = VerifyReq {
                                                    recipe_index: recipe_cursor,
                                                    recipe_name: recipe.name.clone(),
                                                    patch_index: p_idx,
                                                    patch_path: patch.path.clone(),
                                                };
                                                let _ = worker.send(WorkerCommand::Verify(req));
                                            }
                                        }
                                    }
                                }
                                KeyCode::F(4) | KeyCode::Char('r') => {
                                    if let Some(recipe) = recipes.get(recipe_cursor) {
                                        let verify = recipe
                                            .patches
                                            .iter()
                                            .enumerate()
                                            .filter(|(_, patch)| patch.selected)
                                            .map(|(patch_index, patch)| VerifyReq {
                                                recipe_index: recipe_cursor,
                                                recipe_name: recipe.name.clone(),
                                                patch_index,
                                                patch_path: patch.path.clone(),
                                            })
                                            .collect();

                                        worker
                                            .send(WorkerCommand::Prepare {
                                                request: PrepareReq {
                                                    recipe_index: recipe_cursor,
                                                    recipe_name: recipe.name.clone(),
                                                    reset: true,
                                                },
                                                verify,
                                            })
                                            .map_err(|_| io::Error::other("worker send failed"))?;
                                    }
                                }
                                KeyCode::F(8) | KeyCode::Char('a') => {
                                    let _ = worker.send(WorkerCommand::Abort {
                                        recipe_index: recipe_cursor,
                                    });
                                }
                                _ => {}
                            },

                            RightPaneMode::BuildLog => match key.code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    auto_scroll = false;
                                    log_scroll = log_scroll.saturating_sub(1);
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let total = log_buffers
                                        .get(recipe_cursor)
                                        .map(|b| b.len())
                                        .unwrap_or(0);
                                    log_scroll = (log_scroll + 1).min(total.saturating_sub(1));
                                    if log_scroll >= total.saturating_sub(1) {
                                        auto_scroll = true;
                                    }
                                }
                                KeyCode::Home => {
                                    auto_scroll = false;
                                    log_scroll = 0;
                                }
                                KeyCode::End => {
                                    let total = log_buffers
                                        .get(recipe_cursor)
                                        .map(|b| b.len())
                                        .unwrap_or(0);
                                    log_scroll = total.saturating_sub(1);
                                    auto_scroll = true;
                                }
                                KeyCode::PageUp => {
                                    auto_scroll = false;
                                    log_scroll = log_scroll.saturating_sub(15);
                                }
                                KeyCode::PageDown => {
                                    let total = log_buffers
                                        .get(recipe_cursor)
                                        .map(|b| b.len())
                                        .unwrap_or(0);
                                    log_scroll = (log_scroll + 15).min(total.saturating_sub(1));
                                    if log_scroll >= total.saturating_sub(1) {
                                        auto_scroll = true;
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    auto_scroll = !auto_scroll;
                                }
                                KeyCode::F(8) | KeyCode::Char('a') => {
                                    let _ = worker.send(WorkerCommand::Abort {
                                        recipe_index: recipe_cursor,
                                    });
                                }
                                _ => {}
                            },
                        },
                    },
                }
            }
        }
    }

    let _ = worker.send(WorkerCommand::Shutdown);
    Ok(())
}
