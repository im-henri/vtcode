use super::*;
use ratatui::{Terminal, backend::TestBackend};

#[path = "ui_output_autoscroll_tests.rs"]
mod autoscroll_tests;

fn app(log: Log) -> App {
    let mut app = App::test_new();
    app.source_control_enabled = false;
    app.show_explorer = false;
    app.new_file();
    app.editor_mut().unwrap().insert_char('λ');
    app.logs.log = Some(log);
    app.show_output();
    app
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
fn input(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}
fn pointer(app: &mut App, kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers,
        },
    );
}
fn click(app: &mut App, area: Rect) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        pointer(app, kind, area.x, area.y, KeyModifiers::NONE);
    }
}
fn control(app: &App, control: Control) -> Rect {
    app.logs
        .controls
        .iter()
        .find(|(_, kind)| *kind == control)
        .unwrap()
        .0
}

#[test]
fn reconnect_notice_log_action_selects_ssh_output_and_retains_its_full_history() {
    let ssh = Log::default();
    ssh.event("Transport", "Connection lost fixture");
    let mut app = app(ssh.clone());
    app.logs.remote_log = Some(ssh);
    app.logs.select_source(Source::Git);
    app.remote_connection.phase = crate::app_remote_connection::Phase::Waiting;
    app.remote_connection.attempt = 4;
    app.publish_remote_connection_notice(true);
    let screen = draw(&mut app, 130, 35);
    assert!(screen.contains("attempt 5 next"));
    let card = app.side_notice.area.unwrap();
    let editor = app.editor().unwrap().rope.clone();
    click(&mut app, Rect::new(card.x + 3, card.y + 3, 3, 1));
    assert_eq!(app.logs.source, Source::RemoteSsh);
    assert!(app.show_panel && app.panel_tab == 2);
    assert_eq!(app.active_pane, ActivePane::Panel);
    assert!(!app.side_notice.visible());
    assert!(draw(&mut app, 130, 35).contains("Connection lost fixture"));
    assert_eq!(app.editor().unwrap().rope, editor);
    app.remote_connection.attempt = 5;
    app.publish_remote_connection_notice(false);
    assert!(!app.side_notice.visible(), "viewing the log dismisses this incident's notice");
}

#[test]
fn output_is_a_panel_and_preserves_the_open_editor_and_undo_history() {
    let mut app = app(Log::default());
    let id = app.editor().unwrap().navigation_id;
    assert!(
        app.menu_items(3)
            .iter()
            .any(|(label, id)| label == "Output" && *id == "show_output")
    );
    assert!(
        app.palette
            .commands
            .iter()
            .any(|command| command.id == "show_output" && command.name == "View: Output")
    );
    assert_eq!(app.tabs.len(), 1);
    assert!(app.show_panel && app.panel_tab == 2 && app.active_pane == ActivePane::Panel);
    let screen = draw(&mut app, 140, 40);
    assert!(
        screen.contains("Problems")
            && screen.contains("Output")
            && screen.contains("Notifications")
    );
    assert!(
        screen.contains("Remote SSH Connection") && screen.contains("No connection events yet")
    );
    assert_eq!(app.editor().unwrap().navigation_id, id);
    assert_eq!(app.editor().unwrap().rope, "λ");
    let output = app.logs.editor().unwrap().navigation_id;
    for _ in 0..3 {
        app.execute_palette_command("show_output");
    }
    assert_eq!(app.logs.editor().unwrap().navigation_id, output);
    assert_eq!(app.tabs.len(), 1);
    input(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert!(!app.show_panel && app.active_pane == ActivePane::Editor);
    assert!(!app.confirm_close_active);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "");
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, "λ");
}

#[test]
fn git_output_switches_by_mouse_and_keyboard_and_keeps_each_channels_view() {
    let ssh = Log::default();
    ssh.event("Transport", "SSH-only event");
    let mut app = app(ssh.clone());
    let git = app.git_output();
    git.event("Git", "Git-only event recorded while hidden");
    assert!(draw(&mut app, 140, 35).contains("SSH-only event"));
    let ssh_reader = app.logs.editor().unwrap().navigation_id;
    app.logs.reader().view.autoscroll = false;

    let source = control(&app, Control::Source);
    click(&mut app, source);
    let screen = draw(&mut app, 140, 35);
    assert!(screen.contains("Remote SSH Connection") && screen.contains("Git"));
    let mut second = app.logs.dropdown_area.unwrap();
    second.y += 1;
    second.height = 1;
    click(&mut app, second);
    assert_eq!(app.logs.source, Source::Git);
    let screen = draw(&mut app, 140, 35);
    assert!(screen.contains("Git-only event recorded while hidden"));
    assert!(!app.logs.editor().unwrap().rope.to_string().contains("SSH-only event"));
    let git_reader = app.logs.editor().unwrap().navigation_id;
    assert_ne!(git_reader, ssh_reader);
    app.logs.reader().view.autoscroll = false;
    app.logs.reader().editor.selection_anchor = Some((0, 0));

    let source = control(&app, Control::Source);
    click(&mut app, source);
    draw(&mut app, 140, 35);
    input(&mut app, KeyCode::Up, KeyModifiers::NONE);
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.logs.source, Source::RemoteSsh);
    assert!(draw(&mut app, 140, 35).contains("SSH-only event"));
    assert_eq!(app.logs.editor().unwrap().navigation_id, ssh_reader);
    assert!(!app.logs.reader().view.autoscroll);

    app.logs.select_source(Source::Git);
    draw(&mut app, 140, 35);
    assert_eq!(app.logs.editor().unwrap().navigation_id, git_reader);
    assert!(!app.logs.reader().view.autoscroll);
    assert_eq!(app.logs.editor().unwrap().selection_anchor, Some((0, 0)));
    action(&mut app, Control::Clear);
    assert_eq!(app.logs.editor().unwrap().rope.len_chars(), 0);
    app.logs.select_source(Source::RemoteSsh);
    assert!(draw(&mut app, 140, 35).contains("SSH-only event"));
    app.logs.select_source(Source::Git);
    git.event("Git", "After clear");
    draw(&mut app, 140, 35);
    let output = app.logs.editor().unwrap().rope.to_string();
    assert!(output.contains("After clear") && !output.contains("Git-only event"));
    assert_eq!(app.editor().unwrap().rope, "λ");
}

#[test]
fn only_the_visible_output_channel_subscribes_to_wakeups() {
    let ssh = Log::default();
    let mut app = app(ssh.clone());
    let git = app.git_output();
    draw(&mut app, 140, 35);
    let wake = app.background.wake_handle();
    let generation = wake.generation();
    git.event("Git", "Hidden Git activity");
    assert_eq!(wake.generation(), generation);

    app.logs.select_source(Source::Git);
    draw(&mut app, 140, 35);
    let generation = wake.generation();
    ssh.event("Transport", "Hidden SSH activity");
    assert_eq!(wake.generation(), generation);
    git.event("Git", "Visible Git activity");
    assert!(wake.generation() > generation);
    draw(&mut app, 140, 35);
    crate::ui_panels::close_panel(&mut app);
    let generation = wake.generation();
    git.event("Git", "Git continues while closed");
    ssh.event("Transport", "SSH continues while closed");
    assert_eq!(wake.generation(), generation);
    app.show_output();
    draw(&mut app, 140, 35);
    assert_eq!(app.logs.source, Source::Git);
    assert!(app.logs.editor().unwrap().rope.to_string().contains("Git continues while closed"));
    app.show_remote_diagnostics();
    draw(&mut app, 140, 35);
    assert_eq!(app.logs.source, Source::RemoteSsh);
    assert!(app.logs.editor().unwrap().rope.to_string().contains("SSH continues while closed"));
}

#[test]
fn output_focus_blocks_typing_paste_edit_commands_and_saving_the_file_behind_it() {
    let log = Log::default();
    log.event("Transport", "Connection reset λ🦀👩‍💻");
    let mut app = app(log);
    draw(&mut app, 130, 30);
    let output = app.logs.editor().unwrap().rope.clone();
    for code in [
        KeyCode::Char('x'),
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Enter,
    ] {
        input(&mut app, code, KeyModifiers::NONE);
    }
    input(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    for code in ['x', 'v', 'z', 'y', 's'] {
        input(&mut app, KeyCode::Char(code), KeyModifiers::CONTROL);
    }
    assert!(paste(&mut app, "must not touch either document"));
    for id in [
        "cut",
        "paste",
        "undo",
        "redo",
        "save",
        "save_as",
        "delete_line",
        "toggle_read_only",
    ] {
        app.execute_palette_command(id);
    }
    assert_eq!(app.editor().unwrap().rope, "λ");
    assert_eq!(app.logs.editor().unwrap().rope, output);
    assert!(
        app.logs.editor().unwrap().read_only && app.logs.editor().unwrap().undo_stack.is_empty()
    );
    assert!(app.logs.editor().unwrap().selected_text().is_some());
    assert!(!app.save_as_active && app.pending_file_saves.is_empty());
    assert!(app.is_menu_cmd_disabled("paste") && app.is_menu_cmd_disabled("undo"));
    assert!(!app.is_menu_cmd_disabled("copy") && !app.is_menu_cmd_disabled("select_all"));
    {
        let editor = app.editor_mut().unwrap();
        editor.language_override = Some("Rust".into());
        editor.select_all();
        editor.paste_plain_with_text("use z;\nuse a;\nfn sample() {}\n");
    }
    let source = app.editor().unwrap().rope.clone();
    for id in [
        "sort_imports",
        "reverse_lines",
        "toggle_block_comment",
        "eol_crlf",
    ] {
        app.execute_editor_command(id);
    }
    assert_eq!(app.editor().unwrap().rope, source);
}

#[test]
fn bracketed_paste_during_output_popups_cannot_fall_through_to_the_file() {
    for popup in ["none", "context", "menu", "palette"] {
        let mut app = app(Log::default());
        draw(&mut app, 140, 30);
        match popup {
            "context" => app.context_menu_active = true,
            "menu" => {
                app.menu_active = true;
                app.menu_open = true;
            }
            "palette" => app.open_palette(),
            _ => {}
        }
        crate::app_event_loop::handle_paste_input(&mut app, "pasted text");
        assert_eq!(app.editor().unwrap().rope, "λ", "{popup}");
        assert_eq!(app.logs.editor().unwrap().rope.len_chars(), 0, "{popup}");
        if popup == "palette" {
            assert!(app.palette.query.contains("pasted text"));
        }
    }
}

#[test]
fn header_controls_share_one_row_and_source_menu_uses_rendered_geometry() {
    let mut app = app(Log::default());
    for (width, height) in [(18, 12), (40, 15), (64, 20), (90, 24), (140, 30), (200, 40)] {
        draw(&mut app, width, height);
        let panel = app.panel_area.unwrap();
        let mut right = app.panel_tab_areas.last().unwrap().0.right();
        for (area, _) in &app.logs.controls {
            assert_eq!(area.height, 1);
            assert_eq!(area.y, panel.y);
            assert!(area.x >= right && area.right() <= app.panel_chrome.maximize.x);
            right = area.right();
        }
        assert_eq!(app.logs.reader.as_ref().unwrap().area.y, panel.y + 1);
        if let Some((source, _)) = app
            .logs
            .controls
            .iter()
            .find(|(_, control)| *control == Control::Source)
            .copied()
        {
            let arrow = Rect::new(source.right() - 2, source.y, 1, 1);
            click(&mut app, arrow);
            draw(&mut app, width, height);
            assert_eq!(app.logs.dropdown, Some(Control::Source));
            let menu = app.logs.dropdown_area.unwrap();
            assert!(menu.right() <= width && menu.bottom() <= height);
            click(&mut app, menu);
            assert!(app.logs.dropdown.is_none());
        }
    }
}

#[test]
fn output_headers_and_text_fit_zero_tiny_offset_and_narrow_areas() {
    for width in 0..=65 {
        for height in [0, 1, 2, 3, 8, 15] {
            let mut app = app(Log::default());
            let area = Rect::new(3, 2, width, height);
            app.panel_area = Some(area);
            let mut terminal = Terminal::new(TestBackend::new(width + 6, height + 4)).unwrap();
            terminal
                .draw(|frame| {
                    begin_frame(&mut app);
                    crate::ui_panels::draw_bottom_panel(frame, &mut app, area);
                })
                .unwrap();
            for rect in app
                .panel_tab_areas
                .iter()
                .map(|(rect, _)| rect)
                .chain(app.logs.controls.iter().map(|(rect, _)| rect))
            {
                assert!(rect.x >= area.x && rect.right() <= area.right());
                assert!(rect.y >= area.y && rect.bottom() <= area.bottom());
            }
            assert!(app.logs.reader.as_ref().unwrap().text.right() <= area.right());
        }
    }
}

#[test]
fn hidden_output_keeps_recording_history_without_waking_or_formatting_the_ui() {
    let log = Log::default();
    log.event("Transport", "before closing");
    let mut app = app(log.clone());
    assert!(draw(&mut app, 120, 30).contains("before closing"));
    let buffer = app.logs.editor().unwrap().navigation_id;
    let version = app.logs.editor().unwrap().content_version;
    crate::ui_panels::close_panel(&mut app);
    assert!(app.logs.observer.is_none());
    let wake = app.background.wake_handle();
    let generation = wake.generation();
    log.event("Disconnected", "while output was hidden");
    assert_eq!(wake.generation(), generation);
    assert!(!app.logs.take_changed());
    draw(&mut app, 120, 30);
    assert_eq!(app.logs.editor().unwrap().content_version, version);
    app.show_output();
    assert!(draw(&mut app, 120, 30).contains("while output was hidden"));
    assert_eq!(app.logs.editor().unwrap().navigation_id, buffer);
    let version = app.logs.editor().unwrap().content_version;
    for _ in 0..3 {
        draw(&mut app, 120, 30);
        assert!(!app.logs.take_changed());
    }
    assert_eq!(app.logs.editor().unwrap().content_version, version);
    crate::ui_panels::select_tab(&mut app, 1);
    assert!(app.logs.observer.is_none());
    log.event("Test", "while Problems was selected");
    crate::ui_panels::select_tab(&mut app, 2);
    assert!(draw(&mut app, 120, 30).contains("while Problems was selected"));
}

#[test]
fn output_history_loads_in_bounded_batches_and_catches_events_before_subscription() {
    let log = Log::default();
    for index in 0..1200 {
        log.event("Test", &format!("event {index}"));
    }
    let mut app = app(log.clone());
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    begin_frame(&mut app);
    terminal
        .draw(|frame| crate::ui_panels::draw_bottom_panel(frame, &mut app, frame.area()))
        .unwrap();
    assert_eq!(app.logs.reader.as_ref().unwrap().view.entries.len(), 128);
    assert!(app.logs.observer.is_none());
    log.event("Test", "arrived before subscription");
    let wake = app.background.wake_handle();
    let generation = wake.generation();
    app.logs.finish_frame(wake.clone());
    assert!(wake.generation() > generation && app.logs.take_changed());
    for _ in 0..12 {
        draw(&mut app, 120, 30);
    }
    assert_eq!(app.logs.reader.as_ref().unwrap().view.entries.len(), 1201);
    assert!(
        app.logs
            .editor()
            .unwrap()
            .rope
            .to_string()
            .contains("arrived before subscription")
    );
    assert!(!app.logs.take_changed());
}

#[test]
fn selecting_and_scrolling_output_pauses_autoscroll_without_editing_or_scrolling_the_file() {
    let log = Log::default();
    for _ in 0..80 {
        log.event("Test", "Repeated λ🦀👩‍💻 output");
    }
    let mut app = app(log.clone());
    draw(&mut app, 140, 30);
    let text = app.logs.reader.as_ref().unwrap().text;
    pointer(
        &mut app,
        MouseEventKind::ScrollUp,
        text.x + 3,
        text.y,
        KeyModifiers::NONE,
    );
    let scroll = app.logs.editor().unwrap().scroll_offset;
    assert!(!app.logs.reader.as_ref().unwrap().view.autoscroll);
    draw(&mut app, 140, 30);
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        text.x + 5,
        text.y + 1,
        KeyModifiers::NONE,
    );
    pointer(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        text.x + 18,
        text.y + 2,
        KeyModifiers::NONE,
    );
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        text.x + 18,
        text.y + 2,
        KeyModifiers::NONE,
    );
    let selected = app.logs.editor().unwrap().selected_text();
    assert!(selected.is_some());
    log.event("Test", "new event while reading");
    draw(&mut app, 140, 30);
    assert_eq!(app.logs.editor().unwrap().selected_text(), selected);
    assert_eq!(app.logs.editor().unwrap().scroll_offset, scroll);
    assert_eq!(app.editor().unwrap().rope, "λ");
    assert_eq!(app.editor().unwrap().scroll_offset, 0);
    let autoscroll = control(&app, Control::Autoscroll);
    click(&mut app, autoscroll);
    log.event("Test", "latest event");
    assert!(draw(&mut app, 140, 30).contains("latest event"));
    assert!(app.logs.reader.as_ref().unwrap().view.autoscroll);
    assert!(!app.logs.editor().unwrap().has_selection());
}

#[test]
fn output_has_native_navigation_search_forward_backward_and_live_matches() {
    let log = Log::default();
    for index in 0..80 {
        log.event("Test", &format!("row {index} needle λ"));
    }
    let mut app = app(log.clone());
    draw(&mut app, 140, 35);
    input(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
    assert_eq!(app.logs.editor().unwrap().cursor_line, 0);
    input(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(paste(&mut app, "needle"));
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let first = app.logs.editor().unwrap().cursor_line;
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let second = app.logs.editor().unwrap().cursor_line;
    assert!(second > first);
    input(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(app.logs.editor().unwrap().cursor_line, first);
    log.event("Test", "needle appended");
    draw(&mut app, 140, 35);
    assert!(app.logs.reader.as_ref().unwrap().searching());
    input(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.active_pane, ActivePane::Panel);
    input(&mut app, KeyCode::End, KeyModifiers::CONTROL);
    assert_eq!(
        app.logs.editor().unwrap().cursor_line,
        app.logs.editor().unwrap().rope.len_lines() - 1
    );
    assert_eq!(app.editor().unwrap().rope, "λ");
}

#[test]
fn escape_closes_output_find_after_focus_moves_back_to_the_output_text() {
    let log = Log::default();
    log.event("Test", "output text");
    let mut app = app(log);
    draw(&mut app, 140, 30);
    input(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    draw(&mut app, 140, 30);
    let text = app.logs.reader.as_ref().unwrap().text;
    click(&mut app, text);
    assert!(app.logs.reader.as_ref().unwrap().searching());
    assert!(!app.logs.search_focused());
    input(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.logs.reader.as_ref().unwrap().searching());
    assert_eq!(app.active_pane, ActivePane::Panel);
    input(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.active_pane, ActivePane::Editor);
}

#[test]
fn output_find_is_case_insensitive_incremental_and_owns_query_clipboard_history() {
    let log = Log::default();
    log.event("Test", "Needle one λ");
    let mut app = app(log.clone());
    draw(&mut app, 140, 30);
    input(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    paste(&mut app, "needle");
    draw(&mut app, 140, 30);
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.logs.editor().unwrap().selected_text().as_deref(),
        Some("Needle")
    );
    log.event("Test", "needle two λ");
    draw(&mut app, 140, 30);
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.logs.editor().unwrap().selected_text().as_deref(),
        Some("needle")
    );
    input(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(
        app.logs.editor().unwrap().selected_text().as_deref(),
        Some("Needle")
    );
    input(&mut app, KeyCode::Char('c'), KeyModifiers::ALT);
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.logs.editor().unwrap().selected_text().as_deref(),
        Some("needle")
    );
    input(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    paste(&mut app, "other");
    input(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(app.logs.search_focused());
    assert_eq!(app.editor().unwrap().rope, "λ");
}

#[test]
fn fresh_pointer_presses_end_output_capture_and_popups_do_not_steal_owned_releases() {
    let log = Log::default();
    log.event("Test", "select this output");
    let mut app = app(log);
    draw(&mut app, 140, 30);
    let text = app.logs.reader.as_ref().unwrap().text;
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        text.x + 5,
        text.y,
        KeyModifiers::NONE,
    );
    assert!(app.logs.reader.as_ref().unwrap().capturing());
    app.exit_confirm_active = true;
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        text.x + 10,
        text.y,
        KeyModifiers::NONE,
    );
    assert!(!app.logs.reader.as_ref().unwrap().capturing());
    app.exit_confirm_active = false;
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        text.x + 5,
        text.y,
        KeyModifiers::NONE,
    );
    let editor = app.code_area.unwrap();
    click(&mut app, editor);
    assert!(!app.logs.reader.as_ref().unwrap().capturing());
    assert_eq!(app.active_pane, ActivePane::Editor);
    app.show_output();
    crate::ui_desks::open(&mut app, crate::ui_desks::Mode::New);
    assert!(!key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
    ));
    app.desks.popup = Default::default();
    let source = control(&app, Control::Source);
    click(&mut app, source);
    assert!(app.logs.dropdown.is_some());
    click(&mut app, source);
    assert!(
        app.logs.dropdown.is_none(),
        "the same source button closes its dropdown"
    );
}

#[test]
fn overflow_actions_and_keyboard_controls_remain_usable_at_narrow_widths() {
    let log = Log::default();
    log.event("Test", "retained history");
    let mut app = app(log.clone());
    draw(&mut app, 60, 25);
    let more = control(&app, Control::More);
    click(&mut app, more);
    draw(&mut app, 60, 25);
    assert_eq!(app.logs.dropdown, Some(Control::More));
    input(&mut app, KeyCode::Down, KeyModifiers::NONE);
    input(&mut app, KeyCode::Down, KeyModifiers::NONE);
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.logs.editor().unwrap().rope.len_chars(), 0);
    assert!(
        log.report().contains("retained history"),
        "Clear output does not destroy diagnostic capture"
    );
    log.event("Test", "after clear");
    draw(&mut app, 60, 25);
    let text = app.logs.editor().unwrap().rope.to_string();
    assert!(text.contains("after clear") && !text.contains("retained history"));
    app.logs.focus = None;
    input(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.logs.focus, Some(Control::Source));
    input(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.logs.dropdown, Some(Control::Source));
    input(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    input(
        &mut app,
        KeyCode::Char('p'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(app.palette.active);
}

#[test]
fn output_mouse_and_copy_handle_tabs_wide_text_and_joined_emoji() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let log = Log::default();
    log.event("Test", "\t界λ🦀👩‍💻 tail");
    let mut app = app(log);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let (x, y) = (0..30)
        .find_map(|y| (0..140).find_map(|x| (buffer[(x, y)].symbol() == "界").then_some((x, y))))
        .unwrap();
    let point = Rect::new(x, y, 1, 1);
    click(&mut app, point);
    let column = app.logs.editor().unwrap().cursor_col;
    assert_eq!(
        app.logs
            .editor()
            .unwrap()
            .rope
            .line(app.logs.editor().unwrap().cursor_line)
            .char(column),
        '界'
    );
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        x + 1,
        y,
        KeyModifiers::NONE,
    );
    assert_eq!(app.logs.editor().unwrap().cursor_col, column);
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        x + 1,
        y,
        KeyModifiers::NONE,
    );
    input(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    input(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(
        crate::clipboard::get_text().unwrap(),
        app.logs.editor().unwrap().rope.to_string()
    );
    assert_eq!(app.editor().unwrap().rope, "λ");
}

#[test]
fn scrollbars_appear_only_when_needed_and_shift_wheel_scrolls_output_horizontally() {
    let log = Log::default();
    for _ in 0..80 {
        log.event("Test", &"text ".repeat(80));
    }
    let mut app = app(log);
    app.logs.reader().wrap = false;
    draw(&mut app, 100, 30);
    let text = app.logs.reader.as_ref().unwrap().text;
    pointer(
        &mut app,
        MouseEventKind::ScrollDown,
        text.x + 3,
        text.y + 1,
        KeyModifiers::SHIFT,
    );
    assert_eq!(app.logs.reader.as_ref().unwrap().horizontal, 6);
    assert_eq!(app.editor().unwrap().horizontal_scroll, 0);
    draw(&mut app, 100, 30);
    // Drag the vertical slider to both ends, then release outside the panel.
    let area = app.logs.reader.as_ref().unwrap().text;
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.right(),
        area.y,
        KeyModifiers::NONE,
    );
    pointer(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.right(),
        area.bottom() + 10,
        KeyModifiers::NONE,
    );
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        area.right(),
        area.bottom() + 10,
        KeyModifiers::NONE,
    );
    assert!(!app.logs.reader.as_ref().unwrap().capturing());
    assert_eq!(
        app.logs.editor().unwrap().scroll_offset,
        app.logs.editor().unwrap().rope.len_lines() - area.height as usize
    );
}

#[test]
fn panel_maximize_restore_close_and_resize_preserve_preferred_height_and_output() {
    let log = Log::default();
    log.event("Test", "preserved output");
    let mut app = app(log);
    draw(&mut app, 140, 40);
    let height = app.panel_height;
    let normal = app.panel_area.unwrap().height;
    let maximize = app.panel_chrome.maximize;
    click(&mut app, maximize);
    draw(&mut app, 140, 40);
    assert!(app.panel_chrome.maximized && app.panel_area.unwrap().height > normal);
    assert_eq!(app.panel_height, height);
    let maximized = app.panel_area.unwrap();
    pointer(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        maximized.x + 4,
        maximized.y - 1,
        KeyModifiers::NONE,
    );
    pointer(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        maximized.x + 4,
        maximized.y - 1,
        KeyModifiers::NONE,
    );
    assert!(
        app.panel_chrome.maximized,
        "clicking the resize seam does not restore a maximized panel"
    );
    let restore = app.panel_chrome.maximize;
    click(&mut app, restore);
    draw(&mut app, 140, 40);
    assert!(!app.panel_chrome.maximized && app.panel_area.unwrap().height == normal);
    let close = app.panel_chrome.close;
    click(&mut app, close);
    assert!(!app.show_panel && app.logs.observer.is_none());
    app.show_output();
    assert!(draw(&mut app, 140, 40).contains("preserved output"));
}

#[test]
fn output_cannot_capture_top_menus_modals_other_editor_panes_or_terminals() {
    let log = Log::default();
    log.event("Test", "output");
    let mut app = app(log);
    app.tabs
        .push(crate::app::Tab::new_file(Editor::test_with_text(
            "second pane",
        )));
    app.create_split_group(1);
    app.show_output();
    draw(&mut app, 180, 40);
    app.menu_active = true;
    app.menu_open = true;
    assert!(!key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    ));
    let source = control(&app, Control::Source);
    assert!(!mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: source.x,
            row: source.y,
            modifiers: KeyModifiers::NONE
        }
    ));
    app.menu_active = false;
    app.menu_open = false;
    app.exit_confirm_active = true;
    assert!(!paste(&mut app, "modal owns paste"));
    app.exit_confirm_active = false;
    let editor = app.code_area.unwrap();
    click(&mut app, editor);
    assert_eq!(app.active_pane, ActivePane::Editor);
    input(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(app.editor().unwrap().rope.to_string().contains('x'));
    assert_eq!(
        app.logs
            .editor()
            .unwrap()
            .rope
            .to_string()
            .matches("output")
            .count(),
        1
    );
}

#[test]
fn output_focus_keeps_keyboard_and_paste_out_of_editor_and_docked_terminals() {
    for in_editor in [false, true] {
        let log = Log::default();
        log.event("Test", "remote output");
        let mut app = app(log);
        app.execute_palette_command(if in_editor {
            "new_terminal_in_editor_tab"
        } else {
            "new_terminal"
        });
        let index = app.active_terminal;
        let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "shell output");
        let captured = pty.test_capture_input();
        app.terminals[index].terminal_pty = Some(pty);
        app.show_output();
        draw(&mut app, 180, 40);
        input(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(paste(&mut app, "no terminal input"));
        assert!(captured.lock().unwrap().is_empty());
        let surface = app
            .terminal_surfaces
            .iter()
            .find(|surface| surface.index == index)
            .unwrap()
            .area;
        click(&mut app, surface);
        input(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(&*captured.lock().unwrap(), b"x");
        assert!(
            app.logs
                .editor()
                .unwrap()
                .rope
                .to_string()
                .contains("remote output")
        );
    }
}

#[test]
fn desk_and_session_restore_output_visibility_without_serializing_runtime_logs() {
    let log = Log::default();
    log.event("Test", "runtime-only output");
    let mut app = app(log.clone());
    draw(&mut app, 130, 30);
    let id = app.logs.editor().unwrap().navigation_id;
    let first = app.desks.active;
    let other = app.new_desk("Other", false);
    assert_eq!(app.desks.active, other);
    draw(&mut app, 130, 30);
    log.event("Test", "while desk hidden");
    app.switch_desk(first);
    assert!(draw(&mut app, 130, 30).contains("while desk hidden"));
    assert_eq!(app.logs.editor().unwrap().navigation_id, id);
    assert_eq!(app.panel_tab, 2);
    let session = app.build_session();
    let serialized = serde_json::to_string(&session).unwrap();
    assert!(!serialized.contains("runtime-only output"));
    assert_eq!(session.panel_tab, 2);
    assert!(session.show_panel);
}

#[test]
fn output_visual_layout_snapshot() {
    let log = Log::default();
    log.begin("ssh henri@compute.example".as_ref());
    log.event(
        "Handshake",
        "Headless connected · workspace /home/henri/project",
    );
    log.event("Read file", "src/main.rs · 3.4 KiB · completed in 2 ms");
    log.event(
        "Save file",
        "src/main.rs · 1 changed range · completed in 1 ms",
    );
    log.event("Disconnected", "The SSH transport stopped responding. Retrying the connection while preserving local edits and undo history. Open the connection log to inspect the transport and operation details.");
    log.event("Launch failed", "The remote command could not start; see the transport output for details.");
    let mut app = app(log);
    app.tabs[0].editor.rope =
        ropey::Rope::from_str("fn main() {\n    println!(\"Hello, world!\");\n}\n");
    app.panel_height = 10;
    let mut terminal = Terminal::new(TestBackend::new(140, 32)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    if let Some(path) = std::env::var_os("VTCODE_OUTPUT_SNAPSHOT") {
        let buffer = terminal.backend().buffer();
        let cells: Vec<_> = buffer
            .content
            .iter()
            .map(|cell| {
                (
                    cell.symbol(),
                    format!("{:?}", cell.fg),
                    format!("{:?}", cell.bg),
                    format!("{:?}", cell.modifier),
                )
            })
            .collect();
        std::fs::write(path, serde_json::to_vec(&serde_json::json!({ "width": buffer.area.width, "height": buffer.area.height, "cells": cells })).unwrap()).unwrap();
    }
}

#[test]
fn output_wrap_uses_the_wrap_binding_and_remembers_each_channels_preference() {
    let mut app = app(Log::default());
    draw(&mut app, 140, 32);
    let editor_wrap = app.editor().unwrap().word_wrap;
    assert!(app.logs.reader().wrap);
    input(&mut app, KeyCode::Char('z'), KeyModifiers::ALT);
    assert!(!app.logs.reader().wrap);
    assert_eq!(app.editor().unwrap().word_wrap, editor_wrap);
    app.logs.select_source(Source::Git);
    assert!(app.logs.reader().wrap);
    app.logs.select_source(Source::RemoteSsh);
    assert!(!app.logs.reader().wrap);
    assert!(menu_actions(&app, Control::More).iter().any(|(label, kind)| *label == "Wrap lines" && *kind == Control::Wrap));
    action(&mut app, Control::Wrap);
    assert!(app.logs.reader().wrap);
}

#[test]
fn retention_remaps_native_selection_and_scroll_without_rebuilding_retained_text() {
    let log = Log::default();
    for _ in 0..1024 {
        log.event("Test", &"x".repeat(1024));
    }
    let mut editor = text::Reader::default().editor;
    editor.viewport_height = 10;
    let mut view = View::default();
    while view.version != log.version() {
        view.refresh(&mut editor, Some(&log));
    }
    editor.cursor_line = 51;
    editor.cursor_col = 30;
    editor.selection_anchor = Some((50, 10));
    editor.scroll_offset = 45;
    let selection = editor.selected_text();
    log.event("Test", &"y".repeat(1024));
    view.refresh(&mut editor, Some(&log));
    assert_eq!(view.omitted, 1);
    assert_eq!(editor.cursor_line, 50);
    assert_eq!(editor.selection_anchor, Some((49, 10)));
    assert_eq!(editor.selected_text(), selection);
    assert_eq!(editor.scroll_offset, 44);
    assert!(!view.autoscroll);
    assert!(view.entries.iter().all(|(id, _, _)| *id >= 2));
    editor.selection_anchor = Some((0, 10));
    log.event("Test", &"z".repeat(1024));
    view.refresh(&mut editor, Some(&log));
    assert_eq!(
        editor.selection_anchor,
        Some((0, 0)),
        "An evicted endpoint clamps to the start"
    );
}

#[test]
fn live_transport_logs_stay_available_after_disconnect_without_extra_rpc_logging() {
    use crate::remote::{self, client::Client, workspace::Workspace};
    use std::{
        io::BufReader,
        net::{TcpListener, TcpStream},
        time::Duration,
    };
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let service_root = root.path().to_owned();
    let service = std::thread::spawn(move || {
        remote::service::serve(&service_root, server.try_clone().unwrap(), server).unwrap()
    });
    let log = Log::default();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let hello = Client::read_hello(&mut reader).unwrap();
    let client =
        Client::attach_with_diagnostics(reader, socket, hello, Some(log.begin("ssh".as_ref())))
            .unwrap();
    let remote = Workspace::new(client);
    let mut app = app(log.clone());
    app.remote_workspace = Some(remote.clone());
    assert!(draw(&mut app, 160, 30).contains("Starting ssh"));
    let version = log.version();
    remote.client.call(remote::Command::Ping).unwrap();
    assert_eq!(log.version(), version, "Logs adds no normal RPC logging");
    assert!(
        !remote.client.capturing_cpu(),
        "viewing Logs never starts a CPU capture"
    );
    let capture = control(&app, Control::CaptureCpu);
    assert!(!capture.is_empty());
    click(&mut app, capture);
    assert!(
        remote.client.capturing_cpu(),
        "CPU capture requires explicit input"
    );
    assert!(draw(&mut app, 160, 30).contains("Measuring"));
    assert_eq!(app.tabs.len(), 1);
    assert!(app.show_panel && app.panel_tab == 2);
    remote.client.call(remote::Command::Shutdown).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| {
        let client = &app.remote_workspace.as_ref().unwrap().client;
        !client.connected() && !client.capturing_cpu()
    });
    app.poll_background_results();
    assert!(app.show_panel && app.panel_tab == 2);
    // Connection notifications may overlay the tab, but its native buffer is retained.
    draw(&mut app, 160, 30);
    assert!(
        app.logs
            .editor()
            .unwrap()
            .rope
            .to_string()
            .contains("Disconnected")
    );
    crate::ui_panels::close_panel(&mut app);
    app.remote_workspace = None;
    app.show_output();
    app.side_notice.dismiss(crate::app_remote_connection::NOTICE_ID);
    assert!(draw(&mut app, 160, 30).contains("Disconnected"));
    assert!(log.report().contains("Capture stopped"));
    remote.client.disconnect();
    service.join().unwrap();
}
