//! Snapshot-based Git blame. Git and buffer comparisons stay off the UI thread.
use crate::app::{App, Tab, TabKind};
use crate::background::{BackgroundResult, BlameCacheKey};
use ropey::Rope;
use rustc_hash::FxHashMap;
use std::{path::PathBuf, sync::Arc, time::{Duration, Instant}};

#[derive(Debug)]
pub(crate) struct Commit {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub age: String,
    pub date: String,
    pub committed_at: String,
    pub summary: String,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug)]
pub(crate) enum Line {
    Committed(Arc<Commit>),
    Uncommitted,
    Empty,
}

impl Line {
    pub fn label(&self) -> String {
        match self {
            Self::Committed(commit) => format!("{}, {}", commit.author, commit.age),
            Self::Uncommitted => "Uncommitted changes".into(),
            Self::Empty => String::new(),
        }
    }
}

pub(crate) struct Document {
    pub path: PathBuf,
    pub revision: Option<Arc<Revision>>,
    pub rope: Rope,
    pub key: Option<BlameCacheKey>,
    pub lines: Vec<Line>,
    pub commit_lines: Vec<Option<usize>>,
    pub notice: Option<String>,
}

/// The repository and tree path are pinned, even after a rename or HEAD change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    pub workdir: PathBuf,
    pub path: PathBuf,
    pub working_file: PathBuf,
    pub hash: String,
    pub date: String,
    pub summary: String,
    /// A separate highlighting cache identity; this path is never read or written.
    pub syntax_path: PathBuf,
}

impl Revision {
    pub fn file_path(&self) -> PathBuf {
        self.workdir.join(&self.path)
    }

    pub fn short_hash(&self) -> &str {
        self.hash.get(..7).unwrap_or(&self.hash)
    }

    pub fn tab_title(&self) -> String {
        format!(
            "{} @ {}",
            self.path.file_name().unwrap_or_default().to_string_lossy(),
            self.short_hash()
        )
    }
}

fn clean(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(limit)
        .collect()
}

fn commit_date(time: git2::Time) -> String {
    format_commit_time(time, "%Y-%m-%d")
}

fn format_commit_time(time: git2::Time, format: &str) -> String {
    let offset = chrono::FixedOffset::east_opt(time.offset_minutes().saturating_mul(60));
    chrono::DateTime::from_timestamp(time.seconds(), 0)
        .and_then(|date| offset.map(|offset| date.with_timezone(&offset)))
        .map_or_else(
            || "Unknown date".into(),
            |date| date.format(format).to_string(),
        )
}

#[cfg(test)]
pub(crate) fn prepare(path: PathBuf, rope: Rope, previous: Option<Arc<Document>>) -> Document {
    prepare_at(path, rope, previous, None)
}

fn prepare_at(
    path: PathBuf,
    rope: Rope,
    previous: Option<Arc<Document>>,
    revision: Option<Arc<Revision>>,
) -> Document {
    prepare_cancellable(path, rope, previous, revision, None)
}

pub(crate) fn prepare_cancellable(
    path: PathBuf,
    rope: Rope,
    previous: Option<Arc<Document>>,
    revision: Option<Arc<Revision>>,
    cancel: Option<&crate::background::CancellationToken>,
) -> Document {
    let check_cancelled = || {
        if cancel.is_some_and(|cancel| cancel.is_cancelled()) { Err("Git blame cancelled".to_owned()) }
        else { Ok(()) }
    };
    let result = (|| -> Result<Document, String> {
        check_cancelled()?;
        let repository = git2::Repository::discover(revision.as_ref().map_or_else(
            || path.parent().unwrap_or(&path),
            |revision| revision.workdir.as_path(),
        ))
        .map_err(|_| "Git blame is available for files in a Git repository.".to_string())?;
        check_cancelled()?;
        let workdir = repository
            .workdir()
            .ok_or("This repository has no working directory.")?;
        let relative = revision
            .as_ref()
            .map(|revision| revision.path.clone())
            .or_else(|| crate::app_git::repository_relative_path(workdir, &path))
            .ok_or("This file is outside the repository.")?;
        let head = match revision.as_ref() {
            Some(revision) => {
                git2::Oid::from_str(&revision.hash).and_then(|id| repository.find_commit(id))
            }
            None => repository.head().and_then(|head| head.peel_to_commit()),
        }
        .map_err(|_| "This repository has no committed history yet.")?;
        let tree = head.tree().map_err(|e| e.to_string())?;
        let entry = tree
            .get_path(&relative)
            .map_err(|_| "This file has no committed history yet.")?;
        let blob = repository
            .find_blob(entry.id())
            .map_err(|e| e.to_string())?;
        let metadata = if revision.is_none() {
            std::fs::metadata(&path).ok()
        } else {
            None
        };
        let key = BlameCacheKey {
            file_path: path.clone(),
            head_id: head.id().to_string(),
            disk_modified: metadata.as_ref().and_then(|meta| meta.modified().ok()),
            disk_len: metadata.as_ref().map_or(0, std::fs::Metadata::len),
            line_count: rope.len_lines(),
        };
        if let Some(previous) = previous.as_ref().filter(|previous| {
            previous.key.as_ref() == Some(&key) && previous.rope.is_instance(&rope)
        }) {
            return Ok(Document {
                path: path.clone(),
                revision: revision.clone(),
                rope: rope.clone(),
                key: Some(key),
                lines: previous.lines.clone(),
                commit_lines: previous.commit_lines.clone(),
                notice: previous.notice.clone(),
            });
        }
        let mut options = git2::BlameOptions::new();
        options.newest_commit(head.id());
        check_cancelled()?;
        let blame = repository
            .blame_file(&relative, Some(&mut options))
            .map_err(|e| format!("Could not load Git blame: {e}"))?;
        check_cancelled()?;
        let text = rope.to_string();
        // Map unchanged buffer lines to the pinned HEAD's exact line numbers.
        // libgit2's buffer blame can incorrectly shift authors when an insertion
        // and deletion cross commit boundaries. A bounded diff avoids that
        // mutation and also handles a completely empty editor safely.
        let original = std::str::from_utf8(blob.content())
            .map_err(|_| "Git blame is unavailable for this file's stored encoding.")?
            .trim_start_matches('\u{feff}')
            .replace("\r\n", "\n");
        let buffer = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        let old_lines: Vec<_> = original.split_inclusive('\n').collect();
        let new_lines: Vec<_> = buffer.split_inclusive('\n').collect();
        let diff = similar::TextDiff::configure()
            .timeout(std::time::Duration::from_millis(250))
            .diff_slices(&old_lines, &new_lines);
        let mut source_lines = vec![None; new_lines.len()];
        for op in diff
            .ops()
            .iter()
            .filter(|op| op.tag() == similar::DiffTag::Equal)
        {
            for (old, new) in op.old_range().zip(op.new_range()) {
                source_lines[new] = Some(old);
            }
        }
        let mut commits = FxHashMap::default();
        let mut lines = Vec::with_capacity(rope.len_lines());
        let mut commit_lines = Vec::with_capacity(rope.len_lines());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_secs() as i64);
        let mut git_line = 0;
        for line in 0..rope.len_lines() {
            check_cancelled()?;
            let source = rope.line(line);
            let original_line = source_lines.get(git_line).copied().flatten();
            // Map editor lines back to Git's LF-separated lines.
            // All editor rows within one Git line share its attribution.
            git_line += source
                .chunks()
                .map(|chunk| chunk.bytes().filter(|byte| *byte == b'\n').count())
                .sum::<usize>();
            if source.len_chars() == 0 {
                lines.push(Line::Empty);
                commit_lines.push(None);
                continue;
            }
            let Some(hunk) = original_line.and_then(|line| blame.get_line(line + 1)) else {
                lines.push(Line::Uncommitted);
                commit_lines.push(None);
                continue;
            };
            let id = hunk.final_commit_id();
            if id.is_zero() {
                lines.push(Line::Uncommitted);
                commit_lines.push(None);
                continue;
            }
            let commit = commits.entry(id).or_insert_with(|| {
                let signature = hunk.final_signature();
                let info = repository.find_commit(id).ok();
                let time = info
                    .as_ref()
                    .map_or_else(|| signature.when(), |commit| commit.time());
                let timestamp = time.seconds();
                let date = commit_date(time);
                let summary = info
                    .as_ref()
                    .and_then(|commit| commit.summary())
                    .map_or_else(|| "Untitled commit".into(), |text| clean(text, 256));
                let message = info
                    .as_ref()
                    .and_then(|commit| commit.message())
                    .map_or_else(String::new, |text| clean(text, usize::MAX));
                Arc::new(Commit {
                    hash: id.to_string(),
                    author: clean(signature.name().unwrap_or("Unknown"), 256),
                    email: clean(signature.email().unwrap_or(""), 256),
                    age: App::format_time_ago(now, timestamp),
                    date,
                    committed_at: format_commit_time(time, "%Y-%m-%d %H:%M:%S %:z"),
                    summary,
                    path: hunk.path().unwrap_or(&relative).to_path_buf(),
                    message,
                })
            });
            lines.push(Line::Committed(Arc::clone(commit)));
            commit_lines.push(original_line.map(|line| {
                hunk.orig_start_line().saturating_sub(1)
                    + (line + 1).saturating_sub(hunk.final_start_line())
            }));
        }
        Ok(Document {
            path: path.clone(),
            revision: revision.clone(),
            rope: rope.clone(),
            key: Some(key),
            lines,
            commit_lines,
            notice: None,
        })
    })();
    result.unwrap_or_else(|notice| Document {
        path,
        revision,
        rope,
        key: None,
        lines: Vec::new(),
        commit_lines: Vec::new(),
        notice: Some(notice),
    })
}

pub(crate) fn prepare_head(path: PathBuf, revision: Option<Arc<Revision>>, limit: u64) -> Result<Document, String> {
    let repo = git2::Repository::discover(revision.as_ref().map_or_else(|| path.parent().unwrap_or(&path), |revision| revision.workdir.as_path())).map_err(|error| error.to_string())?;
    let head = if let Some(revision) = &revision { repo.find_commit(git2::Oid::from_str(&revision.hash).map_err(|error| error.to_string())?) }
        else { repo.head().and_then(|head| head.peel_to_commit()) }.map_err(|error| error.to_string())?;
    let relative = revision.as_ref().map(|revision| revision.path.clone()).or_else(|| crate::app_git::repository_relative_path(repo.workdir()?, &path)).ok_or("File is outside the repository")?;
    let text = tree_text(&repo, &head.tree().map_err(|error| error.to_string())?, &relative, limit)?.ok_or("This file has no committed history yet")?;
    Ok(prepare_at(path, Rope::from_str(&text), None, revision))
}

pub(crate) fn cached<'a>(
    app: &'a App,
    editor: &crate::editor::Editor,
) -> Option<&'a Arc<Document>> {
    if !app.source_control_enabled { return None; }
    let document = app.blame_documents.get(&editor.navigation_id)?;
    (!editor.bg_loading
        && document.path.as_path() == editor.file_path.as_deref()?
        && document.revision == editor.blame.revision
        && document.rope.is_instance(&editor.rope))
    .then_some(document)
}

pub(crate) struct History {
    pub file_path: PathBuf,
    pub hash: String,
    pub summary: String,
    pub before_label: String,
    pub after_label: String,
    pub contents: Rope,
    pub line: usize,
    pub diff: crate::background::PreparedDiffRefresh,
    pub revision: Arc<Revision>,
    pub previous_revision: Option<Arc<Revision>>,
}

pub(crate) struct Snapshot {
    pub revision: Arc<Revision>,
    pub previous_revision: Option<Arc<Revision>>,
    pub previous_notice: Option<String>,
    pub document: Arc<Document>,
    pub line: usize,
}

fn tree_text(
    repo: &git2::Repository,
    tree: &git2::Tree<'_>,
    path: &std::path::Path,
    limit: u64,
) -> Result<Option<String>, String> {
    let entry = match tree.get_path(path) {
        Ok(entry) => entry,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let blob = repo
        .find_blob(entry.id())
        .map_err(|error| error.to_string())?;
    if blob.size() as u64 > limit {
        return Err("This revision exceeds the configured large-file limit.".into());
    }
    if blob.is_binary() {
        return Err(crate::app_ui_state::BINARY_DIFF_MESSAGE.into());
    }
    let text = std::str::from_utf8(blob.content())
        .map_err(|_| "This revision uses an unsupported text encoding.")?;
    Ok(Some(
        text.trim_start_matches('\u{feff}').replace("\r\n", "\n"),
    ))
}

pub(crate) fn prepare_history(
    file_path: PathBuf,
    info: &Commit,
    line: usize,
    limit: u64,
) -> Result<History, String> {
    prepare_history_at(file_path, info, line, limit, None)
}

pub(crate) fn revision_for(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
    path: PathBuf,
    working_file: PathBuf,
) -> Result<Arc<Revision>, String> {
    let syntax_path = repo
        .path()
        .join("vtcode-history")
        .join(commit.id().to_string())
        .join(&path);
    Ok(Arc::new(Revision {
        workdir: repo
            .workdir()
            .ok_or("This repository has no working directory.")?
            .to_path_buf(),
        path,
        working_file,
        hash: commit.id().to_string(),
        date: commit_date(commit.time()),
        summary: clean(commit.summary().unwrap_or("Untitled commit"), 256),
        syntax_path,
    }))
}

fn parent_revision(
    repo: &git2::Repository,
    revision: &Revision,
) -> Result<Option<Arc<Revision>>, String> {
    let commit = repo
        .find_commit(git2::Oid::from_str(&revision.hash).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if commit.parent_count() == 0 {
        return Ok(None);
    }
    let parent = commit.parent(0).map_err(
        |_| "The parent revision is unavailable in this repository; its history may be shallow.",
    )?;
    let tree = commit.tree().map_err(|e| e.to_string())?;
    let parent_tree = parent.tree().map_err(|e| e.to_string())?;
    let mut path = revision.path.clone();
    match parent_tree.get_path(&path) {
        Ok(_) => {}
        Err(error) if error.code() == git2::ErrorCode::NotFound => {
            let mut changes = repo
                .diff_tree_to_tree(Some(&parent_tree), Some(&tree), None)
                .map_err(|e| e.to_string())?;
            changes
                .find_similar(Some(git2::DiffFindOptions::new().renames(true)))
                .map_err(|e| e.to_string())?;
            let old = changes
                .deltas()
                .find(|delta| {
                    delta.new_file().path() == Some(&revision.path)
                        && delta.status() == git2::Delta::Renamed
                })
                .and_then(|delta| delta.old_file().path().map(std::path::Path::to_path_buf));
            let Some(old) = old else {
                return Ok(None);
            };
            path = old;
        }
        Err(error) => return Err(error.to_string()),
    }
    revision_for(repo, &parent, path, revision.working_file.clone()).map(Some)
}

pub(crate) fn prepare_snapshot(
    revision: Arc<Revision>,
    line: usize,
    limit: u64,
) -> Result<Snapshot, String> {
    let repo = git2::Repository::discover(&revision.workdir).map_err(|e| e.to_string())?;
    let commit = repo
        .find_commit(git2::Oid::from_str(&revision.hash).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let tree = commit.tree().map_err(|e| e.to_string())?;
    let text = tree_text(&repo, &tree, &revision.path, limit)?
        .ok_or("The file is absent from this revision.")?;
    let rope = Rope::from_str(&text);
    let line = line.min(rope.len_lines().saturating_sub(1));
    let document = Arc::new(prepare_at(
        revision.file_path(),
        rope,
        None,
        Some(revision.clone()),
    ));
    let (previous_revision, previous_notice) = match parent_revision(&repo, &revision) {
        Ok(previous) => (previous, None),
        Err(notice) => (None, Some(notice)),
    };
    Ok(Snapshot {
        revision,
        previous_revision,
        previous_notice,
        document,
        line,
    })
}

pub(crate) struct HistoryContents {
    pub file_path: PathBuf, pub hash: String, pub summary: String,
    pub before_label: String, pub after_label: String,
    pub before: String, pub after: String, pub line: usize,
    pub revision: Arc<Revision>, pub previous_revision: Option<Arc<Revision>>,
}

fn prepare_history_at(
    file_path: PathBuf,
    info: &Commit,
    line: usize,
    limit: u64,
    source: Option<&Revision>,
) -> Result<History, String> {
    read_history_at(file_path,info,line,limit,source).map(prepare_history_contents)
}

pub(crate) fn read_history_at(
    file_path: PathBuf,
    info: &Commit,
    line: usize,
    limit: u64,
    source: Option<&Revision>,
) -> Result<HistoryContents, String> {
    let repo = git2::Repository::discover(source.map_or_else(
        || file_path.parent().unwrap_or(&file_path),
        |revision| revision.workdir.as_path(),
    ))
    .map_err(|error| error.to_string())?;
    let id = git2::Oid::from_str(&info.hash).map_err(|error| error.to_string())?;
    let commit = repo.find_commit(id).map_err(|error| error.to_string())?;
    let tree = commit.tree().map_err(|error| error.to_string())?;
    let after = tree_text(&repo, &tree, &info.path, limit)?
        .ok_or("The file is absent from this commit.")?;
    let working_file = source.map_or_else(
        || file_path.clone(),
        |revision| revision.working_file.clone(),
    );
    let revision = revision_for(&repo, &commit, info.path.clone(), working_file)?;
    let previous_revision = parent_revision(&repo, &revision)?;
    let before = if let Some(previous) = &previous_revision {
        let parent = repo
            .find_commit(git2::Oid::from_str(&previous.hash).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        tree_text(
            &repo,
            &parent.tree().map_err(|e| e.to_string())?,
            &previous.path,
            limit,
        )?
    } else {
        None
    };
    let existed = before.is_some();
    let before = before.unwrap_or_default();
    let before_label = if existed {
        format!(
            "Previous version · {}",
            previous_revision.as_ref().unwrap().short_hash()
        )
    } else {
        "Previous version · file did not exist".into()
    };
    Ok(HistoryContents {
        file_path,
        hash: info.hash.clone(),
        summary: info.summary.clone(),
        before_label,
        after_label: format!(
            "{} · {} · {}",
            info.hash.chars().take(7).collect::<String>(),
            info.date,
            info.summary
        ),
        line, before, after,
        revision,
        previous_revision,
    })
}

pub(crate) fn prepare_history_contents(source: HistoryContents) -> History {
    let HistoryContents { file_path, hash, summary, before_label, after_label, before, after, line, revision, previous_revision } = source;
    let contents = Rope::from_str(&after);
    let line = git_row_to_line(&contents,line).min(contents.len_lines().saturating_sub(1));
    let diff_lines = crate::app::compute_file_diff(&before,&after);
    let render_cache = crate::app::DiffRenderCache::new(&diff_lines);
    let target_line_markers = crate::app_git_display::target_line_markers_from_compare_diff(&diff_lines,contents.len_lines());
    History { file_path, hash, summary, before_label, after_label, contents, line, revision, previous_revision,
        diff: crate::background::PreparedDiffRefresh { base_content: before.into(),
            diff_lines,
            render_cache,
            target_line_markers,
        },
    }
}

pub(crate) fn open_previous(app: &mut App, line: usize) {
    let Some(editor) = app.editor() else {
        return;
    };
    let Some(document) = cached(app, editor) else {
        return;
    };
    let Some(Line::Committed(commit)) = document.lines.get(line) else {
        return;
    };
    let commit = commit.clone();
    let path = document.path.clone();
    let revision = document.revision.clone();
    let source = editor.rope.clone();
    let original_line = document
        .commit_lines
        .get(line)
        .copied()
        .flatten()
        .unwrap_or(0);
    let navigation_id = editor.navigation_id;
    let view = &mut app.editor_mut().unwrap().blame;
    view.history_request = view.history_request.wrapping_add(1);
    view.history_loading = true;
    let generation = view.history_request;
    let limit = app.large_file_threshold_mb.saturating_mul(1024 * 1024);
    let sender = app.source_control_sender();
    app.notify("Opening previous version…");
    let remote = app.remote_workspace.clone();
    app.background.spawn("blame-history", move || {
        if sender.is_cancelled() { return; }
        let result = if let Some(remote) = remote {
            remote.blame_history(&path, &commit, original_line, limit, revision.as_deref()).map_err(|error| error.to_string())
        } else { match revision.as_deref() {
            Some(revision) => {
                prepare_history_at(path, &commit, original_line, limit, Some(revision))
            }
            None => prepare_history(path, &commit, original_line, limit),
        } }
        .map(|mut history| {
            history.line = (history.line + git_subrow(&source, line))
                .min(history.contents.len_lines().saturating_sub(1));
            history
        });
        let _ = sender.send(BackgroundResult::BlameHistory {
            navigation_id,
            generation,
            result,
        });
    });
}

pub(crate) fn open_line_file(app: &mut App, line: usize) {
    let Some(editor) = app.editor() else {
        return;
    };
    let Some(document) = cached(app, editor) else {
        return;
    };
    let Some(Line::Committed(commit)) = document.lines.get(line) else {
        return;
    };
    let commit = commit.clone();
    let path = document.path.clone();
    let source = document.revision.clone();
    let source_rope = editor.rope.clone();
    let source_line = line;
    let line = document
        .commit_lines
        .get(line)
        .copied()
        .flatten()
        .unwrap_or(0);
    let navigation_id = editor.navigation_id;
    let view = &mut app.editor_mut().unwrap().blame;
    view.history_request = view.history_request.wrapping_add(1);
    view.history_loading = true;
    let generation = view.history_request;
    let limit = app.large_file_threshold_mb.saturating_mul(1024 * 1024);
    let sender = app.source_control_sender();
    app.notify("Opening file at commit…");
    let remote = app.remote_workspace.clone();
    app.background.spawn("blame-file", move || {
        if sender.is_cancelled() { return; }
        let result = (|| {
            if let Some(remote) = remote {
                return remote.blame_line_snapshot(&path, &commit, source.as_deref(), line, limit).map_err(|error| error.to_string()).map(|mut snapshot| {
                    snapshot.line = (git_row_to_line(&snapshot.document.rope, line) + git_subrow(&source_rope,source_line)).min(snapshot.document.rope.len_lines().saturating_sub(1));
                    snapshot
                });
            }
            let repo = git2::Repository::discover(source.as_ref().map_or_else(
                || path.parent().unwrap_or(&path),
                |revision| revision.workdir.as_path(),
            ))
            .map_err(|e| e.to_string())?;
            let info = repo
                .find_commit(git2::Oid::from_str(&commit.hash).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let working_file = source
                .as_ref()
                .map_or(path, |revision| revision.working_file.clone());
            let revision = revision_for(&repo, &info, commit.path.clone(), working_file)?;
            prepare_snapshot(revision, line, limit).map(|mut snapshot| {
                snapshot.line = (git_row_to_line(&snapshot.document.rope, line)
                    + git_subrow(&source_rope, source_line))
                .min(snapshot.document.rope.len_lines().saturating_sub(1));
                snapshot
            })
        })();
        let _ = sender.send(BackgroundResult::BlameSnapshot {
            navigation_id,
            generation,
            result,
        });
    });
}

pub(crate) fn open_revision(app: &mut App, revision: Arc<Revision>, map_source: bool) {
    if !app.source_control_enabled { return; }
    let Some(editor) = app.editor() else {
        return;
    };
    let source = editor.rope.clone();
    let line = editor.cursor_line;
    let navigation_id = editor.navigation_id;
    let view = &mut app.editor_mut().unwrap().blame;
    view.history_request = view.history_request.wrapping_add(1);
    view.history_loading = true;
    let generation = view.history_request;
    let limit = app.large_file_threshold_mb.saturating_mul(1024 * 1024);
    let sender = app.source_control_sender();
    app.notify("Opening historical file…");
    let remote = app.remote_workspace.clone();
    app.background.spawn("blame-file", move || {
        if sender.is_cancelled() { return; }
        let result = match remote {
            Some(remote) => remote.blame_snapshot(&revision,line,limit).map_err(|error| error.to_string()),
            None => prepare_snapshot(revision,line,limit),
        }.map(|mut snapshot| {
            if map_source {
                snapshot.line = map_line(&source, &snapshot.document.rope, line);
            }
            snapshot
        });
        let _ = sender.send(BackgroundResult::BlameSnapshot {
            navigation_id,
            generation,
            result,
        });
    });
}

fn map_line(source: &Rope, target: &Rope, line: usize) -> usize {
    let old: Vec<_> = source.lines().map(|line| line.to_string()).collect();
    let new: Vec<_> = target.lines().map(|line| line.to_string()).collect();
    let old_lines: Vec<_> = old.iter().map(String::as_str).collect();
    let new_lines: Vec<_> = new.iter().map(String::as_str).collect();
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(250))
        .diff_slices(&old_lines, &new_lines);
    let result = diff
        .ops()
        .iter()
        .find(|op| op.old_range().contains(&line))
        .map_or(line, |op| {
            op.new_range().start
                + if op.tag() == similar::DiffTag::Equal {
                    line - op.old_range().start
                } else {
                    0
                }
        });
    result.min(target.len_lines().saturating_sub(1))
}

fn git_subrow(rope: &Rope, line: usize) -> usize {
    let mut start = line;
    while start > 0 {
        let previous = rope.line(start - 1);
        if previous.chars().last() == Some('\n') {
            break;
        }
        start -= 1;
    }
    line - start
}

fn git_row_to_line(rope: &Rope, git_line: usize) -> usize {
    if git_line == 0 {
        return 0;
    }
    let (mut bytes, mut lines) = (0, 0);
    for chunk in rope.chunks() {
        for (offset, byte) in chunk.bytes().enumerate() {
            if byte == b'\n' {
                lines += 1;
                if lines == git_line {
                    return rope.byte_to_line(bytes + offset + 1);
                }
            }
        }
        bytes += chunk.len();
    }
    rope.len_lines().saturating_sub(1)
}

pub(crate) fn finish_snapshot(
    app: &mut App,
    navigation_id: u64,
    generation: u64,
    result: Result<Snapshot, String>,
) -> bool {
    if !finish_request(app, navigation_id, generation) {
        return false;
    }
    match result {
        Ok(snapshot) => app.open_blame_file(snapshot),
        Err(message) => app.notify_error(format!("Could not open historical file: {message}")),
    }
    true
}

pub(crate) fn finish_history(
    app: &mut App,
    navigation_id: u64,
    generation: u64,
    result: Result<History, String>,
) -> bool {
    if !finish_request(app, navigation_id, generation) {
        return false;
    }
    match result {
        Ok(history) => app.open_blame_history(history),
        Err(message) => app.notify_error(format!("Could not open previous version: {message}")),
    }
    true
}

fn finish_request(app: &mut App, navigation_id: u64, generation: u64) -> bool {
    let editors = app.tabs.iter_mut().chain(
        app.groups
            .iter_mut()
            .enumerate()
            .filter(|(index, _)| *index != app.active_group)
            .flat_map(|(_, group)| &mut group.tabs),
    );
    let Some(tab) = editors.into_iter().find(|tab| {
        tab.editor.navigation_id == navigation_id && tab.editor.blame.history_request == generation
    }) else {
        return false;
    };
    tab.editor.blame.history_loading = false;
    // A late click result must never take focus away from another file or pane.
    if !app
        .editor()
        .is_some_and(|editor| editor.navigation_id == navigation_id)
    {
        return false;
    }
    app.active_pane == crate::app::ActivePane::Editor && !app.terminal_panel_focused
}

impl App {
    pub(crate) fn open_blame_file(&mut self, snapshot: Snapshot) {
        self.push_nav_history();
        if let Some(index) = self.tabs.iter().position(|tab| {
            tab.kind == TabKind::GitRevision
                && tab.editor.blame.revision.as_ref().is_some_and(|revision| {
                    revision.hash == snapshot.revision.hash
                        && revision.path == snapshot.revision.path
                        && revision.workdir == snapshot.revision.workdir
                })
        }) {
            self.active_tab = index;
            self.sync_diff_tab();
            self.clear_diff_state();
            self.editor_mut().unwrap().set_cursor(snapshot.line, 0);
        } else {
            self.suspend_diff_tab();
            self.clear_diff_state();
            let mut editor = self.new_editor();
            editor.language_override = self.language_override_for_path(&snapshot.revision.file_path())
                .or_else(|| self.language_override_for_path(&snapshot.revision.working_file));
            editor.file_path = Some(snapshot.revision.file_path());
            editor.file_size = snapshot.document.rope.len_bytes() as u64;
            editor.rope = snapshot.document.rope.clone();
            editor.saved_rope = Some(editor.rope.clone());
            editor.read_only = true;
            editor.syntax_dirty_from = Some(0);
            editor.syntax_dirty_through = Some(editor.rope.len_lines());
            editor.blame.enabled = snapshot.document.notice.is_none();
            editor.blame.revision = Some(snapshot.revision);
            editor.blame.previous_revision = snapshot.previous_revision;
            editor.blame.previous_notice = snapshot.previous_notice;
            editor.set_cursor(snapshot.line, 0);
            self.blame_documents
                .insert(editor.navigation_id, snapshot.document);
            let mut tab = crate::app::Tab::new_file(editor);
            tab.kind = TabKind::GitRevision;
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
        }
        self.active_pane = crate::app::ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.touch_tab_mru(self.active_tab);
    }
}

const INLINE_QUIET_PERIOD: Duration = Duration::from_secs(2);

fn pending_for_tab<'a>(
    app: &App,
    tab: &'a Tab,
    invalidated: bool,
    now: Instant,
) -> Option<(&'a crate::editor::Editor, Duration)> {
    let editor = &tab.editor;
    if !matches!(tab.kind, TabKind::File | TabKind::GitRevision)
        || editor.bg_loading
        || editor.encoding == "Binary"
        || editor.file_path.is_none()
        || (!invalidated && cached(app, editor).is_some())
    {
        return None;
    }
    let wait = if editor.blame.enabled {
        Duration::ZERO
    } else if !editor.large_file_mode
        && editor
            .blame
            .inline_candidate
            .is_some_and(|(_, line)| line == editor.cursor_line)
    {
        INLINE_QUIET_PERIOD.saturating_sub(now.saturating_duration_since(app.last_user_activity))
    } else {
        return None;
    };
    Some((editor, wait))
}

/// Only visible consumers request blame. An explicit rail takes priority over
/// automatic inline annotations, which wait for a quiet editor. Inspecting
/// demand never clones a rope, allocates a target list, or touches Git/the disk.
fn pending(app: &App, now: Instant) -> Option<(&crate::editor::Editor, Duration)> {
    if !app.source_control_enabled || app.bg_blame_running || crate::ui_inline_blame::obscured(app)
    {
        return None;
    }
    let mut target = app
        .tabs
        .get(app.active_tab)
        .and_then(|tab| pending_for_tab(app, tab, app.blame_requested_path.is_none(), now))
        .filter(|_| !app.should_render_diff_for_active_editor());
    if let Some(tree) = &app.split_tree {
        tree.visit_leaves(&mut |index| {
            if index == app.active_group {
                return;
            }
            let Some(group) = app.groups.get(index) else {
                return;
            };
            let Some(tab) = group.tabs.get(group.active_tab) else {
                return;
            };
            if let Some(candidate) = pending_for_tab(app, tab, false, now)
                && target.as_ref().is_none_or(|(editor, wait)| {
                    candidate.1 < *wait || (candidate.0.blame.enabled && !editor.blame.enabled)
                })
            {
                target = Some(candidate);
            }
        });
    }
    target
}

/// A single deadline for unmet visible demand. Loaded/hidden/unfocused views
/// and an in-flight worker have no timer, so idle sessions do not poll Git.
pub(crate) fn next_wait(app: &App, now: Instant) -> Option<Duration> {
    pending(app, now).map(|(_, wait)| wait)
}

pub(crate) fn refresh(app: &mut App) {
    // Restore the active cache alias when revisiting an unchanged file, without
    // requesting new analysis. Preserve explicit watcher invalidations (None).
    if app.blame_requested_path.is_some()
        && let Some(editor) = app.editor()
        && let Some(document) = cached(app, editor)
        && app.blame_requested_path.as_ref() != editor.file_path.as_ref()
    {
        let document = document.clone();
        app.blame_requested_path = Some(document.path.clone());
        app.blame_cache_key = document.key.clone();
        app.blame_cache = Some((document.path.clone(), document));
    }
    let Some((editor, wait)) = pending(app, Instant::now()) else {
        return;
    };
    if !wait.is_zero() {
        return;
    }
    let navigation_id = editor.navigation_id;
    let path = editor.file_path.clone().unwrap();
    let rope = editor.rope.clone();
    let large = editor.large_file_mode;
    let revision = editor.blame.revision.clone();
    if app
        .editor()
        .is_some_and(|editor| editor.navigation_id == navigation_id)
    {
        app.blame_requested_path = Some(path.clone());
    }
    let previous = app.blame_documents.get(&navigation_id).cloned();
    let sender = app.source_control_sender();
    app.bg_blame_running = true;
    let remote = app.remote_workspace.clone();
    let log = app.git_output();
    app.background.spawn("blame-cache", move || {
        if sender.is_cancelled() {
            return;
        }
        let operation = (remote.is_none() && !large)
            .then(|| crate::git_output::native(&log, "Read blame", &path));
        let document = if large {
            Document {
                path: path.clone(),
                revision,
                rope,
                key: None,
                lines: Vec::new(),
                commit_lines: Vec::new(),
                notice: Some("Git blame is paused in large-file mode.".into()),
            }
        } else if let Some(remote) = remote {
            remote.prepare_blame(path.clone(), rope, revision, sender.cancellation())
        } else {
            prepare_cancellable(
                path.clone(),
                rope,
                previous,
                revision,
                Some(sender.cancellation()),
            )
        };
        if let Some(operation) = &operation {
            if !sender.is_cancelled() {
                operation.completed(
                    document
                        .notice
                        .clone()
                        .unwrap_or_else(|| format!("{} lines prepared", document.lines.len())),
                );
            }
        }
        let _ = sender.send(BackgroundResult::BlameCache {
            navigation_id,
            file_path: path,
            document: Arc::new(document),
        });
    });
}
