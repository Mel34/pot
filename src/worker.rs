use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

use crate::cookbook::PatchStatus;

pub enum WorkerCommand {
    Prepare {
        recipe_index: usize,
        cache_dir: PathBuf,
        package: String,
        version: String,
        log: bool,
    },
    Reset {
        recipe_index: usize,
        cache_dir: PathBuf,
        package: String,
        version: String,
        log: bool,
    },
    Verify {
        recipe_index: usize,
        patch_index: usize,
        patch_path: PathBuf,
        cache_dir: PathBuf,
        package: String,
        version: String,
        log: bool,
    },
    Cook {
        recipe_index: usize,
        recipe_name: String,
        patches: Vec<PathBuf>,
        cache_dir: PathBuf,
        repository_name: String,
        log: bool,
    },
    AbortCook,
    Shutdown,
}

enum WorkerJob {
    Prepare {
        recipe_index: usize,
        cache_dir: PathBuf,
        package: String,
        version: String,
        log: bool,
    },
    Reset {
        recipe_index: usize,
        cache_dir: PathBuf,
        package: String,
        version: String,
        log: bool,
    },
    Verify {
        recipe_index: usize,
        patch_index: usize,
        patch_path: PathBuf,
        source_dir: PathBuf,
    },
    Cook {
        recipe_index: usize,
        recipe_name: String,
        patches: Vec<PathBuf>,
        cache_dir: PathBuf,
        repository_name: String,
        log: bool,
    },
    Shutdown,
}

pub enum WorkerEvent {
    Ready,
    Preparing {
        recipe_index: usize,
    },
    Resetting {
        recipe_index: usize,
    },
    Prepared {
        recipe_index: usize,
        work_dir: PathBuf,
        success: bool,
    },
    Verified {
        recipe_index: usize,
        patch_index: usize,
        status: PatchStatus,
    },
    CookOutput {
        recipe_index: usize,
        output: String,
    },
    CookStatus {
        recipe_index: usize,
        status: String,
    },
    Cooked {
        recipe_index: usize,
        output: String,
        success: bool,
    },
    Stopped,
}

pub fn start() -> (Sender<WorkerCommand>, Receiver<WorkerEvent>) {
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let (worker_event_tx, worker_event_rx) = mpsc::channel();
    let cook_abort = Arc::new(AtomicBool::new(false));

    let (prepare_tx, prepare_rx): (Sender<WorkerJob>, Receiver<WorkerJob>) = mpsc::channel();
    let (verify_tx, verify_rx): (Sender<WorkerJob>, Receiver<WorkerJob>) = mpsc::channel();
    let (cook_tx, cook_rx): (Sender<WorkerJob>, Receiver<WorkerJob>) = mpsc::channel();

    {
        let event_tx = worker_event_tx.clone();

        thread::spawn(move || {
            while let Ok(command) = prepare_rx.recv() {
                match command {
                    WorkerJob::Prepare {
                        recipe_index,
                        cache_dir,
                        package,
                        version,
                        log,
                    } => {
                        event_tx
                            .send(WorkerEvent::Preparing { recipe_index })
                            .ok();

                        let result =
                            crate::prepare::prepare_sources(
                                &cache_dir,
                                &package,
                                &version,
                                log,
                            );

                        event_tx
                            .send(WorkerEvent::Prepared {
                                recipe_index,
                                work_dir: result.work_dir,
                                success: result.success,
                            })
                            .ok();
                    }

                    WorkerJob::Reset {
                        recipe_index,
                        cache_dir,
                        package,
                        version,
                        log,
                    } => {
                        event_tx
                            .send(WorkerEvent::Resetting { recipe_index })
                            .ok();

                        let work_dir = cache_dir.join("work").join(&package);

                        if work_dir.exists() {
                            fs::remove_dir_all(&work_dir)
                                .expect("failed to reset POT source tree");
                        }

                        let result =
                            crate::prepare::prepare_sources(
                                &cache_dir,
                                &package,
                                &version,
                                log,
                            );

                        event_tx
                            .send(WorkerEvent::Prepared {
                                recipe_index,
                                work_dir: result.work_dir,
                                success: result.success,
                            })
                            .ok();
                    }

                    WorkerJob::Shutdown => break,

                    WorkerJob::Verify { .. }
                    | WorkerJob::Cook { .. } => {}

                }
            }
        });
    }

    {
        let event_tx = worker_event_tx.clone();

        thread::spawn(move || {
            while let Ok(command) = verify_rx.recv() {
                match command {
                    WorkerJob::Verify {
                        recipe_index,
                        patch_index,
                        patch_path,
                        source_dir,
                    } => {
                        let status = match crate::cookbook::find_source_dir(
                            &source_dir,
                            &patch_path,
                        ) {
                            Some(source_dir) => {
                                if Command::new("git")
                                    .args(["apply", "--check", "-p3"])
                                    .arg(&patch_path)
                                    .current_dir(source_dir)
                                    .status()
                                    .expect("failed to run git")
                                    .success()
                                {
                                    PatchStatus::Clean
                                } else {
                                    PatchStatus::Failed
                                }
                            }

                            None => PatchStatus::Failed,
                        };

                        event_tx
                            .send(WorkerEvent::Verified {
                                recipe_index,
                                patch_index,
                                status,
                            })
                            .ok();
                    }

                    WorkerJob::Shutdown => break,

                    WorkerJob::Prepare { .. }
                    | WorkerJob::Reset { .. }
                    | WorkerJob::Cook { .. } => {}

                }
            }
        });
    }

    {
        let event_tx = worker_event_tx.clone();
        let cook_abort = cook_abort.clone();

        thread::spawn(move || {
            while let Ok(command) = cook_rx.recv() {
                match command {
                    WorkerJob::Cook {
                        recipe_index,
                        recipe_name,
                        patches,
                        cache_dir,
                        repository_name,
                        log,
                    } => {
                        let (output, success) = crate::kitchen::cook(
                            &recipe_name,
                            &patches,
                            &cache_dir,
                            &repository_name,
                            log,
                            &event_tx,
                            recipe_index,
                            cook_abort.clone(),
                        );

                        event_tx
                            .send(WorkerEvent::Cooked {
                                recipe_index,
                                output,
                                success,
                            })
                            .ok();
                    }

                    WorkerJob::Shutdown => break,

                    WorkerJob::Prepare { .. }
                    | WorkerJob::Reset { .. }
                    | WorkerJob::Verify { .. } => {}

                }
            }
        });
    }

    {
        let prepare_tx = prepare_tx.clone();
        let verify_tx = verify_tx.clone();
        let cook_tx = cook_tx.clone();
        let event_tx = event_tx.clone();
        let cook_abort = cook_abort.clone();

        thread::spawn(move || {
            event_tx.send(WorkerEvent::Ready).ok();

            struct ActivePrepare {
                recipe_index: usize,
            }

            struct PendingPrepare {
                recipe_index: usize,
                cache_dir: PathBuf,
                package: String,
                version: String,
                log: bool,
            }

            struct PendingVerify {
                recipe_index: usize,
                patch_index: usize,
                patch_path: PathBuf,
            }

            let mut pending_prepares: VecDeque<PendingPrepare> = VecDeque::new();
            let mut pending_verifies: VecDeque<PendingVerify> = VecDeque::new();

            let mut active_prepare: Option<ActivePrepare> = None;

            loop {
                while let Ok(event) = worker_event_rx.try_recv() {
                    if let WorkerEvent::Prepared {
                        recipe_index,
                        work_dir,
                        success,
                    } = &event
                    {
                        let active = active_prepare
                            .take()
                            .expect("prepared event without active preparation");

                        assert_eq!(
                            active.recipe_index,
                            *recipe_index,
                            "prepared event does not match active preparation",
                        );

                        if *success {
                            let source_dir = work_dir.join("src");
                            let mut remaining = VecDeque::new();

                            while let Some(pending) = pending_verifies.pop_front() {
                                if pending.recipe_index == *recipe_index {
                                    verify_tx
                                        .send(WorkerJob::Verify {
                                            recipe_index: pending.recipe_index,
                                            patch_index: pending.patch_index,
                                            patch_path: pending.patch_path,
                                            source_dir: source_dir.clone(),
                                        })
                                        .ok();
                                } else {
                                    remaining.push_back(pending);
                                }
                            }

                            pending_verifies = remaining;

                            if let Some(pending) = pending_prepares.pop_front() {
                                active_prepare = Some(ActivePrepare {
                                    recipe_index: pending.recipe_index,
                                });

                                prepare_tx
                                    .send(WorkerJob::Prepare {
                                        recipe_index: pending.recipe_index,
                                        cache_dir: pending.cache_dir,
                                        package: pending.package,
                                        version: pending.version,
                                        log: pending.log,
                                    })
                                    .ok();
                            }
                        }
                    }

                    event_tx.send(event).ok();
                }

                match command_rx.recv_timeout(std::time::Duration::from_millis(10)) {
                    Ok(command) => match command {
                        WorkerCommand::Prepare {
                            recipe_index,
                            cache_dir,
                            package,
                            version,
                            log,
                        } => {
                            if active_prepare.is_none() {
                                active_prepare = Some(ActivePrepare {
                                    recipe_index,
                                });

                                prepare_tx
                                    .send(WorkerJob::Prepare {
                                        recipe_index,
                                        cache_dir,
                                        package,
                                        version,
                                        log,
                                    })
                                    .ok();
                            }
                        }

                        WorkerCommand::Reset {
                            recipe_index,
                            cache_dir,
                            package,
                            version,
                            log,
                        } => {
                            if active_prepare.is_none() {
                                active_prepare = Some(ActivePrepare {
                                    recipe_index,
                                });

                                prepare_tx
                                    .send(WorkerJob::Reset {
                                        recipe_index,
                                        cache_dir,
                                        package,
                                        version,
                                        log,
                                    })
                                    .ok();
                            }
                        }

                        WorkerCommand::Verify {
                            recipe_index,
                            patch_index,
                            patch_path,
                            cache_dir,
                            package,
                            version,
                            log,
                        } => {
                            if active_prepare.is_some() {
                                pending_verifies.push_back(PendingVerify {
                                    recipe_index,
                                    patch_index,
                                    patch_path,
                                });

                                let prepare_queued = pending_prepares
                                    .iter()
                                    .any(|pending| pending.recipe_index == recipe_index);

                                if active_prepare
                                    .as_ref()
                                    .map(|active| active.recipe_index == recipe_index)
                                    .unwrap_or(false)
                                {
                                } else if !prepare_queued {
                                    pending_prepares.push_back(PendingPrepare {
                                        recipe_index,
                                        cache_dir,
                                        package,
                                        version,
                                        log,
                                    });
                                }
                            } else if crate::prepare::is_prepared(&cache_dir, &package) {
                                verify_tx
                                    .send(WorkerJob::Verify {
                                        recipe_index,
                                        patch_index,
                                        patch_path,
                                        source_dir: cache_dir.join("work").join(&package).join("src"),
                                    })
                                    .ok();
                            } else {
                                pending_verifies.push_back(PendingVerify {
                                    recipe_index,
                                    patch_index,
                                    patch_path,
                                });

                                active_prepare = Some(ActivePrepare {
                                    recipe_index,
                                });

                                prepare_tx
                                    .send(WorkerJob::Prepare {
                                        recipe_index,
                                        cache_dir,
                                        package,
                                        version,
                                        log,
                                    })
                                    .ok();
                            }
                        }

                        WorkerCommand::Cook {
                            recipe_index,
                            recipe_name,
                            patches,
                            cache_dir,
                            repository_name,
                            log,
                        } => {
                            cook_abort.store(false, Ordering::Relaxed);

                            cook_tx
                                .send(WorkerJob::Cook {
                                    recipe_index,
                                    recipe_name,
                                    patches,
                                    cache_dir,
                                    repository_name,
                                    log,
                                })
                                .ok();
                        }

                        WorkerCommand::AbortCook => {
                            cook_abort.store(true, Ordering::Relaxed);
                        }

                        WorkerCommand::Shutdown => {
                            prepare_tx.send(WorkerJob::Shutdown).ok();
                            verify_tx.send(WorkerJob::Shutdown).ok();
                            cook_tx.send(WorkerJob::Shutdown).ok();
                            event_tx.send(WorkerEvent::Stopped).ok();
                            break;
                        }
                    },

                    Err(mpsc::RecvTimeoutError::Timeout) => {}

                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    }

    (command_tx, event_rx)
}
