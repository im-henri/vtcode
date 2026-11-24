//! Tooltip reads follow a settled pointer, never a sweep across the file tree.
//! Keep at most one read queued/running and one latest candidate, including
//! when an antivirus, shared disk or remote filesystem delays a metadata call.
//! A remote workspace reads the same metadata on its node, one request per
//! settled hover; a failed read is not retried until the pointer moves.
use crate::{
    app::{App, SidebarView, TabKind},
    background::{BackgroundResult, CancellationToken, ExplorerHoverInfo},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const QUIET: Duration = Duration::from_millis(200);

struct Target {
    path: PathBuf,
    is_dir: bool,
    since: Instant,
}
struct Request {
    path: PathBuf,
    generation: u64,
    cancel: CancellationToken,
}

#[derive(Default)]
pub(crate) struct State {
    target: Option<Target>,
    active: Option<Request>,
    generation: u64,
    /// The hovered path whose read failed (e.g. remote error or missing file).
    failed: Option<PathBuf>,
}

impl State {
    pub(crate) fn invalidate(&mut self, path: Option<&Path>) {
        if let Some(active) = &self.active
            && path.is_none_or(|path| path == active.path)
        {
            active.cancel.cancel();
        }
    }

    /// A current, uncancelled read that found nothing is not retried until
    /// the pointer moves, so an unreachable path cannot spin the event loop.
    pub(crate) fn finish(&mut self, generation: u64, found: bool) -> bool {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.generation != generation)
        {
            return false;
        }
        let active = self.active.take().unwrap();
        if active.cancel.is_cancelled() {
            return false;
        }
        if !found {
            self.failed = Some(active.path);
        }
        true
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.invalidate(None);
    }
}

fn target(app: &App) -> Option<(&Path, bool)> {
    if !app.terminal_has_focus
        || !app.sidebar_is_visible()
        || app.sidebar_view != SidebarView::Explorer
        || app.change_dir_active
        || app.explorer_context_active
        || app.context_menu_active
        || app.menu_open
        || crate::ui_modal::active(app).is_some()
    {
        return None;
    }
    app.hovered_explorer
        .and_then(|index| app.explorer.entries.get(index))
        .map(|entry| (entry.path.as_path(), entry.is_dir))
}

pub(crate) fn next_wait(app: &App, now: Instant) -> Option<Duration> {
    let (path, _) = target(app)?;
    let pending = app.explorer_hover.target.as_ref()?;
    if app.explorer_hover.active.is_some()
        || pending.path != path
        || app.explorer_hover.failed.as_deref() == Some(path)
        || app.explorer_hover_info_cache.contains_key(path)
    {
        return None;
    }
    Some(QUIET.saturating_sub(now.saturating_duration_since(pending.since)))
}

pub(crate) fn poll(app: &mut App, now: Instant) {
    let candidate = target(app);
    if app
        .explorer_hover
        .target
        .as_ref()
        .map(|target| (target.path.as_path(), target.is_dir))
        != candidate
    {
        let candidate = candidate.map(|(path, is_dir)| Target {
            path: path.to_owned(),
            is_dir,
            since: now,
        });
        app.explorer_hover.invalidate(None);
        app.explorer_hover.target = candidate;
        app.explorer_hover.failed = None;
    }
    if next_wait(app, now).is_some_and(|wait| wait.is_zero()) {
        let target = app.explorer_hover.target.as_ref().unwrap();
        let (path, is_dir) = (target.path.clone(), target.is_dir);
        let open = app
            .tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| tab.editor.file_path.as_ref() == Some(&path));
        app.request_explorer_hover_info(path, is_dir, !open);
    }
}

impl App {
    /// The delayed caller and one read slot bound filesystem/line-count work.
    /// Direct callers can request a single read without a pointer, e.g. tests.
    pub(crate) fn request_explorer_hover_info(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        count_lines: bool,
    ) {
        if self.explorer_hover_info_cache.contains_key(&path)
            || self.explorer_hover.active.is_some()
        {
            return;
        }
        self.explorer_hover.generation = self.explorer_hover.generation.wrapping_add(1);
        let generation = self.explorer_hover.generation;
        let cancel = CancellationToken::new();
        self.explorer_hover.active = Some(Request {
            path: path.clone(),
            generation,
            cancel: cancel.clone(),
        });
        let snapshot = if count_lines && !is_dir && !self.pending_file_reloads.contains(&path) {
            self.tabs
                .iter()
                .chain(self.groups.iter().flat_map(|group| &group.tabs))
                .find(|tab| {
                    tab.kind == TabKind::File
                        && tab.editor.file_path.as_ref() == Some(&path)
                        && !tab.editor.modified
                        && !tab.editor.bg_loading
                        && tab.editor.encoding != "Binary"
                })
                .and_then(|tab| {
                    tab.editor.saved_rope.as_ref().map(|rope| {
                        (
                            tab.editor.last_disk_modified,
                            tab.editor.file_size,
                            rope.len_lines(),
                        )
                    })
                })
        } else {
            None
        };
        // Remote view keys are placeholders on this machine: ask the node.
        let remote = self
            .remote_workspace
            .clone()
            .filter(|workspace| workspace.remote_path(&path).is_ok());
        let sender = self.background.sender.clone();
        // Optional metadata must not occupy the sole foreground read worker
        // on a one-core machine. Reuse the serial maintenance worker, which
        // can wait on a slow filesystem without holding up an explicit open.
        let worker = self.background.maintenance_sender();
        let activity = self.background.activity.start(
            crate::performance::worker_label("explorer-hover"),
            Some(&path),
        );
        let queued = worker.send(Box::new(move || {
            let _activity = activity.running();
            let info = (|| {
                if cancel.is_cancelled() {
                    return None;
                }
                if let Some(remote) = remote {
                    let count = count_lines && !is_dir;
                    let stat = remote
                        .hover_info(&path, count && snapshot.is_none(), &cancel)
                        .ok()??;
                    let cached_lines = snapshot
                        .filter(|(modified, size, _)| {
                            modified.is_some() && *modified == stat.modified && *size == stat.size
                        })
                        .map(|(_, _, lines)| lines);
                    return (!cancel.is_cancelled()).then(|| ExplorerHoverInfo {
                        path,
                        is_dir: stat.is_dir,
                        size: stat.size,
                        modified: stat.modified,
                        line_count: count.then(|| cached_lines.or(stat.line_count)).flatten(),
                    });
                }
                let metadata = std::fs::metadata(&path).ok();
                if cancel.is_cancelled() {
                    return None;
                }
                let cached_lines = snapshot.and_then(|(modified, size, lines)| {
                    metadata
                        .as_ref()
                        .filter(|metadata| {
                            modified.is_some()
                                && modified == metadata.modified().ok()
                                && size == metadata.len()
                        })
                        .map(|_| lines)
                });
                let line_count = if count_lines && !is_dir {
                    cached_lines.or_else(|| {
                        if metadata.as_ref().is_none_or(|metadata| {
                            metadata.len() > crate::background::HOVER_LINE_COUNT_MAX_BYTES
                        }) {
                            return None;
                        }
                        std::fs::File::open(&path).ok().and_then(|file| {
                            crate::background::bounded_hover_line_count(file, &cancel)
                        })
                    })
                } else {
                    None
                };
                (!cancel.is_cancelled()).then(|| ExplorerHoverInfo {
                    path,
                    is_dir,
                    size: metadata.as_ref().map_or(0, std::fs::Metadata::len),
                    modified: metadata.and_then(|metadata| metadata.modified().ok()),
                    line_count,
                })
            })();
            // Always retire the slot, including cancellation before execution.
            let _ = sender.send(BackgroundResult::ExplorerHoverInfo { generation, info });
        }));
        if queued.is_err() {
            self.explorer_hover.active = None;
            self.explorer_hover.target = None;
        }
    }
}

#[cfg(test)]
#[path = "app_explorer_hover_tests.rs"]
mod tests;
