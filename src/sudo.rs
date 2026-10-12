#![allow(dead_code)]
//! `sudo` without ever prompting inside the TUI.
//!
//! A password prompt that appears while the interface owns the terminal is
//! invisible and swallows keystrokes, which looks exactly like a freeze. So:
//!
//! * credentials are requested *outside* the TUI ([`authenticate`]) before an
//!   operation that needs them starts,
//! * a keep-alive thread refreshes the sudo timestamp while pot is running, so
//!   a 30 minute build does not outlive the five minute default timeout,
//! * every `sudo` that pot runs itself uses `-n`, so it fails with a readable
//!   message instead of waiting.

use std::ffi::OsStr;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::proc::{self, Captured};

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(45);

/// `sudo -n <args...>` (never prompts).
pub fn command<I, S>(args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("sudo");
    command.arg("-n").args(args);
    command
}

/// Whether sudo will run commands right now without asking for a password.
pub fn credentials_valid() -> bool {
    proc::capture(Command::new("sudo").args(["-n", "-v"]))
        .map(|result| result.success)
        .unwrap_or(false)
}

/// Ask for the password interactively. Must only be called while the terminal
/// is in normal (not raw / alternate screen) mode.
pub fn authenticate() -> Result<(), String> {
    let status = Command::new("sudo")
        .arg("-v")
        .status()
        .map_err(|error| format!("could not run sudo: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("sudo authentication failed".to_owned())
    }
}

/// Run `sudo -n <args...>` to completion, capturing output.
pub fn run<I, S>(args: I) -> Result<Captured, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();

    let result = proc::capture(&mut command(&args)).map_err(|error| error.to_string())?;

    if result.success {
        return Ok(result);
    }

    Err(explain(&args, &result))
}

fn explain<S: AsRef<OsStr>>(args: &[S], result: &Captured) -> String {
    let program = args
        .first()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .unwrap_or_default();

    let detail = result.error_line();

    if detail.contains("password is required") || detail.contains("a terminal is required") {
        return format!(
            "sudo {program}: administrator credentials have expired \
             (run `sudo -v` in another terminal, or restart the action to be asked again)"
        );
    }

    format!("sudo {program} failed: {detail}")
}

/// Keep the sudo timestamp fresh. `on_expired` is called once each time the
/// credentials transition from valid to invalid.
pub fn spawn_keepalive(on_expired: impl Fn() + Send + 'static) {
    let spawned = thread::Builder::new().name("pot-sudo".to_owned()).spawn(move || {
        let mut had_credentials = credentials_valid();

        loop {
            thread::sleep(KEEPALIVE_INTERVAL);

            let valid = credentials_valid();

            if had_credentials && !valid {
                on_expired();
            }

            had_credentials = valid;
        }
    });

    if let Err(error) = spawned {
        crate::diag::log(&format!("could not start sudo keep-alive thread: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_expired_credentials() {
        let result = Captured {
            success: false,
            code: Some(1),
            stdout: String::new(),
            stderr: "sudo: a password is required\n".to_owned(),
        };

        let message = explain(&["umount", "/x"], &result);

        assert!(message.contains("expired"), "{message}");
        assert!(message.starts_with("sudo umount"), "{message}");
    }

    #[test]
    fn other_errors_keep_their_detail() {
        let result = Captured {
            success: false,
            code: Some(32),
            stdout: String::new(),
            stderr: "mount: /x: wrong fs type\n".to_owned(),
        };

        assert_eq!(
            explain(&["mount"], &result),
            "sudo mount failed: mount: /x: wrong fs type"
        );
    }
}
