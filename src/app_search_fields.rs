use crate::app::App;

impl App {
    pub(crate) fn restore_search_histories(&mut self, session: &crate::session::Session) {
        self.search_history = crate::search_history::History::restore(&session.search_history);
        self.global_search_history =
            crate::search_history::History::restore(&session.global_search_history);
        // Older sessions only retained the current workspace query.
        if session.global_search_history.is_empty() {
            self.global_search_history
                .record(&session.global_search_query);
        }
        self.search_query.clear();
        self.search_cursor = 0;
        self.search_selection_anchor = None;
    }

    pub(crate) fn push_global_search_history(&mut self) {
        self.global_search_history.record(&self.global_search_query);
    }

    pub(crate) fn global_search_history_prev(&mut self) {
        if let Some(query) = self
            .global_search_history
            .previous(&self.global_search_query)
        {
            self.apply_global_search_history_query(query);
        }
    }

    pub(crate) fn global_search_history_next(&mut self) {
        if let Some(query) = self.global_search_history.next() {
            self.apply_global_search_history_query(query);
        }
    }

    fn apply_global_search_history_query(&mut self, query: String) {
        self.global_search_query = query;
        self.global_search_field_cursors[0] = self.global_search_query.len();
        self.global_search_field_selection_anchors[0] = None;
        self.update_global_search();
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
