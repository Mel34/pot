use std::process::Command;

pub fn resolve(package: &str) -> (String, String) {
    let output = Command::new("pacman")
        .args(["-Sp", "--print-format", "%r %n %v", package])
        .output()
        .expect("failed to run pacman");

    if !output.status.success() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let selection = String::from_utf8_lossy(&output.stdout);
    let mut fields = selection.split_whitespace();

    let _repository = fields.next().expect("pacman returned no repository");
    let selected_package = fields.next().expect("pacman returned no package");
    let version = fields.next().expect("pacman returned no version");

    if selected_package != package {
        eprintln!("pacman selected unexpected package: {selected_package}");
        std::process::exit(1);
    }

    (selected_package.to_string(), version.to_string())
}
