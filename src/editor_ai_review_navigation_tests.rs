use super::{
    tests::{mouse, pending, render},
    *,
};
use ratatui::{Terminal, backend::TestBackend};

fn multi_pending() -> App {
    let text: String = (0..60)
        .map(|line| format!("line {line:02} · a longer line with words that can wrap\n"))
        .collect();
    let mut app = pending("old\n", &text);
    let range = |line| {
        let rope = &app.editor().unwrap().rope;
        rope.line_to_byte(line)..rope.line_to_byte(line + 1)
    };
    let first = range(2);
    let removed = range(22).start;
    let last = range(48);
    // Checkpoint order can differ from document order across conversations.
    Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0].chunks = vec![
        InlineChunk {
            edit: 61,
            index: 7,
            range: Some(last),
            original: "old last\n".into(),
            line: 49,
            issue: None,
        },
        InlineChunk {
            edit: 42,
            index: 0,
            range: Some(first),
            original: "old first\n".into(),
            line: 3,
            issue: None,
        },
        InlineChunk {
            edit: 100,
            index: 0,
            range: Some(removed..removed),
            original: "removed first\nremoved second\n".into(),
            line: 23,
            issue: None,
        },
    ];
    app
}

fn frame_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn arrow(app: &mut App, next: bool) {
    let hit = app.editor().unwrap().ai_review.navigation_hits[usize::from(next)].clone();
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        hit.area.x,
        hit.area.y,
    );
    mouse(
        app,
        MouseEventKind::Up(MouseButton::Left),
        hit.area.x,
        hit.area.y,
    );
}

fn request_accept(app: &mut App) {
    let hit = app
        .editor()
        .unwrap()
        .ai_review
        .hits
        .iter()
        .rev()
        .find(|hit| hit.chunk.is_some() && hit.accept)
        .unwrap()
        .clone();
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        hit.area.x,
        hit.area.y,
    );
    mouse(
        app,
        MouseEventKind::Up(MouseButton::Left),
        hit.area.x,
        hit.area.y,
    );
}

fn assert_selected(
    app: &App,
    terminal: &Terminal<TestBackend>,
    identity: (u64, usize),
    counter: &str,
) {
    let view = &app.editor().unwrap().ai_review;
    assert_eq!(view.selected, Some(identity));
    assert!(
        frame_text(terminal).contains(counter),
        "{}",
        frame_text(terminal)
    );
    let row = view.selected_row().unwrap();
    assert!(
        (view.row..view.row + view.code.height as usize).contains(&row),
        "selected action row must be visible"
    );
    assert!(
        view.hits
            .iter()
            .any(|hit| hit.chunk == Some(identity) && hit.accept && hit.area.y < view.code.y)
    );
    assert!(
        view.hits
            .iter()
            .any(|hit| hit.chunk == Some(identity) && !hit.accept && hit.area.y < view.code.y)
    );
}

#[test]
fn editor_ai_review_arrows_follow_file_order_wrap_and_reveal_folded_deletions() {
    for wrapped in [false, true] {
        let mut app = multi_pending();
        app.editor_mut().unwrap().word_wrap = wrapped;
        app.editor_mut().unwrap().folded_ranges.insert(20, 26);
        let text = app.editor().unwrap().rope.to_string();
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        render(&mut app, &mut terminal);
        assert_selected(&app, &terminal, (42, 0), "1/3 changes");
        arrow(&mut app, true);
        render(&mut app, &mut terminal);
        assert_selected(&app, &terminal, (100, 0), "2/3 changes");
        assert_eq!(app.editor().unwrap().cursor_line, 22);
        assert!(!app.editor().unwrap().folded_ranges.contains_key(&20));
        assert!(frame_text(&terminal).contains("removed first"));
        arrow(&mut app, true);
        render(&mut app, &mut terminal);
        assert_selected(&app, &terminal, (61, 7), "3/3 changes");
        assert_eq!(app.editor().unwrap().cursor_line, 48);
        arrow(&mut app, true);
        render(&mut app, &mut terminal);
        assert_selected(&app, &terminal, (42, 0), "1/3 changes");
        arrow(&mut app, false);
        render(&mut app, &mut terminal);
        assert_selected(&app, &terminal, (61, 7), "3/3 changes");
        assert_eq!(app.editor().unwrap().rope.to_string(), text);
        assert!(!app.editor().unwrap().modified);
    }
}

#[test]
fn editor_ai_review_selection_survives_new_chunks_and_advances_after_resolution() {
    let mut app = multi_pending();
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    render(&mut app, &mut terminal);
    arrow(&mut app, true);
    render(&mut app, &mut terminal);
    assert_selected(&app, &terminal, (100, 0), "2/3 changes");
    let file = &mut Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0];
    file.chunks.push(InlineChunk {
        edit: 900,
        index: 0,
        range: Some(0..0),
        original: "earlier removal\n".into(),
        line: 1,
        issue: None,
    });
    app.chat.rollback.inline_revision += 1;
    render(&mut app, &mut terminal);
    assert_selected(&app, &terminal, (100, 0), "3/4 changes");
    request_accept(&mut app);
    let file = &mut Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0];
    file.chunks.retain(|chunk| chunk.edit != 100);
    app.chat.rollback.inline_revision += 1;
    render(&mut app, &mut terminal);
    assert_selected(&app, &terminal, (61, 7), "3/3 changes");
    assert_eq!(app.editor().unwrap().cursor_line, 48);
    request_accept(&mut app);
    let file = &mut Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0];
    file.chunks.retain(|chunk| chunk.edit != 61);
    app.chat.rollback.inline_revision += 1;
    render(&mut app, &mut terminal);
    assert_selected(&app, &terminal, (900, 0), "1/2 changes");
}

#[test]
fn editor_ai_review_completed_decision_does_not_move_a_user_editing_caret() {
    for typing in [false, true] {
        let mut app = multi_pending();
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        render(&mut app, &mut terminal);
        request_accept(&mut app);
        let editor = app.editor_mut().unwrap();
        editor.set_cursor(9, 3);
        if typing {
            editor.paste_with_text("USER");
        }
        let cursor = (editor.cursor_line, editor.cursor_col);
        let text = editor.rope.to_string();
        Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0]
            .chunks
            .retain(|chunk| chunk.edit != 42);
        app.chat.rollback.inline_revision += 1;
        render(&mut app, &mut terminal);
        assert_eq!(
            (
                app.editor().unwrap().cursor_line,
                app.editor().unwrap().cursor_col
            ),
            cursor
        );
        assert_eq!(app.editor().unwrap().rope.to_string(), text);
        assert!(app.editor().unwrap().ai_review.advance_requested.is_none());
    }
}

#[test]
fn editor_ai_review_accept_keeps_its_next_chunk_when_an_earlier_change_arrives() {
    let mut app = multi_pending();
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    render(&mut app, &mut terminal);
    arrow(&mut app, true);
    render(&mut app, &mut terminal);
    request_accept(&mut app);
    let file = &mut Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0];
    file.chunks.retain(|chunk| chunk.edit != 100);
    file.chunks.push(InlineChunk {
        edit: 900,
        index: 0,
        range: Some(0..0),
        original: "earlier removal\n".into(),
        line: 1,
        issue: None,
    });
    app.chat.rollback.inline_revision += 1;
    render(&mut app, &mut terminal);
    assert_selected(&app, &terminal, (61, 7), "3/3 changes");
    assert_eq!(app.editor().unwrap().cursor_line, 48);
}

#[test]
fn editor_ai_review_stale_navigation_does_not_select_another_chunk() {
    let mut app = multi_pending();
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    render(&mut app, &mut terminal);
    // Resolve the destination after rendering, before the queued pointer event.
    Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0]
        .chunks
        .retain(|chunk| chunk.edit != 100);
    app.chat.rollback.inline_revision += 1;
    arrow(&mut app, true);
    assert_eq!(app.editor().unwrap().cursor_line, 0);
    assert_eq!(app.editor().unwrap().ai_review.selected, Some((42, 0)));
    render(&mut app, &mut terminal);
    assert_selected(&app, &terminal, (42, 0), "1/2 changes");
}

#[test]
fn editor_ai_review_manual_cursor_and_scrolling_update_the_current_chunk() {
    let mut app = multi_pending();
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    render(&mut app, &mut terminal);
    app.editor_mut().unwrap().set_cursor(22, 0);
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().ai_review.selected, Some((100, 0)));
    let scrollbar = app.scrollbar_area.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        scrollbar.right() - 1,
        scrollbar.bottom() - 1,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        scrollbar.right() - 1,
        scrollbar.bottom() - 1,
    );
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().ai_review.selected, Some((61, 7)));
    assert!(frame_text(&terminal).contains("3/3 changes"));
}

#[test]
fn editor_ai_review_navigation_works_in_inactive_split_and_narrow_toolbar() {
    let mut app = multi_pending();
    let mut other = Editor::test_with_text("other file\n");
    other.file_path = Some(std::path::absolute("other-review.txt").unwrap());
    app.tabs.push(crate::app::Tab::new_file(other));
    app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
    let active = app.active_group;
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    terminal
        .draw(|f| crate::ui_chrome::draw_editor_section(f, &mut app, f.area()))
        .unwrap();
    let (group, view) = app
        .groups
        .iter()
        .enumerate()
        .filter(|(id, _)| *id != active)
        .find_map(|(id, group)| {
            let view = &group.tabs[group.active_tab].editor.ai_review;
            view.active.then_some((id, view))
        })
        .unwrap();
    for hit in &view.navigation_hits {
        assert!(view.area.contains((hit.area.x, hit.area.y).into()));
    }
    let hit = view.navigation_hits[1].clone();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        hit.area.x,
        hit.area.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        hit.area.x,
        hit.area.y,
    );
    assert_eq!(app.active_group, group);
    assert_eq!(app.editor().unwrap().cursor_line, 22);
    assert_eq!(app.editor().unwrap().ai_review.selected, Some((100, 0)));
    for width in [20, 30, 40, 60, 80, 140] {
        let mut app = multi_pending();
        let mut terminal = Terminal::new(TestBackend::new(width, 16)).unwrap();
        render(&mut app, &mut terminal);
        arrow(&mut app, true);
        render(&mut app, &mut terminal);
        assert_selected(&app, &terminal, (100, 0), "2/3 changes");
        let view = &app.editor().unwrap().ai_review;
        for hit in &view.hits {
            assert!(view.area.contains((hit.area.x, hit.area.y).into()));
        }
    }
}
