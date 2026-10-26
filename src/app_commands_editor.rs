use crate::app::App;

impl App {
    /// Dispatch editor text-editing / transform commands.
    pub(crate) fn execute_editor_command(&mut self, id: &str) -> bool {
        if crate::ui_logs::focused(self) && id != "file_associations" { return false; }
        if let Some(language) = crate::app_menu::language_from_command_id(id) {
            self.set_editor_language(Some(language));
            self.notify(format!("Language: {language}"));
            return false;
        }
        match id {
            "file_associations" => self.open_file_associations(),
            "duplicate_line" => { if let Some(ed) = self.editor_mut_writable() { ed.duplicate_line(); } }
            "duplicate_line_up" => { if let Some(ed) = self.editor_mut_writable() { ed.duplicate_line_up(); } }
            "delete_line" => { if let Some(ed) = self.editor_mut_writable() { ed.delete_line(); } }
            "delete_to_line_start" => { if let Some(ed) = self.editor_mut_writable() { ed.delete_to_line_start(); } }
            "delete_to_line_end" => { if let Some(ed) = self.editor_mut_writable() { ed.delete_to_line_end(); } }
            "file_stats" => {
                if let Some(ed) = self.editor() {
                    let (lines, non_blank, words, chars) = ed.compute_file_stats();
                    self.notify(format!("{lines} lines ({non_blank} non-blank), {words} words, {chars} chars"));
                }
            }
            "fold_all" => {
                if let Some(ed) = self.editor_mut() { ed.fold_all(); }
                self.notify("All regions folded");
            }
            "unfold_all" => {
                if let Some(ed) = self.editor_mut() { ed.unfold_all(); }
                self.notify("All regions unfolded");
            }
            "fold_level_1" => {
                if let Some(ed) = self.editor_mut() { ed.fold_at_level(1); }
                self.notify("Folded at level 1");
            }
            "fold_level_2" => {
                if let Some(ed) = self.editor_mut() { ed.fold_at_level(2); }
                self.notify("Folded at level 2");
            }
            "fold_level_3" => {
                if let Some(ed) = self.editor_mut() { ed.fold_at_level(3); }
                self.notify("Folded at level 3");
            }
            "fold_level_4" => {
                if let Some(ed) = self.editor_mut() { ed.fold_at_level(4); }
                self.notify("Folded at level 4");
            }
            "fold_level_5" => {
                if let Some(ed) = self.editor_mut() { ed.fold_at_level(5); }
                self.notify("Folded at level 5");
            }
            "remove_empty_lines" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.save_undo();
                    ed.mark_syntax_dirty_range(0, ed.rope.len_lines());
                    for i in (0..ed.rope.len_lines()).rev() {
                        let is_blank = ed.rope.line(i).chars().all(char::is_whitespace);
                        if is_blank {
                            let start = ed.rope.line_to_char(i);
                            let end = if i + 1 < ed.rope.len_lines() {
                                ed.rope.line_to_char(i + 1)
                            } else {
                                ed.rope.len_chars()
                            };
                            if start < end {
                                ed.rope.remove(start..end);
                            }
                        }
                    }
                    if ed.rope.len_chars() > 0 && ed.rope.char(ed.rope.len_chars() - 1) != '\n' {
                        ed.rope.insert_char(ed.rope.len_chars(), '\n');
                    }
                    ed.modified = true;
                    ed.clamp_cursor();
                }
                self.notify("Empty lines removed");
            }
            "merge_blank_lines" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.save_undo();
                    ed.mark_syntax_dirty_range(0, ed.rope.len_lines());
                    let total = ed.rope.len_lines();
                    let mut i = total.saturating_sub(1);
                    while i > 0 {
                        let cur_blank = ed.rope.line(i).chars().all(char::is_whitespace);
                        let prev_blank = ed.rope.line(i - 1).chars().all(char::is_whitespace);
                        if cur_blank && prev_blank {
                            let start = ed.rope.line_to_char(i);
                            let end = if i + 1 < ed.rope.len_lines() {
                                ed.rope.line_to_char(i + 1)
                            } else {
                                ed.rope.len_chars()
                            };
                            if start < end {
                                ed.rope.remove(start..end);
                            }
                        }
                        i -= 1;
                    }
                    if ed.rope.len_chars() > 0 && ed.rope.char(ed.rope.len_chars() - 1) != '\n' {
                        ed.rope.insert_char(ed.rope.len_chars(), '\n');
                    }
                    ed.modified = true;
                    ed.clamp_cursor();
                }
                self.notify("Consecutive blank lines merged");
            }
            "toggle_comment" => { if let Some(ed) = self.editor_mut_writable() { ed.toggle_line_comment(); } }
            "toggle_block_comment" => { if let Some(ed) = self.editor_mut_writable() { ed.toggle_block_comment(); } }
            "select_to_bracket" => { if let Some(ed) = self.editor_mut() { ed.select_to_matching_bracket(); } }
            "select_inside_brackets" => { if let Some(ed) = self.editor_mut() { ed.select_inside_brackets(); } }
            "select_inside_quotes" => { if let Some(ed) = self.editor_mut() { ed.select_inside_quotes(); } }
            "move_line_up" => { if let Some(ed) = self.editor_mut_writable() { ed.move_line_up(); } }
            "move_line_down" => { if let Some(ed) = self.editor_mut_writable() { ed.move_line_down(); } }
            "fold" => {
                if let Some(ed) = self.editor_mut() {
                    let line = ed.cursor_line;
                    if ed.find_fold_end(line).is_some() && !ed.is_fold_start(line) {
                        ed.toggle_fold(line);
                    }
                }
            }
            "unfold" => {
                if let Some(ed) = self.editor_mut() {
                    let line = ed.cursor_line;
                    if ed.is_fold_start(line) {
                        ed.toggle_fold(line);
                    }
                }
            }
            "sort_imports" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.sort_imports();
                }
                self.notify("Imports sorted");
            }
            "change_language" => {
                self.clear_drag_states();
                self.refresh_palette_commands();
                self.palette.active = true;
                self.palette.query = "Language: ".to_string();
                self.palette.cursor = self.palette.query.len();
                self.palette.selection_anchor = None;
                self.palette.selected = 0;
                self.palette.scroll_offset = 0;
                self.update_palette_filter();
            }
            "lang_auto" => {
                self.set_editor_language(None);
                self.notify("Language: Auto-detect");
            }
            "eol_lf" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.convert_line_endings_to_lf();
                    self.notify("Line endings: LF");
                }
            }
            "eol_crlf" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.convert_line_endings_to_crlf();
                    self.notify("Line endings: CRLF");
                }
            }
            "encoding_utf8" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.set_encoding("UTF-8");
                    self.notify("Encoding set to UTF-8 (save to re-encode)");
                }
            }
            "encoding_utf8bom" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.set_encoding("UTF-8 with BOM");
                    self.notify("Encoding set to UTF-8 with BOM (save to re-encode)");
                }
            }
            "encoding_utf16le" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.set_encoding("UTF-16 LE");
                    self.notify("Encoding set to UTF-16 LE (save to re-encode)");
                }
            }
            "encoding_utf16be" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.set_encoding("UTF-16 BE");
                    self.notify("Encoding set to UTF-16 BE (save to re-encode)");
                }
            }
            "encoding_ascii" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.set_encoding("ASCII");
                    self.notify("Encoding set to ASCII (save to re-encode)");
                }
            }
            "encoding_latin1" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.set_encoding("ISO-8859-1");
                    self.notify("Encoding set to ISO-8859-1 (save to re-encode)");
                }
            }
            "reopen_with_encoding" => {
                self.palette.active = true;
                self.palette.query = "Encoding".to_string();
                self.palette.cursor = self.palette.query.len();
                self.palette.selection_anchor = None;
                self.update_palette_filter();
                self.notify("Select encoding, then the file will be saved with it");
            }
            "open_file_at_cursor" => {
                if let Some(ed) = self.editor() {
                    let line_text = ed.rope.line(ed.cursor_line).to_string();
                    let chars: Vec<char> = line_text.chars().collect();
                    let is_path_char = |c: char| c.is_alphanumeric() || c == '/' || c == '\\' || c == '.' || c == '_' || c == '-';
                    let col = ed.cursor_col.min(chars.len().saturating_sub(1));
                    let mut start = col;
                    while start > 0 && is_path_char(chars[start - 1]) {
                        start -= 1;
                    }
                    let mut end = col;
                    while end < chars.len() && is_path_char(chars[end]) {
                        end += 1;
                    }
                    if end > start {
                        let path_str: String = chars[start..end].iter().collect();
                        let base = self.explorer.root.clone();
                        let full_path = base.join(&path_str);
                        if full_path.exists() && full_path.is_file() {
                            self.open_file(&full_path);
                        } else {
                            self.notify(format!("File not found: {path_str}"));
                        }
                    }
                }
            }
            "to_uppercase" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.transform_selection(str::to_uppercase);
                }
                self.notify("Converted to UPPERCASE");
            }
            "to_lowercase" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.transform_selection(str::to_lowercase);
                }
                self.notify("Converted to lowercase");
            }
            "toggle_case" => {
                if let Some(ed) = self.editor_mut_writable() { ed.toggle_case(); }
                self.notify("Case toggled");
            }
            "trim_trailing" => {
                if let Some(ed) = self.editor_mut_writable() {
                    if ed.trim_trailing_whitespace() {
                        self.notify("Trailing whitespace trimmed");
                    } else {
                        self.notify("No trailing whitespace found");
                    }
                }
            }
            "insert_line_above" => {
                if let Some(ed) = self.editor_mut_writable() { ed.insert_line_above(); }
            }
            "insert_line_below" => {
                if let Some(ed) = self.editor_mut_writable() { ed.insert_line_below(); }
            }
            "goto_bracket" => {
                if let Some(ed) = self.editor_mut() { ed.go_to_matching_bracket(); }
            }
            "paragraph_up" => {
                if let Some(ed) = self.editor_mut() { ed.move_paragraph_up(); }
            }
            "paragraph_down" => {
                if let Some(ed) = self.editor_mut() { ed.move_paragraph_down(); }
            }
            "select_paragraph" => {
                if let Some(ed) = self.editor_mut() { ed.select_paragraph(); }
            }
            "center_cursor" => {
                if let Some(ed) = self.editor_mut() { ed.center_cursor(); }
                self.notify("Cursor centered in viewport");
            }
            "join_lines" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.join_lines();
                    self.notify("Lines joined");
                }
            }
            "indent_2" => {
                if let Some(ed) = self.editor_mut() { ed.tab_size = 2; }
                self.notify("Indent: 2 Spaces");
            }
            "indent_4" => {
                if let Some(ed) = self.editor_mut() { ed.tab_size = 4; }
                self.notify("Indent: 4 Spaces");
            }
            "indent_8" => {
                if let Some(ed) = self.editor_mut() { ed.tab_size = 8; }
                self.notify("Indent: 8 Spaces");
            }
            "expand_selection" => {
                self.expand_editor_selection();
            }
            "shrink_selection" => {
                if !self.lsp_selection_context_current() { self.clear_lsp_selection_context(); }
                if let Some(ed) = self.editor_mut() { ed.shrink_selection(); }
                self.track_lsp_selection_cursor();
            }
            "remove_duplicates" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.remove_duplicate_lines();
                }
                self.notify("Duplicate lines removed");
            }
            "remove_blank_lines" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.remove_blank_lines();
                }
                self.notify("Blank lines removed");
            }
            "reverse_lines" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.reverse_lines();
                }
                self.notify("Lines reversed");
            }
            "shuffle_lines" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.shuffle_lines();
                }
                self.notify("Lines shuffled");
            }
            "to_camel" => {
                if let Some(ed) = self.editor_mut_writable() { ed.convert_to_camel_case(); }
                self.notify("Converted to camelCase");
            }
            "to_pascal" => {
                if let Some(ed) = self.editor_mut_writable() { ed.convert_to_pascal_case(); }
                self.notify("Converted to PascalCase");
            }
            "to_snake" => {
                if let Some(ed) = self.editor_mut_writable() { ed.convert_to_snake_case(); }
                self.notify("Converted to snake_case");
            }
            "to_constant" => {
                if let Some(ed) = self.editor_mut_writable() { ed.convert_to_constant_case(); }
                self.notify("Converted to CONSTANT_CASE");
            }
            "to_kebab" => {
                if let Some(ed) = self.editor_mut_writable() { ed.convert_to_kebab_case(); }
                self.notify("Converted to kebab-case");
            }
            "to_title_case" => {
                if let Some(ed) = self.editor_mut_writable() { ed.convert_to_title_case(); }
                self.notify("Converted to Title Case");
            }
            "indent_line" => {
                if let Some(ed) = self.editor_mut_writable() { ed.indent_lines(); }
            }
            "outdent_line" => {
                if let Some(ed) = self.editor_mut_writable() { ed.dedent_line(); }
            }
            "base64_encode" => {
                if let Some(ed) = self.editor_mut_writable() {
                    if ed.base64_encode_selection() {
                        self.notify("Base64 encoded");
                    } else {
                        self.notify("Select text to encode");
                    }
                }
            }
            "base64_decode" => {
                if let Some(ed) = self.editor_mut_writable() {
                    if ed.base64_decode_selection() {
                        self.notify("Base64 decoded");
                    } else {
                        self.notify("Select text to decode");
                    }
                }
            }
            "url_encode" => {
                if let Some(ed) = self.editor_mut_writable() {
                    if ed.url_encode_selection() {
                        self.notify("URL encoded");
                    } else {
                        self.notify("Select text to encode");
                    }
                }
            }
            "url_decode" => {
                if let Some(ed) = self.editor_mut_writable() {
                    if ed.url_decode_selection() {
                        self.notify("URL decoded");
                    } else {
                        self.notify("Select text to decode");
                    }
                }
            }
            "word_count" => {
                if let Some(ed) = self.editor() {
                    let mut words = 0usize;
                    let mut in_word = false;
                    for chunk in ed.rope.chunks() {
                        for ch in chunk.chars() {
                            if ch.is_whitespace() {
                                in_word = false;
                            } else {
                                if !in_word { words += 1; }
                                in_word = true;
                            }
                        }
                    }
                    let chars = ed.rope.len_chars();
                    let lines = ed.line_count();
                    self.notify(format!("{lines} lines, {words} words, {chars} characters"));
                }
            }
            "char_count" => {
                if let Some(ed) = self.editor() {
                    let chars = ed.rope.len_chars();
                    let bytes = ed.rope.len_bytes();
                    self.notify(format!("{chars} characters, {bytes} bytes"));
                }
            }
            "transpose_chars" => {
                if let Some(ed) = self.editor_mut_writable() { ed.transpose_chars(); }
            }
            "transpose_words" => {
                if let Some(ed) = self.editor_mut_writable() { ed.transpose_words(); }
            }
            "surround_parens" => {
                if let Some(ed) = self.editor_mut_writable() { ed.surround_selection("(", ")"); }
            }
            "surround_brackets" => {
                if let Some(ed) = self.editor_mut_writable() { ed.surround_selection("[", "]"); }
            }
            "surround_braces" => {
                if let Some(ed) = self.editor_mut_writable() { ed.surround_selection("{", "}"); }
            }
            "surround_double_quotes" => {
                if let Some(ed) = self.editor_mut_writable() { ed.surround_selection("\"", "\""); }
            }
            "surround_single_quotes" => {
                if let Some(ed) = self.editor_mut_writable() { ed.surround_selection("'", "'"); }
            }
            "surround_backticks" => {
                if let Some(ed) = self.editor_mut_writable() { ed.surround_selection("`", "`"); }
            }
            "surround_tag" => {
                if let Some(ed) = self.editor_mut_writable() { ed.surround_selection("<div>", "</div>"); }
            }
            "next_change" => {
                if let Some(ed) = self.editor_mut() { ed.next_change(); }
            }
            "indent_to_spaces" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.convert_indentation_to_spaces();
                }
                self.notify("Converted indentation to spaces");
            }
            "indent_to_tabs" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.convert_indentation_to_tabs();
                }
                self.notify("Converted indentation to tabs");
            }
            "toggle_indent_mode" => {
                if let Some(ed) = self.editor_mut() {
                    ed.use_tabs = !ed.use_tabs;
                    let mode = if ed.use_tabs { "Tabs" } else { "Spaces" };
                    self.notify(format!("Indent mode: {mode}"));
                }
            }
            "reindent_lines" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.reindent_lines();
                    self.notify("Reindented lines");
                }
            }
            "toggle_typewriter" => {
                if let Some(ed) = self.editor_mut() {
                    ed.typewriter_mode = !ed.typewriter_mode;
                    let state = if ed.typewriter_mode { "enabled" } else { "disabled" };
                    self.notify(format!("Typewriter mode {state}"));
                }
            }
            "toggle_read_only" => {
                if let Some(ed) = self.editor_mut() {
                    ed.set_read_only(!ed.read_only);
                    let state = if ed.read_only { "enabled" } else { "disabled" };
                    self.notify(format!("Read-only mode {state}"));
                }
            }
            "prev_change" => {
                if let Some(ed) = self.editor_mut() { ed.prev_change(); }
            }
            "revert_file" => {
                if let Some(ed) = self.editor_mut_writable() {
                    match ed.reload_from_disk() {
                        Ok(()) => self.notify("File reverted to saved version"),
                        Err(e) => self.notify_error(format!("Revert failed: {e}")),
                    }
                }
            }
            "format_json" => {
                if let Some(ed) = self.editor_mut_writable() {
                    match ed.format_json() {
                        Ok(()) => self.notify("JSON formatted"),
                        Err(e) => self.notify(e),
                    }
                }
            }
            "minify_json" => {
                if let Some(ed) = self.editor_mut_writable() {
                    match ed.minify_json() {
                        Ok(()) => self.notify("JSON minified"),
                        Err(e) => self.notify(e),
                    }
                }
            }
            "sort_json_keys" => {
                if let Some(ed) = self.editor_mut_writable() {
                    match ed.sort_json_keys() {
                        Ok(()) => self.notify("JSON keys sorted"),
                        Err(e) => self.notify(e),
                    }
                }
            }
            "tab_size_2" => {
                if let Some(ed) = self.editor_mut() { ed.tab_size = 2; }
                self.notify("Tab size: 2");
            }
            "tab_size_4" => {
                if let Some(ed) = self.editor_mut() { ed.tab_size = 4; }
                self.notify("Tab size: 4");
            }
            "tab_size_8" => {
                if let Some(ed) = self.editor_mut() { ed.tab_size = 8; }
                self.notify("Tab size: 8");
            }
            "detect_indentation" => {
                if let Some(ed) = self.editor_mut() {
                    ed.detect_indentation();
                    let style = if ed.use_tabs { "Tabs".to_string() } else { format!("{} Spaces", ed.tab_size) };
                    self.notify(format!("Detected: {style}"));
                }
            }
            _ => return false,
        }
        false
    }
}

impl App {
    fn lsp_selection_editor(&self) -> Option<&crate::editor::Editor> {
        let tab = self.tabs.get(self.active_tab)?;
        // A revision/diff must never replace the working document on a server.
        (tab.kind == crate::app::TabKind::File && !self.diff_selection_owns_cursor()
            && !tab.editor.bg_loading && !tab.editor.large_file_mode).then_some(&tab.editor)
    }

    pub(crate) fn lsp_selection_context_current(&self) -> bool {
        self.lsp_selection_range_source.as_ref().zip(self.lsp_selection_editor()).is_some_and(|((id, path, source), ed)| {
            *id == ed.navigation_id && ed.file_path.as_ref() == Some(path)
                && source.is_instance(&ed.rope)
                && self.lsp_selection_range_cursor == (ed.cursor_line, ed.cursor_col)
        })
    }

    pub(crate) fn clear_lsp_selection_context(&mut self) {
        self.lsp_selection_range_source = None;
        self.lsp_selection_ranges.clear();
        self.lsp_selection_range_cursor = (usize::MAX, usize::MAX);
        self.lsp_manager.cancel_selection_range_request();
    }

    fn track_lsp_selection_cursor(&mut self) {
        if self.lsp_selection_range_source.is_some() {
            if let Some(ed) = self.editor() {
                self.lsp_selection_range_cursor = (ed.cursor_line, ed.cursor_col);
            }
        }
    }

    fn expand_editor_selection(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) { tab.editor.refresh_document(); }
        let current = self.lsp_selection_context_current();
        if !current { self.clear_lsp_selection_context(); }
        let range = self.editor().and_then(|ed| {
            let (sl, sc, el, ec) = ed.selection_range().unwrap_or(
                (ed.cursor_line, ed.cursor_col, ed.cursor_line, ed.cursor_col));
            self.lsp_selection_ranges.iter().copied()
                .find(|&(rsl, rsc, rel, rec)| {
                    (rsl, rsc) <= (sl, sc) && (rel, rec) >= (el, ec)
                        && ((rsl, rsc) < (sl, sc) || (rel, rec) > (el, ec))
                })
        });
        if let Some((sl, sc, el, ec)) = range {
            if let Some(ed) = self.editor_mut() {
                ed.remember_selection();
                ed.set_cursor(el, ec);
                ed.selection_anchor = Some((sl, sc));
            }
        } else {
            if !current {
                if let Some(ed) = self.lsp_selection_editor() {
                    if let Some(path) = ed.file_path.clone() {
                        let id = ed.navigation_id;
                        let version = ed.content_version;
                        let source = ed.rope.clone();
                        let pos = lsp_types::Position {
                            line: ed.cursor_line as u32,
                            character: crate::lsp_positions::to_utf16(&source, ed.cursor_line, ed.cursor_col),
                        };
                        // The request must follow the exact text it describes.
                        self.lsp_manager.sync_document(&path, version, source.clone());
                        if self.lsp_manager.request_selection_range(&path, vec![pos]) {
                            self.lsp_selection_range_source = Some((id, path, source));
                        }
                    }
                }
            }
            if let Some(ed) = self.editor_mut() { ed.expand_selection(); }
        }
        // Local fallback expansion moves the caret while the server works.
        // Keep that movement valid; independent movement still retires replies.
        self.track_lsp_selection_cursor();
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, Tab};
    use crate::editor::Editor;

    /// Push a single editor tab with the given content and set it active.
    fn app_with_content(text: &str) -> App {
        let mut app = App::test_new();
        let editor = Editor::test_with_text(text);
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app
    }

    #[test]
    fn edge_case_read_only_palette_commands_preserve_content_and_history() {
        for command in [
            "undo", "redo", "toggle_comment", "toggle_block_comment",
            "move_line_up", "move_line_down", "transpose_chars", "transpose_words",
            "eol_lf", "eol_crlf", "encoding_utf8bom", "encoding_utf16le",
            "encoding_ascii", "duplicate_line", "delete_line", "indent_line",
            "format_json", "minify_json", "sort_json_keys",
        ] {
            let is_json = command.ends_with("json") || command == "sort_json_keys";
            let mut app = app_with_content(if is_json { "{\"b\": 2, \"a\": 1}" } else { "alpha beta\r\ngamma delta\r\nepsilon\r\n" });
            let editor = app.editor_mut().unwrap();
            editor.language_override = Some("C".into());
            editor.set_cursor(1, 2);
            if !is_json { editor.insert_char('x'); }
            if command == "redo" { editor.undo(); }
            editor.read_only = true;
            let before = editor.rope.clone();
            let state = (editor.content_version, editor.modified, editor.encoding, editor.can_undo(), editor.can_redo());
            app.execute_palette_command(command);
            let editor = app.editor().unwrap();
            assert_eq!(editor.rope, before, "{command} changed read-only text");
            assert_eq!((editor.content_version, editor.modified, editor.encoding, editor.can_undo(), editor.can_redo()), state, "{command} changed read-only state");
        }
    }

    #[test]
    fn edge_case_read_only_keyboard_shortcuts_preserve_content_and_history() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (code, shift) in [('z', false), ('y', false), ('z', true), ('d', true), ('/', false), ('/', true), (']', false), ('[', false), ('j', false), ('t', false), ('k', true)] {
            let mut app = app_with_content("alpha beta\n    gamma delta\nepsilon\n");
            let editor = app.editor_mut().unwrap();
            editor.language_override = Some("C".into());
            editor.set_cursor(1, 6);
            editor.insert_char('x');
            if code == 'y' || (code == 'z' && shift) { editor.undo(); }
            editor.read_only = true;
            let before = editor.rope.clone();
            let history = (editor.can_undo(), editor.can_redo());
            let modifiers = KeyModifiers::CONTROL | if shift { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            assert!(!crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Char(code), modifiers)));
            let editor = app.editor().unwrap();
            assert_eq!(editor.rope, before, "Ctrl+{code}, shift={shift}");
            assert_eq!((editor.can_undo(), editor.can_redo()), history);
        }
    }

    #[test]
    fn edge_case_comment_chords_respect_read_only() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for code in ['c', 'u'] {
            let mut app = app_with_content("// alpha\nbeta\n");
            let editor = app.editor_mut().unwrap();
            editor.language_override = Some("C".into());
            editor.read_only = true;
            let before = editor.rope.clone();
            for key in ['k', code] {
                assert!(!crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL)));
            }
            assert!(!app.chord_pending);
            assert_eq!(app.editor().unwrap().rope, before);
            assert!(!app.editor().unwrap().can_undo());
        }
    }

    #[test]
    fn edge_case_comment_chords_execute_before_standalone_shortcuts() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app_with_content("alpha\n");
        app.editor_mut().unwrap().language_override = Some("C".into());
        for (code, expected) in [('c', "// alpha\n"), ('u', "alpha\n")] {
            for key in ['k', code] {
                assert!(!crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL)));
            }
            assert!(!app.chord_pending);
            assert_eq!(app.editor().unwrap().rope.to_string(), expected);
        }
    }

    #[test]
    fn duplicate_line_copies_current_line_below() {
        let mut app = app_with_content("aaa\nbbb\nccc\n");
        app.editor_mut().unwrap().cursor_line = 1;
        app.editor_mut().unwrap().cursor_col = 0;

        app.execute_editor_command("duplicate_line");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "aaa\nbbb\nbbb\nccc\n");
        assert_eq!(app.editor().unwrap().cursor_line, 2);
    }

    #[test]
    fn duplicate_line_up_copies_current_line_above() {
        let mut app = app_with_content("aaa\nbbb\nccc\n");
        app.editor_mut().unwrap().cursor_line = 1;

        app.execute_editor_command("duplicate_line_up");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "aaa\nbbb\nbbb\nccc\n");
        // cursor stays on original line (now index 1, the first "bbb")
        assert_eq!(app.editor().unwrap().cursor_line, 1);
    }

    #[test]
    fn delete_line_removes_current_line() {
        let mut app = app_with_content("aaa\nbbb\nccc\n");
        app.editor_mut().unwrap().cursor_line = 1;

        app.execute_editor_command("delete_line");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "aaa\nccc\n");
        // cursor stays valid
        assert!(app.editor().unwrap().cursor_line <= 1);
    }

    #[test]
    fn delete_line_single_line_file() {
        let mut app = app_with_content("only\n");
        app.execute_editor_command("delete_line");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "");
        assert_eq!(app.editor().unwrap().cursor_line, 0);
        assert_eq!(app.editor().unwrap().cursor_col, 0);
    }

    #[test]
    fn merge_blank_lines_collapses_consecutive_blanks() {
        let mut app = app_with_content("aaa\n\n\n\nbbb\n\n\nccc\n");

        app.execute_editor_command("merge_blank_lines");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "aaa\n\nbbb\n\nccc\n");
    }

    #[test]
    fn remove_empty_lines_strips_all_blanks() {
        let mut app = app_with_content("aaa\n\nbbb\n\n\nccc\n");

        app.execute_editor_command("remove_empty_lines");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "aaa\nbbb\nccc\n");
    }

    #[test]
    fn to_uppercase_with_selection() {
        let mut app = app_with_content("hello world\n");
        {
            let ed = app.editor_mut().unwrap();
            ed.cursor_line = 0;
            ed.cursor_col = 5;
            ed.selection_anchor = Some((0, 0));
        }

        app.execute_editor_command("to_uppercase");

        let text = app.editor().unwrap().rope.to_string();
        assert!(text.starts_with("HELLO"));
    }

    #[test]
    fn to_lowercase_with_selection() {
        let mut app = app_with_content("HELLO WORLD\n");
        {
            let ed = app.editor_mut().unwrap();
            ed.cursor_line = 0;
            ed.cursor_col = 5;
            ed.selection_anchor = Some((0, 0));
        }

        app.execute_editor_command("to_lowercase");

        let text = app.editor().unwrap().rope.to_string();
        assert!(text.starts_with("hello"));
    }

    #[test]
    fn toggle_comment_adds_and_removes_prefix() {
        // No file extension → default "//" prefix
        let mut app = app_with_content("let x = 1;\nlet y = 2;\n");
        app.editor_mut().unwrap().cursor_line = 0;

        // Comment the line
        app.execute_editor_command("toggle_comment");
        let text = app.editor().unwrap().rope.to_string();
        assert!(text.starts_with("// let x = 1;\n"), "Expected commented line, got: {text}");

        // Uncomment the line
        app.execute_editor_command("toggle_comment");
        let text = app.editor().unwrap().rope.to_string();
        assert!(text.starts_with("let x = 1;\n"), "Expected uncommented line, got: {text}");
    }

    #[test]
    fn move_line_down_swaps_with_next() {
        let mut app = app_with_content("aaa\nbbb\nccc\n");
        app.editor_mut().unwrap().cursor_line = 0;

        app.execute_editor_command("move_line_down");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "bbb\naaa\nccc\n");
        assert_eq!(app.editor().unwrap().cursor_line, 1);
    }

    #[test]
    fn move_line_up_swaps_with_previous() {
        let mut app = app_with_content("aaa\nbbb\nccc\n");
        app.editor_mut().unwrap().cursor_line = 2;

        app.execute_editor_command("move_line_up");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "aaa\nccc\nbbb\n");
        assert_eq!(app.editor().unwrap().cursor_line, 1);
    }

    #[test]
    fn join_lines_merges_cursor_with_next() {
        let mut app = app_with_content("hello\n    world\n");
        app.editor_mut().unwrap().cursor_line = 0;

        app.execute_editor_command("join_lines");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "hello world\n");
    }

    #[test]
    fn insert_line_below_adds_blank_line() {
        let mut app = app_with_content("aaa\nbbb\n");
        app.editor_mut().unwrap().cursor_line = 0;

        app.execute_editor_command("insert_line_below");

        let ed = app.editor().unwrap();
        assert_eq!(ed.rope.len_lines(), 4); // "aaa\n", "\n", "bbb\n", (possible trailing)
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn insert_line_above_adds_blank_line() {
        let mut app = app_with_content("aaa\nbbb\n");
        app.editor_mut().unwrap().cursor_line = 1;

        app.execute_editor_command("insert_line_above");

        let ed = app.editor().unwrap();
        // Line "bbb" is now at index 2; new blank line at index 1
        assert_eq!(ed.rope.len_lines(), 4);
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn reverse_lines_whole_file() {
        let mut app = app_with_content("aaa\nbbb\nccc");

        app.execute_editor_command("reverse_lines");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "ccc\nbbb\naaa");
    }

    #[test]
    fn remove_duplicates_keeps_first_occurrence() {
        let mut app = app_with_content("aaa\nbbb\naaa\nccc\nbbb\n");

        app.execute_editor_command("remove_duplicates");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "aaa\nbbb\nccc\n");
    }

    #[test]
    fn trim_trailing_whitespace_removes_trailing_spaces() {
        let mut app = app_with_content("hello   \nworld  \nfoo\n");

        app.execute_editor_command("trim_trailing");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "hello\nworld\nfoo\n");
    }

    #[test]
    fn toggle_read_only_blocks_edits() {
        let mut app = app_with_content("hello\n");

        app.execute_editor_command("toggle_read_only");
        assert!(app.editor().unwrap().read_only);

        // duplicate_line should be blocked in read-only mode
        app.execute_editor_command("duplicate_line");
        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "hello\n", "Editing should be blocked in read-only mode");

        app.execute_editor_command("toggle_read_only");
        assert!(!app.editor().unwrap().read_only);
    }

    #[test]
    fn delete_to_line_end_removes_after_cursor() {
        let mut app = app_with_content("hello world\n");
        {
            let ed = app.editor_mut().unwrap();
            ed.cursor_line = 0;
            ed.cursor_col = 5;
        }

        app.execute_editor_command("delete_to_line_end");

        let text = app.editor().unwrap().rope.to_string();
        assert_eq!(text, "hello\n");
    }

    #[test]
    fn dynamic_syntect_language_command_sets_editor_override() {
        let mut app = app_with_content("key: value\n");
        let command = crate::app_menu::language_command_id("YAML");

        app.execute_editor_command(&command);

        assert_eq!(app.editor().unwrap().language_override.as_deref(), Some("YAML"));
        assert_eq!(app.notification_history.last().unwrap().0, "Language: YAML");
    }

    #[test]
    fn change_language_opens_all_syntect_modes() {
        let mut app = app_with_content("text\n");
        let expected = app.highlighter.syntax_names();

        app.execute_editor_command("change_language");

        assert!(app.palette.active);
        assert_eq!(app.palette.query, "Language: ");
        let listed: std::collections::HashSet<&str> = app
            .palette
            .filtered
            .iter()
            .filter_map(|index| app.palette.commands.get(*index))
            .filter_map(|command| {
                crate::app_menu::language_from_command_id(command.id.as_ref())
            })
            .collect();
        assert_eq!(listed.len(), expected.len());
        assert!(expected.iter().all(|name| listed.contains(name.as_str())));
    }
}
