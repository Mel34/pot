use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::cookbook::PatchStatus;

pub enum WorkerCommand {
    Prepare {
        recipe_index: usize,
        cache_dir: PathBuf,
        package: String,
        version: String,
        verbose: bool,
    },
    Verify {
        recipe_index: usize,
        patch_index: usize,
        patch_path: PathBuf,
        source_dir: PathBuf,
    },
    Shutdown,
}

pub enum WorkerEvent {
    Ready,
    Preparing {
        recipe_index: usize,
    },
    Prepared {
        recipe_index: usize,
        work_dir: PathBuf,
    },
    Verified {
        recipe_index: usize,
        patch_index: usize,
        status: PatchStatus,
    },
    Stopped,
}

pub fn start() -> (Sender<WorkerCommand>, Receiver<WorkerEvent>) {
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();

    let (prepare_tx, prepare_rx) = mpsc::channel();
    let (verify_tx, verify_rx) = mpsc::channel();

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
                        verbose,
                    } => {
                        event_tx
                            .send(WorkerEvent::Preparing { recipe_index })
                            .ok();

                        let work_dir =
                            crate::prepare::prepare_sources(
                                &cache_dir,
                                &package,
                                &version,
                                verbose,
                            );

                        event_tx
                            .send(WorkerEvent::Prepared {
                                recipe_index,
                                work_dir,
                            })
                            .ok();
                    }

                    WorkerCommand::Shutdown => break,

                    WorkerCommand::Verify { .. } => {}
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

                    WorkerCommand::Prepare { .. } => {}
                }
            }
        });
    }

    {
        let prepare_tx = prepare_tx.clone();
        let verify_tx = verify_tx.clone();
        let event_tx = event_tx.clone();

        thread::spawn(move || {
            event_tx.send(WorkerEvent::Ready).ok();

            while let Ok(command) = command_rx.recv() {
                match command {
                    WorkerCommand::Prepare { .. } => {
                        prepare_tx.send(command).ok();
                    }

                    WorkerCommand::Verify { .. } => {
                        verify_tx.send(command).ok();
                    }

                    WorkerCommand::Shutdown => {
                        prepare_tx.send(WorkerCommand::Shutdown).ok();
                        verify_tx.send(WorkerCommand::Shutdown).ok();

                        event_tx.send(WorkerEvent::Stopped).ok();

                        break;
                    }
                }
            }
        });
    }

    (command_tx, event_rx)
}
