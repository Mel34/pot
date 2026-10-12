use std::path::{Path, PathBuf};
use std::process::Command;

use crate::prepare::GitSource;
use crate::proc;

pub enum Verification {
    Applicable(PathBuf, usize),
    #[allow(dead_code)]
    Applied(PathBuf, usize),
    Conflict(String),
}

/// `-p1` is by far the most common (`a/` `b/` prefixes), so try it first.
const STRIP_LEVELS: [usize; 7] = [1, 0, 2, 3, 4, 5, 6];

pub fn verify(work_dir: &Path, git_sources: &[GitSource], patch: &Path) -> Verification {
    if !patch.is_file() {
        return Verification::Conflict(format!("patch file not found: {}", patch.display()));
    }

    if git_sources.is_empty() {
        return Verification::Conflict(
            "the PKGBUILD has no git+ source to apply patches to".to_owned(),
        );
    }

    let mut notes = Vec::new();

    for source in git_sources {
        let source_dir = work_dir.join("src").join(&source.name);

        if !source_dir.is_dir() {
            notes.push(format!(
                "{}: source directory {} does not exist",
                source.name,
                source_dir.display()
            ));
            continue;
        }

        let mut errors = Vec::new();

        for strip_level in STRIP_LEVELS {
            let forward_error = match git_apply_check(&source_dir, patch, strip_level, false) {
                Ok(None) => return Verification::Applicable(source_dir, strip_level),
                Ok(Some(message)) => message,
                Err(error) => return Verification::Conflict(error),
            };

            match git_apply_check(&source_dir, patch, strip_level, true) {
                Ok(None) => return Verification::Applied(source_dir, strip_level),
                Ok(Some(_)) => {}
                Err(error) => return Verification::Conflict(error),
            }

            errors.push(forward_error);
        }

        notes.push(format!("{}: {}", source.name, best_error(&errors)));
    }

    Verification::Conflict(notes.join("\n"))
}

/// `Ok(None)` if `git apply --check` succeeds, `Ok(Some(why))` if it does
/// not, `Err` if git could not be run at all.
fn git_apply_check(
    source_dir: &Path,
    patch: &Path,
    strip_level: usize,
    reverse: bool,
) -> Result<Option<String>, String> {
    let mut command = Command::new("git");
    command.arg("apply").arg("--check");

    if reverse {
        command.arg("--reverse");
    }

    command
        .arg(format!("-p{strip_level}"))
        .arg(patch)
        .current_dir(source_dir);

    let result = proc::capture(&mut command).map_err(|error| error.to_string())?;

    if result.success {
        return Ok(None);
    }

    let text = result.stderr.trim();

    Ok(Some(if text.is_empty() {
        result.error_line()
    } else {
        text.to_owned()
    }))
}

/// The error from the first strip level at which git actually found the
/// files (later levels only say "No such file").
fn best_error(errors: &[String]) -> String {
    let informative = |text: &&String| {
        !text.is_empty()
            && !text.contains("No such file or directory")
            && !text.contains("does not exist in index")
    };

    errors
        .iter()
        .find(informative)
        .or_else(|| errors.first())
        .cloned()
        .unwrap_or_else(|| "patch does not apply".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(dir).status().unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// work/src/demo is a git checkout containing `hello.txt`.
    fn fixture(name: &str) -> (PathBuf, Vec<GitSource>) {
        let root = std::env::temp_dir().join(format!("pot-patch-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);

        let source = root.join("src").join("demo");
        fs::create_dir_all(&source).unwrap();

        run(&source, &["init", "-q"]);
        fs::write(source.join("hello.txt"), "hello\n").unwrap();

        (
            root,
            vec![GitSource {
                name: "demo".to_owned(),
                url: "https://example.invalid/demo.git".to_owned(),
                reference: None,
            }],
        )
    }

    fn write_patch(root: &Path, from: &str, to: &str) -> PathBuf {
        let path = root.join("change.patch");

        fs::write(
            &path,
            format!(
                "--- a/hello.txt\n+++ b/hello.txt\n@@ -1 +1 @@\n-{from}\n+{to}\n"
            ),
        )
        .unwrap();

        path
    }

    #[test]
    fn applicable_patch_is_detected_with_strip_level() {
        let (root, sources) = fixture("applicable");
        let patch = write_patch(&root, "hello", "goodbye");

        match verify(&root, &sources, &patch) {
            Verification::Applicable(dir, 1) => assert_eq!(dir, root.join("src/demo")),
            Verification::Applicable(_, level) => panic!("unexpected strip level {level}"),
            Verification::Applied(..) => panic!("reported as already applied"),
            Verification::Conflict(error) => panic!("conflict: {error}"),
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn already_applied_patch_is_detected() {
        let (root, sources) = fixture("applied");
        let patch = write_patch(&root, "hello", "goodbye");

        fs::write(root.join("src/demo/hello.txt"), "goodbye\n").unwrap();

        assert!(matches!(verify(&root, &sources, &patch), Verification::Applied(_, 1)));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflict_explains_why() {
        let (root, sources) = fixture("conflict");
        let patch = write_patch(&root, "something else", "goodbye");

        match verify(&root, &sources, &patch) {
            Verification::Conflict(error) => {
                assert!(error.starts_with("demo: "), "{error}");
                assert!(error.contains("hello.txt"), "{error}");
                assert!(!error.contains("No such file"), "{error}");
            }
            _ => panic!("expected a conflict"),
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_sources_and_files_are_explained() {
        let (root, sources) = fixture("missing");

        assert!(matches!(
            verify(&root, &[], &root.join("change.patch")),
            Verification::Conflict(_)
        ));

        match verify(&root, &sources, &root.join("nope.patch")) {
            Verification::Conflict(error) => assert!(error.contains("not found"), "{error}"),
            _ => panic!("expected a conflict"),
        }

        let other = vec![GitSource {
            name: "other".to_owned(),
            url: String::new(),
            reference: None,
        }];

        let patch = write_patch(&root, "hello", "goodbye");

        match verify(&root, &other, &patch) {
            Verification::Conflict(error) => assert!(error.contains("does not exist"), "{error}"),
            _ => panic!("expected a conflict"),
        }

        fs::remove_dir_all(root).unwrap();
    }
}
