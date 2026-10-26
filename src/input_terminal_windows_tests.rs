//! Exercise the complete keyboard route through actual nested ConPTY sessions.

use super::*;
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::time::{Duration, Instant};

use crate::terminal::TerminalEmulator;
use crate::windows_input::ConsoleInputMode;
use crossterm::event::{self, Event, MouseButton, MouseEvent, MouseEventKind};

const CHILD_PREFIX: &str = "input_terminal::windows_tests::";
const READY: &str = "NESTED_INPUT_READY";
const RETURNED: &str = "NESTED_INPUT_RETURNED";
const PAYLOAD: &str = "if (true) {\r\n\tTere 🦀\r\n}\n";

fn keys() -> Vec<KeyEvent> {
    let ctrl = KeyModifiers::CONTROL;
    let shift = KeyModifiers::SHIFT;
    let alt = KeyModifiers::ALT;
    let mut keys = vec![
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('A'), shift),
        KeyEvent::new(KeyCode::Char('s'), ctrl),
        KeyEvent::new(KeyCode::Char('P'), ctrl | shift),
        KeyEvent::new(KeyCode::Char('C'), ctrl | shift),
        KeyEvent::new(KeyCode::Char('v'), ctrl),
        KeyEvent::new(KeyCode::Insert, ctrl),
        KeyEvent::new(KeyCode::Char('p'), alt),
        KeyEvent::new(KeyCode::Enter, shift),
        KeyEvent::new(KeyCode::Enter, ctrl | shift),
        KeyEvent::new(KeyCode::BackTab, ctrl | shift),
        KeyEvent::new(KeyCode::Left, ctrl | shift),
        KeyEvent::new(KeyCode::PageUp, shift),
        KeyEvent::new(KeyCode::Delete, ctrl),
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::F(5), shift),
        KeyEvent::new(KeyCode::Backspace, ctrl),
        KeyEvent::new(KeyCode::Char('õ'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('🦀'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('@'), ctrl | alt),
        KeyEvent::new(KeyCode::Char('q'), ctrl),
        KeyEvent::new(KeyCode::Tab, ctrl),
        KeyEvent::new(KeyCode::PageDown, ctrl),
        KeyEvent::new(KeyCode::Left, alt),
        KeyEvent::new(KeyCode::Right, alt),
    ];
    keys.extend(('A'..='Z').map(|letter| KeyEvent::new(KeyCode::Char(letter), ctrl | shift)));
    keys
}

fn next_event() -> Event {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        assert!(
            event::poll(deadline.saturating_duration_since(Instant::now())).unwrap(),
            "nested input timed out"
        );
        let event = event::read().unwrap();
        if !matches!(event, Event::Resize(..))
            && !matches!(event, Event::Key(key) if key.kind == event::KeyEventKind::Release)
        {
            return event;
        }
    }
    panic!("nested input timed out");
}

fn next_key() -> KeyEvent {
    match next_event() {
        Event::Key(key) => key,
        other => panic!("expected key, received {other:?}"),
    }
}

fn mouse_events() -> [MouseEvent; 3] {
    [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ]
    .map(|kind| MouseEvent {
        kind,
        column: 3,
        row: 2,
        modifiers: KeyModifiers::CONTROL,
    })
}

fn acknowledge(marker: &str) {
    println!("{marker}");
    io::stdout().flush().unwrap();
}

fn console_snapshot() -> (u32, u32, u32, u32) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{GetConsoleCP, GetConsoleMode, GetConsoleOutputCP};
    let mut modes = [0; 2];
    for (name, mode) in ["CONIN$", "CONOUT$"].into_iter().zip(&mut modes) {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap();
        assert_ne!(unsafe { GetConsoleMode(file.as_raw_handle(), mode) }, 0);
    }
    (modes[0], modes[1], unsafe { GetConsoleCP() }, unsafe {
        GetConsoleOutputCP()
    })
}

fn check_parent_input() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, INPUT_RECORD, KEY_EVENT, SetConsoleMode,
        WriteConsoleInputW,
    };
    let input = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .unwrap();
    let records: Vec<INPUT_RECORD> = "\x1b[80;0;16;1;24;1_"
        .encode_utf16()
        .map(|unit| {
            let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
            record.EventType = KEY_EVENT as u16;
            record.Event.KeyEvent.bKeyDown = 1;
            record.Event.KeyEvent.wRepeatCount = 1;
            record.Event.KeyEvent.uChar.UnicodeChar = unit;
            record
        })
        .collect();
    // Inject the transport bytes as raw records, as OpenConsole does. The
    // public injection API would otherwise encode our Win32 packet a second time.
    let mut input_mode = 0;
    assert_ne!(
        unsafe { GetConsoleMode(input.as_raw_handle(), &mut input_mode) },
        0
    );
    assert_ne!(
        unsafe {
            SetConsoleMode(
                input.as_raw_handle(),
                input_mode & !ENABLE_VIRTUAL_TERMINAL_INPUT,
            )
        },
        0
    );
    let mut written = 0;
    assert_ne!(
        unsafe {
            WriteConsoleInputW(
                input.as_raw_handle(),
                records.as_ptr(),
                records.len() as u32,
                &mut written,
            )
        },
        0
    );
    assert_ne!(
        unsafe { SetConsoleMode(input.as_raw_handle(), input_mode) },
        0
    );
    assert_eq!(written as usize, records.len());
    assert_eq!(
        next_key(),
        KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )
    );
}

fn console() -> ConsoleInputMode {
    let mode = ConsoleInputMode::save().unwrap();
    crossterm::terminal::enable_raw_mode().unwrap();
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        event::EnableMouseCapture,
        event::EnableBracketedPaste,
        event::EnableFocusChange
    )
    .unwrap();
    mode.enable_virtual_terminal_input().unwrap();
    mode
}

fn finish_console(mode: ConsoleInputMode) {
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        event::DisableMouseCapture,
        event::DisableBracketedPaste,
        event::DisableFocusChange
    )
    .unwrap();
    crossterm::terminal::disable_raw_mode().unwrap();
    drop(mode);
    println!("{RETURNED}");
    io::stdout().flush().unwrap();
}

fn child_app(fixture: &str) -> App {
    let mut app = App::test_new();
    app.tabs = vec![crate::app::Tab::new_file(crate::editor::Editor::new())];
    app.active_pane = crate::app::ActivePane::Editor;
    let args = vec![
        "--ignored".into(),
        "--exact".into(),
        format!("{CHILD_PREFIX}{fixture}"),
        "--nocapture".into(),
    ];
    app.terminals[0].terminal_pty = Some(
        TerminalEmulator::spawn_command_in_dir(
            std::env::current_exe().unwrap().to_str().unwrap(),
            &args,
            100,
            24,
            &std::env::current_dir().unwrap(),
        )
        .unwrap(),
    );
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    app.terminal_surfaces
        .push(crate::app_terminal_selection::TerminalSurface {
            index: 0,
            area: ratatui::layout::Rect::new(0, 0, 100, 24),
            group: None,
        });
    wait_for(&mut app, READY);
    app
}

fn wait_for(app: &mut App, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pty = app.terminals[0].terminal_pty.as_mut().unwrap();
        pty.process();
        let screen: String = pty
            .grid
            .cells
            .iter()
            .flat_map(|row| row.iter().map(|cell| cell.ch))
            .collect();
        if screen.contains(marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "did not receive {marker}: {screen}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn forward(app: &mut App, index: usize, key: KeyEvent) {
    assert!(
        !crate::input_keyboard::handle_keyboard_event(app, key),
        "parent consumed {key:?} as quit"
    );
    wait_for(app, &format!("NESTED_KEY_{index}_OK"));
}

fn close_child(app: &mut App) {
    crate::input_keyboard::handle_keyboard_event(
        app,
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
    );
    wait_for(app, RETURNED);
    let deadline = Instant::now() + Duration::from_secs(5);
    let pty = app.terminals[0].terminal_pty.as_mut().unwrap();
    while !pty.take_exit_event() {
        pty.process();
        assert!(Instant::now() < deadline, "nested process did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    pty.process();
    assert!(
        !pty.grid.is_alt_screen(),
        "nested process left its alternate screen enabled"
    );
}

#[test]
#[ignore = "subprocess fixture with its own console"]
fn nested_key_reader() {
    assert_eq!(std::env::var("TERM_PROGRAM").unwrap(), "vtcode");
    let mode = console();
    println!("{READY}");
    io::stdout().flush().unwrap();
    for (index, expected) in keys().into_iter().enumerate() {
        assert_eq!(next_key(), expected, "key {index}");
        println!("NESTED_KEY_{index}_OK");
        io::stdout().flush().unwrap();
    }
    for (index, expected) in mouse_events().into_iter().enumerate() {
        assert_eq!(next_event(), Event::Mouse(expected), "mouse {index}");
        acknowledge(&format!("NESTED_MOUSE_{index}_OK"));
    }
    assert_eq!(next_event(), Event::FocusLost);
    acknowledge("NESTED_FOCUS_LOST_OK");
    assert_eq!(next_event(), Event::FocusGained);
    acknowledge("NESTED_FOCUS_GAINED_OK");
    assert_eq!(next_event(), Event::Paste(PAYLOAD.into()));
    acknowledge("NESTED_PASTE_OK");
    assert_eq!(
        next_key(),
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE)
    );
    finish_console(mode);
}

#[test]
#[ignore = "subprocess fixture with a second nested terminal"]
fn nested_key_relay() {
    let mode = console();
    let mut app = child_app("nested_key_reader");
    println!("{READY}");
    io::stdout().flush().unwrap();
    for index in 0..keys().len() {
        let key = next_key();
        forward(&mut app, index, key);
        println!("NESTED_KEY_{index}_OK");
        io::stdout().flush().unwrap();
    }
    for index in 0..mouse_events().len() {
        let Event::Mouse(mouse) = next_event() else {
            panic!("expected mouse event")
        };
        crate::mouse::handle_mouse(&mut app, mouse);
        let marker = format!("NESTED_MOUSE_{index}_OK");
        wait_for(&mut app, &marker);
        acknowledge(&marker);
    }
    for (expected, focused, marker) in [
        (Event::FocusLost, false, "NESTED_FOCUS_LOST_OK"),
        (Event::FocusGained, true, "NESTED_FOCUS_GAINED_OK"),
    ] {
        assert_eq!(next_event(), expected);
        app.forward_terminal_focus(focused);
        wait_for(&mut app, marker);
        acknowledge(marker);
    }
    assert_eq!(next_event(), Event::Paste(PAYLOAD.into()));
    app.paste_terminal_text(0, PAYLOAD);
    wait_for(&mut app, "NESTED_PASTE_OK");
    acknowledge("NESTED_PASTE_OK");
    assert_eq!(
        next_key(),
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE)
    );
    close_child(&mut app);
    drop(app);
    finish_console(mode);
}

#[test]
fn nested_vtcodes_preserve_input() {
    const PROBE: &str = "VTCODE_NESTED_INPUT_TEST";
    if std::env::var_os(PROBE).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{CHILD_PREFIX}nested_vtcodes_preserve_input"),
                "--nocapture",
            ])
            .env(PROBE, "1")
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW, never the developer's console.
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let original = console_snapshot();
    let mode = console();
    let active = console_snapshot();
    check_parent_input();
    for fixture in ["nested_key_reader", "nested_key_relay"] {
        let mut app = child_app(fixture);
        for (index, key) in keys().into_iter().enumerate() {
            forward(&mut app, index, key);
        }
        for (index, mouse) in mouse_events().into_iter().enumerate() {
            crate::mouse::handle_mouse(&mut app, mouse);
            wait_for(&mut app, &format!("NESTED_MOUSE_{index}_OK"));
        }
        app.forward_terminal_focus(false);
        wait_for(&mut app, "NESTED_FOCUS_LOST_OK");
        app.forward_terminal_focus(true);
        wait_for(&mut app, "NESTED_FOCUS_GAINED_OK");
        app.paste_terminal_text(0, PAYLOAD);
        wait_for(&mut app, "NESTED_PASTE_OK");
        close_child(&mut app);
        assert_eq!(
            console_snapshot(),
            active,
            "nested console changed its parent's modes"
        );
        check_parent_input();
        // Exercise the parent's editor after closing its nested terminal.
        drop(app.terminals[0].terminal_pty.take());
        app.terminal_panel_focused = false;
        assert!(!crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)
        ));
        assert_eq!(app.editor().unwrap().rope.to_string(), "z");
    }
    finish_console(mode);
    assert_eq!(console_snapshot(), original);
}
