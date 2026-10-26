use std::sync::Arc;

use ratatui::layout::Rect;

use crate::app::{App, DiffMode, DiffRenderCache};
use crate::editor::Editor;

/// Scroll in rendered diff rows, including rows with no modified-file line.
#[derive(Default)]
pub(crate) struct InlineDiffScroll {
    pub aligned: bool,
    pub row: usize,
    pub column: usize,
    pub area: Rect,
    pub text_area: Rect,
    pub vertical_area: Rect,
    pub horizontal_area: Option<Rect>,
    pub cache: Arc<DiffRenderCache>,
    pub change_rows: crate::ui_editor_minimap::ChangeRows,
    overview_geometry: Option<(usize, usize, bool)>,
    source_scroll: Option<usize>,
    cursor_line: Option<usize>,
    terminal_empty_line: Option<usize>,
    pub(crate) original_terminal: bool,
    vertical_grab: usize,
    horizontal_grab: usize,
}

pub(crate) fn scrollbar_thumb(
    total: usize,
    visible: usize,
    track: usize,
    min_thumb: usize,
) -> (usize, usize) {
    let size = visible
        .saturating_mul(track)
        .checked_div(total)
        .unwrap_or(track);
    (
        total.saturating_sub(visible),
        size.max(min_thumb).min(track),
    )
}

impl InlineDiffScroll {
    pub fn row_count(&self) -> usize {
        let rows = if self.aligned {
            self.cache.rows.len()
        } else {
            self.cache.inline_rows.len()
        };
        rows + usize::from(
            self.terminal_empty_line.is_some() || self.aligned && self.original_terminal,
        )
    }

    pub(crate) fn row_for_line(&self, cache: &DiffRenderCache, line: usize) -> usize {
        if self.terminal_empty_line == Some(line) {
            return if self.aligned {
                cache.rows.len()
            } else {
                cache.inline_rows.len()
            };
        }
        if self.aligned {
            cache.aligned_row_for_target_line(line)
        } else {
            cache.inline_row_for_target_line(line)
        }
    }

    pub fn max_row(&self) -> usize {
        self.row_count()
            .saturating_sub(self.text_area.height.max(1) as usize)
    }

    pub(crate) fn source_scroll_matches(&self, source_line: usize) -> bool {
        self.source_scroll == Some(source_line)
    }

    pub fn max_column(&self) -> usize {
        self.cache
            .max_inline_text_width
            .saturating_sub(self.text_area.width as usize)
    }

    pub fn layout(&mut self, cache: Arc<DiffRenderCache>, editor: &mut Editor) {
        let last = editor.line_count().saturating_sub(1);
        self.terminal_empty_line = (cache.context_lines.is_none()
            && last == cache.target_content_lines()
            && editor.rope.line(last).len_chars() == 0)
            .then_some(last);
        let height = self.vertical_area.height as usize;
        let total = if self.aligned {
            cache.rows.len()
        } else {
            cache.inline_rows.len()
        };
        let total = total
            + usize::from(
                self.terminal_empty_line.is_some() || self.aligned && self.original_terminal,
            );
        let geometry = (height, total, self.aligned);
        if !Arc::ptr_eq(&self.cache, &cache) || self.overview_geometry != Some(geometry) {
            self.overview_geometry = Some(geometry);
            self.change_rows =
                crate::ui_editor_minimap::ChangeRows::project(height, total, |start, end| {
                    cache.overview_status_in_range(self.aligned, start, end)
                });
        }
        if !Arc::ptr_eq(&self.cache, &cache) || self.source_scroll != Some(editor.scroll_offset) {
            self.row = if cache.target_content_lines() == 0 {
                0
            } else {
                self.row_for_line(&cache, editor.scroll_offset)
            };
        }
        if self
            .cursor_line
            .is_some_and(|line| line != editor.cursor_line)
        {
            let cursor = self.row_for_line(&cache, editor.cursor_line);
            if cursor < self.row {
                self.row = cursor;
            } else if cursor >= self.row + self.text_area.height as usize {
                self.row = cursor
                    .saturating_add(1)
                    .saturating_sub(self.text_area.height as usize);
            }
        }
        self.cursor_line = Some(editor.cursor_line);
        self.cache = cache;
        self.row = self.row.min(self.max_row());
        self.column = editor.horizontal_scroll.min(self.max_column());
        editor.viewport_height = self.text_area.height.max(1) as usize;
        editor.viewport_width = self.text_area.width.max(1) as usize;
        self.sync_editor(editor);
    }

    pub fn sync_editor(&mut self, editor: &mut Editor) {
        let content_rows = if self.aligned {
            self.cache.rows.len()
        } else {
            self.cache.inline_rows.len()
        };
        editor.scroll_offset = (if self.row == content_rows
            && let Some(line) = self.terminal_empty_line
        {
            line
        } else if self.aligned {
            self.cache.target_line_for_aligned_row(self.row)
        } else {
            self.cache.target_line_for_inline_row(self.row)
        })
        .min(editor.line_count().saturating_sub(1));
        editor.horizontal_scroll = self.column;
        self.source_scroll = Some(editor.scroll_offset);
    }

    /// A card's visible range belongs to the outer review scrollbar. Preserve
    /// exact deletion-only row anchors and allow its tail to occupy fewer than
    /// a viewport's rows, without the native pane clamping it back to the top.
    pub(crate) fn layout_review(&mut self, cache: Arc<DiffRenderCache>, editor: &mut Editor, row: usize) {
        self.layout(cache, editor);
        self.row = row.min(self.row_count().saturating_sub(1));
        self.sync_editor(editor);
    }

    pub(crate) fn center_cursor(&mut self, editor: &mut Editor) {
        let cursor = if !self.aligned && self.terminal_empty_line != Some(editor.cursor_line) {
            // Inline scroll anchors include preceding deletions. Center the
            // actual matching text, using the ordered display-row index.
            let aligned = self.cache.aligned_row_for_target_line(editor.cursor_line);
            self.cache.inline_rows.partition_point(|entry| {
                entry.aligned_row < aligned || entry.aligned_row == aligned && entry.kind == '-'
            })
        } else {
            self.row_for_line(&self.cache, editor.cursor_line)
        };
        self.row = cursor
            .saturating_sub(self.text_area.height as usize / 2)
            .min(self.max_row());
        self.sync_editor(editor);
    }

    pub fn scroll(&mut self, editor: &mut Editor, horizontal: bool, delta: isize) {
        if horizontal {
            self.column = self
                .column
                .saturating_add_signed(delta)
                .min(self.max_column());
        } else {
            self.row = self.row.saturating_add_signed(delta).min(self.max_row());
        }
        self.sync_editor(editor);
    }

    pub fn scrollbar(&mut self, editor: &mut Editor, position: u16, horizontal: bool, begin: bool) {
        let row_count = self.row_count();
        let (area, total, visible, offset, grab) = if horizontal {
            let Some(area) = self.horizontal_area else {
                return;
            };
            (
                area,
                self.cache.max_inline_text_width,
                self.text_area.width as usize,
                &mut self.column,
                &mut self.horizontal_grab,
            )
        } else {
            (
                self.vertical_area,
                row_count,
                self.text_area.height as usize,
                &mut self.row,
                &mut self.vertical_grab,
            )
        };
        let (start, track) = if horizontal {
            (area.x, area.width)
        } else {
            (area.y, area.height)
        };
        let track = track as usize;
        let (max_scroll, thumb) =
            scrollbar_thumb(total, visible, track, if horizontal { 2 } else { 1 });
        let travel = track.saturating_sub(thumb);
        let top = offset
            .saturating_mul(travel)
            .checked_div(max_scroll)
            .unwrap_or(0);
        let relative = position.saturating_sub(start) as usize;
        if begin {
            if (top..top + thumb).contains(&relative) {
                *grab = relative - top;
                return;
            }
            *grab = thumb / 2;
        }
        *offset = relative
            .saturating_sub(*grab)
            .min(travel)
            .saturating_mul(max_scroll)
            .checked_div(travel)
            .unwrap_or(0);
        self.sync_editor(editor);
    }
}

impl App {
    pub(crate) fn inline_diff_active(&self) -> bool {
        self.diff_mode == DiffMode::Inline && self.should_render_diff_for_active_editor()
    }

    pub(crate) fn row_diff_active(&self) -> bool {
        self.inline_diff_active()
            || self.diff_mode == DiffMode::SideBySide && self.should_render_diff_for_active_editor()
    }

    pub(crate) fn inline_diff_scrollbar_input(
        &mut self,
        position: u16,
        horizontal: bool,
        begin: bool,
    ) -> bool {
        if !self.row_diff_active() {
            return false;
        }
        let mut scroll = std::mem::take(&mut self.diff_inline_scroll);
        if let Some(editor) = self.editor_mut() {
            scroll.scrollbar(editor, position, horizontal, begin);
        }
        self.diff_inline_scroll = scroll;
        true
    }

    pub(crate) fn inline_diff_navigation(&mut self, key: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode;
        if !self.row_diff_active() {
            return false;
        }
        let height = self.diff_inline_scroll.text_area.height.max(1) as isize;
        let delta = match key {
            KeyCode::PageUp => -height,
            KeyCode::PageDown => height,
            _ => return false,
        };
        let mut scroll = std::mem::take(&mut self.diff_inline_scroll);
        if let Some(editor) = self.editor_mut() {
            scroll.scroll(editor, false, delta);
        }
        self.diff_inline_scroll = scroll;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Tab;
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    fn comparison(base: &str, target: &str) -> App {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some("diff.txt".into());
        editor.rope = ropey::Rope::from_str(target);
        editor.read_only = true;
        app.tabs.push(Tab::new_file(editor));
        app.diff_mode = DiffMode::Inline;
        app.show_minimap = false;
        app.compare_base_content = Some(base.to_string().into());
        app.set_compare_diff_lines(crate::app::compute_file_diff(base, target));
        app
    }

    fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
        terminal
            .draw(|frame| {
                let area = frame.area();
                app.editor_area = Some(area);
                crate::ui_diff_display::draw_inline_diff(frame, app, area);
            })
            .unwrap();
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers,
            },
        );
    }

    fn text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn inline_diff_wheel_and_pages_reach_deleted_rows_beyond_the_target() {
        let base: String = (0..60).map(|line| format!("removed {line:02}\n")).collect();
        for target in ["", "kept\n"] {
            let mut app = comparison(&base, target);
            app.editor_mut().unwrap().word_wrap = true;
            let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
            draw(&mut app, &mut terminal);
            let code = app.code_area.unwrap();
            let scrollbar = app.scrollbar_area.unwrap();
            assert_eq!(
                terminal
                    .backend()
                    .buffer()
                    .cell((scrollbar.x, scrollbar.y))
                    .unwrap()
                    .symbol(),
                "█"
            );
            mouse(
                &mut app,
                MouseEventKind::ScrollDown,
                code.x,
                code.y,
                KeyModifiers::NONE,
            );
            draw(&mut app, &mut terminal);
            assert!(app.diff_inline_scroll.row > 0);
            for _ in 0..30 {
                crate::input::handle_editor_key(
                    &mut app,
                    KeyCode::PageDown,
                    KeyModifiers::NONE,
                );
                draw(&mut app, &mut terminal);
            }
            assert_eq!(app.diff_inline_scroll.row, app.diff_inline_scroll.max_row());
            assert!(text(&terminal).contains("removed 59"));
            mouse(
                &mut app,
                MouseEventKind::ScrollUp,
                code.x,
                code.y,
                KeyModifiers::NONE,
            );
            draw(&mut app, &mut terminal);
            assert!(app.diff_inline_scroll.row < app.diff_inline_scroll.max_row());
            for _ in 0..30 {
                crate::input::handle_editor_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
            }
            draw(&mut app, &mut terminal);
            assert_eq!(app.diff_inline_scroll.row, 0);
            assert!(text(&terminal).contains("removed 00"));
        }
    }

    #[test]
    fn inline_diff_scrollbars_drag_to_both_ends_and_keep_the_gutter_fixed() {
        let base: String = (0..40)
            .map(|line| format!("removed {line:02} {} END\n", "x".repeat(100)))
            .collect();
        let mut app = comparison(&base, "new\n");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        draw(&mut app, &mut terminal);
        let vertical = app.scrollbar_area.unwrap();
        let horizontal = app
            .h_scrollbar_area
            .expect("horizontal scrollbar for removed text");
        mouse(
            &mut app,
            MouseEventKind::Moved,
            vertical.x,
            vertical.y,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((vertical.x, vertical.y))
                .unwrap()
                .bg,
            app.colors.ui.scrollbar_colors(app.colors.ui.bg_overview, true, false).1
        );
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            vertical.x,
            vertical.y,
            KeyModifiers::NONE,
        );
        assert_eq!(
            app.diff_inline_scroll.row, 0,
            "grabbing the thumb must not jump"
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            vertical.x,
            vertical.bottom() + 5,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            vertical.x,
            vertical.bottom(),
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(app.diff_inline_scroll.row, app.diff_inline_scroll.max_row());
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            horizontal.x,
            horizontal.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            horizontal.right() + 5,
            horizontal.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            horizontal.right(),
            horizontal.y,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(
            app.diff_inline_scroll.column,
            app.diff_inline_scroll.max_column()
        );
        assert!(text(&terminal).contains("END"));
        assert_eq!(
            terminal.backend().buffer().cell((10, 1)).unwrap().symbol(),
            "-"
        );
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            horizontal.x,
            horizontal.y,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(app.diff_inline_scroll.column, 0);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            vertical.x,
            vertical.y,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(app.diff_inline_scroll.row, 0);
    }

    #[test]
    fn inline_diff_scroll_survives_inactive_split_rendering_and_wheel_input() {
        use crate::app::{SplitDirection, TabKind};
        let base: String = (0..60).map(|line| format!("removed {line}\n")).collect();
        let mut app = comparison(&base, "kept\n");
        app.tabs[0].kind = TabKind::Diff(1);
        app.diff_tabs.owner = Some(1);
        app.tabs.insert(0, Tab::new_file(Editor::new()));
        app.active_tab = 1;
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        let diff_group = app.active_group;
        let mut terminal = Terminal::new(TestBackend::new(120, 15)).unwrap();
        let render = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| {
                    crate::ui_chrome::draw_editor_section(frame, app, frame.area());
                })
                .unwrap();
        };
        render(&mut app, &mut terminal);
        let code = app.code_area.unwrap();
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            code.x,
            code.y,
            KeyModifiers::NONE,
        );
        render(&mut app, &mut terminal);
        let first_row = app.diff_inline_scroll.row;
        assert!(first_row > 0);
        app.switch_group(0);
        render(&mut app, &mut terminal);
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            code.x,
            code.y,
            KeyModifiers::NONE,
        );
        assert_eq!(app.active_group, 0);
        render(&mut app, &mut terminal);
        app.switch_group(diff_group);
        render(&mut app, &mut terminal);
        assert!(app.diff_inline_scroll.row > first_row);
    }

    #[test]
    fn inline_diff_scroll_clamps_after_resize_and_content_refresh() {
        let base: String = (0..60)
            .map(|line| format!("removed {line} {} END\n", "x".repeat(100)))
            .collect();
        let mut app = comparison(&base, "kept\n");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        draw(&mut app, &mut terminal);
        let code = app.code_area.unwrap();
        app.scroll_diff_at(code.x, code.y, false, 1000);
        app.scroll_diff_at(code.x, code.y, true, 1000);
        terminal.backend_mut().resize(140, 30);
        terminal.resize(Rect::new(0, 0, 140, 30)).unwrap();
        draw(&mut app, &mut terminal);
        assert!(app.diff_inline_scroll.row <= app.diff_inline_scroll.max_row());
        assert_eq!(app.diff_inline_scroll.column, 0);
        assert!(app.h_scrollbar_area.is_none());
        app.compare_base_content = Some("old\n".into());
        app.set_compare_diff_lines(crate::app::compute_file_diff("old\n", "kept\n"));
        draw(&mut app, &mut terminal);
        assert_eq!(app.diff_inline_scroll.row, 0);
        assert!(text(&terminal).contains("old"));
        assert!(text(&terminal).contains("kept"));
    }

    #[test]
    fn inline_diff_horizontal_scroll_reaches_the_end_of_wide_unicode_text() {
        let base = format!("{} END\n", "界".repeat(30));
        let mut app = comparison(&base, "short\n");
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        draw(&mut app, &mut terminal);
        let code = app.code_area.unwrap();
        assert!(app.h_scrollbar_area.is_some());
        app.scroll_diff_at(code.x, code.y, true, 1000);
        draw(&mut app, &mut terminal);
        assert!(text(&terminal).contains("END"));
    }

    #[test]
    fn inline_diff_horizontal_wheel_works_even_with_word_wrap_enabled() {
        let mut app = comparison(&format!("{} END\n", "long ".repeat(30)), "short\n");
        app.editor_mut().unwrap().word_wrap = true;
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        draw(&mut app, &mut terminal);
        let code = app.code_area.unwrap();
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            code.x,
            code.y,
            KeyModifiers::SHIFT,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(app.diff_inline_scroll.column, 6);
        mouse(
            &mut app,
            MouseEventKind::ScrollRight,
            code.x,
            code.y,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(app.diff_inline_scroll.column, 12);
        mouse(
            &mut app,
            MouseEventKind::ScrollLeft,
            code.x,
            code.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::ScrollUp,
            code.x,
            code.y,
            KeyModifiers::SHIFT,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(app.diff_inline_scroll.column, 0);
    }

    #[test]
    fn fractional_overview_refreshes_for_layout_and_terminal_line_changes_without_a_new_cache() {
        let base = "before\nold_a\nold_b\nafter";
        let target = "before\nnew\nafter";
        let cache = Arc::new(DiffRenderCache::new(&crate::app::compute_file_diff(
            base, target,
        )));
        let mut scroll = InlineDiffScroll::default();
        scroll.vertical_area = Rect::new(20, 0, 2, 17);
        scroll.text_area = Rect::new(0, 0, 20, 3);
        for aligned in [false, true, false, true] {
            scroll.aligned = aligned;
            for terminal in [false, true, false] {
                let mut editor = Editor::test_with_text(&format!(
                    "{target}{}",
                    if terminal { "\n" } else { "" }
                ));
                for original_terminal in [false, true, false] {
                    scroll.original_terminal = original_terminal;
                    scroll.layout(Arc::clone(&cache), &mut editor);
                    let kinds: Vec<_> = if aligned {
                        cache
                            .rows
                            .iter()
                            .map(|row| {
                                if row.right_kind == '+' {
                                    '+'
                                } else {
                                    row.left_kind
                                }
                            })
                            .collect()
                    } else {
                        cache.inline_rows.iter().map(|row| row.kind).collect()
                    };
                    let expected = crate::ui_editor_minimap::ChangeRows::from_lines(
                        17,
                        scroll.row_count(),
                        kinds
                            .iter()
                            .enumerate()
                            .filter(|(_, kind)| **kind != ' ')
                            .map(|(row, kind)| (row, if *kind == '-' { 'D' } else { 'U' })),
                    );
                    assert_eq!(
                        scroll.change_rows, expected,
                        "aligned={aligned}, terminal={terminal}, original_terminal={original_terminal}"
                    );
                }
            }
        }
    }

    #[test]
    fn indexed_overview_buckets_match_full_row_projection_after_resizing() {
        let base = "first\nold_a\nold_b\nold_c\nmiddle\nlast\n";
        let target = "first\nnew_a\nmiddle\nextra_a\nextra_b\nlast\n";
        let full = DiffRenderCache::new(&crate::app::compute_file_diff(base, target));
        for cache in [Arc::new(full.clone()), Arc::new(full.with_context(1))] {
            for aligned in [false, true] {
                let kinds: Vec<_> = if aligned {
                    cache
                        .rows
                        .iter()
                        .map(|row| {
                            if row.right_kind == '+' {
                                '+'
                            } else {
                                row.left_kind
                            }
                        })
                        .collect()
                } else {
                    cache.inline_rows.iter().map(|row| row.kind).collect()
                };
                let mut editor = Editor::test_with_text(target);
                let mut scroll = InlineDiffScroll {
                    aligned,
                    ..Default::default()
                };
                scroll.text_area = Rect::new(0, 0, 20, 8);
                for height in [0, 1, 2, 3, 8, 17, 41, 2] {
                    scroll.vertical_area = Rect::new(20, 0, 2, height);
                    scroll.layout(Arc::clone(&cache), &mut editor);
                    let expected = crate::ui_editor_minimap::ChangeRows::from_lines(
                        height as usize,
                        scroll.row_count(),
                        kinds
                            .iter()
                            .enumerate()
                            .filter(|(_, kind)| **kind != ' ')
                            .map(|(row, kind)| (row, if *kind == '-' { 'D' } else { 'U' })),
                    );
                    assert_eq!(
                        scroll.change_rows, expected,
                        "aligned={aligned}, height={height}"
                    );
                }
            }
        }
    }
}
