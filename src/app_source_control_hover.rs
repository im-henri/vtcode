use crate::{
    app::{App, GitFileChange, SidebarView, SourceControlRepositoryState},
    background::{BackgroundResult, CancellationToken},
};
use ratatui::layout::Rect;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const HOVER_DELAY: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ChangeStats {
    Text { added: usize, deleted: usize },
    Binary,
    Unavailable,
}

/// Read the same comparison as the status row, limited to its literal paths.
pub(crate) fn read_stats<P: AsRef<Path>>(
    root: &Path,
    change: &GitFileChange<P>,
) -> Result<ChangeStats, git2::Error> {
    if change.directory || change.status == '!' {
        return Ok(ChangeStats::Unavailable);
    }
    let repo = git2::Repository::open(root)?;
    let path = change.path.as_ref();
    let original = change.original_path.as_ref().map(AsRef::as_ref);
    let mut options = git2::DiffOptions::new();
    options
        .disable_pathspec_match(true)
        .pathspec(path)
        .context_lines(0);
    if let Some(original) = original {
        options.pathspec(original);
    }
    let mut diff = if change.staged {
        let head = match repo.head() {
            Ok(head) => Some(head.peel_to_tree()?),
            Err(error)
                if matches!(
                    error.code(),
                    git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
                ) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        repo.diff_tree_to_index(head.as_ref(), None, Some(&mut options))?
    } else {
        options.show_untracked_content(change.status == '?');
        repo.diff_index_to_workdir(None, Some(&mut options))?
    };
    if original.is_some() {
        diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;
    }

    let mut found = false;
    let (mut added, mut deleted) = (0, 0);
    for (index, delta) in diff.deltas().enumerate() {
        let new_path = delta.new_file().path().or_else(|| delta.old_file().path());
        if new_path != Some(path)
            || original.is_some_and(|old| {
                delta.status() != git2::Delta::Renamed || delta.old_file().path() != Some(old)
            })
        {
            continue;
        }
        if delta.status() == git2::Delta::Conflicted {
            return Ok(ChangeStats::Unavailable);
        }
        let patch = git2::Patch::from_diff(&diff, index)?;
        if delta.flags().contains(git2::DiffFlags::BINARY) {
            return Ok(ChangeStats::Binary);
        }
        if let Some(patch) = patch {
            let (_, insertions, deletions) = patch.line_stats()?;
            added += insertions;
            deleted += deletions;
            found = true;
        }
    }
    Ok(if found {
        ChangeStats::Text { added, deleted }
    } else {
        ChangeStats::Unavailable
    })
}

struct Target {
    root: PathBuf,
    view: u64,
    refresh: Option<Instant>,
    file: GitFileChange,
    since: Instant,
    stats: Option<ChangeStats>,
}

impl Target {
    fn matches(&self, app: &App, file: &GitFileChange) -> bool {
        self.root == app.source_control_root()
            && self.view == app.repositories.view_generation
            && self.refresh == app.source_control_last_refresh
            && self.file == *file
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RowHit {
    pub(crate) index: usize,
    pub(crate) row: Rect,
    pub(crate) actions: Rect,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) hit: Option<RowHit>,
    target: Option<Target>,
    active: Option<CancellationToken>,
}

impl State {
    pub(crate) fn clear_target(&mut self) {
        self.target = None;
        if let Some(active) = &self.active {
            // Retain the slot until the worker replies, including on cancellation.
            active.cancel();
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.clear_target();
    }
}

fn hit(app: &App) -> Option<(&GitFileChange, Rect)> {
    let RowHit {
        index,
        row,
        actions,
    } = app.source_control_file_hover.hit?;
    let point = (app.mouse_col, app.mouse_row).into();
    if !row.contains(point)
        || actions.contains(point)
        || !app.workspace_visible
        || !app.terminal_has_focus
        || !app.sidebar_is_visible()
        || app.sidebar_view != SidebarView::SourceControl
        || app.source_control_repository_state != SourceControlRepositoryState::Available
        || app.sc_context_active
        || app.context_menu_active
        || app.menu_active
        || app.palette.active
        || crate::ui_modal::active(app).is_some()
    {
        return None;
    }
    Some((app.source_control_files.get(index)?, row))
}

pub(crate) fn summary(app: &App) -> Option<(&GitFileChange, Rect, ChangeStats)> {
    let (file, area) = hit(app)?;
    let target = app.source_control_file_hover.target.as_ref()?;
    target
        .matches(app, file)
        .then_some((file, area, target.stats?))
}

pub(crate) fn next_wait(app: &App, now: Instant) -> Option<Duration> {
    let hover = &app.source_control_file_hover;
    let target = hover.target.as_ref()?;
    if hover.active.is_some() || target.stats.is_some() {
        return None;
    }
    Some(HOVER_DELAY.saturating_sub(now.saturating_duration_since(target.since)))
}

pub(crate) fn poll(app: &mut App, now: Instant) {
    let candidate = hit(app).map(|(file, _)| file);
    let same = match (&app.source_control_file_hover.target, candidate) {
        (Some(target), Some(file)) => target.matches(app, file),
        (None, None) => true,
        _ => false,
    };
    if !same {
        let target = candidate.map(|file| Target {
            root: app.source_control_root().to_owned(),
            view: app.repositories.view_generation,
            refresh: app.source_control_last_refresh,
            file: file.clone(),
            since: now,
            stats: None,
        });
        app.source_control_file_hover.clear_target();
        app.source_control_file_hover.target = target;
    }
    if !next_wait(app, now).is_some_and(|wait| wait.is_zero()) {
        return;
    }

    let target = app.source_control_file_hover.target.as_ref().unwrap();
    let (root, file) = (target.root.clone(), target.file.clone());
    let remote = app.remote_workspace.clone();
    let sender = app.background.sender.clone();
    app.source_control_file_hover.active = Some(app.background.spawn_cancellable(
        "git-file-stats",
        move |cancel| {
            let stats = if cancel.is_cancelled() || file.status == '!' {
                ChangeStats::Unavailable
            } else if let Some(remote) = remote {
                remote
                    .git_file_stats(&root, file, &cancel)
                    .unwrap_or(ChangeStats::Unavailable)
            } else {
                read_stats(&root, &file).unwrap_or(ChangeStats::Unavailable)
            };
            let _ = sender.send(BackgroundResult::SourceControlFileStats(stats));
        },
    ));
}

pub(crate) fn finish(app: &mut App, stats: ChangeStats) -> bool {
    let hover = &mut app.source_control_file_hover;
    if hover
        .active
        .take()
        .is_none_or(|active| active.is_cancelled())
    {
        return false;
    }
    let Some((file, _)) = hit(app) else {
        return false;
    };
    if app
        .source_control_file_hover
        .target
        .as_ref()
        .is_some_and(|target| target.matches(app, file))
    {
        app.source_control_file_hover.target.as_mut().unwrap().stats = Some(stats);
        return true;
    }
    false
}

#[cfg(test)]
#[path = "app_source_control_hover_tests.rs"]
mod tests;
