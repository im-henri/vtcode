use super::*;
use crate::{
    app::{ActivePane, Tab},
    background::BackgroundResult,
    explorer::{FileEntry, GitStatus},
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture() -> App {
    let mut app = App::new_deferred("loading-workspace".into());
    app.new_file();
    app.tabs[0].editor.file_path = Some(app.explorer.root.join("example.txt"));
    app.active_pane = ActivePane::Editor;
    app.cursor_blinking = false;
    app.idle_quiescent = true;
    app.workspace_watcher_active = true;
    assert!(!app.animation_settings.any_enabled());
    app
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>, cosmetic: bool) -> Buffer {
    app.rendering_animation_frame = cosmetic;
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    app.rendering_animation_frame = false;
    terminal.backend().buffer().clone()
}

fn underlines(buffer: &Buffer, header: Rect) -> Vec<u16> {
    (header.x..header.right())
        .filter(|&x| {
            buffer[(x, header.y)]
                .modifier
                .contains(Modifier::UNDERLINED)
        })
        .collect()
}

fn row_text(buffer: &Buffer, header: Rect) -> String {
    (header.x..header.right())
        .map(|x| buffer[(x, header.y)].symbol())
        .collect()
}

#[test]
fn idle_hidden_and_unfocused_loading_have_no_animation_deadline() {
    let now = Instant::now();
    let mut state = State::default();
    assert_eq!(state.next_wait(true, now), None);
    assert!(!state.advance(true, now));
    state.begin_frame();
    state.visible = true;
    state.animated = true;
    state.finish_frame(now);
    assert_eq!(state.next_wait(true, now), Some(FRAME_INTERVAL));
    assert_eq!(state.next_wait(false, now), None);
    assert!(!state.advance(false, now + Duration::from_secs(5)));
    assert!(state.advance(true, now + Duration::from_secs(5)));
    assert_eq!(
        state.phase_ns, 1_160_000_000,
        "resume at the elapsed-time position"
    );
    assert!(
        !state.advance(true, now + Duration::from_secs(5)),
        "never replay missed ticks in a burst"
    );
    state.begin_frame();
    state.finish_frame(now);
    assert_eq!(state.next_wait(true, now), None);
    assert!(!state.active());
}

#[test]
fn unrelated_input_and_animation_frames_do_not_restart_the_progress_deadline() {
    let now = Instant::now();
    let mut state = State {
        visible: true,
        animated: true,
        ..Default::default()
    };
    state.finish_frame(now);
    for millis in [1, 4, 8, 12, 15] {
        state.begin_frame();
        state.visible = true;
        state.animated = true;
        state.finish_frame(now + Duration::from_millis(millis));
        assert_eq!(state.next, Some(now + FRAME_INTERVAL));
    }
    assert!(state.advance(true, now + FRAME_INTERVAL));
    assert_eq!(state.phase_ns, FRAME_INTERVAL.as_nanos() as u64);
    assert_eq!(
        state.next_wait(true, now + FRAME_INTERVAL),
        Some(FRAME_INTERVAL)
    );
}

#[test]
fn slow_rendering_gets_a_quiet_gap_instead_of_continuous_loading_redraws() {
    let now = Instant::now();
    let mut state = State {
        visible: true,
        animated: true,
        ..Default::default()
    };
    state.finish_frame(now);
    assert!(state.advance(true, now + FRAME_INTERVAL));
    let finished = now + Duration::from_millis(400);
    state.begin_frame();
    state.visible = true;
    state.animated = true;
    state.finish_frame(finished);
    assert_eq!(state.next_wait(true, finished), Some(FRAME_INTERVAL));
    state.record_render(finished, Duration::from_millis(400));
    assert_eq!(
        state.next_wait(true, finished),
        Some(Duration::from_millis(100))
    );
    assert!(!state.advance(true, finished));
}

#[test]
fn loading_position_follows_elapsed_time_through_delayed_frames() {
    let now = Instant::now();
    let mut state = State {
        visible: true,
        animated: true,
        ..Default::default()
    };
    state.finish_frame(now);
    for millis in [16, 32, 250, 1000, 1936, 10_123] {
        let sample = now + Duration::from_millis(millis);
        assert!(state.advance(true, sample));
        assert_eq!(
            state.phase_ns,
            (Duration::from_millis(millis).as_nanos() % CYCLE_DURATION.as_nanos()) as u64
        );
        assert_eq!(state.next_wait(true, sample), Some(FRAME_INTERVAL));
        assert!(!state.advance(true, sample));
    }
}

#[test]
fn ordinary_input_redraws_do_not_postpone_loading_or_inherit_a_tick() {
    let now = Instant::now();
    let mut state = State {
        visible: true,
        animated: true,
        ..Default::default()
    };
    state.finish_frame(now);
    state.record_render(now, Duration::from_millis(2));
    assert!(state.advance(true, now + FRAME_INTERVAL));
    state.begin_frame();
    state.visible = true;
    state.animated = true;
    state.finish_frame(now + FRAME_INTERVAL);
    let finished = now + FRAME_INTERVAL + Duration::from_millis(3);
    state.record_render(finished, Duration::from_millis(3));
    let deadline = state.next;
    for millis in [1, 5, 10, 15] {
        state.begin_frame();
        state.visible = true;
        state.animated = true;
        state.finish_frame(finished + Duration::from_millis(millis));
        state.record_render(
            finished + Duration::from_millis(millis),
            Duration::from_millis(60),
        );
        assert_eq!(
            state.next, deadline,
            "input must not reset the cosmetic deadline"
        );
    }
}

#[test]
fn explorer_bar_moves_over_cached_chrome_without_changing_text_hits_or_layout() {
    for right in [false, true] {
        let mut app = fixture();
        app.sidebar_right = right;
        app.bg_explorer_refresh_running = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
        let first = render(&mut app, &mut terminal, false);
        let area = app.explorer_area.unwrap();
        let header = Rect::new(area.x, area.y, area.width, 1);
        let code = app.code_area;
        let entries = crate::ui_chrome::explorer_tree_content_area(&app, area);
        let text = row_text(&first, header);
        assert!(text.contains("loading"));
        assert!(!underlines(&first, header).is_empty());
        let first_cells = underlines(&first, header);
        let mut previous = first_cells.clone();
        let ticks = (CYCLE_DURATION.as_nanos() / FRAME_INTERVAL.as_nanos()) as u64;
        for tick in 1..=ticks {
            // Exercise the full cosmetic renderer at deterministic positions;
            // scheduling and elapsed-time behavior are covered independently.
            app.loading.phase_ns =
                (FRAME_INTERVAL.as_nanos() * u128::from(tick) % CYCLE_DURATION.as_nanos()) as u64;
            let frame = render(&mut app, &mut terminal, true);
            assert_eq!(row_text(&frame, header), text);
            assert_eq!(app.code_area, code);
            assert_eq!(
                crate::ui_chrome::explorer_tree_content_area(&app, area),
                entries
            );
            let cells = underlines(&frame, header);
            assert!(!cells.is_empty());
            if tick == ticks / 4 {
                assert_ne!(cells, first_cells);
            }
            previous = cells;
        }
        assert_eq!(previous, first_cells, "one complete left-to-right cycle");
        assert!(app.animation_frame_cache.chrome.replays > 0);
        assert!(
            app.animation_frame_cache.replays > 0,
            "loading ticks reuse the editor scene too"
        );
        app.bg_explorer_refresh_running = false;
        let final_frame = render(&mut app, &mut terminal, false);
        assert!(underlines(&final_frame, header).is_empty());
        assert!(!row_text(&final_frame, header).contains("loading"));
        assert_eq!(app.loading.next_wait(true, Instant::now()), None);
    }
}

#[test]
fn changing_sidebar_or_hiding_it_stops_irrelevant_loading_ticks() {
    let mut app = fixture();
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    app.bg_explorer_refresh_running = true;
    render(&mut app, &mut terminal, false);
    assert!(app.loading.active());
    app.sidebar_view = SidebarView::Settings;
    render(&mut app, &mut terminal, false);
    assert!(!app.loading.active());
    app.sidebar_view = SidebarView::Explorer;
    render(&mut app, &mut terminal, false);
    assert!(app.loading.active());
    app.show_explorer = false;
    render(&mut app, &mut terminal, false);
    assert!(!app.loading.active());
}

#[test]
fn search_and_git_use_the_same_loading_feedback_only_for_live_work() {
    for view in [SidebarView::Search, SidebarView::SourceControl] {
        let mut app = fixture();
        app.sidebar_view = view;
        app.bg_global_search_running = view == SidebarView::Search;
        app.bg_git_status_running = view == SidebarView::SourceControl;
        let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
        let frame = render(&mut app, &mut terminal, false);
        let area = app.explorer_area.unwrap();
        assert!(!underlines(&frame, Rect { height: 1, ..area }).is_empty());
        assert!(app.loading.active());
        app.bg_global_search_running = false;
        app.bg_git_status_running = false;
        render(&mut app, &mut terminal, false);
        assert!(!app.loading.active());
    }
}

#[test]
fn opening_saving_and_save_as_have_feedback_without_mutating_buffers() {
    for kind in [0, 1, 2] {
        let mut app = fixture();
        app.show_explorer = false;
        let path = app.tabs[0].editor.file_path.clone().unwrap();
        let id = app.tabs[0].editor.save_identity;
        let before = app.tabs[0].editor.rope.clone();
        match kind {
            0 => app.tabs[0].editor.bg_loading = true,
            1 => {
                app.pending_file_saves
                    .insert(path, (app.workspace_generation, 0.into()));
            }
            _ => {
                app.pending_save_as
                    .insert(id, (app.workspace_generation, "destination.txt".into()));
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
        let frame = render(&mut app, &mut terminal, false);
        assert!(app.loading.active());
        let header = app.breadcrumb_area.or(app.tab_bar_area).unwrap();
        assert!(!underlines(&frame, header).is_empty());
        let text: String = frame.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains(if kind == 0 {
            "Opening file"
        } else {
            "Saving file"
        }));
        assert!(before.is_instance(&app.tabs[0].editor.rope));
        app.tabs[0].editor.bg_loading = false;
        app.pending_file_saves.clear();
        app.pending_save_as.clear();
        render(&mut app, &mut terminal, false);
        assert!(!app.loading.active());
    }
}

#[test]
fn old_workspace_saves_and_hidden_file_headers_do_not_animate_the_current_view() {
    let mut app = fixture();
    app.show_explorer = false;
    let path = app.tabs[0].editor.file_path.clone().unwrap();
    app.pending_file_saves
        .insert(path, (app.workspace_generation.wrapping_sub(1), 0.into()));
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    render(&mut app, &mut terminal, false);
    assert!(!app.loading.active());
    app.tabs[0].editor.bg_loading = true;
    app.show_tab_bar = false;
    app.show_breadcrumbs = false;
    render(&mut app, &mut terminal, false);
    assert!(
        !app.loading.active(),
        "static status text requires no animation timer"
    );
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("unrelated buffer")));
    app.active_tab = 1;
    app.show_tab_bar = true;
    render(&mut app, &mut terminal, false);
    assert!(
        !app.loading.active(),
        "a hidden loading tab does not animate unrelated files"
    );
}

#[test]
fn loading_in_an_inactive_split_has_feedback_and_stops_when_its_desk_is_hidden() {
    use crate::app::{EditorGroup, SplitDirection, SplitNode};
    let mut app = fixture();
    app.show_explorer = false;
    let mut other = EditorGroup::new();
    other.tabs.push(Tab::new_file(Editor::new_loading(
        std::path::Path::new("other.txt"),
        0,
    )));
    app.groups = vec![EditorGroup::new(), other];
    app.split_tree = Some(SplitNode::Split {
        direction: SplitDirection::Horizontal,
        children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)],
        ratios: vec![0.5, 0.5],
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 26)).unwrap();
    let frame = render(&mut app, &mut terminal, false);
    assert!(app.loading.active());
    let other = app.groups[1].editor_area.unwrap();
    assert!(
        (other.y..other.bottom().min(other.y + 2)).any(|y| !underlines(
            &frame,
            Rect::new(other.x, y, other.width, 1)
        )
        .is_empty())
    );
    app.new_desk("Other desk", false);
    render(&mut app, &mut terminal, false);
    assert!(!app.loading.active());
}

#[test]
fn local_expansion_marks_the_folder_and_completion_clears_loading() {
    let mut app = fixture();
    let path = app.explorer.root.join("folder");
    app.explorer.entries = vec![FileEntry {
        path: path.clone(),
        name: "folder".into(),
        is_dir: true,
        depth: 0,
        expanded: false,
        git_status: GitStatus::Unmodified,
        git_status_summary: Default::default(),
    }];
    // Hold an already-running refresh, so the test can inspect queued expansion
    // without racing the worker or sleeping for an arbitrary time.
    app.bg_explorer_refresh_running = true;
    assert!(app.toggle_or_open_explorer_async().is_none());
    assert!(app.explorer_expanding.contains(&path));
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    let frame = render(&mut app, &mut terminal, false);
    assert!(frame.content.iter().any(|cell| cell.symbol() == "◌"));
    app.bg_explorer_refresh_pending = false;
    app.background
        .sender
        .send(BackgroundResult::ExplorerRefreshed {
            generation: app.explorer_refresh_generation,
            canonical_root: app.explorer.root.clone(),
            entries: app.explorer.entries.clone(),
            matches: Default::default(),
        })
        .unwrap();
    assert!(app.poll_background_results());
    assert!(app.explorer_expanding.is_empty());
    assert!(!app.bg_explorer_refresh_running);
    render(&mut app, &mut terminal, false);
    assert!(!app.loading.active());
}

#[test]
fn folder_browser_loading_has_feedback_and_does_not_animate_hidden_editor_work() {
    let mut app = fixture();
    app.change_dir_active = true;
    app.folder_browser_loading = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    render(&mut app, &mut terminal, false);
    assert!(app.loading.active());
    app.folder_browser_loading = false;
    app.bg_explorer_refresh_running = true;
    app.tabs[0].editor.bg_loading = true;
    render(&mut app, &mut terminal, false);
    assert!(!app.loading.active());
}

#[test]
fn tiny_and_resized_headers_are_clipped_without_text_or_hit_changes() {
    for width in [0, 1, 2, 7, 40] {
        let mut app = fixture();
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        app.loading.begin_frame();
        for frame_index in 0..120 {
            app.loading.phase_ns = FRAME_INTERVAL.as_nanos() as u64 * frame_index;
            terminal
                .draw(|f| draw_header(f, &mut app, Rect::new(3, 2, width, 1)))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert!(buffer.content.iter().all(|cell| cell.symbol() == " "));
            if width > 0 {
                assert!(!underlines(buffer, Rect::new(3, 2, width.min(17), 1)).is_empty());
            }
        }
        app.loading.finish_frame(Instant::now());
        assert_eq!(app.loading.active(), width > 0);
        if width <= 1 {
            assert_eq!(
                app.loading.next_wait(true, Instant::now()),
                None,
                "empty and one-cell bars must not schedule unchanged cosmetic frames"
            );
        }
    }
}

#[test]
fn fractional_motion_fades_edges_even_between_unchanged_cell_positions() {
    use ratatui::{style::Color, widgets::Paragraph};
    let mut app = fixture();
    let header = Rect::new(0, 0, 40, 1);
    let mut terminal = Terminal::new(TestBackend::new(50, 3)).unwrap();
    let mut frames = Vec::new();
    for millis in [488, 496] {
        app.loading.phase_ns = Duration::from_millis(millis).as_nanos() as u64;
        terminal
            .draw(|f| {
                f.render_widget(
                    Paragraph::new("Explorer · loading…").style(
                        Style::default()
                            .fg(Color::Rgb(220, 220, 220))
                            .bg(Color::Rgb(20, 20, 20)),
                    ),
                    header,
                );
                draw_header(f, &mut app, header);
            })
            .unwrap();
        frames.push(terminal.backend().buffer().clone());
    }
    assert_eq!(
        underlines(&frames[0], header),
        underlines(&frames[1], header)
    );
    assert_ne!(
        frames[0][(3, 0)].underline_color,
        frames[1][(3, 0)].underline_color
    );
    assert_ne!(
        frames[0][(13, 0)].underline_color,
        frames[1][(13, 0)].underline_color
    );
    assert_eq!(
        frames[0][(7, 0)].underline_color,
        frames[1][(7, 0)].underline_color
    );
    for (a, b) in frames[0].content.iter().zip(&frames[1].content) {
        assert_eq!((a.symbol(), a.fg, a.bg), (b.symbol(), b.fg, b.bg));
    }
}
