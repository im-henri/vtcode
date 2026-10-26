//! Immutable metadata for a hosted journal. Hosts may cache this beside their
//! editor buffers; querying it never inspects files or waits on the journal.
use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct View {
    pub root: PathBuf,
    pub checkpoints: Vec<Checkpoint>,
    pub edits: Vec<Edit>,
    pub active: BTreeSet<u64>,
    pub undoable: BTreeSet<(String, String)>,
    pub interrupted: bool,
}

impl View {
    pub fn summary(&self, owner: &str, session: &str) -> Summary {
        let checkpoints: Vec<_> = self
            .checkpoints
            .iter()
            .filter(|c| c.owner == owner && c.session == session)
            .cloned()
            .collect();
        let ids: BTreeSet<_> = checkpoints.iter().map(|c| c.id).collect();
        Summary {
            root: self.root.clone(),
            edits: self
                .edits
                .iter()
                .filter(|e| ids.contains(&e.checkpoint))
                .cloned()
                .collect(),
            checkpoints,
            active: self.active.iter().find(|id| ids.contains(id)).copied(),
            interrupted: self.interrupted,
            can_undo: self
                .undoable
                .iter()
                .any(|(o, s)| o == owner && s == session),
        }
    }
}

impl Journal {
    pub fn version(&self) -> u64 {
        self.state.version
    }

    pub fn view(&self) -> View {
        View {
            root: self.root.clone(),
            checkpoints: self.state.checkpoints.clone(),
            edits: self.state.edits.clone(),
            active: self.state.active.iter().map(|a| a.checkpoint).collect(),
            undoable: self
                .state
                .completed
                .iter()
                .filter(|c| !c.undone)
                .map(|c| (c.plan.owner.clone(), c.plan.session.clone()))
                .collect(),
            interrupted: self.interrupted(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare(journal: &Journal) {
        let view: View =
            serde_json::from_slice(&serde_json::to_vec(&journal.view()).unwrap()).unwrap();
        for (owner, session) in [
            ("one", "chat"),
            ("two", "chat"),
            ("one", "other"),
            ("missing", ""),
        ] {
            let expected = journal.summary(owner, session);
            let actual = view.summary(owner, session);
            assert_eq!(actual.root, expected.root);
            assert_eq!(actual.active, expected.active);
            assert_eq!(actual.interrupted, expected.interrupted);
            assert_eq!(actual.can_undo, expected.can_undo);
            assert_eq!(
                serde_json::to_value(&actual.edits).unwrap(),
                serde_json::to_value(&expected.edits).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&actual.checkpoints).unwrap(),
                serde_json::to_value(&expected.checkpoints).unwrap()
            );
        }
    }
    #[test]
    fn immutable_journal_view_matches_scoped_summaries_across_capture_and_restore() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        std::fs::write(root.path().join("a.txt"), "before\n").unwrap();
        let mut journal = Journal::open(root.path(), data.path()).unwrap();
        compare(&journal);
        let first = journal
            .begin("one", "chat", "edit", BTreeSet::new())
            .unwrap();
        compare(&journal);
        std::fs::write(root.path().join("a.txt"), "after\n").unwrap();
        journal.finish_checkpoint(first, BTreeSet::new()).unwrap();
        let second = journal
            .begin("two", "chat", "other edit", BTreeSet::new())
            .unwrap();
        compare(&journal);
        std::fs::write(root.path().join("b.txt"), "new\n").unwrap();
        journal.finish_checkpoint(second, BTreeSet::new()).unwrap();
        let plan = journal.plan("one", "chat", &Selection::All).unwrap();
        journal.apply(plan).unwrap();
        compare(&journal);
        let undo = journal.undo_plan("one", "chat").unwrap();
        journal.apply(undo).unwrap();
        compare(&journal);
    }
}
