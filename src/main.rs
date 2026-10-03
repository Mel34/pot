mod paths;

use std::env;

fn main() {
    let package = match env::args().nth(1) {
        Some(package) => package,
        None => {
            eprintln!("usage: pot <package>");
            std::process::exit(2);
        }
    };

    let cache_dir = paths::cache_dir();
    std::fs::create_dir_all(&cache_dir).expect("failed to create POT cache directory");

    let output = std::process::Command::new("pacman")
        .args(["-Sp", "--print-format", "%r %n %v", &package])
        .output()
        .expect("failed to run pacman");

    if !output.status.success() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let selection = String::from_utf8_lossy(&output.stdout);
    let mut fields = selection.split_whitespace();

    let repository = fields.next().expect("pacman returned no repository");
    let selected_package = fields.next().expect("pacman returned no package");
    let version = fields.next().expect("pacman returned no version");

    if selected_package != package {
        eprintln!("pacman selected unexpected package: {selected_package}");
        std::process::exit(1);
    }

    let work_dir = cache_dir.join("work").join(&package);

    std::fs::create_dir_all(cache_dir.join("work")).expect("failed to create POT work directory");

    println!("Repository: {repository}");
    println!("Package:    {selected_package}");
    println!("Version:    {version}");
    println!("Workspace:  {}", work_dir.display());

    let status = std::process::Command::new("pkgctl")
        .args([
            "repo",
            "clone",
            "--protocol",
            "https",
            "--switch",
            version,
            &package,
        ])
        .current_dir(cache_dir.join("work"))
        .status()
        .expect("failed to run pkgctl");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    
    let status = std::process::Command::new("makepkg")
        .args(["-so"])
        .current_dir(&work_dir)
        .status()
        .expect("failed to run makepkg");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let status = std::process::Command::new("git")
        .args([
            "apply",
            "-p3",
            "/home/anon/Projects/nautilus-touchpad-swipe.patch",
        ])
        .current_dir(work_dir.join("src").join("nautilus"))
        .status()
        .expect("failed to apply overlay patch");

    println!("Overlay applied.");

    let status = std::process::Command::new("makepkg")
        .args(["-e"])
        .current_dir(&work_dir)
        .status()
        .expect("failed to run makepkg");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}
