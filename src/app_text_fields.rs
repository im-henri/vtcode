use std::path::PathBuf;
use unicode_segmentation::UnicodeSegmentation;

use crate::app::App;

impl App {
    pub(crate) fn clamp_to_char_boundary(text: &str, mut idx: usize) -> usize {
        idx = idx.min(text.len());
        while idx > 0 && !text.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    pub(crate) fn clamp_to_grapheme_boundary(text: &str, idx: usize) -> usize {
        if idx >= text.len() {
            return text.len();
        }
        text.grapheme_indices(true)
            .take_while(|(byte, _)| *byte <= idx)
            .last()
            .map_or(0, |(byte, _)| byte)
    }

    pub(crate) fn next_available_child_name(dir: &std::path::Path, base_name: &str) -> PathBuf {
        let mut candidate = dir.join(base_name);
        if !candidate.exists() {
            return candidate;
        }
        for i in 1.. {
            let name = format!("{base_name} {i}");
            candidate = dir.join(name);
            if !candidate.exists() {
                return candidate;
            }
        }
        dir.join(base_name)
    }

    pub(crate) fn normalize_inline_cursor(
        text: &str,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
    ) {
        *cursor = Self::clamp_to_grapheme_boundary(text, *cursor);
        if let Some(anchor) = *selection_anchor {
            let normalized = Self::clamp_to_grapheme_boundary(text, anchor);
            *selection_anchor = if normalized == *cursor {
                None
            } else {
                Some(normalized)
            };
        }
    }

    pub(crate) fn inline_selection_range(
        cursor: usize,
        selection_anchor: Option<usize>,
    ) -> Option<(usize, usize)> {
        let anchor = selection_anchor?;
        let start = anchor.min(cursor);
        let end = anchor.max(cursor);
        if start == end {
            None
        } else {
            Some((start, end))
        }
    }

    #[cfg(test)]
    pub(crate) fn inline_cursor_char_index(text: &str, cursor: usize) -> usize {
        let cursor = Self::clamp_to_char_boundary(text, cursor);
        text[..cursor].chars().count()
    }

    fn inline_update_selection(
        selection_anchor: &mut Option<usize>,
        old_cursor: usize,
        new_cursor: usize,
        extend_selection: bool,
    ) {
        if extend_selection {
            if selection_anchor.is_none() {
                *selection_anchor = Some(old_cursor);
            }
            if *selection_anchor == Some(new_cursor) {
                *selection_anchor = None;
            }
        } else {
            *selection_anchor = None;
        }
    }

    fn inline_delete_selection(
        text: &mut String,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
    ) -> bool {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        if let Some((start, end)) = Self::inline_selection_range(*cursor, *selection_anchor) {
            text.drain(start..end);
            *cursor = start;
            *selection_anchor = None;
            return true;
        }
        false
    }

    pub(crate) fn inline_move_left(
        text: &str,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
        extend_selection: bool,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        let old_cursor = *cursor;
        if *cursor > 0 {
            *cursor = Self::inline_prev_char_start(text, *cursor);
        }
        Self::inline_update_selection(selection_anchor, old_cursor, *cursor, extend_selection);
    }

    pub(crate) fn inline_move_right(
        text: &str,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
        extend_selection: bool,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        let old_cursor = *cursor;
        let len = text.len();
        if *cursor < len {
            *cursor = Self::inline_next_char_end(text, *cursor);
        }
        Self::inline_update_selection(selection_anchor, old_cursor, *cursor, extend_selection);
    }

    pub(crate) fn inline_move_home(
        text: &str,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
        extend_selection: bool,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        let old_cursor = *cursor;
        *cursor = 0;
        Self::inline_update_selection(selection_anchor, old_cursor, *cursor, extend_selection);
    }

    pub(crate) fn inline_move_end(
        text: &str,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
        extend_selection: bool,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        let old_cursor = *cursor;
        *cursor = text.len();
        Self::inline_update_selection(selection_anchor, old_cursor, *cursor, extend_selection);
    }

    fn inline_prev_char_start(text: &str, cursor: usize) -> usize {
        text[..cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn inline_next_char_end(text: &str, cursor: usize) -> usize {
        if cursor >= text.len() {
            return text.len();
        }
        let next_len = text[cursor..].graphemes(true).next().map_or(0, str::len);
        (cursor + next_len).min(text.len())
    }

    fn inline_word_class(ch: char) -> u8 {
        if ch.is_whitespace() {
            0
        } else if ch.is_alphanumeric() || ch == '_' {
            1
        } else {
            2
        }
    }

    fn inline_word_left_boundary(text: &str, cursor: usize) -> usize {
        let mut idx = Self::clamp_to_char_boundary(text, cursor);
        while idx > 0 {
            let prev = Self::inline_prev_char_start(text, idx);
            let ch = text[prev..idx].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                break;
            }
            idx = prev;
        }
        let class = if idx > 0 {
            let prev = Self::inline_prev_char_start(text, idx);
            Self::inline_word_class(text[prev..idx].chars().next().unwrap_or(' '))
        } else {
            return idx;
        };
        while idx > 0 {
            let prev = Self::inline_prev_char_start(text, idx);
            let ch = text[prev..idx].chars().next().unwrap_or(' ');
            if Self::inline_word_class(ch) != class {
                break;
            }
            idx = prev;
        }
        idx
    }

    fn inline_word_right_boundary(text: &str, cursor: usize) -> usize {
        let len = text.len();
        let mut idx = Self::clamp_to_char_boundary(text, cursor).min(len);
        while idx < len {
            let next = Self::inline_next_char_end(text, idx);
            let ch = text[idx..next].chars().next().unwrap_or(' ');
            if !ch.is_whitespace() {
                break;
            }
            idx = next;
        }
        let class = if idx < len {
            let next = Self::inline_next_char_end(text, idx);
            Self::inline_word_class(text[idx..next].chars().next().unwrap_or(' '))
        } else {
            return idx;
        };
        while idx < len {
            let next = Self::inline_next_char_end(text, idx);
            let ch = text[idx..next].chars().next().unwrap_or(' ');
            if Self::inline_word_class(ch) != class {
                break;
            }
            idx = next;
        }
        idx
    }

    pub(crate) fn inline_move_word_left(
        text: &str,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
        extend_selection: bool,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        let old_cursor = *cursor;
        *cursor = Self::inline_word_left_boundary(text, *cursor);
        Self::inline_update_selection(selection_anchor, old_cursor, *cursor, extend_selection);
    }

    pub(crate) fn inline_move_word_right(
        text: &str,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
        extend_selection: bool,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        let old_cursor = *cursor;
        *cursor = Self::inline_word_right_boundary(text, *cursor);
        Self::inline_update_selection(selection_anchor, old_cursor, *cursor, extend_selection);
    }

    pub(crate) fn inline_insert_char(
        text: &mut String,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
        c: char,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        let _ = Self::inline_delete_selection(text, cursor, selection_anchor);
        text.insert(*cursor, c);
        *cursor += c.len_utf8();
        *selection_anchor = None;
    }

    pub(crate) fn inline_backspace(
        text: &mut String,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        if Self::inline_delete_selection(text, cursor, selection_anchor) {
            return;
        }
        if *cursor > 0 {
            let prev = Self::inline_prev_char_start(text, *cursor);
            text.drain(prev..*cursor);
            *cursor = prev;
        }
    }

    pub(crate) fn inline_backspace_word(
        text: &mut String,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        if Self::inline_delete_selection(text, cursor, selection_anchor) {
            return;
        }
        if *cursor == 0 {
            return;
        }
        let start = Self::inline_word_left_boundary(text, *cursor);
        text.drain(start..*cursor);
        *cursor = start;
    }

    pub(crate) fn inline_delete(text: &mut String, cursor: &mut usize, selection_anchor: &mut Option<usize>) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        if Self::inline_delete_selection(text, cursor, selection_anchor) {
            return;
        }
        let len = text.len();
        if *cursor < len {
            let next = Self::inline_next_char_end(text, *cursor);
            text.drain(*cursor..next);
        }
    }

    pub(crate) fn inline_delete_word(
        text: &mut String,
        cursor: &mut usize,
        selection_anchor: &mut Option<usize>,
    ) {
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
        if Self::inline_delete_selection(text, cursor, selection_anchor) {
            return;
        }
        if *cursor >= text.len() {
            return;
        }
        let end = Self::inline_word_right_boundary(text, *cursor);
        text.drain(*cursor..end);
    }

    #[cfg(test)]
    pub(crate) fn byte_index_for_char_col(
        text: &str,
        start: usize,
        end: usize,
        target_col: usize,
    ) -> usize {
        if start >= end {
            return start.min(text.len());
        }
        for (col, (i, _)) in text[start..end].char_indices().enumerate() {
            if col == target_col {
                return start + i;
            }
        }
        end
    }

    fn normalize_rename_cursor(&mut self) {
        Self::normalize_inline_cursor(
            &self.rename.query,
            &mut self.rename.cursor,
            &mut self.rename.selection_anchor,
        );
    }

    pub(crate) fn begin_rename(
        &mut self,
        target: PathBuf,
        initial: String,
        select_name: bool,
        placeholder_active: bool,
    ) {
        // Keep an existing file's extension when typing its new name. Use the
        // cached entry type so opening Rename never needs a metadata probe.
        let preserve_extension = select_name
            && !placeholder_active
            && self
                .explorer
                .find_path(&target)
                .is_some_and(|index| !self.explorer.entries[index].is_dir);
        self.rename.target = Some(target);
        self.rename.query = initial;
        self.rename.cursor = if preserve_extension {
            self.rename
                .query
                .rfind('.')
                .filter(|&dot| dot > 0 && dot + 1 < self.rename.query.len())
                .unwrap_or(self.rename.query.len())
        } else {
            self.rename.query.len()
        };
        self.rename.selection_anchor = if select_name && !self.rename.query.is_empty() {
            Some(0)
        } else {
            None
        };
        self.rename.placeholder_active = placeholder_active;
        self.rename.active = true;
        self.normalize_rename_cursor();
    }

    pub(crate) fn cancel_rename(&mut self) {
        if self.remote_workspace.is_some() && self.rename.placeholder_active { self.refresh_explorer_async(); }
        if self.rename.placeholder_active {
            if let Some(target) = &self.rename.target {
                if target.exists() {
                    let _ = if target.is_dir() {
                        std::fs::remove_dir(target)
                    } else {
                        std::fs::remove_file(target)
                    }; // Error OK: cleanup may fail if already removed
                    self.refresh_explorer_after_edit(None);
                }
            }
        }
        self.rename.active = false;
        self.rename.target = None;
        self.rename.selection_anchor = None;
        self.rename.cursor = 0;
        self.rename.query.clear();
        self.rename.placeholder_active = false;
        self.creating_new_file = false;
        self.creating_new_folder = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn rename_app(name: &str, is_dir: bool) -> App {
        let mut app = App::test_new();
        app.active_pane = crate::app::ActivePane::Explorer;
        app.sidebar_view = crate::app::SidebarView::Explorer;
        app.explorer.entries = vec![crate::explorer::FileEntry {
            // No file is required: Rename uses the Explorer's cached type.
            path: PathBuf::from("rename-fixture").join(name),
            name: name.into(),
            is_dir,
            depth: 0,
            expanded: false,
            git_status: crate::explorer::GitStatus::Unmodified,
            git_status_summary: Default::default(),
        }];
        app.explorer.selected = 0;
        app
    }

    fn rename_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
    }

    #[test]
    fn rename_f2_targets_the_focused_view_with_default_and_fallback_bindings() {
        for configured in [true, false] {
            for is_dir in [false, true] {
                let mut app = rename_app("module.py", is_dir);
                app.tabs = vec![crate::app::Tab::new_file(
                    crate::editor::Editor::test_with_text("symbol"),
                )];
                app.active_tab = 0;
                app.explorer.find.open = true;
                app.explorer.find.focus = crate::explorer_find::Focus::Tree;
                if !configured {
                    app.keybindings.clear();
                }

                rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
                assert!(app.rename.active);
                assert!(!app.rename.symbol_active);
                assert!(!app.lsp_prepare_rename_pending);
                assert_eq!(app.rename.target.as_ref(), Some(&app.explorer.entries[0].path));
                rename_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
                assert_eq!(app.rename.query, if is_dir { "x" } else { "x.py" });
                // Repeated presses must not restart an in-progress rename.
                rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
                assert_eq!(app.rename.query, if is_dir { "x" } else { "x.py" });
                rename_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                assert!(!app.rename.active);

                app.active_pane = crate::app::ActivePane::Editor;
                rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
                assert!(app.rename.symbol_active);
                assert_eq!(app.rename.symbol_old, "symbol");
                assert!(!app.rename.active);
            }
        }
    }

    #[test]
    fn rename_f2_does_not_bypass_explorer_menus_or_modal_inputs() {
        for overlay in ["palette", "context_menu", "save_as"] {
            let mut app = rename_app("module.py", false);
            match overlay {
                "palette" => app.open_palette(),
                "context_menu" => app.explorer_context_active = true,
                "save_as" => app.save_as_active = true,
                _ => unreachable!(),
            }
            rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
            assert!(!app.rename.active, "{overlay}");
            assert!(!app.rename.symbol_active, "{overlay}");
        }
    }

    #[test]
    fn rename_f2_ignores_modified_keys_and_other_sidebars() {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::SHIFT, KeyModifiers::ALT] {
            let mut app = rename_app("module.py", false);
            rename_key(&mut app, KeyCode::F(2), modifiers);
            assert!(!app.rename.active, "{modifiers:?}");
        }
        for sidebar in [
            crate::app::SidebarView::Search,
            crate::app::SidebarView::SourceControl,
            crate::app::SidebarView::Settings,
            crate::app::SidebarView::Extensions,
            crate::app::SidebarView::Outline,
        ] {
            let mut app = rename_app("module.py", false);
            app.sidebar_view = sidebar;
            app.tabs = vec![crate::app::Tab::new_file(
                crate::editor::Editor::test_with_text("symbol"),
            )];
            app.active_tab = 0;
            rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
            assert!(!app.rename.active, "{sidebar:?}");
            assert!(!app.rename.symbol_active, "{sidebar:?}");
        }
    }

    #[test]
    fn rename_f2_ignores_missing_explorer_entries() {
        let mut app = rename_app("module.py", false);
        app.explorer.selected = 1;
        rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        assert!(!app.rename.active);
        app.explorer.entries.clear();
        app.explorer.selected = 0;
        rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        assert!(!app.rename.active);
    }

    #[test]
    fn rename_typing_preserves_extensions_from_keyboard_and_context_menu() {
        for (name, stem, renamed) in [
            ("module.py", "module", "new.py"),
            ("archive.tar.gz", "archive.tar", "new.gz"),
            ("cafe\u{301}😀.rs", "cafe\u{301}😀", "new.rs"),
            (".env.local", ".env", "new.local"),
            (".env", ".env", "new"),
            ("README", "README", "new"),
            ("name.", "name.", "new"),
        ] {
            for context_menu in [false, true] {
                let mut app = rename_app(name, false);
                if context_menu {
                    app.explorer_context_target = Some(app.explorer.entries[0].path.clone());
                    let rename = app
                        .explorer_context_menu_items()
                        .iter()
                        .position(|&label| label == "Rename")
                        .unwrap();
                    app.execute_explorer_context(rename);
                } else {
                    rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
                }
                assert!(app.rename.active);
                assert_eq!(
                    App::inline_selection_range(app.rename.cursor, app.rename.selection_anchor),
                    Some((0, stem.len())),
                    "{name}, context menu: {context_menu}"
                );
                for ch in "new".chars() {
                    rename_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
                }
                assert_eq!(app.rename.query, renamed);
            }
        }
    }

    #[test]
    fn rename_folders_and_new_placeholders_select_the_entire_name() {
        for (is_dir, placeholder) in [(true, false), (true, true), (false, true)] {
            let mut app = rename_app("folder.name", is_dir);
            app.begin_rename(
                app.explorer.entries[0].path.clone(),
                "folder.name".into(),
                true,
                placeholder,
            );
            rename_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_eq!(app.rename.query, "x");
        }
    }

    #[test]
    fn rename_allows_replacing_the_extension_and_editing_without_selection() {
        let mut app = rename_app("module.py", false);
        rename_key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        rename_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        rename_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.rename.query, "x");
        rename_key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.rename.query, "module.py");

        app.begin_rename(
            app.explorer.entries[0].path.clone(),
            "module.py".into(),
            false,
            false,
        );
        rename_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.rename.query, "module.pyx");
    }

    // ── clamp_to_char_boundary ──────────────────────────────────────

    #[test]
    fn clamp_within_ascii() {
        assert_eq!(App::clamp_to_char_boundary("hello", 3), 3);
    }

    #[test]
    fn clamp_beyond_len() {
        assert_eq!(App::clamp_to_char_boundary("hi", 100), 2);
    }

    #[test]
    fn clamp_empty_string() {
        assert_eq!(App::clamp_to_char_boundary("", 5), 0);
    }

    #[test]
    fn clamp_mid_multibyte_char() {
        let text = "aé"; // 'a' = 1 byte, 'é' = 2 bytes, total 3
        // byte index 2 is inside the 'é' char (starts at 1, ends at 3)
        assert_eq!(App::clamp_to_char_boundary(text, 2), 1);
    }

    #[test]
    fn clamp_emoji_boundary() {
        let text = "a😀b"; // 'a'=1, '😀'=4, 'b'=1 → len=6
        assert_eq!(App::clamp_to_char_boundary(text, 3), 1); // mid-emoji → back to 'a' end
        assert_eq!(App::clamp_to_char_boundary(text, 5), 5); // 'b' start is valid
    }

    // ── normalize_inline_cursor ─────────────────────────────────────

    #[test]
    fn normalize_clamps_cursor() {
        let text = "abc";
        let mut cursor = 10;
        let mut anchor = None;
        App::normalize_inline_cursor(text, &mut cursor, &mut anchor);
        assert_eq!(cursor, 3);
    }

    #[test]
    fn normalize_collapses_anchor_equal_to_cursor() {
        let text = "abc";
        let mut cursor = 2;
        let mut anchor = Some(2);
        App::normalize_inline_cursor(text, &mut cursor, &mut anchor);
        assert_eq!(anchor, None);
    }

    #[test]
    fn normalize_keeps_valid_anchor() {
        let text = "abcdef";
        let mut cursor = 2;
        let mut anchor = Some(5);
        App::normalize_inline_cursor(text, &mut cursor, &mut anchor);
        assert_eq!(anchor, Some(5));
    }

    // ── inline_selection_range ──────────────────────────────────────

    #[test]
    fn selection_range_none_when_no_anchor() {
        assert_eq!(App::inline_selection_range(3, None), None);
    }

    #[test]
    fn selection_range_none_when_equal() {
        assert_eq!(App::inline_selection_range(3, Some(3)), None);
    }

    #[test]
    fn selection_range_ordered() {
        assert_eq!(App::inline_selection_range(5, Some(2)), Some((2, 5)));
        assert_eq!(App::inline_selection_range(2, Some(5)), Some((2, 5)));
    }

    // ── inline_cursor_char_index ────────────────────────────────────

    #[test]
    fn char_index_ascii() {
        assert_eq!(App::inline_cursor_char_index("hello", 3), 3);
    }

    #[test]
    fn char_index_multibyte() {
        let text = "aéb"; // 'a'=1, 'é'=2, 'b'=1
        assert_eq!(App::inline_cursor_char_index(text, 0), 0);
        assert_eq!(App::inline_cursor_char_index(text, 1), 1); // after 'a'
        assert_eq!(App::inline_cursor_char_index(text, 3), 2); // after 'é'
        assert_eq!(App::inline_cursor_char_index(text, 4), 3); // after 'b'
    }

    #[test]
    fn char_index_empty() {
        assert_eq!(App::inline_cursor_char_index("", 0), 0);
    }

    // ── inline_update_selection ─────────────────────────────────────

    #[test]
    fn update_selection_extend_sets_anchor() {
        let mut anchor = None;
        App::inline_update_selection(&mut anchor, 3, 5, true);
        assert_eq!(anchor, Some(3));
    }

    #[test]
    fn update_selection_extend_collapse_when_returns_to_anchor() {
        let mut anchor = Some(3);
        App::inline_update_selection(&mut anchor, 5, 3, true);
        assert_eq!(anchor, None);
    }

    #[test]
    fn update_selection_no_extend_clears_anchor() {
        let mut anchor = Some(3);
        App::inline_update_selection(&mut anchor, 3, 5, false);
        assert_eq!(anchor, None);
    }

    // ── inline_delete_selection ─────────────────────────────────────

    #[test]
    fn delete_selection_removes_range() {
        let mut text = String::from("hello world");
        let mut cursor = 8; // 'r'
        let mut anchor = Some(5); // ' '
        let deleted = App::inline_delete_selection(&mut text, &mut cursor, &mut anchor);
        assert!(deleted);
        assert_eq!(text, "hellorld");
        assert_eq!(cursor, 5);
        assert_eq!(anchor, None);
    }

    #[test]
    fn delete_selection_noop_without_anchor() {
        let mut text = String::from("hello");
        let mut cursor = 3;
        let mut anchor = None;
        assert!(!App::inline_delete_selection(&mut text, &mut cursor, &mut anchor));
        assert_eq!(text, "hello");
    }

    // ── inline_move_left / right ────────────────────────────────────

    #[test]
    fn move_left_basic() {
        let text = "abcd";
        let mut cursor = 3;
        let mut anchor = None;
        App::inline_move_left(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 2);
    }

    #[test]
    fn move_left_at_start() {
        let text = "abc";
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_move_left(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn move_left_multibyte() {
        let text = "a😀b"; // 'a'=1, '😀'=4, 'b'=1
        let mut cursor = 5; // at 'b'
        let mut anchor = None;
        App::inline_move_left(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 1); // jumped back over 😀
    }

    #[test]
    fn move_right_basic() {
        let text = "abcd";
        let mut cursor = 1;
        let mut anchor = None;
        App::inline_move_right(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 2);
    }

    #[test]
    fn move_right_at_end() {
        let text = "abc";
        let mut cursor = 3;
        let mut anchor = None;
        App::inline_move_right(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 3);
    }

    #[test]
    fn move_right_multibyte() {
        let text = "a😀b"; // 'a'=1, '😀'=4, 'b'=1
        let mut cursor = 1; // after 'a'
        let mut anchor = None;
        App::inline_move_right(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 5); // jumped over 😀
    }

    #[test]
    fn move_left_with_selection() {
        let text = "abcdef";
        let mut cursor = 4;
        let mut anchor = None;
        App::inline_move_left(text, &mut cursor, &mut anchor, true);
        assert_eq!(cursor, 3);
        assert_eq!(anchor, Some(4));
    }

    #[test]
    fn move_right_with_selection() {
        let text = "abcdef";
        let mut cursor = 2;
        let mut anchor = None;
        App::inline_move_right(text, &mut cursor, &mut anchor, true);
        assert_eq!(cursor, 3);
        assert_eq!(anchor, Some(2));
    }

    // ── inline_move_home / end ──────────────────────────────────────

    #[test]
    fn move_home() {
        let text = "hello";
        let mut cursor = 3;
        let mut anchor = None;
        App::inline_move_home(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 0);
        assert_eq!(anchor, None);
    }

    #[test]
    fn move_end() {
        let text = "hello";
        let mut cursor = 1;
        let mut anchor = None;
        App::inline_move_end(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 5);
        assert_eq!(anchor, None);
    }

    #[test]
    fn move_home_with_selection() {
        let text = "hello";
        let mut cursor = 3;
        let mut anchor = None;
        App::inline_move_home(text, &mut cursor, &mut anchor, true);
        assert_eq!(cursor, 0);
        assert_eq!(anchor, Some(3));
    }

    #[test]
    fn move_end_with_selection() {
        let text = "hello";
        let mut cursor = 1;
        let mut anchor = None;
        App::inline_move_end(text, &mut cursor, &mut anchor, true);
        assert_eq!(cursor, 5);
        assert_eq!(anchor, Some(1));
    }

    // ── inline_word_left_boundary / right_boundary ──────────────────

    #[test]
    fn word_left_boundary_basic() {
        let text = "hello world";
        assert_eq!(App::inline_word_left_boundary(text, 8), 6); // mid-"world" → start of "world"
        assert_eq!(App::inline_word_left_boundary(text, 6), 0); // start of "world" → start of "hello"
    }

    #[test]
    fn word_left_boundary_from_start() {
        assert_eq!(App::inline_word_left_boundary("abc", 0), 0);
    }

    #[test]
    fn word_left_boundary_multiple_spaces() {
        let text = "hello   world";
        assert_eq!(App::inline_word_left_boundary(text, 10), 8); // mid-"world" → 'w'
        assert_eq!(App::inline_word_left_boundary(text, 8), 0);  // 'w' → start of "hello"
    }

    #[test]
    fn word_right_boundary_basic() {
        let text = "hello world";
        assert_eq!(App::inline_word_right_boundary(text, 0), 5); // start → end of "hello"
        assert_eq!(App::inline_word_right_boundary(text, 5), 11); // space → end of "world"
    }

    #[test]
    fn word_right_boundary_at_end() {
        let text = "abc";
        assert_eq!(App::inline_word_right_boundary(text, 3), 3);
    }

    #[test]
    fn word_right_boundary_multiple_spaces() {
        let text = "foo   bar";
        assert_eq!(App::inline_word_right_boundary(text, 0), 3); // "foo"
        assert_eq!(App::inline_word_right_boundary(text, 3), 9); // spaces+"bar"
    }

    #[test]
    fn word_boundaries_treat_punctuation_as_a_separate_run() {
        let text = "foo.bar";
        assert_eq!(App::inline_word_left_boundary(text, text.len()), 4);
        assert_eq!(App::inline_word_left_boundary(text, 4), 3);
        assert_eq!(App::inline_word_right_boundary(text, 0), 3);
        assert_eq!(App::inline_word_right_boundary(text, 3), 4);
    }

    // ── inline_move_word_left / right ───────────────────────────────

    #[test]
    fn move_word_left() {
        let text = "one two three";
        let mut cursor = 13; // end
        let mut anchor = None;
        App::inline_move_word_left(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 8); // start of "three"
        App::inline_move_word_left(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 4); // start of "two"
        App::inline_move_word_left(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 0); // start of "one"
    }

    #[test]
    fn move_word_right() {
        let text = "one two three";
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_move_word_right(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 3); // end of "one"
        App::inline_move_word_right(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 7); // end of "two"
        App::inline_move_word_right(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 13); // end of "three"
    }

    #[test]
    fn move_word_left_with_selection() {
        let text = "hello world";
        let mut cursor = 11;
        let mut anchor = None;
        App::inline_move_word_left(text, &mut cursor, &mut anchor, true);
        assert_eq!(cursor, 6);
        assert_eq!(anchor, Some(11));
    }

    // ── inline_insert_char ──────────────────────────────────────────

    #[test]
    fn insert_char_basic() {
        let mut text = String::from("hllo");
        let mut cursor = 1;
        let mut anchor = None;
        App::inline_insert_char(&mut text, &mut cursor, &mut anchor, 'e');
        assert_eq!(text, "hello");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn insert_char_at_start() {
        let mut text = String::from("ello");
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_insert_char(&mut text, &mut cursor, &mut anchor, 'h');
        assert_eq!(text, "hello");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn insert_char_at_end() {
        let mut text = String::from("hell");
        let mut cursor = 4;
        let mut anchor = None;
        App::inline_insert_char(&mut text, &mut cursor, &mut anchor, 'o');
        assert_eq!(text, "hello");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn insert_emoji() {
        let mut text = String::from("ab");
        let mut cursor = 1;
        let mut anchor = None;
        App::inline_insert_char(&mut text, &mut cursor, &mut anchor, '😀');
        assert_eq!(text, "a😀b");
        assert_eq!(cursor, 5); // 1 + 4 (emoji len)
    }

    #[test]
    fn insert_replaces_selection() {
        let mut text = String::from("hello world");
        let mut cursor = 8;
        let mut anchor = Some(5);
        App::inline_insert_char(&mut text, &mut cursor, &mut anchor, 'X');
        assert_eq!(text, "helloXrld");
        assert_eq!(anchor, None);
    }

    #[test]
    fn insert_into_empty() {
        let mut text = String::new();
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_insert_char(&mut text, &mut cursor, &mut anchor, 'a');
        assert_eq!(text, "a");
        assert_eq!(cursor, 1);
    }

    // ── inline_backspace ────────────────────────────────────────────

    #[test]
    fn backspace_basic() {
        let mut text = String::from("hello");
        let mut cursor = 3;
        let mut anchor = None;
        App::inline_backspace(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "helo");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn backspace_at_start() {
        let mut text = String::from("hello");
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_backspace(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "hello");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut text = String::from("hello world");
        let mut cursor = 8;
        let mut anchor = Some(5);
        App::inline_backspace(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "hellorld");
        assert_eq!(cursor, 5);
        assert_eq!(anchor, None);
    }

    #[test]
    fn backspace_multibyte() {
        let mut text = String::from("a😀b");
        let mut cursor = 5; // after 😀
        let mut anchor = None;
        App::inline_backspace(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "ab");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn backspace_empty() {
        let mut text = String::new();
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_backspace(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "");
        assert_eq!(cursor, 0);
    }

    // ── inline_backspace_word ───────────────────────────────────────

    #[test]
    fn backspace_word_basic() {
        let mut text = String::from("hello world");
        let mut cursor = 11; // end
        let mut anchor = None;
        App::inline_backspace_word(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "hello ");
        assert_eq!(cursor, 6);
    }

    #[test]
    fn backspace_word_at_start() {
        let mut text = String::from("hello");
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_backspace_word(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "hello");
    }

    #[test]
    fn backspace_word_deletes_selection() {
        let mut text = String::from("foo bar baz");
        let mut cursor = 7;
        let mut anchor = Some(4);
        App::inline_backspace_word(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "foo  baz");
        assert_eq!(cursor, 4);
        assert_eq!(anchor, None);
    }

    // ── inline_delete ───────────────────────────────────────────────

    #[test]
    fn delete_basic() {
        let mut text = String::from("hello");
        let mut cursor = 2;
        let mut anchor = None;
        App::inline_delete(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "helo");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn delete_at_end() {
        let mut text = String::from("hello");
        let mut cursor = 5;
        let mut anchor = None;
        App::inline_delete(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "hello");
    }

    #[test]
    fn delete_multibyte() {
        let mut text = String::from("a😀b");
        let mut cursor = 1; // at '😀'
        let mut anchor = None;
        App::inline_delete(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "ab");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn delete_deletes_selection() {
        let mut text = String::from("hello world");
        let mut cursor = 2;
        let mut anchor = Some(7);
        App::inline_delete(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "heorld");
        assert_eq!(cursor, 2);
        assert_eq!(anchor, None);
    }

    // ── inline_delete_word ──────────────────────────────────────────

    #[test]
    fn delete_word_basic() {
        let mut text = String::from("hello world");
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_delete_word(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, " world");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn delete_word_at_end() {
        let mut text = String::from("hello");
        let mut cursor = 5;
        let mut anchor = None;
        App::inline_delete_word(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "hello");
    }

    #[test]
    fn delete_word_deletes_selection() {
        let mut text = String::from("aaa bbb ccc");
        let mut cursor = 4;
        let mut anchor = Some(8);
        App::inline_delete_word(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "aaa ccc");
        assert_eq!(cursor, 4);
        assert_eq!(anchor, None);
    }

    // ── empty-string edge cases ─────────────────────────────────────

    #[test]
    fn move_left_empty() {
        let text = "";
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_move_left(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn move_right_empty() {
        let text = "";
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_move_right(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn home_end_empty() {
        let text = "";
        let mut cursor = 0;
        let mut anchor = None;
        App::inline_move_home(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 0);
        App::inline_move_end(text, &mut cursor, &mut anchor, false);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn word_boundaries_empty() {
        assert_eq!(App::inline_word_left_boundary("", 0), 0);
        assert_eq!(App::inline_word_right_boundary("", 0), 0);
    }

    // ── line_start_end / byte_index_for_char_col ────────────────────

    #[test]
    fn byte_index_for_char_col_basic() {
        let text = "hello";
        assert_eq!(App::byte_index_for_char_col(text, 0, 5, 2), 2);
    }

    #[test]
    fn byte_index_for_char_col_past_end() {
        let text = "hi";
        assert_eq!(App::byte_index_for_char_col(text, 0, 2, 10), 2);
    }

    #[test]
    fn byte_index_for_char_col_empty_range() {
        let text = "hello";
        assert_eq!(App::byte_index_for_char_col(text, 3, 3, 0), 3);
    }

    // ── unicode round-trip: insert, move, delete ────────────────────

    #[test]
    fn unicode_round_trip() {
        let mut text = String::new();
        let mut cursor = 0;
        let mut anchor = None;

        // Insert "café"
        for c in "café".chars() {
            App::inline_insert_char(&mut text, &mut cursor, &mut anchor, c);
        }
        assert_eq!(text, "café");
        assert_eq!(cursor, text.len()); // 5 bytes (é=2)

        // Move left twice → should be after 'f'
        App::inline_move_left(&text, &mut cursor, &mut anchor, false);
        App::inline_move_left(&text, &mut cursor, &mut anchor, false);
        assert_eq!(App::inline_cursor_char_index(&text, cursor), 2); // char index 2

        // Delete forward → removes 'f'
        App::inline_delete(&mut text, &mut cursor, &mut anchor);
        assert_eq!(text, "caé");
    }

    // ── inline_prev_char_start / inline_next_char_end ───────────────

    #[test]
    fn prev_char_start_ascii() {
        assert_eq!(App::inline_prev_char_start("abc", 2), 1);
        assert_eq!(App::inline_prev_char_start("abc", 0), 0);
    }

    #[test]
    fn next_char_end_ascii() {
        assert_eq!(App::inline_next_char_end("abc", 1), 2);
        assert_eq!(App::inline_next_char_end("abc", 3), 3);
    }

    #[test]
    fn prev_next_char_multibyte() {
        let text = "a😀b"; // a=1, 😀=4, b=1
        assert_eq!(App::inline_prev_char_start(text, 5), 1); // before 'b' → start of 😀
        assert_eq!(App::inline_next_char_end(text, 1), 5);   // after 'a' → end of 😀
    }
}
