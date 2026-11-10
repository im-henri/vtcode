use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn setup_dialog() -> Fixture {
    let mut fixture = setup();
    fixture.app.active_pane = ActivePane::Editor;
    fixture
}

fn enter(app: &mut App) {
    crate::input_keyboard::handle_keyboard_event(
        app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
}

fn type_path(app: &mut App, path: &str) {
    assert!(app.paste_into_focused_text_input(path));
}

#[test]
fn remote_external_files_open_and_save_without_opening_a_different_workspace() {
    let mut fixture = setup_dialog();
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("external.log");
    std::fs::write(&path, "remote log\n").unwrap();
    let root = fixture.app.explorer.root.clone();
    let uri = url::Url::from_file_path(&path).unwrap();
    let key = fixture
        .app
        .remote_workspace
        .as_ref()
        .unwrap()
        .target_key(uri.as_str())
        .unwrap();
    fixture.app.execute_palette_command("open_file_path");
    assert!(fixture.app.file_open.remote);
    type_path(&mut fixture.app, &path.display().to_string());
    enter(&mut fixture.app);
    drain(&mut fixture);
    assert!(!fixture.app.file_open.active);
    let editor = fixture.app.editor().unwrap();
    assert_eq!(editor.file_path.as_ref(), Some(&key));
    assert_eq!(editor.rope.to_string(), "remote log\n");
    assert!(editor.remote_checkpoint.is_some());
    assert!(!fixture.app.explorer_contains_file_path(&key));
    assert_eq!(fixture.app.explorer.root, root);
    fixture.app.editor_mut().unwrap().insert_char('R');
    fixture.app.save_current_file();
    fixture
        .app
        .wait_for_background(Duration::from_secs(10), |app| {
            app.pending_file_saves.is_empty()
        });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Rremote log\n");
    assert_eq!(fixture.app.explorer.root, root);
}

#[test]
fn local_files_in_a_remote_workbench_open_save_and_save_as_on_the_local_device() {
    let mut fixture = setup_dialog();
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("local.log");
    std::fs::write(&path, "local data\n").unwrap();
    let root = fixture.app.explorer.root.clone();
    fixture.app.begin_open_file_path();
    fixture.app.switch_open_file_device();
    assert!(!fixture.app.file_open.remote);
    type_path(&mut fixture.app, &path.display().to_string());
    enter(&mut fixture.app);
    drain(&mut fixture);
    assert!(!fixture.app.file_open.active);
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&path)
    );
    assert!(fixture.app.editor().unwrap().remote_checkpoint.is_none());
    assert_eq!(
        fixture.app.editor().unwrap().rope.to_string(),
        "local data\n"
    );
    fixture.app.editor_mut().unwrap().insert_char('L');
    fixture.app.save_current_file();
    fixture
        .app
        .wait_for_background(Duration::from_secs(10), |app| {
            app.pending_file_saves.is_empty()
        });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Llocal data\n");
    assert!(!fixture.root.path().join("local.log").exists());
    fixture.app.execute_palette_command("save_as");
    assert_eq!(fixture.app.save_as_query, path.display().to_string());
    fixture.app.save_as_query = "local-copy.log".into();
    fixture.app.execute_save_as();
    fixture
        .app
        .wait_for_background(Duration::from_secs(10), |app| {
            app.pending_save_as.is_empty()
        });
    assert_eq!(
        std::fs::read_to_string(local.path().join("local-copy.log")).unwrap(),
        "Llocal data\n"
    );
    assert!(!fixture.root.path().join("local-copy.log").exists());
    assert_eq!(fixture.app.explorer.root, root);
}

#[test]
fn source_switching_preserves_drafts_and_the_rendered_source_is_explicit() {
    let mut fixture = setup_dialog();
    fixture.app.begin_open_file_path();
    type_path(&mut fixture.app, "/var/log/remote.log");
    for _ in 0..2 {
        crate::input_keyboard::handle_keyboard_event(
            &mut fixture.app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );
    }
    enter(&mut fixture.app);
    assert!(!fixture.app.file_open.remote);
    assert!(fixture.app.file_open.input.query.is_empty());
    type_path(&mut fixture.app, "local.log");
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    terminal
        .draw(|frame| crate::ui_modal::draw_active(frame, &mut fixture.app))
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("this device"));
    assert!(text.contains("Use Remote"));
    let switch = fixture
        .app
        .modal
        .buttons
        .iter()
        .find(|(_, index)| *index == 1)
        .unwrap()
        .0;
    crate::ui_modal::handle_mouse(
        &mut fixture.app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: switch.x,
            row: switch.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(fixture.app.file_open.remote);
    assert_eq!(fixture.app.file_open.input.query, "/var/log/remote.log");
    fixture.app.switch_open_file_device();
    assert_eq!(fixture.app.file_open.input.query, "local.log");
}

#[test]
fn switching_to_a_local_file_does_not_wait_for_a_stalled_remote_open_or_accept_its_late_reply() {
    let mut fixture = setup_dialog();
    file(&fixture, "remote.log", "remote data\n");
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("local.log");
    std::fs::write(&path, "local data\n").unwrap();
    fixture.app.begin_open_file_path();
    type_path(&mut fixture.app, "remote.log");
    fixture.hold.store(true, Ordering::Release);
    enter(&mut fixture.app);
    until(|| fixture.blocked.load(Ordering::Acquire));
    assert!(fixture.app.file_open.pending);
    fixture.app.switch_open_file_device();
    type_path(&mut fixture.app, &path.display().to_string());
    enter(&mut fixture.app);
    fixture
        .app
        .wait_for_background(Duration::from_secs(10), |app| {
            app.editor().is_some_and(|editor| {
                editor.file_path.as_ref() == Some(&path) && !editor.bg_loading
            })
        });
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&path)
    );
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    assert_eq!(
        fixture.app.editor().unwrap().rope.to_string(),
        "local data\n"
    );
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&path)
    );
}

#[test]
fn canceling_a_remote_path_open_keeps_the_existing_file_and_drops_the_reply() {
    let mut fixture = setup_dialog();
    file(&fixture, "remote.log", "remote data\n");
    fixture.app.begin_open_file_path();
    type_path(&mut fixture.app, "remote.log");
    fixture.hold.store(true, Ordering::Release);
    enter(&mut fixture.app);
    until(|| fixture.blocked.load(Ordering::Acquire));
    crate::ui_modal::handle_key(
        &mut fixture.app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    assert!(!fixture.app.file_open.active);
    assert_eq!(fixture.app.tabs.len(), 1);
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), "origin\n");
}

#[test]
fn background_editor_updates_do_not_cancel_the_file_path_prompt() {
    let mut fixture = setup_dialog();
    let target = file(&fixture, "remote.log", "remote data\n");
    fixture.app.begin_open_file_path();
    type_path(&mut fixture.app, "remote.log");
    fixture.hold.store(true, Ordering::Release);
    enter(&mut fixture.app);
    until(|| fixture.blocked.load(Ordering::Acquire));
    // A background load or formatter can update the underlying buffer while
    // the dialog holds focus. Its result must not silently cancel Open.
    fixture.app.editor_mut().unwrap().insert_char('B');
    fixture.app.cancel_stale_remote_location();
    assert!(fixture.app.file_open.pending);
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    assert!(!fixture.app.file_open.active);
    assert_eq!(
        fixture.app.editor().unwrap().file_path.as_ref(),
        Some(&target)
    );
    assert_eq!(
        fixture.app.editor().unwrap().rope.to_string(),
        "remote data\n"
    );
    assert_eq!(fixture.app.tabs[0].editor.rope.to_string(), "Borigin\n");
}
