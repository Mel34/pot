use std::io::{self, Read};
use std::path::Path;
use std::process::Stdio;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
};

use crate::cookbook::Recipe;
use crate::overlay::Overlay;
use crate::repository;

pub fn cook(
    recipe_name: &str,
    patches: &[std::path::PathBuf],
    git_sources: &[String],
    cache_dir: &Path,
    repository_name: &str,
    _log: bool,
    event_tx: &std::sync::mpsc::Sender<crate::worker::WorkerEvent>,
    recipe_index: usize,
    cook_abort: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (String, bool, Option<Overlay>) {
    let work_dir = cache_dir.join("work").join(recipe_name);

    let mut output = String::new();
    let mut success = true;

    let overlay_root = cache_dir.join("overlay").join(recipe_name);
    let overlay = match Overlay::mount(&work_dir, &overlay_root) {
        Ok(overlay) => {
            output.push_str("OverlayFS mounted.\n");
            overlay
        }
        Err(error) => {
            output.push_str(&format!("Failed to mount OverlayFS: {error}\n"));
            return (output, false, None);
        }
    };

    let merged_dir = overlay.merged.clone();

    for patch in patches {
        let verification =
            crate::patch::verify(&merged_dir, git_sources, patch);

        let (source_dir, strip_level, already_applied) = match verification {
            crate::patch::Verification::Applicable(path, strip_level) => {
                (path, strip_level, false)
            }

            crate::patch::Verification::Applied(path, strip_level) => {
                (path, strip_level, true)
            }

            crate::patch::Verification::Conflict(_error) => {
                output.push_str(&format!(
                    "Overlay conflicts: {}\n",
                    patch.display()
                ));
                success = false;
                break;
            }
        };
        
        output.push_str(&format!(
            "Patch source directory: {}\n",
            source_dir.display()
        ));

        if already_applied {
            output.push_str(&format!(
                "Overlay already applied: {}\n",
                patch.display()
            ));
            continue;
        }

        event_tx
            .send(crate::worker::WorkerEvent::CookStatus {
                recipe_index,
                status: format!(
                    "applying {}",
                    patch.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("overlay"),
                ),
            })
            .ok();

        output.push_str(&format!(
            "Applying overlay: {}\n",
            patch.display()
        ));

        let strip_arg = format!("-p{}", strip_level);

        let result = std::process::Command::new("git")
            .args(["apply", &strip_arg])
            .arg(patch)
            .current_dir(&source_dir)
            .output()
            .expect("failed to run git apply");

        output.push_str(&String::from_utf8_lossy(&result.stdout));
        output.push_str(&String::from_utf8_lossy(&result.stderr));

        if !result.status.success() {
            success = false;
            break;
        }
    }

    if success {
        for entry in match std::fs::read_dir(&work_dir) {
            Ok(entries) => entries,
            Err(error) => {
                output.push_str(&format!(
                    "Failed to read work directory: {error}\n"
                ));
                return (output, false, Some(overlay));
            }
        } {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    output.push_str(&format!(
                        "Failed to read work directory entry: {error}\n"
                    ));
                    return (output, false, Some(overlay));
                }
            };

            if path.to_string_lossy().ends_with(".pkg.tar.zst") {
                if let Err(error) = std::fs::remove_file(path) {
                    output.push_str(&format!(
                        "Failed to remove stale package: {error}\n"
                    ));
                    return (output, false, Some(overlay));
                }
            }
        }

        output.push_str("\nRunning makepkg -e...\n\n");

        event_tx
            .send(crate::worker::WorkerEvent::CookStatus {
                recipe_index,
                status: "running makepkg".to_owned(),
            })
            .ok();

        let mut child = std::process::Command::new("makepkg")
            .args(["-e", "--nocheck"])
            .current_dir(&merged_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to run makepkg");

        let stdout = child
            .stdout
            .take()
            .expect("makepkg stdout was not piped");

        let stderr = child
            .stderr
            .take()
            .expect("makepkg stderr was not piped");

        let (output_tx, output_rx) = std::sync::mpsc::channel();

        let stdout_tx = output_tx.clone();
        std::thread::spawn(move || {
            let mut reader = stdout;
            let mut buffer = [0u8; 4096];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if stdout_tx
                            .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let stderr_tx = output_tx.clone();
        std::thread::spawn(move || {
            let mut reader = stderr;
            let mut buffer = [0u8; 4096];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if stderr_tx
                            .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        drop(output_tx);

        while !cook_abort.load(std::sync::atomic::Ordering::Relaxed) {
            match output_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(chunk) => {
                    output.push_str(&chunk);

                    event_tx
                        .send(crate::worker::WorkerEvent::CookOutput {
                            recipe_index,
                            output: chunk,
                        })
                        .ok();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if cook_abort.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = child.kill();
        }

        let status = child
            .wait()
            .expect("failed to wait for makepkg");

        success = status.success();
    }

    if success {
        let repository_dir = repository::repository_dir(repository_name);
        let mut packages = Vec::new();

        let entries = match std::fs::read_dir(&merged_dir) {
            Ok(entries) => entries,
            Err(error) => {
                output.push_str(&format!(
                    "Failed to read merged directory: {error}\n"
                ));
                return (output, false, Some(overlay));
            }
        };

        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    output.push_str(&format!(
                        "Failed to read work directory entry: {error}\n"
                    ));
                    return (output, false, Some(overlay));
                }
            };

            if path.extension().and_then(|ext| ext.to_str()) == Some("zst")
                && path.to_string_lossy().ends_with(".pkg.tar.zst")
            {
                let filename = path
                    .file_name()
                    .expect("package has no filename")
                    .to_owned();

                if let Err(error) = std::fs::set_permissions(
                    &path,
                    std::os::unix::fs::PermissionsExt::from_mode(0o644),
                ) {
                    output.push_str(&format!(
                        "Failed to set package permissions: {error}\n"
                    ));
                    return (output, false, Some(overlay));
                }

                let destination = repository_dir.join(&filename);

                if let Err(error) = std::fs::copy(&path, &destination) {
                    output.push_str(&format!(
                        "Failed to copy package into repository: {error}\n"
                    ));
                    return (output, false, Some(overlay));
                }

                if let Err(error) = std::fs::remove_file(&path) {
                    output.push_str(&format!(
                        "Failed to remove package from overlay: {error}\n"
                    ));
                    return (output, false, Some(overlay));
                }

                packages.push(filename);
            }
        }

        if packages.is_empty() {
            output.push_str("\nmakepkg produced no package artifacts.\n");
            success = false;
        } else {
            output.push_str(&format!(
                "\nMoved {} package artifact(s) to {}.\n",
                packages.len(),
                repository_dir.display()
            ));

            let database =
                repository_dir.join(format!("{repository_name}.db.tar.gz"));

            let mut repo_add = std::process::Command::new("repo-add");
            repo_add.arg(&database);

            for package in &packages {
                repo_add.arg(repository_dir.join(package));
            }

            let result = repo_add
                .output()
                .expect("failed to run repo-add");

            output.push_str(&String::from_utf8_lossy(&result.stdout));
            output.push_str(&String::from_utf8_lossy(&result.stderr));

            if !result.status.success() {
                success = false;
            }
        }
    }

    if success {
        event_tx
            .send(crate::worker::WorkerEvent::CookStatus {
                recipe_index,
                status: "updating repository".to_owned(),
            })
            .ok();

        output.push_str("\nUpdating pacman repository database...\n");

        let config = repository::config_path(repository_name);

        let result = std::process::Command::new("sudo")
            .args([
                "pacman",
                "--config",
                config
                    .to_str()
                    .expect("repository config path is not valid UTF-8"),
                "-Sy",
            ])
            .output()
            .expect("failed to run sudo pacman");

        output.push_str(&String::from_utf8_lossy(&result.stdout));
        output.push_str(&String::from_utf8_lossy(&result.stderr));

        success = result.status.success();
    }

    output.push_str(if success {
        "\nBuild completed successfully."
    } else {
        "\nBuild failed."
    });

    (output, success, Some(overlay))
}

pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    worker_tx: &std::sync::mpsc::Sender<crate::worker::WorkerCommand>,
    worker_rx: &std::sync::mpsc::Receiver<crate::worker::WorkerEvent>,
    recipe_index: usize,
    recipe: &Recipe,
    cache_dir: &Path,
    repository_name: &str,
    log: bool,
    start_cook: bool,
) -> io::Result<bool> {
    let patches = recipe
        .patches
        .iter()
        .filter(|patch| patch.selected)
        .map(|patch| patch.path.clone())
        .collect::<Vec<_>>();

    if start_cook {
        worker_tx
            .send(crate::worker::WorkerCommand::Cook {
                recipe_index,
                recipe_name: recipe.name.clone(),
                patches,
                cache_dir: cache_dir.to_path_buf(),
                repository_name: repository_name.to_owned(),
                log,
            })
            .map_err(|_| io::Error::other("worker thread exited"))?;
    }

    let mut output = if start_cook {
        String::from("Cooking...\n")
    } else {
        String::from("Cook in progress...\n")
    };

    let mut finished = false;
    let mut overlay = None;

    loop {
        while let Ok(event) = worker_rx.try_recv() {
            match event {
                crate::worker::WorkerEvent::CookOutput {
                    recipe_index: cooked_recipe,
                    output: chunk,
                } => {
                    if cooked_recipe == recipe_index {
                        output.push_str(&chunk);
                    }
                }

                crate::worker::WorkerEvent::Cooked {
                    recipe_index: cooked_recipe,
                    output: build_output,
                    success,
                    overlay: cooked_overlay,
                } => {
                    if cooked_recipe == recipe_index {
                        output = build_output;
                        finished = success;
                        overlay = cooked_overlay;
                    }
                }

                _ => {}
            }
        }

        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);

            let block = Block::default()
                .title(format!(" POT — The Kitchen — {} ", recipe.name))
                .borders(Borders::ALL);

            let visible_lines = chunks[0].height.saturating_sub(2) as usize;
            let line_count = output.lines().count();
            let scroll = line_count.saturating_sub(visible_lines) as u16;

            let body = Paragraph::new(output.as_str())
                .block(block)
                .scroll((scroll, 0));

            frame.render_widget(body, chunks[0]);

            frame.render_widget(
                Paragraph::new("[Esc] Return to Cookbook    [Ctrl-C] Abort"),
                chunks[1],
            );
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('c')
                {
                    worker_tx.send(crate::worker::WorkerCommand::AbortCook).ok();
                } else {
                    match key.code {
                        KeyCode::Esc => {
                            drop(overlay);
                            return Ok(finished);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
