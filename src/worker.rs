use std::fs;
use std::path::PathBuf;
use std::process::Command;
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
    AbortCook,
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
        output: String,
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
    let cook_abort = Arc::new(AtomicBool::new(false));

    let (prepare_tx, prepare_rx) = mpsc::channel();
    let (verify_tx, verify_rx) = mpsc::channel();
    let (cook_tx, cook_rx) = mpsc::channel();

    {
        let event_tx = event_tx.clone();

        thread::spawn(move || {
            while let Ok(command) = prepare_rx.recv() {
                match command {
                    WorkerCommand::Prepare {
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
                                output: result.output,
                                success: result.success,
                            })
                            .ok();
                    }

                    WorkerCommand::Reset {
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
                                output: result.output,
                                success: result.success,
                            })
                            .ok();
                    }

                    WorkerCommand::Shutdown => break,

                    WorkerCommand::Verify { .. }
                    | WorkerCommand::Cook { .. }
                    | WorkerCommand::AbortCook => {}
                }
            }
        });
    }

    {
        let event_tx = event_tx.clone();

        thread::spawn(move || {
            while let Ok(command) = verify_rx.recv() {
                match command {
                    WorkerCommand::Verify {
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

                    WorkerCommand::Shutdown => break,

                    WorkerCommand::Prepare { .. }
                    | WorkerCommand::Reset { .. }
                    | WorkerCommand::Cook { .. }
                    | WorkerCommand::AbortCook => {}
                }
            }
        });
    }

    {
        let event_tx = event_tx.clone();
        let cook_abort = cook_abort.clone();

        thread::spawn(move || {
            while let Ok(command) = cook_rx.recv() {
                match command {
                    WorkerCommand::Cook {
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

                    WorkerCommand::Shutdown => break,

                    WorkerCommand::Prepare { .. }
                    | WorkerCommand::Reset { .. }
                    | WorkerCommand::Verify { .. }
                    | WorkerCommand::AbortCook => {}
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

            while let Ok(command) = command_rx.recv() {
                match command {
                    WorkerCommand::Prepare { .. } => {
                        prepare_tx.send(command).ok();
                    }

                    WorkerCommand::Reset { .. } => {
                        prepare_tx.send(command).ok();
                    }

                    WorkerCommand::Verify { .. } => {
                        verify_tx.send(command).ok();
                    }

                    WorkerCommand::Cook { .. } => {
                        cook_abort.store(false, Ordering::Relaxed);
                        cook_tx.send(command).ok();
                    }

                    WorkerCommand::AbortCook => {
                        cook_abort.store(true, Ordering::Relaxed);
                    }

                    WorkerCommand::Shutdown => {
                        prepare_tx.send(WorkerCommand::Shutdown).ok();
                        verify_tx.send(WorkerCommand::Shutdown).ok();
                        cook_tx.send(WorkerCommand::Shutdown).ok();

                        event_tx.send(WorkerEvent::Stopped).ok();

                        break;
                    }
                }
            }
        });
    }

    (command_tx, event_rx)
}
