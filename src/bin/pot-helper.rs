use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
enum Operation {
    MountOverlay {
        lower: PathBuf,
        overlay_root: PathBuf,
    },
    UnmountOverlay {
        merged: PathBuf,
    },
    CleanupOverlay {
        overlay_root: PathBuf,
    },
    SetupRepository {
        repository: String,
        uid: u32,
    },
    SyncPackages {
        repository: String,
    },
    InstallPackage {
        package: String,
    },
}

fn parse_operation(args: &[String]) -> Result<Operation, String> {
    let [operation, rest @ ..] = args else {
        return Err("expected an operation".to_owned());
    };

    let expected_args = match operation.as_str() {
        "mount-overlay" => 2,
        "unmount-overlay" => 1,
        "cleanup-overlay" => 1,
        "setup-repository" => 2,
        "sync-packages" => 1,
        "install-package" => 1,
        _ => return Err(format!("unknown operation: {operation}")),
    };

    if rest.len() != expected_args {
        return Err(format!(
            "{operation} expects {expected_args} argument(s), got {}",
            rest.len()
        ));
    }

    match operation.as_str() {
        "mount-overlay" => Ok(Operation::MountOverlay {
            lower: PathBuf::from(&rest[0]),
            overlay_root: PathBuf::from(&rest[1]),
        }),
        "unmount-overlay" => Ok(Operation::UnmountOverlay {
            merged: PathBuf::from(&rest[0]),
        }),
        "cleanup-overlay" => Ok(Operation::CleanupOverlay {
            overlay_root: PathBuf::from(&rest[0]),
        }),
        "setup-repository" => {
            let uid = rest[1]
                .parse::<u32>()
                .map_err(|error| format!("invalid UID '{}': {error}", rest[1]))?;
            Ok(Operation::SetupRepository {
                repository: rest[0].clone(),
                uid,
            })
        }
        "sync-packages" => Ok(Operation::SyncPackages {
            repository: rest[0].clone(),
        }),
        "install-package" => Ok(Operation::InstallPackage {
            package: rest[0].clone(),
        }),
        _ => unreachable!("operation was checked above"),
    }
}

fn validate_repository_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("repository name cannot be empty".to_owned());
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "invalid repository name '{name}': only ASCII alphanumeric, '-', and '_' are allowed"
        ));
    }

    Ok(())
}

fn validate_overlay_chars(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    if text.contains([',', ':', '\\']) {
        return Err(format!(
            "path '{text}' contains characters (',' ':' or '\\') incompatible with OverlayFS options"
        ));
    }
    Ok(())
}

fn validate_overlay_root_safety(root: &Path) -> Result<(), String> {
    if !root.is_absolute() {
        return Err(format!(
            "overlay root '{}' must be an absolute path",
            root.display()
        ));
    }

    let components: Vec<_> = root.components().collect();
    if components.len() < 3 {
        return Err(format!(
            "refusing to operate on suspicious top-level path: '{}'",
            root.display()
        ));
    }

    Ok(())
}

fn is_mounted(path: &Path) -> bool {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };

    mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .any(|point| Path::new(point) == path)
}

fn run_mount_overlay(lower: &Path, overlay_root: &Path) -> Result<(), String> {
    validate_overlay_root_safety(overlay_root)?;
    validate_overlay_chars(lower)?;
    validate_overlay_chars(overlay_root)?;

    let meta = fs::metadata(lower).map_err(|error| {
        format!(
            "cannot read metadata of lower dir {}: {error}",
            lower.display()
        )
    })?;
    let uid = meta.uid();
    let gid = meta.gid();

    fs::create_dir_all(overlay_root)
        .map_err(|error| format!("cannot create {}: {error}", overlay_root.display()))?;
    let _ = chown(overlay_root, Some(uid), Some(gid));

    let upper = overlay_root.join("upper");
    let work = overlay_root.join("work");
    let merged = overlay_root.join("merged");

    for dir in [&upper, &work] {
        if dir.exists() {
            fs::remove_dir_all(dir)
                .map_err(|error| format!("failed to remove stale {}: {error}", dir.display()))?;
        }
    }

    for dir in [&upper, &work, &merged] {
        fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        chown(dir, Some(uid), Some(gid))
            .map_err(|error| format!("cannot chown {}: {error}", dir.display()))?;
    }

    let options = format!(
        "lowerdir={},upperdir={},workdir={},index=off",
        lower.display(),
        upper.display(),
        work.display()
    );

    let status = Command::new("mount")
        .args([
            "-t",
            "overlay",
            "overlay",
            "-o",
            &options,
            &merged.to_string_lossy(),
        ])
        .status()
        .map_err(|error| format!("failed to execute mount: {error}"))?;

    if !status.success() {
        return Err(format!("mount failed with exit code: {status}"));
    }

    Ok(())
}

fn run_unmount_overlay(merged: &Path) -> Result<(), String> {
    if !is_mounted(merged) {
        return Ok(());
    }

    let status = Command::new("umount").arg(merged).status();
    if let Ok(s) = status {
        if s.success() {
            return Ok(());
        }
    }

    thread::sleep(Duration::from_secs(1));

    let status = Command::new("umount").arg(merged).status();
    if let Ok(s) = status {
        if s.success() {
            return Ok(());
        }
    }

    let fallback = Command::new("umount")
        .args(["-l", &merged.to_string_lossy()])
        .status()
        .map_err(|error| format!("failed to execute lazy unmount: {error}"))?;

    if !fallback.success() {
        return Err(format!("lazy unmount failed with exit code: {fallback}"));
    }

    Ok(())
}

fn run_cleanup_overlay(overlay_root: &Path) -> Result<(), String> {
    validate_overlay_root_safety(overlay_root)?;

    let upper = overlay_root.join("upper");
    let work = overlay_root.join("work");

    for dir in [&upper, &work] {
        if dir.exists() {
            fs::remove_dir_all(dir)
                .map_err(|error| format!("failed to cleanup {}: {error}", dir.display()))?;
        }
    }

    Ok(())
}

fn is_official_repository(name: &str) -> bool {
    matches!(
        name,
        "core"
            | "core-testing"
            | "extra"
            | "extra-testing"
            | "multilib"
            | "multilib-testing"
            | "gnome-unstable"
            | "kde-unstable"
    )
}

fn insert_repository_include(contents: &str, include: &str) -> String {
    let newline = if contents.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut offset = 0;
    let mut insertion = None;

    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];

            if is_official_repository(name) {
                insertion = Some(offset);
                break;
            }
        }

        offset += line.len();
    }

    let mut updated = contents.to_owned();

    if let Some(offset) = insertion {
        updated.insert_str(offset, &format!("{include}{newline}"));
    } else {
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push_str(newline);
        }
        updated.push_str(include);
        updated.push_str(newline);
    }

    updated
}

fn run_setup_repository(repository: &str, uid: u32) -> Result<(), String> {
    validate_repository_name(repository)?;

    let pacman_conf = Path::new("/etc/pacman.conf");
    let include_line = format!("Include = /etc/pacman.d/{repository}");

    let contents = fs::read_to_string(pacman_conf)
        .map_err(|error| format!("failed to read /etc/pacman.conf: {error}"))?;

    if !contents.lines().any(|line| line.trim() == include_line) {
        let updated = insert_repository_include(&contents, &include_line);
        let temp_path = pacman_conf.with_extension("pot-tmp");

        fs::write(&temp_path, updated)
            .map_err(|error| format!("failed to write temporary pacman.conf: {error}"))?;
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o644))
            .map_err(|error| format!("failed to set permissions on pacman.conf: {error}"))?;
        fs::rename(&temp_path, pacman_conf)
            .map_err(|error| format!("failed to update /etc/pacman.conf: {error}"))?;
    }

    let repository_dir = PathBuf::from("/var/cache/pacman").join(repository);
    fs::create_dir_all(&repository_dir)
        .map_err(|error| format!("failed to create {}: {error}", repository_dir.display()))?;

    chown(&repository_dir, Some(uid), Some(uid)).map_err(|error| {
        format!(
            "failed to chown {} to {uid}:{uid}: {error}",
            repository_dir.display()
        )
    })?;

    let config_dir = Path::new("/etc/pacman.d");
    if !config_dir.exists() {
        fs::create_dir_all(config_dir)
            .map_err(|error| format!("failed to create /etc/pacman.d: {error}"))?;
    }

    let config_file = config_dir.join(repository);
    let config_content = format!(
        "[options]\n\
         CacheDir = {}\n\
         CleanMethod = KeepCurrent\n\
         \n\
         [{repository}]\n\
         SigLevel = Optional TrustAll\n\
         Server = file://{}\n",
        repository_dir.display(),
        repository_dir.display()
    );

    let temp_config = config_file.with_extension("pot-tmp");
    fs::write(&temp_config, config_content)
        .map_err(|error| format!("failed to write config {}: {error}", temp_config.display()))?;
    fs::set_permissions(&temp_config, fs::Permissions::from_mode(0o644))
        .map_err(|error| format!("failed to set permissions on config: {error}"))?;
    fs::rename(&temp_config, &config_file)
        .map_err(|error| format!("failed to install {}: {error}", config_file.display()))?;

    Ok(())
}

fn run_sync_packages(repository: &str) -> Result<(), String> {
    validate_repository_name(repository)?;

    let config_file = PathBuf::from("/etc/pacman.d").join(repository);
    if !config_file.exists() {
        return Err(format!(
            "repository configuration does not exist: {}",
            config_file.display()
        ));
    }

    let status = Command::new("pacman")
        .arg("-Sy")
        .status()
        .map_err(|error| format!("failed to execute pacman: {error}"))?;

    if !status.success() {
        return Err(format!("pacman -Sy failed with status: {status}"));
    }

    Ok(())
}

fn run_install_package(package: &str) -> Result<(), String> {
    validate_repository_name(package)?;

    // Purge any stale cached archive from the global pacman cache to prevent
    // pacman / libalpm from failing with a checksum mismatch against an older build.
    let pkg_cache = Path::new("/var/cache/pacman/pkg");
    if let Ok(entries) = fs::read_dir(pkg_cache) {
        let prefix = format!("{package}-");
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(&prefix)
                    && (name.ends_with(".pkg.tar.zst") || name.ends_with(".pkg.tar.xz"))
                {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }

    let status = Command::new("pacman")
        .args(["-S", "--noconfirm", package])
        .status()
        .map_err(|error| format!("failed to execute pacman: {error}"))?;

    if !status.success() {
        return Err(format!("pacman -S failed with status: {status}"));
    }

    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let operation = match parse_operation(&args) {
        Ok(op) => op,
        Err(error) => {
            eprintln!("pot-helper error: {error}");
            return ExitCode::from(1);
        }
    };

    let result = match operation {
        Operation::MountOverlay {
            lower,
            overlay_root,
        } => run_mount_overlay(&lower, &overlay_root),
        Operation::UnmountOverlay { merged } => run_unmount_overlay(&merged),
        Operation::CleanupOverlay { overlay_root } => run_cleanup_overlay(&overlay_root),
        Operation::SetupRepository { repository, uid } => run_setup_repository(&repository, uid),
        Operation::SyncPackages { repository } => run_sync_packages(&repository),
        Operation::InstallPackage { package } => run_install_package(&package),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pot-helper error: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_known_operations() {
        assert_eq!(
            parse_operation(&[
                "mount-overlay".into(),
                "/lower".into(),
                "/cache/overlay".into(),
            ]),
            Ok(Operation::MountOverlay {
                lower: PathBuf::from("/lower"),
                overlay_root: PathBuf::from("/cache/overlay"),
            })
        );
        assert_eq!(
            parse_operation(&["unmount-overlay".into(), "/cache/merged".into()]),
            Ok(Operation::UnmountOverlay {
                merged: PathBuf::from("/cache/merged"),
            })
        );
        assert_eq!(
            parse_operation(&["cleanup-overlay".into(), "/cache/overlay".into()]),
            Ok(Operation::CleanupOverlay {
                overlay_root: PathBuf::from("/cache/overlay"),
            })
        );
        assert_eq!(
            parse_operation(&["setup-repository".into(), "pot".into(), "1000".into()]),
            Ok(Operation::SetupRepository {
                repository: "pot".into(),
                uid: 1000,
            })
        );
        assert_eq!(
            parse_operation(&["install-package".into(), "showtime".into()]),
            Ok(Operation::InstallPackage {
                package: "showtime".into(),
            })
        );
        assert_eq!(
            parse_operation(&["sync-packages".into(), "pot".into()]),
            Ok(Operation::SyncPackages {
                repository: "pot".into(),
            })
        );
    }

    #[test]
    fn rejects_missing_or_unknown_operation() {
        assert!(parse_operation(&[]).is_err());
        assert!(parse_operation(&["run-shell".into()]).is_err());
    }

    #[test]
    fn rejects_wrong_argument_counts() {
        assert!(parse_operation(&["sync-packages".into()]).is_err());
        assert!(parse_operation(&["mount-overlay".into(), "1".into()]).is_err());
    }

    #[test]
    fn repository_name_validation() {
        assert!(validate_repository_name("my_repo-1").is_ok());
        assert!(validate_repository_name("").is_err());
        assert!(validate_repository_name("../etc").is_err());
        assert!(validate_repository_name("repo/name").is_err());
    }

    #[test]
    fn overlay_safety_validation() {
        assert!(validate_overlay_root_safety(Path::new("/home/user/.cache/pot")).is_ok());
        assert!(validate_overlay_root_safety(Path::new("/")).is_err());
        assert!(validate_overlay_root_safety(Path::new("/tmp")).is_err());
        assert!(validate_overlay_root_safety(Path::new("relative/path")).is_err());
    }
}
