use super::*;
use crate::app::App;
use crate::editor::Editor;
use crate::terminal::{TermGrid, TerminalEmulator};
use crossterm::event::{KeyCode, KeyModifiers};

#[test]
fn ssh_selects_the_client_clipboard_instead_of_a_remote_desktop_clipboard() {
    for variables in [
        vec!["SSH_CONNECTION"],
        vec!["SSH_CLIENT"],
        vec!["SSH_TTY"],
        vec!["SSH_CONNECTION", "DISPLAY", "WAYLAND_DISPLAY"],
    ] {
        assert!(remote_session(|name| variables.contains(&name)));
    }
    for variables in [
        vec![],
        vec!["DISPLAY"],
        vec!["WAYLAND_DISPLAY"],
        vec!["WSL_DISTRO_NAME"],
    ] {
        assert!(!remote_session(|name| variables.contains(&name)));
    }
}

#[test]
fn terminal_copy_uses_standard_osc52_and_encodes_control_characters() {
    let mut bytes = Vec::new();
    write_osc52(&mut bytes, "remote").unwrap();
    assert_eq!(bytes, b"\x1b]52;c;cmVtb3Rl\x07");
    for text in ["", "世界 👋\r\nsecond line", "\x1b]52;c;injected\x07"] {
        bytes.clear();
        write_osc52(&mut bytes, text).unwrap();
        let mut grid = TermGrid::new(20, 3);
        let mut parser = vte::Parser::new();
        for byte in &bytes {
            parser.advance(&mut grid, *byte);
        }
        assert_eq!(grid.clipboard_update.as_deref(), Some(text));
        assert!(grid.cells.iter().flatten().all(|cell| cell.ch == ' '));
    }
}

#[test]
fn remote_copy_and_cut_paste_between_files_share_current_text_and_undo() {
    let _clipboard = test_terminal_clipboard();
    let text = "copied 世界 👋";
    set_text("old Windows copy").unwrap();
    for cut in [false, true] {
        let mut source = Editor::test_with_text(&format!("{text}\nremaining"));
        source.selection_anchor = Some((0, 0));
        source.cursor_col = text.chars().count();
        if cut {
            source.cut();
        } else {
            source.copy();
        }
        assert_eq!(get_text().unwrap(), text);
        assert!(!source.clipboard_failed);

        let mut target = Editor::test_with_text("target: ");
        target.cursor_col = 8;
        target.clipboard = "stale tab clipboard".into();
        target.paste();
        assert_eq!(target.rope.to_string(), format!("target: {text}"));
        target.undo();
        assert_eq!(target.rope, "target: ");
        target.redo();
        assert_eq!(target.rope.to_string(), format!("target: {text}"));
        if cut {
            assert_eq!(source.rope, "\nremaining");
            source.undo();
            assert_eq!(source.rope.to_string(), format!("{text}\nremaining"));
        }
    }
}

#[test]
fn remote_line_copy_and_external_paste_keep_clipboard_semantics_in_sync() {
    let _clipboard = test_terminal_clipboard();
    let mut source = Editor::test_with_text("copied α\r\n");
    source.copy();
    let mut target = Editor::test_with_text("target");
    target.cursor_col = 3;
    target.paste();
    assert_eq!(target.rope, "copied α\ntarget");
    target.undo();

    let before = test_terminal_output();
    receive_terminal_paste("new Windows text");
    assert_eq!(
        test_terminal_output(),
        before,
        "pasting must not echo a clipboard write"
    );
    assert_eq!(get_text().unwrap(), "new Windows text");
    target.paste();
    assert_eq!(target.rope, "tarnew Windows textget");
    target.undo();
    assert_eq!(target.rope, "target");
}

#[test]
fn remote_settings_fields_and_editor_share_the_same_clipboard() {
    let _clipboard = test_terminal_clipboard();
    let mut app = App::test_new();
    app.open_settings_tab();
    app.settings_editor_search = "theme".into();
    app.settings_editor_search_cursor = 5;
    app.settings_editor_search_selection_anchor = Some(0);
    assert!(app.handle_focused_text_input_shortcut(KeyCode::Char('c'), KeyModifiers::CONTROL));
    let mut editor = Editor::test_with_text("");
    editor.paste();
    assert_eq!(editor.rope, "theme");
    receive_terminal_paste("font");
    assert!(app.handle_focused_text_input_shortcut(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(app.settings_editor_search, "font");
}

#[test]
fn fragmented_nested_terminal_copy_updates_the_host_clipboard_once() {
    for (text, chunk_size) in [
        ("remote α\r\n世界 👋".to_string(), 1),
        ("large λ\n".repeat(20_000), 257),
    ] {
        let _clipboard = test_terminal_clipboard();
        set_text("old Windows copy").unwrap();
        let before = test_terminal_output().len();
        let mut wire = Vec::new();
        write_osc52(&mut wire, &text).unwrap();
        let mut app = App::test_new();
        let mut terminal = TerminalEmulator::test_with_output(40, 10, "");
        let _alive = terminal.test_keep_running();
        let input = terminal.test_capture_input();
        app.terminals[0].terminal_pty = Some(terminal);
        let chunks: Vec<_> = wire.chunks(chunk_size).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            app.terminals[0]
                .terminal_pty
                .as_ref()
                .unwrap()
                .test_queue_output(chunk);
            while app.terminals[0]
                .terminal_pty
                .as_ref()
                .unwrap()
                .has_pending_output()
            {
                app.process_terminals();
            }
            if i + 1 < chunks.len() {
                assert_eq!(
                    test_terminal_output().len(),
                    before,
                    "partial OSC must not copy"
                );
            }
        }
        assert!(
            get_text().unwrap() == text,
            "fragmented copy lost ({} bytes)",
            text.len()
        );
        assert!(
            test_terminal_output()[before..] == wire,
            "forwarded clipboard request differs"
        );
        assert!(
            input.lock().unwrap().is_empty(),
            "copy must not send text or commands to the remote shell"
        );
        let before = test_terminal_output();
        app.process_terminals();
        assert_eq!(
            test_terminal_output(),
            before,
            "a copy request is consumed once"
        );
    }
}

#[test]
fn nested_terminal_queries_and_invalid_copies_leave_clipboard_unchanged() {
    let _clipboard = test_terminal_clipboard();
    set_text("keep current clipboard").unwrap();
    let before = test_terminal_output();
    for sequence in [
        "\x1b]52;c;?\x07",
        "\x1b]52;c;not!base64\x07",
        "\x1b]52;c;/w==\x07",
        "\x1b]52;p;bm8=\x07",
        "\x1b]52;invalid;bm8=\x07",
        "\x1b]52;c;bm8=;extra\x07",
    ] {
        let mut app = App::test_new();
        let mut terminal = TerminalEmulator::test_with_output(40, 10, sequence);
        let _alive = terminal.test_keep_running();
        let input = terminal.test_capture_input();
        app.terminals[0].terminal_pty = Some(terminal);
        app.process_terminals();
        assert_eq!(get_text().unwrap(), "keep current clipboard");
        assert_eq!(test_terminal_output(), before);
        assert!(
            input.lock().unwrap().is_empty(),
            "queries must not expose clipboard data"
        );
    }
}

#[test]
fn terminal_copy_supports_st_terminators_and_survives_a_screen_reset() {
    for selector in ["", "c", "s", "pc"] {
        let sequence = format!("\x1b]52;{selector};bmV3\x1b\\\x1bc");
        let terminal = TerminalEmulator::test_with_output(20, 3, &sequence);
        assert_eq!(terminal.grid.clipboard_update.as_deref(), Some("new"));
    }
}

#[test]
fn empty_remote_clipboard_does_not_restore_stale_text_or_delete_selection() {
    let _clipboard = test_terminal_clipboard();
    assert!(matches!(
        get_text(),
        Err(arboard::Error::ContentNotAvailable)
    ));
    set_text("old Windows text").unwrap();
    set_text("").unwrap();
    let mut editor = Editor::test_with_text("keep selected text");
    editor.selection_anchor = Some((0, 0));
    editor.cursor_col = 4;
    editor.clipboard = "old tab text".into();
    editor.paste();
    assert_eq!(editor.rope, "keep selected text");
    assert_eq!(editor.selection_anchor, Some((0, 0)));
    assert!(!editor.can_undo());
}

#[test]
fn oversized_terminal_copies_are_rejected_without_a_partial_request() {
    let mut wire = Vec::new();
    assert!(write_osc52(&mut wire, &"x".repeat(MAX_TERMINAL_CLIPBOARD_BYTES + 1)).is_err());
    assert!(wire.is_empty());
    let encoded = vec![b'A'; MAX_TERMINAL_CLIPBOARD_BYTES.div_ceil(3) * 4 + 1];
    assert!(decode_osc52(&[b"52", b"c", &encoded]).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn ssh_clipboard_uses_the_actual_terminal_without_a_desktop_clipboard() {
    use std::io::Read;
    use std::time::{Duration, Instant};

    const PROBE: &str = "VTCODE_SSH_CLIPBOARD_TEST";
    const TEXT: &str = "remote clipboard 世界 👋\nsecond line";
    if std::env::var_os(PROBE).is_some() {
        assert!(io::stdout().is_terminal());
        assert!(uses_terminal_clipboard());
        set_text(TEXT).unwrap();
        assert_eq!(get_text().unwrap(), TEXT);
        receive_terminal_paste("new Windows text");
        assert_eq!(get_text().unwrap(), "new Windows text");
        return;
    }

    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
    command.args([
        "--exact", "clipboard::remote_tests::ssh_clipboard_uses_the_actual_terminal_without_a_desktop_clipboard", "--nocapture",
    ]);
    command.env(PROBE, "1");
    command.env("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22");
    command.env("TERM", "xterm-256color");
    command.env("DISPLAY", ":invalid");
    command.env("WAYLAND_DISPLAY", "vtcode-missing-display");
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 || tx.send(bytes[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(20)) {
            output.extend(bytes);
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("SSH clipboard test timed out");
        }
    };
    reader.join().unwrap();
    output.extend(rx.try_iter().flatten());
    assert!(status.success(), "{}", String::from_utf8_lossy(&output));
    let mut expected = Vec::new();
    write_osc52(&mut expected, TEXT).unwrap();
    assert_eq!(
        output
            .windows(expected.len())
            .filter(|bytes| *bytes == expected)
            .count(),
        1
    );
    // Receiving a client paste must not send another clipboard-write request.
    assert_eq!(
        output
            .windows(7)
            .filter(|bytes| *bytes == b"\x1b]52;c;")
            .count(),
        1
    );
}
