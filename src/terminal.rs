//! Integrated terminal emulator with PTY support.

use ratatui::style::Color;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[path = "terminal_resize.rs"]
mod resize;

#[path = "terminal_selection.rs"]
mod selection;

#[path = "terminal_mouse.rs"]
mod mouse;

#[path = "terminal_row.rs"]
mod row;
pub(crate) use row::TermRow;

const MAX_SCROLLBACK_LINES: usize = 5000;

/// A single cell in the terminal grid.
#[derive(Clone, Debug, PartialEq)]
pub struct TermCell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub underline: bool,
    pub dim: bool,
    pub italic: bool,
    pub strikethrough: bool,
}

impl Default for TermCell {
    fn default() -> Self {
        Self { ch: ' ', fg: Color::Reset, bg: Color::Reset, bold: false, underline: false, dim: false, italic: false, strikethrough: false }
    }
}

/// Saved cursor state: (row, col, fg, bg, bold, underline, dim, italic, strikethrough, inverted).
pub(crate) type SavedCursorState = (usize, usize, Color, Color, bool, bool, bool, bool, bool, bool);

struct AltScreenState {
    cells: Vec<TermRow>,
    wrapped: Vec<bool>,
    scrollback: std::collections::VecDeque<TermRow>,
    scrollback_wrapped: std::collections::VecDeque<bool>,
    cursor_row: usize,
    cursor_col: usize,
    pending_wrap: bool,
    scroll_view: usize,
    width: usize,
    height: usize,
    saved_cursor: Option<SavedCursorState>,
}

enum ConptyResize {
    AwaitingPaint,
    Repainting {
        cells: Vec<TermRow>,
        wrapped: Vec<bool>,
        scrolled_rows: usize,
    },
}

/// Terminal grid state — handles VT escape sequences via `vte::Perform`.
pub struct TermGrid {
    pub cells: Vec<TermRow>,
    pub scrollback: std::collections::VecDeque<TermRow>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub width: usize,
    pub height: usize,
    pub scroll_view: usize, // 0 = live, >0 = viewing scrollback
    // ConPTY repaints the viewport on resize; keep history outside that repaint.
    is_conpty: bool,
    conpty_resize: Option<ConptyResize>,
    // True when this physical row continues on the next row without a newline.
    wrapped: Vec<bool>,
    scrollback_wrapped: std::collections::VecDeque<bool>,
    selection: Option<selection::Selection>,

    // Current text attributes
    fg: Color,
    bg: Color,
    bold: bool,
    underline: bool,
    dim: bool,
    italic: bool,
    strikethrough: bool,
    inverted: bool,

    // Scroll region
    scroll_top: usize,
    scroll_bottom: usize,

    // Modes
    pub cursor_visible: bool,
    pub cursor_shape: u8, // 0=default, 1=block blink, 2=block, 3=underline blink, 4=underline, 5=bar blink, 6=bar
    auto_wrap: bool,
    pub application_cursor_keys: bool,
    pub bracketed_paste: bool,
    pub win32_input_mode: bool,
    pub mouse_tracking: u16,
    sgr_mouse: bool,
    pub focus_reporting: bool,
    pending_wrap: bool,

    saved_cursor: Option<SavedCursorState>,

    alt_saved: Option<AltScreenState>,

    // Title set by OSC
    pub title: Option<String>,
    // The latest OSC 52 write is applied by the workbench, not by log parsers.
    pub(crate) clipboard_update: Option<String>,
}

impl TermGrid {
    pub(crate) fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![TermRow::from(vec![TermCell::default(); width]); height],
            scrollback: std::collections::VecDeque::new(),
            cursor_row: 0,
            cursor_col: 0,
            width,
            height,
            scroll_view: 0,
            is_conpty: false,
            conpty_resize: None,
            selection: None,
            wrapped: vec![false; height],
            scrollback_wrapped: std::collections::VecDeque::new(),
            fg: Color::Reset,
            bg: Color::Reset,
            bold: false,
            underline: false,
            dim: false,
            italic: false,
            strikethrough: false,
            inverted: false,
            scroll_top: 0,
            scroll_bottom: height.saturating_sub(1),
            cursor_visible: true,
            cursor_shape: 0,
            auto_wrap: true,
            application_cursor_keys: false,
            bracketed_paste: false,
            win32_input_mode: false,
            mouse_tracking: 0,
            sgr_mouse: false,
            focus_reporting: false,
            pending_wrap: false,
            saved_cursor: None,
            alt_saved: None,
            title: None,
            clipboard_update: None,
        }
    }

    fn new_row(&self) -> TermRow {
        vec![TermCell::default(); self.width].into()
    }

    fn put_char(&mut self, c: char) {
        if self.pending_wrap && self.auto_wrap {
            self.invalidate_selection_rows(self.cursor_row, self.cursor_row);
            self.wrapped[self.cursor_row] = true;
            self.cursor_col = 0;
            self.linefeed();
            self.pending_wrap = false;
        }
        if self.cursor_col < self.width && self.cursor_row < self.height {
            self.invalidate_selection_rows(self.cursor_row, self.cursor_row);
            self.cells[self.cursor_row][self.cursor_col] = TermCell {
                ch: c, fg: self.fg, bg: self.bg, bold: self.bold, underline: self.underline,
                dim: self.dim, italic: self.italic, strikethrough: self.strikethrough,
            };
            self.cursor_col += 1;
            if self.cursor_col >= self.width {
                if self.auto_wrap {
                    self.pending_wrap = true;
                }
                self.cursor_col = self.width - 1;
            }
        }
    }

    fn linefeed(&mut self) {
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor_row < self.height.saturating_sub(1) {
            self.cursor_row += 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        // Only rows leaving the top of the primary screen enter history.
        // A fixed footer is allowed; a region below a header must not be saved.
        let save_history = self.alt_saved.is_none() && self.scroll_top == 0;
        let full_screen = save_history && self.scroll_bottom == self.height.saturating_sub(1);
        if !full_screen {
            if save_history { self.clear_selection(); }
            else { self.invalidate_selection_rows(self.scroll_top, self.scroll_bottom); }
        }
        if self.scroll_top > self.scroll_bottom || self.scroll_bottom >= self.cells.len() {
            return;
        }
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        // Move row ownership once, rather than shifting the viewport twice for
        // every scrolled line. The displaced rows now occupy the region's tail.
        self.cells[self.scroll_top..=self.scroll_bottom].rotate_left(n);
        self.wrapped[self.scroll_top..=self.scroll_bottom].rotate_left(n);
        for index in self.scroll_bottom + 1 - n..=self.scroll_bottom {
            if save_history {
                self.scrollback.push_back(std::mem::take(&mut self.cells[index]));
                self.scrollback_wrapped.push_back(self.wrapped[index]);
                if full_screen && (self.scroll_view > 0 || self.has_selection()) {
                    self.scroll_view = (self.scroll_view + 1).min(self.scrollback.len());
                }
                // Once history is full, an evicted row already has the storage
                // needed for the new blank row. Preserve selection rebasing and
                // scroll anchoring in exactly the same order as history trimming.
                self.cells[index] = if self.scrollback.len() > MAX_SCROLLBACK_LINES {
                    let excess = self.scrollback.len() - MAX_SCROLLBACK_LINES;
                    self.trim_selection(excess);
                    let mut blank = self.scrollback.pop_front().unwrap();
                    self.scrollback.drain(..excess - 1);
                    self.scrollback_wrapped.drain(..excess);
                    self.scroll_view = self.scroll_view.min(self.scrollback.len());
                    blank.resize(self.width, TermCell::default());
                    blank.fill(TermCell::default());
                    blank
                } else {
                    self.scroll_view = self.scroll_view.min(self.scrollback.len());
                    self.new_row()
                };
            } else {
                self.cells[index].fill(TermCell::default());
            }
            self.wrapped[index] = false;
        }
        if save_history
            && let Some(ConptyResize::Repainting { scrolled_rows, .. }) = &mut self.conpty_resize
        {
            *scrolled_rows += n;
        }
    }

    fn scroll_down(&mut self, n: usize) {
        self.invalidate_selection_rows(self.scroll_top, self.scroll_bottom);
        if self.scroll_top > self.scroll_bottom || self.scroll_bottom >= self.cells.len() {
            return;
        }
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        self.cells[self.scroll_top..=self.scroll_bottom].rotate_right(n);
        self.wrapped[self.scroll_top..=self.scroll_bottom].rotate_right(n);
        for row in &mut self.cells[self.scroll_top..self.scroll_top + n] {
            row.fill(TermCell::default());
        }
        self.wrapped[self.scroll_top..self.scroll_top + n].fill(false);
    }

    fn erase_in_display(&mut self, mode: u16) {
        if mode == 2 || mode == 3 {
            self.conpty_resize = None;
        }
        // ED 3 erases saved lines, not the live viewport (ED 2). Rebase a
        // live-screen selection and discard one that intersects old history.
        if mode == 3 {
            self.trim_selection(self.scrollback.len());
            self.scrollback.clear();
            self.scrollback_wrapped.clear();
            self.scroll_view = 0;
            return;
        }
        match mode {
            0 => self.invalidate_selection_rows(self.cursor_row, self.height.saturating_sub(1)),
            1 => self.invalidate_selection_rows(0, self.cursor_row),
            2 => self.invalidate_selection_rows(0, self.height.saturating_sub(1)),
            _ => {}
        }
        match mode {
            0 => {
                self.erase_in_line(0);
                for row in (self.cursor_row + 1)..self.height {
                    if row < self.cells.len() { self.cells[row].fill(TermCell::default()); self.wrapped[row] = false; }
                }
            }
            1 => {
                for row in 0..self.cursor_row {
                    if row < self.cells.len() { self.cells[row].fill(TermCell::default()); self.wrapped[row] = false; }
                }
                self.erase_in_line(1);
            }
            2 => {
                for row in 0..self.height {
                    if row < self.cells.len() { self.cells[row].fill(TermCell::default()); self.wrapped[row] = false; }
                }
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        if self.cursor_row >= self.cells.len() { return; }
        if mode <= 2 { self.invalidate_selection_rows(self.cursor_row, self.cursor_row); }
        match mode {
            0 => {
                self.wrapped[self.cursor_row] = false;
                for col in self.cursor_col..self.width {
                    if col < self.cells[self.cursor_row].len() {
                        self.cells[self.cursor_row][col] = TermCell::default();
                    }
                }
            }
            1 => {
                for col in 0..=self.cursor_col.min(self.width.saturating_sub(1)) {
                    if col < self.cells[self.cursor_row].len() {
                        self.cells[self.cursor_row][col] = TermCell::default();
                    }
                }
            }
            2 => { self.cells[self.cursor_row].fill(TermCell::default()); self.wrapped[self.cursor_row] = false; }
            _ => {}
        }
    }

    fn set_sgr(&mut self, params: &[u16]) {
        if params.is_empty() { self.reset_attrs(); return; }
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => self.reset_attrs(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                7 => {
                    if !self.inverted {
                        std::mem::swap(&mut self.fg, &mut self.bg);
                        self.inverted = true;
                    }
                }
                9 => self.strikethrough = true,
                22 => { self.bold = false; self.dim = false; }
                23 => self.italic = false,
                24 => self.underline = false,
                27 => {
                    if self.inverted {
                        std::mem::swap(&mut self.fg, &mut self.bg);
                        self.inverted = false;
                    }
                }
                29 => self.strikethrough = false,
                30 => self.fg = Color::Black,
                31 => self.fg = Color::Red,
                32 => self.fg = Color::Green,
                33 => self.fg = Color::Yellow,
                34 => self.fg = Color::Blue,
                35 => self.fg = Color::Magenta,
                36 => self.fg = Color::Cyan,
                37 => self.fg = Color::Gray,
                38 => {
                    if i + 1 < params.len() {
                        if params[i + 1] == 5 && i + 2 < params.len() {
                            self.fg = Color::Indexed(params[i + 2] as u8);
                            i += 2;
                        } else if params[i + 1] == 2 && i + 4 < params.len() {
                            self.fg = Color::Rgb(params[i + 2] as u8, params[i + 3] as u8, params[i + 4] as u8);
                            i += 4;
                        }
                    }
                }
                39 => self.fg = Color::Reset,
                40 => self.bg = Color::Black,
                41 => self.bg = Color::Red,
                42 => self.bg = Color::Green,
                43 => self.bg = Color::Yellow,
                44 => self.bg = Color::Blue,
                45 => self.bg = Color::Magenta,
                46 => self.bg = Color::Cyan,
                47 => self.bg = Color::Gray,
                48 => {
                    if i + 1 < params.len() {
                        if params[i + 1] == 5 && i + 2 < params.len() {
                            self.bg = Color::Indexed(params[i + 2] as u8);
                            i += 2;
                        } else if params[i + 1] == 2 && i + 4 < params.len() {
                            self.bg = Color::Rgb(params[i + 2] as u8, params[i + 3] as u8, params[i + 4] as u8);
                            i += 4;
                        }
                    }
                }
                49 => self.bg = Color::Reset,
                90 => self.fg = Color::DarkGray,
                91 => self.fg = Color::LightRed,
                92 => self.fg = Color::LightGreen,
                93 => self.fg = Color::LightYellow,
                94 => self.fg = Color::LightBlue,
                95 => self.fg = Color::LightMagenta,
                96 => self.fg = Color::LightCyan,
                97 => self.fg = Color::White,
                100 => self.bg = Color::DarkGray,
                101 => self.bg = Color::LightRed,
                102 => self.bg = Color::LightGreen,
                103 => self.bg = Color::LightYellow,
                104 => self.bg = Color::LightBlue,
                105 => self.bg = Color::LightMagenta,
                106 => self.bg = Color::LightCyan,
                107 => self.bg = Color::White,
                _ => {}
            }
            i += 1;
        }
    }

    fn reset_attrs(&mut self) {
        self.fg = Color::Reset;
        self.bg = Color::Reset;
        self.bold = false;
        self.underline = false;
        self.dim = false;
        self.italic = false;
        self.strikethrough = false;
        self.inverted = false;
    }

    pub(crate) fn resize(&mut self, width: usize, height: usize) {
        if width == 0 || height == 0 || (width, height) == (self.width, self.height) { return; }
        self.clear_selection();
        if self.alt_saved.is_some() {
            // Full-screen applications redraw at fixed coordinates after SIGWINCH.
            // Their output must never be mixed into the shell's scrollback.
            for row in &mut self.cells {
                if row.len() != width { row.resize(width, TermCell::default()); }
            }
            self.cells.resize_with(height, || vec![TermCell::default(); width].into());
            self.wrapped = vec![false; height];
            self.pending_wrap = false;
        } else {
            self.resize_primary(width, height);
        }
        self.width = width;
        self.height = height;
        self.scroll_top = 0;
        self.scroll_bottom = height.saturating_sub(1);
        if self.cursor_row >= height { self.cursor_row = height - 1; }
        if self.cursor_col >= width { self.cursor_col = width - 1; }
        if let Some(saved) = &mut self.saved_cursor {
            saved.0 = saved.0.min(height - 1);
            saved.1 = saved.1.min(width - 1);
        }
    }

    pub(crate) fn is_alt_screen(&self) -> bool {
        self.alt_saved.is_some()
    }

    fn enter_alt_screen(&mut self) {
        if self.alt_saved.is_some() { return; }
        self.conpty_resize = None;
        self.clear_selection();
        self.alt_saved = Some(AltScreenState {
            cells: std::mem::replace(&mut self.cells, vec![TermRow::from(vec![TermCell::default(); self.width]); self.height]),
            wrapped: std::mem::replace(&mut self.wrapped, vec![false; self.height]),
            scrollback: std::mem::take(&mut self.scrollback),
            scrollback_wrapped: std::mem::take(&mut self.scrollback_wrapped),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            pending_wrap: self.pending_wrap,
            scroll_view: self.scroll_view,
            width: self.width,
            height: self.height,
            saved_cursor: self.saved_cursor.take(),
        });
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.pending_wrap = false;
        self.scroll_view = 0;
        self.scroll_top = 0;
        self.scroll_bottom = self.height.saturating_sub(1);
    }

    fn exit_alt_screen(&mut self) {
        if let Some(saved) = self.alt_saved.take() {
            self.clear_selection();
            let (width, height) = (self.width, self.height);
            self.cells = saved.cells;
            self.wrapped = saved.wrapped;
            self.scrollback = saved.scrollback;
            self.scrollback_wrapped = saved.scrollback_wrapped;
            self.cursor_row = saved.cursor_row;
            self.cursor_col = saved.cursor_col;
            self.pending_wrap = saved.pending_wrap;
            self.scroll_view = saved.scroll_view;
            self.saved_cursor = saved.saved_cursor;
            self.width = saved.width;
            self.height = saved.height;
            self.scroll_top = 0;
            self.scroll_bottom = saved.height.saturating_sub(1);
            self.resize(width, height);
        }
    }

    /// Get a displayable row, accounting for scrollback viewing offset.
    #[cfg(test)]
    pub(crate) fn display_line(&self, visual_row: usize) -> &[TermCell] {
        self.display_row(visual_row).map_or(&[], |row| row.as_slice())
    }

    pub(crate) fn display_row(&self, visual_row: usize) -> Option<&TermRow> {
        if self.scroll_view > 0 {
            let total = self.scrollback.len();
            let offset = total.saturating_sub(self.scroll_view);
            let idx = offset + visual_row;
            if idx < total {
                self.scrollback.get(idx)
            } else {
                let grid_row = idx - total;
                self.cells.get(grid_row)
            }
        } else {
            self.cells.get(visual_row)
        }
    }
}

impl vte::Perform for TermGrid {
    fn print(&mut self, c: char) {
        self.put_char(c);
        if self.selection.is_none() { self.scroll_view = 0; }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => { // BS
                if self.cursor_col > 0 { self.cursor_col -= 1; }
                self.pending_wrap = false;
            }
            0x09 => { // HT
                let next = (self.cursor_col / 8 + 1) * 8;
                self.cursor_col = next.min(self.width.saturating_sub(1));
                self.pending_wrap = false;
            }
            0x0A..=0x0C => { // LF, VT, FF
                self.linefeed();
                self.pending_wrap = false;
            }
            0x0D => { // CR
                self.cursor_col = 0;
                self.pending_wrap = false;
            }
            _ => {}
        }
        if self.selection.is_none() { self.scroll_view = 0; }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        // vte currently caps parameters (including subparameters) at 32.
        // Keep the fallback for future parser versions with a larger limit.
        let mut inline = [0u16; 32];
        let overflow: Vec<u16>;
        let ps = if params.len() <= inline.len() {
            for (slot, value) in inline.iter_mut().zip(params.iter().flatten()) { *slot = *value; }
            &inline[..params.len()]
        } else {
            overflow = params.iter().flatten().copied().collect();
            overflow.as_slice()
        };
        let p0 = ps.first().copied().unwrap_or(0);
        let p1 = if ps.len() > 1 { ps[1] } else { 0 };

        match action {
            'P' | 'X' | '@' => self.invalidate_selection_rows(self.cursor_row, self.cursor_row),
            _ => {}
        }
        match action {
            'A' => {
                let n = p0.max(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
                self.pending_wrap = false;
            }
            'B' | 'e' => {
                let n = p0.max(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.height.saturating_sub(1));
                self.pending_wrap = false;
            }
            'C' | 'a' => {
                let n = p0.max(1) as usize;
                self.cursor_col = (self.cursor_col + n).min(self.width.saturating_sub(1));
                self.pending_wrap = false;
            }
            'D' => {
                let n = p0.max(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
                self.pending_wrap = false;
            }
            'E' => {
                let n = p0.max(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.height.saturating_sub(1));
                self.cursor_col = 0;
                self.pending_wrap = false;
            }
            'F' => {
                let n = p0.max(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
                self.cursor_col = 0;
                self.pending_wrap = false;
            }
            'G' | '`' => {
                let col = (p0.max(1) as usize).saturating_sub(1);
                self.cursor_col = col.min(self.width.saturating_sub(1));
                self.pending_wrap = false;
            }
            'H' | 'f' => {
                let row = (p0.max(1) as usize).saturating_sub(1);
                let col = (p1.max(1) as usize).saturating_sub(1);
                if matches!(self.conpty_resize, Some(ConptyResize::AwaitingPaint)) {
                    // ConPTY brackets a full repaint with cursor hide/show and
                    // starts at home. Incremental cursor moves need no recovery.
                    self.conpty_resize = if row == 0 && col == 0 && !self.cursor_visible {
                        Some(ConptyResize::Repainting {
                            cells: self.cells.clone(),
                            wrapped: self.wrapped.clone(),
                            scrolled_rows: 0,
                        })
                    } else {
                        None
                    };
                }
                self.cursor_row = row.min(self.height.saturating_sub(1));
                self.cursor_col = col.min(self.width.saturating_sub(1));
                self.pending_wrap = false;
            }
            'J' => self.erase_in_display(p0),
            'K' => self.erase_in_line(p0),
            'L' | 'M' => {
                // IL/DL outside the scrolling region are no-ops. Restrict both
                // the mutation and selection invalidation to that region.
                if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor_row)
                    && self.scroll_bottom < self.cells.len() {
                    self.invalidate_selection_rows(self.cursor_row, self.scroll_bottom);
                    let rows = &mut self.cells[self.cursor_row..=self.scroll_bottom];
                    let wrapped = &mut self.wrapped[self.cursor_row..=self.scroll_bottom];
                    let n = (p0.max(1) as usize).min(rows.len());
                    let blank = if action == 'L' {
                        rows.rotate_right(n);
                        wrapped.rotate_right(n);
                        0..n
                    } else {
                        rows.rotate_left(n);
                        wrapped.rotate_left(n);
                        rows.len() - n..rows.len()
                    };
                    for row in &mut rows[blank.clone()] { row.fill(TermCell::default()); }
                    wrapped[blank].fill(false);
                }
            }
            'P' => {
                let n = p0.max(1) as usize;
                if let Some(tail) = self.cells.get_mut(self.cursor_row)
                    .and_then(|row| row.get_mut(self.cursor_col..)) {
                    let n = n.min(tail.len());
                    tail.rotate_left(n);
                    let kept = tail.len() - n;
                    tail[kept..].fill(TermCell::default());
                }
            }
            'S' => self.scroll_up(p0.max(1) as usize),
            'T' => self.scroll_down(p0.max(1) as usize),
            'X' => {
                let n = p0.max(1) as usize;
                if let Some(tail) = self.cells.get_mut(self.cursor_row)
                    .and_then(|row| row.get_mut(self.cursor_col..)) {
                    let n = n.min(tail.len());
                    tail[..n].fill(TermCell::default());
                }
            }
            'd' => {
                let row = (p0.max(1) as usize).saturating_sub(1);
                self.cursor_row = row.min(self.height.saturating_sub(1));
                self.pending_wrap = false;
            }
            'h' => {
                if intermediates.first() == Some(&b'?') {
                    for mode in ps {
                        match mode {
                            1 => self.application_cursor_keys = true,
                            7 => self.auto_wrap = true,
                            25 => {
                                self.finish_conpty_resize();
                                self.cursor_visible = true;
                            }
                            47 | 1047 | 1049 => self.enter_alt_screen(),
                            1000 | 1002 | 1003 => self.mouse_tracking = *mode,
                            1004 => self.focus_reporting = true,
                            1006 => self.sgr_mouse = true,
                            2004 => self.bracketed_paste = true,
                            9001 => self.win32_input_mode = true,
                            _ => {}
                        }
                    }
                }
            }
            'l' => {
                if intermediates.first() == Some(&b'?') {
                    for mode in ps {
                        match mode {
                            1 => self.application_cursor_keys = false,
                            7 => self.auto_wrap = false,
                            25 => self.cursor_visible = false,
                            47 | 1047 | 1049 => self.exit_alt_screen(),
                            1000 | 1002 | 1003 if self.mouse_tracking == *mode => {
                                self.mouse_tracking = 0;
                            }
                            1004 => self.focus_reporting = false,
                            1006 => self.sgr_mouse = false,
                            2004 => self.bracketed_paste = false,
                            9001 => self.win32_input_mode = false,
                            _ => {}
                        }
                    }
                }
            }
            'm' => self.set_sgr(ps),
            'r' => {
                let top = (p0.max(1) as usize).saturating_sub(1);
                let bot = if p1 > 0 { (p1 as usize).saturating_sub(1) } else { self.height.saturating_sub(1) };
                self.scroll_top = top.min(self.height.saturating_sub(1));
                self.scroll_bottom = bot.min(self.height.saturating_sub(1));
                if self.scroll_top > self.scroll_bottom {
                    std::mem::swap(&mut self.scroll_top, &mut self.scroll_bottom);
                }
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            's' if intermediates.is_empty() => {
                self.saved_cursor = Some((self.cursor_row, self.cursor_col, self.fg, self.bg, self.bold, self.underline, self.dim, self.italic, self.strikethrough, self.inverted));
            }
            'u' if intermediates.is_empty() => {
                if let Some((r, c, fg, bg, bold, ul, dim, italic, strikethrough, inverted)) = self.saved_cursor {
                    self.cursor_row = r;
                    self.cursor_col = c;
                    self.fg = fg;
                    self.bg = bg;
                    self.bold = bold;
                    self.underline = ul;
                    self.dim = dim;
                    self.italic = italic;
                    self.strikethrough = strikethrough;
                    self.inverted = inverted;
                }
            }
            '@' => {
                let n = p0.max(1) as usize;
                if let Some(tail) = self.cells.get_mut(self.cursor_row)
                    .and_then(|row| row.get_mut(self.cursor_col..)) {
                    let n = n.min(tail.len());
                    tail.rotate_right(n);
                    tail[..n].fill(TermCell::default());
                }
            }
            'q' if intermediates == [b' '] => {
                // DECSCUSR: Set Cursor Style
                self.cursor_shape = p0 as u8;
            }
            _ => {}
        }
        if self.selection.is_none() { self.scroll_view = 0; }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => {
                self.saved_cursor = Some((self.cursor_row, self.cursor_col, self.fg, self.bg, self.bold, self.underline, self.dim, self.italic, self.strikethrough, self.inverted));
            }
            b'8' => {
                if let Some((r, c, fg, bg, bold, ul, dim, italic, strikethrough, inverted)) = self.saved_cursor {
                    self.cursor_row = r;
                    self.cursor_col = c;
                    self.fg = fg;
                    self.bg = bg;
                    self.bold = bold;
                    self.underline = ul;
                    self.dim = dim;
                    self.italic = italic;
                    self.strikethrough = strikethrough;
                    self.inverted = inverted;
                }
            }
            b'D' => self.linefeed(),
            b'E' => { self.cursor_col = 0; self.linefeed(); }
            b'M' => {
                if self.cursor_row == self.scroll_top {
                    self.scroll_down(1);
                } else if self.cursor_row > self.scroll_top {
                    self.cursor_row -= 1;
                }
            }
            b'c' => {
                let is_conpty = self.is_conpty;
                let clipboard_update = self.clipboard_update.take();
                *self = TermGrid::new(self.width, self.height);
                self.is_conpty = is_conpty;
                self.clipboard_update = clipboard_update;
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let Some(text) = crate::clipboard::decode_osc52(params) {
            self.clipboard_update = Some(text);
        }
        if params.len() >= 2 && (params[0] == b"0" || params[0] == b"2") {
            if let Ok(title) = std::str::from_utf8(params[1]) {
                self.title = Some(title.to_string());
            }
        }
        // OSC 1: icon title (treat same as window title)
        if params.len() >= 2 && params[0] == b"1" {
            if let Ok(title) = std::str::from_utf8(params[1]) {
                self.title = Some(title.to_string());
            }
        }
    }

    fn hook(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
}

// ===== TerminalEmulator =====

const MAX_PTY_BYTES_PER_TICK: usize = 64 * 1024;
const MAX_PTY_PENDING_BYTES: usize = 10 * 1024 * 1024;
const PTY_PARSE_QUANTUM: usize = 256;
const PTY_PROCESS_BUDGET: Duration = Duration::from_millis(2);

fn queue_output(incoming: &mut VecDeque<u8>, bytes: &[u8]) {
    let bytes = &bytes[bytes.len().saturating_sub(MAX_PTY_PENDING_BYTES)..];
    let discard = (incoming.len() + bytes.len()).saturating_sub(MAX_PTY_PENDING_BYTES);
    incoming.drain(..discard);
    incoming.extend(bytes.iter().copied());
}

fn publish_output(
    incoming: &Mutex<VecDeque<u8>>,
    ready: &AtomicBool,
    wake: &crate::background::UiWake,
    bytes: &[u8],
) {
    if bytes.is_empty() { return; }
    let notify = if let Ok(mut queue) = incoming.lock() {
        let was_empty = queue.is_empty();
        queue_output(&mut queue, bytes);
        // Publish readiness under the same lock as the consumer's reset, so
        // newly arriving output cannot lose its wakeup during a drain.
        ready.store(true, Ordering::Release);
        was_empty
    } else { false };
    if notify { wake.notify(); }
}

pub struct TerminalEmulator {
    pub grid: TermGrid,
    pub(crate) render_rows: std::cell::RefCell<crate::ui_terminal_rows::RowCache>,
    /// Human-readable name of the shell/profile running in this PTY.
    pub profile_name: String,
    parser: vte::Parser,
    incoming: Arc<Mutex<VecDeque<u8>>>,
    incoming_ready: Arc<AtomicBool>,
    output_batch: Vec<u8>,
    output_offset: usize,
    writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    _alive: Arc<std::sync::atomic::AtomicBool>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
    child_killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    exit_events: mpsc::Receiver<()>,
    _wait_thread: Option<std::thread::JoinHandle<()>>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    last_size: (usize, usize),
    wake: crate::background::UiWake,
    remote: Option<(crate::remote::process::Reader, crate::remote::process::Killer)>,
}

impl TerminalEmulator {
    #[cfg(test)]
    pub(crate) fn test_queue_output(&self, bytes: &[u8]) {
        publish_output(&self.incoming, &self.incoming_ready, &self.wake, bytes);
    }

    #[cfg(test)]
    pub(crate) fn test_keep_running(&mut self) -> mpsc::Sender<()> {
        let (sender, receiver) = mpsc::channel();
        self.exit_events = receiver;
        sender
    }

    #[cfg(test)]
    pub(crate) fn test_capture_input(&mut self) -> Arc<Mutex<Vec<u8>>> {
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        *self.writer.lock().unwrap() = Some(Box::new(Capture(bytes.clone())));
        bytes
    }
}

impl TerminalEmulator {
    #[cfg(test)]
    pub(crate) fn test_with_output(width: usize, height: usize, output: &str) -> Self {
        let (_, exit_events) = mpsc::channel();
        let mut terminal = Self {
            grid: TermGrid::new(width, height),
            render_rows: Default::default(),
            profile_name: "Log fixture".into(),
            parser: vte::Parser::new(),
            incoming: Arc::new(Mutex::new(output.bytes().collect())),
            incoming_ready: Arc::new(AtomicBool::new(!output.is_empty())),
            output_batch: Vec::new(),
            output_offset: 0,
            writer: Arc::new(Mutex::new(None)),
            _alive: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            _reader_thread: None,
            child_killer: None,
            exit_events,
            _wait_thread: None,
            master: None,
            last_size: (width, height),
            wake: crate::background::UiWake::default(),
            remote: None,
        };
        terminal.process_while(|| true);
        terminal
    }

    pub(crate) fn from_remote(process: crate::remote::process::RemoteProcess, width: usize, height: usize, wake: crate::background::UiWake) -> std::io::Result<Self> {
        let (reader, writer, killer) = process.split();
        let writer = crate::remote::process::QueuedWriter::new(writer)?;
        let (_, exit_events) = mpsc::channel();
        Ok(Self {
            grid: TermGrid::new(width.max(2), height.max(2)), render_rows: Default::default(), profile_name: "Remote terminal".into(), parser: vte::Parser::new(),
            incoming: Arc::default(), incoming_ready: Arc::new(AtomicBool::new(false)), output_batch: Vec::new(), output_offset: 0,
            writer: Arc::new(Mutex::new(Some(Box::new(writer)))), _alive: Arc::new(AtomicBool::new(true)),
            _reader_thread: None, child_killer: None, exit_events, _wait_thread: None, master: None,
            last_size: (width, height), wake, remote: Some((reader, killer)),
        })
    }

    /// Spawn the first available platform shell in `cwd`.
    ///
    /// VS Code uses a real shell profile rather than emulating a prompt.  Try the
    /// modern platform shell first and gracefully fall back to the system shell.
    pub(crate) fn spawn_in_dir_with_wake(
        width: usize,
        height: usize,
        cwd: &Path,
        wake: crate::background::UiWake,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut candidates: Vec<(String, String)> = Vec::new();

        #[cfg(target_os = "windows")]
        {
            if let Ok(shell) = std::env::var("SHELL") {
                if !shell.trim().is_empty() {
                    candidates.push((shell.clone(), shell_profile_name(&shell)));
                }
            }
            candidates.push(("pwsh.exe".into(), "PowerShell".into()));
            candidates.push(("powershell.exe".into(), "PowerShell".into()));
        }

        #[cfg(not(target_os = "windows"))]
        {
            if let Ok(shell) = std::env::var("SHELL") {
                if !shell.trim().is_empty() {
                    candidates.push((shell.clone(), shell_profile_name(&shell)));
                }
            }
            candidates.push(("bash".into(), "Bash".into()));
            candidates.push(("sh".into(), "Shell".into()));
        }

        let mut attempted: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for (program, profile_name) in candidates {
            let dedupe_key = program.to_ascii_lowercase();
            if attempted.iter().any(|item| item == &dedupe_key) {
                continue;
            }
            attempted.push(dedupe_key);
            match Self::spawn_program(
                &program,
                &[],
                width,
                height,
                cwd,
                profile_name,
                wake.clone(),
            ) {
                Ok(terminal) => return Ok(terminal),
                Err(error) => errors.push(format!("{program}: {error}")),
            }
        }

        Err(format!("no usable shell was found ({})", errors.join("; ")).into())
    }

    /// Spawn a custom command inside a PTY.
    #[cfg(test)]
    pub(crate) fn spawn_command_in_dir(
        program: &str,
        args: &[String],
        width: usize,
        height: usize,
        cwd: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::spawn_command_in_dir_with_wake(
            program,
            args,
            width,
            height,
            cwd,
            crate::background::UiWake::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn spawn_command_in_dir_with_wake(
        program: &str,
        args: &[String],
        width: usize,
        height: usize,
        cwd: &Path,
        wake: crate::background::UiWake,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::spawn_program(
            program,
            args,
            width,
            height,
            cwd,
            shell_profile_name(program),
            wake,
        )
    }

    fn spawn_program(
        program: &str,
        args: &[String],
        width: usize,
        height: usize,
        cwd: &Path,
        profile_name: String,
        wake: crate::background::UiWake,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system.openpty(portable_pty::PtySize {
            rows: height.max(2) as u16,
            cols: width.max(2) as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = portable_pty::CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        let launch_cwd = if cwd.is_dir() {
            cwd.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };
        cmd.cwd(launch_cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "vtcode");

        let mut child = pair.slave.spawn_command(cmd)?;
        let child_killer = child.clone_killer();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let incoming = Arc::new(Mutex::new(VecDeque::new()));
        let incoming_ready = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let inc = incoming.clone();
        let ready = incoming_ready.clone();
        let alv = alive.clone();
        let reader_wake = wake.clone();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        publish_output(&inc, &ready, &reader_wake, &buf[..n]);
                    }
                }
            }
            alv.store(false, std::sync::atomic::Ordering::Relaxed);
            reader_wake.notify();
        });

        // `wait` blocks efficiently in a worker and sends exactly one lifecycle
        // event. The UI never probes the child with `try_wait`.
        let (exit_sender, exit_events) = mpsc::channel();
        let exit_wake = wake.clone();
        let wait_thread = std::thread::spawn(move || {
            if child.wait().is_ok() {
                let _ = exit_sender.send(());
                exit_wake.notify();
            }
        });

        let mut grid = TermGrid::new(width, height);
        grid.is_conpty = cfg!(windows);
        Ok(Self {
            grid,
            render_rows: Default::default(),
            profile_name,
            parser: vte::Parser::new(),
            incoming,
            incoming_ready,
            output_batch: Vec::new(),
            output_offset: 0,
            writer: Arc::new(Mutex::new(Some(writer) as Option<Box<dyn Write + Send>>)),
            _alive: alive,
            _reader_thread: Some(reader_thread),
            child_killer: Some(child_killer),
            exit_events,
            _wait_thread: Some(wait_thread),
            master: Some(pair.master),
            remote: None,
            last_size: (width, height),
            wake,
        })
    }

    /// Process pending output from the PTY. Returns true if new data was processed.
    /// Bound parsing time as well as bytes: escape sequences can do much more
    /// work per byte than ordinary text, especially on a slower CPU.
    pub(crate) fn process(&mut self) -> bool {
        if !self.has_pending_output() { return false; }
        self.process_until(Instant::now() + PTY_PROCESS_BUDGET)
    }

    pub(crate) fn has_pending_output(&self) -> bool {
        self.output_offset < self.output_batch.len() || self.incoming_ready.load(Ordering::Acquire)
            || self.remote.as_ref().is_some_and(|(reader, _)| self._alive.load(Ordering::Relaxed) && reader.ready())
    }

    pub(crate) fn process_until(&mut self, deadline: Instant) -> bool {
        self.process_while(|| Instant::now() < deadline)
    }

    /// Yield between small parser batches, preserving incomplete UTF-8/escape
    /// sequences and leaving older output ahead of newly arriving bytes.
    pub(crate) fn process_while(&mut self, mut continue_processing: impl FnMut() -> bool) -> bool {
        if !self.has_pending_output() { return false; }
        if self.output_offset == self.output_batch.len() {
            self.output_batch.clear();
            self.output_offset = 0;
            if let Some((reader, _)) = &mut self.remote {
                let mut bytes = [0; 4096];
                while self.output_batch.len() < MAX_PTY_BYTES_PER_TICK {
                    match reader.try_read(&mut bytes) {
                        Ok(0) => { self._alive.store(false, Ordering::Relaxed); break; }
                        Ok(count) => self.output_batch.extend_from_slice(&bytes[..count]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            self.output_batch.extend_from_slice(format!("\r\nVTCode: {error}\r\n").as_bytes());
                            self._alive.store(false, Ordering::Relaxed); break;
                        }
                    }
                }
            }
            if let Ok(mut queue) = self.incoming.lock() {
                let take = queue.len().min(MAX_PTY_BYTES_PER_TICK.saturating_sub(self.output_batch.len()));
                // A ring has at most two contiguous spans. Copy them in bulk:
                // extending from Drain copies byte by byte on this hot path.
                self.output_batch.reserve(take);
                let (first, second) = queue.as_slices();
                let leading = first.len().min(take);
                self.output_batch.extend_from_slice(&first[..leading]);
                self.output_batch.extend_from_slice(&second[..take - leading]);
                queue.drain(..take);
                self.incoming_ready.store(!queue.is_empty(), Ordering::Release);
            }
        }
        if self.output_batch.is_empty() { return false; }
        loop {
            let end = (self.output_offset + PTY_PARSE_QUANTUM).min(self.output_batch.len());
            for &byte in &self.output_batch[self.output_offset..end] {
                self.parser.advance(&mut self.grid, byte);
            }
            self.output_offset = end;
            if end == self.output_batch.len() || !continue_processing() { break; }
        }
        // Unparsed bytes stay in this bounded buffer, ahead of newer input.
        // The UI must wake again even if the reader has already gone quiet.
        if self.has_pending_output() { self.wake.notify(); }
        true
    }

    /// Consume the one-shot exit event emitted by the blocking child waiter.
    /// Once received, detach all live PTY handles so cleanup remains harmless.
    pub(crate) fn take_exit_event(&mut self) -> bool {
        if self.remote.is_some() {
            if self._alive.load(Ordering::Relaxed) || self.has_pending_output() { return false; }
            self.remote.take();
            if let Ok(mut writer) = self.writer.lock() { writer.take(); }
            return true;
        }
        match self.exit_events.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {}
            Err(mpsc::TryRecvError::Empty) => return false,
        }
        self.child_killer.take();
        if let Ok(mut writer) = self.writer.lock() {
            writer.take();
        }
        self.master.take();
        self._alive
            .store(false, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// Write bytes to the PTY (keyboard input).
    pub(crate) fn write(&self, data: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            if let Some(ref mut wr) = *w {
                if let Err(error) = wr.write_all(data) {
                    if let Some((_, remote)) = &self.remote {
                        publish_output(&self.incoming, &self.incoming_ready, &self.wake, format!("\r\nVTCode: input was not sent: {error}\r\n").as_bytes());
                        remote.kill();
                    }
                }
                let _ = wr.flush(); // Error OK: PTY flush is best-effort
            }
        }
    }

    /// Resize the terminal.
    pub(crate) fn resize(&mut self, width: usize, height: usize) {
        if width == 0 || height == 0 { return; }
        if (width, height) == self.last_size { return; }
        // Queued output belongs to the old viewport: a trailing line feed
        // may need to archive its top row before the resize repaint arrives.
        self.process();
        // Preserve the per-tick processing limit. Retry on the next frame
        // rather than resizing while old output is still waiting to scroll.
        if self.has_pending_output() {
            return;
        }
        let size = portable_pty::PtySize {
            rows: height as u16,
            cols: width as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        self.last_size = (width, height);
        self.grid.resize(width, height);
        self.grid.conpty_resize = (self.grid.is_conpty
            && self.grid.alt_saved.is_none()
            && self.grid.cursor_visible)
            .then_some(ConptyResize::AwaitingPaint);
        if let Some(ref master) = self.master {
            // A single request avoids redraws at an intermediate geometry.
            // Error OK: PTY resize is best-effort
            let _ = master.resize(size);
        }
        if let Some((_, remote)) = &self.remote {
            let _ = remote.client.notify(crate::remote::Command::ProcessResize { process: remote.id, width: width as u16, height: height as u16 });
        }
    }

    pub(crate) fn scroll_up(&mut self, n: usize) {
        let max = self.grid.scrollback.len();
        self.grid.scroll_view = (self.grid.scroll_view + n).min(max);
    }

    pub(crate) fn scroll_down(&mut self, n: usize) {
        self.grid.scroll_view = self.grid.scroll_view.saturating_sub(n);
    }
}

impl Drop for TerminalEmulator {
    fn drop(&mut self) {
        self._alive
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut writer) = self.writer.lock() {
            writer.take();
        }
        if let Some(mut child_killer) = self.child_killer.take() {
            let _ = child_killer.kill();
        }
    }
}

fn shell_profile_name(program: &str) -> String {
    let file_name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program);
    let stem = file_name
        .strip_suffix(".exe")
        .unwrap_or(file_name)
        .to_ascii_lowercase();
    match stem.as_str() {
        "pwsh" | "powershell" => "PowerShell".into(),
        "cmd" => "Command Prompt".into(),
        "bash" => "Bash".into(),
        "zsh" => "Zsh".into(),
        "fish" => "Fish".into(),
        "sh" => "Shell".into(),
        _ => file_name.to_string(),
    }
}

#[cfg(test)]
pub(crate) fn test_child_command() -> (String, Vec<String>) {
    (
        std::env::current_exe().unwrap().to_string_lossy().into_owned(),
        ["--ignored", "--exact", "terminal::tests::pty_test_child", "--nocapture"]
            .into_iter().map(String::from).collect(),
    )
}

#[cfg(test)]
#[path = "terminal_resize_tests.rs"]
mod resize_tests;

#[cfg(test)]
#[path = "terminal_selection_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "terminal_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
#[path = "terminal_responsiveness_tests.rs"]
mod responsiveness_tests;

#[cfg(test)]
#[path = "terminal_scroll_performance_tests.rs"]
mod scroll_performance_tests;

#[cfg(test)]
#[path = "terminal_edge_case_tests.rs"]
mod edge_case_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "subprocess fixture for PTY tests"]
    fn pty_test_child() {
        println!("VTCODE_PTY_OK");
        std::io::stdout().flush().unwrap();
        std::process::exit(7);
    }

    #[test]
    fn shell_profile_names_match_common_vscode_profiles() {
        assert_eq!(shell_profile_name("pwsh.exe"), "PowerShell");
        assert_eq!(shell_profile_name("C:\\Windows\\System32\\cmd.exe"), "Command Prompt");
        assert_eq!(shell_profile_name("/bin/bash"), "Bash");
    }

    #[test]
    fn real_pty_delivers_output_then_detaches_safely_after_exit() {
        let (program, args) = crate::terminal::test_child_command();

        let cwd = std::env::current_dir().expect("current directory");
        let mut terminal = TerminalEmulator::spawn_command_in_dir(
            &program,
            &args,
            80,
            24,
            &cwd,
        )
        .expect("spawn PTY command");
        let mut found = false;
        for _ in 0..100 {
            terminal.process();
            let screen: String = terminal
                .grid
                .cells
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect();
            if screen.contains("VTCODE_PTY_OK") {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(found, "PTY output should be parsed into the terminal grid");
        let mut exited = false;
        for _ in 0..100 {
            terminal.process();
            if terminal.take_exit_event() {
                exited = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(exited);
        // These used to exercise live handles after the child had already gone
        // away. They must now remain harmless no-ops.
        terminal.write(b"ignored");
        terminal.resize(100, 30);
        terminal.process();
        drop(terminal);
    }

    #[test]
    fn term_grid_new_dimensions() {
        let grid = TermGrid::new(80, 24);
        assert_eq!(grid.width, 80);
        assert_eq!(grid.height, 24);
        assert_eq!(grid.cells.len(), 24);
        assert_eq!(grid.cells[0].len(), 80);
        assert_eq!(grid.cursor_row, 0);
        assert_eq!(grid.cursor_col, 0);
    }

    #[test]
    fn child_input_modes_are_local_and_reset_on_exit_or_terminal_reset() {
        let mut child = TerminalEmulator::test_with_output(20, 3,
            "\x1b[?1049;9001;2004;1003;1006;1004h");
        let sibling = TermGrid::new(20, 3);
        assert!(child.grid.is_alt_screen());
        assert!(child.grid.win32_input_mode);
        assert!(child.grid.bracketed_paste);
        assert!(child.grid.focus_reporting);
        assert!(child.grid.sgr_mouse);
        assert_eq!(child.grid.mouse_tracking, 1003);
        assert!(!sibling.is_alt_screen());
        assert!(!sibling.win32_input_mode);
        assert!(!sibling.bracketed_paste);
        assert!(!sibling.focus_reporting);
        assert_eq!(sibling.mouse_tracking, 0);
        for &byte in b"\x1b[?1049;9001;2004;1003;1006;1004l" {
            child.parser.advance(&mut child.grid, byte);
        }
        assert!(!child.grid.is_alt_screen());
        assert!(!child.grid.win32_input_mode);
        assert!(!child.grid.bracketed_paste);
        assert!(!child.grid.focus_reporting);
        assert_eq!(child.grid.mouse_tracking, 0);
        for &byte in b"\x1b[?9001;1003;1006h\x1bc" {
            child.parser.advance(&mut child.grid, byte);
        }
        assert!(!child.grid.win32_input_mode);
        assert!(!child.grid.sgr_mouse);
        assert_eq!(child.grid.mouse_tracking, 0);
    }

    #[test]
    fn display_line_returns_grid_row() {
        let grid = TermGrid::new(10, 5);
        let line = grid.display_line(0);
        assert_eq!(line.len(), 10);
        assert_eq!(line[0].ch, ' ');
    }

    #[test]
    fn display_line_out_of_bounds_returns_empty() {
        let grid = TermGrid::new(10, 5);
        let line = grid.display_line(99);
        assert!(line.is_empty());
    }

    #[test]
    fn put_char_advances_cursor() {
        let mut grid = TermGrid::new(10, 5);
        grid.put_char('A');
        assert_eq!(grid.cells[0][0].ch, 'A');
        assert_eq!(grid.cursor_col, 1);
    }

    #[test]
    fn new_row_correct_width() {
        let grid = TermGrid::new(15, 3);
        let row = grid.new_row();
        assert_eq!(row.len(), 15);
        assert_eq!(row[0].ch, ' ');
    }

    #[test]
    fn put_char_wraps_at_end_of_line() {
        let mut grid = TermGrid::new(3, 2);
        grid.put_char('A');
        grid.put_char('B');
        grid.put_char('C'); // fills row, pending_wrap
        grid.put_char('D'); // should wrap to next row
        assert_eq!(grid.cursor_row, 1);
        assert_eq!(grid.cells[1][0].ch, 'D');
    }

    #[test]
    fn scroll_view_display_line() {
        let mut grid = TermGrid::new(5, 2);
        // Add scrollback
        grid.scrollback.push_back(vec![TermCell { ch: 'S', ..TermCell::default() }; 5].into());
        grid.scroll_view = 1;
        let line = grid.display_line(0);
        assert_eq!(line[0].ch, 'S');
    }

    #[test]
    fn term_cell_default() {
        let cell = TermCell::default();
        assert_eq!(cell.ch, ' ');
        assert!(!cell.bold);
        assert!(!cell.underline);
    }
}
