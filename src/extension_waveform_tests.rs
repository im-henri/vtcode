//! Real compiled Wasm through VTCode's worker queue, document API, and renderer.
use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::{path::Path, time::Duration};

const WASM: &[u8] = include_bytes!("../tests/fixtures/waveform-viewer/waveform-viewer.wasm");
const VCD: &[u8] = include_bytes!("../tests/fixtures/waveform-viewer/demo.vcd");
fn app(root: &Path) -> App {
    let package = root.join("extensions/waveform-viewer");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("extension.json"),
        include_bytes!("../tests/fixtures/waveform-viewer/extension.json"),
    )
    .unwrap();
    std::fs::write(package.join("waveform-viewer.wasm"), WASM).unwrap();
    let mut app = App::new(root.to_path_buf());
    app.extension_manager.search_roots = vec![root.join("extensions")];
    app.extension_manager.settings_file = root.join("state/extensions.json");
    app.extension_manager.scan_and_sync();
    assert_eq!(
        app.extension_manager.error_count(),
        0,
        "{:?}",
        app.extension_views()
    );
    app
}
fn drain(app: &mut App) {
    for _ in 0..1000 {
        app.extension_manager
            .dispatch_view_events(&mut app.background);
        app.wait_for_background(Duration::from_secs(15), |app| {
            !app.extension_manager.has_busy_runtimes()
        });
        if app.extension_manager.pending_view_events.is_empty() {
            if app.extension_manager.records[0]
                .tick_handlers
                .iter()
                .any(|h| h.export_name == "wave_apply")
            {
                app.extension_manager
                    .invoke_tick_handlers_async(
                        "vtcode.waveform-viewer",
                        vec!["wave_apply".into()],
                        &mut app.background,
                        None,
                    )
                    .unwrap();
                continue;
            }
            if app.extension_manager.records[0]
                .tick_handlers
                .iter()
                .any(|h| h.export_name == "wave_render")
            {
                app.extension_manager
                    .invoke_tick_handlers_async(
                        "vtcode.waveform-viewer",
                        vec!["wave_render".into()],
                        &mut app.background,
                        None,
                    )
                    .unwrap();
                continue;
            }
            return;
        }
    }
    panic!("Waveform input queue did not drain");
}
fn tick(app: &mut App) {
    app.extension_manager
        .invoke_tick_handlers_async(
            "vtcode.waveform-viewer",
            vec!["wave_tick".into()],
            &mut app.background,
            None,
        )
        .unwrap();
    drain(app);
}
fn finish(app: &mut App) {
    for _ in 0..1000 {
        if app.extension_manager.records[0].tick_handlers.is_empty() {
            return;
        }
        tick(app);
        assert!(
            app.extension_manager.records[0].last_error.is_none(),
            "{:?}",
            app.extension_views()
        );
    }
    panic!("Waveform loading/search did not finish");
}
fn open(app: &mut App, path: &Path) {
    assert!(app.run_extension_command_with_optional_context(
        "vtcode.waveform-viewer",
        "wave.open",
        Some(path)
    ));
    drain(app);
    assert!(
        app.active_extension_view().is_some(),
        "{:?}",
        app.extension_views()
    );
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(
        app,
        crossterm::event::KeyEvent::new(code, modifiers),
    );
    drain(app);
    assert!(
        app.extension_manager.records[0].last_error.is_none(),
        "{:?}",
        app.extension_views()
    );
    assert!(!app.notification_history.iter().any(|(message, _, _)| message.contains("out of fuel") || message.contains("wasm trap")), "{:?}", app.notification_history);
}
fn text(app: &App) -> String {
    let view = app.active_extension_view().unwrap();
    app.extension_manager
        .get_view(&view.extension_id, &view.view_id)
        .unwrap()
        .frame
        .unwrap()
        .lines
        .iter()
        .map(|line| line.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
fn resize(app: &mut App, width: u16, height: u16) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            assert!(crate::extension_views::draw_active_view(
                frame,
                app,
                Rect::new(0, 0, width, height)
            ));
        })
        .unwrap();
    drain(app);
}
fn window_ns(app: &App) -> (f64, f64) {
    fn ns(value: &str) -> f64 {
        let mut words = value.split_whitespace();
        let number: f64 = words.next().unwrap().parse().unwrap();
        number
            * match words.next().unwrap() {
                "us" => 1000.0,
                "ms" => 1_000_000.0,
                "s" => 1_000_000_000.0,
                "ps" => 0.001,
                _ => 1.0,
            }
    }
    let rendered = text(app);
    let line = rendered.lines().nth(2).unwrap();
    let window = line.split('│').nth(1).unwrap();
    let mut endpoints = window.split('→');
    (ns(endpoints.next().unwrap()), ns(endpoints.next().unwrap()))
}

fn waveform_mouse(app: &mut App, kind: crossterm::event::MouseEventKind, x: u16, y: u16) {
    waveform_mouse_with_modifiers(app, kind, x, y, KeyModifiers::NONE);
}

fn waveform_mouse_with_modifiers(
    app: &mut App,
    kind: crossterm::event::MouseEventKind,
    x: u16,
    y: u16,
    modifiers: KeyModifiers,
) {
    crate::mouse::handle_mouse(
        app,
        crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers,
        },
    );
    drain(app);
    assert!(
        app.extension_manager.records[0].last_error.is_none(),
        "{:?}",
        app.extension_views()
    );
}

fn signal_names_column(app: &App) -> Vec<String> {
    let content = text(app);
    let body_height = content.lines().count().saturating_sub(8);
    content
        .lines()
        .skip(5)
        .take(body_height)
        .map(|line| {
            line.chars()
                .skip(30)
                .take(27)
                .collect::<String>()
                .trim_start_matches(['▎', '▸', '▾', '↓', '╭', '╰', '│', ' '])
                .trim_end()
                .to_string()
        })
        .filter(|line| line.chars().any(|c| c.is_alphanumeric()))
        .collect()
}

fn label_point(app: &App, label: &str) -> (u16, u16) {
    let rendered = text(app);
    let (row, line) = rendered
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(label))
        .unwrap_or_else(|| panic!("Missing {label:?} in\n{rendered}"));
    let column = line[..line.find(label).unwrap()].chars().count() + 1;
    let area = app.code_area.unwrap();
    (area.x + column as u16, area.y + row as u16)
}

fn click_label(app: &mut App, label: &str) {
    use crossterm::event::{
        MouseButton::Left,
        MouseEventKind::{Down, Up},
    };
    let (x, y) = label_point(app, label);
    waveform_mouse(app, Down(Left), x, y);
    waveform_mouse(app, Up(Left), x, y);
}

fn saved_session(app: &mut App) -> serde_json::Value {
    key(app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    let path = app.extension_manager.records[0]
        .runtime
        .as_ref()
        .unwrap()
        .store
        .data()
        .settings_file_path
        .as_ref()
        .unwrap();
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let session = settings["vtcode.waveform-viewer"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(key, _)| key.starts_with("session:"))
        .unwrap()
        .1
        .as_str()
        .unwrap();
    serde_json::from_str(session).unwrap()
}

/// Run the production idle scheduler and render actual workbench frames. No
/// simulated input and no direct calls to guest task exports advance the load.
fn idle_until(
    app: &mut App,
    terminal: &mut Terminal<TestBackend>,
    ready: impl Fn(&App) -> bool,
) -> (Duration, usize) {
    idle_with_input(app, terminal, ready, |_| {})
}

fn idle_with_input(
    app: &mut App,
    terminal: &mut Terminal<TestBackend>,
    ready: impl Fn(&App) -> bool,
    mut on_turn: impl FnMut(&mut App),
) -> (Duration, usize) {
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_secs(5);
    let wake = app.background.wake_handle();
    let mut draw = true;
    let mut frames = 0;
    loop {
        let observed = wake.generation();
        draw |= app.poll_background_results();
        if draw {
            terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
            frames += 1;
            draw = false;
        }
        on_turn(app);
        crate::extension_scheduler::dispatch(app);
        if ready(app)
            && !app.extension_manager.has_busy_runtimes()
            && app.extension_manager.pending_view_events.is_empty()
        {
            return (start.elapsed(), frames);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "idle scheduler stalled after {frames} frames; viewport: {:?}; tasks: {:?}; notifications: {:?}; view: {}",
            app.code_area,
            app.extension_manager.records[0].tick_handlers,
            app.notification_history
                .iter()
                .rev()
                .take(5)
                .collect::<Vec<_>>(),
            if app.active_extension_view().is_some() {
                text(app)
                    .lines()
                    .take(12)
                    .map(|line| line.chars().take(120).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                "not yet opened".into()
            }
        );
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let wait = crate::extension_scheduler::next_wait(app)
            .map_or(remaining, |wait| wait.min(remaining));
        if !app.background.has_pending_results() {
            wake.wait_for_change(observed, Some(wait));
        }
    }
}

fn dense_vcd() -> Vec<u8> {
    use std::fmt::Write;
    let mut data = String::from(
        "$timescale 1 ns $end $scope module top $end $var wire 1 c clk $end $var wire 32 b data [31:0] $end $upscope $end $enddefinitions $end\n",
    );
    for time in 0..100_000u32 {
        writeln!(data, "#{time}\n{}c\nb{time:032b} b", time % 2).unwrap();
    }
    data.into_bytes()
}

fn waveform_pair(root: &Path) -> App {
    let mut app = app(root);
    app.terminal_has_focus = true;
    app.idle_quiescent = true;
    app.workspace_watcher_active = true;
    for name in ["upper.vcd", "lower.vcd"] {
        let path = root.join(name);
        let dump = if name == "lower.vcd" {
            // Different signal content, with equal geometry for timing comparisons.
            String::from_utf8_lossy(VCD)
                .replace("wdata [31:0]", "other [31:0]")
                .into_bytes()
        } else {
            VCD.to_vec()
        };
        std::fs::write(&path, dump).unwrap();
        open(&mut app, &path);
        finish(&mut app);
    }
    app
}

fn stacked_waveforms(root: &Path, width: u16, height: u16) -> (App, Terminal<TestBackend>) {
    let mut app = waveform_pair(root);
    app.create_split_group_dir(app.active_tab, crate::app::SplitDirection::Vertical);
    assert!(app.split_active());
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    idle_until(&mut app, &mut terminal, |app| {
        app.extension_manager.records[0].tick_handlers.is_empty()
    });
    (app, terminal)
}

#[test]
fn waveform_wasm_tab_edge_drops_create_the_first_split_without_a_text_tab() {
    use crate::app::TabDropZone::{Bottom, Left, Right, Top};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    // Cover all four extension routing directions and both source positions
    // per split axis. Generic tab-drag tests cover their Cartesian product.
    for (source, edge, edge_name) in [
        (0, Left, "left"),
        (1, Right, "right"),
        (0, Top, "top"),
        (1, Bottom, "bottom"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut app = waveform_pair(root.path());
        let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
        idle_until(&mut app, &mut terminal, |app| {
            app.extension_manager.records[0].tick_handlers.is_empty()
        });
        assert_eq!(app.tabs.len(), 2);
        assert!(!app.split_active());
        assert!(
            app.tabs
                .iter()
                .all(|tab| tab.kind == crate::app::TabKind::ExtensionView)
        );
        let view = app.tabs[source].extension_view.clone().unwrap();
        let tab_bar = app.tab_bar_area.unwrap();
        let column = (tab_bar.x..tab_bar.right())
            .find(|&col| crate::mouse::find_tab_at_col(&app, col, tab_bar) == Some(source))
            .unwrap()
            + 2;
        let mouse = |app: &mut App, kind, (column, row)| {
            crate::mouse::handle_mouse(
                app,
                MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
            )
        };
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (column, tab_bar.y),
        );
        idle_until(&mut app, &mut terminal, |app| {
            app.extension_manager.records[0].tick_handlers.is_empty()
        });
        assert_eq!(app.dragging_tab, Some(source));
        assert!(
            app.extension_pointer_capture.is_none(),
            "the tab bar owns the gesture"
        );
        let area = app.editor_area.unwrap();
        let center = (area.x + area.width / 2, area.y + area.height / 2);
        let position = match edge {
            Left => (area.x + 1, center.1),
            Right => (area.right() - 2, center.1),
            Top => (center.0, area.y + 2),
            Bottom => (center.0, area.bottom() - 2),
        };
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), position);
        assert_eq!(app.tab_drop_target_group, Some(app.active_group));
        assert!(app.tab_drop_zone == Some(edge), "{edge_name} split target");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let highlight = match edge {
            Left => (area.x + area.width / 4, center.1),
            Right => (area.right() - area.width / 4, center.1),
            Top => (center.0, area.y + area.height / 4),
            Bottom => (center.0, area.bottom() - area.height / 4),
        };
        assert_eq!(
            terminal.backend().buffer()[highlight].bg,
            app.colors.ui.bg_editor_drop,
            "split preview {edge_name}"
        );
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), position);
        assert!(
            app.split_active(),
            "{edge_name}: releasing a waveform tab must create the first split"
        );
        assert!(app.dragging_tab.is_none());
        assert!(!app.drag_has_moved);
        assert!(app.tab_drop_zone.is_none());
        assert!(app.tab_drop_target_group.is_none());
        assert!(
            app.extension_manager.pending_view_events.is_empty(),
            "a tab drop must not send an unowned release into the waveform"
        );
        idle_until(&mut app, &mut terminal, |app| {
            app.extension_manager.records[0].tick_handlers.is_empty()
        });
        assert_eq!(app.groups.len(), 2);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_extension_view().unwrap().view_id, view.view_id);
        let dragged = app.code_area.unwrap();
        let other = app
            .groups
            .iter()
            .enumerate()
            .find(|(group, _)| *group != app.active_group)
            .unwrap()
            .1;
        assert_eq!(other.tabs.len(), 1);
        let target = other.code_area.unwrap();
        match edge {
            Left => assert!(dragged.right() <= target.x),
            Right => assert!(target.right() <= dragged.x),
            Top => assert!(dragged.bottom() <= target.y),
            Bottom => assert!(target.bottom() <= dragged.y),
        }
        for (meta, area) in [
            (app.active_extension_view().unwrap(), dragged),
            (other.tabs[0].extension_view.as_ref().unwrap(), target),
        ] {
            let frame = app
                .extension_manager
                .get_view(&meta.extension_id, &meta.view_id)
                .unwrap()
                .frame
                .unwrap();
            assert_eq!((frame.width, frame.height), (area.width, area.height));
        }
        assert!(app.extension_manager.records[0].last_error.is_none());
    }
}

#[test]
fn waveform_wasm_stacked_split_drag_uses_latest_position_and_both_views_settle() {
    use crossterm::event::{
        Event,
        MouseButton::Left,
        MouseEvent,
        MouseEventKind::{Down, Drag, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let (mut app, mut terminal) = stacked_waveforms(root.path(), 180, 70);
    let divider = app.split_dividers[0].0;
    let column = divider.x + divider.width / 2;
    let mouse = |kind, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
    crate::app_event_loop::handle_mouse_input(&mut app, &mut pump, mouse(Down(Left), divider.y));
    assert!(app.resizing_split);
    let final_row = divider.y + 8;
    for index in 0..240 {
        sender
            .send(Ok(Event::Mouse(mouse(
                Drag(Left),
                divider.y + (index % 12),
            ))))
            .unwrap();
    }
    sender
        .send(Ok(Event::Mouse(mouse(Drag(Left), final_row))))
        .unwrap();
    sender
        .send(Ok(Event::Mouse(mouse(Up(Left), final_row))))
        .unwrap();
    let Event::Mouse(first) = pump.try_recv().unwrap().unwrap() else {
        panic!("drag")
    };
    crate::app_event_loop::handle_mouse_input(&mut app, &mut pump, first);
    assert_eq!(
        app.mouse_row, final_row,
        "historical drag positions need no intermediate frames"
    );
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(
        app.split_dividers[0].0.y.abs_diff(final_row) <= 1,
        "split ratios round to terminal cells"
    );
    let Event::Mouse(release) = pump.try_recv().unwrap().unwrap() else {
        panic!("release")
    };
    assert_eq!(release.kind, Up(Left));
    crate::app_event_loop::handle_mouse_input(&mut app, &mut pump, release);
    assert!(!app.resizing_split);
    idle_until(&mut app, &mut terminal, |app| {
        app.extension_manager.records[0].tick_handlers.is_empty()
    });
    for group in 0..app.groups.len() {
        let (meta, area) = if group == app.active_group {
            (app.active_extension_view().unwrap(), app.code_area.unwrap())
        } else {
            let group = &app.groups[group];
            (
                group.tabs[group.active_tab]
                    .extension_view
                    .as_ref()
                    .unwrap(),
                group.code_area.unwrap(),
            )
        };
        let frame = app
            .extension_manager
            .get_view(&meta.extension_id, &meta.view_id)
            .unwrap()
            .frame
            .unwrap();
        assert_eq!((frame.width, frame.height), (area.width, area.height));
        let text = frame
            .lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let lower = frame.title.as_deref().unwrap().contains("lower.vcd");
        assert_eq!(
            text.contains("other"),
            lower,
            "each pane keeps its own signals"
        );
        assert_eq!(
            text.contains("wdata"),
            !lower,
            "each pane keeps its own signals"
        );
    }
    assert!(app.extension_manager.pending_view_events.is_empty());
    assert!(crate::extension_scheduler::next_wait(&app).is_none());
    assert!(
        app.notification_history
            .iter()
            .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
    );
}

#[test]
fn waveform_wasm_continuous_resize_renders_both_panes_without_an_extra_apply_turn() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _) = stacked_waveforms(root.path(), 180, 70);
    let mut ids: Vec<_> = app.extension_manager.records[0]
        .views
        .keys()
        .cloned()
        .collect();
    ids.sort();
    assert_eq!(ids.len(), 2);
    let mut published = [0; 2];
    for step in 0..4 {
        for (index, id) in ids.iter().enumerate() {
            let context = app.extension_editor_context_snapshot();
            assert!(
                app.extension_manager.queue_view_event(
                    "vtcode.waveform-viewer",
                    id,
                    crate::extension_wasm::ViewEventHandlerKind::Resize,
                    serde_json::json!({"kind":"resize","width":512,"height":80 + step + index})
                        .to_string(),
                    context,
                    String::new(),
                )
            );
            app.extension_manager
                .dispatch_view_events(&mut app.background);
            app.wait_for_background(Duration::from_secs(5), |app| {
                !app.extension_manager.has_busy_runtimes()
            });
        }
        assert!(
            !app.extension_manager.records[0]
                .tick_handlers
                .iter()
                .any(|handler| handler.export_name == "wave_apply"),
            "resizing loaded panes needs no deferred input-apply callback"
        );
        let before: Vec<_> = ids
            .iter()
            .map(|id| {
                app.extension_manager.records[0].views[id]
                    .frame
                    .clone()
                    .unwrap()
            })
            .collect();
        app.extension_manager
            .invoke_tick_handlers_async(
                "vtcode.waveform-viewer",
                vec!["wave_render".into()],
                &mut app.background,
                None,
            )
            .unwrap();
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.extension_manager.has_busy_runtimes()
        });
        for (index, id) in ids.iter().enumerate() {
            let frame = app.extension_manager.records[0].views[id]
                .frame
                .as_ref()
                .unwrap();
            published[index] += usize::from(!std::sync::Arc::ptr_eq(&before[index], frame));
        }
    }
    assert!(
        published.iter().all(|&count| count >= 2),
        "both panes update during continuous resizing: {published:?}"
    );
    finish(&mut app);
    assert!(
        app.notification_history
            .iter()
            .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
    );
}

#[test]
fn waveform_wasm_resize_of_fully_expanded_watch_limit_fits_the_ui_budget() {
    use crossterm::event::{MouseButton::Right, MouseEventKind::Down};
    use std::fmt::Write;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("expanded.vcd");
    let mut dump = String::from("$scope module top $end\n");
    for index in 0..256 {
        writeln!(dump, "$var wire 64 id{index} bus_{index:03} [63:0] $end").unwrap();
    }
    dump.push_str("$upscope $end\n$enddefinitions $end\n#0\n");
    for index in 0..256 {
        writeln!(dump, "b0 id{index}").unwrap();
    }
    dump.push_str("#100\n");
    std::fs::write(&path, dump).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    let watches: Vec<_> = (0..256)
        .map(|index| {
            serde_json::json!({
                "type": "Signal", "path": format!("top.bus_{index:03}[63:0]"), "bit": null,
                "radix": "Hex", "color": index % 8, "label": "", "expanded": true, "analog": false,
            })
        })
        .collect();
    assert!(app.paste_into_extension_editor(
        &serde_json::json!({"schema":1,"watches":watches}).to_string()
    ));
    drain(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("Session loaded"));
    for (width, height) in [(512, 200), (90, 12), (180, 30)] {
        resize(&mut app, width, height);
        let view = app.active_extension_view().unwrap();
        let frame = app
            .extension_manager
            .get_view(&view.extension_id, &view.view_id)
            .unwrap()
            .frame
            .unwrap();
        assert_eq!((frame.width, frame.height), (width, height));
    }
    app.code_area = Some(Rect::new(0, 0, 180, 30));
    waveform_mouse(&mut app, Down(Right), 45, 3);
    click_label(&mut app, "Fit signal names");
    assert!(saved_session(&mut app)["name_width"].as_u64().unwrap() < 28);
    waveform_mouse(&mut app, Down(Right), 45, 3);
    click_label(&mut app, "Fit values at A");
    assert_eq!(saved_session(&mut app)["value_width"], 11);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("16640 waves selected"));
    key(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
    assert!(text(&app).contains("16640 waves selected") && text(&app).contains("Hexadecimal"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('8'), KeyModifiers::NONE);
    assert!(
        saved_session(&mut app)["watches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["color"] == 7)
    );
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert!(
        saved_session(&mut app)["watches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        saved_session(&mut app)["watches"].as_array().unwrap().len(),
        256
    );
    assert!(
        app.notification_history
            .iter()
            .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
    );
}

/// cargo test --bin vtcode waveform_wasm_split_resize_benchmark -- --ignored --nocapture
#[test]
#[ignore = "manual split resize benchmark; timings are machine dependent"]
fn waveform_wasm_split_resize_benchmark() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for (width, height) in [(180, 60), (512, 160)] {
        let root = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = stacked_waveforms(root.path(), width, height);
        let divider = app.split_dividers[0].0;
        let column = divider.x + divider.width / 2;
        let start_row = divider.y;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: start_row,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.resizing_split);
        let mut draws = Vec::new();
        let mut ui_draws = Vec::new();
        let started = std::time::Instant::now();
        for index in 0..48 {
            let offset = ((index % 16) as i16 - 8) * 2;
            let row = start_row.saturating_add_signed(offset);
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
            );
            app.poll_background_results();
            let draw = std::time::Instant::now();
            terminal
                .draw(|frame| {
                    let ui_start = std::time::Instant::now();
                    crate::ui::draw(frame, &mut app);
                    ui_draws.push(ui_start.elapsed());
                })
                .unwrap();
            draws.push(draw.elapsed());
            crate::extension_scheduler::dispatch(&mut app);
            assert!(app.extension_manager.pending_view_events.len() <= 2);
        }
        let row = app.mouse_row;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
        let (settled, frames) = idle_until(&mut app, &mut terminal, |app| {
            app.extension_manager.records[0].tick_handlers.is_empty()
        });
        draws.sort();
        ui_draws.sort();
        eprintln!(
            "UI drawing median {:.2} ms",
            ui_draws[24].as_secs_f64() * 1000.0
        );
        eprintln!(
            "split resize {width}x{height}: draw median {:.2} ms, p95 {:.2} ms, total {:.2} ms; settle {:.2} ms / {frames} frames",
            draws[24].as_secs_f64() * 1000.0,
            draws[45].as_secs_f64() * 1000.0,
            started.elapsed().as_secs_f64() * 1000.0,
            settled.as_secs_f64() * 1000.0
        );
        assert!(
            app.notification_history
                .iter()
                .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
        );
    }
}

#[test]
fn waveform_wasm_dense_bus_loads_through_the_idle_scheduler_without_losing_values() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("dense.vcd");
    std::fs::write(&path, dense_vcd()).unwrap();
    let mut app = app(root.path());
    app.terminal_has_focus = true;
    app.idle_quiescent = true;
    let mut terminal = Terminal::new(TestBackend::new(180, 48)).unwrap();
    assert!(app.run_extension_command_with_optional_context(
        "vtcode.waveform-viewer",
        "wave.open",
        Some(&path)
    ));
    let (elapsed, frames) = idle_until(&mut app, &mut terminal, |app| {
        app.active_extension_view().is_some()
            && text(app).contains("ready")
            && app.extension_manager.records[0].tick_handlers.is_empty()
    });
    eprintln!("4.4 MiB dense bus: {elapsed:?}, {frames} workbench frames");
    assert!(text(&app).contains("200000"), "{}", text(&app));
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('9'), KeyModifiers::NONE);
    for _ in 0..4 {
        key(&mut app, KeyCode::Char('9'), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    // Inspect the final bus sample, not just a successful parser status.
    assert!(text(&app).contains("1_869f"), "{}", text(&app));
    assert!(
        app.notification_history
            .iter()
            .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
    );
}

/// Compare real Wasm, document I/O, production scheduling, and terminal drawing.
/// Run with cargo test waveform_wasm_loading_benchmark -- --ignored --nocapture.
#[test]
#[ignore = "manual loading benchmark; timings are machine dependent"]
fn waveform_wasm_loading_benchmark() {
    let dense = dense_vcd();
    let mut comments = b"$comment ".to_vec();
    comments.resize(8 * 1024 * 1024, b' ');
    comments.extend_from_slice(b" $end\n");
    comments.extend_from_slice(VCD);
    for (name, data) in [
        ("demo", VCD),
        ("dense_bus", dense.as_slice()),
        ("comments", comments.as_slice()),
    ] {
        let mut times = Vec::new();
        for _ in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(format!("{name}.vcd"));
            std::fs::write(&path, data).unwrap();
            let mut app = app(root.path());
            app.terminal_has_focus = true;
            app.idle_quiescent = true;
            let mut terminal = Terminal::new(TestBackend::new(180, 48)).unwrap();
            let start = std::time::Instant::now();
            assert!(app.run_extension_command_with_optional_context(
                "vtcode.waveform-viewer",
                "wave.open",
                Some(&path)
            ));
            let (_, frames) = idle_until(&mut app, &mut terminal, |app| {
                app.active_extension_view().is_some()
                    && text(app).contains("ready")
                    && app.extension_manager.records[0].tick_handlers.is_empty()
            });
            times.push((start.elapsed(), frames));
            assert!(
                app.notification_history
                    .iter()
                    .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
            );
        }
        times.sort_by_key(|(elapsed, _)| *elapsed);
        let (elapsed, frames) = times[1];
        eprintln!(
            "loading benchmark {name}: {:.2} ms, {frames} frames, {} bytes",
            elapsed.as_secs_f64() * 1000.0,
            data.len()
        );
    }
}

#[test]
fn waveform_wasm_demo_loads_and_repaints_through_the_idle_workbench_scheduler() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("idle-demo.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    app.terminal_has_focus = true;
    app.idle_quiescent = true;
    app.workspace_watcher_active = true;
    let mut terminal = Terminal::new(TestBackend::new(180, 48)).unwrap();
    assert!(app.run_extension_command_with_optional_context(
        "vtcode.waveform-viewer",
        "wave.open",
        Some(&path)
    ));
    let (elapsed, frames) = idle_until(&mut app, &mut terminal, |app| {
        app.active_extension_view().is_some()
            && app.extension_manager.records[0].tick_handlers.is_empty()
            && text(app).contains("ready")
    });
    eprintln!("demo loaded while idle: {elapsed:?}, {frames} workbench frames");
    assert!(
        !app.notification_history
            .iter()
            .any(|(_, level, _)| *level == crate::app::NotifyLevel::Error),
        "{:?}",
        app.notification_history
    );
    assert!(crate::extension_scheduler::next_wait(&app).is_none());
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        rendered.contains("ready"),
        "the completed frame is repainted without input"
    );

    // Repeated workbench draws while a resize is outstanding must not enqueue
    // another resize. A subsequent viewport change must still reach the guest.
    let original = app.code_area.unwrap();
    terminal.backend_mut().resize(150, 38);
    for _ in 0..20 {
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(app.extension_manager.pending_view_events.len(), 1);
    }
    let (_, resize_frames) = idle_until(&mut app, &mut terminal, |app| {
        let meta = app.active_extension_view().unwrap();
        let frame = app
            .extension_manager
            .get_view(&meta.extension_id, &meta.view_id)
            .unwrap()
            .frame
            .unwrap();
        let area = app.code_area.unwrap();
        (frame.width, frame.height) == (area.width, area.height)
            && app.extension_manager.records[0].tick_handlers.is_empty()
    });
    let resized = app.code_area.unwrap();
    assert_ne!(
        (resized.width, resized.height),
        (original.width, original.height)
    );
    assert!(resize_frames <= 4, "{resize_frames} redraws for one resize");

    // A published frame is the redraw boundary: capture/apply callbacks that
    // only queue work must not repaint the old frame in between.
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        crossterm::event::KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
    );
    let (response, frames) = idle_until(&mut app, &mut terminal, |app| {
        text(app).contains("Marker name")
            && app.extension_manager.records[0].tick_handlers.is_empty()
    });
    eprintln!("marker dialog: {response:?}, {frames} workbench frames");
    assert!(frames <= 3, "{frames} redraws for one input");
    assert!(
        app.notification_history
            .iter()
            .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
    );
    assert!(
        !app.poll_background_results(),
        "a settled viewer stops repainting"
    );

    // The canvas is capped while the frame reports the actual viewport size.
    // Oversized terminals still settle after one resize without more input.
    terminal.backend_mut().resize(600, 240);
    let (_, frames) = idle_until(&mut app, &mut terminal, |app| {
        let meta = app.active_extension_view().unwrap();
        let frame = app
            .extension_manager
            .get_view(&meta.extension_id, &meta.view_id)
            .unwrap()
            .frame
            .unwrap();
        let area = app.code_area.unwrap();
        (frame.width, frame.height) == (area.width, area.height)
            && frame.lines.len() <= 200
            && app.extension_manager.records[0].tick_handlers.is_empty()
    });
    assert!(frames <= 4, "{frames} redraws for a capped surface");
    assert!(crate::extension_scheduler::next_wait(&app).is_none());
}

#[test]
fn waveform_wasm_loading_tasks_and_mouse_input_share_the_idle_scheduler() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mouse-stream.vcd");
    let mut data = b"$comment ".to_vec();
    data.extend(std::iter::repeat_n(b' ', 2 * 1024 * 1024));
    data.extend_from_slice(b" $end\n");
    data.extend_from_slice(VCD);
    std::fs::write(&path, data).unwrap();
    let mut app = app(root.path());
    app.terminal_has_focus = true;
    app.workspace_watcher_active = true;
    let mut terminal = Terminal::new(TestBackend::new(512, 200)).unwrap();
    assert!(app.run_extension_command_with_optional_context(
        "vtcode.waveform-viewer",
        "wave.open",
        Some(&path)
    ));
    let mut moves = 0;
    let mut seen_task = false;
    let mut captured_during_load = false;
    let (elapsed, frames) = idle_with_input(
        &mut app,
        &mut terminal,
        |app| {
            app.active_extension_view().is_some()
                && text(app).contains("ready")
                && app.extension_manager.records[0].tick_handlers.is_empty()
        },
        |app| {
            if app.active_extension_view().is_some() && !text(app).contains("ready") {
                let area = app.code_area.unwrap();
                moves += 1;
                crate::mouse::handle_mouse(
                    app,
                    MouseEvent {
                        kind: MouseEventKind::Moved,
                        column: area.x + area.width / 2 + (moves % 10) as u16,
                        row: area.y + 5,
                        modifiers: KeyModifiers::NONE,
                    },
                );
                let task_turn = app
                    .extension_manager
                    .tick_yield_to_input
                    .contains("vtcode.waveform-viewer");
                captured_during_load |= seen_task && !task_turn;
                seen_task |= task_turn;
                assert!(app.extension_manager.pending_view_events.len() <= 2);
            }
        },
    );
    eprintln!("2 MiB load under {moves} mouse moves: {elapsed:?}, {frames} workbench frames");
    assert!(
        moves > 0 && captured_during_load,
        "input and tasks both advanced during loading"
    );
    assert!(
        app.notification_history
            .iter()
            .all(|(_, level, _)| *level != crate::app::NotifyLevel::Error)
    );
    assert!(crate::extension_scheduler::next_wait(&app).is_none());
}

#[test]
fn waveform_wasm_query_dialogs_apply_cancel_and_validate_through_real_mouse_input() {
    use crossterm::event::{
        MouseButton::Left,
        MouseEventKind::{Down, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queries.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 32);
    app.code_area = Some(Rect::new(5, 4, 150, 32));
    let original = saved_session(&mut app);
    click_label(&mut app, " Group ");
    assert!(text(&app).contains("New group"));
    assert!(text(&app).contains("Cancel"));
    let view = app.active_extension_view().unwrap();
    let frame = app
        .extension_manager
        .get_view(&view.extension_id, &view.view_id)
        .unwrap()
        .frame
        .unwrap();
    assert_eq!(
        frame.cursor.unwrap().1,
        15,
        "input is in the centered dialog"
    );
    click_label(&mut app, " Create ");
    assert!(text(&app).contains("Enter a group name"));
    assert!(app.paste_into_extension_editor("replace this"));
    drain(&mut app);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(app.paste_into_extension_editor("AXI control"));
    drain(&mut app);
    click_label(&mut app, " Create ");
    assert!(!text(&app).contains("New group"));
    let created = saved_session(&mut app);
    assert_eq!(created["watches"][0]["name"], "AXI control");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), original);

    click_label(&mut app, " Mark ");
    assert!(app.paste_into_extension_editor("discard me"));
    drain(&mut app);
    click_label(&mut app, " Cancel ");
    assert_eq!(saved_session(&mut app), original);
    click_label(&mut app, " Mark ");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!text(&app).contains("Marker name"));
    assert_eq!(saved_session(&mut app), original);
    click_label(&mut app, " Mark ");
    // Outside dismissal consumes the click, including its captured release.
    waveform_mouse(&mut app, Down(Left), 110, 9);
    waveform_mouse(&mut app, Up(Left), 110, 9);
    assert!(!text(&app).contains("Marker name"));
    assert_eq!(saved_session(&mut app), original);
    click_label(&mut app, " Mark ");
    assert!(app.paste_into_extension_editor("Handshake"));
    drain(&mut app);
    click_label(&mut app, " Create ");
    assert_eq!(saved_session(&mut app)["markers"][0]["name"], "Handshake");
    assert_eq!(std::fs::read(&path).unwrap(), VCD);
}

#[test]
fn waveform_wasm_signal_context_menu_is_opt_in_and_changes_the_clicked_signal() {
    use crossterm::event::{
        MouseButton::{Left, Right},
        MouseEventKind::{Down, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("context.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 32);
    app.code_area = Some(Rect::new(5, 4, 150, 32));
    let original = saved_session(&mut app);
    let rendered = text(&app);
    let row = rendered
        .lines()
        .enumerate()
        .skip(5)
        .find(|(_, line)| {
            line.chars()
                .skip(30)
                .take(27)
                .collect::<String>()
                .contains("wdata")
        })
        .unwrap()
        .0 as u16;
    app.active_pane = crate::app::ActivePane::Explorer;
    waveform_mouse(&mut app, Down(Right), 5 + 45, 4 + row);
    assert_eq!(app.active_pane, crate::app::ActivePane::Editor);
    assert!(
        app.extension_pointer_capture.is_none(),
        "right-click never begins a left gesture"
    );
    assert!(
        !app.context_menu_active,
        "the native menu belongs to the extension"
    );
    assert!(text(&app).contains("Hexadecimal") && text(&app).contains("Analog plot"));
    click_label(&mut app, "Signed decimal");
    let changed = saved_session(&mut app);
    let watches = changed["watches"].as_array().unwrap();
    let target = watches
        .iter()
        .find(|watch| {
            watch["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("wdata[31:0]"))
        })
        .unwrap();
    assert_eq!(target["radix"], "Signed");
    assert_eq!(watches[0]["radix"], original["watches"][0]["radix"]);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), original);
    key(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
    assert!(text(&app).contains("Hexadecimal"));
    for _ in 0..7 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let changed = saved_session(&mut app);
    assert!(changed["watches"].as_array().unwrap().iter().any(|watch| {
        watch["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("wdata[31:0]"))
            && watch["analog"] == true
    }));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    waveform_mouse(&mut app, Down(Right), 5 + 45, 4 + row);
    click_label(&mut app, "Rename…");
    assert!(text(&app).contains("Set a display name"));
    assert!(app.paste_into_extension_editor("Write payload"));
    drain(&mut app);
    click_label(&mut app, " Rename ");
    assert!(text(&app).contains("Write payload"));

    let view_id = app.active_extension_view().unwrap().view_id.clone();
    app.extension_manager.records[0]
        .views
        .get_mut(&view_id)
        .unwrap()
        .context_menu_handler = None;
    assert!(!app.handle_active_extension_view_context_menu(50, 4 + row, KeyModifiers::NONE));
    assert!(
        app.extension_manager.pending_view_events.is_empty(),
        "unregistered right clicks keep their previous routing"
    );
    // Normal left selection still follows the existing mouse contract.
    waveform_mouse(&mut app, Down(Left), 115, 9);
    waveform_mouse(&mut app, Up(Left), 115, 9);
    assert!(!text(&app).contains("Hexadecimal"));
}

#[test]
fn waveform_wasm_drag_badge_follows_pointer_and_clears_after_cancellation() {
    use crossterm::event::{
        MouseButton::Left,
        MouseEventKind::{Down, Drag},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("badge.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 32);
    let (x, y) = label_point(&app, "wdata[31:0]");
    waveform_mouse(&mut app, Down(Left), x, y);
    waveform_mouse(&mut app, Drag(Left), 85, 8);
    assert!(
        text(&app)
            .lines()
            .nth(10)
            .unwrap()
            .contains("Move  wdata[31:0]")
    );
    waveform_mouse(&mut app, Drag(Left), 8, 3);
    assert!(
        text(&app)
            .lines()
            .nth(5)
            .unwrap()
            .contains("Cannot drop  wdata[31:0]")
    );
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!text(&app).contains("Cannot drop"));
    assert!(app.extension_pointer_capture.is_none());
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
}

#[test]
fn waveform_wasm_per_signal_sizes_round_trip_without_a_global_density_control() {
    use crossterm::event::{MouseButton::Right, MouseEventKind::Down};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sizes.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 32);
    app.code_area = Some(Rect::new(0, 0, 150, 32));
    let toolbar = text(&app).lines().nth(1).unwrap().to_string();
    assert!(!toolbar.contains("Compact") && !toolbar.contains("Roomy"));
    let original = saved_session(&mut app);
    assert_eq!(original["watches"][0]["size"], "Normal");
    assert_eq!(original["watches"][4]["size"], "Small");
    assert!(original.get("density").is_none());
    waveform_mouse(&mut app, Down(Right), 45, 5);
    assert!(text(&app).contains("Waveform size"));
    click_label(&mut app, "Small");
    let changed = saved_session(&mut app);
    assert_eq!(changed["watches"][0]["size"], "Small");
    assert_eq!(
        &changed["watches"].as_array().unwrap()[1..],
        &original["watches"].as_array().unwrap()[1..],
        "the clicked signal alone changes size"
    );
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), original);
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), changed);
    app.force_close_tab(app.active_tab);
    drain(&mut app);
    open(&mut app, &path);
    finish(&mut app);
    assert_eq!(
        saved_session(&mut app),
        changed,
        "per-wave sizes survive reopening"
    );
    key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(
        saved_session(&mut app),
        original,
        "h changes only the selected wave"
    );
}

#[test]
fn waveform_wasm_legacy_sessions_default_all_single_bits_to_normal() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy-sizes.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 32);
    let original = saved_session(&mut app);
    let mut scalar = original["watches"][0].clone();
    let mut bus = original["watches"][7].clone();
    bus["expanded"] = true.into();
    let mut detached_bit = bus.clone();
    detached_bit["bit"] = 1.into();
    detached_bit["expanded"] = false.into();
    detached_bit["label"] = "state bit 1".into();
    for watch in [&mut scalar, &mut bus, &mut detached_bit] {
        let fields = watch.as_object_mut().unwrap();
        fields.remove("size");
        fields.remove("bit_size");
        fields.remove("bit_sizes");
    }
    let legacy = serde_json::json!({
        "schema": 1, "density": "Compact", "span": 1000,
        "watches": [scalar, bus, detached_bit],
    });
    assert!(app.paste_into_extension_editor(&legacy.to_string()));
    drain(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let migrated = saved_session(&mut app);
    assert_eq!(migrated["schema"], 2);
    assert_eq!(migrated["watches"][0]["size"], "Normal");
    assert_eq!(migrated["watches"][1]["size"], "Small");
    assert!(migrated["watches"][1].get("bit_size").is_none());
    assert_eq!(migrated["watches"][2]["size"], "Normal");
    let names = signal_names_column(&app);
    assert_eq!(
        names
            .iter()
            .map(|name| name.split_whitespace().next().unwrap())
            .collect::<Vec<_>>(),
        [
            "clk",
            "state[2:0]",
            "state[2]",
            "state[1]",
            "state[0]",
            "state[1]"
        ]
    );
    let rendered = text(&app);
    assert!(
        rendered
            .lines()
            .nth(6)
            .unwrap()
            .chars()
            .skip(73)
            .any(|c| c == '┘')
    );
    app.force_close_tab(app.active_tab);
    drain(&mut app);
    open(&mut app, &path);
    finish(&mut app);
    assert_eq!(saved_session(&mut app), migrated);
}

#[test]
fn waveform_wasm_multiselection_bulk_edits_delete_and_undo_use_real_host_input() {
    use crossterm::event::{
        MouseButton::{Left, Right},
        MouseEventKind::{Down, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("selection.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 180, 40);
    app.code_area = Some(Rect::new(4, 3, 180, 40));
    let original = saved_session(&mut app);
    let click = |app: &mut App, label: &str, modifiers: KeyModifiers| {
        let (x, y) = label_point(app, label);
        waveform_mouse_with_modifiers(app, Down(Left), x, y, modifiers);
        waveform_mouse_with_modifiers(app, Up(Left), x, y, modifiers);
    };
    let context = |app: &mut App, label: &str| {
        let (x, y) = label_point(app, label);
        waveform_mouse(app, Down(Right), x, y);
    };
    click(&mut app, "address[31:0]", KeyModifiers::NONE);
    click(&mut app, "rdata[31:0]", KeyModifiers::CONTROL);
    assert!(text(&app).contains("2 selected"));
    context(&mut app, "address[31:0]");
    assert!(text(&app).contains("2 waves selected"));
    click_label(&mut app, "Unsigned decimal");
    let decimal = saved_session(&mut app);
    assert_eq!(decimal["watches"][4]["radix"], "Unsigned");
    assert_eq!(decimal["watches"][6]["radix"], "Unsigned");
    for i in 0..12 {
        if ![4, 6].contains(&i) {
            assert_eq!(decimal["watches"][i], original["watches"][i]);
        }
    }
    context(&mut app, "rdata[31:0]");
    // Bring the lower part of the menu into view using its keyboard navigation.
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    click_label(&mut app, "Color ›");
    click_label(&mut app, "Pink");
    let colored = saved_session(&mut app);
    assert_eq!(colored["watches"][4]["color"], 4);
    assert_eq!(colored["watches"][6]["color"], 4);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        saved_session(&mut app),
        decimal,
        "one undo restores both colors"
    );
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        saved_session(&mut app),
        original,
        "one undo restores both number formats"
    );

    click(&mut app, "address[31:0]", KeyModifiers::NONE);
    click(&mut app, "rdata[31:0]", KeyModifiers::SHIFT);
    assert!(text(&app).contains("3 selected"));
    key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    assert!(text(&app).contains("4 selected"));
    key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
    assert!(text(&app).contains("3 selected"));
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    let deleted = saved_session(&mut app);
    assert_eq!(deleted["watches"].as_array().unwrap().len(), 9);
    let expected: Vec<_> = original["watches"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(i, _)| !(4..=6).contains(i))
        .map(|(_, w)| w.clone())
        .collect();
    assert_eq!(deleted["watches"], serde_json::json!(expected));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), original);
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), deleted);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);

    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("12 selected"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!text(&app).contains("12 selected"));
    click(&mut app, "wdata[31:0]", KeyModifiers::NONE);
    click(&mut app, "rdata[31:0]", KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('8'), KeyModifiers::NONE);
    let final_session = saved_session(&mut app);
    app.force_close_tab(app.active_tab);
    drain(&mut app);
    open(&mut app, &path);
    finish(&mut app);
    assert_eq!(saved_session(&mut app), final_session);
    assert_eq!(std::fs::read(&path).unwrap(), VCD);
}

#[test]
fn waveform_wasm_columns_cursor_handles_and_snap_use_real_mouse_and_persist() {
    use crossterm::event::{
        MouseButton::{Left, Right},
        MouseEventKind::{Down, Drag, Moved, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("controls.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 32);
    app.code_area = Some(Rect::new(5, 4, 150, 32));
    let original = saved_session(&mut app);
    let divider = text(&app)
        .lines()
        .nth(3)
        .unwrap()
        .chars()
        .enumerate()
        .filter(|(_, c)| *c == '│')
        .map(|(x, _)| x as u16)
        .last()
        .unwrap()
        + 5;
    waveform_mouse(&mut app, Moved, divider, 7);
    assert!(text(&app).contains('↔'));
    waveform_mouse(&mut app, Down(Left), divider, 7);
    waveform_mouse(&mut app, Drag(Left), divider + 16, 7);
    waveform_mouse(&mut app, Up(Left), divider + 16, 7);
    let resized = saved_session(&mut app);
    assert_eq!(resized["value_width"], 29);
    assert_eq!(resized["cursor"], original["cursor"]);
    assert_eq!(resized["watches"], original["watches"]);
    assert!(app.extension_pointer_capture.is_none());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), original);
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), resized);
    click_label(&mut app, "snap on");
    assert_eq!(saved_session(&mut app)["snap"], false);
    key(&mut app, KeyCode::Char('B'), KeyModifiers::SHIFT);
    let before_cursor = saved_session(&mut app);
    assert!(text(&app).contains("[A]") && text(&app).contains("[B]"));
    let (x, y) = label_point(&app, "[B]");
    waveform_mouse(&mut app, Down(Left), x, y);
    waveform_mouse(&mut app, Drag(Left), x + 10, y + 2);
    waveform_mouse(&mut app, Up(Left), x + 10, y + 2);
    let positioned = saved_session(&mut app);
    assert_eq!(positioned["cursor"], before_cursor["cursor"]);
    assert_ne!(positioned["alt_cursor"], before_cursor["alt_cursor"]);
    assert!(app.extension_pointer_capture.is_none());
    let (x, y) = label_point(&app, "[B]");
    waveform_mouse(&mut app, Down(Left), x, y);
    waveform_mouse(&mut app, Drag(Left), x + 8, y);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(saved_session(&mut app), positioned);
    waveform_mouse(&mut app, Down(Right), 50, 7);
    assert!(text(&app).contains("Fit values at A"));
    click_label(&mut app, "Reset column widths");
    assert_eq!(saved_session(&mut app)["value_width"], 13);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), positioned);
    app.force_close_tab(app.active_tab);
    drain(&mut app);
    open(&mut app, &path);
    finish(&mut app);
    assert_eq!(saved_session(&mut app), positioned);
    assert_eq!(std::fs::read(&path).unwrap(), VCD);
}

#[test]
fn waveform_wasm_scrollbars_and_small_menu_shortcuts_preserve_waveform_state() {
    use crossterm::event::{
        MouseButton::{Left, Right},
        MouseEventKind::{Down, Drag, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("scrollbars.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 18);
    app.code_area = Some(Rect::new(0, 0, 150, 18));
    let original = saved_session(&mut app);
    waveform_mouse(&mut app, Down(Left), 70, 5);
    waveform_mouse(&mut app, Drag(Left), 70, 14);
    waveform_mouse(&mut app, Up(Left), 70, 14);
    assert!(!signal_names_column(&app).iter().any(|name| name == "clk"));
    assert!(text(&app).contains("voltage"));
    assert_eq!(saved_session(&mut app), original);
    waveform_mouse(&mut app, Down(Left), 70, 14);
    waveform_mouse(&mut app, Drag(Left), 70, 5);
    waveform_mouse(&mut app, Up(Left), 70, 5);
    assert!(signal_names_column(&app).iter().any(|name| name == "clk"));
    waveform_mouse(&mut app, Down(Left), 29, 5);
    waveform_mouse(&mut app, Drag(Left), 29, 14);
    waveform_mouse(&mut app, Up(Left), 29, 14);
    assert!(!text(&app).lines().nth(5).unwrap().starts_with(" ▾ tb"));
    assert_eq!(saved_session(&mut app), original);
    let (x, y) = label_point(&app, "address[31:0]");
    waveform_mouse(&mut app, Down(Right), x, y);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(saved_session(&mut app)["watches"][4]["radix"], "Signed");
    let (x, y) = label_point(&app, "address[31:0]");
    waveform_mouse(&mut app, Down(Right), x, y);
    key(&mut app, KeyCode::Char('8'), KeyModifiers::NONE);
    assert_eq!(saved_session(&mut app)["watches"][4]["color"], 7);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(saved_session(&mut app), original);
    assert!(app.extension_pointer_capture.is_none());
}

#[test]
fn waveform_wasm_browser_drag_previews_inserts_and_undoes_via_real_mouse_routing() {
    use crossterm::event::{
        MouseButton::Left,
        MouseEventKind::{Down, Drag, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("drag.vcd");
    std::fs::write(&path, b"$timescale 1 ns $end $scope module top $end $var wire 1 ! clk $end $var wire 1 # reset $end $var wire 32 $ data [31:0] $end $var wire 1 % valid $end $upscope $end $enddefinitions $end #0 0! 0# b01100000000100001000000000000000 $ 0% #100 1! 1# b01100000000000001110111101000000 $ 1% #1000").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    let session = serde_json::json!({
        "schema": 1, "cursor": 240, "alt_cursor": 420, "span": 1000, "density": "Roomy",
        "watches": (["top.clk", "top.reset", "top.valid"].map(|path| serde_json::json!({
            "type": "Signal", "path": path, "bit": null, "radix": "Hex",
            "color": 0, "label": "", "expanded": false, "analog": false,
        }))),
    });
    assert!(app.paste_into_extension_editor(&session.to_string()));
    drain(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    for ch in "top.".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    resize(&mut app, 150, 38);
    app.code_area = Some(Rect::new(5, 4, 150, 38));
    let before = signal_names_column(&app);
    let measurement = text(&app).lines().nth(2).unwrap().to_owned();
    waveform_mouse(&mut app, Down(Left), 11, 11); // All signals: data, view row 7
    assert!(app.extension_pointer_capture.is_some());
    assert_eq!(
        signal_names_column(&app),
        before,
        "press alone does not add"
    );
    // A burst remains one queued motion and uses the latest position.
    for column in 80..125 {
        crate::mouse::handle_mouse(
            &mut app,
            crossterm::event::MouseEvent {
                kind: Drag(Left),
                column,
                row: 13,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.extension_manager.pending_view_events.len(), 1);
    }
    drain(&mut app); // before valid, view row 9
    let preview = text(&app);
    assert!(preview.contains("↓ data[31:0]"), "{preview}");
    assert!(
        preview.contains("Release to add top.data[31:0] before top.valid"),
        "{preview}"
    );
    assert!(
        preview.contains("Signals · 3"),
        "preview is not a saved edit"
    );
    assert_eq!(preview.lines().nth(2).unwrap(), measurement);
    waveform_mouse(&mut app, Up(Left), 100, 13);
    assert!(app.extension_pointer_capture.is_none());
    let placed = signal_names_column(&app);
    assert_eq!(
        placed.iter().position(|n| n.contains("data[31:0]")),
        Some(2)
    );
    assert_eq!(placed.iter().position(|n| n.contains("valid")), Some(3));
    assert!(text(&app).contains("Signals · 4"));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        signal_names_column(&app),
        before,
        "one undo removes the entire insertion"
    );

    // Outside releases retain routing but cancel the proposed insertion.
    waveform_mouse(&mut app, Down(Left), 11, 11);
    waveform_mouse(&mut app, Drag(Left), 100, 13);
    waveform_mouse(&mut app, Up(Left), 1, 1);
    assert!(app.extension_pointer_capture.is_none());
    assert_eq!(signal_names_column(&app), before);
    assert!(!text(&app).contains("Release to add"));
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());

    // Escape cancels capture before the key is delivered, and a later release
    // cannot resurrect the cancelled preview.
    waveform_mouse(&mut app, Down(Left), 11, 11);
    waveform_mouse(&mut app, Drag(Left), 100, 13);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.extension_pointer_capture.is_none());
    waveform_mouse(&mut app, Up(Left), 100, 13);
    assert_eq!(signal_names_column(&app), before);
    assert!(!app.notification_history.iter().any(|(message, _, _)| message.contains("out of fuel") || message.contains("wasm trap")));
}

#[test]
fn waveform_wasm_pointer_capture_cancels_on_resize_and_is_opt_in() {
    use crossterm::event::{
        MouseButton::Left,
        MouseEventKind::{Down, Drag, Up},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("capture.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 38);
    app.code_area = Some(Rect::new(5, 4, 150, 38));
    waveform_mouse(&mut app, Down(Left), 100, 11);
    waveform_mouse(&mut app, Drag(Left), 110, 11);
    assert!(app.extension_pointer_capture.is_some());
    resize(&mut app, 80, 24);
    app.code_area = Some(Rect::new(5, 4, 80, 24));
    waveform_mouse(&mut app, Up(Left), 110, 11);
    assert!(app.extension_pointer_capture.is_none());
    assert!(text(&app).contains("Drag cancelled"));
    let view_id = app.active_extension_view().unwrap().view_id.clone();
    let frame = app
        .extension_manager
        .get_view("vtcode.waveform-viewer", &view_id)
        .unwrap()
        .frame
        .unwrap();
    assert_eq!(
        (frame.width, frame.height),
        (80, 24),
        "cancel must preserve the latest resize"
    );
    app.extension_manager.records[0]
        .views
        .get_mut(&view_id)
        .unwrap()
        .pointer_capture = false;
    waveform_mouse(&mut app, Down(Left), 50, 10);
    assert!(
        app.extension_pointer_capture.is_none(),
        "legacy routing is unchanged without opt-in"
    );
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
}

#[test]
fn waveform_wasm_loads_and_responds_to_real_workbench_input_without_touching_source() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("demo.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    assert!(text(&app).contains("Reading waveform"));
    finish(&mut app);
    assert!(text(&app).contains("15 signals"));
    assert!(text(&app).contains("clk"));
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert!(text(&app).contains("A 10 ns"));
    key(&mut app, KeyCode::Char('+'), KeyModifiers::NONE);
    let (start, end) = window_ns(&app);
    assert_eq!(end - start, 500.0);
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("Go to time"));
    assert!(app.paste_into_extension_editor("240 ns"));
    drain(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("A 240 ns"));
    key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(app.paste_into_extension_editor("Handshake"));
    drain(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("session saved"));
    assert_eq!(std::fs::read(&path).unwrap(), VCD);
    assert!(!app.tabs[app.active_tab].has_unsaved_changes());
    assert!(
        app.extension_manager.records[0]
            .runtime
            .as_ref()
            .unwrap()
            .store
            .data()
            .documents
            .iter()
            .all(|d| !d.info().dirty)
    );
}

#[test]
fn waveform_wasm_fst_is_decoded_on_a_background_worker() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("gcd.fst");
    std::fs::write(
        &path,
        include_bytes!("../tests/fixtures/waveform-viewer/gcd.fst"),
    )
    .unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    assert!(text(&app).contains("FST"));
    assert!(text(&app).contains("ready"), "{}", text(&app));
    assert!(!text(&app).contains("Reading waveform"));
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
    let commands = app.extension_manager.file_open_commands_for_path(&path);
    assert_eq!(commands[0].command_id, "wave.open");
}

#[test]
fn waveform_wasm_resize_and_palette_render_on_wide_narrow_and_tiny_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("demo.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    for (width, height) in [
        (150, 38),
        (100, 28),
        (80, 24),
        (48, 16),
        (16, 5),
        (1, 1),
        (240, 75),
        (512, 200),
    ] {
        resize(&mut app, width, height);
        assert!(
            app.extension_manager.records[0].last_error.is_none(),
            "{width}×{height}: {:?}",
            app.extension_views()
        );
        let view = app.active_extension_view().unwrap();
        let frame = app
            .extension_manager
            .get_view(&view.extension_id, &view.view_id)
            .unwrap()
            .frame
            .unwrap();
        assert_eq!(
            (frame.width, frame.height),
            (width, height),
            "{:?}",
            app.notification_history
        );
    }
    resize(&mut app, 150, 38);
    assert!(app.handle_extension_view_scroll_input(110, 8, -1, KeyModifiers::CONTROL, "vertical"));
    drain(&mut app);
    let (start, end) = window_ns(&app);
    assert_eq!(end - start, 500.0);
    assert!(app.handle_extension_view_scroll_input(110, 8, 1, KeyModifiers::NONE, "horizontal"));
    drain(&mut app);
    let (panned_start, panned_end) = window_ns(&app);
    assert!(panned_start > start);
    assert_eq!(panned_end - panned_start, 500.0);
    crate::mouse::handle_mouse(
        &mut app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollLeft,
            column: 110,
            row: 8,
            modifiers: KeyModifiers::CONTROL,
        },
    );
    drain(&mut app);
    let (start, end) = window_ns(&app);
    assert_eq!(end - start, 250.0);
    for column in [8, 40, 110] {
        for (kind, expected_start) in [
            (crossterm::event::MouseEventKind::ScrollDown, start + 50.0),
            (crossterm::event::MouseEventKind::ScrollUp, start),
        ] {
            crate::mouse::handle_mouse(
                &mut app,
                crossterm::event::MouseEvent {
                    kind,
                    column,
                    row: 8,
                    modifiers: KeyModifiers::SHIFT,
                },
            );
            drain(&mut app);
            assert_eq!(window_ns(&app), (expected_start, expected_start + 250.0));
        }
    }
    assert!(app.handle_extension_view_scroll_input(
        110,
        8,
        1,
        KeyModifiers::SHIFT | KeyModifiers::ALT,
        "vertical"
    ));
    drain(&mut app);
    assert_eq!(window_ns(&app), (start + 50.0, end + 50.0));
    assert!(app.extension_manager.records[0].last_error.is_none());
}

#[test]
fn waveform_wasm_ruler_range_preview_overview_drag_and_scrollable_help_work_through_mouse_input() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("navigation.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 24);
    key(&mut app, KeyCode::Char('+'), KeyModifiers::NONE);
    let original = window_ns(&app);
    waveform_mouse_with_modifiers(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        95,
        4,
        KeyModifiers::SHIFT,
    );
    waveform_mouse_with_modifiers(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        130,
        4,
        KeyModifiers::SHIFT,
    );
    assert_eq!(window_ns(&app), original);
    assert!(text(&app).contains("Zoom range:"));
    assert!(text(&app).contains("Release to zoom"));
    waveform_mouse_with_modifiers(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        130,
        4,
        KeyModifiers::SHIFT,
    );
    let (start, end) = window_ns(&app);
    let span = end - start;
    assert!(span < original.1 - original.0 && span > 0.0);
    assert!(!text(&app).contains("Zoom range:"));

    waveform_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 149, 21);
    assert_eq!(window_ns(&app), (1000.0 - span, 1000.0));
    waveform_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 71, 21);
    assert_eq!(window_ns(&app), (0.0, span));
    waveform_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 71, 25);
    assert!(app.extension_pointer_capture.is_none());
    assert_eq!(window_ns(&app), (0.0, span));
    let saved = window_ns(&app);
    waveform_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 149, 21);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        window_ns(&app),
        saved,
        "Escape restores the previous overview window"
    );

    waveform_mouse_with_modifiers(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        115,
        4,
        KeyModifiers::ALT,
    );
    waveform_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 115, 4);
    assert!(!text(&app).contains("B —"), "the ruler can place cursor B");
    key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(text(&app).contains("WAVEFORM EXPLORER"));
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    assert!(text(&app).contains("scrollbar thumbs"));
    waveform_mouse_with_modifiers(
        &mut app,
        MouseEventKind::ScrollUp,
        110,
        8,
        KeyModifiers::CONTROL,
    );
    assert!(text(&app).contains("WAVEFORM EXPLORER"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(window_ns(&app), saved);
    assert!(app.extension_manager.records[0].last_error.is_none());
}

#[test]
fn waveform_wasm_sessions_restore_after_a_new_runtime_and_files_keep_separate_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one.vcd");
    let other = root.path().join("two.vcd");
    std::fs::write(&path, VCD).unwrap();
    std::fs::write(&other, VCD).unwrap();
    let mut first = app(root.path());
    open(&mut first, &path);
    finish(&mut first);
    key(&mut first, KeyCode::Right, KeyModifiers::NONE);
    key(&mut first, KeyCode::Char('s'), KeyModifiers::CONTROL);
    open(&mut first, &other);
    finish(&mut first);
    assert!(text(&first).contains("A 0 ns"));
    open(&mut first, &path);
    assert!(text(&first).contains("A 10 ns"));
    drop(first);
    let mut second = app(root.path());
    open(&mut second, &path);
    finish(&mut second);
    assert!(text(&second).contains("A 10 ns"));
    assert!(text(&second).contains("session restored"));
}

#[test]
fn waveform_wasm_bad_files_and_cancelled_reads_leave_a_usable_view() {
    let root = tempfile::tempdir().unwrap();
    let bad = root.path().join("bad.vcd");
    std::fs::write(&bad, b"not a VCD").unwrap();
    let mut app = app(root.path());
    open(&mut app, &bad);
    finish(&mut app);
    assert!(text(&app).contains("Unexpected VCD token"));
    let path = root.path().join("cancel.vcd");
    std::fs::write(&path, VCD.repeat(100)).unwrap();
    open(&mut app, &path);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(text(&app).contains("Loading cancelled"));
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
    key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(text(&app).contains("WAVEFORM EXPLORER"));
}

#[test]
fn waveform_wasm_reload_reads_new_simulations_and_preserves_the_view() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("live.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let mut next = VCD.to_vec();
    next.extend_from_slice(b"\n#2000\n1!\n");
    std::fs::write(&path, &next).unwrap();
    key(&mut app, KeyCode::F(5), KeyModifiers::NONE);
    finish(&mut app);
    assert!(text(&app).contains("session restored"));
    assert!(text(&app).contains("A 10 ns"));
    assert!(text(&app).contains("clk"));
    key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(window_ns(&app).1, 2000.0);
    assert_eq!(std::fs::read(&path).unwrap(), next);

    // The host's reload API must not silently discard a binary editor's edits.
    app.extension_manager.records[0]
        .runtime
        .as_mut()
        .unwrap()
        .store
        .data_mut()
        .documents[0]
        .edit(0, 1, b"X")
        .unwrap();
    key(&mut app, KeyCode::F(5), KeyModifiers::NONE);
    assert!(text(&app).contains("Unsaved document changes"));
    assert!(
        app.extension_manager.records[0]
            .runtime
            .as_ref()
            .unwrap()
            .store
            .data()
            .documents[0]
            .info()
            .dirty
    );
    assert_eq!(std::fs::read(&path).unwrap(), next);
}

#[test]
fn waveform_wasm_close_releases_tasks_saves_session_and_reopens_fresh_disk_data() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("live.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    app.force_close_tab(app.active_tab);
    drain(&mut app);
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
    let mut next = VCD.to_vec();
    next.extend_from_slice(b"\n#3000\n0!\n");
    std::fs::write(&path, next).unwrap();
    open(&mut app, &path);
    finish(&mut app);
    assert!(text(&app).contains("session restored"));
    assert!(text(&app).contains("A 10 ns"));
    key(&mut app, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(window_ns(&app).1, 3000.0);
    app.force_close_tab(app.active_tab);
    drain(&mut app);

    // Closing an incomplete read also unregisters workers, without saving an empty session.
    open(&mut app, &path);
    app.force_close_tab(app.active_tab);
    drain(&mut app);
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
    open(&mut app, &path);
    finish(&mut app);
    assert!(text(&app).contains("A 10 ns"));
}

#[test]
fn waveform_wasm_large_hierarchy_filter_and_session_import_fit_worker_budgets() {
    use std::fmt::Write;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hierarchy.vcd");
    let mut dump = String::from("$scope module top $end\n");
    for index in 0..15_000 {
        writeln!(dump, "$var wire 8 id{index} register_{index:05} [7:0] $end").unwrap();
    }
    dump.push_str("$upscope $end\n$enddefinitions $end\n#0\n");
    for index in 0..15_000 {
        writeln!(dump, "b0 id{index}").unwrap();
    }
    dump.push_str("#100\n");
    std::fs::write(&path, dump).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    assert!(text(&app).contains("15000 signals"), "{}", text(&app));
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(app.paste_into_extension_editor("register_14999"));
    drain(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("14999"), "{}", text(&app));
    let watches = (0..256)
        .map(|index| {
            serde_json::json!({
                "type": "Signal", "path": format!("top.register_{index:05}[7:0]"), "bit": null,
                "radix": "Hex", "color": index % 8, "label": "", "expanded": false, "analog": false,
            })
        })
        .collect::<Vec<_>>();
    let session = serde_json::json!({ "schema": 1, "watches": watches });
    assert!(app.paste_into_extension_editor(&session.to_string()));
    drain(&mut app);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app).contains("Session loaded"));
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(text(&app).contains("session saved"));
    resize(&mut app, 512, 200);
    app.code_area = Some(Rect::new(0, 0, 512, 200));
    crate::mouse::handle_mouse(
        &mut app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 150,
            row: 6,
            modifiers: KeyModifiers::NONE,
        },
    );
    drain(&mut app);
    assert!(text(&app).contains("Value @ t"));
    assert!(!app.notification_history.iter().any(|(message, _, _)| message.contains("out of fuel") || message.contains("wasm trap")));
}

#[test]
fn waveform_wasm_rapid_reopen_waits_for_close_cleanup_and_background_tasks_survive_last_tab() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rapid.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    app.force_close_tab(app.active_tab);
    app.extension_manager
        .dispatch_view_events(&mut app.background);
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.extension_manager.has_busy_runtimes()
    });
    assert!(crate::app_event_loop::next_visible_extension_tick_wait(&app).is_some());
    let ticks = app
        .extension_manager
        .visible_tick_handlers(&rustc_hash::FxHashSet::default());
    assert!(ticks.iter().any(|(_, h)| h.export_name == "wave_apply"));
    assert!(ticks.iter().all(|(_, h)| h.background));
    open(&mut app, &path);
    finish(&mut app);
    assert!(text(&app).contains("session restored"));
    assert!(text(&app).contains("A 10 ns"));
}

#[test]
fn waveform_wasm_registered_view_can_reopen_from_the_palette_after_releasing_data() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("palette.vcd");
    std::fs::write(&path, VCD).unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 38);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let view = app.active_extension_view().unwrap().clone();
    app.force_close_tab(app.active_tab);
    drain(&mut app);
    assert!(app.open_extension_view(&view.extension_id, &view.view_id));
    resize(&mut app, 150, 38);
    finish(&mut app);
    assert!(text(&app).contains("session restored"));
    assert!(text(&app).contains("A 10 ns"));
}

#[test]
fn waveform_wasm_real_pointer_hover_is_passive_coalesced_and_clears_on_exit() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hover.vcd");
    std::fs::write(&path, b"$timescale 1 ns $end $scope module top $end $var wire 32 ! data [31:0] $end $upscope $end $enddefinitions $end #0 b01100000000100001000000000000000 ! #100 b01100000000000001110111101000000 ! #1000").unwrap();
    let mut app = app(root.path());
    open(&mut app, &path);
    finish(&mut app);
    resize(&mut app, 150, 38);
    // Exercise real workbench coordinates, including a non-zero view origin.
    app.code_area = Some(Rect::new(5, 4, 150, 38));
    let before = window_ns(&app);
    let header = text(&app).lines().nth(2).unwrap().to_string();
    for column in 85..125 {
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row: 9,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.extension_manager.pending_view_events.len(), 1);
    }
    drain(&mut app);
    let inspected = text(&app);
    assert!(inspected.contains("Value @ t"), "{inspected}");
    assert!(inspected.contains("HEX  6000_ef40"), "{inspected}");
    assert!(inspected.contains("held 900 ns"));
    assert_eq!(window_ns(&app), before);
    assert_eq!(inspected.lines().nth(2).unwrap(), header);
    // Keys are ordering barriers: never coalesce a hover across a click or key.
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 86,
            row: 9,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.handle_active_extension_view_key(KeyCode::Char('s'), KeyModifiers::NONE));
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 87,
            row: 9,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.extension_manager.pending_view_events.len(), 3);
    drain(&mut app);
    assert!(text(&app).contains("snap off"));
    assert!(text(&app).contains("Value @ t"));
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        },
    );
    drain(&mut app);
    assert!(text(&app).contains("Value @ A"));
    assert!(!text(&app).contains("HEX  6000_ef40"));
    assert!(app.extension_manager.records[0].tick_handlers.is_empty());
    let view_id = app.active_extension_view().unwrap().view_id.clone();
    let view = app.extension_manager.records[0]
        .views
        .get_mut(&view_id)
        .unwrap();
    assert!(view.mouse_handler.is_some());
    view.hover_handler = None;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 90,
            row: 9,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(
        app.extension_manager.pending_view_events.is_empty(),
        "mouse handlers without hover opt-in must not receive moves"
    );
    assert!(!app.notification_history.iter().any(|(message, _, _)| message.contains("out of fuel") || message.contains("wasm trap")));
}

#[test]
fn waveform_file_associations_offer_and_open_both_text_vcd_and_binary_fst() {
    for (name, bytes) in [
        ("demo.vcd", VCD),
        (
            "gcd.fst",
            include_bytes!("../tests/fixtures/waveform-viewer/gcd.fst").as_slice(),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let mut app = app(root.path());
        app.open_file(&path);
        app.wait_for_background(Duration::from_secs(15), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        assert!(
            app.binary_open_prompt_active,
            "{name}: {:?}",
            app.notification_history
        );
        assert!(
            app.binary_open_prompt_commands
                .iter()
                .any(|command| command.extension_id == "vtcode.waveform-viewer"
                    && command.command_id == "wave.open")
        );
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        finish(&mut app);
        assert!(
            app.active_extension_view().is_some(),
            "{name}: {:?}",
            app.notification_history
        );
        assert!(text(&app).contains(if name.ends_with("fst") { "FST" } else { "VCD" }));
    }
}

#[test]
fn background_task_fuel_is_larger_but_ui_callback_budget_remains_bounded() {
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let module = wasmtime::Module::new(
        &engine,
        r#"(module
      (func (export "work") (result i32) (local $n i32)
        i32.const 4000000 local.set $n
        (loop $repeat local.get $n i32.const 1 i32.sub local.tee $n br_if $repeat)
        i32.const 0))"#,
    )
    .unwrap();
    let mut store = wasmtime::Store::new(&engine, crate::extension_wasm::HostState::default());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    assert!(crate::extension_wasm::call_noarg_export(&mut store, &instance, "work", true).is_err());
    store
        .data_mut()
        .tick_handlers
        .push(crate::extension_manifest::ExtensionTickHandler {
            export_name: "work".into(),
            interval_ms: 16,
            background: true,
        });
    assert_eq!(
        crate::extension_wasm::call_noarg_export(&mut store, &instance, "work", true).unwrap(),
        Some(0)
    );
    assert!(store.get_fuel().unwrap() < 250_000_000);
}
