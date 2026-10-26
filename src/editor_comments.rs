use crate::editor::Editor;
use crate::editor_line_ops::LinePrefixEdit;

#[cfg(test)]
#[path = "editor_comment_performance_tests.rs"]
mod performance_tests;

struct CommentLine {
    line: usize,
    index: usize,
    indent: usize,
    nonempty: bool,
    commented: bool,
    remove: usize,
}

/// Inspect only the edges. The unchanged body can stay in the Rope, including
/// for multi-megabyte selections, Unicode whitespace and multiline comments.
fn trimmed_range(text: ropey::RopeSlice<'_>) -> std::ops::Range<usize> {
    let start = text.chars().take_while(|ch| ch.is_whitespace()).count();
    let trailing = text.chars_at(text.len_chars()).reversed()
        .take(text.len_chars() - start).take_while(|ch| ch.is_whitespace()).count();
    start..text.len_chars() - trailing
}

/// Require a complete, non-overlapping pair and remove only its outer layer.
fn uncomment_range(text: ropey::RopeSlice<'_>, open: &str, close: &str) -> Option<std::ops::Range<usize>> {
    let trimmed = trimmed_range(text);
    let opening = open.chars().count();
    let closing = close.chars().count();
    if trimmed.len() < opening + closing { return None; }
    if !text.slice(trimmed.start..trimmed.start + opening).chars().eq(open.chars())
        || !text.slice(trimmed.end - closing..trimmed.end).chars().eq(close.chars())
    { return None; }
    let inner_start = trimmed.start + opening;
    let inner = trimmed_range(text.slice(inner_start..trimmed.end - closing));
    Some(inner_start + inner.start..inner_start + inner.end)
}

enum PairedCommentAction {
    Toggle,
    Add,
    Remove,
}

struct PairedLineEdit {
    line: usize,
    index: usize,
    start_col: usize,
    old_end_col: usize,
    kept_start: usize,
    kept_end: usize,
    added_prefix: usize,
    new_end_col: usize,
}

impl Editor {
    fn edit_paired_line_comments(&mut self, open: &str, close: &str, action: PairedCommentAction) {
        crate::editor_document::edit!(self, {
        let lines = self.line_prefix_edit_lines();
        let remove = match action {
            PairedCommentAction::Add => false,
            PairedCommentAction::Remove => true,
            PairedCommentAction::Toggle => lines.clone().all(|(_, _, line)| {
                trimmed_range(line).is_empty() || uncomment_range(line, open, close).is_some()
            }),
        };
        let prefix = format!("{open} ");
        let suffix = format!(" {close}");
        let mut edits = Vec::new();
        for (line_number, line_start, line) in lines {
            let trimmed = trimmed_range(line);
            if trimmed.is_empty() { continue; }
            let start_col = trimmed.start;
            let old_end_col = trimmed.end;
            let (kept_start, kept_end, added_prefix, new_end_col) = if remove {
                let Some(inner) = uncomment_range(line, open, close) else { continue; };
                (inner.start, inner.end, 0, start_col + inner.len())
            } else {
                (start_col, old_end_col, prefix.chars().count(),
                    old_end_col + prefix.chars().count() + suffix.chars().count())
            };
            edits.push(PairedLineEdit {
                line: line_number, index: line_start + start_col, start_col,
                old_end_col, kept_start, kept_end, added_prefix, new_end_col,
            });
        }
        if edits.is_empty() { return; }
        self.desired_col = None;
        self.save_undo();
        self.mark_syntax_dirty_range(edits[0].line, edits.last().unwrap().line);
        for edit in edits.iter().rev() {
            let line_start = edit.index - edit.start_col;
            if remove {
                self.rope.remove(line_start + edit.kept_end..line_start + edit.old_end_col);
                self.rope.remove(edit.index..line_start + edit.kept_start);
            } else {
                self.rope.insert(line_start + edit.old_end_col, &suffix);
                self.rope.insert(edit.index, &prefix);
            }
        }
        // Each caret/anchor is mapped once, including carets in indentation,
        // delimiters and trailing whitespace. Unchanged rows remain untouched.
        let remap = |(line, col): &mut (usize, usize)| {
            if let Ok(index) = edits.binary_search_by_key(line, |edit| edit.line) {
                let edit = &edits[index];
                if *col > edit.old_end_col {
                    *col = edit.new_end_col + *col - edit.old_end_col;
                } else if *col >= edit.start_col {
                    *col = edit.start_col + edit.added_prefix
                        + (*col).clamp(edit.kept_start, edit.kept_end) - edit.kept_start;
                }
            }
        };
        let mut cursor = (self.cursor_line, self.cursor_col);
        remap(&mut cursor);
        (self.cursor_line, self.cursor_col) = cursor;
        if let Some(anchor) = self.selection_anchor.as_mut() { remap(anchor); }
        for cursor in &mut self.extra_cursors { remap(cursor); }
        for anchor in self.extra_cursor_anchors.iter_mut().flatten() { remap(anchor); }
        self.modified = true;
        self.ensure_visible();
            })
    }

    pub(crate) fn toggle_line_comment(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        // For HTML/XML, use paired comment wrapping (<!-- ... -->)
        if let Some((open, close)) = self.guess_line_comment_pair() {
            self.edit_paired_line_comments(open, close, PairedCommentAction::Toggle);
            return;
        }

        let prefix = self.guess_comment_prefix();
        let lines = self.comment_line_metadata(prefix);
        let all_commented = lines.iter().all(|line| line.commented);
        let min_indent = lines.iter().filter(|line| line.nonempty).map(|line| line.indent).min().unwrap_or(0);
        let insert = format!("{prefix} ");
        let edits: Vec<_> = lines.iter().map(|line| LinePrefixEdit {
            line: line.line,
            index: line.index + if all_commented { line.indent } else { min_indent.min(line.indent) },
            col: if all_commented { line.indent } else { min_indent.min(line.indent) },
            removed: if all_commented { line.remove } else { 0 },
            insert: if all_commented { "" } else { &insert },
        }).collect();
        self.apply_line_prefix_edits(&edits);
            })
    }

    /// Read just the indentation and comment prefix once per line. Long code
    /// lines no longer get copied three times to toggle a two-character marker.
    fn comment_line_metadata(&self, prefix: &str) -> Vec<CommentLine> {
        self.line_prefix_edit_lines().map(|(line_number, index, line)| {
            let indent = line.chars().take_while(|&c| c.is_whitespace() && c != '\n' && c != '\r').count();
            let mut chars = line.chars_at(indent);
            let nonempty = chars.clone().next().is_some_and(|c| c != '\n' && c != '\r');
            let commented = prefix.chars().all(|expected| chars.next() == Some(expected));
            let remove = if commented { prefix.len() + usize::from(chars.next() == Some(' ')) } else { 0 };
            CommentLine { line: line_number, index, indent, nonempty, commented, remove }
        }).collect()
    }

    /// Add line comment to all lines in selection/current line (without toggling)
    pub(crate) fn add_line_comment(&mut self) {
        crate::editor_document::edit!(self, {
        if let Some((open, close)) = self.guess_line_comment_pair() {
            self.edit_paired_line_comments(open, close, PairedCommentAction::Add);
            return;
        }
        let prefix = self.guess_comment_prefix();
        let lines = self.comment_line_metadata(prefix);
        let insert = format!("{prefix} ");
        let edits: Vec<_> = lines.iter().map(|line| LinePrefixEdit {
            line: line.line, index: line.index + line.indent, col: line.indent, removed: 0, insert: &insert,
        }).collect();
        self.apply_line_prefix_edits(&edits);
            })
    }

    /// Remove line comment from all lines in selection/current line (without toggling)
    pub(crate) fn remove_line_comment(&mut self) {
        crate::editor_document::edit!(self, {
        if let Some((open, close)) = self.guess_line_comment_pair() {
            self.edit_paired_line_comments(open, close, PairedCommentAction::Remove);
            return;
        }
        let prefix = self.guess_comment_prefix();
        let lines = self.comment_line_metadata(prefix);
        let edits: Vec<_> = lines.iter().filter(|line| line.commented).map(|line| LinePrefixEdit {
            line: line.line, index: line.index + line.indent, col: line.indent, removed: line.remove, insert: "",
        }).collect();
        self.apply_line_prefix_edits(&edits);
            })
    }

    pub(crate) fn guess_comment_prefix(&self) -> &'static str {
        let ext = self.file_path.as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "py" | "rb" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "r" | "pl" | "pm"
            | "ini" | "cfg" | "conf" | "dockerfile" | "makefile" | "mk" | "cmake" | "gitignore" => "#",
            "lua" | "hs" | "sql" | "ads" | "adb" => "--",
            "bat" | "cmd" => "REM",
            "asm" | "s" | "lisp" | "cl" | "clj" | "cljs" | "edn" | "scm" | "rkt" => ";",
            "vim" => "\"",
            "erl" | "hrl" | "ex" | "exs" | "latex" | "tex" => "%",
            "f90" | "f95" | "f03" | "f08" => "!",
            _ => "//",
        }
    }

    /// For languages that only have block-style comments (HTML/XML), returns (open, close).
    pub(crate) fn guess_line_comment_pair(&self) -> Option<(&'static str, &'static str)> {
        let ext = self.file_path.as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "html" | "htm" | "xml" | "svg" | "vue" | "svelte" => Some(("<!--", "-->")),
            "css" | "scss" | "less" => Some(("/*", "*/")),
            _ => None,
        }
    }

    pub(crate) fn guess_block_comment(&self) -> Option<(&'static str, &'static str)> {
        let ext = self.file_path.as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "rs" | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "cs" |
            "java" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" |
            "tsx" | "jsx" | "go" | "swift" | "kt" | "scala" | "dart" |
            "php" | "css" | "scss" | "less" | "sass" => Some(("/*", "*/")),
            "html" | "htm" | "xml" | "svg" | "vue" | "svelte" => Some(("<!--", "-->")),
            "lua" => Some(("--[[", "]]")),
            _ => None,
        }
    }

    pub(crate) fn toggle_block_comment(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let Some((open, close)) = self.guess_block_comment() else { return; };

        // Compute the implicit whole-line range without changing the selection
        // captured by undo.
        let (start_idx, end_idx, sl) = if let Some((start, end, line, _)) = self.clamped_selection_indices() {
            (start, end, line)
        } else {
            let line = self.cursor_line.min(self.line_count() - 1);
            let start = self.rope.line_to_char(line);
            (start, start + self.line_len(line), line)
        };
        if start_idx == end_idx { return; }
        let inner = uncomment_range(self.rope.slice(start_idx..end_idx), open, close);

        self.save_undo();
        let end_line = self.rope.char_to_line(end_idx);
        // Capture secondary carets in the original document before a multiline
        // uncomment can remove their rows.
        let position = |(line, col): (usize, usize)| {
            let line = line.min(self.line_count() - 1);
            self.rope.line_to_char(line) + col.min(self.line_len(line))
        };
        let extra_positions: Vec<_> = self.extra_cursors.iter().copied().map(position).collect();
        let extra_anchors: Vec<_> = self.extra_cursor_anchors.iter().map(|anchor| anchor.map(position)).collect();

        // Check if already wrapped in block comment
        let (kept_start, kept_end, added_prefix, new_end) = if let Some(inner) = inner {
            let kept_start = start_idx + inner.start;
            let kept_end = start_idx + inner.end;
            self.rope.remove(kept_end..end_idx);
            self.rope.remove(start_idx..kept_start);
            (kept_start, kept_end, 0, start_idx + inner.len())
        } else {
            let prefix = format!("{open} ");
            let suffix = format!(" {close}");
            self.rope.insert(end_idx, &suffix);
            self.rope.insert(start_idx, &prefix);
            (start_idx, end_idx, prefix.chars().count(),
                end_idx + prefix.chars().count() + suffix.chars().count())
        };
        let remap = |index: usize| {
            let index = if index < start_idx { index }
                else if index > end_idx { new_end + index - end_idx }
                else { start_idx + added_prefix + index.clamp(kept_start, kept_end) - kept_start };
            let line = self.rope.char_to_line(index);
            (line, index - self.rope.line_to_char(line))
        };
        for (cursor, index) in self.extra_cursors.iter_mut().zip(extra_positions) { *cursor = remap(index); }
        for (anchor, index) in self.extra_cursor_anchors.iter_mut().zip(extra_anchors) { *anchor = index.map(remap); }
        self.cursor_line = self.rope.char_to_line(new_end);
        self.cursor_col = new_end - self.rope.line_to_char(self.cursor_line);
        self.mark_syntax_dirty_range(sl, end_line.max(self.cursor_line));

        self.modified = true;
        self.clear_selection();
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use ropey::Rope;
    use std::path::PathBuf;

    // ── toggle_line_comment ──────────────────────────────────────

    #[test]
    fn toggle_comment_add_rust() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("let x = 1;");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.toggle_line_comment();
        assert_eq!(ed.rope.line(0).to_string(), "// let x = 1;");
    }

    #[test]
    fn toggle_comment_remove_rust() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("// let x = 1;");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.toggle_line_comment();
        assert_eq!(ed.rope.line(0).to_string(), "let x = 1;");
    }

    #[test]
    fn line_comment_prefixes_follow_the_file_extension() {
        let mut ed = Editor::new();
        for (path, expected) in [
            (Some("main.rs"), "//"),
            (Some("index.html"), "//"),
            (Some("app.py"), "#"),
            (Some("script.lua"), "--"),
            (None, "//"),
            (Some("script.sh"), "#"),
            (Some("query.sql"), "--"),
            (Some("doc.tex"), "%"),
            (Some("run.bat"), "REM"),
            (Some("code.f90"), "!"),
            (Some("core.clj"), ";"),
        ] {
            ed.file_path = path.map(PathBuf::from);
            assert_eq!(ed.guess_comment_prefix(), expected, "{path:?}");
        }
    }

    #[test]
    fn paired_line_comments_distinguish_markup_css_and_rust() {
        let mut ed = Editor::new();
        for (path, expected) in [
            ("index.html", Some(("<!--", "-->"))),
            ("style.css", Some(("/*", "*/"))),
            ("main.rs", None),
        ] {
            ed.file_path = Some(PathBuf::from(path));
            assert_eq!(ed.guess_line_comment_pair(), expected, "{path}");
        }
    }

    #[test]
    fn block_comment_delimiters_follow_the_file_extension() {
        let mut ed = Editor::new();
        for (path, expected) in [
            ("lib.rs", Some(("/*", "*/"))),
            ("page.html", Some(("<!--", "-->"))),
            ("init.lua", Some(("--[[", "]]"))),
            ("app.py", None),
            ("notes.txt", None),
        ] {
            ed.file_path = Some(PathBuf::from(path));
            assert_eq!(ed.guess_block_comment(), expected, "{path}");
        }
    }

    // ── toggle_line_comment (more cases) ────────────────────────

    #[test]
    fn toggle_comment_python() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("app.py"));
        ed.rope = Rope::from_str("x = 1");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.toggle_line_comment();
        assert_eq!(ed.rope.to_string(), "# x = 1");
        // Toggle again removes the comment
        ed.cursor_col = 0;
        ed.toggle_line_comment();
        assert_eq!(ed.rope.to_string(), "x = 1");
    }

    #[test]
    fn toggle_comment_html_paired() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("index.html"));
        ed.rope = Rope::from_str("<div>hello</div>");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.toggle_line_comment();
        assert_eq!(ed.rope.to_string(), "<!-- <div>hello</div> -->");
        // Toggle again removes the wrapping
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.toggle_line_comment();
        assert_eq!(ed.rope.to_string(), "<div>hello</div>");
    }

    #[test]
    fn toggle_comment_preserves_indentation() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("    let x = 1;\n    let y = 2;\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 1;
        ed.cursor_col = 14;
        ed.toggle_line_comment();
        let result = ed.rope.to_string();
        assert!(result.contains("    // let x = 1;"));
        assert!(result.contains("    // let y = 2;"));
    }

    // ── add_line_comment ────────────────────────────────────────

    #[test]
    fn add_line_comment_always_adds() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("// already");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.add_line_comment();
        // Should add a second comment prefix (not toggle off)
        assert_eq!(ed.rope.line(0).to_string(), "// // already");
    }

    #[test]
    fn add_line_comment_html_wraps() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("page.html"));
        ed.rope = Rope::from_str("<p>text</p>");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.add_line_comment();
        assert_eq!(ed.rope.line(0).to_string(), "<!-- <p>text</p> -->");
    }

    // ── remove_line_comment ─────────────────────────────────────

    #[test]
    fn remove_line_comment_strips_prefix() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("// hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.remove_line_comment();
        assert_eq!(ed.rope.to_string(), "hello world");
    }

    #[test]
    fn remove_line_comment_noop_when_no_comment() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.remove_line_comment();
        assert_eq!(ed.rope.to_string(), "hello world");
    }

    #[test]
    fn remove_line_comment_html_unwraps() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("page.html"));
        ed.rope = Rope::from_str("<!-- <p>hi</p> -->");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.remove_line_comment();
        assert_eq!(ed.rope.line(0).to_string(), "<p>hi</p>");
    }

    // ── toggle_block_comment ────────────────────────────────────

    #[test]
    fn toggle_block_comment_add() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("let x = 1;");
        // Select "x = 1" (cols 4..9)
        ed.cursor_line = 0;
        ed.selection_anchor = Some((0, 4));
        ed.cursor_col = 9;
        ed.toggle_block_comment();
        assert!(ed.rope.to_string().contains("/* x = 1 */"));
    }

    #[test]
    fn toggle_block_comment_remove() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        ed.rope = Rope::from_str("let /* x = 1 */ ;");
        // Select the block-commented part "/* x = 1 */"
        ed.cursor_line = 0;
        ed.selection_anchor = Some((0, 4));
        ed.cursor_col = 15;
        ed.toggle_block_comment();
        assert!(ed.rope.to_string().contains("x = 1"));
        assert!(!ed.rope.to_string().contains("/*"));
    }

    #[test]
    fn toggle_block_comment_noop_for_python() {
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("app.py"));
        ed.rope = Rope::from_str("x = 1");
        ed.cursor_line = 0;
        ed.selection_anchor = Some((0, 0));
        ed.cursor_col = 5;
        ed.toggle_block_comment();
        // Python has no block comment, should be unchanged
        assert_eq!(ed.rope.to_string(), "x = 1");
    }
}
