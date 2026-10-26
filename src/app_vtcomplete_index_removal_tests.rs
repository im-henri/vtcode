use super::*;
use crate::{
    app::Tab,
    completion_index::{CompletionIndex, IndexPhase, IndexSpeed},
    editor::Editor,
};
use std::path::Path;

fn poll_quiet_buffer(app: &mut App) {
    app.last_edit_time = Instant::now() - Duration::from_secs(1);
    app.poll_completion_index();
}

fn wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "{description}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_state(index: &CompletionIndex, files: usize, phase: IndexPhase) {
    wait_until(
        "completion index did not settle after removal lifecycle change",
        || {
            let status = index.status();
            assert!(status.error.is_none(), "index error: {status:?}");
            status.indexed_files == files
                && status.pending_files == 0
                && status.phase == phase
                && status.settings == index.settings()
        },
    );
}

fn rescan_through_worker_barrier(index: &CompletionIndex, files: usize) {
    let settings = index.settings();
    assert!(settings.enabled && !settings.paused);
    let mut paused = settings.clone();
    paused.paused = true;
    index.configure(paused);
    wait_for_state(index, files, IndexPhase::Paused);
    let before = index.revision();
    index.rescan();
    index.configure(settings);
    // The rescan is queued before resume. Reaching the resumed Idle phase
    // therefore cannot accidentally match a preceding cached Idle snapshot.
    wait_until(
        "rescan resurrected a removed file or did not finish",
        || {
            let status = index.status();
            assert!(status.error.is_none(), "index error: {status:?}");
            index.revision() > before
                && status.phase == IndexPhase::Idle
                && status.indexed_files == files
                && status.pending_files == 0
                && status.settings == index.settings()
        },
    );
}

fn has_word(index: &CompletionIndex, prefix: &str, word: &str) -> bool {
    index
        .query(prefix, "")
        .iter()
        .any(|entry| entry.word == word)
}

fn dirty_file_app(temp: &tempfile::TempDir) -> (App, PathBuf) {
    let root = temp.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("removed.custom");
    std::fs::write(&path, "savedRemovalIdentifier").unwrap();
    let mut app = App::new_deferred(root.clone());
    app.completion_index = CompletionIndex::new_for_test(root, temp.path().join("cache"));
    let mut settings = app.completion_index.settings();
    settings.speed = IndexSpeed::Fast;
    app.completion_index.configure(settings);
    let mut editor = Editor::test_with_text("unsavedRemovalIdentifier");
    editor.file_path = Some(path.clone());
    editor.modified = true;
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 0;
    poll_quiet_buffer(&mut app);
    wait_until("dirty buffer was not indexed", || {
        has_word(
            &app.completion_index,
            "unsavedRemoval",
            "unsavedRemovalIdentifier",
        )
    });
    wait_for_state(&app.completion_index, 1, IndexPhase::Idle);
    (app, path)
}

fn assert_erased(app: &App, path: &Path) {
    assert!(
        !app.completion_index
            .file_page(0, 32)
            .paths
            .contains(&path.to_owned())
    );
    assert_eq!(app.completion_index_next_wait(), None);
}

#[test]
fn erasing_an_open_buffer_does_not_requeue_it_on_idle_or_when_it_closes() {
    let temp = tempfile::tempdir().unwrap();
    let (mut app, path) = dirty_file_app(&temp);
    assert!(app.completion_index.remove_file(&path));
    wait_for_state(&app.completion_index, 0, IndexPhase::Idle);
    for _ in 0..3 {
        poll_quiet_buffer(&mut app);
    }
    assert_erased(&app, &path);
    let before_close = app.completion_index.revision();
    app.tabs.clear();
    poll_quiet_buffer(&mut app);
    wait_until("close lifecycle did not finish", || {
        let status = app.completion_index.status();
        app.completion_index.revision() > before_close
            && status.phase == IndexPhase::Idle
            && status.indexed_files == 0
            && status.pending_files == 0
    });
    rescan_through_worker_barrier(&app.completion_index, 0);
    assert!(!has_word(
        &app.completion_index,
        "savedRemoval",
        "savedRemovalIdentifier"
    ));
    assert!(!has_word(
        &app.completion_index,
        "unsavedRemoval",
        "unsavedRemovalIdentifier"
    ));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "savedRemovalIdentifier"
    );
}

#[test]
fn a_new_edit_can_index_the_buffer_again_without_restoring_or_unignoring_it() {
    let temp = tempfile::tempdir().unwrap();
    let (mut app, path) = dirty_file_app(&temp);
    assert!(app.completion_index.remove_file(&path));
    wait_for_state(&app.completion_index, 0, IndexPhase::Idle);
    for ch in " freshAfterErase ".chars() {
        app.editor_mut().unwrap().insert_char(ch);
    }
    poll_quiet_buffer(&mut app);
    wait_until("new editing did not index the buffer", || {
        has_word(&app.completion_index, "freshAfter", "freshAfterErase")
    });
    wait_for_state(&app.completion_index, 1, IndexPhase::Idle);
    assert_eq!(app.completion_index.file_page(0, 32).paths, [path]);
    assert!(!has_word(
        &app.completion_index,
        "savedRemoval",
        "savedRemovalIdentifier"
    ));
}

#[test]
fn reopening_an_erased_file_indexes_it_as_a_new_opened_file() {
    let temp = tempfile::tempdir().unwrap();
    let (mut app, path) = dirty_file_app(&temp);
    assert!(app.completion_index.remove_file(&path));
    wait_for_state(&app.completion_index, 0, IndexPhase::Idle);
    app.tabs.clear();
    poll_quiet_buffer(&mut app);
    wait_for_state(&app.completion_index, 0, IndexPhase::Idle);
    let mut editor = Editor::test_with_text("savedRemovalIdentifier");
    editor.file_path = Some(path.clone());
    app.tabs.push(Tab::new_file(editor));
    poll_quiet_buffer(&mut app);
    wait_until("reopened file did not index", || {
        has_word(
            &app.completion_index,
            "savedRemoval",
            "savedRemovalIdentifier",
        )
    });
    wait_for_state(&app.completion_index, 1, IndexPhase::Idle);
    assert_eq!(app.completion_index.file_page(0, 32).paths, [path]);
}

#[test]
fn save_as_after_erasure_indexes_the_new_path_without_reintroducing_the_old_path() {
    let temp = tempfile::tempdir().unwrap();
    let (mut app, original) = dirty_file_app(&temp);
    assert!(app.completion_index.remove_file(&original));
    wait_for_state(&app.completion_index, 0, IndexPhase::Idle);
    let destination = original.with_file_name("renamed.custom");
    std::fs::write(&destination, app.editor().unwrap().rope.to_string()).unwrap();
    let editor = app.editor_mut().unwrap();
    editor.file_path = Some(destination.clone());
    editor.modified = false;
    poll_quiet_buffer(&mut app);
    wait_until("new Save As path did not index", || {
        has_word(
            &app.completion_index,
            "unsavedRemoval",
            "unsavedRemovalIdentifier",
        )
    });
    wait_for_state(&app.completion_index, 1, IndexPhase::Idle);
    assert_eq!(app.completion_index.file_page(0, 32).paths, [destination]);
    assert_erased(&app, &original);
}
