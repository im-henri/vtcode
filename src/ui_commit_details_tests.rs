use super::*;
use crate::commit_details::{CommitData, CommitDiff, CommitFile};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> App {
    let mut app = App::test_new();
    let mut tab = crate::app::Tab::new_file(crate::editor::Editor::new());
    tab.kind = crate::app::TabKind::CommitDetails;
    tab.editor.read_only = true;
    app.tabs.push(tab);
    app.active_tab = 0;
    app.active_pane = crate::app::ActivePane::Editor;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    // Full-file fixtures continue to exercise both native diff renderers;
    // the product review defaults to changes plus three context lines.
    app.commit_details.diff_context.changes_only = false;
    app.commit_details.data = CommitData {
        hash: "a4c8d21e03ac392f243b67e0caf093f15a25ae31".into(),
        message: "Add keyboard navigation to Source Control\n\nKeep commit history, changed files, and the patch in one place.\n\nUse Tab to move between panes and Alt+Left / Alt+Right to browse commits. Preserve focus when the terminal is resized.\n\nReviewed-by: Ada Lovelace".into(),
        author: "Alex Morgan".into(), email: "alex@example.com".into(), time_ago: "2h ago".into(),
        date: "08 Oct 2026, 14:32 +03:00".into(), parents: vec!["28bc450e03ac392f243b67e0caf093f15a25ae31".into()],
        references: vec![crate::app::GitCommitReference { name: "main".into(), remote: false }, crate::app::GitCommitReference { name: "origin/main".into(), remote: true }],
        files: vec![
            CommitFile { status: 'M', path: "src/source_control.rs".into(), added: 42, deleted: 12, ..CommitFile::default() },
            CommitFile { status: 'A', path: "src/commit_details.rs".into(), added: 128, deleted: 0, ..CommitFile::default() },
            CommitFile { status: 'R', path: "src/ui/history.rs".into(), old_path: Some("src/log.rs".into()), added: 8, deleted: 4, ..CommitFile::default() },
            CommitFile { status: 'M', path: "tests/navigation.rs".into(), added: 24, deleted: 3, ..CommitFile::default() },
            CommitFile { status: 'M', path: "README.md".into(), added: 5, deleted: 1, ..CommitFile::default() },
        ],
    };
    app.commit_details.diff_file = Some(0);
    app.tabs[0].editor.file_path = Some(std::path::PathBuf::from("Commit a4c8d21"));
    let prefix = (1..24)
        .map(|line| format!("// Context line {line}\n"))
        .collect::<String>();
    let base = format!(
        "{prefix}fn handle_source_control_key() {{\n    match key.code {{\n        KeyCode::Enter => open_commit_popup(),\n        _ => {{}}\n    }}\n}}\n"
    );
    let target = format!(
        "{prefix}fn handle_source_control_key() {{\n    match key.code {{\n        KeyCode::Enter => open_commit_inspector(),\n        KeyCode::Tab => focus_next_pane(),\n        KeyCode::Up => select_previous_file(),\n        KeyCode::Down => select_next_file(),\n        _ => {{}}\n    }}\n}}\n"
    );
    set_diff(&mut app, &base, &target);
    app
}

fn review_fixture(files: usize) -> App {
    let mut app = fixture();
    app.commit_details.data.files = (0..files)
        .map(|index| CommitFile {
            status: 'M',
            path: format!("src/review_{index}.rs"),
            added: 1,
            deleted: 1,
            ..Default::default()
        })
        .collect();
    app.commit_details.diff_context.changes_only = true;
    for index in 0..files {
        app.stash_commit_review_file();
        app.commit_details.diff_file = Some(index);
        let diff = crate::commit_details::prepare_diff(crate::commit_details::DiffContents {
            path: app.explorer.root.join(format!("src/review_{index}.rs")),
            base: format!("fn previous_{index}() {{\n    let value = 1;\n}}\n"),
            target: format!("fn current_{index}() {{\n    let value = 2;\n}}\n"),
            labels: crate::app::DiffLabels::new("Parent", "Commit"),
        });
        app.install_commit_diff(diff);
    }
    app.activate_commit_review_file(0);
    app.commit_details.selected = 0;
    app
}

#[test]
fn commit_review_is_two_horizontal_regions_on_small_and_large_screens() {
    for (width, height) in [(40, 18), (60, 24), (80, 24), (110, 34), (180, 44)] {
        let mut app = review_fixture(3);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let view = current_layout(&app);
        assert_eq!(view.overview.x, view.files.x);
        assert_eq!(view.overview.width, view.files.width);
        assert_eq!(view.primary_divider.height, 1);
        assert!(view.overview.bottom() <= view.files.y);
        assert!(view.files.height >= 4, "{width}x{height}: {view:?}");
        assert_eq!(app.commit_details.diff_file, Some(0));
        assert!(app.code_area.is_some());
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(),
            &format!("commit-review-{width}x{height}"),
        );
    }
}

#[test]
fn commit_review_collapse_expand_all_and_keyboard_are_consistent() {
    let mut app = review_fixture(4);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let view = current_layout(&app);
    click(&mut app, view.collapse_all.x + 1, view.collapse_all.y);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.commit_details.review.total, 4);
    assert_eq!(app.commit_details.review.visible.len(), 4);
    assert!(app.commit_details.review.pending.is_none());
    assert!(
        app.code_area.is_none(),
        "collapsed files cannot retain native hit areas"
    );
    let second = app.commit_details.review.visible[1];
    click(&mut app, second.header.x + 2, second.header.y);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.commit_details.selected_file(), Some(1));
    assert!(!app.commit_details.review.collapsed.contains(&1));
    assert!(app.commit_details.review.collapsed.contains(&0));
    handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert!(app.commit_details.review.collapsed.contains(&1));
    handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(!app.commit_details.review.collapsed.contains(&1));
    click(&mut app, view.expand_all.x + 1, view.expand_all.y);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.commit_details.review.collapsed.is_empty());
    assert!(app.commit_details.review.total > view.files.height as usize);
}

#[test]
fn commit_review_one_scrollbar_crosses_files_and_restores_native_selection() {
    let mut app = review_fixture(5);
    let mut terminal = Terminal::new(TestBackend::new(90, 34)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let first = app.commit_details.diff_full_cache.clone().unwrap();
    let code = app.code_area.unwrap();
    for (kind, x) in [
        (MouseEventKind::Down(MouseButton::Left), code.x),
        (MouseEventKind::Drag(MouseButton::Left), code.x + 2),
        (MouseEventKind::Up(MouseButton::Left), code.x + 2),
    ] {
        crate::mouse::handle_mouse(&mut app, mouse_at(kind, x, code.y));
    }
    assert_eq!(app.diff_selected_text().as_deref(), Some("fn"));
    let area = current_layout(&app).files;
    for _ in 0..9 {
        assert!(handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::ScrollDown, area.x + 10, area.y + 3)
        ));
        terminal.draw(|f| draw(f, &mut app)).unwrap();
    }
    assert!(
        app.commit_details
            .review
            .visible
            .iter()
            .any(|card| card.file >= 3)
    );
    assert_eq!(
        app.commit_details.selected_file(),
        Some(0),
        "wheel motion must not change keyboard focus or selected file"
    );
    assert_eq!(app.diff_selected_text().as_deref(), Some("fn"));
    assert!(
        std::sync::Arc::ptr_eq(&first, app.commit_details.diff_full_cache.as_ref().unwrap()),
        "scrolling must move native views rather than rebuild caches"
    );
    assert!(
        app.scrollbar_area.is_none(),
        "only the review owns a vertical scrollbar"
    );
    app.commit_details.review.scroll = 0;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.diff_selected_text().as_deref(), Some("fn"));
}

#[test]
fn commit_review_clicking_another_card_uses_its_geometry_and_source() {
    let mut app = review_fixture(3);
    app.commit_details.diff_expanded = true;
    let mut terminal = Terminal::new(TestBackend::new(90, 44)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let second = *app
        .commit_details
        .review
        .visible
        .iter()
        .find(|card| card.file == 1)
        .unwrap();
    assert!(!second.body.is_empty());
    let y = second.body.y + 1;
    let x = second.body.x + 12;
    for (kind, x) in [
        (MouseEventKind::Down(MouseButton::Left), x),
        (MouseEventKind::Drag(MouseButton::Left), x + 2),
        (MouseEventKind::Up(MouseButton::Left), x + 2),
    ] {
        crate::mouse::handle_mouse(&mut app, mouse_at(kind, x, y));
    }
    assert_eq!(app.commit_details.selected_file(), Some(1));
    assert_eq!(app.commit_details.diff_file, Some(1));
    assert!(app.editor().unwrap().rope.to_string().contains("current_1"));
    assert!(app.diff_selected_text().is_some());
    let selection = app.diff_selected_text();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.diff_selected_text(), selection);
}

#[test]
fn commit_review_does_not_read_collapsed_files_and_limits_pending_requests() {
    let mut app = fixture();
    app.commit_details.active = true;
    app.commit_details.review.collapsed = (0..app.commit_details.data.files.len()).collect();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.commit_details.review.pending.is_none());
    assert!(app.code_area.is_none());
    app.request_commit_diff();
    assert!(app.commit_details.review.pending.is_none());
    app.commit_details.review.collapsed.remove(&1);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.commit_details.review.pending, Some(1));
    for _ in 0..10 {
        terminal.draw(|f| draw(f, &mut app)).unwrap();
    }
    assert_eq!(app.commit_details.review.pending, Some(1));
    app.commit_details.review.collapsed.insert(1);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(
        app.commit_details.review.pending,
        Some(1),
        "one existing read may finish, but must not fan out"
    );
}

#[test]
fn commit_review_late_neighbour_results_do_not_replace_active_source_or_selection() {
    let mut app = review_fixture(1);
    app.commit_details.active = true;
    app.commit_details
        .data
        .files
        .push(CommitFile::new('M', "second.rs"));
    app.commit_details.review.pending = Some(1);
    let original = app.editor().unwrap().rope.clone();
    let path = app.explorer.root.join("second.rs");
    let snapshot = || {
        crate::commit_details::prepare_diff(crate::commit_details::DiffContents {
            path: path.clone(),
            base: "old\n".into(),
            target: "new\n".into(),
            labels: crate::app::DiffLabels::new("Parent", "Commit"),
        })
    };
    let root = app.source_control_root().to_owned();
    let generation = app.commit_details.generation;
    let diff = snapshot();
    assert!(!app.apply_commit_diff(crate::commit_details::DiffResult {
        root: root.clone(),
        generation: generation.wrapping_sub(1),
        file: 1,
        result: Ok(diff)
    }));
    assert_eq!(app.commit_details.review.pending, Some(1));
    let diff = snapshot();
    assert!(app.apply_commit_diff(crate::commit_details::DiffResult {
        root,
        generation,
        file: 1,
        result: Ok(diff)
    }));
    assert!(app.commit_details.review.cached.contains_key(&1));
    assert_eq!(app.commit_details.diff_file, Some(0));
    assert_eq!(app.editor().unwrap().rope, original);
    assert!(app.commit_details.review.pending.is_none());
}

#[test]
fn commit_review_paging_native_code_scrolls_the_review_without_changing_focus() {
    let mut app = review_fixture(12);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    app.commit_details.focus = Focus::Diff;
    let key = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
    assert!(owns_key(&app, key));
    handle_key(&mut app, key);
    let offset = app.commit_details.review.scroll;
    assert!(offset > 0);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.commit_details.review.scroll, offset);
    assert_eq!(app.commit_details.focus, Focus::Diff);
    assert_eq!(app.commit_details.selected_file(), Some(0));
    assert!(app.commit_details.review.visible.first().unwrap().file > 0);
}

#[test]
fn commit_review_shift_wheel_pans_the_hovered_file_without_stealing_selection() {
    let mut app = review_fixture(3);
    app.activate_commit_review_file(1);
    app.install_commit_diff(crate::commit_details::prepare_diff(
        crate::commit_details::DiffContents {
            path: app.explorer.root.join("src/review_1.rs"),
            base: "before\n".into(),
            target: format!("{} after\n", "long text ".repeat(100)),
            labels: crate::app::DiffLabels::new("Parent", "Commit"),
        },
    ));
    app.activate_commit_review_file(0);
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let card = *app
        .commit_details
        .review
        .visible
        .iter()
        .find(|card| card.file == 1)
        .unwrap();
    let scroll = app.commit_details.review.scroll;
    assert!(handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            modifiers: KeyModifiers::SHIFT,
            column: card.body.x + 15,
            row: card.body.y + 1,
        }
    ));
    assert_eq!(app.commit_details.selected_file(), Some(0));
    assert_eq!(app.commit_details.diff_file, Some(0));
    assert_eq!(app.commit_details.review.scroll, scroll);
    app.activate_commit_review_file(1);
    assert!(app.editor().unwrap().horizontal_scroll > 0);
    app.activate_commit_review_file(0);
    assert_eq!(app.editor().unwrap().horizontal_scroll, 0);
}

#[test]
fn commit_review_bounds_cached_revisions_and_preserves_visible_files() {
    let mut app = review_fixture(20);
    app.commit_details.review.visible = [1, 2]
        .into_iter()
        .map(|file| crate::commit_review::Card {
            file,
            ..Default::default()
        })
        .collect();
    let source = app.editor().unwrap().rope.clone();
    app.evict_commit_review_files();
    assert_eq!(app.commit_details.review.cached.len(), 8);
    assert!(app.commit_details.review.cached.contains_key(&1));
    assert!(app.commit_details.review.cached.contains_key(&2));
    assert_eq!(app.editor().unwrap().rope, source);
    app.reset_commit_details();
    assert!(app.commit_details.review.cached.is_empty());
    assert!(app.commit_details.review.pending.is_none());
}

#[test]
fn commit_review_adapts_to_small_screens_and_keeps_layout_choices_when_panes_reset() {
    let mut app = review_fixture(2);
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    activate(&mut app, 9);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.diff_mode == crate::app::DiffMode::SideBySide);
    assert!(app.diff_left_code_area.is_some());
    activate(&mut app, 8);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.diff_mode == crate::app::DiffMode::SideBySide);
    // Resetting the review panes must not reset the user's saved diff layout.
    activate(&mut app, 9);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    activate(&mut app, 8);
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
}

#[test]
fn commit_review_find_context_survives_neighbour_paints_and_closes_cleanly() {
    let mut app = review_fixture(3);
    let base = (0..20).map(|line| format!("let value_{line} = {line};\n")).collect::<String>();
    let target = base.replace("value_2 ", "changed_2 ").replace("value_18 ", "changed_18 ");
    app.set_commit_diff_context(true, 0);
    set_diff(&mut app, &base, &target);
    app.commit_details.focus = Focus::Diff;
    app.search_active = true;
    app.search_input_focused = false;
    app.search_matches = vec![(10, 0, 3)];
    app.editor_mut().unwrap().set_cursor(10, 0);
    let mut terminal = Terminal::new(TestBackend::new(90, 50)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.commit_details.review.visible.iter().any(|card| card.file != 0));
    assert_eq!(app.commit_details.context_revealed_line, Some(10));
    assert!(app.diff_render_cache.visible_target_lines.contains(&10));
    app.search_active = false;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(app.commit_details.context_revealed_line, None);
    assert!(!app.diff_render_cache.visible_target_lines.contains(&10));
    assert!(app.commit_details.diff_context.changes_only);
    assert_eq!(app.commit_details.diff_context.lines, 0);
    assert_eq!(app.editor().unwrap().rope.to_string(), target);
}

#[test]
fn ui_combinations_find_focus_does_not_block_commit_review_wheel_or_card_controls() {
    let mut app = review_fixture(6);
    app.search_active = true;
    app.search_input_focused = true;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let view = current_layout(&app);
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(
            MouseEventKind::ScrollDown,
            view.files.x + 2,
            view.files.bottom() - 2,
        ),
    );
    assert!(app.commit_details.review.scroll > 0);
    assert!(
        app.search_input_focused,
        "wheel input preserves the Find field focus"
    );
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let card = app.commit_details.review.visible[0];
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(
            MouseEventKind::Down(MouseButton::Left),
            card.header.x + 2,
            card.header.y,
        ),
    );
    assert!(app.commit_details.review.collapsed.contains(&card.file));
    assert!(app.search_active);
    assert!(
        !app.search_input_focused,
        "clicking a card returns keyboard focus to the review"
    );
}

#[test]
fn commit_review_stale_header_cannot_open_a_different_filtered_file() {
    let mut app = review_fixture(3);
    let mut terminal = Terminal::new(TestBackend::new(90, 44)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let old = *app.commit_details.review.visible.iter().find(|card| card.file == 1).unwrap();
    app.commit_details.edit_filter(KeyCode::Null, KeyModifiers::NONE, Some("review_0.rs"));
    click(&mut app, old.open.x + 1, old.open.y);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.commit_details.selected_file(), Some(0));
    assert_eq!(app.commit_details.diff_file, Some(0));
}

fn set_diff(app: &mut App, base: &str, target: &str) {
    let lines = crate::app::compute_file_diff(base, target);
    let cache = crate::app::DiffRenderCache::new(&lines);
    let target = ropey::Rope::from_str(target);
    let markers =
        crate::app_git_display::target_line_markers_from_compare_diff(&lines, target.len_lines());
    app.commit_details.diff_file = app.commit_details.selected_file();
    app.commit_details.diff_loading = false;
    app.install_commit_diff(CommitDiff {
        path: app.explorer.root.join("src/source_control.rs"),
        target,
        labels: crate::app::DiffLabels::new("28bc450 (parent)", "a4c8d21 (commit)"),
        prepared: crate::background::PreparedDiffRefresh {
            base_content: base.into(),
            diff_lines: lines,
            render_cache: cache,
            target_line_markers: markers,
        },
    });
}

#[test]
fn commit_copy_feedback_is_visible_for_both_actions_without_moving_controls() {
    for action in [2, 3] {
        for copied in [true, false] {
            for width in [40, 80, 150] {
                let mut app = fixture();
                app.commit_details.data.message =
                    "Fix commit feedback 世界\n\nPreserve the complete message.\n".into();
                let expected_value = if action == 2 {
                    app.commit_details.data.hash.clone()
                } else {
                    app.commit_details.data.message.clone()
                };
                let mut terminal = Terminal::new(TestBackend::new(width, 26)).unwrap();
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let before = current_layout(&app);
                let mut received = String::new();
                copy_commit_value_with(&mut app, action, |text| {
                    received = text.into();
                    copied
                });
                assert_eq!(
                    received, expected_value,
                    "copy full text without truncating or normalizing it"
                );
                let feedback = app.commit_details.copy_feedback.as_ref().unwrap();
                assert_eq!(feedback.action, action);
                assert_eq!(feedback.copied, copied);
                let expires_at = feedback.expires_at;
                assert!(
                    !app.commit_details
                        .expire_copy_feedback(std::time::Instant::now())
                );
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let after = current_layout(&app);
                assert_eq!(after.buttons, before.buttons);
                assert_eq!(after.body, before.body);
                assert!(
                    after
                        .buttons
                        .iter()
                        .all(|button| button.bottom() <= after.feedback.y)
                );
                let text_at = |area: Rect| {
                    (area.x..area.right())
                        .map(|x| terminal.backend().buffer()[(x, area.y)].symbol())
                        .collect::<String>()
                };
                let confirmation = text_at(after.feedback);
                let button = text_at(after.buttons[action]);
                if copied {
                    assert!(confirmation.contains(if action == 2 {
                        "Copied full commit SHA"
                    } else {
                        "Copied commit message"
                    }));
                    assert!(button.contains('✓'));
                } else {
                    assert!(confirmation.contains(if action == 2 {
                        "Could not copy SHA"
                    } else {
                        "Could not copy message"
                    }));
                    assert!(button.contains(if after.compact { "!" } else { "Failed" }));
                    assert!(!button.contains('✓'));
                    assert!(matches!(
                        app.notification_history.last().unwrap().1,
                        crate::app::NotifyLevel::Error
                    ));
                }
                let other = if action == 2 { 3 } else { 2 };
                assert!(
                    text_at(after.buttons[other]).contains(if other == 2 {
                        "SHA"
                    } else {
                        "message"
                    }) || after.compact && text_at(after.buttons[other]).contains("Message")
                );
                crate::ui_control_visibility_tests::snapshot(
                    terminal.backend().buffer(),
                    &format!("commit-copy-{action}-{copied}-{width}"),
                );

                assert!(app.commit_details.expire_copy_feedback(expires_at));
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let confirmation: String = (after.feedback.x..after.feedback.right())
                    .map(|x| terminal.backend().buffer()[(x, after.feedback.y)].symbol())
                    .collect();
                assert!(confirmation.trim().is_empty());
                let button: String = (after.buttons[action].x..after.buttons[action].right())
                    .map(|x| terminal.backend().buffer()[(x, after.buttons[action].y)].symbol())
                    .collect();
                assert!(button.contains(if action == 2 {
                    "SHA"
                } else if after.compact {
                    "Message"
                } else {
                    "Copy message"
                }));
            }
        }
    }
}

#[test]
fn commit_copy_feedback_retries_and_resets_without_reporting_disabled_copies() {
    let mut app = fixture();
    app.commit_details.loading = true;
    copy_commit_value_with(&mut app, 3, |_| {
        panic!("incomplete commit message must not be copied")
    });
    assert!(app.commit_details.copy_feedback.is_none());
    app.commit_details.loading = false;
    copy_commit_value_with(&mut app, 3, |_| false);
    let old_deadline = std::time::Instant::now() - std::time::Duration::from_secs(1);
    app.commit_details
        .copy_feedback
        .as_mut()
        .unwrap()
        .expires_at = old_deadline;
    copy_commit_value_with(&mut app, 3, |_| true);
    let feedback = app.commit_details.copy_feedback.as_ref().unwrap();
    assert!(feedback.copied && feedback.expires_at > old_deadline);
    assert_eq!(app.status_message, "Copied commit message");
    app.reset_commit_details();
    assert!(app.commit_details.copy_feedback.is_none());
    copy_commit_value_with(&mut app, 2, |_| panic!("missing SHA must not be copied"));
    assert!(app.commit_details.copy_feedback.is_none());
}

#[test]
fn commit_inspector_changes_only_context_preserves_source_ranges_and_merges_hunks() {
    let base = (0..100)
        .map(|line| format!("let value_{line} = {line};\n"))
        .collect::<String>();
    let target = base
        .replace("value_20", "updated_20")
        .replace("value_80", "updated_80");
    let full = crate::app::DiffRenderCache::new(&crate::app::compute_file_diff(&base, &target));
    for context in [0, 1, 3, 30, usize::MAX] {
        let cache = full.with_context(context);
        let expected: Vec<_> = (0..100)
            .filter(|line: &usize| line.abs_diff(20) <= context || line.abs_diff(80) <= context)
            .collect();
        assert_eq!(cache.visible_target_lines, expected);
        assert_eq!(cache.hunks, full.hunks);
        for line in expected {
            let row = cache.aligned_row_for_target_line(line);
            assert!(cache.rows[row].right_line == Some(line) || cache.rows[row].hidden_lines > 0);
            let inline = cache.inline_row_for_target_line(line);
            assert_eq!(cache.target_line_for_inline_row(inline), line);
        }
        let hidden: usize = cache.rows.iter().map(|row| row.hidden_lines).sum();
        assert_eq!(hidden + cache.visible_target_lines.len(), 100);
        if context >= 30 {
            assert_eq!(hidden, 0, "overlapping context windows must merge");
        }
    }
    let unchanged = crate::app::DiffRenderCache::new(&crate::app::compute_file_diff(&base, &base));
    assert!(unchanged.with_context(3).rows.is_empty());
    let deleted = crate::app::DiffRenderCache::new(&crate::app::compute_file_diff(&base, ""));
    assert_eq!(deleted.with_context(0).rows.len(), 100);
    assert_eq!(deleted.with_context(0).inline_rows.len(), 100);
}

#[test]
fn commit_inspector_context_controls_accept_zero_typed_counts_and_restore_full_file() {
    let mut app = fixture();
    let full = app.diff_render_cache.clone();
    let source = app.editor().unwrap().rope.clone();
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let view = current_layout(&app);
    click(&mut app, view.diff_changes.x + 2, view.diff_changes.y);
    assert!(app.commit_details.diff_context.changes_only);
    assert_eq!(app.diff_render_cache.context_lines, Some(3));
    click(&mut app, view.context_count.x + 1, view.context_count.y);
    for key in [
        KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    ] {
        handle_key(&mut app, key);
    }
    assert_eq!(app.diff_render_cache.context_lines, Some(0));
    assert!(
        app.diff_render_cache
            .rows
            .iter()
            .all(|row| row.hidden_lines > 0 || row.left_kind == '-' || row.right_kind == '+')
    );
    assert!(!enabled(&app, 12));
    handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.commit_details.diff_context.lines, 1);
    paste_context_count(&mut app, "25");
    handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.commit_details.diff_context.lines, 25);
    assert_eq!(app.diff_render_cache.context_lines, Some(25));
    click(&mut app, view.context_count.x, view.context_count.y);
    paste_context_count(&mut app, "9999");
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.commit_details.diff_context.lines, 25);
    click(&mut app, view.diff_full.x, view.diff_full.y);
    assert!(!app.commit_details.diff_context.changes_only);
    assert!(std::sync::Arc::ptr_eq(&full, &app.diff_render_cache));
    assert_eq!(
        app.editor().unwrap().rope,
        source,
        "context is a view; it cannot change a revision"
    );
    app.set_commit_diff_context(true, 7);
    set_diff(&mut app, "fn old() {}\n", "fn new() {}\n");
    assert_eq!(
        app.diff_render_cache.context_lines,
        Some(7),
        "file changes preserve the context choice"
    );
    app.reset_commit_details();
    assert!(app.commit_details.diff_context.changes_only);
    assert_eq!(app.commit_details.diff_context.lines, 7);
}

#[test]
fn commit_inspector_context_uses_native_layouts_syntax_selection_and_scrollbars() {
    for mode in [
        crate::app::DiffMode::SideBySide,
        crate::app::DiffMode::Inline,
    ] {
        let mut app = fixture();
        let base = (0..200)
            .map(|line| format!("let value_{line} = \"line {line}\";\n"))
            .collect::<String>();
        let target = base
            .replace("value_20", "updated_20")
            .replace("value_180", "updated_180");
        set_diff(&mut app, &base, &target);
        app.diff_mode = mode;
        app.preferred_diff_mode = mode;
        app.set_commit_diff_context(true, 3);
        app.commit_details.focus = Focus::Diff;
        let mut terminal = Terminal::new(TestBackend::new(180, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        highlight_diff(&mut app);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let visible = app.diff_render_cache.visible_target_lines.clone();
        assert_eq!(visible, (17..=23).chain(177..=183).collect::<Vec<_>>());
        let code = app.code_area.unwrap();
        let body = app.commit_details.review.visible.iter()
            .find(|card| Some(card.file) == app.commit_details.diff_file).unwrap().body;
        let mode_name = if mode == crate::app::DiffMode::SideBySide {
            "side-by-side"
        } else {
            "inline"
        };
        let panes = if mode == crate::app::DiffMode::SideBySide {
            let layout = crate::ui_diff_display::side_by_side_diff_layout(body, app.diff_split_ratio);
            vec![(layout.left.x, layout.left.right()), (app.gutter_area.unwrap().x, code.right())]
        } else {
            vec![(body.x, code.right())]
        };
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(), &format!("commit-context-gutters-{mode_name}"),
        );
        for (start, end) in panes {
            let mut separators = 0;
            for row in code.y..code.bottom() {
                let text: String = (start..end)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol()).collect();
                if text.contains("unchanged lines hidden") {
                    separators += 1;
                    assert!(text.starts_with("… "),
                        "{mode_name} context separator must start in the line-number gutter: {text:?}");
                    assert!(text.trim_end().ends_with("hidden …"),
                        "{mode_name} context separator must not leave source text behind: {text:?}");
                    assert!((start..end).all(|column|
                        terminal.backend().buffer()[(column, row)].bg == app.colors.bg_dark),
                        "the separator background must span the gutter and code");
                }
            }
            assert!(separators > 0, "the rendered {mode_name} pane must expose a context separator");
        }
        let editor = app.editor().unwrap();
        let screen_row = editor
            .diff_display_rows
            .as_ref()
            .unwrap()
            .iter()
            .position(|line| *line == Some(20))
            .unwrap();
        let x = code.x + editor.diff_text_inset as u16;
        let y = code.y + screen_row as u16;
        for (kind, x) in [
            (MouseEventKind::Down(MouseButton::Left), x),
            (MouseEventKind::Drag(MouseButton::Left), x + 3),
            (MouseEventKind::Up(MouseButton::Left), x + 3),
        ] {
            crate::mouse::handle_mouse(&mut app, mouse_at(kind, x, y));
        }
        assert!(app.has_diff_selection() || app.editor().unwrap().has_selection());
        assert_eq!(
            app.diff_selected_text()
                .or_else(|| app.editor().unwrap().selected_text())
                .as_deref(),
            Some("let")
        );
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let selected = terminal.backend().buffer()[(x, y)].bg;
        assert_ne!(selected, app.colors.bg_diff_added);
        app.editor_mut().unwrap().set_cursor(23, 0);
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        assert_eq!(
            app.editor().unwrap().cursor_line,
            177,
            "navigation skips hidden source lines"
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
        assert_eq!(app.editor().unwrap().cursor_line, 180);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let files = current_layout(&app).files;
        let bar = Rect::new(files.right() - 1, files.y, 1, files.height);
        if app.diff_inline_scroll.row_count() <= app.diff_inline_scroll.text_area.height as usize {
            for row in bar.y..bar.bottom() {
                for column in bar.x..bar.right() {
                    assert!(
                        terminal.backend().buffer()[(column, row)]
                            .symbol()
                            .trim()
                            .is_empty(),
                        "a fitting compact diff must not leave full-file scrollbar glyphs"
                    );
                }
            }
        }
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Down(MouseButton::Left),
                bar.x,
                bar.bottom() - 1,
            ),
        );
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Drag(MouseButton::Left),
                bar.x,
                bar.bottom() + 4,
            ),
        );
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Up(MouseButton::Left),
                bar.x,
                bar.bottom() + 4,
            ),
        );
        assert!(!app.dragging_v_scrollbar);
        assert_eq!(
            app.commit_details.review.scroll,
            app.commit_details
                .review
                .total
                .saturating_sub(current_layout(&app).files.height as usize)
        );
        assert_eq!(app.editor().unwrap().rope.to_string(), target);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(),
            if mode == crate::app::DiffMode::Inline {
                "commit-context-inline"
            } else {
                "commit-context-side-by-side"
            },
        );
    }
}

fn highlight_diff(app: &mut App) {
    app.refresh_visible_highlights_async();
    assert!(
        app.visible_highlight_key.is_some(),
        "commit tabs must schedule syntax highlighting"
    );
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        let editor = app.editor().unwrap();
        let start = app
            .diff_render_cache
            .aligned_row_for_target_line(editor.scroll_offset);
        app.diff_render_cache
            .rows
            .iter()
            .skip(start)
            .take(editor.viewport_height.max(50) + 25)
            .all(|row| {
                [
                    (row.left_line, row.left_highlight_hash()),
                    (row.right_line, row.right_highlight_hash()),
                ]
                .into_iter()
                .all(|(line, hash)| {
                    line.is_none_or(|line| {
                        app.highlighter
                            .highlighted_fragment_by_hash_if_cached(
                                hash,
                                line,
                                editor.file_path.as_deref(),
                                editor.syntax_language_token(),
                                &app.syntax_theme,
                            )
                            .is_some()
                    })
                })
            })
    });
}

#[test]
fn commit_inspector_context_reveals_find_results_without_changing_the_setting() {
    for mode in [
        crate::app::DiffMode::Inline,
        crate::app::DiffMode::SideBySide,
    ] {
        let mut app = fixture();
        let base = (0..100)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect::<String>();
        let target = base.replace("value_20", "updated_20");
        set_diff(&mut app, &base, &target);
        app.diff_mode = mode;
        app.preferred_diff_mode = mode;
        app.set_commit_diff_context(true, 3);
        app.commit_details.focus = Focus::Diff;
        let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(!app.diff_render_cache.visible_target_lines.contains(&70));
        app.search_active = true;
        app.search_query = "value_70".into();
        app.update_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_file_search_running && !app.file_search_refresh_pending
        });
        assert_eq!(app.search_matches.len(), 1);
        app.jump_to_current_match();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(app.editor().unwrap().cursor_line, 70);
        assert_eq!(app.commit_details.context_revealed_line, Some(70));
        assert!(app.diff_render_cache.visible_target_lines.contains(&70));
        assert_eq!(app.diff_render_cache.context_lines, Some(3));
        assert_eq!(app.diff_render_cache.hunks.len(), 1);
        assert!(
            app.editor()
                .unwrap()
                .diff_display_rows
                .as_ref()
                .unwrap()
                .contains(&Some(70))
        );
        app.search_active = false;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(app.commit_details.context_revealed_line, None);
        assert!(!app.diff_render_cache.visible_target_lines.contains(&70));
        assert_eq!(app.diff_render_cache.context_lines, Some(3));
        assert_eq!(app.editor().unwrap().rope.to_string(), target);
    }
}

#[test]
fn commit_inspector_context_scrolls_in_inactive_editor_groups() {
    for mode in [
        crate::app::DiffMode::Inline,
        crate::app::DiffMode::SideBySide,
    ] {
        let mut app = fixture();
        let base = (0..200)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect::<String>();
        let target = base
            .replace("value_20", "updated_20")
            .replace("value_180", "updated_180");
        set_diff(&mut app, &base, &target);
        app.diff_mode = mode;
        app.preferred_diff_mode = mode;
        app.set_commit_diff_context(true, 3);
        add_code_tab(&mut app);
        app.create_split_group_dir(0, crate::app::SplitDirection::Horizontal);
        let inspector_group = app.active_group;
        app.switch_group(0);
        let mut terminal = Terminal::new(TestBackend::new(180, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let code = app.groups[inspector_group].code_area.unwrap();
        let before = app.commit_details.review.scroll;
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::ScrollDown, code.x + 4, code.y + 1),
        );
        assert_eq!(
            app.active_group, 0,
            "scrolling must preserve keyboard focus"
        );
        assert!(app.commit_details.review.scroll > before);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Down(MouseButton::Left),
                code.x + 4,
                code.y + 1,
            ),
        );
        assert_eq!(app.active_group, inspector_group);
        assert!(app.context_diff_active());
        assert_eq!(app.editor().unwrap().rope.to_string(), target);
    }
}

#[test]
fn commit_inspector_responsive_layout_stays_inside_terminal() {
    let mut app = fixture();
    for width in [0, 1, 8, 24, 40, 60, 80, 100, 140, 220] {
        for height in [0, 1, 5, 12, 18, 24, 32, 45] {
            for expanded in [false, true] {
                app.commit_details.diff_expanded = expanded;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let view = app.commit_details.layout.as_ref().unwrap();
                for rect in [
                    view.area,
                    view.overview,
                    view.files,
                    view.patch,
                    view.title,
                    view.feedback,
                    view.primary_divider,
                    view.filter_clear,
                    view.hunk_previous,
                    view.hunk_next,
                    view.diff_full,
                    view.diff_changes,
                    view.context_label,
                    view.context_less,
                    view.context_count,
                    view.context_more,
                    view.filter,
                ] {
                    if rect.width > 0 && rect.height > 0 {
                        assert!(
                            rect.right() <= width && rect.bottom() <= height,
                            "{width}x{height}: {rect:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn commit_inspector_preview_and_themes() {
    let mut failures = Vec::new();
    for theme in crate::ui_control_visibility_tests::audit_themes() {
        let mut app = fixture();
        app.colors = theme.to_theme_colors();
        for (width, height) in [(80, 26), (110, 34), (150, 44)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let view = app.commit_details.layout.as_ref().unwrap();
            crate::ui_control_visibility_tests::check_region(
                terminal.backend().buffer(),
                view.area,
                &format!("{} inspector {width}x{height}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            let buffer = terminal.backend().buffer();
            let selected_foreground = buffer[(view.files.x + 3, view.files.y)].fg;
            for x in view.files.x..view.files.right() - 4 {
                let cell = &buffer[(x, view.files.y)];
                if !cell.symbol().trim().is_empty() {
                    assert_eq!(
                        cell.fg, selected_foreground,
                        "{}: selection text must use its own foreground",
                        theme.name
                    );
                }
            }
            crate::ui_control_visibility_tests::snapshot(
                terminal.backend().buffer(),
                &format!("commit-inspector-{}-{width}", theme.name),
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn commit_inspector_reuses_both_native_diff_layouts_and_keeps_plain_dividers() {
    let mut app = fixture();
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let view = current_layout(&app);
    assert!(native_diff_ready(&app));
    assert!(app.diff_split_area.is_some());
    assert!(app.diff_left_code_area.is_some());
    assert!(view.patch.contains(app.code_area.unwrap().as_position()));
    for (area, symbol) in [(view.primary_divider, "─")] {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), symbol);
            }
        }
    }
    let cache = app.diff_render_cache.clone();
    let cursor = app.editor().unwrap().cursor_line;
    activate(&mut app, 9);
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    highlight_diff(&mut app);
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    assert!(std::sync::Arc::ptr_eq(&cache, &app.diff_render_cache));
    assert_eq!(app.editor().unwrap().cursor_line, cursor);
    assert!(app.diff_inline_scroll.area.height > 0);
    assert!(app.scrollbar_area.is_none());
    crate::ui_control_visibility_tests::snapshot(
        terminal.backend().buffer(),
        "commit-native-diff-inline",
    );
    activate(&mut app, 9);
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert!(app.diff_mode == crate::app::DiffMode::SideBySide);
}

#[test]
fn commit_inspector_highlights_both_revisions_in_both_native_layouts() {
    let mut app = fixture();
    app.syntax_theme = "dark-plus".into();
    app.show_minimap = false;
    set_diff(
        &mut app,
        "fn demo() {\n    let message = \"before\";\n    // previous comment\n}\n",
        "fn demo() {\n    let message = \"after\";\n    // updated comment\n}\n",
    );
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    for mode in [
        crate::app::DiffMode::SideBySide,
        crate::app::DiffMode::Inline,
    ] {
        app.diff_mode = mode;
        app.preferred_diff_mode = mode;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        highlight_diff(&mut app);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        for code in [app.diff_left_code_area, app.code_area]
            .into_iter()
            .flatten()
        {
            let mut colors = Vec::new();
            for y in code.y..code.bottom() {
                for x in code.x..code.right() {
                    let cell = &buffer[(x, y)];
                    if !cell.symbol().trim().is_empty() && !colors.contains(&cell.fg) {
                        colors.push(cell.fg);
                    }
                }
            }
            assert!(
                colors.len() >= 3,
                "keywords, strings and comments need syntax colors in each revision: {colors:?}"
            );
        }
    }
    let version = app.editor().unwrap().content_version;
    set_diff(
        &mut app,
        "fn previous() {}\n",
        "// a different commit\nfn current() {}\n",
    );
    assert!(
        app.editor().unwrap().content_version > version,
        "replacing the snapshot must invalidate syntax requests"
    );
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    highlight_diff(&mut app);
}

#[test]
fn commit_inspector_uses_native_hunk_navigation_and_editor_selection() {
    let mut app = fixture();
    let base = (0..100)
        .map(|i| format!("let value_{i} = {i};\n"))
        .collect::<String>();
    let target = base
        .replace("value_20 = 20", "value_20 = 200")
        .replace("value_80 = 80", "value_80 = 800");
    set_diff(&mut app, &base, &target);
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.diff_render_cache.hunks.len(), 2);
    handle_key(&mut app, KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
    assert_eq!(app.editor().unwrap().cursor_line, 80);
    handle_key(&mut app, KeyEvent::new(KeyCode::F(7), KeyModifiers::SHIFT));
    assert_eq!(app.editor().unwrap().cursor_line, 20);
    assert_eq!(app.commit_details.focus, Focus::Diff);
    let key = KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT);
    assert!(!owns_key(&app, key));
    crate::input_keyboard::handle_keyboard_event(&mut app, key);
    assert!(app.editor().unwrap().selection_anchor.is_some());
    assert!(app.editor().unwrap().read_only);
    assert_eq!(app.editor().unwrap().rope.to_string(), target);
}

#[test]
fn commit_inspector_diff_routes_mouse_selection_scrolling_and_resizing_to_native_handlers() {
    for mode in [
        crate::app::DiffMode::SideBySide,
        crate::app::DiffMode::Inline,
    ] {
        let mut app = fixture();
        app.show_minimap = false;
        let base = (0..200)
            .map(|i| format!("let value_{i} = \"{}\";\n", "context ".repeat(25)))
            .collect::<String>();
        let target = base.replace("value_10", "updated_10");
        set_diff(&mut app, &base, &target);
        app.diff_mode = mode;
        app.preferred_diff_mode = mode;
        let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let code = app.code_area.unwrap();
        let (row, column) = if mode == crate::app::DiffMode::SideBySide {
            (
                app.aligned_diff_scroll_row(app.editor().unwrap().scroll_offset) + 1,
                app.editor().unwrap().horizontal_scroll + 6,
            )
        } else {
            (
                app.diff_render_cache.inline_rows[app.diff_inline_scroll.row + 1].aligned_row,
                app.diff_inline_scroll.column + 8,
            )
        };
        let source = &app.diff_render_cache.rows[row];
        let text = if mode == crate::app::DiffMode::Inline
            && app.diff_render_cache.inline_rows[app.diff_inline_scroll.row + 1].kind == '-'
        {
            &source.left_text
        } else {
            &source.right_text
        };
        let expected: String = text.chars().skip(column).take(10).collect();
        assert_eq!(expected.chars().count(), 10);
        for (kind, x) in [
            (MouseEventKind::Down(MouseButton::Left), code.x + 8),
            (MouseEventKind::Drag(MouseButton::Left), code.x + 18),
            (MouseEventKind::Up(MouseButton::Left), code.x + 18),
        ] {
            crate::mouse::handle_mouse(&mut app, mouse_at(kind, x, code.y + 1));
        }
        assert_eq!(app.commit_details.focus, Focus::Diff);
        assert!(app.has_diff_selection() || app.editor().unwrap().has_selection());
        assert_eq!(
            app.diff_selected_text()
                .or_else(|| app.editor().unwrap().selected_text()),
            Some(expected)
        );
        let scroll = app.commit_details.review.scroll;
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::ScrollDown, code.x + 4, code.y + 2),
        );
        assert!(app.commit_details.review.scroll > scroll);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let files = current_layout(&app).files;
        let bar = Rect::new(files.right() - 1, files.y, 1, files.height);
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Down(MouseButton::Left),
                bar.x,
                bar.bottom() - 1,
            ),
        );
        assert!(app.commit_details.scrollbar_drag.is_some());
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Drag(MouseButton::Left),
                bar.x,
                bar.bottom() + 5,
            ),
        );
        crate::mouse::handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Up(MouseButton::Left),
                bar.x,
                bar.bottom() + 5,
            ),
        );
        assert!(app.commit_details.scrollbar_drag.is_none());
        assert!(app.commit_details.review.scroll > 100);
        app.commit_details.review.scroll = 0;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        if mode == crate::app::DiffMode::SideBySide {
            let divider = app.diff_divider_area.unwrap();
            let ratio = app.diff_split_ratio;
            for (kind, x) in [
                (MouseEventKind::Down(MouseButton::Left), divider.x),
                (MouseEventKind::Drag(MouseButton::Left), divider.x + 8),
                (MouseEventKind::Up(MouseButton::Left), divider.x + 8),
            ] {
                crate::mouse::handle_mouse(&mut app, mouse_at(kind, x, divider.y + 2));
            }
            assert!(app.diff_split_ratio > ratio);
            assert!(!app.resizing_diff_split);
        }
        assert_eq!(app.editor().unwrap().rope.to_string(), target);
        assert!(app.editor().unwrap().read_only);
    }
}

#[test]
fn commit_inspector_diff_supports_native_find_and_tab_switching() {
    let mut app = fixture();
    add_code_tab(&mut app);
    app.commit_details.focus = Focus::Diff;
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    for key in [
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
    ] {
        assert!(!owns_key(&app, key));
        crate::input_keyboard::handle_keyboard_event(&mut app, key);
    }
    assert!(app.search_active && app.search_input_focused);
    assert!(app.search_query.contains('m'));
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.search_bar_area.is_some());
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL);
    assert!(!owns_key(&app, key));
    crate::input_keyboard::handle_keyboard_event(&mut app, key);
    assert!(app.tab_switcher_active);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert_eq!(app.active_tab, 1);
    app.sync_diff_tab();
    assert!(!app.should_render_diff_for_active_editor());
}

#[test]
fn commit_inspector_inactive_diff_scrolls_and_receives_syntax_highlighting() {
    let mut app = fixture();
    let base = (0..150)
        .map(|i| format!("let value_{i} = {i};\n"))
        .collect::<String>();
    set_diff(&mut app, &base, &base.replace("value_10", "updated_10"));
    app.diff_mode = crate::app::DiffMode::Inline;
    add_code_tab(&mut app);
    app.create_split_group_dir(0, crate::app::SplitDirection::Horizontal);
    let inspector_group = app.active_group;
    app.switch_group(0);
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    app.refresh_visible_highlights_async();
    assert!(app.groups[inspector_group].visible_highlight_key.is_some());
    let code = app.groups[inspector_group].code_area.unwrap();
    let before = app.commit_details.review.scroll;
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(MouseEventKind::ScrollDown, code.x + 4, code.y + 2),
    );
    assert_eq!(
        app.active_group, 0,
        "scrolling must preserve keyboard focus"
    );
    assert!(app.commit_details.review.scroll > before);
    assert!(!app.should_render_diff_for_active_editor());
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(
            MouseEventKind::Down(MouseButton::Left),
            code.x + 4,
            code.y + 2,
        ),
    );
    assert_eq!(app.active_group, inspector_group);
    assert!(app.should_render_diff_for_active_editor());
    assert_eq!(app.commit_details.focus, Focus::Diff);
}

#[test]
fn commit_inspector_clear_filter_preserves_file_and_expanded_patch_switches_files() {
    let mut app = fixture();
    app.commit_details
        .edit_filter(KeyCode::Null, KeyModifiers::NONE, Some("history"));
    let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    crate::ui_control_visibility_tests::snapshot(
        terminal.backend().buffer(),
        "commit-inspector-filtered",
    );
    assert_eq!(app.commit_details.selected_file(), Some(2));
    let clear = app.commit_details.layout.as_ref().unwrap().filter_clear;
    click(&mut app, clear.x, clear.y);
    assert_eq!(app.commit_details.selected_file(), Some(2));
    assert!(app.commit_details.query.is_empty());
    handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.commit_details.focus, Focus::Files);
    assert_eq!(app.commit_details.selected_file(), Some(2));
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    );
    handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
    assert_eq!(app.commit_details.selected_file(), Some(3));
    assert!(app.commit_details.diff_expanded);
    assert_eq!(app.commit_details.focus, Focus::Diff);
}

#[test]
fn commit_inspector_scrollbars_drag_to_both_ends_and_release_outside() {
    let mut app = fixture();
    app.commit_details.data.files = (0..100)
        .map(|i| CommitFile::new('M', format!("src/file{i}.rs")))
        .collect();
    app.commit_details.data.message = "A long message\n\n".repeat(60);
    let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let view = app.commit_details.layout.clone().unwrap();
    for focus in [Focus::Files, Focus::Message] {
        let (area, count, _) = scrollbar_pane(&app, &view, focus);
        let event = |kind, row| MouseEvent {
            kind,
            column: area.right() - 1,
            row,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(
            &mut app,
            event(MouseEventKind::Down(MouseButton::Left), area.y),
        );
        assert_eq!(app.commit_details.scrollbar_drag, Some((focus, 0)));
        handle_mouse(
            &mut app,
            event(MouseEventKind::Drag(MouseButton::Left), u16::MAX),
        );
        assert_eq!(
            scrollbar_pane(&app, &view, focus).2,
            count - area.height as usize
        );
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(
            scrollbar_pane(&app, &view, focus).2,
            count - area.height as usize
        );
        handle_mouse(&mut app, event(MouseEventKind::Drag(MouseButton::Left), 0));
        assert_eq!(scrollbar_pane(&app, &view, focus).2, 0);
        handle_mouse(
            &mut app,
            event(MouseEventKind::Up(MouseButton::Left), u16::MAX),
        );
        assert!(app.commit_details.scrollbar_drag.is_none());
        handle_mouse(
            &mut app,
            event(MouseEventKind::Down(MouseButton::Left), area.bottom() - 1),
        );
        assert_eq!(
            scrollbar_pane(&app, &view, focus).2,
            count - area.height as usize
        );
        handle_mouse(
            &mut app,
            event(MouseEventKind::Up(MouseButton::Left), area.bottom() - 1),
        );
    }
}

#[test]
fn commit_inspector_toolbar_skips_unavailable_controls_and_recovers_focus_after_resize() {
    let mut app = fixture();
    app.commit_details.focus = Focus::Action(7);
    let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.commit_details.focus, Focus::Action(7));
    let mut narrow = Terminal::new(TestBackend::new(40, 34)).unwrap();
    narrow.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.commit_details.focus, Focus::Files);
    app.commit_details.focus = Focus::Action(2);
    handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.commit_details.focus, Focus::Action(16));
    handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.commit_details.focus, Focus::Files);
    assert_eq!(app.commit_details.selected, 0);
}

#[test]
fn commit_inspector_mouse_selects_preview_and_outside_click_preserves_tab() {
    let mut app = review_fixture(3);
    app.commit_details.active = true;
    app.commit_details.review.collapsed = (0..3).collect();
    let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let view = app.commit_details.layout.clone().unwrap();
    click(&mut app, view.files.x + 4, view.files.y + 2);
    assert_eq!(app.commit_details.selected, 2);
    assert!(app.commit_details.active);
    assert!(
        app.tabs.len() == 1,
        "selecting a file should stay in the commit tab"
    );
    click(&mut app, view.area.right(), view.area.bottom());
    assert!(app.commit_details.active);
    assert!(app.is_commit_details_tab());
}

#[test]
fn commit_inspector_keyboard_filter_focus_paging_and_escape() {
    let mut app = fixture();
    app.commit_details.active = true;
    app.commit_details.loading = true;
    let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    handle_key(&mut app, key(KeyCode::End));
    assert_eq!(app.commit_details.selected, 4);
    handle_key(&mut app, key(KeyCode::Char('/')));
    for c in "history".chars() {
        handle_key(&mut app, key(KeyCode::Char(c)));
    }
    assert_eq!(app.commit_details.selected_file(), Some(2));
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.commit_details.focus, Focus::Files);
    handle_key(&mut app, key(KeyCode::Tab));
    assert_eq!(app.commit_details.focus, Focus::Filter);
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(app.commit_details.query.is_empty());
    assert!(app.commit_details.active);
    handle_key(&mut app, key(KeyCode::Esc));
    assert_eq!(app.commit_details.focus, Focus::Files);
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(app.commit_details.active);
}

#[test]
fn commit_inspector_wraps_unicode_without_splitting_graphemes() {
    let text = "Hello 東京 e\u{301} 👩‍💻 with a verylongfilename.rs";
    let lines = wrapped(text, 12);
    assert!(
        lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 12)
    );
    assert!(lines.iter().any(|line| line.contains("e\u{301}")));
    assert!(lines.iter().any(|line| line.contains("👩‍💻")));
}

#[test]
fn commit_inspector_resize_reveals_patch_and_long_paths_keep_the_filename() {
    let mut app = fixture();
    app.commit_details.diff_expanded = true;
    let mut terminal = Terminal::new(TestBackend::new(150, 44)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert!(native_diff_ready(&app));
    assert!(app.diff_split_area.is_some());
    let path = display_path("very/long/path/to/東京.rs", 12);
    assert!(path.ends_with("東京.rs"));
    assert!(UnicodeWidthStr::width(path.as_str()) <= 12);
}

#[test]
fn source_control_keyboard_routes_between_sidebar_and_commit_inspector() {
    use crate::app::{ActivePane, GitCommitInfo, SidebarView, SourceControlRepositoryState};
    let mut app = fixture();
    app.sidebar_view = SidebarView::SourceControl;
    app.source_control_repository_state = SourceControlRepositoryState::Available;
    app.active_pane = ActivePane::Explorer;
    app.show_explorer = true;
    app.git_log = (0..3)
        .map(|i| GitCommitInfo {
            hash: format!("{i:07x}"),
            full_hash: format!("{i:040x}"),
            graph: "●".into(),
            message: format!("Commit {i}"),
            author: "Alex".into(),
            time_ago: "2h ago".into(),
            branches: vec![],
        })
        .collect();
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    crate::input_keyboard::handle_keyboard_event(&mut app, key(KeyCode::Tab));
    assert!(app.sc_graph_focused);
    crate::input_keyboard::handle_keyboard_event(&mut app, key(KeyCode::Down));
    assert_eq!(app.git_log_selected, 1);
    crate::input_keyboard::handle_keyboard_event(&mut app, key(KeyCode::Enter));
    assert!(app.commit_details.active);
    assert_eq!(app.commit_details.data.message, "Commit 1");
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
    );
    assert_eq!(app.commit_details.data.message, "Commit 0");
    crate::input_keyboard::handle_keyboard_event(&mut app, key(KeyCode::Esc));
    assert!(app.commit_details.active);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    );
    assert!(!app.commit_details.active);
    app.active_pane = ActivePane::Explorer;
    crate::input_keyboard::handle_keyboard_event(&mut app, key(KeyCode::Tab));
    assert!(app.source_control_input_mode);
    crate::input_keyboard::handle_keyboard_event(&mut app, key(KeyCode::Tab));
    assert!(!app.source_control_input_mode);
    assert!(!app.sc_graph_focused);
}

#[test]
fn commit_inspector_expanded_patch_can_always_be_dismissed_while_loading_or_failed() {
    for failed in [false, true] {
        let mut app = App::test_new();
        app.commit_details.active = true;
        app.commit_details.diff_expanded = true;
        app.commit_details.loading = !failed;
        app.commit_details.error = failed.then(|| "Unavailable commit".into());
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        handle_key(&mut app, escape);
        assert!(!app.commit_details.diff_expanded);
        handle_key(&mut app, escape);
        assert!(app.commit_details.active);
    }
}

fn add_code_tab(app: &mut App) {
    let mut editor = crate::editor::Editor::new();
    editor.file_path = Some("notes.rs".into());
    editor.rope = ropey::Rope::from_str("fn main() {}\n");
    app.tabs.push(crate::app::Tab::new_file(editor));
}

fn mouse_at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn commit_inspector_dividers_resize_both_layouts_and_reset_without_losing_selection() {
    for width in [80, 150] {
        let mut app = fixture();
        let mut terminal = Terminal::new(TestBackend::new(width, 44)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let original = current_layout(&app);
        app.commit_details.selected = 2;
        let sash = original.primary_divider;
        assert!(sash.width > 0 && sash.height > 0);
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), sash.x, sash.y),
        );
        assert_eq!(app.commit_details.divider_drag, Some(Divider::Primary));
        handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Drag(MouseButton::Left),
                sash.x + 10,
                sash.y + 5,
            ),
        );
        handle_mouse(
            &mut app,
            mouse_at(
                MouseEventKind::Up(MouseButton::Left),
                sash.x + 10,
                sash.y + 5,
            ),
        );
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let resized = current_layout(&app);
        assert_ne!(resized.primary_divider, original.primary_divider);
        assert_eq!(app.commit_details.selected_file(), Some(2));
        assert!(app.commit_details.divider_drag.is_none());

        activate(&mut app, 8);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(
            current_layout(&app).primary_divider,
            original.primary_divider
        );

        let sash = current_layout(&app).primary_divider;
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            handle_mouse(
                &mut app,
                mouse_at(
                    kind,
                    if matches!(kind, MouseEventKind::Down(_)) {
                        sash.x
                    } else {
                        u16::MAX
                    },
                    if matches!(kind, MouseEventKind::Down(_)) {
                        sash.y
                    } else {
                        u16::MAX
                    },
                ),
            );
        }
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let view = current_layout(&app);
        assert!(view.files.width >= 36 && view.files.height >= 3);
        assert!(app.commit_details.divider_drag.is_none());
    }
}

#[test]
fn commit_inspector_divider_double_click_and_keyboard_resize_share_the_layout() {
    let mut app = fixture();
    let mut terminal = Terminal::new(TestBackend::new(150, 44)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let original = current_layout(&app);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT),
    );
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let resized = current_layout(&app);
    assert!(resized.overview.height > original.overview.height);
    let sash = resized.primary_divider;
    for _ in 0..2 {
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Down(MouseButton::Left), sash.x, sash.y),
        );
        handle_mouse(
            &mut app,
            mouse_at(MouseEventKind::Up(MouseButton::Left), sash.x, sash.y),
        );
    }
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(
        current_layout(&app).overview.height,
        original.overview.height
    );
    assert_eq!(app.commit_details.panes, PaneSizes::default());
    assert!(app.commit_details.divider_drag.is_none());
}

#[test]
fn commit_inspector_scrolling_files_preserves_preview_and_keyboard_focus() {
    let mut app = fixture();
    app.commit_details.data.files = (0..100)
        .map(|i| CommitFile::new('M', format!("src/file{i}.rs")))
        .collect();
    app.commit_details.focus = Focus::Diff;
    let mut terminal = Terminal::new(TestBackend::new(150, 44)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let view = current_layout(&app);
    scroll(
        &mut app,
        MouseEventKind::ScrollDown,
        view.files.x + 3,
        view.files.y,
    );
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.commit_details.review.scroll, 3);
    assert_eq!(app.commit_details.selected_file(), Some(0));
    assert_eq!(app.commit_details.diff_file, Some(0));
    assert_eq!(app.commit_details.focus, Focus::Diff);
    let column = view.files.right() - 1;
    handle_mouse(
        &mut app,
        mouse_at(
            MouseEventKind::Down(MouseButton::Left),
            column,
            view.files.bottom() - 1,
        ),
    );
    handle_mouse(
        &mut app,
        mouse_at(
            MouseEventKind::Up(MouseButton::Left),
            column,
            view.files.bottom() - 1,
        ),
    );
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.commit_details.selected_file(), Some(0));
    assert!(app.commit_details.review.scroll > 3);
    handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
    assert_eq!(app.commit_details.selected_file(), Some(1));
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    assert_eq!(app.commit_details.review.visible.first().unwrap().file, 1);
}

#[test]
fn commit_inspector_is_an_editor_tab_and_workbench_shortcuts_still_work() {
    let mut app = fixture();
    app.commit_details.active = true;
    add_code_tab(&mut app);
    app.commit_details.query = "source_control".into();
    app.commit_details.query_cursor = app.commit_details.query.len();
    app.commit_details.focus = Focus::Filter;
    let mut terminal = Terminal::new(TestBackend::new(150, 44)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.commit_details.visible);
    let view = current_layout(&app);
    assert_eq!(view.area.y, app.tab_bar_area.unwrap().bottom());
    assert!(view.patch.contains(app.code_area.unwrap().as_position()));
    assert!(!owns_key(
        &app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL)
    ));
    assert!(!owns_key(
        &app,
        KeyEvent::new(
            KeyCode::Char('F'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )
    ));
    assert!(owns_key(
        &app,
        KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )
    ));
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL),
    );
    assert!(app.tab_switcher_active);
    assert_eq!(app.tab_mru_order[app.tab_switcher_selected], 1);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert_eq!(app.active_tab, 1);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(!app.commit_details.visible);
    assert!(!handle_mouse(
        &mut app,
        mouse_at(
            MouseEventKind::Down(MouseButton::Left),
            view.files.x,
            view.files.y
        )
    ));
    let before = app.tabs[1].editor.rope.to_string();
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert_ne!(app.tabs[1].editor.rope.to_string(), before);
    assert_eq!(app.commit_details.query, "source_control");
    app.active_tab = 0;
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
    );
    assert!(app.palette.active);
    assert!(!owns_key(
        &app,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
    ));
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.palette.active);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].kind, crate::app::TabKind::File);
    assert!(!app.commit_details.active);
}

#[test]
fn commit_inspector_filter_keeps_altgr_text_in_its_own_field() {
    let mut app = fixture();
    let contents = app.tabs[0].editor.rope.to_string();
    app.commit_details.focus = Focus::Filter;
    for character in ['@', 'c', '€'] {
        let key = KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert!(owns_key(&app, key));
        crate::input_keyboard::handle_keyboard_event(&mut app, key);
    }
    assert_eq!(app.commit_details.query, "@c€");
    assert_eq!(app.tabs[0].editor.rope.to_string(), contents);
    app.commit_details.focus = Focus::Files;
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ),
    );
    assert!(!app.commit_details.diff_expanded);
    assert_eq!(app.commit_details.query, "@c€");
}

#[test]
fn commit_inspector_survives_moving_to_a_group_and_focuses_from_its_own_content() {
    let mut app = fixture();
    add_code_tab(&mut app);
    app.create_split_group_dir(0, crate::app::SplitDirection::Horizontal);
    assert!(app.is_commit_details_tab());
    let inspector_group = app.active_group;
    app.tab_tooltip_tab = Some(0);
    app.tab_tooltip_hover_start = Some(std::time::Instant::now());
    app.tab_tooltip_shown = true;
    app.switch_group(0);
    assert!(app.tab_tooltip_tab.is_none());
    assert!(app.tab_tooltip_hover_start.is_none());
    assert!(!app.tab_tooltip_shown);
    assert!(!app.is_commit_details_tab());
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    activate(&mut app, 16);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let view = current_layout(&app);
    assert!(app.commit_details.visible);
    app.tab_tooltip_tab = Some(0);
    app.tab_tooltip_hover_start = Some(std::time::Instant::now());
    app.tab_tooltip_shown = true;
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(
            MouseEventKind::Down(MouseButton::Left),
            view.files.x + 3,
            view.files.y + 1,
        ),
    );
    assert_eq!(app.active_group, inspector_group);
    assert!(app.tab_tooltip_tab.is_none());
    assert!(app.tab_tooltip_hover_start.is_none());
    assert!(!app.tab_tooltip_shown);
    assert!(app.is_commit_details_tab());
    assert_eq!(app.commit_details.selected_file(), Some(1));
}

#[test]
fn commit_inspector_tab_does_not_show_the_selected_diff_file_as_a_tooltip() {
    let mut app = fixture();
    add_code_tab(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let tabs = app.tab_bar_area.unwrap();
    let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, tabs);
    let file = hits
        .tabs
        .iter()
        .find(|(index, _, _)| *index == 1)
        .unwrap()
        .1;
    let details = hits
        .tabs
        .iter()
        .find(|(index, _, _)| *index == 0)
        .unwrap()
        .1;

    // Normal file tabs still show their path after the hover delay.
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(MouseEventKind::Moved, file.x + 1, file.y),
    );
    assert_eq!(app.tab_tooltip_tab, Some(1));
    app.tab_tooltip_hover_start =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.tab_tooltip_shown);

    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(MouseEventKind::Moved, details.x + 1, details.y),
    );
    assert_eq!(app.hovered_tab, Some(0));
    assert!(app.tab_tooltip_tab.is_none());
    assert!(app.tab_tooltip_hover_start.is_none());
    assert!(!app.tab_tooltip_shown);

    // A stale index must also be discarded before rendering another frame,
    // even when the pointer never moves after the inspector becomes visible.
    app.tab_tooltip_tab = Some(0);
    app.tab_tooltip_hover_start =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    app.tab_tooltip_shown = true;
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.tab_tooltip_tab.is_none());
    assert!(app.tab_tooltip_hover_start.is_none());
    assert!(!app.tab_tooltip_shown);
}

#[test]
fn commit_inspector_native_tab_drag_crosses_its_content_and_hover_clears_tooltips() {
    let mut app = fixture();
    add_code_tab(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(180, 44)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let view = current_layout(&app);
    app.hovered_tab = Some(0);
    app.tab_tooltip_tab = Some(0);
    app.tab_tooltip_shown = true;
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(MouseEventKind::Moved, view.files.x + 1, view.files.y),
    );
    assert!(app.hovered_tab.is_none());
    assert!(app.tab_tooltip_tab.is_none());
    assert!(!app.tab_tooltip_shown);
    let tabs = app.tab_bar_area.unwrap();
    let tab_x = crate::ui_chrome_tabs::TabBarHits::new(&app, tabs).tabs[0]
        .1
        .x;
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(MouseEventKind::Down(MouseButton::Left), tab_x + 3, tabs.y),
    );
    assert_eq!(app.dragging_tab, Some(0));
    let editor = app.editor_area.unwrap();
    let point = (editor.right() - 2, editor.y + editor.height / 2);
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(MouseEventKind::Drag(MouseButton::Left), point.0, point.1),
    );
    assert!(app.tab_drop_zone == Some(crate::app::TabDropZone::Right));
    crate::mouse::handle_mouse(
        &mut app,
        mouse_at(MouseEventKind::Up(MouseButton::Left), point.0, point.1),
    );
    assert!(app.split_active());
    assert!(app.is_commit_details_tab());
    assert!(app.dragging_tab.is_none());
    assert_eq!(app.commit_details.selected_file(), Some(0));
}

#[test]
fn commit_inspector_workbench_previews_use_a_native_tab_and_leave_source_control_visible() {
    for theme in crate::ui_control_visibility_tests::audit_themes() {
        let mut app = fixture();
        app.colors = theme.to_theme_colors();
        add_code_tab(&mut app);
        app.show_explorer = true;
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
        app.source_control_files = app
            .commit_details
            .data
            .files
            .iter()
            .map(|file| crate::app::GitFileChange {
                path: file.path.clone(),
                original_path: file.old_path.clone(),
                status: file.status,
                staged: false,
            })
            .collect();
        app.git_log = (0..8)
            .map(|i| crate::app::GitCommitInfo {
                hash: if i == 0 {
                    "a4c8d21".into()
                } else {
                    format!("{i:07x}")
                },
                full_hash: if i == 0 {
                    app.commit_details.data.hash.clone()
                } else {
                    format!("{i:040x}")
                },
                graph: "●".into(),
                message: if i == 0 {
                    "Add keyboard navigation".into()
                } else {
                    format!("Refine Source Control {i}")
                },
                author: "Alex Morgan".into(),
                time_ago: "2h ago".into(),
                branches: vec![],
            })
            .collect();
        for (width, height) in [(110, 34), (180, 48)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            highlight_diff(&mut app);
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let view = current_layout(&app);
            assert!(view.area.x >= app.explorer_area.unwrap().right());
            assert_eq!(view.area.y, app.tab_bar_area.unwrap().bottom());
            assert_eq!(view.area.height, app.editor_area.unwrap().height);
            crate::ui_control_visibility_tests::snapshot(
                terminal.backend().buffer(),
                &format!("commit-tab-workbench-{}-{width}", theme.name),
            );
        }
    }
}
