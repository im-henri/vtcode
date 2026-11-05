use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Segment {
    pub start: usize,
    pub end: usize,
    pub cell: usize,
}

/// Wrap at source word boundaries without splitting a displayed grapheme.
pub(crate) fn segments(
    text: &str,
    width: usize,
    glyph_width: impl Fn(&str, usize) -> usize,
) -> impl Iterator<Item = Segment> {
    let width = width.max(1);
    let mut byte = 0;
    let mut column = 0;
    let mut cell = 0;
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let (start, start_cell) = (column, cell);
        let mut boundary = None;
        let mut used = 0;
        for (relative, glyph) in text[byte..].grapheme_indices(true) {
            let cells = glyph_width(glyph, cell);
            if used > 0 && used + cells > width {
                (byte, column, cell) = boundary.unwrap_or((byte + relative, column, cell));
                return Some(Segment {
                    start,
                    end: column,
                    cell: start_cell,
                });
            }
            used += cells.min(width);
            column += glyph.chars().count();
            cell += cells;
            if glyph.chars().all(char::is_whitespace)
                || matches!(glyph, "/" | "\\" | "-" | "," | ";")
            {
                boundary = Some((byte + relative + glyph.len(), column, cell));
            }
        }
        done = true;
        Some(Segment {
            start,
            end: column,
            cell: start_cell,
        })
    })
}
