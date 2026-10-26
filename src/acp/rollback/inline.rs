//! Cached editor previews. Snapshot and working-file I/O belongs on the worker;
//! projecting a preview onto an open buffer uses only these immutable strings.
use super::*;
use std::{ops::Range, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InlineChunk {
    pub edit: u64,
    pub index: usize,
    pub range: Option<Range<usize>>,
    pub original: Arc<str>,
    pub line: usize,
    pub issue: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileReview {
    pub path: PathBuf,
    pub text: Arc<str>,
    pub chunks: Vec<InlineChunk>,
}

impl FileReview {
    /// Disjoint user edits move the decorations. Overlapping edits invalidate
    /// their placement rather than highlighting unrelated text at a stale line.
    pub fn project(&self, current: &str) -> Vec<InlineChunk> {
        let changes = merge::hunks(&self.text, current);
        self.chunks
            .iter()
            .map(|chunk| {
                let mut chunk = chunk.clone();
                if let Some(range) = &chunk.range {
                    chunk.range = project_range(&self.text, range, current, &changes);
                    if chunk.range.is_none() {
                        chunk.issue = Some("Changed since AI edit".into());
                    }
                }
                chunk
            })
            .collect()
    }
}

fn project_range(
    base: &str,
    range: &Range<usize>,
    current: &str,
    changes: &[merge::Hunk],
) -> Option<Range<usize>> {
    if changes.iter().any(|change| {
        if range.is_empty() {
            range.start <= change.old.end && change.old.start <= range.end
        } else if change.old.is_empty() {
            range.start < change.old.start && change.old.start < range.end
        } else {
            range.start < change.old.end && change.old.start < range.end
        }
    }) {
        return None;
    }
    let shift: isize = changes
        .iter()
        .filter(|h| h.old.end <= range.start)
        .map(|h| h.new.len() as isize - h.old.len() as isize)
        .sum();
    let start = range.start.checked_add_signed(shift)?;
    let end = start.checked_add(range.len())?;
    (base.get(range.clone())? == current.get(start..end)?).then_some(start..end)
}

impl Journal {
    pub fn inline_reviews(&self) -> Vec<FileReview> {
        let mut files: BTreeMap<PathBuf, FileReview> = BTreeMap::new();
        for edit in self.state.edits.iter().filter(|e| e.pending() > 0) {
            let file = files.entry(edit.path.clone()).or_insert_with(|| {
                let text = storage::checked_path(&self.root, &edit.path)
                    .ok()
                    .and_then(|path| {
                        // Bound the preview independently of the file renderer.
                        if path.metadata().ok()?.len() > TEXT_LIMIT {
                            return None;
                        }
                        fs::read_to_string(path).ok()
                    })
                    .unwrap_or_default();
                FileReview {
                    path: self.root.join(&edit.path),
                    text: text.into(),
                    chunks: Vec::new(),
                }
            });
            let text = |state: &Option<FileState>| -> Option<String> {
                match state {
                    None => Some(String::new()),
                    Some(state) if state.blob.length <= TEXT_LIMIT => {
                        String::from_utf8(storage::bytes(&self.repository, &state.blob).ok()?)
                            .ok()
                            .filter(|s| !s.contains('\0'))
                    }
                    _ => None,
                }
            };
            let before = text(&edit.before);
            let after = text(&edit.after);
            let mut expected = after.clone();
            if !edit.whole_file
                && let (Some(before), Some(expected)) = (&before, &mut expected)
            {
                for chunk in edit
                    .chunks
                    .iter()
                    .rev()
                    .filter(|c| c.decision == Decision::Declined)
                {
                    expected.replace_range(chunk.hunk.new.clone(), &before[chunk.hunk.old.clone()]);
                }
            }
            let changes = expected
                .as_ref()
                .map(|s| merge::hunks(s, &file.text))
                .unwrap_or_default();
            let mut shift = 0isize;
            for (index, chunk) in edit.chunks.iter().enumerate() {
                if chunk.decision == Decision::Declined {
                    shift += chunk.hunk.old.len() as isize - chunk.hunk.new.len() as isize;
                    continue;
                }
                if chunk.decision != Decision::Pending {
                    continue;
                }
                let original: Arc<str> = before
                    .as_ref()
                    .and_then(|s| {
                        if edit.whole_file {
                            Some(s.as_str())
                        } else {
                            s.get(chunk.hunk.old.clone())
                        }
                    })
                    .unwrap_or("")
                    .into();
                let range = expected.as_ref().and_then(|expected| {
                    let range = if edit.whole_file {
                        0..expected.len()
                    } else {
                        let start = chunk.hunk.new.start.checked_add_signed(shift)?;
                        start..start + chunk.hunk.new.len()
                    };
                    project_range(expected, &range, &file.text, &changes)
                });
                let issue = edit.conflict.clone().or_else(|| {
                    range
                        .is_none()
                        .then(|| "Changed since AI edit; review before resolving".into())
                });
                file.chunks.push(InlineChunk {
                    edit: edit.id,
                    index,
                    range,
                    original,
                    line: chunk.line,
                    issue,
                });
            }
        }
        files.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_rebase_pending_chunks_after_rejection_and_disjoint_user_edits() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let path = root.path().join("a.txt");
        fs::write(&path, "first\nkeep\nlast\n").unwrap();
        let mut journal = Journal::open(root.path(), data.path()).unwrap();
        journal
            .begin("agent", "chat", "change", BTreeSet::new())
            .unwrap();
        fs::write(&path, "FIRST\nINSERTED\nkeep\nLAST\n").unwrap();
        journal.finish(BTreeSet::new()).unwrap();
        let id = journal.summary("agent", "chat").edits[0].id;
        let plan = journal.plan_editor(&Selection::Chunk(id, 0)).unwrap();
        journal.apply(plan).unwrap();
        fs::write(&path, "USER\nfirst\nkeep\nLAST\n").unwrap();
        let review = journal.inline_reviews().remove(0);
        assert_eq!(review.chunks.len(), 1);
        let chunk = &review.chunks[0];
        assert_eq!(&review.text[chunk.range.clone().unwrap()], "LAST\n");
        assert_eq!(&*chunk.original, "last\n");
        assert_eq!(chunk.index, 1);
        let current = format!("BUFFER\n{}", review.text);
        let projected = review.project(&current);
        assert_eq!(&current[projected[0].range.clone().unwrap()], "LAST\n");
        let changed = current.replace("LAST", "USER LAST");
        assert!(review.project(&changed)[0].range.is_none());
        let plan = journal
            .plan_editor(&Selection::File("a.txt".into()))
            .unwrap();
        journal.apply(plan).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "USER\nfirst\nkeep\nlast\n"
        );
        assert!(journal.inline_reviews().is_empty());
    }

    #[test]
    fn new_files_and_deletion_only_changes_have_reviewable_virtual_chunks() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let path = root.path().join("delete.txt");
        fs::write(&path, "kept\nremoved\n").unwrap();
        let mut journal = Journal::open(root.path(), data.path()).unwrap();
        journal
            .begin("agent", "chat", "change", BTreeSet::new())
            .unwrap();
        fs::write(&path, "kept\n").unwrap();
        fs::write(root.path().join("new.txt"), "new file\n").unwrap();
        journal.finish(BTreeSet::new()).unwrap();
        let files = journal.inline_reviews();
        assert_eq!(files.len(), 2);
        let deleted = files
            .iter()
            .find(|file| file.path.ends_with("delete.txt"))
            .unwrap();
        assert_eq!(deleted.chunks[0].range, Some(5..5));
        assert_eq!(&*deleted.chunks[0].original, "removed\n");
        let created = files
            .iter()
            .find(|file| file.path.ends_with("new.txt"))
            .unwrap();
        assert_eq!(created.chunks[0].range, Some(0..9));
        assert!(created.chunks[0].original.is_empty());
    }
}
