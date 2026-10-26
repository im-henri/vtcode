//! Convert only damaged terminal rows. Cursor and selection never enter the
//! retained base cells, so moving an overlay cannot dirty cached terminal text.

use crate::terminal::{TermGrid, TermRow};
use crate::theme_workbench::WorkbenchColors;
use crate::ui_editor_highlight::line_lru::LineLru;
use ratatui::{
    buffer::{Buffer, Cell},
    layout::Rect,
    style::{Color, Modifier},
};

const MAX_CACHED_CELLS: usize = 256 * 1024;
const MAX_CACHED_ROWS: usize = 2_048;

#[derive(Clone, Copy, Eq, PartialEq)]
struct Context {
    foreground: Color,
    background: Color,
    ansi: [Color; 16],
    first: usize,
    width: usize,
}

#[derive(Default)]
struct ConvertedRow {
    generation: u64,
    context: u64,
    cells: Vec<PaintCell>,
}

// Ratatui's owned symbol cells are expensive to clone. Keep only the compact,
// copyable inputs needed to paint a fresh frame; normalize NULs once here.
#[derive(Clone, Copy, Default)]
struct PaintCell {
    character: char,
    foreground: Color,
    background: Color,
    modifiers: Modifier,
}

#[derive(Default)]
pub(crate) struct RowCache {
    rows: LineLru<ConvertedRow>,
    allocated_cells: usize,
    context: Option<Context>,
    epoch: u64,
    #[cfg(test)]
    conversions: usize,
}

impl RowCache {
    #[cfg(test)]
    pub(crate) fn test_stats(&self) -> (usize, usize, usize) {
        (self.conversions, self.rows.len(), self.allocated_cells)
    }

    #[cfg(test)]
    pub(crate) fn test_row(&self, identity: usize) -> Option<(*const u8, usize)> {
        self.rows
            .get(identity)
            .map(|row| (row.cells.as_ptr().cast(), row.cells.len()))
    }

    fn evict_oldest(&mut self) {
        if let Some((_, row)) = self.rows.pop_front() {
            self.allocated_cells -= row.cells.capacity();
        }
    }

    pub(crate) fn paint(
        &mut self,
        buffer: &mut Buffer,
        grid: &TermGrid,
        area: Rect,
        palette: &WorkbenchColors,
    ) {
        let clipped = area.intersection(buffer.area);
        if clipped.is_empty() {
            return;
        }
        let first = (clipped.x - area.x) as usize;
        let width = clipped.width as usize;
        let context = Context {
            foreground: palette.fg_terminal,
            background: palette.bg_terminal,
            ansi: palette.ansi,
            first,
            width,
        };
        if self.context != Some(context) {
            self.context = Some(context);
            self.epoch = self.epoch.wrapping_add(1);
            if self.epoch == 0 {
                self.rows.clear();
                self.allocated_cells = 0;
                self.epoch = 1;
            }
        }
        let limit = (clipped.height as usize * 2)
            .min(MAX_CACHED_ROWS)
            .min(MAX_CACHED_CELLS / width)
            .max(1);
        while self.rows.len() > limit {
            self.evict_oldest();
        }
        let mut selection = grid
            .has_selection()
            .then(|| crate::ui_colors::TerminalSelectionStyle::new(palette));
        for y in clipped.y..clipped.bottom() {
            let visual = (y - area.y) as usize;
            let source = grid.display_row(visual);
            let identity = source.map_or(0, TermRow::identity);
            let generation = source.map_or(0, TermRow::generation);
            let cached = self
                .rows
                .get_or_insert_recycled_mut(identity, limit, |old| {
                    let mut row = old.unwrap_or_default();
                    // Recycle the allocation, never another row's validity tags.
                    // Unrelated rows can have equal generation numbers.
                    row.context = 0;
                    row
                });
            let start = (y - buffer.area.y) as usize * buffer.area.width as usize
                + (clipped.x - buffer.area.x) as usize;
            let target = &mut buffer.content[start..start + width];
            if cached.generation != generation || cached.context != self.epoch {
                let old_capacity = cached.cells.capacity();
                convert_and_paint(cached, source, first, target, palette);
                self.allocated_cells =
                    self.allocated_cells - old_capacity + cached.cells.capacity();
                cached.generation = generation;
                cached.context = self.epoch;
                #[cfg(test)]
                {
                    self.conversions += 1;
                }
            } else {
                for (target, base) in target.iter_mut().zip(&cached.cells) {
                    paint_cell(target, base);
                }
            }
            if let Some(selection) = &mut selection {
                let columns = grid.selection_columns(visual);
                let left = columns.start.max(first);
                let right = columns
                    .end
                    .min(first + width)
                    .min(source.map_or(0, |row| row.len()));
                for column in left..right {
                    let cell = &mut target[column - first];
                    cell.set_style(selection.apply(cell.style(), palette.fg_terminal));
                }
            }
            // Count actual retained capacities, including rows from older
            // geometries. Evict as we go, bounding peak storage to the budget
            // plus the one row being converted, rather than a full new screen.
            while self.allocated_cells > MAX_CACHED_CELLS {
                self.evict_oldest();
            }
        }
    }
}

#[inline]
fn paint_cell(target: &mut Cell, base: &PaintCell) {
    // Patch only the same fields as the original renderer. Inherited
    // modifiers, underline color, and image skip flags remain untouched.
    target.set_char(base.character);
    target.fg = base.foreground;
    target.bg = base.background;
    target.modifier |= base.modifiers;
}

fn convert_and_paint(
    cached: &mut ConvertedRow,
    source: Option<&TermRow>,
    first: usize,
    target: &mut [Cell],
    palette: &WorkbenchColors,
) {
    // Reuse both changed rows and storage evicted by the indexed LRU. Avoid
    // retaining a huge old row after a substantial reduction in pane width.
    let width = target.len();
    if cached.cells.capacity() > width.saturating_mul(4) {
        cached.cells = Vec::new();
    }
    cached.cells.resize(width, PaintCell::default());
    let source = source.and_then(|row| row.get(first..)).unwrap_or(&[]);
    let available = source.len().min(width);
    // Convert and paint damaged cells in one pass. A full-screen repaint
    // should not scan the new cache a second time just to copy it to the frame.
    for ((cell, target), source) in cached.cells[..available]
        .iter_mut()
        .zip(&mut target[..available])
        .zip(source)
    {
        cell.character = if source.ch == '\0' { ' ' } else { source.ch };
        cell.foreground = palette.terminal_color(source.fg, true);
        cell.background = palette.terminal_color(source.bg, false);
        let mut modifiers = Modifier::empty();
        for (enabled, modifier) in [
            (source.bold, Modifier::BOLD),
            (source.underline, Modifier::UNDERLINED),
            (source.dim, Modifier::DIM),
            (source.italic, Modifier::ITALIC),
            (source.strikethrough, Modifier::CROSSED_OUT),
        ] {
            if enabled {
                modifiers |= modifier;
            }
        }
        cell.modifiers = modifiers;
        paint_cell(target, cell);
    }
    let blank = PaintCell {
        character: ' ',
        foreground: palette.fg_terminal,
        background: palette.bg_terminal,
        modifiers: Modifier::empty(),
    };
    for (cell, target) in cached.cells[available..]
        .iter_mut()
        .zip(&mut target[available..])
    {
        *cell = blank;
        paint_cell(target, cell);
    }
}
