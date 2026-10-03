use std::env;
use std::process::Command;

fn main() {
    let package = match env::args().nth(1) {
        Some(package) => package,
        None => {
            eprintln!("usage: pot <package>");
            std::process::exit(2);
        }
    };

    let output = Command::new("pacman")
        .args(["-Sp", "--print-format", "%r %n %v", &package])
        .output()
        .expect("failed to run pacman");

    if !output.status.success() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(output.status.code().unwrap_or(1));
    }

    print!("{}", String::from_utf8_lossy(&output.stdout));
}
