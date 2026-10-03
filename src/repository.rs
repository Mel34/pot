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

pub fn config_path() -> PathBuf {
    PathBuf::from("/etc/pacman.d").join(name())
}

fn server_path(config: &str) -> PathBuf {
    let server = config
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Server =").map(str::trim))
        .expect("pacman repository configuration has no Server entry");

    let path = server
        .strip_prefix("file://")
        .expect("pacman repository Server is not a local file:// URL");

    PathBuf::from(path)
}

pub fn repository_dir() -> PathBuf {
    let config = std::fs::read_to_string(config_path())
        .expect("failed to read pacman repository configuration");

    server_path(&config)
}

#[cfg(test)]
mod tests {
    use super::server_path;
    use std::path::Path;

    #[test]
    fn parses_local_server_path() {
        let config = "[alimyon]\nServer = file:///var/cache/pacman/alimyon\n";

        assert_eq!(
            server_path(config),
            Path::new("/var/cache/pacman/alimyon")
        );
    }

    #[test]
    fn parses_server_with_whitespace() {
        let config = "\n[alimyon]\n  Server =   file:///var/cache/pacman/alimyon  \n";

        assert_eq!(
            server_path(config),
            Path::new("/var/cache/pacman/alimyon")
        );
    }

    #[test]
    #[should_panic(expected = "pacman repository configuration has no Server entry")]
    fn rejects_missing_server() {
        server_path("[alimyon]\n");
    }

    #[test]
    #[should_panic(expected = "pacman repository Server is not a local file:// URL")]
    fn rejects_non_local_server() {
        server_path("[alimyon]\nServer = https://example.com/repo\n");
    }
}
