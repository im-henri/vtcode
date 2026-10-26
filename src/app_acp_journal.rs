//! Filesystem backend for the existing checkpoint worker. All methods which
//! inspect or mutate files run on that worker; presentation methods are local.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use vtcode::acp::rollback::{self as native, FileReview, Selection, Summary};

pub(super) enum Journal {
    Local(native::Journal),
    Remote(crate::remote::rollback::Journal),
}
pub(super) enum Plan {
    Local {
        plan: native::Plan,
        paths: Vec<PathBuf>,
    },
    Remote(crate::remote::rollback::Plan),
}
impl Plan {
    fn local(plan: native::Plan) -> Self {
        let paths = plan.paths().map(Path::to_path_buf).collect();
        Self::Local { plan, paths }
    }
    pub(super) fn paths(&self) -> impl Iterator<Item = &Path> {
        let paths = match self {
            Self::Local { paths, .. } => paths,
            Self::Remote(plan) => &plan.paths,
        };
        paths.iter().map(PathBuf::as_path)
    }
}
fn applied(paths: Vec<PathBuf>) -> Vec<(PathBuf, bool)> {
    paths
        .into_iter()
        .map(|path| {
            // Only a confirmed deletion closes an open tab. An unavailable
            // filesystem must leave its buffer intact for the reload error.
            let exists = !std::fs::metadata(&path).is_err_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                )
            });
            (path, exists)
        })
        .collect()
}
impl Journal {
    pub(super) fn open(root: &Path, directory: &Path) -> Result<Self, String> {
        native::Journal::open(root, directory).map(Self::Local)
    }
    pub(super) fn open_remote(
        workspace: crate::remote::workspace::Workspace,
    ) -> Result<Option<Self>, String> {
        crate::remote::rollback::Journal::open(workspace).map(|journal| journal.map(Self::Remote))
    }
    pub(super) fn workspace_root(&self) -> &Path {
        match self {
            Self::Local(journal) => journal.workspace_root(),
            Self::Remote(journal) => journal.workspace_root(),
        }
    }
    pub(super) fn active(&self) -> Option<u64> {
        match self {
            Self::Local(journal) => journal.active(),
            Self::Remote(journal) => journal.active(),
        }
    }
    pub(super) fn inline_reviews(&self) -> Vec<FileReview> {
        match self {
            Self::Local(journal) => journal.inline_reviews(),
            Self::Remote(journal) => journal.inline_reviews(),
        }
    }
    pub(super) fn summary(&self, owner: &str, session: &str) -> Summary {
        match self {
            Self::Local(journal) => journal.summary(owner, session),
            Self::Remote(journal) => journal.summary(owner, session),
        }
    }
    pub(super) fn begin(
        &mut self,
        owner: &str,
        session: &str,
        message: &str,
        protected: BTreeSet<PathBuf>,
    ) -> Result<u64, String> {
        match self {
            Self::Local(journal) => journal.begin(owner, session, message, protected),
            Self::Remote(journal) => journal.begin(owner, session, message, protected),
        }
    }
    #[cfg(test)]
    pub(super) fn finish(&mut self, protected: BTreeSet<PathBuf>) -> Result<(), String> {
        match self {
            Self::Local(journal) => journal.finish(protected),
            Self::Remote(journal) => {
                let checkpoint = journal.active().ok_or("No active checkpoint")?;
                journal.finish_checkpoint(checkpoint, protected)
            }
        }
    }
    pub(super) fn finish_checkpoint(
        &mut self,
        checkpoint: u64,
        protected: BTreeSet<PathBuf>,
    ) -> Result<(), String> {
        match self {
            Self::Local(journal) => journal.finish_checkpoint(checkpoint, protected),
            Self::Remote(journal) => journal.finish_checkpoint(checkpoint, protected),
        }
    }
    pub(super) fn capture_checkpoint(
        &mut self,
        checkpoint: u64,
        protected: BTreeSet<PathBuf>,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<(), String> {
        match self {
            Self::Local(journal) => journal.capture_checkpoint(checkpoint, protected, paths),
            Self::Remote(journal) => journal.capture_checkpoint(checkpoint, protected, paths),
        }
    }
    pub(super) fn finish_uncertain(&mut self) -> Result<(), String> {
        match self {
            Self::Local(journal) => journal.finish_uncertain(),
            Self::Remote(journal) => journal.finish_uncertain(),
        }
    }
    pub(super) fn accept_chunks(&mut self, chunks: &[(u64, usize)]) -> Result<(), String> {
        match self {
            Self::Local(journal) => journal.accept_chunks(chunks),
            Self::Remote(journal) => journal.accept_chunks(chunks),
        }
    }
    #[cfg(test)]
    pub(super) fn accept(
        &mut self,
        owner: &str,
        session: &str,
        selection: &Selection,
    ) -> Result<(), String> {
        match self {
            Self::Local(journal) => journal.accept(owner, session, selection),
            Self::Remote(journal) => {
                let chunks = journal.summary(owner, session).pending_chunks(selection)?;
                journal.accept_chunks(&chunks)
            }
        }
    }
    pub(super) fn plan(
        &mut self,
        owner: &str,
        session: &str,
        selection: &Selection,
    ) -> Result<Plan, String> {
        match self {
            Self::Local(journal) => journal.plan(owner, session, selection).map(Plan::local),
            Self::Remote(journal) => journal
                .plan(Some((owner, session)), selection)
                .map(Plan::Remote),
        }
    }
    pub(super) fn plan_editor(&mut self, selection: &Selection) -> Result<Plan, String> {
        match self {
            Self::Local(journal) => journal.plan_editor(selection).map(Plan::local),
            Self::Remote(journal) => journal.plan(None, selection).map(Plan::Remote),
        }
    }
    pub(super) fn undo_plan(&mut self, owner: &str, session: &str) -> Result<Plan, String> {
        match self {
            Self::Local(journal) => journal.undo_plan(owner, session).map(Plan::local),
            Self::Remote(journal) => journal.undo_plan(owner, session).map(Plan::Remote),
        }
    }
    pub(super) fn review(&mut self, id: u64) -> Result<(PathBuf, String, String), String> {
        match self {
            Self::Local(journal) => journal.review(id),
            Self::Remote(journal) => journal.review(id),
        }
    }
    pub(super) fn apply(&mut self, plan: Plan) -> Result<Vec<(PathBuf, bool)>, String> {
        match (self, plan) {
            (Self::Local(journal), Plan::Local { plan, .. }) => journal.apply(plan).map(applied),
            (Self::Remote(journal), Plan::Remote(plan)) => journal.apply(plan),
            _ => Err("Restore plan belongs to another filesystem".into()),
        }
    }
    pub(super) fn recover(&mut self) -> Result<Vec<(PathBuf, bool)>, String> {
        match self {
            Self::Local(journal) => journal.recover().map(applied),
            Self::Remote(journal) => journal.recover(),
        }
    }
}
