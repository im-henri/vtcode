use super::*;
use crate::{
    app::{ActivePane, Tab},
    background::{BackgroundResult, CancellationToken},
    editor::Editor,
    settings::{Settings, SettingsScope},
    text_input::TextInputId,
    ui_search_controls::{SearchControl, SearchScope},
};
use crossterm::event::{KeyCode, KeyModifiers};

fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("sample.txt"), "needle\nother\n").unwrap();
    let mut app = App::new_deferred(root.path().to_owned());
    app.source_control_enabled = false;
    app.global_search_query = "needle".into();
    (root, app)
}

fn finish(app: &mut App) {
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert!(
        app.global_search_error.is_none(),
        "{:?}",
        app.global_search_error
    );
    assert_eq!(app.global_search_results.len(), 1);
    assert_eq!(&*app.global_search_results[0].line_text, "needle");
    assert!(app.global_search_pending_since.is_none());
}

#[test]
fn workspace_search_debounce_waits_for_the_latest_edit_without_occupying_a_worker() {
    let (_root, mut app) = fixture();
    let now = Instant::now();
    assert_eq!(app.search_debounce_ms, 300);
    app.global_search_query = "other".into();
    app.schedule_global_search_at(now);
    assert!(app.bg_global_search_running);
    assert_eq!(
        app.global_search_debounce_wait(now),
        Some(Duration::from_millis(300))
    );
    assert!(!app.poll_pending_global_search(now + Duration::from_millis(299)));
    assert!(app.search_cancel_token.is_none());
    assert!(app.global_search_service.get().is_none());
    let generation = app.search_generation;
    app.global_search_query = "needle".into();
    app.schedule_global_search_at(now + Duration::from_millis(200));
    assert_ne!(app.search_generation, generation);
    assert!(!app.poll_pending_global_search(now + Duration::from_millis(300)));
    assert!(!app.poll_pending_global_search(now + Duration::from_millis(499)));
    let latest = app.search_generation;
    assert!(app.poll_pending_global_search(now + Duration::from_millis(500)));
    assert_eq!(app.search_generation, latest);
    assert!(!app.poll_pending_global_search(now + Duration::from_secs(1)));
    finish(&mut app);
}

#[test]
fn workspace_search_debounce_cancels_stale_replies_and_stop_and_clear_cancel_the_timer() {
    let (root, mut app) = fixture();
    let old = app.search_generation;
    let cancel = CancellationToken::new();
    app.search_cancel_token = Some(cancel.clone());
    let now = Instant::now();
    app.schedule_global_search_at(now);
    assert!(cancel.is_cancelled());
    app.background
        .sender
        .send(BackgroundResult::GlobalSearchFailed {
            generation: old,
            error: "old failure".into(),
        })
        .unwrap();
    app.background
        .sender
        .send(BackgroundResult::GlobalSearchResults {
            generation: old,
            done: true,
            truncated: true,
            results: vec![GlobalSearchResult {
                file_path: root.path().join("sample.txt"),
                line_num: 0,
                line_text: "stale".into(),
                col_start: 0,
                col_end: 5,
                context_before: None,
                context_after: None,
            }],
        })
        .unwrap();
    app.poll_background_results();
    assert!(app.bg_global_search_running);
    assert!(app.global_search_results.is_empty());
    assert!(app.global_search_error.is_none());
    assert!(!app.global_search_truncated);
    app.stop_global_search();
    assert!(!app.bg_global_search_running);
    assert!(app.global_search_stopped);
    assert_eq!(app.global_search_debounce_wait(now), None);
    assert!(!app.poll_pending_global_search(now + Duration::from_secs(1)));
    app.schedule_global_search_at(now);
    app.clear_global_search();
    assert!(!app.bg_global_search_running);
    assert!(!app.poll_pending_global_search(now + Duration::from_secs(1)));
    assert!(app.global_search_service.get().is_none());
    app.global_search_query = "needle".into();
    app.schedule_global_search_at(now);
    let next = tempfile::tempdir().unwrap();
    app.change_workspace_ready(next.path(), None);
    assert_eq!(app.explorer.root, next.path());
    assert_eq!(app.global_search_debounce_wait(now), None);
    assert!(!app.poll_pending_global_search(now + Duration::from_secs(1)));
}

#[test]
fn workspace_search_debounce_covers_keyboard_paste_and_filter_history_but_not_replacement_text() {
    for field in [0, 2, 3] {
        let (_root, mut app) = fixture();
        app.search_focus_field = field;
        crate::input::handle_global_search_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(
            app.global_search_pending_since.is_some(),
            "keyboard field {field}"
        );
        assert!(app.search_cancel_token.is_none());
        let id = TextInputId::GlobalSearch(field);
        assert!(
            app.edit_text_input(id, KeyCode::Null, KeyModifiers::NONE, Some("pasted"))
                .changed
        );
        app.text_input_changed(id);
        assert!(
            app.global_search_pending_since.is_some(),
            "paste field {field}"
        );
        assert!(app.search_cancel_token.is_none());
        app.global_search_field_history_mut(field)
            .record("historical");
        crate::input::handle_global_search_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.global_search_field_state_mut(field).0, "historical");
        assert!(app.global_search_pending_since.is_some());
        let pending = app.global_search_pending_since;
        let generation = app.search_generation;
        app.search_focus_field = 1;
        crate::input::handle_global_search_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        let id = TextInputId::GlobalSearch(1);
        assert!(
            app.edit_text_input(id, KeyCode::Null, KeyModifiers::NONE, Some("replacement"))
                .changed
        );
        app.text_input_changed(id);
        assert_eq!(app.global_search_pending_since, pending);
        assert_eq!(app.search_generation, generation);
        app.stop_global_search();
    }
}

#[test]
fn workspace_search_debounce_zero_enter_and_refresh_start_immediately() {
    for mode in ["zero", "enter", "refresh"] {
        let (_root, mut app) = fixture();
        app.search_debounce_ms = if mode == "zero" { 0 } else { 60_000 };
        app.schedule_global_search();
        match mode {
            "enter" => {
                crate::input::handle_global_search_key(&mut app, KeyCode::Enter, KeyModifiers::NONE)
            }
            "refresh" => crate::ui_search_controls::activate(
                &mut app,
                SearchScope::Workspace,
                SearchControl::Refresh,
            ),
            _ => {}
        }
        assert!(app.search_cancel_token.is_some(), "{mode}");
        assert!(app.global_search_pending_since.is_none(), "{mode}");
        finish(&mut app);
    }
}

#[test]
fn workspace_search_debounce_does_not_delay_file_find() {
    let (_root, mut app) = fixture();
    app.search_debounce_ms = 60_000;
    app.schedule_global_search();
    let pending = app.global_search_pending_since;
    let generation = app.search_generation;
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("needle needle\n")));
    app.active_pane = ActivePane::Editor;
    app.search_active = true;
    app.search_query.clear();
    for ch in "needle".chars() {
        crate::input::handle_search_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    assert_eq!(app.search_matches, [(0, 0, 6), (0, 7, 13)]);
    assert!(!app.bg_file_search_running && !app.file_search_refresh_pending);
    assert_eq!(app.global_search_pending_since, pending);
    assert_eq!(app.search_generation, generation);
    app.stop_global_search();
}

#[test]
fn workspace_search_debounce_setting_merges_persists_and_resets_by_scope() {
    let (root, mut app) = fixture();
    app.settings_global_path = root.path().join("user.json");
    app.settings_workspace_path = root.path().join("workspace.json");
    app.settings_persistence_enabled = true;
    assert_eq!(app.settings_defaults.search_debounce_ms, Some(300));
    assert_eq!(App::setting_field_kind("search_debounce_ms"), "number");
    assert_eq!(App::settings_category_for_id("search_debounce_ms"), 3);
    let missing: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(missing.search_debounce_ms, None);
    for (scope, value) in [(SettingsScope::Global, 750), (SettingsScope::Workspace, 0)] {
        app.settings_scope = scope;
        app.begin_numeric_setting_edit("search_debounce_ms", "300");
        app.settings_value_buffer = value.to_string();
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.search_debounce_ms, value);
        let path = if scope == SettingsScope::Global {
            &app.settings_global_path
        } else {
            &app.settings_workspace_path
        };
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(document["search.searchOnTypeDebouncePeriod"], value);
        assert_eq!(
            crate::settings::load_from_file(path).search_debounce_ms,
            Some(value)
        );
    }
    app.settings_scope = SettingsScope::Global;
    let item = app
        .settings_items()
        .into_iter()
        .find(|(_, _, id)| id == "search_debounce_ms")
        .unwrap();
    assert_eq!(item.1, "750");
    assert_eq!(
        app.search_debounce_ms, 0,
        "viewing User scope does not change the Workspace value"
    );
    app.settings_scope = SettingsScope::Workspace;
    assert!(app.reset_setting_in_selected_scope("search_debounce_ms"));
    assert_eq!(app.search_debounce_ms, 750);
    app.settings_scope = SettingsScope::Global;
    assert!(app.reset_setting_in_selected_scope("search_debounce_ms"));
    assert_eq!(app.search_debounce_ms, 300);
}

#[test]
fn workspace_search_debounce_recalculates_a_pending_wait_when_the_setting_changes() {
    let (_root, mut app) = fixture();
    let now = Instant::now();
    app.search_debounce_ms = 1000;
    app.schedule_global_search_at(now);
    app.apply_settings(&Settings {
        search_debounce_ms: Some(50),
        ..Default::default()
    });
    assert_eq!(
        app.global_search_debounce_wait(now),
        Some(Duration::from_millis(50))
    );
    assert!(!app.poll_pending_global_search(now + Duration::from_millis(49)));
    assert!(app.poll_pending_global_search(now + Duration::from_millis(50)));
    finish(&mut app);
}

#[test]
fn workspace_search_debounce_coalesces_remote_queries_before_any_rpc() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (root, _) = fixture();
    let traffic = Arc::new(AtomicUsize::new(0));
    let remote = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
        root.path(),
        Arc::default(),
        traffic.clone(),
    ));
    let mut app = App::new_deferred(remote.view_root.clone());
    app.source_control_enabled = false;
    app.remote_workspace = Some(remote.clone());
    let before = traffic.load(Ordering::Relaxed);
    let now = Instant::now();
    for (i, text) in ["n", "ne", "nee", "need", "needl", "needle"]
        .iter()
        .enumerate()
    {
        app.global_search_query = (*text).into();
        app.schedule_global_search_at(now + Duration::from_millis(i as u64 * 25));
    }
    assert_eq!(traffic.load(Ordering::Relaxed), before);
    assert!(!app.poll_pending_global_search(now + Duration::from_millis(424)));
    assert!(app.poll_pending_global_search(now + Duration::from_millis(425)));
    finish(&mut app);
    assert!(traffic.load(Ordering::Relaxed) > before);
    remote
        .client
        .call(crate::remote::Command::Shutdown)
        .unwrap();
}
