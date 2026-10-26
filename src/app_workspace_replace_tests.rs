use super::*;
use crate::{
    app::{Tab, TabKind},
    editor::Editor,
    remote::{self, RemotePath},
};
use std::{sync::Arc, time::Duration};

struct Release(Arc<std::sync::atomic::AtomicBool>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[test]
fn workspace_replace_rejects_stale_regex_ranges_instead_of_inserting_a_fallback_template() {
    for (text, end) in [("bar keep", 3), ("foobar keep", 6)] {
        let before = ropey::Rope::from_str(text);
        let (prepared, replaced) = prepare_snapshot_matches(
            before.clone(),
            vec![(1, 0, end)],
            ReplaceSnapshotOptions {
                line_numbers_are_one_based: true,
                search_query: "(foo)",
                replacement: "$1!",
                case_sensitive: true,
                use_regex: true,
                whole_word: false,
                preserve: false,
                selection: None,
            },
        );
        assert_eq!(prepared.rope(), &before, "stale range in {text:?}");
        assert_eq!(replaced, 0);
    }
}

fn replace_and_wait(app: &mut App) {
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_global_search_running);
    assert!(
        !app.global_search_results.is_empty(),
        "search must find fixture matches"
    );
    app.replace_all_in_files();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_global_replace_running && !app.bg_global_search_running
    });
}

#[test]
fn workspace_replace_keeps_hidden_desk_drafts_local_and_undoable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hidden.txt");
    std::fs::write(&path, "disk foo").unwrap();
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    let mut editor = Editor::test_with_text("disk foo");
    editor.file_path = Some(path.clone());
    editor.record_saved_snapshot(editor.rope.clone());
    editor.select_all();
    editor.paste_plain_with_text("draft foo");
    let mut shared = Editor::new();
    shared.copy_document_from(&editor);
    app.tabs.push(Tab::new_file(editor));
    let main = app.desks.active;
    app.new_desk("other", false);
    assert!(app.tabs.iter().all(|tab| tab.kind != TabKind::File));
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "bar".into();
    replace_and_wait(&mut app);
    shared.refresh_document();
    assert_eq!(shared.rope.to_string(), "draft bar");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "disk foo");
    shared.undo();
    assert_eq!(shared.rope.to_string(), "draft foo");
    shared.redo();
    app.switch_desk(main);
    assert_eq!(app.editor_mut().unwrap().rope.to_string(), "draft bar");
}

#[test]
fn workspace_replace_updates_closed_remote_files_and_preserves_hidden_unsaved_buffers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("closed.txt"), "foo λ foo\r\n").unwrap();
    std::fs::write(root.path().join("hidden.txt"), "disk foo").unwrap();
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    let mut editor = Editor::test_with_text("disk foo");
    editor.file_path = Some(workspace.view_key(&RemotePath::new(b"hidden.txt".to_vec()).unwrap()));
    editor.record_saved_snapshot(editor.rope.clone());
    editor.select_all();
    editor.paste_plain_with_text("draft foo");
    let mut shared = Editor::new();
    shared.copy_document_from(&editor);
    app.tabs.push(Tab::new_file(editor));
    app.new_desk("other", false);
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "猫".into();
    replace_and_wait(&mut app);
    shared.refresh_document();
    assert_eq!(shared.rope.to_string(), "draft 猫");
    assert_eq!(
        std::fs::read_to_string(root.path().join("hidden.txt")).unwrap(),
        "disk foo"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("closed.txt")).unwrap(),
        "猫 λ 猫\r\n"
    );
    shared.undo();
    assert_eq!(shared.rope.to_string(), "draft foo");
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn workspace_replace_local_edits_undo_typing_and_cancellation_do_not_wait_for_remote_ack() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let root = tempfile::tempdir().unwrap();
    for name in ["open.txt", "closed.txt"] {
        std::fs::write(root.path().join(name), "foo").unwrap();
    }
    let hold = Arc::new(AtomicBool::new(false));
    let _release = Release(hold.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let transferred = Arc::new(AtomicUsize::new(0));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_status(
        root.path(),
        hold.clone(),
        transferred.clone(),
        blocked.clone(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    let key = |name: &str| workspace.view_key(&RemotePath::new(name.as_bytes().to_vec()).unwrap());
    let mut editor = Editor::test_with_text("foo");
    editor.file_path = Some(key("open.txt"));
    editor.record_saved_snapshot(editor.rope.clone());
    app.tabs.push(Tab::new_file(editor));
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "bar".into();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert_eq!(app.global_search_results.len(), 2);
    let late_loaded = workspace.load(&key("closed.txt")).unwrap();
    let mut late = Editor::test_with_text("foo");
    late.file_path = Some(key("closed.txt"));
    late.encoding = late_loaded.encoding;
    late.saved_encoding = late_loaded.encoding;
    late.file_size = late_loaded.file_size;
    late.last_disk_modified = late_loaded.last_disk_modified;
    late.set_remote_checkpoint(late_loaded.remote_checkpoint);
    late.record_saved_snapshot(late.rope.clone());
    hold.store(true, Ordering::Release);
    app.replace_all_in_files();
    app.wait_for_background(Duration::from_secs(5), |app| {
        blocked.load(Ordering::Acquire)
            && app
                .editor()
                .is_some_and(|editor| editor.rope.to_string() == "bar")
    });
    assert!(
        app.bg_global_replace_running,
        "local edits appear before the remote reply"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("closed.txt")).unwrap(),
        "bar"
    );
    let traffic = transferred.load(Ordering::Relaxed);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "foo");
    for _ in 0..64 {
        let editor = app.editor_mut().unwrap();
        editor.move_right();
        editor.insert_char('λ');
    }
    let draft = app.editor().unwrap().rope.to_string();
    assert_eq!(
        transferred.load(Ordering::Relaxed),
        traffic,
        "editing and undo send no RPCs"
    );
    // Model a clean view that finished opening just before the disk update.
    // Completion must inspect it and reload through the ordinary undoable path.
    app.tabs.push(Tab::new_file(late));
    app.workspace_replace.cancel.as_ref().unwrap().cancel();
    hold.store(false, Ordering::Release);
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_global_replace_running
            && !app.bg_global_search_running
            && !app.background.has_active_jobs()
            && app.tabs[1].editor.rope.to_string() == "bar"
    });
    assert_eq!(
        app.tabs[0].editor.rope.to_string(),
        draft,
        "the final reply cannot reapply an undone local edit"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("open.txt")).unwrap(),
        "foo"
    );
    assert!(
        app.notification_history
            .iter()
            .any(|(message, _, _)| message == "Stopped replacement after 2 occurrences in 2 files")
    );
    app.tabs[1].editor.undo();
    assert_eq!(app.tabs[1].editor.rope.to_string(), "foo");
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn workspace_replace_completion_from_an_old_workspace_cannot_finish_or_edit_the_new_one() {
    let first = tempfile::tempdir().unwrap();
    let next = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(first.path().to_path_buf());
    app.source_control_enabled = false;
    app.bg_global_replace_running = true;
    let cancel = crate::background::CancellationToken::new();
    app.workspace_replace.cancel = Some(cancel.clone());
    let workspace = app.workspace_generation;
    let generation = app.global_replace_generation;
    app.change_workspace(next.path());
    assert!(cancel.is_cancelled());
    assert!(!app.bg_global_replace_running);
    let mut editor = Editor::test_with_text("foo");
    let path = next.path().join("file.txt");
    editor.file_path = Some(path.clone());
    let prepared = crate::formatters::prepare(&editor.rope, "bar");
    let result = crate::background::OpenFileReplaceResult {
        path,
        source_document_id: editor.save_identity,
        source_content_version: editor.content_version,
        prepared,
        replacements: 1,
    };
    app.tabs.push(Tab::new_file(editor));
    app.bg_global_replace_running = true; // a different operation now owns it
    assert!(!app.apply_workspace_replace_prepared(workspace, generation, vec![result], 0));
    assert!(!app.finish_workspace_replace(
        workspace,
        generation,
        Default::default(),
        Default::default(),
        None
    ));
    assert!(app.bg_global_replace_running);
    assert_eq!(app.editor().unwrap().rope.to_string(), "foo");
}

#[test]
#[ignore = "manual batch compilation/template reuse and dense stale-result benchmark"]
fn benchmark_workspace_replace_program_reuse_and_dense_stale_matches() {
    use std::{hint::black_box, time::Instant};
    let source = ropey::Rope::from_str("xfoo!\r\nkeep λ\n");
    let options = crate::workspace_replace::Options {
        query: "(?<=x)(foo)(?=!)".into(),
        replacement: "$1猫".into(),
        case_sensitive: true,
        regex: true,
        whole_word: false,
        preserve_case: false,
    };
    let program = SnapshotReplacer::workspace(&options).unwrap();
    let cancel = crate::background::CancellationToken::new();
    let mut timings = [Vec::new(), Vec::new()];
    for pass in 0..6 {
        for step in 0..2 {
            let mode = (pass + step) % 2;
            let start = Instant::now();
            for _ in 0..1000 {
                let result = if mode == 0 {
                    prepare_snapshot_matches(
                        source.clone(),
                        vec![(1, 1, 4)],
                        ReplaceSnapshotOptions {
                            line_numbers_are_one_based: true,
                            search_query: &options.query,
                            replacement: &options.replacement,
                            case_sensitive: true,
                            use_regex: true,
                            whole_word: false,
                            preserve: false,
                            selection: None,
                        },
                    )
                } else {
                    program
                        .prepare(source.clone(), vec![(1, 1, 4)], &cancel)
                        .unwrap()
                };
                assert_eq!(result.0.rope().to_string(), "xfoo猫!\r\nkeep λ\n");
                black_box(result);
            }
            if pass > 0 {
                timings[mode].push(start.elapsed().as_secs_f64() * 1000.);
            }
        }
    }
    let stale_source = ropey::Rope::from_str(&"bar ".repeat(100_000));
    let stale = (0..100_000)
        .map(|i| (1, i * 4, i * 4 + 3))
        .collect::<Vec<_>>();
    let start = Instant::now();
    assert!(program.prepare(stale_source, stale, &cancel).is_err());
    let stale_ms = start.elapsed().as_secs_f64() * 1000.;
    let medians = timings.map(|mut values| {
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    });
    eprintln!(
        "workspace_replace batches=1000 compile_each_ms={:.3} reuse_ms={:.3} stale_ranges=100000 rejection_ms={stale_ms:.3}",
        medians[0], medians[1]
    );
}
