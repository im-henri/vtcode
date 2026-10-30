//! Virtual rows for pending AI changes and inline blame details. These rows
//! never enter the rope; caret and mouse selection still address real text.
use crate::{
    app::{ActivePane, App, TabKind},
    editor::{Editor, WrappedRowInfo},
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Paragraph,
};
use ropey::Rope;
use std::{ops::Range, sync::Arc};
use vtcode::acp::rollback::{FileReview, InlineChunk};

const MAX_REMOVED_ROWS: usize = 200;

fn review_background(colors: &crate::app_theme::ThemeColors, accept: bool) -> Color {
    crate::app_theme::lerp_color(
        colors.bg_editor,
        if accept {
            colors.bg_diff_added_highlight
        } else {
            colors.bg_diff_removed_highlight
        },
        0.3,
    )
}

fn clear_review_row(f: &mut Frame, area: Rect, background: Color) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            f.buffer_mut()[(x, y)].reset();
            f.buffer_mut()[(x, y)].set_bg(background);
        }
    }
}

#[derive(Clone)]
enum ExtraKind {
    Actions(usize),
    Merge { line: usize, offset: u16, footer: bool },
    Removed(Arc<str>),
    More(usize),
    Blame(u16),
}
#[derive(Clone)]
struct Extra {
    base: usize,
    position: usize,
    kind: ExtraKind,
}

#[derive(Clone, Debug)]
pub(crate) struct ReviewHit {
    pub area: Rect,
    pub chunk: Option<(u64, usize)>,
    pub accept: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ReviewNavigationHit {
    pub area: Rect,
    pub chunk: (u64, usize),
}

struct ReviewAdvance {
    chunk: (u64, usize),
    next: Option<(u64, usize)>,
    cursor: (usize, usize),
    accepted_buffer: Option<Rope>,
}

#[derive(Default)]
pub(crate) struct ReviewView {
    pub active: bool,
    pub source_rows: Vec<Option<WrappedRowInfo>>,
    pub blame_rows: Vec<(u16, u16)>,
    pub hits: Vec<ReviewHit>,
    pub(crate) merge_hits: Vec<crate::merge_conflict_ui::Hit>,
    merge_index: Option<Arc<crate::merge_conflicts::Index>>,
    merge_editor: bool,
    pub navigation_hits: Vec<ReviewNavigationHit>,
    pub area: Rect,
    code: Rect,
    pub row: usize,
    total: usize,
    source_top: Option<usize>,
    cursor: Option<(usize, usize)>,
    snapshot: Option<Rope>,
    generation: u64,
    has_review: bool,
    inline_blame: Option<crate::editor_blame::InlineDetails>,
    width: usize,
    pub(crate) wrapped: bool,
    folds: std::collections::BTreeMap<usize, usize>,
    chunks: Vec<InlineChunk>,
    order: Vec<usize>,
    selected: Option<(u64, usize)>,
    advance_requested: Option<ReviewAdvance>,
    extras: Vec<Extra>,
    highlights: Vec<Range<usize>>,
    grab: usize,
}

fn chunk_line(editor: &Editor, chunk: &InlineChunk) -> usize {
    chunk
        .range
        .as_ref()
        .map_or(chunk.line.saturating_sub(1), |range| {
            editor
                .rope
                .byte_to_line(range.start.min(editor.rope.len_bytes()))
        })
        .min(editor.line_count().saturating_sub(1))
}

fn base_for_line(editor: &mut Editor, line: usize, wrapped: bool, width: usize) -> usize {
    if wrapped {
        return editor.wrapped_row_for_position(line, 0, width);
    }
    let mut target = line;
    for (&start, &end) in &editor.folded_ranges {
        if start < target && target < end {
            target = start;
            break;
        }
    }
    let hidden: usize = editor
        .folded_ranges
        .iter()
        .filter(|(start, _)| **start < target)
        .map(|(&start, &end)| end.min(target).saturating_sub(start + 1))
        .sum();
    target.saturating_sub(hidden)
}

fn line_for_base(editor: &Editor, row: usize) -> usize {
    let mut line = row;
    for (&start, &end) in &editor.folded_ranges {
        if line > start {
            line = line.saturating_add(end.saturating_sub(start + 1));
        }
    }
    line.min(editor.line_count().saturating_sub(1))
}

impl ReviewView {
    fn selected_index(&self) -> Option<usize> {
        self.order.iter().position(|&index| {
            self.selected == Some((self.chunks[index].edit, self.chunks[index].index))
        })
    }

    fn select_index(&mut self, index: usize) {
        self.selected = self.order.get(index).map(|&index| {
            let chunk = &self.chunks[index];
            (chunk.edit, chunk.index)
        });
    }

    fn select_cursor(&mut self, editor: &Editor) {
        let index = self
            .order
            .iter()
            .enumerate()
            .min_by_key(|(_, index)| {
                let chunk = &self.chunks[**index];
                let start = chunk_line(editor, chunk);
                let end = chunk.range.as_ref().map_or(start, |range| {
                    editor.rope.byte_to_line(
                        range
                            .end
                            .saturating_sub(1)
                            .max(range.start)
                            .min(editor.rope.len_bytes()),
                    )
                });
                start.saturating_sub(editor.cursor_line) + editor.cursor_line.saturating_sub(end)
            })
            .map(|(index, _)| index);
        if let Some(index) = index {
            self.select_index(index);
        }
    }

    fn selected_row(&self) -> Option<usize> {
        self.extras.iter().find_map(|extra| {
            if let ExtraKind::Actions(index) = extra.kind
                && self.selected == Some((self.chunks[index].edit, self.chunks[index].index))
            {
                Some(extra.position)
            } else {
                None
            }
        })
    }

    fn reveal_selected(&mut self, editor: &mut Editor) {
        if let Some(row) = self.selected_row() {
            self.row = row.saturating_sub(2).min(self.max_row());
            self.sync(editor);
        }
    }

    fn navigate(
        &mut self,
        editor: &mut Editor,
        file: &FileReview,
        generation: u64,
        identity: (u64, usize),
    ) {
        self.advance_requested = None;
        let code = self.code;
        self.prepare(editor, Some(file), generation, code, self.inline_blame);
        let Some(chunk) = self
            .chunks
            .iter()
            .find(|chunk| (chunk.edit, chunk.index) == identity)
        else {
            return;
        };
        let line = chunk_line(editor, chunk);
        editor.selection_anchor = None;
        editor.extra_cursors.clear();
        editor.extra_cursor_anchors.clear();
        editor.set_cursor(line, 0);
        // set_cursor unfolds the destination. Rebuild virtual row positions
        // before scrolling so folds and wrapped/removed lines cannot hide it.
        self.prepare(editor, Some(file), generation, code, self.inline_blame);
        self.selected = Some(identity);
        self.reveal_selected(editor);
    }

    fn select_viewport(&mut self) {
        let mut preceding = None;
        let mut next = None;
        for extra in &self.extras {
            if let ExtraKind::Actions(index) = extra.kind {
                let chunk = &self.chunks[index];
                let identity = (chunk.edit, chunk.index);
                if extra.position < self.row {
                    preceding = Some(identity);
                } else {
                    next = Some((extra.position, identity));
                    break;
                }
            }
        }
        self.selected = match next {
            Some((row, identity))
                if row < self.row + self.code.height as usize || preceding.is_none() =>
            {
                Some(identity)
            }
            _ => preceding,
        };
    }

    fn before(&self, base: usize) -> usize {
        base + self.extras.partition_point(|extra| extra.base < base)
    }
    fn source(&self, visual: usize) -> usize {
        let count = self
            .extras
            .partition_point(|extra| extra.position <= visual);
        if let Some(extra) = count.checked_sub(1).and_then(|i| self.extras.get(i))
            && extra.position == visual
        {
            return extra.base;
        }
        visual.saturating_sub(count)
    }
    fn max_row(&self) -> usize {
        self.total.saturating_sub(self.code.height.max(1) as usize)
    }

    fn prepare(
        &mut self,
        editor: &mut Editor,
        file: Option<&FileReview>,
        generation: u64,
        code: Rect,
        inline_blame: Option<crate::editor_blame::InlineDetails>,
    ) {
        let merge_index = editor.merge_conflicts.index.as_ref()
            .filter(|index| index.source.is_instance(&editor.rope) && !index.regions.is_empty()).cloned();
        let previous_index = self.selected_index().unwrap_or(0);
        let previous_selected = self.selected;
        let previous_inline = self.inline_blame;
        let closing_visible_blame = inline_blame.is_none()
            && previous_inline.is_some()
            && self.extras.iter().any(|extra| {
                matches!(extra.kind, ExtraKind::Blame(_)) && extra.position == self.row
            });
        let first = editor.scroll_offset.min(editor.line_count());
        let end = first
            .saturating_add(code.height as usize)
            .min(editor.line_count());
        let bounded = editor.large_file_mode
            || editor.current_line_len() > crate::highlight::MAX_SYNTAX_LINE_CHARS
            || (first..end)
                .any(|line| editor.line_len(line) > crate::highlight::MAX_SYNTAX_LINE_CHARS);
        let wrapped = editor.word_wrap && !bounded;
        let width = if wrapped && editor.word_wrap_column > 0 {
            editor.word_wrap_column.min(code.width.max(1) as usize)
        } else {
            code.width.max(1) as usize
        };
        let content_changed = self
            .snapshot
            .as_ref()
            .is_none_or(|rope| !rope.is_instance(&editor.rope));
        // A completed decision must not move the caret after the user resumes
        // typing or navigates elsewhere while its worker is still finishing.
        if self.advance_requested.as_ref().is_some_and(|request| {
            request.cursor != (editor.cursor_line, editor.cursor_col)
                || request
                    .accepted_buffer
                    .as_ref()
                    .map_or(editor.modified, |buffer| !buffer.is_instance(&editor.rope))
        }) {
            self.advance_requested = None;
        }
        let layout_changed = self.merge_editor != editor.merge_conflicts.review.is_some()
            || self.merge_index.as_ref().map(Arc::as_ptr) != merge_index.as_ref().map(Arc::as_ptr)
            || !self.active
            || self.generation != generation
            || self.has_review != file.is_some()
            || self.inline_blame != inline_blame
            || content_changed
            || self.width != width
            || self.wrapped != wrapped
            || self.folds != editor.folded_ranges;
        let source_scroll = if closing_visible_blame {
            base_for_line(editor, previous_inline.unwrap().line, wrapped, width)
        } else if wrapped {
            editor.wrap_scroll_offset
        } else {
            base_for_line(editor, editor.scroll_offset, false, width)
        };
        if layout_changed {
            if content_changed
                || self.generation != generation
                || !self.active
                || self.has_review != file.is_some()
            {
                self.chunks = if editor.rope.len_bytes() <= 8 * 1024 * 1024 {
                    file.map_or_else(Vec::new, |file| file.project(&editor.rope.to_string()))
                } else {
                    file.map_or_else(Vec::new, |file| file.chunks.clone())
                };
                self.snapshot = Some(editor.rope.clone());
                self.generation = generation;
                self.has_review = file.is_some();
            }
            self.extras.clear();
            self.highlights.clear();
            let mut events = Vec::new();
            for (index, chunk) in self.chunks.iter().enumerate() {
                let line = chunk_line(editor, chunk);
                if let Some(range) = &chunk.range
                    && !range.is_empty()
                {
                    let start = editor
                        .rope
                        .byte_to_line(range.start.min(editor.rope.len_bytes()));
                    let end = editor
                        .rope
                        .byte_to_line(range.end.saturating_sub(1).min(editor.rope.len_bytes()));
                    self.highlights.push(start..end + 1);
                }
                let base = base_for_line(editor, line, wrapped, width);
                events.push((base, line, index));
            }
            events.sort_unstable();
            self.order = events.iter().map(|&(_, _, index)| index).collect();
            if self.selected_index().is_none() {
                self.select_index(previous_index.checked_rem(self.order.len()).unwrap_or(0));
            }
            for (base, _, index) in events {
                self.extras.push(Extra {
                    base,
                    position: 0,
                    kind: ExtraKind::Actions(index),
                });
                let mut removed = 0;
                let mut hidden = 0;
                for line in self.chunks[index].original.split_inclusive('\n') {
                    let line = line.trim_end_matches(['\r', '\n']);
                    let ranges = if wrapped {
                        Editor::wrap_ranges_for_line_text_tabbed(line, width, editor.tab_size)
                    } else {
                        vec![(0, line.chars().count())]
                    };
                    for (start, end) in ranges {
                        if removed < MAX_REMOVED_ROWS {
                            let text: String = line
                                .chars()
                                .skip(start)
                                .take(end.saturating_sub(start))
                                .collect();
                            self.extras.push(Extra {
                                base,
                                position: 0,
                                kind: ExtraKind::Removed(text.into()),
                            });
                            removed += 1;
                        } else {
                            hidden += 1;
                        }
                    }
                }
                if hidden > 0 {
                    self.extras.push(Extra {
                        base,
                        position: 0,
                        kind: ExtraKind::More(hidden),
                    });
                }
            }
            let base_total = if wrapped {
                editor.wrapped_total_rows(width)
            } else {
                editor.visible_line_count()
            };
            if let Some(inline) = inline_blame {
                let base = if wrapped {
                    editor.wrapped_row_for_position(
                        inline.line,
                        editor.line_len(inline.line),
                        width,
                    ) + 1
                } else {
                    base_for_line(editor, inline.line, false, width) + 1
                }
                .min(base_total);
                for row in 0..inline.height {
                    self.extras.push(Extra {
                        base,
                        position: 0,
                        kind: ExtraKind::Blame(row),
                    });
                }
                // Blame follows its source line and precedes AI actions for the
                // next one; stable ordering retains the AI rows' existing order.
                self.extras
                    .sort_by_key(|extra| (extra.base, !matches!(extra.kind, ExtraKind::Blame(_))));
            }
            if let Some(index) = &merge_index {
                for &region in &index.regions {
                    // A folded conflict has no visible header/actions until unfolded.
                    if editor.folded_ranges.iter().any(|(&start, &end)| start < region.start && region.start < end) { continue; }
                    let base = base_for_line(editor, region.start, wrapped, width);
                    for offset in 0..crate::merge_conflict_ui::action_rows(code.width) {
                        self.extras.push(Extra { base, position: 0, kind: ExtraKind::Merge { line: region.start, offset, footer: false } });
                    }
                    if editor.merge_conflicts.review.is_none() {
                        let base = if wrapped {
                            editor.wrapped_row_for_position(region.end, editor.line_len(region.end), width) + 1
                        } else { base_for_line(editor, region.end, false, width) + 1 }.min(base_total);
                        self.extras.push(Extra { base, position: 0, kind: ExtraKind::Merge { line: region.start, offset: 0, footer: true } });
                    }
                }
            }
            self.merge_index = merge_index;
            self.merge_editor = editor.merge_conflicts.review.is_some();
            self.extras.sort_by_key(|extra| (extra.base, !matches!(extra.kind, ExtraKind::Blame(_))));
            for (index, extra) in self.extras.iter_mut().enumerate() {
                extra.position = extra.base + index;
            }
            self.total = base_total.saturating_add(self.extras.len());
            self.row = self.before(source_scroll);
            self.inline_blame = inline_blame;
            self.width = width;
            self.wrapped = wrapped;
            self.folds = editor.folded_ranges.clone();
        } else if self.source_top != Some(source_scroll)
            && self.cursor == Some((editor.cursor_line, editor.cursor_col))
        {
            self.row = self.before(source_scroll);
        }
        self.code = code;
        if inline_blame.is_some()
            && previous_inline != inline_blame
            && let Some(start) = self.extras.iter().find_map(|extra| {
                matches!(extra.kind, ExtraKind::Blame(0)).then_some(extra.position)
            })
        {
            let end = start + inline_blame.unwrap().height as usize;
            self.row = self
                .row
                .min(start.saturating_sub(1))
                .max(end.saturating_sub(code.height as usize));
        }
        let cursor = (editor.cursor_line, editor.cursor_col);
        let selection_resolved = previous_selected.is_some() && previous_selected != self.selected;
        let advance = selection_resolved
            && self
                .advance_requested
                .as_ref()
                .is_some_and(|request| Some(request.chunk) == previous_selected);
        if advance
            && let Some(next) = self
                .advance_requested
                .as_ref()
                .and_then(|request| request.next)
            && self
                .chunks
                .iter()
                .any(|chunk| (chunk.edit, chunk.index) == next)
        {
            self.selected = Some(next);
        }
        if !advance
            && (selection_resolved
                || self.cursor.is_none()
                || self.cursor.is_some_and(|previous| previous != cursor))
        {
            self.select_cursor(editor);
        }
        if self.cursor.is_some_and(|previous| previous != cursor) {
            let base = if wrapped {
                editor.wrapped_row_for_position(cursor.0, cursor.1, width)
            } else {
                base_for_line(editor, cursor.0, false, width)
            };
            let row = base + self.extras.partition_point(|extra| extra.base <= base);
            if row < self.row {
                self.row = self.before(base);
            } else if row >= self.row + code.height as usize {
                self.row = row
                    .saturating_add(1)
                    .saturating_sub(code.height.max(1) as usize);
            }
        }
        self.cursor = Some(cursor);
        self.row = self.row.min(self.max_row());
        self.active = true;
        self.sync(editor);
        if advance && let Some(file) = file {
            if let Some(identity) = self.selected {
                self.navigate(editor, file, generation, identity);
            }
        }
    }

    fn sync(&mut self, editor: &mut Editor) {
        let base = self.source(self.row);
        if self.wrapped {
            editor.wrap_scroll_offset = base;
            editor.sync_line_scroll_from_wrap(self.width);
        } else {
            editor.scroll_offset = line_for_base(editor, base);
        }
        self.source_top = Some(base);
    }

    fn scroll(&mut self, editor: &mut Editor, delta: isize) {
        self.advance_requested = None;
        self.row = self.row.saturating_add_signed(delta).min(self.max_row());
        self.select_viewport();
        self.sync(editor);
    }

    fn scrollbar(&mut self, editor: &mut Editor, area: Rect, position: u16, begin: bool) {
        self.advance_requested = None;
        let (maximum, thumb) = crate::inline_diff::scrollbar_thumb(
            self.total,
            self.code.height as usize,
            area.height as usize,
            1,
        );
        let travel = (area.height as usize).saturating_sub(thumb);
        let top = self
            .row
            .saturating_mul(travel)
            .checked_div(maximum)
            .unwrap_or(0);
        let relative = position.saturating_sub(area.y) as usize;
        if begin {
            if (top..top + thumb).contains(&relative) {
                self.grab = relative - top;
                return;
            }
            self.grab = thumb / 2;
        }
        self.row = relative
            .saturating_sub(self.grab)
            .min(travel)
            .saturating_mul(maximum)
            .checked_div(travel)
            .unwrap_or(0);
        self.select_viewport();
        self.sync(editor);
    }
}

fn draw_button(
    f: &mut Frame,
    app: &App,
    view: &mut ReviewView,
    hit: ReviewHit,
    label: &str,
    enabled: bool,
) {
    let area = hit.area;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let color = if hit.accept {
        app.colors.fg_git_added
    } else {
        app.colors.fg_git_deleted
    };
    let surface = f
        .buffer_mut()
        .cell((area.x, area.y))
        .map_or(app.colors.bg_editor, |cell| cell.bg);
    let mut style =
        crate::ui_widgets::action_button_style(&app.colors, surface, false, hovered, enabled);
    if enabled {
        style = style
            .fg(app.colors.ui.readable_foreground(
                color,
                style.bg.unwrap_or(app.colors.bg_sidebar),
                4.5,
            ))
            .add_modifier(Modifier::BOLD);
    }
    f.render_widget(Paragraph::new(label).style(style), area);
    if enabled {
        view.hits.push(hit);
    }
}

struct Toolbar {
    height: u16,
    title: Rect,
    counter: Rect,
    previous: Rect,
    next: Rect,
    accept: Rect,
    reject: Rect,
    accept_all: Rect,
    reject_all: Rect,
    compact_chunk: bool,
    compact_all: bool,
}

fn take_right(area: &mut Rect, width: u16) -> Rect {
    let width = width.min(area.width);
    area.width -= width;
    Rect::new(area.right(), area.y, width, area.height)
}

impl Toolbar {
    fn new(area: Rect, total: usize) -> Self {
        let counter_width = format!(" {total}/{total} changes ")
            .len()
            .min(u16::MAX as usize) as u16;
        let height = if area.width >= 11 + counter_width + 48 {
            1
        } else {
            2
        }
        .min(area.height.saturating_sub(1));
        let mut buttons = Rect::new(
            area.x,
            area.y + height.saturating_sub(1),
            area.width,
            u16::from(height > 0),
        );
        let compact_all = area.width < 40;
        let compact_chunk = area.width < 30;
        let all_width = if compact_all { 7 } else { 12 };
        let chunk_width = if compact_chunk { 3 } else { 8 };
        let gap = u16::from(area.width > 2 * (all_width + chunk_width));
        let reject_all = take_right(&mut buttons, all_width);
        let accept_all = take_right(&mut buttons, all_width);
        take_right(&mut buttons, gap);
        let reject = take_right(&mut buttons, chunk_width);
        let accept = take_right(&mut buttons, chunk_width);
        take_right(&mut buttons, gap);
        let mut navigation = if height > 1 {
            Rect::new(area.x, area.y, area.width, 1)
        } else {
            buttons
        };
        let next = take_right(&mut navigation, 3);
        let previous = take_right(&mut navigation, 3);
        let counter = take_right(&mut navigation, counter_width);
        Self {
            height,
            title: navigation,
            counter,
            previous,
            next,
            accept,
            reject,
            accept_all,
            reject_all,
            compact_chunk,
            compact_all,
        }
    }
}

fn draw_navigation_button(
    f: &mut Frame,
    app: &App,
    view: &mut ReviewView,
    area: Rect,
    label: &str,
    index: Option<usize>,
) {
    let target = index.and_then(|index| view.order.get(index)).map(|&index| {
        let chunk = &view.chunks[index];
        (chunk.edit, chunk.index)
    });
    let enabled = target.is_some() && area.width == 3 && area.height > 0;
    let hovered = enabled && area.contains((app.mouse_col, app.mouse_row).into());
    f.render_widget(
        Paragraph::new(label).style({
            let style = app.colors.ui.toolbar_style(
                app.colors.ui.fg_sidebar,
                app.colors.bg_sidebar,
                hovered,
                false,
                false,
            );
            if enabled {
                style
            } else {
                style.fg(crate::ui_colors::disabled_text_color(
                    app.colors.fg_dim,
                    style.fg.unwrap_or(app.colors.ui.fg_sidebar),
                    app.colors.bg_sidebar,
                    app.colors.ui.contrast_colors(),
                ))
            }
        }),
        area,
    );
    if enabled {
        view.navigation_hits.push(ReviewNavigationHit {
            area,
            chunk: target.unwrap(),
        });
    }
}

fn draw_toolbar(
    f: &mut Frame,
    app: &App,
    view: &mut ReviewView,
    toolbar: &Toolbar,
    accept_enabled: bool,
    reject_enabled: bool,
) {
    let style = Style::default()
        .fg(app.colors.fg_accent)
        .bg(app.colors.bg_sidebar)
        .add_modifier(Modifier::BOLD);
    f.render_widget(Paragraph::new(" ◇ AI edits").style(style), toolbar.title);
    let current = view.selected_index();
    let total = view.order.len();
    let label = format!(" {}/{total} changes ", current.map_or(0, |index| index + 1));
    f.render_widget(Paragraph::new(label).style(style), toolbar.counter);
    draw_navigation_button(
        f,
        app,
        view,
        toolbar.previous,
        " ▲ ",
        current.map(|index| if index == 0 { total - 1 } else { index - 1 }),
    );
    draw_navigation_button(
        f,
        app,
        view,
        toolbar.next,
        " ▼ ",
        current.map(|index| (index + 1) % total),
    );
    let chunk = view.selected;
    for (area, chunk, accept, label, enabled) in [
        (
            toolbar.accept,
            chunk,
            true,
            if toolbar.compact_chunk {
                " ✓ "
            } else {
                " Accept "
            },
            accept_enabled && chunk.is_some(),
        ),
        (
            toolbar.reject,
            chunk,
            false,
            if toolbar.compact_chunk {
                " × "
            } else {
                " Reject "
            },
            reject_enabled && chunk.is_some(),
        ),
        (
            toolbar.accept_all,
            None,
            true,
            if toolbar.compact_all {
                " ✓ all "
            } else {
                " Accept all "
            },
            accept_enabled,
        ),
        (
            toolbar.reject_all,
            None,
            false,
            if toolbar.compact_all {
                " × all "
            } else {
                " Reject all "
            },
            reject_enabled,
        ),
    ] {
        draw_button(
            f,
            app,
            view,
            ReviewHit {
                area,
                chunk,
                accept,
            },
            label,
            enabled,
        );
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let area = crate::merge_conflict_ui::draw_merge_editor(f, app, area);
    let has_merge = app.editor().is_some_and(|editor| editor.merge_conflicts.current(&editor.rope).is_some_and(|index| !index.regions.is_empty()));
    let file = app
        .tabs
        .get(app.active_tab)
        .filter(|t| t.kind == TabKind::File)
        .and_then(|tab| tab.editor.file_path.as_ref())
        .and_then(|path| app.editor_file_review(path));
    if file.is_none() && !has_merge && !crate::editor_blame::has_details(app) {
        if let Some(editor) = app.editor_mut() {
            editor.ai_review = Default::default();
        }
        crate::ui_editor::draw_editor_content(f, app, area);
        return;
    }
    let toolbar = file
        .as_ref()
        .map(|file| Toolbar::new(area, file.chunks.len()));
    let header_height = toolbar.as_ref().map_or(0, |toolbar| toolbar.height);
    let body = Rect::new(
        area.x,
        area.y + header_height,
        area.width,
        area.height.saturating_sub(header_height),
    );
    let accept_enabled = app.chat.rollback.enabled;
    let reject_enabled =
        !app.chat.rollback.busy() && !app.chat.rollback.active && !app.chat.any_agent_running();
    let saved_sticky = std::mem::replace(&mut app.sticky_scroll, false);
    crate::ui_editor::draw_editor_content(f, app, body);
    let Some(mut code) = app.code_area else {
        app.sticky_scroll = saved_sticky;
        return;
    };
    let Some(mut gutter) = app.gutter_area else {
        app.sticky_scroll = saved_sticky;
        return;
    };
    let mut paint = Rect::new(
        gutter.x,
        code.y,
        code.right().saturating_sub(gutter.x),
        code.height,
    );
    let inline_blame = crate::editor_blame::inline_layout(app, paint);
    if file.is_none() && !has_merge && inline_blame.is_none() {
        app.editor_mut().unwrap().ai_review = Default::default();
        app.sticky_scroll = saved_sticky;
        return;
    }
    let generation = if file.is_some() {
        app.chat.rollback.inline_revision
    } else {
        0
    };
    let mut view = std::mem::take(&mut app.tabs[app.active_tab].editor.ai_review);
    // Revealing inserted rows can bring a long line into the source viewport,
    // which changes its horizontal scrollbar or wrapping. Settle the actual
    // code bounds before copying cells so no card covers an editor scrollbar.
    for _ in 0..4 {
        let editor = &app.tabs[app.active_tab].editor;
        let previous_view = (
            editor.scroll_offset,
            editor.wrap_scroll_offset,
            editor.cursor_line,
            editor.cursor_col,
        );
        let inline_blame = crate::editor_blame::inline_layout(app, paint);
        view.prepare(
            &mut app.tabs[app.active_tab].editor,
            file.as_ref(),
            generation,
            code,
            inline_blame,
        );
        let editor = &app.tabs[app.active_tab].editor;
        if previous_view
            == (
                editor.scroll_offset,
                editor.wrap_scroll_offset,
                editor.cursor_line,
                editor.cursor_col,
            )
        {
            break;
        }
        crate::ui_editor::draw_editor_content(f, app, body);
        let (Some(actual_code), Some(actual_gutter)) = (app.code_area, app.gutter_area) else {
            break;
        };
        if actual_code == code && actual_gutter == gutter {
            break;
        }
        code = actual_code;
        gutter = actual_gutter;
        paint = Rect::new(
            gutter.x,
            code.y,
            code.right().saturating_sub(gutter.x),
            code.height,
        );
    }
    view.area = area;
    view.hits.clear();
    view.merge_hits.clear();
    view.navigation_hits.clear();
    // The ordinary renderer supplies syntax, diagnostics, selection, and cursor
    // cells. Insert virtual rows into that bounded viewport afterwards.
    app.sticky_scroll = saved_sticky;
    let cells: Vec<_> = (paint.y..paint.bottom())
        .map(|y| {
            (paint.x..paint.right())
                .map(|x| f.buffer_mut()[(x, y)].clone())
                .collect::<Vec<_>>()
        })
        .collect();
    let editor = &mut app.tabs[app.active_tab].editor;
    let base_start = view.source(view.row);
    let source_rows = if base_start >= view.total.saturating_sub(view.extras.len()) {
        Vec::new()
    } else if view.wrapped {
        editor.wrapped_rows(base_start, code.height as usize, view.width)
    } else {
        editor
            .visible_lines(editor.scroll_offset, code.height as usize)
            .into_iter()
            .map(|line_idx| WrappedRowInfo {
                line_idx,
                start_col: 0,
                end_col: editor.line_len(line_idx),
                line_len: editor.line_len(line_idx),
                is_first: true,
            })
            .collect()
    };
    let horizontal = editor.horizontal_scroll;
    view.source_rows.clear();
    view.blame_rows.clear();
    let mut contrast = crate::ui_colors::TextContrastCache::new(&app.colors.ui);
    for row in 0..code.height as usize {
        let visual = view.row + row;
        let y = code.y + row as u16;
        let extra_index = view.extras.partition_point(|extra| extra.position < visual);
        let extra = view
            .extras
            .get(extra_index)
            .filter(|extra| extra.position == visual)
            .cloned();
        if let Some(extra) = extra {
            view.source_rows.push(None);
            clear_review_row(
                f,
                Rect::new(paint.x, y, paint.width, 1),
                app.colors.bg_sidebar,
            );
            match extra.kind {
                ExtraKind::Merge { line, offset, footer } => {
                    clear_review_row(f, Rect::new(paint.x, y, paint.width, 1), app.colors.bg_editor);
                    crate::merge_conflict_ui::draw_actions(f, app, Rect::new(code.x, y, code.width, 1),
                        line, offset, footer, &mut view.merge_hits);
                }
                ExtraKind::Blame(offset) => {
                    view.blame_rows.push((row as u16, offset));
                    clear_review_row(
                        f,
                        Rect::new(paint.x, y, paint.width, 1),
                        app.colors.bg_editor,
                    );
                }
                ExtraKind::Actions(index) => {
                    let chunk = &view.chunks[index];
                    let identity = Some((chunk.edit, chunk.index));
                    let marker = if view.selected == identity {
                        " ▸"
                    } else {
                        ""
                    };
                    let label = if chunk.issue.is_some() {
                        format!("{marker} AI edit · changed")
                    } else {
                        let position = view.order.iter().position(|&i| i == index).unwrap_or(0);
                        format!(
                            "{marker} AI edit {}/{}",
                            position + 1,
                            file.as_ref().unwrap().chunks.len()
                        )
                    };
                    let button_width = 8.min(code.width / 2);
                    let reject = Rect::new(
                        code.right().saturating_sub(button_width),
                        y,
                        button_width,
                        1,
                    );
                    let accept =
                        Rect::new(reject.x.saturating_sub(button_width), y, button_width, 1);
                    f.render_widget(
                        Paragraph::new(label).style(
                            Style::default()
                                .fg(app.colors.fg_accent)
                                .bg(app.colors.bg_sidebar),
                        ),
                        Rect::new(paint.x, y, accept.x.saturating_sub(paint.x), 1),
                    );
                    draw_button(
                        f,
                        app,
                        &mut view,
                        ReviewHit {
                            area: accept,
                            chunk: identity,
                            accept: true,
                        },
                        " Accept ",
                        accept_enabled,
                    );
                    draw_button(
                        f,
                        app,
                        &mut view,
                        ReviewHit {
                            area: reject,
                            chunk: identity,
                            accept: false,
                        },
                        " Reject ",
                        reject_enabled,
                    );
                }
                ExtraKind::Removed(text) => {
                    let text: String = text
                        .chars()
                        .skip(if view.wrapped { 0 } else { horizontal })
                        .take(code.width as usize)
                        .collect();
                    let background = review_background(&app.colors, false);
                    let mut style = Style::default()
                        .fg(contrast.foreground(app.colors.fg_text, background))
                        .bg(background);
                    if let Some(border) = app.colors.ui.border_diff_removed {
                        style = style
                            .add_modifier(Modifier::UNDERLINED)
                            .underline_color(border);
                    }
                    f.render_widget(
                        Paragraph::new("").style(style),
                        Rect::new(paint.x, y, paint.width, 1),
                    );
                    f.render_widget(
                        Paragraph::new("−").style(
                            Style::default()
                                .fg(app.colors.fg_git_deleted)
                                .bg(review_background(&app.colors, false)),
                        ),
                        Rect::new(gutter.x, y, gutter.width, 1),
                    );
                    f.render_widget(
                        Paragraph::new(text).style(style),
                        Rect::new(code.x, y, code.width, 1),
                    );
                }
                ExtraKind::More(hidden) => {
                    f.render_widget(
                        Paragraph::new(format!(" … {hidden} more removed lines")).style(
                            Style::default()
                                .fg(app.colors.fg_dim)
                                .bg(review_background(&app.colors, false)),
                        ),
                        Rect::new(paint.x, y, paint.width, 1),
                    );
                }
            }
        } else {
            let source = view.source(visual).saturating_sub(base_start);
            let mapped = (visual < view.total)
                .then(|| source_rows.get(source).copied())
                .flatten();
            view.source_rows.push(mapped);
            for (column, x) in (paint.x..paint.right()).enumerate() {
                if let Some(cell) =
                    mapped.and_then(|_| cells.get(source).and_then(|row| row.get(column)))
                {
                    f.buffer_mut()[(x, y)] = cell.clone();
                } else {
                    f.buffer_mut()[(x, y)].reset();
                    f.buffer_mut()[(x, y)].set_bg(app.colors.bg_editor);
                }
            }
            if let Some(source) = mapped {
                crate::merge_conflict_ui::draw_marker_label(f, app, code, row as u16, source);
            }
            if let Some(mapped) = mapped
                && view
                    .highlights
                    .iter()
                    .any(|range| range.contains(&mapped.line_idx))
            {
                for x in paint.x..paint.right() {
                    let cell = &mut f.buffer_mut()[(x, y)];
                    if ![
                        app.colors.bg_cursor,
                        app.colors.bg_selection,
                        app.colors.bg_search_match,
                    ]
                    .contains(&cell.bg)
                    {
                        let background = review_background(&app.colors, true);
                        cell.set_fg(contrast.foreground(cell.fg, background));
                        cell.set_bg(background);
                        if let Some(border) = app.colors.ui.border_diff_added {
                            cell.set_style(
                                Style::default()
                                    .add_modifier(Modifier::UNDERLINED)
                                    .underline_color(border),
                            );
                        }
                    }
                }
                f.buffer_mut()[(gutter.x, y)]
                    .set_symbol("▌")
                    .set_fg(app.colors.fg_accent)
                    .set_bg(review_background(&app.colors, true));
            }
        }
    }
    // Inline blame moves with the same source cells, never with a virtual row.
    let editor = &mut app.tabs[app.active_tab].editor;
    for annotation in [&mut editor.blame.inline_annotation, &mut editor.blame.inline_candidate] {
        *annotation = annotation.and_then(|(mut area, line)| {
            let source = source_rows.get(area.y.checked_sub(code.y)? as usize)?;
            let row = view.source_rows.iter().position(|mapped| mapped.is_some_and(|mapped| {
                mapped.line_idx == source.line_idx && mapped.start_col == source.start_col
            }))?;
            area.y = code.y + row as u16;
            Some((area, line))
        });
    }
    if let Some(scrollbar) = app.scrollbar_area {
        let (maximum, thumb) = crate::inline_diff::scrollbar_thumb(
            view.total,
            code.height as usize,
            scrollbar.height as usize,
            1,
        );
        let top = view
            .row
            .saturating_mul((scrollbar.height as usize).saturating_sub(thumb))
            .checked_div(maximum)
            .unwrap_or(0);
        for row in 0..scrollbar.height as usize {
            // The source renderer already painted Git/unsaved change markers.
            // Keep them when adapting the thumb to virtual conflict/review rows.
            if app.cached_overview_change_rows.row(row).status().is_some() {
                continue;
            }
            f.render_widget(
                Paragraph::new(if (top..top + thumb).contains(&row) {
                    "┃"
                } else {
                    "│"
                })
                .style(
                    Style::default()
                        .fg(app.colors.fg_dim)
                        .bg(app.colors.bg_editor),
                ),
                Rect::new(
                    scrollbar.right().saturating_sub(1),
                    scrollbar.y + row as u16,
                    1,
                    1,
                ),
            );
        }
    }
    if let Some(toolbar) = toolbar {
        f.render_widget(
            Paragraph::new("").style(Style::default().bg(app.colors.bg_sidebar)),
            Rect::new(area.x, area.y, area.width, header_height),
        );
        draw_toolbar(f, app, &mut view, &toolbar, accept_enabled, reject_enabled);
    }
    app.tabs[app.active_tab].editor.ai_review = view;
}

impl App {
    pub(crate) fn editor_review_navigation(&mut self, key: KeyCode) -> bool {
        let Some(editor) = self.editor_mut() else {
            return false;
        };
        if !editor.ai_review.active {
            return false;
        }
        let height = editor.ai_review.code.height.max(1) as isize;
        let delta = match key {
            KeyCode::PageUp => -height,
            KeyCode::PageDown => height,
            _ => return false,
        };
        let mut view = std::mem::take(&mut editor.ai_review);
        view.scroll(editor, delta);
        editor.ai_review = view;
        true
    }

    pub(crate) fn handle_editor_review_mouse(&mut self, mouse: MouseEvent) -> bool {
        if crate::merge_conflict_ui::handle_mouse(self, mouse) { return true; }
        let position = (mouse.column, mouse.row).into();
        let target = self
            .tabs
            .get(self.active_tab)
            .filter(|t| {
                matches!(t.kind, TabKind::File | TabKind::GitRevision)
                    && t.editor.ai_review.active
                    && t.editor.ai_review.area.contains(position)
            })
            .map(|_| (self.active_group, self.active_tab))
            .or_else(|| {
                self.groups
                    .iter()
                    .enumerate()
                    .filter(|(id, _)| *id != self.active_group)
                    .find_map(|(id, group)| {
                        group
                            .tabs
                            .get(group.active_tab)
                            .filter(|t| {
                                matches!(t.kind, TabKind::File | TabKind::GitRevision)
                                    && t.editor.ai_review.active
                                    && t.editor.ai_review.area.contains(position)
                            })
                            .map(|_| (id, group.active_tab))
                    })
            });
        let Some((group, tab)) = target else {
            return false;
        };
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) && !mouse.modifiers.contains(KeyModifiers::SHIFT)
        {
            let delta = self.scroll_speed as isize
                * if mouse.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                };
            let editor = if group == self.active_group {
                &mut self.tabs[tab].editor
            } else {
                &mut self.groups[group].tabs[tab].editor
            };
            let mut view = std::mem::take(&mut editor.ai_review);
            view.scroll(editor, delta);
            editor.ai_review = view;
            return true;
        }
        let scrollbar = if group == self.active_group {
            self.scrollbar_area
        } else {
            self.groups[group].scrollbar_area
        };
        if let Some(scrollbar) = scrollbar
            && ((mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && scrollbar.contains(position))
                || (mouse.kind == MouseEventKind::Drag(MouseButton::Left)
                    && self.dragging_v_scrollbar))
        {
            if group != self.active_group {
                self.switch_group(group);
            }
            self.dragging_v_scrollbar = true;
            self.active_pane = ActivePane::Editor;
            let editor = &mut self.tabs[tab].editor;
            let mut view = std::mem::take(&mut editor.ai_review);
            view.scrollbar(
                editor,
                scrollbar,
                mouse.row,
                mouse.kind == MouseEventKind::Down(MouseButton::Left),
            );
            editor.ai_review = view;
            return true;
        }
        let editor = if group == self.active_group {
            &mut self.tabs[tab].editor
        } else {
            &mut self.groups[group].tabs[tab].editor
        };
        let navigation = editor
            .ai_review
            .navigation_hits
            .iter()
            .find(|hit| hit.area.contains(position))
            .cloned();
        if let Some(hit) = navigation
            && !matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let Some(path) = editor.file_path.clone() else {
                    return true;
                };
                if group != self.active_group {
                    self.switch_group(group);
                }
                self.active_pane = ActivePane::Editor;
                self.terminal_panel_focused = false;
                if let Some(file) = self.editor_file_review(&path)
                    && file
                        .chunks
                        .iter()
                        .any(|chunk| (chunk.edit, chunk.index) == hit.chunk)
                {
                    self.push_nav_history();
                    let generation = self.chat.rollback.inline_revision;
                    let editor = &mut self.tabs[tab].editor;
                    let mut view = std::mem::take(&mut editor.ai_review);
                    view.navigate(editor, &file, generation, hit.chunk);
                    editor.ai_review = view;
                    self.finish_navigation();
                }
            }
            self.hovered_editor_pos = None;
            self.hovered_gutter_line = None;
            self.mouse_hover_changed = true;
            return true;
        }
        let merge_hit = editor.ai_review.merge_hits.iter().find(|hit| hit.area.contains(position)).cloned();
        if let Some(hit) = merge_hit
            && !matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
            let current = editor.content_version == hit.version;
            if let MouseEventKind::Down(button) = mouse.kind {
                crate::ui_panels::capture_pointer(self, button);
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if !current { return true; }
                if group != self.active_group { self.switch_group(group); }
                self.active_pane = ActivePane::Editor;
                self.merge_action(hit.action, Some(hit.line));
            }
            self.hovered_editor_pos = None;
            self.hovered_gutter_line = None;
            self.mouse_hover_changed = true;
            return true;
        }
        let hit = editor
            .ai_review
            .hits
            .iter()
            .find(|hit| hit.area.contains(position))
            .cloned();
        if let Some(hit) = hit
            && !matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let Some(path) = editor.file_path.clone() else {
                    return true;
                };
                if group != self.active_group {
                    self.switch_group(group);
                }
                self.active_pane = ActivePane::Editor;
                self.terminal_panel_focused = false;
                if let Some(identity) = hit.chunk
                    && self.editor_file_review(&path).is_some_and(|file| {
                        file.chunks.iter().any(|c| (c.edit, c.index) == identity)
                    })
                {
                    let editor = &mut self.tabs[tab].editor;
                    editor.ai_review.selected = Some(identity);
                    let next = editor.ai_review.selected_index().map(|index| {
                        let next =
                            editor.ai_review.order[(index + 1) % editor.ai_review.order.len()];
                        let chunk = &editor.ai_review.chunks[next];
                        (chunk.edit, chunk.index)
                    });
                    // Acceptance keeps the buffer intact, so existing unsaved
                    // edits do not prevent advancing. Capture the click state
                    // to cancel only if the user edits or moves afterwards.
                    editor.ai_review.advance_requested =
                        (hit.accept || !editor.modified).then(|| ReviewAdvance {
                            chunk: identity,
                            next,
                            cursor: (editor.cursor_line, editor.cursor_col),
                            accepted_buffer: hit.accept.then(|| editor.rope.clone()),
                        });
                } else if hit.chunk.is_none() {
                    self.tabs[tab].editor.ai_review.advance_requested = None;
                }
                self.resolve_editor_changes(&path, hit.chunk, hit.accept);
            }
            self.hovered_editor_pos = None;
            self.hovered_gutter_line = None;
            self.mouse_hover_changed = true;
            return true;
        }
        let code = editor.ai_review.code;
        let source = mouse
            .row
            .checked_sub(code.y)
            .and_then(|row| editor.ai_review.source_rows.get(row as usize))
            .copied()
            .flatten();
        if (mouse.row < code.y || source.is_none() && mouse.row < code.bottom())
            && !matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
            self.mouse_hover_changed = true;
            self.hovered_editor_pos = None;
            self.hovered_gutter_line = None;
            return true;
        }
        if mouse.kind == MouseEventKind::Moved
            && let Some(source) = source
        {
            if mouse.column < code.x {
                self.hovered_gutter_line = Some(source.line_idx);
                self.hovered_editor_pos = None;
            } else if mouse.column < code.right() {
                let offset = if editor.ai_review.wrapped {
                    source.start_col
                } else {
                    editor.horizontal_scroll
                };
                self.hovered_editor_pos = Some((
                    source.line_idx,
                    (offset + mouse.column.saturating_sub(code.x) as usize).min(source.end_col),
                ));
                self.hovered_gutter_line = None;
            } else {
                return false;
            }
            self.mouse_hover_changed = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
#[path = "editor_ai_review_navigation_tests.rs"]
mod navigation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Tab;
    use ratatui::{Terminal, backend::TestBackend};

    pub(super) fn pending(before: &str, after: &str) -> App {
        let mut app = App::test_new();
        app.show_minimap = false;
        app.chat.rollback.enabled = true;
        let mut editor = Editor::test_with_text(after);
        editor.file_path = Some(std::path::absolute("inline-review.txt").unwrap());
        let path = editor.file_path.clone().unwrap();
        app.tabs.push(Tab::new_file(editor));
        app.chat.rollback.inline_reviews = std::sync::Arc::new(vec![FileReview {
            path,
            text: after.into(),
            chunks: vec![InlineChunk {
                edit: 42,
                index: 0,
                range: Some(
                    0..after.lines().next().unwrap_or("").len() + usize::from(after.contains('\n')),
                ),
                original: before.into(),
                line: 1,
                issue: None,
            }],
        }]);
        app
    }
    pub(super) fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) {
        terminal
            .draw(|f| {
                app.editor_area = Some(f.area());
                crate::ui_editor::draw_editor(f, app, f.area());
            })
            .unwrap();
    }
    pub(super) fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::empty(),
            },
        );
    }
    #[test]
    fn editor_ai_review_colors_and_actions_are_visible_in_the_editable_file() {
        for theme in ["dark", "light", "high-contrast"] {
            for width in [30, 80, 140] {
                let mut app = pending("original\n", "AI replacement\nkept\n");
                app.set_theme_by_key(theme);
                let mut terminal = Terminal::new(TestBackend::new(width, 14)).unwrap();
                render(&mut app, &mut terminal);
                let view = &app.editor().unwrap().ai_review;
                assert_eq!(view.hits.len(), 6, "{theme} {width}");
                for hit in &view.hits {
                    assert!(view.area.contains((hit.area.x, hit.area.y).into()));
                }
                let cells = terminal.backend().buffer();
                let original = cells.content.iter().find(|cell| {
                    cell.symbol() == "o" && cell.bg == review_background(&app.colors, false)
                });
                assert!(
                    original.is_some(),
                    "removed text must have a red diff background"
                );
                let replacement = view
                    .source_rows
                    .iter()
                    .position(|row| row.is_some_and(|row| row.line_idx == 0))
                    .unwrap();
                let cell = &cells[(view.code.x + 4, view.code.y + replacement as u16)];
                assert_eq!(
                    cell.bg,
                    review_background(&app.colors, true),
                    "{theme} {width}"
                );
                let action_row = cells
                    .content
                    .chunks(width as usize)
                    .nth(view.code.y as usize)
                    .unwrap()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(
                    !action_row.contains("replacement"),
                    "virtual rows must clear the code underneath"
                );
                assert_eq!(
                    app.editor().unwrap().rope.to_string(),
                    "AI replacement\nkept\n"
                );
                if let Some(directory) = std::env::var_os("VTCODE_EDITOR_REVIEW_CAPTURE") {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let cells: Vec<_> = cells.content.iter().map(|cell| serde_json::json!({"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})).collect();
                    std::fs::write(
                        directory.join(format!("editor-review-{theme}-{width}.json")),
                        serde_json::to_vec(
                            &serde_json::json!({"width":width,"height":14,"cells":cells}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn editor_ai_review_clicks_and_selection_map_through_removed_and_wrapped_rows() {
        for wrapped in [false, true] {
            let mut app = pending(
                "old first\nold second\n",
                "replacement is a long line that wraps at this narrow width\nkept\nfinal\n",
            );
            app.editor_mut().unwrap().word_wrap = wrapped;
            let mut terminal = Terminal::new(TestBackend::new(34, 18)).unwrap();
            render(&mut app, &mut terminal);
            let view = &app.editor().unwrap().ai_review;
            let y = view.code.y
                + view
                    .source_rows
                    .iter()
                    .position(|row| row.is_some_and(|r| r.line_idx == 1))
                    .unwrap() as u16;
            let x = view.code.x + 2;
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
            assert_eq!(
                (
                    app.editor().unwrap().cursor_line,
                    app.editor().unwrap().cursor_col
                ),
                (1, 2),
                "wrap={wrapped}"
            );
            render(&mut app, &mut terminal);
            let view = &app.editor().unwrap().ai_review;
            let y = view.code.y
                + view
                    .source_rows
                    .iter()
                    .position(|row| row.is_some_and(|r| r.line_idx == 2))
                    .unwrap() as u16;
            let gutter_x = view.code.x - 2;
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                gutter_x,
                y,
            );
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), gutter_x, y);
            assert_eq!(
                app.editor().unwrap().selected_text().as_deref(),
                Some("final\n")
            );
            assert_eq!(
                app.editor().unwrap().rope.to_string(),
                "replacement is a long line that wraps at this narrow width\nkept\nfinal\n"
            );
        }
    }

    #[test]
    fn editor_ai_review_wheel_pages_scrollbar_and_keyboard_reach_real_text() {
        let before: String = (0..35).map(|line| format!("removed {line:02}\n")).collect();
        for wrapped in [false, true] {
            let mut app = pending(&before, "replacement\nlast\n");
            app.editor_mut().unwrap().word_wrap = wrapped;
            let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
            render(&mut app, &mut terminal);
            let top = app.editor().unwrap().ai_review.hits[0].area;
            mouse(&mut app, MouseEventKind::ScrollDown, top.x, top.y);
            render(&mut app, &mut terminal);
            assert!(app.editor().unwrap().ai_review.row > 0);
            for _ in 0..6 {
                app.editor_review_navigation(KeyCode::PageDown);
                render(&mut app, &mut terminal);
            }
            assert!(
                app.editor()
                    .unwrap()
                    .ai_review
                    .source_rows
                    .iter()
                    .flatten()
                    .any(|r| r.line_idx == 1)
            );
            let scrollbar = app.scrollbar_area.unwrap();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                scrollbar.right() - 1,
                scrollbar.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                scrollbar.right() - 1,
                scrollbar.bottom() - 1,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                scrollbar.right() - 1,
                scrollbar.bottom() - 1,
            );
            render(&mut app, &mut terminal);
            assert!(
                app.editor()
                    .unwrap()
                    .ai_review
                    .source_rows
                    .iter()
                    .flatten()
                    .any(|r| r.line_idx == 1)
            );
            let editor = app.editor_mut().unwrap();
            editor.cursor_line = 1;
            editor.ensure_visible();
            render(&mut app, &mut terminal);
            assert!(
                app.editor()
                    .unwrap()
                    .ai_review
                    .source_rows
                    .iter()
                    .flatten()
                    .any(|r| r.line_idx == 1)
            );
        }
    }

    #[test]
    fn editor_ai_review_decorations_follow_disjoint_typing_and_flag_overlaps() {
        let mut app = pending("old\n", "new\nkept\n");
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        render(&mut app, &mut terminal);
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 1;
        editor.paste_with_text("user\n");
        render(&mut app, &mut terminal);
        assert_eq!(app.editor().unwrap().ai_review.chunks[0].range, Some(0..4));
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 0;
        editor.cursor_col = 1;
        editor.paste_with_text("USER");
        render(&mut app, &mut terminal);
        assert!(app.editor().unwrap().ai_review.chunks[0].range.is_none());
        assert!(app.editor().unwrap().ai_review.highlights.is_empty());
    }

    #[test]
    fn editor_ai_review_split_panes_keep_independent_controls_and_mouse_coordinates() {
        let mut app = pending("old\n", "new\nkept\n");
        let mut other = Editor::test_with_text("other file\n");
        other.file_path = Some(std::path::absolute("other-editor.txt").unwrap());
        app.tabs.push(Tab::new_file(other));
        app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
        let active = app.active_group;
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal
            .draw(|f| crate::ui_chrome::draw_editor_section(f, &mut app, f.area()))
            .unwrap();
        assert!(!app.editor().unwrap().ai_review.active);
        let (group, review) = app
            .groups
            .iter()
            .enumerate()
            .filter(|(id, _)| *id != active)
            .find_map(|(id, group)| {
                group
                    .tabs
                    .get(group.active_tab)
                    .filter(|tab| tab.editor.ai_review.active)
                    .map(|tab| (id, &tab.editor.ai_review))
            })
            .unwrap();
        assert_eq!(review.hits.len(), 6);
        let y = review.code.y
            + review
                .source_rows
                .iter()
                .position(|row| row.is_some_and(|r| r.line_idx == 1))
                .unwrap() as u16;
        let x = review.code.x + 2;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
        assert_eq!(app.active_group, group);
        assert_eq!(
            (
                app.editor().unwrap().cursor_line,
                app.editor().unwrap().cursor_col
            ),
            (1, 2)
        );
    }

    #[test]
    fn editor_ai_review_deletion_only_and_bounded_lines_keep_real_cursor_rows() {
        let mut app = pending("removed\n", "kept\n");
        let file = &mut Arc::get_mut(&mut app.chat.rollback.inline_reviews).unwrap()[0];
        file.chunks[0].range = Some(5..5);
        file.chunks[0].line = 2;
        let mut terminal = Terminal::new(TestBackend::new(50, 14)).unwrap();
        render(&mut app, &mut terminal);
        let view = &app.editor().unwrap().ai_review;
        assert_eq!(view.hits.len(), 6);
        assert!(view.highlights.is_empty());
        assert!(view.source_rows.iter().flatten().any(|r| r.line_idx == 1));
        let line = "A".repeat(crate::highlight::MAX_SYNTAX_LINE_CHARS + 100);
        let after = format!("{line}\nkept\n");
        let mut app = pending("old\n", &after);
        app.editor_mut().unwrap().word_wrap = true;
        render(&mut app, &mut terminal);
        assert!(!app.editor().unwrap().ai_review.wrapped);
        let view = &app.editor().unwrap().ai_review;
        let y = view.code.y
            + view
                .source_rows
                .iter()
                .position(|row| row.is_some_and(|r| r.line_idx == 1))
                .unwrap() as u16;
        let x = view.code.x + 1;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
        assert_eq!(app.editor().unwrap().cursor_line, 1);
    }
}
