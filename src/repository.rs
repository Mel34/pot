//! The local pacman repository pot publishes to.
//!
//! The `ensure*` functions run *before* the TUI starts and talk to the user on
//! the plain terminal (including `sudo` password prompts). Everything else is
//! safe to call from background threads: it only returns errors.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use nix::unistd::Uid;

use crate::proc;

pub fn name() -> Result<String, String> {
    let result = proc::capture(Command::new("hostnamectl").arg("--static"))
        .map_err(|error| format!("cannot determine the host name: {error}"))?;

    if !result.success {
        return Err(format!("hostnamectl --static failed: {}", result.error_line()));
    }

    let name = result.stdout.trim().to_owned();

    if name.is_empty() {
        return Err("hostnamectl returned an empty hostname; set `Repository = <name>` in the pot configuration".to_owned());
    }

    Ok(name)
}

pub fn config_path(repository: &str) -> PathBuf {
    PathBuf::from("/etc/pacman.d").join(repository)
}

pub fn repository_dir(repository: &str) -> Result<PathBuf, String> {
    let result = proc::capture(&mut Command::new("pacman-conf")).map_err(|error| error.to_string())?;

    if !result.success {
        return Err(format!("pacman-conf failed: {}", result.error_line()));
    }

    let mut in_repository = false;

    for line in result.stdout.lines() {
        let line = line.trim();

        if line.starts_with('[') && line.ends_with(']') {
            in_repository = &line[1..line.len() - 1] == repository;
            continue;
        }

        if in_repository {
            if let Some(server) = line.strip_prefix("Server =") {
                let server = server.trim();

                if let Some(path) = server.strip_prefix("file://") {
                    return Ok(PathBuf::from(path));
                }

                return Err(format!(
                    "repository {repository} does not use a local file:// server"
                ));
            }
        }
    }

    Ok(PathBuf::from("/var/cache/pacman").join(repository))
}

pub fn database_path(repository: &str) -> Result<PathBuf, String> {
    Ok(repository_dir(repository)?.join(format!("{repository}.db.tar.gz")))
}

fn confirm_creation(repository: &str, message: &str) -> Result<(), String> {
    println!("{message}");
    print!("Create it? [Y/n] ");

    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush stdout: {error}"))?;

    let mut answer = String::new();

    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("failed to read response: {error}"))?;

    let answer = answer.trim();

    if !answer.is_empty() && !answer.eq_ignore_ascii_case("y") {
        return Err("Local repository was not created.".to_owned());
    }

    println!("Creating local repository '{repository}'...");

    Ok(())
}

/// Run a command that may need to talk to the user (sudo password prompt).
fn interactive(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("failed to run {what}: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

fn ensure_pacman_conf(repository: &str) -> Result<(), String> {
    let path = PathBuf::from("/etc/pacman.conf");
    let include = format!("Include = /etc/pacman.d/{repository}");

    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read /etc/pacman.conf: {error}"))?;

    if contents.lines().any(|line| line.trim() == include) {
        return Ok(());
    }

    let mut updated = contents;

    if !updated.ends_with('\n') {
        updated.push('\n');
    }

    updated.push_str(&include);
    updated.push('\n');

    let temp_path = std::env::temp_dir().join("pot-pacman.conf");

    std::fs::write(&temp_path, updated)
        .map_err(|error| format!("failed to create temporary pacman.conf: {error}"))?;

    let result = interactive(
        Command::new("sudo").args(["install", "-m", "644"]).arg(&temp_path).arg(&path),
        "sudo install (pacman.conf)",
    );

    let _ = std::fs::remove_file(&temp_path);

    result?;

    println!("Added repository '{repository}' to /etc/pacman.conf.");

    Ok(())
}

pub fn ensure(repository: &str) -> Result<(), String> {
    ensure_inner(repository, false)
}

pub fn ensure_confirmed(repository: &str) -> Result<(), String> {
    ensure_inner(repository, true)
}

fn ensure_inner(repository: &str, confirmed: bool) -> Result<(), String> {
    ensure_pacman_conf(repository)?;

    let config_path = config_path(repository);

    if config_path.exists() {
        let database = database_path(repository)?;

        if database.exists() {
            return Ok(());
        }

        if !confirmed {
            confirm_creation(
                repository,
                &format!("Local repository '{repository}' has no database."),
            )?;
        }

        interactive(Command::new("repo-add").arg(&database), "repo-add")?;

        println!("Local repository '{repository}' database created.");

        return Ok(());
    }

    if !confirmed {
        confirm_creation(
            repository,
            &format!("Local repository '{repository}' is not configured."),
        )?;
    }

    let repository_dir = PathBuf::from("/var/cache/pacman").join(repository);

    interactive(
        Command::new("sudo").args(["mkdir", "-p"]).arg(&repository_dir),
        "sudo mkdir",
    )?;

    let uid = Uid::effective().as_raw();

    interactive(
        Command::new("sudo")
            .args(["chown", &format!("{uid}:{uid}")])
            .arg(&repository_dir),
        "sudo chown",
    )?;

    let config = format!(
        "[options]\n\
         CacheDir = {}\n\
         CleanMethod = KeepCurrent\n\
         \n\
         [{repository}]\n\
         SigLevel = Optional TrustAll\n\
         Server = file://{}\n",
        repository_dir.display(),
        repository_dir.display()
    );

    let temp_path = std::env::temp_dir().join(format!("pot-repository-{repository}"));

    std::fs::write(&temp_path, config)
        .map_err(|error| format!("failed to create temporary repository configuration: {error}"))?;

    let result = interactive(
        Command::new("sudo")
            .args(["install", "-m", "644"])
            .arg(&temp_path)
            .arg(&config_path),
        "sudo install (repository configuration)",
    );

    let _ = std::fs::remove_file(&temp_path);

    result?;

    let database = repository_dir.join(format!("{repository}.db.tar.gz"));

    interactive(Command::new("repo-add").arg(&database), "repo-add")?;

    println!("Local repository '{repository}' created.");

    Ok(())
}
