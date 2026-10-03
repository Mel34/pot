mod paths;
mod repository;

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
    let cookbook_dir = paths::cookbook_dir();
    let package_cookbook_dir = cookbook_dir.join(&package);
    let repository_dir = repository::repository_dir();

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
    println!("Repository: {}", repository_dir.display());
    println!("Cookbook:   {}", package_cookbook_dir.display());

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

    let mut patches = Vec::new();

    if package_cookbook_dir.exists() {
        for entry in std::fs::read_dir(&package_cookbook_dir)
            .expect("failed to read cookbook package directory")
        {
            let path = entry
                .expect("failed to read cookbook package directory entry")
                .path();

            if path.extension().and_then(|ext| ext.to_str()) == Some("patch") {
                patches.push(path);
            }
        }
    }

    patches.sort();

    for patch in patches {
        println!("Applying overlay: {}", patch.display());

        let status = std::process::Command::new("git")
            .args(["apply", "-p3"])
            .arg(&patch)
            .current_dir(work_dir.join("src").join("nautilus"))
            .status()
            .expect("failed to apply overlay patch");

        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
    }

    println!("Overlays applied.");

    for entry in std::fs::read_dir(&work_dir).expect("failed to read POT work directory") {
        let path = entry
            .expect("failed to read POT work directory entry")
            .path();

        if path.to_string_lossy().ends_with(".pkg.tar.zst") {
            std::fs::remove_file(&path).expect("failed to remove stale package artifact");
        }
    }

    let status = std::process::Command::new("makepkg")
        .args(["-e"])
        .current_dir(&work_dir)
        .status()
        .expect("failed to run makepkg");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let mut packages = Vec::new();

    for entry in std::fs::read_dir(&work_dir).expect("failed to read POT work directory") {
        let path = entry
            .expect("failed to read POT work directory entry")
            .path();

        if path.extension().and_then(|ext| ext.to_str()) == Some("zst")
            && path.to_string_lossy().ends_with(".pkg.tar.zst")
        {
            let filename = path.file_name().expect("package has no filename");
            let destination = repository_dir.join(filename);

            std::fs::rename(&path, &destination)
                .expect("failed to move package into local repository");

            packages.push(filename.to_owned());
        }
    }

    if packages.is_empty() {
        eprintln!("makepkg produced no package artifacts");
        std::process::exit(1);
    }

    println!(
        "Moved {} package artifact(s) to {}.",
        packages.len(),
        repository_dir.display()
    );

    let hostname = repository::name();
    let database = repository_dir.join(format!("{hostname}.db.tar.gz"));

    let mut repo_add = std::process::Command::new("sudo");
    repo_add.arg("repo-add").arg(database);

    for package in &packages {
        repo_add.arg(repository_dir.join(package));
    }

    let status = repo_add.status().expect("failed to run repo-add");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let config = repository::config_path();

    let status = std::process::Command::new("sudo")
        .args([
            "pacman",
            "--config",
            config
                .to_str()
                .expect("repository config path is not valid UTF-8"),
            "-Sy",
        ])
        .status()
        .expect("failed to refresh local pacman repository");

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    println!("Local repository updated.");
}
