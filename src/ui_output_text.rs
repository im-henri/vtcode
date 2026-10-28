//! A read-only panel buffer using the editor's rope, navigation and coordinate
//! conversion. It never borrows focus or document state from an editor tab.
use super::*;
use crate::text_input::{TextInput, TextInputHistory, TextInputHitMap};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;

pub(super) fn line_width(text: &str) -> usize {
    let mut cells = 0;
    for glyph in text.graphemes(true) {
        cells += layout::glyph_width(glyph, cells);
    }
    cells
}

#[derive(Clone, Copy)]
struct Bar {
    area: Rect,
    offset: usize,
    maximum: usize,
    thumb: u16,
    length: u16,
    horizontal: bool,
}
impl Bar {
    fn new(area: Rect, offset: usize, maximum: usize, visible: usize, horizontal: bool) -> Self {
        let length = if horizontal { area.width } else { area.height };
        let thumb = ((length as u128 * visible as u128) / (maximum + visible).max(1) as u128)
            .max(1)
            .min(length as u128) as u16;
        Self {
            area,
            offset,
            maximum,
            thumb,
            length,
            horizontal,
        }
    }
    fn top(self) -> u16 {
        (self.offset as u128 * self.length.saturating_sub(self.thumb) as u128
            / self.maximum.max(1) as u128) as u16
    }
    fn coordinate(self, mouse: MouseEvent) -> u16 {
        if self.horizontal {
            mouse.column
        } else {
            mouse.row
        }
    }
    fn origin(self) -> u16 {
        if self.horizontal {
            self.area.x
        } else {
            self.area.y
        }
    }
    fn at(self, position: u16, grab: u16) -> usize {
        let travel = self.length.saturating_sub(self.thumb);
        let position = position
            .saturating_sub(self.origin())
            .saturating_sub(grab)
            .min(travel);
        (position as u128 * self.maximum as u128 / travel.max(1) as u128) as usize
    }
    fn draw(
        self,
        frame: &mut Frame,
        colors: &crate::app_theme::ThemeColors,
        hovered: bool,
        dragging: bool,
    ) {
        let (track, thumb) = colors
            .ui
            .scrollbar_colors(colors.ui.bg_panel, hovered, dragging);
        let top = self.top();
        for i in 0..self.length {
            let point = if self.horizontal {
                (self.area.x + i, self.area.y)
            } else {
                (self.area.x, self.area.y + i)
            };
            if let Some(cell) = frame.buffer_mut().cell_mut(point) {
                cell.set_symbol(if self.horizontal { "▄" } else { "▐" })
                    .set_style(Style::default().bg(track).fg(
                        if (top..top + self.thumb).contains(&i) {
                            thumb
                        } else {
                            track
                        },
                    ));
            }
        }
    }
}

#[derive(Default)]
struct Find {
    open: bool,
    focused: bool,
    query: String,
    cursor: usize,
    anchor: Option<usize>,
    history: TextInputHistory,
    hit: Option<TextInputHitMap>,
    matches: Vec<usize>,
    version: u64,
    searched: String,
    case_sensitive: bool,
    searched_case: bool,
    pattern: Option<regex::Regex>,
    snapshot: Option<ropey::Rope>,
    case: Rect,
    previous: Rect,
    next: Rect,
    close: Rect,
    drag_anchor: Option<usize>,
}
impl Find {
    fn update(&mut self, editor: &Editor) {
        let same_query = self.searched == self.query && self.searched_case == self.case_sensitive;
        if !self.open || (same_query && self.version == editor.content_version) {
            return;
        }
        if !same_query {
            self.pattern = (!self.query.is_empty())
                .then(|| {
                    regex::RegexBuilder::new(&regex::escape(&self.query))
                        .case_insensitive(!self.case_sensitive)
                        .build()
                        .ok()
                })
                .flatten();
        }
        // Appends keep existing matches and inspect only the newly affected
        // line. Prefix retention or a new query rebuilds the bounded log search.
        let from = if same_query {
            self.snapshot
                .as_ref()
                .and_then(|old| {
                    let change = crate::rope_changes::changed_chars(old, &editor.rope);
                    (change.0.start == old.len_chars()).then(|| {
                        editor
                            .rope
                            .line_to_char(editor.rope.char_to_line(change.1.start))
                    })
                })
                .unwrap_or(0)
        } else {
            0
        };
        self.matches
            .truncate(self.matches.partition_point(|&position| position < from));
        if !self.query.is_empty() {
            if self.case_sensitive {
                let search = crate::editor::RopeSearch::new(&self.query);
                self.matches
                    .extend(search.matches(&editor.rope, from, editor.rope.len_chars()));
            } else if let Some(pattern) = &self.pattern {
                let first = editor.rope.char_to_line(from);
                let mut base = from;
                for line in editor.rope.lines_at(first) {
                    let text = line
                        .as_str()
                        .map(std::borrow::Cow::Borrowed)
                        .unwrap_or_else(|| std::borrow::Cow::Owned(line.to_string()));
                    let mut byte = 0;
                    let mut characters = 0;
                    for found in pattern.find_iter(&text) {
                        characters += text[byte..found.start()].chars().count();
                        self.matches.push(base + characters);
                        characters += found.as_str().chars().count();
                        byte = found.end();
                    }
                    base += line.len_chars();
                }
            }
        }
        self.searched.clone_from(&self.query);
        self.searched_case = self.case_sensitive;
        self.version = editor.content_version;
        self.snapshot = Some(editor.rope.clone());
    }
    fn jump(&self, editor: &mut Editor, backwards: bool, include_current: bool) {
        if self.matches.is_empty() {
            return;
        }
        let current = editor.clamped_selection_indices().map_or_else(
            || editor.rope.line_to_char(editor.cursor_line) + editor.cursor_col,
            |(start, _, _, _)| start,
        );
        let position = if backwards {
            let before = self.matches.partition_point(|&index| index < current);
            if before == 0 {
                self.matches.len() - 1
            } else {
                before - 1
            }
        } else {
            let after = self.matches.partition_point(|&index| {
                if include_current {
                    index < current
                } else {
                    index <= current
                }
            });
            after % self.matches.len()
        };
        let start = self.matches[position];
        let end = start + self.query.chars().count();
        let start_line = editor.rope.char_to_line(start);
        let end_line = editor.rope.char_to_line(end);
        editor.set_cursor_without_scrolling(end_line, end - editor.rope.line_to_char(end_line));
        editor.selection_anchor = Some((start_line, start - editor.rope.line_to_char(start_line)));
        editor.scroll_offset = start_line.saturating_sub(editor.viewport_height / 2);
    }
}

pub(super) struct Reader {
    pub editor: Editor,
    pub view: View,
    pub area: Rect,
    pub text: Rect,
    pub horizontal: usize,
    pub wrap: bool,
    layout: layout::Layout,
    rows: Vec<(usize, layout::Segment)>,
    pending_anchor: Option<(usize, usize)>,
    vertical_bar: Option<Bar>,
    horizontal_bar: Option<Bar>,
    bar_drag: Option<(Bar, u16)>,
    selecting: bool,
    last_click: Option<(std::time::Instant, (u16, u16), u8)>,
    find: Find,
}
impl Default for Reader {
    fn default() -> Self {
        let mut editor = Editor::new();
        editor.read_only = true;
        editor.word_wrap = false;
        editor.scroll_past_end = false;
        editor.whitespace_mode = crate::app::WhitespaceMode::None;
        editor.language_override = Some("plain text".into());
        Self {
            editor,
            view: View::default(),
            area: Rect::default(),
            text: Rect::default(),
            horizontal: 0,
            wrap: true,
            layout: layout::Layout::default(),
            rows: Vec::new(),
            pending_anchor: None,
            vertical_bar: None,
            horizontal_bar: None,
            bar_drag: None,
            selecting: false,
            last_click: None,
            find: Find::default(),
        }
    }
}
impl Reader {
    fn offset(&self) -> usize {
        if self.wrap {
            self.editor.wrap_scroll_offset
        } else {
            self.editor.scroll_offset
        }
    }
    fn total_rows(&self) -> usize {
        if self.wrap {
            self.layout.total()
        } else {
            self.editor.rope.len_lines()
        }
    }
    fn scroll_to(&mut self, offset: usize) {
        let offset = offset.min(self.total_rows().saturating_sub(self.text.height as usize));
        if self.wrap {
            self.editor.wrap_scroll_offset = offset;
            self.editor.scroll_offset = self.layout.line_at(offset).map_or(0, |(line, _)| line);
        } else {
            self.editor.scroll_offset = offset;
        }
    }
    fn scroll_by(&mut self, delta: isize) {
        self.scroll_to(self.offset().saturating_add_signed(delta));
    }
    fn scrolls_past_end(&self, rows: usize) -> bool {
        rows > self.total_rows()
            .saturating_sub(self.text.height as usize)
            .saturating_sub(self.offset())
    }
    fn row_at(&self, row: usize) -> Option<(usize, layout::Segment)> {
        if self.wrap {
            self.layout.segment_at(&self.editor, row)
        } else {
            self.editor.rope.get_line(row).map(|line| {
                (
                    row,
                    layout::Segment {
                        start: 0,
                        end: Editor::line_content_len(line),
                        cell: 0,
                    },
                )
            })
        }
    }
    fn position_on_row(&self, row: usize, target: usize) -> (usize, usize) {
        let Some((line, segment)) = self.row_at(row.min(self.total_rows().saturating_sub(1)))
        else {
            return (0, 0);
        };
        let body = self
            .editor
            .rope
            .line(line)
            .slice(segment.start..segment.end);
        let text = body
            .as_str()
            .map(std::borrow::Cow::Borrowed)
            .unwrap_or_else(|| std::borrow::Cow::Owned(body.to_string()));
        let (mut cell, mut column) = (segment.cell, segment.start);
        for glyph in text.graphemes(true) {
            let full = layout::glyph_width(glyph, cell);
            let width = if self.wrap {
                full.min(self.text.width.max(1) as usize)
            } else {
                full
            };
            if target < cell - segment.cell + width {
                break;
            }
            cell += full;
            column += glyph.chars().count();
        }
        (line, column)
    }
    fn move_vertical(&mut self, delta: isize) {
        let row = self.layout.row_for_position(
            &self.editor,
            self.editor.cursor_line,
            self.editor.cursor_col,
        );
        let first_cell = self.row_at(row).map_or(0, |(_, segment)| segment.cell);
        let desired = self.editor.desired_col.unwrap_or_else(|| {
            self.column_x(self.editor.cursor_line, self.editor.cursor_col)
                .saturating_sub(first_cell)
        });
        let target = row
            .saturating_add_signed(delta)
            .min(self.total_rows().saturating_sub(1));
        let (line, column) = self.position_on_row(target, desired);
        self.editor.set_cursor_without_scrolling(line, column);
        self.editor.desired_col = Some(desired);
    }
    fn jump_find(&mut self, backwards: bool, include_current: bool) {
        if self.find.matches.is_empty() {
            return;
        }
        self.find.jump(&mut self.editor, backwards, include_current);
        if self.wrap {
            let (line, column) = self
                .editor
                .selection_anchor
                .unwrap_or((self.editor.cursor_line, self.editor.cursor_col));
            let row = self.layout.row_for_position(&self.editor, line, column);
            self.scroll_to(row.saturating_sub(self.text.height as usize / 2));
        }
        self.reveal();
        self.view.autoscroll = false;
    }
    pub fn toggle_wrap(&mut self) {
        let anchor = self
            .row_at(self.offset())
            .map(|(line, segment)| (line, segment.start));
        self.wrap = !self.wrap;
        self.pending_anchor = anchor;
        self.horizontal = 0;
        self.bar_drag = None;
    }
    pub fn hide(&mut self) {
        self.area = Rect::default();
        self.text = Rect::default();
        self.selecting = false;
        self.bar_drag = None;
        self.find.focused = false;
        self.find.drag_anchor = None;
    }
    fn reveal(&mut self) {
        if self.wrap {
            let row = self.layout.row_for_position(
                &self.editor,
                self.editor.cursor_line,
                self.editor.cursor_col,
            );
            let offset = self.offset();
            if row < offset {
                self.scroll_to(row);
            } else if row >= offset + self.text.height as usize {
                self.scroll_to(row.saturating_sub(self.text.height.saturating_sub(1) as usize));
            }
            self.horizontal = 0;
            return;
        }
        let x = self.column_x(self.editor.cursor_line, self.editor.cursor_col);
        if x < self.horizontal {
            self.horizontal = x;
        } else if x >= self.horizontal.saturating_add(self.text.width as usize) {
            self.horizontal = x.saturating_sub(self.text.width.saturating_sub(1) as usize);
        }
    }
    pub fn resume_autoscroll(&mut self) {
        self.view.autoscroll = true;
        self.horizontal = 0;
        end(&mut self.editor);
        self.scroll_to(self.total_rows().saturating_sub(self.text.height as usize));
    }
    pub fn clear(&mut self, log: Option<&Log>) {
        self.editor.rope = ropey::Rope::new();
        self.editor.content_version = self.editor.content_version.wrapping_add(1);
        self.editor.set_cursor_without_scrolling(0, 0);
        self.editor.clear_selection();
        self.editor.scroll_offset = 0;
        self.horizontal = 0;
        self.layout = layout::Layout::default();
        self.rows.clear();
        self.pending_anchor = None;
        self.view = View::default();
        self.view.log = log.cloned();
        self.view.version = log.map_or(0, Log::version);
        self.find.version = u64::MAX;
    }
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        colors: &crate::app_theme::ThemeColors,
        area: Rect,
        focused: bool,
        mouse: (u16, u16),
        log: Option<&Log>,
        source: Source,
    ) {
        self.area = area;
        let bg = colors.ui.bg_panel;
        let fg = colors.ui.readable_foreground(colors.fg_text, bg, 4.5);
        let style = Style::default().fg(fg).bg(bg);
        frame.render_widget(Paragraph::new("").style(style), area);
        if area.is_empty() {
            self.hide();
            return;
        }
        let find_height = u16::from(self.find.open && area.height >= 2);
        let raw = Rect::new(
            area.x,
            area.y + find_height,
            area.width,
            area.height - find_height,
        );
        self.editor.viewport_height = raw.height as usize;
        let old_first_line = self.view.first_line;
        let anchor = self.pending_anchor.take().or_else(|| {
            (self.wrap && self.layout.width > 0)
                .then(|| self.row_at(self.offset()))
                .flatten()
                .map(|(line, segment)| (line, segment.start))
        });
        self.view.refresh(&mut self.editor, log);
        self.find.update(&self.editor);
        let width = self
            .view
            .widths
            .last_key_value()
            .map_or(0, |(&width, _)| width);
        let mut vertical = self.wrap && self.vertical_bar.is_some();
        let mut horizontal = false;
        if self.wrap {
            let candidate = raw.width.saturating_sub(u16::from(vertical)).max(1) as usize;
            self.layout.sync(&self.editor, &self.view, candidate);
            vertical = raw.width > 1 && self.layout.total() > raw.height as usize;
            let final_width = raw.width.saturating_sub(u16::from(vertical)).max(1) as usize;
            if final_width != candidate {
                self.layout.sync(&self.editor, &self.view, final_width);
            }
        } else {
            for _ in 0..2 {
                vertical |= raw.width > 1
                    && self.editor.rope.len_lines()
                        > raw.height.saturating_sub(u16::from(horizontal)) as usize;
                horizontal |= raw.height > 1
                    && width > raw.width.saturating_sub(u16::from(vertical)) as usize;
            }
        }
        self.text = Rect::new(
            raw.x,
            raw.y,
            raw.width.saturating_sub(u16::from(vertical)),
            raw.height.saturating_sub(u16::from(horizontal)),
        );
        self.editor.viewport_height = self.text.height as usize;
        self.editor.viewport_width = self.text.width as usize;
        let maximum = self.total_rows().saturating_sub(self.text.height as usize);
        if self.view.autoscroll {
            end(&mut self.editor);
            self.scroll_to(maximum);
        } else if let Some((line, column)) = anchor {
            let removed = self.view.first_line.saturating_sub(old_first_line) as usize;
            let (line, column) = if line < removed {
                (0, 0)
            } else {
                (line - removed, column)
            };
            let line = line.min(self.editor.rope.len_lines().saturating_sub(1));
            if self.wrap {
                self.scroll_to(self.layout.row_for_position(&self.editor, line, column));
            } else {
                self.editor.scroll_offset = line.min(maximum);
                self.horizontal = self.column_x(line, column);
            }
        } else {
            self.scroll_to(self.offset());
        }
        self.horizontal = if self.wrap {
            0
        } else {
            self.horizontal
                .min(width.saturating_sub(self.text.width as usize))
        };
        self.vertical_bar = vertical.then(|| {
            Bar::new(
                Rect::new(self.text.right(), self.text.y, 1, self.text.height),
                self.offset(),
                maximum,
                self.text.height as usize,
                false,
            )
        });
        self.horizontal_bar = horizontal.then(|| {
            Bar::new(
                Rect::new(self.text.x, self.text.bottom(), self.text.width, 1),
                self.horizontal,
                width.saturating_sub(self.text.width as usize),
                self.text.width as usize,
                true,
            )
        });
        let selection = self
            .editor
            .clamped_selection_indices()
            .map(|(start, end, _, _)| (start, end));
        let selection_style = style.bg(colors.bg_selection).fg(colors
            .ui
            .fg_editor_selection
            .unwrap_or_else(|| colors.ui.readable_foreground(fg, colors.bg_selection, 4.5)));
        let search_style = style
            .bg(colors.bg_search_match)
            .fg(colors
                .ui
                .readable_foreground(fg, colors.bg_search_match, 4.5));
        let needle_chars = self.find.query.chars().count();
        self.rows.clear();
        if self.wrap {
            if let Some((first, skip)) = self.layout.line_at(self.offset()) {
                'lines: for (index, line) in self.editor.rope.lines_at(first).enumerate() {
                    let body = line.slice(..Editor::line_content_len(line));
                    let text = body
                        .as_str()
                        .map(std::borrow::Cow::Borrowed)
                        .unwrap_or_else(|| std::borrow::Cow::Owned(body.to_string()));
                    for segment in layout::segments(&text, self.layout.width).skip(if index == 0 {
                        skip
                    } else {
                        0
                    }) {
                        if self.rows.len() == self.text.height as usize {
                            break 'lines;
                        }
                        self.rows.push((first + index, segment));
                    }
                }
            }
        } else {
            for line in self.editor.scroll_offset
                ..self
                    .editor
                    .scroll_offset
                    .saturating_add(self.text.height as usize)
            {
                if let Some(row) = self.row_at(line) {
                    self.rows.push(row);
                } else {
                    break;
                }
            }
        }
        let ink = super::styles::Ink::new(colors, bg);
        for (row, &(line_number, segment)) in self.rows.iter().enumerate() {
            let y = self.text.y + row as u16;
            let Some(line) = self.editor.rope.get_line(line_number) else {
                break;
            };
            let length = Editor::line_content_len(line);
            let prefix = line.slice(..length.min(64));
            let prefix = prefix
                .as_str()
                .map(std::borrow::Cow::Borrowed)
                .unwrap_or_else(|| std::borrow::Cow::Owned(prefix.to_string()));
            let header = super::styles::Header::parse(&prefix);
            let body = line.slice(segment.start..segment.end);
            let visible = body
                .as_str()
                .map(std::borrow::Cow::Borrowed)
                .unwrap_or_else(|| std::borrow::Cow::Owned(body.to_string()));
            let mut column = segment.start;
            let mut x = 0;
            let mut logical_cell = segment.cell;
            let line_start = self.editor.rope.line_to_char(line_number);
            for glyph in visible.graphemes(true) {
                let full_cells = layout::glyph_width(glyph, logical_cell);
                let cells = if self.wrap {
                    full_cells.min(self.text.width.max(1) as usize)
                } else {
                    full_cells
                };
                let absolute = line_start + column;
                let selected =
                    selection.is_some_and(|(start, end)| (start..end).contains(&absolute));
                let found = self
                    .find
                    .matches
                    .partition_point(|&start| start <= absolute)
                    .checked_sub(1)
                    .is_some_and(|i| absolute < self.find.matches[i] + needle_chars);
                let glyph_style = if selected {
                    selection_style
                } else if found {
                    search_style
                } else {
                    style.fg(ink.foreground(header, column))
                };
                let left = x.max(self.horizontal);
                let right = (x + cells).min(self.horizontal + self.text.width as usize);
                if left < right {
                    let rect = Rect::new(
                        self.text.x + (left - self.horizontal) as u16,
                        y,
                        (right - left) as u16,
                        1,
                    );
                    if glyph == "\t" || left != x || right != x + cells {
                        for column in rect.x..rect.right() {
                            if let Some(cell) = frame.buffer_mut().cell_mut((column, y)) {
                                cell.set_char(' ').set_style(glyph_style);
                            }
                        }
                    } else if glyph.len() == 1 && glyph.as_bytes()[0].is_ascii_graphic()
                        || glyph == " "
                    {
                        if let Some(cell) = frame.buffer_mut().cell_mut((rect.x, y)) {
                            cell.set_char(glyph.as_bytes()[0] as char)
                                .set_style(glyph_style);
                        }
                    } else if !glyph.chars().any(char::is_control) {
                        frame.buffer_mut().set_stringn(
                            rect.x,
                            y,
                            glyph,
                            rect.width as usize,
                            glyph_style,
                        );
                    }
                }
                x += cells;
                logical_cell += full_cells;
                column += glyph.chars().count();
                if x >= self.horizontal + self.text.width as usize {
                    break;
                }
            }
        }
        for bar in [self.vertical_bar, self.horizontal_bar]
            .into_iter()
            .flatten()
        {
            bar.draw(
                frame,
                colors,
                bar.area.contains(mouse.into()),
                self.bar_drag
                    .is_some_and(|(drag, _)| drag.horizontal == bar.horizontal),
            );
        }
        if log.is_none() || self.editor.rope.len_chars() == 0 {
            let message = if source == Source::Git {
                "No Git operations yet. Git activity is recorded even while Output is hidden."
            } else if log.is_none() {
                "No remote SSH session has been started in this window."
            } else {
                "No connection events yet."
            };
            frame.render_widget(
                Paragraph::new(message).style(style.fg(colors.ui.description_color(bg))),
                Rect::new(
                    self.text.x + self.text.width.min(2),
                    self.text.y,
                    self.text.width.saturating_sub(2),
                    self.text.height.min(1),
                ),
            );
        }
        if find_height > 0 {
            self.draw_find(frame, colors, Rect::new(area.x, area.y, area.width, 1));
        } else {
            self.find.hit = None;
        }
        if focused
            && !self.find.focused
            && !self.text.is_empty()
            && self.editor.rope.len_chars() > 0
        {
            let line = self.editor.cursor_line;
            let visual_row = if self.wrap {
                self.layout
                    .row_for_position(&self.editor, line, self.editor.cursor_col)
            } else {
                line
            };
            let cell = self
                .row_at(visual_row)
                .map_or(0, |(_, segment)| segment.cell);
            let x = self
                .column_x(line, self.editor.cursor_col)
                .saturating_sub(cell);
            if (self.offset()..self.offset() + self.text.height as usize).contains(&visual_row)
                && (self.horizontal..self.horizontal + self.text.width as usize).contains(&x)
            {
                frame.set_cursor_position((
                    self.text.x + (x - self.horizontal) as u16,
                    self.text.y + (visual_row - self.offset()) as u16,
                ));
            }
        }
    }
    fn draw_find(&mut self, frame: &mut Frame, colors: &crate::app_theme::ThemeColors, area: Rect) {
        let controls = area.width.min(20);
        let field = Rect::new(area.x, area.y, area.width - controls, 1);
        let input = crate::ui_widgets::InputField::new(&self.find.query)
            .theme_colors(colors)
            .placeholder_text("Find in output")
            .selection_range(
                self.find
                    .anchor
                    .map(|anchor| (anchor.min(self.find.cursor), anchor.max(self.find.cursor))),
            );
        let input = if self.find.focused {
            input.cursor_pos(self.find.cursor)
        } else {
            input.cursor_marker("")
        };
        let hit = input.render(frame, field);
        self.find.hit = Some(hit);
        self.find.case = Rect::new(field.right(), area.y, controls.min(3), 1);
        self.find.previous = Rect::new(
            self.find.case.right(),
            area.y,
            controls.saturating_sub(3).min(3),
            1,
        );
        self.find.next = Rect::new(
            self.find.previous.right(),
            area.y,
            controls.saturating_sub(6).min(3),
            1,
        );
        self.find.close = Rect::new(
            area.right().saturating_sub(3).max(self.find.next.right()),
            area.y,
            controls.saturating_sub(9).min(3),
            1,
        );
        for (rect, label) in [
            (self.find.case, "Aa "),
            (self.find.previous, " ↑ "),
            (self.find.next, " ↓ "),
            (self.find.close, " ✕ "),
        ] {
            frame.render_widget(
                Paragraph::new(label).style(colors.ui.toolbar_style(
                    colors.fg_dim,
                    colors.ui.bg_panel,
                    false,
                    rect == self.find.case && self.find.case_sensitive,
                    false,
                )),
                rect,
            );
        }
        let start = self.find.next.right();
        let count = if self.find.query.is_empty() {
            String::new()
        } else if !self.find.case_sensitive && self.find.pattern.is_none() {
            "Error".into()
        } else {
            self.find.matches.len().to_string()
        };
        frame.render_widget(
            Paragraph::new(count).style(Style::default().fg(colors.fg_dim).bg(colors.ui.bg_panel)),
            Rect::new(start, area.y, self.find.close.x.saturating_sub(start), 1),
        );
    }
    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focused = true;
        self.find.cursor = self.find.query.len();
        self.find.anchor = Some(0);
        self.find.update(&self.editor);
    }
    pub fn paste(&mut self, text: &str) {
        if self.find.open && self.find.focused {
            let find = &mut self.find;
            find.history.edit(
                TextInput::new(&mut find.query, &mut find.cursor, &mut find.anchor),
                KeyCode::Null,
                KeyModifiers::NONE,
                Some(text),
            );
            find.update(&self.editor);
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        if self.find.open && key.code == KeyCode::Esc {
            self.find.open = false;
            self.find.focused = false;
            self.find.drag_anchor = None;
            return;
        }
        if self.find.open && self.find.focused {
            match key.code {
                KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::ALT) => {
                    self.find.case_sensitive = !self.find.case_sensitive;
                    self.find.update(&self.editor);
                }
                KeyCode::Esc => {
                    self.find.open = false;
                    self.find.focused = false;
                    self.find.drag_anchor = None;
                }
                KeyCode::Enter => {
                    self.jump_find(key.modifiers.contains(KeyModifiers::SHIFT), false);
                }
                KeyCode::Tab => self.find.focused = false,
                _ => {
                    let find = &mut self.find;
                    let changed = find
                        .history
                        .edit(
                            TextInput::new(&mut find.query, &mut find.cursor, &mut find.anchor),
                            key.code,
                            key.modifiers,
                            None,
                        )
                        .changed;
                    if changed {
                        find.update(&self.editor);
                        self.jump_find(false, true);
                    }
                }
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let resume_autoscroll = !shift
            && match key.code {
                KeyCode::Down if ctrl => self.scrolls_past_end(1),
                KeyCode::Down if self.wrap => {
                    self.layout.row_for_position(&self.editor, self.editor.cursor_line, self.editor.cursor_col)
                        + 1 >= self.total_rows()
                }
                KeyCode::Down => self.editor.cursor_line + 1 >= self.editor.rope.len_lines(),
                KeyCode::PageDown => self.scrolls_past_end(self.text.height as usize),
                _ => false,
            };
        if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        ) {
            if shift {
                self.editor.start_selection();
            } else {
                self.editor.clear_selection();
            }
            self.view.autoscroll = false;
        }
        match key.code {
            KeyCode::Left if ctrl => self.editor.move_word_left(),
            KeyCode::Right if ctrl => self.editor.move_word_right(),
            KeyCode::Up if ctrl => self.scroll_by(-1),
            KeyCode::Down if ctrl => self.scroll_by(1),
            KeyCode::Up if self.wrap => self.move_vertical(-1),
            KeyCode::Down if self.wrap => self.move_vertical(1),
            KeyCode::Up => self.editor.move_up(),
            KeyCode::Down => self.editor.move_down(),
            KeyCode::Left => self.editor.move_left(),
            KeyCode::Right => self.editor.move_right(),
            KeyCode::Home if ctrl => self.editor.move_to_top(),
            KeyCode::End if ctrl => self.editor.move_to_bottom(),
            KeyCode::Home => self.editor.move_to_line_start(),
            KeyCode::End => self.editor.move_to_line_end(),
            KeyCode::PageUp if self.wrap => {
                self.scroll_by(-(self.text.height as isize));
                self.move_vertical(-(self.text.height as isize));
            }
            KeyCode::PageDown if self.wrap => {
                self.scroll_by(self.text.height as isize);
                self.move_vertical(self.text.height as isize);
            }
            KeyCode::PageUp => self.editor.page_up(),
            KeyCode::PageDown => self.editor.page_down(),
            KeyCode::Char('a' | 'A') if ctrl => {
                self.editor.select_all();
                self.view.autoscroll = false;
            }
            KeyCode::Esc => self.editor.clear_selection(),
            KeyCode::F(3) => {
                self.find.update(&self.editor);
                self.jump_find(shift, false);
            }
            _ => {}
        }
        if !(ctrl && matches!(key.code, KeyCode::Up | KeyCode::Down)) {
            self.reveal();
        }
        if resume_autoscroll {
            self.resume_autoscroll();
        }
    }
    fn column_x(&self, line: usize, column: usize) -> usize {
        let Some(line) = self.editor.rope.get_line(line) else {
            return 0;
        };
        let prefix = line.slice(..column.min(Editor::line_content_len(line)));
        if let Some(text) = prefix.as_str() {
            line_width(text)
        } else {
            line_width(&prefix.to_string())
        }
    }
    fn position(&self, mouse: MouseEvent) -> (usize, usize) {
        let row = mouse
            .row
            .saturating_sub(self.text.y)
            .min(self.text.height.saturating_sub(1)) as usize;
        let cell = mouse
            .column
            .saturating_sub(self.text.x)
            .min(self.text.width.saturating_sub(1)) as usize;
        self.position_on_row(self.offset() + row, self.horizontal + cell)
    }
    pub fn searching(&self) -> bool {
        self.find.open
    }
    pub fn search_focused(&self) -> bool {
        self.find.open && self.find.focused
    }
    pub fn selected_text(&self) -> Option<String> {
        if self.search_focused() {
            let anchor = self.find.anchor?;
            let (start, end) = (anchor.min(self.find.cursor), anchor.max(self.find.cursor));
            (start < end).then(|| self.find.query[start..end].to_string())
        } else {
            self.editor.selected_text()
        }
    }
    pub fn has_selection(&self) -> bool {
        if self.search_focused() {
            self.find
                .anchor
                .is_some_and(|anchor| anchor != self.find.cursor)
        } else {
            self.editor.has_selection()
        }
    }
    pub fn cancel_capture(&mut self) {
        self.selecting = false;
        self.bar_drag = None;
        self.find.drag_anchor = None;
    }
    pub fn blur_search(&mut self) {
        self.find.focused = false;
        self.find.drag_anchor = None;
    }
    pub fn mouse(&mut self, mouse: MouseEvent, speed: usize) -> bool {
        if self.area.is_empty() {
            return false;
        }
        let point = (mouse.column, mouse.row).into();
        if let Some(anchor) = self.find.drag_anchor {
            if matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            ) {
                if let Some(hit) = &self.find.hit {
                    self.find.cursor = hit.byte_at(mouse.column, mouse.row);
                    self.find.anchor = (anchor != self.find.cursor).then_some(anchor);
                }
                if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                    self.find.drag_anchor = None;
                }
                return true;
            }
        }
        if let Some((bar, grab)) = self.bar_drag {
            if matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            ) {
                let offset = bar.at(bar.coordinate(mouse), grab);
                if bar.horizontal {
                    self.horizontal = offset;
                    self.view.autoscroll = false;
                } else if mouse.row >= bar.area.bottom() {
                    self.resume_autoscroll();
                } else {
                    self.scroll_to(offset);
                    self.view.autoscroll = false;
                }
                if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                    self.bar_drag = None;
                }
                return true;
            }
        }
        if self.selecting
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            if mouse.row < self.text.y {
                self.scroll_by(-1);
            }
            if mouse.row >= self.text.bottom() {
                self.scroll_by(1);
            }
            let position = self.position(mouse);
            self.editor
                .set_cursor_without_scrolling(position.0, position.1);
            if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                self.selecting = false;
            }
            return true;
        }
        if !self.area.contains(point) {
            return false;
        }
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.selecting = false;
            self.bar_drag = None;
        }
        if self.find.open && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(hit) = self
                .find
                .hit
                .as_ref()
                .filter(|hit| hit.contains(mouse.column, mouse.row))
            {
                self.find.focused = true;
                let previous = self.find.cursor;
                self.find.cursor = hit.byte_at(mouse.column, mouse.row);
                self.find.anchor = if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    Some(self.find.anchor.unwrap_or(previous))
                } else {
                    None
                };
                self.find.drag_anchor = Some(self.find.anchor.unwrap_or(self.find.cursor));
                return true;
            }
            if self.find.close.contains(point) {
                self.find.open = false;
                self.find.focused = false;
                return true;
            }
            if self.find.case.contains(point) {
                self.find.case_sensitive = !self.find.case_sensitive;
                self.find.update(&self.editor);
                return true;
            }
            if self.find.previous.contains(point) || self.find.next.contains(point) {
                self.jump_find(self.find.previous.contains(point), false);
                return true;
            }
        }
        match mouse.kind {
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                if self.wrap {
                    return true;
                }
                self.horizontal = self.horizontal.saturating_add_signed(
                    if mouse.kind == MouseEventKind::ScrollLeft {
                        -6
                    } else {
                        6
                    },
                );
                self.view.autoscroll = false;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if mouse.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                if self.wrap {
                    return true;
                }
                self.horizontal = self.horizontal.saturating_add_signed(
                    if mouse.kind == MouseEventKind::ScrollUp {
                        -6
                    } else {
                        6
                    },
                );
                self.view.autoscroll = false;
            }
            MouseEventKind::ScrollUp => {
                self.scroll_by(-(speed.min(isize::MAX as usize) as isize));
                self.view.autoscroll = false;
            }
            MouseEventKind::ScrollDown => {
                if self.scrolls_past_end(speed) {
                    self.resume_autoscroll();
                } else {
                    self.scroll_by(speed.min(isize::MAX as usize) as isize);
                    self.view.autoscroll = false;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.view.autoscroll = false;
                self.find.focused = false;
                for bar in [self.vertical_bar, self.horizontal_bar]
                    .into_iter()
                    .flatten()
                {
                    if bar.area.contains(point) {
                        let relative = bar.coordinate(mouse) - bar.origin();
                        let grab = if (bar.top()..bar.top() + bar.thumb).contains(&relative) {
                            relative - bar.top()
                        } else {
                            bar.thumb / 2
                        };
                        self.bar_drag = Some((bar, grab));
                        let offset = bar.at(bar.coordinate(mouse), grab);
                        if bar.horizontal {
                            self.horizontal = offset;
                        } else {
                            self.scroll_to(offset);
                        }
                        return true;
                    }
                }
                if self.text.contains(point) {
                    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        self.editor.start_selection();
                    } else {
                        self.editor.clear_selection();
                    }
                    let position = self.position(mouse);
                    self.editor
                        .set_cursor_without_scrolling(position.0, position.1);
                    self.editor.start_selection();
                    let now = std::time::Instant::now();
                    let count = self
                        .last_click
                        .filter(|(when, at, _)| {
                            *at == (mouse.column, mouse.row)
                                && now.saturating_duration_since(*when).as_millis() < 500
                        })
                        .map_or(1, |(_, _, count)| count % 3 + 1);
                    self.last_click = Some((now, (mouse.column, mouse.row), count));
                    if count == 2 {
                        self.editor.select_word_at_cursor();
                    }
                    if count == 3 {
                        self.editor.select_line();
                    }
                    self.selecting = true;
                }
            }
            _ => {}
        }
        true
    }
    pub fn capturing(&self) -> bool {
        self.selecting || self.bar_drag.is_some() || self.find.drag_anchor.is_some()
    }
}

#[cfg(test)]
#[path = "ui_output_readability_tests.rs"]
mod readability_tests;
