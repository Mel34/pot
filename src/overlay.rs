use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::sudo;

pub struct Overlay {
    pub merged: PathBuf,
    mounted: bool,
}

impl Overlay {
    pub fn mount(lower: &Path, overlay_root: &Path) -> Result<Self, String> {
        let upper = overlay_root.join("upper");
        let work = overlay_root.join("work");
        let merged = overlay_root.join("merged");

        for path in [lower, &upper, &work] {
            let text = path.to_string_lossy();

            if text.contains([',', ':', '\\']) {
                return Err(format!(
                    "path '{text}' contains a character (',' ':' or '\\') that OverlayFS mount options cannot express"
                ));
            }
        }

        for directory in [&upper, &work] {
            if directory.exists() {
                if let Err(_) = std::fs::remove_dir_all(directory) {
                    let _ = sudo::run(["rm".as_ref(), "-rf".as_ref(), directory.as_os_str()]);
                }
            }
        }

        for directory in [&upper, &work, &merged] {
            std::fs::create_dir_all(directory)
                .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        }

        let options = format!(
            "lowerdir={},upperdir={},workdir={},index=off",
            lower.display(),
            upper.display(),
            work.display()
        );

        sudo::run([
            "mount".as_ref(),
            "-t".as_ref(),
            "overlay".as_ref(),
            "overlay".as_ref(),
            "-o".as_ref(),
            std::ffi::OsStr::new(&options),
            merged.as_os_str(),
        ])
        .map_err(|error| format!("failed to mount OverlayFS: {error}"))?;

        Ok(Self {
            merged,
            mounted: true,
        })
    }

    fn unmount_inner(&mut self) -> Result<(), String> {
        if !self.mounted {
            return Ok(());
        }

        unmount_path(&self.merged)?;
        self.mounted = false;

        Ok(())
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        if let Err(error) = self.unmount_inner() {
            crate::diag::log(&format!(
                "failed to unmount {}: {error}",
                self.merged.display()
            ));
        }
    }
}

/// `umount`, retrying once and finally falling back to a lazy unmount so that
/// a straggling process cannot leave a stale mount behind.
pub fn unmount_path(path: &Path) -> Result<(), String> {
    let first = sudo::run(["umount".as_ref(), path.as_os_str()]);

    if first.is_ok() {
        return Ok(());
    }

    thread::sleep(Duration::from_secs(1));

    if sudo::run(["umount".as_ref(), path.as_os_str()]).is_ok() {
        return Ok(());
    }

    sudo::run(["umount".as_ref(), "-l".as_ref(), path.as_os_str()])
        .map(|_| ())
        .map_err(|error| format!("{error} (initially: {})", first.unwrap_err()))
}

/// Whether something is mounted exactly at `path`.
pub fn is_mounted(path: &Path) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo")
        .map(|contents| mount_points(&contents).iter().any(|point| point == path))
        .unwrap_or(false)
}

fn mount_points(mountinfo: &str) -> Vec<PathBuf> {
    mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(|field| PathBuf::from(unescape_octal(field)))
        .collect()
}

/// mountinfo escapes space, tab, newline and backslash as `\040` etc.
fn unescape_octal(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4].iter().all(|b| (b'0'..=b'7').contains(b))
        {
            let value = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');

            result.push(value);
            index += 4;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8_lossy(&result).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mount_points() {
        let mountinfo = "\
36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw
40 35 0:35 / /home/me/My\\040Cache/pot/overlay/x/merged rw - overlay overlay rw,lowerdir=/a
";

        assert_eq!(
            mount_points(mountinfo),
            vec![
                PathBuf::from("/mnt2"),
                PathBuf::from("/home/me/My Cache/pot/overlay/x/merged"),
            ]
        );
    }

    #[test]
    fn unescape_handles_trailing_backslash() {
        assert_eq!(unescape_octal("a\\"), "a\\");
        assert_eq!(unescape_octal("a\\134b"), "a\\b");
    }

    #[test]
    fn rejects_paths_overlayfs_cannot_express() {
        let result = Overlay::mount(Path::new("/tmp/a,b"), Path::new("/tmp/pot-overlay-test"));

        assert!(result.is_err());
    }
}
