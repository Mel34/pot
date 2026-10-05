use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn log(message: &str) {
    if std::env::var("DEBUG").as_deref() != Ok("1") {
        return;
    }

    if let Ok(home) = std::env::var("HOME") {
        let path = Path::new(&home).join("Projects/pot/pot.log");

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{message}");
        }
    }
}

pub enum Verification {
    Applicable(PathBuf, usize),
    Applied(PathBuf, usize),
    Conflict(String),
}

pub fn verify(
    source_root: &Path,
    git_sources: &[String],
    patch: &Path,
) -> Verification {
    let mut last_check_error = String::new();

    for source in git_sources {
        let source = source.split("#").next().unwrap_or(source);
        let source = source.split("?").next().unwrap_or(source);

        let name = source
            .rsplit("/")
            .next()
            .unwrap_or(source)
            .strip_suffix(".git")
            .unwrap_or(source);

        let source_dir = source_root.join("src").join(name);

        log(&format!(
            "POT VERIFY: source_root={} source={} name={} source_dir={} exists={}",
            source_root.display(),
            source,
            name,
            source_dir.display(),
            source_dir.is_dir(),
        ));

        if !source_dir.is_dir() {
            continue;
        }

        for strip_level in 0..=6 {
            log(&format!(
                "POT VERIFY: git apply --check -p{} patch={} cwd={}",
                strip_level,
                patch.display(),
                source_dir.display(),
            ));

            let check = Command::new("git")
                .args(["apply", "--check", &format!("-p{}", strip_level)])
                .arg(patch)
                .current_dir(&source_dir)
                .output()
                .expect("failed to run git apply --check");

            log(&format!(
                "POT VERIFY: forward -p{} status={} stderr={:?}",
                strip_level,
                check.status,
                String::from_utf8_lossy(&check.stderr),
            ));

            if check.status.success() {
                return Verification::Applicable(source_dir, strip_level);
            }

            let reverse_check = Command::new("git")
                .args([
                    "apply",
                    "--reverse",
                    "--check",
                    &format!("-p{}", strip_level),
                ])
                .arg(patch)
                .current_dir(&source_dir)
                .output()
                .expect("failed to run git apply --reverse --check");

            log(&format!(
                "POT VERIFY: reverse -p{} status={} stderr={:?}",
                strip_level,
                reverse_check.status,
                String::from_utf8_lossy(&reverse_check.stderr),
            ));

            if reverse_check.status.success() {
                return Verification::Applied(source_dir, strip_level);
            }

            last_check_error = format!(
                "{}{}",
                String::from_utf8_lossy(&check.stderr),
                String::from_utf8_lossy(&reverse_check.stderr),
            );
        }
    }

    Verification::Conflict(last_check_error)
}
