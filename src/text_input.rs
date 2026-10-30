use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;

use crate::app::App;

/// Stable identity for every field backed by the shared text-input
/// controller.  Keeping identity separate from geometry lets the renderer
/// rebuild hit maps every frame without moving field state into the UI layer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TextInputId {
    SearchFind,
    SearchReplace,
    QuickOpen,
    GotoLine,
    GlobalSearch(u8),
    ExplorerFind,
    GitBranch,
    CommandPalette,
    SymbolList,
    TaskPicker,
    SaveAs,
    FolderAddress,
    FolderSearch,
    FolderName,
    RenameSymbol,
    TerminalRename,
    SettingsSearch,
    SettingsValue,
    AgentSetting(usize),
    LspSetting(usize),
    FormatterSetting(usize),
    FileAssociation(usize),
    Exclusion(usize),
    ScopeBubbleLanguage,
    ExtensionsFilter,
    ExplorerRename,
    SourceControlCommit,
    Chat,
}

type Snapshot = (String, usize, Option<usize>);

#[path = "deferred_input.rs"]
mod deferred;
pub(crate) use deferred::{DeferredInput, FrozenInput, ResolvedInput};

/// Field-local history shared by single-line controls and the commit editor.
/// Programmatic replacements start a new history; navigating breaks typing groups.
#[derive(Clone, Default)]
pub(crate) struct TextInputHistory {
    revision: u64,
    current: Option<Snapshot>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<(std::time::Instant, crate::editor::CharClass)>,
    deferred: Option<Box<DeferredInput>>,
}

impl TextInputHistory {
    pub(crate) fn revision(&self) -> u64 { self.revision }
    pub(crate) fn break_group(&mut self) {
        self.last_edit = None;
        if let Some(deferred) = self.deferred.take() { deferred.invalidate(); }
    }

    fn remember(stack: &mut Vec<Snapshot>, snapshot: Snapshot) {
        stack.push(snapshot);
        while stack.len() > 100
            || (stack.len() > 1 && stack.iter().map(|s| s.0.len()).sum::<usize>() > 1024 * 1024)
        {
            stack.remove(0);
        }
    }

    pub(crate) fn edit(
        &mut self,
        input: TextInput<'_>,
        code: KeyCode,
        modifiers: KeyModifiers,
        paste: Option<&str>,
    ) -> TextInputResult {
        self.edit_at(input, code, modifiers, paste, None, std::time::Instant::now())
    }

    pub(crate) fn conditional_paste(&mut self, input: TextInput<'_>, id: u64, paste: &str) -> TextInputResult {
        self.edit_at(input, KeyCode::Null, KeyModifiers::empty(), Some(paste), Some(id), std::time::Instant::now())
    }

    fn edit_at(
        &mut self,
        mut input: TextInput<'_>,
        code: KeyCode,
        modifiers: KeyModifiers,
        paste: Option<&str>,
        conditional: Option<u64>,
        when: std::time::Instant,
    ) -> TextInputResult {
        App::normalize_inline_cursor(input.text, input.cursor, input.selection_anchor);
        let before = (input.text.clone(), *input.cursor, *input.selection_anchor);
        let mut deferred = self.deferred.take();
        if let Some(previous) = &self.current {
            if previous.0 != before.0 {
                if let Some(old) = deferred.take() { old.invalidate(); }
                let revision = self.revision.wrapping_add(1);
                *self = Self::default();
                self.revision = revision;
            } else if previous.1 != before.1 || previous.2 != before.2 {
                if let Some(old) = deferred.take() { old.invalidate(); }
                self.break_group();
            }
        }
        if deferred.is_some() && paste.is_none() && deferred::clipboard_key(code, modifiers) {
            if let Some(old) = deferred.take() { old.invalidate(); }
        }
        if conditional.is_some() && deferred.is_none() && DeferredInput::can_start(&before) {
            deferred = Some(Box::new(DeferredInput::new(self.clone(), before.clone(), conditional.unwrap())));
        }
        let primary = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && !modifiers.contains(KeyModifiers::ALT);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        let undo = primary && matches!(code, KeyCode::Char('z' | 'Z')) && !shift;
        let redo = primary
            && (matches!(code, KeyCode::Char('y' | 'Y'))
                || (shift && matches!(code, KeyCode::Char('z' | 'Z'))));
        let consumed;
        if paste.is_none() && (undo || redo) {
            let snapshot = if undo {
                self.undo.pop()
            } else {
                self.redo.pop()
            };
            if let Some(snapshot) = snapshot {
                Self::remember(
                    if undo { &mut self.redo } else { &mut self.undo },
                    before.clone(),
                );
                (*input.text, *input.cursor, *input.selection_anchor) = snapshot;
            }
            self.break_group();
            consumed = true;
        } else {
            consumed = if let Some(value) = paste {
                input.insert_text(value);
                true
            } else {
                input.handle_key(code, modifiers).consumed
            };
            if before.0 != *input.text {
                let class = match code {
                    KeyCode::Char(c) if !primary && paste.is_none() => {
                        Some(crate::editor::CharClass::of(c))
                    }
                    _ => None,
                };
                let grouped = before.2.is_none()
                    && class.is_some_and(|class| {
                        self.last_edit.is_some_and(|(time, previous)| {
                            when.saturating_duration_since(time).as_millis() < 500 && previous == class
                        })
                    });
                if !grouped {
                    Self::remember(&mut self.undo, before.clone());
                }
                self.redo.clear();
                self.last_edit = class.map(|class| (when, class));
            } else {
                self.break_group();
            }
        }
        let changed = before.0 != *input.text;
        if changed { self.revision = self.revision.wrapping_add(1); }
        self.current = Some((input.text.clone(), *input.cursor, *input.selection_anchor));
        if let Some(mut deferred) = deferred {
            if deferred.record(code, modifiers, paste, conditional, when, input.multiline, input.accept) {
                self.deferred = Some(deferred);
            } else {
                deferred.invalidate();
            }
        }
        TextInputResult { consumed, changed }
    }
}

/// Screen-to-text mapping emitted by the shared input widget.
///
/// `column_to_byte` has one entry per terminal cell in `area`.  It deliberately
/// stores UTF-8 byte offsets because that is also what `TextInput` uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TextInputHitMap {
    area: Rect,
    rows_to_byte: Vec<Vec<usize>>,
}

impl TextInputHitMap {
    pub(crate) fn new(area: Rect, mut column_to_byte: Vec<usize>, text_len: usize) -> Self {
        let area = Rect::new(area.x, area.y, area.width, area.height.min(1));
        Self::normalize_row(&mut column_to_byte, area.width, text_len);
        Self {
            area,
            rows_to_byte: vec![column_to_byte],
        }
    }

    pub(crate) fn multiline(
        area: Rect,
        mut rows_to_byte: Vec<Vec<usize>>,
        text_len: usize,
    ) -> Self {
        rows_to_byte.truncate(area.height as usize);
        for row in &mut rows_to_byte {
            Self::normalize_row(row, area.width, text_len);
        }
        while rows_to_byte.len() < area.height as usize {
            rows_to_byte.push(vec![text_len; area.width as usize]);
        }
        Self { area, rows_to_byte }
    }

    fn normalize_row(row: &mut Vec<usize>, width: u16, text_len: usize) {
        row.truncate(width as usize);
        row.resize(width as usize, text_len);
    }

    pub(crate) fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.area.x
            && column < self.area.x.saturating_add(self.area.width)
            && row >= self.area.y
            && row < self.area.y.saturating_add(self.area.height)
    }

    pub(crate) fn byte_at(&self, column: u16, row: u16) -> usize {
        if self.rows_to_byte.is_empty() {
            return 0;
        }
        let row_index = row.saturating_sub(self.area.y) as usize;
        let bytes = &self.rows_to_byte[row_index.min(self.rows_to_byte.len() - 1)];
        if bytes.is_empty() {
            return 0;
        }
        let relative = column.saturating_sub(self.area.x) as usize;
        bytes[relative.min(bytes.len() - 1)]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TextInputRegion {
    id: TextInputId,
    hit_map: TextInputHitMap,
    file_search_owner: Option<u64>,
}

/// Result of routing a key through the shared text-input controller.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TextInputResult {
    pub(crate) consumed: bool,
    pub(crate) changed: bool,
}

impl TextInputResult {
    const IGNORED: Self = Self {
        consumed: false,
        changed: false,
    };
    const NAVIGATED: Self = Self {
        consumed: true,
        changed: false,
    };
    const CHANGED: Self = Self {
        consumed: true,
        changed: true,
    };
}

/// The one editing controller used by VT Code's text fields.
///
/// Cursor and selection positions are UTF-8 byte offsets. Keeping the state
/// borrowed lets dialogs retain their existing domain state while sharing all
/// editing behavior (selection, word movement, deletion, and clipboard).
pub(crate) struct TextInput<'a> {
    text: &'a mut String,
    cursor: &'a mut usize,
    selection_anchor: &'a mut Option<usize>,
    multiline: bool,
    accept: fn(char) -> bool,
}

impl<'a> TextInput<'a> {
    pub(crate) fn new(
        text: &'a mut String,
        cursor: &'a mut usize,
        selection_anchor: &'a mut Option<usize>,
    ) -> Self {
        Self {
            text,
            cursor,
            selection_anchor,
            multiline: false,
            accept: |_| true,
        }
    }

    pub(crate) fn multiline(mut self, multiline: bool) -> Self {
        self.multiline = multiline;
        self
    }

    pub(crate) fn filter(mut self, accept: fn(char) -> bool) -> Self {
        self.accept = accept;
        self
    }

    pub(crate) fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> TextInputResult {
        App::normalize_inline_cursor(self.text, self.cursor, self.selection_anchor);

        let shift = modifiers.contains(KeyModifiers::SHIFT);
        let alt = modifiers.contains(KeyModifiers::ALT);
        let primary = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) && !alt;

        match code {
            KeyCode::Char(c) if primary && c.eq_ignore_ascii_case(&'a') => {
                self.select_all();
                TextInputResult::NAVIGATED
            }
            KeyCode::Char(c) if primary && c.eq_ignore_ascii_case(&'c') => {
                self.copy_selection();
                TextInputResult::NAVIGATED
            }
            KeyCode::Char(c) if primary && c.eq_ignore_ascii_case(&'x') => {
                if self.cut_selection() {
                    TextInputResult::CHANGED
                } else {
                    TextInputResult::NAVIGATED
                }
            }
            KeyCode::Char(c) if primary && c.eq_ignore_ascii_case(&'v') => {
                if self.paste_clipboard() {
                    TextInputResult::CHANGED
                } else {
                    TextInputResult::NAVIGATED
                }
            }
            KeyCode::Insert if primary => {
                self.copy_selection();
                TextInputResult::NAVIGATED
            }
            KeyCode::Insert if shift => {
                if self.paste_clipboard() {
                    TextInputResult::CHANGED
                } else {
                    TextInputResult::NAVIGATED
                }
            }
            KeyCode::Delete if shift && !primary => {
                if self.cut_selection() {
                    TextInputResult::CHANGED
                } else {
                    TextInputResult::NAVIGATED
                }
            }
            KeyCode::Char(c)
                if (!primary && !alt)
                    || modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if (self.multiline || !matches!(c, '\r' | '\n'))
                    && !c.is_control()
                    && (self.accept)(c)
                {
                    App::inline_insert_char(self.text, self.cursor, self.selection_anchor, c);
                    TextInputResult::CHANGED
                } else {
                    TextInputResult::NAVIGATED
                }
            }
            KeyCode::Backspace => {
                if primary {
                    App::inline_backspace_word(self.text, self.cursor, self.selection_anchor);
                } else {
                    App::inline_backspace(self.text, self.cursor, self.selection_anchor);
                }
                TextInputResult::CHANGED
            }
            KeyCode::Delete => {
                if primary {
                    App::inline_delete_word(self.text, self.cursor, self.selection_anchor);
                } else {
                    App::inline_delete(self.text, self.cursor, self.selection_anchor);
                }
                TextInputResult::CHANGED
            }
            KeyCode::Left => {
                self.move_left(primary, shift);
                TextInputResult::NAVIGATED
            }
            KeyCode::Right => {
                self.move_right(primary, shift);
                TextInputResult::NAVIGATED
            }
            KeyCode::Home => {
                if primary || !self.multiline {
                    App::inline_move_home(self.text, self.cursor, self.selection_anchor, shift);
                } else {
                    self.move_line_edge(false, shift);
                }
                TextInputResult::NAVIGATED
            }
            KeyCode::End => {
                if primary || !self.multiline {
                    App::inline_move_end(self.text, self.cursor, self.selection_anchor, shift);
                } else {
                    self.move_line_edge(true, shift);
                }
                TextInputResult::NAVIGATED
            }
            KeyCode::Up if self.multiline => {
                self.move_vertical(false, shift);
                TextInputResult::NAVIGATED
            }
            KeyCode::Down if self.multiline => {
                self.move_vertical(true, shift);
                TextInputResult::NAVIGATED
            }
            KeyCode::Enter if self.multiline => {
                App::inline_insert_char(self.text, self.cursor, self.selection_anchor, '\n');
                TextInputResult::CHANGED
            }
            _ => TextInputResult::IGNORED,
        }
    }

    pub(crate) fn insert_text(&mut self, value: &str) -> bool {
        App::normalize_inline_cursor(self.text, self.cursor, self.selection_anchor);
        let accepted: String = value
            .chars()
            .filter(|&c| self.multiline || !matches!(c, '\r' | '\n'))
            .filter(|&c| c != '\r')
            .filter(|&c| (self.accept)(c))
            .collect();
        if accepted.is_empty() {
            return false;
        }

        if let Some((start, end)) =
            App::inline_selection_range(*self.cursor, *self.selection_anchor)
        {
            self.text.replace_range(start..end, &accepted);
            *self.cursor = start + accepted.len();
        } else {
            self.text.insert_str(*self.cursor, &accepted);
            *self.cursor += accepted.len();
        }
        *self.selection_anchor = None;
        true
    }

    fn select_all(&mut self) {
        if self.text.is_empty() {
            *self.cursor = 0;
            *self.selection_anchor = None;
        } else {
            *self.cursor = self.text.len();
            *self.selection_anchor = Some(0);
        }
    }

    fn selected_text(&self) -> Option<&str> {
        let (start, end) = App::inline_selection_range(*self.cursor, *self.selection_anchor)?;
        self.text.get(start..end)
    }

    fn copy_selection(&self) -> bool {
        let Some(selected) = self.selected_text() else {
            return false;
        };
        crate::clipboard::set_text(selected).is_ok()
    }

    fn cut_selection(&mut self) -> bool {
        if !self.copy_selection() {
            return false;
        }
        App::inline_delete(self.text, self.cursor, self.selection_anchor);
        true
    }

    fn paste_clipboard(&mut self) -> bool {
        let Ok(value) = crate::clipboard::get_text() else {
            return false;
        };
        self.insert_text(&value)
    }

    fn move_left(&mut self, by_word: bool, extend: bool) {
        if !extend {
            if let Some((start, _)) =
                App::inline_selection_range(*self.cursor, *self.selection_anchor)
            {
                *self.cursor = start;
                *self.selection_anchor = None;
                return;
            }
        }
        if by_word {
            App::inline_move_word_left(self.text, self.cursor, self.selection_anchor, extend);
        } else {
            App::inline_move_left(self.text, self.cursor, self.selection_anchor, extend);
        }
    }

    fn move_right(&mut self, by_word: bool, extend: bool) {
        if !extend {
            if let Some((_, end)) =
                App::inline_selection_range(*self.cursor, *self.selection_anchor)
            {
                *self.cursor = end;
                *self.selection_anchor = None;
                return;
            }
        }
        if by_word {
            App::inline_move_word_right(self.text, self.cursor, self.selection_anchor, extend);
        } else {
            App::inline_move_right(self.text, self.cursor, self.selection_anchor, extend);
        }
    }

    fn update_selection(&mut self, old_cursor: usize, new_cursor: usize, extend: bool) {
        if extend {
            if self.selection_anchor.is_none() {
                *self.selection_anchor = Some(old_cursor);
            }
            if *self.selection_anchor == Some(new_cursor) {
                *self.selection_anchor = None;
            }
        } else {
            *self.selection_anchor = None;
        }
    }

    fn line_bounds(&self) -> (usize, usize) {
        let cursor = App::clamp_to_char_boundary(self.text, *self.cursor);
        let start = self.text[..cursor].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |i| start + i);
        (start, end)
    }

    fn move_line_edge(&mut self, to_end: bool, extend: bool) {
        let old = *self.cursor;
        let (start, end) = self.line_bounds();
        *self.cursor = if to_end { end } else { start };
        self.update_selection(old, *self.cursor, extend);
    }

    fn byte_at_column(text: &str, start: usize, end: usize, target_col: usize) -> usize {
        text[start..end]
            .char_indices()
            .nth(target_col)
            .map_or(end, |(idx, _)| start + idx)
    }

    fn move_vertical(&mut self, down: bool, extend: bool) {
        let old = *self.cursor;
        let (start, end) = self.line_bounds();
        let col = self.text[start..old].chars().count();
        let target = if down {
            if end == self.text.len() {
                old
            } else {
                let next_start = end + 1;
                let next_end = self.text[next_start..]
                    .find('\n')
                    .map_or(self.text.len(), |idx| next_start + idx);
                Self::byte_at_column(self.text, next_start, next_end, col)
            }
        } else if start == 0 {
            old
        } else {
            let previous_end = start - 1;
            let previous_start = self.text[..previous_end]
                .rfind('\n')
                .map_or(0, |idx| idx + 1);
            Self::byte_at_column(self.text, previous_start, previous_end, col)
        };
        *self.cursor = target;
        self.update_selection(old, target, extend);
    }
}

#[cfg(test)]
pub(crate) fn selection_char_range(
    text: &str,
    cursor: usize,
    selection_anchor: Option<usize>,
) -> Option<(usize, usize)> {
    let (start, end) = App::inline_selection_range(cursor, selection_anchor)?;
    let start = App::clamp_to_char_boundary(text, start);
    let end = App::clamp_to_char_boundary(text, end);
    Some((text[..start].chars().count(), text[..end].chars().count()))
}

impl App {
    pub(crate) fn text_selection_unit(text: &str, byte: usize, line: bool) -> (usize, usize) {
        let byte = Self::clamp_to_grapheme_boundary(text, byte);
        let start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
        let end = text[byte..].find('\n').map_or(text.len(), |i| byte + i);
        if line {
            return (start, (end + usize::from(end < text.len())).min(text.len()));
        }
        let class = |grapheme: &str| {
            let c = grapheme.chars().next().unwrap_or(' ');
            if c.is_alphanumeric() || c == '_' {
                0
            } else if c.is_whitespace() {
                1
            } else {
                2
            }
        };
        let at = if byte == end {
            text[start..byte]
                .grapheme_indices(true)
                .last()
                .map_or(byte, |(i, _)| start + i)
        } else {
            byte
        };
        let Some(ch) = text[at..end].graphemes(true).next() else {
            return (at, at);
        };
        let kind = class(ch);
        let left = text[start..at]
            .grapheme_indices(true)
            .rev()
            .take_while(|(_, c)| class(*c) == kind)
            .last()
            .map_or(at, |(i, _)| start + i);
        let right = text[at..end]
            .grapheme_indices(true)
            .take_while(|(_, c)| class(*c) == kind)
            .last()
            .map_or(at, |(i, c)| at + i + c.len());
        (left, right)
    }

    pub(crate) fn register_text_input(&mut self, id: TextInputId, hit_map: TextInputHitMap) {
        let file_search_owner = matches!(id, TextInputId::SearchFind | TextInputId::SearchReplace)
            .then(|| self.file_search_owner()).flatten();
        self.text_input_regions
            .push(TextInputRegion { id, hit_map, file_search_owner });
    }

    pub(crate) fn text_input_area(&self, id: TextInputId) -> Option<Rect> {
        self.text_input_regions.iter().rev()
            .find(|region| region.id == id && self.text_input_region_is_current(region))
            .map(|region| region.hit_map.area)
    }

    pub(crate) fn remove_text_input_regions(&mut self, matches: impl Fn(TextInputId) -> bool) {
        self.text_input_regions.retain(|region| !matches(region.id));
    }

    fn text_input_state_mut(
        &mut self,
        id: TextInputId,
    ) -> (&mut String, &mut usize, &mut Option<usize>) {
        if matches!(id, TextInputId::SearchFind | TextInputId::SearchReplace) {
            let state: &mut crate::file_search::State = self;
            return if id == TextInputId::SearchFind {
                (&mut state.search_query, &mut state.search_cursor, &mut state.search_selection_anchor)
            } else {
                (&mut state.replace_query, &mut state.replace_cursor, &mut state.replace_selection_anchor)
            };
        }
        match id {
            TextInputId::ScopeBubbleLanguage => {
                let field = &mut self.scope_bubble_settings.field;
                (&mut field.text, &mut field.cursor, &mut field.anchor)
            }
            TextInputId::Exclusion(index) => {
                let field = &mut self.exclusion_settings.fields[index];
                (&mut field.text, &mut field.cursor, &mut field.anchor)
            }
            TextInputId::FileAssociation(index) => {
                let field = &mut self.file_associations.fields[index];
                (&mut field.text, &mut field.cursor, &mut field.anchor)
            }
            TextInputId::Chat => {
                let state = &mut self.chat.connection;
                (&mut state.input, &mut state.cursor, &mut state.anchor)
            }
            TextInputId::AgentSetting(id) => {
                let field = &mut self.agent_settings.draft.as_mut().expect("agent form is open").fields[id];
                (&mut field.text, &mut field.cursor, &mut field.anchor)
            }
            TextInputId::LspSetting(id) => {
                let field = &mut self.lsp_settings.draft.as_mut().expect("LSP form is open").fields[id];
                (&mut field.text, &mut field.cursor, &mut field.anchor)
            }
            TextInputId::FormatterSetting(id) => {
                let field = &mut self.formatter_settings.fields[id];
                (&mut field.text, &mut field.cursor, &mut field.anchor)
            }
            TextInputId::FolderAddress => (
                &mut self.change_dir_query,
                &mut self.folder_browser.address_cursor,
                &mut self.folder_browser.address_anchor,
            ),
            TextInputId::FolderSearch => (
                &mut self.folder_browser.search,
                &mut self.folder_browser.search_cursor,
                &mut self.folder_browser.search_anchor,
            ),
            TextInputId::FolderName => (
                &mut self.folder_browser.folder,
                &mut self.folder_browser.folder_cursor,
                &mut self.folder_browser.folder_anchor,
            ),
            TextInputId::SearchFind | TextInputId::SearchReplace => unreachable!(),
            TextInputId::QuickOpen => (
                &mut self.quick_open_query,
                &mut self.quick_open_cursor,
                &mut self.quick_open_selection_anchor,
            ),
            TextInputId::GotoLine => (
                &mut self.goto_line_query,
                &mut self.goto_line_cursor,
                &mut self.goto_line_selection_anchor,
            ),
            TextInputId::GlobalSearch(field) => self.global_search_field_state_mut(field),
            TextInputId::ExplorerFind => (
                &mut self.explorer.find.query,
                &mut self.explorer.find.cursor,
                &mut self.explorer.find.anchor,
            ),
            TextInputId::GitBranch => (
                &mut self.git_branch_picker_query,
                &mut self.git_branch_picker_cursor,
                &mut self.git_branch_picker_selection_anchor,
            ),
            TextInputId::CommandPalette => (
                &mut self.palette.query,
                &mut self.palette.cursor,
                &mut self.palette.selection_anchor,
            ),
            TextInputId::SymbolList => (
                &mut self.symbol_list_query,
                &mut self.symbol_list_cursor,
                &mut self.symbol_list_selection_anchor,
            ),
            TextInputId::TaskPicker => (
                &mut self.task_picker_query,
                &mut self.task_picker_cursor,
                &mut self.task_picker_selection_anchor,
            ),
            TextInputId::SaveAs => (
                &mut self.save_as_query,
                &mut self.save_as_cursor,
                &mut self.save_as_selection_anchor,
            ),
            TextInputId::RenameSymbol => (
                &mut self.rename.symbol_new,
                &mut self.rename.symbol_cursor,
                &mut self.rename.symbol_selection_anchor,
            ),
            TextInputId::TerminalRename => (
                &mut self.terminal_rename.query,
                &mut self.terminal_rename.cursor,
                &mut self.terminal_rename.anchor,
            ),
            TextInputId::SettingsSearch => (
                &mut self.settings_editor_search,
                &mut self.settings_editor_search_cursor,
                &mut self.settings_editor_search_selection_anchor,
            ),
            TextInputId::SettingsValue => (
                &mut self.settings_value_buffer,
                &mut self.settings_value_cursor,
                &mut self.settings_value_selection_anchor,
            ),
            TextInputId::ExtensionsFilter => (
                &mut self.extensions_filter,
                &mut self.extensions_filter_cursor,
                &mut self.extensions_filter_selection_anchor,
            ),
            TextInputId::ExplorerRename => (
                &mut self.rename.query,
                &mut self.rename.cursor,
                &mut self.rename.selection_anchor,
            ),
            TextInputId::SourceControlCommit => (
                &mut self.source_control_commit_msg,
                &mut self.source_control_commit_cursor,
                &mut self.source_control_commit_selection_anchor,
            ),
        }
    }

    pub(crate) fn focus_text_input(&mut self, id: TextInputId) {
        if matches!(
            id,
            TextInputId::SaveAs
                | TextInputId::RenameSymbol
                | TextInputId::TerminalRename
                | TextInputId::GotoLine
                | TextInputId::CommandPalette
        ) {
            self.modal.focus = 0;
        }
        self.terminal_panel_focused = false;
        if id != TextInputId::SettingsValue && self.settings_value_editing {
            self.finish_numeric_setting_edit();
        }
        match id {
            TextInputId::FolderAddress => self.folder_browser.focus = crate::folder_browser::Focus::Address,
            TextInputId::AgentSetting(id) => {
                self.agent_settings.focus = crate::ui_agent_settings::Action::Field(id);
                self.agent_settings.reveal_focus = true;
            }
            TextInputId::LspSetting(id) => {
                self.lsp_settings.focus = crate::ui_lsp_settings::Action::Field(id);
                self.lsp_settings.reveal_focus = true;
            }
            TextInputId::FormatterSetting(id) => {
                self.formatter_settings.focus = crate::ui_formatter_settings::Action::Field(id);
                self.formatter_settings.reveal_focus = true;
            }
            TextInputId::ScopeBubbleLanguage => {
                self.scope_bubble_settings.focus = crate::ui_scope_bubble_settings::Action::Language;
                self.scope_bubble_settings.reveal_focus = true;
            }
            TextInputId::Exclusion(id) => { self.exclusion_settings.focus = crate::ui_exclusion_settings::Action::Field(id); }
            TextInputId::FileAssociation(id) => {
                self.file_associations.focus = crate::ui_file_associations::Action::Field(id);
                self.file_associations.reveal_focus = true;
            }
            TextInputId::Chat => self.focus_chat(),
            TextInputId::FolderSearch => self.folder_browser.focus = crate::folder_browser::Focus::Search,
            TextInputId::FolderName => self.folder_browser.focus = crate::folder_browser::Focus::Folder,
            TextInputId::SearchFind => {
                self.active_pane = crate::app::ActivePane::Editor;
                self.search_input_focused = true;
                self.search_focus_replace = false;
            }
            TextInputId::SearchReplace => {
                self.active_pane = crate::app::ActivePane::Editor;
                self.search_input_focused = true;
                self.search_focus_replace = true;
            }
            TextInputId::GlobalSearch(field) => {
                self.search_input_focused = false;
                self.search_focus_field = field.min(3);
                self.global_search_active = true;
                self.active_pane = crate::app::ActivePane::Explorer;
            }
            TextInputId::ExplorerFind => {
                self.active_pane = crate::app::ActivePane::Explorer;
                self.explorer.find.focus = crate::explorer_find::Focus::Input;
            }
            TextInputId::SettingsSearch => {
                self.settings_focus_categories = false;
                self.settings_focus_control = false;
                self.active_pane = crate::app::ActivePane::Editor;
            }
            TextInputId::SettingsValue => {
                self.settings_focus_categories = false;
                self.settings_focus_control = true;
                self.active_pane = crate::app::ActivePane::Editor;
            }
            TextInputId::ExtensionsFilter => {
                self.active_pane = crate::app::ActivePane::Explorer;
                self.ext_sidebar.search_focused = true;
            }
            TextInputId::ExplorerRename => {
                self.active_pane = crate::app::ActivePane::Explorer;
            }
            TextInputId::SourceControlCommit => {
                self.source_control_input_mode = true;
                self.active_pane = crate::app::ActivePane::Explorer;
            }
            _ => {}
        }
        self.cursor_visible = true;
        self.last_blink = std::time::Instant::now();
    }

    fn set_text_input_cursor(&mut self, id: TextInputId, byte: usize, anchor: Option<usize>) {
        let (text, cursor, selection_anchor) = self.text_input_state_mut(id);
        let byte = Self::clamp_to_grapheme_boundary(text, byte);
        let anchor = anchor.map(|value| Self::clamp_to_grapheme_boundary(text, value));
        *cursor = byte;
        *selection_anchor = anchor.filter(|value| *value != byte);
    }

    pub(crate) fn focused_text_input_id(&self) -> Option<TextInputId> {
        if self.change_dir_active {
            return self.folder_browser.focus.input();
        }
        if let Some(kind) = crate::ui_modal::active(self) {
            if kind == crate::ui_modal::Kind::AgentSettings {
                return self.agent_settings.focused_input();
            }
            if kind == crate::ui_modal::Kind::LspSettings {
                return self.lsp_settings.focused_input();
            }
            if kind == crate::ui_modal::Kind::FormatterSettings {
                return self.formatter_settings.focused_input();
            }
            if kind == crate::ui_modal::Kind::ScopeBubbleSettings {
                return self.scope_bubble_settings.focused_input();
            }
            if kind == crate::ui_modal::Kind::ExclusionSettings { return self.exclusion_settings.focused_input(); }
            if kind == crate::ui_modal::Kind::FileAssociations {
                return self.file_associations.focused_input();
            }
            return kind
                .input()
                .filter(|_| self.modal.kind != Some(kind) || self.modal.focus == 0);
        }
        if let Some(kind) = crate::ui_quick_picker::active(self) {
            return kind.input();
        }
        if self.rename.active {
            return Some(TextInputId::ExplorerRename);
        }
        if self.save_as_active {
            return Some(TextInputId::SaveAs);
        }
        if self.rename.symbol_active {
            return Some(TextInputId::RenameSymbol);
        }
        if self.symbol_list_active {
            return Some(TextInputId::SymbolList);
        }
        if self.task_picker_active {
            return Some(TextInputId::TaskPicker);
        }
        if self.palette.active {
            return Some(TextInputId::CommandPalette);
        }
        if self.goto_line_active {
            return Some(TextInputId::GotoLine);
        }
        if self.git_branch_picker_active {
            return Some(TextInputId::GitBranch);
        }
        if self.quick_open_active {
            return Some(TextInputId::QuickOpen);
        }
        let sidebar_search_active =
            self.show_explorer && self.sidebar_view == crate::app::SidebarView::Search;
        if self.global_search_active
            && (!sidebar_search_active || self.active_pane == crate::app::ActivePane::Explorer)
        {
            return Some(TextInputId::GlobalSearch(self.search_focus_field));
        }
        if self.show_explorer && self.sidebar_view == crate::app::SidebarView::Explorer
            && self.active_pane == crate::app::ActivePane::Explorer
            && self.explorer.find.open && self.explorer.find.focus == crate::explorer_find::Focus::Input
        {
            return Some(TextInputId::ExplorerFind);
        }
        if self.active_pane == crate::app::ActivePane::Editor
            && self.file_search_visible() && self.search_input_focused
            && !self.docked_terminal_has_focus()
        {
            return Some(if self.search_focus_replace {
                TextInputId::SearchReplace
            } else {
                TextInputId::SearchFind
            });
        }
        if self.settings_editor_has_focus() && self.settings_value_editing {
            return Some(TextInputId::SettingsValue);
        }
        if self.settings_editor_has_focus()
            && !self.settings_focus_categories
            && !self.settings_focus_control
            && !self.settings_kb_editing
            && !self.settings_dropdown_open
        {
            return Some(TextInputId::SettingsSearch);
        }
        if self.sidebar_view == crate::app::SidebarView::SourceControl
            && self.active_pane == crate::app::ActivePane::Explorer
            && self.source_control_input_mode
        {
            return Some(TextInputId::SourceControlCommit);
        }
        if self.sidebar_view == crate::app::SidebarView::Extensions
            && self.active_pane == crate::app::ActivePane::Explorer
            && self.ext_sidebar.search_focused
        {
            return Some(TextInputId::ExtensionsFilter);
        }
        if self.chat.visible && self.active_pane == crate::app::ActivePane::Chat && self.chat.picker.is_none() {
            return Some(TextInputId::Chat);
        }
        None
    }

    /// Retain history while a field is open, but never resurrect a previous
    /// dialog's edits when a new dialog happens to start with identical text.
    pub(crate) fn sync_text_input_sessions(&mut self) {
        if !self.search_active { self.input_histories = Default::default(); }
        let closed: Vec<_> = self
            .text_input_histories
            .keys()
            .copied()
            .filter(|id| !match id {
                TextInputId::FolderAddress | TextInputId::FolderSearch | TextInputId::FolderName => self.change_dir_active,
                TextInputId::SearchFind | TextInputId::SearchReplace => self.search_active,
                TextInputId::QuickOpen => self.quick_open_active,
                TextInputId::ExplorerFind => self.explorer.find.open,
                TextInputId::GotoLine => self.goto_line_active,
                TextInputId::GlobalSearch(_) => {
                    self.global_search_active
                        || self.sidebar_view == crate::app::SidebarView::Search
                }
                TextInputId::GitBranch => self.git_branch_picker_active,
                TextInputId::CommandPalette => self.palette.active,
                TextInputId::SymbolList => self.symbol_list_active,
                TextInputId::TaskPicker => self.task_picker_active,
                TextInputId::SaveAs => self.save_as_active,
                TextInputId::RenameSymbol => self.rename.symbol_active,
                TextInputId::TerminalRename => self.terminal_rename.target.is_some(),
                TextInputId::SettingsSearch => self.settings_editor_active,
                TextInputId::SettingsValue => self.settings_value_editing,
                TextInputId::AgentSetting(_) => self.agent_settings.active && self.agent_settings.draft.is_some(),
                TextInputId::LspSetting(_) => self.lsp_settings.active && self.lsp_settings.draft.is_some(),
                TextInputId::FormatterSetting(_) => self.formatter_settings.active,
                TextInputId::ScopeBubbleLanguage => self.scope_bubble_settings.active && self.scope_bubble_settings.editing,
                TextInputId::Exclusion(_) => self.exclusion_settings.active && self.exclusion_settings.editing,
                TextInputId::FileAssociation(_) => self.file_associations.active && self.file_associations.editing,
                TextInputId::ExtensionsFilter => {
                    self.sidebar_view == crate::app::SidebarView::Extensions
                }
                TextInputId::ExplorerRename => self.rename.active,
                TextInputId::SourceControlCommit => true,
                TextInputId::Chat => true,
            })
            .collect();
        for id in closed {
            self.text_input_histories.remove(&id);
        }
    }

    /// Route editing shortcuts before workbench/editor keybindings get a
    /// chance to claim Ctrl+A/C/X/V and word-navigation combinations.
    pub(crate) fn handle_focused_text_input_shortcut(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        let primary = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && !modifiers.contains(KeyModifiers::ALT);
        // Windows reports AltGr-produced text as Ctrl+Alt+Char. A focused
        // field must consume it before the editor's AltGr fallback runs.
        let altgr_text = modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(code, KeyCode::Char(_));
        let editing_shortcut = (primary
            && matches!(
                code,
                KeyCode::Char(
                    'a' | 'A' | 'c' | 'C' | 'x' | 'X' | 'v' | 'V' | 'z' | 'Z' | 'y' | 'Y'
                ) | KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Insert
            ))
            || (modifiers.contains(KeyModifiers::SHIFT)
                && matches!(code, KeyCode::Insert | KeyCode::Delete))
            || altgr_text;
        if !editing_shortcut {
            return false;
        }
        let Some(id) = self.focused_text_input_id() else {
            return false;
        };

        let result = self.edit_text_input(id, code, modifiers, None);
        if !result.consumed {
            return false;
        }
        if result.changed {
            self.text_input_changed(id);
        }
        true
    }

    pub(crate) fn text_input_changed(&mut self, id: TextInputId) {
        match id {
            TextInputId::Chat => self.refresh_chat_file_completion(),
            TextInputId::AgentSetting(_) => {
                self.agent_settings.error = None;
                self.agent_settings.reveal_focus = true;
            }
            TextInputId::LspSetting(_) => {
                self.lsp_settings.error = None;
                self.lsp_settings.reveal_focus = true;
            }
            TextInputId::FormatterSetting(_) => {
                crate::ui_formatter_settings::input_changed(self);
            }
            TextInputId::ScopeBubbleLanguage => crate::ui_scope_bubble_settings::input_changed(self),
            TextInputId::Exclusion(_) => { self.exclusion_settings.error = None; }
            TextInputId::FileAssociation(_) => {
                self.file_associations.hits.clear();
                self.file_associations.error = None;
                self.file_associations.reveal_focus = true;
            }
            TextInputId::SaveAs | TextInputId::RenameSymbol | TextInputId::TerminalRename => crate::ui_modal::field_changed(self),
            TextInputId::FolderSearch => self.filter_folder_browser(),
            TextInputId::FolderAddress | TextInputId::FolderName => self.folder_browser.error = None,
            TextInputId::QuickOpen => self.update_quick_open(),
            TextInputId::ExplorerFind => self.update_explorer_find(),
            TextInputId::GitBranch => {
                self.git_branch_dialog.field_changed();
                self.git_branch_picker_selected = 0;
                self.git_branch_picker_scroll_offset = 0;
            }
            TextInputId::GotoLine => {
                if self.goto_line_query.is_empty() {
                    self.restore_goto_line_origin();
                } else {
                    self.preview_goto_line();
                }
            }
            TextInputId::GlobalSearch(field) => {
                self.global_search_field_history_mut(field).edited();
                if field != 1 { self.schedule_global_search(); }
            }
            TextInputId::CommandPalette => {
                self.palette.theme_install_error = None;
                if self.palette.extension_versions.is_some() {
                    self.filter_extension_versions();
                } else {
                    self.update_palette_filter();
                }
            }
            TextInputId::SymbolList => self.update_symbol_filter(),
            TextInputId::TaskPicker => self.task_picker_selected = 0,
            TextInputId::SearchFind => {
                self.search_history.edited();
                self.update_search();
            }
            TextInputId::SettingsSearch => {
                self.settings_focus_control = false;
                self.settings_editor_scroll = 0;
                self.ensure_settings_selection_visible();
            }
            TextInputId::SettingsValue => {}
            TextInputId::ExtensionsFilter => {
                self.extensions_selected = 0;
                self.extensions_scroll = 0;
                self.ext_sidebar.collapsed = false;
                self.clamp_extensions_selection();
            }
            TextInputId::SearchReplace
            | TextInputId::ExplorerRename
            | TextInputId::SourceControlCommit => {}
        }
    }

    pub(crate) fn edit_text_input(
        &mut self,
        id: TextInputId,
        code: KeyCode,
        modifiers: KeyModifiers,
        paste: Option<&str>,
    ) -> TextInputResult {
        if id == TextInputId::GitBranch && !self.git_branch_dialog.accepts_text() {
            return TextInputResult { consumed: true, changed: false };
        }
        if id == TextInputId::Chat {
            let before = self.chat.input.clone();
            self.edit_chat_input(code, modifiers, paste);
            return TextInputResult { consumed: true, changed: before != self.chat.input };
        }
        for (other, history) in &mut self.text_input_histories {
            if *other != id {
                history.break_group();
            }
        }
        if id != TextInputId::SearchFind { self.input_histories[0].break_group(); }
        if id != TextInputId::SearchReplace { self.input_histories[1].break_group(); }
        if id == TextInputId::SourceControlCommit {
            let before = self.source_control_commit_msg.clone();
            self.edit_commit_input(code, modifiers, paste);
            return TextInputResult {
                consumed: true,
                changed: before != self.source_control_commit_msg,
            };
        }
        let mut history = match id {
            TextInputId::SearchFind => std::mem::take(&mut self.input_histories[0]),
            TextInputId::SearchReplace => std::mem::take(&mut self.input_histories[1]),
            _ => self.text_input_histories.remove(&id).unwrap_or_default(),
        };
        // Starting a new search by replacing the entire field commits the old
        // query, without filling history with every intermediate keystroke.
        let previous_query = if matches!(id, TextInputId::SearchFind | TextInputId::GlobalSearch(_)) {
            let (text, cursor, anchor) = self.text_input_state_mut(id);
            (!text.is_empty() && Self::inline_selection_range(*cursor, *anchor) == Some((0, text.len())))
                .then(|| text.clone())
        } else {
            None
        };
        let numeric_setting = id == TextInputId::SettingsValue
            && self.settings_value_setting_id.as_deref().is_none_or(|setting| {
                Self::setting_field_kind(setting) == "number"
            });
        let (text, cursor, anchor) = self.text_input_state_mut(id);
        let mut input = TextInput::new(text, cursor, anchor);
        if id == TextInputId::GotoLine {
            input = input.filter(|c| c.is_ascii_digit() || c == ':');
        } else if numeric_setting || id == TextInputId::AgentSetting(2) {
            input = input.filter(|c| c.is_ascii_digit());
        }
        let result = history.edit(input, code, modifiers, paste);
        match id {
            TextInputId::SearchFind => self.input_histories[0] = history,
            TextInputId::SearchReplace => self.input_histories[1] = history,
            _ => { self.text_input_histories.insert(id, history); }
        }
        if result.changed && let Some(query) = previous_query {
            match id {
                TextInputId::SearchFind => self.search_history.record(&query),
                TextInputId::GlobalSearch(field) => self.global_search_field_history_mut(field).record(&query),
                _ => unreachable!(),
            }
        }
        result
    }

    fn text_input_is_overlay(&self, id: TextInputId) -> bool {
        match id {
            TextInputId::AgentSetting(_) | TextInputId::LspSetting(_) | TextInputId::FormatterSetting(_) | TextInputId::FileAssociation(_) | TextInputId::Exclusion(_) | TextInputId::ScopeBubbleLanguage => true,
            TextInputId::FolderAddress | TextInputId::FolderSearch | TextInputId::FolderName => true,
            TextInputId::SearchFind
            | TextInputId::SearchReplace
            | TextInputId::QuickOpen
            | TextInputId::GotoLine
            | TextInputId::GitBranch
            | TextInputId::CommandPalette
            | TextInputId::SymbolList
            | TextInputId::TaskPicker
            | TextInputId::SaveAs
            | TextInputId::RenameSymbol
            | TextInputId::TerminalRename => true,
            TextInputId::GlobalSearch(_) => {
                !(self.show_explorer && self.sidebar_view == crate::app::SidebarView::Search)
            }
            TextInputId::SettingsSearch
            | TextInputId::ExplorerFind
            | TextInputId::SettingsValue
            | TextInputId::Chat
            | TextInputId::ExtensionsFilter
            | TextInputId::ExplorerRename
            | TextInputId::SourceControlCommit => false,
        }
    }

    fn text_input_region_is_current(&self, region: &TextInputRegion) -> bool {
        !matches!(region.id, TextInputId::SearchFind | TextInputId::SearchReplace)
            || region.file_search_owner == self.file_search_owner()
    }

    /// Handle pointer editing for every registered text field.  The same hit
    /// map is used for click-to-place, Shift-click, and drag selection, so a
    /// clipped field cannot render one viewport while clicking another.
    #[cfg(test)]
    pub(crate) fn handle_text_input_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        self.handle_text_input_mouse_scoped(kind, column, row, modifiers, None)
    }

    pub(crate) fn handle_overlay_text_input_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        self.handle_text_input_mouse_scoped(kind, column, row, modifiers, Some(true))
    }

    pub(crate) fn handle_background_text_input_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        // Menus are drawn above workbench fields. Let menu routing handle
        // their clicks before an obscured field can focus or select text.
        if self.menu_active && self.menu_open {
            return false;
        }
        self.handle_text_input_mouse_scoped(kind, column, row, modifiers, Some(false))
    }

    fn handle_text_input_mouse_scoped(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
        overlay: Option<bool>,
    ) -> bool {
        if overlay != Some(true) && self.handle_commit_input_scrollbar_mouse(kind, column, row) {
            return true;
        }
        match kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if overlay == Some(true)
                    || !self.text_input_regions.iter().any(|region| {
                        region.id == TextInputId::SourceControlCommit
                            && region.hit_map.contains(column, row)
                    })
                {
                    return false;
                }
                let max_scroll = self
                    .commit_input
                    .rows
                    .len()
                    .saturating_sub(self.commit_input.height);
                self.commit_input.scroll = if kind == MouseEventKind::ScrollUp {
                    self.commit_input.scroll.saturating_sub(3)
                } else {
                    (self.commit_input.scroll + 3).min(max_scroll)
                };
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some((id, byte, owner)) = self
                    .text_input_regions
                    .iter()
                    .rev()
                    .find(|region| {
                        overlay.is_none_or(|value| self.text_input_is_overlay(region.id) == value)
                            && region.hit_map.contains(column, row)
                    })
                    .map(|region| (region.id, region.hit_map.byte_at(column, row), region.file_search_owner))
                else {
                    // An overlay pass that misses must not erase a background
                    // field's gesture before the background pass sees the click.
                    if overlay.is_none_or(|scope| {
                        self.text_input_click_id
                            .is_none_or(|id| self.text_input_is_overlay(id) == scope)
                    }) {
                        self.text_input_drag = None;
                        self.text_input_click_id = None;
                        self.text_input_drag_unit = None;
                    }
                    return false;
                };

                if matches!(id, TextInputId::SearchFind | TextInputId::SearchReplace) {
                    self.focus_file_search_owner(owner);
                }
                self.focus_text_input(id);
                let extend = modifiers.contains(KeyModifiers::SHIFT);
                if id == TextInputId::SourceControlCommit {
                    self.break_commit_input_group();
                } else if id == TextInputId::SearchFind {
                    self.input_histories[0].break_group();
                } else if id == TextInputId::SearchReplace {
                    self.input_histories[1].break_group();
                } else if let Some(history) = self.text_input_histories.get_mut(&id) {
                    history.break_group();
                }
                self.text_input_drag_unit = None;
                let now = std::time::Instant::now();
                self.click_count = if self.text_input_click_id == Some(id)
                    && now.duration_since(self.last_click_time).as_millis() < 400
                    && self.last_click_pos == (column, row)
                {
                    (self.click_count + 1).min(3)
                } else {
                    1
                };
                self.text_input_click_id = Some(id);
                self.last_click_time = now;
                self.last_click_pos = (column, row);
                if !extend && self.click_count >= 2 {
                    let line = self.click_count >= 3;
                    let (text, _, _) = self.text_input_state_mut(id);
                    let (start, end) = Self::text_selection_unit(text, byte, line);
                    self.set_text_input_cursor(id, end, Some(start));
                    self.text_input_drag_unit = Some((start, end, line));
                    self.text_input_drag = Some((id, start));
                    return true;
                }
                let drag_anchor = {
                    let (text, cursor, selection_anchor) = self.text_input_state_mut(id);
                    Self::normalize_inline_cursor(text, cursor, selection_anchor);
                    if extend {
                        selection_anchor.unwrap_or(*cursor)
                    } else {
                        byte
                    }
                };
                self.set_text_input_cursor(id, byte, extend.then_some(drag_anchor));
                self.text_input_drag = Some((id, drag_anchor));
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some((id, anchor)) = self.text_input_drag else {
                    return false;
                };
                if overlay.is_some_and(|value| self.text_input_is_overlay(id) != value) {
                    return false;
                }
                let Some((mut byte, area)) = self
                    .text_input_regions
                    .iter()
                    .rev()
                    .find(|region| region.id == id && self.text_input_region_is_current(region))
                    .map(|region| (region.hit_map.byte_at(column, row), region.hit_map.area))
                else {
                    return false;
                };
                self.focus_text_input(id);
                if id == TextInputId::SourceControlCommit {
                    if row < area.y || row >= area.bottom() {
                        let max_scroll = self
                            .commit_input
                            .rows
                            .len()
                            .saturating_sub(self.commit_input.height);
                        self.commit_input.scroll = if row < area.y {
                            self.commit_input.scroll.saturating_sub(1)
                        } else {
                            (self.commit_input.scroll + 1).min(max_scroll)
                        };
                        let view_row = row
                            .saturating_sub(area.y)
                            .min(area.height.saturating_sub(1))
                            as usize;
                        byte = self.commit_input.byte_at_column(
                            self.commit_input.scroll + view_row,
                            column.saturating_sub(area.x) as usize,
                        );
                    }
                } else {
                    // Move beyond the clipped edge so the next paint scrolls
                    // the same field viewport used by hit testing.
                    let (text, cursor, _) = self.text_input_state_mut(id);
                    if column < area.x {
                        byte = text[..*cursor]
                            .grapheme_indices(true)
                            .next_back()
                            .map_or(0, |(i, _)| i);
                    } else if column >= area.right() {
                        byte = text[*cursor..]
                            .graphemes(true)
                            .next()
                            .map_or(*cursor, |g| *cursor + g.len());
                    }
                }
                if let Some((start, end, line)) = self.text_input_drag_unit {
                    let (text, _, _) = self.text_input_state_mut(id);
                    let (left, right) = Self::text_selection_unit(text, byte, line);
                    let (cursor, anchor) = if byte < start {
                        (left, end)
                    } else {
                        (right.max(end), start)
                    };
                    self.set_text_input_cursor(id, cursor, Some(anchor));
                    return true;
                }
                self.set_text_input_cursor(id, byte, Some(anchor));
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.text_input_drag.is_some() => {
                if overlay.is_some_and(|scope| {
                    self.text_input_drag
                        .is_some_and(|(id, _)| self.text_input_is_overlay(id) != scope)
                }) {
                    return false;
                }
                self.text_input_drag = None;
                self.text_input_drag_unit = None;
                true
            }
            _ => false,
        }
    }

    /// Route bracketed-paste text to the currently focused shared text field.
    /// Returns false when focus belongs to the code editor or another control.
    pub(crate) fn paste_into_focused_text_input(&mut self, value: &str) -> bool {
        if self.theme_install_prompt_visible() && self.install_prompt_running() {
            return true;
        }
        self.sync_text_input_sessions();
        let Some(id) = self.focused_text_input_id() else {
            return self.change_dir_active
                || crate::ui_modal::active(self).is_some()
                || crate::ui_quick_picker::active(self).is_some();
        };
        let result = self.edit_text_input(id, KeyCode::Null, KeyModifiers::NONE, Some(value));
        if result.changed {
            self.text_input_changed(id);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELDS: &[TextInputId] = &[
        TextInputId::SearchFind,
        TextInputId::SearchReplace,
        TextInputId::QuickOpen,
        TextInputId::GotoLine,
        TextInputId::GlobalSearch(0),
        TextInputId::GlobalSearch(1),
        TextInputId::GlobalSearch(2),
        TextInputId::GlobalSearch(3),
        TextInputId::ExplorerFind,
        TextInputId::GitBranch,
        TextInputId::CommandPalette,
        TextInputId::SymbolList,
        TextInputId::TaskPicker,
        TextInputId::SaveAs,
        TextInputId::FolderAddress,
        TextInputId::FolderSearch,
        TextInputId::FolderName,
        TextInputId::RenameSymbol,
        TextInputId::TerminalRename,
        TextInputId::SettingsSearch,
        TextInputId::SettingsValue,
        TextInputId::AgentSetting(0),
        TextInputId::LspSetting(0),
        TextInputId::FormatterSetting(0),
        TextInputId::ScopeBubbleLanguage,
        TextInputId::FileAssociation(0),
        TextInputId::FileAssociation(1),
        TextInputId::ExtensionsFilter,
        TextInputId::ExplorerRename,
        TextInputId::SourceControlCommit,
        TextInputId::Chat,
    ];

    fn activate_field(app: &mut App, id: TextInputId) {
        app.active_pane = crate::app::ActivePane::Editor;
        match id {
            TextInputId::AgentSetting(_) => {
                app.open_agent_settings();
                crate::ui_agent_settings::activate(app, crate::ui_agent_settings::Action::Add);
                app.focus_text_input(id);
            }
            TextInputId::LspSetting(_) => {
                app.open_lsp_settings();
                crate::ui_lsp_settings::activate(app, crate::ui_lsp_settings::Action::Add);
                app.focus_text_input(id);
            }
            TextInputId::FormatterSetting(_) => {
                app.open_formatter_settings(None);
                app.focus_text_input(id);
            }
            TextInputId::ScopeBubbleLanguage => {
                app.open_scope_bubble_settings();
                crate::ui_scope_bubble_settings::activate(app, crate::ui_scope_bubble_settings::Action::New);
                app.focus_text_input(id);
            }
            TextInputId::Exclusion(_) => { app.open_exclusion_settings("files_exclude", None, true); }
            TextInputId::FileAssociation(_) => {
                app.open_file_associations();
                crate::ui_file_associations::activate(app, crate::ui_file_associations::Action::New);
                app.focus_text_input(id);
            }
            TextInputId::Chat => app.focus_chat(),
            TextInputId::FolderAddress | TextInputId::FolderSearch | TextInputId::FolderName => {
                app.change_dir_active = true;
                app.focus_text_input(id);
            }
            TextInputId::SearchFind | TextInputId::SearchReplace => {
                app.search_active = true;
                app.search_input_focused = true;
                app.replace_mode = id == TextInputId::SearchReplace;
                app.search_focus_replace = app.replace_mode;
            }
            TextInputId::QuickOpen => app.quick_open_active = true,
            TextInputId::ExplorerFind => app.open_explorer_find(),
            TextInputId::GotoLine => app.goto_line_active = true,
            TextInputId::GlobalSearch(field) => {
                app.global_search_active = true;
                app.search_focus_field = field;
            }
            TextInputId::GitBranch => app.git_branch_picker_active = true,
            TextInputId::CommandPalette => app.palette.active = true,
            TextInputId::SymbolList => app.symbol_list_active = true,
            TextInputId::TaskPicker => app.task_picker_active = true,
            TextInputId::SaveAs => app.save_as_active = true,
            TextInputId::RenameSymbol => app.rename.symbol_active = true,
            TextInputId::TerminalRename => {
                app.new_terminal_tab();
                app.begin_terminal_rename(app.active_terminal);
            }
            TextInputId::SettingsSearch | TextInputId::SettingsValue => {
                app.settings_editor_active = true;
                app.settings_focus_categories = false;
                if id == TextInputId::SettingsValue {
                    app.begin_numeric_setting_edit("tab_size", "");
                }
            }
            TextInputId::ExtensionsFilter => {
                app.sidebar_view = crate::app::SidebarView::Extensions;
                app.active_pane = crate::app::ActivePane::Explorer;
            }
            TextInputId::ExplorerRename => app.rename.active = true,
            TextInputId::SourceControlCommit => {
                app.sidebar_view = crate::app::SidebarView::SourceControl;
                app.active_pane = crate::app::ActivePane::Explorer;
                app.source_control_input_mode = true;
            }
        }
    }

    #[test]
    fn every_field_routes_typing_paste_selection_and_undo_without_editing_the_file() {
        use crossterm::event::KeyEvent;
        for &id in FIELDS {
            let mut app = App::test_new();
            let mut editor = crate::editor::Editor::test_with_text("file");
            editor.insert_char('x');
            let file_text = editor.rope.to_string();
            app.tabs.push(crate::app::Tab::new_file(editor));
            app.active_tab = 0;
            activate_field(&mut app, id);
            *app.text_input_state_mut(id).0 = String::new();
            let key = |app: &mut App, code, modifiers| {
                crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
            };
            for c in "123".chars() {
                key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
            }
            assert_eq!(app.text_input_state_mut(id).0, "123", "{id:?}");
            assert!(app.paste_into_focused_text_input("45"));
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert_eq!(app.text_input_state_mut(id).0, "123", "paste undo: {id:?}");
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert_eq!(app.text_input_state_mut(id).0, "", "typing undo: {id:?}");
            key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
            assert_eq!(app.text_input_state_mut(id).0, "123", "redo: {id:?}");
            key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            key(&mut app, KeyCode::Char('6'), KeyModifiers::NONE);
            assert_eq!(app.text_input_state_mut(id).0, "6", "replace: {id:?}");
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
            let (text, cursor, anchor) = app.text_input_state_mut(id);
            assert_eq!(text, "123", "selection undo: {id:?}");
            assert_eq!((*cursor, *anchor), (3, Some(0)), "{id:?}");
            key(
                &mut app,
                KeyCode::Char('z'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(app.text_input_state_mut(id).0, "6", "shift redo: {id:?}");
            assert_eq!(app.tabs[0].editor.rope.to_string(), file_text, "{id:?}");
        }
    }

    #[test]
    fn every_field_supports_word_drag_line_selection_and_shift_click() {
        for &id in FIELDS {
            let mut app = App::test_new();
            activate_field(&mut app, id);
            let value = "one two three";
            *app.text_input_state_mut(id).0 = value.into();
            app.register_text_input(
                id,
                TextInputHitMap::new(
                    Rect::new(2, 2, 20, 1),
                    (0..value.len()).collect(),
                    value.len(),
                ),
            );
            let click = |app: &mut App, column, modifiers| {
                assert!(app.handle_text_input_mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    column,
                    2,
                    modifiers
                ));
                assert!(app.handle_text_input_mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    column,
                    2,
                    modifiers
                ));
            };
            click(&mut app, 7, KeyModifiers::NONE);
            // Second click selects the complete word; dragging extends by words.
            assert!(app.handle_text_input_mouse(
                MouseEventKind::Down(MouseButton::Left),
                7,
                2,
                KeyModifiers::NONE
            ));
            let (_, cursor, anchor) = app.text_input_state_mut(id);
            assert_eq!(
                App::inline_selection_range(*cursor, *anchor),
                Some((4, 7)),
                "{id:?}"
            );
            assert!(app.handle_text_input_mouse(
                MouseEventKind::Drag(MouseButton::Left),
                12,
                2,
                KeyModifiers::NONE
            ));
            let (_, cursor, anchor) = app.text_input_state_mut(id);
            assert_eq!(
                App::inline_selection_range(*cursor, *anchor),
                Some((4, 13)),
                "{id:?}"
            );
            app.handle_text_input_mouse(
                MouseEventKind::Up(MouseButton::Left),
                12,
                2,
                KeyModifiers::NONE,
            );
            click(&mut app, 7, KeyModifiers::NONE);
            let (_, cursor, anchor) = app.text_input_state_mut(id);
            assert_eq!(
                App::inline_selection_range(*cursor, *anchor),
                Some((0, 13)),
                "triple click: {id:?}"
            );
            click(&mut app, 3, KeyModifiers::NONE);
            click(&mut app, 6, KeyModifiers::SHIFT);
            let (_, cursor, anchor) = app.text_input_state_mut(id);
            assert_eq!(
                App::inline_selection_range(*cursor, *anchor),
                Some((1, 4)),
                "shift click: {id:?}"
            );
        }
    }

    #[test]
    fn every_rendered_field_supports_double_and_triple_click_through_workbench_routing() {
        use crossterm::event::MouseEvent;
        use ratatui::{backend::TestBackend, Frame, Terminal};

        fn render(frame: &mut Frame, app: &mut App, id: TextInputId) {
            app.text_input_regions.clear();
            let area = frame.area();
            match id {
                TextInputId::Chat => crate::ui_chat::draw(frame, app, area),
                TextInputId::FolderAddress | TextInputId::FolderSearch | TextInputId::FolderName => {
                    crate::ui_folder_browser::draw(frame, app)
                }
                TextInputId::SearchFind | TextInputId::SearchReplace => {
                    crate::ui_search_dialog::draw_search_bar(frame, app)
                }
                TextInputId::GlobalSearch(_) => {
                    crate::ui_search_panel::draw_search_panel(frame, app, area)
                }
                TextInputId::QuickOpen => crate::ui_search_dialog::draw_quick_open(frame, app),
                TextInputId::GotoLine => crate::ui_search_dialog::draw_goto_line(frame, app),
                TextInputId::GitBranch => crate::ui_pickers::draw_git_branch_picker(frame, app),
                TextInputId::CommandPalette => crate::ui_pickers::draw_command_palette(frame, app),
                TextInputId::SymbolList => crate::ui_pickers::draw_symbol_list(frame, app),
                TextInputId::TaskPicker => crate::ui_pickers::draw_task_picker(frame, app),
                TextInputId::SaveAs => crate::ui_dialogs::draw_save_as_dialog(frame, app),
                TextInputId::RenameSymbol => {
                    crate::ui_dialogs::draw_rename_symbol_dialog(frame, app)
                }
                TextInputId::TerminalRename => crate::ui_modal::draw(frame, app, crate::ui_modal::Kind::RenameTerminal),
                TextInputId::SettingsSearch | TextInputId::SettingsValue => {
                    crate::ui_settings::draw_settings_editor(frame, app, area)
                }
                TextInputId::AgentSetting(_) => crate::ui_agent_settings::draw(frame, app),
                TextInputId::LspSetting(_) => crate::ui_lsp_settings::draw(frame, app),
                TextInputId::FormatterSetting(_) => crate::ui_formatter_settings::draw(frame, app),
                TextInputId::ScopeBubbleLanguage => crate::ui_scope_bubble_settings::draw(frame, app),
                TextInputId::Exclusion(_) => crate::ui_exclusion_settings::draw(frame, app),
                TextInputId::FileAssociation(_) => crate::ui_file_associations::draw(frame, app),
                TextInputId::ExtensionsFilter => {
                    crate::ui_helpers::draw_extensions_sidebar(frame, app, area)
                }
                TextInputId::ExplorerRename => crate::ui_chrome::draw_explorer(frame, app, area),
                TextInputId::ExplorerFind => crate::ui_chrome::draw_explorer(frame, app, area),
                TextInputId::SourceControlCommit => {
                    crate::ui_source_control::draw_source_control_sidebar(frame, app, area)
                }
            }
        }

        for &id in FIELDS {
            let mut app = App::test_new();
            activate_field(&mut app, id);
            app.show_explorer = true;
            if matches!(id, TextInputId::GlobalSearch(_)) {
                app.sidebar_view = crate::app::SidebarView::Search;
            }
            if id == TextInputId::SettingsValue {
                app.settings_category_selected = 1;
                app.settings_selected = app
                    .settings_items()
                    .iter()
                    .position(|(_, _, id)| id == "tab_size")
                    .unwrap();
            }
            if id == TextInputId::ExplorerRename {
                let path = std::path::PathBuf::from("fixture.txt");
                app.rename.target = Some(path.clone());
                app.explorer.entries = vec![crate::explorer::FileEntry {
                    path,
                    name: "fixture.txt".into(),
                    is_dir: false,
                    depth: 0,
                    expanded: false,
                    git_status: crate::explorer::GitStatus::Unmodified,
                    git_status_summary: Default::default(),
                }];
                app.explorer.selected = 0;
                app.explorer.scroll_offset = 0;
            }
            app.source_control_repository_state =
                crate::app::SourceControlRepositoryState::Available;
            app.sc_graph_collapsed = true;
            let numeric = matches!(id, TextInputId::SettingsValue | TextInputId::GotoLine);
            let value = if numeric { "12345" } else { "alpha beta gamma" };
            let (text, cursor, anchor) = app.text_input_state_mut(id);
            *text = value.into();
            *cursor = text.len();
            *anchor = None;
            let mut terminal = Terminal::new(TestBackend::new(120, 45)).unwrap();
            terminal.draw(|frame| render(frame, &mut app, id)).unwrap();
            let hit_map = &app
                .text_input_regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("field was not rendered: {id:?}"))
                .hit_map;
            let target_byte = if numeric { 2 } else { 8 };
            let (column, row) = (hit_map.area.top()..hit_map.area.bottom())
                .find_map(|y| {
                    (hit_map.area.left()..hit_map.area.right())
                        .find(|&x| hit_map.byte_at(x, y) == target_byte)
                        .map(|x| (x, y))
                })
                .unwrap_or_else(|| panic!("text was not visible: {id:?}"));
            for click in 1..=3 {
                if click > 1 {
                    // This fixture tests consecutive-click routing and selection.
                    // Rendering or parallel test load must not expire the gesture.
                    app.last_click_time = std::time::Instant::now();
                }
                for kind in [
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Up(MouseButton::Left),
                ] {
                    crate::mouse::handle_mouse(
                        &mut app,
                        MouseEvent {
                            kind,
                            column,
                            row,
                            modifiers: KeyModifiers::NONE,
                        },
                    );
                }
                // Real frames rebuild the hit maps and may change focus styling.
                terminal.draw(|frame| render(frame, &mut app, id)).unwrap();
                if click >= 2 {
                    let (_, cursor, anchor) = app.text_input_state_mut(id);
                    let expected = if click == 3 || numeric {
                        (0, value.len())
                    } else {
                        (6, 10)
                    };
                    assert_eq!(
                        App::inline_selection_range(*cursor, *anchor),
                        Some(expected),
                        "click {click}: {id:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn numeric_fields_filter_paste_and_keep_history_separate_between_settings() {
        let mut app = App::test_new();
        activate_field(&mut app, TextInputId::SettingsValue);
        app.begin_numeric_setting_edit("tab_size", "4");
        assert!(app.paste_into_focused_text_input("a12\r\n3.é"));
        assert_eq!(app.settings_value_buffer, "123");
        app.edit_text_input(
            TextInputId::SettingsValue,
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
            None,
        );
        assert_eq!(app.settings_value_buffer, "4");
        assert_eq!(app.settings_value_selection_anchor, Some(0));
        assert!(app.paste_into_focused_text_input("abc\n"));
        assert_eq!(app.settings_value_buffer, "4");
        assert_eq!(app.settings_value_selection_anchor, Some(0));
        app.begin_numeric_setting_edit("scroll_speed", "4");
        app.edit_text_input(
            TextInputId::SettingsValue,
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
            None,
        );
        assert_eq!(app.settings_value_buffer, "4");
    }

    #[test]
    fn field_history_is_isolated_and_external_replacements_invalidate_it() {
        let mut app = App::test_new();
        for (id, text) in [
            (TextInputId::SearchFind, "find"),
            (TextInputId::SearchReplace, "replace"),
        ] {
            app.edit_text_input(id, KeyCode::Null, KeyModifiers::NONE, Some(text));
        }
        app.edit_text_input(
            TextInputId::SearchFind,
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
            None,
        );
        assert_eq!(app.search_query, "");
        assert_eq!(app.replace_query, "replace");
        app.replace_query = "new session".into();
        app.edit_text_input(
            TextInputId::SearchReplace,
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
            None,
        );
        assert_eq!(app.replace_query, "new session");
    }

    #[test]
    fn unicode_movement_deletion_and_word_selection_keep_graphemes_whole() {
        let mut app = App::test_new();
        let id = TextInputId::SearchReplace;
        let word = "cafe\u{301}";
        let text = format!("{word}👩‍💻");
        app.edit_text_input(id, KeyCode::Null, KeyModifiers::NONE, Some(&text));
        app.edit_text_input(id, KeyCode::Left, KeyModifiers::NONE, None);
        assert_eq!(app.replace_cursor, word.len());
        app.edit_text_input(id, KeyCode::Backspace, KeyModifiers::NONE, None);
        assert_eq!(app.replace_query, "caf👩‍💻");
        app.edit_text_input(id, KeyCode::Char('z'), KeyModifiers::CONTROL, None);
        assert_eq!(app.replace_query, text);
        assert_eq!(App::text_selection_unit(&text, 4, false), (0, word.len()));
        app.edit_text_input(id, KeyCode::Delete, KeyModifiers::NONE, None);
        assert_eq!(app.replace_query, word);
    }

    #[test]
    fn reopening_a_dialog_with_identical_text_does_not_restore_old_undo() {
        let mut app = App::test_new();
        app.save_as_active = true;
        app.edit_text_input(
            TextInputId::SaveAs,
            KeyCode::Null,
            KeyModifiers::NONE,
            Some("name.rs"),
        );
        app.save_as_active = false;
        app.sync_text_input_sessions();
        app.save_as_active = true;
        app.edit_text_input(
            TextInputId::SaveAs,
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
            None,
        );
        assert_eq!(app.save_as_query, "name.rs");
    }

    #[test]
    fn blocked_input_and_empty_undo_do_not_report_changes() {
        let mut app = App::test_new();
        let id = TextInputId::SettingsValue;
        for (code, modifiers, paste) in [
            (KeyCode::Backspace, KeyModifiers::NONE, None),
            (KeyCode::Delete, KeyModifiers::NONE, None),
            (KeyCode::Char('a'), KeyModifiers::NONE, None),
            (KeyCode::Char('z'), KeyModifiers::CONTROL, None),
            (KeyCode::Null, KeyModifiers::NONE, Some("not a number\n")),
        ] {
            assert!(!app.edit_text_input(id, code, modifiers, paste).changed);
        }
    }

    #[test]
    fn dragging_past_a_clipped_field_advances_the_caret_to_reveal_more_text() {
        let mut app = App::test_new();
        app.search_query = "abcdefghijklmnop".into();
        app.register_text_input(
            TextInputId::SearchFind,
            TextInputHitMap::new(
                Rect::new(2, 2, 6, 1),
                (0..6).collect(),
                app.search_query.len(),
            ),
        );
        app.handle_text_input_mouse(
            MouseEventKind::Down(MouseButton::Left),
            3,
            2,
            KeyModifiers::NONE,
        );
        app.handle_text_input_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            7,
            2,
            KeyModifiers::NONE,
        );
        app.handle_text_input_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            10,
            2,
            KeyModifiers::NONE,
        );
        assert_eq!(app.search_cursor, 6);
        app.handle_text_input_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            10,
            2,
            KeyModifiers::NONE,
        );
        assert_eq!(app.search_cursor, 7);
        assert_eq!(app.search_selection_anchor, Some(1));
    }

    fn edit(
        text: &mut String,
        cursor: &mut usize,
        anchor: &mut Option<usize>,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> TextInputResult {
        TextInput::new(text, cursor, anchor).handle_key(code, modifiers)
    }

    #[test]
    fn shift_arrows_create_and_shrink_selection() {
        let mut text = "aéz".to_owned();
        let mut cursor = text.len();
        let mut anchor = None;
        edit(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Left,
            KeyModifiers::SHIFT,
        );
        assert_eq!(selection_char_range(&text, cursor, anchor), Some((2, 3)));
        edit(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Right,
            KeyModifiers::SHIFT,
        );
        assert_eq!(anchor, None);
    }

    #[test]
    fn typing_replaces_selection() {
        let mut text = "hello".to_owned();
        let mut cursor = 4;
        let mut anchor = Some(1);
        let result = edit(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Char('i'),
            KeyModifiers::NONE,
        );
        assert_eq!(result, TextInputResult::CHANGED);
        assert_eq!(text, "hio");
        assert_eq!(cursor, 2);
        assert_eq!(anchor, None);
    }

    #[test]
    fn plain_arrow_collapses_selection() {
        let mut text = "hello".to_owned();
        let mut cursor = 4;
        let mut anchor = Some(1);
        edit(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Left,
            KeyModifiers::NONE,
        );
        assert_eq!(cursor, 1);
        assert_eq!(anchor, None);
    }

    #[test]
    fn filtered_input_and_paste_use_the_same_rules() {
        fn number_location(c: char) -> bool {
            c.is_ascii_digit() || c == ':'
        }
        let mut text = String::new();
        let mut cursor = 0;
        let mut anchor = None;
        let mut input = TextInput::new(&mut text, &mut cursor, &mut anchor).filter(number_location);
        input.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        input.insert_text("12:x34\n");
        assert_eq!(text, "12:34");
    }

    #[test]
    fn multiline_vertical_motion_clamps_to_each_line() {
        let mut text = "abcd\nx\nwxyz".to_owned();
        let mut cursor = 3;
        let mut anchor = None;
        TextInput::new(&mut text, &mut cursor, &mut anchor)
            .multiline(true)
            .handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(cursor, 6);
        TextInput::new(&mut text, &mut cursor, &mut anchor)
            .multiline(true)
            .handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(cursor, 8);
    }

    #[test]
    fn ctrl_backspace_respects_punctuation_boundaries() {
        let mut text = "foo.bar".to_owned();
        let mut cursor = text.len();
        let mut anchor = None;
        edit(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Backspace,
            KeyModifiers::CONTROL,
        );
        assert_eq!(text, "foo.");
        edit(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Backspace,
            KeyModifiers::CONTROL,
        );
        assert_eq!(text, "foo");
    }

    #[test]
    fn bracketed_paste_routes_to_active_quick_open_field() {
        let mut app = App::test_new();
        app.quick_open_active = true;
        assert!(app.paste_into_focused_text_input("src/main.rs"));
        assert_eq!(app.quick_open_query, "src/main.rs");
        assert_eq!(app.quick_open_cursor, app.quick_open_query.len());
    }

    #[test]
    fn visible_search_sidebar_does_not_steal_editor_paste() {
        let mut app = App::test_new();
        app.global_search_active = true;
        app.show_explorer = true;
        app.sidebar_view = crate::app::SidebarView::Search;
        app.active_pane = crate::app::ActivePane::Editor;
        assert!(!app.paste_into_focused_text_input("editor text"));
        assert!(app.global_search_query.is_empty());
    }

    #[test]
    fn visible_unfocused_find_does_not_steal_editor_paste() {
        let mut app = App::test_new();
        app.search_active = true;
        app.search_input_focused = false;
        app.search_query = "needle".into();

        assert!(!app.paste_into_focused_text_input("editor text"));
        assert_eq!(app.search_query, "needle");
    }

    #[test]
    fn mouse_click_and_drag_use_registered_byte_positions() {
        let mut app = App::test_new();
        app.search_active = true;
        app.search_input_focused = false;
        app.search_query = "hello".into();
        app.search_cursor = app.search_query.len();
        app.search_selection_anchor = Some(0);
        app.register_text_input(
            TextInputId::SearchFind,
            TextInputHitMap::new(Rect::new(10, 4, 8, 1), vec![0, 0, 0, 1, 2, 3, 4, 5], 5),
        );

        assert!(app.handle_text_input_mouse(
            MouseEventKind::Down(MouseButton::Left),
            13,
            4,
            KeyModifiers::NONE,
        ));
        assert!(app.search_input_focused);
        assert_eq!(app.search_cursor, 1);
        assert_eq!(app.search_selection_anchor, None);

        assert!(app.handle_text_input_mouse(
            MouseEventKind::Drag(MouseButton::Left),
            16,
            4,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.search_cursor, 4);
        assert_eq!(app.search_selection_anchor, Some(1));
        assert!(app.handle_text_input_mouse(
            MouseEventKind::Up(MouseButton::Left),
            16,
            4,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.text_input_drag, None);
    }

    #[test]
    fn settings_search_click_uses_the_same_focus_and_caret_path() {
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.settings_focus_categories = true;
        app.settings_editor_search = "editor font".into();
        app.settings_editor_search_cursor = app.settings_editor_search.len();
        app.active_pane = crate::app::ActivePane::Explorer;
        app.register_text_input(
            TextInputId::SettingsSearch,
            TextInputHitMap::new(
                Rect::new(2, 2, 6, 1),
                vec![0, 0, 1, 2, 3, 4],
                app.settings_editor_search.len(),
            ),
        );

        assert!(app.handle_text_input_mouse(
            MouseEventKind::Down(MouseButton::Left),
            5,
            2,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.settings_editor_search_cursor, 2);
        assert!(!app.settings_focus_categories);
        assert_eq!(app.active_pane, crate::app::ActivePane::Editor);
    }
}
