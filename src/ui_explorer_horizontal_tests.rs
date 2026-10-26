use super::*;
use crate::{app::{ActivePane, SidebarView, Tab}, editor::Editor, explorer::FileEntry};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn entry(app: &App, name: &str, depth: usize) -> FileEntry {
    FileEntry { path: app.explorer.root.join(name), name: name.into(), depth, is_dir: false,
        expanded: false, git_status: GitStatus::Unmodified, git_status_summary: Default::default() }
}

fn replace(app: &mut App, entries: Vec<FileEntry>) {
    app.explorer.apply_refresh_entries(app.explorer.root.clone(), entries, Default::default());
}

fn fixture(right: bool) -> App {
    let mut app = App::test_new();
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Explorer;
    app.sidebar_right = right;
    app.explorer_width = 26;
    app.show_open_editors = false;
    app.show_panel = false;
    app.active_pane = ActivePane::Editor;
    app.tabs = vec![Tab::new_file(Editor::test_with_text(&"x".repeat(200)))];
    app.editor_mut().unwrap().horizontal_scroll = 9;
    app.cached_max_line_width = 200;
    app
}

fn draw(app: &mut App) -> Buffer {
    draw_width(app, 100)
}

fn draw_width(app: &mut App, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap().buffer.clone()
}

fn horizontal(app: &App) -> Option<ExplorerScrollbar> {
    explorer_scrollbar(app, app.explorer_area.unwrap(), ExplorerSection::FilesHorizontal)
}

fn mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16, modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(app, MouseEvent { kind, column: col, row, modifiers });
}

#[test]
fn horizontal_explorer_wheel_routes_to_tree_on_both_sidebar_sides() {
    for right in [false, true] {
        let mut app = fixture(right);
        let entries = (0..40).map(|i| entry(&app, &format!("a_very_long_file_name_number_{i:02}.rs"), 12)).collect();
        replace(&mut app, entries);
        draw(&mut app);
        let tree = explorer_tree_content_area(&app, app.explorer_area.unwrap());
        let x = tree.x + 4;
        let y = tree.y + 2;
        for (kind, modifiers, expected) in [
            (MouseEventKind::ScrollDown, KeyModifiers::SHIFT, 6),
            (MouseEventKind::ScrollRight, KeyModifiers::NONE, 12),
            (MouseEventKind::ScrollUp, KeyModifiers::SHIFT, 6),
            (MouseEventKind::ScrollLeft, KeyModifiers::NONE, 0),
        ] {
            mouse(&mut app, kind, x, y, modifiers);
            assert_eq!(app.explorer.horizontal.offset, expected);
            assert_eq!(app.explorer.scroll_offset, 0);
            assert_eq!(app.editor().unwrap().horizontal_scroll, 9);
        }
        mouse(&mut app, MouseEventKind::ScrollDown, x, y, KeyModifiers::NONE);
        assert!(app.explorer.scroll_offset > 0, "ordinary wheel remains vertical");
        let area = app.explorer_area.unwrap();
        mouse(&mut app, MouseEventKind::ScrollRight, area.x + 3, area.y, KeyModifiers::NONE);
        assert_eq!(app.explorer.horizontal.offset, 0, "headers stay fixed");
        assert_eq!(app.editor().unwrap().horizontal_scroll, 9);
    }
}

#[test]
fn horizontal_explorer_only_reserves_space_for_overflow_and_clamps_after_changes() {
    for right in [false, true] {
        let mut app = fixture(right);
        let short = entry(&app, "small.rs", 0);
        replace(&mut app, vec![short.clone()]);
        draw(&mut app);
        assert!(horizontal(&app).is_none());
        let height = app.explorer.visible_height;
        let long = entry(&app, "long_name_with_a_distinct_last.rs", 14);
        replace(&mut app, vec![long]);
        draw(&mut app);
        assert_eq!(app.explorer.visible_height + 1, height);
        let bar = horizontal(&app).unwrap();
        assert!(bar.area.x >= app.explorer_area.unwrap().x);
        scroll_explorer_horizontal_at(&mut app, bar.area.x, bar.area.y, isize::MAX);
        let buffer = draw(&mut app);
        let tree = explorer_tree_content_area(&app, app.explorer_area.unwrap());
        let line: String = (tree.x..tree.right()).map(|x| buffer[(x, tree.y)].symbol()).collect();
        assert!(line.contains("distinct_last.rs"), "{line:?}");
        assert_eq!(app.explorer.horizontal.offset, horizontal(&app).unwrap().max_scroll);
        app.explorer_width = 85;
        draw_width(&mut app, 180);
        assert!(horizontal(&app).is_none());
        assert_eq!(app.explorer.horizontal.offset, 0);
        app.explorer_width = 26;
        draw(&mut app);
        app.explorer.horizontal.offset = 500;
        replace(&mut app, vec![short]);
        draw(&mut app);
        assert!(horizontal(&app).is_none());
        assert_eq!(app.explorer.horizontal.offset, 0);
        assert_eq!(app.explorer.visible_height, height);
    }
}

#[test]
fn horizontal_explorer_drag_captures_pointer_without_opening_files_or_resizing() {
    for right in [false, true] {
        let mut app = fixture(right);
        let long = entry(&app, "a_long_name_that_extends_beyond_the_panel.rs", 20);
        replace(&mut app, vec![long]);
        app.explorer.horizontal.offset = 7;
        let buffer = draw(&mut app);
        let bar = horizontal(&app).unwrap();
        let x = bar.area.x + bar.thumb_top;
        assert_eq!(buffer[(x, bar.area.y)].symbol(), "▄");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, bar.area.y, KeyModifiers::NONE);
        assert_eq!(app.explorer.horizontal.offset, 7, "grabbing must not jump");
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, 0, KeyModifiers::NONE);
        assert_eq!(app.explorer.horizontal.offset, 7, "vertical motion must not move the thumb");
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 1000, 0, KeyModifiers::NONE);
        assert_eq!(app.explorer.horizontal.offset, bar.max_scroll);
        assert!(!app.resizing_explorer);
        assert!(app.dragging_explorer_path.is_none());
        assert_eq!(app.explorer_width, 26);
        assert_eq!(app.tabs.len(), 1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 1000, 0, KeyModifiers::NONE);
        assert!(app.explorer_scrollbar_drag.is_none());
        draw(&mut app);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), bar.area.x, bar.area.y, KeyModifiers::NONE);
        assert_eq!(app.explorer.horizontal.offset, 0, "track click reaches start");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), bar.area.x, bar.area.y, KeyModifiers::NONE);
        assert_eq!(app.explorer.selected, 0);
    }
}

#[test]
fn horizontal_explorer_and_vertical_scrollbars_have_stable_shared_geometry() {
    let mut app = fixture(false);
    replace(&mut app, vec![]);
    draw(&mut app);
    let area = app.explorer_area.unwrap();
    let raw = explorer_tree_raw_area(&app, area);
    // Horizontal overflow removes a row, which then requires a vertical bar.
    let name = "x".repeat(raw.width as usize - 2);
    let entries = (0..raw.height).map(|i| { let mut item = entry(&app, &name, 0); item.path = app.explorer.root.join(i.to_string()); item }).collect();
    replace(&mut app, entries);
    let mut previous = None;
    for _ in 0..4 {
        draw(&mut app);
        let h = horizontal(&app).unwrap();
        let v = explorer_scrollbar(&app, area, ExplorerSection::Files).unwrap();
        assert_eq!(v.area.bottom(), h.area.y);
        assert_eq!(h.area.right(), v.area.x);
        assert_eq!(app.explorer.visible_height, v.area.height as usize);
        let geometry = (h.area, v.area);
        if let Some(previous) = previous { assert_eq!(geometry, previous); }
        previous = Some(geometry);
    }
}

#[test]
fn horizontal_explorer_width_refreshes_for_same_length_snapshots_and_git_badges() {
    let mut app = fixture(false);
    replace(&mut app, vec![]);
    draw(&mut app);
    let width = explorer_tree_raw_area(&app, app.explorer_area.unwrap()).width as usize;
    let name = "x".repeat(width - 3); // Unknown file icon is two cells.
    let item = entry(&app, &name, 0);
    let name = "x".repeat(width - item.prefix_width());
    let item = entry(&app, &name, 0);
    replace(&mut app, vec![item]);
    draw(&mut app);
    assert!(horizontal(&app).is_none(), "exact fit has no bar");
    app.explorer.apply_git_status_changes(None, [(name.as_str(), 'M')]);
    draw(&mut app);
    assert!(horizontal(&app).is_some(), "badge contributes to row width");
    app.explorer.apply_git_status_changes(None, std::iter::empty());
    draw(&mut app);
    assert!(horizontal(&app).is_none());
    app.editor_mut().unwrap().file_path = Some(app.explorer.entries[0].path.clone());
    app.editor_mut().unwrap().insert_char('z');
    draw(&mut app);
    assert_eq!(horizontal(&app).unwrap().max_scroll, 2, "unsaved dot contributes to width");
    app.editor_mut().unwrap().undo();
    draw(&mut app);
    assert!(horizontal(&app).is_none(), "undoing the edit removes the extra width");
    let wider = entry(&app, "this_replaces_one_entry_with_a_longer_one", 3);
    replace(&mut app, vec![wider]);
    draw(&mut app);
    assert!(horizontal(&app).is_some());
}

#[test]
fn hidden_untracked_colors_match_clean_files_folders_and_workspace() {
    use crate::{app_theme::EditorTheme, source_control_rows::Visibility};
    for right in [false, true] {
        for theme in [EditorTheme::Dark, EditorTheme::Light] {
            let mut app = fixture(right);
            app.colors = theme.colors();
            app.explorer_width = 48;
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
            let mut folder = entry(&app, "new-dir", 0);
            folder.is_dir = true;
            folder.expanded = true;
            let mut child = entry(&app, "inside_界.rs", 1);
            child.path = folder.path.join(&child.name);
            let mut ignored = entry(&app, "ignored.log", 0);
            ignored.git_status = GitStatus::Ignored;
            ignored.git_status_summary = GitStatus::Ignored.into();
            let entries = vec![folder, child, entry(&app, "new.rs", 0), entry(&app, "clean.rs", 0), ignored];
            replace(&mut app, entries);
            let refresh = |app: &mut App| app.explorer.apply_git_status_changes(None,
                [("new-dir/inside_界.rs", '?'), ("new.rs", '?')]);
            for focus in 0..4 {
                app.active_pane = if focus == 2 { ActivePane::Explorer } else { ActivePane::Editor };
                app.hovered_explorer = (focus == 1).then_some(0);
                app.explorer_multi_selected.clear();
                app.explorer_selection_anchor = None;
                if focus == 2 {
                    app.explorer_multi_selected.push(app.explorer.entries[0].path.clone());
                    app.explorer_selection_anchor = Some(0);
                }
                app.dragging_explorer_path = (focus == 3).then(|| app.explorer.root.join("other.rs"));
                app.explorer_drop_target_idx = (focus == 3).then_some(0);
                refresh(&mut app);
                app.set_source_control_visibility(Visibility::default());
                let shown = draw(&mut app);
                app.set_source_control_visibility(Visibility { untracked: false });
                // A late snapshot must obey current visibility for colors too.
                refresh(&mut app);
                let statuses = app.explorer.git_statuses.clone();
                let hidden = draw(&mut app);
                assert_eq!(app.explorer.git_statuses, statuses, "Filtering is only a presentation change");
                app.explorer.apply_git_status_changes(None, std::iter::empty::<(&str, char)>());
                let clean = draw(&mut app);
                assert_eq!(hidden, clean,
                    "Hidden U decorations must leave the exact clean text/icon/header colors (right={right}, focus={focus})");
                assert_ne!(shown, hidden, "The fixture must exercise visible Git decorations");
                refresh(&mut app);
                app.set_source_control_visibility(Visibility::default());
                assert_eq!(draw(&mut app), shown, "Restoring visibility restores cached colors immediately");
            }
        }
    }
}

#[test]
fn hidden_untracked_colors_preserve_tracked_folder_colors_and_disable_all_with_source_control() {
    use crate::{app_theme::EditorTheme, source_control_rows::Visibility};
    for right in [false, true] {
        for theme in [EditorTheme::Dark, EditorTheme::Light] {
            let mut app = fixture(right);
            app.colors = theme.colors();
            app.explorer_width = 48;
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
            let mut folder = entry(&app, "mixed", 0);
            folder.is_dir = true;
            folder.expanded = true;
            let mut entries = vec![folder];
            for name in ["modified.rs", "new.rs"] {
                let mut child = entry(&app, name, 1);
                child.path = app.explorer.root.join("mixed").join(name);
                entries.push(child);
            }
            for name in ["modified.rs", "added.rs", "deleted.rs", "renamed.rs", "new.rs"] {
                entries.push(entry(&app, name, 0));
            }
            let mut ignored = entry(&app, "ignored.log", 0);
            ignored.git_status = GitStatus::Ignored;
            ignored.git_status_summary = GitStatus::Ignored.into();
            entries.push(ignored);
            replace(&mut app, entries);
            let changes = [("mixed/modified.rs", 'M'), ("mixed/new.rs", '?'), ("modified.rs", 'M'),
                ("added.rs", 'A'), ("deleted.rs", 'D'), ("renamed.rs", 'R'), ("new.rs", '?')];
            app.explorer.apply_git_status_changes(None, changes);
            app.editor_mut().unwrap().file_path = Some(app.explorer.root.join("modified.rs"));
            app.editor_mut().unwrap().insert_char('z');
            app.set_source_control_visibility(Visibility { untracked: false });
            let hidden = draw(&mut app);
            app.explorer.apply_git_status_changes(None, changes.into_iter().filter(|(_, status)| *status != '?'));
            assert_eq!(draw(&mut app), hidden,
                "Mixed directories must use the remaining tracked status color, including the dirty marker");
            app.explorer.apply_git_status_changes(None, changes);
            app.source_control_enabled = false;
            let disabled = draw(&mut app);
            // Retained local/remote snapshots cannot leak colors after disabling Git.
            app.explorer.set_source_control_enabled(false);
            assert_eq!(draw(&mut app), disabled,
                "Disabled Source Control must render like a tree without Git data");
        }
    }
}

#[test]
fn untracked_filter_hides_only_u_badges_on_files_folders_and_workspace() {
    use crate::source_control_rows::Visibility;
    for right in [false, true] {
        let mut app = fixture(right);
        app.explorer_width = 48;
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        let mut folder = entry(&app, "src", 0);
        folder.is_dir = true;
        let changes = [("modified.rs", 'M'), ("new.rs", '?'), ("added.rs", 'A'), ("deleted.rs", 'D'), ("renamed.rs", 'R')];
        let mut entries = vec![folder];
        entries.extend(changes.iter().map(|(name, _)| entry(&app, name, 0)));
        let mut ignored = entry(&app, "ignored.log", 0);
        ignored.git_status = GitStatus::Ignored;
        ignored.git_status_summary = GitStatus::Ignored.into();
        entries.push(ignored);
        replace(&mut app, entries);
        let refresh = |app: &mut App| app.explorer.apply_git_status_changes(None,
            changes.iter().map(|&(path, status)| (path, status))
                .chain([("src/inner/new.rs", '?'), ("src/inner/edited.rs", 'M')]));
        refresh(&mut app);
        app.explorer.find.results.rows.insert(app.explorer.root.join("src"), crate::explorer_find::Match {
            descendants: 12, ..Default::default()
        });
        app.editor_mut().unwrap().file_path = Some(app.explorer.root.join("modified.rs"));
        app.editor_mut().unwrap().insert_char('z');
        let rows = |app: &App, buffer: &Buffer| {
            let area = app.explorer_area.unwrap();
            let header = explorer_section_area(app, explorer_sections(app, area).1).y;
            let tree = explorer_tree_content_area(app, area);
            std::iter::once(header).chain(tree.y..tree.y + app.explorer.entries.len() as u16)
                .map(|y| (area.x..area.right()).map(|x| buffer[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>()
        };
        let shown = draw(&mut app);
        let shown_rows = rows(&app, &shown);
        assert!(shown_rows[0].contains("M R A U D"), "{:?}", shown_rows[0]);
        assert!(shown_rows[1].contains("M U 12"), "{:?}", shown_rows[1]);
        for (index, badge) in ['M', 'U', 'A', 'D', 'R'].into_iter().enumerate() {
            let badge = badge.to_string();
            assert!(shown_rows[index + 2].split_whitespace().any(|token| token == badge), "{:?}", shown_rows[index + 2]);
        }
        let statuses = app.explorer.git_statuses.clone();
        app.set_source_control_visibility(Visibility { untracked: false });
        // Simulate a late local/remote Git result; hidden badges cannot return.
        refresh(&mut app);
        let hidden = draw(&mut app);
        let hidden_rows = rows(&app, &hidden);
        for (shown, hidden) in shown_rows.iter().zip(&hidden_rows) {
            let expected: Vec<_> = shown.split_whitespace().filter(|token| *token != "U").collect();
            assert_eq!(hidden.split_whitespace().collect::<Vec<_>>(), expected, "only U badges are hidden");
        }
        assert!(hidden_rows[1].contains("12"), "find counts remain visible");
        assert!(hidden_rows[2].contains('●'), "unsaved changes remain visible");
        let ignored_row = explorer_tree_content_area(&app, app.explorer_area.unwrap()).y + 6;
        let area = app.explorer_area.unwrap();
        for x in area.x..area.right() { assert_eq!(hidden[(x, ignored_row)], shown[(x, ignored_row)]); }
        assert_eq!(app.explorer.git_statuses, statuses, "the display filter retains Git data");
        app.set_source_control_visibility(Visibility::default());
        let restored = draw(&mut app);
        assert_eq!(rows(&app, &restored), shown_rows, "toggle restores cached decorations immediately");
        // Even a retained snapshot cannot show badges after Source Control is disabled.
        app.source_control_enabled = false;
        let disabled = draw(&mut app);
        for row in rows(&app, &disabled) {
            assert!(!row.split_whitespace().any(|token| matches!(token, "M" | "R" | "A" | "U" | "D")), "{row:?}");
        }
    }
}

#[test]
fn untracked_filter_removes_badge_width_and_clamps_horizontal_scroll_without_hiding_dirty_dot() {
    use crate::source_control_rows::Visibility;
    for right in [false, true] {
        let mut app = fixture(right);
        replace(&mut app, vec![]);
        draw(&mut app);
        let width = explorer_tree_raw_area(&app, app.explorer_area.unwrap()).width as usize;
        let item = entry(&app, "x", 0);
        let name = "x".repeat(width - item.prefix_width());
        let item = entry(&app, &name, 0);
        replace(&mut app, vec![item]);
        app.explorer.apply_git_status_changes(None, [(name.as_str(), '?')]);
        draw(&mut app);
        assert!(horizontal(&app).is_some());
        app.explorer.horizontal.offset = usize::MAX;
        app.set_source_control_visibility(Visibility { untracked: false });
        draw(&mut app);
        assert!(horizontal(&app).is_none(), "hidden badges occupy no width");
        assert_eq!(app.explorer.horizontal.offset, 0);
        app.editor_mut().unwrap().file_path = Some(app.explorer.entries[0].path.clone());
        app.editor_mut().unwrap().insert_char('z');
        draw(&mut app);
        assert_eq!(horizontal(&app).unwrap().max_scroll, 2, "dirty indicator still contributes to width");
        app.editor_mut().unwrap().undo();
        draw(&mut app);
        assert!(horizontal(&app).is_none());
        app.set_source_control_visibility(Visibility::default());
        draw(&mut app);
        assert!(horizontal(&app).is_some(), "visible badges use the separate cached width");
        app.explorer.apply_git_status_changes(None, [(name.as_str(), 'M')]);
        app.set_source_control_visibility(Visibility { untracked: false });
        draw(&mut app);
        assert!(horizontal(&app).is_some(), "tracked badges still occupy their visible width");
    }
}

#[test]
fn horizontal_explorer_keeps_find_and_open_editor_sections_fixed() {
    let mut app = fixture(false);
    app.show_open_editors = true;
    app.explorer.find.open = true;
    app.tabs.extend((0..20).map(|_| Tab::new_file(Editor::new())));
    let long = entry(&app, "a_long_name_beyond_the_visible_file_tree.rs", 16);
    replace(&mut app, vec![long.clone()]);
    draw(&mut app);
    let area = app.explorer_area.unwrap();
    let find = explorer_find_area(&app, area);
    let bar = horizontal(&app).unwrap();
    assert!(bar.area.y >= find.bottom());
    let open = explorer_scrollbar(&app, area, ExplorerSection::OpenEditors).unwrap();
    let open_offset = open.offset;
    mouse(&mut app, MouseEventKind::ScrollRight, bar.area.x, bar.area.y, KeyModifiers::NONE);
    assert!(app.explorer.horizontal.offset > 0);
    assert_eq!(explorer_scrollbar(&app, area, ExplorerSection::OpenEditors).unwrap().offset, open_offset);
    let offset = app.explorer.horizontal.offset;
    assert!(scroll_explorer_horizontal_at(&mut app, find.x, find.y, 6));
    assert_eq!(app.explorer.horizontal.offset, offset);
    let tree = explorer_tree_content_area(&app, area);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Right), tree.x + 2, tree.y, KeyModifiers::NONE);
    assert_eq!(app.explorer_context_target, Some(long.path), "row hit testing follows scrolled text");
}

#[test]
fn horizontal_explorer_unicode_highlighting_and_rename_stay_inside_viewport() {
    let mut app = fixture(false);
    let item = entry(&app, "组件_café_e\u{301}_👩‍💻_last.rs", 18);
    let mut matches = crate::explorer_find::Matches::default();
    matches.rows.insert(item.path.clone(), crate::explorer_find::Match { matched: true, indices: vec![0, 9, 11, 12], ..Default::default() });
    app.explorer.apply_refresh_entries(app.explorer.root.clone(), vec![item.clone()], matches);
    draw(&mut app);
    let expected = item.prefix_width() + UnicodeWidthStr::width(item.name.as_str());
    assert_eq!(app.explorer.content_width(crate::explorer::GitBadgeVisibility::All), expected);
    let bar = horizontal(&app).unwrap();
    scroll_explorer_horizontal_at(&mut app, bar.area.x, bar.area.y, isize::MAX);
    let buffer = draw(&mut app);
    let tree = explorer_tree_content_area(&app, app.explorer_area.unwrap());
    let line: String = (tree.x..tree.right()).map(|x| buffer[(x, tree.y)].symbol()).collect();
    assert!(line.contains("last.rs"), "{line:?}");
    app.explorer.horizontal.offset = 0;
    app.rename.active = true;
    app.rename.target = Some(item.path);
    app.rename.query = "renamed_file.rs".into();
    let buffer = draw(&mut app);
    assert!(app.explorer.horizontal.offset > 0, "rename reveals deeply indented field");
    let line: String = (tree.x..tree.right()).map(|x| buffer[(x, tree.y)].symbol()).collect();
    assert!(line.contains("renamed"), "{line:?}");
    for width in [1, 2, 5] {
        let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
        terminal.draw(|frame| draw_explorer(frame, &mut app, Rect::new(0, 0, width, 8))).unwrap();
    }
}
