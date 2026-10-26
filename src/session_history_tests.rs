use super::*;
use crate::editor::{Editor, UndoSnapshot};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

fn snapshot(rope: Rope, index: usize) -> UndoSnapshot {
    (
        rope,
        index,
        index + 1,
        vec![(index, 2)],
        vec![Some((index, 1))],
        Some((0, index)),
        index % 3 != 0,
    )
}

fn metadata(entries: &[UndoSnapshot]) -> Vec<SavedUndoEntry> {
    entries
        .iter()
        .map(|entry| SavedUndoEntry {
            content_bytes: Some(entry.0.len_bytes()),
            cursor_line: entry.1,
            cursor_col: entry.2,
            extra_cursors: entry.3.clone(),
            extra_cursor_anchors: entry.4.clone(),
            selection_anchor: entry.5,
            modified: entry.6,
        })
        .collect()
}

fn encode(undo: &[UndoSnapshot], redo: &[UndoSnapshot]) -> (Vec<u8>, [u8; 32]) {
    let mut bytes = Vec::new();
    let digest = write(&mut bytes, undo, redo).unwrap();
    assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&bytes)));
    (bytes, digest)
}

fn roundtrip(undo: &[UndoSnapshot], redo: &[UndoSnapshot]) -> (UndoStack, UndoStack) {
    let (bytes, digest) = encode(undo, redo);
    let restored = read(Cursor::new(bytes), metadata(undo), metadata(redo), digest).unwrap();
    assert_eq!(restored.0, undo);
    assert_eq!(restored.1, redo);
    restored
}

fn leaf_bytes<'a>(ropes: impl Iterator<Item = &'a Rope>) -> usize {
    let mut unique = HashMap::new();
    for rope in ropes {
        for chunk in rope.chunks() {
            unique.insert((chunk.as_ptr() as usize, chunk.len()), chunk.len());
        }
    }
    unique.values().sum()
}

#[test]
fn histories_roundtrip_empty_unicode_crlf_and_legacy_separator_text() {
    let texts = [
        "",
        "é😀\r\n",
        "before\n\x00\x01UNDO_SEP\x01\x00\nafter",
        "\r",
        "\r\n",
        "界",
        "",
    ];
    let entries: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| snapshot(Rope::from_str(text), i))
        .collect();
    for split in 0..=entries.len() {
        let undo = &entries[..split];
        let redo: Vec<_> = entries[split..].iter().rev().cloned().collect();
        roundtrip(undo, &redo);
    }
    roundtrip(&[], &[]);
}

#[test]
fn identical_nonadjacent_ropes_share_identity_without_losing_snapshot_metadata() {
    let rope = Rope::from_str(&"line 🦀\n".repeat(10_000));
    let entries = [
        snapshot(rope.clone(), 1),
        snapshot("other".into(), 2),
        snapshot(rope.clone(), 3),
    ];
    let (bytes, _) = encode(&entries, &[]);
    assert!(bytes.len() < rope.len_bytes() + 100);
    let (restored, _) = roundtrip(&entries, &[]);
    assert!(restored[0].0.is_instance(&restored[2].0));
    assert_ne!(restored[0].1, restored[2].1);
    assert_ne!(restored[0].6, restored[2].6);
}

#[test]
fn shared_recovery_base_roundtrips_undo_redo_and_reuses_current_rope() {
    let base = Rope::from_str(&"line é🦀\r\n".repeat(10_000));
    let mut before = base.clone();
    before.remove(5..6);
    let mut after = base.clone();
    after.insert(10, "new");
    let states = [snapshot(before, 1), snapshot(base.clone(), 2), snapshot(after, 3)];
    for split in 0..=states.len() {
        let undo = &states[..split];
        let redo: Vec<_> = states[split..].iter().rev().cloned().collect();
        let mut bytes = Vec::new();
        let digest = write_with_base(&mut bytes, undo, &redo, Some(&base)).unwrap();
        assert!(bytes.len() < 1024, "only changes, not a second document: {}", bytes.len());
        let (restored_undo, restored_redo) = read_with_base(Cursor::new(&bytes), metadata(undo),
            metadata(&redo), digest, Some(base.clone())).unwrap();
        assert_eq!(restored_undo, undo);
        assert_eq!(restored_redo, redo);
        assert!(restored_undo.iter().chain(&restored_redo)
            .find(|entry| entry.1 == 2).unwrap().0.is_instance(&base));
        assert!(read(Cursor::new(&bytes), metadata(undo), metadata(&redo), digest).is_err(),
            "shared-base histories must never be decoded without their base");
    }
    for base in [Rope::new(), "é\r\n🦀".into()] {
        let entries = [snapshot(base.clone(), 1), snapshot(Rope::new(), 2)];
        let mut bytes = Vec::new();
        let digest = write_with_base(&mut bytes, &entries, &[], Some(&base)).unwrap();
        let (restored, _) = read_with_base(Cursor::new(bytes), metadata(&entries), vec![], digest, Some(base)).unwrap();
        assert_eq!(restored, entries);
    }
}

fn local_history(states: usize) -> Vec<UndoSnapshot> {
    let mut rope = Rope::from_str(&"fn café() { let value = 123; }\r\n".repeat(32_000));
    let mut entries = Vec::new();
    for i in 0..states {
        entries.push(snapshot(rope.clone(), i));
        // Distant edits must not turn into a document-sized replacement.
        rope.insert(200 + i, "界");
        rope.insert(rope.len_chars() - 200 - i, "🦀");
    }
    entries
}

#[test]
fn distant_multicursor_edits_restore_shared_text_and_small_payloads() {
    let entries = local_history(50);
    let undo = &entries[..25];
    let redo: Vec<_> = entries[25..].iter().rev().cloned().collect();
    let changes = edits(&entries[0].0, &entries[1].0);
    assert!(changes.len() >= 2, "distant changes must stay separate");
    let (bytes, digest) = encode(undo, &redo);
    assert!(bytes.len() < entries[0].0.len_bytes() * 2);
    let (restored_undo, restored_redo) =
        read(Cursor::new(bytes), metadata(undo), metadata(&redo), digest).unwrap();
    assert_eq!(restored_undo, undo);
    assert_eq!(restored_redo, redo);
    let unique = leaf_bytes(
        restored_undo
            .iter()
            .chain(&restored_redo)
            .map(|entry| &entry.0),
    );
    assert!(
        unique < entries[0].0.len_bytes() * 3,
        "{unique} retained text bytes"
    );
}

#[test]
fn repeated_shared_chunks_reordered_regions_and_independent_ropes_roundtrip() {
    let block = Rope::from_str(&"repeat café\n".repeat(500));
    let mut rope = block.clone();
    rope.append(block.clone());
    rope.append(block.clone());
    let mut entries = vec![snapshot(rope.clone(), 0)];
    for position in [2, block.len_chars() + 3, rope.len_chars() - 5] {
        rope.insert(position, "界");
        entries.push(snapshot(rope.clone(), entries.len()));
    }
    let tail = rope.split_off(block.len_chars());
    let mut moved = tail;
    moved.append(rope);
    entries.push(snapshot(moved.clone(), entries.len()));
    moved.remove(3..moved.len_chars() - 3);
    entries.push(snapshot(moved, entries.len()));
    for text in ["é😀\r\n", "ê🦀\r\n", "ê🦀\n", "é🦀\n", "e\u{301}\n", ""] {
        entries.push(snapshot(Rope::from_str(text), entries.len()));
    }
    roundtrip(&entries, &[]);
}

#[test]
fn local_changes_and_equal_independent_ropes_need_no_document_sized_index() {
    let old = Rope::from_str(&"é🦀\r\n".repeat(100_000));
    let mut new = old.clone();
    let at = old.len_chars() / 2;
    new.insert(at, "界");
    assert_eq!(edits(&old, &new), [(at..at, at..at + 1)]);
    let independent = Rope::from_str(&old.to_string());
    assert!(edits(&old, &independent).is_empty());
    new.remove(at..at + 1);
    assert!(edits(&old, &new).is_empty());
}

#[test]
fn sampled_anchors_bound_storage_and_keep_distant_unicode_edits_small() {
    let old = Rope::from_str(&"é🦀\r\n".repeat(1_000_000));
    let anchors = chunk_anchors(old.slice(..));
    assert!(old.chunks().count() > MAX_CHUNK_ANCHORS);
    let positions: usize = anchors.values().map(Positions::len).sum();
    assert!(positions <= MAX_CHUNK_ANCHORS, "{positions} anchors");
    let index_bytes = anchors.capacity() * std::mem::size_of::<((usize, usize), Positions)>()
        + anchors
            .values()
            .map(|positions| positions.rest.capacity() * std::mem::size_of::<PositionRun>())
            .sum::<usize>();
    assert!(
        index_bytes < 1024 * 1024,
        "{index_bytes} bytes of anchor storage"
    );
    let mut new = old.clone();
    new.insert(new.len_chars() - 100, "界");
    new.insert(100, "λ");
    let changes = edits(&old, &new);
    assert_eq!(changes.len(), 2);
    let mut replayed = old.clone();
    for (removed, inserted) in changes.iter().rev() {
        replayed.remove(removed.clone());
        replayed.insert(removed.start, &new.slice(inserted.clone()).to_string());
    }
    assert_eq!(replayed, new);
    assert_eq!(
        changes
            .iter()
            .map(|(_, inserted)| new.slice(inserted.clone()).len_bytes())
            .sum::<usize>(),
        5
    );
    roundtrip(&[snapshot(old, 0), snapshot(new, 1)], &[]);
}

#[test]
fn sampling_repeated_shared_leaves_does_not_turn_small_edits_into_large_payloads() {
    let block = Rope::from_str(&"é🦀\r\n".repeat(2048));
    let mut rope = Rope::new();
    for _ in 0..512 {
        rope.append(block.clone());
    }
    assert!(rope.chunks().count() > MAX_CHUNK_ANCHORS);
    assert!(
        chunk_anchors(rope.slice(..))
            .values()
            .any(|positions| positions.len() > 1)
    );
    let mut snapshots = vec![snapshot(rope.clone(), 0)];
    for step in 1..=4 {
        rope.insert(rope.len_chars() - 100 * step, "界");
        rope.insert(100 * step, "λ");
        snapshots.push(snapshot(rope.clone(), step));
        let changes = edits(&snapshots[step - 1].0, &rope);
        let bytes: usize = changes
            .iter()
            .map(|(_, range)| rope.slice(range.clone()).len_bytes())
            .sum();
        assert!(
            bytes < 32 * 1024,
            "{bytes} replacement bytes for two tiny edits"
        );
    }
    let (bytes, _) = encode(&snapshots, &[]);
    assert!(bytes.len() < snapshots[0].0.len_bytes() + 128 * 1024);
    roundtrip(
        &snapshots[..3],
        &snapshots[3..].iter().rev().cloned().collect::<Vec<_>>(),
    );
}

#[test]
fn editing_undo_redo_after_restore_matches_uninterrupted_editor() {
    let mut editor = Editor::test_with_text(&"fn café() {\r\n    work();\r\n}\r\n".repeat(100));
    editor.record_saved_snapshot(editor.rope.clone());
    editor.set_cursor(1, 4);
    editor.extra_cursors = vec![(100, 2)];
    editor.extra_cursor_anchors = vec![None];
    editor.paste_plain_with_text("λ😀\r\n");
    editor.set_cursor(10, 2);
    editor.selection_anchor = Some((12, 1));
    editor.insert_char_replacing_selection(' ');
    editor.set_cursor(15, 4);
    editor.insert_char('界');
    editor.undo();
    let (undo, redo) = roundtrip(&editor.export_undo_stack(), &editor.export_redo_stack());
    let mut restored = Editor::test_with_text(&editor.rope.to_string());
    restored.saved_rope = editor.saved_rope.clone();
    restored.modified = editor.modified;
    restored.cursor_line = editor.cursor_line;
    restored.cursor_col = editor.cursor_col;
    restored.selection_anchor = editor.selection_anchor;
    restored.extra_cursors = editor.extra_cursors.clone();
    restored.extra_cursor_anchors = editor.extra_cursor_anchors.clone();
    restored.import_undo_redo(undo, redo);
    for redo in [true, false, false, false, false, true, true, true] {
        if redo {
            editor.redo();
            restored.redo();
        } else {
            editor.undo();
            restored.undo();
        }
        assert_eq!(restored.rope, editor.rope);
        assert_eq!(restored.modified, editor.modified);
        assert_eq!(
            (restored.cursor_line, restored.cursor_col),
            (editor.cursor_line, editor.cursor_col)
        );
        assert_eq!(restored.selection_anchor, editor.selection_anchor);
        assert_eq!(restored.extra_cursors, editor.extra_cursors);
        assert_eq!(restored.extra_cursor_anchors, editor.extra_cursor_anchors);
    }
    editor.paste_plain_with_text("new branch 🦀");
    restored.paste_plain_with_text("new branch 🦀");
    assert_eq!(restored.rope, editor.rope);
    restored.undo();
    editor.undo();
    assert_eq!(restored.rope, editor.rope);
}

#[test]
fn regular_alias_positions_stay_inline_and_irregular_runs_keep_exact_lookup() {
    let mut regular = Positions::default();
    for index in 0..1_000_000 {
        regular.push(index * 1024);
    }
    assert_eq!(regular.len(), 1_000_000);
    assert_eq!(regular.rest.capacity(), 0);
    assert_eq!(
        regular.at_or_after(500_000 * 1024 + 1),
        Some(500_001 * 1024)
    );
    assert_eq!(regular.at_or_after(999_999 * 1024 + 1), None);

    let mut positions = Positions::default();
    let mut reference = Vec::new();
    let mut offset = 0;
    for index in 0..1000 {
        offset += (index / 7) % 13 + 1;
        positions.push(offset);
        reference.push(offset);
    }
    assert_eq!(positions.len(), reference.len());
    for minimum in 0..=offset + 1 {
        let expected = reference
            .get(reference.partition_point(|&position| position < minimum))
            .copied();
        assert_eq!(
            positions.at_or_after(minimum),
            expected,
            "minimum={minimum}"
        );
    }
}

#[test]
fn sampled_aliases_replay_random_mixed_unicode_edits_without_large_replacements() {
    let block = Rope::from_str(&"é🦀\r\n".repeat(2048));
    let mut old = Rope::new();
    for _ in 0..512 {
        old.append(block.clone());
    }
    let mut seed = 0x87354f1cu64;
    for step in 0..100 {
        let mut new = old.clone();
        for operation in 0..6 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let at = (seed >> 32) as usize % new.len_chars();
            let end = (at + (seed % 200) as usize).min(new.len_chars());
            match operation % 3 {
                0 => {
                    new.remove(at..end);
                    new.insert(at, "界λ🦀\r\n");
                }
                1 => new.insert(at, "new content\r\n"),
                _ => new.insert(at, "é\n"),
            }
        }
        let changes = edits(&old, &new);
        let bytes: usize = changes
            .iter()
            .map(|(_, range)| new.slice(range.clone()).len_bytes())
            .sum();
        assert!(bytes < 64 * 1024, "step {step}: {bytes} replacement bytes");
        let mut replayed = old.clone();
        for (removed, inserted) in changes.iter().rev() {
            replayed.remove(removed.clone());
            replayed.insert(removed.start, &new.slice(inserted.clone()).to_string());
        }
        assert_eq!(replayed, new, "step {step}");
        old = new;
    }
}

#[test]
fn randomized_unicode_history_roundtrips_and_keeps_order() {
    let mut random = 0x76ca_fe51_u64;
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random as usize
    };
    let texts = ["", "界", "😀", "é", "\r", "\n", "\r\n", "a\u{301}", "\0"];
    let mut rope = Rope::from_str(&"initial text\r\n".repeat(300));
    let mut entries = Vec::new();
    for i in 0..100 {
        entries.push(snapshot(rope.clone(), i));
        let start = next() % (rope.len_chars() + 1);
        let end = (start + next() % 30).min(rope.len_chars());
        rope.remove(start..end);
        rope.insert(start, texts[next() % texts.len()]);
        if i % 13 == 0 {
            rope = Rope::from_str(&rope.to_string());
        }
    }
    for split in [0, 1, 49, 50, 99, 100] {
        let redo: Vec<_> = entries[split..].iter().rev().cloned().collect();
        roundtrip(&entries[..split], &redo);
    }
}

#[test]
fn corrupt_truncated_appended_and_wrong_digest_histories_are_rejected() {
    let entries = [
        snapshot("text é😀\n".into(), 0),
        snapshot("new text é😀\n".into(), 1),
    ];
    let (bytes, digest) = encode(&entries, &[]);
    for length in 0..bytes.len() {
        assert!(
            read(
                Cursor::new(&bytes[..length]),
                metadata(&entries),
                vec![],
                digest
            )
            .is_err()
        );
    }
    for index in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[index] ^= 1;
        assert!(
            read(Cursor::new(damaged), metadata(&entries), vec![], digest).is_err(),
            "byte {index}"
        );
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(read(Cursor::new(extra), metadata(&entries), vec![], digest).is_err());
    assert!(read(Cursor::new(&bytes), metadata(&entries), vec![], [0; 32]).is_err());
    let mut wrong = metadata(&entries);
    wrong[1].content_bytes = Some(0);
    assert!(read(Cursor::new(&bytes), wrong, vec![], digest).is_err());
    assert!(read(Cursor::new(&bytes), metadata(&entries[..1]), vec![], digest).is_err());
}

fn checked_bytes(bytes: Vec<u8>, entries: &[UndoSnapshot]) -> io::Result<(UndoStack, UndoStack)> {
    let digest = Sha256::digest(&bytes).into();
    read(Cursor::new(bytes), metadata(entries), vec![], digest)
}

#[test]
fn valid_checksums_cannot_hide_invalid_patch_bounds_references_or_utf8() {
    let entries = [snapshot("abc".into(), 0), snapshot("abc".into(), 1)];
    let mut base = MAGIC.to_vec();
    base.push(FULL);
    number(&mut base, 3).unwrap();
    base.extend(b"abc");
    for (start, end) in [(4, 4), (2, 1), (0, usize::MAX), (usize::MAX, usize::MAX)] {
        let mut bytes = base.clone();
        bytes.push(DELTA);
        number(&mut bytes, 1).unwrap();
        for value in [start, end, 0] {
            number(&mut bytes, value).unwrap();
        }
        assert!(checked_bytes(bytes, &entries).is_err());
    }
    for reference in [1, 2, usize::MAX] {
        let mut bytes = base.clone();
        bytes.push(REFERENCE);
        number(&mut bytes, reference).unwrap();
        assert!(checked_bytes(bytes, &entries).is_err());
    }
    let mut overlap = base.clone();
    overlap.push(DELTA);
    number(&mut overlap, 2).unwrap();
    for value in [1, 2, 0, 0, 2, 0] {
        number(&mut overlap, value).unwrap();
    }
    assert!(checked_bytes(overlap, &entries).is_err());
    let mut excessive = base.clone();
    excessive.push(DELTA);
    number(&mut excessive, usize::MAX).unwrap();
    assert!(checked_bytes(excessive, &entries).is_err());
    for tag in [DELTA, REFERENCE, 99] {
        let mut bytes = MAGIC.to_vec();
        bytes.push(tag);
        number(&mut bytes, 0).unwrap();
        assert!(checked_bytes(bytes, &entries[..1]).is_err());
    }
    let mut utf8 = MAGIC.to_vec();
    utf8.push(FULL);
    number(&mut utf8, 3).unwrap();
    utf8.extend([0xff, 0xff, 0xff]);
    assert!(checked_bytes(utf8, &entries[..1]).is_err());
}

#[test]
fn codec_handles_short_reads_and_short_writes() {
    struct ShortWriter(Vec<u8>);
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = bytes.len().min(3);
            self.0.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    struct ShortReader(Cursor<Vec<u8>>);
    impl Read for ShortReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let n = bytes.len().min(2);
            self.0.read(&mut bytes[..n])
        }
    }
    let entries = [snapshot("é😀\r\n".into(), 0), snapshot("é界\r\n".into(), 1)];
    let mut output = ShortWriter(Vec::new());
    let digest = write(&mut output, &entries, &[]).unwrap();
    assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&output.0)));
    let (restored, _) = read(
        ShortReader(Cursor::new(output.0)),
        metadata(&entries),
        vec![],
        digest,
    )
    .unwrap();
    assert_eq!(restored, entries);
}

struct Project(tempfile::TempDir);
impl Project {
    fn new() -> Self {
        Self(tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap())
    }
    fn root(&self) -> &std::path::Path {
        self.0.path()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let state = crate::portable::workspace_dir(self.root());
        assert!(state.starts_with(crate::portable::data_dir().join("workspaces")));
        let _ = std::fs::remove_dir_all(state);
    }
}

fn recovery_storage_roundtrip(bytes: usize) {
    use crate::session::{self, Session};
    let project = Project::new();
    let mut rope = Rope::from_reader(std::io::repeat(b'a').take(bytes as u64)).unwrap();
    let original = rope.clone();
    let undo = vec![snapshot(rope.clone(), 0)];
    rope.insert(bytes / 2, "edited é🦀\r\n");
    let mut future = rope.clone();
    future.insert(25, "redo");
    let redo = vec![snapshot(future, 1)];
    let mut tab = session::tests::recovery_test_tab("");
    tab.pending_unsaved_content = Some(rope.clone());
    tab.undo_entries = Some(undo.clone());
    tab.redo_entries = Some(redo.clone());
    let started = std::time::Instant::now();
    session::save_session(project.root(), &Session { tabs: vec![tab], ..Default::default() }).unwrap();
    let elapsed = started.elapsed();
    let stored_bytes: u64 = std::fs::read_dir(session::unsaved_dir(project.root())).unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len()).sum();
    assert!(stored_bytes < rope.len_bytes() as u64 + 16 * 1024,
        "recovery duplicated the document: {stored_bytes} bytes for {} bytes", rope.len_bytes());
    let mut saved = session::load_session(project.root()).unwrap().tabs.remove(0);
    let manifest = saved.clone();
    session::hydrate_saved_tab(project.root(), &mut saved);
    assert_eq!(saved.pending_unsaved_content.as_ref().unwrap(), &rope);
    assert_eq!(saved.undo_entries.as_ref().unwrap(), &undo);
    assert_eq!(saved.redo_entries.as_ref().unwrap(), &redo);
    let restored = saved.pending_unsaved_content.as_ref().unwrap();
    assert!(leaf_bytes(std::iter::once(restored).chain(saved.undo_entries.as_ref().unwrap().iter()
        .chain(saved.redo_entries.as_ref().unwrap()).map(|entry| &entry.0))) < rope.len_bytes() * 2);

    // Retaining a manifest before hydration must keep the base as well as its
    // patches, without rewriting either file or requiring the original file.
    let retained = Session { tabs: vec![manifest], ..Default::default() };
    let original_file = project.root().join("test.txt");
    std::fs::write(&original_file, "unrelated external content").unwrap();
    session::save_session(project.root(), &retained).unwrap();
    let mut restored = session::load_session(project.root()).unwrap().tabs.remove(0);
    session::hydrate_saved_tab(project.root(), &mut restored);
    assert_eq!(restored.pending_unsaved_content.as_ref().unwrap(), &rope);
    assert_eq!(restored.undo_entries.as_ref().unwrap()[0].0, original);
    println!("{bytes} byte document: {stored_bytes} recovery bytes; write {elapsed:?}");
}

#[test]
fn recovery_persists_one_document_plus_small_undo_redo_deltas() {
    recovery_storage_roundtrip(1024 * 1024);
}

#[test]
#[ignore = "128 MiB disk and restore benchmark; run alone"]
fn benchmark_large_recovery_storage() {
    recovery_storage_roundtrip(128 * 1024 * 1024);
}

#[test]
fn shared_base_checksum_rejects_stale_history_without_replacing_recovery_text() {
    use crate::session::{self, Session};
    let project = Project::new();
    let tab = session::tests::recovery_test_tab("original recovery text");
    session::save_session(project.root(), &Session { tabs: vec![tab], ..Default::default() }).unwrap();
    let mut saved = session::load_session(project.root()).unwrap().tabs.remove(0);
    let text_path = session::unsaved_dir(project.root()).join(saved.unsaved_file.as_ref().unwrap());
    std::fs::write(text_path, "modified recovery text").unwrap(); // Same byte length.
    session::hydrate_saved_tab(project.root(), &mut saved);
    assert_eq!(saved.pending_unsaved_content.as_ref().unwrap(), "modified recovery text");
    assert!(saved.undo_entries.is_none() && saved.redo_entries.is_none());
}

#[test]
fn persisted_history_trims_each_stack_without_an_external_base_dependency() {
    let project = Project::new();
    let entries: Vec<_> = (0..75)
        .map(|i| snapshot(Rope::from_str(&format!("text {i} é😀\r\n")), i))
        .collect();
    let redo: Vec<_> = entries.iter().rev().cloned().collect();
    let mut retained = HashSet::new();
    let name =
        crate::session::save_test_undo_history(project.root(), &entries, &redo, &mut retained).unwrap();
    assert_eq!(retained.len(), 2);
    let (undo, restored_redo) = crate::session::load_undo_history(project.root(), &name).unwrap();
    assert_eq!(undo, entries[25..]);
    assert_eq!(restored_redo, redo[25..]);
}

#[test]
fn legacy_history_is_read_retained_and_upgraded_after_hydration() {
    use crate::session::{self, SavedTab, Session};
    let project = Project::new();
    for lengths in [false, true] {
        let entries = [snapshot("old é😀 text".into(), 0), snapshot("".into(), 1)];
        let mut saved = metadata(&entries);
        if !lengths {
            for entry in &mut saved {
                entry.content_bytes = None;
            }
        }
        let content = session::save_recovery_file(project.root(), "_legacy.txt", |writer| {
            entries[0].0.write_to(&mut *writer)?;
            writer.write_all(session::UNDO_SEPARATOR)
        })
        .unwrap();
        let empty = session::save_recovery_file(project.root(), "_empty.txt", |_| Ok(())).unwrap();
        let history = serde_json::json!({"undo": saved, "redo": [],
            "undo_contents_file": content, "redo_contents_file": empty});
        let name = session::save_recovery_file(project.root(), "_legacy.json", |writer| {
            serde_json::to_writer(writer, &history).map_err(io::Error::other)
        })
        .unwrap();
        let mut state = Session {
            tabs: vec![SavedTab {
                file_path: Some("missing.txt".into()),
                cursor_line: 0,
                cursor_col: 0,
                scroll_offset: 0,
                horizontal_scroll: 0,
                pinned: false,
                preview: false,
                unsaved_file: None,
                pending_unsaved_content: Some("live text".into()),
                undo_history_file: Some(name),
                language_override: None,
                undo_entries: None,
                redo_entries: None,
                bookmarks: vec![],
                folded_ranges: vec![],
            }],
            ..Session::default()
        };
        session::save_session(project.root(), &state).unwrap();
        state = session::load_session(project.root()).unwrap();
        session::hydrate_saved_tab(project.root(), &mut state.tabs[0]);
        assert_eq!(state.tabs[0].undo_entries.as_ref().unwrap(), &entries);
        session::save_session(project.root(), &state).unwrap();
        let upgraded = session::load_session(project.root()).unwrap();
        let name = upgraded.tabs[0].undo_history_file.as_ref().unwrap();
        let bytes = std::fs::read(session::unsaved_dir(project.root()).join(name)).unwrap();
        let history: session::SavedUndoHistory = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(history.version, 3);
        assert_eq!(history.content_files().unwrap().len(), 2);
    }
}

#[test]
fn invalid_manifests_or_payloads_do_not_replace_recovered_live_text() {
    use crate::session::{self, SavedTab};
    let project = Project::new();
    let entries = [snapshot("previous text".into(), 0)];
    let name =
        session::save_test_undo_history(project.root(), &entries, &[], &mut HashSet::new()).unwrap();
    let path = session::unsaved_dir(project.root()).join(&name);
    let original = std::fs::read(&path).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let content = value["contents_file"].as_str().unwrap().to_owned();
    for (key, replacement) in [
        ("version", serde_json::json!(9)),
        ("contents_sha256", serde_json::Value::Null),
        ("contents_file", serde_json::json!("../escape")),
    ] {
        let mut changed = value.clone();
        changed[key] = replacement;
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(session::load_undo_history(project.root(), &name).is_none());
    }
    std::fs::write(&path, &original).unwrap();
    std::fs::write(
        session::unsaved_dir(project.root()).join(content),
        "corrupt bytes",
    )
    .unwrap();
    let mut tab = SavedTab {
        file_path: None,
        cursor_line: 0,
        cursor_col: 0,
        scroll_offset: 0,
        horizontal_scroll: 0,
        pinned: false,
        preview: false,
        unsaved_file: None,
        pending_unsaved_content: Some("irreplaceable live text".into()),
        undo_history_file: Some(name),
        language_override: None,
        undo_entries: None,
        redo_entries: None,
        bookmarks: vec![],
        folded_ranges: vec![],
    };
    session::hydrate_saved_tab(project.root(), &mut tab);
    assert_eq!(
        tab.pending_unsaved_content.as_ref().unwrap(),
        "irreplaceable live text"
    );
    assert!(tab.undo_entries.is_none());
    assert!(tab.redo_entries.is_none());
}

#[test]
#[ignore = "manual optimized undo recovery codec benchmark; run on one idle CPU"]
fn benchmark_shared_undo_recovery() {
    use std::hint::black_box;
    use std::time::{Duration, Instant};
    let entries = local_history(100);
    let undo = &entries[..50];
    let redo: Vec<_> = entries[50..].iter().rev().cloned().collect();
    let mut old_bytes = Vec::new();
    for entry in undo.iter().chain(&redo) {
        entry.0.write_to(&mut old_bytes).unwrap();
    }
    let (bytes, digest) = encode(undo, &redo);
    let old_read = || {
        let mut input = Cursor::new(&old_bytes);
        undo.iter()
            .chain(&redo)
            .map(|entry| {
                Rope::from_reader(input.by_ref().take(entry.0.len_bytes() as u64)).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let (loaded_undo, loaded_redo) =
        read(Cursor::new(&bytes), metadata(undo), metadata(&redo), digest).unwrap();
    let shared_leaf_bytes =
        leaf_bytes(loaded_undo.iter().chain(&loaded_redo).map(|entry| &entry.0));
    let old_leaf_bytes = leaf_bytes(old_read().iter());
    let mut writes = [Vec::new(), Vec::new()];
    let mut reads = [Vec::new(), Vec::new()];
    for sample in 0..11 {
        for order in 0..2 {
            let mode = (sample + order) % 2;
            let mut output = Vec::new();
            let started = Instant::now();
            if mode == 0 {
                for entry in undo.iter().chain(&redo) {
                    entry.0.write_to(&mut output).unwrap();
                }
            } else {
                black_box(write(&mut output, black_box(undo), black_box(&redo)).unwrap());
            }
            let elapsed_write = started.elapsed();
            let undo_meta = metadata(undo);
            let redo_meta = metadata(&redo);
            let started = Instant::now();
            if mode == 0 {
                let loaded = old_read();
                let elapsed = started.elapsed();
                black_box(&loaded);
                if sample >= 2 {
                    reads[mode].push(elapsed);
                }
            } else {
                let loaded = read(Cursor::new(&bytes), undo_meta, redo_meta, digest).unwrap();
                let elapsed = started.elapsed();
                black_box(&loaded);
                if sample >= 2 {
                    reads[mode].push(elapsed);
                }
            }
            if sample >= 2 {
                writes[mode].push(elapsed_write);
            }
            black_box(output);
        }
    }
    let median = |values: &mut Vec<Duration>| {
        values.sort();
        values[values.len() / 2].as_secs_f64() * 1_000.0
    };
    println!(
        "100 snapshots, {} base bytes: full payload {} bytes, shared payload {} bytes; text leaves {} -> {} bytes; encode {:.3} -> {:.3} ms; restore {:.3} -> {:.3} ms",
        entries[0].0.len_bytes(),
        old_bytes.len(),
        bytes.len(),
        old_leaf_bytes,
        shared_leaf_bytes,
        median(&mut writes[0]),
        median(&mut writes[1]),
        median(&mut reads[0]),
        median(&mut reads[1])
    );
}

#[test]
#[ignore = "manual optimized memory benchmark; run alone on one idle CPU"]
fn benchmark_memory_history_edit_plan() {
    use std::{hint::black_box, time::Instant};
    const BYTES: usize = 128 * 1024 * 1024;
    let rope = Rope::from_reader(std::io::repeat(b'a').take(BYTES as u64)).unwrap();
    for distant in [false, true] {
        let mut edited = rope.clone();
        if distant {
            edited.insert(BYTES - 100, "界");
            edited.insert(100, "🦀");
        } else {
            edited.insert(BYTES / 2, "界");
        }
        let mut samples = Vec::new();
        let mut replacement_bytes = 0;
        for sample in 0..9 {
            let started = Instant::now();
            let changes = edits(black_box(&rope), black_box(&edited));
            let elapsed = started.elapsed();
            replacement_bytes = changes
                .iter()
                .map(|(_, range)| edited.slice(range.clone()).len_bytes())
                .sum();
            black_box(changes);
            if sample > 1 {
                samples.push(elapsed);
            }
        }
        samples.sort();
        let (removed, inserted) = crate::rope_changes::changed_chars(&rope, &edited);
        let indexed = if removed.is_empty() || inserted.is_empty() {
            0
        } else {
            chunk_anchors(rope.slice(removed))
                .values()
                .map(Positions::len)
                .sum::<usize>()
        };
        println!(
            "128 MiB undo edit plan, distant={distant}: {:?}; replacement {replacement_bytes} bytes; {indexed} anchors (whole document has {} chunks)",
            samples[samples.len() / 2],
            rope.chunks().count()
        );
    }
}
