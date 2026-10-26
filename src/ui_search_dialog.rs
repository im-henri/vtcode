use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;
use crate::helpers::len_as_u16;
use crate::text_input::TextInputId;
use crate::ui_pickers::{DIALOG_MIN_HEIGHT, truncate_to_width};
use crate::ui_widgets::{InputField, PopupDialog, PopupPosition};

/// Maximum number of items shown in the vtcomplete popup.
const VTCOMPLETE_MAX_ITEMS: usize = 8;
/// Minimum width; long labels grow the popup up to a quiet, bounded maximum.
const VTCOMPLETE_POPUP_WIDTH: u16 = 30;
/// Maximum visible results in the global search dialog.
const GLOBAL_SEARCH_MAX_RESULTS: u16 = 15;

const MIN_SEARCH_BAR_WIDTH: u16 = 40;
// Keep the menu and editor tab rows accessible above the Find widget.
const SEARCH_BAR_TOP_OFFSET: u16 = 2;

pub(crate) struct SearchBarLayout {
    pub width: u16,
    pub resizing: bool,
    viewport: Rect,
}

impl Default for SearchBarLayout {
    fn default() -> Self {
        Self { width: 50, resizing: false, viewport: Rect::default() }
    }
}

impl SearchBarLayout {
    fn area(&mut self, viewport: Rect, height: u16) -> Rect {
        self.viewport = viewport;
        let max_width = viewport.width.saturating_sub(2);
        let width = self.width.clamp(MIN_SEARCH_BAR_WIDTH.min(max_width), max_width);
        Rect::new(viewport.x + viewport.width.saturating_sub(width + 1),
            viewport.y + SEARCH_BAR_TOP_OFFSET, width, height)
    }

    fn resize(&mut self, col: u16) {
        let max_width = self.viewport.width.saturating_sub(2);
        self.width = self.viewport.right().saturating_sub(1).saturating_sub(col)
            .clamp(MIN_SEARCH_BAR_WIDTH.min(max_width), max_width);
    }
}

pub(crate) fn search_bar_resize_hit(app: &App, col: u16, row: u16) -> bool {
    app.search_active && app.search_bar_area.is_some_and(|area| {
        !area.is_empty() && col == area.x && row >= area.y && row < area.bottom()
    })
}

/// Keep a resize gesture captured when it crosses inputs, panes, or the widget.
pub(crate) fn handle_search_bar_resize(app: &mut App, mouse: crossterm::event::MouseEvent) -> bool {
    use crossterm::event::{MouseButton, MouseEventKind};
    if !app.search_bar_layout.resizing { return false; }
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.search_bar_layout.resizing = false;
        return false;
    }
    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
            if app.search_active { app.search_bar_layout.resize(mouse.column); }
            if mouse.kind == MouseEventKind::Up(MouseButton::Left) || !app.search_active {
                app.search_bar_layout.resizing = false;
            }
        }
        _ => {}
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompletionPopupGeometry {
    pub popup_area: Rect,
    pub inner_area: Rect,
    pub scroll: usize,
}

pub(crate) fn completion_cursor_position(app: &mut App) -> Option<(u16, u16)> {
    let code_area = app.code_area?;
    let editor = app.editor_mut()?;

    if editor.word_wrap {
        let wrap_width = editor.effective_wrap_width();
        let cursor_wrap_row =
            editor.wrapped_row_for_position(editor.cursor_line, editor.cursor_col, wrap_width);
        let segment_start = editor
            .wrapped_rows(cursor_wrap_row, 1, wrap_width)
            .first()
            .map_or(0, |wrapped| wrapped.start_col);
        let rel_row = cursor_wrap_row.saturating_sub(editor.wrap_scroll_offset);
        let rel_col = editor.cursor_col.saturating_sub(segment_start);
        return Some((
            code_area.x + (rel_col as u16).min(code_area.width.saturating_sub(1)),
            code_area.y + (rel_row as u16).min(code_area.height.saturating_sub(1)),
        ));
    }

    let line = editor.rope.line(editor.cursor_line);
    let visible_col = editor.cursor_col.saturating_sub(editor.horizontal_scroll);
    let tab_size = editor.tab_size.max(1);
    let mut display_x = 0usize;
    let max_display_x = code_area.width.saturating_sub(1) as usize;
    for ch in line
        .chars_at(editor.horizontal_scroll.min(line.len_chars()))
        .take(visible_col)
    {
        if display_x >= max_display_x {
            break;
        }
        if ch == '\t' {
            display_x += tab_size - (display_x % tab_size);
        } else {
            display_x += UnicodeWidthChar::width(ch).unwrap_or(1);
        }
    }
    Some((
        code_area.x + display_x.min(max_display_x) as u16,
        code_area.y
            + (editor.cursor_line.saturating_sub(editor.scroll_offset) as u16)
                .min(code_area.height.saturating_sub(1)),
    ))
}

fn completion_popup_geometry(
    app: &mut App,
    viewport: Rect,
    item_count: usize,
    max_visible: usize,
    requested_width: u16,
    selected: usize,
) -> Option<CompletionPopupGeometry> {
    if item_count == 0 || viewport.width < 3 || viewport.height < 3 {
        return None;
    }
    let (cursor_x, cursor_y) = completion_cursor_position(app)?;
    let visible_count = item_count
        .min(max_visible)
        .min(viewport.height.saturating_sub(2) as usize);
    if visible_count == 0 {
        return None;
    }
    let popup_width = requested_width.min(viewport.width);
    let popup_height = visible_count as u16 + 2;
    let viewport_right = viewport.x.saturating_add(viewport.width);
    let viewport_bottom = viewport.y.saturating_add(viewport.height);
    let x = cursor_x
        .min(viewport_right.saturating_sub(popup_width))
        .max(viewport.x);
    let below_y = cursor_y.saturating_add(1);
    let y = if below_y.saturating_add(popup_height) <= viewport_bottom {
        below_y
    } else {
        cursor_y.saturating_sub(popup_height).max(viewport.y)
    };
    let popup_area = Rect::new(x, y, popup_width, popup_height);
    let inner_area = Rect::new(
        popup_area.x.saturating_add(1),
        popup_area.y.saturating_add(1),
        popup_area.width.saturating_sub(2),
        popup_area.height.saturating_sub(2),
    );
    let scroll = selected.saturating_sub(visible_count.saturating_sub(1));
    Some(CompletionPopupGeometry {
        popup_area,
        inner_area,
        scroll,
    })
}

pub(crate) fn vtcomplete_popup_geometry(
    app: &mut App,
    viewport: Rect,
) -> Option<CompletionPopupGeometry> {
    let item_count = app.vtcomplete_suggestions.len();
    let selected = app.vtcomplete_selected;
    let width = app
        .vtcomplete_suggestions
        .iter()
        .take(50)
        .map(|label| {
            label
                .chars()
                .take(64)
                .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
                .sum::<usize>()
                + app
                    .vtcomplete_details
                    .get(label)
                    .map_or(0, |detail| detail.annotation.len().saturating_sub(4))
        })
        .max()
        .unwrap_or(0)
        .saturating_add(12)
        .clamp(VTCOMPLETE_POPUP_WIDTH as usize, 76) as u16;
    completion_popup_geometry(
        app,
        viewport,
        item_count,
        VTCOMPLETE_MAX_ITEMS,
        width,
        selected,
    )
}

/// The visible suggestion list owns the wheel over its surface. A scroll
/// elsewhere dismisses it before the editor moves, including pending results.
pub(crate) fn handle_vtcomplete_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> bool {
    use crossterm::event::MouseEventKind;
    if !matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) || !app.vtcomplete_active
        || app.vtcomplete_suggestions.is_empty()
        || crate::ui_lsp_completion::visible(app)
    {
        return false;
    }
    let viewport = app.viewport_area;
    let inside = vtcomplete_popup_geometry(app, viewport).is_some_and(|geometry| {
        geometry
            .popup_area
            .contains((mouse.column, mouse.row).into())
    });
    if !inside {
        app.dismiss_vtcomplete();
        return false;
    }
    app.vtcomplete_selected = if mouse.kind == MouseEventKind::ScrollDown {
        app.vtcomplete_selected
            .saturating_add(1)
            .min(app.vtcomplete_suggestions.len() - 1)
    } else {
        app.vtcomplete_selected.saturating_sub(1)
    };
    app.vtcomplete_navigated = true;
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    app.mouse_hover_changed = true;
    true
}

pub(crate) fn lsp_completion_popup_geometry(
    app: &mut App,
    viewport: Rect,
) -> Option<CompletionPopupGeometry> {
    crate::ui_lsp_completion::layout(app, viewport).map(|layout| layout.list)
}

pub(crate) fn completion_index_at(
    geometry: CompletionPopupGeometry,
    col: u16,
    row: u16,
    item_count: usize,
) -> Option<usize> {
    let inner = geometry.inner_area;
    if col < inner.x
        || col >= inner.x.saturating_add(inner.width)
        || row < inner.y
        || row >= inner.y.saturating_add(inner.height)
    {
        return None;
    }
    let index = geometry.scroll + row.saturating_sub(inner.y) as usize;
    (index < item_count).then_some(index)
}

fn completion_row_style(app: &App, surface: ratatui::style::Color, is_selected: bool, is_hovered: bool) -> Style {
    app.colors.ui.suggest_row_style(surface, is_selected, is_hovered)
}

pub(crate) fn draw_search_bar(f: &mut Frame, app: &mut App) {
    use crate::ui_search_controls::{SearchControl, SearchScope, draw_button, draw_tooltip};

    app.search_control_regions.retain(|button| button.scope != SearchScope::File);
    let area = f.area();
    let bar_height = ((if app.replace_mode { 4 } else { 3 }) + u16::from(app.search_error.is_some()))
        .min(area.height.saturating_sub(SEARCH_BAR_TOP_OFFSET));
    let bar_area = app.search_bar_layout.area(area, bar_height);
    app.search_bar_area = Some(bar_area);
    if bar_area.width < 12 || bar_height < 3 { return; }
    // The find widget overlays the editor, including custom gradients. Keep
    // its alpha until that surface is available, before clearing editor text.
    let search_bg = crate::ui_widgets::widget_background(f, &app.colors, bar_area);
    let block = Block::default()
        .title(if app.replace_mode { " Find & Replace " } else { " Find " })
        .title_style(Style::default().fg(app.colors.ui.widget_text_color(search_bg)).bg(search_bg))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.colors.ui.widget_border.border(search_bg).unwrap_or(search_bg)))
        .style(Style::default().fg(app.colors.ui.widget_text_color(search_bg)).bg(search_bg));
    let inner = block.inner(bar_area);
    // A styled block does not erase editor text or its syntax modifiers.
    f.render_widget(Clear, bar_area);
    f.render_widget(block, bar_area);

    draw_button(f, app, SearchScope::File, SearchControl::Close,
        Rect::new(bar_area.right() - 4, bar_area.y, 3, 1), "×", false, false);
    let toggle_label = if app.replace_mode { "▾" } else { "▸" };
    draw_button(f, app, SearchScope::File, SearchControl::ToggleReplace,
        Rect::new(inner.x, inner.y, 2, 1), toggle_label, false, false);

    let match_info = if app.search_error.is_some() {
        String::new()
    } else if app.bg_file_search_running || app.file_search_refresh_pending {
        " Searching…".into()
    } else if app.search_matches.is_empty() {
        if app.search_query.is_empty() { String::new() } else { " No results".into() }
    } else {
        format!(" {}/{}{}", app.search_current + 1, app.search_matches.len(),
            if app.search_truncated { "+" } else { "" })
    };
    let query_width = inner.width.saturating_sub(18);
    let query_area = Rect::new(inner.x + 2, inner.y, query_width, 1);
    let focused = app.search_input_focused && !app.search_focus_replace;
    let input_bg = app.colors.ui.input_style(search_bg).bg.unwrap_or(search_bg);
    let field = InputField::new(&app.search_query)
        .theme_control(&app.colors, app.colors.ui.input_colors, search_bg)
        .placeholder_text("Find")
        .suffix(&match_info, app.colors.ui.description_color(input_bg))
        .cursor_marker(if focused { "│" } else { "" })
        .cursor_pos(app.search_cursor)
        .selection_range(if focused { App::inline_selection_range(app.search_cursor, app.search_selection_anchor) } else { None });
    let hit_map = field.render(f, query_area);
    app.register_text_input(TextInputId::SearchFind, hit_map);

    let options = [
        (SearchControl::MatchCase, "Aa", app.search_case_sensitive),
        (SearchControl::WholeWord, "ab", app.search_whole_word),
        (SearchControl::Regex, ".*", app.search_regex),
        (SearchControl::InSelection, "⊞", app.search_in_selection),
    ];
    let mut x = query_area.right();
    for (control, label, active) in options {
        let width = inner.right().saturating_sub(x).min(3);
        draw_button(f, app, SearchScope::File, control, Rect::new(x, inner.y, width, 1), label, active, false);
        x += width;
    }
    let no_matches = app.search_matches.is_empty();
    for (control, label) in [(SearchControl::Previous, "↑"), (SearchControl::Next, "↓")] {
        let width = inner.right().saturating_sub(x).min(2);
        draw_button(f, app, SearchScope::File, control, Rect::new(x, inner.y, width, 1), label, false, no_matches);
        x += width;
    }

    if app.replace_mode && inner.height >= 2 {
        let replace_area = Rect::new(inner.x + 2, inner.y + 1, inner.width.saturating_sub(12), 1);
        let focused = app.search_input_focused && app.search_focus_replace;
        let field = InputField::new(&app.replace_query)
            .theme_control(&app.colors, app.colors.ui.input_colors, search_bg)
            .placeholder_text("Replace")
            .cursor_marker(if focused { "│" } else { "" })
            .cursor_pos(app.replace_cursor)
            .selection_range(if focused { App::inline_selection_range(app.replace_cursor, app.replace_selection_anchor) } else { None });
        let hit_map = field.render(f, replace_area);
        app.register_text_input(TextInputId::SearchReplace, hit_map);
        let disabled = no_matches || app.bg_file_search_running || app.file_search_refresh_pending || app.bg_file_replace_running
            || app.editor().is_none_or(|editor| editor.read_only || editor.bg_loading);
        let buttons = [
            (SearchControl::PreserveCase, "AB", 3, app.search_preserve_case, false),
            (SearchControl::Replace, "↵", 3, false, disabled),
            (SearchControl::ReplaceAll, "⇆", 4, false, disabled),
        ];
        let mut x = replace_area.right();
        for (control, label, width, active, disabled) in buttons {
            let width = inner.right().saturating_sub(x).min(width);
            draw_button(f, app, SearchScope::File, control, Rect::new(x, inner.y + 1, width, 1), label, active, disabled);
            x += width;
        }
    }
    if let Some(error) = &app.search_error {
        let row = inner.y + if app.replace_mode { 2 } else { 1 };
        if row < inner.bottom() {
            f.render_widget(Paragraph::new(error.as_str())
                .style(Style::default().fg(app.colors.ui.error_text_color(search_bg)).bg(search_bg)),
                Rect::new(inner.x, row, inner.width, 1));
        }
    }
    let resize_hovered = search_bar_resize_hit(app, app.mouse_col, app.mouse_row);
    if resize_hovered || app.search_bar_layout.resizing {
        let style = Style::default().bg(search_bg)
            .fg(app.colors.ui.focus_border.over(search_bg)
                .unwrap_or_else(|| app.colors.ui.widget_text_color(search_bg)));
        for row in inner.y..inner.bottom() {
            f.render_widget(Paragraph::new("┃").style(style), Rect::new(bar_area.x, row, 1, 1));
        }
        if resize_hovered && !app.search_bar_layout.resizing {
            crate::ui_widgets::Tooltip::hover(vec![Line::raw("Drag left or right to resize")], &app.colors)
                .prefer_below().render(f, bar_area.x, bar_area.bottom().saturating_sub(1));
        }
    } else {
        draw_tooltip(f, app, SearchScope::File);
    }
}

pub(crate) fn draw_quick_open(f: &mut Frame, app: &mut App) {
    crate::ui_quick_picker::draw(f, app, crate::ui_quick_picker::Kind::Files);
}

pub(crate) fn draw_goto_line(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::Goto);
}

pub(crate) fn draw_vtcomplete(f: &mut Frame, app: &mut App) {
    let Some(geometry) = vtcomplete_popup_geometry(app, f.area()) else {
        return;
    };
    let popup_area = geometry.popup_area;
    let inner = geometry.inner_area;

    let query = completion_query(app);

    let surface = crate::ui_widgets::surface_background(f, popup_area, app.colors.bg_editor);
    let popup_bg = app.colors.ui.suggest_colors.background.over(surface).unwrap_or(surface);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" VTComplete ")
        .title_bottom(format!(
            " Tab insert · {} / {} ",
            app.vtcomplete_selected + 1,
            app.vtcomplete_suggestions.len()
        ))
        .title_style(
            Style::default()
                .fg(app.colors.ui.description_color(popup_bg))
                .bg(popup_bg),
        )
        .border_style(Style::default().fg(app.colors.ui.suggest_colors.border.border(popup_bg).unwrap_or(popup_bg)))
        .style(Style::default().bg(popup_bg));

    f.render_widget(Clear, popup_area);
    f.render_widget(block, popup_area);

    let hovered = completion_index_at(
        geometry,
        app.mouse_col,
        app.mouse_row,
        app.vtcomplete_suggestions.len(),
    );
    for (vi, (i, suggestion)) in app
        .vtcomplete_suggestions
        .iter()
        .enumerate()
        .skip(geometry.scroll)
        .take(inner.height as usize)
        .enumerate()
    {
        let row_area = Rect::new(inner.x, inner.y + vi as u16, inner.width, 1);
        let is_selected = i == app.vtcomplete_selected;
        let is_hovered = !is_selected && hovered == Some(i);
        let row_style = completion_row_style(app, popup_bg, is_selected, is_hovered);

        let call = suggestion.ends_with("()");
        let detail = app.vtcomplete_details.get(suggestion);
        let kind = detail.map_or(
            if call {
                crate::app_vtcomplete::CompletionKind::Function
            } else {
                crate::app_vtcomplete::CompletionKind::Word
            },
            |detail| detail.kind,
        );
        let annotation = detail.map_or(if call { "call" } else { "word" }, |detail| {
            detail.annotation
        });
        let annotation_width = if inner.width >= 26 {
            annotation.len() + 2
        } else {
            0
        };
        let label_width = (inner.width as usize).saturating_sub(3 + annotation_width);
        let display = completion_label(suggestion, label_width);
        let matches = crate::app::fuzzy_match_indices(&display, &query)
            .map(|(_, indices)| indices)
            .unwrap_or_default();
        let highlight = row_style
            .fg(app.colors.ui.suggest_match_color(popup_bg, is_selected))
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![Span::styled(format!(" {} ", kind.icon()), row_style)];
        for (index, ch) in display.chars().enumerate() {
            spans.push(Span::styled(
                ch.to_string(),
                if matches.binary_search(&index).is_ok() {
                    highlight
                } else {
                    row_style
                },
            ));
        }
        let used = UnicodeWidthStr::width(display.as_str());
        spans.push(Span::styled(
            " ".repeat(label_width.saturating_sub(used)),
            row_style,
        ));
        if annotation_width > 0 {
            let dim = app
                .colors
                .ui
                .description_color(row_style.bg.unwrap_or(popup_bg));
            spans.push(Span::styled(
            format!("  {annotation}"),
                if is_selected {
                    row_style
                } else {
                    row_style.fg(dim)
                },
        ));
        }
        // Clear the whole row first so wide Unicode labels retain a continuous
        // selection and hover surface without byte-based padding errors.
        f.render_widget(Block::default().style(row_style), row_area);
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style), row_area);
    }
}

/// The UI only needs the visible match, never a whole oversized identifier.
fn completion_query(app: &App) -> String {
    let Some(editor) = app.editor() else {
        return String::new();
    };
    let Some(line) = editor.rope.get_line(editor.cursor_line) else {
        return String::new();
    };
    let col = editor.cursor_col.min(line.len_chars());
    let reversed: Vec<char> = line
        .chars_at(col)
        .reversed()
        .take(128)
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '$')
        .collect();
    reversed.into_iter().rev().collect()
}

fn completion_label(text: &str, width: usize) -> String {
    let mut used = 0;
    text.chars()
        .take(256)
        .take_while(|ch| {
            used += UnicodeWidthChar::width(*ch).unwrap_or(0);
            used <= width
        })
        .collect()
}

pub(crate) fn draw_lsp_completion(f: &mut Frame, app: &mut App) {
    crate::ui_lsp_completion::draw(f, app);
}

pub(crate) fn draw_global_search(f: &mut Frame, app: &mut App) {
    let max_results = GLOBAL_SEARCH_MAX_RESULTS;
    let height = (len_as_u16(app.global_search_results.len()) + 3)
        .min(max_results + 3)
        .max(DIALOG_MIN_HEIGHT);

    let popup = PopupDialog::new("Search in Files").size(70, height).position(PopupPosition::TopCenter(2));
    let popup_bg = popup.widget_background(f, &app.colors);
    let popup_fg = app.colors.ui.widget_text_color(popup_bg);
    let inner = popup
        .title_color(popup_fg)
        .border_color(app.colors.ui.widget_border.border(popup_bg).unwrap_or(popup_bg))
        .bg(popup_bg)
        .render(f);

    if inner.height == 0 {
        return;
    }

    // Close button (×) in top-right corner of border
    let popup_x = inner.x.saturating_sub(1);
    let popup_w = inner.width + 2;
    let popup_y = inner.y.saturating_sub(1);
    let close_x = popup_x + popup_w - 4;
    let close_hovered = app.hovered_global_search_close;
    let close_style = app.colors.ui.toolbar_layer_style(app.colors.ui.widget_foreground, popup_bg, close_hovered, false, false);
    if close_x + 3 <= f.area().width {
        let area_w = f.area().width;
        let buf = f.buffer_mut();
        for (i, ch) in [' ', '×', ' '].iter().enumerate() {
            let cx = close_x + i as u16;
            if cx < area_w {
                buf[(cx, popup_y)].set_char(*ch).set_style(close_style);
            }
        }
    }

    // Input line
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let result_count =
        if app.global_search_results.is_empty() && !app.global_search_query.is_empty() {
            " No results".to_string()
        } else if !app.global_search_results.is_empty() {
            let suffix = if app.global_search_truncated {
                " (truncated)"
            } else {
                ""
            };
            format!(" {} results{}", app.global_search_results.len(), suffix)
        } else {
            String::new()
        };
    let hit_map = InputField::new(&app.global_search_query)
        .theme_control(&app.colors, app.colors.ui.input_colors, popup_bg)
        .prefix("🔍 ", app.colors.ui.icon_color(app.colors.ui.input_style(popup_bg).bg.unwrap_or(popup_bg)))
        .cursor_marker("│")
        .cursor_pos(app.global_search_field_cursors[0])
        .selection_range(
            App::inline_selection_range(
                app.global_search_field_cursors[0],
                app.global_search_field_selection_anchors[0],
            ),
        )
        .suffix(&result_count, app.colors.ui.description_color(app.colors.ui.input_style(popup_bg).bg.unwrap_or(popup_bg)))
        .render(f, input_area);
    app.register_text_input(TextInputId::GlobalSearch(0), hit_map);

    // Results
    let results_area_height = inner.height.saturating_sub(1) as usize;
    let scroll_offset = app
        .global_search_selected
        .saturating_sub(results_area_height.saturating_sub(1));

    for (idx, result) in app
        .global_search_results
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(results_area_height)
    {
        let y = inner.y + 1 + (idx - scroll_offset) as u16;
        if y >= inner.y + inner.height {
            break;
        }

        let is_selected = idx == app.global_search_selected;
        let hovered = (inner.x..inner.right()).contains(&app.mouse_col) && app.mouse_row == y;
        let row_style = app.colors.ui.list_row_style(app.colors.ui.widget_foreground, popup_bg, is_selected, true, hovered);
        let bg = row_style.bg.unwrap_or(popup_bg);
        let fg = row_style.fg.unwrap_or(popup_fg);

        let file_name = result
            .file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");
        let trimmed = result.line_text.trim();
        let trim_offset =
            result.line_text.chars().count() - result.line_text.trim_start().chars().count();
        let line_prefix = format!("{file_name}:{} ", result.line_num);

        let entry_area = Rect::new(inner.x, y, inner.width, 1);
        let max_preview = (inner.width as usize)
            .saturating_sub(unicode_width::UnicodeWidthStr::width(line_prefix.as_str()));
        let preview: String = truncate_to_width(trimmed, max_preview);

        // Highlight matching text within preview
        let match_start = result.col_start.saturating_sub(trim_offset);
        let match_end = result.col_end.saturating_sub(trim_offset);
        let match_style = app.colors.ui.search_match_style(row_style);
        let dim_style = Style::default().fg(app.colors.ui.readable_foreground(fg, bg, 4.5)).bg(bg);

        let mut spans = vec![Span::styled(line_prefix, Style::default().fg(fg).bg(bg))];
        let preview_char_count = preview.chars().count();
        if match_start < preview_char_count && match_end <= preview_char_count {
            let byte_start = preview
                .char_indices()
                .nth(match_start)
                .map_or(preview.len(), |(b, _)| b);
            let byte_end = preview
                .char_indices()
                .nth(match_end)
                .map_or(preview.len(), |(b, _)| b);
            spans.push(Span::styled(&preview[..byte_start], dim_style));
            spans.push(Span::styled(&preview[byte_start..byte_end], match_style));
            spans.push(Span::styled(&preview[byte_end..], dim_style));
        } else {
            spans.push(Span::styled(preview, dim_style));
        }

        let entry = Paragraph::new(Line::from(spans)).style(row_style);
        f.render_widget(entry, entry_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Tab;
    use crate::editor::Editor;
    use ratatui::{Terminal, backend::TestBackend};

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        (0..buffer.area.width)
            .filter_map(|column| buffer.cell((column, row)))
            .map(|cell| cell.symbol())
            .collect()
    }

    fn reference_completion_column(editor: &Editor, area: Rect) -> u16 {
        let text = editor.rope.line(editor.cursor_line).to_string();
        let mut display_x = 0usize;
        for ch in text
            .chars()
            .skip(editor.horizontal_scroll)
            .take(editor.cursor_col.saturating_sub(editor.horizontal_scroll))
        {
            display_x += if ch == '\t' {
                editor.tab_size.max(1) - display_x % editor.tab_size.max(1)
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(1)
            };
        }
        area.x + display_x.min(area.width.saturating_sub(1) as usize) as u16
    }

    #[test]
    fn completion_position_streams_clipped_unicode_and_far_horizontal_offsets() {
        let text = format!("{}\r\n", "a\t界e\u{301}\u{200d}\0".repeat(200));
        let mut app = App::test_new();
        app.tabs.push(Tab::new_file(Editor::test_with_text(&text)));
        app.active_tab = 0;
        for width in [0, 1, 2, 30, 80] {
            let area = Rect::new(5, 7, width, 20);
            app.code_area = Some(area);
            for horizontal in [0, 1, 31, 120_000, 180_000] {
                for distance in [0, 1, 10, 80, 1_000, 120_000] {
                    let editor = app.editor_mut().unwrap();
                    editor.horizontal_scroll = horizontal;
                    editor.cursor_col = horizontal.saturating_add(distance).min(editor.line_len(0));
                    let column = reference_completion_column(editor, area);
                    assert_eq!(completion_cursor_position(&mut app), Some((column, area.y)));
                }
            }
        }
        app.editor_mut().unwrap().rope =
            ropey::Rope::from_str(&format!("{}\r\n", "a\t界e\u{301}\u{200d}\0".repeat(20_000)));
        let area = Rect::new(5, 7, 80, 20);
        app.code_area = Some(area);
        for horizontal in [0, 120_000] {
            let editor = app.editor_mut().unwrap();
            editor.horizontal_scroll = horizontal;
            editor.cursor_col = editor.line_len(0);
            let column = reference_completion_column(editor, area);
            assert_eq!(completion_cursor_position(&mut app), Some((column, area.y)));
        }
    }

    #[test]
    fn completion_position_uses_configured_wrap_column() {
        let mut editor = Editor::test_with_text("abcdefghijklmnop\n");
        editor.viewport_width = 80;
        editor.word_wrap_column = 4;
        editor.word_wrap = true;
        editor.cursor_col = 9;
        let mut app = App::test_new();
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.code_area = Some(Rect::new(5, 7, 80, 20));
        assert_eq!(completion_cursor_position(&mut app), Some((6, 9)));
        assert!(app.editor().unwrap().wrap_layout_ready(4));
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_clipped_completion_popup_position() {
        use std::{hint::black_box, time::Instant};
        let mut editor = Editor::test_with_text(&"word\t界e\u{301}".repeat(300_000));
        editor.cursor_col = 1_000_000;
        let mut app = App::test_new();
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        let area = Rect::new(5, 7, 80, 20);
        app.code_area = Some(area);
        let expected = Some((
            reference_completion_column(app.editor().unwrap(), area),
            area.y,
        ));
        assert_eq!(completion_cursor_position(&mut app), expected);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let started = Instant::now();
                for _ in 0..16 {
                    let position = if optimized {
                        completion_cursor_position(black_box(&mut app))
                    } else {
                        Some((
                            reference_completion_column(black_box(app.editor().unwrap()), area),
                            area.y,
                        ))
                    };
                    assert_eq!(black_box(position), expected);
                }
                if optimized {
                    after.push(started.elapsed());
                } else {
                    before.push(started.elapsed());
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "clipped completion popup, 3MiB Unicode line: before={:?} after={:?} per position",
            before[3] / 16,
            after[3] / 16
        );
    }

    #[test]
    fn find_toggle_tooltip_renders_below_without_covering_controls() {
        let mut app = App::test_new();
        app.search_active = true;

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| draw_search_bar(frame, &mut app))
            .unwrap();
        let button = app.search_control_regions.iter()
            .find(|button| button.control == crate::ui_search_controls::SearchControl::MatchCase)
            .unwrap().area;
        crate::mouse::handle_mouse(&mut app, crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: button.x, row: button.y, modifiers: crossterm::event::KeyModifiers::NONE,
        });
        terminal.draw(|frame| draw_search_bar(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        // The Aa control must remain visible while hovered.
        let label: String = (button.x..button.right()).map(|x| buffer[(x, button.y)].symbol()).collect();
        assert!(label.contains("Aa"));

        let bar_bottom = app.search_bar_area.unwrap().bottom();
        let tooltip_row = (bar_bottom..20)
            .find(|&row| buffer_row_text(buffer, row).contains("Match Case"));
        assert_eq!(tooltip_row, Some(bar_bottom + 1));
    }

    #[test]
    fn vtcomplete_hovered_row_uses_hover_background() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("pr");
        editor.cursor_col = 2;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.code_area = Some(Rect::new(5, 5, 70, 15));
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = vec![
            "print".to_string(),
            "println".to_string(),
            "printf".to_string(),
        ];

        let viewport = Rect::new(0, 0, 100, 30);
        let geometry = vtcomplete_popup_geometry(&mut app, viewport).unwrap();
        app.mouse_col = geometry.inner_area.x;
        app.mouse_row = geometry.inner_area.y + 1;
        let popup_bg = app.colors.ui.suggest_colors.background.over(app.colors.bg_editor).unwrap_or(app.colors.bg_editor);
        let hover_bg = app.colors.ui.list_hover_background.over(popup_bg).unwrap_or(popup_bg);
        let selected_bg = app.colors.ui.suggest_selection.background.over(popup_bg).unwrap_or(popup_bg);

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| draw_vtcomplete(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(
            buffer
                .cell((geometry.inner_area.x, geometry.inner_area.y))
                .unwrap()
                .bg,
            selected_bg,
        );
        assert_eq!(
            buffer
                .cell((geometry.inner_area.x, geometry.inner_area.y + 1))
                .unwrap()
                .bg,
            hover_bg,
        );
        assert_eq!(
            buffer
                .cell((geometry.inner_area.x, geometry.inner_area.y + 2))
                .unwrap()
                .bg,
            popup_bg,
        );
    }

    #[test]
    fn builtin_popup_highlights_fuzzy_matches_and_explains_learned_calls() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("gU");
        editor.cursor_col = 2;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.code_area = Some(Rect::new(2, 2, 76, 15));
        app.vtcomplete_suggestions = vec!["getUser()".into(), "getURL()".into()];
        let geometry = vtcomplete_popup_geometry(&mut app, Rect::new(0, 0, 90, 24)).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal
            .draw(|frame| draw_vtcomplete(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = buffer_row_text(buffer, geometry.inner_area.y);
        assert!(row.contains("ƒ getUser()"), "{row}");
        assert!(row.contains("call"), "{row}");
        let first = geometry.inner_area.x + 3;
        assert!(
            buffer[(first, geometry.inner_area.y)]
                .modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            buffer[(first + 3, geometry.inner_area.y)]
                .modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            !buffer[(first + 1, geometry.inner_area.y)]
                .modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn native_prediction_kinds_render_cleanly_in_dark_light_and_narrow_popups() {
        use crate::app_vtcomplete::{CompletionDetail, CompletionKind as Kind};
        for theme in crate::app_theme::load_builtin_themes() {
            for width in [28, 90] {
                let mut app = App::test_new();
                app.colors = theme.to_theme_colors();
                let mut editor = Editor::test_with_text("re");
                editor.cursor_col = 2;
                app.tabs.push(Tab::new_file(editor));
                app.active_tab = 0;
                app.code_area = Some(Rect::new(0, 0, width, 16));
                for (label, kind, annotation) in [
                    ("readPacket()", Kind::Function, "call"),
                    ("readTimeout", Kind::Member, "member"),
                    ("requestPayload", Kind::Argument, "argument"),
                    ("retryEnabled", Kind::Value, "value"),
                    ("readConfig", Kind::Word, "recent"),
                    ("ret", Kind::Snippet, "snippet"),
                ] {
                    app.vtcomplete_suggestions.push(label.into());
                    app.vtcomplete_details.insert(
                        label.into(),
                        CompletionDetail {
                            kind,
                            annotation,
                            ..Default::default()
                        },
                    );
                }
                let viewport = Rect::new(0, 0, width, 18);
                let geometry = vtcomplete_popup_geometry(&mut app, viewport).unwrap();
                assert!(geometry.popup_area.right() <= viewport.right());
                let mut terminal = Terminal::new(TestBackend::new(width, 18)).unwrap();
                terminal
                    .draw(|frame| draw_vtcomplete(frame, &mut app))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                assert!(buffer_row_text(buffer, geometry.popup_area.y).contains("VTComplete"));
                assert!(buffer_row_text(buffer, geometry.inner_area.y).contains("readPacket()"));
                if width == 90 {
                    assert!(
                        buffer_row_text(buffer, geometry.inner_area.y + 2).contains("argument")
                    );
                    assert!(buffer_row_text(buffer, geometry.inner_area.y + 5).contains("snippet"));
                }
                crate::ui_control_visibility_tests::snapshot(
                    buffer,
                    &format!("vtcomplete {} {width}", theme.name),
                );
            }
        }
    }

    #[test]
    fn completion_labels_clip_by_cells_and_query_is_bounded() {
        assert_eq!(completion_label("界名test", 5), "界名t");
        assert_eq!(completion_label("界名", 1), "");
        let mut app = App::test_new();
        let text = "a".repeat(10_000);
        let mut editor = Editor::test_with_text(&text);
        editor.cursor_col = 10_000;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        assert_eq!(completion_query(&app).len(), 128);
    }

    fn wheel_completion_app() -> App {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&"getUsers();\n".repeat(100));
        editor.cursor_line = 20;
        editor.cursor_col = 3;
        editor.scroll_offset = 10;
        editor.viewport_height = 20;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.active_pane = crate::app::ActivePane::Editor;
        app.viewport_area = Rect::new(0, 0, 100, 30);
        app.code_area = Some(Rect::new(2, 2, 80, 20));
        app.show_explorer = false;
        app.show_panel = false;
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = (0..20).map(|index| format!("getUser{index}")).collect();
        app
    }

    #[test]
    fn wheel_over_builtin_popup_selects_without_scrolling_or_editing_the_document() {
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        let mut app = wheel_completion_app();
        let before = app.editor().unwrap().rope.clone();
        let viewport = app.viewport_area;
        let geometry = vtcomplete_popup_geometry(&mut app, viewport).unwrap();
        let mouse = |kind| MouseEvent {
            kind,
            column: geometry.inner_area.x + 2,
            row: geometry.inner_area.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown));
        assert_eq!(app.vtcomplete_selected, 1);
        assert!(app.vtcomplete_navigated);
        assert_eq!(app.editor().unwrap().scroll_offset, 10);
        assert!(app.editor().unwrap().rope.is_instance(&before));
        assert_eq!(
            (
                app.editor().unwrap().cursor_line,
                app.editor().unwrap().cursor_col
            ),
            (20, 3)
        );
        assert_eq!(
            (app.mouse_col, app.mouse_row),
            (geometry.inner_area.x + 2, geometry.inner_area.y + 1)
        );
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp));
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp));
        assert_eq!(
            app.vtcomplete_selected, 0,
            "selection clamps at the first row"
        );
        app.vtcomplete_selected = app.vtcomplete_suggestions.len() - 1;
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown));
        assert_eq!(
            app.vtcomplete_selected,
            app.vtcomplete_suggestions.len() - 1
        );
        assert_eq!(app.editor().unwrap().scroll_offset, 10);
    }

    #[test]
    fn wheel_outside_builtin_popup_cancels_pending_results_then_scrolls_the_editor() {
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        let mut app = wheel_completion_app();
        let token = crate::background::CancellationToken::new();
        app.vtcomplete_cancel_token = Some(token.clone());
        let generation = app.vtcomplete_generation;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 75,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!app.vtcomplete_active);
        assert!(app.vtcomplete_suggestions.is_empty());
        assert!(token.is_cancelled());
        assert_ne!(app.vtcomplete_generation, generation);
        assert!(
            app.editor().unwrap().scroll_offset < 10,
            "ordinary editor scrolling still runs"
        );
    }

    #[test]
    fn lsp_and_builtin_completions_share_hover_style() {
        let app = App::test_new();
        let popup_bg = app.colors.ui.suggest_colors.background.over(app.colors.bg_editor).unwrap_or(app.colors.bg_editor);
        let style = completion_row_style(&app, popup_bg, false, true);
        assert_eq!(style.bg, app.colors.ui.list_hover_background.over(popup_bg).or(Some(popup_bg)));
        let foreground = app.colors.ui.suggest_colors.foreground.over(style.bg.unwrap()).unwrap_or(app.colors.fg_text);
        assert_eq!(style.fg, Some(app.colors.ui.readable_foreground(foreground, style.bg.unwrap(), 4.5)));
    }
}
