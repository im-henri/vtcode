use super::*;
use crate::{app::Tab, editor::Editor};

fn app_with(text: &str) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some("source.custom".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.symbol_list_active = true;
    app.sidebar_view = SidebarView::Outline;
    app
}

fn finish(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.source_symbol_work.picker_pending() && !app.source_symbol_work.outline_pending()
    });
}

#[test]
fn streamed_parser_matches_contiguous_parser_across_rope_chunks_and_long_lines() {
    let names = [
        "short_name".to_owned(),
        "résumé_界<Generic".to_owned(),
        "αβγ".repeat(500),
    ];
    for indent in [0, 1, 1020, 4096] {
        for &(prefix, _) in SOURCE_SYMBOL_PATTERNS {
            for name in &names {
                let text = format!(
                    "{}{prefix}{name}({});\r\n",
                    " ".repeat(indent),
                    "unrelated ".repeat(2000)
                );
                let rope = ropey::Rope::from_str(&text);
                let mut scratch = String::new();
                let expected = source_symbol(&text)
                    .filter(|(n, _)| !n.is_empty())
                    .map(|(n, k)| (n.to_owned(), k));
                assert_eq!(
                    symbol_in_line(rope.line(0), &mut scratch, &CancellationToken::new()),
                    expected
                );
                assert!(
                    scratch.capacity() < 1024,
                    "unrelated long line contents must not be flattened"
                );
            }
        }
    }
    let rope = ropey::Rope::from_str(&format!("{{{}\n", " ".repeat(8 * 1024 * 1024)));
    let mut scratch = String::new();
    assert!(symbol_in_line(rope.line(0), &mut scratch, &CancellationToken::new()).is_none());
    assert!(scratch.capacity() < 1024);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let rope = ropey::Rope::from_str(&format!("{}fn target() {{}}\n", " ".repeat(64 * 1024)));
    assert!(symbol_in_line(rope.line(0), &mut scratch, &cancel).is_none());
}

#[test]
fn small_requests_stay_synchronous_and_background_scans_keep_all_symbols_in_order() {
    let mut app = app_with("pub(crate) fn first() {}\nstruct Second {}\n");
    app.build_symbol_list();
    assert!(!app.source_symbol_work.picker_pending());
    assert_eq!(
        app.symbol_list_items
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "Second"]
    );
    let text = (0..10_000)
        .map(|n| format!("fn symbol_{n}() {{}}\n"))
        .collect::<String>();
    app.tabs[0].editor = Editor::test_with_text(&text);
    app.build_symbol_list();
    assert!(app.source_symbol_work.picker_pending());
    finish(&mut app);
    assert_eq!(app.symbol_list_items.len(), 10_000);
    for (line, item) in app.symbol_list_items.iter().enumerate() {
        assert_eq!((item.line, item.tab_index), (line, Some(0)));
        assert_eq!(item.name, format!("symbol_{line}"));
    }
}

#[test]
fn pending_picker_uses_latest_query_and_cannot_replace_closed_or_newer_targets() {
    let text = "fn old_name() {}\n".repeat(10_000);
    let mut app = app_with(&text);
    app.build_symbol_list();
    app.symbol_list_query = "new".into();
    app.update_symbol_filter();
    app.tabs[0].editor = Editor::test_with_text("fn new_name() {}\n");
    app.build_symbol_list();
    finish(&mut app);
    assert_eq!(app.symbol_list_items.len(), 1);
    assert_eq!(app.symbol_list_items[0].name, "new_name");
    assert_eq!(app.symbol_list_filtered, [0]);
    app.tabs[0].editor = Editor::test_with_text(&text);
    app.build_symbol_list();
    let serial = app.source_symbol_work.picker.as_ref().unwrap().0;
    let key = Key::from_tab(0, &app.tabs[0].editor);
    app.symbol_list_active = false;
    app.source_symbol_work.cancel_picker();
    assert!(!app.apply_source_symbols(Reply {
        serial,
        group: 0,
        destination: Destination::Picker(false),
        keys: vec![key],
        rows: vec![]
    }));
    assert!(!app.source_symbol_work.picker_pending());
}

#[test]
fn stale_editor_results_restart_and_lsp_outline_results_take_precedence() {
    let mut app = app_with(&"fn before() {}\n".repeat(10_000));
    app.build_symbol_list();
    app.symbol_list_query = "after".into();
    app.update_symbol_filter();
    app.tabs[0].editor = Editor::test_with_text("fn after() {}\n");
    finish(&mut app);
    assert_eq!(app.symbol_list_items[0].name, "after");
    assert_eq!(app.symbol_list_filtered, [0]);
    app.tabs[0].editor = Editor::test_with_text(&"fn fallback() {}\n".repeat(10_000));
    app.update_outline();
    assert!(app.source_symbol_work.outline_pending());
    app.lsp_outline_available = true;
    app.outline_items = vec![SymbolItem {
        name: "from_lsp".into(),
        kind: "function",
        line: 0,
        tab_index: Some(0),
        file_name: None,
        file_path: None,
    }];
    finish(&mut app);
    assert_eq!(app.outline_items[0].name, "from_lsp");
}

#[test]
fn outline_scroll_is_clamped_when_async_results_arrive_not_when_they_are_requested() {
    let mut app = app_with(&format!(
        "{}fn last() {{}}\n",
        "// ordinary line\n".repeat(10_000)
    ));
    app.outline_scroll = 25;
    app.update_outline();
    assert_eq!(app.outline_scroll, 25);
    finish(&mut app);
    assert_eq!(app.outline_items[0].name, "last");
    assert_eq!(app.outline_scroll, 0);
}

#[test]
fn large_scans_yield_to_other_interactive_work_before_publishing_all_results() {
    use std::sync::mpsc;
    let mut app = app_with(&"fn item() {}\n".repeat(20_000));
    app.background.use_single_cpu_worker();
    let pool = app.background.interactive_sender();
    let (entered, started) = mpsc::channel();
    let (release, unblock) = mpsc::channel();
    pool.send(Box::new(move || {
        entered.send(()).unwrap();
        unblock.recv_timeout(Duration::from_secs(5)).unwrap();
    }))
    .unwrap();
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    app.build_symbol_list();
    assert!(app.source_symbol_work.picker_pending());
    assert!(app.symbol_list_items.is_empty());
    let (entered, marker) = mpsc::channel();
    let (release_marker, unblock) = mpsc::channel();
    pool.send(Box::new(move || {
        entered.send(()).unwrap();
        unblock.recv_timeout(Duration::from_secs(5)).unwrap();
    }))
    .unwrap();
    release.send(()).unwrap();
    marker.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        !app.background.has_pending_results(),
        "the source scan must yield instead of finishing ahead of already-queued interactive work"
    );
    app.symbol_list_query = "item".into();
    app.update_symbol_filter();
    release_marker.send(()).unwrap();
    finish(&mut app);
    assert_eq!(app.symbol_list_filtered.len(), 20_000);
}
