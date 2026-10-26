use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};
use std::borrow::Cow;

use crate::app::{ActivePane, App, SidebarView};
use crate::explorer_find::Focus;
use crate::text_input::TextInputId;
use crate::ui_widgets::{InputField, Tooltip};

#[cfg(test)]
#[path = "ui_explorer_find_tests.rs"]
mod tests;

pub(crate) struct Layout {
    pub panel: Rect,
    pub title: Rect,
    pub input: Rect,
    pub filter: Rect,
    pub fuzzy: Rect,
    pub regex: Rect,
    pub collapse: Rect,
    pub close: Rect,
    pub status: Rect,
    pub error: Rect,
}

pub(crate) fn layout(app: &App, area: Rect) -> Layout {
    let panel = crate::ui_chrome::explorer_find_area(app, area);
    let inner = Block::bordered().inner(panel);
    let top = Rect::new(inner.x, panel.y, inner.width, panel.height.min(1));
    let close_width = top.width.min(if top.width >= 27 { 12 } else { 3 });
    let title = Rect::new(top.x, top.y, top.width - close_width, top.height);
    let close = Rect::new(title.right(), top.y, close_width, top.height);
    let row = |index: u16| {
        Rect::new(
            inner.x,
            inner.y.saturating_add(index.min(inner.height)),
            inner.width,
            u16::from(index < inner.height),
        )
    };
    let input = row(0);
    // With only two interior rows, keep the hidden-files notice visible.
    // Options remain reachable through their keyboard shortcuts.
    let bottom = if inner.height == 2 {
        Rect::default()
    } else {
        row(1)
    };
    let collapse = Rect::new(
        bottom.right().saturating_sub(bottom.width.min(3)),
        bottom.y,
        bottom.width.min(3),
        bottom.height,
    );
    let options = Rect::new(
        bottom.x,
        bottom.y,
        bottom.width.saturating_sub(3),
        bottom.height,
    );
    let compact = options.width < 25;
    let filter = Rect::new(
        options.x,
        options.y,
        options.width.min(if compact { 3 } else { 9 }),
        options.height,
    );
    let fuzzy = Rect::new(
        filter.right(),
        bottom.y,
        options
            .right()
            .saturating_sub(filter.right())
            .min(if compact { 3 } else { 8 }),
        bottom.height,
    );
    let regex = Rect::new(
        fuzzy.right(),
        bottom.y,
        options
            .right()
            .saturating_sub(fuzzy.right())
            .min(if compact { 4 } else { 8 }),
        bottom.height,
    );
    let status = row(if inner.height == 2 { 1 } else { 2 });
    let error = if app.explorer.find.results.error.is_some() {
        row(3)
    } else {
        Rect::default()
    };
    Layout {
        panel,
        title,
        input,
        filter,
        fuzzy,
        regex,
        collapse,
        close,
        status,
        error,
    }
}

impl App {
    pub(crate) fn open_explorer_find(&mut self) {
        self.show_explorer = true;
        self.sidebar_view = SidebarView::Explorer;
        self.global_search_active = false;
        self.search_input_focused = false;
        self.explorer_context_active = false;
        if !self.explorer.find.open {
            // A new search session discovers external files immediately, even
            // before a watcher burst arrives. Keystrokes reuse this fresh scan.
            self.explorer.invalidate_directory_cache();
        }
        self.explorer.find.open = true;
        self.explorer.find.cursor = self.explorer.find.query.len();
        self.explorer.find.anchor = (!self.explorer.find.query.is_empty()).then_some(0);
        self.focus_text_input(TextInputId::ExplorerFind);
    }

    pub(crate) fn update_explorer_find(&mut self) {
        self.explorer.prepare_find();
        self.explorer_multi_selected.clear();
        self.explorer_selection_anchor = None;
        self.hovered_explorer = None;
        self.dragging_explorer_path = None;
        self.explorer_drop_target_idx = None;
        self.explorer_scrollbar_drag = None;
        if !self.explorer.find.loading {
            self.set_explorer_single_selection(self.explorer.selected);
        }
        self.refresh_explorer_tree_async();
    }

    pub(crate) fn close_explorer_find(&mut self) {
        self.explorer.find.open = false;
        self.explorer.find.query.clear();
        self.explorer.find.cursor = 0;
        self.explorer.find.anchor = None;
        self.explorer.find.focus = Focus::Tree;
        self.update_explorer_find();
        self.set_explorer_single_selection(self.explorer.selected);
        self.remove_text_input_regions(|id| id == TextInputId::ExplorerFind);
    }
}

fn can_collapse_results(app: &App) -> bool {
    app.explorer.find.filtering()
        && !app.explorer.find.loading
        && app.explorer.find.results.filtered_tree.is_some()
        && !app.explorer.entries.is_empty()
}

fn activate(app: &mut App, focus: Focus) {
    match focus {
        Focus::Filter => app.explorer.find.filter = !app.explorer.find.filter,
        Focus::Fuzzy => {
            app.explorer.find.fuzzy = !app.explorer.find.fuzzy;
            if app.explorer.find.fuzzy {
                app.explorer.find.regex = false;
            }
        }
        Focus::Regex => {
            app.explorer.find.regex = !app.explorer.find.regex;
            if app.explorer.find.regex {
                app.explorer.find.fuzzy = false;
            }
        }
        Focus::Collapse => {
            if can_collapse_results(app) {
                app.collapse_all_explorer_async();
            }
            return;
        }
        Focus::Close => {
            app.close_explorer_find();
            return;
        }
        _ => return,
    }
    app.update_explorer_find();
}

fn active(app: &App) -> bool {
    app.show_explorer && app.sidebar_view == SidebarView::Explorer
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !active(app)
        || app.active_pane != ActivePane::Explorer
        || app.rename.active
        || app.explorer_context_active
        || app.palette.active
        || crate::ui_quick_picker::active(app).is_some()
        || app.global_search_active
        || app.search_input_focused
    {
        return false;
    }
    if matches!(key.code, KeyCode::Char('f' | 'F'))
        && (key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::ALT)
            || key.modifiers == (KeyModifiers::SUPER | KeyModifiers::ALT))
    {
        app.open_explorer_find();
        return true;
    }
    if !app.explorer.find.open {
        return false;
    }
    let focus = app.explorer.find.focus;
    if key.code == KeyCode::Esc {
        app.close_explorer_find();
        return true;
    }
    if key.modifiers == KeyModifiers::ALT {
        match key.code {
            KeyCode::Char('f' | 'F') => {
                activate(app, Focus::Filter);
                return true;
            }
            KeyCode::Char('m' | 'M') => {
                activate(app, Focus::Fuzzy);
                return true;
            }
            KeyCode::Char('r' | 'R') => {
                activate(app, Focus::Regex);
                return true;
            }
            _ => {}
        }
    }
    if matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        let order = [
            Focus::Input,
            Focus::Filter,
            Focus::Fuzzy,
            Focus::Regex,
            Focus::Collapse,
            Focus::Close,
            Focus::Tree,
        ];
        let i = order.iter().position(|&item| item == focus).unwrap_or(0);
        let backwards = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
        app.explorer.find.focus =
            order[(i + if backwards { order.len() - 1 } else { 1 }) % order.len()];
        return true;
    }
    if matches!(
        focus,
        Focus::Filter | Focus::Fuzzy | Focus::Regex | Focus::Collapse | Focus::Close
    ) {
        if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
            activate(app, focus);
            return true;
        }
    }
    let from_input =
        focus == Focus::Input && matches!(key.code, KeyCode::Down | KeyCode::Up | KeyCode::Enter);
    let navigating_matches = focus == Focus::Tree
        && !app.explorer.find.query.is_empty()
        && matches!(key.code, KeyCode::Down | KeyCode::Up);
    if from_input || navigating_matches || key.code == KeyCode::F(3) {
        // Do not transfer focus onto an old, unmatched row while a replacement
        // search is still running. Its first match is selected on completion.
        if app.explorer.find.loading {
            return true;
        }
        let backwards = key.code == KeyCode::Up || key.modifiers.contains(KeyModifiers::SHIFT);
        app.explorer.select_find_match(backwards, from_input);
        app.set_explorer_single_selection(app.explorer.selected);
        app.explorer.find.focus = Focus::Tree;
        return true;
    }
    if focus == Focus::Input {
        let result = app.edit_text_input(TextInputId::ExplorerFind, key.code, key.modifiers, None);
        if result.changed {
            app.text_input_changed(TextInputId::ExplorerFind);
        }
        return result.consumed;
    }
    false
}

/// Shared input routing already handles caret placement and drag selection.
/// This handler owns the option buttons and releases input focus on other clicks.
pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !active(app) || !app.explorer.find.open {
        return false;
    }
    let Some(area) = app.explorer_area else {
        return false;
    };
    let layout = layout(app, area);
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        for (rect, focus) in [
            (layout.filter, Focus::Filter),
            (layout.fuzzy, Focus::Fuzzy),
            (layout.regex, Focus::Regex),
            (layout.collapse, Focus::Collapse),
            (layout.close, Focus::Close),
        ] {
            if rect.contains((mouse.column, mouse.row).into()) {
                app.active_pane = ActivePane::Explorer;
                app.terminal_panel_focused = false;
                activate(app, focus);
                if app.explorer.find.open {
                    app.focus_text_input(TextInputId::ExplorerFind);
                }
                return true;
            }
        }
        app.explorer.find.focus = Focus::Tree;
    }
    false
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if !app.explorer.find.open {
        return;
    }
    let layout = layout(app, area);
    let focused = app.active_pane == ActivePane::Explorer;
    let find = &app.explorer.find;
    let bg = app.colors.ui.widget_background_color(app.colors.bg_sidebar);
    // Keep the mode visible even when focus moves into the tree or editor.
    // Text and a frame carry the distinction as well as the theme's colors.
    let accent = if find.filtering() {
        app.colors.ui.warning_text_color(bg)
    } else {
        app.colors.ui.readable_foreground(
            app.colors
                .ui
                .focus_border
                .over(bg)
                .unwrap_or(app.colors.fg_accent),
            bg,
            4.5,
        )
    };
    let mode_style = Style::default()
        .fg(accent)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    f.render_widget(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent))
            .style(Style::default().bg(bg)),
        layout.panel,
    );
    f.render_widget(
        Paragraph::new(if find.filtering() && layout.status.is_empty() {
            " FILTERED "
        } else {
            "Find by Name"
        })
        .style(mode_style),
        layout.title,
    );
    let input_focused = focused && find.focus == Focus::Input;
    let hit_map = InputField::new(&find.query)
        .theme_control(&app.colors, app.colors.ui.input_colors, bg)
        .placeholder_text("File or folder name")
        .prefix("", Color::Reset)
        .cursor_marker(if input_focused && app.cursor_visible {
            "│"
        } else {
            ""
        })
        .cursor_pos(find.cursor)
        .selection_range(App::inline_selection_range(find.cursor, find.anchor))
        .render(f, layout.input);
    app.register_text_input(TextInputId::ExplorerFind, hit_map);
    let find = &app.explorer.find;
    for (rect, label, focus, selected) in [
        (
            layout.filter,
            if layout.filter.width < 9 {
                " ≡ "
            } else if find.filter {
                "✓ Filter"
            } else {
                "  Filter"
            },
            Focus::Filter,
            find.filter,
        ),
        (
            layout.fuzzy,
            if layout.fuzzy.width < 8 {
                " ≈ "
            } else if find.fuzzy {
                "✓ Fuzzy"
            } else {
                "  Fuzzy"
            },
            Focus::Fuzzy,
            find.fuzzy,
        ),
        (
            layout.regex,
            if layout.regex.width < 8 {
                " .* "
            } else if find.regex {
                "✓ Regex"
            } else {
                "  Regex"
            },
            Focus::Regex,
            find.regex,
        ),
        (layout.collapse, " ⊟ ", Focus::Collapse, false),
        (
            layout.close,
            if layout.close.width < 12 {
                " ✕ "
            } else if find.filtering() {
                " Show All ✕ "
            } else {
                " Close    ✕ "
            },
            Focus::Close,
            false,
        ),
    ] {
        let disabled = focus == Focus::Collapse && !can_collapse_results(app);
        let hovered = !disabled && rect.contains((app.mouse_col, app.mouse_row).into());
        let mut style = app.colors.ui.input_option_style(
            app.colors.ui.sidebar_text_color(bg),
            bg,
            selected,
            hovered,
        );
        if focus == Focus::Close {
            style = app.colors.ui.button_style(bg, false, hovered, true);
        }
        if disabled {
            style = style.fg(crate::ui_colors::disabled_text_color(
                app.colors
                    .ui
                    .disabled_foreground
                    .over(bg)
                    .unwrap_or(app.colors.fg_dim),
                app.colors.ui.sidebar_text_color(bg),
                bg,
                app.colors.ui.contrast_colors(),
            ));
        }
        if focused && find.focus == focus {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        f.render_widget(Paragraph::new(label).style(style), rect);
    }
    let status = status_text(find);
    let status = crate::ui_pickers::truncate_to_width(&status, layout.status.width as usize);
    f.render_widget(
        Paragraph::new(status).style(if find.filtering() {
            mode_style
        } else {
            Style::default()
                .fg(app.colors.ui.description_color(bg))
                .bg(bg)
        }),
        layout.status,
    );
    if let Some(error) = &find.results.error {
        f.render_widget(
            Paragraph::new(error.as_str()).style(
                Style::default()
                    .fg(app.colors.ui.error_text_color(bg))
                    .bg(bg),
            ),
            layout.error,
        );
    }
}

fn status_text(find: &crate::explorer_find::State) -> Cow<'static, str> {
    if find.filtering() {
        if find.loading {
            "Files hidden · searching…".into()
        } else if find.results.error.is_some() {
            "Files hidden · invalid pattern".into()
        } else {
            format!(
                "Files hidden · {} {}",
                find.results.count,
                if find.results.count == 1 {
                    "match"
                } else {
                    "matches"
                }
            )
            .into()
        }
    } else if find.query.is_empty() {
        if find.filter {
            "Filter ready · type a name"
        } else {
            "Type a file or folder name"
        }
        .into()
    } else if find.loading {
        "Searching names…".into()
    } else if find.results.error.is_some() {
        "Invalid pattern".into()
    } else {
        format!(
            "{} {} · highlight only",
            find.results.count,
            if find.results.count == 1 {
                "match"
            } else {
                "matches"
            }
        )
        .into()
    }
}

pub(crate) fn draw_tooltip(f: &mut Frame, app: &App) {
    if !active(app) || !app.explorer.find.open {
        return;
    }
    let Some(area) = app.explorer_area else {
        return;
    };
    let layout = layout(app, area);
    if let Some(error) = &app.explorer.find.results.error
        && (layout.input.contains((app.mouse_col, app.mouse_row).into())
            || layout.error.contains((app.mouse_col, app.mouse_row).into())
            || layout
                .status
                .contains((app.mouse_col, app.mouse_row).into()))
    {
        Tooltip::hover(vec![Line::raw(error.as_str())], &app.colors)
            .max_width(100)
            .prefer_below()
            .render(f, layout.input.x, layout.error.y);
        return;
    }
    if layout
        .status
        .contains((app.mouse_col, app.mouse_row).into())
    {
        let mut lines = vec![Line::raw(status_text(&app.explorer.find))];
        if app.explorer.find.filtering() {
            lines.push(Line::raw("Show All or Escape restores the full Explorer."));
        }
        Tooltip::hover(lines, &app.colors).prefer_below().render(
            f,
            layout.status.x,
            layout.status.y,
        );
        return;
    }
    for (rect, text) in [
        (layout.filter, "Filter / Highlight (Alt+F)"),
        (layout.fuzzy, "Fuzzy Match (Alt+M)"),
        (layout.regex, "Use Regular Expression (Alt+R)"),
        (layout.collapse, "Collapse All Results"),
        (
            layout.close,
            if app.explorer.find.filtering() {
                "Show All Files and Close Find by Name (Escape)"
            } else {
                "Close Find by Name (Escape)"
            },
        ),
    ] {
        if rect.contains((app.mouse_col, app.mouse_row).into()) {
            Tooltip::hover(vec![Line::raw(text)], &app.colors)
                .prefer_below()
                .render(f, rect.x, rect.y);
            break;
        }
    }
}

pub(crate) fn highlighted_name<'a>(
    name: &'a str,
    indices: &[usize],
    style: Style,
    highlight: Style,
) -> Vec<Span<'a>> {
    use unicode_segmentation::UnicodeSegmentation;
    if indices.is_empty() {
        return vec![Span::styled(name, style)];
    }
    let mut spans = Vec::new();
    let mut start = 0;
    let mut matched_index = 0;
    let mut was_match = false;
    let mut i = 0;
    for (byte, grapheme) in name.grapheme_indices(true) {
        // Matching positions and filename characters both advance in order.
        // Consume duplicates too; don't binary-search the same list per char.
        while indices.get(matched_index).is_some_and(|&index| index < i) {
            matched_index += 1;
        }
        let end = i + grapheme.chars().count();
        // A style boundary must not split a grapheme: doing so expands ZWJ
        // emoji and detaches combining marks, changing horizontal geometry.
        let matched = indices.get(matched_index).is_some_and(|&index| index < end);
        if matched != was_match {
            if byte > start {
                spans.push(Span::styled(&name[start..byte], if was_match { highlight } else { style }));
            }
            start = byte;
            was_match = matched;
        }
        i = end;
    }
    spans.push(Span::styled(
        &name[start..],
        if was_match { highlight } else { style },
    ));
    spans
}
