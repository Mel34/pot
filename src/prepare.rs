use std::path::{Path, PathBuf};
use std::process::Command;

pub fn prepare_sources(
    cache_dir: &Path,
    package: &str,
    version: &str,
    verbose: bool,
) -> PathBuf {
    let work_root = cache_dir.join("work");

    std::fs::create_dir_all(&work_root)
        .expect("failed to create POT work directory");

    if !run_command(
        Command::new("pkgctl")
            .args([
                "repo",
                "clone",
                "--protocol",
                "https",
                "--switch",
                version,
                package,
            ])
            .current_dir(&work_root),
        verbose,
    ) {
        std::process::exit(1);
    }

    let work_dir = work_root.join(package);

    if !work_dir.join("src").is_dir() {
        if !run_command(
            Command::new("makepkg").args(["-so"]).current_dir(&work_dir),
            verbose,
        ) {
            std::process::exit(1);
        }
    }

    work_dir
}

fn run_command(command: &mut Command, verbose: bool) -> bool {
    if verbose {
        command.status().expect("failed to run command").success()
    } else {
        let output = command.output().expect("failed to run command");

        if !output.status.success() {
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }

        output.status.success()
    }
}
