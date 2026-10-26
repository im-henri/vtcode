//! Filesystem-independent Git task descriptions. The same native implementation
//! runs on a local worker or on the headless service beside the repository.
use crate::{
    app::{App, GitFileChange},
    background::{BackgroundResult, GitActionOutcome},
    remote::{Command, Reply},
};
use std::path::Path;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum Task<P = String> {
    Stage(Vec<GitFileChange<P>>),
    Unstage(Vec<GitFileChange<P>>),
    Discard(Vec<GitFileChange<P>>),
    StageAll,
    UnstageAll,
    Init,
    Amend {
        message: String,
    },
    Stash {
        stash_message: String,
        label: String,
    },
    StashPop,
    CreateBranch {
        branch_name: String,
    },
    CheckoutLocal {
        branch_name: String,
    },
    CheckoutRemote {
        remote_ref: String,
        local_name: String,
    },
    Commit {
        message: String,
        stage_all: bool,
    },
}
impl<P> Task<P> {
    pub(crate) fn try_map_paths<Q, E>(
        self,
        mut convert: impl FnMut(P) -> Result<Q, E>,
    ) -> Result<Task<Q>, E> {
        let mut changes = |files: Vec<GitFileChange<P>>| {
            files
                .into_iter()
                .map(|file| file.try_map_paths(&mut convert))
                .collect::<Result<_, _>>()
        };
        Ok(match self {
            Self::Stage(files) => Task::Stage(changes(files)?),
            Self::Unstage(files) => Task::Unstage(changes(files)?),
            Self::Discard(files) => Task::Discard(changes(files)?),
            Self::StageAll => Task::StageAll,
            Self::UnstageAll => Task::UnstageAll,
            Self::Init => Task::Init,
            Self::Amend { message } => Task::Amend { message },
            Self::Stash {
                stash_message,
                label,
            } => Task::Stash {
                stash_message,
                label,
            },
            Self::StashPop => Task::StashPop,
            Self::CreateBranch { branch_name } => Task::CreateBranch { branch_name },
            Self::CheckoutLocal { branch_name } => Task::CheckoutLocal { branch_name },
            Self::CheckoutRemote {
                remote_ref,
                local_name,
            } => Task::CheckoutRemote {
                remote_ref,
                local_name,
            },
            Self::Commit { message, stage_all } => Task::Commit { message, stage_all },
        })
    }
}
pub(crate) fn execute<P: AsRef<Path>>(
    root: &Path,
    task: Task<P>,
) -> Result<GitActionOutcome, String> {
    match task {
        Task::Amend { message } => {
            let root = root.to_path_buf();
            let repo = git2::Repository::open(&root).map_err(|error| error.to_string())?;
            let sig = repo
                .signature()
                .or_else(|_| git2::Signature::now("VTCode User", "user@vtcode.dev"))
                .map_err(|error| error.to_string())?;
            let head_commit = repo
                .head()
                .and_then(|head| head.peel_to_commit())
                .map_err(|error| error.to_string())?;
            let mut index = repo.index().map_err(|error| error.to_string())?;
            let tree_oid = index.write_tree().map_err(|error| error.to_string())?;
            let tree = repo
                .find_tree(tree_oid)
                .map_err(|error| error.to_string())?;
            head_commit
                .amend(
                    Some("HEAD"),
                    Some(&sig),
                    Some(&sig),
                    None,
                    Some(&message),
                    Some(&tree),
                )
                .map_err(|error| error.to_string())?;
            Ok(crate::background::GitActionOutcome {
                message: format!("Amended: {message}"),
                refresh_log: true,
                clear_commit_message: Some(crate::app_git::GitCommitRequest { root, message }),
                leave_amend_mode: true,
                ..Default::default()
            })
        }
        Task::Stash {
            stash_message,
            label,
        } => {
            let root = root.to_path_buf();
            let mut repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
            let sig = repo
                .signature()
                .or_else(|_| git2::Signature::now("VTCode User", "user@vtcode.dev"))
                .map_err(|error| error.to_string())?;
            repo.stash_save(&sig, &stash_message, None)
                .map_err(|error| error.to_string())?;
            Ok(crate::background::GitActionOutcome {
                message: format!("Stashed: {label}"),
                refresh_explorer: true,
                refresh_line_status: true,
                reload_all_clean_files: true,
                ..Default::default()
            })
        }
        Task::StashPop => {
            let root = root.to_path_buf();
            let mut repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
            let mut options = git2::StashApplyOptions::new();
            repo.stash_pop(0, Some(&mut options))
                .map_err(|error| error.to_string())?;
            Ok(crate::background::GitActionOutcome {
                message: "Stash popped".to_string(),
                refresh_explorer: true,
                refresh_line_status: true,
                reload_all_clean_files: true,
                ..Default::default()
            })
        }
        Task::CreateBranch { branch_name } => {
            let root = root.to_path_buf();
            let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
            if repo
                .find_branch(&branch_name, git2::BranchType::Local)
                .is_ok()
            {
                return Err(format!("branch already exists: {branch_name}"));
            }
            let head_commit = repo
                .head()
                .and_then(|head| head.peel_to_commit())
                .map_err(|error| error.to_string())?;
            repo.branch(&branch_name, &head_commit, false)
                .map_err(|error| error.to_string())?;
            crate::app_git_branch::checkout_branch(&repo, &branch_name)?;
            Ok(crate::background::GitActionOutcome {
                message: format!("Switched to branch: {branch_name}"),
                refresh_explorer: true,
                refresh_log: true,
                refresh_line_status: true,
                reload_all_clean_files: true,
                close_branch_picker: true,
                ..Default::default()
            })
        }
        Task::CheckoutLocal { branch_name } => {
            let root = root.to_path_buf();
            let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
            repo.find_branch(&branch_name, git2::BranchType::Local)
                .map_err(|error| error.to_string())?;
            crate::app_git_branch::checkout_branch(&repo, &branch_name)?;
            Ok(crate::background::GitActionOutcome {
                message: format!("Switched to branch: {branch_name}"),
                refresh_explorer: true,
                refresh_log: true,
                refresh_line_status: true,
                reload_all_clean_files: true,
                close_branch_picker: true,
                ..Default::default()
            })
        }
        Task::CheckoutRemote {
            remote_ref,
            local_name,
        } => {
            let root = root.to_path_buf();
            let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
            let remote_branch = repo
                .find_branch(&remote_ref, git2::BranchType::Remote)
                .map_err(|error| error.to_string())?;
            let commit = remote_branch
                .get()
                .peel_to_commit()
                .map_err(|error| error.to_string())?;

            match repo.find_branch(&local_name, git2::BranchType::Local) {
                Ok(mut branch) => {
                    if branch.upstream().is_err() {
                        let _ = branch.set_upstream(Some(&remote_ref));
                    }
                }
                Err(_) => {
                    let mut branch = repo
                        .branch(&local_name, &commit, false)
                        .map_err(|error| error.to_string())?;
                    let _ = branch.set_upstream(Some(&remote_ref));
                }
            }

            crate::app_git_branch::checkout_branch(&repo, &local_name)?;
            Ok(crate::background::GitActionOutcome {
                message: format!("Switched to branch: {local_name}"),
                refresh_explorer: true,
                refresh_log: true,
                refresh_line_status: true,
                reload_all_clean_files: true,
                close_branch_picker: true,
                ..Default::default()
            })
        }
        Task::Stage(changes) => crate::app_git::changes::stage(root, &changes),
        Task::Unstage(changes) => crate::app_git::changes::unstage(root, &changes),
        Task::Discard(changes) => crate::app_git::changes::discard(root, &changes),
        Task::Commit { message, stage_all } => {
            crate::app_git::commit_repository(root, &message, stage_all)
        }
        Task::Init => {
            git2::Repository::init(root).map_err(|error| error.to_string())?;
            Ok(GitActionOutcome {
                message: "Initialized Git repository".into(),
                refresh_explorer: true,
                ..Default::default()
            })
        }
        Task::StageAll => {
            let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
            repo.add_ignore_rule(&format!("{}*/", crate::formatters::TEMP_DIR_PREFIX))
                .map_err(|error| error.to_string())?;
            let mut index = repo.index().map_err(|error| error.to_string())?;
            crate::app_git::changes::prepare_conflicts_for_stage_all(&repo, &mut index)?;
            index
                .add_all(std::iter::once("*"), git2::IndexAddOption::DEFAULT, None)
                .map_err(|error| error.to_string())?;
            index.write().map_err(|error| error.to_string())?;
            Ok(GitActionOutcome {
                message: "Staged all changes".into(),
                refresh_line_status: true,
                ..Default::default()
            })
        }
        Task::UnstageAll => {
            let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
            let mut index = repo.index().map_err(|error| error.to_string())?;
            match repo.head().and_then(|head| head.peel_to_tree()) {
                Ok(tree) => index.read_tree(&tree).map_err(|error| error.to_string())?,
                Err(error)
                    if matches!(
                        error.code(),
                        git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
                    ) =>
                {
                    index.clear().map_err(|error| error.to_string())?
                }
                Err(error) => return Err(error.to_string()),
            }
            index.write().map_err(|error| error.to_string())?;
            Ok(GitActionOutcome {
                message: "Unstaged all changes".into(),
                refresh_line_status: true,
                ..Default::default()
            })
        }
    }
}

impl App {
    pub(crate) fn queue_git_task(
        &mut self,
        worker: &'static str,
        action: &'static str,
        task: Task,
    ) {
        self.git_output();
        let Some(remote) = self.remote_workspace.clone() else {
            self.run_git_action_async(worker, action, move |root| execute(&root, task));
            return;
        };
        if !self.source_control_enabled {
            return;
        }
        if self.bg_git_action_running || self.bg_git_remote_running {
            self.notify("Another git operation is already running");
            return;
        }
        if !matches!(task, Task::Init)
            && self.repositories.discovered
            && self.repositories.active.is_none()
        {
            self.notify("Open a Git repository in Source Control first");
            return;
        }
        let key = self.source_control_root().to_owned();
        let repo = match remote.remote_path(&key) {
            Ok(repo) => repo,
            Err(error) => {
                self.notify_error(error.to_string());
                return;
            }
        };
        let task = match task
            .try_map_paths(|text| remote.remote_path(&key.join(text))?.relative_to(&repo))
        {
            Ok(task) => task,
            Err(error) => {
                self.notify_error(format!("Invalid remote Git path: {error}"));
                return;
            }
        };
        self.bg_git_action_running = true;
        self.status_message = format!("Git {action}...");
        let sender = self.background.sender.clone();
        self.background.save_for_file(&key.clone(), move || {
            let result = remote
                .client
                .call(Command::GitAction { repo, task })
                .and_then(|reply| {
                    let Reply::GitAction(mut outcome) = reply else {
                        return Err(std::io::Error::other("Invalid remote Git action response"));
                    };
                    if let Some(request) = &mut outcome.clear_commit_message {
                        request.root = key.clone();
                    }
                    if let Some(request) = &mut outcome.stage_all_commit_request {
                        request.root = key.clone();
                    }
                    Ok(outcome)
                })
                .map_err(|error| error.to_string());
            let _ = sender.send(BackgroundResult::GitActionDone { action, result });
        });
    }
}
