use super::*;
use crate::{app::Tab, editor::Editor, text_input::TextInputId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> (tempfile::TempDir, App) {
    let temp = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.remote_targets.path = Some(temp.path().join("remote-targets.json"));
    app.tabs = vec![Tab::new_file(Editor::test_with_text("local unsaved work"))];
    app.open_remote_sidebar();
    (temp, app)
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}
fn draw(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
fn add(app: &mut App, name: &str, host: &str) {
    app.edit_remote_target(None, false);
    assert!(app.paste_into_focused_text_input(name));
    key(app, KeyCode::Tab);
    assert!(app.paste_into_focused_text_input(host));
    app.save_remote_target(false);
    assert!(app.remote_targets.form.is_none());
}

#[test]
fn keyboard_form_creates_edits_and_reloads_a_target_without_touching_local_buffers() {
    let (_temp, mut app) = fixture();
    let screen = draw(&mut app, 120, 35);
    assert!(screen.contains("No remote targets yet."));
    key(&mut app, KeyCode::Char('n'));
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::RemoteTarget(0))
    );
    assert!(app.paste_into_focused_text_input("Development"));
    key(&mut app, KeyCode::Tab);
    assert!(app.paste_into_focused_text_input("user@dev"));
    key(&mut app, KeyCode::Tab);
    assert!(app.paste_into_focused_text_input("/work/project λ"));
    key(&mut app, KeyCode::Enter);
    assert!(app.remote_targets.form.is_none());
    app.reload_remote_targets();
    assert_eq!(
        app.remote_targets.store.as_ref().unwrap().targets[0].folder,
        "/work/project λ"
    );
    key(&mut app, KeyCode::F(2));
    app.edit_text_input(
        TextInputId::RemoteTarget(0),
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        None,
    );
    assert!(app.paste_into_focused_text_input("Renamed"));
    app.save_remote_target(false);
    assert_eq!(
        app.remote_targets.store.as_ref().unwrap().targets[0].name,
        "Renamed"
    );
    assert_eq!(app.tabs[0].editor.rope.to_string(), "local unsaved work");
}

#[test]
fn validation_and_concurrent_changes_keep_the_form_and_saved_data_intact() {
    let (_temp, mut app) = fixture();
    add(&mut app, "Development", "dev");
    app.edit_remote_target(None, false);
    app.remote_targets.form.as_mut().unwrap().advanced = true;
    app.focus_text_input(TextInputId::RemoteTarget(4));
    draw(&mut app, 60, 18);
    app.save_remote_target(false);
    assert!(app.remote_targets.form.as_ref().unwrap().error.is_some());
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::RemoteTarget(0))
    );
    let screen = draw(&mut app, 60, 18);
    assert!(app.text_input_area(TextInputId::RemoteTarget(0)).is_some());
    assert!(
        screen.contains("Target name"),
        "Validation must reveal the field label as well as its input"
    );
    key(&mut app, KeyCode::Esc);
    app.edit_remote_target(Some(0), false);
    let path = app.remote_targets.path.clone().unwrap();
    let mut other = Store::load(&path).unwrap();
    other.save(vec![]).unwrap();
    app.save_remote_target(false);
    assert!(
        app.remote_targets
            .form
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("changed in another window")
    );
    assert!(Store::load(&path).unwrap().targets.is_empty());
}

#[test]
fn search_duplicate_and_confirmed_removal_work_with_keyboard_and_persist() {
    let (_temp, mut app) = fixture();
    add(&mut app, "Development", "dev");
    add(&mut app, "Production", "prod");
    app.focus_text_input(TextInputId::RemoteFilter);
    assert!(app.paste_into_focused_text_input("prod"));
    assert_eq!(app.remote_targets.indices().len(), 1);
    assert_eq!(
        app.remote_targets
            .target(app.remote_targets.selected_index().unwrap())
            .unwrap()
            .name,
        "Production"
    );
    key(&mut app, KeyCode::Esc);
    app.edit_remote_target(Some(0), true);
    app.save_remote_target(false);
    assert_eq!(app.remote_targets.store.as_ref().unwrap().targets.len(), 3);
    let index = app.remote_targets.selected_index().unwrap();
    crate::ui_remote_targets::activate(&mut app, Action::Remove(index));
    key(&mut app, KeyCode::Enter); // Cancel is the safe default.
    assert_eq!(app.remote_targets.store.as_ref().unwrap().targets.len(), 3);
    crate::ui_remote_targets::activate(&mut app, Action::Remove(index));
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    app.reload_remote_targets();
    assert_eq!(app.remote_targets.store.as_ref().unwrap().targets.len(), 2);
}

#[test]
fn remote_sidebar_and_forms_fit_small_viewports_and_restore_from_the_session() {
    let (_temp, mut app) = fixture();
    add(&mut app, "Development", "dev");
    let session = app.build_session();
    assert_eq!(session.sidebar_view.as_deref(), Some("remote"));
    for (width, height) in [(140, 40), (80, 24), (40, 12), (16, 5), (3, 3), (1, 1)] {
        draw(&mut app, width, height);
        app.edit_remote_target(Some(0), false);
        app.remote_targets.form.as_mut().unwrap().advanced = true;
        draw(&mut app, width, height);
        key(&mut app, KeyCode::Esc);
        assert!(app.remote_targets.form.is_none());
    }
}

fn spin(app: &mut App, ready: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.service_workspaces_until(
            crate::ui_work::Service::Workers,
            Instant::now() + std::time::Duration::from_millis(20),
            || true,
        );
        app.poll_workspace_requests();
        if ready(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for remote target (attempt={}, opening={}, modal={:?}, status={:?}): {:?}\n{}",
            app.remote_targets.attempt.is_some(),
            app.workspace_opening(),
            crate::ui_modal::active(app),
            app.status_message,
            app.remote_targets
                .attempt
                .as_ref()
                .and_then(|attempt| attempt.error.as_ref()),
            app.remote_targets
                .attempt
                .as_ref()
                .map(|attempt| attempt
                    .setup
                    .as_ref()
                    .map(|setup| setup.diagnostics().report())
                    .unwrap_or_else(|| "SSH is still starting".into()))
                .unwrap_or_default()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn peer_target(root: &std::path::Path, authenticate: bool) -> Target {
    let executable = std::env::current_exe().unwrap();
    Target {
        name: "Fixture".into(),
        host: if authenticate {
            "fixture-auth"
        } else {
            "fixture"
        }
        .into(),
        remote_command: root.to_str().unwrap().into(),
        ssh_command: shell_words::join([
            executable.to_str().unwrap(),
            "--ignored",
            "--exact",
            "app_remote_targets::tests::headless_target_peer",
            "--nocapture",
            "--",
        ]),
        ..Default::default()
    }
}

fn preview_login(app: &mut App) -> u64 {
    let id = 7;
    let mut attempt = Attempt {
        id,
        target: Target {
            name: "Development".into(),
            host: "henri@devbox".into(),
            ..Default::default()
        },
        starting: None,
        setup: None,
        grid: crate::terminal::TermGrid::new(80, 24),
        rows: Default::default(),
        parser: vte::Parser::new(),
        error: None,
        input_sent: false,
        input_error: None,
        focus: 0,
        pending_output: None,
    };
    for &byte in b"Connecting to devbox...\r\nhenri@devbox's password: " {
        attempt.parser.advance(&mut attempt.grid, byte);
    }
    app.remote_targets.attempt = Some(attempt);
    app.open_remote_connection_tab(id);
    id
}

#[test]
fn remote_target_login_is_a_full_editor_tab_and_split_views_share_its_lifetime() {
    let (_temp, mut app) = fixture();
    let id = preview_login(&mut app);
    assert_eq!(app.tabs[app.active_tab].kind, TabKind::RemoteConnection(id));
    assert!(crate::ui_modal::active(&app).is_none());
    let screen = draw(&mut app, 120, 35);
    assert!(screen.contains("password:"));
    assert!(screen.contains("Passwords and passphrases are hidden"));
    assert!(app.remote_targets.connection_area.width > 70);
    assert!(app.remote_targets.connection_area.height > 25);
    assert_eq!(app.tabs[0].editor.rope, "local unsaved work");
    assert_eq!(
        app.build_session().tabs.len(),
        1,
        "Sign-in tabs are ephemeral"
    );
    let peer = app.clone_desk_tab(&app.tabs[app.active_tab]);
    app.tabs.push(peer);
    app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
    draw(&mut app, 120, 35);
    app.close_tab(app.active_tab);
    assert!(
        app.remote_targets.attempt.is_some(),
        "A second view still owns this sign-in"
    );
    app.open_remote_connection_tab(id);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
    );
    assert!(
        app.remote_targets.attempt.is_none(),
        "Closing the last view cancels the attempt"
    );
    assert_eq!(app.tabs[0].editor.rope, "local unsaved work");
}

#[test]
fn remote_target_typing_reaches_the_private_console_and_passwords_stay_hidden() {
    let (temp, mut app) = fixture();
    app.connect_remote_target(peer_target(temp.path(), true));
    spin(&mut app, |app| {
        app.remote_targets.attempt.as_ref().is_some_and(|attempt| {
            attempt
                .grid
                .cells
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect::<String>()
                .contains("Password:")
        })
    });
    draw(&mut app, 120, 35);
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
    app.macro_recording = true;
    app.screencast_mode = true;
    for ch in "fixture-secret".chars() {
        crate::app_event_loop::handle_keyboard_input(
            &mut app,
            &mut pump,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    assert!(app.macro_buffer.is_empty());
    assert!(app.screencast_keys.is_empty());
    app.macro_recording = false;
    app.screencast_mode = false;
    let attempt = app.remote_targets.attempt.as_ref().unwrap();
    assert!(attempt.input_sent);
    assert!(attempt.input_error.is_none());
    let screen = draw(&mut app, 120, 35);
    assert!(screen.contains("Input sent to SSH"));
    assert!(!screen.contains("fixture-secret"));
    assert!(
        !app.remote_targets
            .attempt
            .as_ref()
            .unwrap()
            .setup
            .as_ref()
            .unwrap()
            .diagnostics()
            .report()
            .contains("fixture-secret")
    );
    key(&mut app, KeyCode::Enter);
    spin(&mut app, |app| app.remote_workspace.is_some());
    assert!(
        app.tabs
            .iter()
            .all(|tab| !matches!(tab.kind, TabKind::RemoteConnection(_)))
    );
    app.remote_workspace.as_ref().unwrap().disconnect();
}

#[test]
fn remote_target_sign_in_can_finish_in_a_parked_workspace_without_stealing_focus() {
    let (temp, mut app) = fixture();
    app.connect_remote_target(peer_target(temp.path(), true));
    spin(&mut app, |app| {
        app.remote_targets.attempt.as_ref().is_some_and(|attempt| {
            attempt
                .grid
                .cells
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect::<String>()
                .contains("Password:")
        })
    });
    crate::app_event_loop::handle_paste_input(&mut app, "fixture-secret\r");
    let mut other = App::test_new();
    other.tabs = vec![Tab::new_file(Editor::test_with_text("another workspace"))];
    app.insert_workspace(other);
    let visible = app.workspaces.active;
    spin(&mut app, |app| {
        app.workspaces
            .parked
            .iter()
            .any(|parked| parked.app.remote_workspace.is_some())
    });
    assert_eq!(app.workspaces.active, visible);
    assert_eq!(app.tabs[0].editor.rope, "another workspace");
    let origin = app
        .workspaces
        .parked
        .iter()
        .find(|parked| {
            parked
                .app
                .tabs
                .iter()
                .any(|tab| tab.editor.rope == "local unsaved work")
        })
        .unwrap();
    assert!(origin.app.remote_targets.attempt.is_none());
    assert!(
        origin
            .app
            .tabs
            .iter()
            .all(|tab| !matches!(tab.kind, TabKind::RemoteConnection(_)))
    );
    app.workspaces
        .parked
        .iter()
        .find_map(|parked| parked.app.remote_workspace.as_ref())
        .unwrap()
        .disconnect();
}

#[test]
fn remote_target_connect_authenticate_switch_and_reuse_preserve_the_local_workbench() {
    let (temp, mut app) = fixture();
    let root = temp.path().join("remote workspace λ");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("file.txt"), "remote content").unwrap();
    let target = peer_target(&root, true);
    app.connect_remote_target(target.clone());
    spin(&mut app, |app| {
        app.remote_targets.attempt.as_ref().is_some_and(|attempt| {
            attempt
                .grid
                .cells
                .iter()
                .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
                .collect::<String>()
                .contains("Password:")
        })
    });
    crate::app_event_loop::handle_paste_input(&mut app, "fixture-secret\r");
    spin(&mut app, |app| app.remote_workspace.is_some());
    let remote = app.remote_workspace.clone().unwrap();
    assert!(remote.client.connected());
    let local_id = app
        .workspaces
        .parked
        .iter()
        .find(|parked| parked.app.remote_workspace.is_none())
        .unwrap()
        .id;
    assert_eq!(
        app.workspaces
            .parked
            .iter()
            .find(|parked| parked.id == local_id)
            .unwrap()
            .app
            .tabs[0]
            .editor
            .rope
            .to_string(),
        "local unsaved work"
    );
    let remote_id = app.workspaces.active;
    app.connect_remote_target(target.clone());
    assert!(
        app.remote_targets.attempt.is_none(),
        "Reuse the existing connection"
    );
    app.poll_workspace_requests();
    assert_eq!(app.workspaces.active, remote_id);
    app.workspaces.request = Some(crate::app_workspaces::Request::Switch(local_id));
    app.poll_workspace_requests();
    assert_eq!(app.tabs[0].editor.rope.to_string(), "local unsaved work");
    app.connect_remote_target(target);
    app.poll_workspace_requests();
    assert_eq!(app.workspaces.active, remote_id);
    app.remote_targets.path = Some(temp.path().join("saved-current.json"));
    app.open_remote_sidebar();
    app.save_current_remote_target();
    let saved = app.remote_targets.form.as_ref().unwrap().target();
    assert!(
        remote
            .recipe
            .as_ref()
            .unwrap()
            .matches(&saved.launch().unwrap()),
        "Saving a direct connection preserves the exact launch recipe"
    );
    app.cancel_remote_target_dialog();
    app.workspaces.parked.clear(); // Exercise a window launched directly over SSH.
    app.execute_palette_command("remote_disconnect");
    spin(&mut app, |app| app.workspaces.exit.is_some());
    assert!(
        !app.workspaces.parked.is_empty(),
        "Closing the only remote folder prepares a local window"
    );
    assert!(app.exit_keep_unsaved_quit);
    app.start_exit_recovery();
    spin(&mut app, |app| app.exit_recovery.saved);
    app.shutdown_anim_active = true;
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while app.remote_workspace.is_some() {
        app.poll_background_results();
        app.advance_workspace_exit();
        assert!(
            Instant::now() < deadline,
            "Remote close did not return to the local window"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!app.shutdown_anim_active);
    remote.disconnect();
}

#[test]
fn remote_target_cancel_terminates_the_owned_authentication_process() {
    let (temp, mut app) = fixture();
    app.connect_remote_target(peer_target(temp.path(), true));
    spin(&mut app, |app| {
        app.remote_targets.attempt.as_ref().is_some_and(|attempt| {
            attempt
                .grid
                .cells
                .iter()
                .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
                .collect::<String>()
                .contains("Password:")
        })
    });
    let log = app
        .remote_targets
        .attempt
        .as_ref()
        .unwrap()
        .setup
        .as_ref()
        .unwrap()
        .diagnostics();
    key(&mut app, KeyCode::Esc);
    assert!(app.remote_targets.attempt.is_none());
    spin(&mut app, |_| log.report().contains("exit code"));
    assert!(log.report().contains("local cleanup requested"));
    assert!(app.remote_workspace.is_none());
    assert!(app.workspaces.request.is_none());
    assert_eq!(app.tabs[0].editor.rope.to_string(), "local unsaved work");
}

#[test]
fn remote_target_failed_launch_stays_actionable_and_can_be_edited() {
    let (_temp, mut app) = fixture();
    let target = Target {
        name: "Unavailable".into(),
        host: "host".into(),
        ssh_command: "vtcode-missing-ssh-test-program-7439".into(),
        ..Default::default()
    };
    app.remote_targets
        .store
        .as_mut()
        .unwrap()
        .save(vec![target.clone()])
        .unwrap();
    app.connect_remote_target(target);
    spin(&mut app, |app| {
        app.remote_targets
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.error.is_some())
    });
    let screen = draw(&mut app, 100, 30);
    assert!(
        screen.contains("Retry") && screen.contains("Edit Target") && screen.contains("Copy Log")
    );
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Enter);
    assert!(app.remote_targets.attempt.is_none());
    assert_eq!(
        app.remote_targets.form.as_ref().unwrap().fields[0].text,
        "Unavailable"
    );
}

#[test]
#[ignore = "Internal SSH/authentication fixture, launched by remote target tests"]
fn headless_target_peer() {
    use std::io::Write;
    let args: Vec<_> = std::env::args().collect();
    let host = &args[args.len() - 2];
    let root = std::path::Path::new(args.last().unwrap());
    if host == "fixture-auth" {
        #[cfg(windows)]
        {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .open("CONOUT$")
                .unwrap();
            output.write_all(b"Password:").unwrap();
            output.flush().unwrap();
        }
        #[cfg(not(windows))]
        {
            print!("Password:");
            std::io::stdout().flush().unwrap();
        }
        #[cfg(windows)]
        let password = {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Console::{INPUT_RECORD, KEY_EVENT, ReadConsoleInputW};
            // Windows OpenSSH reads the native private console, independently
            // of the child's inherited stdio handle and terminal input mode.
            let console = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("CONIN$")
                .unwrap();
            let mut units = Vec::new();
            loop {
                let mut record = INPUT_RECORD::default();
                let mut count = 0;
                assert_ne!(
                    unsafe {
                        ReadConsoleInputW(console.as_raw_handle(), &mut record, 1, &mut count)
                    },
                    0
                );
                if count == 0 || record.EventType != KEY_EVENT as u16 {
                    continue;
                }
                let key = unsafe { record.Event.KeyEvent };
                if key.bKeyDown == 0 {
                    continue;
                }
                let unit = unsafe { key.uChar.UnicodeChar };
                if matches!(unit, 10 | 13) {
                    break;
                }
                if unit != 0 {
                    units.extend(std::iter::repeat_n(unit, key.wRepeatCount.max(1) as usize));
                }
            }
            String::from_utf16(&units).unwrap().into_bytes()
        };
        #[cfg(not(windows))]
        let password = {
            use std::io::Read;
            let mut bytes = Vec::new();
            loop {
                let mut byte = [0];
                std::io::stdin().read_exact(&mut byte).unwrap();
                if matches!(byte[0], b'\r' | b'\n') {
                    break;
                }
                bytes.push(byte[0]);
            }
            bytes
        };
        assert_eq!(password, b"fixture-secret");
    }
    crate::remote::service::run(root).unwrap();
}

#[test]
fn remote_target_mouse_forms_and_list_scrolling_have_current_hit_regions() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (_temp, mut app) = fixture();
    for index in 0..20 {
        add(&mut app, &format!("Development {index:02}"), "dev");
    }
    draw(&mut app, 120, 30);
    let sidebar = app.explorer_area.unwrap();
    let mouse = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::ScrollDown, sidebar.x + 3, sidebar.y + 12),
    );
    assert!(app.remote_targets.scroll > 0);
    let scroll = app.remote_targets.scroll;
    draw(&mut app, 120, 30);
    assert_eq!(
        app.remote_targets.scroll, scroll,
        "Drawing must preserve wheel scrolling"
    );
    let add_button = app
        .remote_targets
        .hits
        .iter()
        .find(|(_, action)| *action == Action::Add)
        .unwrap()
        .0;
    crate::mouse::handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            add_button.x,
            add_button.y,
        ),
    );
    crate::mouse::handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            add_button.x,
            add_button.y,
        ),
    );
    draw(&mut app, 120, 30);
    let host = app.text_input_area(TextInputId::RemoteTarget(1)).unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), host.x + 2, host.y),
    );
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Up(MouseButton::Left), host.x + 2, host.y),
    );
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::RemoteTarget(1))
    );
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(
        app.remote_targets.form.as_ref().unwrap().fields[1].text,
        "x"
    );
    crate::ui_remote_targets::activate(&mut app, Action::Advanced);
    app.focus_text_input(TextInputId::RemoteTarget(4));
    let screen = draw(&mut app, 60, 18);
    assert!(screen.contains("SSH command"));
    assert!(app.text_input_area(TextInputId::RemoteTarget(4)).is_some());
    assert!(
        app.text_input_area(TextInputId::RemoteTarget(0)).is_none(),
        "Scrolled-out fields must release their pointer regions"
    );
    assert_eq!(app.tabs[0].editor.rope.to_string(), "local unsaved work");
}

#[test]
fn remote_target_right_click_and_middle_click_never_count_as_a_double_click() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (_temp, mut app) = fixture();
    add(&mut app, "Development", "dev");
    draw(&mut app, 120, 35);
    let area = app
        .remote_targets
        .hits
        .iter()
        .find(|(_, action)| *action == Action::Select(0))
        .unwrap()
        .0;
    let mouse = |button| MouseEvent {
        kind: MouseEventKind::Down(button),
        column: area.x + 2,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    };
    crate::mouse::handle_mouse(&mut app, mouse(MouseButton::Right));
    assert!(app.remote_targets.menu.is_some());
    key(&mut app, KeyCode::Esc);
    draw(&mut app, 120, 35);
    crate::mouse::handle_mouse(&mut app, mouse(MouseButton::Left));
    assert!(app.remote_targets.attempt.is_none());
    crate::mouse::handle_mouse(&mut app, mouse(MouseButton::Middle));
    assert!(app.remote_targets.attempt.is_none());
    assert!(app.remote_targets.last_click.is_none());
}

#[test]
fn remote_target_reload_keeps_last_good_targets_and_conflicts_offer_a_copy() {
    let (_temp, mut app) = fixture();
    add(&mut app, "Development", "dev");
    let path = app.remote_targets.path.clone().unwrap();
    app.edit_remote_target(Some(0), false);
    app.remote_targets.form.as_mut().unwrap().fields[2].text = "~/draft".into();
    let mut other = Store::load(&path).unwrap();
    let original = other.targets[0].clone();
    let mut external = original.clone();
    external.host = "other-host".into();
    other.upsert(Some(&original), external.clone()).unwrap();
    app.save_remote_target(false);
    assert!(app.remote_targets.form.as_ref().unwrap().conflict);
    let screen = draw(&mut app, 90, 26);
    assert!(screen.contains("Save a Copy"));
    app.save_remote_target_copy();
    assert!(app.remote_targets.form.is_none());
    let saved = Store::load(&path).unwrap();
    assert_eq!(saved.find("Development").unwrap(), &external);
    assert_eq!(saved.find("Development copy").unwrap().folder, "~/draft");
    std::fs::write(&path, b"bad external edit").unwrap();
    app.reload_remote_targets();
    assert!(app.remote_targets.error.is_some());
    assert_eq!(app.remote_targets.store.as_ref().unwrap().targets.len(), 2);
    assert!(!app.remote_targets.can_edit());
    let screen = draw(&mut app, 120, 35);
    assert!(screen.contains("Showing last saved targets"));
    assert!(screen.contains("Development"));
    assert_eq!(std::fs::read(&path).unwrap(), b"bad external edit");
}

#[test]
fn remote_target_login_focus_does_not_capture_editor_or_form_input() {
    let (_temp, mut app) = fixture();
    let id = preview_login(&mut app);
    app.active_tab = 0;
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.tabs[0].editor.rope, "xlocal unsaved work");
    assert!(!crate::ui_remote_targets::connection_has_focus(&app));
    app.edit_remote_target(None, false);
    app.paste_into_focused_text_input("A new target");
    assert_eq!(
        app.remote_targets.form.as_ref().unwrap().fields[0].text,
        "A new target"
    );
    key(&mut app, KeyCode::Esc);
    assert!(
        app.remote_targets.attempt.is_some(),
        "Canceling a form must not cancel background sign-in"
    );
    app.open_remote_connection_tab(id);
    assert!(crate::ui_remote_targets::connection_has_focus(&app));
    let wake = app.background.wake_handle();
    let generation = wake.generation();
    assert!(!app.poll_remote_target(Instant::now() - std::time::Duration::from_millis(1)));
    assert_eq!(
        wake.generation(),
        generation,
        "An idle sign-in must not continuously wake the UI"
    );
    for (width, height) in [(140, 40), (80, 24), (40, 12), (16, 5), (3, 3), (1, 1)] {
        draw(&mut app, width, height);
        assert!(
            app.remote_targets
                .connection_hits
                .iter()
                .all(|(area, _)| area.intersection(Rect::new(0, 0, width, height)) == *area)
        );
    }
}

#[test]
fn remote_target_ui_previews() {
    let (_temp, mut app) = fixture();
    app.show_minimap = false;
    app.explorer_width = 38;
    for (name, host) in [
        ("Development", "henri@devbox"),
        ("GPU Cluster", "login.cluster"),
        ("Staging", "staging"),
    ] {
        add(&mut app, name, host);
    }
    fn capture(app: &mut App, name: &str, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        if let Some(path) = std::env::var_os("VTCODE_REMOTE_UI_PREVIEW_DIR") {
            let path = std::path::PathBuf::from(path);
            std::fs::create_dir_all(&path).unwrap();
            let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
                "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg), "modifiers": format!("{:?}", cell.modifier)
            })).collect();
            std::fs::write(
                path.join(format!("{name}.json")),
                serde_json::to_vec(
                    &serde_json::json!({"width":width,"height":height,"cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }
    capture(&mut app, "remote-sidebar", 120, 35);
    app.edit_remote_target(Some(0), false);
    capture(&mut app, "remote-target-form", 120, 35);
    app.remote_targets.form.as_mut().unwrap().advanced = true;
    capture(&mut app, "remote-target-advanced", 120, 35);
    capture(&mut app, "remote-target-small", 60, 18);
    app.focus_text_input(TextInputId::RemoteTarget(4));
    capture(&mut app, "remote-target-small-advanced", 60, 18);
    app.remote_targets.form.as_mut().unwrap().fields[0]
        .text
        .clear();
    app.save_remote_target(false);
    capture(&mut app, "remote-target-validation", 120, 35);
    app.cancel_remote_target_dialog();
    preview_login(&mut app);
    capture(&mut app, "remote-target-sign-in", 120, 35);
    capture(&mut app, "remote-target-sign-in-small", 80, 24);
    app.remote_targets.attempt.as_mut().unwrap().error =
        Some("Permission denied. Check the SSH host and your credentials, then try again.".into());
    capture(&mut app, "remote-target-failed", 120, 35);
}
