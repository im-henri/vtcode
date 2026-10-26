use crate::{
    app::{App, SplitDirection, Tab},
    editor::Editor,
};

fn app() -> App {
    let mut app = App::test_new();
    for (name, text) in [
        ("main.rs", "fn main() {}\n"),
        ("test.rs", "test\n"),
        ("notes.md", "notes\n"),
    ] {
        let mut ed = Editor::test_with_text(text);
        ed.file_path = Some(std::path::PathBuf::from(name));
        ed.saved_rope = Some(ed.rope.clone());
        app.tabs.push(Tab::new_file(ed));
    }
    app.active_tab = 0;
    app
}

#[test]
fn desk_switch_restores_panes_tools_and_keeps_shared_settings() {
    let mut app = app();
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    let tree = app.split_tree.clone();
    app.show_panel = true;
    app.panel_tab = 3;
    app.panel_height = 14;
    app.show_explorer = false;
    app.search_query = "needle".into();
    app.global_search_query = "workspace needle".into();
    let old = app.desks.active;
    let id = app.new_desk("Debugging", false);
    assert!(app.tabs.is_empty());
    assert!(!app.split_active());
    assert!(!app.show_panel);
    assert!(app.show_explorer);
    assert!(app.search_query.is_empty());
    assert_eq!(app.build_session().search_history, ["needle"]);
    assert_eq!(app.build_session().global_search_history, ["workspace needle"]);
    app.show_line_numbers = false;
    app.switch_desk(old);
    assert_eq!(app.split_tree, tree);
    assert_eq!(app.tabs[0].editor.file_name(), "notes.md");
    assert!(app.show_panel);
    assert_eq!(app.panel_tab, 3);
    assert_eq!(app.panel_height, 14);
    assert!(!app.show_explorer);
    assert_eq!(app.search_query, "needle");
    assert!(!app.show_line_numbers);
    app.switch_desk(id);
    assert!(app.tabs.is_empty());
}

#[test]
fn duplicate_desk_shares_unsaved_edits_and_undo_without_moving_other_caret() {
    let mut app = app();
    app.editor_mut().unwrap().insert_char('A');
    let source = app.editor().unwrap().navigation_id;
    app.new_desk("Copy", true);
    assert_ne!(app.editor().unwrap().navigation_id, source);
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().cursor_col = 0;
    app.editor_mut().unwrap().insert_char('B');
    app.switch_desk(0);
    assert!(app.editor().unwrap().rope.to_string().starts_with("BA"));
    assert_ne!(app.editor().unwrap().cursor_col, 0);
    app.editor_mut().unwrap().undo();
    assert!(app.editor().unwrap().rope.to_string().starts_with('A'));
    let id = app.desks.items[1].id;
    app.switch_desk(id);
    assert!(app.editor().unwrap().rope.to_string().starts_with('A'));
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "fn main() {}\n");
    assert!(!app.editor().unwrap().modified);
    app.switch_desk(0);
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn untitled_document_is_shared_only_when_layout_is_duplicated() {
    let mut app = app();
    app.tabs.truncate(1);
    app.tabs[0].editor.file_path = None;
    let id = app.new_desk("Other", true);
    app.editor_mut().unwrap().insert_char('x');
    app.switch_desk(0);
    assert!(app.editor().unwrap().rope.to_string().starts_with('x'));
    app.new_file();
    app.editor_mut().unwrap().insert_char('y');
    app.switch_desk(id);
    assert!(!app.editor().unwrap().rope.to_string().starts_with('y'));
}

#[test]
fn opening_a_hidden_unsaved_document_reuses_live_text_without_disk_load() {
    let mut app = app();
    app.tabs[0].editor.insert_char('!');
    app.new_desk("Empty", false);
    app.open_file(std::path::Path::new("main.rs"));
    assert!(!app.editor().unwrap().bg_loading);
    assert!(app.editor().unwrap().rope.to_string().starts_with('!'));
    assert_eq!(app.current_desk_name(), "Empty");
}

#[test]
fn split_cleanup_and_merge_do_not_touch_hidden_desks() {
    let mut app = app();
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    let original = app.tabs[0].editor.navigation_id;
    let id = app.new_desk("Second", true);
    app.close_all_splits();
    assert_eq!(app.tabs.len(), 3);
    assert!(!app.split_active());
    app.switch_desk(0);
    assert!(app.split_active());
    assert_eq!(app.tabs[0].editor.navigation_id, original);
    app.close_saved_tabs();
    app.switch_desk(id);
    assert_eq!(app.tabs.len(), 3);
}

#[test]
fn grouping_keeps_members_contiguous_and_selected_document_stable() {
    let mut app = app();
    let selected = app.tabs[2].editor.navigation_id;
    app.active_tab = 2;
    app.desks
        .selected_tabs
        .extend([app.tabs[0].editor.navigation_id, selected]);
    app.group_selected_tabs("Implementation", None);
    assert_eq!(app.active_tab, 1);
    assert_eq!(app.tabs[1].editor.navigation_id, selected);
    assert_eq!(app.tab_group(&app.tabs[0]).unwrap().name, "Implementation");
    assert!(app.tab_group(&app.tabs[1]).is_some());
    assert!(app.tab_group(&app.tabs[2]).is_none());
}

#[test]
fn collapsed_groups_keep_active_and_pinned_tabs_visible() {
    let mut app = app();
    app.desks
        .selected_tabs
        .extend(app.tabs.iter().map(|t| t.editor.navigation_id));
    app.group_selected_tabs("Feature", None);
    let group = app.tab_group(&app.tabs[0]).unwrap().id;
    app.desks.groups.get_mut(&group).unwrap().collapsed = true;
    app.tabs[2].pinned = true;
    let hits =
        crate::ui_chrome_tabs::TabBarHits::new(&app, ratatui::layout::Rect::new(0, 0, 120, 1));
    assert_eq!(hits.groups.len(), 1);
    assert_eq!(
        hits.tabs.iter().map(|t| t.0).collect::<Vec<_>>(),
        vec![0, 2]
    );
    app.touch_tab_mru(1);
    assert!(!app.desks.groups[&group].collapsed);
}

#[test]
fn empty_desk_closes_without_confirmation_from_dropdown_and_command() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};
    for dropdown in [false, true] {
        let mut app = app();
        let empty = app.new_desk("Empty", false);
        if dropdown {
            crate::ui_desks::open(&mut app, crate::ui_desks::Mode::Switch);
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| crate::ui_desks::draw(f, &mut app)).unwrap();
            let popup = app.desks.popup.area;
            let y = (popup.y..popup.bottom()).find(|&y| {
                let row = (popup.x..popup.right())
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>();
                row.contains("Close desk")
            }).unwrap();
            assert!(crate::ui_desks::handle_mouse(&mut app, MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: popup.x + 2,
                row: y,
                modifiers: KeyModifiers::empty(),
            }));
        } else {
            assert!(crate::ui_desks::command(&mut app, "desk_close"));
        }
        assert!(app.desks.popup.mode.is_none());
        assert_eq!(app.desks.active, 0);
        assert_eq!(app.desk_count(), 1);
        assert_eq!(app.tabs.len(), 3, "files in the other desk stay open");
        assert!(app.desks.items.iter().any(|d| d.id == empty && d.closed));
    }

    let mut empty = App::test_new();
    assert!(empty.tabs.is_empty());
    assert!(crate::ui_desks::command(&mut empty, "desk_close"));
    assert!(empty.desks.popup.mode.is_none());
    assert_eq!(empty.desk_count(), 1, "keep a usable desk after closing the last one");
}

#[test]
fn closing_desk_still_confirms_tabs_in_inactive_panes_and_untitled_documents() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = app();
    app.editor_mut().unwrap().insert_char('!');
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    app.tabs.clear();
    assert!(!app.current_desk_is_empty(), "another pane still contains files");
    let desk = app.desks.active;
    assert!(crate::ui_desks::command(&mut app, "desk_close"));
    assert_eq!(app.desks.popup.mode, Some(crate::ui_desks::Mode::Close));
    assert_eq!(app.desks.active, desk);
    crate::ui_desks::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
    assert_eq!(app.desks.active, desk);
    assert!(app.groups.iter().flat_map(|g| &g.tabs).any(|t| t.has_unsaved_changes()));

    let untitled = app.new_desk("Untitled", false);
    app.new_file();
    app.editor_mut().unwrap().insert_char('x');
    assert!(crate::ui_desks::command(&mut app, "desk_close"));
    assert_eq!(app.desks.popup.mode, Some(crate::ui_desks::Mode::Close));
    assert_eq!(app.desks.active, untitled);
    assert!(app.editor().unwrap().modified);
}

#[test]
fn close_and_reopen_desk_preserves_dirty_text_history_and_group_metadata() {
    let mut app = app();
    app.group_selected_tabs("Work", None);
    app.editor_mut().unwrap().insert_char('!');
    app.close_desk();
    assert_eq!(app.desk_count(), 1);
    assert!(app.tabs.is_empty());
    app.reopen_desk();
    assert!(app.editor().unwrap().modified);
    assert!(app.tab_group(&app.tabs[0]).is_some());
    app.editor_mut().unwrap().undo();
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn session_keeps_desk_owners_nested_layout_and_group_membership() {
    let mut app = app();
    app.group_selected_tabs("Work", None);
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    app.new_desk("Debugging", true);
    let session = app.build_session();
    let json = serde_json::to_string(&session).unwrap();
    let session: crate::session::Session = serde_json::from_str(&json).unwrap();
    let mut restored = App::test_new();
    restored.restore_session(&session);
    assert_eq!(restored.current_desk_name(), "Debugging");
    assert_eq!(
        restored.groups.iter().map(|g| g.desk).collect::<Vec<_>>(),
        app.groups.iter().map(|g| g.desk).collect::<Vec<_>>()
    );
    assert_eq!(restored.split_tree, app.split_tree);
    restored.switch_desk(0);
    assert!(restored.split_active());
    let pane = restored.desk_panes()[0];
    restored.switch_group(pane);
    assert_eq!(restored.tab_group(&restored.tabs[0]).unwrap().name, "Work");
}

#[test]
fn background_edit_to_hidden_view_is_published_without_focus_change() {
    let mut app = app();
    let view = app.tabs[0].editor.navigation_id;
    app.new_desk("Other", true);
    let current = (app.desks.active, app.active_group, app.active_tab);
    let tab = app
        .groups
        .iter_mut()
        .flat_map(|g| &mut g.tabs)
        .find(|t| t.editor.navigation_id == view)
        .unwrap();
    tab.editor.insert_char('!');
    app.sync_document_view(view);
    assert_eq!(
        current,
        (app.desks.active, app.active_group, app.active_tab)
    );
    assert!(app.editor().unwrap().rope.to_string().starts_with('!'));
}

#[test]
fn leading_plus_and_selector_do_not_overlap_files_or_repeat_in_splits() {
    let mut app = app();
    let area = ratatui::layout::Rect::new(2, 3, 80, 1);
    let (plus, picker, width) = crate::ui_desks::controls(&app, area, 0);
    assert_eq!(plus.width, 3);
    assert!(picker.is_empty());
    assert_eq!(width, 4);
    app.new_desk("An exceptionally long desk name", true);
    app.desks.host = app.active_group;
    for width in [4, 8, 16, 40, 80] {
        let area = ratatui::layout::Rect::new(2, 3, width, 1);
        let (_, picker, prefix) = crate::ui_desks::controls(&app, area, app.active_group);
        assert!(prefix <= width);
        assert!(picker.right() <= area.right());
        let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, area);
        assert!(hits.tabs.iter().all(|(_, r, _)| r.x >= area.x + prefix));
    }
    assert_eq!(crate::ui_desks::controls(&app, area, usize::MAX).2, 0);
}

#[test]
fn document_and_history_are_shared_before_any_workbench_synchronization() {
    let mut a = Editor::test_with_text("base\n");
    a.saved_rope = Some(a.rope.clone());
    let mut b = Editor::new();
    b.copy_document_from(&a);
    let mut c = Editor::new();
    c.copy_document_from(&b);
    assert!(a.shares_document(&b) && b.shares_document(&c));
    a.paste_plain_with_text("one ");
    assert!(
        b.can_undo(),
        "history is the same queue, without a sync pass"
    );
    b.cursor_col = 4;
    b.paste_plain_with_text("two ");
    c.undo();
    assert_eq!(c.rope.to_string(), "one base\n");
    a.undo();
    assert_eq!(a.rope.to_string(), "base\n");
    assert!(!a.modified);
    b.redo();
    assert_eq!(b.rope.to_string(), "one base\n");
    c.refresh_document();
    c.cursor_col = 0;
    c.paste_plain_with_text("branch ");
    assert!(!a.can_redo());
    assert!(!b.can_redo());
    a.refresh_document();
    assert_eq!(a.rope.to_string(), c.rope.to_string());
    assert_eq!(a.undo_history_key(), c.undo_history_key());
}

#[test]
fn shared_save_checkpoint_and_last_remaining_view_keep_one_history() {
    let mut a = Editor::test_with_text("saved");
    a.saved_rope = Some(a.rope.clone());
    let mut b = Editor::new();
    b.copy_document_from(&a);
    a.paste_plain_with_text("edit ");
    b.refresh_document();
    b.record_saved_snapshot(b.rope.clone());
    a.refresh_document();
    assert!(!a.modified);
    a.undo();
    assert!(a.modified);
    b.redo();
    assert!(!b.modified);
    drop(a);
    b.paste_plain_with_text("last ");
    let mut c = Editor::new();
    c.copy_document_from(&b);
    assert_eq!(c.rope.to_string(), b.rope.to_string());
    c.undo();
    assert!(!c.modified);
    assert_eq!(c.rope.to_string(), "edit saved");
}

#[test]
fn hidden_view_selection_is_clamped_after_shared_deletion() {
    let mut a = Editor::test_with_text("one\ntwo\nthree\n");
    let mut b = Editor::new();
    b.copy_document_from(&a);
    b.cursor_line = 3;
    b.selection_anchor = Some((2, 5));
    b.extra_cursors.push((3, 0));
    b.extra_cursor_anchors.push(Some((2, 4)));
    a.selection_anchor = Some((0, 0));
    a.cursor_line = 3;
    a.insert_char_replacing_selection('x');
    b.refresh_document();
    assert_eq!(b.rope.to_string(), "x");
    assert_eq!(b.cursor_line, 0);
    assert_eq!(b.selection_anchor, Some((0, 1)));
    assert_eq!(b.extra_cursors, vec![(0, 0)]);
    b.undo();
    assert_eq!(b.rope.to_string(), "one\ntwo\nthree\n");
}

#[test]
fn group_can_move_between_panes_without_losing_members_or_document_focus() {
    let mut app = app();
    app.desks
        .selected_tabs
        .extend(app.tabs[..2].iter().map(|t| t.editor.navigation_id));
    app.group_selected_tabs("Work", None);
    let group = app.tab_group(&app.tabs[0]).unwrap().id;
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    let target = app.active_group;
    app.move_named_group(group, target, None);
    assert_eq!(
        app.groups
            .iter()
            .flat_map(|g| &g.tabs)
            .chain(&app.tabs)
            .filter(|t| app.desks.membership.get(&t.editor.navigation_id) == Some(&group))
            .count(),
        2
    );
}

#[test]
fn duplicated_loading_views_do_not_reapply_recovery_or_clear_shared_history() {
    let mut a = Editor::new();
    a.bg_loading = true;
    let mut b = Editor::new();
    b.copy_document_from(&a);
    a.apply_bg_loaded("disk".into(), "UTF-8", 4, 50 * 1024 * 1024);
    a.paste_plain_with_text("recovered ");
    b.apply_bg_loaded("disk".into(), "UTF-8", 4, 50 * 1024 * 1024);
    assert_eq!(b.rope.to_string(), "recovered disk");
    assert!(b.can_undo());
    b.undo();
    assert_eq!(b.rope.to_string(), "disk");
}

#[test]
fn closing_a_shared_dirty_tab_keeps_the_document_in_the_other_desk() {
    let mut app = app();
    app.new_desk("Copy", true);
    app.editor_mut().unwrap().paste_plain_with_text("edit ");
    app.close_tab(0);
    assert!(!app.confirm_close_active);
    app.switch_desk(0);
    assert!(app.editor().unwrap().rope.to_string().starts_with("edit "));
    app.close_tab(0);
    assert!(app.confirm_close_active);
}

#[test]
fn desk_picker_keeps_pane_counts_and_unsaved_markers_visible_for_long_names() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    let mut app = app();
    app.editor_mut().unwrap().insert_char('!');
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    app.switch_group(0);
    app.create_split_group_dir(1, SplitDirection::Vertical);
    assert_eq!(app.desk_panes().len(), 3);
    let other = app.new_desk("Other", false);
    for name in [
        "really long desk name ".repeat(6),
        "編輯工作桌名稱".repeat(12),
        "👩‍💻e\u{301}".repeat(16),
    ] {
        let name = format!("{name} unique-tail");
        app.desks.items[0].name = name.clone();
        for width in [100, 32, 20, 12] {
            app.tab_bar_area = Some(Rect::new(0, 0, width, 1));
            crate::ui_desks::open(&mut app, crate::ui_desks::Mode::Switch);
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal.draw(|f| crate::ui_desks::draw(f, &mut app)).unwrap();
            let popup = app.desks.popup.area;
            let y = app.desks.popup.input.bottom();
            let row = (popup.x..popup.right())
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>();
            assert!(row.contains('…'), "long name needs an ellipsis: {row}");
            assert!(row.contains('●'), "unsaved marker must fit: {row}");
            assert!(row.contains(if width >= 32 { "3 panes" } else { "3" }), "count must fit: {row}");

            // Filtering uses the full name, and clicking the clipped row still
            // selects the right desk, including when only its suffix matches.
            crate::ui_desks::paste(&mut app, "unique-tail");
            terminal.draw(|f| crate::ui_desks::draw(f, &mut app)).unwrap();
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: app.desks.popup.area.x + 2,
                row: app.desks.popup.input.bottom(),
                modifiers: KeyModifiers::empty(),
            };
            assert!(crate::ui_desks::handle_mouse(&mut app, click));
            assert_eq!(app.desks.active, 0);
            assert_eq!(app.current_desk_name(), name);
            assert_eq!(app.desk_panes().len(), 3);
            app.switch_desk(other);
        }
    }
}

#[test]
fn desk_name_clipping_does_not_split_unicode_graphemes() {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let name = "👩‍💻e\u{301}界".repeat(8);
    for width in 0..24 {
        let clipped = crate::ui_desks::clipped_name(&name, width);
        assert!(clipped.width() <= width);
        let prefix = clipped.strip_suffix('…').unwrap_or(&clipped);
        assert!(name.starts_with(prefix));
        assert!(name.grapheme_indices(true).any(|(byte, _)| byte == prefix.len()));
    }
}

#[test]
fn render_desk_controls_and_group_popups() {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    let mut app = app();
    app.materialize_desk_pane();
    app.tab_bar_area = Some(Rect::new(0, 0, 100, 1));
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    for scenario in ["single", "grouped", "picker", "long-name", "new", "narrow"] {
        match scenario {
            "grouped" => {
                app.desks
                    .selected_tabs
                    .extend(app.tabs[..2].iter().map(|t| t.editor.navigation_id));
                app.group_selected_tabs("Implementation", None);
                app.new_desk("Debugging", true);
                app.desks.host = app.active_group;
            }
            "picker" => crate::ui_desks::open(&mut app, crate::ui_desks::Mode::Switch),
            "long-name" => {
                app.desks.items[0].name = "A really long desk name for a feature implementation".repeat(2);
                crate::ui_desks::open(&mut app, crate::ui_desks::Mode::Switch);
            }
            "new" => crate::ui_desks::open(&mut app, crate::ui_desks::Mode::New),
            "narrow" => {
                app.desks.popup = Default::default();
            }
            _ => {}
        }
        let width = if scenario == "narrow" { 32 } else { 100 };
        app.tab_bar_area = Some(Rect::new(0, 0, width, 1));
        terminal
            .draw(|f| {
                crate::ui_chrome_tabs::draw_tab_bar(f, &app, Rect::new(0, 0, width, 1));
                crate::ui_desks::draw(f, &mut app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let first_row = (0..width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        assert!(first_row.contains('+'));
        if scenario == "single" {
            assert!(!first_row.contains("Main"));
        }
        if scenario == "grouped" {
            assert!(first_row.contains("Debugging"));
            assert!(first_row.contains("Implementation"));
        }
        if scenario == "narrow" {
            assert!(
                first_row.contains("main"),
                "active file must remain visible: {first_row}"
            );
        }
        if let Ok(folder) = std::env::var("VTCODE_DESK_RENDER_DIR") {
            let rows=(0..24).map(|y|(0..100).map(|x| {
                let c=&buffer[(x,y)];serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)})
            }).collect::<Vec<_>>()).collect::<Vec<_>>();
            std::fs::write(
                std::path::Path::new(&folder).join(format!("desk-{scenario}.json")),
                serde_json::to_vec(&rows).unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn recovery_writes_one_document_and_reopens_its_shared_history() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("main.rs");
    std::fs::write(&path, "saved\n").unwrap();
    let mut app = App::new(root.path().to_owned());
    let mut ed = Editor::test_with_text("saved\n");
    ed.file_path = Some(path);
    ed.saved_rope = Some(ed.rope.clone());
    app.tabs = vec![Tab::new_file(ed)];
    app.editor_mut().unwrap().paste_plain_with_text("edited ");
    app.new_desk("Copy", true);
    app.new_desk("Another", true);
    let session = app.build_session();
    crate::session::save_session(root.path(), &session).unwrap();
    let saved = crate::session::load_session(root.path()).unwrap();
    let text = saved.tabs[0].unsaved_file.as_ref().unwrap();
    let history = saved.tabs[0].undo_history_file.as_ref().unwrap();
    for tab in saved
        .tabs
        .iter()
        .chain(saved.groups.iter().flat_map(|g| &g.tabs))
    {
        assert_eq!(tab.unsaved_file.as_ref(), Some(text));
        assert_eq!(tab.undo_history_file.as_ref(), Some(history));
    }
    let mut restored = App::new(root.path().to_owned());
    restored.restore_session(&saved);
    restored.switch_desk(0);
    restored.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.tabs
            .iter()
            .chain(app.groups.iter().flat_map(|g| &g.tabs))
            .any(|t| t.editor.bg_loading)
    });
    assert_eq!(
        restored.editor().unwrap().rope.to_string(),
        "edited saved\n"
    );
    restored.editor_mut().unwrap().undo();
    assert_eq!(restored.editor().unwrap().rope.to_string(), "saved\n");
    let other = restored.desks.items[1].id;
    restored.switch_desk(other);
    assert!(!restored.editor().unwrap().modified);
    assert!(restored.editor().unwrap().can_redo());
}

#[test]
fn session_restores_settings_tab_in_its_own_desk_and_group() {
    let mut app = app();
    app.open_settings_tab();
    app.group_selected_tabs("Tools", None);
    let settings_desk = app.desks.active;
    app.new_desk("Code", false);
    let session = app.build_session();
    let mut restored = App::test_new();
    restored.restore_session(&session);
    restored.switch_desk(settings_desk);
    assert_eq!(
        restored.tabs[restored.active_tab].kind,
        crate::app::TabKind::Settings
    );
    assert_eq!(
        restored
            .tab_group(&restored.tabs[restored.active_tab])
            .unwrap()
            .name,
        "Tools"
    );
    assert!(restored.settings_editor_active);
}

#[test]
fn group_reordering_in_one_pane_preserves_active_tab_and_mru() {
    let mut app = app();
    app.tab_mru_order = vec![0, 2, 1];
    let ids: Vec<_> = app.tabs.iter().map(|t| t.editor.navigation_id).collect();
    app.desks.selected_tabs.extend(ids[..2].iter().copied());
    app.group_selected_tabs("Work", None);
    let group = app.tab_group(&app.tabs[0]).unwrap().id;
    app.move_named_group(group, 0, None);
    assert_eq!(
        app.tabs
            .iter()
            .map(|t| t.editor.navigation_id)
            .collect::<Vec<_>>(),
        vec![ids[2], ids[0], ids[1]]
    );
    assert_eq!(app.tabs[app.active_tab].editor.navigation_id, ids[0]);
    assert_eq!(app.tab_mru_order, vec![1, 0, 2]);
    assert!(!app.split_active());
}

#[test]
fn dragging_active_group_to_another_pane_keeps_the_same_file_focused() {
    let mut app = app();
    let active = app.tabs[0].editor.navigation_id;
    let other = app.tabs[2].editor.navigation_id;
    app.desks
        .selected_tabs
        .extend(app.tabs[..2].iter().map(|t| t.editor.navigation_id));
    app.group_selected_tabs("Work", None);
    let group = app.tab_group(&app.tabs[0]).unwrap().id;
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    let target = app.active_group;
    app.switch_group(0);
    app.move_named_group(group, target, Some(other));
    assert_eq!(app.editor().unwrap().navigation_id, active);
    assert_eq!(app.tabs.len(), 3);
    assert!(!app.split_active());
    assert!(app.tab_mru_order.iter().all(|i| *i < 3));
}

#[test]
fn corrupt_desk_layout_is_repaired_without_losing_active_files() {
    let mut app = app();
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    app.new_desk("Copy", true);
    let mut session = app.build_session();
    let saved = session.desks.as_mut().unwrap();
    for desk in &mut saved.items {
        let layout = desk.layout.as_mut().unwrap();
        layout.active_group = usize::MAX;
        layout.tree = Some(crate::app::SplitNode::Leaf(usize::MAX));
        layout.panel_height = 0;
        layout.panel_tab = 255;
        layout.explorer_width = 0;
    }
    let mut restored = App::test_new();
    restored.restore_session(&session);
    assert_eq!(restored.editor().unwrap().file_name(), "notes.md");
    for id in restored
        .desks
        .items
        .iter()
        .map(|d| d.id)
        .collect::<Vec<_>>()
    {
        restored.switch_desk(id);
        let leaves = restored.desk_panes();
        assert_eq!(leaves.len(), 2);
        assert!(
            leaves
                .iter()
                .all(|&i| restored.groups.get(i).is_some_and(|g| g.desk == id))
        );
        assert!(
            restored.panel_height >= 3 && matches!(restored.panel_tab, 1 | 3) && restored.explorer_width >= 15
        );
    }
}

#[test]
fn desk_plus_and_multiselect_use_the_drawn_hit_targets() {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Modifier};
    let mut app = app();
    let area = Rect::new(0, 0, 100, 1);
    app.tab_bar_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let click = |x, y, modifiers| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers,
    };
    assert!(crate::ui_desks::handle_mouse(
        &mut app,
        click(1, 0, KeyModifiers::empty())
    ));
    assert_eq!(app.desks.popup.mode, Some(crate::ui_desks::Mode::New));
    crate::ui_desks::paste(&mut app, "Debugging");
    terminal
        .draw(|f| crate::ui_desks::draw(f, &mut app))
        .unwrap();
    let popup = app.desks.popup.area;
    let empty_row = app.desks.popup.input.bottom();
    let copy_row = popup.bottom() - 2;
    assert_eq!(copy_row, empty_row + 1, "no footer below the two choices");
    assert!(crate::ui_desks::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: popup.x + 2,
            row: copy_row,
            modifiers: KeyModifiers::empty(),
        },
    ));
    terminal
        .draw(|f| crate::ui_desks::draw(f, &mut app))
        .unwrap();
    assert_eq!(app.desks.popup.selected, 1);
    let buffer = terminal.backend().buffer();
    assert_ne!(buffer[(popup.x + 2, copy_row)].bg, buffer[(popup.x + 2, empty_row)].bg);
    for x in popup.x + 1..popup.right() - 1 {
        assert!(!buffer[(x, copy_row)].modifier.contains(Modifier::UNDERLINED));
    }
    assert!(crate::ui_desks::handle_mouse(
        &mut app,
        click(popup.x + 2, copy_row, KeyModifiers::empty()),
    ));
    assert_eq!(app.current_desk_name(), "Debugging");
    assert_eq!(app.tabs.len(), 3, "Copy layout uses the bottom row's hit target");
    crate::ui_desks::open(&mut app, crate::ui_desks::Mode::New);
    crate::ui_desks::paste(&mut app, "Empty");
    crate::ui_desks::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert_eq!(app.current_desk_name(), "Empty");
    assert!(app.tabs.is_empty());
    app.switch_desk(0);
    app.desks.host = app.active_group;
    app.tab_bar_area = Some(area);
    terminal
        .draw(|f| crate::ui_chrome_tabs::draw_tab_bar(f, &app, area))
        .unwrap();
    let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, area);
    let second = hits.tabs[1].1;
    assert!(crate::ui_desks::handle_mouse(
        &mut app,
        click(second.x + 2, 0, KeyModifiers::CONTROL)
    ));
    assert_eq!(app.desks.selected_tabs.len(), 2);
    app.group_selected_tabs("Work", None);
    assert!(app.tabs[..2].iter().all(|t| app.tab_group(t).is_some()));
}

#[test]
fn document_metadata_is_shared_without_changing_view_preferences() {
    let mut app = app();
    app.tabs[0].editor.tab_size = 2;
    app.tabs[0].editor.use_tabs = true;
    app.tabs[0].editor.ec_trim_trailing = Some(true);
    app.tabs[0].editor.ec_insert_final_newline = Some(false);
    app.tabs[0].editor.word_wrap = true;
    let copy = app.new_desk("Copy", true);
    assert_eq!(app.editor().unwrap().tab_size, 2);
    assert!(app.editor().unwrap().use_tabs);
    assert_eq!(app.editor().unwrap().ec_trim_trailing, Some(true));
    assert_eq!(app.editor().unwrap().ec_insert_final_newline, Some(false));
    assert!(app.editor().unwrap().word_wrap);
    app.editor_mut().unwrap().tab_size = 8;
    app.editor_mut().unwrap().use_tabs = false;
    app.editor_mut().unwrap().word_wrap = false;
    app.sync_duplicate_file_tabs();
    app.switch_desk(0);
    assert_eq!(app.editor().unwrap().tab_size, 8);
    assert!(!app.editor().unwrap().use_tabs);
    assert!(app.editor().unwrap().word_wrap);
    app.switch_desk(copy);
    assert!(!app.editor().unwrap().word_wrap);
}

#[test]
fn manual_tab_scrolling_does_not_snap_back_when_groups_are_present() {
    let mut app = app();
    app.group_selected_tabs("Work", None);
    app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 34, 1));
    app.tab_scroll_offset = 2;
    app.tab_scroll_manual_anchor = Some(0);
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, 2);
    app.tab_scroll_manual_anchor = None;
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, 0);
}

#[test]
fn desk_switch_dismisses_keyboard_shortcuts_input_before_the_next_frame() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = app();
    app.open_keybindings_tab();
    assert!(crate::ui_keybindings::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    ));
    let other = app.new_desk("Code", false);
    assert!(!crate::ui_keybindings::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    ));
    app.switch_desk(0);
    assert!(crate::ui_keybindings::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    ));
    app.switch_desk(other);
    assert!(!crate::ui_keybindings::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    ));
}

#[test]
fn save_finishing_in_hidden_view_preserves_new_edits_and_shared_undo() {
    let mut a = Editor::test_with_text("base\n");
    a.saved_rope = Some(a.rope.clone());
    let mut b = Editor::new();
    b.copy_document_from(&a);
    a.paste_plain_with_text("saved ");
    let version = a.content_version;
    let submitted = a.rope.clone();
    b.paste_plain_with_text("new ");
    a.apply_background_save(
        version,
        &submitted,
        submitted.clone(),
        "UTF-8",
        None,
        None,
        None,
        50 * 1024 * 1024,
    );
    b.refresh_document();
    assert_eq!(b.rope.to_string(), "new saved base\n");
    assert!(b.modified);
    b.undo();
    assert_eq!(b.rope.to_string(), "saved base\n");
    assert!(!b.modified);
    a.redo();
    assert_eq!(a.rope.to_string(), "new saved base\n");
    assert!(a.modified);
}

#[test]
fn language_server_edits_and_undo_share_the_document_across_views() {
    let mut a = Editor::test_with_text("original\n");
    a.saved_rope = Some(a.rope.clone());
    let mut b = Editor::new();
    b.copy_document_from(&a);
    crate::app_lsp_edits::apply(
        &mut a,
        &[lsp_types::TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 0),
                lsp_types::Position::new(0, 8),
            ),
            new_text: "formatted".into(),
        }],
    )
    .unwrap();
    b.refresh_document();
    assert_eq!(b.rope.to_string(), "formatted\n");
    b.undo();
    assert_eq!(b.rope.to_string(), "original\n");
    assert!(!b.modified);
    a.redo();
    assert_eq!(a.rope.to_string(), "formatted\n");
    assert!(a.modified);
}
