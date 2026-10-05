use std::path::{Path, PathBuf};
use std::process::Command;

pub struct PrepareResult {
    pub work_dir: PathBuf,
    pub success: bool,
    pub git_sources: Vec<String>,
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
            git_sources: Vec::new(),
        };
    }

    let work_dir = work_root.join(package);

    let srcinfo = run_command(
        Command::new("makepkg")
            .args(["--printsrcinfo"])
            .current_dir(&work_dir),
    );

    if !srcinfo.success {
        return PrepareResult {
            work_dir,
            success: false,
            git_sources: Vec::new(),
        };
    }

    let git_sources: Vec<String> = srcinfo
        .stdout
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("source = git+"))
        .map(str::to_owned)
        .collect();


    if let Some(source) = git_sources.first() {
        if !prepare_git_source(&work_dir, source) {
            return PrepareResult {
                work_dir,
                success: false,
                git_sources,
            };
        }
    }

    let makepkg = run_command(
        Command::new("makepkg")
            .args(["-sfeo", "--noconfirm", "--holdver"])
            .current_dir(&work_dir),
    );

    PrepareResult {
        work_dir,
        success: makepkg.success,
        git_sources,
    }
}

fn prepare_git_source(work_dir: &Path, source: &str) -> bool {
    let source = source.strip_suffix('"').unwrap_or(source);

    let (url, fragment) = match source.split_once('#') {
        Some(value) => value,
        None => return false,
    };

    let (url, _) = url.split_once('?').unwrap_or((url, ""));
    let (kind, revision) = match fragment.split_once('=') {
        Some(value) => value,
        None => return false,
    };

    if kind != "tag" && kind != "commit" {
        return false;
    }

    let repo_name = match url.rsplit('/').next() {
        Some(name) => name.strip_suffix(".git").unwrap_or(name),
        None => return false,
    };

    let cache_repo = work_dir.join(repo_name);
    let src_dir = work_dir.join("src").join(repo_name);

    if !cache_repo.exists() {
        let init = run_command(
            Command::new("git")
                .args(["init", "--bare"])
                .arg(&cache_repo),
        );

        if !init.success {
            return false;
        }

        let remote = run_command(
            Command::new("git")
                .args(["-C"])
                .arg(&cache_repo)
                .args(["remote", "add", "origin"])
                .arg(url),
        );

        if !remote.success {
            return false;
        }
    }

    if !src_dir.exists() {
        let fetch_ref = format!(
            "refs/{}/{}:refs/{}/{}",
            if kind == "tag" { "tags" } else { "heads" },
            revision,
            if kind == "tag" { "tags" } else { "heads" },
            revision
        );

        let fetch = run_command(
            Command::new("git")
                .args(["-C"])
                .arg(&cache_repo)
                .args(["fetch", "--depth=1", "origin"])
                .arg(&fetch_ref),
        );

        if !fetch.success {
            return false;
        }

        let clone = run_command(
            Command::new("git")
                .args(["clone", "--no-checkout"])
                .arg(&cache_repo)
                .arg(&src_dir),
        );

        if !clone.success {
            return false;
        }

        let checkout = run_command(
            Command::new("git")
                .args(["-C"])
                .arg(&src_dir)
                .args(["checkout", "--detach"])
                .arg(revision),
        );

        if !checkout.success {
            return false;
        }
    }

    true
}

pub fn git_sources(cache_dir: &Path, package: &str) -> Vec<String> {
    let work_dir = cache_dir.join("work").join(package);

    let output = Command::new("makepkg")
        .args(["--printsrcinfo"])
        .current_dir(&work_dir)
        .output()
        .expect("failed to run makepkg");

    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("source = git+"))
        .map(str::to_owned)
        .collect()
}

pub fn is_prepared(cache_dir: &Path, package: &str) -> bool {
    cache_dir.join("work").join(package).join("src").is_dir()
}

struct CommandResult {
    success: bool,
    stdout: String,
}

fn run_command(command: &mut Command) -> CommandResult {
    let output = command.output().expect("failed to run command");

    CommandResult {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    }
}
