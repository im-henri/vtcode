use crate::app::{App, SplitDirection, TabKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Workspace {
    directory: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.path().join(name);
        std::fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
        App::open_target_path(&path)
    }

    fn app(&self) -> App {
        App::new_deferred(self.path().to_path_buf())
    }

    fn reopen(&self) -> App {
        let mut app = self.app();
        let session = crate::session::load_session(self.path()).expect("saved workspace session");
        app.restore_session(&session);
        wait_for_files(&mut app);
        app
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Every fixture owns a unique workspace key; never remove shared state.
        let state = crate::portable::workspace_dir(self.path());
        assert!(state.starts_with(crate::portable::data_dir().join("workspaces")));
        let _ = std::fs::remove_dir_all(state);
    }
}

fn wait_for_files(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
            .all(|tab| !tab.editor.bg_loading)
    });
}

fn open_files(app: &mut App, paths: &[PathBuf]) {
    for path in paths {
        app.open_file(path);
    }
    wait_for_files(app);
}

#[track_caller]
fn assert_tabs(app: &App, expected: &[PathBuf]) {
    assert_eq!(
        app.tabs
            .iter()
            .map(|tab| tab.editor.file_path.clone().expect("file tab"))
            .collect::<Vec<_>>(),
        expected,
    );
}

#[test]
fn workspace_switch_restores_saved_directories_and_saves_outgoing_tabs() {
    let a = Workspace::new();
    let b = Workspace::new();
    let c = Workspace::new();
    let a_files = [a.file("a-one.txt"), a.file("a-two.txt")];
    let b_files = [
        b.file("b-one.txt"),
        b.file("b-two.txt"),
        b.file("b-three.txt"),
    ];
    let c_files = [c.file("c-one.txt")];
    {
        let mut app = a.app();
        open_files(&mut app, &a_files);
        app.active_tab = 0;
        app.tabs[0].pinned = true;
        app.tabs[0].editor.set_cursor(1, 4);
        app.explorer_width = 43;
        app.sc_visibility.untracked = false;
        app.show_panel = true;
        app.panel_height = 17;
        app.panel_tab = 3;
        app.save_session();
    }
    {
        let mut app = b.app();
        open_files(&mut app, &b_files);
        app.active_tab = 1;
        app.explorer_width = 51;
        app.show_minimap = false;
        app.save_session();
    }

    let mut app = c.app();
    open_files(&mut app, &c_files);
    app.explorer_width = 62;
    app.change_workspace(a.path());
    wait_for_files(&mut app);
    assert_tabs(&app, &a_files);
    assert_eq!(app.active_tab, 0);
    assert!(app.tabs[0].pinned);
    assert_eq!(
        (
            app.tabs[0].editor.cursor_line,
            app.tabs[0].editor.cursor_col
        ),
        (1, 4)
    );
    assert_eq!(app.explorer_width, 43);
    assert!(!app.sc_visibility.untracked);
    assert!(app.show_panel);
    assert_eq!((app.panel_height, app.panel_tab), (17, 3));

    let saved_c = c.reopen();
    assert_tabs(&saved_c, &c_files);
    assert_eq!(saved_c.explorer_width, 62);
    drop(saved_c);

    app.change_workspace(b.path());
    wait_for_files(&mut app);
    assert_tabs(&app, &b_files);
    assert_eq!(app.active_tab, 1);
    assert_eq!(app.explorer_width, 51);
    assert_eq!(app.sc_visibility, Default::default());
    assert!(!app.show_minimap);
    assert!(!app.show_panel);

    app.change_workspace(c.path());
    wait_for_files(&mut app);
    assert_tabs(&app, &c_files);
    assert_eq!(app.explorer_width, 62);
    assert_eq!(app.sc_visibility, Default::default());
    app.change_workspace(a.path());
    wait_for_files(&mut app);
    assert_tabs(&app, &a_files);
    assert_eq!(app.active_tab, 0);
    assert_eq!(app.explorer_width, 43);
    assert!(!app.sc_visibility.untracked);
}

#[test]
fn workspace_switch_clears_old_splits_for_empty_and_new_directories() {
    let split_workspace = Workspace::new();
    let saved_empty = Workspace::new();
    let new_workspace = Workspace::new();
    let files = [
        split_workspace.file("one.txt"),
        split_workspace.file("two.txt"),
        split_workspace.file("three.txt"),
    ];
    {
        let mut empty = saved_empty.app();
        empty.explorer_width = 48;
        empty.save_session();
    }
    let mut app = split_workspace.app();
    open_files(&mut app, &files);
    app.create_split_group_dir(2, SplitDirection::Vertical);
    assert_eq!(app.groups.len(), 2);
    assert_eq!(app.active_group, 1);
    assert_tabs(&app, &files[2..]);

    app.change_workspace(saved_empty.path());
    assert!(app.tabs.is_empty());
    assert!(app.groups.is_empty());
    assert!(app.split_tree.is_none());
    assert!(app.editor().is_none());
    assert_eq!(app.explorer_width, 48);

    app.change_workspace(split_workspace.path());
    wait_for_files(&mut app);
    assert_eq!(app.groups.len(), 2);
    assert_eq!(app.active_group, 1);
    assert!(app.split_tree.is_some());
    assert_tabs(&app, &files[2..]);
    app.switch_group(0);
    assert_tabs(&app, &files[..2]);

    app.change_workspace(new_workspace.path());
    assert!(app.tabs.is_empty());
    assert!(app.groups.is_empty());
    assert!(app.split_tree.is_none());
    assert_eq!(app.active_tab, 0);
    assert_eq!(app.active_group, 0);
    assert!(app.tab_mru_order.is_empty());

    let restored = split_workspace.reopen();
    assert_eq!(restored.groups.len(), 2);
    assert_eq!(restored.active_group, 0);
    assert_tabs(&restored, &files[..2]);
}

#[test]
fn workspace_switch_preserves_unsaved_edits_and_undo_after_reopening() {
    let a = Workspace::new();
    let b = Workspace::new();
    let path = a.file("edited.txt");
    let original = std::fs::read_to_string(&path).unwrap();
    let mut app = a.app();
    open_files(&mut app, std::slice::from_ref(&path));
    app.tabs[0].editor.insert_char('!');
    let edited = format!("!{original}");
    assert_eq!(app.tabs[0].editor.rope.to_string(), edited);

    app.change_workspace(b.path());
    assert!(app.tabs.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    app.change_workspace(a.path());
    wait_for_files(&mut app);
    assert_tabs(&app, std::slice::from_ref(&path));
    assert_eq!(app.tabs[0].editor.rope.to_string(), edited);
    assert!(app.tabs[0].editor.modified);
    assert_eq!(app.tabs[0].editor.cursor_col, 1);
    app.change_workspace(b.path());
    drop(app);

    let mut reopened = a.reopen();
    assert_eq!(reopened.tabs[0].editor.rope.to_string(), edited);
    assert!(reopened.tabs[0].editor.modified);
    reopened.tabs[0].editor.undo();
    assert_eq!(reopened.tabs[0].editor.rope.to_string(), original);
    reopened.tabs[0].editor.redo();
    assert_eq!(reopened.tabs[0].editor.rope.to_string(), edited);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn rapid_workspace_switch_keeps_pending_recovery_for_shared_file_isolated() {
    let a = Workspace::new();
    let b = Workspace::new();
    let c = Workspace::new();
    let shared_file = a.file("shared.txt");
    let original = std::fs::read_to_string(&shared_file).unwrap();
    for (workspace, marker) in [(&a, 'A'), (&b, 'B')] {
        let mut app = workspace.app();
        open_files(&mut app, std::slice::from_ref(&shared_file));
        app.tabs[0].editor.insert_char(marker);
        app.save_session();
    }

    let mut app = c.app();
    app.change_workspace(a.path());
    assert!(app.tabs[0].editor.bg_loading);
    // Switch before applying the first worker's results. Both sessions open
    // the same path but have different recovery buffers for that file.
    app.change_workspace(b.path());
    assert!(app.tabs[0].editor.bg_loading);
    wait_for_files(&mut app);
    assert_tabs(&app, std::slice::from_ref(&shared_file));
    assert_eq!(app.tabs[0].editor.rope.to_string(), format!("B{original}"));

    app.change_workspace(a.path());
    wait_for_files(&mut app);
    assert_eq!(app.tabs[0].editor.rope.to_string(), format!("A{original}"));
    assert!(app.tabs[0].editor.modified);
    app.change_workspace(c.path());
    assert!(app.tabs.is_empty());
    drop(app);

    let reopened = a.reopen();
    assert_eq!(
        reopened.tabs[0].editor.rope.to_string(),
        format!("A{original}")
    );
}

#[test]
fn invalid_workspace_and_current_directory_leave_open_view_unchanged() {
    let workspace = Workspace::new();
    let path = workspace.file("current.txt");
    let mut app = workspace.app();
    open_files(&mut app, std::slice::from_ref(&path));
    app.tabs[0].editor.insert_char('!');
    app.explorer_width = 57;
    let before = app.build_session();

    app.change_workspace(&workspace.path().join("missing-directory"));
    assert_eq!(app.explorer.root, workspace.path());
    assert_eq!(app.build_session(), before);
    assert!(app
        .notification_history
        .iter()
        .any(|(message, _, _)| message.contains("Not a directory")));
    assert!(!crate::session::session_path(workspace.path()).exists());

    app.change_workspace(&workspace.path().join("."));
    assert_eq!(app.explorer.root, workspace.path());
    assert_eq!(app.build_session(), before);
    assert!(!app.tabs[0].editor.bg_loading);
    assert!(!crate::session::session_path(workspace.path()).exists());
}

#[test]
fn workspace_switch_keeps_current_view_when_session_save_fails() {
    let current = Workspace::new();
    let destination = Workspace::new();
    let path = current.file("unsaved.txt");
    let mut app = current.app();
    open_files(&mut app, std::slice::from_ref(&path));
    app.tabs[0].editor.insert_char('!');
    app.explorer_width = 57;
    let before = app.build_session();
    // A directory at the manifest path reliably blocks the final write on
    // every platform, without relying on host permissions or read-only bits.
    std::fs::create_dir_all(crate::session::session_path(current.path())).unwrap();

    app.change_workspace(destination.path());

    assert_eq!(app.explorer.root, current.path());
    assert_eq!(app.build_session(), before);
    assert!(app.tabs[0].editor.modified);
    assert!(app
        .notification_history
        .iter()
        .any(|(message, _, _)| message.contains("Could not save current workspace")));
    assert!(!crate::session::session_path(destination.path()).exists());
}

#[test]
fn workspace_switch_preserves_dirty_custom_editor_in_inactive_split() {
    let current = Workspace::new();
    let destination = Workspace::new();
    let paths = [current.file("custom.txt"), current.file("regular.txt")];
    let mut app = current.app();
    open_files(&mut app, &paths);
    app.tabs[0].kind = TabKind::ExtensionView;
    app.tabs[0].editor.insert_char('!');
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    assert_eq!(app.active_group, 1);
    let custom_content = app.groups[0].tabs[0].editor.rope.to_string();

    app.change_workspace(destination.path());

    assert_eq!(app.explorer.root, current.path());
    assert_eq!(app.groups.len(), 2);
    assert_eq!(app.active_group, 1);
    assert_tabs(&app, &paths[1..]);
    assert_eq!(app.groups[0].tabs[0].kind, TabKind::ExtensionView);
    assert!(app.groups[0].tabs[0].editor.modified);
    assert_eq!(
        app.groups[0].tabs[0].editor.rope.to_string(),
        custom_content
    );
    assert!(app
        .notification_history
        .iter()
        .any(|(message, _, _)| message.contains("Save custom editor changes")));
    assert!(!crate::session::session_path(destination.path()).exists());
}

#[test]
fn workspace_switch_dismisses_actions_targeting_previous_tabs() {
    let current = Workspace::new();
    let destination = Workspace::new();
    let current_file = current.file("current.txt");
    let destination_file = destination.file("destination.txt");
    {
        let mut saved = destination.app();
        open_files(&mut saved, std::slice::from_ref(&destination_file));
        saved.save_session();
    }
    let mut app = current.app();
    open_files(&mut app, std::slice::from_ref(&current_file));
    // These actions point at tab zero, which the new workspace will also use.
    app.confirm_close_active = true;
    app.confirm_close_tab = 0;
    app.save_as_active = true;
    app.save_as_query = current_file.display().to_string();
    app.tab_context_active = true;
    app.tab_context_tab = 0;
    app.vtcomplete_active = true;
    app.binary_open_prompt_skip_once = Some(current_file);

    app.change_workspace(destination.path());
    wait_for_files(&mut app);

    assert_tabs(&app, std::slice::from_ref(&destination_file));
    assert!(!app.confirm_close_active);
    assert!(!app.save_as_active);
    assert!(!app.tab_context_active);
    assert!(!app.vtcomplete_active);
    assert!(app.binary_open_prompt_skip_once.is_none());
}

#[test]
fn previous_workspace_save_completion_preserves_destination_recovery() {
    let a = Workspace::new();
    let b = Workspace::new();
    let shared = a.file("shared.txt");
    let original = std::fs::read_to_string(&shared).unwrap();
    {
        let mut saved = b.app();
        open_files(&mut saved, std::slice::from_ref(&shared));
        saved.tabs[0].editor.insert_char('B');
        saved.save_session();
    }
    let mut app = a.app();
    open_files(&mut app, std::slice::from_ref(&shared));
    let original_generation = app.workspace_generation;
    app.pending_file_saves
        .insert(shared.clone(), (original_generation, 0.into()));
    app.tabs[0].editor.insert_char('A');
    let deferred = app.tabs[0]
        .editor
        .background_save_request(false, false, false)
        .unwrap();
    app.queue_background_save_job(deferred);
    assert_eq!(app.deferred_file_saves[&shared].0, original_generation);

    app.change_workspace(b.path());
    wait_for_files(&mut app);
    let recovered = format!("B{original}");
    assert_eq!(app.tabs[0].editor.content_version, 0);
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileSaved {
            source_document_id: 0,
            previous_disk: None,
            workspace_generation: original_generation,
            path: shared.clone(),
            error: None,
            last_disk_modified: None,
            file_size: Some(original.len() as u64),
            source_content_version: 0,
            source_rope: ropey::Rope::from_str(&original),
            saved_encoding: "UTF-8",
            transformed_lines: Some((0, usize::MAX)),
            disk_written: true,
            saved_rope: Some(ropey::Rope::from_str(&original)),
        })
        .unwrap();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.deferred_file_saves.contains_key(&shared)
    });
    assert_eq!(app.tabs[0].editor.rope.to_string(), recovered);
    assert!(app.tabs[0].editor.modified);
    // The queued write still belongs to A even though it starts in B.
    assert_eq!(app.pending_file_saves[&shared].0, original_generation);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(app.tabs[0].editor.rope.to_string(), recovered);
    assert!(app.tabs[0].editor.modified);
}

#[test]
fn previous_workspace_load_results_do_not_replace_new_shared_file_view() {
    let a = Workspace::new();
    let b = Workspace::new();
    let shared = a.file("shared.txt");
    let mut app = a.app();
    let old_generation = app.workspace_generation;
    app.change_workspace(b.path());
    app.tabs.push(crate::app::Tab::new_file(
        crate::editor::Editor::new_loading(&shared, 0),
    ));
    for generation in [old_generation, app.workspace_generation] {
        app.background
            .sender
            .send(crate::background::BackgroundResult::FileLoaded {
                workspace_generation: generation,
                path: shared.clone(),
                rope: ropey::Rope::from_str("current content\n"),
                encoding: "UTF-8",
                file_size: 16,
                last_disk_modified: None,
                session_restores: Vec::new(),
                skip_open_prompt: true,
            })
            .unwrap();
        app.poll_background_results();
        assert_eq!(app.tabs[0].editor.bg_loading, generation == old_generation);
    }
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileReloaded {
            targets: app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs))
                .map(|tab| (tab.editor.navigation_id, tab.editor.content_version)).collect(),
            workspace_generation: old_generation,
            path: shared,
            result: Ok(crate::editor::LoadedTextFile {
            remote_checkpoint: None,
                rope: ropey::Rope::from_str("stale reload\n"),
                encoding: "UTF-8",
                file_size: 13,
                last_disk_modified: None,
            }),
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(app.tabs[0].editor.rope.to_string(), "current content\n");
}
