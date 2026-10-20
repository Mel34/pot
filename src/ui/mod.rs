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
    CheckUpdateReq, CookReq, InstallReq, Op, PrepareReq, VerifyBatchReq, VerifyReq,
    WorkerCommand, WorkerEvent,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateState {
    Unknown,
    Available,
    Refreshing,
    ReadyToCook,
    CookFailed,
    Current,
    CachedNewer,
    CheckFailed,
}

fn update_state_for(result: &Result<VersionStatus, String>) -> UpdateState {
    match result {
        Ok(VersionStatus::UpdateAvailable) => UpdateState::Available,
        Ok(VersionStatus::Current) => UpdateState::Current,
        Ok(VersionStatus::CachedNewer) => UpdateState::CachedNewer,
        Err(_) => UpdateState::CheckFailed,
    }
}

fn should_apply_update_check(state: UpdateState) -> bool {
    !matches!(
        state,
        UpdateState::Refreshing | UpdateState::ReadyToCook | UpdateState::CookFailed
    )
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

fn update_verified_patch(
    recipe: &mut Recipe,
    patch_index: usize,
    status: PatchStatus,
) {
    if let Some(patch) = recipe.patches.get_mut(patch_index) {
        patch.status = status;
        if status == PatchStatus::Failed && patch.selected {
            patch.selected = false;
            let _ = recipe.save_selection();
        }
    }
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
    let mut update_states: Vec<UpdateState> = vec![UpdateState::Unknown; recipes.len()];
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
                    let was_refreshing = update_states[recipe_index] == UpdateState::Refreshing;
                    match result {
                        Ok(()) => {
                            cook_status[recipe_index] = Some("[ready to cook]".to_owned());
                            if was_refreshing {
                                update_states[recipe_index] = UpdateState::ReadyToCook;
                            }
                        }
                        Err(error) => {
                            cook_status[recipe_index] = Some(format!("[prepare failed: {error}]"));
                            if was_refreshing {
                                update_states[recipe_index] = UpdateState::Available;
                            }
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
                        update_verified_patch(recipe, patch_index, status);
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
                            if matches!(
                                update_states[recipe_index],
                                UpdateState::ReadyToCook | UpdateState::CookFailed
                            ) {
                                update_states[recipe_index] = UpdateState::Current;
                            }
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
                            if matches!(
                                update_states[recipe_index],
                                UpdateState::ReadyToCook | UpdateState::CookFailed
                            ) {
                                update_states[recipe_index] = UpdateState::CookFailed;
                            }
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
                    if should_apply_update_check(update_states[recipe_index]) {
                        update_states[recipe_index] = update_state_for(&result);
                        cook_status[recipe_index] = Some(match result {
                            Ok(VersionStatus::UpdateAvailable) => "[update]".to_owned(),
                            Ok(VersionStatus::Current) => "[up to date]".to_owned(),
                            Ok(VersionStatus::CachedNewer) => "[cached newer]".to_owned(),
                            Err(error) => format!("[update check failed: {error}]"),
                        });
                    }
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
                    match update_states[recipe_cursor] {
                        UpdateState::Available => "Refresh",
                        UpdateState::Refreshing => "Wait",
                        UpdateState::ReadyToCook | UpdateState::CookFailed => "Cook",
                        UpdateState::CachedNewer
                            if prepare::state(&env.cache_dir, &recipe.name) == PrepState::Ready =>
                        {
                            "Cook"
                        }
                        UpdateState::CachedNewer => "Prepare",
                        _ if artifacts
                            .get(recipe_cursor)
                            .and_then(|a| a.as_ref())
                            .is_some()
                            || cook_status[recipe_cursor].as_deref()
                                == Some("[cooked successfully]")
                            || cook_status[recipe_cursor].as_deref() == Some("[installed]") =>
                        {
                            "Install"
                        }
                        _ if prepare::state(&env.cache_dir, &recipe.name) == PrepState::Ready => {
                            "Cook"
                        }
                        _ => "Prepare",
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

                                    if update_states[recipe_cursor] == UpdateState::Available {
                                        artifacts[recipe_cursor] = None;
                                        update_states[recipe_cursor] = UpdateState::Refreshing;

                                        let verify = recipe
                                            .patches
                                            .iter()
                                            .enumerate()
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
                                    } else if matches!(
                                        update_states[recipe_cursor],
                                        UpdateState::ReadyToCook | UpdateState::CookFailed
                                    ) {
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
                                    } else if update_states[recipe_cursor] != UpdateState::CachedNewer
                                        && (artifacts
                                            .get(recipe_cursor)
                                            .and_then(|a| a.as_ref())
                                            .is_some()
                                            || cook_status[recipe_cursor].as_deref()
                                                == Some("[cooked successfully]")
                                            || cook_status[recipe_cursor].as_deref()
                                                == Some("[installed]"))
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
                                match crate::helper::sync_packages(&env.repository_name) {
                                    Ok(()) => {
                                        for (index, recipe) in recipes.iter().enumerate() {
                                            if worker_status[index].is_some()
                                                || cook_status[index].as_deref()
                                                    == Some("[checking update...]")
                                            {
                                                continue;
                                            }

                                            cook_status[index] =
                                                Some("[checking update...]".to_owned());

                                            worker
                                                .send(WorkerCommand::CheckUpdate(CheckUpdateReq {
                                                    recipe_index: index,
                                                    recipe_name: recipe.name.clone(),
                                                }))
                                                .map_err(|_| {
                                                    io::Error::other("worker send failed")
                                                })?;
                                        }
                                    }
                                    Err(error) => {
                                        modal = Some(Modal::Notice(format!(
                                            "Database refresh failed: {error}"
                                        )));
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
                                        let patches = recipe
                                            .patches
                                            .iter_mut()
                                            .enumerate()
                                            .filter_map(|(patch_index, patch)| {
                                                if patch.selected {
                                                    patch.status = PatchStatus::Checking;
                                                    Some(VerifyReq {
                                                        recipe_index: recipe_cursor,
                                                        recipe_name: recipe.name.clone(),
                                                        patch_index,
                                                        patch_path: patch.path.clone(),
                                                    })
                                                } else {
                                                    None
                                                }
                                            })
                                            .collect::<Vec<_>>();

                                        if !patches.is_empty() {
                                            let request = VerifyBatchReq {
                                                recipe_index: recipe_cursor,
                                                recipe_name: recipe.name.clone(),
                                                patches,
                                            };
                                            let _ = worker.send(WorkerCommand::VerifyBatch(request));
                                        }
                                    }
                                }
                                KeyCode::F(4) | KeyCode::Char('r') => {
                                    if let Some(recipe) = recipes.get(recipe_cursor) {
                                        let verify = recipe
                                            .patches
                                            .iter()
                                            .enumerate()
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

#[cfg(test)]
mod update_state_tests {
    use super::*;

    #[test]
    fn maps_update_check_results_to_dedicated_states() {
        assert_eq!(
            update_state_for(&Ok(VersionStatus::UpdateAvailable)),
            UpdateState::Available
        );
        assert_eq!(
            update_state_for(&Ok(VersionStatus::Current)),
            UpdateState::Current
        );
        assert_eq!(
            update_state_for(&Ok(VersionStatus::CachedNewer)),
            UpdateState::CachedNewer
        );
        assert_eq!(
            update_state_for(&Err("test failure".to_owned())),
            UpdateState::CheckFailed
        );
    }

    #[test]
    fn failed_verification_unselects_and_persists_only_the_failed_patch() {
        use crate::cookbook::Patch;

        let root = std::env::temp_dir().join(format!(
            "pot-ui-verification-selection-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut recipe = Recipe {
            name: "selection-test".to_owned(),
            path: root.clone(),
            patches: vec![
                Patch {
                    path: root.join("bad.patch"),
                    selected: true,
                    status: PatchStatus::Checking,
                },
                Patch {
                    path: root.join("good.patch"),
                    selected: true,
                    status: PatchStatus::Checking,
                },
            ],
        };

        update_verified_patch(&mut recipe, 0, PatchStatus::Failed);
        update_verified_patch(&mut recipe, 1, PatchStatus::Clean);

        assert_eq!(recipe.patches[0].status, PatchStatus::Failed);
        assert!(!recipe.patches[0].selected);
        assert_eq!(recipe.patches[1].status, PatchStatus::Clean);
        assert!(recipe.patches[1].selected);
        assert_eq!(
            std::fs::read_to_string(root.join(".pot")).unwrap(),
            "good.patch\n"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_checks_do_not_overwrite_active_lifecycle_states() {
        for state in [
            UpdateState::Refreshing,
            UpdateState::ReadyToCook,
            UpdateState::CookFailed,
        ] {
            assert!(!should_apply_update_check(state), "{state:?}");
        }

        for state in [
            UpdateState::Unknown,
            UpdateState::Available,
            UpdateState::Current,
            UpdateState::CachedNewer,
            UpdateState::CheckFailed,
        ] {
            assert!(should_apply_update_check(state), "{state:?}");
        }
    }
}
