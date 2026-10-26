use super::*;

const BEFORE: &str = "head\nremove\n\nend\n";
const AFTER: &str = "head\n\ninserted\nend\n";

fn legacy_state(f: &mut Fixture) -> State {
    f.write("a", BEFORE);
    f.begin();
    f.write("a", AFTER);
    f.finish();
    let mut state = f.journal.state.clone();
    state.edits[0].chunks = vec![
        Chunk {
            hunk: merge::Hunk {
                old: 5..12,
                new: 15..15,
            },
            decision: Decision::Accepted,
            line: 4,
            preview: "Deleted lines".into(),
        },
        Chunk {
            hunk: merge::Hunk {
                old: 12..12,
                new: 6..15,
            },
            decision: Decision::Pending,
            line: 3,
            preview: "inserted".into(),
        },
    ];
    state
}

fn backups(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("state-before-chunk-repair-")
        })
        .collect()
}

#[test]
fn legacy_chunk_repair_keeps_decisions_ids_history_and_undo_before_chat_continuation() {
    let mut f = Fixture::new();
    let state = legacy_state(&mut f);
    let checkpoint = state.checkpoints[0].id;
    let edit_id = state.edits[0].id;
    storage::atomic_json(&f.data.path().join("state.json"), &state).unwrap();
    let original = fs::read(f.data.path().join("state.json")).unwrap();
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);

    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    assert_eq!(fs::read_to_string(root.path().join("a")).unwrap(), AFTER);
    assert_eq!(journal.state.namespace, state.namespace);
    assert_eq!(journal.state.next, state.next);
    assert_eq!(journal.state.version, state.version + 1);
    assert_eq!(journal.state.checkpoints.len(), 1);
    assert_eq!(journal.state.checkpoints[0].id, checkpoint);
    assert_eq!(journal.state.edits[0].id, edit_id);
    assert_eq!(journal.state.edits[0].chunks.len(), 2);
    assert_eq!(
        journal.state.edits[0].chunks[0].decision,
        Decision::Accepted
    );
    assert_eq!(journal.state.edits[0].chunks[1].decision, Decision::Pending);
    assert_eq!(journal.state.edits[0].chunks[0].hunk.new, 5..5);
    assert_eq!(journal.state.edits[0].chunks[0].line, 2);
    assert_eq!(journal.state.edits[0].chunks[1].hunk.old, 13..13);
    let saved_backups = backups(data.path());
    assert_eq!(saved_backups.len(), 1);
    assert_eq!(fs::read(&saved_backups[0]).unwrap(), original);

    // The pending insertion is still independently reviewable, and an undo
    // retains both that chunk's identity and the already accepted deletion.
    let plan = journal
        .plan("agent", "chat", &Selection::Chunk(edit_id, 1))
        .unwrap();
    journal.apply(plan).unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "head\n\nend\n"
    );
    drop(journal);
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    assert_eq!(backups(data.path()), saved_backups);
    let undo = journal.undo_plan("agent", "chat").unwrap();
    journal.apply(undo).unwrap();
    assert_eq!(fs::read_to_string(root.path().join("a")).unwrap(), AFTER);
    assert_eq!(
        journal.state.edits[0].chunks[0].decision,
        Decision::Accepted
    );
    assert_eq!(journal.state.edits[0].chunks[1].decision, Decision::Pending);

    let continued = journal
        .begin("agent", "chat", "continue old chat", BTreeSet::new())
        .unwrap();
    journal
        .finish_checkpoint(continued, BTreeSet::new())
        .unwrap();
    let new_chat = journal
        .begin("agent", "new chat", "new message", BTreeSet::new())
        .unwrap();
    journal
        .finish_checkpoint(new_chat, BTreeSet::new())
        .unwrap();
    assert_eq!(journal.summary("agent", "chat").checkpoints.len(), 2);
    assert_eq!(journal.summary("agent", "new chat").checkpoints.len(), 1);
}

#[test]
fn unverified_chunk_repair_never_changes_metadata_or_working_files() {
    let mut f = Fixture::new();
    let baseline = legacy_state(&mut f);
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    for corruption in ["gap", "snapshot", "identity", "reference"] {
        let mut state = baseline.clone();
        match corruption {
            "gap" => state.edits[0].chunks[1].hunk.new.start += 1,
            "snapshot" => state.edits[0].before.as_mut().unwrap().blob.hash = "0".repeat(40),
            "identity" => state.checkpoints.push(state.checkpoints[0].clone()),
            "reference" => state.completed.push(Completed {
                plan: Plan {
                    version: state.version,
                    operation: state.next - 1,
                    owner: "agent".into(),
                    session: "chat".into(),
                    undo_of: None,
                    replacements: vec![],
                    decisions: vec![(state.edits[0].id, 2)],
                },
                previous: vec![],
                undone: false,
            }),
            _ => unreachable!(),
        }
        storage::atomic_json(&data.path().join("state.json"), &state).unwrap();
        let original = fs::read(data.path().join("state.json")).unwrap();
        assert!(
            Journal::open(root.path(), data.path()).is_err(),
            "{corruption}"
        );
        assert_eq!(fs::read(data.path().join("state.json")).unwrap(), original);
        assert_eq!(fs::read_to_string(root.path().join("a")).unwrap(), AFTER);
        assert!(backups(data.path()).is_empty());
    }
}
