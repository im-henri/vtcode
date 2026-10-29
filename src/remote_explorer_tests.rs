use super::*;
use crate::{app::App, background::BackgroundResult};
use ratatui::{Terminal, backend::TestBackend};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[test]
fn vscode_ordering_remote_locale_changes_apply_without_an_epoch_change() {
    let root = tempfile::tempdir().unwrap();
    for name in ["z.h", "õ.h", "system.h", "system_defines.h"] { std::fs::write(root.path().join(name), "needle").unwrap(); }
    let mut cache = None;
    let read_names = |cache: &mut _, locale| {
        let query = serde_json::from_value(serde_json::json!({
            "epoch": 1, "expanded": [], "sort": "Name", "find": null,
            "source_control": false, "sort_locale": locale
        })).unwrap();
        read(root.path(), cache, query, &crate::background::CancellationToken::new()).unwrap()
            .entries.into_iter().map(|row| row.name).collect::<Vec<_>>()
    };
    assert_eq!(read_names(&mut cache, "en-US"), ["õ.h", "system_defines.h", "system.h", "z.h"]);
    assert_eq!(read_names(&mut cache, "et-EE"), ["system_defines.h", "system.h", "z.h", "õ.h"]);
}

#[test]
fn vscode_ordering_remote_search_uses_native_names_instead_of_opaque_view_keys() {
    let fixture = Fixture::new();
    let mut results = crate::search_results::SearchResults::default();
    results.set_remote(Some(fixture.workspace.clone()));
    for name in ["z.h", "a/file10.h", "system.h", "a/file2.h", "system_defines.h", "a/deep/x.h"] {
        results.extend([crate::app::GlobalSearchResult {
            file_path: fixture.workspace.target_key(name).unwrap(), line_num: 1, line_text: "needle".into(),
            col_start: 0, col_end: 6, context_before: None, context_after: None,
        }]);
    }
    assert_eq!(results.iter().map(|row| fixture.workspace.remote_path(&row.file_path).unwrap().display())
        .collect::<Vec<_>>(), ["system_defines.h", "system.h", "z.h", "a/file2.h", "a/file10.h", "a/deep/x.h"]);
}

struct Fixture {
    root: tempfile::TempDir,
    app: App,
    workspace: workspace::Workspace,
    hold: Arc<AtomicBool>,
    blocked: Arc<AtomicBool>,
    sent: Arc<AtomicUsize>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in ["a/nested", "b", "c"] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
        }
        for name in [
            "a/nested/deep.txt",
            "a/first.txt",
            "b/second.txt",
            "c/third.txt",
            "last.txt",
        ] {
            std::fs::write(root.path().join(name), name).unwrap();
        }
        let hold = Arc::new(AtomicBool::new(false));
        let blocked = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicUsize::new(0));
        let workspace = workspace::Workspace::new(super::super::tests::connection_with_status(
            root.path(),
            hold.clone(),
            sent.clone(),
            blocked.clone(),
        ));
        let mut app = App::new_deferred(workspace.view_root.clone());
        app.remote_workspace = Some(workspace.clone());
        app.source_control_enabled = false;
        app.new_file();
        app.refresh_explorer_tree_async();
        let mut fixture = Self {
            root,
            app,
            workspace,
            hold,
            blocked,
            sent,
        };
        fixture.wait();
        fixture
    }
    fn wait(&mut self) {
        self.app
            .wait_for_background(Duration::from_secs(10), |app| {
                !app.bg_explorer_refresh_running && !app.bg_explorer_refresh_pending
            });
    }
    fn toggle(&mut self, name: &str) {
        let key = self.workspace.target_key(name).unwrap();
        self.app.explorer.selected = self.app.explorer.find_path(&key).expect(name);
        assert!(self.app.toggle_or_open_explorer_async().is_none());
    }
    fn names(&self) -> Vec<String> {
        self.app
            .explorer
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
    fn stall(&mut self) {
        self.hold.store(true, Ordering::Release);
        self.app.refresh_explorer_tree_async();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.blocked.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            self.app.poll_background_results();
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.hold.store(false, Ordering::Release);
        if let Some(remote) = &self.app.remote_workspace {
            remote.client.disconnect();
        }
        self.workspace.client.disconnect();
    }
}

#[test]
fn stalled_remote_loading_animates_locally_without_sending_requests_or_losing_the_tree() {
    let mut f = Fixture::new();
    f.stall();
    let names = f.names();
    let sent = f.sent.load(Ordering::Relaxed);
    let generation = f.app.explorer_refresh_generation;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut f.app)).unwrap();
    let mut now = Instant::now();
    for _ in 0..120 {
        let wait = f.app.loading.next_wait(true, now).expect("visible Explorer loading");
        now += wait;
        assert!(f.app.loading.advance(true, now));
        f.app.rendering_animation_frame = true;
        terminal.draw(|frame| crate::ui::draw(frame, &mut f.app)).unwrap();
        f.app.rendering_animation_frame = false;
    }
    assert_eq!(f.sent.load(Ordering::Relaxed), sent, "painting progress sends no remote requests");
    assert_eq!(f.app.explorer_refresh_generation, generation);
    assert_eq!(f.names(), names);
    f.hold.store(false, Ordering::Release);
    f.wait();
    terminal.draw(|frame| crate::ui::draw(frame, &mut f.app)).unwrap();
    assert_eq!(f.app.loading.next_wait(true, Instant::now()), None);
}

#[test]
fn remote_explorer_delete_key_confirms_and_removes_all_selected_files() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut f = Fixture::new();
    f.toggle("b");
    f.wait();
    let paths = [f.workspace.target_key("last.txt").unwrap(),
        f.workspace.target_key("b/second.txt").unwrap()];
    f.app.active_pane = crate::app::ActivePane::Explorer;
    f.app.set_explorer_single_selection(f.app.explorer.find_path(&paths[0]).unwrap());
    f.app.toggle_explorer_selection(f.app.explorer.find_path(&paths[1]).unwrap());
    assert!(!crate::input_keyboard::handle_keyboard_event(&mut f.app,
        KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)));
    assert_eq!(f.app.confirm_delete_targets, paths);
    assert!(f.root.path().join("last.txt").exists() && f.root.path().join("b/second.txt").exists());
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| crate::ui_modal::draw_active(frame, &mut f.app)).unwrap();
    let dialog: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(dialog.contains("Delete these 2 files or folders?"));
    assert!(dialog.contains("last.txt") && dialog.contains("b/second.txt"));
    assert!(!crate::input_keyboard::handle_keyboard_event(&mut f.app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    f.app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_filesystem_action_running && !app.bg_explorer_refresh_running && !app.bg_explorer_refresh_pending
    });
    assert!(!f.root.path().join("last.txt").exists());
    assert!(!f.root.path().join("b/second.txt").exists());
    assert!(f.root.path().join("c/third.txt").exists());
}

#[test]
fn stalled_explorer_collapse_and_rapid_expansion_keep_siblings_and_input_consistent() {
    let mut f = Fixture::new();
    f.toggle("a");
    f.wait();
    f.toggle("a/nested");
    f.wait();
    assert!(f.names().iter().any(|name| name == "deep.txt"));
    f.stall();
    f.toggle("a");
    assert_eq!(f.names(), ["a", "b", "c", "last.txt"]);
    f.toggle("b");
    f.toggle("c");
    f.toggle("b");
    for (index, entry) in f.app.explorer.entries.iter().enumerate() {
        assert_eq!(f.app.explorer.find_path(&entry.path), Some(index));
    }
    assert_eq!(f.names(), ["a", "b", "c", "last.txt"]);
    f.app.editor_mut().unwrap().insert_char('λ');
    f.app.editor_mut().unwrap().undo();
    f.app.editor_mut().unwrap().redo();
    assert_eq!(f.app.editor().unwrap().rope, "λ");
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut f.app))
        .unwrap();
    f.hold.store(false, Ordering::Release);
    f.wait();
    assert_eq!(f.names(), ["a", "b", "c", "third.txt", "last.txt"]);
    assert!(f.app.remote_explorer_error.is_none());
}

#[test]
fn disconnect_keeps_cached_tree_and_recovers_after_reconnect() {
    let mut f = Fixture::new();
    f.toggle("a");
    f.wait();
    let expected = f.names();
    f.app.editor_mut().unwrap().insert_char('x');
    f.stall();
    let mut loading_terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    loading_terminal.draw(|frame| crate::ui::draw(frame, &mut f.app)).unwrap();
    assert!(f.app.loading.active());
    f.toggle("b");
    assert!(
        f.app
            .explorer_expanding
            .contains(&f.workspace.target_key("b").unwrap())
    );
    f.workspace.client.disconnect();
    f.app.wait_for_background(Duration::from_secs(10), |app| {
        app.remote_connection.phase == crate::app_remote_connection::Phase::Stopped
    });
    assert_eq!(
        f.names(),
        expected,
        "A stalled reply is not an empty directory"
    );
    assert!(!f.app.bg_explorer_refresh_running);
    assert!(f.app.explorer_expanding.is_empty());
    assert!(f.app.remote_explorer_error.is_some());
    loading_terminal.draw(|frame| crate::ui::draw(frame, &mut f.app)).unwrap();
    assert!(!f.app.loading.active(), "offline cached folders are not presented as still loading");
    f.toggle("b");
    assert_eq!(f.names(), expected);
    let b = f.workspace.target_key("b").unwrap();
    assert!(!f.app.explorer.entries[f.app.explorer.find_path(&b).unwrap()].expanded);
    f.toggle("a");
    assert_eq!(f.names(), ["a", "b", "c", "last.txt"]);
    f.app.show_output();
    assert_eq!(f.app.panel_tab, 2);
    assert!(f.app.show_panel);
    let log_view = f.app.logs.editor().unwrap().navigation_id;
    f.hold.store(false, Ordering::Release);
    let next = f
        .workspace
        .resume_on(workspace::Workspace::new(super::super::tests::connection(
            f.root.path(),
            Arc::default(),
            Arc::default(),
        )))
        .unwrap();
    f.app.resume_remote_services(next);
    f.wait();
    assert_eq!(f.app.logs.editor().unwrap().navigation_id, log_view,
        "Reconnecting must keep the Output buffer");
    f.app.active_tab = f.app.tabs.iter().position(|tab| tab.kind == crate::app::TabKind::File).unwrap();
    assert_eq!(f.app.editor().unwrap().rope, "x");
    f.app.editor_mut().unwrap().undo();
    assert_eq!(f.app.editor().unwrap().rope, "");
    f.app.show_output();
    assert_eq!(f.app.logs.editor().unwrap().navigation_id, log_view);
    f.toggle("b");
    f.wait();
    assert_eq!(f.names(), ["a", "b", "second.txt", "c", "last.txt"]);
    assert!(f.app.remote_explorer_error.is_none());
}

#[test]
fn failed_or_stale_explorer_replies_cannot_erase_tree_or_clear_another_active_request() {
    let mut f = Fixture::new();
    let expected = f.names();
    f.app.explorer.selected = 2;
    f.app.explorer.scroll_offset = 1;
    f.app.explorer_refresh_generation = 100;
    f.app.explorer_refresh_active_generation = Some(100);
    f.app.bg_explorer_refresh_running = true;
    for generation in [99, 100] {
        f.app
            .background
            .sender
            .send(BackgroundResult::ExplorerRefreshFailed {
                generation,
                error: "SSH pipe interrupted".into(),
            })
            .unwrap();
        f.app.poll_background_results();
        assert_eq!(f.names(), expected);
        assert_eq!(f.app.explorer.selected, 2);
        assert_eq!(f.app.explorer.scroll_offset, 1);
        assert_eq!(f.app.bg_explorer_refresh_running, generation == 99);
    }
    assert!(
        f.app
            .remote_explorer_error
            .as_deref()
            .unwrap()
            .contains("SSH pipe interrupted")
    );
    f.app.refresh_explorer_tree_async();
    f.wait();
    assert_eq!(f.names(), expected);
    assert!(f.app.remote_explorer_error.is_none());
}

#[test]
fn collapsing_all_removes_descendants_before_any_network_response() {
    let mut f = Fixture::new();
    f.toggle("a");
    f.wait();
    f.toggle("a/nested");
    f.wait();
    f.toggle("b");
    f.wait();
    f.stall();
    let selected = f.workspace.target_key("b/second.txt").unwrap();
    f.app.explorer.selected = f.app.explorer.find_path(&selected).unwrap();
    f.app.collapse_all_explorer_async();
    assert_eq!(f.names(), ["a", "b", "c", "last.txt"]);
    assert_eq!(f.app.explorer.entries[f.app.explorer.selected].name, "b");
    assert!(f.app.explorer.entries.iter().all(|entry| !entry.expanded));
    f.hold.store(false, Ordering::Release);
    f.wait();
    assert_eq!(f.names(), ["a", "b", "c", "last.txt"]);
}

#[test]
fn cached_filtered_folders_can_still_expand_locally_while_disconnected() {
    let mut f = Fixture::new();
    f.app.open_explorer_find();
    f.app.explorer.find.filter = true;
    f.app.explorer.find.query = "txt".into();
    f.app.update_explorer_find();
    f.wait();
    assert!(f.app.explorer.find.results.filtered_tree.is_some());
    f.app.collapse_all_explorer_async();
    f.workspace.client.disconnect();
    f.app.poll_background_results();
    f.toggle("a");
    assert!(f.names().iter().any(|name| name == "first.txt"));
    assert!(!f.app.bg_explorer_refresh_running);
    assert!(f.app.explorer_expanding.is_empty());
}
