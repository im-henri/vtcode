//! Rebase/cherry-pick state is read beside the repository, including on SSH.
use crate::background::GitActionOutcome;
use git2::{Repository, RepositoryState, Status};
use std::{io::Read, path::Path, process::Stdio};

#[cfg(test)]
#[path = "git_operation_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Kind {
    Rebase,
    CherryPick,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Rebase => "Rebase",
            Self::CherryPick => "Cherry-pick",
        }
    }
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
        }
    }
    pub(crate) fn continue_label(self) -> &'static str {
        match self {
            Self::Rebase => "Continue Rebase",
            Self::CherryPick => "Continue Cherry-pick",
        }
    }
    pub(crate) fn abort_label(self) -> &'static str {
        match self {
            Self::Rebase => "Abort Rebase",
            Self::CherryPick => "Abort Cherry-pick",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct State {
    pub kind: Kind,
    pub token: String,
    pub branch: String,
    pub current: String,
    pub step: Option<(usize, usize)>,
    pub blocker: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum Action {
    Start { kind: Kind, target: String },
    Continue { kind: Kind, token: String },
    Skip { kind: Kind, token: String },
    Abort { kind: Kind, token: String },
}

fn metadata(path: impl AsRef<Path>) -> String {
    let mut value = String::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(64 * 1024).read_to_string(&mut value);
    }
    value.trim().to_owned()
}

fn operation_metadata(repo: &Repository) -> Option<(Kind, Option<std::path::PathBuf>, String)> {
    let git = repo.path();
    let rebase = if git.join("rebase-merge").is_dir() {
        Some(git.join("rebase-merge"))
    } else if git.join("rebase-apply").is_dir() && !git.join("rebase-apply/applying").exists() {
        Some(git.join("rebase-apply"))
    } else {
        None
    };
    let cherry_head = metadata(git.join("CHERRY_PICK_HEAD"));
    let todo = metadata(git.join("sequencer/todo"));
    let kind = if rebase.is_some() {
        Kind::Rebase
    } else if !cherry_head.is_empty() || todo.lines().any(|line| line.starts_with("pick ")) {
        Kind::CherryPick
    } else {
        return None;
    };
    Some((kind, rebase, cherry_head))
}

pub(crate) fn in_progress(repo: &Repository) -> bool {
    operation_metadata(repo).is_some()
}

pub(crate) fn read(repo: &Repository) -> Result<Option<State>, String> {
    read_with_summary(repo, None)
}

/// Status refresh already read the worktree. Reuse that snapshot for the
/// operation banner instead of walking a large repository a second time.
pub(crate) fn read_with_changes<P>(repo: &Repository, files: &[crate::app::GitFileChange<P>]) -> Result<Option<State>, String> {
    let conflicts = files.iter().filter(|file| file.status == '!').count();
    let unstaged = files.iter().filter(|file| !file.staged && matches!(file.status, 'M' | 'D' | 'R' | 'A')).count();
    read_with_summary(repo, Some((conflicts, unstaged)))
}

fn read_with_summary(repo: &Repository, summary: Option<(usize, usize)>) -> Result<Option<State>, String> {
    let Some((kind, rebase, cherry_head)) = operation_metadata(repo) else { return Ok(None); };
    let git = repo.path();
    let head = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| error.to_string())?;
    let current_oid = if let Some(directory) = &rebase {
        let value = metadata(git.join("REBASE_HEAD"));
        if value.is_empty() {
            metadata(directory.join("original-commit"))
        } else {
            value
        }
    } else {
        cherry_head.clone()
    };
    let current_commit = current_oid
        .parse()
        .ok()
        .and_then(|oid| repo.find_commit(oid).ok());
    let current = current_commit
        .as_ref()
        .map(|commit| {
            format!(
                "{} {}",
                &commit.id().to_string()[..7],
                commit
                    .summary()
                    .unwrap_or("Commit")
                    .chars()
                    .take(200)
                    .collect::<String>()
            )
        })
        .unwrap_or_else(|| "Review resolved changes before continuing".into());
    let branch = rebase
        .as_ref()
        .map(|directory| metadata(directory.join("head-name")))
        .unwrap_or_else(|| {
            repo.head()
                .ok()
                .and_then(|head| head.shorthand().map(str::to_owned))
                .unwrap_or_default()
        });
    let branch = branch
        .strip_prefix("refs/heads/")
        .unwrap_or(&branch)
        .to_owned();
    let step = rebase.as_ref().and_then(|directory| {
        let number = |a, b| {
            metadata(directory.join(a))
                .parse::<usize>()
                .ok()
                .or_else(|| metadata(directory.join(b)).parse::<usize>().ok())
        };
        Some((number("msgnum", "next")?, number("end", "last")?))
    });
    let origin = rebase
        .as_ref()
        .map(|directory| metadata(directory.join("orig-head")))
        .unwrap_or_else(|| metadata(git.join("sequencer/head")));
    let anchor = rebase
        .as_ref()
        .map(|directory| directory.join("orig-head"))
        .unwrap_or_else(|| {
            if cherry_head.is_empty() {
                git.join("sequencer/head")
            } else {
                git.join("CHERRY_PICK_HEAD")
            }
        });
    let started = std::fs::metadata(anchor)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    let token = format!(
        "{}:{}:{origin}:{current_oid}:{step:?}:{started:?}",
        kind.command(),
        head.id()
    );
    let (conflicts, unstaged) = if let Some(summary) = summary { summary } else {
        let mut options = git2::StatusOptions::new();
        options.include_untracked(false).include_ignored(false);
        let statuses = repo.statuses(Some(&mut options)).map_err(|error| error.to_string())?;
        let conflicts = statuses.iter().filter(|entry| entry.status().contains(Status::CONFLICTED)).count();
        let unstaged = statuses.iter().filter(|entry| entry.status().intersects(
            Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_RENAMED | Status::WT_TYPECHANGE | Status::WT_UNREADABLE,
        )).count();
        (conflicts, unstaged)
    };
    let mut blocker = if conflicts > 0 {
        Some(format!(
            "Resolve and stage {conflicts} conflicted file{}.",
            if conflicts == 1 { "" } else { "s" }
        ))
    } else if unstaged > 0 {
        Some(format!(
            "Save and stage {unstaged} changed file{}.",
            if unstaged == 1 { "" } else { "s" }
        ))
    } else {
        None
    };
    if blocker.is_none() {
        let tree = head.tree().map_err(|error| error.to_string())?;
        let index = repo.index().map_err(|error| error.to_string())?;
        let diff = repo
            .diff_tree_to_index(Some(&tree), Some(&index), None)
            .map_err(|error| error.to_string())?;
        let mut markers = false;
        let original_tree = current_commit
            .as_ref()
            .and_then(|commit| commit.tree().ok());
        diff.print(git2::DiffFormat::Patch, |delta, _, line| {
            if line.origin() == '+'
                && crate::merge_conflicts::has_markers_in_reader(line.content()).unwrap_or(true)
            {
                // Commits can intentionally contain conflict examples. Only
                // reject markers introduced by the unresolved application.
                let original = original_tree
                    .as_ref()
                    .and_then(|tree| tree.get_path(delta.new_file().path()?).ok())
                    .and_then(|entry| repo.find_blob(entry.id()).ok());
                let existed = original.as_ref().is_some_and(|blob| {
                    blob.content()
                        .split_inclusive(|byte| *byte == b'\n')
                        .any(|original| original == line.content())
                });
                if !existed {
                    markers = true;
                }
            }
            true
        })
        .map_err(|error| error.to_string())?;
        if markers {
            blocker = Some("Remove conflict markers from staged changes.".into());
        } else if kind == Kind::CherryPick && !cherry_head.is_empty() && diff.deltas().len() == 0 {
            blocker = Some("No changes remain. Use Skip Commit.".into());
        }
    }
    Ok(Some(State {
        kind,
        token,
        branch,
        current,
        step,
        blocker,
    }))
}

pub(crate) fn execute(root: &Path, action: Action) -> Result<GitActionOutcome, String> {
    let repo = Repository::open(root).map_err(|error| error.to_string())?;
    let mut command = crate::git_repositories::git_command_for(&repo)?;
    command
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let kind = match &action {
        Action::Start { kind, target } => {
            if repo.state() != RepositoryState::Clean || read(&repo)?.is_some() {
                return Err("Finish or abort the current Git operation first.".into());
            }
            let mut options = git2::StatusOptions::new();
            options.include_untracked(false).include_ignored(false);
            if repo
                .statuses(Some(&mut options))
                .map_err(|error| error.to_string())?
                .iter()
                .any(|entry| entry.status() != Status::CURRENT)
            {
                return Err(
                    "Commit or stash tracked changes before starting this operation.".into(),
                );
            }
            if *kind == Kind::Rebase && repo.head_detached().map_err(|error| error.to_string())? {
                return Err("Switch to the branch you want to rebase first.".into());
            }
            let target = repo
                .revparse_single(target.trim())
                .and_then(|object| object.peel_to_commit())
                .map_err(|error| format!("Choose a valid branch or commit: {error}"))?;
            if *kind == Kind::CherryPick && target.parent_count() > 1 {
                return Err(
                    "This is a merge commit. Choose its mainline parent using Git in the terminal."
                        .into(),
                );
            }
            command.arg(kind.command());
            if *kind == Kind::Rebase {
                command.arg("--no-autostash");
            } else {
                command.arg("--no-edit");
            }
            // Resolve to an object ID before spawning Git; user input can never become an option.
            command.arg(target.id().to_string());
            *kind
        }
        Action::Continue { kind, token }
        | Action::Skip { kind, token }
        | Action::Abort { kind, token } => {
            let state = read(&repo)?
                .ok_or("There is no rebase or cherry-pick to continue. Refresh Source Control.")?;
            if state.kind != *kind || state.token != *token {
                return Err("The Git operation changed. Refresh Source Control and review its current state.".into());
            }
            if matches!(action, Action::Continue { .. })
                && let Some(reason) = state.blocker
            {
                return Err(reason);
            }
            command.arg(kind.command()).arg(match action {
                Action::Continue { .. } => "--continue",
                Action::Skip { .. } => "--skip",
                _ => "--abort",
            });
            *kind
        }
    };
    let output = command
        .output()
        .map_err(|error| format!("Could not run Git: {error}"))?;
    let state = read(&repo)?;
    if !output.status.success() && !state.as_ref().is_some_and(|state| state.blocker.is_some()) {
        let details = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        return Err(details.trim().chars().take(6000).collect());
    }
    let message = if let Some(state) = state {
        format!(
            "{} paused. {}",
            kind.name(),
            state
                .blocker
                .as_deref()
                .unwrap_or("Ready to continue in Source Control.")
        )
    } else if matches!(action, Action::Abort { .. }) {
        format!("{} aborted", kind.name())
    } else {
        format!("{} completed", kind.name())
    };
    Ok(GitActionOutcome {
        message,
        refresh_explorer: true,
        refresh_log: true,
        refresh_line_status: true,
        reload_all_clean_files: true,
        close_branch_picker: true,
        ..Default::default()
    })
}
