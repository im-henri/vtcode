// Independent, allocating specification. Preserve every logical separator,
// including consecutive CR/CRLF; the former frozen implementation lost rows.
use super::*;
impl Editor {
    fn reference_line_endings(&mut self, crlf: bool) {
        let source = self.rope.to_string();
        let lf = source.replace("\r\n", "\n").replace('\r', "\n");
        let converted = if crlf { lf.replace('\n', "\r\n") } else { lf };
        if converted != source {
            let rope = Rope::from_str(&converted);
            assert_eq!(rope.len_lines(), self.rope.len_lines());
            let changed: Vec<_> = self.rope.lines().zip(rope.lines()).enumerate()
                .filter_map(|(line, (before, after))| (before != after).then_some(line)).collect();
            self.save_undo();
            self.rope = rope;
            self.mark_syntax_dirty_range(changed[0], *changed.last().unwrap());
            self.modified = true;
        }
        self.cached_line_ending = if crlf { "CRLF" } else { "LF" };
    }

    pub(crate) fn reference_line_endings_to_lf(&mut self) {
        self.reference_line_endings(false);
    }

    pub(crate) fn reference_line_endings_to_crlf(&mut self) {
        self.reference_line_endings(true);
    }
}
