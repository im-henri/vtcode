use super::*;
use crate::{app::Tab, editor::Editor};
#[path = "vtcomplete_responsiveness_bench.rs"]
mod benchmarks;

fn inventory_app(count: usize) -> App {
    let mut app = App::test_new();
    app.completion_index
        .configure(crate::completion_index::IndexSettings::default());
    app.completion_index_sync.enabled = true;
    app.completion_index_sync.settings = Some(app.completion_index.settings());
    for n in 0..count {
        let mut editor = Editor::test_with_text("identifierValue");
        let path = app.explorer.root.join(format!("observed{n}.rs"));
        editor.file_path = Some(path.clone());
        app.completion_index_sync.observe(
            editor.navigation_id,
            path,
            editor.content_version,
            editor.modified,
        );
        app.tabs.push(Tab::new_file(editor));
    }
    app.active_tab = 0;
    app.last_edit_time = Instant::now() - Duration::from_secs(1);
    app
}

#[test]
fn inventory_and_bulk_close_are_paged_and_never_create_an_immediate_retry_loop() {
    let count = INVENTORY_PER_TURN * 3;
    let mut app = inventory_app(count);
    app.poll_completion_index();
    assert_eq!(
        app.completion_index_sync.scan,
        Some((0, INVENTORY_PER_TURN))
    );
    assert!(
        app.completion_index_next_wait()
            .is_some_and(|wait| !wait.is_zero())
    );
    for _ in 0..12 {
        app.poll_completion_index();
    }
    assert_eq!(app.completion_index_next_wait(), None);
    assert_eq!(app.completion_index_sync.observed.len(), count);
    app.tabs.clear();
    app.poll_completion_index();
    assert_eq!(
        app.completion_index_sync.observed.len(),
        count - INVENTORY_PER_TURN
    );
    for _ in 0..12 {
        app.poll_completion_index();
    }
    assert!(app.completion_index_sync.observed.is_empty());
    assert!(app.completion_index_sync.paths.is_empty());
    assert_eq!(app.completion_index_next_wait(), None);
}

#[test]
fn foreground_deferral_takes_precedence_over_an_expired_edit_deadline() {
    let mut app = inventory_app(1);
    app.completion_index_sync.buffers_changed = true;
    let foreground = app.vtcomplete_worker.activity().enter();
    app.poll_completion_index();
    assert!(app.completion_index_sync.scan.is_none());
    assert!(
        app.completion_index_next_wait()
            .is_some_and(|wait| !wait.is_zero())
    );
    drop(foreground);
}

#[test]
fn path_reference_counts_release_only_the_last_alias_after_save_as_or_close() {
    let mut state = State::default();
    let original = PathBuf::from("shared.rs");
    let destination = PathBuf::from("renamed.rs");
    assert!(state.observe(1, original.clone(), 0, true).is_none());
    assert!(state.observe(2, original.clone(), 0, true).is_none());
    assert!(state.observe(1, destination, 1, false).is_none());
    let old = state.observed.remove(&2).unwrap();
    assert_eq!(state.release_path(old.0), Some(original));
}
