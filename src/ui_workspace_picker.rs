//! Shared geometry and input for the workspace switcher and saved sets.
use crate::{
    app::App,
    text_input::{TextInput, TextInputHistory, TextInputHitMap},
};
use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
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
    pub(crate) fn input_state(&mut self) -> (&mut String, &mut usize, &mut Option<usize>) {
        (&mut self.text, &mut self.cursor, &mut self.anchor)
    }

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
    let background = app
        .colors
        .ui
        .quick_background
        .over(app.colors.bg_editor)
        .unwrap_or(app.colors.bg_editor);
    Style::default()
        .bg(background)
        .fg(app.colors.ui.quick_text_color(background, false))
}
pub(crate) fn secondary(app: &App) -> Style {
    let background = base(app).bg.unwrap();
    base(app).fg(app.colors.ui.description_color(background))
}
pub(crate) fn selected(app: &App) -> Style {
    let background = app
        .colors
        .ui
        .quick_focus_background
        .over(base(app).bg.unwrap())
        .unwrap_or(base(app).bg.unwrap());
    base(app)
        .bg(background)
        .fg(app.colors.ui.quick_text_color(background, true))
}

fn backdrop(f: &mut Frame, app: &App, area: Rect) {
    let surface = app.colors.bg_editor;
    let fade = |color, foreground| {
        let color = if color == Color::Reset {
            if foreground {
                app.colors.fg_text
            } else {
                surface
            }
        } else {
            app.colors.ui.terminal_color(color, foreground)
        };
        crate::app_theme::lerp_color(color, surface, 0.76)
    };
    // Fade actual colors rather than relying on terminal-specific DIM handling.
    // Only the rendered frame changes; the workbench and its caches stay intact.
    for cell in &mut f.buffer_mut().content {
        if cell.modifier.contains(Modifier::REVERSED) {
            std::mem::swap(&mut cell.fg, &mut cell.bg);
        }
        cell.fg = fade(cell.fg, true);
        cell.bg = fade(cell.bg, false);
        cell.underline_color = fade(cell.underline_color, true);
        cell.modifier &= Modifier::HIDDEN;
    }
    let shadow = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width,
        area.height,
    )
    .intersection(f.area());
    for y in shadow.y..shadow.bottom() {
        for x in shadow.x..shadow.right() {
            if area.contains((x, y).into()) {
                continue;
            }
            let cell = &mut f.buffer_mut()[(x, y)];
            if let Some(background) = app.colors.ui.widget_shadow.border(cell.bg) {
                cell.reset();
                cell.set_bg(background).set_fg(background);
            }
        }
    }
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
    let width = screen
        .width
        .saturating_sub(u16::from(screen.width >= 16) * 4)
        .min(96);
    let height = (desired + usize::from(screen.height >= 12) * 2)
        .min(
            screen
                .height
                .saturating_sub(u16::from(screen.height >= 12) * 2) as usize,
        )
        .max(3) as u16;
    let x = screen.x + (screen.width - width) / 2;
    let y = screen.y + (screen.height - height) / 2;
    let area = Rect::new(x, y, width, height);
    backdrop(f, app, area);
    let background = base(app).bg.unwrap();
    let border = app
        .colors
        .ui
        .active_contrast_border
        .border(background)
        .or_else(|| app.colors.ui.focus_border.border(background))
        .or_else(|| app.colors.ui.widget_border.border(background))
        .unwrap_or(app.colors.fg_accent_blue);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(base(app))
        .border_style(base(app).fg(app.colors.ui.readable_foreground(border, background, 4.5)));
    let title = if title.is_empty() {
        "Workspaces"
    } else {
        title
    };
    block = block.title(Line::styled(
        format!(
            " {} ",
            crate::ui_workspaces::clipped(title, width.saturating_sub(8) as usize)
        ),
        base(app).add_modifier(Modifier::BOLD),
    ));
    let mut inner = block.inner(area);
    if width >= 40 {
        inner.x += 1;
        inner.width = inner.width.saturating_sub(2);
    }
    if height >= 12 {
        inner.y += 1;
        inner.height = inner.height.saturating_sub(2);
    }
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
    let row_height = if f.area().height >= 24 { 2 } else { 1 };
    let inner = surface(f, app, "", (count.clamp(1, 6) * row_height + 14).max(20))?;
    let mut top = inner.y;
    let mut space = inner.height;
    if space >= 8 {
        workspace_summary(f, app, Rect::new(inner.x, top, inner.width, 1));
        let note = if app.workspace_opening() {
            "Updating folders… You can keep working while they load."
        } else {
            match app.current_workspace_set() {
                Some((_, true)) => "Folder changes have not been saved to this workspace.",
                Some(_) => "Choose a folder or manage a saved workspace.",
                None => "Save this folder collection to reopen it together later.",
            }
        };
        let rows = if space >= 16 { 2 } else { 1 };
        if rows > 1 {
            detail(f, app, Rect::new(inner.x, top + 1, inner.width, 1), note);
        }
        top += rows;
        space -= rows;
    }
    if space >= 5 {
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
        let rows = if space >= 12 { 2 } else { 1 };
        top += rows;
        space -= rows;
    }
    if space >= 4 {
        let rect = Rect::new(inner.x + 1, top, inner.width.saturating_sub(2), 1);
        let input_foreground = secondary(app).fg.unwrap();
        let viewport = app.text_input_viewport(crate::text_input::TextInputId::WorkspaceFilter);
        let search = &mut app.workspaces.popup.search;
        search.input = Some(
            crate::ui_widgets::InputField::new(&search.text)
                .viewport(viewport)
                .prefix("⌕ ", input_foreground)
                .cursor_pos(search.cursor)
                .selection_range(App::inline_selection_range(search.cursor, search.anchor))
                .cursor_marker("│")
                .placeholder_text(if saved {
                    "Filter saved workspaces…"
                } else {
                    "Filter folders by name or path…"
                })
                .theme_colors(&app.colors)
                .render(f, rect),
        );
        let hit = search.input.clone().unwrap();
        app.register_text_input(crate::text_input::TextInputId::WorkspaceFilter, hit);
        let rows = if space >= 10 { 2 } else { 1 };
        top += rows;
        space -= rows;
    }
    let hint = u16::from(space >= 6);
    if hint > 0 {
        let hint_text = if inner.width < 24 {
            "↑↓ ↵ Esc"
        } else if inner.width < 40 {
            "↑↓ Move  Enter  Esc"
        } else if inner.width < 80 {
            if saved {
                "↑↓ Move  Enter Details  Ctrl+Tab Views"
            } else {
                "↑↓ Move  Enter Switch  Ctrl+Tab Views"
            }
        } else if saved {
            "↑↓ Select   Enter Details   Ctrl+Tab Views   Esc Close"
        } else {
            "↑↓ Select   Enter Switch   Delete Close folder   Ctrl+Tab Views   Esc Close"
        };
        self::detail(
            f,
            app,
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            hint_text,
        );
        space -= hint;
    }
    let detail = if space >= 7 {
        detail_rows.max(2) as u16
    } else {
        0
    };
    let separator = u16::from(space >= actions as u16 + detail + 2);
    // On tiny terminals, keep the selected action reachable instead of reserving
    // space for decoration or offscreen choices.
    let action_focus = app.workspaces.popup.selected >= count;
    let action_height = u16::from(actions > 0 && (space >= 2 || action_focus));
    let action_gap = u16::from(space > action_height + detail + separator + 1);
    let choice_height = space.saturating_sub(action_height + action_gap + detail + separator);
    let row_height = (row_height as u16).min(choice_height.max(1));
    let choice_height = choice_height / row_height * row_height;
    let choices = Rect::new(inner.x, top, inner.width, choice_height);
    let detail_area = Rect::new(inner.x, choices.bottom(), inner.width, detail);
    let action_y = detail_area.bottom() + separator + action_gap;
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

pub(crate) fn workspace_summary(f: &mut Frame, app: &App, row: Rect) {
    let (label, status) = match app.current_workspace_set() {
        Some((set, true)) => (set.name, "Unsaved changes"),
        Some((set, false)) => (set.name, "Saved"),
        None if !app.workspaces.sets.loaded && app.workspace_sets_busy() => {
            ("Current workspace".into(), "Loading…")
        }
        None if !app.workspaces.sets.loaded && app.workspaces.sets.error.is_some() => {
            ("Current workspace".into(), "Catalog unavailable")
        }
        None => ("Unsaved workspace".into(), "Not saved"),
    };
    if row.is_empty() {
        return;
    }
    f.render_widget(Block::default().style(base(app)), row);
    let count = app.open_folder_count();
    let badge = format!(
        "{status} · {count} folder{}",
        if count == 1 { "" } else { "s" }
    );
    let badge_width = badge.width().min(row.width.saturating_sub(4) as usize) as u16;
    if badge_width > 0 {
        let background = base(app).bg.unwrap();
        let color = if status == "Saved" {
            app.colors.fg_accent_blue
        } else {
            app.colors.fg_warning
        };
        let color = app.colors.ui.readable_foreground(color, background, 4.5);
        f.render_widget(
            Paragraph::new(crate::ui_workspaces::clipped(&badge, badge_width as usize))
                .style(base(app).fg(color)),
            Rect::new(row.right() - badge_width - 1, row.y, badge_width, 1),
        );
    }
    {
        let label = crate::ui_workspaces::clipped(
            &label,
            row.width.saturating_sub(badge_width + 3) as usize,
        );
        f.render_widget(
            Paragraph::new(format!(" {label}")).style(base(app).add_modifier(Modifier::BOLD)),
            Rect::new(row.x, row.y, (label.width() as u16 + 1).min(row.width), 1),
        );
    }
}

/// Commands have their own footer, separate from the selectable folder rows.
pub(crate) fn action_buttons(
    f: &mut Frame,
    app: &App,
    row: Rect,
    labels: &[&str],
    selected_index: usize,
    first_index: usize,
) -> Vec<(Rect, usize)> {
    if row.is_empty() || labels.is_empty() {
        return vec![];
    }
    let compact = labels.iter().map(|label| label.width() + 6).sum::<usize>() > row.width as usize;
    let mut x = row.x;
    let mut hits = vec![];
    for (index, label) in labels.iter().enumerate() {
        let label = if compact {
            match *label {
                "+ Add folder…" => "+ Add",
                "+ Open folder…" => "+ Open",
                "Save workspace…" | "+ Save workspace…" => "Save…",
                "Open workspace" => {
                    if row.width >= 40 {
                        "Open workspace"
                    } else {
                        "Open"
                    }
                }
                "Update from open folders…" => {
                    if row.width >= 80 {
                        "Update folders…"
                    } else {
                        "Update…"
                    }
                }
                "Rename workspace…" => "Rename…",
                "Delete workspace…" => "Delete…",
                other => other,
            }
        } else {
            label
        };
        let width = (label.width() as u16 + 4).min(row.right().saturating_sub(x));
        if width < 3 {
            continue;
        }
        let rect = Rect::new(x, row.y, width, 1);
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        let focused = selected_index == first_index + index;
        let mut style =
            app.colors
                .ui
                .button_style(base(app).bg.unwrap(), index == 0, hovered, true);
        if focused {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        f.render_widget(
            Paragraph::new(crate::ui_workspaces::clipped(
                &format!("[ {label} ]"),
                width as usize,
            ))
            .style(style),
            rect,
        );
        hits.push((rect, first_index + index));
        x = rect.right().saturating_add(2);
    }
    // A terminal narrower than the footer still exposes its focused command.
    if !hits.iter().any(|(_, index)| *index == selected_index)
        && selected_index >= first_index
        && let Some(label) = labels.get(selected_index - first_index)
    {
        let rect = Rect::new(row.x, row.y, row.width, 1);
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(crate::ui_workspaces::clipped(
                &format!("[ {label} ]"),
                row.width as usize,
            ))
            .style(
                app.colors
                    .ui
                    .button_style(base(app).bg.unwrap(), true, false, true)
                    .add_modifier(Modifier::UNDERLINED),
            ),
            rect,
        );
        return vec![(rect, selected_index)];
    }
    hits
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
            base(app).bg.unwrap(),
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
