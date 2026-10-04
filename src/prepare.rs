use std::path::{Path, PathBuf};
use std::process::Command;

pub struct PrepareResult {
    pub work_dir: PathBuf,
    pub success: bool,
}

pub fn prepare_sources(
    cache_dir: &Path,
    package: &str,
    version: &str,
    _log: bool,
) -> PrepareResult {
    let work_root = cache_dir.join("work");

    std::fs::create_dir_all(&work_root)
        .expect("failed to create POT work directory");

    let clone = run_command(
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
    );

    if !clone.success {
        return PrepareResult {
            work_dir: work_root.join(package),
            success: false,
        };
    }

    let work_dir = work_root.join(package);

    let makepkg = run_command(
        Command::new("makepkg")
            .args(["-sfo", "--noconfirm"])
            .current_dir(&work_dir),
    );

    PrepareResult {
        work_dir,
        success: makepkg.success,
    }
}

pub fn is_prepared(cache_dir: &Path, package: &str) -> bool {
    cache_dir.join("work").join(package).join("src").is_dir()
}

struct CommandResult {
    success: bool,
}

fn run_command(command: &mut Command) -> CommandResult {
    let output = command.output().expect("failed to run command");

    CommandResult {
        success: output.status.success(),
    }
}
