use super::*;
use crate::{
    app::{ActivePane, Tab},
    background::UiWake,
    editor::Editor,
    startup::{Prepared, Target},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

fn empty() -> App {
    let mut app = App::new_startup(PathBuf::new(), UiWake::default());
    app.capture_session_baseline();
    app
}

fn screen(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn source_control_without_a_folder_is_idle_even_after_settings_changes() {
    use crate::app::SourceControlRepositoryState as State;
    let mut app = empty();
    for enabled in [true, false, false, true, true] {
        app.set_source_control_enabled(enabled);
        app.update_source_control_repository_state();
        app.refresh_source_control_async();
        app.refresh_git_log_async();
        assert_eq!(app.source_control_repository_state, State::NoFolder);
        assert_eq!(app.background.activity.job_count(), 0);
    }
    for command in [
        "git_refresh",
        "git_rebase",
        "git_checkout",
        "git_stage_all",
        "source_control_toggle",
        "open_parent",
        "goto_workspace_symbol",
    ] {
        assert!(app.is_menu_cmd_disabled(command), "{command}");
    }
    for command in [
        "git_refresh",
        "git_rebase",
        "git_checkout",
        "git_stage_all",
        "source_control_toggle",
    ] {
        assert!(app.execute_git_command(command));
        assert_eq!(app.background.activity.job_count(), 0);
    }
    assert!(!app.context_menu_active && !app.git_branch_picker_active);
    assert_eq!(app.source_control_repository_state, State::NoFolder);
    app.palette.recent = vec![
        "git_refresh".into(),
        "open_parent".into(),
        "source_control".into(),
    ];
    for query in ["", "Git", "Folder"] {
        app.palette.query = query.into();
        app.filter_palette_commands();
        assert!(app.palette.filtered.iter().all(|&index| {
            let id = app.palette.commands[index].id.as_ref();
            !id.starts_with("git_")
                && !matches!(
                    id,
                    "source_control_toggle" | "open_parent" | "open_workspace_settings_json"
                )
        }));
    }
}

#[test]
fn empty_folder_panels_offer_only_relevant_controls_and_a_working_open_button() {
    use crate::app::SidebarView;
    for view in [
        SidebarView::Explorer,
        SidebarView::SourceControl,
        SidebarView::Search,
    ] {
        for width in [60, 110] {
            for sidebar_right in [false, true] {
                for theme in [
                    crate::app_theme::EditorTheme::Dark,
                    crate::app_theme::EditorTheme::Light,
                ] {
                    let mut app = empty();
                    app.colors = theme.colors();
                    app.sidebar_view = view;
                    app.activity_bar_selected = match view {
                        SidebarView::Search => 1,
                        SidebarView::SourceControl => 2,
                        _ => 0,
                    };
                    app.active_pane = ActivePane::Explorer;
                    app.sidebar_right = sidebar_right;
                    app.global_search_query = "needle".into();
                    let mut terminal = Terminal::new(TestBackend::new(width, 28)).unwrap();
                    terminal
                        .draw(|frame| crate::ui::draw(frame, &mut app))
                        .unwrap();
                    let text = screen(&terminal);
                    assert!(
                        text.contains("No folder open"),
                        "{view:?} at {width}: {text}"
                    );
                    for misleading in [
                        "Detecting Git",
                        "Loading Git",
                        "Initialize Repository",
                        "No results",
                        "Actions ▾",
                        " ⤒ ",
                        " ↻ ",
                        "Open Editors",
                    ] {
                        assert!(!text.contains(misleading), "{view:?}: {misleading}");
                    }
                    assert!(app.source_control_initialize_area.is_none());
                    assert!(app.source_control_actions_area.is_none());
                    assert!(app.source_control_refresh_area.is_none());
                    assert!(app.source_control_toggle_area.is_none());
                    assert!(
                        app.text_input_area(crate::text_input::TextInputId::GlobalSearch(0))
                            .is_none()
                    );
                    assert!(app.search_control_regions.is_empty());
                    let activity = app.activity_bar_area.unwrap();
                    let source = crate::ui_chrome::activity_item_regions(activity)
                        .into_iter()
                        .find(|(_, index)| *index == 2)
                        .unwrap()
                        .0;
                    let buffer = terminal.backend().buffer();
                    assert!((source.y..source.bottom()).all(|y| {
                        (source.x..source.right()).all(|x| buffer[(x, y)].symbol() != "?")
                    }));
                    let button = crate::ui_chrome::empty_folder_button_area(
                        &app,
                        app.explorer_area.unwrap(),
                    )
                    .unwrap();
                    let label: String = (button.x..button.right())
                        .map(|x| buffer[(x, button.y + button.height / 2)].symbol())
                        .collect();
                    assert!(label.contains("Open folder") || label.contains("Open…"));
                    assert!(
                        button.height >= 3,
                        "the empty panel should use padded buttons"
                    );
                    crate::ui_control_visibility_tests::snapshot(
                        buffer,
                        &format!(
                            "No folder - {view:?} - {} - {width} - {sidebar_right}",
                            theme.syntax_theme_key()
                        ),
                    );
                    crate::mouse::handle_mouse(
                        &mut app,
                        crossterm::event::MouseEvent {
                            kind: crossterm::event::MouseEventKind::Down(
                                crossterm::event::MouseButton::Left,
                            ),
                            column: button.x + 1,
                            row: button.y,
                            modifiers: KeyModifiers::NONE,
                        },
                    );
                    assert!(app.change_dir_active && !app.folder_browser.add_workspace);
                    assert!(!app.has_open_folder());
                }
            }
        }
    }
}

#[test]
fn empty_panel_saved_workspaces_action_supports_mouse_and_keyboard_navigation() {
    use crate::app::SidebarView;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for view in [
        SidebarView::Explorer,
        SidebarView::SourceControl,
        SidebarView::Search,
    ] {
        for keyboard in [false, true] {
            let mut app = empty();
            app.workspaces.sets.loaded = true;
            app.sidebar_view = view;
            app.active_pane = ActivePane::Explorer;
            app.global_search_active = view == SidebarView::Search;
            let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let saved =
                crate::ui_empty_folder::saved_area(&app, app.explorer_area.unwrap()).unwrap();
            if keyboard {
                crate::input_keyboard::handle_keyboard_event(
                    &mut app,
                    KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                );
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                let label_cell = &terminal.backend().buffer()
                    [(saved.x + saved.width / 2, saved.y + saved.height / 2)];
                assert!(label_cell.modifier.contains(ratatui::style::Modifier::BOLD));
                crate::input_keyboard::handle_keyboard_event(
                    &mut app,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                );
            } else {
                crate::mouse::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: saved.x,
                        row: saved.bottom() - 1,
                        modifiers: KeyModifiers::NONE,
                    },
                );
            }
            assert!(app.workspaces.popup.open, "{view:?}");
            assert!(matches!(
                app.workspaces.popup.sets.mode,
                Some(crate::ui_workspace_sets::Mode::List)
            ));
            assert!(!app.change_dir_active && !app.has_open_folder());
        }
    }
}

#[test]
fn empty_panel_layout_keeps_actions_inside_narrow_and_short_sidebars() {
    use crate::app::SidebarView;
    use ratatui::layout::Rect;
    for view in [
        SidebarView::Explorer,
        SidebarView::SourceControl,
        SidebarView::Search,
    ] {
        for width in [6, 12, 18, 28, 45] {
            for height in [3, 6, 10, 15, 26] {
                let mut app = empty();
                app.sidebar_view = view;
                // Imported themes can outline buttons; a padded button must
                // still render a solid surface, including its blank rows.
                app.colors.ui.primary_button.border =
                    crate::theme_workbench::ColorLayer::opaque(ratatui::style::Color::White);
                app.colors.ui.secondary_button.border =
                    crate::theme_workbench::ColorLayer::opaque(ratatui::style::Color::Gray);
                let area = Rect::new(3, 2, width, height);
                app.explorer_area = Some(area);
                let mut terminal = Terminal::new(TestBackend::new(width + 6, height + 4)).unwrap();
                terminal
                    .draw(|frame| crate::ui_empty_folder::draw(frame, &mut app, area))
                    .unwrap();
                for button in [
                    crate::ui_chrome::empty_folder_button_area(&app, area),
                    crate::ui_empty_folder::saved_area(&app, area),
                ]
                .into_iter()
                .flatten()
                {
                    assert_eq!(button.intersection(area), button);
                    assert!(!button.is_empty());
                    for y in button.y..button.bottom() {
                        for x in button.x..button.right() {
                            assert!(
                                !terminal.backend().buffer()[(x, y)]
                                    .modifier
                                    .contains(ratatui::style::Modifier::UNDERLINED)
                            );
                        }
                    }
                }
                let buffer = terminal.backend().buffer();
                for y in 0..buffer.area.height {
                    for x in 0..buffer.area.width {
                        if !area.contains((x, y).into()) {
                            assert_eq!(buffer[(x, y)].symbol(), " ");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn empty_panel_hover_redraws_when_entering_and_leaving_buttons() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let mut app = empty();
    app.sidebar_view = crate::app::SidebarView::SourceControl;
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let open =
        crate::ui_chrome::empty_folder_button_area(&app, app.explorer_area.unwrap()).unwrap();
    for (col, row, changed) in [
        (open.x, open.y, true),
        (open.x + 1, open.y, false),
        (open.x, open.bottom() + 1, true),
    ] {
        app.mouse_hover_changed = false;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: col,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.mouse_hover_changed, changed);
    }
}

#[test]
fn empty_panels_have_no_hidden_keyboard_inputs_or_debounced_search() {
    use crate::app::SidebarView;
    for view in [
        SidebarView::Explorer,
        SidebarView::SourceControl,
        SidebarView::Search,
    ] {
        let mut app = empty();
        app.sidebar_view = view;
        app.active_pane = ActivePane::Explorer;
        app.global_search_active = view == SidebarView::Search;
        app.global_search_query = "needle".into();
        app.search_debounce_ms = 500;
        app.schedule_global_search();
        assert!(!app.bg_global_search_running);
        assert!(app.global_search_pending_since.is_none());
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.global_search_query, "needle");
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(app.change_dir_active, "{view:?}");
        assert!(app.global_search_service.get().is_none());
    }
}

#[test]
fn no_folder_settings_cannot_select_an_invisible_folder_scope() {
    let mut app = empty();
    app.open_settings_tab();
    for code in [KeyCode::Right, KeyCode::Left, KeyCode::Right] {
        crate::input_dialogs::handle_settings_editor_key(
            &mut app,
            KeyEvent::new(code, KeyModifiers::ALT),
        );
        assert_eq!(app.settings_scope, crate::settings::SettingsScope::Global);
    }
    assert_eq!(
        app.completion_dropdown_options("vtcomplete_sources")
            .unwrap(),
        &["Files you open"]
    );
    assert!(!app.set_completion_dropdown("vtcomplete_sources", "Entire workspace"));
    assert!(!app.completion_index.settings().project_wide);
}

#[test]
fn no_file_outline_explains_what_to_open() {
    let mut app = empty();
    app.sidebar_view = crate::app::SidebarView::Outline;
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let text = screen(&terminal);
    assert!(text.contains("Open a file to see symbols"));
    assert!(!text.contains("No symbols found"));
    crate::ui_control_visibility_tests::snapshot(
        terminal.backend().buffer(),
        "No folder - outline",
    );
}

#[test]
fn extensions_load_only_when_requested_and_are_not_reported_missing_while_loading() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = empty();
    app.extension_manager.search_roots = vec![directory.path().join("extensions")];
    app.extension_manager.settings_file = directory.path().join("extension-settings.json");
    assert!(!app.bg_extension_scan_running);
    assert_eq!(app.extension_scan_generation, 0);
    app.sidebar_view = crate::app::SidebarView::Extensions;
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.bg_extension_scan_running);
    assert!(screen(&terminal).contains("Loading extensions…"));
    assert!(!app.bg_syntax_asset_refresh_running);
    let generation = app.extension_scan_generation;
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert_eq!(app.extension_scan_generation, generation);
    settle(&mut app, |app| !app.bg_extension_scan_running);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(!screen(&terminal).contains("Loading extensions…"));
    assert_eq!(app.extension_scan_generation, generation);
    app.start_empty_window_document_services();
    assert_eq!(app.extension_scan_generation, generation);
    assert!(!app.has_open_folder());
}

fn settle(app: &mut App, ready: impl Fn(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(12);
    loop {
        app.poll_background_results();
        app.poll_workspace_requests();
        if ready(app) {
            return;
        }
        assert!(
            Instant::now() < end,
            "empty-window operation did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn no_path_is_not_the_current_directory_and_dot_is_explicit() {
    for raw in [None, Some(std::ffi::OsString::new())] {
        let target = Target::resolve(raw);
        assert!(target.root.as_os_str().is_empty());
        assert!(target.file.is_none());
    }
    let target = Target::resolve(Some(".".into()));
    assert_eq!(
        target.root,
        crate::folder_browser::normalized_path(Path::new("."))
    );
    assert!(target.file.is_none());
    assert_ne!(
        crate::portable::workspace_dir(Path::new("")),
        crate::portable::workspace_dir(Path::new("."))
    );
}

#[test]
fn no_folder_schedules_no_project_services_even_when_git_and_indexing_are_enabled() {
    let mut app = empty();
    app.settings_persistence_enabled = true;
    assert!(app.source_control_enabled);
    app.completion_index
        .configure(crate::completion_index::IndexSettings {
            enabled: true,
            project_wide: true,
            ..Default::default()
        });
    // Theme discovery is global and independent of the folder-service probe.
    app.workspace_observation.assets_started = true;
    app.start_workspace_services();
    app.refresh_explorer_async();
    app.refresh_source_control_async();
    app.refresh_git_branch_async();
    app.start_workspace_watcher();
    for _ in 0..20 {
        assert!(!app.poll_completion_index());
    }
    assert_eq!(app.background.activity.job_count(), 0);
    assert!(!app.bg_explorer_refresh_running);
    assert!(!app.bg_git_status_running);
    assert!(!app.bg_git_branch_running);
    assert!(!app.bg_extension_scan_running);
    assert!(!app.bg_syntax_asset_refresh_running);
    assert!(!app.workspace_watcher_active);
    assert!(app.explorer.entries.is_empty());
    assert_eq!(app.open_folder_count(), 0);
    assert!(app.lsp_manager.clients.is_empty());
    assert!(
        app.terminals
            .iter()
            .all(|terminal| terminal.terminal_pty.is_none())
    );
    assert!(
        app.current_extension_editor_context()
            .workspace_root
            .is_none()
    );
}

#[test]
fn empty_editor_offers_open_folder_without_close_or_save_workspace() {
    let mut app = empty();
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    crate::ui_control_visibility_tests::snapshot(
        terminal.backend().buffer(),
        "Empty window - no folder",
    );
    assert!(screen.contains("No folder"));
    assert!(screen.contains("Open folder"));
    assert!(
        !app.menu_entries(0)
            .iter()
            .any(|entry| matches!(entry.command, "workspace_close" | "workspace_set_save"))
    );
    let area =
        crate::ui_chrome::empty_folder_button_area(&app, app.explorer_area.unwrap()).unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    );
    assert!(app.change_dir_active && !app.folder_browser.add_workspace);
    assert!(!app.has_open_folder());
}

#[test]
fn opening_the_first_folder_starts_services_without_parking_a_phantom_folder() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("project.txt"), "project").unwrap();
    let mut app = empty();
    app.workspaces.request = Some(Request::Open(root.path().to_owned()));
    settle(&mut app, |app| app.has_open_folder());
    assert_eq!(app.open_folder_count(), 1);
    assert!(app.workspaces.parked.is_empty());
    settle(&mut app, |app| {
        app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "project.txt")
    });
    assert!(app.workspace_observation.started);
}

#[test]
fn opening_a_folder_keeps_unsaved_loose_editors_live_and_out_of_saved_workspaces() {
    let root = tempfile::tempdir().unwrap();
    let mut app = empty();
    app.workspaces.sets.path = root.path().join("workspaces.json");
    app.workspaces.sets.loaded = true;
    let mut editor = Editor::test_with_text("keep these loose edits");
    editor.modified = true;
    app.tabs.push(Tab::new_file(editor));
    app.active_pane = ActivePane::Editor;
    let loose_id = app.workspaces.active;
    app.workspaces.request = Some(Request::Open(root.path().to_owned()));
    settle(&mut app, |app| app.has_open_folder());
    assert_eq!(app.open_folder_count(), 1);
    assert_eq!(app.workspaces.parked.len(), 1);
    assert_ne!(
        app.source_control_repository_state,
        crate::app::SourceControlRepositoryState::NoFolder
    );
    app.save_workspace_set("Project".into(), None);
    settle(&mut app, |app| !app.workspace_sets_busy());
    assert_eq!(app.saved_workspace_sets()[0].roots.len(), 1);
    assert!(app.switch_workspace(loose_id));
    assert!(!app.has_open_folder());
    assert_eq!(
        app.source_control_repository_state,
        crate::app::SourceControlRepositoryState::NoFolder
    );
    assert!(app.editor().unwrap().modified);
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "keep these loose edits"
    );
    assert!(!app.workspace_watcher_active);
}

#[test]
fn opening_a_saved_workspace_from_empty_does_not_ask_to_close_the_launch_directory() {
    use crate::workspace_sets::{Catalog, Location, SavedSet, Scope};
    let root = tempfile::tempdir().unwrap();
    let mut app = empty();
    app.workspaces.sets.loaded = true;
    app.workspaces.sets.catalog = Catalog::default();
    app.workspaces.sets.catalog.sets.push(SavedSet {
        id: 1,
        name: "Saved project".into(),
        scope: Scope::Local,
        roots: vec![Location::local(root.path(), &crate::portable::base_dir())],
        active: 0,
    });
    app.workspaces.request = Some(Request::OpenSet(1));
    settle(&mut app, |app| {
        app.has_open_folder() && !app.workspace_set_switching()
    });
    assert_eq!(app.open_folder_count(), 1);
    assert!(app.workspaces.parked.is_empty());
    assert!(app.workspace_set_confirmation().is_none());
}

#[test]
fn opening_a_terminal_without_a_folder_uses_a_real_cwd_without_attaching_it() {
    let mut app = empty();
    app.new_terminal_tab();
    app.ensure_terminal_pty_at(app.active_terminal, 40, 8);
    assert!(
        app.terminals[app.active_terminal].terminal_pty.is_some(),
        "{:?}",
        app.terminals[app.active_terminal].terminal_launch_error
    );
    assert!(app.terminals[app.active_terminal].terminal_cwd.is_dir());
    assert!(!app.has_open_folder());
    assert_eq!(app.open_folder_count(), 0);
}

#[test]
fn loose_file_opening_and_completion_do_not_attach_or_scan_a_folder() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("loose.txt");
    std::fs::write(&path, "windowOnlyIdentifier").unwrap();
    let mut app = empty();
    app.settings_persistence_enabled = true;
    app.quick_open_query = path.to_string_lossy().into_owned();
    app.update_quick_open();
    settle(&mut app, |app| !app.bg_quick_open_running);
    assert!(app.quick_open_results.contains(&path));
    app.open_file(&path);
    settle(&mut app, |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    app.poll_completion_index();
    assert!(app.completion_index.buffer_only());
    assert!(!app.completion_index.settings().project_wide);
    assert!(!app.has_open_folder());
    assert!(!app.workspace_watcher_active);
    assert_eq!(app.open_folder_count(), 0);
    app.global_search_query = "needle".into();
    app.update_global_search();
    assert!(!app.bg_global_search_running);
    assert!(app.global_search_service.get().is_none());
}

#[test]
fn empty_saved_workspace_list_keeps_both_tabs_and_omits_an_invalid_save_action() {
    let mut app = empty();
    app.workspaces.sets.loaded = true;
    crate::ui_workspace_sets::open(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(!app.workspaces.popup.tabs[0].is_empty());
    assert!(!app.workspaces.popup.tabs[1].is_empty());
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("No saved workspaces yet"));
    crate::ui_control_visibility_tests::snapshot(
        terminal.backend().buffer(),
        "Empty window - saved workspaces",
    );
    assert!(!screen.contains("Save workspace…"));
}

#[test]
fn empty_launch_child() {
    let Some(root) = std::env::var_os("VTCODE_EMPTY_LAUNCH_PROBE") else {
        return;
    };
    let root = PathBuf::from(root);
    let prepared = Prepared::load(None);
    assert!(prepared.target.root.as_os_str().is_empty());
    let mut defaults = crate::settings::Settings::default();
    defaults.validate();
    assert_eq!(prepared.workspace, defaults);
    let probe = root.join("cwd-probe.txt").to_string_lossy().into_owned();
    assert!(prepared.session.as_ref().is_none_or(|session| {
        session
            .tabs
            .iter()
            .all(|tab| tab.file_path.as_deref() != Some(probe.as_str()))
    }));
    let app = prepared.into_app(UiWake::default());
    assert!(!app.has_open_folder());
    assert!(!app.workspace_watcher_active);
}

#[test]
fn no_argument_launch_from_a_repository_does_not_restore_that_repository() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    std::fs::write(root.path().join("cwd-probe.txt"), "must not be restored").unwrap();
    let mut source = App::new_startup(root.path().to_owned(), UiWake::default());
    let mut editor = Editor::test_with_text("must not be restored");
    editor.file_path = Some(root.path().join("cwd-probe.txt"));
    source.tabs.push(Tab::new_file(editor));
    crate::session::save_session(root.path(), &source.build_session()).unwrap();
    crate::settings::save_to_file(
        &crate::settings::project_settings_path(root.path()),
        &crate::settings::Settings {
            tab_size: Some(17),
            ..Default::default()
        },
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app_workspaces::empty_tests::empty_launch_child",
            "--nocapture",
        ])
        .current_dir(root.path())
        .env("VTCODE_EMPTY_LAUNCH_PROBE", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
