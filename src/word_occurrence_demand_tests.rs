use super::*;
use crate::{app::Tab, background::CancellationToken, editor::Editor};

fn fixture() -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        "needle other needle\n",
    ))];
    app.active_tab = 0;
    app.last_user_activity = std::time::Instant::now() - WORD_OCCURRENCE_QUIET_PERIOD;
    app
}

#[test]
fn hidden_word_occurrences_do_not_lookup_words_or_schedule_scans_after_cursor_pauses() {
    let mut app = fixture();
    // Even a document-sized word must not be extracted for a hidden count.
    app.tabs[0].editor.rope = ropey::Rope::from_str(&"identifier".repeat(8192));
    assert!(app.word_occurrence_target().is_none());
    assert!(app.editor_cursor_word_cache.borrow().is_empty());

    for column in [0, 64, 1024, 512, 0] {
        app.tabs[0].editor.cursor_col = column;
        refresh_caches(&mut app);
        assert!(app.pending_word_occurrence.is_none());
        assert!(!app.bg_word_occ_running);
        assert!(!app.word_occurrence_deferred);
        assert!(!deferred_word_occurrence_is_due(&app));
        assert!(app.cached_word_occ_target.is_none());
    }
}

#[test]
fn word_occurrences_require_a_visible_status_item_or_count_details() {
    let mut app = fixture();
    assert!(app.word_occurrence_target().is_none());
    app.status_bar_pins.insert("word_occ".into(), true);
    assert!(app.word_occurrence_target().is_some());
    app.show_status_bar = false;
    assert!(app.word_occurrence_target().is_none());
    app.show_status_bar = true;
    app.zen_mode = true;
    assert!(app.word_occurrence_target().is_none());
    app.status_bar_pins.insert("word_occ".into(), false);

    crate::ui_status::open_details(&mut app);
    assert!(app.word_occurrence_target().is_some());
    app.status_details.customize = true;
    assert!(app.word_occurrence_target().is_none());
    app.status_details.customize = false;
    app.settings_active = true;
    assert!(app.word_occurrence_target().is_none());
    app.settings_active = false;
    assert!(app.word_occurrence_target().is_some());
    app.status_details.open = false;
    app.zen_mode = false;
    assert!(app.word_occurrence_target().is_none());
}

#[test]
fn hiding_word_occurrences_cancels_in_flight_work_and_discards_cached_counts() {
    for hide in ["unpin", "bar", "zen", "details", "customize", "obscured"] {
        let mut app = fixture();
        if matches!(hide, "details" | "customize" | "obscured") {
            crate::ui_status::open_details(&mut app);
        } else {
            app.status_bar_pins.insert("word_occ".into(), true);
        }
        let target = app.word_occurrence_target().unwrap();
        let token = CancellationToken::new();
        app.pending_word_occurrence = Some((target.clone(), token.clone()));
        app.bg_word_occ_running = true;
        app.cached_word_occ = Some("2 occurrences".into());
        app.cached_word_occ_target = Some(target);
        match hide {
            "unpin" => {
                app.status_bar_pins.insert("word_occ".into(), false);
            }
            "bar" => app.show_status_bar = false,
            "zen" => app.zen_mode = true,
            "details" => app.status_details.open = false,
            "customize" => app.status_details.customize = true,
            "obscured" => app.settings_active = true,
            _ => unreachable!(),
        }
        refresh_caches(&mut app);
        assert!(token.is_cancelled(), "{hide}");
        assert!(app.pending_word_occurrence.is_none(), "{hide}");
        assert!(!app.bg_word_occ_running, "{hide}");
        assert!(app.cached_word_occ.is_none(), "{hide}");
        assert!(app.cached_word_occ_target.is_none(), "{hide}");
    }
}

#[test]
fn hiding_a_deferred_word_count_removes_its_idle_wakeup() {
    let mut app = fixture();
    app.status_bar_pins.insert("word_occ".into(), true);
    app.last_user_activity = std::time::Instant::now();
    refresh_caches(&mut app);
    assert!(app.word_occurrence_deferred);
    assert!(app.pending_word_occurrence.is_none());

    app.status_bar_pins.insert("word_occ".into(), false);
    refresh_caches(&mut app);
    app.last_user_activity -= WORD_OCCURRENCE_QUIET_PERIOD;
    assert!(!app.word_occurrence_deferred);
    assert!(!deferred_word_occurrence_is_due(&app));
    assert!(app.pending_word_occurrence.is_none());
}

#[test]
fn a_hidden_word_occurrence_reply_does_not_repopulate_caches_or_request_a_frame() {
    let mut app = fixture();
    app.status_bar_pins.insert("word_occ".into(), true);
    let target = app.word_occurrence_target().unwrap();
    app.bg_word_occ_running = true;
    app.status_bar_pins.insert("word_occ".into(), false);
    app.background
        .sender
        .send(crate::background::BackgroundResult::WordOccurrences { target, count: 2 })
        .unwrap();

    assert!(!app.poll_background_results());
    assert!(!app.bg_word_occ_running);
    assert!(app.cached_word_occ.is_none());
    assert!(app.cached_word_occ_target.is_none());
}

#[test]
fn opening_word_occurrence_details_fetches_current_counts_and_reuses_them() {
    let mut app = fixture();
    refresh_caches(&mut app);
    assert!(app.pending_word_occurrence.is_none());
    crate::ui_status::open_details(&mut app);
    refresh_caches(&mut app);
    assert!(app.pending_word_occurrence.is_some());
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_word_occ_running);
    assert_eq!(app.cached_word_occ.as_deref(), Some("2 occurrences"));
    let target = app.cached_word_occ_target.clone();
    refresh_caches(&mut app);
    assert!(app.pending_word_occurrence.is_none());
    assert_eq!(app.cached_word_occ_target, target);

    app.status_details.open = false;
    refresh_caches(&mut app);
    app.tabs[0].editor.set_cursor(0, 0);
    app.tabs[0].editor.paste_plain_with_text("needle ");
    app.tabs[0].editor.set_cursor(0, 0);
    crate::ui_status::open_details(&mut app);
    refresh_caches(&mut app);
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_word_occ_running);
    assert_eq!(app.cached_word_occ.as_deref(), Some("3 occurrences"));
    assert_ne!(app.cached_word_occ_target, target);
}

#[test]
fn either_word_occurrence_consumer_can_keep_an_existing_request_alive() {
    let mut app = fixture();
    crate::ui_status::open_details(&mut app);
    app.status_bar_pins.insert("word_occ".into(), true);
    let target = app.word_occurrence_target().unwrap();
    let token = CancellationToken::new();
    app.pending_word_occurrence = Some((target, token.clone()));
    app.bg_word_occ_running = true;

    app.status_bar_pins.insert("word_occ".into(), false);
    refresh_caches(&mut app);
    assert!(!token.is_cancelled());
    assert!(app.pending_word_occurrence.is_some());
    app.status_bar_pins.insert("word_occ".into(), true);
    app.status_details.open = false;
    refresh_caches(&mut app);
    assert!(!token.is_cancelled());
    assert!(app.pending_word_occurrence.is_some());

    app.status_bar_pins.insert("word_occ".into(), false);
    refresh_caches(&mut app);
    assert!(token.is_cancelled());
    assert!(app.pending_word_occurrence.is_none());
}
