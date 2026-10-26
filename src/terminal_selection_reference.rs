// Frozen comparison copier from 5da4a00; test-only.
use super::*;
impl TermGrid {
    pub(crate) fn reference_selected_text(&self) -> Option<String> {
        let selection = self.selection.filter(|selection| selection.active)?;
        let (start, end) = selection.bounds();
        let mut result = String::new();
        for index in start.row..=end.row {
            let (cells, wrapped): (&[TermCell], bool) = if index < self.scrollback.len() {
                (
                    self.scrollback.get(index)?,
                    *self.scrollback_wrapped.get(index)?,
                )
            } else {
                let row = index - self.scrollback.len();
                (self.cells.get(row)?, *self.wrapped.get(row)?)
            };
            let left = if index == start.row { start.column } else { 0 };
            let right = if index == end.row {
                end.column.saturating_add(1).min(cells.len())
            } else {
                cells.len()
            };
            let text: String = cells
                .get(left..right)?
                .iter()
                .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
                .collect();
            if wrapped {
                result.push_str(&text);
            } else {
                result.push_str(text.trim_end_matches(' '));
                if index < end.row {
                    result.push('\n');
                }
            }
        }
        Some(result)
    }
}
