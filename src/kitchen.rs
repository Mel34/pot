use std::io;
use std::path::Path;

use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
};

use crate::cookbook::{Recipe, find_source_dir};
use crate::repository;

pub fn cook(
    recipe_name: &str,
    patches: &[std::path::PathBuf],
    cache_dir: &Path,
    repository_name: &str,
    _log: bool,
) -> (String, bool) {
    let work_dir = cache_dir.join("work").join(recipe_name);
    let src_dir = work_dir.join("src");

    let mut output = String::new();
    let mut success = true;

    for patch in patches {
        let source_dir = match find_source_dir(&src_dir, patch) {
            Some(path) => path,
            None => {
                output.push_str(&format!(
                    "Failed to locate source directory for {}.\n",
                    patch.display()
                ));
                success = false;
                break;
            }
        };

        output.push_str(&format!(
            "Applying overlay: {}\n",
            patch.display()
        ));

        let result = std::process::Command::new("git")
            .args(["apply", "-p3"])
            .arg(patch)
            .current_dir(source_dir)
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
                return (output, false);
            }
        } {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    output.push_str(&format!(
                        "Failed to read work directory entry: {error}\n"
                    ));
                    return (output, false);
                }
            };

            if path.to_string_lossy().ends_with(".pkg.tar.zst") {
                if let Err(error) = std::fs::remove_file(path) {
                    output.push_str(&format!(
                        "Failed to remove stale package: {error}\n"
                    ));
                    return (output, false);
                }
            }
        }

        output.push_str("\nRunning makepkg -e...\n\n");

        let result = std::process::Command::new("makepkg")
            .args(["-e"])
            .current_dir(&work_dir)
            .output()
            .expect("failed to run makepkg");

        output.push_str(&String::from_utf8_lossy(&result.stdout));
        output.push_str(&String::from_utf8_lossy(&result.stderr));

        success = result.status.success();
    }

    if success {
        let repository_dir = repository::repository_dir(repository_name);
        let mut packages = Vec::new();

        let entries = match std::fs::read_dir(&work_dir) {
            Ok(entries) => entries,
            Err(error) => {
                output.push_str(&format!(
                    "Failed to read work directory: {error}\n"
                ));
                return (output, false);
            }
        };

        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    output.push_str(&format!(
                        "Failed to read work directory entry: {error}\n"
                    ));
                    return (output, false);
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
                    return (output, false);
                }

                if let Err(error) =
                    std::fs::rename(&path, repository_dir.join(&filename))
                {
                    output.push_str(&format!(
                        "Failed to move package into repository: {error}\n"
                    ));
                    return (output, false);
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

    (output, success)
}

pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    worker_tx: &std::sync::mpsc::Sender<crate::worker::WorkerCommand>,
    worker_rx: &std::sync::mpsc::Receiver<crate::worker::WorkerEvent>,
    recipe: &Recipe,
    cache_dir: &Path,
    repository_name: &str,
    log: bool,
) -> io::Result<()> {
    let patches = recipe
        .patches
        .iter()
        .filter(|patch| patch.selected)
        .map(|patch| patch.path.clone())
        .collect::<Vec<_>>();

    worker_tx
        .send(crate::worker::WorkerCommand::Cook {
            recipe_name: recipe.name.clone(),
            patches,
            cache_dir: cache_dir.to_path_buf(),
            repository_name: repository_name.to_owned(),
            log,
        })
        .map_err(|_| io::Error::other("worker thread exited"))?;

    let mut output = String::from("Cooking...\n");

    loop {
        while let Ok(event) = worker_rx.try_recv() {
            if let crate::worker::WorkerEvent::Cooked {
                output: build_output,
                success: _,
            } = event
            {
                output = build_output;
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

            let body = Paragraph::new(output.as_str()).block(block);

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
