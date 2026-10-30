use crate::app::{App, GitBranchPickerItem};
use std::process::Command;

pub(crate) fn checkout_branch(repo: &git2::Repository, branch_name: &str) -> Result<(), String> {
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    let reference = format!("refs/heads/{branch_name}");
    let object = repo
        .revparse_single(&reference)
        .map_err(|error| error.to_string())?;
    repo.checkout_tree(&object, Some(&mut checkout))
        .map_err(|error| error.to_string())?;
    repo.set_head(&reference).map_err(|error| error.to_string())
}

impl App {

    fn run_git_remote_command_async(
        &mut self,
        worker_name: &'static str,
        op: &'static str,
        args: Vec<String>,
        push: bool,
    ) {
        if !self.source_control_enabled { return; }
        if self.repositories.discovered && self.repositories.active.is_none() {
            self.notify("Open a Git repository in Source Control first");
            return;
        }
        if self.bg_git_remote_running || self.bg_git_action_running {
            self.notify("Another git operation is already running");
            return;
        }
        self.bg_git_remote_running = true;
        let root = self.source_control_root().to_owned();
        let sender = self.background.sender.clone();
        let remote = self.remote_workspace.clone();
        let log = self.git_output();
        let generation = self.workspace_generation;
        self.background.spawn(worker_name, move || {
            let operation = remote.is_none().then(|| crate::git_output::native(&log,
                match op { "fetch" => "git fetch --all --prune", "pull" => "git pull", _ => "git push" }, &root));
            let result = match remote {
                Some(remote) => (|| -> std::io::Result<_> {
                    let repo = remote.remote_path(&root)?;
                    let crate::remote::Reply::GitNetwork(result) = remote.client.call(crate::remote::Command::GitNetwork { repo, args, push })? else { return Err(std::io::Error::other("Invalid remote Git network response")); };
                    Ok(result)
                })().unwrap_or_else(|error| crate::remote::git::NetworkResult { stdout: String::new(), stderr: error.to_string(), success: false }),
                None => crate::remote::git::network(&root,args,push),
            };
            if let Some(operation) = &operation { operation.network(&result); }
            let result = crate::background::BackgroundResult::GitRemoteOp { op,
                stdout: result.stdout, stderr: result.stderr, success: result.success };
            let _ = sender.send(crate::background::BackgroundResult::WorkspaceResult { generation, result: Box::new(result) });
        });
        self.notify(format!("Git {op} started..."));
    }

    pub(crate) fn git_fetch_async(&mut self) {
        self.run_git_remote_command_async(
            "git-fetch",
            "fetch",
            vec!["fetch".into(), "--all".into(), "--prune".into()], false,
        );
    }

    pub(crate) fn git_pull_async(&mut self) {
        self.run_git_remote_command_async("git-pull", "pull", vec!["pull".into()], false);
    }

    pub(crate) fn git_push_async(&mut self) {
        self.run_git_remote_command_async("git-push", "push", Vec::new(), true);
    }

    pub(crate) fn open_git_branch_picker(&mut self) {
        if !self.source_control_enabled { return; }
        if self.bg_git_branch_picker_running {
            return;
        }
        if self.bg_git_action_running || self.bg_git_remote_running {
            self.notify("Wait for the current Git operation to finish");
            return;
        }
        self.git_branch_dialog = crate::ui_git_branches::State::for_repository(self.source_control_root().to_owned());
        self.git_branch_picker_items.clear();
        self.git_branch_picker_query.clear();
        self.git_branch_picker_cursor = 0;
        self.git_branch_picker_selection_anchor = None;
        self.git_branch_picker_selected = 0;
        self.git_branch_picker_scroll_offset = 0;
        self.git_branch_picker_active = true;
        self.refresh_git_branch_async();
        self.bg_git_branch_picker_running = true;
        let root = self.source_control_root().to_owned();
        let sender = self.repository_sender();
        let remote = self.remote_workspace.clone();
        let log = self.git_output();
        self.background.spawn("git-branches", move || {
            if sender.is_cancelled() { return; }
            let operation = remote.is_none().then(|| crate::git_output::native(&log, "Read branches", &root));
            let result = match remote {
                Some(remote) => (|| -> std::io::Result<_> {
                    let repo = remote.remote_path(&root)?;
                    let crate::remote::Reply::GitBranches(branches) = remote.client.call(crate::remote::Command::GitBranches { repo })? else { return Err(std::io::Error::other("Invalid remote branches reply")); };
                    Ok(branches)
                })().map_err(|error| error.to_string()),
                None => branches(&root),
            };
            if let Some(operation) = &operation {
                operation.result(&result, |branches| format!("{} branches", branches.len()));
            }
            let _ = sender.send(crate::background::BackgroundResult::GitBranchesLoaded(
                result,
            ));
        });
    }

    pub(crate) fn git_branch_picker_total_rows(&self) -> usize {
        self.git_branch_picker_filtered_iter().count()
    }

    pub(crate) fn git_branch_picker_visible_rows(&self) -> usize {
        crate::ui_quick_picker::visible_rows(self, crate::ui_quick_picker::Kind::Branches)
    }

    pub(crate) fn sync_git_branch_picker_scroll(&mut self) {
        let total = self.git_branch_picker_total_rows();
        if total == 0 {
            self.git_branch_picker_selected = 0;
            self.git_branch_picker_scroll_offset = 0;
            return;
        }

        self.git_branch_picker_selected = self.git_branch_picker_selected.min(total - 1);
        let visible_rows = self.git_branch_picker_visible_rows().max(1);
        if self.git_branch_picker_selected < self.git_branch_picker_scroll_offset {
            self.git_branch_picker_scroll_offset = self.git_branch_picker_selected;
        } else if self.git_branch_picker_selected
            >= self.git_branch_picker_scroll_offset + visible_rows
        {
            self.git_branch_picker_scroll_offset =
                self.git_branch_picker_selected + 1 - visible_rows;
        }

        let max_scroll = total.saturating_sub(visible_rows);
        self.git_branch_picker_scroll_offset = self.git_branch_picker_scroll_offset.min(max_scroll);
    }

    pub(crate) fn git_branch_picker_filtered_items(&self) -> Vec<GitBranchPickerItem> {
        self.git_branch_picker_filtered_iter().cloned().collect()
    }

    pub(crate) fn git_branch_picker_filtered_iter(&self) -> impl Iterator<Item = &GitBranchPickerItem> {
        let unfiltered = self.git_branch_picker_query.trim().is_empty();
        self.git_branch_picker_items
            .iter()
            .filter(move |item| {
                // Ordinary name queries can use borrowed fields. The label
                // fallback preserves queries such as "local: feature".
                unfiltered || crate::app::contains_ignore_case(
                        item.target_branch_name(),
                        &self.git_branch_picker_query,
                    )
                    || item.remote_ref().is_some_and(|remote_ref| {
                        crate::app::contains_ignore_case(remote_ref, &self.git_branch_picker_query)
                    })
                    || crate::app::contains_ignore_case(&item.label(), &self.git_branch_picker_query)
            })
    }

    pub(crate) fn git_branch_picker_create_candidate(&self) -> Option<String> {
        let query = self.git_branch_picker_query.trim();
        if query.is_empty() {
            return None;
        }
        if self
            .git_branch_picker_items
            .iter()
            .any(|item| item.target_branch_name() == query || item.remote_ref() == Some(query))
        {
            return None;
        }
        Some(query.to_string())
    }

    pub(crate) fn checkout_git_branch_item(&mut self, item: GitBranchPickerItem) {
        match item {
            GitBranchPickerItem::Local { name } => self.checkout_local_branch(&name),
            GitBranchPickerItem::Remote {
                remote_ref,
                local_name,
            } => self.checkout_remote_branch(&remote_ref, &local_name),
        }
    }

    pub(crate) fn create_and_checkout_branch(&mut self, branch_name: &str) {
        let branch_name = branch_name.trim().to_string();
        if branch_name.is_empty() {
            self.notify("Branch name cannot be empty");
            return;
        }
        self.queue_git_task("git-create-branch", "create branch", crate::git_tasks::Task::CreateBranch { branch_name });
    }

    fn checkout_local_branch(&mut self, branch_name: &str) {
        let branch_name = branch_name.to_string();
        self.queue_git_task("git-checkout", "checkout", crate::git_tasks::Task::CheckoutLocal { branch_name });
    }

    fn checkout_remote_branch(&mut self, remote_ref: &str, local_name: &str) {
        let remote_ref = remote_ref.to_string();
        let local_name = local_name.to_string();
        self.queue_git_task("git-checkout-remote", "checkout", crate::git_tasks::Task::CheckoutRemote { remote_ref, local_name });
    }
}


pub(crate) fn branches(root: &std::path::Path) -> Result<Vec<GitBranchPickerItem>, String> {
    let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
    let current = repo.head().ok().filter(|head| head.is_branch())
        .and_then(|head| head.shorthand().map(str::to_owned));
    let mut items = Vec::new();
    if let Ok(branches) = repo.branches(Some(git2::BranchType::Local)) {
        for branch in branches.flatten() {
            if let Some(name) = branch.0.name().ok().flatten() {
                items.push(GitBranchPickerItem::Local {
                    name: name.to_string(),
                });
            }
        }
    }
    if let Ok(branches) = repo.branches(Some(git2::BranchType::Remote)) {
        for branch in branches.flatten() {
            let Some(remote_ref) = branch.0.name().ok().flatten() else {
                continue;
            };
            if remote_ref.ends_with("/HEAD") {
                continue;
            }
            let local_name = remote_ref
                .split_once('/')
                .map(|(_, suffix)| suffix)
                .unwrap_or(remote_ref)
                .to_string();
            items.push(GitBranchPickerItem::Remote {
                remote_ref: remote_ref.to_string(),
                local_name,
            });
        }
    }
    items.sort_by(|left, right| {
        let is_current = |item: &GitBranchPickerItem| matches!(item, GitBranchPickerItem::Local { name } if Some(name) == current.as_ref());
        let left_remote = matches!(left, GitBranchPickerItem::Remote { .. });
        let right_remote = matches!(right, GitBranchPickerItem::Remote { .. });
        is_current(right).cmp(&is_current(left)).then_with(|| left_remote
            .cmp(&right_remote)
            .then_with(|| left.label().cmp(&right.label())))
    });
    Ok(items)
}


pub(crate) fn current_local_branch_name(repo: &git2::Repository) -> Result<String, String> {
        let head = repo.head().map_err(|e| format!("Git HEAD error: {e}"))?;
        if !head.is_branch() {
            return Err("Detached HEAD is not supported for this action".into());
        }
        head.shorthand()
            .map(str::to_string)
            .ok_or_else(|| "Current branch name is unavailable".to_string())
    }


pub(crate) fn push_command(root: &std::path::Path) -> Result<(Command, Vec<String>), String> {
    let repo = git2::Repository::open(&root).map_err(|error| error.to_string())?;
    let branch_name = current_local_branch_name(&repo)?;
    let mut args = vec!["push".to_string()];
    let upstream_missing = repo
        .find_branch(&branch_name, git2::BranchType::Local)
        .ok()
        .and_then(|branch| branch.upstream().ok())
        .is_none();
    if upstream_missing {
        let remote_name = repo
            .remotes()
            .ok()
            .and_then(|remotes| {
                remotes
                    .iter()
                    .flatten()
                    .find(|name| !name.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "origin".to_string());
        args.extend(["-u".to_string(), remote_name, branch_name]);
    }
    Ok((crate::git_repositories::git_command_for(&repo)?, args))
}
