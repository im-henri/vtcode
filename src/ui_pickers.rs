use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthChar;

use crate::app::App;
use crate::ui_widgets::{PopupDialog, PopupPosition};

/// Maximum visible items in the command palette and branch picker.
pub(crate) const PALETTE_MAX_ITEMS: u16 = 12;
/// Minimum dialog height for search/symbol dialogs.
pub(crate) const DIALOG_MIN_HEIGHT: u16 = 5;
/// Maximum items in the tab MRU switcher list.
const TAB_SWITCHER_MAX_ITEMS: usize = 12;
/// Minimum height for the tab MRU switcher (items area).
const TAB_SWITCHER_MIN_ITEMS: u16 = 6;
/// Truncate a string to fit within `max_width` display columns (width-aware).
pub(crate) fn truncate_to_width(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
        if w + cw > max_width {
            break;
        }
        result.push(ch);
        w += cw;
    }
    result
}

pub(crate) fn draw_git_branch_picker(f: &mut Frame, app: &mut App) {
    crate::ui_quick_picker::draw(f, app, crate::ui_quick_picker::Kind::Branches);
}

pub(crate) fn draw_command_palette(f: &mut Frame, app: &mut App) {
    if app.theme_install_prompt_visible() {
        crate::ui_modal::draw_active(f, app);
    } else {
        crate::ui_quick_picker::draw(f, app, crate::ui_quick_picker::Kind::Commands);
    }
}

pub(crate) fn draw_change_dir(f: &mut Frame, app: &mut App) {
    crate::ui_folder_browser::draw(f, app);
}

pub(crate) fn draw_symbol_list(f: &mut Frame, app: &mut App) {
    crate::ui_quick_picker::draw(f, app, crate::ui_quick_picker::Kind::Symbols);
}

pub(crate) fn draw_task_picker(f: &mut Frame, app: &mut App) {
    crate::ui_quick_picker::draw(f, app, crate::ui_quick_picker::Kind::Tasks);
}

pub(crate) fn draw_tab_switcher(f: &mut Frame, app: &App) {
    let area = f.area();
    let list_width = 40u16.min(area.width.saturating_sub(4));
    let preview_width = 50u16.min(area.width.saturating_sub(list_width + 6));
    let total_width = list_width + preview_width + 1;
    let max_items = app.tab_mru_order.len().min(TAB_SWITCHER_MAX_ITEMS) as u16;
    let height = max_items.max(TAB_SWITCHER_MIN_ITEMS) + 2;
    let y = 3u16.min(area.height.saturating_sub(height));

    let inner = PopupDialog::new("Open Editors")
        .border_color(app.colors.fg_accent)
        .bg(app.colors.bg_sidebar)
        .size(total_width, height)
        .position(PopupPosition::TopCenter(y))
        .render(f);

    let list_area = Rect::new(inner.x, inner.y, list_width.min(inner.width), inner.height);
    let offset = app.tab_switcher_selected.saturating_sub(usize::from(inner.height).saturating_sub(1))
        .min(app.tab_mru_order.len().saturating_sub(usize::from(inner.height)));
    let items: Vec<ListItem> = app
        .tab_mru_order
        .iter()
        .enumerate()
        .skip(offset)
        .take(inner.height as usize)
        .map(|(i, &tab_idx)| {
            let is_selected = i == app.tab_switcher_selected;
            let bg = if is_selected {
                app.colors.ui.bg_quick_selected
            } else {
                app.colors.bg_sidebar
            };
            let fg = if is_selected {
                app.colors.ui.fg_quick_selected
            } else {
                app.colors.ui.fg_quick_input
            };
            let style = Style::default().fg(fg).bg(bg);
            let line = if tab_idx < app.tabs.len() {
                let tab = &app.tabs[tab_idx];
                let ed = &tab.editor;
                let icon = crate::ui_dialogs::tab_icon(tab.kind, ed.file_name());
                let modified = if tab.has_unsaved_changes() { "● " } else { "" };
                let icon_style = if tab.kind == crate::app::TabKind::File {
                    style.fg(crate::ui_dialogs::file_icon_colored(ed.file_name()).1)
                } else { style };
                Line::from(vec![
                    Span::styled(if modified.is_empty() { "  " } else { "  ● " }, style),
                    Span::styled(icon, icon_style),
                    Span::styled(match tab.kind {
                        crate::app::TabKind::CommitDetails => std::borrow::Cow::Borrowed("Commit details"),
                        crate::app::TabKind::File => ed.display_name(),
                        _ => std::borrow::Cow::Borrowed(ed.file_name()),
                    }, style),
                ])
            } else {
                Line::from(Span::styled("  (unknown)", style))
            };
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).style(Style::default().bg(app.colors.bg_sidebar));
    f.render_widget(list, list_area);

    // Preview panel for selected tab
    if preview_width > 5 && list_width < inner.width {
        let preview_x = inner.x + list_width + 1;
        let preview_area = Rect::new(
            preview_x,
            inner.y,
            inner.width.saturating_sub(list_width + 1),
            inner.height,
        );
        // Draw separator
        let buf = f.buffer_mut();
        for y in inner.y..inner.y + inner.height {
            if let Some(cell) = buf.cell_mut((preview_x.saturating_sub(1), y)) {
                cell.set_char('│');
                cell.set_fg(app.colors.fg_dim);
            }
        }
        // Get preview content from selected tab
        if let Some(&tab_idx) = app.tab_mru_order.get(app.tab_switcher_selected) {
            if tab_idx < app.tabs.len() {
                let ed = &app.tabs[tab_idx].editor;
                let mut text = String::with_capacity(usize::from(preview_area.width));
                for (i, line) in ed.rope.lines().take(usize::from(preview_area.height)).enumerate() {
                    text.clear();
                    text.extend(line.chars().take(usize::from(preview_area.width)));
                    let y = preview_area.y + i as u16;
                    let para = Paragraph::new(Span::styled(
                        text.trim_end_matches(['\n', '\r']),
                        Style::default()
                            .fg(app.colors.fg_dim)
                            .bg(app.colors.bg_sidebar),
                    ));
                    f.render_widget(para, Rect::new(preview_area.x, y, preview_area.width, 1));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn truncate_to_width_keeps_ascii_and_wide_glyphs_within_the_cell_budget() {
        for (text, width, expected) in [
            ("hello world", 5, "hello"),
            ("abc", 3, "abc"),
            ("", 5, ""),
            ("abc", 0, ""),
            ("你好世界", 4, "你好"),
            ("你好", 3, "你"),
            ("hi", 100, "hi"),
        ] {
            assert_eq!(
                truncate_to_width(text, width),
                expected,
                "text={text:?}, width={width}"
            );
        }
    }

    fn input_field_text(text: &str, focused: bool, prefix: &str) -> String {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(20, 1)).unwrap();
        terminal.draw(|frame| {
            let area = frame.area();
            crate::ui_widgets::InputField::new(text)
                .prefix(prefix, Color::Reset)
                .placeholder_text(if focused { "" } else { "type here" })
                .cursor_marker("")
                .render(frame, area);
        }).unwrap();
        terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn input_field_empty_unfocused_shows_placeholder() {
        let text = input_field_text("", false, "");
        assert!(text.contains("type here"));
    }

    #[test]
    fn input_field_empty_focused_no_cursor() {
        let text = input_field_text("", true, "");
        assert!(!text.contains("type here"));
    }

    #[test]
    fn input_field_with_text() {
        let text = input_field_text("hello", false, "");
        assert!(text.contains("hello"));
    }

    #[test]
    fn input_field_with_label() {
        let text = input_field_text("world", false, "> ");
        assert!(text.starts_with("> "));
        assert!(text.contains("world"));
    }

}
