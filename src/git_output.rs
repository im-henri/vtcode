//! Git metadata is recorded on existing workers. Remote request tracing lives
//! entirely on the client: no new protocol traffic or headless logging work.
use crate::{
    app::App,
    remote::{
        Command,
        diagnostics::{Log, Operation},
    },
};
use std::path::Path;

pub(crate) fn is_git_command(command: &Command) -> bool {
    matches!(
        command,
        Command::GitStatus { .. }
            | Command::GitAction { .. }
            | Command::GitBranches { .. }
            | Command::GitNetwork { .. }
            | Command::GitSources { .. }
            | Command::GitDiffSources { .. }
            | Command::HunkSources { .. }
            | Command::HunkAction(_)
            | Command::CommitDetails { .. }
            | Command::CommitDiff { .. }
            | Command::Blame { .. }
            | Command::BlameHistory { .. }
            | Command::HistoryStart { .. }
            | Command::HistoryNext { .. }
    )
}

pub(crate) fn native(log: &Log, name: &'static str, path: &Path) -> Operation {
    let operation = log.operation("native Git", name, &path.to_string_lossy());
    operation.stage("Running");
    operation
}

impl App {
    pub(crate) fn git_output(&self) -> Log {
        let log = self.logs.git_log();
        if let Some(remote) = &self.remote_workspace {
            remote.client.set_git_output(&log);
        }
        log
    }
}

#[cfg(test)]
#[path = "git_output_tests.rs"]
mod tests;
