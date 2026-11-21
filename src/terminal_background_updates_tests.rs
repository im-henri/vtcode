use super::*;
use crate::{
    app::{Tab, TerminalTab},
    editor::Editor,
    terminal::TerminalEmulator,
};
use ratatui::{TerminalOptions, Viewport, layout::Rect};
use std::{
    cell::Cell,
    io::Write,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::Instant,
};

struct SlowOutput {
    bytes: Arc<Mutex<Vec<u8>>>,
    written: bool,
}

impl Write for SlowOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        self.written |= !bytes.is_empty();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.written) {
            // Ensure the final output arrives during the redraw quiet gap.
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    }
}

struct ClearObserver;
impl Drop for ClearObserver {
    fn drop(&mut self) {
        super::pointer_event_loop_latency_tests::clear_observer();
    }
}

fn screen_text(app: &App) -> String {
    app.terminals[0]
        .terminal_pty
        .as_ref()
        .unwrap()
        .grid
        .cells
        .iter()
        .flatten()
        .map(|cell| cell.ch)
        .collect()
}

#[test]
fn remote_workbench_repaints_editor_text_on_unfocused_resize() {
    let directory = tempfile::tempdir().unwrap();
    let text = "RemoteResizeContent\n";
    std::fs::write(directory.path().join("resize.txt"), text).unwrap();
    let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
        directory.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    app.show_explorer = false;
    app.show_minimap = false;
    app.show_status_bar = false;
    app.cursor_blinking = false;
    for effect in crate::animation_config::AnimationEffect::ALL {
        app.animation_settings.set(effect, false);
    }
    app.open_file(&workspace.target_key("resize.txt").unwrap());
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.rope, text);
    editor.insert_char('!');
    let contents = editor.rope.clone();

    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = SlowOutput {
        bytes: output.clone(),
        written: false,
    };
    let backend =
        crate::terminal_output::AnsiBackend::new(crate::terminal_output::buffered_output(writer));
    let mut terminal = Terminal::with_options(
        crate::terminal_output::test_cursor_backend(backend),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 140, 32)),
        },
    )
    .unwrap();
    let wake = app.background.wake_handle();
    let (sender, receiver) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let (fail_sender, fail_wake) = (sender.clone(), wake.clone());
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(5)).is_err() {
            let _ = fail_sender.send(Err(io::Error::other(
                "Resized remote workbench did not repaint",
            )));
            fail_wake.notify();
        }
    });
    let phase = Rc::new(Cell::new(0));
    let observed = phase.clone();
    let mut initial_width = 0;
    super::pointer_event_loop_latency_tests::observe(move |app| {
        let width = app.code_area.unwrap().width;
        match observed.get() {
            0 => {
                initial_width = width;
                sender.send(Ok(Event::FocusLost)).unwrap();
                sender.send(Ok(Event::Resize(100, 24))).unwrap();
                observed.set(1);
            }
            1 if !app.terminal_has_focus && width < initial_width => {
                // The coalesced burst ends at the same width, but the host
                // may already have reflowed the screen at the intermediate one.
                sender.send(Ok(Event::Resize(120, 28))).unwrap();
                sender.send(Ok(Event::Resize(100, 24))).unwrap();
                observed.set(2);
            }
            2 if !app.terminal_has_focus => {
                observed.set(3);
                return true;
            }
            _ => return false,
        }
        wake.notify();
        false
    });
    let _clear = ClearObserver;
    let result = run_app(
        &mut terminal,
        &mut app,
        TerminalEventPump::from_receiver(receiver),
        None,
    );
    let _ = done.send(());
    watchdog.join().unwrap();
    result.unwrap();
    assert_eq!(phase.get(), 3);
    assert_eq!(terminal.get_frame().area(), Rect::new(0, 0, 100, 24));
    let bytes = output.lock().unwrap().clone();
    let screen = TerminalEmulator::test_with_output(100, 24, &String::from_utf8_lossy(&bytes));
    let visible: String = screen
        .grid
        .cells
        .iter()
        .flatten()
        .map(|cell| cell.ch)
        .collect();
    assert!(visible.contains("!RemoteResizeContent"), "{visible:?}");
    assert_eq!(app.editor().unwrap().rope, contents);
    assert!(app.editor().unwrap().modified);
    assert!(workspace.client.connected());
}

#[test]
fn visible_terminal_output_and_completion_are_painted_after_window_focus_is_lost() {
    for location in ["dock", "editor", "split"] {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new_deferred(directory.path().into());
        app.source_control_enabled = false;
        app.show_explorer = false;
        app.show_status_bar = false;
        app.cursor_blinking = false;
        app.workspace_watcher_active = true;
        for effect in crate::animation_config::AnimationEffect::ALL {
            app.animation_settings.set(effect, false);
        }
        app.tabs = vec![Tab::new_file(Editor::test_with_text("document"))];
        let mut pty = TerminalEmulator::test_with_output(80, 12, "");
        let _keep_running = pty.test_keep_running();
        let mut tab = TerminalTab::new("Build".into(), directory.path().into());
        tab.terminal_pty = Some(pty);
        app.terminals = vec![tab];
        app.active_terminal = 0;
        if location == "dock" {
            app.show_terminal_sidebar = true;
            app.terminal_sidebar_revealed = true;
            app.terminal_panel_focused = true;
        } else {
            assert!(app.detach_terminal_to_group(0, app.active_group));
            if location == "split" {
                app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
                app.switch_group(0);
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut terminal = Terminal::with_options(
            ratatui::backend::CrosstermBackend::new(SlowOutput {
                bytes: output.clone(),
                written: false,
            }),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 140, 32)),
            },
        )
        .unwrap();
        let wake = app.background.wake_handle();
        let (sender, receiver) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let fail_sender = sender.clone();
        let fail_wake = wake.clone();
        let watchdog = std::thread::spawn(move || {
            if done.recv_timeout(Duration::from_secs(5)).is_err() {
                let _ = fail_sender.send(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "unfocused terminal frame never arrived",
                )));
                fail_wake.notify();
            }
        });
        let phase = Rc::new(Cell::new(0));
        let observed = phase.clone();
        super::pointer_event_loop_latency_tests::observe(move |app| {
            match observed.get() {
                0 => {
                    assert!(app.terminal_has_focus);
                    assert!(
                        app.terminal_surfaces
                            .iter()
                            .any(|surface| surface.index == 0 && !surface.area.is_empty())
                    );
                    sender.send(Ok(Event::FocusLost)).unwrap();
                    app.terminals[0]
                        .terminal_pty
                        .as_ref()
                        .unwrap()
                        .test_queue_output(b"\x1b]633;C\x07BUILD_STEP_ONE\r\n");
                    observed.set(1);
                    wake.notify();
                }
                1 if !app.terminal_has_focus && screen_text(app).contains("BUILD_STEP_ONE") => {
                    app.terminals[0]
                        .terminal_pty
                        .as_ref()
                        .unwrap()
                        .test_queue_output(b"BUILD_DONE\r\n\x1b]633;D;0\x07");
                    observed.set(2);
                    wake.notify();
                }
                2 if !app.terminal_has_focus && screen_text(app).contains("BUILD_DONE") => {
                    observed.set(3);
                    return true;
                }
                _ => {}
            }
            false
        });
        let _clear = ClearObserver;
        let result = run_app(
            &mut terminal,
            &mut app,
            TerminalEventPump::from_receiver(receiver),
            None,
        );
        let _ = finished.send(());
        watchdog.join().unwrap();
        result.unwrap_or_else(|error| panic!("{location}: {error}; phase {}", phase.get()));
        assert_eq!(phase.get(), 3, "{location}");
        assert!(!app.terminal_has_focus, "no refocus event is needed");
        assert!(app.terminals[0].completion.unread);
        assert_eq!(app.terminals[0].completion.finished_count(), 1);
        let bytes = output.lock().unwrap();
        for marker in [b"BUILD_STEP_ONE".as_slice(), b"BUILD_DONE".as_slice()] {
            assert!(
                bytes.windows(marker.len()).any(|bytes| bytes == marker),
                "{location}: output must reach the displayed terminal"
            );
        }
        assert!(
            !app.service_workspaces_until(
                crate::ui_work::Service::Terminals,
                Instant::now() + Duration::from_millis(4),
                || true
            ),
            "a quiet terminal must not request another redraw"
        );
    }
}
