//! Copilot sends its /context chart as plain text, with significant whitespace.
//! Keep that report out of CommonMark's paragraph reflow without changing how
//! ordinary agent replies (or explicitly fenced reports) render.
use super::*;

fn cell(c: char) -> bool {
    matches!(c, '○' | '◌' | '●' | '◎' | '·' | '◦' | '◉' | '◍' | '•')
}

/// Separate the chart from its legend at the last column gap before text.
/// The legend can start with a chart symbol too (e.g. "○ System Prompt").
fn columns(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end();
    let mut cells = 0;
    let mut gap = None;
    let mut column = None;
    for (index, c) in line.char_indices() {
        if c == ' ' {
            gap.get_or_insert(index);
            continue;
        }
        if let Some(start) = gap.take()
            && index - start >= 2
            && cells >= 3
        {
            column = Some((start, index));
        }
        if !cell(c) {
            return column.map(|(start, end)| (&line[..start], &line[end..]));
        }
        cells += 1;
    }
    (cells >= 3).then_some((line, ""))
}

/// Preserve the reported strings, including rounded counts and "<1%" values.
fn legend_fields(line: &str) -> Option<(&str, &str, &str)> {
    let (rest, percent) = line.trim_end().rsplit_once(' ')?;
    if !percent.starts_with('(') || !percent.ends_with("%)") {
        return None;
    }
    let (label, count) = rest.trim_end().rsplit_once(' ')?;
    let label = label.trim_end();
    if !label.chars().next().is_some_and(cell) {
        return None;
    }
    Some((label, count, percent))
}

fn stacked(rows: &mut Vec<ChatRow>, chart: &[(&str, &str)], width: usize, c: &ThemeColors) {
    for (grid, _) in chart {
        wrapped(rows, grid.trim_start(), width, c.ui.fg_sidebar, None);
    }
    plain(rows, String::new(), c.fg_dim, None);
    let fields: Vec<_> = chart
        .iter()
        .map(|(_, legend)| legend_fields(legend))
        .collect();
    let label_width = fields
        .iter()
        .flatten()
        .map(|f| f.0.width())
        .max()
        .unwrap_or(0);
    let count_width = fields
        .iter()
        .flatten()
        .map(|f| f.1.width())
        .max()
        .unwrap_or(0);
    let percent_width = fields
        .iter()
        .flatten()
        .map(|f| f.2.width())
        .max()
        .unwrap_or(0);
    for ((_, legend), fields) in chart.iter().zip(fields) {
        if legend.is_empty() {
            continue;
        }
        if let Some((label, count, percent)) = fields {
            let line = format!(
                "{label}{}{count}{}{percent}",
                " ".repeat(label_width - label.width() + count_width - count.width() + 2),
                " ".repeat(percent_width - percent.width() + 2),
            );
            wrapped(rows, &line, width, c.ui.fg_sidebar, None);
        } else {
            wrapped(rows, legend, width, c.ui.fg_sidebar, None);
        }
    }
}

pub(super) fn render(rows: &mut Vec<ChatRow>, text: &str, width: usize, c: &ThemeColors) -> bool {
    // Most streamed messages need no extra parsing or allocation.
    if !text.contains("Context Usage") {
        return false;
    }
    let text = clean_terminal(text);
    let lines: Vec<_> = text.trim().lines().collect();
    let Some(title) = lines.first() else {
        return false;
    };
    if title.trim().trim_start_matches('#').trim() != "Context Usage"
        || !lines.iter().skip(1).any(|line| columns(line).is_some())
    {
        return false;
    }
    plain(rows, String::new(), c.fg_dim, None);
    row(
        rows,
        Line::styled(
            "Context Usage",
            Style::default()
                .fg(c.fg_accent_blue)
                .add_modifier(Modifier::BOLD),
        ),
        None,
        None,
    );
    let mut index = 1;
    while index < lines.len() {
        let start = index;
        let mut chart = Vec::new();
        while let Some(parts) = lines.get(index).and_then(|line| columns(line)) {
            chart.push(parts);
            index += 1;
        }
        if chart.is_empty() {
            wrapped(rows, lines[index], width, c.ui.fg_sidebar, None);
            index += 1;
        } else if lines[start..index].iter().any(|line| line.width() > width) {
            stacked(rows, &chart, width, c);
        } else {
            for line in &lines[start..index] {
                wrapped(rows, line, width, c.ui.fg_sidebar, None);
            }
        }
    }
    true
}
