use std::path::{Path, PathBuf};
use std::process::Command;

pub struct PrepareResult {
    pub work_dir: PathBuf,
    pub output: String,
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

    let mut output = String::new();

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

    output.push_str(&clone.output);

    if !clone.success {
        return PrepareResult {
            work_dir: work_root.join(package),
            output,
            success: false,
        };
    }

    let work_dir = work_root.join(package);

    let makepkg = run_command(
        Command::new("makepkg")
            .args(["-sfo", "--noconfirm"])
            .current_dir(&work_dir),
    );

    output.push_str(&makepkg.output);

    PrepareResult {
        work_dir,
        output,
        success: makepkg.success,
    }
}

pub fn is_prepared(cache_dir: &Path, package: &str) -> bool {
    cache_dir.join("work").join(package).join("src").is_dir()
}

struct CommandResult {
    output: String,
    success: bool,
}

fn run_command(command: &mut Command) -> CommandResult {
    let output = command.output().expect("failed to run command");

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    CommandResult {
        output: text,
        success: output.status.success(),
    }
}
