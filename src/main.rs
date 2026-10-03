mod cookbook;
mod config;
mod paths;
mod repository;
mod ui;

use std::env;
use std::process::Command;

fn run_command(command: &mut Command, verbose: bool) -> bool {
    if verbose {
        command
            .status()
            .expect("failed to run command")
            .success()
    } else {
        let output = command
            .output()
            .expect("failed to run command");

        if !output.status.success() {
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }

        output.status.success()
    }
}

fn main() {
    let config_path = paths::config_path();

    if !config_path.exists() {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .expect("failed to create POT configuration directory");
        }

        let hostname = repository::name();

        std::fs::write(
            &config_path,
            format!(
                "################################################\n\
                 # POT configuration                            #\n\
                 # Uncomment an option to override the default. #\n\
                 #                                              #\n\
                 # Defaults shown below are used when options   #\n\
                 # are commented out. The default repository    #\n\
                 # name is the machine's hostname.              #\n\
                 ################################################\n\
                 \n\
                 # Path to the cookbook.\n\
                 # Default: ~/Projects/cookbook\n\
                 # Cookbook = ~/Projects/cookbook\n\
                 \n\
                 # Name of the local pacman repository.\n\
                 # Default: {hostname}\n\
                 # Repository = {hostname}\n\
                 \n\
                 # Interface verbosity.\n\
                 # Default: false\n\
                 # Verbose = false\n",
            ),
        )
        .expect("failed to create POT configuration file");
    }

    let config = config::load(&config_path);
    let verbose = config.verbose.unwrap_or(false);

    let package = match env::args().nth(1) {
        Some(package) => package,
        None => {
            let repository_name = config
                .repository
                .clone()
                .unwrap_or_else(repository::name);

            repository::ensure(&repository_name);

            let cookbook_dir = paths::cookbook_dir(config.cookbook.as_deref());

            let recipes = match cookbook::discover(&cookbook_dir) {
                Ok(recipes) => recipes,
                Err(error) => {
                    eprintln!("Failed to discover cookbook: {error}");
                    std::process::exit(1);
                }
            };

            if let Err(error) = ui::run(recipes) {
                eprintln!("POT interface error: {error}");
                std::process::exit(1);
            }

            return;
        }
    };

    let cache_dir = paths::cache_dir();
    let cookbook_dir = paths::cookbook_dir(config.cookbook.as_deref());
    let package_cookbook_dir = cookbook_dir.join(&package);

    let repository_name = config
        .repository
        .clone()
        .unwrap_or_else(repository::name);

    repository::ensure(&repository_name);

    let repository_dir = repository::repository_dir(&repository_name);

    std::fs::create_dir_all(&cache_dir)
        .expect("failed to create POT cache directory");

    let output = Command::new("pacman")
        .args(["-Sp", "--print-format", "%r %n %v", &package])
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

    let work_dir = cache_dir.join("work").join(&package);

    std::fs::create_dir_all(cache_dir.join("work"))
        .expect("failed to create POT work directory");

    println!("Repository: {repository_name}");
    println!("Package:    {selected_package}");
    println!("Version:    {version}");
    println!("Repository: {}", repository_dir.display());
    println!("Cookbook:   {}", package_cookbook_dir.display());

    if !run_command(
        Command::new("pkgctl")
            .args([
                "repo",
                "clone",
                "--protocol",
                "https",
                "--switch",
                version,
                &package,
            ])
            .current_dir(cache_dir.join("work")),
        verbose,
    ) {
        std::process::exit(1);
    }

    if !run_command(
        Command::new("makepkg")
            .args(["-so"])
            .current_dir(&work_dir),
        verbose,
    ) {
        std::process::exit(1);
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

        if !run_command(
            Command::new("git")
                .args(["apply", "-p3"])
                .arg(&patch)
                .current_dir(work_dir.join("src").join("nautilus")),
            verbose,
        ) {
            std::process::exit(1);
        }
    }

    println!("Overlays applied.");

    for entry in std::fs::read_dir(&work_dir)
        .expect("failed to read POT work directory entry")
    {
        let path = entry
            .expect("failed to read POT work directory entry")
            .path();

        if path.to_string_lossy().ends_with(".pkg.tar.zst") {
            std::fs::remove_file(&path)
                .expect("failed to remove stale package artifact");
        }
    }

    if !run_command(
        Command::new("makepkg")
            .args(["-e"])
            .current_dir(&work_dir),
        verbose,
    ) {
        std::process::exit(1);
    }

    let mut packages = Vec::new();

    for entry in std::fs::read_dir(&work_dir)
        .expect("failed to read POT work directory")
        .flatten()
    {
        let path = entry.path();

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

    let database = repository_dir.join(format!("{repository_name}.db.tar.gz"));

    let mut repo_add = Command::new("sudo");
    repo_add.arg("repo-add").arg(database);

    for package in &packages {
        repo_add.arg(repository_dir.join(package));
    }

    if !run_command(&mut repo_add, verbose) {
        std::process::exit(1);
    }

    let config = repository::config_path(&repository_name);

    if !run_command(
        Command::new("sudo")
            .args([
                "pacman",
                "--config",
                config
                    .to_str()
                    .expect("repository config path is not valid UTF-8"),
                "-Sy",
            ]),
        verbose,
    ) {
        std::process::exit(1);
    }

    println!("Local repository updated.");
}
