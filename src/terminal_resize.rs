use super::{ConptyResize, TermCell, TermGrid};

impl TermGrid {
    pub(super) fn finish_conpty_resize(&mut self) {
        let Some(ConptyResize::Repainting {
            cells,
            wrapped,
            scrolled_rows,
        }) = self.conpty_resize.take() else {
            return;
        };
        // ConPTY can combine input/output and a resize into one full repaint.
        // Rows displaced by that output are omitted without a scrolling VT
        // sequence. Locate the old viewport's surviving prefix in the repaint
        // and archive the rows it skipped, preserving their wrap and styles.
        let paint_start = self.scrollback.len().saturating_sub(scrolled_rows);
        let mut displaced = 0;
        let mut best_overlap = 0;
        for offset in 0..cells.len() {
            if cells[offset].iter().all(|cell| cell.ch == ' ') {
                continue;
            }
            let matched = cells[offset..]
                .iter()
                .zip(self.scrollback.iter().skip(paint_start).chain(&self.cells))
                .take_while(|(old, new)| {
                    old.iter().map(|cell| cell.ch).eq(new.iter().map(|cell| cell.ch))
                })
                .count();
            // Require overlap across a row boundary and prefer the earliest
            // equally good match, so repeated log lines are not duplicated.
            if matched >= 2 && matched > best_overlap {
                displaced = offset;
                best_overlap = matched;
            }
        }
        if displaced == 0 {
            return;
        }
        for (index, (row, wrapped)) in cells.into_iter().zip(wrapped).take(displaced).enumerate() {
            self.scrollback.insert(paint_start + index, row);
            self.scrollback_wrapped.insert(paint_start + index, wrapped);
        }
        if self.scroll_view > 0 {
            self.scroll_view += displaced;
        }
        self.trim_scrollback();
    }

    pub(super) fn trim_scrollback(&mut self) {
        let excess = self.scrollback.len().saturating_sub(super::MAX_SCROLLBACK_LINES);
        self.trim_selection(excess);
        self.scrollback.drain(..excess);
        self.scrollback_wrapped.drain(..excess);
        self.scroll_view = self.scroll_view.min(self.scrollback.len());
    }

    pub(super) fn resize_primary(&mut self, width: usize, height: usize) {
        if width != self.width {
            self.reflow(width, height);
            self.trim_scrollback();
            return;
        }
        let old_history = self.scrollback.len();
        // A short log usually has unused rows below its prompt. Remove those
        // first, rather than scrolling the log away and leaving only blanks.
        while self.cells.len() > height
            && self.cells.len() > self.cursor_row + 1
            && self
                .cells
                .last()
                .unwrap()
                .iter()
                .all(|cell| *cell == TermCell::default())
        {
            self.cells.pop();
            self.wrapped.pop();
        }
        if self.cells.len() > height {
            let removed = self.cells.len() - height;
            self.scrollback.extend(self.cells.drain(..removed));
            self.scrollback_wrapped
                .extend(self.wrapped.drain(..removed));
            self.cursor_row = self.cursor_row.saturating_sub(removed);
            if let Some(saved) = &mut self.saved_cursor {
                saved.0 = saved.0.saturating_sub(removed);
            }
        } else if self.cells.len() < height {
            // Increasing the pane height reveals the existing log above it.
            // ConPTY repaints only its viewport after growing. Restoring old
            // history into that viewport would let the repaint overwrite it.
            let restored = if self.is_conpty {
                0
            } else {
                (height - self.cells.len()).min(self.scrollback.len())
            };
            let start = self.scrollback.len() - restored;
            self.cells.splice(0..0, self.scrollback.drain(start..));
            self.wrapped
                .splice(0..0, self.scrollback_wrapped.drain(start..));
            self.cursor_row += restored;
            if let Some(saved) = &mut self.saved_cursor {
                saved.0 += restored;
            }
            self.cells.resize_with(height, || vec![TermCell::default(); width].into());
            self.wrapped.resize(height, false);
        }
        if self.scroll_view > 0 {
            self.scroll_view =
                (self.scroll_view + self.scrollback.len()).saturating_sub(old_history);
        }
        self.trim_scrollback();
    }

    fn reflow(&mut self, width: usize, height: usize) {
        let history = self.scrollback.len();
        // Map the live cursor, saved cursor, and top of the scrolled viewport
        // through the same logical lines as the text, including delayed wraps.
        let mut anchors = vec![(
            history + self.cursor_row,
            self.cursor_col,
            self.pending_wrap,
        )];
        if let Some(saved) = self.saved_cursor {
            anchors.push((history + saved.0, saved.1, false));
        }
        let view_anchor = if self.scroll_view > 0 {
            anchors.push((history.saturating_sub(self.scroll_view), 0, false));
            Some(anchors.len() - 1)
        } else {
            None
        };
        let history_anchor = if self.is_conpty {
            anchors.push((history, 0, false));
            Some(anchors.len() - 1)
        } else {
            None
        };
        let mut mapped = vec![(0, 0, false); anchors.len()];
        let mut rows: Vec<_> = self
            .scrollback
            .drain(..)
            .chain(self.cells.drain(..))
            .collect();
        let wraps: Vec<_> = self
            .scrollback_wrapped
            .drain(..)
            .chain(self.wrapped.drain(..))
            .collect();
        let last_anchor = anchors.iter().map(|anchor| anchor.0).max().unwrap_or(0);
        while rows.len() > last_anchor + 1
            && rows
                .last()
                .unwrap()
                .iter()
                .all(|cell| *cell == TermCell::default())
        {
            rows.pop();
        }
        let mut output = Vec::with_capacity(rows.len());
        let mut output_wrapped = Vec::with_capacity(rows.len());
        let mut logical = Vec::new();
        let row_count = rows.len();
        for (index, mut row) in rows.into_iter().enumerate() {
            // Reflow ConPTY's history separately from its viewport, including
            // lines that originally wrapped across that boundary. A repaint
            // must never receive a row containing any historical text.
            let wrapped =
                wraps[index] && index + 1 < row_count && !(self.is_conpty && index + 1 == history);
            let mut len = if wrapped {
                row.len()
            } else {
                row.iter()
                    .rposition(|cell| *cell != TermCell::default())
                    .map_or(0, |col| col + 1)
            };
            for (anchor, &(anchor_row, col, pending)) in anchors.iter().enumerate() {
                if anchor_row != index {
                    continue;
                }
                let offset = logical.len() + col + usize::from(pending);
                let delayed = pending && offset > 0 && offset % width == 0;
                let cell_offset = offset.saturating_sub(usize::from(delayed));
                mapped[anchor] = (
                    output.len() + cell_offset / width,
                    cell_offset % width,
                    delayed,
                );
                len = len.max(col + 1);
            }
            // Most shell logs have short, hard-ended rows. Keep their existing
            // allocation instead of moving every cell through `logical` and
            // cloning it into a new row on every one-column pane resize.
            if logical.is_empty() && !wrapped && len <= width {
                row.resize(width, TermCell::default());
                output.push(row);
                output_wrapped.push(false);
                continue;
            }
            row.resize(len, TermCell::default());
            logical.extend(row);
            if !wrapped {
                if logical.is_empty() {
                    output.push(vec![TermCell::default(); width].into());
                    output_wrapped.push(false);
                } else {
                    let chunks = logical.len().div_ceil(width);
                    for (chunk_index, chunk) in logical.chunks(width).enumerate() {
                        let mut row = chunk.to_vec();
                        row.resize(width, TermCell::default());
                        output.push(row.into());
                        output_wrapped.push(chunk_index + 1 < chunks);
                    }
                }
                logical.clear();
            }
        }
        let mut live_start = output.len().saturating_sub(height);
        if let Some(anchor) = history_anchor {
            // Keep the reflowed history outside ConPTY's live viewport even
            // when width and height change together.
            live_start = live_start.max(mapped[anchor].0).min(output.len());
        }
        self.cells = output.split_off(live_start);
        self.wrapped = output_wrapped.split_off(live_start);
        self.scrollback = output.into();
        self.scrollback_wrapped = output_wrapped.into();
        self.cells.resize_with(height, || vec![TermCell::default(); width].into());
        self.wrapped.resize(height, false);
        self.cursor_row = mapped[0].0.saturating_sub(live_start);
        self.cursor_col = mapped[0].1;
        self.pending_wrap = mapped[0].2;
        if let Some(saved) = &mut self.saved_cursor {
            saved.0 = mapped[1].0.saturating_sub(live_start);
            saved.1 = mapped[1].1;
        }
        if let Some(anchor) = view_anchor {
            self.scroll_view = live_start.saturating_sub(mapped[anchor].0);
        }
    }
}
