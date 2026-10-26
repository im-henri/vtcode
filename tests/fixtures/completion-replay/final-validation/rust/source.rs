//! Geometry and gestures for column dividers, cursor handles and scrollbars.
use crate::navigation::scrollbar;
use crate::render::display_value;
use crate::viewer::{Event, Focus, Viewer};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Names,
    Values,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollPane {
    Browser,
    Waves,
}
#[derive(Clone, Copy, Debug)]
pub enum ControlDrag {
    Column {
        column: Column,
        press: u16,
        displayed: u16,
        names: u16,
        values: u16,
    },
    Scroll {
        pane: ScrollPane,
        press: u16,
        grip: u16,
        original: usize,
    },
    Cursor {
        alt: bool,
        press: u16,
        offset: i32,
        original: u64,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct CursorHandle {
    pub x: u16,
    pub width: u16,
    pub column: u16,
    pub alt: bool,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Scrollbar {
    pub start: u16,
    pub size: u16,
    pub total: usize,
    pub visible: u16,
    pub scroll: usize,
}

impl Viewer {
    /// The same handle positions are used for painting and hit testing. Separate
    /// nearby A/B labels so either cursor can still be grabbed independently.
    pub fn cursor_handles(&self) -> Vec<CursorHandle> {
        let l = self.layout();
        if l.wave_width == 0 {
            return vec![];
        }
        let width = 3.min(l.wave_width);
        let mut handles: Vec<CursorHandle> = Vec::with_capacity(2);
        for (time, alt) in [
            (Some(self.session.cursor), false),
            (self.session.alt_cursor, true),
        ] {
            let Some(column) = time.and_then(|time| self.column_at(time)) else {
                continue;
            };
            let mut x = column
                .saturating_sub(1)
                .max(l.wave_x)
                .min(l.wave_x + l.wave_width - width);
            if let Some(a) = handles.first()
                && x < a.x + a.width
                && x + width > a.x
            {
                if a.x + a.width + 1 + width <= l.wave_x + l.wave_width {
                    x = a.x + a.width + 1;
                } else if a.x > l.wave_x + width {
                    x = a.x - width - 1;
                } else {
                    continue;
                }
            }
            handles.push(CursorHandle {
                x,
                width,
                column,
                alt,
            });
        }
        handles
    }

    pub(crate) fn window_readout(&self) -> String {
        format!(
            "│   {} → {}   │   ",
            self.wave.timescale.format(self.session.start),
            self.wave
                .timescale
                .format(self.session.start.saturating_add(self.session.span))
        )
    }

    pub fn snap_control(&self) -> Option<(u16, u16)> {
        let a = self.wave.timescale.format(self.session.cursor);
        let b = self
            .session
            .alt_cursor
            .map(|t| self.wave.timescale.format(t))
            .unwrap_or_else(|| "—".into());
        let delta = self
            .session
            .alt_cursor
            .map(|t| self.wave.timescale.format(t.abs_diff(self.session.cursor)))
            .unwrap_or_else(|| "—".into());
        let x = 1 + format!("A {a}   B {b}   Δ {delta}   {}", self.window_readout()).width();
        let width = if self.session.snap { 7 } else { 8 };
        (x + usize::from(width) < usize::from(self.width.min(512))).then_some((x as u16, width))
    }

    pub(crate) fn scroll_bar(&self, pane: ScrollPane) -> Option<Scrollbar> {
        let l = self.layout();
        let (total, scroll) = match pane {
            ScrollPane::Browser if l.browser > 0 => (self.browser_nodes.len(), self.browser_scroll),
            ScrollPane::Waves if l.value > 0 => (
                self.rows()
                    .last()
                    .map(|row| usize::from(row.line + row.height))
                    .unwrap_or(0),
                self.scroll,
            ),
            _ => return None,
        };
        let (start, size) = scrollbar(total, scroll, l.body_height)?;
        Some(Scrollbar {
            start: l.body_y + start,
            size,
            total,
            visible: l.body_height,
            scroll,
        })
    }

    fn set_scroll(&mut self, pane: ScrollPane, value: usize) {
        match pane {
            ScrollPane::Browser => self.browser_scroll = value,
            ScrollPane::Waves => self.scroll = value,
        }
    }

    pub(crate) fn begin_control(&mut self, event: &Event) -> bool {
        let l = self.layout();
        let (x, y) = (event.view_column, event.view_row);
        if y == 2
            && self
                .snap_control()
                .is_some_and(|(start, width)| x >= start && x < start + width)
        {
            self.toggle_snap();
            return true;
        }
        if y == 3
            && let Some(handle) = self
                .cursor_handles()
                .into_iter()
                .find(|h| x >= h.x && x < h.x + h.width)
        {
            self.focus = Focus::Waves;
            self.control_drag = Some(ControlDrag::Cursor {
                alt: handle.alt,
                press: x,
                offset: i32::from(x) - i32::from(handle.column),
                original: if handle.alt {
                    self.session.alt_cursor.unwrap()
                } else {
                    self.session.cursor
                },
            });
            return true;
        }
        if y < 3 || y >= l.body_y + l.body_height {
            return false;
        }
        if y >= l.body_y {
            for pane in [ScrollPane::Browser, ScrollPane::Waves] {
                let column = match pane {
                    ScrollPane::Browser => l.browser.checked_sub(1),
                    ScrollPane::Waves => l.wave_x.checked_sub(1),
                };
                if column != Some(x) {
                    continue;
                }
                if let Some(bar) = self.scroll_bar(pane) {
                    self.focus = if pane == ScrollPane::Browser {
                        Focus::Browser
                    } else {
                        Focus::Waves
                    };
                    if y >= bar.start && y < bar.start + bar.size {
                        self.control_drag = Some(ControlDrag::Scroll {
                            pane,
                            press: y,
                            grip: y - bar.start,
                            original: bar.scroll,
                        });
                    } else {
                        let page = usize::from(bar.visible.saturating_sub(1).max(1));
                        self.set_scroll(
                            pane,
                            if y < bar.start {
                                bar.scroll.saturating_sub(page)
                            } else {
                                bar.scroll
                                    .saturating_add(page)
                                    .min(bar.total - usize::from(bar.visible))
                            },
                        );
                    }
                    return true;
                }
            }
        }
        let column = if l.name > 0 && x == l.browser + l.name - 1 {
            Some(Column::Names)
        } else if l.value > 0 && x == l.wave_x - 1 {
            Some(Column::Values)
        } else {
            None
        };
        if let Some(column) = column {
            self.control_drag = Some(ControlDrag::Column {
                column,
                press: x,
                displayed: if column == Column::Names {
                    l.name
                } else {
                    l.value
                },
                names: self.session.name_width,
                values: self.session.value_width,
            });
            return true;
        }
        false
    }

    pub(crate) fn update_control(&mut self, event: &Event) {
        let Some(drag) = self.control_drag else {
            return;
        };
        match drag {
            ControlDrag::Column {
                column,
                press,
                displayed,
                names,
                values,
            } => {
                let requested =
                    i32::from(displayed) + i32::from(event.view_column) - i32::from(press);
                match column {
                    Column::Names => {
                        self.session.name_width = if event.view_column == press {
                            names
                        } else {
                            requested.clamp(12, 80) as u16
                        }
                    }
                    Column::Values => {
                        self.session.value_width = if event.view_column == press {
                            values
                        } else {
                            requested.clamp(7, 64) as u16
                        }
                    }
                }
                if event.kind == "up"
                    && (names, values) != (self.session.name_width, self.session.value_width)
                {
                    let mut before = self.session.clone();
                    before.name_width = names;
                    before.value_width = values;
                    self.checkpoint_session(before);
                    self.status = "Column width updated · Ctrl+Z to undo · Right-click its header to auto-fit".into();
                }
            }
            ControlDrag::Scroll {
                pane,
                press,
                grip,
                original,
            } => {
                if let Some(bar) = self.scroll_bar(pane) {
                    let travel = bar.visible - bar.size;
                    let position = event
                        .view_row
                        .saturating_sub(self.layout().body_y + grip)
                        .min(travel);
                    let scroll = usize::from(position) * (bar.total - usize::from(bar.visible))
                        / usize::from(travel.max(1));
                    self.set_scroll(
                        pane,
                        if event.view_row == press {
                            original
                        } else {
                            scroll
                        },
                    );
                }
            }
            ControlDrag::Cursor {
                alt,
                press,
                offset,
                original,
            } => {
                if event.view_column == press {
                    if alt {
                        self.session.alt_cursor = Some(original);
                    } else {
                        self.session.cursor = original;
                    }
                } else {
                    let x = (i32::from(event.view_column) - offset)
                        .clamp(0, i32::from(self.width.min(512).saturating_sub(1)))
                        as u16;
                    self.set_cursor(x, alt);
                }
                if event.kind == "up" {
                    self.status = format!(
                        "Cursor {} at {} · Drag its handle to adjust",
                        if alt { "B" } else { "A" },
                        self.wave.timescale.format(if alt {
                            self.session.alt_cursor.unwrap()
                        } else {
                            self.session.cursor
                        })
                    );
                }
            }
        }
        if event.kind == "up" {
            self.control_drag = None;
        }
    }

    pub(crate) fn cancel_control(&mut self) -> bool {
        let Some(drag) = self.control_drag.take() else {
            return false;
        };
        match drag {
            ControlDrag::Column { names, values, .. } => {
                self.session.name_width = names;
                self.session.value_width = values;
            }
            ControlDrag::Scroll { pane, original, .. } => self.set_scroll(pane, original),
            ControlDrag::Cursor { alt, original, .. } => {
                if alt {
                    self.session.alt_cursor = Some(original);
                } else {
                    self.session.cursor = original;
                }
            }
        }
        true
    }

    pub(crate) fn toggle_snap(&mut self) {
        self.session.snap = !self.session.snap;
        self.status = format!(
            "Edge snapping {} · Click the snap indicator or press s to toggle",
            if self.session.snap { "on" } else { "off" }
        );
    }

    pub(crate) fn control_hint(&self) -> Option<String> {
        if let Some(drag) = self.control_drag {
            return Some(match drag {
                ControlDrag::Column { column, .. } => format!(
                    "Resize {} column · Release to apply · Esc restores width",
                    if column == Column::Names {
                        "signal names"
                    } else {
                        "values"
                    }
                ),
                ControlDrag::Scroll { pane, .. } => format!(
                    "Scroll {} · Drag the thumb · Esc restores position",
                    if pane == ScrollPane::Browser {
                        "hierarchy"
                    } else {
                        "signals"
                    }
                ),
                ControlDrag::Cursor { alt, .. } => format!(
                    "Cursor {} · {} · Release to place · Esc restores time",
                    if alt { "B" } else { "A" },
                    self.wave.timescale.format(if alt {
                        self.session.alt_cursor.unwrap()
                    } else {
                        self.session.cursor
                    })
                ),
            });
        }
        let hover = self.hover?;
        let l = self.layout();
        if hover.y == 2
            && self
                .snap_control()
                .is_some_and(|(x, width)| hover.x >= x && hover.x < x + width)
        {
            return Some("Edge snapping · Click to toggle · s".into());
        }
        if hover.y == 3
            && let Some(h) = self
                .cursor_handles()
                .into_iter()
                .find(|h| hover.x >= h.x && hover.x < h.x + h.width)
        {
            return Some(format!(
                "Cursor {} at {} · Drag to adjust without moving the other cursor",
                if h.alt { "B" } else { "A" },
                self.wave.timescale.format(if h.alt {
                    self.session.alt_cursor.unwrap()
                } else {
                    self.session.cursor
                })
            ));
        }
        if hover.y >= l.body_y && hover.y < l.body_y + l.body_height {
            for pane in [ScrollPane::Browser, ScrollPane::Waves] {
                let column = if pane == ScrollPane::Browser {
                    l.browser.checked_sub(1)
                } else {
                    l.wave_x.checked_sub(1)
                };
                if column == Some(hover.x) && self.scroll_bar(pane).is_some() {
                    return Some(
                        "Drag the scrollbar thumb · Click its track to page through signals".into(),
                    );
                }
            }
        }
        if hover.y >= 3
            && hover.y < l.body_y + l.body_height
            && (l.name > 0 && hover.x == l.browser + l.name - 1
                || l.value > 0 && hover.x == l.wave_x - 1)
        {
            return Some(
                "Drag to resize this column · Right-click the column header to auto-fit".into(),
            );
        }
        if matches!(hover.y, 3 | 4) && hover.x >= l.browser && hover.x < l.wave_x {
            return Some("Right-click this header to fit signal names or values · Drag either divider to resize".into());
        }
        None
    }

    pub(crate) fn fit_columns(&mut self, names: bool, values: bool, reset: bool) {
        let (mut name_width, mut value_width) = (self.session.name_width, self.session.value_width);
        if reset {
            name_width = 28;
            value_width = 13;
        } else {
            if names {
                name_width = self
                    .rows()
                    .into_iter()
                    .map(|row| {
                        let label = self.watch_row_label(row).width();
                        let badge = row.bit.is_none()
                            && matches!(
                                self.session.watches[row.watch],
                                crate::viewer::Watch::Signal { bit: None, .. }
                            )
                            && self
                                .signal_id(row.watch)
                                .is_some_and(|id| self.wave.signals[id].width > 1)
                            && label + 10 >= 23;
                        label + if badge { 10 } else { 5 }
                    })
                    .max()
                    .unwrap_or(28)
                    .clamp(12, 80) as u16;
            }
            if values {
                value_width = self
                    .session
                    .watches
                    .iter()
                    .enumerate()
                    .filter_map(|(index, watch)| {
                        let crate::viewer::Watch::Signal { radix, bit, .. } = watch else {
                            return None;
                        };
                        self.watch_value(index, None, self.session.cursor)
                            .map(|value| {
                                display_value(
                                    &value,
                                    if bit.is_some() {
                                        crate::model::Radix::Binary
                                    } else {
                                        *radix
                                    },
                                )
                                .width()
                                    + 2
                            })
                    })
                    .max()
                    .unwrap_or(13)
                    .clamp(11, 64) as u16;
            }
        }
        if (name_width, value_width) == (self.session.name_width, self.session.value_width) {
            return;
        }
        self.checkpoint();
        self.session.name_width = name_width;
        self.session.value_width = value_width;
        self.status = "Columns fitted · Ctrl+Z to undo · Drag either divider to adjust".into();
    }
}
