//! Demand-driven Git history. A sleeping worker retains its revwalk between
//! pages, avoiding both eager history loading and repeated offset scans.
use std::{path::PathBuf, sync::mpsc};

use crate::{
    app::{App, GitCommitInfo, GitCommitReference},
    background::{BackgroundResult, CancellationToken},
};

pub(crate) const PAGE_SIZE: usize = 200;
const PREFETCH_ROWS: usize = 20;

/// Stable lanes do not shift when a branch ends. Connections are prepared once
/// on the worker and continue across page boundaries.
#[derive(Default)]
struct GraphLanes(Vec<Option<git2::Oid>>);

impl GraphLanes {
    fn row(&mut self, id: git2::Oid, parents: impl Iterator<Item = git2::Oid>) -> String {
        let lane = self
            .0
            .iter()
            .position(|entry| *entry == Some(id))
            .unwrap_or_else(|| {
                if let Some(index) = self.0.iter().position(Option::is_none) {
                    index
                } else {
                    self.0.push(None);
                    self.0.len() - 1
                }
            });
        let before = self.0.clone();
        self.0[lane] = None;
        let mut targets = Vec::new();
        for parent in parents {
            let target = self
                .0
                .iter()
                .position(|entry| *entry == Some(parent))
                .unwrap_or_else(|| {
                    let index = if targets.is_empty() && self.0[lane].is_none() {
                        lane
                    } else if let Some(index) = self.0.iter().position(Option::is_none) {
                        index
                    } else {
                        self.0.push(None);
                        self.0.len() - 1
                    };
                    self.0[index] = Some(parent);
                    index
                });
            targets.push(target);
        }
        // Bitmask: up=1, down=2, left=4, right=8.
        let mut cells = vec![0u8; before.len().max(self.0.len()) * 2 - 1];
        for (index, entry) in before.iter().enumerate() {
            if entry.is_some() {
                cells[index * 2] |= 1;
            }
        }
        for (index, entry) in self.0.iter().enumerate() {
            if entry.is_some() {
                cells[index * 2] |= 2;
            }
        }
        for target in targets {
            let (start, end) = (lane.min(target) * 2, lane.max(target) * 2);
            for position in start..end {
                cells[position] |= 8;
                cells[position + 1] |= 4;
            }
        }
        let row = cells
            .into_iter()
            .enumerate()
            .map(|(index, bits)| {
                if index == lane * 2 {
                    return '●';
                }
                match bits {
                    1..=3 => '│',
                    4 | 8 | 12 => '─',
                    5 => '╯',
                    6 => '╮',
                    9 => '╰',
                    10 => '╭',
                    7 => '┤',
                    11 => '├',
                    13 => '┴',
                    14 => '┬',
                    15 => '┼',
                    _ => ' ',
                }
            })
            .collect();
        while self.0.last().is_some_and(Option::is_none) {
            self.0.pop();
        }
        row
    }
}

#[derive(Default)]
pub(crate) struct GitHistory {
    pub generation: u64,
    pub has_more: bool,
    pub error: Option<String>,
    control: Option<HistoryControl>,
}

struct HistoryControl {
    requests: mpsc::Sender<()>,
    cancel: CancellationToken,
}

impl Drop for HistoryControl {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) struct HistoryPage {
    pub root: PathBuf,
    pub generation: u64,
    pub offset: usize,
    pub result: Result<(Vec<GitCommitInfo>, bool), String>,
}

fn serve_history(
    root: PathBuf,
    generation: u64,
    requests: mpsc::Receiver<()>,
    cancel: CancellationToken,
    sender: crate::background::BackgroundResultSender,
    log: crate::remote::diagnostics::Log,
) {
    let view_root = root.clone();
    let mut operation = Some(crate::git_output::native(&log, "Read history", &root));
    stream_history(root, requests, cancel, move |offset, result| {
        if let Some(operation) = operation.take() {
            operation.result(&result, |(commits, more)| format!("{} commits; more={more}", commits.len()));
        }
        sender.send(BackgroundResult::GitLogPage(HistoryPage { root: view_root.clone(), generation, offset, result })).is_ok()
    });
}

pub(crate) fn stream_history(root: PathBuf, requests: mpsc::Receiver<()>, cancel: CancellationToken,
    mut publish: impl FnMut(usize, Result<(Vec<GitCommitInfo>, bool), String>) -> bool) {
    // Do not traverse/open repository metadata until the first page is wanted.
    if requests.recv().is_err() || cancel.is_cancelled() { return; }
    let mut offset = 0;
    let result = (|| -> Result<(), git2::Error> {
        let canonical = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let repo = git2::Repository::open(canonical)?;
        let mut refs: rustc_hash::FxHashMap<git2::Oid, Vec<GitCommitReference>> =
            Default::default();
        if let Ok(branches) = repo.branches(None) {
            for (branch, kind) in branches.flatten() {
                if let Ok(commit) = branch.get().peel_to_commit() {
                    refs.entry(commit.id())
                        .or_default()
                        .push(GitCommitReference {
                            name: branch.name().ok().flatten().unwrap_or("").to_owned(),
                            remote: kind == git2::BranchType::Remote,
                        });
                }
            }
        }
        let mut walk = repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        match repo.head() {
            Ok(head) => walk.push(head.peel_to_commit()?.id())?,
            Err(error)
                if matches!(
                    error.code(),
                    git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
                ) =>
            {
                // A revwalk with no starting commit may try HEAD again
                // on next(); finish an unborn branch explicitly instead.
                publish(0, Ok((Vec::new(), false)));
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        let mut walk = walk.peekable();
        let mut lanes = GraphLanes::default();
        let mut first = true;
        while (std::mem::take(&mut first) || requests.recv().is_ok()) && !cancel.is_cancelled() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64);
            let mut commits = Vec::with_capacity(PAGE_SIZE);
            for _ in 0..PAGE_SIZE {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let Some(oid) = walk.next() else {
                    break;
                };
                let oid = oid?;
                let commit = repo.find_commit(oid)?;
                let full_hash = oid.to_string();
                commits.push(GitCommitInfo {
                    graph: lanes.row(oid, commit.parent_ids()),
                    hash: full_hash[..7].to_owned(),
                    full_hash,
                    message: commit.summary().unwrap_or("").to_owned(),
                    author: commit.author().name().unwrap_or("").to_owned(),
                    time_ago: App::format_time_ago(now, commit.time().seconds()),
                    branches: refs.get(&oid).cloned().unwrap_or_default(),
                });
            }
            let has_more = walk.peek().is_some();
            let count = commits.len();
            if cancel.is_cancelled()
                || !publish(offset, Ok((commits, has_more)))
            {
                return Ok(());
            }
            offset += count;
            if !has_more {
                break;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        if !cancel.is_cancelled() {
            publish(offset, Err(error.to_string()));
        }
    }
}

impl App {
    fn invalidate_git_history(&mut self) {
        self.git_history.control = None;
        self.git_history.generation = self.git_history.generation.wrapping_add(1);
        self.git_history.has_more = false;
        self.git_history.error = None;
        self.bg_git_log_running = false;
    }

    pub(crate) fn suspend_remote_git_history(&mut self) -> bool {
        let was_requested = self.git_history.control.is_some();
        self.invalidate_git_history();
        was_requested
    }

    pub(crate) fn clear_git_history(&mut self) {
        self.invalidate_git_history();
        self.git_log.clear();
        self.git_log_scroll = 0;
        self.git_log_selected = 0;
    }

    pub(crate) fn refresh_git_log_async(&mut self) {
        if !self.source_control_enabled { return; }
        if self.repositories.discovered && self.repositories.active.is_none() { return; }
        self.invalidate_git_history();
        let root = self.source_control_root().to_owned();
        let generation = self.git_history.generation;
        let (requests, receiver) = mpsc::channel();
        let cancel = CancellationToken::new();
        self.git_history.control = Some(HistoryControl {
            requests,
            cancel: cancel.clone(),
        });
        self.git_history.has_more = true;
        let sender = self.background.sender.clone();
        let remote = self.remote_workspace.clone();
        let log = self.git_output();
        // This worker sleeps on its request channel between pages, so it must
        // not occupy the shared pool or keep the editor's idle loop awake.
        self.background.spawn_untracked("git-history", move || {
            match remote {
                Some(remote) => crate::remote::history::serve_local(remote, root, generation, receiver, cancel, sender),
                None => serve_history(root, generation, receiver, cancel, sender, log),
            }
        });
        self.load_more_git_history();
    }

    pub(crate) fn load_more_git_history(&mut self) {
        if self.bg_git_log_running || !self.git_history.has_more {
            return;
        }
        let Some(control) = self.git_history.control.as_ref() else {
            return;
        };
        if control.requests.send(()).is_ok() {
            self.bg_git_log_running = true;
        } else {
            self.git_history.has_more = false;
            self.git_history.error = Some("History loader stopped".into());
        }
    }

    pub(crate) fn prefetch_git_history(&mut self, visible_rows: usize) {
        if self
            .git_log_scroll
            .saturating_add(visible_rows)
            .saturating_add(PREFETCH_ROWS)
            >= self.git_log.len()
        {
            self.load_more_git_history();
        }
    }

    pub(crate) fn git_history_has_footer(&self) -> bool {
        self.git_history.has_more || self.bg_git_log_running || self.git_history.error.is_some()
    }

    pub(crate) fn apply_git_history_page(&mut self, page: HistoryPage) -> bool {
        if page.root != self.source_control_root() || page.generation != self.git_history.generation {
            return false;
        }
        if page.offset != 0 && page.offset != self.git_log.len() {
            return false;
        }
        self.bg_git_log_running = false;
        match page.result {
            Ok((commits, has_more)) => {
                if page.offset == 0 {
                    self.git_log = commits;
                    self.git_log_scroll = self
                        .git_log_scroll
                        .min(self.git_log.len().saturating_sub(1));
                    self.git_log_selected = self
                        .git_log_selected
                        .min(self.git_log.len().saturating_sub(1));
                } else {
                    self.git_log.extend(commits);
                }
                self.git_history.has_more = has_more;
                self.git_history.error = None;
                if !has_more {
                    self.git_history.control = None;
                }
            }
            Err(error) => {
                self.notify_error(format!("Could not load Git history: {error}"));
                self.git_history.error = Some(error);
                self.git_history.has_more = false;
                self.git_history.control = None;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn graph_lanes_connect_merges_and_keep_other_lanes_stable() {
        let id = |n| git2::Oid::from_str(&format!("{n:040x}")).unwrap();
        let mut lanes = GraphLanes::default();
        assert_eq!(lanes.row(id(5), [id(4), id(3)].into_iter()), "●─╮");
        assert_eq!(lanes.row(id(4), [id(2)].into_iter()), "● │");
        assert_eq!(lanes.row(id(3), [id(2)].into_iter()), "├─●");
        assert_eq!(lanes.row(id(2), [id(1)].into_iter()), "●");
        assert_eq!(lanes.row(id(1), [].into_iter()), "●");
        assert!(lanes.0.is_empty());
    }

    fn history_repo(count: usize) -> (PathBuf, Vec<String>) {
        let root = std::env::temp_dir().join(format!(
            "vtcode-history-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let repo = git2::Repository::init(&root).unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo
            .find_reference("HEAD")
            .unwrap()
            .symbolic_target()
            .unwrap()
            .to_owned();
        let mut ids = Vec::new();
        let mut parent = None;
        for index in 0..count {
            let signature = git2::Signature::new(
                "History Test",
                "history@example.com",
                &git2::Time::new(1_700_000_000 + index as i64, 0),
            )
            .unwrap();
            let parents = parent.iter().collect::<Vec<_>>();
            let id = repo
                .commit(
                    None,
                    &signature,
                    &signature,
                    &format!("Commit {index}"),
                    &tree,
                    &parents,
                )
                .unwrap();
            ids.push(id.to_string());
            parent = Some(repo.find_commit(id).unwrap());
        }
        // The graph needs all commits, but no test observes its intermediate
        // HEAD/reflog updates. Publish the completed fixture once.
        if let Some(parent) = parent {
            repo.reference(&head, parent.id(), false, "history fixture")
                .unwrap();
        }
        ids.reverse();
        (root, ids)
    }

    #[test]
    fn history_preserves_reference_kind_when_local_and_remote_names_match() {
        let (root, _) = history_repo(1);
        {
            let repo = git2::Repository::open(&root).unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("origin/main", &head, false).unwrap();
            repo.reference(
                "refs/remotes/origin/main",
                head.id(),
                false,
                "test remote reference",
            )
            .unwrap();
        }
        let mut app = App::new(root.clone());
        app.refresh_git_log_async();
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert_eq!(app.git_log.len(), 1);
        for remote in [false, true] {
            assert!(app.git_log[0].branches.contains(&GitCommitReference {
                name: "origin/main".into(),
                remote
            }));
        }
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_loads_all_pages_on_demand_without_duplicates_or_scroll_jumps() {
        // Two complete pages and a one-commit tail cover prefetch, the exact
        // boundaries, and exhaustion without manufacturing unused history.
        let (root, expected) = history_repo(PAGE_SIZE * 2 + 1);
        let mut app = App::new(root.clone());
        app.refresh_git_log_async();
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert_eq!(app.git_log.len(), PAGE_SIZE);
        assert!(app.git_history.has_more);
        assert!(app.git_history.control.is_some());
        app.prefetch_git_history(10);
        assert!(
            !app.bg_git_log_running,
            "top of graph must not load another page"
        );

        app.git_log_scroll = PAGE_SIZE - 25;
        app.git_log_selected = PAGE_SIZE - 20;
        app.prefetch_git_history(10);
        app.prefetch_git_history(10);
        assert!(app.bg_git_log_running);
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert_eq!(app.git_log.len(), PAGE_SIZE * 2);
        assert_eq!(app.git_log_scroll, PAGE_SIZE - 25);
        assert_eq!(app.git_log_selected, PAGE_SIZE - 20);
        app.git_log_scroll = PAGE_SIZE * 2 - 5;
        app.prefetch_git_history(10);
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert_eq!(
            app.git_log
                .iter()
                .map(|commit| commit.full_hash.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(!app.git_history.has_more);
        assert!(app.git_history.control.is_none());
        app.load_more_git_history();
        assert!(!app.bg_git_log_running);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_pages_include_both_sides_of_a_merge() {
        let (root, mut expected) = history_repo(PAGE_SIZE + 1);
        {
            let repo = git2::Repository::open(&root).unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            let tree = head.tree().unwrap();
            let mut side = repo
                .find_commit(git2::Oid::from_str(expected.last().unwrap()).unwrap())
                .unwrap();
            let signature = git2::Signature::now("Test", "test@example.com").unwrap();
            for index in 0..2 {
                let id = repo
                    .commit(
                        None,
                        &signature,
                        &signature,
                        &format!("Side {index}"),
                        &tree,
                        &[&side],
                    )
                    .unwrap();
                expected.push(id.to_string());
                side = repo.find_commit(id).unwrap();
            }
            let merge = repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    "Merge",
                    &tree,
                    &[&head, &side],
                )
                .unwrap();
            expected.push(merge.to_string());
        }
        let mut app = App::new(root.clone());
        app.refresh_git_log_async();
        loop {
            app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
            if !app.git_history.has_more {
                break;
            }
            app.load_more_git_history();
        }
        let mut actual = app
            .git_log
            .iter()
            .map(|commit| commit.full_hash.clone())
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refreshed_or_changed_workspaces_reject_stale_history_pages() {
        let (root, _) = history_repo(3);
        let (other, expected) = history_repo(5);
        let mut app = App::new(root.clone());
        app.refresh_git_log_async();
        let old_generation = app.git_history.generation;
        app.refresh_git_log_async();
        assert!(!app.apply_git_history_page(HistoryPage {
            root: root.clone(),
            generation: old_generation,
            offset: 0,
            result: Ok((Vec::new(), false)),
        }));
        assert!(app.bg_git_log_running);
        app.explorer.root = other.clone();
        app.clear_git_history();
        app.refresh_git_log_async();
        assert!(!app.apply_git_history_page(HistoryPage {
            root: root.clone(),
            generation: app.git_history.generation,
            offset: 0,
            result: Ok((Vec::new(), false)),
        }));
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert_eq!(
            app.git_log
                .iter()
                .map(|commit| commit.full_hash.clone())
                .collect::<Vec<_>>(),
            expected
        );
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn empty_history_finishes_without_another_page() {
        let (root, _) = history_repo(0);
        let mut app = App::new(root.clone());
        app.refresh_git_log_async();
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert!(app.git_log.is_empty());
        assert!(!app.git_history.has_more);
        assert!(
            app.git_history.error.is_none(),
            "{:?}",
            app.git_history.error
        );
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_page_boundary_does_not_offer_an_empty_extra_page() {
        let (root, _) = history_repo(PAGE_SIZE);
        let mut app = App::new(root.clone());
        app.refresh_git_log_async();
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_log_running);
        assert_eq!(app.git_log.len(), PAGE_SIZE);
        assert!(!app.git_history_has_footer());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
