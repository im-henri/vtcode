//! Shared geometry and input for the workspace switcher and saved sets.
use crate::{
    app::App,
    text_input::{TextInput, TextInputHistory, TextInputHitMap},
};
use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(crate) struct Search {
    pub text: String,
    cursor: usize,
    anchor: Option<usize>,
    history: TextInputHistory,
    input: Option<TextInputHitMap>,
    dragging: bool,
}
impl Search {
    pub fn edit(&mut self, key: KeyEvent, paste: Option<&str>) -> bool {
        self.history
            .edit(
                TextInput::new(&mut self.text, &mut self.cursor, &mut self.anchor),
                key.code,
                key.modifiers,
                paste,
            )
            .changed
    }
    pub fn mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(map) = &self.input else {
            return false;
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if map.contains(mouse.column, mouse.row) => {
                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    self.anchor.get_or_insert(self.cursor);
                } else {
                    self.anchor = None;
                }
                self.cursor = map.byte_at(mouse.column, mouse.row);
                self.dragging = true;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                self.anchor.get_or_insert(self.cursor);
                self.cursor = map.byte_at(mouse.column, mouse.row);
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => self.dragging = false,
            _ => return false,
        }
        true
    }
}

pub(crate) fn base(app: &App) -> Style {
    Style::default()
        .bg(app.colors.bg_sidebar)
        .fg(app.colors.ui.sidebar_text_color(app.colors.bg_sidebar))
}
pub(crate) fn secondary(app: &App) -> Style {
    base(app).fg(app
        .colors
        .ui
        .readable_foreground(app.colors.fg_dim, app.colors.bg_sidebar, 4.5))
}
pub(crate) fn selected(app: &App) -> Style {
    base(app)
        .bg(app.colors.bg_selection)
        .fg(app
            .colors
            .ui
            .readable_foreground(app.colors.fg_selected, app.colors.bg_selection, 4.5))
}

pub(crate) fn surface(f: &mut Frame, app: &mut App, title: &str, desired: usize) -> Option<Rect> {
    let popup = &mut app.workspaces.popup;
    popup.area = Rect::default();
    popup.tabs = [Rect::default(); 2];
    popup.dismiss = Rect::default();
    popup.search.input = None;
    let screen = f.area();
    if screen.width < 4 || screen.height < 4 {
        return None;
    }
    let width = screen.width.min(72);
    let height = desired.min(screen.height.saturating_sub(1) as usize).max(3) as u16;
    let anchor = crate::ui_chrome::workspace_controls(app).map(|(rect, _)| rect);
    let x = anchor
        .map_or(screen.x + (screen.width - width) / 2, |rect| rect.x)
        .clamp(screen.x, screen.right().saturating_sub(width));
    let y = app
        .menu_bar_area
        .map_or(screen.y + 1, |bar| bar.bottom())
        .min(screen.bottom().saturating_sub(height))
        .max(screen.y);
    let area = Rect::new(x, y, width, height);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(base(app))
        .border_style(secondary(app));
    if !title.is_empty() {
        block = block.title(format!(
            " {} ",
            crate::ui_workspaces::clipped(title, width.saturating_sub(8) as usize)
        ));
    }
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    app.workspaces.popup.area = area;
    if width >= 12 {
        let dismiss = Rect::new(area.right() - 4, area.y, 3, 1);
        f.render_widget(Paragraph::new(" × ").style(secondary(app)), dismiss);
        app.workspaces.popup.dismiss = dismiss;
    }
    Some(inner)
}

pub(crate) struct ListLayout {
    pub choices: Rect,
    pub actions: Rect,
    pub detail: Rect,
    pub row_height: u16,
}
pub(crate) fn list(
    f: &mut Frame,
    app: &mut App,
    saved: bool,
    count: usize,
    actions: usize,
    detail_rows: usize,
) -> Option<ListLayout> {
    let row_height = if !saved && f.area().height >= 12 {
        2
    } else {
        1
    };
    let inner = surface(
        f,
        app,
        "",
        count.clamp(1, 6) * row_height + actions + detail_rows + 5,
    )?;
    let mut top = inner.y;
    let mut space = inner.height;
    if space >= 4 {
        let mut x = inner.x;
        let labels = if inner.width >= 32 {
            [" Open folders ", " Saved workspaces "]
        } else if inner.width >= 21 {
            [" Folders ", " Workspaces "]
        } else {
            ["Folders ", "Workspaces"]
        };
        for (index, label) in labels.into_iter().enumerate() {
            let rect = Rect::new(
                x,
                top,
                (label.width() as u16).min(inner.right().saturating_sub(x)),
                1,
            );
            let active = saved == (index == 1);
            f.render_widget(
                Paragraph::new(label).style(if active {
                    selected(app).add_modifier(Modifier::BOLD)
                } else {
                    secondary(app)
                }),
                rect,
            );
            app.workspaces.popup.tabs[index] = rect;
            x = rect.right();
        }
        top += 1;
        space -= 1;
    }
    if space >= actions as u16 + 3 {
        let rect = Rect::new(inner.x + 1, top, inner.width.saturating_sub(2), 1);
        let search = &mut app.workspaces.popup.search;
        search.input = Some(
            crate::ui_widgets::InputField::new(&search.text)
                .prefix("", app.colors.fg_input)
                .cursor_pos(search.cursor)
                .selection_range(App::inline_selection_range(search.cursor, search.anchor))
                .cursor_marker("│")
                .placeholder_text(if saved {
                    "Find a saved workspace…"
                } else {
                    "Find an open folder…"
                })
                .theme_colors(&app.colors)
                .render(f, rect),
        );
        top += 1;
        space -= 1;
    }
    let detail = if space >= actions as u16 + detail_rows as u16 + 2 {
        detail_rows as u16
    } else {
        0
    };
    let separator = u16::from(space >= actions as u16 + detail + 2);
    // On tiny terminals, keep the selected action reachable instead of reserving
    // space for decoration or offscreen choices.
    let action_focus = app.workspaces.popup.selected >= count;
    let action_height =
        (actions as u16).min(space.saturating_sub(detail + separator + u16::from(!action_focus)));
    let choice_height = space.saturating_sub(action_height + detail + separator);
    let row_height = (row_height as u16).min(choice_height.max(1));
    let choice_height = choice_height / row_height * row_height;
    let choices = Rect::new(inner.x, top, inner.width, choice_height);
    let detail_area = Rect::new(inner.x, choices.bottom(), inner.width, detail);
    let action_y = detail_area.bottom() + separator;
    if separator > 0 {
        self::separator(
            f,
            app,
            Rect::new(inner.x, detail_area.bottom(), inner.width, 1),
        );
    }
    Some(ListLayout {
        choices,
        actions: Rect::new(inner.x, action_y, inner.width, action_height),
        detail: detail_area,
        row_height,
    })
}
pub(crate) fn separator(f: &mut Frame, app: &App, rect: Rect) {
    f.render_widget(
        Paragraph::new("─".repeat(rect.width as usize)).style(secondary(app)),
        rect,
    );
}

pub(crate) fn scroll(scroll: &mut usize, selected: usize, count: usize, visible: u16) -> usize {
    *scroll = (*scroll).min(count.saturating_sub(1));
    if selected < count {
        if selected < *scroll {
            *scroll = selected;
        }
        if selected >= *scroll + visible as usize {
            *scroll = (selected + 1).saturating_sub(visible as usize);
        }
    }
    *scroll
}
pub(crate) fn choice(
    f: &mut Frame,
    app: &App,
    rect: Rect,
    label: &str,
    badge: &str,
    active: bool,
    enabled: bool,
) {
    let style = if active && enabled {
        selected(app)
    } else if enabled {
        base(app)
    } else {
        secondary(app)
    };
    f.render_widget(Block::default().style(style), rect);
    let badge_width = badge.width().min(rect.width.saturating_sub(5) as usize);
    let label_width =
        (rect.width as usize).saturating_sub(badge_width + 2 + usize::from(badge_width > 0));
    let label = crate::ui_workspaces::clipped(label, label_width);
    let badge = crate::ui_workspaces::clipped(badge, badge_width);
    let pad = (rect.width as usize).saturating_sub(label.width() + badge.width() + 2);
    let detail = if active { style } else { secondary(app) };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {label}"), style),
            Span::raw(crate::helpers::spaces(pad)),
            Span::styled(badge, detail),
        ]))
        .style(style),
        rect,
    );
}
pub(crate) fn detail(f: &mut Frame, app: &App, rect: Rect, text: &str) {
    if rect.is_empty() {
        return;
    }
    f.render_widget(
        Paragraph::new(crate::ui_performance::clip_middle(
            &crate::ui_workspaces::escaped(text),
            rect.width.saturating_sub(2) as usize,
        ))
        .style(secondary(app)),
        Rect::new(
            rect.x + 1,
            rect.y,
            rect.width.saturating_sub(2),
            rect.height,
        ),
    );
}
pub(crate) fn subtitle(f: &mut Frame, app: &App, rect: Rect, text: &str, active: bool) {
    let style = if active {
        selected(app)
    } else {
        secondary(app)
    };
    f.render_widget(Block::default().style(style), rect);
    f.render_widget(
        Paragraph::new(text).style(style),
        Rect::new(
            rect.x + 3,
            rect.y,
            rect.width.saturating_sub(4),
            rect.height,
        ),
    );
}
/// Primary action is visually last. Return primary, cancel hit regions.
pub(crate) fn buttons(
    f: &mut Frame,
    app: &App,
    row: Rect,
    primary: &str,
    primary_selected: bool,
    cancel_selected: bool,
    enabled: bool,
) -> [Rect; 2] {
    if row.is_empty() {
        return [Rect::default(); 2];
    }
    let compact = (row.width as usize) < primary.width() + 15;
    let primary = if compact {
        match primary {
            "Save workspace" => "Save",
            "Open workspace" => "Open",
            "Update workspace" => "Update",
            "Delete workspace" => "Delete",
            "Close folder" => "Close",
            _ => primary,
        }
    } else {
        primary
    };
    let padding = if compact { 2 } else { 4 };
    let primary_width =
        ((primary.width() + padding) as u16).min(row.width.saturating_sub(1).max(1));
    let cancel_width = if compact { 8 } else { 10 };
    let gap = u16::from(row.width > primary_width + cancel_width);
    let cancel_width = cancel_width.min(row.width.saturating_sub(primary_width + gap));
    let primary_rect = Rect::new(row.right() - primary_width, row.y, primary_width, 1);
    let cancel_rect = Rect::new(primary_rect.x - gap - cancel_width, row.y, cancel_width, 1);
    for (rect, label, focused, primary) in [
        (cancel_rect, "Cancel", cancel_selected, false),
        (primary_rect, primary, primary_selected, true),
    ] {
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        let mut style = app.colors.ui.button_style(
            app.colors.bg_sidebar,
            primary,
            hovered,
            enabled || !primary,
        );
        if focused {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        let style = if primary {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        };
        f.render_widget(
            Paragraph::new(crate::ui_workspaces::clipped(
                &if compact {
                    format!("[{label}]")
                } else {
                    format!("[ {label} ]")
                },
                rect.width as usize,
            ))
            .style(style),
            rect,
        );
    }
    [primary_rect, cancel_rect]
}

#[cfg(test)]
#[path = "ui_workspace_picker_tests.rs"]
mod tests;
