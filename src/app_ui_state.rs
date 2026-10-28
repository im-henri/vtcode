use std::path::Path;

use crate::app::{compute_file_diff, ActivePane, App, DiffMode, NotifyLevel, SidebarView};

pub(crate) const BINARY_DIFF_MESSAGE: &str = "Diff is unavailable for binary files.";

#[cfg(test)]
#[path = "palette_theme_tests.rs"]
mod palette_theme_tests;

#[cfg(test)]
#[path = "syntax_reload_tests.rs"]
mod syntax_reload_tests;

#[cfg(test)]
#[path = "app_workbench_search_performance_tests.rs"]
mod workbench_search_performance_tests;

enum CompareBaseSnapshot {
    Text(std::sync::Arc<str>),
    Rope(ropey::Rope),
    Disk(std::path::PathBuf),
}

type ProblemItem = crate::background::ProblemItem;
type ProblemAnalysis = (Vec<ProblemItem>, usize, usize, usize, usize);

fn analyze_problem_snapshots(files: Vec<(String, ropey::Rope, u64)>) -> ProblemAnalysis {
    let markers = ["TODO", "FIXME", "HACK", "BUG", "NOTE"];
    let mut items = Vec::with_capacity(64);
    let mut line_buf = String::with_capacity(128);

    'files: for (file_name, rope, navigation_id) in files {
        for (line_index, line) in rope.lines().enumerate() {
            if items.len() >= 1_000 {
                break 'files;
            }
            let text = if let Some(text) = line.as_str() {
                text
            } else {
                line_buf.clear();
                for chunk in line.chunks() {
                    line_buf.push_str(chunk);
                }
                &line_buf
            };
            let marker = markers.iter().find(|marker| {
                text.find(*marker).is_some_and(|position| {
                    let end = position + marker.len();
                    let bytes = text.as_bytes();
                    let before_ok = position == 0 || !bytes[position - 1].is_ascii_alphanumeric();
                    let after_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
                    before_ok && after_ok
                })
            });
            if let Some(marker) = marker {
                items.push((
                    file_name.clone(),
                    line_index + 1,
                    (*marker).to_string(),
                    text.trim().to_string(),
                    navigation_id,
                ));
            }
        }
    }

    let severity = |marker: &str| match marker {
        "FIXME" | "BUG" => 0,
        "TODO" | "HACK" => 1,
        _ => 2,
    };
    items.sort_unstable_by(|left, right| {
        severity(&left.2)
            .cmp(&severity(&right.2))
            .then(left.0.cmp(&right.0))
            .then(left.1.cmp(&right.1))
            .then(left.4.cmp(&right.4))
    });

    let mut errors = 0;
    let mut warnings = 0;
    let mut info = 0;
    let mut files_seen = rustc_hash::FxHashSet::default();
    for (_, _, marker, _, navigation_id) in &items {
        match marker.as_str() {
            "FIXME" | "BUG" => errors += 1,
            "TODO" | "HACK" => warnings += 1,
            "NOTE" => info += 1,
            _ => {}
        }
        files_seen.insert(*navigation_id);
    }
    let file_count = files_seen.len();
    drop(files_seen);
    (items, errors, warnings, info, file_count)
}

impl App {
    pub(crate) fn problems_analysis_target(&self) -> crate::background::ProblemsAnalysisTarget {
        crate::background::ProblemsAnalysisTarget {
            active_group: self.active_group,
            files: self
                .tabs
                .iter()
                .filter(|tab| !tab.editor.large_file_mode && !tab.editor.bg_loading)
                .map(|tab| crate::background::ProblemsFileTarget {
                    navigation_id: tab.editor.navigation_id,
                    file_path: tab.editor.file_path.clone(),
                    file_name: tab.editor.file_name().to_string(),
                    content_version: tab.editor.content_version,
                    len_bytes: tab.editor.rope.len_bytes(),
                    line_count: tab.editor.rope.len_lines(),
                })
                .collect(),
        }
    }

    /// Validate a completed scan without rebuilding owned filenames on each draw.
    pub(crate) fn problems_analysis_is_current(
        &self,
        target: &crate::background::ProblemsAnalysisTarget,
    ) -> bool {
        if target.active_group != self.active_group {
            return false;
        }
        let mut files = target.files.iter();
        for editor in self.tabs.iter()
            .map(|tab| &tab.editor)
            .filter(|editor| !editor.large_file_mode && !editor.bg_loading)
        {
            let Some(file) = files.next() else {
                return false;
            };
            if file.navigation_id != editor.navigation_id
                || file.content_version != editor.content_version
                || file.len_bytes != editor.rope.len_bytes()
                || file.line_count != editor.rope.len_lines()
                || file.file_path.as_ref().map(|path| path.as_os_str())
                    != editor.file_path.as_ref().map(|path| path.as_os_str())
                || file.file_name != editor.file_name()
            {
                return false;
            }
        }
        files.next().is_none()
    }

    /// Queue an exact Problems-panel scan. Rope snapshots share their backing
    /// tree, so requesting analysis does no whole-document work on the UI.
    pub(crate) fn request_problems_analysis(&mut self) {
        if self.bg_problems_analysis_running
            || self.cached_problems_target.as_ref()
                .is_some_and(|target| self.problems_analysis_is_current(target))
        {
            return;
        }
        let target = self.problems_analysis_target();
        let files = self
            .tabs
            .iter()
            .filter(|tab| !tab.editor.large_file_mode && !tab.editor.bg_loading)
            .map(|tab| (tab.editor.file_name().to_string(), tab.editor.rope.clone(), tab.editor.navigation_id))
            .collect();
        let sender = self.background.sender.clone();
        let pool_sender = self.background.pool_sender();
        self.bg_problems_analysis_running = true;
        let queued = pool_sender.send(Box::new(move || {
            let (items, error_count, warning_count, info_count, file_count) =
                analyze_problem_snapshots(files);
            let _ = sender.send(crate::background::BackgroundResult::ProblemsAnalysis {
                target,
                items,
                error_count,
                warning_count,
                info_count,
                file_count,
            });
        }));
        if queued.is_err() {
            self.bg_problems_analysis_running = false;
        }
    }

    pub(crate) fn clear_drag_states(&mut self) {
        crate::ui_extension_info::cancel_drag(self);
        self.keybindings_ui.cancel_drag();
        self.context_menu_button_drag = None;
        self.search_bar_layout.resizing = false;
        self.commit_input.scrollbar_drag = None;
        self.commit_details.divider_drag = None;
        self.commit_details.scrollbar_drag = None;
        self.chat.layout.dragging = None;
        self.chat.scroll_drag = None;
        self.dragging_tab = None;
        self.dragging_tab_source_group = None;
        self.dragging_terminal_tab = None;
        self.dragging_explorer_path = None;
        self.explorer_drop_target_idx = None;
        self.dragging_text = false;
        self.dragging_text_start = None;
        self.editor_selection_drag = None;
        self.alt_cursor_mouse_down_active = false;
        self.tab_drop_zone = None;
        self.tab_drop_target_group = None;
        self.terminal_dock_drop_target = None;
        self.resizing_explorer = false;
        self.explorer_scrollbar_drag = None;
        self.resizing_panel = false;
        self.resizing_sc_graph = false;
        self.resizing_split = false;
        self.resizing_diff_split = false;
        self.settings_scrollbar_drag = None;
        self.dragging_settings_categories = false;
        self.dragging_divider = None;
        self.dragging_v_scrollbar = false;
        self.dragging_h_scrollbar = false;
        self.dragging_diff_left_h_scrollbar = false;
        self.dragging_minimap = false;
        self.minimap_drag_start = None;
    }

    pub(crate) fn clear_tab_tooltip(&mut self) {
        self.tab_tooltip_tab = None;
        self.tab_tooltip_hover_start = None;
        self.tab_tooltip_shown = false;
    }

    pub(crate) fn reset_hover_states(&mut self) {
        self.clear_tab_tooltip();
        self.chrome_hover = crate::ui_status::HoverState::default();
        self.hovered_tab = None;
        self.hovered_tab_close_btn = false;
        self.hovered_tab_scroll_left = false;
        self.hovered_tab_scroll_right = false;
        self.hovered_activity = None;
        self.hovered_menu = None;
        self.hovered_status_item = None;
        self.hovered_explorer = None;
        self.hovered_close_button = false;
        self.hovered_panel_close = false;
        self.hovered_panel_tab = None;
        self.hovered_terminal_plus = false;
        self.hovered_terminal_tab = None;
        self.hovered_terminal_close = None;
        self.hovered_terminal_sidebar_toggle = false;
        self.hovered_new_tab_button = false;
        self.hovered_scrollbar = false;
        self.hovered_minimap = false;
        self.hovered_h_scrollbar = false;
        self.hovered_search_toggle = None;
        self.hovered_gutter_line = None;
        self.hovered_editor_pos = None;
        self.outline_hovered = None;
    }

    pub(crate) fn sync_activity_bar_selection(&mut self) {
        self.activity_bar_selected = match self.sidebar_view {
            SidebarView::Explorer => 0,
            SidebarView::Search => 1,
            SidebarView::SourceControl => 2,
            SidebarView::Extensions => 4,
            SidebarView::Outline => 5,
            SidebarView::Settings => usize::MAX,
        };
    }

    /// Clear transient editor state that shouldn't persist across tab switches.
    pub(crate) fn clear_transient_editor_state(&mut self) {
        self.dismiss_vtcomplete();
        self.clear_tab_tooltip();
        self.clear_lsp_link_hover();
        self.dismiss_lsp_hover();
    }

    pub(crate) fn clear_lsp_hover(&mut self) -> bool {
        let visible = self.lsp_hover_active && self.lsp_manager.hover_content.is_some();
        self.lsp_hover_active = false;
        self.lsp_hover_pending_pos = None;
        self.lsp_hover_request_time = None;
        let id = self.lsp_manager.active_hover_id.take();
        self.lsp_manager.cancel_request_id(id);
        self.lsp_manager.hover_content = None;
        visible
    }

    pub(crate) fn dismiss_lsp_hover(&mut self) {
        self.clear_lsp_hover();
        self.lsp_hover_suppressed = true;
    }

    pub(crate) fn open_palette(&mut self) {
        if self.palette.active {
            self.close_palette();
            return;
        }
        self.clear_drag_states();
        self.refresh_palette_commands();
        self.palette.active = true;
        self.palette.install_theme_mode = false;
        self.palette.install_extension_mode = false;
        self.palette.install_syntax_mode = false;
        self.palette.query.clear();
        self.palette.cursor = 0;
        self.palette.selection_anchor = None;
        self.palette.selected = 0;
        self.palette.scroll_offset = 0;
        self.update_palette_filter();
    }

    pub(crate) fn close_palette(&mut self) {
        self.cancel_extension_version_flow();
        self.palette.active = false;
        self.palette.install_theme_mode = false;
        self.palette.install_extension_mode = false;
        self.palette.install_syntax_mode = false;
        self.restore_palette_theme_preview();
    }

    pub(crate) fn palette_selected_theme_key(&self) -> Option<String> {
        let &index = self.palette.filtered.get(self.palette.selected)?;
        if self.extension_palette_action_map.contains_key(&index) { return None; }
        let command = self.palette.commands.get(index)?;
        let key = crate::app_theme::config_key_from_command_id(&command.id)?;
        self.loaded_themes.iter().any(|theme| theme.config_key == key).then_some(key)
    }

    pub(crate) fn preview_palette_theme(&mut self) {
        let selected = if self.palette.active && !self.theme_install_prompt_visible() {
            self.palette_selected_theme_key()
        } else { None };
        if let Some(key) = selected {
            // Repeated mouse movement and text-cursor keys must not restart
            // syntax highlighting for an unchanged preview.
            if key != self.theme_key {
                self.palette.theme_preview_original.get_or_insert_with(|| self.theme_key.clone());
                self.set_theme_by_key(&key);
            }
        } else {
            self.restore_palette_theme_preview();
        }
    }

    pub(crate) fn restore_palette_theme_preview(&mut self) {
        if let Some(original) = self.palette.theme_preview_original.take() {
            if self.theme_key != original { self.set_theme_by_key(&original); }
        }
    }

    /// Settings/session snapshots must never commit a highlighted theme preview.
    pub(crate) fn committed_theme_key(&self) -> &str {
        self.palette.theme_preview_original.as_deref()
            .or_else(|| {
                if self.settings_dropdown_committed {
                    None
                } else {
                    self.settings_dropdown_theme_original.as_deref()
                }
            })
            .unwrap_or(&self.theme_key)
    }

    pub(crate) fn update_palette_filter(&mut self) {
        self.filter_palette_commands();
        self.preview_palette_theme();
    }

    pub(crate) fn filter_palette_commands(&mut self) {
        if self.theme_install_prompt_visible() { self.palette.filtered.clear(); return; }
        let query = &self.palette.query;
        if query.is_empty() {
            // Show recently used commands first, then the rest
            // Build reverse map: recent_key -> index for O(1) lookup
            let mut key_to_index: rustc_hash::FxHashMap<String, usize> =
                rustc_hash::FxHashMap::default();
            for i in 0..self.palette.commands.len() {
                key_to_index.insert(self.palette_recent_key_for_index(i), i);
            }
            let mut recent_indices: Vec<usize> = Vec::new();
            let mut recent_set = rustc_hash::FxHashSet::default();
            for recent_id in &self.palette.recent {
                if let Some(&pos) = key_to_index.get(recent_id) {
                    if recent_set.insert(pos) {
                        recent_indices.push(pos);
                    }
                }
            }
            let mut rest: Vec<usize> = (0..self.palette.commands.len())
                .filter(|i| !recent_set.contains(i))
                .collect();
            recent_indices.append(&mut rest);
            self.palette.filtered = recent_indices;
        } else {
            // Commands always ignore case. Normalize once so uppercase input
            // cannot enable smart case or change the result ranking.
            let query = query.to_ascii_lowercase();
            let mut scorer = crate::fuzzy_score::PreparedFuzzyScore::new(&query);
            let mut scored: Vec<(usize, i64)> = self
                .palette
                .commands
                .iter()
                .enumerate()
                .filter_map(|(i, cmd)| scorer.score(&cmd.name).map(|score| (i, score)))
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1));
            self.palette.filtered = scored.into_iter().map(|(i, _)| i).collect();
        }
        if self.palette.selected >= self.palette.filtered.len() {
            self.palette.selected = 0;
        }
    }

    pub(crate) fn palette_recent_key_for_index(&self, index: usize) -> String {
        if let Some(action) = self.extension_palette_action_map.get(&index) {
            return action.recent_key();
        }
        self.palette.commands[index].id.to_string()
    }

    /// Rebuild the cached keybinding settings entries (label/value/id).
    /// Call once after keybindings are loaded or modified.
    pub(crate) fn rebuild_keybinding_settings_cache(&mut self) {
        const GROUPS: [(&str, &str, &str); 10] = [
            ("File", "── File ──", "heading_kb_file"),
            ("Editor", "── Editor ──", "heading_kb_editor"),
            ("Search", "── Search ──", "heading_kb_search"),
            ("Navigation", "── Navigation ──", "heading_kb_navigation"),
            ("View", "── View ──", "heading_kb_view"),
            (
                "File Explorer",
                "── File Explorer ──",
                "heading_kb_file_explorer",
            ),
            (
                "Source Control",
                "── Source Control ──",
                "heading_kb_source_control",
            ),
            ("Extensions", "── Extensions ──", "heading_kb_extensions"),
            ("Terminal", "── Terminal ──", "heading_kb_terminal"),
            ("General", "── General ──", "heading_kb_general"),
        ];
        let binding_group = |id: &str| -> &'static str {
            match id {
                "save" | "new_file" | "close_tab" => "File",
                "find" | "find_replace" | "find_next" | "find_prev" | "global_search" => "Search",
                "go_to_line" | "go_to_definition" | "find_references" | "tab_switcher_next"
                | "tab_switcher_prev" | "next_tab" | "prev_tab" => "Navigation",
                "toggle_explorer" => "File Explorer",
                "toggle_panel" | "toggle_menu_bar" | "show_shortcuts" => "View",
                "toggle_terminal" => "Terminal",
                "source_control" => "Source Control",
                "extensions" => "Extensions",
                "palette" | "quick_open" => "General",
                _ => "Editor",
            }
        };
        let command_group = |name: &str, id: &str| -> &'static str {
            if name.starts_with("File:") {
                if name.contains("Open Folder") || name.contains("Open Parent") {
                    "File Explorer"
                } else {
                    "File"
                }
            } else if name.starts_with("Edit:")
                || name.starts_with("Selection:")
                || name.starts_with("Fold:")
                || name.starts_with("Transform:")
                || name.starts_with("Bookmarks:")
                || name.starts_with("Editor:")
            {
                "Editor"
            } else if name.starts_with("Search:") {
                "Search"
            } else if name.starts_with("View:") {
                if name.contains("Explorer") || name.contains("Sidebar") {
                    "File Explorer"
                } else {
                    "View"
                }
            } else if name.starts_with("Git:") {
                "Source Control"
            } else if name.starts_with("Terminal:") {
                "Terminal"
            } else if name.starts_with("Extensions:") {
                "Extensions"
            } else if matches!(
                id,
                "goto_line"
                    | "goto_symbol"
                    | "goto_workspace_symbol"
                    | "navigate_back"
                    | "navigate_forward"
                    | "go_to_definition"
                    | "find_references"
                    | "goto_last_edit"
                    | "next_tab"
                    | "prev_tab"
                    | "tab_switcher_next"
                    | "tab_switcher_prev"
            ) {
                "Navigation"
            } else if matches!(
                id,
                "toggle_explorer" | "toggle_sidebar" | "collapse_explorer"
            ) {
                "File Explorer"
            } else if id == "source_control" {
                "Source Control"
            } else if id == "extensions" {
                "Extensions"
            } else if matches!(
                id,
                "new_terminal"
                    | "new_terminal_in_editor_tab"
                    | "close_terminal"
                    | "toggle_terminal"
                    | "detach_terminal_to_tab"
                    | "move_terminal_to_right"
            ) {
                "Terminal"
            } else {
                "General"
            }
        };
        let sanitize = |id: &str| -> String {
            id.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect()
        };
        let mut grouped: std::collections::BTreeMap<&'static str, Vec<(String, String, String)>> =
            std::collections::BTreeMap::new();
        let mut configurable_ids: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();

        self.keybinding_settings_cache.clear();
        for (idx, kb) in self.keybindings.iter().enumerate() {
            configurable_ids.insert(kb.id.to_string());
            let group = binding_group(kb.id);
            let kb_id = format!("kb_{idx}");
            let label = format!("Shortcut: {}", kb.label);
            let value = kb.display();
            grouped
                .entry(group)
                .or_default()
                .push((label, value, kb_id));
        }

        let mut seen_readonly: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
        let syntax_names = self.highlighter.syntax_names();
        let commands = Self::build_command_list_with_themes(&self.loaded_themes, &syntax_names);
        for (idx, command) in commands.iter().enumerate() {
            let shortcut = command.shortcut.trim();
            if shortcut.is_empty() {
                continue;
            }
            let cmd_id = command.id.as_ref();
            if configurable_ids.contains(cmd_id) {
                continue;
            }
            let dedupe_key = format!("{cmd_id}|{shortcut}");
            if !seen_readonly.insert(dedupe_key) {
                continue;
            }
            let group = command_group(&command.name, cmd_id);
            let setting_id = format!("shortcut_ro_{}_{}", sanitize(cmd_id), idx);
            let label = format!("Shortcut: {} ({shortcut})", command.name);
            grouped
                .entry(group)
                .or_default()
                .push((label, "Read-only".to_string(), setting_id));
        }

        for (group, heading_label, heading_id) in GROUPS {
            let Some(entries) = grouped.remove(group) else {
                continue;
            };
            if entries.is_empty() {
                continue;
            }
            self.keybinding_settings_cache.push((
                heading_label.to_string(),
                String::new(),
                heading_id.to_string(),
            ));
            self.keybinding_settings_cache.extend(entries);
        }
        self.refresh_shortcut_hints();
    }

    pub(crate) fn toggle_explorer(&mut self) {
        self.show_explorer = !self.show_explorer;
        if !self.show_explorer && self.active_pane == ActivePane::Explorer {
            self.active_pane = ActivePane::Editor;
        }
    }

    // ── Global search ──────────────────────────────────────────────────

    pub(crate) fn focus_global_search(&mut self) {
        self.show_explorer = true;
        self.sidebar_view = SidebarView::Search;
        self.activity_bar_selected = 1;
        self.active_pane = ActivePane::Explorer;
        self.global_search_active = true;
        self.search_focus_field = 0;
        self.search_input_focused = false;
        self.terminal_panel_focused = false;
    }

    pub(crate) fn open_global_search_result(&mut self) -> bool {
        self.push_global_search_history();
        if !self.is_global_search_result_visible(self.global_search_selected) {
            return false;
        }
        if let Some(result) = self
            .global_search_results
            .get(self.global_search_selected)
            .cloned()
        {
            self.push_nav_history();
            self.open_file(&result.file_path);
            if let Some(ed) = self.editor_mut() {
                let position = (result.line_num.saturating_sub(1), result.col_start);
                ed.clear_selection();
                ed.set_cursor(position.0, position.1);
            }
            self.finish_navigation();
            return true;
        }
        false
    }

    pub(crate) fn is_global_search_file_collapsed(&self, path: &Path) -> bool {
        self.global_search_results.is_collapsed(path)
    }

    fn apply_global_search_collapsed_visibility(&self) -> bool {
        self.show_explorer && self.sidebar_view == SidebarView::Search
    }

    fn is_global_search_result_visible(&self, idx: usize) -> bool {
        self.global_search_results.get(idx).is_some_and(|result| {
            !self.apply_global_search_collapsed_visibility()
                || !self.is_global_search_file_collapsed(&result.file_path)
        })
    }

    fn first_visible_global_search_result_index(&self) -> Option<usize> {
        self.global_search_results.first_visible_from(0)
    }

    fn global_search_visual_metrics(&self) -> (Option<usize>, usize) {
        (self.global_search_results.result_row(self.global_search_selected),
            self.global_search_results.row_count())
    }

    /// Reveal the keyboard-selected result without coupling ordinary mouse
    /// wheel scrolling to selection movement.
    pub(crate) fn reveal_global_search_selection(&mut self) {
        if !self.show_explorer || self.sidebar_view != SidebarView::Search {
            return;
        }
        let (Some(selected_row), total_rows) = self.global_search_visual_metrics() else {
            self.global_search_scroll = 0;
            return;
        };
        let visible_rows = self
            .explorer_area
            .map(|area| area.height.saturating_sub(8) as usize)
            .unwrap_or(1)
            .max(1);
        let max_scroll = total_rows.saturating_sub(visible_rows);
        self.global_search_scroll = self.global_search_scroll.min(max_scroll);
        if selected_row < self.global_search_scroll {
            self.global_search_scroll = selected_row;
        } else if selected_row >= self.global_search_scroll.saturating_add(visible_rows) {
            self.global_search_scroll = selected_row.saturating_sub(visible_rows - 1);
        }
    }

    pub(crate) fn ensure_global_search_selection_visible(&mut self) {
        if self.global_search_results.is_empty() {
            self.global_search_selected = 0;
            self.global_search_scroll = 0;
            return;
        }
        if self.global_search_selected >= self.global_search_results.len() {
            self.global_search_selected = self.global_search_results.len() - 1;
        }
        if self.is_global_search_result_visible(self.global_search_selected) {
            return;
        }
        let next = self.global_search_results.first_visible_from(self.global_search_selected + 1)
            .or_else(|| self.global_search_results.last_visible_before(self.global_search_selected));
        if let Some(idx) = next.or_else(|| self.first_visible_global_search_result_index()) {
            self.global_search_selected = idx;
        }
    }

    pub(crate) fn global_search_select_prev_visible(&mut self) {
        if self.global_search_selected == 0 {
            if !self.is_global_search_result_visible(self.global_search_selected) {
                self.ensure_global_search_selection_visible();
            }
            return;
        }
        let previous = if self.apply_global_search_collapsed_visibility() {
            self.global_search_results.last_visible_before(self.global_search_selected)
        } else {
            self.global_search_selected.min(self.global_search_results.len()).checked_sub(1)
        };
        if let Some(idx) = previous {
            self.global_search_selected = idx;
        } else if !self.is_global_search_result_visible(self.global_search_selected) {
            self.ensure_global_search_selection_visible();
        }
    }

    pub(crate) fn global_search_select_next_visible(&mut self) {
        let len = self.global_search_results.len();
        if len == 0 || self.global_search_selected >= len.saturating_sub(1) {
            if !self.is_global_search_result_visible(self.global_search_selected) {
                self.ensure_global_search_selection_visible();
            }
            return;
        }
        let next = if self.apply_global_search_collapsed_visibility() {
            self.global_search_results.first_visible_from(self.global_search_selected + 1)
        } else { Some(self.global_search_selected + 1) };
        if let Some(idx) = next {
            self.global_search_selected = idx;
        } else if !self.is_global_search_result_visible(self.global_search_selected) {
            self.ensure_global_search_selection_visible();
        }
    }

    pub(crate) fn toggle_global_search_file_collapsed(&mut self, path: &Path) {
        self.global_search_results.set_collapsed(path, !self.is_global_search_file_collapsed(path));
        self.ensure_global_search_selection_visible();
        self.reveal_global_search_selection();
    }

    pub(crate) fn all_global_search_files_collapsed(&self) -> bool {
        self.global_search_results.all_collapsed()
    }

    pub(crate) fn global_search_all_files_toggle_label(&self) -> &'static str {
        if self.all_global_search_files_collapsed() {
            "⊞"
        } else {
            "⊟"
        }
    }

    pub(crate) fn toggle_global_search_all_files_collapsed(&mut self) {
        if self.global_search_results.is_empty() {
            return;
        }
        self.global_search_results.set_all_collapsed(!self.all_global_search_files_collapsed());
        self.ensure_global_search_selection_visible();
    }

    // ── Notifications ──────────────────────────────────────────────────

    pub(crate) fn notify(&mut self, msg: impl Into<String>) {
        self.notify_level(msg, NotifyLevel::Info);
    }

    pub(crate) fn notify_warn(&mut self, msg: impl Into<String>) {
        self.notify_level(msg, NotifyLevel::Warning);
    }

    pub(crate) fn notify_error(&mut self, msg: impl Into<String>) {
        self.notify_level(msg, NotifyLevel::Error);
    }

    pub(crate) fn show_save_error(&mut self, message: impl Into<String>) {
        self.save_conflict = None;
        self.modal.kind = None;
        let message = message.into();
        self.notify_error(message.clone());
        self.status_message = "Save failed; your edits remain open.".into();
        self.save_error = Some(message);
    }

    fn notify_level(&mut self, msg: impl Into<String>, level: NotifyLevel) {
        let msg = msg.into();
        self.notification_history.push((
            msg,
            level,
            self.notification_history.last().map_or(0, |(_, _, sequence)| sequence.saturating_add(1)),
        ));
        if self.notification_history.len() > 200 {
            self.notification_history.remove(0);
        }
    }

    // ── Diff state ─────────────────────────────────────────────────────

    pub(crate) fn select_file_for_compare(&mut self, path: std::path::PathBuf) {
        if path.is_dir() {
            self.notify("Select a file to compare");
            return;
        }
        let label = self.compare_path_label(Some(&path), "file");
        self.selected_compare_path = Some(path);
        self.notify(format!("Selected for compare: {label}"));
    }

    pub(crate) fn can_compare_with_selected(&self, path: &Path) -> bool {
        self.selected_compare_path
            .as_deref()
            .is_some_and(|base| !Self::same_open_path(base, path))
    }

    pub(crate) fn compare_file_with_selected(&mut self, target: std::path::PathBuf) {
        let Some(base) = self.selected_compare_path.clone() else {
            self.notify("No file selected for compare. Use 'Select for Compare' first.");
            return;
        };
        if Self::same_open_path(&base, &target) {
            self.notify("Select a different file to compare with the selected file");
            return;
        }
        if target.is_dir() {
            self.notify("Select a file to compare");
            return;
        }
        self.begin_path_compare(base, target);
    }

    pub(crate) fn refresh_compare_diff(&mut self) {
        // Live Source Control tabs refresh both revisions together, including
        // inactive tabs. The generic comparer handles fixed snapshots/files.
        if self.active_diff_tab_id().is_some() {
            return;
        }
        if self.diff_mode != DiffMode::Off
            && self.active_editor_matches_diff_target()
            && self.compare_has_binary_editor()
        {
            self.reject_binary_diff();
            return;
        }
        if !self.should_render_diff_for_active_editor() {
            return;
        }

        let Some(editor) = self.editor() else {
            return;
        };
        if editor.bg_loading || editor.large_file_mode {
            return;
        }

        let target_version = Some(editor.content_version);
        let base_revision = self.compare_base_revision();
        if self.compare_diff_target_version == target_version
            && self.compare_diff_base_revision == Some(base_revision)
        {
            return;
        }
        if self.bg_diff_refresh_running {
            return;
        }

        let target = crate::background::DiffRefreshTarget {
            generation: self.diff_refresh_generation,
            active_group: self.active_group,
            active_tab: self.active_tab,
            file_path: editor.file_path.clone(),
            content_version: editor.content_version,
            len_bytes: editor.rope.len_bytes(),
            line_count: editor.rope.len_lines(),
            last_disk_modified: editor.last_disk_modified,
        };
        let target_rope = editor.rope.clone();

        let base_snapshot = if let Some(base_path) = self.compare_base_path.clone() {
            let open_rope = self
                .tabs
                .iter()
                .find(|tab| {
                    tab.kind == crate::app::TabKind::File
                        && !tab.editor.bg_loading
                        && tab.editor.file_path.as_deref() == Some(base_path.as_path())
                })
                .map(|tab| tab.editor.rope.clone())
                .or_else(|| {
                    self.groups
                        .iter()
                        .enumerate()
                        .find_map(|(group_idx, group)| {
                            if group_idx == self.active_group {
                                return None;
                            }
                            group
                                .tabs
                                .iter()
                                .find(|tab| {
                                    tab.kind == crate::app::TabKind::File
                                        && !tab.editor.bg_loading
                                        && tab.editor.file_path.as_deref()
                                            == Some(base_path.as_path())
                                })
                                .map(|tab| tab.editor.rope.clone())
                        })
                });
            open_rope.map_or(
                CompareBaseSnapshot::Disk(base_path),
                CompareBaseSnapshot::Rope,
            )
        } else if let Some(rope) = self.pending_compare_base_rope.clone() {
            CompareBaseSnapshot::Rope(rope)
        } else {
            CompareBaseSnapshot::Text(
                self.compare_base_content
                    .clone()
                    .unwrap_or_else(|| std::sync::Arc::<str>::from("")),
            )
        };

        let sender = self.background.sender.clone();
        self.bg_diff_refresh_running = true;
        self.background.spawn("diff-refresh", move || {
            let mut completed_base_revision = base_revision;
            let result = (|| -> Result<crate::background::PreparedDiffRefresh, String> {
                let base_content: std::sync::Arc<str> = match base_snapshot {
                    CompareBaseSnapshot::Text(content) => content,
                    CompareBaseSnapshot::Rope(rope) => rope.to_string().into(),
                    CompareBaseSnapshot::Disk(path) => {
                        let loaded = crate::editor::load_text_file(&path).map_err(|error| {
                            format!("Cannot read compare base {}: {error}", path.display())
                        })?;
                        if loaded.encoding == "Binary" {
                            return Err(BINARY_DIFF_MESSAGE.into());
                        }
                        completed_base_revision = crate::app::DiffBaseRevision::DiskFile {
                            len: loaded.file_size,
                            modified: loaded.last_disk_modified,
                        };
                        loaded.rope.to_string().into()
                    }
                };
                let target_content = target_rope.to_string();
                let diff_lines = compute_file_diff(base_content.as_ref(), &target_content);
                let render_cache = crate::app::DiffRenderCache::new(&diff_lines);
                let target_line_markers =
                    crate::app_git_display::target_line_markers_from_compare_diff(
                        &diff_lines,
                        target.line_count,
                    );
                Ok(crate::background::PreparedDiffRefresh {
                    base_content,
                    diff_lines,
                    render_cache,
                    target_line_markers,
                })
            })();
            let _ = sender.send(crate::background::BackgroundResult::DiffRefresh {
                target,
                base_revision: completed_base_revision,
                result,
            });
        });
    }

    /// Start a file-to-file compare without reading or diffing either file on
    /// the terminal event loop. Large target files naturally wait for their
    /// existing asynchronous open to complete.
    pub(crate) fn begin_path_compare(
        &mut self,
        base_path: std::path::PathBuf,
        target_path: std::path::PathBuf,
    ) {
        let base_name = base_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("base")
            .to_string();
        let target_name = target_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("target")
            .to_string();
        self.open_file(&target_path);
        self.clear_diff_state();
        self.diff_labels = Some(crate::app::DiffLabels::new(
            self.compare_path_label(Some(&base_path), &base_name),
            self.compare_path_label(Some(&target_path), &target_name),
        ));
        self.compare_base_path = Some(base_path);
        self.compare_base_content = Some(std::sync::Arc::<str>::from(""));
        self.diff_tab_label = Some(format!("{base_name} ↔ {target_name}"));
        self.diff_target_path = Some(target_path);
        self.diff_mode = self.preferred_diff_mode;
        self.status_message = format!("Preparing diff: {base_name} ↔ {target_name}...");
        self.refresh_compare_diff();
    }

    /// Compare the active editor with another tab. Rope cloning is O(1); any
    /// flattening and diff construction happens on the worker.
    pub(crate) fn begin_tab_compare(&mut self, base_tab: usize) -> bool {
        let Some(base) = self.tabs.get(base_tab) else {
            return false;
        };
        if base.editor.encoding == "Binary"
            || self.editor().is_some_and(|editor| editor.encoding == "Binary")
        {
            self.reject_binary_diff();
            return false;
        }
        let base = &self.tabs[base_tab];
        let base_name = base.editor.file_name().to_string();
        let base_path = base.editor.file_path.clone();
        let base_rope = base.editor.rope.clone();
        let Some(target) = self.editor() else {
            return false;
        };
        let target_name = target.file_name().to_string();
        let target_path = target.file_path.clone();

        self.clear_diff_state();
        self.diff_labels = Some(crate::app::DiffLabels::new(
            self.compare_path_label(base_path.as_deref(), &base_name),
            self.compare_path_label(target_path.as_deref(), &target_name),
        ));
        self.compare_base_path = base_path;
        self.pending_compare_base_rope = Some(base_rope);
        self.compare_base_content = Some(std::sync::Arc::<str>::from(""));
        self.diff_tab_label = Some(format!("{base_name} ↔ {target_name}"));
        self.diff_target_path = target_path;
        self.diff_mode = self.preferred_diff_mode;
        self.status_message = format!("Preparing diff: {base_name} ↔ {target_name}...");
        self.refresh_compare_diff();
        true
    }

    fn compare_path_label(&self, path: Option<&Path>, fallback: &str) -> String {
        path.map_or_else(
            || fallback.to_string(),
            |path| {
                path.strip_prefix(&self.explorer.root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/")
            },
        )
    }

    pub(crate) fn set_compare_diff_lines(&mut self, compare_diff_lines: Vec<(char, String)>) {
        let target_line_markers = crate::app_git_display::target_line_markers_from_compare_diff(
            &compare_diff_lines,
            self.editor().map_or(0, |editor| editor.rope.len_lines()),
        );
        let render_cache = crate::app::DiffRenderCache::new(&compare_diff_lines);
        self.apply_prepared_compare_diff(compare_diff_lines, render_cache, target_line_markers);
    }

    /// Install diff data already prepared on a worker thread. This is kept
    /// separate from `set_compare_diff_lines` so large compares do not rebuild
    /// row alignment and character highlights during a UI frame.
    pub(crate) fn apply_prepared_compare_diff(
        &mut self,
        compare_diff_lines: Vec<(char, String)>,
        render_cache: crate::app::DiffRenderCache,
        target_line_markers: Vec<char>,
    ) {
        let target_version = self.editor().map(|editor| editor.content_version);
        let base_revision = self.compare_base_revision();
        self.compare_diff_lines = compare_diff_lines;
        self.diff_render_cache = std::sync::Arc::new(render_cache);
        self.compare_diff_target_version = target_version;
        self.compare_diff_base_revision = Some(base_revision);
        if let Some(editor) = self.editor_mut() {
            editor.set_git_line_status(target_line_markers);
        }
    }

    pub(crate) fn compare_base_revision(&self) -> crate::app::DiffBaseRevision {
        let Some(path) = self.compare_base_path.as_deref() else {
            return crate::app::DiffBaseRevision::Snapshot;
        };

        if let Some(tab) = self.tabs.iter().find(|tab| {
            tab.kind == crate::app::TabKind::File && tab.editor.file_path.as_deref() == Some(path)
        }) {
            return crate::app::DiffBaseRevision::OpenEditor {
                content_version: tab.editor.content_version,
                line_count: tab.editor.rope.len_lines(),
                last_disk_modified: tab.editor.last_disk_modified,
            };
        }

        for (group_idx, group) in self.groups.iter().enumerate() {
            if group_idx == self.active_group {
                continue;
            }
            if let Some(tab) = group.tabs.iter().find(|tab| {
                tab.kind == crate::app::TabKind::File
                    && tab.editor.file_path.as_deref() == Some(path)
            }) {
                return crate::app::DiffBaseRevision::OpenEditor {
                    content_version: tab.editor.content_version,
                    line_count: tab.editor.rope.len_lines(),
                    last_disk_modified: tab.editor.last_disk_modified,
                };
            }
        }

        // Disk metadata is captured by the diff worker (and by watcher events),
        // never by the UI path that decides whether a refresh is needed.
        self.compare_diff_base_revision
            .unwrap_or(crate::app::DiffBaseRevision::Missing)
    }

    pub(crate) fn active_editor_matches_diff_target(&self) -> bool {
        if !self.rendering_inactive_editor && self.active_diff_tab_id() != self.diff_tabs.owner {
            return false;
        }
        let Some(target_path) = self.diff_target_path.as_ref() else {
            return true;
        };
        self.editor()
            .and_then(|e| e.file_path.as_ref())
            .is_some_and(|path| path == target_path)
    }

    pub(crate) fn should_render_diff_for_active_editor(&self) -> bool {
        self.diff_mode != DiffMode::Off
            && self.compare_base_content.is_some()
            && self.active_editor_matches_diff_target()
            && !self.compare_has_binary_editor()
    }

    fn compare_has_binary_editor(&self) -> bool {
        self.editor().is_some_and(|editor| editor.encoding == "Binary")
            || self.compare_base_path.as_ref().is_some_and(|base| {
                self.tabs
                    .iter()
                    .chain(self.groups.iter().flat_map(|group| &group.tabs))
                    .any(|tab| {
                        tab.kind == crate::app::TabKind::File
                            && tab.editor.file_path.as_ref() == Some(base)
                            && tab.editor.encoding == "Binary"
                    })
            })
    }

    pub(crate) fn reject_binary_diff(&mut self) {
        self.clear_diff_state();
        self.status_message = BINARY_DIFF_MESSAGE.into();
        self.notify(BINARY_DIFF_MESSAGE);
    }

    /// Scroll editor to the first changed line in the diff.
    pub(crate) fn scroll_to_first_diff_change(&mut self) {
        // The worker-prepared render cache already records exact hunk targets,
        // avoiding another scan through a large unchanged prefix on the UI thread.
        let target_line = self
            .diff_render_cache
            .hunks
            .first()
            .map_or(0, |hunk| hunk.target_start);
        if let Some(ed) = self.editor_mut() {
            let max_line = ed.rope.len_lines().saturating_sub(1);
            let line = target_line.min(max_line);
            ed.cursor_line = line;
            ed.cursor_col = 0;
            // Center the change in viewport
            let half_viewport = 10; // approximate
            ed.scroll_offset = line.saturating_sub(half_viewport);
        }
    }

    pub(crate) fn active_diff_uses_snapshot_target(&self) -> bool {
        self.diff_mode != DiffMode::Off
            && self.active_editor_matches_diff_target()
            && self.editor().is_some_and(|editor| editor.read_only)
    }

    pub(crate) fn has_diff_layout_preference(&self) -> bool {
        self.settings_workspace.diff_side_by_side.is_some() || self.settings_global.diff_side_by_side.is_some()
    }

    pub(crate) fn save_diff_layout_preference(&mut self) {
        let side_by_side = match self.diff_mode {
            DiffMode::Off => return,
            DiffMode::SideBySide => true,
            DiffMode::Inline => false,
        };
        self.preferred_diff_mode = self.diff_mode;
        // A layout choice follows the user unless the workspace explicitly
        // overrides it. The currently selected Settings page is unrelated.
        let scope = if self.settings_workspace.diff_side_by_side.is_some() {
            crate::settings::SettingsScope::Workspace
        } else { crate::settings::SettingsScope::Global };
        if let Err(error) = self.patch_settings_scope(scope, "diffEditor.renderSideBySide", |settings| {
            settings.diff_side_by_side = Some(side_by_side);
        }) {
            self.notify_error(format!("Could not save diff layout: {error}"));
        }
    }

    pub(crate) fn clear_diff_state(&mut self) {
        self.clear_diff_selection();
        self.diff_scope_source.forget_original_view(&mut self.animations);
        self.diff_scope_source = Default::default();
        let diff_target_path = self.diff_target_path.clone();
        self.git_compare_generation = self.git_compare_generation.wrapping_add(1);
        self.diff_refresh_generation = self.diff_refresh_generation.wrapping_add(1);
        self.pending_git_compare_path = None;
        self.bg_git_compare_running = false;
        self.bg_diff_refresh_running = false;
        self.diff_mode = DiffMode::Off;
        self.diff_layout_toggle_area = None;
        self.diff_tab_label = None;
        self.diff_labels = None;
        self.diff_target_path = None;
        self.compare_base_path = None;
        self.compare_base_content = None;
        self.pending_compare_base_rope = None;
        self.compare_diff_lines.clear();
        self.diff_render_cache = std::sync::Arc::new(crate::app::DiffRenderCache::default());
        self.compare_diff_target_version = None;
        self.compare_diff_base_revision = None;
        self.diff_split_area = None;
        self.diff_divider_area = None;
        self.diff_left_code_area = None;
        self.diff_left_h_scrollbar_area = None;
        self.diff_left_horizontal_scroll = 0;
        self.diff_inline_scroll = crate::inline_diff::InlineDiffScroll::default();
        self.resizing_diff_split = false;
        self.dragging_diff_left_h_scrollbar = false;
        if let Some(diff_target_path) = diff_target_path {
            for tab in &mut self.tabs {
                if tab.editor.file_path.as_deref() == Some(diff_target_path.as_path()) {
                    tab.editor.clear_git_line_status();
                }
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    if tab.editor.file_path.as_deref() == Some(diff_target_path.as_path()) {
                        tab.editor.clear_git_line_status();
                    }
                }
            }
        }
    }

    // ── Theme ──────────────────────────────────────────────────────────

    /// Discover the remaining theme catalog after the first workbench frame.
    pub(crate) fn request_external_themes_async(&mut self) {
        if self.bg_external_theme_scan_running {
            return;
        }
        self.bg_external_theme_scan_running = true;
        let sender = self.background.sender.clone();
        let pool = self.background.maintenance_sender();
        let activity = self.background.activity.start("Loading themes", None);
        if pool
            .send(Box::new(move || {
                let _activity = activity.running();
                let mut themes = crate::app_theme::load_builtin_themes();
                crate::app_theme::merge_themes(&mut themes, crate::app_theme::load_external_themes());
                let _ = sender.send(
                    crate::background::BackgroundResult::ExternalThemesLoaded(themes),
                );
            }))
            .is_err()
        {
            self.bg_external_theme_scan_running = false;
        }
    }

    /// Layer discovered themes over embedded defaults and re-apply a theme
    /// requested by settings once it becomes available.
    pub(crate) fn install_external_themes(
        &mut self,
        themes: Vec<crate::app_theme::ThemeFile>,
    ) -> bool {
        self.bg_external_theme_scan_running = false;
        let previous_theme = self.theme_key.clone();
        let themes = self.filter_removed_themes(themes);
        let mut placeholder_changed = previous_theme != self.theme_key;
        if let Some(key) = self.startup_theme_placeholder.take()
            && !themes.iter().any(|theme| theme.config_key == key)
            && let Some(placeholder) = self.loaded_themes.iter_mut().find(|theme| theme.config_key == key)
        {
            // Keep an unavailable user's theme choice intact on disk. The
            // fallback stays usable and no loading label remains indefinitely.
            placeholder.name = format!("Unavailable theme: {key}");
            if self.theme_key == key { self.theme_name = placeholder.name.clone(); }
            placeholder_changed = true;
        }
        // An unchanged startup theme must not invalidate syntax caches or
        // manufacture another frame when the background catalog arrives.
        let themes: Vec<_> = themes.into_iter().filter(|incoming| {
            !self.loaded_themes.iter().any(|loaded| loaded == incoming)
        }).collect();
        if themes.is_empty() {
            if placeholder_changed { self.refresh_palette_commands(); }
            return placeholder_changed;
        }

        let requested_key = self
            .settings_effective
            .theme
            .clone()
            .unwrap_or_else(|| self.committed_theme_key().to_string());
        let requested_arrived = themes
            .iter()
            .any(|theme| theme.config_key == requested_key);
        let palette_was_active = self.palette.active;
        let previous_generation = self.highlighter.registry_generation();
        for theme in &themes {
            if let Some(syntax_theme) = &theme.textmate_theme {
                self.highlighter
                    .register_theme(&theme.syntax_theme, syntax_theme.clone());
            }
        }
        crate::app_theme::merge_themes(&mut self.loaded_themes, themes);
        self.refresh_palette_commands();

        let theme_changed = requested_arrived && self.set_theme_by_key(&requested_key);
        self.preview_palette_theme();
        let generation = self.highlighter.registry_generation();
        if generation != previous_generation && self.applied_theme_generation != Some(generation) {
            // Replacing even an unselected theme rejects jobs from the old
            // registry. Their viewport keys must not leave restored files
            // waiting for a tab switch to schedule a replacement.
            self.applied_theme_generation = Some(generation);
            self.invalidate_visible_highlights();
            self.refresh_visible_highlights_async();
        }
        // Merely enriching a hidden command palette must not manufacture a
        // terminal frame after startup.
        theme_changed || palette_was_active || placeholder_changed
    }

    /// Validate installed grammar sources and parse optional user/workspace
    /// overlays after the startup screen, on workspace changes, or on demand.
    /// Coalesce requests so an edit during a build gets one fresh follow-up.
    pub(crate) fn request_syntax_assets_async(&mut self) {
        if self.bg_syntax_asset_refresh_running {
            self.bg_syntax_asset_refresh_pending = true;
            return;
        }
        let candidates = vec![
            crate::portable::data_dir().join("syntaxes"),
            crate::portable::workspace_dir(&self.explorer.root).join("syntaxes"),
        ];
        let refresh = self.highlighter.syntax_asset_refresh(candidates);
        self.bg_syntax_asset_refresh_running = true;
        let sender = self.background.sender.clone();
        let root = self.explorer.root.clone();
        let pool = self.background.maintenance_sender();
        let activity = self.background.activity.start("Loading syntaxes", None);
        if pool
            .send(Box::new(move || {
                let _activity = activity.running();
                let result = refresh.run_with_cached(|loaded| {
                    let _ = sender.send(crate::background::BackgroundResult::StartupSyntaxCacheLoaded {
                        root: root.clone(), loaded,
                    });
                });
                let _ = sender.send(
                    crate::background::BackgroundResult::SyntaxAssetsLoaded { root, result },
                );
            }))
            .is_err()
        {
            self.bg_syntax_asset_refresh_running = false;
            self.syntax_reload_requested = false;
            self.notify_error("Could not start loading syntaxes");
        }
    }

    pub(crate) fn reload_syntaxes(&mut self) {
        self.syntax_reload_requested = true;
        self.request_syntax_assets_async();
    }

    /// Atomically publish a refreshed syntax registry. Highlight work already
    /// running against the previous immutable registry is rejected by its
    /// generation, while the currently painted cells remain stable until a
    /// fresh viewport batch is ready.
    pub(crate) fn install_syntax_assets(
        &mut self,
        root: std::path::PathBuf,
        result: Result<Option<crate::highlight::LoadedSyntaxAssets>, String>,
    ) -> bool {
        let was_running = self.bg_syntax_asset_refresh_running;
        self.bg_syntax_asset_refresh_running = false;
        if self.bg_syntax_asset_refresh_pending || root != self.explorer.root {
            self.bg_syntax_asset_refresh_pending = false;
            self.request_syntax_assets_async();
            return false;
        }
        let explicit_reload = std::mem::take(&mut self.syntax_reload_requested);
        let loaded = match result {
            Ok(Some(loaded)) => loaded,
            Ok(None) => {
                if explicit_reload { self.notify("Syntaxes reloaded"); }
                return was_running;
            }
            Err(error) => {
                log_warn!("failed to refresh syntax assets: {error}");
                self.notify_error(format!("Could not load syntaxes: {error}"));
                return true;
            }
        };
        if let Some(error) = loaded.cache_write_error.as_deref() {
            log_warn!("{error}");
            self.notify_warn(format!("Syntaxes loaded for this session, but {error}"));
        }
        if let Some(warning) = loaded.warning.as_deref() {
            self.notify_warn(warning);
        } else if explicit_reload {
            self.notify("Syntaxes reloaded");
        }
        self.apply_loaded_syntax_assets(loaded);
        // Redraw to remove the loading indicator and update language lists.
        true
    }

    pub(crate) fn apply_loaded_syntax_assets(&mut self, loaded: crate::highlight::LoadedSyntaxAssets) {
        self.highlighter.install_syntax_assets(loaded);
        self.invalidate_visible_highlights();
        self.refresh_palette_commands();
        self.refresh_visible_highlights_async();
    }

    /// Set the active theme by config_key. Returns true if the theme was found and applied.
    pub(crate) fn set_theme_by_key(&mut self, key: &str) -> bool {
        if let Some(tf) = self.loaded_themes.iter().find(|t| t.config_key == key) {
            let mut highlights_changed = self.syntax_theme != tf.syntax_theme;
            self.theme_key = tf.config_key.clone();
            self.theme_name = tf.name.clone();
            self.syntax_theme = tf.syntax_theme.clone();
            let selected = crate::vscode_theme::customized_theme(
                tf, &self.settings_effective.workbench_color_customizations,
            );
            self.colors = selected.to_theme_colors();
            if let Some(syntax) = selected.textmate_theme.clone() {
                highlights_changed |= self.highlighter.register_theme(&selected.syntax_theme, syntax);
            }
            self.semantic_theme_colors = selected.semantic_colors.clone();
            // Chat display preferences also save settings. Keep editor colors
            // and in-flight work intact unless the effective syntax changed.
            // Include registrations made by background theme installation.
            let generation = self.highlighter.registry_generation();
            if highlights_changed || self.applied_theme_generation != Some(generation) {
                self.applied_theme_generation = Some(generation);
                self.invalidate_visible_highlights();
                self.refresh_visible_highlights_async();
            }
            true
        } else {
            false
        }
    }

    /// Cycle to the next theme in the loaded theme list
    pub(crate) fn next_theme(&mut self) {
        if self.loaded_themes.is_empty() {
            return;
        }
        let idx = self
            .loaded_themes
            .iter()
            .position(|t| t.config_key == self.theme_key)
            .unwrap_or(0);
        let next_idx = (idx + 1) % self.loaded_themes.len();
        let key = self.loaded_themes[next_idx].config_key.clone();
        self.set_theme_by_key(&key);
    }

    // ── Navigation & edit history ──────────────────────────────────────

    pub(crate) fn record_edit_position(&mut self) {
        if let Some(ed) = self.editor() {
            self.last_edit_position = Some((self.active_tab, ed.cursor_line, ed.cursor_col));
            self.last_edit_time = std::time::Instant::now();
        }
    }

    pub(crate) fn push_clipboard_history(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Remove duplicate if exists
        self.clipboard_history.retain(|s| s != text);
        self.clipboard_history.push_back(text.to_string());
        // Keep max 20 entries
        if self.clipboard_history.len() > 20 {
            self.clipboard_history.pop_front();
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_global_search_result(id: usize) -> crate::app::GlobalSearchResult {
        crate::app::GlobalSearchResult {
            file_path: PathBuf::from(format!("file-{id}.txt")),
            line_num: id + 1,
            line_text: format!("match {id}").into(),
            col_start: 0,
            col_end: 5,
            context_before: None,
            context_after: None,
        }
    }

    // ── clear_drag_states ────────────────────────────────────────────

    #[test]
    fn clear_drag_states_resets_all_drag_fields() {
        let mut app = App::test_new();
        app.search_bar_layout.resizing = true;
        app.commit_input.scrollbar_drag = Some((2, 4));
        app.dragging_tab = Some(3);
        app.dragging_tab_source_group = Some(1);
        app.dragging_terminal_tab = Some(2);
        app.dragging_explorer_path = Some(PathBuf::from("foo"));
        app.explorer_drop_target_idx = Some(2);
        app.dragging_text = true;
        app.dragging_text_start = Some((1, 2));
        app.editor_selection_drag = Some(42);
        app.alt_cursor_mouse_down_active = true;
        app.resizing_explorer = true;
        app.explorer_scrollbar_drag = Some((crate::ui_chrome::ExplorerSection::Files, 2, 4));
        app.resizing_panel = true;
        app.resizing_sc_graph = true;
        app.resizing_split = true;
        app.settings_scrollbar_drag = Some(crate::settings_layout::Drag { row: 0, offset: 0 });
        app.dragging_v_scrollbar = true;
        app.dragging_h_scrollbar = true;
        app.dragging_minimap = true;
        app.terminal_dock_drop_target = Some(crate::app::TerminalDock::Right);

        app.clear_drag_states();

        assert!(!app.search_bar_layout.resizing);
        assert!(app.commit_input.scrollbar_drag.is_none());
        assert_eq!(app.dragging_tab, None);
        assert_eq!(app.dragging_tab_source_group, None);
        assert_eq!(app.dragging_terminal_tab, None);
        assert_eq!(app.dragging_explorer_path, None);
        assert_eq!(app.explorer_drop_target_idx, None);
        assert!(!app.dragging_text);
        assert_eq!(app.dragging_text_start, None);
        assert_eq!(app.editor_selection_drag, None);
        assert!(!app.alt_cursor_mouse_down_active);
        assert!(app.tab_drop_zone.is_none());
        assert_eq!(app.tab_drop_target_group, None);
        assert_eq!(app.terminal_dock_drop_target, None);
        assert!(!app.resizing_explorer);
        assert!(app.explorer_scrollbar_drag.is_none());
        assert!(!app.resizing_panel);
        assert!(!app.resizing_sc_graph);
        assert!(!app.resizing_split);
        assert!(app.settings_scrollbar_drag.is_none());
        assert_eq!(app.dragging_divider, None);
        assert!(!app.dragging_v_scrollbar);
        assert!(!app.dragging_h_scrollbar);
        assert!(!app.dragging_minimap);
    }

    // ── reset_hover_states ───────────────────────────────────────────

    #[test]
    fn reset_hover_states_clears_all_hover_fields() {
        let mut app = App::test_new();
        app.hovered_tab = Some(2);
        app.hovered_tab_close_btn = true;
        app.hovered_tab_scroll_left = true;
        app.hovered_tab_scroll_right = true;
        app.tab_tooltip_tab = Some(2);
        app.tab_tooltip_hover_start = Some(std::time::Instant::now());
        app.tab_tooltip_shown = true;
        app.hovered_activity = Some(1);
        app.hovered_menu = Some(0);
        app.hovered_status_item = Some(3);
        app.hovered_explorer = Some(5);
        app.hovered_close_button = true;
        app.hovered_panel_close = true;
        app.hovered_panel_tab = Some(1);
        app.hovered_terminal_plus = true;
        app.hovered_terminal_tab = Some(0);
        app.hovered_terminal_close = Some(1);
        app.hovered_new_tab_button = true;
        app.hovered_scrollbar = true;
        app.hovered_minimap = true;
        app.hovered_h_scrollbar = true;
        app.hovered_search_toggle = Some(2);
        app.hovered_gutter_line = Some(10);
        app.hovered_editor_pos = Some((1, 2));
        app.outline_hovered = Some(3);

        app.reset_hover_states();

        assert_eq!(app.hovered_tab, None);
        assert!(!app.hovered_tab_close_btn);
        assert!(!app.hovered_tab_scroll_left);
        assert!(!app.hovered_tab_scroll_right);
        assert!(app.tab_tooltip_tab.is_none());
        assert!(app.tab_tooltip_hover_start.is_none());
        assert!(!app.tab_tooltip_shown);
        assert_eq!(app.hovered_activity, None);
        assert_eq!(app.hovered_menu, None);
        assert_eq!(app.hovered_status_item, None);
        assert_eq!(app.hovered_explorer, None);
        assert!(!app.hovered_close_button);
        assert!(!app.hovered_panel_close);
        assert_eq!(app.hovered_panel_tab, None);
        assert!(!app.hovered_terminal_plus);
        assert_eq!(app.hovered_terminal_tab, None);
        assert_eq!(app.hovered_terminal_close, None);
        assert!(!app.hovered_new_tab_button);
        assert!(!app.hovered_scrollbar);
        assert!(!app.hovered_minimap);
        assert!(!app.hovered_h_scrollbar);
        assert_eq!(app.hovered_search_toggle, None);
        assert_eq!(app.hovered_gutter_line, None);
        assert_eq!(app.hovered_editor_pos, None);
        assert_eq!(app.outline_hovered, None);
    }

    // ── open_palette ─────────────────────────────────────────────────

    #[test]
    fn open_palette_activates_and_clears_query() {
        let mut app = App::test_new();
        assert!(!app.palette.active);

        app.open_palette();

        assert!(app.palette.active);
        assert!(app.palette.query.is_empty());
        assert_eq!(app.palette.cursor, 0);
        assert_eq!(app.palette.selected, 0);
        assert_eq!(app.palette.scroll_offset, 0);
    }

    #[test]
    fn open_palette_toggles_off_when_already_active() {
        let mut app = App::test_new();
        app.open_palette();
        assert!(app.palette.active);

        app.open_palette();
        assert!(!app.palette.active);
    }

    #[test]
    fn open_palette_populates_filtered_commands() {
        let mut app = App::test_new();
        app.open_palette();
        // After opening, filtered list should not be empty (commands are loaded)
        assert!(!app.palette.filtered.is_empty());
    }

    // ── update_palette_filter ────────────────────────────────────────

    #[test]
    fn update_palette_filter_empty_query_shows_all() {
        let mut app = App::test_new();
        app.open_palette();
        app.palette.query.clear();
        app.update_palette_filter();
        assert_eq!(app.palette.filtered.len(), app.palette.commands.len());
    }

    #[test]
    fn update_palette_filter_with_query_narrows_results() {
        let mut app = App::test_new();
        app.open_palette();
        let all_count = app.palette.filtered.len();
        app.palette.query = "theme".to_string();
        app.update_palette_filter();
        assert!(app.palette.filtered.len() < all_count);
        assert!(!app.palette.filtered.is_empty());
    }

    #[test]
    fn update_palette_filter_nonsense_query_yields_empty() {
        let mut app = App::test_new();
        app.open_palette();
        app.palette.query = "zzzxqwvnonsense999".to_string();
        app.update_palette_filter();
        assert!(app.palette.filtered.is_empty());
    }

    // ── toggle_explorer ──────────────────────────────────────────────

    #[test]
    fn toggle_explorer_flips_show_explorer() {
        let mut app = App::test_new();
        let original = app.show_explorer;
        app.toggle_explorer();
        assert_eq!(app.show_explorer, !original);
        app.toggle_explorer();
        assert_eq!(app.show_explorer, original);
    }

    #[test]
    fn toggle_explorer_resets_active_pane_when_hiding() {
        let mut app = App::test_new();
        app.show_explorer = true;
        app.active_pane = ActivePane::Explorer;
        app.toggle_explorer();
        assert!(!app.show_explorer);
        assert_eq!(app.active_pane, ActivePane::Editor);
    }

    // ── palette_recent_key_for_index ─────────────────────────────────

    #[test]
    fn palette_recent_key_returns_command_id() {
        let mut app = App::test_new();
        app.open_palette();
        if !app.palette.commands.is_empty() {
            let key = app.palette_recent_key_for_index(0);
            assert_eq!(key, app.palette.commands[0].id);
        }
    }

    // ── push_clipboard_history ───────────────────────────────────────

    #[test]
    fn push_clipboard_history_adds_entry() {
        let mut app = App::test_new();
        app.push_clipboard_history("hello");
        assert_eq!(app.clipboard_history.len(), 1);
        assert_eq!(app.clipboard_history[0], "hello");
    }

    #[test]
    fn push_clipboard_history_ignores_empty_string() {
        let mut app = App::test_new();
        app.push_clipboard_history("");
        assert!(app.clipboard_history.is_empty());
    }

    #[test]
    fn push_clipboard_history_deduplicates() {
        let mut app = App::test_new();
        app.push_clipboard_history("dup");
        app.push_clipboard_history("other");
        app.push_clipboard_history("dup");
        assert_eq!(app.clipboard_history.len(), 2);
        // "dup" should be at the back (most recent)
        assert_eq!(app.clipboard_history.back().unwrap(), "dup");
        assert_eq!(app.clipboard_history.front().unwrap(), "other");
    }

    #[test]
    fn push_clipboard_history_caps_at_20() {
        let mut app = App::test_new();
        for i in 0..25 {
            app.push_clipboard_history(&format!("item-{i}"));
        }
        assert_eq!(app.clipboard_history.len(), 20);
        // Oldest entries should have been evicted
        assert_eq!(app.clipboard_history.front().unwrap(), "item-5");
        assert_eq!(app.clipboard_history.back().unwrap(), "item-24");
    }

    // ── record_edit_position ─────────────────────────────────────────

    #[test]
    fn record_edit_position_stores_cursor() {
        let mut app = App::test_new();
        if app.editor().is_some() {
            app.record_edit_position();
            assert!(app.last_edit_position.is_some());
            let (tab, _line, _col) = app.last_edit_position.unwrap();
            assert_eq!(tab, app.active_tab);
        }
    }

    // ── global search collapse ───────────────────────────────────────

    #[test]
    fn search_navigation_clamps_selection_after_results_are_replaced_or_cleared() {
        for sidebar in [false, true] {
            let mut app = App::test_new();
            app.show_explorer = sidebar;
            app.sidebar_view = SidebarView::Search;
            app.global_search_results = (0..3).map(sample_global_search_result).collect();
            app.global_search_selected = usize::MAX;
            app.global_search_select_prev_visible();
            assert_eq!(app.global_search_selected, 2);
            app.global_search_selected = usize::MAX;
            app.global_search_select_next_visible();
            assert_eq!(app.global_search_selected, 2);
            app.global_search_results.clear();
            app.global_search_select_prev_visible();
            assert_eq!(app.global_search_selected, 0);
        }
    }

    #[test]
    fn all_global_search_files_collapsed_false_when_empty() {
        let app = App::test_new();
        // No results → not "all collapsed"
        assert!(!app.all_global_search_files_collapsed());
    }

    #[test]
    fn toggle_global_search_all_files_collapsed_collapses_all() {
        let mut app = App::test_new();
        app.global_search_results = vec![
            sample_global_search_result(0),
            sample_global_search_result(1),
        ].into();
        app.show_explorer = true;
        app.sidebar_view = SidebarView::Search;

        assert!(!app.all_global_search_files_collapsed());
        app.toggle_global_search_all_files_collapsed();
        assert!(app.all_global_search_files_collapsed());
    }

    #[test]
    fn toggle_global_search_all_files_collapsed_expands_when_all_collapsed() {
        let mut app = App::test_new();
        app.global_search_results = vec![
            sample_global_search_result(0),
            sample_global_search_result(1),
        ].into();
        // Collapse all first
        app.global_search_results.set_all_collapsed(true);
        assert!(app.all_global_search_files_collapsed());
        app.toggle_global_search_all_files_collapsed();
        assert!(!app.all_global_search_files_collapsed());
    }

    #[test]
    fn global_search_all_files_toggle_label_shows_correct_symbol() {
        let mut app = App::test_new();
        app.global_search_results = vec![sample_global_search_result(0)].into();
        assert_eq!(app.global_search_all_files_toggle_label(), "⊟");
        app.global_search_results.set_collapsed(Path::new("file-0.txt"), true);
        assert_eq!(app.global_search_all_files_toggle_label(), "⊞");
    }

    #[test]
    fn toggle_global_search_file_collapsed_toggles_single_file() {
        let mut app = App::test_new();
        let path = PathBuf::from("test.rs");
        assert!(!app.is_global_search_file_collapsed(&path));
        app.toggle_global_search_file_collapsed(&path);
        assert!(app.is_global_search_file_collapsed(&path));
        app.toggle_global_search_file_collapsed(&path);
        assert!(!app.is_global_search_file_collapsed(&path));
    }

    // ── set_theme_by_key / next_theme ────────────────────────────────

    #[test]
    fn startup_theme_arrival_uses_workspace_choice_and_overrides() {
        let portable_base = tempfile::tempdir().unwrap();
        let extension = portable_base.path().join(".vtcode/theme-extensions/publisher.theme");
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::write(extension.join("package.json"), serde_json::json!({
            "publisher":"publisher", "name":"theme", "contributes":{"themes":[
                {"label":"Startup Light","uiTheme":"vs","path":"light.json"},
                {"label":"Other variant","path":"missing.json"}
            ]}
        }).to_string()).unwrap();
        std::fs::write(extension.join("light.json"), r##"{"colors":{"editor.background":"#ffffff","editor.foreground":"#010203"}}"##).unwrap();
        let mut app = App::test_new();
        app.settings_global.theme = Some("dark".into());
        app.settings_workspace.theme = Some("vscode-publisher-theme-startup-light".into());
        app.settings_workspace.workbench_color_customizations.insert("editor.background".into(), serde_json::json!("#efeee0"));
        app.apply_effective_settings();
        let theme = crate::app_theme_loader::load_installed_theme(portable_base.path(), "vscode-publisher-theme-startup-light").unwrap();
        app.install_external_themes(vec![theme]);
        assert_eq!(app.theme_key, "vscode-publisher-theme-startup-light");
        let background = ratatui::style::Color::Rgb(239,238,224);
        assert_eq!(app.colors.bg_editor, background);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80,24)).unwrap();
        terminal.draw(|frame| crate::ui_welcome::draw_startup_screen(frame, frame.area(), &app.colors)).unwrap();
        assert_eq!(terminal.backend().buffer().cell((0,0)).unwrap().bg, background);

        // The catalog arrives later with the original (un-customized) theme.
        // It must neither replace the applied overrides nor reset highlighting.
        let loaded = app.loaded_themes.iter().find(|t| t.config_key == app.theme_key).unwrap().clone();
        let (_, before) = app.highlighter.shared_sets();
        assert!(!app.install_external_themes(vec![loaded]));
        let (_, after) = app.highlighter.shared_sets();
        assert!(std::sync::Arc::ptr_eq(&before, &after));
        assert_eq!(app.colors.bg_editor, background);
    }

    #[test]
    fn missing_startup_theme_keeps_a_stable_builtin_fallback() {
        let portable_base = tempfile::tempdir().unwrap();
        let root = portable_base.path().join(".vtcode/theme-extensions");
        let extension = root.join("publisher.theme");
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::write(extension.join("package.json"), serde_json::json!({
            "publisher":"publisher", "name":"theme", "contributes":{"themes":[
                {"label":"Removed","path":"removed.json"},
                {"label":"Remaining","path":"remaining.json"}
            ]}
        }).to_string()).unwrap();
        let content = r##"{"colors":{"editor.background":"#ffffff"}}"##;
        std::fs::write(extension.join("removed.json"), content).unwrap();
        std::fs::write(extension.join("remaining.json"), content).unwrap();
        std::fs::remove_file(extension.join("removed.json")).unwrap();
        let mut app = App::test_new();
        app.settings_workspace.theme = Some("vscode-publisher-theme-removed".into());
        app.apply_effective_settings();
        assert!(crate::app_theme_loader::load_installed_theme(portable_base.path(), "vscode-publisher-theme-removed").is_none());
        assert_eq!(app.theme_key, "dark");
        assert!(!app.install_external_themes(Vec::new()));
        assert!(!app.install_external_themes(crate::vscode_theme::load_extension_themes(&root)));
        assert_eq!(app.theme_key, "dark");
        assert!(app.loaded_themes.iter().any(|theme| theme.config_key == "vscode-publisher-theme-remaining"));
        assert!(!app.loaded_themes.iter().any(|theme| theme.config_key == "vscode-publisher-theme-removed"));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    }

    #[test]
    fn theme_customizations_override_only_the_selected_surface() {
        let mut app = App::test_new();
        let mut settings = app.settings_effective.clone();
        settings.workbench_color_customizations.insert("tab.activeForeground".into(), serde_json::json!("#123456"));
        settings.workbench_color_customizations.insert("[Dark+]".into(), serde_json::json!({"editorLineNumber.foreground":"#654321"}));
        app.apply_settings(&settings);
        assert_eq!(app.colors.ui.fg_tab_active, ratatui::style::Color::Rgb(18,52,86));
        assert_eq!(app.colors.ui.fg_line_number, ratatui::style::Color::Rgb(101,67,33));
        assert!(app.set_theme_by_key("light"));
        assert_eq!(app.colors.ui.fg_tab_active, ratatui::style::Color::Rgb(18,52,86));
        assert_ne!(app.colors.ui.fg_line_number, ratatui::style::Color::Rgb(101,67,33));
    }

    #[test]
    fn set_theme_by_key_returns_false_for_unknown_key() {
        let mut app = App::test_new();
        let result = app.set_theme_by_key("nonexistent_theme_key");
        assert!(!result);
    }

    #[test]
    fn set_theme_by_key_applies_known_theme() {
        let mut app = App::test_new();
        if let Some(tf) = app.loaded_themes.first() {
            let key = tf.config_key.clone();
            let name = tf.name.clone();
            let result = app.set_theme_by_key(&key);
            assert!(result);
            assert_eq!(app.theme_key, key);
            assert_eq!(app.theme_name, name);
        }
    }

    #[test]
    fn next_theme_cycles_through_themes() {
        let mut app = App::test_new();
        if app.loaded_themes.len() >= 2 {
            let first_key = app.loaded_themes[0].config_key.clone();
            let second_key = app.loaded_themes[1].config_key.clone();
            app.set_theme_by_key(&first_key);
            app.next_theme();
            assert_eq!(app.theme_key, second_key);
        }
    }

    #[test]
    fn next_theme_wraps_around() {
        let mut app = App::test_new();
        if !app.loaded_themes.is_empty() {
            let last_key = app.loaded_themes.last().unwrap().config_key.clone();
            let first_key = app.loaded_themes[0].config_key.clone();
            app.set_theme_by_key(&last_key);
            app.next_theme();
            assert_eq!(app.theme_key, first_key);
        }
    }

    // ── should_render_diff_for_active_editor ─────────────────────────

    #[test]
    fn should_render_diff_false_when_diff_mode_off() {
        let app = App::test_new();
        assert!(!app.should_render_diff_for_active_editor());
    }

    #[test]
    fn should_render_diff_false_when_no_base_content() {
        let mut app = App::test_new();
        app.diff_mode = DiffMode::SideBySide;
        app.compare_base_content = None;
        assert!(!app.should_render_diff_for_active_editor());
    }

    #[test]
    fn should_render_diff_true_when_conditions_met() {
        let mut app = App::test_new();
        app.diff_mode = DiffMode::SideBySide;
        app.compare_base_content = Some("base".to_string().into());
        app.diff_target_path = None; // None means "any editor matches"
        assert!(app.should_render_diff_for_active_editor());
    }

    #[test]
    fn binary_files_do_not_enter_path_or_tab_diff_views() {
        for binary_base in [false, true] {
            for already_open in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let base = root.path().join("base.dat");
                let target = root.path().join("target.dat");
                let base_content: &[u8] = if binary_base { b"base\0data" } else { b"base text\n" };
                let target_content: &[u8] = if binary_base { b"target text\n" } else { b"target\0data" };
                std::fs::write(&base, base_content).unwrap();
                std::fs::write(&target, target_content).unwrap();
                let mut app = App::new(root.path().to_path_buf());
                if already_open {
                    app.open_file(&base);
                    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                        app.editor().is_some_and(|editor| !editor.bg_loading)
                    });
                    app.open_file(&target);
                    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                        app.editor().is_some_and(|editor| !editor.bg_loading)
                    });
                    assert!(!app.begin_tab_compare(0));
                    assert!(app.diff_mode == DiffMode::Off);
                }
                app.begin_path_compare(base, target);
                app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                    app.editor().is_some_and(|editor| !editor.bg_loading)
                        && !app.bg_diff_refresh_running
                });
                assert!(!app.should_render_diff_for_active_editor());
                assert!(app.diff_mode == DiffMode::Off);
                assert!(app.compare_diff_lines.is_empty());
                assert!(app.notification_history.iter().any(|notice| {
                    notice.0 == BINARY_DIFF_MESSAGE
                }));
            }
        }
    }

    #[test]
    fn binary_reload_clears_an_existing_text_diff() {
        for reload_base in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let base = root.path().join("base.txt");
            let target = root.path().join("target.txt");
            std::fs::write(&base, "before\n").unwrap();
            std::fs::write(&target, "after\n").unwrap();
            let mut app = App::new(root.path().to_path_buf());
            app.open_file(&base);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.editor().is_some_and(|editor| !editor.bg_loading)
            });
            app.begin_path_compare(base.clone(), target.clone());
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.compare_diff_target_version.is_some()
            });
            assert!(app.should_render_diff_for_active_editor());
            let changed = if reload_base { base } else { target };
            std::fs::write(&changed, b"now\0binary").unwrap();
            app.queue_file_reload(changed);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.diff_mode == DiffMode::Off
            });
            assert!(!app.should_render_diff_for_active_editor());
            assert!(app.compare_diff_lines.is_empty());
        }
    }

    #[test]
    fn new_file_and_tab_diffs_use_last_selected_layout() {
        let dir = tempfile::tempdir().unwrap();
        let base_path = dir.path().join("base.txt");
        let target_path = dir.path().join("target.txt");
        std::fs::write(&base_path, "before\n").unwrap();
        std::fs::write(&target_path, "after\n").unwrap();
        let mut app = App::test_new();
        for (path, content) in [(&base_path, "before\n"), (&target_path, "after\n")] {
            let mut editor = crate::editor::Editor::new();
            editor.file_path = Some(path.clone());
            editor.rope = ropey::Rope::from_str(content);
            app.tabs.push(crate::app::Tab::new_file(editor));
        }
        app.active_tab = 1;
        assert!(app.begin_tab_compare(0));
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_diff_refresh_running
        });
        assert!(app.diff_mode == DiffMode::SideBySide);

        for mode in [DiffMode::Inline, DiffMode::SideBySide] {
            app.execute_palette_command("toggle_diff_layout");
            app.clear_diff_state();
            app.begin_path_compare(base_path.clone(), target_path.clone());
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                !app.bg_diff_refresh_running
            });
            assert!(app.should_render_diff_for_active_editor());
            assert!(app.diff_mode == mode);

            app.clear_diff_state();
            assert!(app.begin_tab_compare(0));
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                !app.bg_diff_refresh_running
            });
            assert!(app.should_render_diff_for_active_editor());
            assert!(app.diff_mode == mode);
        }
    }

    #[test]
    fn refresh_compare_diff_reuses_cache_while_content_is_unchanged() {
        let mut app = App::test_new();
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str("after\n");
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.active_tab = 0;
        app.diff_mode = DiffMode::SideBySide;
        app.compare_base_content = Some("before\n".to_string().into());
        app.set_compare_diff_lines(compute_file_diff("before\n", "after\n"));
        let cache = std::sync::Arc::clone(&app.diff_render_cache);

        app.refresh_compare_diff();

        assert!(std::sync::Arc::ptr_eq(&cache, &app.diff_render_cache));
    }

    #[test]
    fn refresh_compare_diff_rebuilds_cache_after_target_edit() {
        let mut app = App::test_new();
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str("after\n");
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.active_tab = 0;
        app.diff_mode = DiffMode::SideBySide;
        app.compare_base_content = Some("before\n".to_string().into());
        app.set_compare_diff_lines(compute_file_diff("before\n", "after\n"));
        let cache = std::sync::Arc::clone(&app.diff_render_cache);

        let editor = app.editor_mut().expect("active editor");
        editor.rope = ropey::Rope::from_str("new target\n");
        editor.content_version += 1;
        app.refresh_compare_diff();
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            !std::sync::Arc::ptr_eq(&cache, &app.diff_render_cache)
        });

        assert!(!std::sync::Arc::ptr_eq(&cache, &app.diff_render_cache));
        assert_eq!(
            app.compare_diff_lines,
            compute_file_diff("before\n", "new target\n")
        );
    }

    // ── notify / notify_warn / notify_error ──────────────────────────

    #[test]
    fn notify_adds_info_notification() {
        let mut app = App::test_new();
        app.notify("test message");
        assert_eq!(app.notification_history.len(), 1);
        assert_eq!(app.notification_history[0].0, "test message");
    }

    #[test]
    fn notify_warn_adds_warning_level() {
        let mut app = App::test_new();
        app.notify_warn("warning msg");
        assert_eq!(app.notification_history.len(), 1);
        assert!(matches!(
            app.notification_history[0].1,
            NotifyLevel::Warning
        ));
    }

    #[test]
    fn notify_error_adds_error_level() {
        let mut app = App::test_new();
        app.notify_error("error msg");
        assert!(matches!(app.notification_history[0].1, NotifyLevel::Error));
    }

    #[test]
    fn notifications_preserve_messages_for_the_panel() {
        let mut app = App::test_new();
        for i in 0..8 {
            app.notify(format!("msg {i}"));
        }
        assert_eq!(app.notification_history.len(), 8);
        assert_eq!(app.notification_history.last().unwrap().0, "msg 7");
    }

    #[test]
    fn notify_caps_history_at_200() {
        let mut app = App::test_new();
        for i in 0..205 {
            app.notify(format!("msg {i}"));
        }
        assert_eq!(app.notification_history.len(), 200);
    }

    // ── clear_diff_state ─────────────────────────────────────────────

    #[test]
    fn clear_diff_state_resets_diff_fields() {
        let mut app = App::test_new();
        app.diff_mode = DiffMode::Inline;
        app.preferred_diff_mode = DiffMode::Inline;
        app.diff_tab_label = Some("diff-label".to_string());
        app.diff_labels = Some(crate::app::DiffLabels::new("base", "target"));
        app.selected_compare_path = Some(PathBuf::from("selected.rs"));
        app.compare_base_path = Some(PathBuf::from("base.rs"));
        app.diff_target_path = Some(PathBuf::from("target.rs"));
        app.compare_base_content = Some("base content".to_string().into());
        app.set_compare_diff_lines(vec![(' ', "line".to_string())]);

        app.clear_diff_state();

        assert!(matches!(app.diff_mode, DiffMode::Off));
        assert_eq!(app.diff_tab_label, None);
        assert_eq!(app.diff_labels, None);
        assert_eq!(app.diff_target_path, None);
        assert_eq!(app.compare_base_path, None);
        assert_eq!(app.selected_compare_path, Some(PathBuf::from("selected.rs")));
        assert_eq!(app.compare_base_content, None);
        assert!(app.compare_diff_lines.is_empty());
        assert!(app.diff_render_cache.rows.is_empty());
        assert_eq!(app.compare_diff_target_version, None);
        assert_eq!(app.compare_diff_base_revision, None);
        assert!(app.preferred_diff_mode == DiffMode::Inline);
    }
}
