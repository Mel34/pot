//! Background work: preparing sources, verifying patches and cooking.
//!
//! The UI never blocks on any of this. It sends [`WorkerCommand`]s and reads
//! [`WorkerEvent`]s. Three lane threads (prepare, verify, cook) do the work; a
//! dispatcher thread owns a [`Scheduler`], a plain state machine that decides
//! what may run when. Every job runs under `catch_unwind`, so a bug or a
//! missing tool becomes an error message in the UI instead of a row that is
//! stuck on "preparing" forever.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::cookbook::PatchStatus;
use crate::overlay::Overlay;
use crate::prepare::{self, GitSource, PrepState};
use crate::proc::{self, Chunk, RunError};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::Cancelled => write!(f, "cancelled"),
            JobError::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl From<String> for JobError {
    fn from(message: String) -> Self {
        JobError::Failed(message)
    }
}

impl From<&str> for JobError {
    fn from(message: &str) -> Self {
        JobError::Failed(message.to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Prepare,
    Reset,
    Cook,
}

/// Everything the background jobs need to know about the installation.
#[derive(Clone)]
pub struct Env {
    pub cache_dir: PathBuf,
    pub repository_name: String,
    sources: Arc<Mutex<HashMap<PathBuf, Vec<GitSource>>>>,
}

impl Env {
    pub fn new(cache_dir: PathBuf, repository_name: String) -> Self {
        Self {
            cache_dir,
            repository_name,
            sources: Arc::default(),
        }
    }

    pub fn work_root(&self) -> PathBuf {
        self.cache_dir.join("work")
    }

    pub fn work_dir(&self, recipe: &str) -> PathBuf {
        self.work_root().join(recipe)
    }

    pub fn overlay_root(&self, recipe: &str) -> PathBuf {
        self.cache_dir.join("overlay").join(recipe)
    }

    pub fn log_path(&self, recipe: &str, kind: &str) -> PathBuf {
        self.cache_dir.join("logs").join(format!("{recipe}.{kind}.log"))
    }

    /// The `git+` sources of a prepared tree (cached: `makepkg --printsrcinfo`
    /// is not free and verifying N patches would run it N times).
    pub fn git_sources(&self, work_dir: &Path) -> Result<Vec<GitSource>, String> {
        if let Some(found) = self.cache().get(work_dir) {
            return Ok(found.clone());
        }

        let sources = prepare::load_git_sources(work_dir)?;

        self.cache().insert(work_dir.to_path_buf(), sources.clone());

        Ok(sources)
    }

    pub fn forget_sources(&self, work_dir: &Path) {
        self.cache().remove(work_dir);
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Vec<GitSource>>> {
        self.sources.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug, Clone)]
pub struct VerifyReq {
    pub recipe_index: usize,
    pub recipe_name: String,
    pub patch_index: usize,
    pub patch_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PrepareReq {
    pub recipe_index: usize,
    pub recipe_name: String,
    pub reset: bool,
}

#[derive(Debug, Clone)]
pub struct CookReq {
    pub recipe_index: usize,
    pub recipe_name: String,
    pub patches: Vec<PathBuf>,
}

pub enum WorkerCommand {
    /// Prepare (or, with `reset`, re-prepare) a recipe and then verify the
    /// given patches against the result.
    Prepare {
        request: PrepareReq,
        verify: Vec<VerifyReq>,
    },
    Verify(VerifyReq),
    Cook(CookReq),
    /// Cancel whatever is running or queued for this recipe.
    Abort {
        recipe_index: usize,
    },
    Shutdown,
}

pub enum WorkerEvent {
    /// Work on a recipe has actually begun (as opposed to merely queued).
    Started { recipe_index: usize, op: Op },
    /// A new high-level step ("cloning PKGBUILD", ...).
    Phase { recipe_index: usize, text: String },
    Output { recipe_index: usize, chunk: Chunk },
    Prepared {
        recipe_index: usize,
        result: Result<(), JobError>,
    },
    Verified {
        recipe_index: usize,
        patch_index: usize,
        status: PatchStatus,
        #[allow(dead_code)]
        detail: Option<String>,
    },
    Cooked {
        recipe_index: usize,
        result: Result<(), JobError>,
        #[allow(dead_code)]
        overlay: Option<Overlay>,
    },
    /// Something the user should know that is not tied to a recipe.
    #[allow(dead_code)]
    Notice(String),
    Stopped,
}

// ---------------------------------------------------------------------------
// Reporter: how a job tells the UI (and the log file) what it is doing
// ---------------------------------------------------------------------------

pub struct Reporter {
    events: Sender<WorkerEvent>,
    recipe_index: usize,
    file: Mutex<Option<File>>,
}

impl Reporter {
    pub fn new(events: Sender<WorkerEvent>, recipe_index: usize, log_path: &Path, title: &str) -> Self {
        let file = log_path
            .parent()
            .and_then(|parent| fs::create_dir_all(parent).ok())
            .and_then(|_| File::create(log_path).ok());

        let reporter = Self {
            events,
            recipe_index,
            file: Mutex::new(file),
        };

        reporter.note(title);

        reporter
    }

    pub fn phase(&self, text: &str) {
        self.send(WorkerEvent::Phase {
            recipe_index: self.recipe_index,
            text: text.to_owned(),
        });

        self.note(text);
    }

    /// A line written by pot itself (as opposed to a child process).
    pub fn note(&self, text: &str) {
        self.line(&format!("[pot] {text}"));
    }

    pub fn line(&self, text: &str) {
        self.chunk(Chunk::Line(text.to_owned()));
    }

    pub fn chunk(&self, chunk: Chunk) {
        if let Chunk::Line(line) = &chunk {
            if let Some(file) = self.file.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
                let _ = writeln!(file, "{line}");
            }
        }

        self.send(WorkerEvent::Output {
            recipe_index: self.recipe_index,
            chunk,
        });
    }

    /// Run a long-running command, streaming its output.
    ///
    /// `label` names the command in error messages ("git fetch").
    pub fn run(
        &self,
        label: &str,
        command: &mut Command,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), JobError> {
        self.note(&format!("$ {}", proc::display_command(command)));

        let outcome = proc::run(command, cancel, &mut |chunk| self.chunk(chunk));

        match outcome {
            Ok(outcome) if outcome.success() => Ok(()),

            Ok(outcome) => {
                let detail = outcome
                    .tail
                    .iter()
                    .rev()
                    .map(|line| line.trim())
                    .find(|line| !line.is_empty())
                    .map(|line| format!(": {line}"))
                    .unwrap_or_default();

                Err(JobError::Failed(format!(
                    "{label} failed ({}){detail}",
                    outcome.describe()
                )))
            }

            Err(RunError::Cancelled) => Err(JobError::Cancelled),

            Err(error) => Err(JobError::Failed(error.to_string())),
        }
    }

    /// Record how the job ended, so the log is self-explanatory.
    pub fn finish(&self, result: &Result<(), JobError>) {
        match result {
            Ok(()) => self.note("done"),
            Err(JobError::Cancelled) => self.note("cancelled"),
            Err(JobError::Failed(message)) => self.note(&format!("ERROR: {message}")),
        }
    }

    fn send(&self, event: WorkerEvent) {
        let _ = self.events.send(event);
    }
}

// ---------------------------------------------------------------------------
// Scheduler: pure bookkeeping, no threads, no I/O
// ---------------------------------------------------------------------------

pub enum Action {
    Prepare(PrepareReq),
    Verify(VerifyReq),
    Cook(CookReq),
    AbortCook,
    AbortPrepare,
    Emit(WorkerEvent),
}

/// Decides what runs when. One source preparation at a time, one cook at a
/// time, verifications run once their recipe has been prepared.
///
/// Nothing is ever silently dropped: every request either runs, waits, or is
/// answered with an event explaining why it cannot.
#[derive(Default)]
pub struct Scheduler {
    active_prepare: Option<usize>,
    queue: VecDeque<PrepareReq>,
    waiting_verifies: Vec<VerifyReq>,
    active_cook: Option<usize>,
}

impl Scheduler {
    fn preparing(&self, recipe_index: usize) -> bool {
        self.active_prepare == Some(recipe_index)
            || self.queue.iter().any(|queued| queued.recipe_index == recipe_index)
    }

    fn wait_for_prepare(&mut self, request: VerifyReq) {
        let duplicate = self.waiting_verifies.iter().any(|waiting| {
            waiting.recipe_index == request.recipe_index && waiting.patch_index == request.patch_index
        });

        if !duplicate {
            self.waiting_verifies.push(request);
        }
    }

    pub fn command(
        &mut self,
        command: WorkerCommand,
        prepared: &dyn Fn(&str) -> bool,
    ) -> Vec<Action> {
        match command {
            WorkerCommand::Prepare { request, verify } => self.prepare(request, verify),

            WorkerCommand::Verify(request) => {
                if self.preparing(request.recipe_index) {
                    self.wait_for_prepare(request);
                    Vec::new()
                } else if prepared(&request.recipe_name) {
                    vec![Action::Verify(request)]
                } else {
                    // Nothing to verify against yet: prepare first.
                    let prepare = PrepareReq {
                        recipe_index: request.recipe_index,
                        recipe_name: request.recipe_name.clone(),
                        reset: false,
                    };

                    self.prepare(prepare, vec![request])
                }
            }

            WorkerCommand::Cook(request) => {
                if let Some(running) = self.active_cook {
                    let message = if running == request.recipe_index {
                        "this recipe is already being cooked"
                    } else {
                        "another recipe is being cooked; wait for it or abort it first"
                    };

                    vec![Action::Emit(WorkerEvent::Cooked {
                        recipe_index: request.recipe_index,
                        result: Err(message.into()),
                        overlay: None,
                    })]
                } else if self.preparing(request.recipe_index) {
                    vec![Action::Emit(WorkerEvent::Cooked {
                        recipe_index: request.recipe_index,
                        result: Err("sources are still being prepared".into()),
                        overlay: None,
                    })]
                } else {
                    self.active_cook = Some(request.recipe_index);
                    vec![Action::Cook(request)]
                }
            }

            WorkerCommand::Abort { recipe_index } => {
                let mut actions = Vec::new();

                if self.active_cook == Some(recipe_index) {
                    actions.push(Action::AbortCook);
                }

                if self.active_prepare == Some(recipe_index) {
                    actions.push(Action::AbortPrepare);
                }

                if let Some(position) = self
                    .queue
                    .iter()
                    .position(|queued| queued.recipe_index == recipe_index)
                {
                    self.queue.remove(position);

                    actions.push(Action::Emit(WorkerEvent::Prepared {
                        recipe_index,
                        result: Err(JobError::Cancelled),
                    }));

                    actions.extend(self.release_verifies(recipe_index, "cancelled"));
                }

                actions
            }

            // Handled by the dispatcher loop itself.
            WorkerCommand::Shutdown => Vec::new(),
        }
    }

    fn prepare(&mut self, request: PrepareReq, verify: Vec<VerifyReq>) -> Vec<Action> {
        let recipe_index = request.recipe_index;

        if self.active_cook == Some(recipe_index) {
            return vec![Action::Emit(WorkerEvent::Prepared {
                recipe_index,
                result: Err("this recipe is being cooked".into()),
            })];
        }

        for waiting in verify {
            self.wait_for_prepare(waiting);
        }

        if self.preparing(recipe_index) {
            return Vec::new();
        }

        if self.active_prepare.is_none() {
            self.active_prepare = Some(recipe_index);
            vec![Action::Prepare(request)]
        } else {
            self.queue.push_back(request);
            Vec::new()
        }
    }

    /// Verifications that were waiting for `recipe_index` are answered with
    /// "unknown" rather than left spinning.
    fn release_verifies(&mut self, recipe_index: usize, reason: &str) -> Vec<Action> {
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting_verifies)
            .into_iter()
            .partition(|waiting| waiting.recipe_index == recipe_index);

        self.waiting_verifies = others;

        mine.into_iter()
            .map(|waiting| {
                Action::Emit(WorkerEvent::Verified {
                    recipe_index,
                    patch_index: waiting.patch_index,
                    status: PatchStatus::Unknown,
                    detail: Some(format!("not checked: {reason}")),
                })
            })
            .collect()
    }

    pub fn prepare_finished(&mut self, recipe_index: usize, success: bool) -> Vec<Action> {
        self.active_prepare = None;

        let mut actions = Vec::new();

        if success {
            let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting_verifies)
                .into_iter()
                .partition(|waiting| waiting.recipe_index == recipe_index);

            self.waiting_verifies = others;
            actions.extend(mine.into_iter().map(Action::Verify));
        } else {
            actions.extend(self.release_verifies(recipe_index, "source preparation did not finish"));
        }

        // A failure must not stall everything queued behind it.
        if let Some(next) = self.queue.pop_front() {
            self.active_prepare = Some(next.recipe_index);
            actions.push(Action::Prepare(next));
        }

        actions
    }

    pub fn cook_finished(&mut self) {
        self.active_cook = None;
    }
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

enum Msg {
    Command(WorkerCommand),
    PrepareDone { recipe_index: usize, success: bool },
    CookDone,
}

#[derive(Clone, Default)]
struct Flags {
    shutdown: Arc<AtomicBool>,
    prepare_abort: Arc<AtomicBool>,
    cook_abort: Arc<AtomicBool>,
}

pub struct Worker {
    msgs: Sender<Msg>,
    pub events: Receiver<WorkerEvent>,
    #[allow(dead_code)]
    events_tx: Sender<WorkerEvent>,
}

impl Worker {
    pub fn send(&self, command: WorkerCommand) -> io::Result<()> {
        self.msgs
            .send(Msg::Command(command))
            .map_err(|_| io::Error::other("the background worker has stopped"))
    }


}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

fn spawn_named<F>(name: &str, body: F) -> io::Result<JoinHandle<()>>
where
    F: FnOnce() + Send + 'static,
{
    thread::Builder::new().name(name.to_owned()).spawn(body)
}

pub fn start(env: Env) -> io::Result<Worker> {
    let (msgs_tx, msgs_rx) = mpsc::channel::<Msg>();
    let (events_tx, events_rx) = mpsc::channel::<WorkerEvent>();

    let (prepare_tx, prepare_rx) = mpsc::channel::<PrepareReq>();
    let (verify_tx, verify_rx) = mpsc::channel::<VerifyReq>();
    let (cook_tx, cook_rx) = mpsc::channel::<CookReq>();

    let flags = Flags::default();

    let prepare_lane = {
        let (env, events, msgs, flags) = (env.clone(), events_tx.clone(), msgs_tx.clone(), flags.clone());

        spawn_named("pot-prepare", move || {
            for request in prepare_rx {
                run_prepare(&env, &request, &events, &msgs, &flags);
            }
        })?
    };

    let verify_lane = {
        let (env, events) = (env.clone(), events_tx.clone());

        spawn_named("pot-verify", move || {
            for request in verify_rx {
                run_verify(&env, &request, &events);
            }
        })?
    };

    let cook_lane = {
        let (env, events, msgs, flags) = (env.clone(), events_tx.clone(), msgs_tx.clone(), flags.clone());

        spawn_named("pot-cook", move || {
            for request in cook_rx {
                run_cook(&env, &request, &events, &msgs, &flags);
            }
        })?
    };

    {
        let events = events_tx.clone();

        spawn_named("pot-dispatch", move || {
            let mut scheduler = Scheduler::default();

            let is_prepared =
                |recipe: &str| prepare::state(&env.cache_dir, recipe) == PrepState::Ready;

            while let Ok(msg) = msgs_rx.recv() {
                let actions = match msg {
                    Msg::Command(WorkerCommand::Shutdown) => break,
                    Msg::Command(command) => scheduler.command(command, &is_prepared),
                    Msg::PrepareDone { recipe_index, success } => {
                        scheduler.prepare_finished(recipe_index, success)
                    }
                    Msg::CookDone => {
                        scheduler.cook_finished();
                        Vec::new()
                    }
                };

                for action in actions {
                    match action {
                        Action::Prepare(request) => {
                            flags.prepare_abort.store(false, Ordering::Relaxed);
                            let _ = prepare_tx.send(request);
                        }
                        Action::Verify(request) => {
                            let _ = verify_tx.send(request);
                        }
                        Action::Cook(request) => {
                            flags.cook_abort.store(false, Ordering::Relaxed);
                            let _ = cook_tx.send(request);
                        }
                        Action::AbortCook => flags.cook_abort.store(true, Ordering::Relaxed),
                        Action::AbortPrepare => flags.prepare_abort.store(true, Ordering::Relaxed),
                        Action::Emit(event) => {
                            let _ = events.send(event);
                        }
                    }
                }
            }

            // Stop everything: running jobs notice the flag, kill their
            // process groups and return; the lanes end when their queues close.
            flags.shutdown.store(true, Ordering::Relaxed);

            drop(prepare_tx);
            drop(verify_tx);
            drop(cook_tx);

            for lane in [prepare_lane, verify_lane, cook_lane] {
                let _ = lane.join();
            }

            let _ = events.send(WorkerEvent::Stopped);
        })?;
    }

    Ok(Worker {
        msgs: msgs_tx,
        events: events_rx,
        events_tx,
    })
}

fn run_prepare(
    env: &Env,
    request: &PrepareReq,
    events: &Sender<WorkerEvent>,
    msgs: &Sender<Msg>,
    flags: &Flags,
) {
    let op = if request.reset { Op::Reset } else { Op::Prepare };

    let _ = events.send(WorkerEvent::Started {
        recipe_index: request.recipe_index,
        op,
    });

    let title = match op {
        Op::Reset => format!("Resetting {}", request.recipe_name),
        _ => format!("Preparing {}", request.recipe_name),
    };

    let reporter = Reporter::new(
        events.clone(),
        request.recipe_index,
        &env.log_path(&request.recipe_name, "prepare"),
        &title,
    );

    let cancel = || flags.shutdown.load(Ordering::Relaxed) || flags.prepare_abort.load(Ordering::Relaxed);

    let result = catch_unwind(AssertUnwindSafe(|| {
        prepare::prepare_sources(env, &request.recipe_name, request.reset, &reporter, &cancel)
    }))
    .unwrap_or_else(|payload| Err(JobError::Failed(format!("internal error: {}", panic_message(&payload)))));

    reporter.finish(&result);

    let success = result.is_ok();

    let _ = events.send(WorkerEvent::Prepared {
        recipe_index: request.recipe_index,
        result,
    });

    let _ = msgs.send(Msg::PrepareDone {
        recipe_index: request.recipe_index,
        success,
    });
}

fn run_verify(env: &Env, request: &VerifyReq, events: &Sender<WorkerEvent>) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let work_dir = env.work_dir(&request.recipe_name);

        match env.git_sources(&work_dir) {
            Err(error) => (PatchStatus::Failed, Some(error)),

            Ok(sources) => match crate::patch::verify(&work_dir, &sources, &request.patch_path) {
                crate::patch::Verification::Applicable(..) => (PatchStatus::Clean, None),
                crate::patch::Verification::Applied(..) => {
                    (PatchStatus::Clean, Some("already applied to the sources".to_owned()))
                }
                crate::patch::Verification::Conflict(error) => (PatchStatus::Failed, Some(error)),
            },
        }
    }))
    .unwrap_or_else(|payload| {
        (
            PatchStatus::Failed,
            Some(format!("internal error: {}", panic_message(&payload))),
        )
    });

    let _ = events.send(WorkerEvent::Verified {
        recipe_index: request.recipe_index,
        patch_index: request.patch_index,
        status: outcome.0,
        detail: outcome.1,
    });
}

fn run_cook(
    env: &Env,
    request: &CookReq,
    events: &Sender<WorkerEvent>,
    msgs: &Sender<Msg>,
    flags: &Flags,
) {
    let _ = events.send(WorkerEvent::Started {
        recipe_index: request.recipe_index,
        op: Op::Cook,
    });

    let reporter = Reporter::new(
        events.clone(),
        request.recipe_index,
        &env.log_path(&request.recipe_name, "cook"),
        &format!("Cooking {}", request.recipe_name),
    );

    let cancel = || flags.shutdown.load(Ordering::Relaxed) || flags.cook_abort.load(Ordering::Relaxed);

    // Outside the catch_unwind so a panic cannot lose a mounted overlay.
    let mut overlay = None;

    let result = catch_unwind(AssertUnwindSafe(|| {
        crate::kitchen::cook(env, request, &reporter, &cancel, &mut overlay)
    }))
    .unwrap_or_else(|payload| Err(JobError::Failed(format!("internal error: {}", panic_message(&payload)))));

    reporter.finish(&result);

    let _ = events.send(WorkerEvent::Cooked {
        recipe_index: request.recipe_index,
        result,
        overlay,
    });

    let _ = msgs.send(Msg::CookDone);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare_cmd(index: usize) -> WorkerCommand {
        WorkerCommand::Prepare {
            request: PrepareReq {
                recipe_index: index,
                recipe_name: format!("r{index}"),
                reset: false,
            },
            verify: Vec::new(),
        }
    }

    fn verify_req(index: usize, patch: usize) -> VerifyReq {
        VerifyReq {
            recipe_index: index,
            recipe_name: format!("r{index}"),
            patch_index: patch,
            patch_path: PathBuf::from(format!("p{patch}.patch")),
        }
    }

    fn cook_req(index: usize) -> CookReq {
        CookReq {
            recipe_index: index,
            recipe_name: format!("r{index}"),
            patches: Vec::new(),
        }
    }

    fn describe(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .map(|action| match action {
                Action::Prepare(request) => format!("prepare {}", request.recipe_index),
                Action::Verify(request) => format!("verify {}/{}", request.recipe_index, request.patch_index),
                Action::Cook(request) => format!("cook {}", request.recipe_index),
                Action::AbortCook => "abort-cook".to_owned(),
                Action::AbortPrepare => "abort-prepare".to_owned(),
                Action::Emit(WorkerEvent::Prepared { recipe_index, result }) => {
                    format!("prepared {recipe_index} ok={}", result.is_ok())
                }
                Action::Emit(WorkerEvent::Cooked { recipe_index, result, .. }) => {
                    format!("cooked {recipe_index} ok={}", result.is_ok())
                }
                Action::Emit(WorkerEvent::Verified { recipe_index, patch_index, status, .. }) => {
                    format!("verified {recipe_index}/{patch_index} {status:?}")
                }
                Action::Emit(_) => "event".to_owned(),
            })
            .collect()
    }

    const NOT_PREPARED: &dyn Fn(&str) -> bool = &|_| false;
    const PREPARED: &dyn Fn(&str) -> bool = &|_| true;

    #[test]
    fn second_prepare_waits_and_then_runs() {
        let mut scheduler = Scheduler::default();

        assert_eq!(describe(&scheduler.command(prepare_cmd(0), NOT_PREPARED)), ["prepare 0"]);
        assert!(scheduler.command(prepare_cmd(1), NOT_PREPARED).is_empty());

        assert_eq!(describe(&scheduler.prepare_finished(0, true)), ["prepare 1"]);
        assert!(scheduler.prepare_finished(1, true).is_empty());
    }

    #[test]
    fn a_failed_prepare_does_not_stall_the_queue_and_releases_verifies() {
        let mut scheduler = Scheduler::default();

        scheduler.command(prepare_cmd(0), NOT_PREPARED);
        scheduler.command(WorkerCommand::Verify(verify_req(0, 3)), NOT_PREPARED);
        scheduler.command(prepare_cmd(1), NOT_PREPARED);

        assert_eq!(
            describe(&scheduler.prepare_finished(0, false)),
            ["verified 0/3 Unknown", "prepare 1"]
        );
    }

    #[test]
    fn verifies_run_after_their_prepare_succeeds() {
        let mut scheduler = Scheduler::default();

        scheduler.command(prepare_cmd(0), NOT_PREPARED);
        assert!(scheduler.command(WorkerCommand::Verify(verify_req(0, 0)), NOT_PREPARED).is_empty());
        assert!(scheduler.command(WorkerCommand::Verify(verify_req(0, 1)), NOT_PREPARED).is_empty());
        // The same patch twice is only verified once.
        assert!(scheduler.command(WorkerCommand::Verify(verify_req(0, 1)), NOT_PREPARED).is_empty());

        assert_eq!(
            describe(&scheduler.prepare_finished(0, true)),
            ["verify 0/0", "verify 0/1"]
        );
    }

    #[test]
    fn verify_on_a_prepared_recipe_runs_immediately_and_otherwise_prepares_first() {
        let mut scheduler = Scheduler::default();

        assert_eq!(
            describe(&scheduler.command(WorkerCommand::Verify(verify_req(2, 0)), PREPARED)),
            ["verify 2/0"]
        );

        assert_eq!(
            describe(&scheduler.command(WorkerCommand::Verify(verify_req(3, 1)), NOT_PREPARED)),
            ["prepare 3"]
        );

        assert_eq!(describe(&scheduler.prepare_finished(3, true)), ["verify 3/1"]);
    }

    #[test]
    fn prepare_requests_carry_their_verifications() {
        let mut scheduler = Scheduler::default();

        let command = WorkerCommand::Prepare {
            request: PrepareReq {
                recipe_index: 0,
                recipe_name: "r0".to_owned(),
                reset: true,
            },
            verify: vec![verify_req(0, 0), verify_req(0, 2)],
        };

        assert_eq!(describe(&scheduler.command(command, PREPARED)), ["prepare 0"]);
        assert_eq!(
            describe(&scheduler.prepare_finished(0, true)),
            ["verify 0/0", "verify 0/2"]
        );
    }

    #[test]
    fn only_one_cook_at_a_time_and_never_during_prepare() {
        let mut scheduler = Scheduler::default();

        assert_eq!(describe(&scheduler.command(WorkerCommand::Cook(cook_req(0)), PREPARED)), ["cook 0"]);
        assert_eq!(
            describe(&scheduler.command(WorkerCommand::Cook(cook_req(1)), PREPARED)),
            ["cooked 1 ok=false"]
        );
        assert_eq!(
            describe(&scheduler.command(prepare_cmd(0), PREPARED)),
            ["prepared 0 ok=false"]
        );

        scheduler.cook_finished();

        assert_eq!(describe(&scheduler.command(prepare_cmd(1), PREPARED)), ["prepare 1"]);
        assert_eq!(
            describe(&scheduler.command(WorkerCommand::Cook(cook_req(1)), PREPARED)),
            ["cooked 1 ok=false"]
        );
    }

    #[test]
    fn abort_reaches_the_running_job_and_removes_queued_ones() {
        let mut scheduler = Scheduler::default();

        scheduler.command(prepare_cmd(0), NOT_PREPARED);
        scheduler.command(prepare_cmd(1), NOT_PREPARED);
        scheduler.command(WorkerCommand::Verify(verify_req(1, 0)), NOT_PREPARED);

        assert_eq!(
            describe(&scheduler.command(WorkerCommand::Abort { recipe_index: 0 }, NOT_PREPARED)),
            ["abort-prepare"]
        );

        assert_eq!(
            describe(&scheduler.command(WorkerCommand::Abort { recipe_index: 1 }, NOT_PREPARED)),
            ["prepared 1 ok=false", "verified 1/0 Unknown"]
        );

        assert!(scheduler.prepare_finished(0, false).is_empty());

        scheduler.command(WorkerCommand::Cook(cook_req(2)), PREPARED);
        assert_eq!(
            describe(&scheduler.command(WorkerCommand::Abort { recipe_index: 2 }, PREPARED)),
            ["abort-cook"]
        );
    }

    #[test]
    fn duplicate_prepare_for_the_same_recipe_is_ignored() {
        let mut scheduler = Scheduler::default();

        scheduler.command(prepare_cmd(0), NOT_PREPARED);
        assert!(scheduler.command(prepare_cmd(0), NOT_PREPARED).is_empty());
        assert!(scheduler.prepare_finished(0, true).is_empty());
    }

    #[test]
    fn a_panicking_job_becomes_an_error_not_a_stuck_row() {
        let (events_tx, events_rx) = mpsc::channel();
        let (msgs_tx, msgs_rx) = mpsc::channel();

        let env = Env::new(
            std::env::temp_dir().join(format!("pot-worker-test-{}", std::process::id())),
            "test".to_owned(),
        );

        // An empty recipe name makes the work directory the work root itself;
        // whatever happens, the job must report back.
        run_prepare(
            &env,
            &PrepareReq {
                recipe_index: 7,
                recipe_name: String::new(),
                reset: false,
            },
            &events_tx,
            &msgs_tx,
            &Flags::default(),
        );

        let mut saw_prepared = false;

        while let Ok(event) = events_rx.try_recv() {
            if let WorkerEvent::Prepared { recipe_index: 7, .. } = event {
                saw_prepared = true;
            }
        }

        assert!(saw_prepared);
        assert!(matches!(msgs_rx.try_recv(), Ok(Msg::PrepareDone { recipe_index: 7, .. })));

        let _ = fs::remove_dir_all(env.cache_dir);
    }
}
