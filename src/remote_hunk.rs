//! Exact, asynchronous Git hunk operations. No repository I/O runs in the local
//! input loop, and an old preview cannot authorize applying a replacement hunk.
use super::*;
use crate::{app::App, app_git_display::parse_git_file_patch};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Sources {
    pub repository: RemotePath,
    pub revision: Revision,
    pub patch: String,
    pub index: String,
    pub worktree: String,
    pub trackable: bool,
}
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub repository: RemotePath,
    pub revision: Revision,
    pub trackable: bool,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Kind {
    Stage,
    Unstage,
    Discard,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Action {
    pub file: RemotePath,
    pub repository: RemotePath,
    pub revision: Revision,
    pub kind: Kind,
    pub header: String,
    pub hunk: crate::app_git_display::GitDiffHunk,
}
pub(crate) fn sources(
    root: &Path,
    file: RemotePath,
    repository: Option<RemotePath>,
    staged: bool,
) -> io::Result<Sources> {
    read_sources(root, file, repository, staged, None, Some(MAX_PAYLOAD_BYTES / 4))
}

pub(crate) fn native_sources(
    root: &Path,
    file: RemotePath,
    repository: Option<RemotePath>,
    staged: bool,
    live: Option<&ropey::Rope>,
) -> io::Result<Sources> {
    read_sources(root, file, repository, staged, live, None)
}

fn read_sources(
    root: &Path,
    file: RemotePath,
    repository: Option<RemotePath>,
    staged: bool,
    live: Option<&ropey::Rope>,
    max_bytes: Option<usize>,
) -> io::Result<Sources> {
    let native = file.resolve(root)?;
    let hint = repository
        .as_ref()
        .map(|path| path.resolve(root))
        .transpose()?;
    let repo = crate::git_repositories::open_file_repository(&native, hint.as_deref())
        .map_err(io::Error::other)?;
    let workdir = std::fs::canonicalize(
        repo.workdir()
            .ok_or_else(|| io::Error::other("Repository has no worktree"))?,
    )?;
    let relative = crate::app_git::repository_relative_path(&workdir, &native)
        .ok_or_else(|| io::Error::other("File is outside the repository"))?;
    if repo
        .status_file(&relative)
        .is_ok_and(|status| status.is_ignored())
    {
        return Err(io::Error::other("No changes found"));
    }
    let document = if let Some(rope) = live {
        service::Document { rope: rope.clone(), encoding: "UTF-8", revision: super::revision(rope, "UTF-8") }
    } else {
        service::read_document(&native)?.0
    };
    let index = crate::app_git::index_file_content(&repo, &relative).map_err(io::Error::other)?;
    let trackable = index.is_some()
        || repo
            .head()
            .and_then(|head| head.peel_to_tree())
            .and_then(|tree| tree.get_path(&relative))
            .is_ok();
    let index = index.unwrap_or_default().to_string();
    if max_bytes.is_some_and(|limit| document.rope.len_bytes().max(index.len()) > limit) {
        return Err(io::Error::other(
            "Hunk preview is unavailable for this large file; use the full diff",
        ));
    }
    let patch = if !staged && live.is_some() {
        // The live buffer supersedes the disk diff. It can outlive an external
        // deletion and must not be replaced with another disk read.
        String::new()
    } else {
        let mut command = crate::git_repositories::git_command(&workdir).map_err(io::Error::other)?;
        command.args([
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--inter-hunk-context=0",
            "--unified=0",
        ]);
        if staged {
            command.arg("--cached");
        }
        let output = command.arg("--").arg(&relative).output()?;
        if !output.status.success() {
            return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned()));
        }
        String::from_utf8(output.stdout).map_err(io::Error::other)?
    };
    Ok(Sources {
        repository: git::from_native(&workdir, root)?,
        revision: document.revision,
        patch,
        index,
        worktree: document.rope.to_string(),
        trackable,
    })
}

pub(crate) fn prepare(
    sources: Sources,
    staged: bool,
    live: Option<&ropey::Rope>,
    line: usize,
    tab_size: usize,
) -> Result<crate::ui_hunk_peek::HunkPeek, String> {
    let normalize = |text: String| {
        if text.contains("\r\n") {
            text.replace("\r\n", "\n")
        } else {
            text
        }
    };
    let index = normalize(sources.index);
    let worktree = normalize(live.map_or(sources.worktree, ropey::Rope::to_string));
    let mut peek =
        if !staged && (live.is_some() || (sources.patch.is_empty() && !sources.trackable)) {
            crate::ui_hunk_peek::HunkPeek::from_texts(&index, &worktree, line, tab_size)?
        } else {
            let patch = parse_git_file_patch(&sources.patch).ok_or("No changes found")?;
            let equal: Vec<_> = if staged {
                similar::TextDiff::from_lines(&index, &worktree)
                    .ops()
                    .iter()
                    .filter_map(|op| match *op {
                        similar::DiffOp::Equal {
                            old_index,
                            new_index,
                            len,
                        } => Some((old_index, new_index, len)),
                        _ => None,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mapped = equal
                .iter()
                .find_map(|&(old, new, len)| {
                    (line >= new && line < new + len).then(|| old + line - new)
                })
                .unwrap_or(line);
            let mut peek = crate::ui_hunk_peek::HunkPeek::for_line(
                &patch,
                if staged { &index } else { &worktree },
                mapped,
                tab_size,
            )?;
            if staged {
                for hunk in &mut peek.hunks {
                    hunk.action_line = hunk.action_line.and_then(|line| {
                        let range = equal.partition_point(|&(old, _, len)| old + len <= line);
                        equal.get(range).and_then(|&(old, new, _)| (line >= old).then(|| new + line - old))
                    });
                }
            }
            peek.hunks[peek.selected].action_line = Some(line);
            peek
        };
    peek.remote = Some(Snapshot {
        repository: sources.repository,
        revision: sources.revision,
        trackable: sources.trackable,
    });
    Ok(peek)
}

pub(crate) fn execute(
    root: &Path,
    action: Action,
) -> io::Result<crate::background::GitActionOutcome> {
    let staged = action.kind == Kind::Unstage;
    let current = sources(root, action.file, Some(action.repository), staged)?;
    let patch = parse_git_file_patch(&current.patch)
        .ok_or_else(|| io::Error::other("The hunk is no longer present"))?;
    if !current.trackable
        || current.revision != action.revision
        || patch.header != action.header
        || !patch.hunks.contains(&action.hunk)
    {
        return Err(io::Error::other(
            "The file or Git index changed since this preview. Reopen the hunk and try again.",
        ));
    }
    let mut args = vec![
        "apply".into(),
        "--whitespace=nowarn".into(),
        "--unidiff-zero".into(),
    ];
    if action.kind != Kind::Discard {
        args.push("--cached".into());
    }
    if action.kind != Kind::Stage {
        args.push("--reverse".into());
    }
    App::run_git_apply(
        &current.repository.resolve(root)?,
        &args,
        &format!("{}{}", patch.header, action.hunk.text),
    )
    .map_err(io::Error::other)?;
    Ok(crate::background::GitActionOutcome {
        message: match action.kind {
            Kind::Stage => "Staged hunk",
            Kind::Unstage => "Reverted staged hunk",
            Kind::Discard => "Discarded hunk",
        }
        .into(),
        refresh_line_status: true,
        reload_all_clean_files: action.kind == Kind::Discard,
        ..Default::default()
    })
}

impl App {
    pub(crate) fn remote_hunk_action(&mut self, path: &Path, _line: usize, kind: Kind) {
        self.git_output();
        if let Err(error) = self.ensure_hunk_actions_ready(path, kind == Kind::Discard) {
            self.notify_error(error);
            return;
        }
        let Some(remote) = self.remote_workspace.clone() else {
            return;
        };
        let action = (|| -> Result<Action, String> {
            let peek = self
                .hunk_peek
                .as_ref()
                .filter(|_| self.hunk_popup_active && self.hunk_popup_file.as_deref() == Some(path))
                .ok_or("Open the hunk preview before applying a hunk")?;
            let snapshot = peek
                .remote
                .as_ref()
                .ok_or("The hunk preview is still loading")?;
            if !snapshot.trackable {
                return Err("Hunk actions are unavailable for untracked files".into());
            }
            Ok(Action {
                file: remote
                    .remote_path(path)
                    .map_err(|error| error.to_string())?,
                repository: snapshot.repository.clone(),
                revision: snapshot.revision.clone(),
                kind,
                header: peek.source.header.clone(),
                hunk: peek
                    .source
                    .hunks
                    .get(peek.selected)
                    .ok_or("No hunk selected")?
                    .clone(),
            })
        })();
        let action = match action {
            Ok(action) => action,
            Err(error) => {
                self.notify_error(error);
                return;
            }
        };
        self.run_git_action_async("remote-git-hunk", "apply hunk", move |_| {
            match remote
                .client
                .call(Command::HunkAction(action))
                .map_err(|error| error.to_string())?
            {
                Reply::GitAction(outcome) => Ok(outcome),
                _ => Err("Invalid hunk action reply".into()),
            }
        });
    }
}
