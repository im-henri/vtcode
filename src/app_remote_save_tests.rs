use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Saving {
    app: App,
    workspace: remote::workspace::Workspace,
    root: tempfile::TempDir,
    path: PathBuf,
    hold: Arc<AtomicBool>,
    blocked: Arc<AtomicBool>,
    outgoing: Arc<AtomicUsize>,
    incoming: Arc<AtomicUsize>,
}
impl Drop for Saving {
    fn drop(&mut self) {
        self.hold.store(false, Ordering::Release);
        self.workspace.client.disconnect();
    }
}
impl Saving {
    fn new(bytes: impl AsRef<[u8]>) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.txt"), bytes).unwrap();
        let hold = Arc::new(AtomicBool::new(false));
        let blocked = Arc::new(AtomicBool::new(false));
        let outgoing = Arc::new(AtomicUsize::new(0));
        let incoming = Arc::new(AtomicUsize::new(0));
        let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_traffic(
            root.path(),
            hold.clone(),
            outgoing.clone(),
            blocked.clone(),
            incoming.clone(),
        ));
        let mut app = App::new_deferred(workspace.view_root.clone());
        app.remote_workspace = Some(workspace.clone());
        app.source_control_enabled = false;
        app.bg_explorer_refresh_running = true;
        app.trim_trailing_on_save = false;
        app.insert_final_newline = false;
        app.format_on_save = false;
        let path = workspace.target_key("file.txt").unwrap();
        app.open_file(&path);
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.editor().is_some_and(|editor| !editor.bg_loading)
        });
        workspace.client.call(remote::Command::Ping).unwrap();
        Self {
            app,
            workspace,
            root,
            path,
            hold,
            blocked,
            outgoing,
            incoming,
        }
    }
    fn bytes(&self) -> (usize, usize) {
        (
            self.outgoing.load(Ordering::Acquire),
            self.incoming.load(Ordering::Acquire),
        )
    }
    fn finish(&mut self) {
        self.hold.store(false, Ordering::Release);
        self.app
            .wait_for_background(Duration::from_secs(10), |app| {
                app.pending_file_saves.is_empty()
                    && app.deferred_file_saves.is_empty()
                    && app.pending_disk_checks.is_empty()
                    && app.pending_file_reloads.is_empty()
            });
        self.workspace.client.call(remote::Command::Ping).unwrap(); // finish any transport writes before measuring
    }
    fn wait_blocked(&mut self) {
        self.app.wait_for_background(Duration::from_secs(5), |_| {
            self.blocked.load(Ordering::Acquire)
        });
    }
}

#[test]
fn clean_remote_ctrl_s_adds_no_bytes_even_before_the_first_save_and_for_large_or_utf16_files() {
    let utf16 = [
        vec![0xff, 0xfe],
        "λ🦀\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect(),
    ]
    .concat();
    for bytes in [
        b"unchanged\n".to_vec(),
        "unchanged λ🦀\r\n".repeat(80_000).into_bytes(),
        utf16,
    ] {
        let mut f = Saving::new(&bytes);
        let encoding = f.app.editor().unwrap().encoding;
        let modified = std::fs::metadata(f.root.path().join("file.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let traffic = f.bytes();
        // Clean format-on-save must not invoke a formatter or open settings.
        f.app.format_on_save = true;
        for _ in 0..64 {
            f.app.save_current_file();
        }
        assert!(f.app.deferred_file_saves.is_empty());
        f.app.wait_for_background(Duration::from_secs(10), |app| {
            app.pending_file_saves.is_empty()
        });
        assert_eq!(
            f.bytes(),
            traffic,
            "preparing a clean buffer must not issue Save, Inspect, formatter or LSP requests"
        );
        assert!(!f.app.editor().unwrap().modified);
        assert_eq!(f.app.editor().unwrap().encoding, encoding);
        assert_eq!(f.app.editor().unwrap().file_size, bytes.len() as u64);
        assert!(f.app.editor().unwrap().saved_save_options.is_some());
        assert!(!f.app.formatter_settings.active);
        let before_jobs = f.app.background.activity.job_count();
        for _ in 0..128 {
            f.app.save_current_file();
        }
        assert_eq!(f.bytes(), traffic);
        assert_eq!(
            f.app.background.activity.job_count(),
            before_jobs,
            "repeated clean saves do not even start a local job"
        );
        assert_eq!(
            std::fs::read(f.root.path().join("file.txt")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::metadata(f.root.path().join("file.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
    }
}

#[test]
fn rapid_ctrl_s_during_a_stalled_reply_keeps_one_write_and_clean_repeats_stay_silent() {
    let mut f = Saving::new("saved\n");
    f.app.editor_mut().unwrap().insert_char('A');
    f.hold.store(true, Ordering::Release);
    f.app.save_current_file();
    f.wait_blocked();
    let sent = f.bytes().0;
    for _ in 0..128 {
        f.app.save_current_file();
    }
    assert_eq!(f.app.pending_file_saves.len(), 1);
    assert!(f.app.deferred_file_saves.is_empty());
    assert_eq!(f.bytes().0, sent);
    f.finish();
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(),
        "Asaved\n"
    );
    let traffic = f.bytes();
    for _ in 0..128 {
        f.app.save_current_file();
    }
    assert!(f.app.pending_file_saves.is_empty() && f.app.deferred_file_saves.is_empty());
    assert_eq!(f.bytes(), traffic);
    assert!(!f.app.editor().unwrap().modified);
}

#[test]
fn shared_panes_join_the_same_pending_save_and_reuse_the_completed_clean_checkpoint() {
    let mut f = Saving::new("saved\n");
    let mut second = Editor::new();
    second.copy_document_from(f.app.editor().unwrap());
    f.app.tabs.push(Tab::new_file(second));
    f.app.create_split_group_dir(1, SplitDirection::Horizontal);
    f.app.editor_mut().unwrap().insert_char('A');
    f.hold.store(true, Ordering::Release);
    f.app.save_current_file();
    f.wait_blocked();
    f.app.switch_group(0);
    for _ in 0..64 { f.app.save_current_file(); }
    assert!(f.app.deferred_file_saves.is_empty());
    f.finish();
    let traffic = f.bytes();
    let jobs = f.app.background.activity.job_count();
    for group in [1, 0, 1] {
        f.app.switch_group(group);
        for _ in 0..64 { f.app.save_current_file(); }
        assert_eq!(f.app.editor().unwrap().rope, "Asaved\n");
        assert!(!f.app.editor().unwrap().modified);
    }
    assert!(f.app.pending_file_saves.is_empty());
    assert_eq!(f.app.background.activity.job_count(), jobs);
    assert_eq!(f.bytes(), traffic);
    assert_eq!(std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(), "Asaved\n");
}

#[test]
fn pending_save_keeps_latest_text_or_undo_even_when_undo_is_temporarily_clean() {
    for action in ["type", "undo", "type_then_undo", "encoding"] {
        let mut f = Saving::new("saved\n");
        f.app.editor_mut().unwrap().insert_char('A');
        f.hold.store(true, Ordering::Release);
        f.app.save_current_file();
        f.wait_blocked();
        match action {
            "type" => f.app.editor_mut().unwrap().insert_char('B'),
            "undo" => {
                f.app.editor_mut().unwrap().undo();
                assert!(!f.app.editor().unwrap().modified);
            }
            "type_then_undo" => {
                f.app.editor_mut().unwrap().insert_char('B');
                f.app.save_current_file();
                f.app.editor_mut().unwrap().undo();
            }
            "encoding" => f.app.editor_mut().unwrap().set_encoding("UTF-16 LE"),
            _ => unreachable!(),
        }
        f.app.save_current_file();
        for _ in 0..32 {
            f.app.save_current_file();
        }
        assert_eq!(f.app.deferred_file_saves.len(), 1, "{action}");
        f.finish();
        let expected = match action {
            "type" => "ABsaved\n",
            "undo" => "saved\n",
            _ => "Asaved\n",
        };
        let loaded = crate::editor::load_text_file(&f.root.path().join("file.txt")).unwrap();
        assert_eq!(loaded.rope, expected, "{action}");
        assert_eq!(f.app.editor().unwrap().rope, expected, "{action}");
        assert!(!f.app.editor().unwrap().modified, "{action}");
        assert_eq!(
            loaded.encoding,
            f.app.editor().unwrap().encoding,
            "{action}"
        );
        let traffic = f.bytes();
        for _ in 0..32 {
            f.app.save_current_file();
        }
        assert_eq!(
            f.bytes(),
            traffic,
            "{action}: latest completion is the new clean checkpoint"
        );
    }
}

#[test]
fn changing_save_options_back_to_the_pending_intent_cancels_an_obsolete_deferred_save() {
    let mut f = Saving::new("saved  ");
    f.app.editor_mut().unwrap().insert_char('A');
    f.hold.store(true, Ordering::Release);
    f.app.save_current_file();
    f.wait_blocked();
    f.app.trim_trailing_on_save = true;
    f.app.insert_final_newline = true;
    for _ in 0..32 {
        f.app.save_current_file();
    }
    assert_eq!(f.app.deferred_file_saves.len(), 1);
    f.app.trim_trailing_on_save = false;
    f.app.insert_final_newline = false;
    f.app.save_current_file();
    assert!(
        f.app.deferred_file_saves.is_empty(),
        "the latest explicit Save supersedes the old cleanup request"
    );
    f.finish();
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(),
        "Asaved  "
    );
    assert!(!f.app.editor().unwrap().modified);
}

#[test]
fn new_cleanup_options_on_a_clean_remote_file_still_save_and_preserve_undo() {
    let mut f = Saving::new("saved  ");
    f.app.save_current_file();
    f.finish();
    assert!(!f.app.editor().unwrap().modified);
    f.app.trim_trailing_on_save = true;
    f.app.insert_final_newline = true;
    f.app.save_current_file();
    f.finish();
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(),
        "saved\n"
    );
    assert!(!f.app.editor().unwrap().modified);
    f.app.editor_mut().unwrap().undo();
    assert_eq!(f.app.editor().unwrap().rope, "saved  ");
    assert!(f.app.editor().unwrap().modified);
    f.app.editor_mut().unwrap().redo();
    assert_eq!(f.app.editor().unwrap().rope, "saved\n");
    assert!(!f.app.editor().unwrap().modified);
}

#[test]
fn clean_save_never_overwrites_an_unobserved_external_write_and_dirty_save_still_conflicts() {
    let mut f = Saving::new("saved\n");
    f.app.save_current_file();
    f.finish();
    std::fs::write(f.root.path().join("file.txt"), "external\n").unwrap();
    let traffic = f.bytes();
    for _ in 0..32 {
        f.app.save_current_file();
    }
    assert_eq!(f.bytes(), traffic);
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(),
        "external\n"
    );
    // A native notification/focus check, not repeated Save requests, handles
    // external changes. A deliberate edit still validates the disk revision.
    f.app.editor_mut().unwrap().insert_char('A');
    f.app.save_current_file();
    f.finish();
    assert!(f.app.save_conflict.is_some());
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(),
        "external\n"
    );
    assert!(f.app.editor().unwrap().modified);
    f.app.dismiss_save_error();
    f.app.editor_mut().unwrap().undo();
    f.app.queue_remote_file_change(f.path.clone());
    f.app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().unwrap().rope == "external\n"
    });
    assert!(!f.app.editor().unwrap().modified);
}

#[test]
fn disk_events_received_during_local_noop_preparation_are_rechecked_after_completion() {
    for deleted in [false, true] {
        let mut f = Saving::new("saved\n");
        let request = Arc::new(f.app.editor().unwrap().background_save_request(false, false, false).unwrap());
        let checkpoint = f.app.editor().unwrap().remote_checkpoint.clone().unwrap();
        let modified = f.app.editor().unwrap().last_disk_modified;
        f.app.pending_file_saves.insert(f.path.clone(), (f.app.workspace_generation, crate::editor::SaveIntent::new(request.clone())));
        if deleted { std::fs::remove_file(f.root.path().join("file.txt")).unwrap(); }
        else { std::fs::write(f.root.path().join("file.txt"), "external\n").unwrap(); }
        f.app.background.sender.send(BackgroundResult::FileDiskState(WorkspacePathChange {
            path: f.path.clone(), observed_at: Instant::now(), exists: !deleted,
            modified: None, len: None,
        })).unwrap();
        f.app.poll_background_results();
        assert!(f.app.remote_changes_deferred.contains(&f.path));
        let generation = f.app.workspace_generation;
        f.app.background.sender.send(BackgroundResult::RemoteFileResult { checkpoint,
            result: Box::new(BackgroundResult::FileSaved {
                source_document_id: request.source_document_id, workspace_generation: generation,
                path: f.path.clone(), error: None, last_disk_modified: modified,
                file_size: Some(6), source_content_version: request.source_content_version,
                source_rope: request.rope.clone(), previous_disk: request.expected_disk.clone(),
                saved_encoding: request.encoding, transformed_lines: None, disk_written: false,
                saved_rope: Some(request.rope.clone()),
            }),
        }).unwrap();
        f.app.poll_background_results();
        f.app.wait_for_background(Duration::from_secs(10), |app| {
            if deleted { app.tabs.iter().all(|tab| tab.editor.file_path.as_ref() != Some(&f.path)) }
            else { app.editor().unwrap().rope == "external\n" }
        });
        assert_eq!(f.root.path().join("file.txt").exists(), !deleted);
        if !deleted { assert!(!f.app.editor().unwrap().modified); }
    }
}
