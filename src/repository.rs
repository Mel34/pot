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
