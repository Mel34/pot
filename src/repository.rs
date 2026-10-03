use std::path::PathBuf;
use std::process::Command;

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

pub fn repository_dir() -> PathBuf {
    let repository = name();

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

                eprintln!(
                    "repository {repository} does not use a local file:// server"
                );
                std::process::exit(1);
            }
        }
    }

    eprintln!("repository {repository} was not found in pacman configuration");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::repository_dir;
}
