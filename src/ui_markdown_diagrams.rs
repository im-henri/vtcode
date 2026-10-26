//! A deliberately bounded Mermaid subset, drawn in terminal cells without a
//! browser, subprocess, network request, or work on an unchanged preview frame.
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug)]
pub(super) enum Diagram {
    Flow {
        nodes: Vec<(String, String)>,
        edges: Vec<(usize, usize, String)>,
        direction: String,
    },
    Sequence {
        people: Vec<(String, String)>,
        messages: Vec<(usize, usize, String, bool)>,
    },
    Pie {
        title: String,
        slices: Vec<(String, f64)>,
    },
}

impl Diagram {
    pub(super) fn parse(source: &str) -> Option<Self> {
        // Unsupported/oversized input falls back to the original code fence.
        if source.len() > 65_536 {
            return None;
        }
        let mut lines = source
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with("%%"));
        let header = lines.next()?.trim_end_matches(';').trim();
        let mut diagram = if let Some(dir) = header
            .strip_prefix("flowchart ")
            .or_else(|| header.strip_prefix("graph "))
        {
            if !matches!(dir, "TD" | "TB" | "BT" | "LR" | "RL") {
                return None;
            }
            Self::Flow {
                nodes: vec![],
                edges: vec![],
                direction: dir.into(),
            }
        } else if header == "sequenceDiagram" {
            Self::Sequence {
                people: vec![],
                messages: vec![],
            }
        } else if header == "pie" || header.starts_with("pie ") {
            let rest = header[3..]
                .trim()
                .strip_prefix("showData")
                .unwrap_or(header[3..].trim())
                .trim();
            let title = if rest.is_empty() {
                "Distribution"
            } else {
                rest.strip_prefix("title ")?
            };
            Self::Pie {
                title: title.into(),
                slices: vec![],
            }
        } else {
            return None;
        };
        for line in lines {
            match &mut diagram {
                Self::Flow { nodes, edges, .. } => {
                    // A semicolon is a statement separator only outside labels.
                    for statement in statements(line)? {
                        let (mut from, mut rest) = flow_node(statement, nodes)?;
                        while !rest.trim().is_empty() {
                            rest = rest.trim_start();
                            let (tail, arrow) = if let Some(tail) = rest.strip_prefix("-->") {
                                (tail, "")
                            } else if let Some(tail) = rest.strip_prefix("-.->") {
                                (tail, "dashed")
                            } else if let Some(tail) = rest.strip_prefix("==>") {
                                (tail, "thick")
                            } else {
                                return None;
                            };
                            rest = tail.trim_start();
                            let label = if let Some(tail) = rest.strip_prefix('|') {
                                let (label, tail) = tail.split_once('|')?;
                                rest = tail;
                                label.trim().trim_matches('"').to_owned()
                            } else {
                                arrow.to_owned()
                            };
                            let (to, tail) = flow_node(rest, nodes)?;
                            edges.push((from, to, label));
                            from = to;
                            rest = tail;
                            if edges.len() > 128 {
                                return None;
                            }
                        }
                    }
                }
                Self::Sequence { people, messages } => {
                    if let Some(person) = line
                        .strip_prefix("participant ")
                        .or_else(|| line.strip_prefix("actor "))
                    {
                        let (id, label) = person.split_once(" as ").unwrap_or((person, person));
                        person_index(people, id.trim(), Some(label.trim()))?;
                    } else {
                        let (route, label) = line.split_once(':')?;
                        let mut found = None;
                        for arrow in ["-->>", "->>", "-->", "->"] {
                            if let Some((from, to)) = route.split_once(arrow) {
                                let from = person_index(people, from.trim(), None)?;
                                let to = person_index(people, to.trim(), None)?;
                                found =
                                    Some((from, to, label.trim().into(), arrow.starts_with("--")));
                                break;
                            }
                        }
                        messages.push(found?);
                        if messages.len() > 128 {
                            return None;
                        }
                    }
                }
                Self::Pie { title, slices } => {
                    if let Some(text) = line.strip_prefix("title ") {
                        *title = text.to_owned();
                        continue;
                    }
                    let rest = line.strip_prefix('"')?;
                    let (label, value) = rest.split_once('"')?;
                    let value: f64 = value.trim().strip_prefix(':')?.trim().parse().ok()?;
                    if !value.is_finite() || value < 0.0 {
                        return None;
                    }
                    slices.push((label.to_owned(), value));
                    if slices.len() > 64 {
                        return None;
                    }
                }
            }
        }
        match &diagram {
            Self::Flow { nodes, .. } if nodes.is_empty() => None,
            Self::Sequence { people, .. } if people.is_empty() => None,
            Self::Pie { slices, .. } if slices.is_empty() => None,
            _ => Some(diagram),
        }
    }

    pub(super) fn title(&self) -> &str {
        match self {
            Self::Flow { .. } => "Flowchart",
            Self::Sequence { .. } => "Sequence",
            Self::Pie { title, .. } => title,
        }
    }

    pub(super) fn lines(&self, width: usize) -> Vec<String> {
        let width = width.clamp(1, 160);
        match self {
            Self::Flow {
                nodes,
                edges,
                direction,
            } => {
                if matches!(direction.as_str(), "TD" | "TB")
                    && let Some(lines) = flow_grid(nodes, edges, width)
                {
                    return lines;
                }
                // Narrow panes, cycles and horizontal graphs retain every node
                // and edge in a compact, labeled connection map.
                let mut lines = Vec::new();
                for (index, (id, label)) in nodes.iter().enumerate() {
                    lines.extend(wrap(&format!("[{id}] {label}"), width));
                    for (_, to, caption) in edges.iter().filter(|(from, _, _)| *from == index) {
                        let edge = if caption.is_empty() {
                            format!("  → [{}]", nodes[*to].0)
                        } else {
                            format!("  {caption} → [{}]", nodes[*to].0)
                        };
                        lines.extend(wrap(&edge, width));
                    }
                }
                lines
            }
            Self::Sequence { people, messages } => sequence(people, messages, width),
            Self::Pie { slices, .. } => {
                // Scaling by the maximum keeps a sum of finite large values
                // from overflowing. Bars are a terminal equivalent of slices.
                let max = slices.iter().map(|(_, n)| *n).fold(0.0_f64, f64::max);
                let sum: f64 = if max > 0.0 {
                    slices.iter().map(|(_, n)| n / max).sum()
                } else {
                    0.0
                };
                let mut lines = Vec::new();
                for (label, value) in slices {
                    let fraction = if sum > 0.0 { value / max / sum } else { 0.0 };
                    lines.extend(wrap(
                        &format!("{label} · {value} ({:.1}%)", fraction * 100.0),
                        width,
                    ));
                    lines.push("█".repeat((fraction * width as f64).round() as usize));
                }
                lines
            }
        }
    }
}

fn statements(line: &str) -> Option<Vec<&str>> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut start = 0;
    let mut result = Vec::new();
    for (i, ch) in line.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        }
        if !quoted {
            match ch {
                '[' | '(' | '{' => depth += 1,
                ']' | ')' | '}' => depth = depth.checked_sub(1)?,
                ';' if depth == 0 => {
                    if !line[start..i].trim().is_empty() {
                        result.push(line[start..i].trim());
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    if depth != 0 || quoted {
        return None;
    }
    if !line[start..].trim().is_empty() {
        result.push(line[start..].trim());
    }
    Some(result)
}

fn person_index(
    people: &mut Vec<(String, String)>,
    id: &str,
    label: Option<&str>,
) -> Option<usize> {
    if id.is_empty() || !id.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
        return None;
    }
    if let Some(index) = people.iter().position(|(key, _)| key == id) {
        if let Some(label) = label {
            people[index].1 = label.into();
        }
        return Some(index);
    }
    if people.len() == 64 {
        return None;
    }
    people.push((id.into(), label.unwrap_or(id).into()));
    Some(people.len() - 1)
}

fn flow_node<'a>(source: &'a str, nodes: &mut Vec<(String, String)>) -> Option<(usize, &'a str)> {
    let source = source.trim_start();
    let end = source
        .find(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .unwrap_or(source.len());
    let id = &source[..end];
    let mut rest = source[end..].trim_start();
    let mut label = None;
    if let Some(open) = rest.chars().next().filter(|c| matches!(c, '[' | '(' | '{')) {
        let close = match open {
            '[' => ']',
            '(' => ')',
            _ => '}',
        };
        let mut quoted = false;
        let end = rest[1..].char_indices().find_map(|(i, c)| {
            if c == '"' {
                quoted = !quoted;
            }
            (c == close && !quoted).then_some(i + 1)
        })?;
        label = Some(rest[1..end].trim().trim_matches('"'));
        rest = &rest[end + 1..];
    }
    Some((person_index(nodes, id, label)?, rest))
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut used = 0;
    for glyph in text.graphemes(true) {
        if glyph == "\n" {
            rows.push(String::new());
            used = 0;
            continue;
        }
        if glyph.chars().any(char::is_control) {
            continue;
        }
        let size = glyph.width();
        if size > width {
            continue;
        }
        if used + size > width {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().unwrap().push_str(glyph);
        used += size;
    }
    rows
}

struct Canvas {
    cells: Vec<Vec<String>>,
    wires: Vec<Vec<u8>>,
    width: usize,
}
impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![vec![" ".into(); width]; height],
            wires: vec![vec![0; width]; height],
            width,
        }
    }
    fn text(&mut self, x: usize, y: usize, text: &str) {
        let mut x = x;
        for glyph in text.graphemes(true) {
            let size = glyph.width();
            if y >= self.cells.len() || x + size > self.width {
                break;
            }
            if size == 0 || glyph.chars().any(char::is_control) {
                continue;
            }
            self.cells[y][x] = glyph.into();
            for col in x + 1..x + size {
                self.cells[y][col].clear();
            }
            x += size;
        }
    }
    fn connect(&mut self, x: usize, y: usize, to_x: usize, to_y: usize) {
        if x == to_x {
            for row in y.min(to_y)..y.max(to_y) {
                self.wires[row][x] |= 2;
                self.wires[row + 1][x] |= 1;
            }
        } else {
            for col in x.min(to_x)..x.max(to_x) {
                self.wires[y][col] |= 4;
                self.wires[y][col + 1] |= 8;
            }
        }
    }
    fn paint_wires(&mut self) {
        for (y, row) in self.wires.iter().enumerate() {
            for (x, mask) in row.iter().enumerate() {
                let glyph = match mask {
                    0 => continue,
                    1..=3 => "│",
                    4 | 8 | 12 => "─",
                    5 => "└",
                    6 => "┌",
                    7 => "├",
                    9 => "┘",
                    10 => "┐",
                    11 => "┤",
                    13 => "┴",
                    14 => "┬",
                    _ => "┼",
                };
                self.cells[y][x] = glyph.into();
            }
        }
    }
    fn lines(self) -> Vec<String> {
        self.cells
            .into_iter()
            .map(|row| row.concat().trim_end().to_owned())
            .collect()
    }
}

fn flow_grid(
    nodes: &[(String, String)],
    edges: &[(usize, usize, String)],
    width: usize,
) -> Option<Vec<String>> {
    let mut indegree = vec![0; nodes.len()];
    for (_, to, _) in edges {
        indegree[*to] += 1;
    }
    let mut queue: std::collections::VecDeque<usize> =
        (0..nodes.len()).filter(|i| indegree[*i] == 0).collect();
    let mut depth = vec![0; nodes.len()];
    let mut visited = 0;
    while let Some(from) = queue.pop_front() {
        visited += 1;
        for (_, to, _) in edges.iter().filter(|(index, _, _)| *index == from) {
            depth[*to] = depth[*to].max(depth[from] + 1);
            indegree[*to] -= 1;
            if indegree[*to] == 0 {
                queue.push_back(*to);
            }
        }
    }
    if visited != nodes.len()
        || edges
            .iter()
            .any(|(from, to, _)| depth[*to] != depth[*from] + 1)
    {
        return None;
    }
    let levels = depth.iter().max().copied()? + 1;
    let mut positions = vec![(0, 0, 0, 0); nodes.len()];
    let mut labels = vec![vec![]; nodes.len()];
    let mut routes = vec![0; edges.len()];
    let mut y = 0;
    for level in 0..levels {
        let layer: Vec<_> = (0..nodes.len()).filter(|i| depth[*i] == level).collect();
        let cell = width / layer.len();
        if cell < 12 {
            return None;
        }
        let box_width = (cell - 2).min(36);
        let mut height = 3;
        for &node in &layer {
            labels[node] = wrap(&nodes[node].1, box_width - 4);
            height = height.max(labels[node].len() + 2);
        }
        for (col, &node) in layer.iter().enumerate() {
            positions[node] = (col * cell + (cell - box_width) / 2, y, box_width, height);
        }
        y += height;
        for (index, (from, _, label)) in edges
            .iter()
            .enumerate()
            .filter(|(_, (from, _, _))| depth[*from] == level)
        {
            let _ = from;
            if label.width() > width.saturating_sub(4) {
                return None;
            }
            routes[index] = y + 1;
            y += if label.is_empty() { 2 } else { 3 };
        }
        y += 1;
    }
    if y > 4096 {
        return None;
    }
    let mut canvas = Canvas::new(width, y.saturating_sub(1));
    for (index, (from, to, _)) in edges.iter().enumerate() {
        let (fx, fy, fw, fh) = positions[*from];
        let (tx, ty, tw, _) = positions[*to];
        let (a, b, row) = (fx + fw / 2, tx + tw / 2, routes[index]);
        canvas.connect(a, fy + fh, a, row);
        canvas.connect(a, row, b, row);
        canvas.connect(b, row, b, ty - 1);
    }
    canvas.paint_wires();
    // Labels are painted after every connector so crossing edges cannot erase
    // the text of a branch condition.
    for (index, (from, to, label)) in edges.iter().enumerate() {
        let (tx, ty, tw, _) = positions[*to];
        canvas.text(tx + tw / 2, ty - 1, "▼");
        if !label.is_empty() {
            let (fx, _, fw, _) = positions[*from];
            let (tx, _, tw, _) = positions[*to];
            let (a, b, row) = (fx + fw / 2, tx + tw / 2, routes[index]);
            let x = (a.min(b) + 2).min(width.saturating_sub(label.width()));
            canvas.text(x, row + 1, label);
        }
    }
    for (index, (x, y, w, h)) in positions.into_iter().enumerate() {
        canvas.text(x, y, &format!("╭{}╮", "─".repeat(w - 2)));
        for row in 1..h - 1 {
            canvas.text(x, y + row, "│");
            if let Some(label) = labels[index].get(row - 1) {
                canvas.text(x + 2, y + row, label);
            }
            canvas.text(x + w - 1, y + row, "│");
        }
        canvas.text(x, y + h - 1, &format!("╰{}╯", "─".repeat(w - 2)));
    }
    Some(canvas.lines())
}

fn sequence(
    people: &[(String, String)],
    messages: &[(usize, usize, String, bool)],
    width: usize,
) -> Vec<String> {
    let column = width / people.len();
    if column < 12 || messages.iter().any(|(a, b, _, _)| a == b) {
        let mut lines: Vec<_> = people
            .iter()
            .flat_map(|(id, label)| wrap(&format!("[{id}] {label}"), width))
            .collect();
        lines.extend(messages.iter().flat_map(|(a, b, label, _)| {
            wrap(
                &format!("{} → {}: {label}", people[*a].1, people[*b].1),
                width,
            )
        }));
        return lines;
    }
    let mut rows = Vec::new();
    let headings: Vec<_> = people
        .iter()
        .map(|(_, name)| wrap(name, column - 2))
        .collect();
    let height = headings.iter().map(Vec::len).max().unwrap_or(1);
    let mut canvas = Canvas::new(width, height);
    for (i, lines) in headings.iter().enumerate() {
        for (y, line) in lines.iter().enumerate() {
            canvas.text(i * column + (column - line.width()) / 2, y, line);
        }
    }
    rows.extend(canvas.lines());
    for (a, b, label, dashed) in messages {
        let (from, to) = (a * column + column / 2, b * column + column / 2);
        let left = from.min(to);
        let span = from.abs_diff(to);
        let labels = wrap(label, span.saturating_sub(2).max(1));
        let mut canvas = Canvas::new(width, labels.len() + 2);
        for i in 0..people.len() {
            for y in 0..labels.len() + 2 {
                canvas.text(i * column + column / 2, y, "│");
            }
        }
        for (y, line) in labels.iter().enumerate() {
            canvas.text(left + 1, y, line);
        }
        let y = labels.len();
        canvas.text(
            left + 1,
            y,
            &if *dashed { "╌" } else { "─" }.repeat(span.saturating_sub(1)),
        );
        canvas.text(
            if from < to { to - 1 } else { to + 1 },
            y,
            if from < to { "→" } else { "←" },
        );
        rows.extend(canvas.lines());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flow_branches_merges_and_narrow_fallback_keep_labels() {
        let diagram = Diagram::parse("flowchart TD\nA[Input] --> B{Valid?}\nB -->|Yes| C[Render]\nB -->|No| D[Fallback]\nC --> E[Output]\nD --> E").unwrap();
        let wide = diagram.lines(90).join("\n");
        for label in [
            "Input", "Valid?", "Yes", "No", "Render", "Fallback", "Output", "╭", "▼",
        ] {
            assert!(wide.contains(label), "{label}: {wide}");
        }
        for width in 1..100 {
            assert!(
                diagram
                    .lines(width)
                    .iter()
                    .all(|line| line.width() <= width)
            );
        }
        assert!(diagram.lines(20).join("\n").contains("Yes → [C]"));
    }
    #[test]
    fn diagrams_reject_partial_parse_and_preserve_cycles() {
        for source in [
            "flowchart TD\nA --> B\nsubgraph Unknown",
            "sequenceDiagram\nloop unsupported",
            "pie\n\"bad\": NaN",
            "flowchart TD\nA[unclosed",
        ] {
            assert!(Diagram::parse(source).is_none(), "{source}");
        }
        let cycle = Diagram::parse("graph LR\nA[one; two] --> B; B --> A").unwrap();
        assert!(cycle.lines(40).join("\n").contains("→ [A]"));
    }
    #[test]
    fn sequence_and_pie_render_with_unicode_and_extreme_values() {
        let seq = Diagram::parse("sequenceDiagram\nparticipant U as User\nparticipant R as 界面\nU->>R: Request\nR-->>U: Reply").unwrap();
        let text = seq.lines(60).join("\n");
        for label in ["User", "界面", "Request", "Reply", "→", "←"] {
            assert!(text.contains(label), "{text}");
        }
        let pie = Diagram::parse("pie title Shares\n\"A\": 1e308\n\"B\": 1e308").unwrap();
        assert!(pie.lines(30).join("\n").contains("50.0%"));
        for width in 1..80 {
            assert!(
                seq.lines(width)
                    .iter()
                    .chain(pie.lines(width).iter())
                    .all(|line| line.width() <= width)
            );
        }
    }
}
