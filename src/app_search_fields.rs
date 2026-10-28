use crate::app::App;

impl App {
    pub(crate) fn file_search_visible(&self) -> bool {
        self.search_active
            && !self.tabs
                .get(self.active_tab)
                .is_some_and(|tab| tab.kind == crate::app::TabKind::Terminal)
    }

    pub(crate) fn restore_search_histories(&mut self, session: &crate::session::Session) {
        self.search_history = crate::search_history::History::restore(&session.search_history);
        self.global_search_history =
            crate::search_history::History::restore(&session.global_search_history);
        // Older sessions only retained the current workspace query.
        if session.global_search_history.is_empty() {
            self.global_search_history
                .record(&session.global_search_query);
        }
        for (field, entries, current) in [
            (1, &session.global_search_replace_history, &session.global_search_replace_query),
            (2, &session.search_include_history, &session.search_include_pattern),
            (3, &session.search_exclude_history, &session.search_exclude_pattern),
        ] {
            let history = self.global_search_field_history_mut(field);
            *history = crate::search_history::History::restore(entries);
            if entries.is_empty() { history.record(current); }
        }
        self.search_query.clear();
        self.search_cursor = 0;
        self.search_selection_anchor = None;
    }

    pub(crate) fn push_global_search_history(&mut self) {
        for field in 0..4 {
            let value = self.global_search_field_state_mut(field).0.clone();
            self.global_search_field_history_mut(field).record(&value);
        }
    }

    pub(crate) fn global_search_field_history_mut(&mut self, field: u8) -> &mut crate::search_history::History {
        match field.min(3) {
            0 => &mut self.global_search_history,
            1 => &mut self.global_search_replace_history,
            2 => &mut self.search_include_history,
            _ => &mut self.search_exclude_history,
        }
    }

    pub(crate) fn global_search_history_prev(&mut self) {
        let field = self.search_focus_field.min(3);
        let current = self.global_search_field_state_mut(field).0.clone();
        if let Some(value) = self.global_search_field_history_mut(field).previous(&current) {
            self.apply_global_search_history_field(field, value);
        }
    }

    pub(crate) fn global_search_history_next(&mut self) {
        let field = self.search_focus_field.min(3);
        if let Some(value) = self.global_search_field_history_mut(field).next() {
            self.apply_global_search_history_field(field, value);
        }
    }

    fn apply_global_search_history_field(&mut self, field: u8, value: String) {
        let (text, cursor, anchor) = self.global_search_field_state_mut(field);
        *text = value;
        *cursor = text.len();
        *anchor = None;
        if field != 1 { self.schedule_global_search(); }
    }

    pub(crate) fn global_search_field_state_mut(
        &mut self,
        field: u8,
    ) -> (&mut String, &mut usize, &mut Option<usize>) {
        match field.min(3) {
            0 => (
                &mut self.global_search_query,
                &mut self.global_search_field_cursors[0],
                &mut self.global_search_field_selection_anchors[0],
            ),
            1 => (
                &mut self.global_search_replace_query,
                &mut self.global_search_field_cursors[1],
                &mut self.global_search_field_selection_anchors[1],
            ),
            2 => (
                &mut self.search_include_pattern,
                &mut self.global_search_field_cursors[2],
                &mut self.global_search_field_selection_anchors[2],
            ),
            _ => (
                &mut self.search_exclude_pattern,
                &mut self.global_search_field_cursors[3],
                &mut self.global_search_field_selection_anchors[3],
            ),
        }
    }

    pub(crate) fn global_search_set_field_cursor_char_index(
        &mut self,
        field: u8,
        target_char: usize,
    ) {
        let (text, cursor, selection_anchor) = self.global_search_field_state_mut(field);
        let mut byte_idx = text.len();
        for (i, (byte_idx_candidate, _)) in text.char_indices().enumerate() {
            if i == target_char {
                byte_idx = byte_idx_candidate;
                break;
            }
        }
        *cursor = byte_idx;
        *selection_anchor = None;
        Self::normalize_inline_cursor(text, cursor, selection_anchor);
    }
}
