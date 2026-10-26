//! Bounded prefix recognition, independent of the message body. Theme colors
//! are resolved once per paint; selection and search take precedence over ink.
use crate::app_theme::ThemeColors;
use ratatui::style::Color;

#[derive(Clone, Copy, Default)]
pub(super) struct Header {
    timestamp: usize,
    level_end: usize,
    severity: usize,
}

impl Header {
    pub fn parse(text: &str) -> Self {
        let bytes = text.as_bytes();
        if bytes.first() != Some(&b'[')
            || bytes.get(5) != Some(&b'-')
            || bytes.get(11) != Some(&b'T')
        {
            return Self::default();
        }
        let Some(end) = bytes.iter().take(32).position(|&byte| byte == b']') else {
            return Self::default();
        };
        let timestamp = end + 1;
        let rest = &text[timestamp..];
        let (level_end, severity) = if rest.starts_with(" [info]") {
            (timestamp + 7, 0)
        } else if rest.starts_with(" [warning]") {
            (timestamp + 10, 1)
        } else if rest.starts_with(" [error]") {
            (timestamp + 8, 2)
        } else {
            (timestamp, 0)
        };
        Self {
            timestamp,
            level_end,
            severity,
        }
    }
}

pub(super) struct Ink {
    text: Color,
    timestamp: Color,
    levels: [Color; 3],
}

impl Ink {
    pub fn new(colors: &ThemeColors, background: Color) -> Self {
        let readable = |color| colors.ui.readable_foreground(color, background, 4.5);
        Self {
            text: readable(colors.fg_text),
            timestamp: readable(
                colors
                    .ui
                    .output_timestamp
                    .over(background)
                    .unwrap_or(colors.fg_dim),
            ),
            levels: [
                colors.ui.fg_diagnostic_info,
                colors.ui.fg_diagnostic_warning,
                colors.ui.fg_diagnostic_error,
            ]
            .map(readable),
        }
    }
    pub fn foreground(&self, header: Header, column: usize) -> Color {
        if column < header.timestamp {
            self.timestamp
        } else if column < header.level_end {
            self.levels[header.severity]
        } else {
            self.text
        }
    }
}
