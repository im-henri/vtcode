//! Rendering visits adjacent lines. Keep the Rope iterator positioned there;
//! a fold/jump seeks directly instead of walking the skipped source.
pub(super) struct SourceLines<'a> {
    rope: &'a ropey::Rope,
    lines: ropey::iter::Lines<'a>,
    next: usize,
}

impl<'a> SourceLines<'a> {
    pub(super) fn new(rope: &'a ropey::Rope, first: usize) -> Self {
        let first = first.min(rope.len_lines());
        Self { rope, lines: rope.lines_at(first), next: first }
    }

    pub(super) fn get(&mut self, line: usize) -> ropey::RopeSlice<'a> {
        if line != self.next {
            self.lines = self.rope.lines_at(line);
        }
        self.next = line + 1;
        self.lines.next().expect("visible source line")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_source_lines_match_seeks_through_folds_wraps_and_reverse_jumps() {
        for text in [String::new(), "a\r\n界\r\r\n\nlast".into(), "tab\t é 界\n".repeat(2000)] {
            let rope = ropey::Rope::from_str(&text);
            let mut lines = SourceLines::new(&rope, usize::MAX);
            for line in (0..rope.len_lines()).chain((0..rope.len_lines()).rev())
                .chain((0..rope.len_lines()).step_by(11)).chain([0, 0, 0])
            {
                assert_eq!(lines.get(line), rope.line(line));
            }
        }
    }
}
