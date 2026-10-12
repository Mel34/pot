//! Tiny append-only diagnostics log.
//!
//! The TUI owns the terminal, so anything that goes wrong in the background
//! (a panic in a helper thread, a failed unmount) must never be printed with
//! `eprintln!`. It is written to `<cache>/pot.log` instead.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn init(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let _ = LOG_PATH.set(path);
}

pub fn log(message: &str) {
    let Some(path) = LOG_PATH.get() else {
        return;
    };

    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{seconds}] {message}");
    }
}
