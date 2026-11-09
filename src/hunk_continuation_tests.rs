use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn document(changed: [bool; 3]) -> String {
    format!(
        "header\n{}\nkeep 1\nkeep 2\n{}\nkeep 3\nkeep 4\n{}\ntail\n",
        if changed[0] { "FIRST" } else { "first" },
        if changed[1] { "MIDDLE" } else { "middle" },
        if changed[2] { "LAST" } else { "last" }
    )
}

fn click_selected(app: &mut App, symbol: &str) -> (u16, u16) {
    let mut terminal = Terminal::new(TestBackend::new(130, 60)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_editor::draw_editor(frame, app, Rect::new(10, 3, 110, 48));
            crate::ui_hunk_peek::draw(frame, app);
        })
        .unwrap();
    let area = app.hunk_popup_rect(130, 60);
    let hits: Vec<_> = (area.y + 2..area.bottom() - 1)
        .flat_map(|row| (area.x..area.right()).map(move |col| (col, row)))
        .filter(|&(col, row)| terminal.backend().buffer()[(col, row)].symbol() == symbol)
        .collect();
    let peek = app.hunk_peek.as_ref().unwrap();
    let visible_before = peek.hunks[..peek.selected]
        .iter()
        .filter(|hunk| hunk.end_row > peek.scroll)
        .count();
    let (column, row) = hits[visible_before];
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
    (column, row)
}

#[test]
fn gutter_actions_advance_to_the_next_chunk_wrap_and_close_after_the_last() {
    for action in ["stage", "unstage", "discard"] {
        let original = document([false; 3]);
        let changed = document([true; 3]);
        let (root, file, mut app) = make_git_app_with_file(action, &original, &changed);
        let staged = action == "unstage";
        if staged {
            run_git(&root, &["add", "file.txt"]);
        }
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.open_hunk_popup(10, 4, file.clone(), 4, if staged { 'S' } else { 'U' });
        app.wait_for_hunk_preview();
        assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 1);
        let mut index = [staged; 3];
        let mut worktree = [true; 3];
        for (step, which) in [1, 2, 0].into_iter().enumerate() {
            let symbol = if action == "stage" { "+" } else { "−" };
            let (column, row) = click_selected(&mut app, symbol);
            assert!(
                app.hunk_popup_active,
                "{action} must retain the view while completing"
            );
            if app.bg_git_action_running {
                let generation = app.hunk_popup_generation;
                crate::mouse::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column,
                        row,
                        modifiers: KeyModifiers::NONE,
                    },
                );
                assert_eq!(app.hunk_popup_generation, generation);
                assert!(app.hunk_preview_action_pending());
            }
            app.wait_for_git_action();
            app.wait_for_hunk_preview();
            match action {
                "stage" => index[which] = true,
                "unstage" => index[which] = false,
                _ => worktree[which] = false,
            }
            assert_eq!(
                git_stdout(&root, &["show", ":file.txt"]),
                document(index),
                "{action}/{step}"
            );
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                document(worktree),
                "{action}/{step}"
            );
            assert_eq!(app.editor().unwrap().rope.to_string(), document(worktree));
            if step < 2 {
                assert!(app.hunk_popup_active);
                assert_eq!(app.hunk_peek.as_ref().unwrap().hunks.len(), 2 - step);
                assert_eq!(
                    app.hunk_peek.as_ref().unwrap().selected,
                    usize::from(step == 0)
                );
                assert_eq!(app.hunk_popup_line, if step == 0 { 7 } else { 1 });
            } else {
                assert!(!app.hunk_popup_active);
                assert!(app.hunk_peek.is_none() && app.hunk_preview_continuation.is_none());
            }
        }
        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn closing_a_preview_during_staging_prevents_late_automatic_navigation() {
    let original = document([false; 3]);
    let changed = document([true; 3]);
    let (root, file, mut app) = make_git_app_with_file("stage-close", &original, &changed);
    app.open_hunk_popup(10, 4, file, 1, 'U');
    app.wait_for_hunk_preview();
    app.execute_hunk_popup_action(0);
    assert!(app.hunk_preview_action_pending());
    app.close_hunk_popup();
    app.wait_for_git_action();
    assert!(!app.hunk_popup_active);
    assert!(app.hunk_peek.is_none() && app.hunk_preview_feedback.is_none());
    assert!(app.hunk_preview_continuation.is_none());
    assert_eq!(
        git_stdout(&root, &["show", ":file.txt"]),
        document([true, false, false])
    );
    drop(app);
    let _ = std::fs::remove_dir_all(root);
}
