use super::*;
use crate::{app::Tab, background::BackgroundTaskSender, editor::Editor};
use std::{sync::mpsc, time::Instant};
const WAIT: Duration = Duration::from_secs(5);

#[test]
#[ignore = "manual controlled-queue latency measurement; use one logical processor"]
fn benchmark_editor_queue_latency() {
    let app = editor_app("value");
    let mut fifo = Vec::new();
    let mut preferred = Vec::new();
    for sample in 0..11 {
        for interactive in [sample % 2 == 0, sample % 2 != 0] {
            let (release, running) = hold(app.background.pool_sender());
            running.recv_timeout(WAIT).unwrap();
            for _ in 0..24 {
                app.background
                    .pool_sender()
                    .send(Box::new(|| {
                        let start = Instant::now();
                        while start.elapsed() < Duration::from_micros(300) {
                            std::hint::spin_loop();
                        }
                    }))
                    .unwrap();
            }
            let (done, completion) = mpsc::channel();
            let pool = if interactive {
                app.background.interactive_sender()
            } else {
                app.background.pool_sender()
            };
            pool.send(Box::new(move || done.send(Instant::now()).unwrap()))
                .unwrap();
            let start = Instant::now();
            release.send(()).unwrap();
            let elapsed = completion.recv_timeout(WAIT).unwrap() - start;
            if interactive {
                preferred.push(elapsed);
            } else {
                fifo.push(elapsed);
            }
            let deadline = Instant::now() + WAIT;
            while app.background.active_job_count() > 0 {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
        }
    }
    fifo.sort();
    preferred.sort();
    eprintln!(
        "one-worker dispatch behind 24 x 300us analysis jobs: FIFO={:?}, interactive={:?}",
        fifo[5], preferred[5]
    );
}

fn hold(pool: BackgroundTaskSender) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
    let (release, resume) = mpsc::channel();
    let (started, running) = mpsc::channel();
    pool.send(Box::new(move || {
        started.send(()).unwrap();
        let _ = resume.recv();
    }))
    .unwrap();
    (release, running)
}

fn editor_app(text: &str) -> App {
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

#[test]
fn cursor_feedback_runs_before_queued_bulk_analysis_on_a_single_worker() {
    let mut app = editor_app("(value)");
    let (first, running) = hold(app.background.pool_sender());
    running.recv_timeout(WAIT).unwrap();
    let (_second, second_running) = hold(app.background.pool_sender());
    refresh_caches(&mut app);
    assert!(app.bg_cursor_match_running);
    first.send(()).unwrap();
    // Once bulk work starts, no other CPU job can execute. The actual cursor
    // request must already have published its answer despite being enqueued last.
    second_running.recv_timeout(WAIT).unwrap();
    app.poll_background_results();
    assert!(!app.bg_cursor_match_running);
    assert_eq!(app.cached_matching_bracket, Some((0, 6)));
}

#[test]
fn filesystem_browsing_and_hover_finish_while_cpu_analysis_is_blocked() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("folder")).unwrap();
    let file = root.path().join("source.txt");
    std::fs::write(&file, "one\ntwo\n").unwrap();
    let mut app = editor_app("value");
    let (_release, running) = hold(app.background.pool_sender());
    running.recv_timeout(WAIT).unwrap();
    app.explorer.root = root.path().to_path_buf();
    app.folder_browser_path = root.path().to_path_buf();
    app.folder_browser.locations = vec![("Workspace".into(), root.path().to_path_buf())];
    app.refresh_folder_browser();
    app.request_explorer_hover_info(file.clone(), false, true);
    app.wait_for_background(WAIT, |app| {
        !app.folder_browser_loading && app.explorer_hover_info_cache.contains_key(&file)
    });
    assert_eq!(app.folder_browser_entries, vec![root.path().join("folder")]);
    assert_eq!(app.explorer_hover_info_cache[&file].line_count, Some(3));
}

#[test]
fn maintenance_cannot_occupy_editor_workers_and_uses_one_reused_worker() {
    let mut app = editor_app("(value)");
    let (_release, running) = hold(app.background.maintenance_sender());
    running.recv_timeout(WAIT).unwrap();
    refresh_caches(&mut app);
    app.wait_for_background(WAIT, |app| !app.bg_cursor_match_running);
    assert_eq!(app.cached_matching_bracket, Some((0, 6)));
    // One CPU worker and one lazily allocated maintenance worker.
    assert_eq!(app.background.queue_snapshot().2, 2);
}

#[test]
fn editor_refresh_reuses_bracket_summaries_across_edits_and_undo() {
    let mut app = editor_app(&"fn f() {\n    call(value);\n}\n".repeat(2_000));
    app.bracket_pair_colorization = true;
    refresh_caches(&mut app);
    app.wait_for_background(WAIT, |app| !app.bg_bracket_analysis_running);
    assert!(app.cached_bracket_depth_index.as_ref().unwrap().enabled());
    app.editor_mut().unwrap().paste_with_text("{{\n");
    app.last_edit_time = Instant::now() - Duration::from_secs(1);
    refresh_caches(&mut app);
    app.wait_for_background(WAIT, |app| !app.bg_bracket_analysis_running);
    let index = app.cached_bracket_depth_index.as_ref().unwrap();
    assert!(index.matches(app.editor().unwrap()));
    assert!(index.scanned_lines <= 256);
    assert_eq!(index.line_starting_depth(3_001), Some(2));
    app.editor_mut().unwrap().undo();
    refresh_caches(&mut app);
    app.wait_for_background(WAIT, |app| !app.bg_bracket_analysis_running);
    let index = app.cached_bracket_depth_index.as_ref().unwrap();
    assert!(index.matches(app.editor().unwrap()));
    assert!(index.scanned_lines <= 256);
    assert_eq!(index.line_starting_depth(3_000), Some(0));
}
