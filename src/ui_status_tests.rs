use super::*;
use crate::app::Tab;
use ratatui::{Terminal, backend::TestBackend};

fn app_with_file() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("fn main() {\n    println!(\"Hello, world!\");\n}\n");
    editor.file_path = Some("src/main.rs".into());
    editor.language_override = Some("Rust".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.cached_git_branch = "main".into();
    app
}

fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (buffer.area.x..buffer.area.right())
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    app.status_bar_area = Some(Rect::new(0, height - 1, width, 1));
    terminal
        .draw(|frame| {
            draw_bar(frame, app, Rect::new(0, height - 1, width, 1));
            draw_overlays(frame, app);
        })
        .unwrap();
    terminal
}

#[test]
fn edge_case_selection_counts_follow_the_active_document() {
    let mut app = app_with_file();
    app.tabs = ["a\nbc", "abcd"].into_iter().map(|text| {
        let mut editor = Editor::test_with_text(text);
        let end = editor.rope.char_to_line(3);
        editor.set_cursor(end, 3 - editor.rope.line_to_char(end));
        editor.selection_anchor = Some((0, 0));
        Tab::new_file(editor)
    }).collect();
    for (tab, expected) in [(0, "(3 selected, 2 lines)"), (1, "(3 selected)"), (0, "(3 selected, 2 lines)")] {
        app.active_tab = tab;
        draw(&mut app, 160, 24);
        assert_eq!(app.cached_sel_stats.as_deref(), Some(expected));
    }

    // A reload can replace a rope without changing its version or character range.
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str("abcd");
    editor.set_cursor(0, 3);
    editor.selection_anchor = Some((0, 0));
    draw(&mut app, 160, 24);
    assert_eq!(app.cached_sel_stats.as_deref(), Some("(3 selected)"));
}

#[test]
fn language_server_status_opens_setup_from_bar_and_details() {
    for pinned in [true, false] {
        let mut app = app_with_file();
        click_language_server_status(&mut app, pinned);
        assert!(app.lsp_settings.active);
        assert_eq!(app.settings_category_selected, 8);
        assert!(!app.status_details.open);
        assert_eq!(crate::ui_modal::active(&app), Some(crate::ui_modal::Kind::LspSettings));
    }
}

#[test]
fn completion_index_is_discoverable_from_the_status_bar_and_details() {
    let mut app = app_with_file();
    draw(&mut app, 180, 30);
    assert!(
        app.status_bar_items
            .iter()
            .any(|item| item.2 == "completion_index")
    );
    assert!(
        all_detail_rows(&app)
            .iter()
            .any(|row| row.id == "completion_index" && row.action == Some("completion_index"))
    );
    activate(&mut app, "completion_index");
    assert!(app.completion_index_ui.open);
    assert!(!app.status_details.open);
}

#[test]
fn completion_control_survives_selection_and_activity_when_compact_items_fit() {
    let mut app = app_with_file();
    app.cached_git_branch = "feature/a-long-branch-name".into();
    app.cached_error_count = 12;
    app.cached_warning_count = 3;
    for width in [60, 64, 72, 80, 100] {
        for selected in [false, true, false] {
            let editor = app.editor_mut().unwrap();
            editor.set_cursor(2, 1);
            editor.selection_anchor = selected.then_some((0, 0));
            draw(&mut app, width, 8);
            assert!(
                app.status_bar_items
                    .iter()
                    .any(|item| item.2 == "completion_index"),
                "learning control disappeared at {width} with selection={selected}: {:?}",
                app.status_bar_items
            );
        }
    }
    app.editor_mut().unwrap().selection_anchor = Some((0, 0));
    app.formatting.feedback = Some(("Formatting complete".into(), Instant::now()));
    draw(&mut app, 80, 8);
    let &(x, width, _) = app
        .status_bar_items
        .iter()
        .find(|item| item.2 == "completion_index")
        .expect("a transient formatting message must not displace the learning control");
    assert!(width >= 3);
    let before = app.editor().unwrap().rope.clone();
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), x + 1, 7),
    );
    assert!(app.completion_index_ui.open);
    assert_eq!(app.editor().unwrap().rope, before);
}

#[test]
fn learning_status_uses_statusbar_foreground_even_when_accents_match_its_background() {
    use crate::completion_index::{IndexPhase, IndexSettings, IndexStatus};
    let mut app = app_with_file();
    for theme in [
        crate::app_theme::EditorTheme::Dark,
        crate::app_theme::EditorTheme::Light,
    ] {
        app.colors = theme.colors();
        app.colors.ui.preserve_theme_colors = true;
        // Imported themes intentionally preserve their roles. An editor accent
        // is allowed to match the status surface; it is not a text role here.
        app.colors.fg_accent = app.colors.bg_statusbar;
        app.colors.fg_warning = app.colors.bg_statusbar;
        for phase in [
            IndexPhase::Loading,
            IndexPhase::Indexing,
            IndexPhase::Scanning,
            IndexPhase::Idle,
            IndexPhase::Limited,
            IndexPhase::Paused,
            IndexPhase::Disabled,
        ] {
            let status = IndexStatus {
                phase,
                indexed_files: 42,
                settings: IndexSettings {
                    enabled: phase != IndexPhase::Disabled,
                    paused: phase == IndexPhase::Paused,
                    ..Default::default()
                },
                ..Default::default()
            };
            let item = completion_status_item(&status, app.colors.fg_statusbar);
            assert_eq!(item.color, app.colors.fg_statusbar);
            let area = Rect::new(0, 0, item.width() as u16, 1);
            let mut terminal = Terminal::new(TestBackend::new(area.width, 1)).unwrap();
            for hovered in [false, true] {
                app.status_bar_items.clear();
                app.hovered_status_item = hovered.then_some(0);
                terminal
                    .draw(|frame| {
                        draw_item(frame, &mut app, area, &item.text, item.action, item.color)
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                for x in 1..area.width - 1 {
                    if !buffer[(x, 0)].symbol().trim().is_empty() {
                        let cell = &buffer[(x, 0)];
                        assert!(
                            crate::ui_colors::contrast_ratio(cell.fg, cell.bg).unwrap()
                                >= if hovered { 3.0 } else { 4.5 },
                            "{phase:?} hovered={hovered}: {:?} on {:?}",
                            cell.fg,
                            cell.bg
                        );
                    }
                }
            }
        }
    }
}

fn click_language_server_status(app: &mut App, pinned: bool) {
    app.set_status_bar_item_pinned("language_server", pinned);
    draw(app, 160, 30);
    let (x, y) = if pinned {
        let (x, _, _) = *app.status_bar_items.iter()
            .find(|item| item.2 == "language_server").unwrap();
        (x, 29)
    } else {
        open_details(app);
        let index = detail_rows(app).iter()
            .position(|row| row.id == "language_server").unwrap();
        app.status_details.selected = index;
        app.status_details.follow_selection = true;
        draw(app, 160, 30);
        let area = app.status_details.rows.iter().find(|(_, i)| *i == index).unwrap().0;
        (area.x + 1, area.y)
    };
    crate::mouse::handle_mouse(app, mouse(MouseEventKind::Down(MouseButton::Left), x, y));
}

#[test]
fn language_server_status_reuses_settings_in_other_split() {
    use crate::app::{ActivePane, SplitDirection};
    use crate::ui_lsp_settings::{Action, activate};

    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        for hidden in [false, true] {
            for pinned in [false, true] {
                let mut app = app_with_file();
                let editor = app.editor_mut().unwrap();
                editor.insert_char('x');
                editor.cursor_line = 1;
                editor.cursor_col = 8;
                editor.selection_anchor = Some((0, 3));
                let document_id = editor.navigation_id;
                let content = editor.rope.clone();
                app.open_settings_tab();
                let settings_id = app.tabs[app.active_tab].editor.navigation_id;
                app.create_split_group_dir(app.active_tab, direction);
                let settings_group = app.active_group;
                if hidden {
                    app.tabs.push(Tab::new_file(Editor::test_with_text("other pane\n")));
                    app.active_tab = 1;
                    app.tab_scroll_offset = 1;
                    app.tab_bar_area = Some(Rect::new(80, 0, 35, 1));
                }

                // Repeated clicks must focus the original tab, including when
                // it is behind another tab or scrolled out of the tab strip.
                for _ in 0..2 {
                    app.switch_group(0);
                    click_language_server_status(&mut app, pinned);
                    assert_eq!(app.active_group, settings_group);
                    assert_eq!(app.active_pane, ActivePane::Editor);
                    assert_eq!(app.tabs[app.active_tab].kind, TabKind::Settings);
                    assert_eq!(app.tabs[app.active_tab].editor.navigation_id, settings_id);
                    assert_eq!(app.tab_scroll_offset, 0);
                    assert_eq!(app.tab_mru_order.first(), Some(&app.active_tab));
                    assert_eq!(app.tabs.iter().chain(app.groups.iter().flat_map(|g| &g.tabs))
                        .filter(|tab| tab.kind == TabKind::Settings).count(), 1);
                    assert!(app.lsp_settings.active);
                    assert_eq!(app.settings_category_selected, 8);
                    assert_eq!(app.settings_items()[app.settings_selected].2, "lsp_servers");
                    assert_eq!(app.groups.len(), 2);
                    let editor = &app.groups[0].tabs[0].editor;
                    assert_eq!(editor.navigation_id, document_id);
                    assert_eq!(editor.rope, content);
                    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 8));
                    assert_eq!(editor.selection_anchor, Some((0, 3)));
                    assert!(editor.modified);
                    activate(&mut app, Action::Close);
                }
                app.switch_group(0);
                app.editor_mut().unwrap().undo();
                assert!(!app.editor().unwrap().rope.to_string().starts_with('x'));
                // The general Settings command shares the same tab lookup.
                app.execute_palette_command("settings");
                assert_eq!(app.active_group, settings_group);
                assert_eq!(app.tabs[app.active_tab].editor.navigation_id, settings_id);
            }
        }
    }
}

#[test]
fn default_status_is_quiet_and_secondary_metadata_remains_available() {
    let mut app = app_with_file();
    let terminal = draw(&mut app, 160, 24);
    let text = row(&terminal, 23);
    for expected in ["main", "Ln 1, Col 1", "Rust", "…"] {
        assert!(text.contains(expected), "{text}");
    }
    for removed in [
        "Spaces:",
        "UTF-8",
        "LF",
        "Word Wrap:",
        " B",
        "Top",
        "LSP:",
        "⊘ 0",
        "⚠ 0",
    ] {
        assert!(
            !text.contains(removed),
            "unnecessary default status: {text}"
        );
    }
    open_details(&mut app);
    let terminal = draw(&mut app, 160, 24);
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    for expected in [
        "File details",
        "main.rs",
        "Encoding",
        "UTF-8",
        "Indentation",
        "Line endings",
        "Language server",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
}

#[test]
fn non_document_views_do_not_invent_file_metadata() {
    let mut app = app_with_file();
    app.tabs.clear();
    let terminal = draw(&mut app, 80, 24);
    assert!(!row(&terminal, 23).contains("Ln "));
    assert!(
        app.status_bar_items
            .iter()
            .any(|item| item.2 == "file_details")
    );
    open_details(&mut app);
    assert!(app.status_details.open);
    assert!(file_detail_rows(&app).is_empty());
    dismiss(&mut app);

    let mut tab = Tab::new_file(Editor::new());
    tab.kind = TabKind::Terminal;
    app.tabs.push(tab);
    let terminal = draw(&mut app, 80, 24);
    assert!(!row(&terminal, 23).contains("Plain Text"));
    assert!(
        !app.status_bar_items
            .iter()
            .any(|item| item.2 == "goto_line")
    );
}

#[test]
fn status_resizes_without_overlaps_or_partial_hit_targets() {
    let mut app = app_with_file();
    app.cached_git_branch = "feature/界面/very-long-branch-name/👩‍💻".into();
    app.cached_error_count = 12;
    app.cached_warning_count = 3;
    for width in 1..=180 {
        let terminal = draw(&mut app, width, 2);
        let mut end = 0;
        for &(x, cells, action) in &app.status_bar_items {
            assert!(cells > 0, "zero-width {action} at {width}");
            assert!(
                x >= end && x + cells <= width,
                "overlapping {action} at {width}: {:?}",
                app.status_bar_items
            );
            end = x + cells;
        }
        assert_eq!(app.status_bar_items.last().unwrap().2, "performance");
        if width >= 40 {
            assert!(
                app.status_bar_items
                    .iter()
                    .any(|item| item.2 == "goto_line")
            );
            assert!(app.status_bar_items.iter().any(|item| item.2 == "problems"));
            assert!(row(&terminal, 1).contains('…'));
        }
    }
}

#[test]
fn customization_moves_items_between_bar_and_dropdown_with_mouse_and_keyboard() {
    let mut app = app_with_file();
    open_details(&mut app);
    draw(&mut app, 100, 30);
    let button = app.status_details.customize_button;
    crate::mouse::handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x + 1,
            button.y,
        ),
    );
    assert!(app.status_details.customize);

    draw(&mut app, 100, 30);
    let encoding_index = ITEMS.iter().position(|item| item.id == "encoding").unwrap();
    let (encoding, _) = app
        .status_details
        .rows
        .iter()
        .find(|(_, index)| *index == encoding_index)
        .copied()
        .unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            encoding.x + 2,
            encoding.y,
        ),
    );
    assert!(is_pinned(&app, "encoding"));
    assert!(app.status_details.open);
    assert_eq!(
        app.settings_global.status_bar_items.get("encoding"),
        Some(&true)
    );

    app.status_details.selected = ITEMS.iter().position(|item| item.id == "language").unwrap();
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    );
    assert!(!is_pinned(&app, "language"));
    let terminal = draw(&mut app, 100, 30);
    assert!(row(&terminal, 29).contains("UTF-8"));
    assert!(!row(&terminal, 29).contains("Rust"));
    assert!(
        app.status_bar_items
            .iter()
            .any(|item| item.2 == "goto_line")
    );

    let button = app.status_details.customize_button;
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x + 1,
            button.y,
        ),
    );
    draw(&mut app, 100, 30);
    assert!(!app.status_details.customize);
    assert!(detail_rows(&app).iter().any(|row| row.id == "language"));
    assert!(!detail_rows(&app).iter().any(|row| row.id == "encoding"));
    assert_eq!(app.editor().unwrap().rope.to_string(), before_text());
}

#[test]
fn right_click_selects_the_item_and_customization_works_without_a_document() {
    let mut app = app_with_file();
    draw(&mut app, 80, 24);
    let &(x, _, _) = app
        .status_bar_items
        .iter()
        .find(|item| item.2 == "language")
        .unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Right), x + 1, 23),
    );
    assert!(app.status_details.customize);
    assert_eq!(ITEMS[app.status_details.selected].id, "language");
    handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!is_pinned(&app, "language"));
    dismiss(&mut app);

    app.tabs.clear();
    app.execute_palette_command("customize_status_bar");
    assert!(app.status_details.customize);
    draw(&mut app, 80, 24);
    app.status_details.selected = ITEMS.iter().position(|item| item.id == "encoding").unwrap();
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    );
    assert!(is_pinned(&app, "encoding"));
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.status_details.open);
}

#[test]
fn overflow_keeps_pinned_items_reachable_and_all_items_can_live_in_the_dropdown() {
    let mut app = app_with_file();
    app.cached_word_count = Some("8W 45C 4L".into());
    app.cached_word_occ = Some("1 of 2".into());
    app.search_active = true;
    app.search_query = "main".into();
    app.search_matches = vec![(0, 3, 7)];
    for item in ITEMS {
        app.set_status_bar_item_pinned(item.id, true);
    }
    for width in 1..=160 {
        draw(&mut app, width, 24);
        let mut end = 0;
        for &(x, cells, action) in &app.status_bar_items {
            assert!(
                cells > 0 && x >= end && x + cells <= width,
                "{action} at {width}"
            );
            end = x + cells;
        }
        let details = detail_rows(&app);
        for item in all_detail_rows(&app) {
            let visible = app.status_bar_items.iter().any(|&(_, _, id)| id == item.id);
            assert_eq!(
                details.iter().any(|row| row.id == item.id),
                !visible,
                "{} at {width}",
                item.id
            );
        }
    }
    draw(&mut app, 30, 24);
    assert!(detail_rows(&app).iter().any(|row| row.id == "encoding"));
    for item in ITEMS {
        app.set_status_bar_item_pinned(item.id, false);
    }
    let terminal = draw(&mut app, 80, 24);
    assert_eq!(row(&terminal, 23).trim(), "…");
    assert_eq!(app.status_bar_items.len(), 1);
    open_details(&mut app);
    draw(&mut app, 80, 24);
    app.status_details.selected = detail_rows(&app)
        .iter()
        .position(|row| row.id == "performance")
        .unwrap();
    handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.background.activity.details_open);
    assert!(!app.status_details.open);
}

#[test]
fn word_count_placement_and_existing_controls_stay_in_sync() {
    let mut app = app_with_file();
    app.cached_word_count = Some("8W 45C 4L".into());
    app.set_status_bar_item_pinned("word_count", true);
    assert!(app.show_word_count);
    let terminal = draw(&mut app, 100, 24);
    assert!(row(&terminal, 23).contains("8W"));
    app.execute_palette_command("toggle_word_count");
    assert!(!is_pinned(&app, "word_count"));
    app.toggle_setting("word_count");
    assert!(is_pinned(&app, "word_count"));
    assert!(app.reset_setting_in_selected_scope("word_count"));
    assert!(!is_pinned(&app, "word_count"));
    assert!(
        !app.settings_global
            .status_bar_items
            .contains_key("word_count")
    );

    app.settings_workspace.show_word_count = Some(true);
    app.apply_effective_settings();
    assert!(is_pinned(&app, "word_count"));
    app.set_status_bar_item_pinned("word_count", false);
    assert!(!is_pinned(&app, "word_count"));
    assert_eq!(app.settings_workspace.show_word_count, Some(false));
    assert_eq!(
        app.settings_workspace.status_bar_items.get("word_count"),
        Some(&false)
    );
    assert!(
        !app.settings_global
            .status_bar_items
            .contains_key("word_count")
    );
}

#[test]
fn placements_persist_without_flattening_scopes_or_changing_document_modes() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = app_with_file();
    app.settings_global_path = directory.path().join("global/settings.json");
    app.settings_workspace_path = directory.path().join("workspace/settings.json");
    app.settings_persistence_enabled = true;
    app.settings_workspace.tab_size = Some(8);
    app.settings_global.theme = Some("dark".into());
    app.editor_mut().unwrap().word_wrap = true;
    app.editor_mut().unwrap().read_only = true;
    app.set_status_bar_item_pinned("encoding", true);
    assert!(app.editor().unwrap().word_wrap);
    assert!(app.editor().unwrap().read_only);
    assert!(!app.settings_workspace_path.exists());
    let global = crate::settings::load_from_file(&app.settings_global_path);
    assert_eq!(global.status_bar_items.get("encoding"), Some(&true));
    assert_eq!(global.theme.as_deref(), Some("dark"));
    assert_eq!(global.tab_size, None);

    let mut restored = app_with_file();
    restored.settings_global = global;
    restored.apply_effective_settings();
    let terminal = draw(&mut restored, 100, 24);
    assert!(row(&terminal, 23).contains("UTF-8"));

    app.settings_workspace
        .status_bar_items
        .insert("encoding".into(), false);
    app.settings_workspace
        .status_bar_items
        .insert("line_ending".into(), true);
    app.apply_effective_settings();
    app.set_status_bar_item_pinned("encoding", true);
    app.set_status_bar_item_pinned("language", false);
    let workspace = crate::settings::load_from_file(&app.settings_workspace_path);
    assert_eq!(workspace.status_bar_items.get("encoding"), Some(&true));
    assert_eq!(workspace.status_bar_items.get("line_ending"), Some(&true));
    assert_eq!(workspace.tab_size, Some(8));
    let global = crate::settings::load_from_file(&app.settings_global_path);
    assert_eq!(global.status_bar_items.get("language"), Some(&false));
    assert!(!global.status_bar_items.contains_key("line_ending"));
    assert!(is_pinned(&app, "line_ending"));
    assert!(!is_pinned(&app, "language"));
}

#[test]
fn exceptional_modes_are_visible_and_their_actions_work() {
    let mut app = app_with_file();
    app.macro_recording = true;
    app.editor_mut().unwrap().read_only = true;
    app.editor_mut().unwrap().overwrite_mode = true;
    let terminal = draw(&mut app, 80, 24);
    let text = row(&terminal, 23);
    for expected in ["REC", "Read-only", "OVR", "Ln 1, Col 1"] {
        assert!(text.contains(expected), "{text}");
    }
    activate(&mut app, "overwrite_mode");
    assert!(!app.editor().unwrap().overwrite_mode);
    activate(&mut app, "macro");
    assert!(!app.macro_recording);
    activate(&mut app, "read_only");
    assert!(!app.editor().unwrap().read_only);

    app.tabs[0].kind = TabKind::GitRevision;
    app.editor_mut().unwrap().read_only = true;
    activate(&mut app, "read_only");
    assert!(
        app.editor().unwrap().read_only,
        "historical versions must stay read-only"
    );
    assert!(
        file_detail_rows(&app)
            .iter()
            .find(|row| row.label == "Read-only")
            .unwrap()
            .action
            .is_none()
    );
}

#[test]
fn details_support_mouse_keyboard_and_escape_without_editing() {
    let mut app = app_with_file();
    draw(&mut app, 100, 24);
    let &(x, _, _) = app
        .status_bar_items
        .iter()
        .find(|item| item.2 == "file_details")
        .unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), x + 1, 23),
    );
    assert!(app.status_details.open);
    assert!(
        !app.has_text_input_focus(),
        "the editor caret should stop blinking while details own focus"
    );
    draw(&mut app, 100, 24);
    let before = app.editor().unwrap().rope.to_string();
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), before);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!app.status_details.open);

    app.execute_palette_command("file_details");
    assert!(
        app.status_details.open,
        "details are reachable from the command palette"
    );
    handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.status_details.selected, 1);
    handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.status_details.open);
    assert!(app.palette.active);
    assert_eq!(app.palette.query, "Encoding");

    app.close_palette();
    open_details(&mut app);
    draw(&mut app, 100, 24);
    let wrap_index = detail_rows(&app)
        .iter()
        .position(|row| row.id == "word_wrap")
        .unwrap();
    let (wrap, _) = app
        .status_details
        .rows
        .iter()
        .find(|(_, index)| *index == wrap_index)
        .copied()
        .unwrap();
    let before = app.editor().unwrap().word_wrap;
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), wrap.x + 2, wrap.y),
    );
    assert_eq!(app.editor().unwrap().word_wrap, !before);
    assert!(app.status_details.open);
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
    );
    assert!(!app.status_details.open);
    assert_eq!(app.editor().unwrap().rope.to_string(), before_text());
}

fn before_text() -> &'static str {
    "fn main() {\n    println!(\"Hello, world!\");\n}\n"
}

#[test]
fn small_details_popovers_scroll_and_keep_all_controls_in_bounds() {
    let mut app = app_with_file();
    for customize in [false, true] {
        for (width, height) in [(8, 4), (20, 8), (40, 12), (80, 24), (80, 40)] {
            open_details(&mut app);
            app.status_details.customize = customize;
            let terminal = draw(&mut app, width, height);
            if customize && width >= 40 {
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(
                    text.contains("Bottom bar"),
                    "placement labels should fit at {width} columns"
                );
                assert!(
                    text.contains("Dropdown"),
                    "placement labels should fit at {width} columns"
                );
            }
            let screen = Rect::new(0, 0, width, height);
            assert_eq!(
                app.status_details.area.intersection(screen),
                app.status_details.area
            );
            assert_eq!(
                app.status_details.close.intersection(screen),
                app.status_details.close
            );
            for (area, _) in &app.status_details.rows {
                assert_eq!(area.intersection(screen), *area);
                assert!(area.bottom() <= app.status_details.area.bottom());
            }
            if height >= 8 {
                let list_height = app.status_details.area.height.saturating_sub(4);
                let overflows = detail_rows(&app).len() > list_height as usize;
                assert_eq!(!app.status_details.scrollbar.is_empty(), overflows);
                let last_row = app.status_details.rows.last().unwrap().0;
                assert_eq!(last_row.bottom(), app.status_details.area.bottom() - 1);
            }
            handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
            draw(&mut app, width, height);
            assert_eq!(
                app.status_details.selected,
                detail_rows(&app).len().saturating_sub(1)
            );
            handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            assert_eq!(app.status_details.selected, 0);
        }
    }
    open_customization(&mut app, None);
    draw(&mut app, 40, 12);
    let scrollbar = app.status_details.scrollbar;
    let pins = app.status_bar_pins.clone();
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            scrollbar.x,
            scrollbar.bottom() - 1,
        ),
    );
    draw(&mut app, 40, 12);
    assert!(
        app.status_details
            .rows
            .iter()
            .any(|&(_, index)| index == ITEMS.len() - 1)
    );
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            scrollbar.x,
            scrollbar.y,
        ),
    );
    handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            scrollbar.x,
            scrollbar.y,
        ),
    );
    assert_eq!(app.status_details.scroll, 0);
    assert_eq!(
        app.status_bar_pins, pins,
        "scrollbar clicks must not toggle a pin"
    );
    open_details(&mut app);
    draw(&mut app, 40, 10);
    let area = app.status_details.area;
    for _ in 0..8 {
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::ScrollDown, area.x + 2, area.y + 2),
        );
    }
    let terminal = draw(&mut app, 40, 10);
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        text.contains("Language server"),
        "metadata must remain reachable by scrolling: {text}"
    );
}

#[test]
fn details_yield_to_other_overlays_and_workbench_shortcuts() {
    let mut app = app_with_file();
    open_details(&mut app);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    );
    assert!(!app.status_details.open);
    assert!(app.palette.active);
    app.close_palette();
    open_details(&mut app);
    app.goto_line_active = true;
    draw(&mut app, 80, 24);
    assert!(!app.status_details.open);
}

#[test]
fn hover_hints_are_delayed_and_do_not_survive_other_overlays() {
    let mut app = app_with_file();
    draw(&mut app, 80, 24);
    let &(x, _, _) = app
        .status_bar_items
        .iter()
        .find(|item| item.2 == "file_details")
        .unwrap();
    app.mouse_col = x + 1;
    app.mouse_row = 23;
    let terminal = draw(&mut app, 80, 24);
    assert!(!app.chrome_hover.shown);
    assert!(!row(&terminal, 21).contains("File details"));
    app.chrome_hover.started = Some(Instant::now() - HOVER_DELAY);
    assert!(hover_due(&app));
    draw(&mut app, 80, 24);
    assert!(app.chrome_hover.shown);
    assert!(
        !hover_due(&app),
        "a shown hint must not cause perpetual wakeups"
    );
    app.open_palette();
    draw(&mut app, 80, 24);
    assert!(app.chrome_hover.started.is_none());
}

#[test]
fn file_details_and_status_remain_readable_across_themes() {
    let mut app = app_with_file();
    let mut failures = Vec::new();
    for theme in crate::ui_control_visibility_tests::audit_themes() {
        app.colors = theme.to_theme_colors();
        for customize in [false, true] {
            open_details(&mut app);
            app.status_details.customize = customize;
            let terminal = draw(&mut app, 100, 24);
            for (area, label) in [
                (Rect::new(0, 23, 100, 1), "status"),
                (app.status_details.area, "file details"),
            ] {
                crate::ui_control_visibility_tests::check_region(
                    terminal.backend().buffer(),
                    area,
                    &format!("{}: {label}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn pinned_monitor_does_not_suppress_other_status_bar_tooltips() {
    let mut app = app_with_file();
    app.background.activity.details_open = true;
    app.performance_view.pinned = true;
    draw(&mut app, 80, 24);
    let &(x, _, _) = app.status_bar_items.iter().find(|item| item.2 == "file_details").unwrap();
    app.mouse_col = x + 1;
    app.mouse_row = 23;
    draw(&mut app, 80, 24);
    app.chrome_hover.started = Some(Instant::now() - HOVER_DELAY);
    draw(&mut app, 80, 24);
    assert!(app.chrome_hover.shown);
    assert!(app.background.activity.details_open);
    app.performance_view.pinned = false;
    draw(&mut app, 80, 24);
    assert!(!app.chrome_hover.shown);
}

#[test]
fn preview_polished_workbench_at_multiple_sizes() {
    let mut app = app_with_file();
    app.show_panel = false;
    app.show_minimap = false;
    app.show_explorer = false;
    app.chat.visible = false;
    for (width, height, details, customize) in [
        (120, 30, false, false),
        (80, 24, false, false),
        (80, 24, true, false),
        (40, 16, true, false),
        (120, 30, true, true),
        (80, 24, true, true),
        (40, 16, true, true),
    ] {
        app.status_details = DetailsState {
            open: details,
            customize,
            ..DetailsState::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(
            !terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "~"),
            "unused editor rows should remain quiet"
        );
        if let Some(directory) = std::env::var_os("VTCODE_UI_PREVIEW_DIR") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
                "text":cell.symbol(), "fg":format!("{:?}",cell.fg), "bg":format!("{:?}",cell.bg),
                "bold":cell.modifier.contains(Modifier::BOLD), "underline":cell.modifier.contains(Modifier::UNDERLINED),
            })).collect();
            std::fs::write(
                directory.join(if customize {
                    format!("customize-{width}.json")
                } else {
                    format!("workbench-{width}-{details}.json")
                }),
                serde_json::to_vec(
                    &serde_json::json!({"width":width,"height":height,"cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn compact_panel_headers_keep_the_active_tab_and_close_control_usable() {
    let mut app = app_with_file();
    app.show_panel = true;
    app.panel_tab = 3;
    app.terminal_panel_focused = false;
    for width in [12, 18, 28, 42, 80, 120] {
        let area = Rect::new(5, 2, width, 6);
        app.panel_area = Some(area);
        let mut terminal = Terminal::new(TestBackend::new(width + 10, 10)).unwrap();
        terminal
            .draw(|frame| crate::ui_panels::draw_bottom_panel(frame, &mut app, area))
            .unwrap();
        assert!(
            app.panel_tab_areas
                .iter()
                .any(|(_, index)| *index == app.panel_tab)
        );
        let close_x = area.right() - 2;
        assert_eq!(terminal.backend().buffer()[(close_x, area.y)].symbol(), "✕");
        for (tab, index) in app.panel_tab_areas.clone() {
            assert!(tab.right() <= area.right() - 3);
            crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Moved, tab.x, tab.y));
            assert_eq!(app.hovered_panel_tab, Some(index));
            crate::mouse::handle_mouse(
                &mut app,
                mouse(MouseEventKind::Down(MouseButton::Left), tab.x, tab.y),
            );
            assert_eq!(app.panel_tab, index);
        }
        crate::mouse::handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), close_x, area.y),
        );
        assert!(!app.show_panel);
        app.show_panel = true;
    }
}

#[test]
fn activity_navigation_keeps_settings_reachable_in_short_terminals() {
    for height in 1..=30 {
        let area = Rect::new(3, 2, 4, height);
        let items = crate::ui_chrome::activity_item_regions(area);
        assert_eq!(items.last().unwrap().1, 6);
        assert_eq!(items.last().unwrap().0.bottom(), area.bottom());
        let mut end = area.y;
        for (item, _) in &items {
            assert!(item.height > 0 && item.y >= end && item.bottom() <= area.bottom());
            end = item.bottom();
        }
        if height >= 6 {
            assert_eq!(items.len(), 6);
        }
    }
}
