//! Running external commands without ever touching the terminal.
//!
//! Every child process started through this module
//!
//! * has stdin connected to `/dev/null`, so nothing can wait on the keyboard,
//! * has stdout and stderr piped and streamed line by line, so the UI can show
//!   what is happening while it happens,
//! * lives in its own process group, so cancelling really stops `makepkg`
//!   *and* the compilers it spawned,
//! * can never hang the caller by leaving a daemon (`gpg-agent`, ...) holding
//!   the output pipes open after the command itself has exited.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const DRAIN_IDLE: Duration = Duration::from_millis(200);
const DRAIN_MAX: Duration = Duration::from_secs(2);
const TERM_GRACE: Duration = Duration::from_secs(5);
const TAIL_LINES: usize = 12;
const MAX_LINE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// A finished line of output.
    Line(String),
    /// A line that is still being rewritten in place (progress bars).
    Progress(String),
}

#[derive(Debug)]
pub enum RunError {
    /// The program could not be started at all.
    Spawn { program: String, source: io::Error },
    /// The caller asked us to stop and the process was terminated.
    Cancelled,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn { program, source } if source.kind() == io::ErrorKind::NotFound => {
                write!(f, "`{program}` was not found in PATH")
            }
            RunError::Spawn { program, source } => {
                write!(f, "could not start `{program}`: {source}")
            }
            RunError::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    /// The last few lines the command printed, for error summaries.
    pub tail: Vec<String>,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.status.success()
    }

    pub fn describe(&self) -> String {
        match (self.status.code(), self.status.signal()) {
            (Some(code), _) => format!("exit status {code}"),
            (None, Some(signal)) => format!("killed by signal {signal}"),
            _ => "unknown exit status".to_owned(),
        }
    }
}

#[derive(Debug)]
pub struct Captured {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    /// The most useful single line of an error message.
    pub fn error_line(&self) -> String {
        self.stderr
            .lines()
            .rev()
            .chain(self.stdout.lines().rev())
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| match self.code {
                Some(code) => format!("exit status {code}"),
                None => "terminated by a signal".to_owned(),
            })
    }
}

pub fn program_name(command: &Command) -> String {
    command.get_program().to_string_lossy().into_owned()
}

/// `program arg1 arg2 ...`, for showing in logs.
pub fn display_command(command: &Command) -> String {
    let mut text = program_name(command);

    for argument in command.get_args() {
        text.push(' ');
        text.push_str(&quote(argument));
    }

    text
}

fn quote(argument: &OsStr) -> String {
    let text = argument.to_string_lossy();

    if !text.is_empty() && !text.contains(|c: char| c.is_whitespace() || "'\"\\$`".contains(c)) {
        return text.into_owned();
    }

    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Run a short command to completion and capture its output.
///
/// stdin is `/dev/null`. Use [`run`] for anything long-running.
pub fn capture(command: &mut Command) -> Result<Captured, RunError> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|source| RunError::Spawn {
            program: program_name(command),
            source,
        })?;

    Ok(Captured {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run a command, streaming its combined output to `on_chunk`.
///
/// `cancel` is polled about ten times a second; when it returns `true` the
/// whole process group is terminated and [`RunError::Cancelled`] is returned.
pub fn run(
    command: &mut Command,
    cancel: &dyn Fn() -> bool,
    on_chunk: &mut dyn FnMut(Chunk),
) -> Result<Outcome, RunError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let program = program_name(command);
    let spawn_error = |source: std::io::Error| RunError::Spawn {
        program: program.clone(),
        source,
    };
    let mut child = command.spawn().map_err(&spawn_error)?;
    let pid = child.id();

    let (tx, rx) = mpsc::channel::<Chunk>();

    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, tx.clone());
    }

    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, tx.clone());
    }

    drop(tx);

    let mut tail: VecDeque<String> = VecDeque::new();

    let mut emit = |chunk: Chunk| {
        if let Chunk::Line(line) = &chunk {
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }

            tail.push_back(line.clone());
        }

        on_chunk(chunk);
    };

    let mut pipes_open = true;

    loop {
        if pipes_open {
            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(chunk) => emit(chunk),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => pipes_open = false,
            }
        } else {
            thread::sleep(POLL_INTERVAL);
        }

        if cancel() {
            terminate(&mut child, pid);
            drain(&rx, &mut emit);
            return Err(RunError::Cancelled);
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                drain(&rx, &mut emit);

                return Ok(Outcome {
                    status,
                    tail: tail.into_iter().collect(),
                });
            }
            Ok(None) => {}
            Err(error) => return Err(spawn_error(error)),
        }
    }
}

/// Collect whatever is still buffered after the child has exited, but never
/// wait for pipes that some leftover grandchild keeps open.
fn drain(rx: &mpsc::Receiver<Chunk>, emit: &mut dyn FnMut(Chunk)) {
    let deadline = Instant::now() + DRAIN_MAX;

    while Instant::now() < deadline {
        match rx.recv_timeout(DRAIN_IDLE) {
            Ok(chunk) => emit(chunk),
            Err(_) => break,
        }
    }
}

fn terminate(child: &mut Child, pid: u32) {
    let group = Pid::from_raw(pid as i32);

    let _ = killpg(group, Signal::SIGTERM);

    let deadline = Instant::now() + TERM_GRACE;

    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }

        thread::sleep(Duration::from_millis(50));
    }

    let _ = killpg(group, Signal::SIGKILL);
    let _ = child.wait();
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R, tx: mpsc::Sender<Chunk>) {
    let spawned = thread::Builder::new()
        .name("pot-reader".to_owned())
        .spawn(move || {
            let mut splitter = LineSplitter::default();
            let mut buffer = [0u8; 8192];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut receiver_gone = false;

                        splitter.feed(&buffer[..count], &mut |chunk| {
                            if tx.send(chunk).is_err() {
                                receiver_gone = true;
                            }
                        });

                        if receiver_gone {
                            return;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }

            splitter.finish(&mut |chunk| {
                let _ = tx.send(chunk);
            });
        });

    if let Err(error) = spawned {
        crate::diag::log(&format!("could not start output reader thread: {error}"));
    }
}

/// Turns a byte stream into lines, understanding `\r` progress updates.
#[derive(Default)]
pub struct LineSplitter {
    buffer: Vec<u8>,
    last_progress: Option<String>,
    after_carriage_return: bool,
}

impl LineSplitter {
    pub fn feed(&mut self, bytes: &[u8], out: &mut dyn FnMut(Chunk)) {
        for &byte in bytes {
            match byte {
                b'\n' => {
                    let line = if self.buffer.is_empty() && self.after_carriage_return {
                        self.last_progress.take().unwrap_or_default()
                    } else {
                        sanitize(&String::from_utf8_lossy(&self.buffer))
                    };

                    self.buffer.clear();
                    self.last_progress = None;
                    self.after_carriage_return = false;

                    out(Chunk::Line(line));
                }

                b'\r' => {
                    if !self.buffer.is_empty() {
                        let text = sanitize(&String::from_utf8_lossy(&self.buffer));

                        self.buffer.clear();
                        self.last_progress = Some(text.clone());

                        out(Chunk::Progress(text));
                    }

                    self.after_carriage_return = true;
                }

                _ => {
                    self.after_carriage_return = false;
                    self.buffer.push(byte);

                    if self.buffer.len() >= MAX_LINE_BYTES {
                        let line = sanitize(&String::from_utf8_lossy(&self.buffer));
                        self.buffer.clear();
                        out(Chunk::Line(line));
                    }
                }
            }
        }
    }

    pub fn finish(&mut self, out: &mut dyn FnMut(Chunk)) {
        if !self.buffer.is_empty() {
            let line = sanitize(&String::from_utf8_lossy(&self.buffer));
            self.buffer.clear();
            out(Chunk::Line(line));
        } else if self.after_carriage_return {
            if let Some(text) = self.last_progress.take() {
                out(Chunk::Line(text));
            }
        }
    }
}

/// Make arbitrary program output safe to put into a terminal buffer: ANSI
/// escape sequences are removed, tabs become spaces and every other control
/// character is dropped.
pub fn sanitize(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '\u{1b}' => match chars.peek().copied() {
                // CSI: ESC [ ... final byte in @..~
                Some('[') => {
                    chars.next();

                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... BEL or ESC \
                Some(']') => {
                    chars.next();

                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }

                        if next == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                // Any other two-byte escape.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\t' => result.push_str("    "),
            control if control.is_control() => {}
            other => result.push(other),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn split(input: &[&[u8]]) -> Vec<Chunk> {
        let mut splitter = LineSplitter::default();
        let mut chunks = Vec::new();

        for part in input {
            splitter.feed(part, &mut |chunk| chunks.push(chunk));
        }

        splitter.finish(&mut |chunk| chunks.push(chunk));

        chunks
    }

    fn line(text: &str) -> Chunk {
        Chunk::Line(text.to_owned())
    }

    fn progress(text: &str) -> Chunk {
        Chunk::Progress(text.to_owned())
    }

    #[test]
    fn splits_plain_lines_across_reads() {
        assert_eq!(
            split(&[b"one\ntw", b"o\n\nthree"]),
            vec![line("one"), line("two"), line(""), line("three")]
        );
    }

    #[test]
    fn carriage_returns_become_progress_and_the_last_state_is_kept() {
        assert_eq!(
            split(&[b"10%\r50%\r100% done\n"]),
            vec![progress("10%"), progress("50%"), line("100% done")]
        );
    }

    #[test]
    fn crlf_is_a_single_line_break() {
        assert_eq!(split(&[b"abc\r\ndef\r\n"]), vec![progress("abc"), line("abc"), progress("def"), line("def")]);
    }

    #[test]
    fn sanitize_strips_escapes_and_controls() {
        assert_eq!(sanitize("\u{1b}[1;31mred\u{1b}[0m"), "red");
        assert_eq!(sanitize("\u{1b}]0;title\u{7}text"), "text");
        assert_eq!(sanitize("a\tb"), "a    b");
        assert_eq!(sanitize("bell\u{7}!"), "bell!");
        assert_eq!(sanitize("ünïcode ✓"), "ünïcode ✓");
    }

    #[test]
    fn display_command_quotes_awkward_arguments() {
        let mut command = Command::new("git");
        command.args(["apply", "-p1", "my patch.diff"]);

        assert_eq!(display_command(&command), "git apply -p1 'my patch.diff'");
    }

    #[test]
    fn streams_output_and_reports_exit_status() {
        let mut lines = Vec::new();

        let outcome = run(
            Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            &|| false,
            &mut |chunk| lines.push(chunk),
        )
        .unwrap();

        assert!(!outcome.success());
        assert_eq!(outcome.describe(), "exit status 3");
        assert!(lines.contains(&line("out")));
        assert!(lines.contains(&line("err")));
    }

    #[test]
    fn missing_program_is_a_clear_error() {
        let error = run(
            &mut Command::new("pot-definitely-not-a-program"),
            &|| false,
            &mut |_| {},
        )
        .unwrap_err();

        assert!(error.to_string().contains("was not found in PATH"), "{error}");
    }

    #[test]
    fn stdin_is_closed() {
        let mut lines = Vec::new();

        // `cat` would wait forever if stdin were the terminal.
        let outcome = run(&mut Command::new("cat"), &|| false, &mut |chunk| lines.push(chunk)).unwrap();

        assert!(outcome.success());
        assert!(lines.is_empty());
    }

    #[test]
    fn cancel_kills_the_whole_process_group() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();

        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(400));
            flag.store(true, Ordering::Relaxed);
        });

        let started = Instant::now();

        // The shell forks a long-running grandchild, like makepkg forks gcc.
        let result = run(
            Command::new("sh").args(["-c", "sleep 30 & sleep 30; wait"]),
            &|| cancel.load(Ordering::Relaxed),
            &mut |_| {},
        );

        canceller.join().unwrap();

        assert!(matches!(result, Err(RunError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn leftover_background_process_does_not_hang_the_caller() {
        let started = Instant::now();

        // A grandchild that keeps stdout open, like gpg-agent started by makepkg.
        let outcome = run(
            Command::new("sh").args(["-c", "(sleep 6 &) ; echo done"]),
            &|| false,
            &mut |_| {},
        )
        .unwrap();

        assert!(outcome.success());
        assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
    }

    #[test]
    fn capture_returns_stdout_and_stderr() {
        let captured = capture(Command::new("sh").args(["-c", "echo a; echo b >&2; exit 2"])).unwrap();

        assert!(!captured.success);
        assert_eq!(captured.code, Some(2));
        assert_eq!(captured.stdout.trim(), "a");
        assert_eq!(captured.error_line(), "b");
    }
}
