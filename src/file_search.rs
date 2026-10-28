//! Find/replace state is owned by an editor tab, including inactive split panes.
use crate::app::App;

#[cfg(test)]
#[path = "file_search_tests.rs"]
mod tests;

#[derive(Default)]
pub struct State {
    pub(crate) viewport: Option<ratatui::layout::Rect>,
    pub search_active: bool,
    pub search_pre_cursor: Option<(usize, usize, usize)>, // (line, col, scroll_offset) before search
    pub search_query: String,
    pub search_cursor: usize,
    pub search_selection_anchor: Option<usize>,
    // Character offsets from the starting line; the end may extend to later lines.
    pub search_matches: Vec<(usize, usize, usize)>,
    pub search_error: Option<String>,
    pub search_truncated: bool,
    pub search_current: usize,
    /// The bar can remain open while keyboard focus returns to the editor.
    pub search_input_focused: bool,
    pub replace_mode: bool,
    pub replace_query: String,
    pub replace_cursor: usize,
    pub replace_selection_anchor: Option<usize>,
    pub search_focus_replace: bool, // true when replace input is focused
    pub search_in_selection: bool,
    pub search_selection_range: Option<(usize, usize, usize, usize)>, // (start_line, start_col, end_line, end_col)
    pub search_bar_area: Option<ratatui::layout::Rect>,
    pub(crate) search_bar_layout: crate::ui_search_dialog::SearchBarLayout,
    pub(crate) bg_file_search_running: bool,
    pub(crate) file_search_generation: u64,
    pub(crate) file_search_cancel_token: Option<crate::background::CancellationToken>,
    pub(crate) file_search_matches_target: Option<crate::background::EditorAnalysisTarget>,
    pub(crate) file_search_key: Option<crate::app_search_algo::FileSearchKey>,
    pub(crate) file_search_refresh_pending: bool,
    pub(crate) input_histories: [crate::text_input::TextInputHistory; 2],
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = &self.file_search_cancel_token {
            cancel.cancel();
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Bar {
    pub owner: Option<u64>,
    pub area: ratatui::layout::Rect,
}

impl App {
    pub(crate) fn apply_tab_search_matches(
        &mut self,
        owner: u64,
        generation: u64,
        target: crate::background::EditorAnalysisTarget,
        matches: Vec<(usize, usize, usize)>,
        truncated: bool,
        error: Option<String>,
    ) -> bool {
        if self.file_search_owner() == Some(owner) {
            if generation != self.file_search_generation {
                return false;
            }
            self.bg_file_search_running = false;
            self.file_search_cancel_token = None;
            if self.editor_content_target().as_ref() != Some(&target) {
                self.file_search_key = None;
                self.file_search_refresh_pending = self.search_active;
                return true;
            }
            self.apply_file_search_matches(target, matches, truncated, error);
            return true;
        }
        let Some(tab) = self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .find(|tab| tab.editor.navigation_id == owner)
        else {
            return false;
        };
        let state = &mut tab.file_search;
        if state.file_search_generation != generation {
            return false;
        }
        state.bg_file_search_running = false;
        state.file_search_cancel_token = None;
        let editor = &tab.editor;
        if editor.content_version != target.content_version
            || editor.rope.len_bytes() != target.len_bytes
            || editor.file_path != target.file_path
            || editor.modified != target.modified
            || editor.last_disk_modified != target.disk_modified
        {
            state.file_search_key = None;
            state.file_search_refresh_pending = state.search_active;
            return true;
        }
        let previous = state.search_matches.get(state.search_current).copied();
        state.search_current = previous
            .and_then(|hit| matches.binary_search(&hit).ok())
            .unwrap_or_else(|| {
                matches.partition_point(|&(line, col, _)| {
                    (line, col) < (editor.cursor_line, editor.cursor_col)
                })
            })
            .min(matches.len().saturating_sub(1));
        state.search_matches = matches;
        state.search_truncated = truncated;
        state.search_error = error.as_deref().map(crate::search_regex::error_summary);
        state.file_search_matches_target = Some(target);
        true
    }

    pub(crate) fn file_search_owner(&self) -> Option<u64> {
        self.tabs
            .get(self.active_tab)
            .map(|tab| tab.editor.navigation_id)
    }

    pub(crate) fn focus_file_search_owner(&mut self, owner: Option<u64>) {
        let Some(owner) = owner else {
            return;
        };
        let location = self
            .tabs
            .iter()
            .position(|tab| tab.editor.navigation_id == owner)
            .map(|tab| (self.active_group, tab))
            .or_else(|| {
                self.groups.iter().enumerate().find_map(|(group, state)| {
                    state
                        .tabs
                        .iter()
                        .position(|tab| tab.editor.navigation_id == owner)
                        .map(|tab| (group, tab))
                })
            });
        if let Some((group, tab)) = location {
            self.switch_group(group);
            self.active_tab = tab;
            self.active_pane = crate::app::ActivePane::Editor;
            self.terminal_panel_focused = false;
        }
    }

    pub(crate) fn file_search_bar_at(&self, col: u16, row: u16) -> Option<Bar> {
        self.file_search_bars
            .iter()
            .rev()
            .copied()
            .find(|bar| bar.area.contains((col, row).into()))
    }
}

pub(crate) fn begin_frame(app: &mut App) {
    app.file_search_bars.clear();
    for tab in app
        .tabs
        .iter_mut()
        .chain(app.groups.iter_mut().flat_map(|group| &mut group.tabs))
    {
        tab.file_search.viewport = None;
        tab.file_search.search_bar_area = None;
    }
}

impl std::ops::Deref for App {
    type Target = State;
    fn deref(&self) -> &State {
        self.tabs
            .get(self.active_tab)
            .map_or(&self.empty_file_search, |tab| &tab.file_search)
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut State {
        self.tabs
            .get_mut(self.active_tab)
            .map_or(&mut self.empty_file_search, |tab| &mut tab.file_search)
    }
}
