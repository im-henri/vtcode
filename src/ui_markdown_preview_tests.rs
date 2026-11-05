use super::*;
use crate::{
    app::{App, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};

fn fixture() -> (App, Terminal<TestBackend>) {
    let mut app = App::test_new();
    let mut editor = Editor::new();
    editor.rope = ropey::Rope::from_str("# Old heading\n\nText in the old preview.\n");
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    (app, Terminal::new(TestBackend::new(180, 30)).unwrap())
}
fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>, width: u16) {
    terminal
        .draw(|frame| draw_markdown_preview(frame, app, Rect::new(0, 0, width, 25)))
        .unwrap();
}
fn settle(app: &mut App, terminal: &mut Terminal<TestBackend>, width: u16) {
    settle_preview_for_test(app, |app| draw(app, terminal, width));
}
fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < until,
            "preview worker did not reach the controlled state"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn hold_preparation(
    document: Arc<PreparedDocument>,
) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (locked, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _held = document.inline();
        locked.send(()).unwrap();
        // Keep the gate closed while low-priority workers reach it under a
        // loaded suite. The separate UI assertions still enforce responsiveness;
        // this timeout only prevents a locking regression from hanging the test.
        let _ = released.recv_timeout(Duration::from_secs(30));
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    (release, worker)
}

#[test]
fn preview_preparation_and_results_can_cross_worker_boundaries() {
    fn send<T: Send>() {}
    fn shared<T: Send + Sync>() {}
    shared::<PreparedDocument>();
    send::<CachedPreview>();
    send::<Outcome>();
}

#[test]
fn cancelled_preparation_and_reflow_do_not_publish_partial_layout() {
    let source = include_str!("../examples/markdown-preview.md");
    let document = PreparedDocument::new(source);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(PreparedDocument::prepare(source, Some(&cancel)).is_none());
    assert!(
        document
            .layout_with_cancel(
                60,
                Palette::from(&crate::app_theme::EditorTheme::Dark.colors()),
                None,
                Some(&cancel)
            )
            .is_none()
    );
}

#[test]
fn cold_preview_parsing_and_layout_run_off_the_ui_thread() {
    let (mut app, mut terminal) = fixture();
    draw(&mut app, &mut terminal, 80);
    let slot = &app.markdown_previews.slots[0];
    assert!(slot.ready.is_none());
    assert!(slot.pending.is_some());
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .contains("Preparing Markdown preview")
    );
    settle(&mut app, &mut terminal, 80);
    with_preview_for_test(&app, |preview| {
        assert_ne!(preview.document.prepared_on, std::thread::current().id());
        assert_ne!(preview.laid_out_on, std::thread::current().id());
        assert!(
            preview
                .text_rows
                .iter()
                .any(|text| text.contains("Old heading"))
        );
    });
}

#[test]
fn completion_wakes_the_event_loop_and_requests_redraw_without_input() {
    let (mut app, mut terminal) = fixture();
    app.tabs[0].kind = crate::app::TabKind::MarkdownPreview;
    app.markdown_preview = true;
    let wake = app.background.wake_handle();
    let observed = wake.generation();
    draw(&mut app, &mut terminal, 80);
    wake.wait_for_change(observed, Some(Duration::from_secs(5)));
    wait(|| app.markdown_previews.jobs.load(Ordering::Relaxed) == 0);
    assert_ne!(wake.generation(), observed);
    assert!(
        app.sync_markdown_preview_state(),
        "completion must request a repaint"
    );
    draw(&mut app, &mut terminal, 80);
    with_preview_for_test(&app, |preview| {
        assert!(
            preview
                .text_rows
                .iter()
                .any(|row| row.contains("Old heading"))
        )
    });
    assert!(
        !app.sync_markdown_preview_state(),
        "an idle preview must not keep repainting"
    );
}

#[test]
fn stalled_reflow_keeps_the_old_display_and_coalesces_edits_and_resizes() {
    let (mut app, mut terminal) = fixture();
    settle(&mut app, &mut terminal, 80);
    let document = with_preview_for_test(&app, |preview| preview.document.clone());
    let accesses = document.cache_accesses.load(Ordering::Relaxed);
    let (release, holder) = hold_preparation(document.clone());
    draw(&mut app, &mut terminal, 60);
    wait(|| document.cache_accesses.load(Ordering::Relaxed) > accesses);
    let start = Instant::now();
    for index in 0..40 {
        app.editor_mut().unwrap().rope =
            ropey::Rope::from_str(&format!("# New {index}\n\nBody {index}.\n"));
        draw(&mut app, &mut terminal, 70 + index % 7);
        assert_eq!(app.markdown_previews.slots.len(), 1);
        assert_eq!(
            app.markdown_previews.jobs.load(Ordering::Relaxed),
            1,
            "new requests must replace the desired state instead of queueing more work"
        );
        with_preview_for_test(&app, |preview| {
            assert!(
                preview
                    .text_rows
                    .iter()
                    .any(|text| text.contains("Old heading"))
            )
        });
    }
    let responsive = start.elapsed() < Duration::from_secs(2);
    let _ = release.send(());
    holder.join().unwrap();
    assert!(responsive, "drawing waited for the blocked layout mutex");
    settle(&mut app, &mut terminal, 74);
    with_preview_for_test(&app, |preview| {
        assert!(preview.rope.is_instance(&app.editor().unwrap().rope));
        assert_eq!(preview.width, 67);
        assert!(preview.text_rows.iter().any(|text| text.contains("New 39")));
        assert!(
            !preview
                .text_rows
                .iter()
                .any(|text| text.contains("Old heading"))
        );
    });
}

#[test]
fn closing_a_preview_cancels_stalled_work_without_joining_it() {
    let (mut app, mut terminal) = fixture();
    settle(&mut app, &mut terminal, 80);
    let document = with_preview_for_test(&app, |preview| preview.document.clone());
    let accesses = document.cache_accesses.load(Ordering::Relaxed);
    let (release, holder) = hold_preparation(document.clone());
    draw(&mut app, &mut terminal, 60);
    wait(|| document.cache_accesses.load(Ordering::Relaxed) > accesses);
    let start = Instant::now();
    app.markdown_previews.retain_open(&[], &app.background);
    let responsive = start.elapsed() < Duration::from_secs(1);
    assert!(app.markdown_previews.slots.is_empty());
    let _ = release.send(());
    holder.join().unwrap();
    assert!(
        responsive,
        "closing a preview must not wait for filesystem/layout workers"
    );
    wait(|| app.markdown_previews.jobs.load(Ordering::Relaxed) == 0);
    assert!(
        app.markdown_previews.slots.is_empty(),
        "a cancelled result cannot reopen a view"
    );
}

#[test]
fn hiding_a_preview_cancels_work_and_does_not_retry_it_while_hidden() {
    let (mut app, mut terminal) = fixture();
    app.tabs[0].kind = crate::app::TabKind::MarkdownPreview;
    app.markdown_preview = true;
    settle(&mut app, &mut terminal, 80);
    let document = with_preview_for_test(&app, |preview| preview.document.clone());
    let accesses = document.cache_accesses.load(Ordering::Relaxed);
    let (release, holder) = hold_preparation(document.clone());
    draw(&mut app, &mut terminal, 60);
    wait(|| document.cache_accesses.load(Ordering::Relaxed) > accesses);
    app.tabs.push(Tab::new_file(Editor::new()));
    app.active_tab = 1;
    app.sync_markdown_preview_state();
    assert!(
        app.markdown_previews.slots[0]
            .pending
            .as_ref()
            .unwrap()
            .cancel
            .is_cancelled()
    );
    let _ = release.send(());
    holder.join().unwrap();
    wait(|| app.markdown_previews.jobs.load(Ordering::Relaxed) == 0);
    app.sync_markdown_preview_state();
    assert!(
        !app.sync_markdown_preview_state(),
        "hidden previews must stay idle"
    );
    assert!(app.markdown_previews.slots[0].pending.is_none());
}

#[test]
fn split_views_keep_independent_widths_without_reflowing_on_warm_frames() {
    let (mut app, mut terminal) = fixture();
    let mut second = Editor::new();
    second.rope = app.editor().unwrap().rope.clone();
    app.tabs.push(Tab::new_file(second));
    let draw_both = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
        terminal
            .draw(|frame| {
                app.active_tab = 0;
                draw_markdown_preview(frame, app, Rect::new(0, 0, 70, 25));
                app.active_tab = 1;
                draw_markdown_preview(frame, app, Rect::new(72, 0, 95, 25));
                app.active_tab = 0;
            })
            .unwrap();
    };
    settle_preview_for_test(&mut app, |app| draw_both(app, &mut terminal));
    let left = app
        .markdown_previews
        .ready(app.tabs[0].editor.navigation_id);
    let right = app
        .markdown_previews
        .ready(app.tabs[1].editor.navigation_id);
    assert_eq!(left.width, 63);
    assert_eq!(right.width, 88);
    let left_document = left.document.clone();
    let right_document = right.document.clone();
    let accesses = (
        left_document.cache_accesses.load(Ordering::Relaxed),
        right_document.cache_accesses.load(Ordering::Relaxed),
    );
    for _ in 0..10 {
        draw_both(&mut app, &mut terminal);
        assert_eq!(app.markdown_previews.jobs.load(Ordering::Relaxed), 0);
    }
    assert_eq!(
        accesses,
        (
            left_document.cache_accesses.load(Ordering::Relaxed),
            right_document.cache_accesses.load(Ordering::Relaxed)
        )
    );
}

#[test]
fn a_cancelled_style_preparation_cannot_poison_a_restored_display() {
    let (mut app, mut terminal) = fixture();
    app.editor_mut().unwrap().rope =
        ropey::Rope::from_str("# Color\n\n```unrecognized_preview_language\nliteral code\n```\n");
    settle(&mut app, &mut terminal, 80);
    let slot = &mut app.markdown_previews.slots[0];
    let mut changed = slot.wanted.clone();
    changed.palette.code = Color::Magenta;
    let prepared = slot.preparation.take();
    let _unpublished = build_preview(
        changed,
        prepared,
        0,
        None,
        25,
        app.highlighter.shared_sets(),
        &CancellationToken::new(),
    );
    // Model a later document parse being cancelled before it can return a
    // preparation: only the prior display still owns the old document.
    settle(&mut app, &mut terminal, 80);
    with_preview_for_test(&app, |preview| {
        let (syntaxes, themes) = app.highlighter.shared_sets();
        let fresh = render_document(
            &app.editor().unwrap().rope.to_string(),
            preview.width,
            Palette::from(&app.colors),
            Some((&syntaxes, themes.themes.get(&app.syntax_theme))),
        );
        assert_eq!(format!("{:?}", preview.rows), format!("{fresh:?}"));
    });
}

#[test]
#[ignore = "manual UI-thread latency measurement"]
fn benchmark_background_markdown_preview_frames() {
    let (mut app, mut terminal) = fixture();
    for repeat in [1, 40, 400] {
        app.editor_mut().unwrap().rope =
            ropey::Rope::from_str(&include_str!("../examples/markdown-preview.md").repeat(repeat));
        for width in [80, 45] {
            let start = Instant::now();
            draw(&mut app, &mut terminal, width);
            let first = start.elapsed();
            let mut worst = first;
            let until = Instant::now() + Duration::from_secs(10);
            while app.markdown_previews.busy() {
                assert!(Instant::now() < until, "preview did not finish");
                std::thread::sleep(Duration::from_millis(1));
                let frame = Instant::now();
                draw(&mut app, &mut terminal, width);
                worst = worst.max(frame.elapsed());
            }
            println!(
                "background Markdown: {} bytes, width {width}, first UI frame {:.2} ms, worst UI frame {:.2} ms, ready {:.2} ms",
                app.editor().unwrap().rope.len_bytes(),
                first.as_secs_f64() * 1000.0,
                worst.as_secs_f64() * 1000.0,
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

#[test]
fn view_churn_cannot_bypass_the_global_admission_limit() {
    let (mut app, mut terminal) = fixture();
    settle(&mut app, &mut terminal, 80);
    let first = with_preview_for_test(&app, |preview| preview.document.clone());
    let mut second = Editor::new();
    second.rope = ropey::Rope::from_str("# Second\n\nAnother paragraph.\n");
    app.tabs.push(Tab::new_file(second));
    app.active_tab = 1;
    settle(&mut app, &mut terminal, 80);
    let second = with_preview_for_test(&app, |preview| preview.document.clone());
    let accesses = (
        first.cache_accesses.load(Ordering::Relaxed),
        second.cache_accesses.load(Ordering::Relaxed),
    );
    let (release_first, first_holder) = hold_preparation(first.clone());
    let (release_second, second_holder) = hold_preparation(second.clone());
    app.active_tab = 0;
    draw(&mut app, &mut terminal, 60);
    app.active_tab = 1;
    draw(&mut app, &mut terminal, 60);
    wait(|| {
        first.cache_accesses.load(Ordering::Relaxed) > accesses.0
            && second.cache_accesses.load(Ordering::Relaxed) > accesses.1
    });
    for index in 0..20 {
        let editor = Editor::new();
        let current = app.editor_mut().unwrap();
        current.navigation_id = editor.navigation_id;
        current.rope = ropey::Rope::from_str(&format!("# Latest view {index}\n"));
        draw(&mut app, &mut terminal, 80);
        assert_eq!(app.markdown_previews.jobs.load(Ordering::Relaxed), MAX_JOBS);
        assert!(app.markdown_previews.slots.len() <= RECENT_PREVIEWS + 2);
    }
    let _ = release_first.send(());
    let _ = release_second.send(());
    first_holder.join().unwrap();
    second_holder.join().unwrap();
    settle(&mut app, &mut terminal, 80);
    with_preview_for_test(&app, |preview| {
        assert!(
            preview
                .text_rows
                .iter()
                .any(|text| text.contains("Latest view 19"))
        )
    });
}

#[test]
fn retention_uses_recency_and_evicts_oversized_inactive_previews() {
    let (mut app, mut terminal) = fixture();
    app.tabs = (0..7)
        .map(|index| {
            let mut editor = Editor::new();
            editor.rope = ropey::Rope::from_str(&format!("# View {index}\n\nParagraph.\n"));
            Tab::new_file(editor)
        })
        .collect();
    let views: Vec<_> = app
        .tabs
        .iter()
        .map(|tab| tab.editor.navigation_id)
        .collect();
    for index in [0, 1, 2, 3, 4, 5, 0, 6] {
        app.active_tab = index;
        settle(&mut app, &mut terminal, 80);
    }
    assert!(
        app.markdown_previews
            .slots
            .iter()
            .any(|slot| slot.view == views[0])
    );
    assert!(
        !app.markdown_previews
            .slots
            .iter()
            .any(|slot| slot.view == views[1])
    );
    for _ in 0..3 {
        settle(&mut app, &mut terminal, 80);
    }
    let retained = app
        .markdown_previews
        .slots
        .iter_mut()
        .find(|slot| slot.view == views[0])
        .unwrap();
    let mut stale = retained.ready.take().unwrap();
    stale.retained_bytes = RETAINED_BYTES + 1;
    retained.retired.push(stale);
    settle(&mut app, &mut terminal, 80);
    assert!(
        !app.markdown_previews
            .slots
            .iter()
            .any(|slot| slot.view == views[0])
    );
    assert!(
        app.markdown_previews
            .slots
            .iter()
            .any(|slot| slot.view == views[6])
    );
}
