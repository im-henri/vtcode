//! Interaction regressions for the reduced animation state machine.
use super::*;
use unicode_width::UnicodeWidthChar;

fn document(text: &str, cursor: (usize, usize), revision: u64) -> Snapshot {
    let rope = Rope::from_str(text);
    let offset = source_offset(&rope, cursor.0, cursor.1);
    Snapshot {
        id: 41,
        rope,
        cursor,
        offset,
        content_version: revision,
        undo_revision: 0,
        scroll: (0, 0, 0),
        selection: None,
        area: Rect::new(2, 1, 80, 8),
    }
}

fn visible(snapshot: &Snapshot) -> Vec<Anchor> {
    let mut result = Vec::new();
    for line in 0..snapshot.rope.len_lines().min(snapshot.area.height as usize) {
        let source = snapshot.rope.line(line).to_string();
        let source = source.trim_end_matches(['\n', '\r']);
        let mut column = 0;
        for (col, ch) in source.chars().enumerate() {
            let width = if ch == '\t' {
                4 - column % 4
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(0)
            };
            if width > 0 && column + width <= snapshot.area.width as usize {
                result.push(Anchor {
                    line,
                    col,
                    x: snapshot.area.x + column as u16,
                    y: snapshot.area.y + line as u16,
                    width: width as u16,
                    ch,
                });
            }
            column += width;
        }
        if column < snapshot.area.width as usize {
            result.push(Anchor {
                line,
                col: source.chars().count(),
                x: snapshot.area.x + column as u16,
                y: snapshot.area.y + line as u16,
                width: 1,
                ch: '\0',
            });
        }
    }
    result
}

fn observe(
    state: &mut AnimationState,
    snapshot: &Snapshot,
    settings: &AnimationSettings,
    now: Instant,
) -> Vec<AnimationEvent> {
    state.observe(snapshot.clone(), &visible(snapshot), settings, now)
}

fn cursor_sample(
    events: &[AnimationEvent],
    settings: &AnimationSettings,
    now: Instant,
) -> SmearSample {
    events
        .iter()
        .find_map(|event| event.smear_sample(settings, now))
        .expect("a live cursor ribbon")
}

fn close_point(actual: MotionPoint, expected: MotionPoint, tolerance: f32) {
    assert!(
        (actual.x - expected.x).abs() <= tolerance && (actual.y - expected.y).abs() <= tolerance,
        "{actual:?} differs from {expected:?} by more than {tolerance}"
    );
}

#[test]
fn state_refinement_rapid_navigation_and_typing_preserve_all_live_corners_and_velocities() {
    let mut settings = AnimationSettings::default();
    settings.duration_ms = 650;
    settings.set(Effect::CursorSmear, true);
    let mut state = AnimationState::default();
    let now = Instant::now();
    let mut text = vec!["a".repeat(72); 4].join("\n");
    let mut snapshot = document(&text, (0, 0), 1);
    observe(&mut state, &snapshot, &settings, now);
    snapshot.cursor = (0, 60);
    snapshot.offset = source_offset(&snapshot.rope, 0, 60);
    let mut events = observe(&mut state, &snapshot, &settings, now);
    for turn in 1..=20 {
        let at = now + Duration::from_millis(turn * 17);
        let before = cursor_sample(&events, &settings, at);
        let target = if turn % 2 == 0 {
            (turn as usize % 4, 3)
        } else {
            (turn as usize % 4, 61)
        };
        // Real insertions during a navigation ribbon exercise the transition
        // to compact typing without allowing the old shape to jump or restart.
        if turn == 7 || turn == 14 {
            text.insert(0, ' ');
        }
        snapshot = document(
            &text,
            target,
            1 + u64::from(turn >= 7) + u64::from(turn >= 14),
        );
        events = observe(&mut state, &snapshot, &settings, at);
        let after = cursor_sample(&events, &settings, at);
        for index in 0..4 {
            close_point(after.corners[index], before.corners[index], 0.001);
            close_point(after.velocities[index], before.velocities[index], 0.08);
        }
        close_point(after.shade_direction, before.shade_direction, 0.0);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.trigger, Trigger::Cursor { .. }))
                .count(),
            1,
            "a turn retargets the one live body"
        );
    }
    let finished = now + Duration::from_secs(3);
    assert!(state.advance_frame(&settings, finished));
    assert_eq!(state.next_frame_wait(&settings, finished), None);
    assert!(!state.advance_frame(&settings, finished));
}

#[test]
fn state_refinement_whitespace_only_setting_ignores_letters_newlines_undo_and_hidden_edits() {
    let mut settings = AnimationSettings::default();
    settings.set(Effect::WhitespaceReveal, true);
    let now = Instant::now();
    for (new_text, cursor, undo, hide_whitespace, expected) in [
        ("seedx", (0, 5), false, false, false),
        ("seed\n", (1, 0), false, false, false),
        ("seed ", (0, 5), true, false, false),
        ("seed\t", (0, 5), false, true, false),
        ("seed\t", (0, 5), false, false, true),
        ("seed ", (0, 5), false, false, true),
    ] {
        let mut state = AnimationState::default();
        let before = document("seed", (0, 4), 1);
        observe(&mut state, &before, &settings, now);
        let mut after = document(new_text, cursor, 2);
        after.undo_revision = u64::from(undo);
        let anchors: Vec<_> = visible(&after)
            .into_iter()
            .filter(|anchor| !hide_whitespace || !matches!(anchor.ch, ' ' | '\t'))
            .collect();
        let events = state.observe(after, &anchors, &settings, now);
        assert_eq!(
            !events.is_empty(),
            expected,
            "{new_text:?}, undo={undo}, hidden={hide_whitespace}"
        );
        assert_eq!(state.next_frame_wait(&settings, now).is_some(), expected);
    }
}

#[test]
fn state_refinement_shrinking_selection_paste_traces_the_replacement_whitespace_only() {
    let mut settings = AnimationSettings::default();
    settings.set(Effect::WhitespaceReveal, true);
    let mut state = AnimationState::default();
    let now = Instant::now();
    let mut before = document("prefixSEVERAL WORDSsuffix", (0, 19), 1);
    before.selection = Some((0, 6, 0, 19));
    observe(&mut state, &before, &settings, now);
    let after = document("prefix \t suffix", (0, 9), 2);
    assert!(after.rope.len_chars() < before.rope.len_chars());
    let events = observe(&mut state, &after, &settings, now);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].trigger,
        Trigger::Insert {
            start: 6,
            end: 9,
            paste: true
        }
    ));
    assert_eq!(events[0].started, now);
    assert_eq!(
        state.views[&after.id].snapshot.rope.to_string(),
        "prefix \t suffix"
    );
}

#[test]
fn state_refinement_later_edits_remap_live_whitespace_without_restarting_its_clock() {
    let mut settings = AnimationSettings::default();
    settings.set(Effect::WhitespaceReveal, true);
    let mut state = AnimationState::default();
    let now = Instant::now();
    let before = document("abcdefgh", (0, 4), 1);
    observe(&mut state, &before, &settings, now);
    let inserted = document("abcd \t efgh", (0, 7), 2);
    let first = observe(&mut state, &inserted, &settings, now);
    assert!(matches!(
        first[0].trigger,
        Trigger::Insert {
            start: 4,
            end: 7,
            ..
        }
    ));
    let later = now + Duration::from_millis(25);
    let prefixed = document("Xabcd \t efgh", (0, 1), 3);
    let moved = observe(&mut state, &prefixed, &settings, later);
    assert_eq!(moved.len(), 1, "typing a letter adds no decorative event");
    assert!(matches!(
        moved[0].trigger,
        Trigger::Insert {
            start: 5,
            end: 8,
            ..
        }
    ));
    assert_eq!(moved[0].started, now);
    assert!(moved[0].progress(&settings, later) > 0.0);
    let restored = document("abcd \t efgh", (0, 0), 4);
    let moved_back = observe(&mut state, &restored, &settings, later);
    assert_eq!(moved_back.len(), 1);
    assert!(matches!(
        moved_back[0].trigger,
        Trigger::Insert {
            start: 4,
            end: 7,
            ..
        }
    ));
    assert_eq!(moved_back[0].started, now);
}

#[test]
fn state_refinement_replacing_a_live_range_discards_it_and_translates_disjoint_whitespace() {
    let mut settings = AnimationSettings::default();
    settings.set(Effect::WhitespaceReveal, true);
    let mut state = AnimationState::default();
    let now = Instant::now();
    observe(&mut state, &document("abcdefgh", (0, 4), 1), &settings, now);
    observe(
        &mut state,
        &document("abcd \t efgh", (0, 7), 2),
        &settings,
        now,
    );
    let second_at = now + Duration::from_millis(10);
    let second = document("abcd \t efgh ", (0, 12), 3);
    let events = observe(&mut state, &second, &settings, second_at);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1].trigger,
        Trigger::Insert {
            start: 11,
            end: 12,
            ..
        }
    ));
    let deletion = document("abcd\t efgh ", (0, 4), 4);
    let remaining = observe(
        &mut state,
        &deletion,
        &settings,
        second_at + Duration::from_millis(10),
    );
    assert_eq!(remaining.len(), 1);
    assert!(matches!(
        remaining[0].trigger,
        Trigger::Insert {
            start: 10,
            end: 11,
            ..
        }
    ));
    assert_eq!(remaining[0].started, second_at);
}

#[test]
fn state_refinement_coalesced_navigation_and_edits_stay_local_in_repeated_text() {
    for (old_text, old_cursor, new_text, new_cursor, expected) in [
        ("aaaaa", 1, "aaaaaa", 5, (4, 4, 5)),
        ("aaaaa", 4, "aaaaaa", 2, (1, 1, 2)),
        ("aaaaaa", 1, "aaaaa", 4, (4, 5, 4)),
        ("aaaaaa", 2, "aaaaaaa", 3, (2, 2, 3)),
    ] {
        let before = document(old_text, (0, old_cursor), 1);
        let after = document(new_text, (0, new_cursor), 2);
        assert_eq!(
            changed_range(&before, &after, &visible(&before), &visible(&after)),
            expected
        );
    }
}

#[test]
fn state_refinement_scope_changes_crossfade_then_keep_membership_without_an_idle_clock() {
    let mut settings = AnimationSettings::default();
    settings.duration_ms = 800;
    settings.set(Effect::ScopeConnector, true);
    let mut state = AnimationState::default();
    let now = Instant::now();
    let initial = document("if outer\n  if inner\n  endif\nendif", (0, 0), 1);
    observe(&mut state, &initial, &settings, now);
    let outer = ((0, 0), (3, 0));
    let inner = ((1, 2), (2, 2));
    state.update_scope(initial.id, Some((outer.1, outer.0)), &settings, now);
    let entered_at = now + Duration::from_millis(40);
    let events = state.update_scope(initial.id, Some(inner), &settings, entered_at);
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| matches!(event.trigger,
        Trigger::Scope { pair, exiting: Some(at), .. } if pair == outer && at == entered_at)));
    assert!(events.iter().any(|event| matches!(event.trigger,
        Trigger::Scope { pair, exiting: None, .. } if pair == inner)
        && event.started == entered_at));
    let exit_finished =
        entered_at + AnimationEvent::scope_exit_duration(&settings) + Duration::from_millis(1);
    let events = state.update_scope(initial.id, Some(inner), &settings, exit_finished);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].started, entered_at);
    let settled = now + Duration::from_secs(2);
    assert!(state.advance_frame(&settings, settled));
    assert!(
        state
            .update_scope(initial.id, Some(inner), &settings, settled)
            .is_empty()
    );
    assert_eq!(state.next_frame_wait(&settings, settled), None);

    // Leaving a settled connector creates a finite exit, even though its old
    // entrance clock is complete. Exit lifetime is independent of progress().
    let leaving = state.update_scope(initial.id, None, &settings, settled);
    assert_eq!(leaving.len(), 1);
    assert!(
        matches!(leaving[0].trigger, Trigger::Scope { pair, exiting: Some(at), .. }
        if pair == inner && at == settled)
    );
    assert_eq!(leaving[0].progress(&settings, settled), 1.0);
    assert!(leaving[0].active(&settings, settled));
    assert!(state.next_frame_wait(&settings, settled).is_some());
    let finished =
        settled + AnimationEvent::scope_exit_duration(&settings) + Duration::from_millis(1);
    assert!(state.advance_frame(&settings, finished));
    assert!(
        state
            .update_scope(initial.id, None, &settings, finished)
            .is_empty()
    );
    assert_eq!(state.next_frame_wait(&settings, finished), None);
    assert!(!state.advance_frame(&settings, finished));
}

#[test]
fn state_refinement_rapid_scope_changes_bound_outgoing_history_and_finish_cleanly() {
    let mut settings = AnimationSettings::default();
    settings.duration_ms = 1_500;
    settings.set(Effect::ScopeConnector, true);
    let mut state = AnimationState::default();
    let now = Instant::now();
    let source = "if condition\nendif\n".repeat(40);
    let initial = document(&source, (0, 0), 1);
    observe(&mut state, &initial, &settings, now);
    for index in 0..60 {
        let at = now + Duration::from_millis(index);
        let pair = ((index as usize, 0), (index as usize + 1, 0));
        let events = state.update_scope(initial.id, Some(pair), &settings, at);
        assert!(events.len() <= 5);
        assert!(
            events
                .iter()
                .filter(|event| matches!(
                    event.trigger,
                    Trigger::Scope {
                        exiting: Some(_),
                        ..
                    }
                ))
                .count()
                <= 4
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.trigger, Trigger::Scope { exiting: None, .. }))
                .count(),
            1
        );
    }
    let finished = now + Duration::from_secs(3);
    assert!(state.advance_frame(&settings, finished));
    assert_eq!(state.next_frame_wait(&settings, finished), None);
    assert_eq!(state.views[&initial.id].scope, Some(((59, 0), (60, 0))));
}
