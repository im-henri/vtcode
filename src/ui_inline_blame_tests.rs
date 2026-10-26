use super::*;
use crate::{
    app::{ActivePane, Tab, TabDropZone},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton};
use ratatui::{Terminal, backend::TestBackend};

fn fixture(text: &str, message: &str) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(app.explorer.root.join("inline-blame.rs"));
    editor.word_wrap = false;
    let commit = Arc::new(git_blame::Commit {
        hash: "0123456789abcdef0123456789abcdef01234567".into(),
        author: "Alex Morgan".into(),
        email: "alex@example.com".into(),
        age: "2d ago".into(),
        date: "2026-10-14".into(),
        committed_at: "2026-10-14 10:30:00 +03:00".into(),
        summary: "Improve the editor".into(),
        path: "inline-blame.rs".into(),
        message: message.into(),
    });
    app.blame_documents.insert(
        editor.navigation_id,
        Arc::new(git_blame::Document {
            path: editor.file_path.clone().unwrap(),
            revision: None,
            rope: editor.rope.clone(),
            key: None,
            lines: (0..editor.rope.len_lines())
                .map(|_| git_blame::Line::Committed(commit.clone()))
                .collect(),
            commit_lines: (0..editor.rope.len_lines()).map(Some).collect(),
            notice: None,
        }),
    );
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.terminal_has_focus = true;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app
}

fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    render_at(app, width, height, Instant::now())
}

fn render_at(app: &mut App, width: u16, height: u16, now: Instant) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            app.editor_area = Some(frame.area());
            crate::ui_editor::draw_editor(frame, app, frame.area());
            draw_at(frame, app, now);
        })
        .unwrap();
    terminal
}

fn output(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn pointer(app: &mut App, area: Rect, kind: MouseEventKind) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            column: area.x,
            row: area.y,
            kind,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn open(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    render(app, width, height);
    let (anchor, _) = app
        .editor()
        .unwrap()
        .blame
        .inline_annotation
        .expect("visible annotation");
    pointer(app, anchor, MouseEventKind::Moved);
    app.inline_blame_hover.as_mut().unwrap().started -= HOVER_DELAY;
    render(app, width, height)
}

#[test]
fn inline_blame_waits_two_seconds_before_formatting_and_schedules_no_visible_card_timer() {
    let mut app = fixture("value\n", "Delayed commit message");
    render(&mut app, 90, 20);
    let (anchor, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
    pointer(&mut app, anchor, MouseEventKind::Moved);
    let started = app.inline_blame_hover.as_ref().unwrap().started;
    assert_eq!(next_wait(&app, started), Some(HOVER_DELAY));
    assert!(!hover_due(&app));
    for elapsed in [Duration::ZERO, HOVER_DELAY - Duration::from_nanos(1)] {
        let terminal = render_at(&mut app, 90, 20, started + elapsed);
        assert!(!output(&terminal).contains("Commit details"));
        let hover = app.inline_blame_hover.as_ref().unwrap();
        assert!(hover.area.is_empty() && hover.rows.is_empty());
        assert_eq!(
            next_wait(&app, started + elapsed),
            Some(HOVER_DELAY - elapsed)
        );
    }
    assert_eq!(next_wait(&app, started + HOVER_DELAY), Some(Duration::ZERO));
    let terminal = render_at(&mut app, 90, 20, started + HOVER_DELAY);
    assert!(output(&terminal).contains("Delayed commit message"));
    assert_eq!(next_wait(&app, started + HOVER_DELAY), None);
    assert!(
        !hover_due(&app),
        "a visible card must not repeatedly wake the UI"
    );
}

#[test]
fn inline_blame_pending_delay_restarts_on_motion_but_not_duplicate_pointer_events() {
    let mut app = fixture("value\n", "Pointer dwell");
    render(&mut app, 90, 20);
    let (anchor, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
    pointer(&mut app, anchor, MouseEventKind::Moved);
    let started = Instant::now() - HOVER_DELAY;
    app.inline_blame_hover.as_mut().unwrap().started = started;
    pointer(&mut app, anchor, MouseEventKind::Moved);
    assert_eq!(app.inline_blame_hover.as_ref().unwrap().started, started);
    assert!(hover_due(&app));
    let moved = Rect::new(anchor.x + 1, anchor.y, 1, 1);
    pointer(&mut app, moved, MouseEventKind::Moved);
    let restarted = app.inline_blame_hover.as_ref().unwrap().started;
    assert!(restarted > started);
    assert!(
        !output(&render_at(&mut app, 90, 20, restarted + HOVER_DELAY / 2))
            .contains("Commit details")
    );
    pointer(&mut app, Rect::new(0, 0, 1, 1), MouseEventKind::Moved);
    assert!(app.inline_blame_hover.is_none());
    assert_eq!(next_wait(&app, restarted + HOVER_DELAY), None);
    pointer(&mut app, moved, MouseEventKind::Moved);
    let returned = app.inline_blame_hover.as_ref().unwrap().started;
    assert_eq!(next_wait(&app, returned), Some(HOVER_DELAY));
}

#[test]
fn scrolling_or_clicking_a_pending_blame_annotation_cancels_the_popup() {
    for kind in [
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollLeft,
        MouseEventKind::ScrollRight,
        MouseEventKind::Down(MouseButton::Left),
    ] {
        let mut app = fixture(&"value\n".repeat(100), "Do not show while scrolling");
        render(&mut app, 90, 20);
        let (anchor, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
        pointer(&mut app, anchor, MouseEventKind::Moved);
        assert!(app.inline_blame_hover.is_some());
        pointer(&mut app, anchor, kind);
        assert!(app.inline_blame_hover.is_none(), "{kind:?}");
        assert!(!output(&render(&mut app, 90, 20)).contains("Commit details"));
        assert_eq!(next_wait(&app, Instant::now() + HOVER_DELAY), None);
    }
}

#[test]
fn pending_blame_never_opens_for_an_edited_hidden_or_obscured_source() {
    for change in ["edit", "tab", "modal", "focus"] {
        let mut app = fixture("value\n", "Stale pending commit");
        render(&mut app, 90, 20);
        let (anchor, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
        pointer(&mut app, anchor, MouseEventKind::Moved);
        let started = app.inline_blame_hover.as_ref().unwrap().started;
        match change {
            "edit" => app.editor_mut().unwrap().rope.insert(0, "x"),
            "tab" => {
                app.tabs
                    .push(Tab::new_file(Editor::test_with_text("other\n")));
                app.active_tab = 1;
            }
            "modal" => app.context_menu_active = true,
            "focus" => app.terminal_has_focus = false,
            _ => unreachable!(),
        }
        let terminal = render_at(&mut app, 90, 20, started + HOVER_DELAY);
        assert!(
            !output(&terminal).contains("Stale pending commit"),
            "{change}"
        );
        assert!(app.inline_blame_hover.is_none(), "{change}");
        assert_eq!(next_wait(&app, started + HOVER_DELAY), None);
    }
}

#[test]
fn inline_blame_hover_shows_metadata_and_complete_multiline_message() {
    let mut app = fixture(
        "let value = 42;\n",
        "Improve the editor\n\nExplain why this change was needed.\nKeep this second paragraph too.",
    );
    let terminal = open(&mut app, 110, 30);
    crate::ui_control_visibility_tests::snapshot(
        terminal.backend().buffer(),
        "inline blame hover details",
    );
    let text = output(&terminal);
    for expected in [
        "Commit details",
        "0123456789abcdef0123456789abcdef01234567",
        "Alex Morgan <alex@example.com>",
        "2026-10-14 10:30:00 +03:00",
        "Improve the editor",
        "Explain why this change was needed.",
        "Keep this second paragraph too.",
    ] {
        assert!(text.contains(expected), "missing {expected:?}");
    }
    assert!(
        app.hovered_editor_pos.is_none(),
        "blame must not start an LSP hover"
    );
    assert!(app.hovered_gutter_line.is_none());
    let hover = app.inline_blame_hover.as_ref().unwrap();
    assert!(hover.area.intersection(hover.target.anchor).is_empty());
}

#[test]
fn inline_blame_hover_uses_painted_columns_and_only_visible_annotation_text() {
    let mut app = fixture("\tlet 世界 = 42;\n", "Unicode geometry");
    app.editor_mut().unwrap().horizontal_scroll = 3;
    let terminal = render(&mut app, 80, 15);
    let (anchor, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
    let label: String = (anchor.x..anchor.right())
        .map(|x| terminal.backend().buffer()[(x, anchor.y)].symbol())
        .collect();
    assert_eq!(label, "Alex Morgan, 2d ago");
    pointer(
        &mut app,
        Rect::new(anchor.x - 1, anchor.y, 1, 1),
        MouseEventKind::Moved,
    );
    assert!(
        app.inline_blame_hover.is_none(),
        "the padding is not an annotation"
    );
    pointer(&mut app, anchor, MouseEventKind::Moved);
    assert!(app.inline_blame_hover.is_some());
    render(&mut app, anchor.x + 4, 15);
    let (clipped, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
    assert!(clipped.width < anchor.width);
    assert!(
        app.code_area
            .unwrap()
            .contains((clipped.right() - 1, clipped.y).into())
    );
    render(&mut app, 8, 15);
    assert!(app.editor().unwrap().blame.inline_annotation.is_none());
    assert!(app.inline_blame_hover.is_none());
}

#[test]
fn inline_blame_hover_scrolls_to_the_last_message_line_without_scrolling_source() {
    let message = (0..90)
        .map(|line| format!("Message line {line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = fixture("let value = 42;\nsecond line\n", &message);
    open(&mut app, 90, 16);
    let area = app.inline_blame_hover.as_ref().unwrap().area;
    let source_scroll = app.editor().unwrap().scroll_offset;
    pointer(&mut app, area, MouseEventKind::Moved);
    assert!(
        app.inline_blame_hover.is_some(),
        "entering the card keeps it open"
    );
    for _ in 0..100 {
        pointer(&mut app, area, MouseEventKind::ScrollDown);
    }
    let terminal = render(&mut app, 90, 16);
    crate::ui_control_visibility_tests::snapshot(
        terminal.backend().buffer(),
        "inline blame hover scrolled message",
    );
    assert!(output(&terminal).contains("Message line 89"));
    assert_eq!(app.editor().unwrap().scroll_offset, source_scroll);
    assert_eq!(app.inline_blame_hover.as_ref().unwrap().area, area);
    for _ in 0..100 {
        pointer(&mut app, area, MouseEventKind::ScrollUp);
    }
    assert!(output(&render(&mut app, 90, 16)).contains("Author"));
    pointer(&mut app, Rect::new(0, 0, 1, 1), MouseEventKind::Moved);
    assert!(app.inline_blame_hover.is_none());
}

#[test]
fn inline_blame_hover_dismisses_on_keyboard_and_reopens_only_after_mouse_movement() {
    let mut app = fixture("let value = 42;\nsecond line\n", "Keyboard dismiss");
    open(&mut app, 90, 20);
    let (anchor, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );
    assert!(app.inline_blame_hover.is_none());
    render(&mut app, 90, 20);
    assert!(app.inline_blame_hover.is_none());
    pointer(
        &mut app,
        Rect::new(anchor.x + 1, anchor.y, 1, 1),
        MouseEventKind::Moved,
    );
    assert!(app.inline_blame_hover.is_some());
    app.context_menu_active = true;
    render(&mut app, 90, 20);
    assert!(app.inline_blame_hover.is_none());
}

#[test]
fn inline_blame_hover_rejects_changed_buffers_and_handles_uncommitted_lines() {
    let mut app = fixture("let value = 42;\n", "Committed content");
    open(&mut app, 90, 20);
    app.editor_mut().unwrap().rope.insert(0, "x");
    render(&mut app, 90, 20);
    assert!(
        app.inline_blame_hover.is_none(),
        "never show stale attribution after an edit"
    );
    let editor = app.editor().unwrap();
    let id = editor.navigation_id;
    let rope = editor.rope.clone();
    let document = Arc::get_mut(app.blame_documents.get_mut(&id).unwrap()).unwrap();
    document.rope = rope;
    document.lines[0] = git_blame::Line::Uncommitted;
    let terminal = open(&mut app, 90, 20);
    assert!(output(&terminal).contains("has not been committed"));
    assert!(!output(&terminal).contains("alex@example.com"));
}

#[test]
fn inline_blame_hover_stays_inside_short_frames_and_flips_above_the_last_line() {
    let mut app = fixture(
        &"value\n".repeat(30),
        "A long message\n\nMore information\nAnd another paragraph",
    );
    for (width, height) in [(100, 24), (35, 12), (24, 8), (24, 4)] {
        app.editor_mut().unwrap().cursor_line = 28;
        app.editor_mut().unwrap().scroll_offset = 29 - height as usize;
        let terminal = open(&mut app, width, height);
        if let Some(hover) = &app.inline_blame_hover {
            assert_eq!(
                hover.area.intersection(terminal.backend().buffer().area),
                hover.area
            );
            assert_eq!(hover.area.intersection(app.code_area.unwrap()), hover.area);
            assert!(hover.area.intersection(hover.target.anchor).is_empty());
            assert!(hover.area.bottom() <= hover.target.anchor.y);
        }
    }
}

#[test]
fn inline_blame_hover_works_in_an_inactive_split_without_changing_focus() {
    let mut app = fixture("value\n", "Inactive split message");
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("other file\n")));
    app.create_split_group_at(1, TabDropZone::Right);
    let inactive = 1 - app.active_group;
    let focused = app.active_group;
    let mut terminal = Terminal::new(TestBackend::new(180, 30)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_helpers::draw_split_editor_for_group(
                frame,
                &mut app,
                Rect::new(0, 0, 90, 30),
                inactive,
            );
        })
        .unwrap();
    let tab = &app.groups[inactive].tabs[app.groups[inactive].active_tab];
    let (anchor, _) = tab.editor.blame.inline_annotation.unwrap();
    pointer(&mut app, anchor, MouseEventKind::Moved);
    app.inline_blame_hover.as_mut().unwrap().started -= HOVER_DELAY;
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert!(output(&terminal).contains("Inactive split message"));
    assert_eq!(app.active_group, focused);
    pointer(&mut app, anchor, MouseEventKind::Down(MouseButton::Left));
    assert!(app.inline_blame_hover.is_none());
}

#[test]
fn inline_blame_hover_follows_source_rows_when_ai_review_inserts_virtual_rows() {
    use vtcode::acp::rollback::{FileReview, InlineChunk};
    let text = "replacement\nvalue\nkept\n";
    let mut app = fixture(text, "The source line's commit message");
    app.animation_settings.set(
        crate::animation_config::AnimationEffect::ScopeConnector,
        true,
    );
    app.editor_mut().unwrap().cursor_line = 1;
    app.chat.rollback.enabled = true;
    app.chat.rollback.inline_reviews = Arc::new(vec![FileReview {
        path: app.editor().unwrap().file_path.clone().unwrap(),
        text: text.into(),
        chunks: vec![InlineChunk {
            edit: 1,
            index: 0,
            range: Some(0..12),
            original: "removed\nold line\n".into(),
            line: 1,
            issue: None,
        }],
    }]);
    let terminal = open(&mut app, 100, 28);
    let editor = app.editor().unwrap();
    assert!(editor.ai_review.active);
    let (anchor, line) = editor.blame.inline_annotation.unwrap();
    let row = usize::from(anchor.y - app.code_area.unwrap().y);
    assert!(row > 1, "review rows shifted the annotation");
    let (candidate, candidate_line) = editor.blame.inline_candidate.unwrap();
    assert_eq!(candidate.y, anchor.y, "Cold blame demand follows the same source cells");
    assert_eq!(candidate_line, line);
    assert_eq!(editor.ai_review.source_rows[row].unwrap().line_idx, line);
    assert_eq!(line, 1);
    assert!(output(&terminal).contains("The source line's commit message"));
    app.rendering_animation_frame = true;
    for _ in 0..3 {
        let terminal = render(&mut app, 100, 28);
        assert_eq!(
            app.editor().unwrap().blame.inline_annotation,
            Some((anchor, line))
        );
        assert_eq!(app.editor().unwrap().blame.inline_candidate, Some((candidate, line)));
        assert!(output(&terminal).contains("The source line's commit message"));
    }
    assert!(app.animation_frame_cache.replays >= 3);
}

#[test]
fn inline_blame_hover_survives_animation_replay_and_closes_when_its_tab_is_hidden() {
    let mut app = fixture("value\n", "Cached commit message");
    app.animation_settings.set(
        crate::animation_config::AnimationEffect::ScopeConnector,
        true,
    );
    open(&mut app, 90, 20);
    let (anchor, _) = app.editor().unwrap().blame.inline_annotation.unwrap();
    let captures = app.animation_frame_cache.captures;
    let rows = app.inline_blame_hover.as_ref().unwrap().rows.as_ptr();
    app.rendering_animation_frame = true;
    for _ in 0..3 {
        begin_frame(&mut app);
        let terminal = render(&mut app, 90, 20);
        assert!(output(&terminal).contains("Cached commit message"));
        assert_eq!(
            app.editor().unwrap().blame.inline_annotation.unwrap().0,
            anchor
        );
        assert_eq!(
            app.inline_blame_hover.as_ref().unwrap().rows.as_ptr(),
            rows,
            "reuse wrapped message rows"
        );
    }
    assert_eq!(app.animation_frame_cache.captures, captures);
    assert!(app.animation_frame_cache.replays >= 3);
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("different tab\n")));
    app.active_tab = 1;
    render(&mut app, 90, 20);
    assert!(app.inline_blame_hover.is_none());
}
