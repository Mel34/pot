use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use nix::unistd::Uid;

pub fn name() -> String {
    let output = Command::new("hostnamectl")
        .arg("--static")
        .output()
        .expect("failed to run hostnamectl");

    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();

    if name.is_empty() {
        eprintln!("hostnamectl returned an empty hostname");
        std::process::exit(1);
    }

    name
}

pub fn config_path(repository: &str) -> PathBuf {
    PathBuf::from("/etc/pacman.d").join(repository)
}

pub fn repository_dir(repository: &str) -> PathBuf {
    let output = Command::new("pacman-conf")
        .output()
        .expect("failed to run pacman-conf");

    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let config = String::from_utf8_lossy(&output.stdout);

    let mut in_repository = false;

    for line in config.lines() {
        let line = line.trim();

        if line.starts_with('[') && line.ends_with(']') {
            in_repository = &line[1..line.len() - 1] == repository;
            continue;
        }

        if in_repository {
            if let Some(server) = line.strip_prefix("Server =") {
                let server = server.trim();

                if let Some(path) = server.strip_prefix("file://") {
                    return PathBuf::from(path);
                }

                eprintln!("repository {repository} does not use a local file:// server");
                std::process::exit(1);
            }
        }
    }

    PathBuf::from("/var/cache/pacman").join(repository)
}

pub fn ensure(repository: &str) {
    if config_path(repository).exists() {
        return;
    }

    println!("Local repository '{repository}' is not configured.");
    print!("Create it? [Y/n] ");
    io::stdout().flush().expect("failed to flush stdout");

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .expect("failed to read response");

    let answer = answer.trim();

    if !answer.is_empty() && !answer.eq_ignore_ascii_case("y") {
        eprintln!("Local repository was not created.");
        std::process::exit(1);
    }

    let repository_dir = PathBuf::from("/var/cache/pacman").join(repository);
    let config_path = config_path(repository);

    let status = Command::new("sudo")
        .args(["mkdir", "-p"])
        .arg(&repository_dir)
        .status()
        .expect("failed to run sudo");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let uid = Uid::effective().as_raw();

    let status = Command::new("sudo")
        .args(["chown", &format!("{uid}:{uid}")])
        .arg(&repository_dir)
        .status()
        .expect("failed to run sudo");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

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
        .expect("failed to create temporary repository configuration");

    let status = Command::new("sudo")
        .args(["install", "-m", "644"])
        .arg(&temp_path)
        .arg(&config_path)
        .status()
        .expect("failed to run sudo");

    let _ = std::fs::remove_file(&temp_path);

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let database = repository_dir.join(format!("{repository}.db.tar.gz"));

    let status = Command::new("sudo")
        .arg("repo-add")
        .arg(&database)
        .status()
        .expect("failed to run repo-add");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    println!("Local repository '{repository}' created.");
}
