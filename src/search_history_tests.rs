use super::*;
use crate::{app::App, text_input::TextInputId};
use crossterm::event::{KeyCode, KeyModifiers};

struct Workspace(tempfile::TempDir);

impl Workspace {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn app(&self) -> App {
        let mut app = App::new_deferred(self.0.path().to_owned());
        app.source_control_enabled = false;
        app
    }

    fn reopen(&self) -> App {
        let session = crate::session::load_session(self.0.path()).unwrap();
        let mut app = self.app();
        app.restore_session(&session);
        app
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let state = crate::portable::workspace_dir(self.0.path());
        assert!(state.starts_with(crate::portable::data_dir().join("workspaces")));
        let _ = std::fs::remove_dir_all(state);
    }
}

#[test]
fn search_history_restore_deduplicates_and_bounds_recent_queries() {
    let mut entries = (0..60)
        .map(|index| format!("query {index}"))
        .collect::<Vec<_>>();
    entries.extend(["query 30".into(), String::new(), "世界\nα.*β".into()]);
    let history = History::restore(&entries);
    let saved = history.saved("");
    assert_eq!(saved.len(), 50);
    assert_eq!(saved.first().unwrap(), "query 11");
    assert_eq!(&saved[48..], ["query 30", "世界\nα.*β"]);
    assert_eq!(saved.iter().filter(|query| *query == "query 30").count(), 1);
}

#[test]
fn search_history_browsing_restores_the_draft_and_does_not_promote_previews() {
    let mut history = History::restore(&["first".into(), "second".into()]);
    assert_eq!(history.previous("draft"), Some("second".into()));
    assert_eq!(history.previous("second"), Some("first".into()));
    assert_eq!(history.previous("first"), Some("first".into()));
    assert_eq!(history.saved("first"), ["first", "second", "draft"]);
    assert_eq!(history.next(), Some("second".into()));
    assert_eq!(history.next(), Some("draft".into()));
    assert_eq!(history.next(), None);
    assert_eq!(history.previous("edited draft"), Some("second".into()));
    history.edited();
    assert_eq!(history.next(), None);
    assert_eq!(history.previous("new term"), Some("second".into()));
    assert_eq!(history.next(), Some("new term".into()));
}

#[test]
fn both_search_histories_survive_restart_including_uncommitted_queries() {
    let workspace = Workspace::new();
    {
        let mut app = workspace.app();
        for query in ["file first", "file second"] {
            app.search_query = query.into();
            app.push_search_history();
        }
        app.search_query = "file draft".into();
        app.global_search_query = "workspace first".into();
        app.push_global_search_history();
        app.global_search_query = "workspace draft".into();
        app.save_session();
    }
    let mut app = workspace.reopen();
    app.search_query = "new file draft".into();
    crate::input::handle_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.search_query, "file draft");
    crate::input::handle_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.search_query, "file second");
    for _ in 0..2 {
        crate::input::handle_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(app.search_query, "new file draft");

    app.global_search_query = "new workspace draft".into();
    app.global_search_replace_query = "replacement".into();
    app.global_search_field_selection_anchors[0] = Some(0);
    let generation = app.search_generation;
    crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "workspace draft");
    assert_ne!(
        app.search_generation, generation,
        "recalling a query reruns the search"
    );
    assert_eq!(
        app.global_search_field_cursors[0],
        app.global_search_query.len()
    );
    assert_eq!(app.global_search_field_selection_anchors[0], None);
    crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "workspace first");
    for _ in 0..2 {
        crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(app.global_search_query, "new workspace draft");
    assert_eq!(app.global_search_replace_query, "replacement");
}

#[test]
fn replacing_a_search_field_keeps_the_previous_value_without_recording_prefixes() {
    let workspace = Workspace::new();
    for id in [TextInputId::SearchFind, TextInputId::GlobalSearch(0), TextInputId::GlobalSearch(1),
        TextInputId::GlobalSearch(2), TextInputId::GlobalSearch(3)] {
        let mut app = workspace.app();
        let (text, cursor, anchor) = if id == TextInputId::SearchFind {
            let state: &mut crate::file_search::State = &mut app;
            (&mut state.search_query, &mut state.search_cursor, &mut state.search_selection_anchor)
        } else {
            let TextInputId::GlobalSearch(field) = id else { unreachable!() };
            app.global_search_field_state_mut(field)
        };
        *text = "old term".into();
        *cursor = text.len();
        *anchor = Some(0);
        assert!(
            app.edit_text_input(id, KeyCode::Char('n'), KeyModifiers::NONE, None)
                .changed
        );
        assert!(
            app.edit_text_input(id, KeyCode::Char('e'), KeyModifiers::NONE, None)
                .changed
        );
        assert!(
            app.edit_text_input(id, KeyCode::Char('w'), KeyModifiers::NONE, None)
                .changed
        );
        let saved = app.build_session();
        let history = match id {
            TextInputId::SearchFind => saved.search_history,
            TextInputId::GlobalSearch(0) => saved.global_search_history,
            TextInputId::GlobalSearch(1) => saved.global_search_replace_history,
            TextInputId::GlobalSearch(2) => saved.search_include_history,
            TextInputId::GlobalSearch(3) => saved.search_exclude_history,
            _ => unreachable!(),
        };
        assert_eq!(history, ["old term", "new"]);
    }
}

#[test]
fn editing_a_recalled_workspace_query_resets_navigation_for_keyboard_and_paste() {
    let workspace = Workspace::new();
    let mut app = workspace.app();
    app.global_search_query = "α".into();
    app.push_global_search_history();
    app.global_search_query = "draft".into();
    crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    crate::input::handle_global_search_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "αx");
    crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert!(
        app.edit_text_input(
            TextInputId::GlobalSearch(0),
            KeyCode::Null,
            KeyModifiers::NONE,
            Some("β")
        )
        .changed
    );
    app.text_input_changed(TextInputId::GlobalSearch(0));
    crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "αβ");
    assert_eq!(app.global_search_field_cursors[0], "αβ".len());
}

#[test]
fn workspace_fields_browse_independent_histories_restore_drafts_and_persist() {
    let workspace = Workspace::new();
    let values = [
        ["first term", "second term"],
        ["first replacement", "second replacement"],
        ["./first/*/path", "./世界/*/path"],
        ["**/first", "**/second"],
    ];
    let set = |app: &mut App, field, value: &str| {
        let (text, cursor, anchor) = app.global_search_field_state_mut(field);
        *text = value.into();
        *cursor = text.len();
        *anchor = None;
    };
    let mut app = workspace.app();
    for index in 0..2 {
        for field in 0..4 { set(&mut app, field, values[field as usize][index]); }
        app.push_global_search_history();
    }
    for field in 0..4 { set(&mut app, field, &format!("saved draft {field}")); }
    app.save_session();
    let mut app = workspace.reopen();
    for field in 0..4 { set(&mut app, field, &format!("new draft {field}")); }
    for field in 0..4 {
        app.search_focus_field = field;
        app.global_search_field_selection_anchors[field as usize] = Some(0);
        for expected in [format!("saved draft {field}"), values[field as usize][1].into(), values[field as usize][0].into()] {
            let generation = app.search_generation;
            crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
            let (text, cursor, anchor) = app.global_search_field_state_mut(field);
            assert_eq!(*text, expected);
            assert_eq!(*cursor, expected.len());
            assert_eq!(*anchor, None);
            if field == 1 { assert_eq!(app.search_generation, generation); }
            else { assert_ne!(app.search_generation, generation); }
        }
        for _ in 0..3 {
            crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        }
        for other in 0..4 {
            assert_eq!(app.global_search_field_state_mut(other).0, &format!("new draft {other}"));
        }
    }
}

#[test]
fn editing_recalled_filter_and_replacement_values_resets_their_history_cursor() {
    let workspace = Workspace::new();
    for field in 1..4 {
        let mut app = workspace.app();
        app.search_focus_field = field;
        *app.global_search_field_state_mut(field).0 = "α".into();
        app.push_global_search_history();
        *app.global_search_field_state_mut(field).0 = "draft".into();
        crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        crate::input::handle_global_search_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.global_search_field_state_mut(field).0, "αx");
        crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert!(app.edit_text_input(TextInputId::GlobalSearch(field), KeyCode::Null,
            KeyModifiers::NONE, Some("β")).changed);
        app.text_input_changed(TextInputId::GlobalSearch(field));
        crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.global_search_field_state_mut(field).0, "αβ");
    }
}

#[test]
fn sessions_without_field_histories_keep_their_last_filter_and_replacement_values() {
    let workspace = Workspace::new();
    let mut app = workspace.app();
    let saved = crate::session::Session {
        global_search_replace_query: "replacement".into(),
        search_include_pattern: "./my/*/path".into(),
        search_exclude_pattern: "**/skip".into(),
        ..Default::default()
    };
    app.restore_search_histories(&saved);
    for (field, expected) in [(1, "replacement"), (2, "./my/*/path"), (3, "**/skip")] {
        app.search_focus_field = field;
        crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.global_search_field_state_mut(field).0, expected);
    }
}

#[test]
fn workspace_history_keys_leave_other_fields_and_result_navigation_available() {
    let workspace = Workspace::new();
    let mut app = workspace.app();
    app.global_search_query = "old term".into();
    app.push_global_search_history();
    app.global_search_query = "current term".into();
    app.global_search_results = (1..=3)
        .map(|line| crate::app::GlobalSearchResult {
            file_path: workspace.0.path().join("file.txt"),
            line_num: line,
            line_text: "current term".into(),
            col_start: 0,
            col_end: 7,
            context_before: None,
            context_after: None,
        })
        .collect::<Vec<_>>()
        .into();
    crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::ALT);
    assert_eq!(app.global_search_selected, 1);
    assert_eq!(app.global_search_query, "current term");
    app.search_focus_field = 1;
    app.global_search_replace_query = "replacement".into();
    crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.global_search_selected, 1, "plain arrows belong to field history");
    crate::input::handle_global_search_key(&mut app, KeyCode::Down, KeyModifiers::ALT);
    assert_eq!(app.global_search_selected, 2);
    assert_eq!(app.global_search_replace_query, "replacement");
    app.search_focus_field = 0;
    crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "old term");
}

#[test]
fn workspace_switches_isolate_both_search_histories_and_clear_browsing_state() {
    let first = Workspace::new();
    let second = Workspace::new();
    let mut app = first.app();
    app.search_query = "first file".into();
    app.global_search_query = "first workspace".into();
    app.change_workspace_ready(second.0.path(), None);
    assert_eq!(app.explorer.root, second.0.path());
    assert!(app.search_query.is_empty());
    assert!(app.build_session().search_history.is_empty());
    assert!(app.build_session().global_search_history.is_empty());
    app.search_query = "second file".into();
    app.global_search_query = "second workspace".into();
    app.change_workspace_ready(first.0.path(), None);
    app.search_history_prev();
    assert_eq!(app.search_query, "first file");
    assert_eq!(
        app.build_session().global_search_history,
        ["first workspace"]
    );
    assert_eq!(
        second.reopen().build_session().search_history,
        ["second file"]
    );
    assert_eq!(
        second.reopen().build_session().global_search_history,
        ["second workspace"]
    );
}

#[test]
fn launching_a_file_loads_search_history_without_restoring_other_tabs() {
    let workspace = Workspace::new();
    let old_path = workspace.0.path().join("previous.txt");
    let opened_path = workspace.0.path().join("opened.txt");
    std::fs::write(&old_path, "previous").unwrap();
    std::fs::write(&opened_path, "opened").unwrap();
    let mut previous = workspace.app();
    let mut editor = crate::editor::Editor::test_with_text("previous");
    editor.file_path = Some(old_path);
    previous.tabs.push(crate::app::Tab::new_file(editor));
    previous.search_query = "file term".into();
    previous.global_search_query = "workspace term".into();
    previous.save_session();
    let prepared = crate::startup::Prepared::load(Some(opened_path.as_os_str().to_owned()));
    let app = prepared.into_app(Default::default());
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(
        app.tabs[0].editor.file_path.as_deref(),
        Some(opened_path.as_path())
    );
    assert_eq!(app.build_session().search_history, ["file term"]);
    assert_eq!(
        app.build_session().global_search_history,
        ["workspace term"]
    );
}

#[test]
fn older_sessions_seed_workspace_history_from_their_saved_query() {
    let workspace = Workspace::new();
    let mut json = serde_json::to_value(crate::session::Session::default()).unwrap();
    json["globalSearchQuery"] = "legacy query".into();
    assert!(json.get("searchHistory").is_none());
    assert!(json.get("globalSearchHistory").is_none());
    let session = serde_json::from_value(json).unwrap();
    let mut app = workspace.app();
    app.restore_session(&session);
    app.global_search_query.clear();
    app.global_search_history_prev();
    assert_eq!(app.global_search_query, "legacy query");
    assert!(app.build_session().search_history.is_empty());
}
