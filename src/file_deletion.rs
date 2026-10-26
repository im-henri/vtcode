//! Cancellable Explorer deletion. Count without collecting the tree in memory,
//! then remove entries in postorder without following directory links.
use crate::background::{
    BackgroundResult, BackgroundResultSender, CancellationToken, FilesystemActionOutcome,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[path = "file_deletion_io.rs"]
mod io;

pub(crate) const SHOW_DELAY: Duration = Duration::from_millis(200);
pub(crate) const FINISHED_DELAY: Duration = Duration::from_secs(4);
const UPDATE_INTERVAL: Duration = Duration::from_millis(80);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Phase {
    #[default]
    Queued,
    Counting,
    Deleting,
    Complete,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Progress {
    pub phase: Phase,
    pub total: usize,
    pub processed: usize,
    pub deleted: usize,
    pub failed: usize,
    pub total_known: bool,
}

#[derive(Default)]
pub(crate) struct Shared {
    progress: Mutex<Progress>,
    pending: AtomicBool,
}

impl Shared {
    pub(crate) fn snapshot(&self) -> Progress {
        *self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    pub(crate) fn acknowledge(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

pub(crate) struct Deletion {
    pub shared: Arc<Shared>,
    pub cancel: CancellationToken,
    pub label: String,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub shown: bool,
    pub area: Option<ratatui::layout::Rect>,
    pub button: Option<ratatui::layout::Rect>,
}

impl Drop for Deletion {
    fn drop(&mut self) {
        if self.finished.is_none() {
            self.cancel.cancel();
        }
    }
}

impl Deletion {
    pub(crate) fn next_wait(&self) -> Option<Duration> {
        if let Some(finished) = self.finished {
            Some(FINISHED_DELAY.saturating_sub(finished.elapsed()))
        } else if !self.shown {
            Some(SHOW_DELAY.saturating_sub(self.started.elapsed()))
        } else {
            None
        }
    }
    pub(crate) fn visible(&self) -> bool {
        self.shown || self.started.elapsed() >= SHOW_DELAY || self.cancel.is_cancelled()
    }
}

struct Reporter<'a> {
    shared: &'a Shared,
    sender: &'a BackgroundResultSender,
    cancel: &'a CancellationToken,
    last: Instant,
}

impl Reporter<'_> {
    fn publish(&mut self, progress: Progress, force: bool) {
        if !force && self.last.elapsed() < UPDATE_INTERVAL {
            return;
        }
        self.last = Instant::now();
        *self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = progress;
        // One coalesced wakeup, rather than an event or allocation per file.
        if !self.shared.pending.swap(true, Ordering::AcqRel) {
            let _ = self
                .sender
                .send(BackgroundResult::DeletionProgress(self.cancel.clone()));
        }
    }
}

fn roots(targets: Vec<PathBuf>, cancel: &CancellationToken) -> std::io::Result<Vec<PathBuf>> {
    let mut roots = Vec::with_capacity(targets.len());
    for target in targets {
        if cancel.is_cancelled() {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let absolute = std::path::absolute(target)?;
        let name = absolute
            .file_name()
            .ok_or_else(|| std::io::Error::other("Cannot delete a filesystem root"))?;
        // Resolve the parent only: deleting a symlink removes the link itself.
        // All traversal and canonicalization happen on the worker.
        let parent = absolute
            .parent()
            .ok_or_else(|| std::io::Error::other("Invalid deletion path"))?;
        let resolved = std::fs::canonicalize(parent)?.join(name);
        roots.push(resolved);
    }
    roots.sort_unstable();
    roots.dedup();
    let mut selected: Vec<PathBuf> = Vec::with_capacity(roots.len());
    for root in roots {
        if !selected
            .last()
            .is_some_and(|parent| root.starts_with(parent))
        {
            selected.push(root);
        }
    }
    Ok(selected)
}

fn remove_entry(root: &mut io::Root, entry: &walkdir::DirEntry) -> std::io::Result<()> {
    root.remove(entry).map_err(|error| {
        std::io::Error::new(error.kind(), format!("{}: {error}", entry.path().display()))
    })
}

pub(crate) fn perform(
    targets: Vec<PathBuf>,
    cancel: &CancellationToken,
    mut publish: impl FnMut(Progress, bool),
) -> FilesystemActionOutcome {
    let mut progress = Progress {
        phase: Phase::Counting,
        ..Default::default()
    };
    let mut outcome = FilesystemActionOutcome::default();
    let mut last_error = None;
    publish(progress, true);
    outcome.check_deleted_paths = targets.clone();
    let targets = match roots(targets, cancel) {
        Ok(targets) => targets,
        Err(error) => {
            progress.phase = if cancel.is_cancelled() {
                Phase::Cancelled
            } else {
                Phase::Failed
            };
            progress.failed = usize::from(!cancel.is_cancelled());
            publish(progress, true);
            outcome.message = if cancel.is_cancelled() {
                "Deletion cancelled: no items deleted".into()
            } else {
                format!("Delete failed: {error}")
            };
            outcome.warning = !cancel.is_cancelled();
            return outcome;
        }
    };
    // WalkDir bounds open directory handles and does not retain a full list of
    // files. No file contents or sizes are read merely to delete directory entries.
    'count: for target in &targets {
        let mut walk = walkdir::WalkDir::new(target)
            .follow_links(false)
            .follow_root_links(false)
            .max_open(16)
            .into_iter();
        loop {
            if cancel.is_cancelled() {
                break 'count;
            }
            let Some(entry) = walk.next() else {
                break;
            };
            match entry {
                Ok(_) => progress.total += 1,
                Err(error) => {
                    if error
                        .io_error()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                    {
                        continue;
                    }
                    last_error = Some(error.to_string());
                    progress.failed += 1;
                    break 'count;
                }
            }
            publish(progress, false);
        }
    }
    if !cancel.is_cancelled() && progress.failed == 0 {
        progress.total_known = true;
        progress.phase = Phase::Deleting;
        publish(progress, true);
        'delete: for target in &targets {
            let mut root = match io::Root::new(target) {
                Ok(root) => root,
                Err(error) => {
                    progress.failed += 1;
                    last_error = Some(format!("{}: {error}", target.display()));
                    continue;
                }
            };
            let mut walk = walkdir::WalkDir::new(target)
                .follow_links(false)
                .follow_root_links(false)
                .contents_first(true)
                .max_open(16)
                .into_iter();
            loop {
                // Check before iterator advancement as well as before each
                // unlink, including while a large directory is being traversed.
                if cancel.is_cancelled() {
                    break 'delete;
                }
                let Some(entry) = walk.next() else {
                    break;
                };
                if cancel.is_cancelled() {
                    break 'delete;
                }
                let result = match entry {
                    Ok(entry) => remove_entry(&mut root, &entry),
                    Err(error) => Err(std::io::Error::new(
                        error
                            .io_error()
                            .map_or(std::io::ErrorKind::Other, |error| error.kind()),
                        error.to_string(),
                    )),
                };
                progress.processed += 1;
                progress.total = progress.total.max(progress.processed);
                match result {
                    Ok(()) => progress.deleted += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        progress.failed += 1;
                        last_error = Some(error.to_string());
                    }
                }
                publish(progress, false);
            }
            if std::fs::symlink_metadata(target)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                outcome.deleted.push(target.clone());
            }
        }
    }
    progress.phase = if cancel.is_cancelled() && progress.processed < progress.total {
        Phase::Cancelled
    } else if progress.failed != 0 {
        Phase::Failed
    } else {
        Phase::Complete
    };
    // Count-phase cancellation must remain cancellation even with no entries yet.
    if cancel.is_cancelled() && progress.deleted == 0 {
        progress.phase = Phase::Cancelled;
    }
    if progress.phase == Phase::Complete {
        progress.total = progress.processed;
    }
    outcome.message = match progress.phase {
        Phase::Cancelled => format!(
            "Deletion cancelled: {} items deleted; remaining items kept",
            progress.deleted
        ),
        Phase::Failed => format!(
            "Deletion failed: {} items deleted; {} errors{}",
            progress.deleted,
            progress.failed,
            last_error
                .as_deref()
                .map(|error| format!(": {error}"))
                .unwrap_or_default()
        ),
        _ => format!(
            "Deleted {} {}",
            progress.deleted,
            if progress.deleted == 1 {
                "item"
            } else {
                "items"
            }
        ),
    };
    outcome.warning = progress.failed != 0;
    publish(progress, true);
    outcome
}

impl crate::app::App {
    pub(crate) fn start_file_deletion(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        if self.bg_filesystem_action_running {
            self.notify("Another file operation is already running");
            return;
        }
        let label = if targets.len() == 1 {
            targets[0]
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            format!("{} selected items", targets.len())
        };
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let sender = self.background.sender.clone();
        let generation = self.workspace_generation;
        let remote = self.remote_workspace.clone();
        self.bg_filesystem_action_running = true;
        self.status_message = "Deleting…".into();
        let cancel = self
            .background
            .spawn_cancellable("explorer-delete", move |cancel| {
                let mut reporter = Reporter {
                    shared: &worker_shared,
                    sender: &sender,
                    cancel: &cancel,
                    last: Instant::now(),
                };
                let result = match remote {
                    Some(remote) => (|| -> std::io::Result<_> {
                        let paths = targets.iter().map(|path| remote.remote_path(path)).collect::<std::io::Result<_>>()?;
                        remote.filesystem(crate::remote::fs::Task::Delete { paths }, &cancel, |progress| reporter.publish(progress, true))
                    })().map_err(|error| error.to_string()),
                    None => Ok(perform(targets, &cancel, |progress, force| reporter.publish(progress, force))),
                };
                let result = BackgroundResult::FilesystemActionDone {
                    action: "Delete",
                    result,
                };
                let _ = sender.send(BackgroundResult::WorkspaceResult { generation, result: Box::new(result) });
            });
        self.file_deletion = Some(Deletion {
            shared,
            cancel,
            label,
            started: Instant::now(),
            finished: None,
            shown: false,
            area: None,
            button: None,
        });
    }

    pub(crate) fn cancel_file_deletion(&mut self) {
        if let Some(deletion) = self
            .file_deletion
            .as_mut()
            .filter(|deletion| deletion.finished.is_none())
        {
            deletion.cancel.cancel();
            deletion.shown = true;
            self.status_message =
                "Stopping deletion after the current filesystem operation…".into();
        }
    }

    pub(crate) fn update_file_deletion_timer(&mut self) -> bool {
        let Some(deletion) = &self.file_deletion else {
            return false;
        };
        if deletion
            .finished
            .is_some_and(|finished| finished.elapsed() >= FINISHED_DELAY)
        {
            self.file_deletion = None;
            true
        } else {
            self.terminal_has_focus && !deletion.shown && deletion.visible()
        }
    }
}

#[cfg(test)]
#[path = "file_deletion_tests.rs"]
mod tests;
