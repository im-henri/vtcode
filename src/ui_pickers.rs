use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::App;

/// Maximum visible items in the command palette and branch picker.
pub(crate) const PALETTE_MAX_ITEMS: u16 = 12;
/// Minimum dialog height for search/symbol dialogs.
pub(crate) const DIALOG_MIN_HEIGHT: u16 = 5;
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
