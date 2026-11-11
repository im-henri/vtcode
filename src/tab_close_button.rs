use super::App;
use crate::app::Tab;
use ratatui::{Frame, layout::Rect};

// Keep one cell between the label and the clickable × and its right padding.
pub(super) const PADDING: &str = "   ";
const WIDTH: usize = PADDING.len() - 1;

#[derive(Clone, Copy)]
pub(super) struct CloseButton {
    start: usize,
}

impl CloseButton {
    pub(super) fn for_tab(tab: &Tab, tab_width: usize) -> Option<Self> {
        (!tab.pinned).then(|| Self {
            start: tab_width - WIDTH,
        })
    }

    pub(super) fn rect(self, tab: Rect, clipped: usize) -> Rect {
        // Keep a selection cell visible and only expose a hit when the × fits.
        let Some(offset) = self.start.checked_sub(clipped) else {
            return Rect::default();
        };
        if offset == 0 || offset >= usize::from(tab.width) || tab.height == 0 {
            return Rect::default();
        }
        Rect::new(
            tab.x + offset as u16,
            tab.y,
            WIDTH.min(usize::from(tab.width) - offset) as u16,
            1,
        )
    }
}

pub(super) fn paint(frame: &mut Frame, app: &App, close: Rect) {
    if close.is_empty() {
        return;
    }
    let buffer = frame.buffer_mut();
    let glyph = &mut buffer[(close.x, close.y)];
    glyph.set_symbol("×");
    if app.mouse_position_known
        && close.contains((app.mouse_col, app.mouse_row).into())
        && app.dragging_tab.is_none()
    {
        let style = app
            .colors
            .ui
            .toolbar_style(glyph.fg, glyph.bg, true, false, false);
        buffer.set_style(close, style);
    }
}
