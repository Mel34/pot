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

fn is_official_repository(name: &str) -> bool {
    matches!(
        name,
        "core"
            | "core-testing"
            | "extra"
            | "extra-testing"
            | "multilib"
            | "multilib-testing"
            | "gnome-unstable"
            | "kde-unstable"
    )
}

fn insert_repository_include(contents: &str, include: &str) -> String {
    let newline = if contents.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut offset = 0;
    let mut insertion = None;

    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];

            if is_official_repository(name) {
                insertion = Some(offset);
                break;
            }
        }

        offset += line.len();
    }

    let mut updated = contents.to_owned();

    if let Some(offset) = insertion {
        updated.insert_str(offset, &format!("{include}{newline}"));
    } else {
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push_str(newline);
        }
        updated.push_str(include);
        updated.push_str(newline);
    }

    updated
}

#[allow(dead_code)]
fn ensure_pacman_conf(repository: &str) -> Result<(), String> {
    let path = PathBuf::from("/etc/pacman.conf");
    let include = format!("Include = /etc/pacman.d/{repository}");

    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read /etc/pacman.conf: {error}"))?;

    if contents.lines().any(|line| line.trim() == include) {
        return Ok(());
    }

    let updated = insert_repository_include(&contents, &include);

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

    let uid = Uid::effective().as_raw();
    crate::helper::setup_repository(repository, uid)?;

    let repository_dir = PathBuf::from("/var/cache/pacman").join(repository);
    let database = repository_dir.join(format!("{repository}.db.tar.gz"));

    interactive(Command::new("repo-add").arg(&database), "repo-add")?;

    println!("Local repository '{repository}' created.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::insert_repository_include;

    const INCLUDE: &str = "Include = /etc/pacman.d/test-host";

    #[test]
    fn inserts_before_first_active_official_testing_repository() {
        let config = "# header\n[options]\nColor\n\n# repos\n#[core]\n[core-testing]\nInclude = mirrorlist\n";

        assert_eq!(
            insert_repository_include(config, INCLUDE),
            "# header\n[options]\nColor\n\n# repos\n#[core]\nInclude = /etc/pacman.d/test-host\n[core-testing]\nInclude = mirrorlist\n"
        );
    }

    #[test]
    fn inserts_before_first_unstable_repository() {
        let config = "[options]\nColor\n[kde-unstable]\nInclude = mirrorlist\n[core-testing]\n";

        assert_eq!(
            insert_repository_include(config, INCLUDE),
            "[options]\nColor\nInclude = /etc/pacman.d/test-host\n[kde-unstable]\nInclude = mirrorlist\n[core-testing]\n"
        );
    }

    #[test]
    fn skips_custom_repository_and_inserts_before_first_official_repository() {
        let config = "[options]\nColor\n[my-custom]\nServer = file:///packages\n[extra]\nInclude = mirrorlist\n";

        assert_eq!(
            insert_repository_include(config, INCLUDE),
            "[options]\nColor\n[my-custom]\nServer = file:///packages\nInclude = /etc/pacman.d/test-host\n[extra]\nInclude = mirrorlist\n"
        );
    }

    #[test]
    fn ignores_staging_repositories() {
        let config = "[options]\nColor\n[core-staging]\nInclude = mirrorlist\n[core]\nInclude = mirrorlist\n";

        assert_eq!(
            insert_repository_include(config, INCLUDE),
            "[options]\nColor\n[core-staging]\nInclude = mirrorlist\nInclude = /etc/pacman.d/test-host\n[core]\nInclude = mirrorlist\n"
        );
    }

    #[test]
    fn appends_when_no_official_repository_exists() {
        let config = "[options]\nColor\n[my-custom]\nServer = file:///packages\n";

        assert_eq!(
            insert_repository_include(config, INCLUDE),
            "[options]\nColor\n[my-custom]\nServer = file:///packages\nInclude = /etc/pacman.d/test-host\n"
        );
    }

    #[test]
    fn preserves_crlf_line_endings() {
        let config = "[options]\r\nColor\r\n[core]\r\nInclude = mirrorlist\r\n";

        assert_eq!(
            insert_repository_include(config, INCLUDE),
            "[options]\r\nColor\r\nInclude = /etc/pacman.d/test-host\r\n[core]\r\nInclude = mirrorlist\r\n"
        );
    }
}
