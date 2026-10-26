use super::*;
use crate::{
    app::Tab,
    background::{BackgroundResult, CancellationToken, EditorWorkKind},
    editor::Editor,
};
use std::{cell::Cell, sync::mpsc, time::Instant};

fn app(text: &str) -> App {
    let mut app = App::test_new();
    app.background.use_single_cpu_worker();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(text))];
    app.active_tab = 0;
    app.bracket_pair_colorization = false;
    for effect in crate::animation_config::AnimationEffect::ALL {
        app.animation_settings.set(effect, false);
    }
    app.cached_editor_analysis_target = app.editor_analysis_target();
    app.last_edit_time = Instant::now() - Duration::from_secs(1);
    app
}

// Hold the only worker without sleeps, so obsolete jobs are deterministically
// still queued while real refresh_caches calls process newer UI state.
fn hold_worker(app: &App) -> mpsc::Sender<()> {
    let (release, wait) = mpsc::channel();
    let (started, running) = mpsc::channel();
    app.background
        .pool_sender()
        .send(Box::new(move || {
            started.send(()).unwrap();
            let _ = wait.recv();
        }))
        .unwrap();
    // Priority work may pass a queued blocker; wait until it owns the sole
    // worker before exercising the cancellation/coalescing invariants.
    running.recv_timeout(Duration::from_secs(3)).unwrap();
    release
}

#[test]
fn cancelled_cursor_job_does_not_cache_a_negative_when_caret_returns() {
    let mut app = app("(value)\n");
    let release = hold_worker(&app);
    refresh_caches(&mut app);
    let token = app.pending_cursor_match.as_ref().unwrap().1.clone();
    app.editor_mut().unwrap().cursor_col = 3;
    refresh_caches(&mut app);
    assert!(token.is_cancelled());
    assert!(
        app.bg_cursor_match_running,
        "keep the slot until cancellation is acknowledged"
    );
    let jobs = app.background.active_job_count();
    for col in [0, 6, 1, 5, 0] {
        app.editor_mut().unwrap().cursor_col = col;
        refresh_caches(&mut app);
    }
    assert_eq!(
        app.background.active_job_count(),
        jobs,
        "cursor repeat cannot fill the queue"
    );
    release.send(()).unwrap();
    app.wait_for_background(Duration::from_secs(3), |app| !app.bg_cursor_match_running);
    assert_ne!(app.cached_cursor_match_target, app.cursor_match_target());
    refresh_caches(&mut app);
    app.wait_for_background(Duration::from_secs(3), |app| !app.bg_cursor_match_running);
    assert_eq!(app.cached_cursor_match_target, app.cursor_match_target());
    assert_eq!(app.cached_matching_bracket, Some((0, 6)));
}

#[test]
fn superseded_metrics_resume_with_current_text_without_disabling_features() {
    let mut app = app("first words\n");
    app.cached_editor_analysis_target = None;
    let release = hold_worker(&app);
    refresh_caches(&mut app);
    let token = app.pending_editor_analysis.as_ref().unwrap().1.clone();
    app.editor_mut().unwrap().paste_with_text("new ");
    app.last_edit_time = Instant::now();
    refresh_caches(&mut app);
    assert!(token.is_cancelled());
    release.send(()).unwrap();
    app.wait_for_background(Duration::from_secs(3), |app| {
        !app.bg_editor_analysis_running
    });
    assert!(app.editor_analysis_deferred);
    assert!(!app.editor().unwrap().large_file_mode);
    assert!(app.cached_editor_analysis_target.is_none());
    app.last_edit_time = Instant::now() - EDITOR_ANALYSIS_EDIT_QUIET_PERIOD;
    refresh_caches(&mut app);
    app.wait_for_background(Duration::from_secs(3), |app| {
        !app.bg_editor_analysis_running
    });
    assert_eq!(
        app.cached_editor_analysis_target,
        app.editor_analysis_target()
    );
    assert_eq!(app.cached_editor_word_count, 3);
}

#[test]
fn cancelling_analysis_across_undo_redo_keeps_saved_state_and_latest_metrics() {
    let mut app = app("saved words\n");
    let saved = app.editor().unwrap().rope.clone();
    app.editor_mut().unwrap().saved_rope = Some(saved.clone());
    app.editor_mut().unwrap().paste_with_text("new ");
    let release = hold_worker(&app);
    refresh_caches(&mut app);
    let token = app.pending_editor_analysis.as_ref().unwrap().1.clone();
    app.editor_mut().unwrap().undo();
    refresh_caches(&mut app);
    assert!(token.is_cancelled());
    release.send(()).unwrap();
    app.wait_for_background(Duration::from_secs(3), |app| {
        !app.bg_editor_analysis_running
    });
    refresh_caches(&mut app);
    app.wait_for_background(Duration::from_secs(3), |app| {
        !app.bg_editor_analysis_running
    });
    assert_eq!(app.editor().unwrap().rope, saved);
    assert!(!app.editor().unwrap().modified);
    assert_eq!(app.cached_editor_word_count, 2);
    app.editor_mut().unwrap().redo();
    refresh_caches(&mut app);
    app.wait_for_background(Duration::from_secs(3), |app| {
        !app.bg_editor_analysis_running
    });
    assert!(app.editor().unwrap().modified);
    assert_eq!(app.editor().unwrap().rope.to_string(), "new saved words\n");
    assert_eq!(app.cached_editor_word_count, 3);
    assert_eq!(
        app.cached_editor_analysis_target,
        app.editor_analysis_target()
    );
}

#[test]
fn scroll_keeps_useful_document_index_but_edits_cancel_it() {
    let mut app = app(&format!("{{\n{}}}\n", "value\n".repeat(100)));
    app.bracket_pair_colorization = true;
    let release = hold_worker(&app);
    refresh_caches(&mut app);
    let token = app.pending_bracket_analysis.as_ref().unwrap().1.clone();
    app.editor_mut().unwrap().scroll_offset = 40;
    refresh_caches(&mut app);
    assert!(!token.is_cancelled());
    app.editor_mut().unwrap().paste_with_text("[");
    refresh_caches(&mut app);
    assert!(token.is_cancelled());
    release.send(()).unwrap();
    app.wait_for_background(Duration::from_secs(3), |app| {
        !app.bg_bracket_analysis_running
    });
    assert!(app.cached_bracket_depth_index.is_none());
    refresh_caches(&mut app);
    app.wait_for_background(Duration::from_secs(3), |app| {
        !app.bg_bracket_analysis_running
    });
    let index = app.cached_bracket_depth_index.as_ref().unwrap();
    assert!(index.matches(app.editor().unwrap()));
    assert!(index.enabled());
    assert_eq!(index.line_starting_depth(40), Some(2));
}

#[test]
fn closing_editor_cancels_all_optional_work_and_does_not_schedule_a_redraw() {
    let mut app = app("(value)\n");
    app.bracket_pair_colorization = true;
    app.cached_editor_analysis_target = None;
    let release = hold_worker(&app);
    refresh_caches(&mut app);
    let tokens = [
        app.pending_editor_analysis.as_ref().unwrap().1.clone(),
        app.pending_bracket_analysis.as_ref().unwrap().1.clone(),
        app.pending_cursor_match.as_ref().unwrap().1.clone(),
    ];
    app.tabs.clear();
    refresh_caches(&mut app);
    assert!(tokens.iter().all(CancellationToken::is_cancelled));
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.background.active_job_count() > 0 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!app.poll_background_results());
    assert!(
        !app.bg_editor_analysis_running
            && !app.bg_bracket_analysis_running
            && !app.bg_cursor_match_running
    );
    assert!(!app.editor_analysis_deferred);
}

#[test]
fn stale_cancellation_acknowledgement_cannot_clear_a_different_request() {
    let mut app = app("(value)");
    let target = app.cursor_match_target().unwrap();
    let current = CancellationToken::new();
    app.pending_cursor_match = Some((target, current.clone()));
    app.bg_cursor_match_running = true;
    app.background
        .sender
        .send(BackgroundResult::EditorWorkCancelled {
            kind: EditorWorkKind::Cursor,
            token: CancellationToken::new(),
        })
        .unwrap();
    assert!(!app.poll_background_results());
    assert!(app.bg_cursor_match_running);
    assert!(
        app.pending_cursor_match
            .as_ref()
            .unwrap()
            .1
            .same_task(&current)
    );
}

#[test]
fn viewport_beyond_replaced_document_is_empty_and_does_not_underflow() {
    let editor = Editor::test_with_text("{\n}\n");
    let index = crate::background::BracketDepthIndex::build(
        editor.rope.clone(),
        editor.navigation_id,
        editor.content_version,
        editor.wrap_revision,
    );
    for (first, height) in [(1_000, 50), (usize::MAX, usize::MAX)] {
        assert_eq!(
            analyze_viewport_brackets(&editor.rope, &index, first, height),
            (0, Vec::new())
        );
    }
}

#[test]
fn markup_body_motion_needs_no_worker_but_tags_and_embedded_brackets_still_match() {
    let mut app = app("<div title=\"界\">body text (value)</div>\r\nplain text\r\n");
    app.editor_mut().unwrap().language_override = Some("HTML".into());
    app.editor_mut().unwrap().cursor_line = 1;
    app.cached_editor_analysis_target = app.editor_analysis_target();
    for col in 0..10 {
        app.editor_mut().unwrap().cursor_col = col;
        refresh_caches(&mut app);
        assert!(!app.bg_cursor_match_running);
        assert_eq!(app.cached_cursor_match_target, app.cursor_match_target());
    }
    let editor = app.editor_mut().unwrap();
    editor.cursor_line = 0;
    for col in 0..=editor.rope.line(0).len_chars() {
        editor.cursor_col = col;
        if !editor.may_have_cursor_pair() {
            assert_eq!(editor.find_matching_bracket(), None, "column {col}");
            assert_eq!(editor.find_matching_tag(), None, "column {col}");
        }
    }
    // Inconclusive bounded prefix searches must defer to the worker instead
    // of dropping matches on long attributes or malformed input.
    let mut editor =
        Editor::test_with_text(&format!("<div title=\"{}\">text</div>", "界".repeat(5_000)));
    editor.language_override = Some("HTML".into());
    editor.cursor_col = 4_000;
    assert!(editor.may_have_cursor_pair());
    assert!(editor.find_matching_tag().is_some());
}

#[test]
fn running_pair_scans_cancel_on_long_lines_in_both_directions() {
    let mut editor = Editor::test_with_text(&format!("({})", "value ".repeat(20_000)));
    for col in [0, editor.rope.len_chars() - 1] {
        editor.cursor_col = col;
        let checks = Cell::new(0);
        let cancel = || {
            checks.set(checks.get() + 1);
            checks.get() >= 8
        };
        assert_eq!(editor.find_matching_bracket_cancellable(&cancel), None);
        assert!(checks.get() <= 10, "do not finish the obsolete scan");
        assert!(editor.find_matching_bracket().is_some());
    }
    let mut editor = Editor::test_with_text(&format!("<div>\n{}</div>", "text\n".repeat(1_000)));
    editor.language_override = Some("HTML".into());
    for (line, col) in [(0, 2), (1_001, 3)] {
        editor.cursor_line = line;
        editor.cursor_col = col;
        let checks = Cell::new(0);
        assert_eq!(
            editor.find_matching_tag_cancellable(&|| {
                checks.set(checks.get() + 1);
                checks.get() >= 8
            }),
            None
        );
        assert!(checks.get() <= 9);
        assert!(editor.find_matching_tag().is_some());
    }
    let mut editor = Editor::test_with_text(&format!(
        "<div title=\"{}\">body</div>",
        "界".repeat(100_000)
    ));
    editor.language_override = Some("HTML".into());
    editor.cursor_col = 90_000;
    let checks = Cell::new(0);
    assert_eq!(
        editor.find_matching_tag_cancellable(&|| {
            checks.set(checks.get() + 1);
            checks.get() >= 8
        }),
        None
    );
    assert_eq!(
        checks.get(),
        8,
        "long lines also check cancellation while copying/searching"
    );
    assert!(editor.find_matching_tag().is_some());
}

#[test]
fn running_content_analysis_cancels_without_returning_partial_metrics() {
    let rope = ropey::Rope::from_str(&"// TODO BUG FIXME HACK\nwork();\n".repeat(1_000));
    let checks = Cell::new(0);
    assert!(
        analyze_editor_content_until(&rope, Instant::now() + Duration::from_secs(5), &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 8
        })
        .is_none()
    );
    assert_eq!(checks.get(), 8);
    let (errors, warnings, _, _, _) = analyze_editor_content(&rope);
    assert_eq!((errors, warnings), (2_000, 2_000));
}

#[test]
#[ignore = "manual performance measurement; run on one logical processor"]
fn benchmark_single_worker_obsolete_pair_latency() {
    let mut app = app(&format!(
        "{{\n{}",
        "    call(value, other);\n".repeat(100_000)
    ));
    let rope = app.editor().unwrap().rope.clone();
    for cancelled in [false, true] {
        let mut samples = Vec::new();
        for _ in 0..9 {
            let release = hold_worker(&app);
            let (probe, done) = mpsc::channel();
            let rope = rope.clone();
            app.background
                .pool_sender()
                .send(Box::new(move || {
                    let mut editor = Editor::new();
                    editor.rope = rope;
                    let result = editor.find_matching_bracket_cancellable(&|| cancelled);
                    std::hint::black_box(result);
                }))
                .unwrap();
            app.background
                .pool_sender()
                .send(Box::new(move || {
                    probe.send(Instant::now()).unwrap();
                }))
                .unwrap();
            let start = Instant::now();
            release.send(()).unwrap();
            samples.push(done.recv_timeout(Duration::from_secs(5)).unwrap() - start);
        }
        samples.sort_unstable();
        eprintln!(
            "single worker, obsolete pair cancelled={cancelled}: median={:?}, p90={:?}",
            samples[4], samples[8]
        );
    }
    app.tabs.clear();
}
