use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    explorer::{FileEntry, GitStatus},
};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use std::time::Instant;

fn pointer(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub(super) fn fixture(tabs: usize) -> App {
    let mut app = App::test_new();
    app.tabs = (0..tabs)
        .map(|i| {
            let mut editor = Editor::test_with_text(
                &"fn main() { let value = 123; } // sample code\n".repeat(2_000),
            );
            editor.word_wrap = false;
            editor.file_path = Some(app.explorer.root.join(format!("file-{i:04}.rs")));
            Tab::new_file(editor)
        })
        .collect();
    app.active_tab = 0;
    app.show_explorer = true;
    app.show_minimap = true;
    app.cursor_blinking = false;
    app.explorer.entries = (0..5_000)
        .map(|i| {
            let name = format!("file-{i:04}.rs");
            let path = app.explorer.root.join(&name);
            app.explorer_hover_info_cache.insert(
                path.clone(),
                crate::background::ExplorerHoverInfo {
                    path: path.clone(),
                    is_dir: false,
                    size: 4096,
                    modified: None,
                    line_count: Some(200),
                },
            );
            FileEntry {
                path,
                name,
                is_dir: false,
                depth: 0,
                expanded: false,
                git_status: GitStatus::Unmodified,
                git_status_summary: Default::default(),
            }
        })
        .collect();
    app
}

#[test]
fn plain_code_hover_tracks_the_pointer_without_requesting_a_frame() {
    let mut app = fixture(1);
    app.show_explorer = false;
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let before = terminal.backend().buffer().clone();
    let area = app.code_area.unwrap();
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for index in 0..100u16 {
        let event = pointer(
            MouseEventKind::Moved,
            area.x + 2 + index % 24,
            area.y + index % 20,
        );
        assert!(!handle_mouse_input(&mut app, &mut pump, event));
        assert_eq!((app.mouse_col, app.mouse_row), (event.column, event.row));
        assert_eq!(
            app.hovered_editor_pos,
            Some((usize::from(index % 20), usize::from(2 + index % 24)))
        );
    }
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    assert_eq!(terminal.backend().buffer(), &before);
}

#[test]
fn marker_hover_repaints_on_entry_motion_and_exit_but_plain_text_stays_quiet() {
    let mut app = fixture(1);
    app.tabs[0] = Tab::new_file(Editor::test_with_text(
        "x TODO: marker description\nordinary source text\n",
    ));
    app.show_explorer = false;
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let area = app.code_area.unwrap();
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for column in [2, 3, 4, 5] {
        assert!(handle_mouse_input(
            &mut app,
            &mut pump,
            pointer(MouseEventKind::Moved, area.x + column, area.y)
        ));
    }
    assert!(handle_mouse_input(
        &mut app,
        &mut pump,
        pointer(MouseEventKind::Moved, area.x + 3, area.y + 1)
    ));
    assert!(!handle_mouse_input(
        &mut app,
        &mut pump,
        pointer(MouseEventKind::Moved, area.x + 7, area.y + 1)
    ));
}

#[test]
fn moving_into_code_clears_chrome_hover_once() {
    let mut app = fixture(1);
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let area = app.code_area.unwrap();
    app.hovered_tab_scroll_right = true;
    app.hovered_terminal_sidebar_toggle = true;
    app.outline_hovered = Some(1);
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    assert!(handle_mouse_input(
        &mut app,
        &mut pump,
        pointer(MouseEventKind::Moved, area.x + 3, area.y + 2)
    ));
    assert!(!app.hovered_tab_scroll_right && !app.hovered_terminal_sidebar_toggle);
    assert!(app.outline_hovered.is_none());
    assert!(!handle_mouse_input(
        &mut app,
        &mut pump,
        pointer(MouseEventKind::Moved, area.x + 4, area.y + 2)
    ));
}

#[test]
fn repeated_layout_drag_cells_skip_redraw_but_still_revalidate_geometry_and_blink() {
    for kind in 0..4 {
        let mut app = super::input_performance_tests::pane_resize_app(kind);
        let (_sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        let drag = pointer(MouseEventKind::Drag(MouseButton::Left), 34, 15);
        assert!(handle_mouse_input(&mut app, &mut pump, drag));
        for _ in 0..128 {
            assert!(
                !handle_mouse_input(&mut app, &mut pump, drag),
                "unchanged pane {kind}"
            );
        }
        app.cursor_visible = false;
        assert!(
            handle_mouse_input(&mut app, &mut pump, drag),
            "caret relighting still paints"
        );
        if kind == 0 {
            app.explorer_width = 15;
            assert!(
                handle_mouse_input(&mut app, &mut pump, drag),
                "stale geometry must still be corrected"
            );
            assert_ne!(app.explorer_width, 15);
        }
        assert!(handle_mouse_input(
            &mut app,
            &mut pump,
            pointer(MouseEventKind::Up(MouseButton::Left), 34, 15)
        ));
    }
}

#[test]
fn tab_tooltip_delay_survives_movement_within_the_same_tab() {
    let mut app = fixture(1);
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let area = app.tab_bar_area.unwrap();
    let hit = crate::ui_chrome_tabs::TabBarHits::new(&app, area).tabs[0].1;
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    handle_mouse_input(
        &mut app,
        &mut pump,
        pointer(MouseEventKind::Moved, hit.x + 3, hit.y),
    );
    let since = Instant::now() - Duration::from_millis(300);
    app.tab_tooltip_hover_start = Some(since);
    handle_mouse_input(
        &mut app,
        &mut pump,
        pointer(MouseEventKind::Moved, hit.x + 4, hit.y),
    );
    assert_eq!(app.tab_tooltip_tab, Some(0));
    assert_eq!(app.tab_tooltip_hover_start, Some(since));
}

#[test]
fn popup_clicks_use_the_last_painted_viewport_even_if_the_host_size_differs() {
    let mut app = fixture(1);
    app.tabs[0] = Tab::new_file(Editor::test_with_text("pr"));
    app.tabs[0].editor.cursor_col = 2;
    app.viewport_area = ratatui::layout::Rect::new(0, 0, 180, 60);
    app.code_area = Some(ratatui::layout::Rect::new(120, 40, 50, 15));
    app.vtcomplete_active = true;
    app.vtcomplete_suggestions = vec!["print".into(), "println".into()];
    let viewport = app.viewport_area;
    let popup = crate::ui_search_dialog::vtcomplete_popup_geometry(&mut app, viewport).unwrap();
    assert!(crate::mouse_overlay::handle_popup_click(
        &mut app,
        popup.inner_area.x,
        popup.inner_area.y + 1
    ));
    assert_eq!(app.editor().unwrap().rope.to_string(), "println");
}

#[test]
fn multiline_pointer_bursts_preserve_unicode_folds_selections_and_edit_history() {
    for wrapped in [false, true] {
        let (mut actual, mut actual_terminal) = super::mouse_selection_latency_tests::fixture(true);
        let (mut expected, mut expected_terminal) =
            super::mouse_selection_latency_tests::fixture(true);
        for (app, terminal) in [
            (&mut actual, &mut actual_terminal),
            (&mut expected, &mut expected_terminal),
        ] {
            app.editor_mut().unwrap().word_wrap = wrapped;
            app.editor_mut().unwrap().folded_ranges.insert(5, 15);
            terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        }
        let area = actual.code_area.unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        for index in 0..512u16 {
            let event = pointer(
                MouseEventKind::Drag(MouseButton::Left),
                area.x + 2 + index * 37 % (area.width - 4),
                area.y + 1 + index * 17 % (area.height - 2),
            );
            sender.send(Ok(Event::Mouse(event))).unwrap();
            mouse::handle_mouse(&mut expected, event);
        }
        let Some(Event::Mouse(first)) = pump.try_recv().unwrap() else {
            panic!("missing pointer");
        };
        assert!(handle_mouse_input(&mut actual, &mut pump, first));
        assert!(
            !pump.has_pending_event(),
            "paint the final endpoint, not historical rows"
        );
        let key = |app: &App| {
            (
                app.editor().unwrap().cursor_line,
                app.editor().unwrap().cursor_col,
                app.editor().unwrap().selection_anchor,
                app.editor().unwrap().selected_text(),
            )
        };
        assert_eq!(key(&actual), key(&expected));
        actual_terminal
            .draw(|frame| ui::draw(frame, &mut actual))
            .unwrap();
        expected_terminal
            .draw(|frame| ui::draw(frame, &mut expected))
            .unwrap();
        assert_eq!(
            actual_terminal.backend().buffer(),
            expected_terminal.backend().buffer()
        );
        for app in [&mut actual, &mut expected] {
            let editor = app.editor_mut().unwrap();
            let original = editor.rope.clone();
            editor.paste_plain_with_text("replacement λ\nnext line");
            let changed = editor.rope.clone();
            editor.undo();
            assert_eq!(editor.rope, original);
            editor.redo();
            assert_eq!(editor.rope, changed);
        }
        assert_eq!(key(&actual), key(&expected));
    }
}

#[test]
#[ignore = "Optimized full-workbench latency probe; use one CPU and --nocapture"]
fn benchmark_local_workbench_pointer_latency() {
    for tabs in [1, 128] {
        for case in ["code-hover", "explorer-hover", "selection", "resize"] {
            let mut samples: [Vec<Duration>; 3] = Default::default();
            for _ in 0..5 {
                let mut app = fixture(tabs);
                let mut terminal = Terminal::new(TestBackend::new(200, 65)).unwrap();
                for _ in 0..3 {
                    refresh_caches(&mut app);
                    app.wait_for_background(Duration::from_secs(10), |app| {
                        app.background.active_job_count() == 0
                    });
                    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
                }
                let code = app.code_area.unwrap();
                if case == "selection" {
                    mouse::handle_mouse(
                        &mut app,
                        pointer(
                            MouseEventKind::Down(MouseButton::Left),
                            code.x + 2,
                            code.y + 1,
                        ),
                    );
                }
                if case == "resize" {
                    app.resizing_explorer = true;
                }
                let (_sender, receiver) = std::sync::mpsc::channel();
                let mut pump = TerminalEventPump::from_receiver(receiver);
                let mut total = [Duration::ZERO; 3];
                for index in 0..120u16 {
                    let code = app.code_area.unwrap();
                    let event = match case {
                        "code-hover" => pointer(
                            MouseEventKind::Moved,
                            code.x + 2 + index % 30,
                            code.y + index % 20,
                        ),
                        "explorer-hover" => {
                            let tree = crate::ui_chrome::explorer_tree_content_area(
                                &app,
                                app.explorer_area.unwrap(),
                            );
                            pointer(
                                MouseEventKind::Moved,
                                tree.x + 5,
                                tree.y + index % tree.height.min(25),
                            )
                        }
                        "selection" => pointer(
                            MouseEventKind::Drag(MouseButton::Left),
                            code.x + 2 + index % 30,
                            code.y + index % 20,
                        ),
                        _ => pointer(MouseEventKind::Drag(MouseButton::Left), 20 + index % 50, 25),
                    };
                    let now = Instant::now();
                    let redraw = handle_mouse_input(&mut app, &mut pump, event);
                    total[0] += now.elapsed();
                    if !redraw {
                        continue;
                    }
                    let now = Instant::now();
                    app.sync_duplicate_file_tabs();
                    app.ensure_active_tab_visible();
                    refresh_caches(&mut app);
                    total[1] += now.elapsed();
                    let now = Instant::now();
                    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
                    total[2] += now.elapsed();
                }
                for (sample, total) in samples.iter_mut().zip(total) {
                    sample.push(total / 120);
                }
            }
            for sample in &mut samples {
                sample.sort();
            }
            eprintln!(
                "workbench {case} tabs={tabs}: input={:?}, preparation={:?}, draw={:?}",
                samples[0][2], samples[1][2], samples[2][2]
            );
        }
    }
}

#[test]
#[ignore = "Queued two-dimensional pointer workload with optional slow-output simulation"]
fn benchmark_multiline_selection_backlog() {
    for output_wait in [0, 2] {
        let (mut app, mut terminal) = super::mouse_selection_latency_tests::fixture(true);
        let area = app.code_area.unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        for index in 0..512u16 {
            sender
                .send(Ok(Event::Mouse(pointer(
                    MouseEventKind::Drag(MouseButton::Left),
                    area.x + 2 + index * 37 % (area.width - 4),
                    area.y + 1 + index * 17 % (area.height - 2),
                ))))
                .unwrap();
        }
        let start = Instant::now();
        let mut frames = 0;
        while let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() {
            if handle_mouse_input(&mut app, &mut pump, mouse) {
                terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
                if output_wait > 0 {
                    std::thread::sleep(Duration::from_millis(output_wait));
                }
                frames += 1;
            }
        }
        eprintln!(
            "multiline selection queued=512 output_wait_ms={output_wait} frames={frames} elapsed={:?}",
            start.elapsed()
        );
    }
}
