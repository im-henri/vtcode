//! A hover concerns the token under the pointer, not every token on its line.
//! Keep both lookup and copied preview text local, including on generated lines.
use super::*;

const MARKERS: [(&str, &str); 5] = [
    ("TODO", "Todo"),
    ("FIXME", "Fix Me"),
    ("HACK", "Hack"),
    ("BUG", "Bug"),
    ("NOTE", "Note"),
];
const PREVIEW_CHARS: usize = 60;

fn marker_at(line: ropey::RopeSlice<'_>, column: usize) -> Option<(usize, usize)> {
    if column >= line.len_chars() {
        return None;
    }
    // Five marker characters and their adjacent boundary characters suffice.
    let start = column.saturating_sub(5);
    let end = column.saturating_add(6).min(line.len_chars());
    let mut local = ['\0'; 11];
    let len = end - start;
    for (slot, ch) in local.iter_mut().zip(line.slice(start..end).chars()) {
        *slot = ch;
    }
    for (index, &(marker, _)) in MARKERS.iter().enumerate() {
        let first = column.saturating_sub(marker.len() - 1);
        for position in first..=column {
            let offset = position - start;
            let after = offset + marker.len();
            if after > len
                || !local[offset..after].iter().copied().eq(marker.chars())
                || (position > 0 && local[offset - 1].is_ascii_alphanumeric())
                || (after < len && local[after].is_ascii_alphanumeric())
            {
                continue;
            }

            return Some((index, position));
        }
    }
    None
}

pub(crate) fn has_tooltip(app: &App, position: Option<(usize, usize)>) -> bool {
    position.is_some_and(|(line, column)| {
        app.editor().and_then(|editor| editor.rope.get_line(line))
            .is_some_and(|line| marker_at(line, column).is_some())
    })
}

fn preview(line: ropey::RopeSlice<'_>, column: usize) -> Option<(usize, String)> {
    let (index, position) = marker_at(line, column)?;
    let (marker, label) = MARKERS[index];
    // Read only enough description for the visible tooltip. In particular,
    // never format a full line and then truncate it.
    let rest: String = line.chars_at(position + marker.len())
        .skip_while(|&ch| ch == ':' || ch == ' ')
        .take(PREVIEW_CHARS).collect();
    let rest = rest.trim_end();
    let text = if rest.is_empty() { format!(" ⚠ {label} ") }
        else { format!(" ⚠ {label}: {rest} ") };
    Some((index, text.chars().take(PREVIEW_CHARS).collect()))
}

pub(crate) fn draw(f: &mut Frame, app: &App, code_area: Rect) {
    if code_area.is_empty() {
        return;
    }
    let Some((line, column)) = app.hovered_editor_pos else {
        return;
    };
    let Some(source) = app.editor().and_then(|editor| editor.rope.get_line(line)) else {
        return;
    };
    let Some((marker, text)) = preview(source, column) else {
        return;
    };
    let color = [
        app.colors.fg_todo,
        app.colors.fg_fixme,
        app.colors.fg_hack,
        app.colors.fg_error,
        app.colors.fg_info_label,
    ][marker];
    let width =
        unicode_width::UnicodeWidthStr::width(text.as_str()).min(f.area().width as usize) as u16;
    // Source columns are not screen cells after tabs, wrapping or horizontal
    // scrolling. The pointer is already in screen coordinates.
    let x = app
        .mouse_col
        .clamp(code_area.x, code_area.right().saturating_sub(1));
    let y = app
        .mouse_row
        .clamp(code_area.y, code_area.bottom().saturating_sub(1));
    let y = if y + 1 < code_area.bottom() {
        y + 1
    } else {
        y.saturating_sub(1)
    };
    let area = Rect::new(x.min(f.area().right().saturating_sub(width)), y, width, 1);
    f.render_widget(
        Paragraph::new(Span::styled(
            text,
            Style::default().fg(color).bg(app.colors.bg_sidebar),
        )),
        area,
    );
}

#[cfg(test)]
#[path = "ui_marker_tooltip_tests.rs"]
mod tests;
