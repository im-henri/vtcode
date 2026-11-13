use crate::{
    app::{ActivePane, App, Tab, TabKind},
    background::{BackgroundResult, GitActionOutcome},
    git_tasks::Task,
};
use git2::{Commit, Error, ErrorCode, Reference, Repository, RepositoryState};
use ratatui::layout::Rect;
use ropey::Rope;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) const TITLE: &str = "Amend Commit";
const HEAD_CHANGED: &str = "HEAD changed. Abort and reopen Amend to edit the current commit.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Target {
    oid: String,
    reference: Vec<u8>,
}

impl Target {
    pub(crate) fn label(&self) -> String {
        let reference = self
            .reference
            .strip_prefix(b"refs/heads/")
            .unwrap_or(&self.reference);
        let branch = if reference == b"HEAD" {
            "Detached HEAD".into()
        } else {
            String::from_utf8_lossy(reference)
        };
        format!("{branch} · {}", self.oid.get(..7).unwrap_or(&self.oid))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CommitMessage {
    pub(crate) target: Target,
    pub(crate) text: String,
}

fn check_operation(repo: &Repository) -> Result<(), Error> {
    if !matches!(
        repo.state(),
        RepositoryState::Clean | RepositoryState::Bisect
    ) || crate::git_operation::in_progress(repo)
    {
        return Err(Error::from_str(
            "Finish the current Git operation before amending.",
        ));
    }
    Ok(())
}

fn head(repo: &Repository) -> Result<(Reference<'_>, Commit<'_>), Error> {
    let reference = repo.head().map_err(|error| match error.code() {
        ErrorCode::UnbornBranch | ErrorCode::NotFound => {
            Error::from_str("Create a commit before amending.")
        }
        _ => error,
    })?;
    let oid = reference
        .target()
        .ok_or_else(|| Error::from_str("HEAD does not resolve to a commit."))?;
    let commit = repo.find_commit(oid)?;
    Ok((reference, commit))
}

fn message_text(commit: &Commit<'_>) -> String {
    let bytes = commit.message_raw_bytes();
    if let Some(label) = commit.message_encoding() {
        if label.eq_ignore_ascii_case("ISO-8859-1") {
            return encoding_rs::mem::decode_latin1(bytes).into_owned();
        }
        if let Some(encoding) = encoding_rs::Encoding::for_label_no_replacement(label.as_bytes()) {
            return encoding.decode_without_bom_handling(bytes).0.into_owned();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn validate_message(message: &str) -> Result<(), &'static str> {
    if message.trim().is_empty() {
        return Err("Enter a commit message before amending.");
    }
    if message.contains('\0') {
        return Err("Remove NUL characters from the commit message.");
    }
    Ok(())
}

pub(crate) fn read_message(root: &Path) -> Result<CommitMessage, String> {
    let read = || -> Result<_, Error> {
        let repo = Repository::open(root)?;
        check_operation(&repo)?;
        let (reference, commit) = head(&repo)?;
        Ok(CommitMessage {
            target: Target {
                oid: commit.id().to_string(),
                reference: reference.name_bytes().to_vec(),
            },
            text: message_text(&commit),
        })
    };
    read().map_err(|error| error.message().to_owned())
}

pub(crate) fn execute(
    root: &Path,
    target: Target,
    message: Option<&str>,
) -> Result<GitActionOutcome, String> {
    let amend = || -> Result<_, Error> {
        let repo = Repository::open(root)?;
        // Keep the branch selection stable. For an attached HEAD, libgit2's
        // set_target also checks the branch's old object ID under its ref lock.
        let mut transaction = repo.transaction()?;
        transaction.lock_ref("HEAD")?;
        check_operation(&repo)?;
        let (mut reference, commit) = head(&repo)?;
        if reference.name_bytes() != target.reference || commit.id().to_string() != target.oid {
            return Err(Error::from_str(HEAD_CHANGED));
        }
        if let Some(message) = message {
            validate_message(message).map_err(Error::from_str)?;
        }
        let signature = repo
            .signature()
            .or_else(|_| git2::Signature::now("VTCode User", "user@vtcode.dev"))?;
        let tree = repo.find_tree(repo.index()?.write_tree()?)?;
        let encoding = message.and(commit.message_encoding()).map(|_| "UTF-8");
        let mut id = commit.amend(
            None,
            None,
            Some(&signature),
            encoding,
            message.or_else(|| commit.message_raw()),
            Some(&tree),
        )?;
        if message.is_none()
            && commit.message_raw().is_none()
            && commit.message_raw_bytes().starts_with(b"\n")
        {
            // libgit2's implicit message strips leading newlines. Preserve
            // them even when the raw message cannot be passed as a Rust str.
            let amended = repo.find_commit(id)?;
            let mut raw = amended.raw_header_bytes().to_vec();
            raw.push(b'\n');
            raw.extend_from_slice(commit.message_raw_bytes());
            id = repo.odb()?.write(git2::ObjectType::Commit, &raw)?;
        }
        let amended = repo.find_commit(id)?;
        let subject = amended.summary().unwrap_or("commit");
        let reflog = format!("commit (amend): {subject}");
        if reference.name_bytes() == b"HEAD" {
            transaction.set_target("HEAD", id, Some(&signature), &reflog)?;
            transaction.commit()?;
        } else {
            reference.set_target(id, &reflog).map_err(|error| {
                if error.code() == ErrorCode::Modified {
                    Error::from_str(HEAD_CHANGED)
                } else {
                    error
                }
            })?;
        }
        Ok(GitActionOutcome {
            message: format!("Amended: {subject}"),
            refresh_log: true,
            refresh_line_status: true,
            ..Default::default()
        })
    };
    amend().map_err(|error| error.message().to_owned())
}

pub(crate) struct Draft {
    root: PathBuf,
    pub(crate) repository: String,
    pub(crate) target: Option<Target>,
    pub(crate) submitting: bool,
    pub(crate) error: Option<String>,
    pub(crate) buttons: Option<Buttons>,
}

#[derive(Clone, Copy)]
pub(crate) struct Buttons {
    pub(crate) amend: Rect,
    pub(crate) abort: Rect,
}

impl App {
    pub(crate) fn amend_tab_is_active(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.kind == TabKind::AmendCommit)
    }

    fn amend_editor_mut(&mut self, view: u64) -> Option<&mut crate::editor::Editor> {
        let (group, index) = self.tab_location(view)?;
        let tab = if group == self.active_group {
            &mut self.tabs[index]
        } else {
            &mut self.groups[group].tabs[index]
        };
        Some(&mut tab.editor)
    }

    pub(crate) fn git_amend_commit(&mut self) {
        if !self.source_control_enabled {
            return;
        }
        if self.repositories.discovered && self.repositories.active.is_none() {
            self.notify("Open a Git repository in Source Control first");
            return;
        }
        let root = self.source_control_root().to_owned();
        let existing = self.amend_commits.iter().find_map(|(&id, draft)| {
            if Self::same_open_path(&draft.root, &root) {
                self.tab_location(id)
            } else {
                None
            }
        });
        if existing.is_none() && (self.bg_git_action_running || self.bg_git_remote_running) {
            self.notify("Another Git operation is already running");
            return;
        }
        self.record_navigation();
        self.auto_save_on_focus_change();
        self.suspend_diff_tab();
        self.clear_transient_editor_state();
        self.source_control_input_mode = false;
        self.search_input_focused = false;
        if let Some((group, index)) = existing {
            self.switch_group(group);
            self.active_tab = index;
        } else {
            let mut editor = self.new_editor();
            editor.suggested_file_name = Some(TITLE.into());
            editor.read_only = true;
            editor.word_wrap = true;
            let view = editor.navigation_id;
            let mut tab = Tab::new_file(editor);
            tab.kind = TabKind::AmendCommit;
            self.active_tab = self.insert_tab_after_active(tab);
            let remote = self.remote_workspace.clone();
            let repository = remote
                .as_ref()
                .and_then(|remote| remote.display_path(&root).ok())
                .unwrap_or_else(|| root.display().to_string());
            self.amend_commits.insert(
                view,
                Draft {
                    root: root.clone(),
                    repository,
                    target: None,
                    submitting: false,
                    error: None,
                    buttons: None,
                },
            );
            let sender = self.background.sender.clone();
            let log = self.git_output();
            self.background.spawn("git-amend-message", move || {
                let result = if let Some(remote) = remote {
                    remote
                        .git_amend_message(&root)
                        .map_err(|error| error.to_string())
                } else {
                    let operation = crate::git_output::native(&log, "Read amend message", &root);
                    let result = read_message(&root);
                    operation.result(&result, |_| "Commit message loaded".into());
                    result
                }
                .map(|message| (message.target, Rope::from_str(&message.text)));
                let _ = sender.send(BackgroundResult::AmendCommitMessage { view, result });
            });
        }
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.settings_editor_active = false;
        self.touch_tab_mru(self.active_tab);
        self.ensure_active_tab_visible();
        self.sync_diff_tab();
        self.record_navigation();
    }

    pub(crate) fn apply_amend_message(
        &mut self,
        view: u64,
        result: Result<(Target, Rope), String>,
    ) -> bool {
        let Some(draft) = self.amend_commits.get_mut(&view) else {
            return false;
        };
        match result {
            Ok((target, rope)) => {
                draft.target = Some(target);
                let Some(editor) = self.amend_editor_mut(view) else {
                    return false;
                };
                editor.saved_rope = Some(rope.clone());
                editor.cached_line_ending = crate::editor::rope_line_ending(&rope);
                editor.rope = rope;
                editor.content_version = editor.content_version.wrapping_add(1);
                editor.set_read_only(false);
            }
            Err(error) => draft.error = Some(error),
        }
        true
    }

    pub(crate) fn submit_amend(&mut self, view: u64) {
        let Some(draft) = self.amend_commits.get(&view) else {
            return;
        };
        let Some(target) = draft.target.clone().filter(|_| !draft.submitting) else {
            return;
        };
        if self.bg_git_action_running || self.bg_git_remote_running || !self.source_control_enabled
        {
            return;
        }
        let root = draft.root.clone();
        if self.pending_paste_document() == Some(view) {
            self.notify_warn("Wait for the current paste before amending.");
            return;
        }
        let Some(editor) = self.amend_editor_mut(view) else {
            return;
        };
        let message =
            (editor.saved_rope.as_ref() != Some(&editor.rope)).then(|| editor.rope.to_string());
        if let Some(text) = &message
            && let Err(error) = validate_message(text)
        {
            self.amend_commits.get_mut(&view).unwrap().error = Some(error.into());
            return;
        }
        if self.queue_git_task_at(root, "git-amend", "amend", Task::Amend { target, message }) {
            let draft = self.amend_commits.get_mut(&view).unwrap();
            draft.submitting = true;
            draft.error = None;
            if let Some(editor) = self.amend_editor_mut(view) {
                editor.set_read_only(true);
            }
        }
    }

    pub(crate) fn abort_amend(&mut self, view: u64) {
        if self
            .amend_commits
            .get(&view)
            .is_some_and(|draft| !draft.submitting)
        {
            self.force_close_tab_view(view);
        }
    }

    pub(crate) fn finish_amend(&mut self, result: &Result<GitActionOutcome, String>) {
        let Some((&view, draft)) = self
            .amend_commits
            .iter_mut()
            .find(|(_, draft)| draft.submitting)
        else {
            return;
        };
        match result {
            Ok(_) => self.force_close_tab_view(view),
            Err(error) => {
                draft.submitting = false;
                draft.error = Some(error.clone());
                if let Some(editor) = self.amend_editor_mut(view) {
                    editor.set_read_only(false);
                }
            }
        }
    }
}
