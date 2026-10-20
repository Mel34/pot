use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::proc;

pub fn helper_path() -> PathBuf {
    let lib_path = Path::new("/usr/lib/pot/pot-helper");
    if lib_path.exists() {
        return lib_path.to_path_buf();
    }

    let bin_path = Path::new("/usr/bin/pot-helper");
    if bin_path.exists() {
        return bin_path.to_path_buf();
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("pot-helper");
            if sibling.exists() {
                return sibling;
            }
        }
    }

    PathBuf::from("pot-helper")
}

pub fn command<I, S>(operation: &str, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("pkexec");
    cmd.arg(helper_path());
    cmd.arg(operation);
    for arg in args {
        cmd.arg(arg);
    }
    cmd
}

pub fn run<I, S>(operation: &str, args: I) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = command(operation, args);
    let captured = proc::capture(&mut cmd).map_err(|error| error.to_string())?;

    if captured.success {
        return Ok(());
    }

    let detail = captured.error_line();
    if detail.is_empty() {
        Err(format!("pot-helper {operation} failed"))
    } else {
        Err(format!("pot-helper {operation}: {detail}"))
    }
}

pub fn mount_overlay(lower: &Path, overlay_root: &Path) -> Result<(), String> {
    run(
        "mount-overlay",
        [lower.as_os_str(), overlay_root.as_os_str()],
    )
}

pub fn unmount_overlay(merged: &Path) -> Result<(), String> {
    run("unmount-overlay", [merged.as_os_str()])
}

#[allow(dead_code)]
pub fn cleanup_overlay(overlay_root: &Path) -> Result<(), String> {
    run("cleanup-overlay", [overlay_root.as_os_str()])
}

pub fn setup_repository(repository: &str, uid: u32) -> Result<(), String> {
    let uid_str = uid.to_string();
    run(
        "setup-repository",
        [OsStr::new(repository), OsStr::new(&uid_str)],
    )
}

pub fn sync_packages(repository: &str) -> Result<(), String> {
    run("sync-packages", [OsStr::new(repository)])
}

pub fn sync_packages_command(repository: &str) -> Command {
    command("sync-packages", [repository])
}

pub fn install_package_command(package: &str) -> Command {
    command("install-package", [package])
}
