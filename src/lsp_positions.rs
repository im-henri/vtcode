//! LSP uses UTF-16 code units; the editor uses Unicode scalar indices.
//! Ropey's indexed conversions avoid scanning long lines for every request.
use lsp_types::{Position, Range};
use ropey::{Rope, RopeSlice};

fn line_text(rope: &Rope, line: usize) -> RopeSlice<'_> {
    let line = rope.line(line.min(rope.len_lines() - 1));
    let mut end = line.len_chars();
    while end > 0 && matches!(line.char(end - 1), '\r' | '\n') {
        end -= 1;
    }
    line.slice(..end)
}

pub(crate) fn to_utf16(rope: &Rope, line: usize, column: usize) -> u32 {
    let text = line_text(rope, line);
    text.char_to_utf16_cu(column.min(text.len_chars())) as u32
}

pub(crate) fn from_utf16(rope: &Rope, line: usize, column: u32) -> usize {
    let text = line_text(rope, line);
    text.utf16_cu_to_char((column as usize).min(text.len_utf16_cu()))
}

pub(crate) fn offset(rope: &Rope, position: Position) -> Option<usize> {
    let line = position.line as usize;
    (line < rope.len_lines())
        .then(|| rope.line_to_char(line) + from_utf16(rope, line, position.character))
}

pub(crate) fn range(rope: &Rope, range: Range) -> Option<(usize, usize)> {
    if range.start > range.end {
        return None;
    }
    let start = offset(rope, range.start)?;
    let end = offset(rope, range.end)?;
    (start <= end).then_some((start, end))
}

impl crate::editor::Editor {
    pub(crate) fn set_lsp_cursor(&mut self, line: usize, column: u32) {
        if self.bg_loading {
            self.set_cursor(line, column as usize);
            self.pending_cursor_utf16 = true;
        } else {
            self.set_cursor(line, from_utf16(&self.rope, line, column));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_positions_round_trip_across_lines_and_rope_chunks() {
        for text in [
            "".into(),
            "ASCII\r\né😀界e\u{301}\t終".into(),
            "é😀界".repeat(5000),
        ] {
            let rope = Rope::from_str(&text);
            for line in 0..rope.len_lines() {
                let slice = line_text(&rope, line);
                for col in 0..=slice.len_chars() {
                    assert_eq!(from_utf16(&rope, line, to_utf16(&rope, line, col)), col);
                }
            }
        }
    }

    #[test]
    fn clamps_columns_without_consuming_line_endings_or_losing_last_character() {
        let rope = Rope::from_str("😀x\r\nend");
        assert_eq!(from_utf16(&rope, 0, 1), 0);
        assert_eq!(from_utf16(&rope, 0, u32::MAX), 2);
        assert_eq!(to_utf16(&rope, 0, usize::MAX), 3);
        assert_eq!(offset(&rope, Position::new(1, 3)), Some(rope.len_chars()));
        assert_eq!(offset(&rope, Position::new(2, 0)), None);
        assert_eq!(
            range(&rope, Range::new(Position::new(1, 1), Position::new(0, 1))),
            None
        );
    }
}
