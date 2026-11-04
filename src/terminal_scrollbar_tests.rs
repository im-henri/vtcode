use super::*;
use crate::{
    app::{ActivePane, SplitDirection, Tab},
    editor::Editor,
    terminal::TerminalEmulator,
};
use crossterm::event::KeyModifiers;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::sync::{Arc, Mutex, mpsc};

struct Fixture {
    app: App,
    input: Arc<Mutex<Vec<u8>>>,
    _running: mpsc::Sender<()>,
}

impl Fixture {
    fn new(location: usize) -> Self {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text("keep this document"))];
        app.show_explorer = false;
        app.show_panel = false;
        app.show_terminal_sidebar = true;
        app.terminal_sidebar_revealed = true;
        app.active_pane = ActivePane::Editor;
        app.terminal_panel_focused = location == 0;
        let mut pty = TerminalEmulator::test_with_output(20, 4, "");
        let input = pty.test_capture_input();
        let running = pty.test_keep_running();
        app.terminals[0].is_placeholder = false;
        app.terminals[0].terminal_pty = Some(pty);
        if location >= 2 {
            assert!(app.detach_terminal_to_group(0, 0));
            if location == 3 {
                let selected = app.active_tab;
                app.create_split_group_dir(selected, SplitDirection::Horizontal);
                app.switch_group(0);
            }
        }
        let mut fixture = Self {
            app,
            input,
            _running: running,
        };
        fixture.draw(120, 30);
        fixture
    }

    fn draw(&mut self, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut self.app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn feed(&mut self, text: &str) {
        let pty = self.app.terminals[0].terminal_pty.as_mut().unwrap();
        pty.test_queue_output(text.as_bytes());
        while pty.process_while(|| true) {}
    }

    fn history(&mut self) {
        self.feed(
            &(0..1_000)
                .map(|row| format!("line {row:04}\r\n"))
                .collect::<String>(),
        );
        self.draw(120, 30);
    }

    fn bar(&self) -> Surface {
        *self
            .app
            .terminal_scrollbar
            .areas
            .iter()
            .find(|bar| bar.terminal.index == 0)
            .unwrap()
    }

    fn grid(&self) -> &TermGrid {
        &self.app.terminals[0].terminal_pty.as_ref().unwrap().grid
    }

    fn view(&self) -> String {
        (0..self.grid().height)
            .flat_map(|row| self.grid().display_line(row).iter().map(|cell| cell.ch))
            .collect()
    }

    fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) {
        crate::mouse::handle_mouse(
            &mut self.app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    fn assert_painted_bar(&mut self) {
        let buffer = self.draw(120, 30);
        let bar = self.bar();
        let metrics = Metrics::new(self.grid(), bar.area);
        let hovered = bar
            .area
            .contains((self.app.mouse_col, self.app.mouse_row).into());
        let active = self.app.terminal_scrollbar.drag.is_some();
        let (track, thumb) =
            self.app
                .colors
                .ui
                .scrollbar_colors(self.app.colors.ui.bg_terminal, hovered, active);
        assert_ne!(track, thumb, "the default theme must show a visible thumb");
        for row in 0..bar.area.height {
            let cell = &buffer[(bar.area.x, bar.area.y + row)];
            assert_eq!(cell.symbol(), "▐");
            assert_eq!(cell.bg, track);
            let in_thumb =
                (metrics.thumb_top..metrics.thumb_top + metrics.thumb_height).contains(&row);
            assert_eq!(cell.fg, if in_thumb { thumb } else { track });
        }
    }
}

#[test]
fn docked_and_editor_terminals_reserve_a_visible_gutter_without_reflowing_on_output() {
    for location in 0..4 {
        let mut fixture = Fixture::new(location);
        let bar = fixture.bar();
        let dimensions = (fixture.grid().width, fixture.grid().height);
        assert_eq!(bar.area.width, 1);
        assert_eq!(bar.area.x, bar.terminal.area.right());
        assert_eq!(bar.area.height, bar.terminal.area.height);
        assert!(bar.area.intersection(bar.terminal.area).is_empty());
        assert_eq!(
            dimensions,
            (
                usize::from(bar.terminal.area.width),
                usize::from(bar.terminal.area.height)
            )
        );
        fixture.assert_painted_bar();
        fixture.history();
        assert_eq!((fixture.grid().width, fixture.grid().height), dimensions);
        fixture.assert_painted_bar();
        fixture.app.mouse_hover_changed = false;
        fixture.mouse(MouseEventKind::Moved, bar.area.x, bar.area.y);
        assert!(fixture.app.mouse_hover_changed);
        fixture.assert_painted_bar();
        fixture.mouse(MouseEventKind::Moved, 0, 0);
        fixture.assert_painted_bar();
    }
}

#[test]
fn track_clicks_drags_and_wheels_scroll_the_owner_and_resume_following_at_the_bottom() {
    for location in 0..4 {
        let mut fixture = Fixture::new(location);
        fixture.history();
        fixture.feed("\x1b[?1003h\x1b[?1006h");
        let bar = fixture.bar();
        let focus = (
            fixture.app.active_group,
            fixture.app.active_tab,
            fixture.app.active_pane,
            fixture.app.terminal_panel_focused,
        );
        fixture.mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            bar.area.y,
        );
        assert_eq!(fixture.grid().scroll_view, fixture.grid().scrollback.len());
        assert!(fixture.app.terminal_scrollbar.drag.is_some());
        fixture.mouse(MouseEventKind::Up(MouseButton::Left), 0, bar.area.y);
        assert!(fixture.app.terminal_scrollbar.drag.is_none());
        let before = fixture.view();
        fixture.feed("incoming output\r\n");
        assert_eq!(fixture.view(), before);
        let before = fixture.grid().scroll_view;
        fixture.mouse(MouseEventKind::ScrollDown, bar.area.x, bar.area.y);
        assert_eq!(
            fixture.grid().scroll_view,
            before - fixture.app.scroll_speed
        );
        fixture.mouse(MouseEventKind::ScrollUp, bar.area.x, bar.area.y);
        assert_eq!(fixture.grid().scroll_view, before);
        let grid = &mut fixture.app.terminals[0].terminal_pty.as_mut().unwrap().grid;
        grid.begin_selection(0, 0);
        grid.extend_selection(0, 7);
        grid.finish_selection();
        assert!(grid.has_selection());
        fixture.mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            bar.area.y,
        );
        fixture.mouse(
            MouseEventKind::Drag(MouseButton::Left),
            0,
            bar.area.bottom() + 3,
        );
        fixture.mouse(
            MouseEventKind::Up(MouseButton::Left),
            0,
            bar.area.bottom() + 3,
        );
        assert_eq!(fixture.grid().scroll_view, 0);
        assert!(!fixture.grid().has_selection());
        fixture.feed("following output\r\n");
        assert_eq!(fixture.grid().scroll_view, 0);
        assert!(fixture.view().contains("following output"));
        assert_eq!(
            (
                fixture.app.active_group,
                fixture.app.active_tab,
                fixture.app.active_pane,
                fixture.app.terminal_panel_focused
            ),
            focus
        );
        assert!(fixture.app.terminal_selection_drag.is_none());
        assert!(fixture.app.terminal_mouse_capture.is_none());
        assert!(fixture.input.lock().unwrap().is_empty());
        assert!(
            fixture
                .app
                .tabs
                .iter()
                .chain(fixture.app.groups.iter().flat_map(|group| &group.tabs))
                .filter(|tab| tab.kind == crate::app::TabKind::File)
                .all(|tab| tab.editor.rope == "keep this document")
        );
    }
}

#[test]
fn a_stationary_thumb_does_not_jump_with_rounding_or_new_output() {
    let mut fixture = Fixture::new(0);
    fixture.history();
    let bar = fixture.bar();
    fixture.mouse(
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.area.bottom() - 1,
    );
    fixture.feed("live while held\r\n");
    fixture.mouse(
        MouseEventKind::Drag(MouseButton::Left),
        bar.area.x,
        bar.area.bottom() - 1,
    );
    fixture.mouse(
        MouseEventKind::Up(MouseButton::Left),
        bar.area.x,
        bar.area.bottom() - 1,
    );
    assert_eq!(fixture.grid().scroll_view, 0);
    fixture.app.terminals[0]
        .terminal_pty
        .as_mut()
        .unwrap()
        .scroll_up(337);
    fixture.draw(120, 30);
    let metrics = Metrics::new(fixture.grid(), bar.area);
    let row = bar.area.y + metrics.thumb_top;
    fixture.mouse(MouseEventKind::Down(MouseButton::Left), bar.area.x, row);
    fixture.mouse(MouseEventKind::Drag(MouseButton::Left), bar.area.x, row);
    fixture.mouse(MouseEventKind::Up(MouseButton::Left), bar.area.x, row);
    assert_eq!(fixture.grid().scroll_view, 337);
}

#[test]
fn clear_saved_lines_and_reset_reset_the_scrollbar_without_changing_pty_dimensions() {
    for clear in ["\x1b[H\x1b[2J\x1b[3J", "\x1b[3J", "\x1bc"] {
        let mut fixture = Fixture::new(0);
        fixture.history();
        let bar = fixture.bar();
        let dimensions = (fixture.grid().width, fixture.grid().height);
        fixture.mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            bar.area.y,
        );
        fixture.feed(clear);
        fixture.mouse(
            MouseEventKind::Drag(MouseButton::Left),
            bar.area.x,
            bar.area.y + 3,
        );
        fixture.mouse(
            MouseEventKind::Up(MouseButton::Left),
            bar.area.x,
            bar.area.y + 3,
        );
        assert!(fixture.grid().scrollback.is_empty());
        assert_eq!(fixture.grid().scroll_view, 0);
        assert_eq!((fixture.grid().width, fixture.grid().height), dimensions);
        fixture.assert_painted_bar();
        let metrics = Metrics::new(fixture.grid(), bar.area);
        assert_eq!(
            (metrics.thumb_top, metrics.thumb_height),
            (0, bar.area.height)
        );
        fixture.mouse(MouseEventKind::ScrollUp, bar.area.x, bar.area.y);
        assert_eq!(fixture.grid().scroll_view, 0);
        assert!(fixture.input.lock().unwrap().is_empty());
    }
}

#[test]
fn clearing_only_the_screen_keeps_the_scrollbar_for_retained_history() {
    let mut fixture = Fixture::new(0);
    fixture.history();
    let saved = fixture.grid().scrollback.len();
    fixture.feed("\x1b[2J\x1b[H");
    fixture.assert_painted_bar();
    assert_eq!(fixture.grid().scrollback.len(), saved);
    let bar = fixture.bar();
    fixture.mouse(
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.area.y,
    );
    fixture.mouse(
        MouseEventKind::Up(MouseButton::Left),
        bar.area.x,
        bar.area.y,
    );
    assert!(fixture.view().contains("line 0000"));
}

#[test]
fn full_screen_applications_have_no_shell_scroll_range_and_restore_it_on_exit() {
    for mode in [47, 1047, 1049] {
        let mut fixture = Fixture::new(2);
        fixture.history();
        fixture.app.terminals[0]
            .terminal_pty
            .as_mut()
            .unwrap()
            .scroll_up(100);
        let saved = fixture.grid().scrollback.len();
        fixture.feed(&format!("\x1b[?{mode}h\x1b[?1003;1006hfull screen"));
        fixture.assert_painted_bar();
        let bar = fixture.bar();
        assert_eq!(Metrics::new(fixture.grid(), bar.area).max, 0);
        fixture.mouse(MouseEventKind::ScrollUp, bar.area.x, bar.area.y);
        fixture.mouse(MouseEventKind::ScrollDown, bar.area.x, bar.area.y);
        fixture.mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            bar.area.y,
        );
        fixture.mouse(
            MouseEventKind::Up(MouseButton::Left),
            bar.area.x,
            bar.area.bottom() - 1,
        );
        assert!(fixture.app.terminal_scrollbar.drag.is_none());
        assert_eq!(fixture.grid().scroll_view, 0);
        fixture.feed(&format!("\x1b[?{mode}l"));
        fixture.assert_painted_bar();
        assert_eq!(fixture.grid().scrollback.len(), saved);
        assert_eq!(fixture.grid().scroll_view, 100);
        assert!(fixture.input.lock().unwrap().is_empty());
    }
}

#[test]
fn disappearing_surfaces_and_popups_cannot_retarget_a_scrollbar_drag_or_swallow_release() {
    for change in 0..4 {
        let mut fixture = Fixture::new(0);
        fixture.history();
        let bar = fixture.bar();
        fixture.mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            bar.area.bottom() - 1,
        );
        match change {
            0 => fixture.app.change_dir_active = true,
            1 => {
                fixture.app.show_terminal_sidebar = false;
                fixture.draw(120, 30);
            }
            2 => {
                fixture.draw(90, 22);
            }
            _ => fixture.app.close_terminal_tab_at(0),
        }
        fixture.mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0);
        fixture.mouse(MouseEventKind::Up(MouseButton::Left), 0, 0);
        assert!(fixture.app.terminal_scrollbar.drag.is_none());
        assert!(fixture.app.terminal_selection_drag.is_none());
        assert!(fixture.app.terminal_mouse_capture.is_none());
        assert_eq!(
            fixture.app.editor().unwrap().rope.to_string(),
            "keep this document"
        );
        if change == 1 || change == 2 {
            assert_eq!(fixture.grid().scroll_view, 0);
        }
        assert!(fixture.input.lock().unwrap().is_empty());
    }
}

#[test]
fn narrow_and_empty_terminal_rectangles_keep_valid_content_and_thumb_bounds() {
    for width in 0..4 {
        for height in 0..4 {
            let area = Rect::new(2, 3, width, height);
            let content = content_area(area);
            assert!(content.width <= area.width);
            assert_eq!(content.height, area.height);
            if width > 0 {
                assert!(content.width > 0);
            }
            let grid = TermGrid::new(usize::from(content.width), usize::from(height));
            let metrics = Metrics::new(&grid, Rect::new(content.right(), area.y, 1, height));
            assert!(metrics.thumb_top + metrics.thumb_height <= height);
            assert_eq!(metrics.offset_at(u16::MAX), 0);
            assert_eq!(metrics.drag_offset(u16::MAX, 0, usize::MAX), 0);
        }
    }
}
