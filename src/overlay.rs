use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Overlay {
    pub merged: PathBuf,
    upper: PathBuf,
    work: PathBuf,
}

impl Overlay {
    pub fn mount(
        lower: &Path,
        overlay_root: &Path,
    ) -> io::Result<Self> {
        let upper = overlay_root.join("upper");
        let work = overlay_root.join("work");
        let merged = overlay_root.join("merged");

        std::fs::create_dir_all(&upper)?;
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&merged)?;

        let options = format!(
            "lowerdir={},upperdir={},workdir={}",
            lower.display(),
            upper.display(),
            work.display()
        );

        let status = Command::new("sudo")
            .args([
                "mount",
                "-t",
                "overlay",
                "overlay",
                "-o",
                &options,
            ])
            .arg(&merged)
            .status()?;

        if !status.success() {
            return Err(io::Error::other("failed to mount OverlayFS"));
        }

        Ok(Self {
            merged,
            upper,
            work,
        })
    }

    pub fn unmount(&self) -> io::Result<()> {
        let status = Command::new("sudo")
            .arg("umount")
            .arg(&self.merged)
            .status()?;

        if !status.success() {
            return Err(io::Error::other("failed to unmount OverlayFS"));
        }

        Ok(())
    }
}
