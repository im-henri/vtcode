//! Close controls share label cells, without changing tab widths on hover.
use super::App;
use crate::app::Tab;
use ratatui::{Frame, layout::Rect, style::Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
pub(super) struct CloseOverlay {
    start: usize,
    width: usize,
    gap: usize,
}

impl CloseOverlay {
    pub(super) fn for_tab(tab: &Tab, name: &str, tab_width: usize) -> Option<Self> {
        if tab.pinned {
            return None;
        }
        // Preserve very short names. Their ordinary trailing padding becomes
        // the close control, leaving the filename itself available to select.
        let name_width = name.width();
        let (width, gap) = if name_width <= 2 {
            (1, 0)
        } else {
            let mut glyphs = name
                .graphemes(true)
                .rev()
                .map(UnicodeWidthStr::width)
                .filter(|width| *width > 0);
            let last = glyphs.next().unwrap_or(1);
            let previous = glyphs.next().unwrap_or(0);
            // Leave a blank before the cross, clearing a whole grapheme while
            // retaining a readable part of even a very short filename.
            let gap = if name_width > last + previous {
                previous
            } else {
                0
            };
            (last + 1, gap)
        };
        Some(Self {
            start: tab_width.saturating_sub(width),
            width,
            gap,
        })
    }

    pub(super) fn gap_rect(self, tab: Rect, clipped: usize) -> Rect {
        let Some(offset) = self
            .start
            .checked_sub(self.gap)
            .and_then(|start| start.checked_sub(clipped))
        else {
            return Rect::default();
        };
        if self.gap == 0 || offset >= usize::from(tab.width) || tab.height == 0 {
            return Rect::default();
        }
        Rect::new(
            tab.x + offset as u16,
            tab.y,
            self.gap.min(usize::from(tab.width) - offset) as u16,
            1,
        )
    }

    pub(super) fn rect(self, tab: Rect, clipped: usize) -> Rect {
        // Never leave an invisible close target when its glyph is scrolled
        // off the left edge. The remaining label still selects the tab.
        let Some(offset) = self.start.checked_sub(clipped) else {
            return Rect::default();
        };
        if offset >= usize::from(tab.width) || tab.height == 0 {
            return Rect::default();
        }
        if offset == 0 && self.width >= usize::from(tab.width) {
            return Rect::default();
        }
        Rect::new(
            tab.x + offset as u16,
            tab.y,
            self.width.min(usize::from(tab.width) - offset) as u16,
            1,
        )
    }
}

pub(super) fn paint(frame: &mut Frame, app: &App, tab: Rect, close: Rect, gap: Rect, style: Style) {
    let pointer = (app.mouse_col, app.mouse_row).into();
    if !app.mouse_position_known
        || close.is_empty()
        || !tab.contains(pointer)
        || app.dragging_tab.is_some()
    {
        return;
    }
    for x in gap.x..gap.right() {
        frame.buffer_mut()[(x, gap.y)]
            .set_symbol(" ")
            .set_style(style);
    }
    let style = app.colors.ui.toolbar_style(
        style.fg.unwrap_or(app.colors.ui.fg_tab_active),
        style.bg.unwrap_or(app.colors.bg_tabbar),
        close.contains(pointer),
        false,
        false,
    );
    // Clear the entire overlapped grapheme, including wide/combined glyphs.
    for x in close.x..close.right() {
        frame.buffer_mut()[(x, close.y)]
            .set_symbol(" ")
            .set_style(style);
    }
    frame.buffer_mut()[(close.x, close.y)].set_symbol("×");
}
