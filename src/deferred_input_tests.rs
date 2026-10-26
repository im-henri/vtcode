use super::*;
use std::time::Duration;

struct Field {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    history: TextInputHistory,
}
impl Field {
    fn new(text: &str) -> Self {
        Self {
            text: text.into(),
            cursor: text.len(),
            anchor: None,
            history: Default::default(),
        }
    }
    fn edit(
        &mut self,
        code: KeyCode,
        mods: KeyModifiers,
        paste: Option<&str>,
        conditional: Option<u64>,
        when: Instant,
    ) {
        self.history.edit_at(
            TextInput::new(&mut self.text, &mut self.cursor, &mut self.anchor).multiline(true),
            code,
            mods,
            paste,
            conditional,
            when,
        );
    }
    fn resolve(&mut self, decisions: &[(u64, bool)]) {
        let replay = self
            .history
            .deferred_input(&self.text, self.cursor, self.anchor)
            .unwrap()
            .clone();
        let resolved = replay
            .resolve(
                &decisions.iter().copied().collect(),
                &crate::background::CancellationToken::new(),
            )
            .unwrap();
        assert!(self.history.apply_resolved(
            TextInput::new(&mut self.text, &mut self.cursor, &mut self.anchor),
            replay.stamp(),
            resolved
        ));
    }
}

#[test]
fn resolved_file_paste_preserves_replaced_selection_and_native_undo_groups() {
    let start = Instant::now();
    let mut field = Field::new("keep selected tail");
    field.cursor = 13;
    field.anchor = Some(5);
    field.edit(
        KeyCode::Null,
        KeyModifiers::NONE,
        Some("/node/file.rs"),
        Some(1),
        start,
    );
    field.edit(
        KeyCode::Char('λ'),
        KeyModifiers::NONE,
        None,
        None,
        start + Duration::from_millis(20),
    );
    field.edit(
        KeyCode::Char('b'),
        KeyModifiers::NONE,
        None,
        None,
        start + Duration::from_millis(600),
    );
    field.resolve(&[(1, true)]);
    assert_eq!(field.text, "keep λb tail");
    field.edit(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL,
        None,
        None,
        start + Duration::from_secs(1),
    );
    assert_eq!(field.text, "keep λ tail");
    field.edit(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL,
        None,
        None,
        start + Duration::from_secs(2),
    );
    assert_eq!(field.text, "keep selected tail");
    assert_eq!((field.cursor, field.anchor), (13, Some(5)));
    field.edit(
        KeyCode::Char('y'),
        KeyModifiers::CONTROL,
        None,
        None,
        start + Duration::from_secs(3),
    );
    assert_eq!(field.text, "keep λ tail");
}

#[test]
fn mixed_pastes_and_undo_redo_match_native_accepted_and_literal_input() {
    let start = Instant::now();
    let mut actual = Field::new("before");
    let mut expected = Field::new("before");
    for (index, (code, mods, paste, id)) in [
        (
            KeyCode::Null,
            KeyModifiers::NONE,
            Some("path space.txt"),
            Some(1),
        ),
        (KeyCode::Char(' '), KeyModifiers::NONE, None, None),
        (
            KeyCode::Null,
            KeyModifiers::NONE,
            Some("not a file 🦀\r\n"),
            Some(2),
        ),
        (KeyCode::Char('x'), KeyModifiers::NONE, None, None),
        (KeyCode::Char('z'), KeyModifiers::CONTROL, None, None),
        (KeyCode::Char('y'), KeyModifiers::CONTROL, None, None),
        (
            KeyCode::Null,
            KeyModifiers::NONE,
            Some("file:///a.py"),
            Some(3),
        ),
        (KeyCode::Backspace, KeyModifiers::CONTROL, None, None),
    ]
    .into_iter()
    .enumerate()
    {
        let when = start + Duration::from_millis(index as u64 * 100);
        actual.edit(code, mods, paste, id, when);
        if !matches!(id, Some(1 | 3)) {
            expected.edit(code, mods, paste, None, when);
        }
    }
    actual.resolve(&[(1, true), (2, false), (3, true)]);
    assert_eq!(
        (&actual.text, actual.cursor, actual.anchor),
        (&expected.text, expected.cursor, expected.anchor)
    );
    for (code, mods) in [
        (KeyCode::Char('z'), KeyModifiers::CONTROL),
        (KeyCode::Char('y'), KeyModifiers::CONTROL),
    ] {
        actual.edit(code, mods, None, None, start + Duration::from_secs(3));
        expected.edit(code, mods, None, None, start + Duration::from_secs(3));
        assert_eq!(
            (&actual.text, actual.cursor, actual.anchor),
            (&expected.text, expected.cursor, expected.anchor)
        );
    }
}

#[test]
fn newer_input_invalidates_worker_snapshot_and_frozen_send_is_independent() {
    let start = Instant::now();
    let mut field = Field::new("");
    field.edit(
        KeyCode::Null,
        KeyModifiers::NONE,
        Some("file.rs"),
        Some(1),
        start,
    );
    let stale = field
        .history
        .deferred_input(&field.text, field.cursor, field.anchor)
        .unwrap()
        .clone();
    field.edit(KeyCode::Char('x'), KeyModifiers::NONE, None, None, start);
    assert!(
        stale
            .resolve(
                &HashMap::new(),
                &crate::background::CancellationToken::new()
            )
            .is_none()
    );
    let frozen = std::mem::take(&mut field.history)
        .freeze(&field.text, field.cursor, field.anchor)
        .unwrap();
    field.text = "new draft".into();
    let resolved = frozen
        .replay
        .resolve(
            &HashMap::from([(1, true)]),
            &crate::background::CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(resolved.text, "x");
    assert_eq!(field.text, "new draft");
    let restored = frozen.restore();
    assert_eq!(restored.text, "file.rsx");
    assert!(
        restored
            .history
            .deferred_input(&restored.text, restored.cursor, restored.anchor)
            .is_some()
    );
}

#[test]
fn control_shift_delete_is_replayable_word_deletion_instead_of_clipboard_cut() {
    let start = Instant::now();
    let mut actual = Field::new("alpha beta");
    let mut expected = Field::new("alpha beta");
    actual.cursor = 0;
    expected.cursor = 0;
    actual.edit(
        KeyCode::Null,
        KeyModifiers::NONE,
        Some("file.rs"),
        Some(1),
        start,
    );
    let modifiers = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    actual.edit(KeyCode::Delete, modifiers, None, None, start);
    expected.edit(KeyCode::Delete, modifiers, None, None, start);
    actual.resolve(&[(1, true)]);
    assert_eq!(actual.text, expected.text);
    assert_eq!(actual.cursor, expected.cursor);
    assert!(clipboard_key(KeyCode::Delete, KeyModifiers::SHIFT));
    assert!(!clipboard_key(KeyCode::Delete, modifiers));
}

#[test]
fn external_edits_clipboard_actions_and_budget_overflow_preserve_visible_text() {
    let start = Instant::now();
    let mut field = Field::new("draft");
    field.edit(
        KeyCode::Null,
        KeyModifiers::NONE,
        Some("file.rs"),
        Some(1),
        start,
    );
    field.text.push('!');
    assert!(
        field
            .history
            .deferred_input(&field.text, field.cursor, field.anchor)
            .is_none()
    );
    field.edit(KeyCode::Char('a'), KeyModifiers::NONE, None, None, start);
    assert!(field.history.deferred.is_none());
    let mut field = Field::new("");
    field.edit(
        KeyCode::Null,
        KeyModifiers::NONE,
        Some("file.rs"),
        Some(2),
        start,
    );
    field.edit(KeyCode::Char('c'), KeyModifiers::CONTROL, None, None, start);
    assert!(field.history.deferred.is_none());
    let mut field = Field::new("");
    field.edit(
        KeyCode::Null,
        KeyModifiers::NONE,
        Some("file.rs"),
        Some(3),
        start,
    );
    for _ in 0..MAX_EVENTS {
        field.edit(KeyCode::Char('a'), KeyModifiers::NONE, None, None, start);
    }
    assert!(field.history.deferred.is_none());
    assert!(field.text.starts_with("file.rs"));
}

#[test]
fn thousands_of_shared_one_event_chunks_drop_without_recursive_stack_growth() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let mut events = Events::default();
            for _ in 0..MAX_EVENTS {
                let snapshot = events.clone();
                assert!(events.push(Event {
                    code: KeyCode::Null,
                    modifiers: KeyModifiers::NONE,
                    paste: None,
                    conditional: None,
                    when: Instant::now(),
                    multiline: true,
                    accept: |_| true
                }));
                drop(snapshot);
            }
            assert_eq!(events.chunks().len(), MAX_EVENTS);
            drop(events);
        })
        .unwrap()
        .join()
        .unwrap();
}
