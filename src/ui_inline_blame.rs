//! Hover details for the current line's author/age annotation. The renderer
//! supplies the exact hit region; the existing blame snapshot supplies all data.
use std::{
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style, text::Span};

use crate::{
    app::{App, TabKind},
    git_blame,
    ui_text_selection::{self, Selection},
    ui_widgets::{VerticalScrollbar, surface_background},
};

const HOVER_DELAY: Duration = Duration::from_secs(2);

struct Target {
    navigation_id: u64,
    anchor: Rect,
    bounds: Rect,
    line: usize,
    document: Arc<git_blame::Document>,
}

impl Target {
    fn same(&self, other: &Self) -> bool {
        self.navigation_id == other.navigation_id
            && self.anchor == other.anchor
            && self.bounds == other.bounds
            && self.line == other.line
            && Arc::ptr_eq(&self.document, &other.document)
    }
}

pub(crate) struct Hover {
    target: Target,
    started: Instant,
    pointer: (u16, u16),
    area: Rect,
    width: u16,
    message: String,
    rows: Vec<Range<usize>>,
    scroll: usize,
    max_scroll: usize,
    scrollbar_drag: Option<ScrollbarDrag>,
    selection: Selection,
}

#[derive(Clone, Copy)]
struct ScrollbarDrag {
    start_row: u16,
    start_offset: usize,
    grab: u16,
}

impl Hover {
    fn contains(&self, column: u16, row: u16) -> bool {
        let point = (column, row).into();
        self.target.anchor.contains(point) || self.area.contains(point)
    }

    fn text_area(&self) -> Rect {
        Rect::new(
            self.area.x + 1,
            self.area.y + 1,
            self.area
                .width
                .saturating_sub(2 + u16::from(self.max_scroll > 0)),
            self.area.height.saturating_sub(2),
        )
    }

    fn scrollbar(&self) -> Option<(Rect, VerticalScrollbar)> {
        let text = self.text_area();
        (self.max_scroll > 0 && !text.is_empty()).then(|| {
            let area = Rect::new(text.right(), text.y, 1, text.height);
            let bar =
                VerticalScrollbar::new(area.x, area.y, area.height, self.rows.len(), self.scroll);
            (area, bar)
        })
    }

    fn start_scrollbar_drag(&mut self, mouse: MouseEvent) -> bool {
        let Some((area, bar)) = self.scrollbar() else {
            return false;
        };
        if !area.contains((mouse.column, mouse.row).into()) {
            return false;
        }
        let (top, size) = bar.thumb();
        let row = mouse.row - area.y;
        let grab = if (top..top + size).contains(&row) {
            row - top
        } else {
            let grab = size / 2;
            self.scroll = self.thumb_offset(&bar, mouse.row, grab);
            grab
        };
        self.selection.dragging = false;
        self.scrollbar_drag = Some(ScrollbarDrag {
            start_row: mouse.row,
            start_offset: self.scroll,
            grab,
        });
        true
    }

    fn thumb_offset(&self, bar: &VerticalScrollbar, row: u16, grab: u16) -> usize {
        let (_, size) = bar.thumb();
        let travel = bar.height.saturating_sub(size);
        let top = row
            .saturating_sub(bar.top_y.saturating_add(grab))
            .min(travel);
        // The widget rounds down when painting the thumb. Round up when
        // mapping a thumb cell back to text so it stays under the pointer.
        (u128::from(top) * self.max_scroll as u128).div_ceil(u128::from(travel.max(1))) as usize
    }

    fn dragging(&self) -> bool {
        self.scrollbar_drag.is_some() || self.selection.dragging
    }

    fn byte_at(&self, mouse: MouseEvent, text_area: Rect) -> usize {
        let index = mouse
            .row
            .saturating_sub(text_area.y)
            .min(text_area.height - 1);
        let row = &self.rows[self.scroll + usize::from(index)];
        ui_text_selection::byte_at(&self.message, row, text_area, mouse)
    }
}

pub(crate) fn begin_frame(app: &mut App) {
    // Animation replays reuse the same source cells and their hit region.
    if app.rendering_animation_frame {
        return;
    }
    if let Some(editor) = app.editor_mut() {
        editor.blame.inline_candidate = None;
        editor.blame.inline_annotation = None;
    }
    for (index, group) in app.groups.iter_mut().enumerate() {
        if index != app.active_group
            && let Some(tab) = group.tabs.get_mut(group.active_tab)
        {
            tab.editor.blame.inline_candidate = None;
            tab.editor.blame.inline_annotation = None;
        }
    }
}

fn source(app: &App, matches: impl Fn(u64, Rect) -> bool) -> Option<Target> {
    app.tabs
        .get(app.active_tab)
        .map(|tab| (tab, app.code_area))
        .into_iter()
        .chain(
            app.groups
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != app.active_group)
                .filter_map(|(_, group)| {
                    group
                        .tabs
                        .get(group.active_tab)
                        .map(|tab| (tab, group.code_area))
                }),
        )
        .find_map(|(tab, bounds)| {
            let editor = &tab.editor;
            if !matches!(tab.kind, TabKind::File | TabKind::GitRevision) || editor.blame.enabled {
                return None;
            }
            let (anchor, line) = editor.blame.inline_annotation?;
            if line != editor.cursor_line || !matches(editor.navigation_id, anchor) {
                return None;
            }
            let document = git_blame::cached(app, editor)?;
            if matches!(document.lines.get(line)?, git_blame::Line::Empty) {
                return None;
            }
            Some(Target {
                navigation_id: editor.navigation_id,
                anchor,
                bounds: bounds?,
                line,
                document: document.clone(),
            })
        })
}

pub(crate) fn obscured(app: &App) -> bool {
    !app.terminal_has_focus
        || app.change_dir_active
        || app.menu_open
        || app.context_menu_active
        || app.tab_context_active
        || app.explorer_context_active
        || app.sc_context_active
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
}

/// One deadline while the pointer is resting on an annotation. A visible card
/// has no timer, and commit-message formatting waits until the deadline too.
pub(crate) fn next_wait(app: &App, now: Instant) -> Option<Duration> {
    let hover = app.inline_blame_hover.as_ref()?;
    (hover.area.is_empty() && !obscured(app) && hover.contains(app.mouse_col, app.mouse_row))
        .then(|| HOVER_DELAY.saturating_sub(now.saturating_duration_since(hover.started)))
}

pub(crate) fn hover_due(app: &App) -> bool {
    next_wait(app, Instant::now()).is_some_and(|wait| wait.is_zero())
}

/// Deliver leave even when another workbench surface consumes the event.
pub(crate) fn leave(app: &mut App, column: u16, row: u16) {
    if app
        .inline_blame_hover
        .as_ref()
        .is_some_and(|hover| !hover.contains(column, row))
    {
        app.inline_blame_hover = None;
        app.mouse_hover_changed = true;
    }
}

fn captured(hover: &Hover, kind: MouseEventKind) -> bool {
    hover.dragging()
        && matches!(
            kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        )
}

pub(crate) fn captured_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    app.inline_blame_hover
        .as_ref()
        .is_some_and(|hover| captured(hover, mouse.kind))
        && handle_mouse(app, mouse)
}

pub(crate) fn copy_key(app: &mut App, key: KeyEvent) -> bool {
    if !ui_text_selection::is_copy_key(key) || obscured(app) {
        return false;
    }
    let Some(text) = app
        .inline_blame_hover
        .as_ref()
        .and_then(|hover| hover.selection.text(&hover.message))
    else {
        return false;
    };
    if crate::clipboard::set_text(text).is_ok() {
        app.status_message = "Copied selected blame text".into();
    } else {
        app.notify_error("Could not copy selected blame text: clipboard unavailable");
    }
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if obscured(app) {
        app.inline_blame_hover = None;
        return false;
    }
    let point = (mouse.column, mouse.row).into();
    if let Some(hover) = &mut app.inline_blame_hover
        && (hover.area.contains(point) || captured(hover, mouse.kind))
    {
        let text_area = hover.text_area();
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                hover.scrollbar_drag = None;
                if !hover.start_scrollbar_drag(mouse) {
                    hover.selection = Selection::default();
                    if text_area.contains(point) {
                        hover.selection.start(hover.byte_at(mouse, text_area));
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
                if hover.scrollbar_drag.is_some() =>
            {
                if let Some((drag, (_, bar))) = hover.scrollbar_drag.zip(hover.scrollbar()) {
                    hover.scroll = if mouse.row == drag.start_row {
                        drag.start_offset.min(hover.max_scroll)
                    } else {
                        hover.thumb_offset(&bar, mouse.row, drag.grab)
                    };
                }
                if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                    hover.scrollbar_drag = None;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
                if hover.selection.dragging =>
            {
                if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
                    if mouse.row < text_area.y {
                        hover.scroll = hover.scroll.saturating_sub(1);
                    } else if mouse.row >= text_area.bottom() {
                        hover.scroll = (hover.scroll + 1).min(hover.max_scroll);
                    }
                }
                hover.selection.extend(hover.byte_at(mouse, text_area));
                hover.selection.dragging = mouse.kind != MouseEventKind::Up(MouseButton::Left);
            }
            MouseEventKind::ScrollDown => {
                hover.scroll = hover.scroll.saturating_add(3).min(hover.max_scroll)
            }
            MouseEventKind::ScrollUp => hover.scroll = hover.scroll.saturating_sub(3),
            _ => {}
        }
    } else if mouse.kind == MouseEventKind::Moved {
        let Some(target) = source(app, |_, anchor| anchor.contains(point)) else {
            return false;
        };
        if app
            .inline_blame_hover
            .as_ref()
            .is_none_or(|hover| !hover.target.same(&target))
        {
            app.inline_blame_hover = Some(Hover {
                target,
                started: Instant::now(),
                pointer: (mouse.column, mouse.row),
                area: Rect::default(),
                width: 0,
                message: String::new(),
                rows: Vec::new(),
                scroll: 0,
                max_scroll: 0,
                scrollbar_drag: None,
                selection: Selection::default(),
            });
        } else if let Some(hover) = &mut app.inline_blame_hover
            && hover.area.is_empty()
            && hover.pointer != (mouse.column, mouse.row)
        {
            hover.started = Instant::now();
            hover.pointer = (mouse.column, mouse.row);
        }
    } else {
        app.inline_blame_hover = None;
        return false;
    }
    app.dismiss_lsp_hover();
    app.hovered_gutter_line = None;
    app.hovered_editor_pos = None;
    app.mouse_hover_changed = true;
    true
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    draw_at(f, app, Instant::now());
}

fn draw_at(f: &mut Frame, app: &mut App, now: Instant) {
    let Some(mut hover) = app.inline_blame_hover.take() else {
        return;
    };
    if obscured(app)
        || (!hover.dragging()
            && hover.selection.range().is_none()
            && !hover.contains(app.mouse_col, app.mouse_row))
        || source(app, |id, anchor| {
            id == hover.target.navigation_id && anchor == hover.target.anchor
        })
        .is_none_or(|target| !hover.target.same(&target))
    {
        return;
    }
    if now.saturating_duration_since(hover.started) < HOVER_DELAY {
        app.inline_blame_hover = Some(hover);
        return;
    }
    let bounds = hover.target.bounds.intersection(f.area());
    let anchor = hover.target.anchor;
    let width = bounds.width.min(84);
    let below = bounds.bottom().saturating_sub(anchor.bottom());
    let height = anchor.y.saturating_sub(bounds.y).max(below).min(20);
    if width < 12 || height < 4 {
        return;
    }
    let content_width = width - 4;
    if hover.width != content_width {
        hover.message = match &hover.target.document.lines[hover.target.line] {
            git_blame::Line::Committed(commit) => {
                crate::editor_blame::commit_details_message(commit, content_width)
            }
            git_blame::Line::Uncommitted => {
                "This line belongs to your current working changes and has not been committed."
                    .into()
            }
            git_blame::Line::Empty => return,
        };
        hover.rows = crate::editor_blame::wrapped_message_ranges(&hover.message, content_width);
        hover.width = content_width;
        hover.selection = Selection::default();
        hover.scrollbar_drag = None;
    }
    let visible = usize::from(height - 2);
    hover.max_scroll = hover.rows.len().saturating_sub(visible);
    hover.scroll = hover.scroll.min(hover.max_scroll);
    let selection_style = if hover.selection.range().is_some() {
        let base = app.colors.bg_editor;
        let surface = app
            .colors
            .ui
            .hover_colors
            .background
            .over(base)
            .unwrap_or(base);
        app.colors.ui.input_selection_style(surface)
    } else {
        Style::default()
    };
    let mut lines: Vec<_> = hover
        .rows
        .iter()
        .skip(hover.scroll)
        .take(visible)
        .map(|row| hover.selection.line(&hover.message, row, selection_style))
        .collect();
    // Keep the card width and anchor stable while scrolling between short and
    // long lines.
    if let Some(line) = lines.first_mut() {
        let padding = usize::from(content_width).saturating_sub(line.width());
        line.spans.push(Span::raw(" ".repeat(padding)));
    }
    let title = if matches!(
        hover.target.document.lines[hover.target.line],
        git_blame::Line::Uncommitted
    ) {
        "Uncommitted changes"
    } else {
        "Commit details"
    };
    let mut tooltip = crate::ui_widgets::Tooltip::hover(lines, &app.colors)
        .title(title, Style::default())
        .max_width(width)
        .max_height(height)
        .avoid(Some(anchor));
    if below >= height {
        tooltip = tooltip.prefer_below();
    }
    // Keep the popup away from other panes, terminals, tabs and status controls.
    hover.area = tooltip.render(
        f,
        anchor.x.min(bounds.right() - width).max(bounds.x),
        anchor.y,
    );
    if !hover.area.is_empty() {
        if let Some((area, bar)) = hover.scrollbar() {
            let bg = surface_background(f, area, app.colors.bg_editor);
            let hovered = area.contains((app.mouse_col, app.mouse_row).into());
            let (track, thumb) =
                app.colors
                    .ui
                    .scrollbar_colors(bg, hovered, hover.scrollbar_drag.is_some());
            bar.symbols('┃', '│').colors(thumb, track, track).render(f);
        }
        app.inline_blame_hover = Some(hover);
    }
}

#[cfg(test)]
#[path = "ui_inline_blame_tests.rs"]
mod tests;
