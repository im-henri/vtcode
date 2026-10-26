use crate::app::{App, PaletteCommand, TabKind};

impl App {
    /// Dispatch view / layout / display-toggle commands.
    pub(crate) fn execute_view_command(&mut self, id: &str) -> bool {
        match id {
            "cancel_paste" => self.cancel_pending_paste(),
            "file_details" => crate::ui_status::open_details(self),
            "customize_status_bar" => crate::ui_status::open_customization(self, None),
            "performance" => {
                crate::ui_performance_details::show_or_focus(self);
            }
            "toggle_maximized" => {
                let is_maximized = !self.show_explorer && !self.show_panel;
                if is_maximized {
                    self.show_explorer = true;
                    self.show_panel = true;
                    self.notify("Editor restored");
                } else {
                    self.show_explorer = false;
                    crate::ui_panels::hide_for_layout(self);
                    self.notify("Editor maximized");
                }
            }
            "toggle_word_count" => {
                self.show_word_count = !self.show_word_count;
                self.set_status_bar_item_pinned("word_count", self.show_word_count);
                self.notify(if self.show_word_count {
                    "Word count: ON"
                } else {
                    "Word count: OFF"
                });
            }
            "toggle_indent_rainbow" => {
                self.indent_rainbow = !self.indent_rainbow;
                self.notify(if self.indent_rainbow {
                    "Indent rainbow: ON"
                } else {
                    "Indent rainbow: OFF"
                });
            }
            "toggle_color_preview" => {
                self.show_color_preview = !self.show_color_preview;
                self.notify(if self.show_color_preview {
                    "Color preview: ON"
                } else {
                    "Color preview: OFF"
                });
            }
            "toggle_bracket_pair_colorization" => {
                self.bracket_pair_colorization = !self.bracket_pair_colorization;
                self.notify(if self.bracket_pair_colorization {
                    "Bracket pair colorization: ON"
                } else {
                    "Bracket pair colorization: OFF"
                });
            }
            "toggle_error_lens" => {
                self.error_lens = !self.error_lens;
                self.notify(if self.error_lens {
                    "Error Lens: ON — TODO/FIXME/HACK/BUG/NOTE and language-server diagnostics"
                } else {
                    "Error Lens: OFF"
                });
            }
            "toggle_inlay_hints" => {
                self.show_inlay_hints = !self.show_inlay_hints;
                if !self.show_inlay_hints {
                    self.lsp_inlay_hints.clear();
                }
                self.lsp_inlay_hint_last_scroll = usize::MAX;
                self.notify(if self.show_inlay_hints {
                    "Inlay Hints: ON"
                } else {
                    "Inlay Hints: OFF"
                });
            }
            "toggle_code_lens" => {
                self.show_code_lenses = !self.show_code_lenses;
                if !self.show_code_lenses {
                    self.lsp_code_lenses.clear();
                }
                self.lsp_code_lens_last_version = u64::MAX;
                self.notify(if self.show_code_lenses {
                    "Code Lens: ON"
                } else {
                    "Code Lens: OFF"
                });
            }
            "toggle_on_type_formatting" => {
                self.on_type_formatting_enabled = !self.on_type_formatting_enabled;
                self.notify(if self.on_type_formatting_enabled {
                    "On-Type Formatting: ON"
                } else {
                    "On-Type Formatting: OFF"
                });
            }
            "toggle_semantic_tokens" => {
                self.show_semantic_tokens = !self.show_semantic_tokens;
                if !self.show_semantic_tokens {
                    self.lsp_semantic_tokens.clear();
                }
                self.lsp_semantic_tokens_last_version = u64::MAX;
                self.notify(if self.show_semantic_tokens {
                    "Semantic Highlighting: ON"
                } else {
                    "Semantic Highlighting: OFF"
                });
            }
            "toggle_sticky_scroll" => {
                self.sticky_scroll = !self.sticky_scroll;
                self.notify(if self.sticky_scroll {
                    "Sticky Scroll: ON"
                } else {
                    "Sticky Scroll: OFF"
                });
            }
            "toggle_screencast_mode" => {
                self.screencast_mode = !self.screencast_mode;
                if !self.screencast_mode {
                    self.screencast_keys.clear();
                }
                self.notify(if self.screencast_mode {
                    "Screencast Mode: ON"
                } else {
                    "Screencast Mode: OFF"
                });
            }
            "toggle_explorer" | "toggle_sidebar" => self.toggle_explorer(),
            "toggle_minimap" => {
                self.show_minimap = !self.show_minimap;
                self.notify(if self.show_minimap {
                    "Minimap: On"
                } else {
                    "Minimap: Off"
                });
            }
            "ruler_80" => {
                if let Some(pos) = self.rulers.iter().position(|&r| r == 80) {
                    self.rulers.remove(pos);
                    self.notify("Ruler at 80 removed");
                } else {
                    self.rulers.push(80);
                    self.rulers.sort_unstable();
                    self.notify("Ruler at 80 added");
                }
            }
            "ruler_120" => {
                if let Some(pos) = self.rulers.iter().position(|&r| r == 120) {
                    self.rulers.remove(pos);
                    self.notify("Ruler at 120 removed");
                } else {
                    self.rulers.push(120);
                    self.rulers.sort_unstable();
                    self.notify("Ruler at 120 added");
                }
            }
            "ruler_clear" => {
                self.rulers.clear();
                self.notify("All rulers cleared");
            }
            "center_screen" => {
                if let Some(ed) = self.editor_mut() {
                    ed.center_cursor();
                }
            }
            "toggle_panel" => {
                self.show_panel = !self.show_panel;
                if !self.show_panel {
                    self.terminal_panel_focused = false;
                    crate::ui_panels::close_panel(self);
                }
                self.notify(if self.show_panel {
                    "Panel: On"
                } else {
                    "Panel: Off"
                });
            }
            "show_notifications" => {
                self.show_panel = true;
                crate::ui_panels::select_tab(self, 3);
            }
            "toggle_problems" => crate::ui_panels::toggle_problems(self),
            "toggle_line_numbers" => {
                self.show_line_numbers = !self.show_line_numbers;
                self.notify(if self.show_line_numbers {
                    "Line Numbers: On"
                } else {
                    "Line Numbers: Off"
                });
            }
            "toggle_indent_guides" => {
                self.show_indent_guides = !self.show_indent_guides;
                self.notify(if self.show_indent_guides {
                    "Indent Guides: On"
                } else {
                    "Indent Guides: Off"
                });
            }
            "toggle_breadcrumbs" => {
                self.show_breadcrumbs = !self.show_breadcrumbs;
                self.notify(if self.show_breadcrumbs {
                    "Breadcrumbs: On"
                } else {
                    "Breadcrumbs: Off"
                });
            }
            "toggle_activity_bar" => {
                let visible = if self.zen_mode {
                    let mut state = self
                        .zen_pre_state
                        .unwrap_or((true, true, false, true, true, true));
                    state.5 = !state.5;
                    let visible = state.5;
                    self.zen_pre_state = Some(state);
                    self.show_activity_bar = false;
                    visible
                } else {
                    self.show_activity_bar = !self.show_activity_bar;
                    self.show_activity_bar
                };
                self.notify(if visible {
                    "Activity Bar: On"
                } else {
                    "Activity Bar: Off"
                });
            }
            "toggle_status_bar" => {
                self.show_status_bar = !self.show_status_bar;
                self.notify(if self.show_status_bar {
                    "Status Bar: On"
                } else {
                    "Status Bar: Off"
                });
            }
            "toggle_tab_bar" => {
                self.show_tab_bar = !self.show_tab_bar;
                self.notify(if self.show_tab_bar {
                    "Tab Bar: On"
                } else {
                    "Tab Bar: Off"
                });
            }
            "toggle_menu_bar" => {
                self.show_menu_bar = !self.show_menu_bar;
                self.notify(if self.show_menu_bar {
                    "Menu Bar: On"
                } else {
                    "Menu Bar: Off"
                });
            }
            "toggle_relative_lines" => {
                self.relative_line_numbers = !self.relative_line_numbers;
                self.notify(if self.relative_line_numbers {
                    "Relative Line Numbers: On"
                } else {
                    "Line Numbers: Absolute"
                });
            }
            "toggle_word_wrap" => {
                if let Some(ed) = self.editor_mut() {
                    let wrap_width = ed.effective_wrap_width();
                    if ed.word_wrap {
                        let cursor_wrap_row =
                            ed.wrapped_row_for_position(ed.cursor_line, ed.cursor_col, wrap_width);
                        let cursor_screen_row = cursor_wrap_row
                            .saturating_sub(ed.wrap_scroll_offset)
                            .min(ed.viewport_height.saturating_sub(1));
                        ed.word_wrap = false;
                        ed.scroll_offset = ed.cursor_line.saturating_sub(cursor_screen_row);
                    } else {
                        let cursor_screen_row = ed
                            .cursor_line
                            .saturating_sub(ed.scroll_offset)
                            .min(ed.viewport_height.saturating_sub(1));
                        ed.word_wrap = true;
                        ed.horizontal_scroll = 0;
                        let cursor_wrap_row =
                            ed.wrapped_row_for_position(ed.cursor_line, ed.cursor_col, wrap_width);
                        ed.set_wrap_scroll_offset(
                            cursor_wrap_row.saturating_sub(cursor_screen_row),
                        );
                    }
                    self.default_word_wrap = ed.word_wrap;
                }
                let msg = self.editor().map_or("", |e| {
                    if e.word_wrap {
                        "Word Wrap: On"
                    } else {
                        "Word Wrap: Off"
                    }
                });
                self.notify(msg);
            }
            "toggle_whitespace" => {
                let current_mode = self
                    .tabs
                    .get(self.active_tab)
                    .filter(|t| t.kind == TabKind::File)
                    .map(|t| t.editor.whitespace_mode)
                    .or_else(|| {
                        self.tabs
                            .iter()
                            .find(|t| t.kind == TabKind::File)
                            .map(|t| t.editor.whitespace_mode)
                    })
                    .or_else(|| {
                        self.groups
                            .iter()
                            .flat_map(|g| g.tabs.iter())
                            .find(|t| t.kind == TabKind::File)
                            .map(|t| t.editor.whitespace_mode)
                    })
                    .unwrap_or(crate::app::WhitespaceMode::None);
                let next_mode = current_mode.next();
                self.default_whitespace_mode = next_mode;
                for tab in &mut self.tabs {
                    if tab.kind == TabKind::File {
                        tab.editor.whitespace_mode = next_mode;
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        if tab.kind == TabKind::File {
                            tab.editor.whitespace_mode = next_mode;
                        }
                    }
                }
                self.notify(format!("Render Whitespace: {}", next_mode.name()));
            }
            "toggle_cursor_style" => {
                use crate::app::CursorStyle;
                self.cursor_style = match self.cursor_style {
                    CursorStyle::Line => CursorStyle::Block,
                    CursorStyle::Block => CursorStyle::Underline,
                    CursorStyle::Underline => CursorStyle::Line,
                };
                let name = match self.cursor_style {
                    CursorStyle::Line => "Line",
                    CursorStyle::Block => "Block",
                    CursorStyle::Underline => "Underline",
                };
                self.notify(format!("Cursor Style: {name}"));
            }
            "toggle_cursor_blinking" => {
                self.cursor_blinking = !self.cursor_blinking;
                self.notify(if self.cursor_blinking {
                    "Cursor Blinking: On"
                } else {
                    "Cursor Blinking: Off"
                });
            }
            "toggle_focus_mode" => {
                self.focus_mode = !self.focus_mode;
                self.notify(if self.focus_mode {
                    "Focus mode: ON"
                } else {
                    "Focus mode: OFF"
                });
            }
            "toggle_sidebar_position" => {
                self.sidebar_right = !self.sidebar_right;
                self.notify(if self.sidebar_right {
                    "Sidebar: Right"
                } else {
                    "Sidebar: Left"
                });
            }
            "toggle_zen" => {
                self.zen_mode = !self.zen_mode;
                self.zen_exit_area = None;
                if self.zen_mode {
                    if self.active_pane == crate::app::ActivePane::Chat { self.active_pane = crate::app::ActivePane::Editor; }
                    self.zen_pre_state = Some((
                        self.show_explorer,
                        self.show_minimap,
                        self.show_panel,
                        self.show_tab_bar,
                        self.show_status_bar,
                        self.show_activity_bar,
                    ));
                    self.show_explorer = false;
                    self.show_minimap = false;
                    crate::ui_panels::hide_for_layout(self);
                    self.show_tab_bar = false;
                    self.show_status_bar = false;
                    self.show_activity_bar = false;
                    self.notify("Zen Mode: On");
                } else {
                    if let Some((exp, mini, panel, tabs, status, activity)) =
                        self.zen_pre_state.take()
                    {
                        self.show_explorer = exp;
                        self.show_minimap = mini;
                        self.show_panel = panel;
                        self.show_tab_bar = tabs;
                        self.show_status_bar = status;
                        self.show_activity_bar = activity;
                    } else {
                        self.show_explorer = true;
                        self.show_minimap = true;
                        self.show_tab_bar = true;
                        self.show_status_bar = true;
                        self.show_activity_bar = true;
                    }
                    self.notify("Zen Mode: Off");
                }
            }
            "toggle_diff_layout" => {
                if !self.should_render_diff_for_active_editor() {
                    return true;
                }
                self.diff_mode = match self.diff_mode {
                    crate::app::DiffMode::SideBySide => crate::app::DiffMode::Inline,
                    crate::app::DiffMode::Inline => crate::app::DiffMode::SideBySide,
                    crate::app::DiffMode::Off => return true,
                };
                self.preferred_diff_mode = self.diff_mode;
                if self.is_commit_details_tab() {
                    self.commit_details.review.forced_mode = Some(self.diff_mode);
                }
                // Keep the cached comparison and editor position while replacing
                // geometry that belongs to the previous layout on the next frame.
                self.diff_split_area = None;
                self.diff_divider_area = None;
                self.diff_left_code_area = None;
                self.diff_left_h_scrollbar_area = None;
                self.resizing_diff_split = false;
                self.dragging_diff_left_h_scrollbar = false;
                self.active_pane = crate::app::ActivePane::Editor;
            }
            "toggle_diff_view" => {
                use crate::app::DiffMode;
                if self.diff_mode == DiffMode::Off {
                    if !self.request_compare_with_git_async() {
                        self.notify("No git version found for this file");
                    } else {
                        self.notify(if self.preferred_diff_mode == DiffMode::Inline {
                            "Preparing inline diff"
                        } else {
                            "Preparing side-by-side diff"
                        });
                    }
                    return true;
                }
                self.diff_mode = match self.diff_mode {
                    DiffMode::Off => unreachable!(),
                    DiffMode::SideBySide => DiffMode::Inline,
                    DiffMode::Inline => DiffMode::Off,
                };
                if self.diff_mode != DiffMode::Off {
                    self.preferred_diff_mode = self.diff_mode;
                }
                match self.diff_mode {
                    DiffMode::Off => {
                        self.clear_diff_state();
                        self.notify("Diff view disabled");
                    }
                    DiffMode::SideBySide => {
                        self.refresh_git_line_status_async();
                        self.notify("Diff: side-by-side");
                    }
                    DiffMode::Inline => {
                        self.refresh_git_line_status_async();
                        self.scroll_to_first_diff_change();
                        self.notify("Diff: inline");
                    }
                }
            }
            "next_diff_hunk" => {
                if let Some(editor) = self.editor() {
                    let cur = editor.cursor_line;
                    let found = if self.should_render_diff_for_active_editor()
                        && !self.diff_render_cache.hunks.is_empty()
                    {
                        self.diff_render_cache.next_hunk_target(cur)
                    } else {
                        let total = editor.rope.len_lines();
                        let status = editor.git_line_status();
                        let mut found = None;
                        if !status.is_empty() {
                            let mut i = cur + 1;
                            let cur_s = status.get(cur).copied().unwrap_or(' ');
                            while i < total
                                && status.get(i).copied().unwrap_or(' ') == cur_s
                                && cur_s != ' '
                            {
                                i += 1;
                            }
                            while i < total && status.get(i).copied().unwrap_or(' ') == ' ' {
                                i += 1;
                            }
                            if i < total {
                                found = Some(i);
                            }
                        }
                        found
                    };
                    if let Some(line) = found {
                        if let Some(editor) = self.editor_mut() {
                            editor.clear_selection();
                            editor.set_cursor(line, 0);
                        }
                    } else {
                        self.notify("No more changes below");
                    }
                }
            }
            "prev_diff_hunk" => {
                if let Some(editor) = self.editor() {
                    let cur = editor.cursor_line;
                    let found = if self.should_render_diff_for_active_editor()
                        && !self.diff_render_cache.hunks.is_empty()
                    {
                        self.diff_render_cache.previous_hunk_target(cur)
                    } else {
                        let status = editor.git_line_status();
                        let mut found = None;
                        if !status.is_empty() && cur > 0 {
                            let mut i = cur.saturating_sub(1);
                            let cur_s = status.get(cur).copied().unwrap_or(' ');
                            while i > 0
                                && status.get(i).copied().unwrap_or(' ') == cur_s
                                && cur_s != ' '
                            {
                                i -= 1;
                            }
                            while i > 0 && status.get(i).copied().unwrap_or(' ') == ' ' {
                                i -= 1;
                            }
                            if status.get(i).copied().unwrap_or(' ') != ' ' {
                                while i > 0 && status.get(i - 1).copied().unwrap_or(' ') != ' ' {
                                    i -= 1;
                                }
                                found = Some(i);
                            }
                        }
                        found
                    };
                    if let Some(line) = found {
                        if let Some(editor) = self.editor_mut() {
                            editor.clear_selection();
                            editor.set_cursor(line, 0);
                        }
                    } else {
                        self.notify("No more changes above");
                    }
                }
            }
            "compare_with_tab" => {
                if self.tabs.len() < 2 {
                    self.notify("Need at least 2 open tabs to compare");
                } else {
                    self.palette.active = true;
                    self.palette.query.clear();
                    self.palette.cursor = 0;
                    self.palette.selection_anchor = None;
                    self.palette.selected = 0;
                    self.palette.scroll_offset = 0;
                    self.palette.commands = self
                        .tabs
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != self.active_tab)
                        .map(|(i, tab)| {
                            let name = tab.editor.file_name().to_string();
                            PaletteCommand {
                                name: format!("Compare with: {name}"),
                                shortcut: String::new(),
                                id: std::borrow::Cow::Owned(format!("diff_compare_{i}")),
                            }
                        })
                        .collect();
                }
            }
            "compare_with_git" => {
                if !self.request_compare_with_git_async() {
                    self.notify("No git version found for this file");
                } else {
                    self.notify("Preparing Git diff");
                }
            }
            "explorer_sort_name" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Name;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Name");
            }
            "explorer_sort_size" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Size;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Size");
            }
            "explorer_sort_modified" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Modified;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Date Modified");
            }
            "explorer_sort_type" => {
                self.explorer.sort_mode = crate::explorer::ExplorerSortMode::Type;
                self.refresh_explorer_tree_async();
                self.notify("Explorer: Sort by Type");
            }
            "markdown_preview" => {
                self.toggle_markdown_preview_tab();
            }
            "split_next_tab" | "split_prev_tab" => {
                let panes=self.desk_panes();
                if panes.len()>1 {
                    let current=panes.iter().position(|&i|i==self.active_group).unwrap_or(0);
                    let next=if id=="split_next_tab" {(current+1)%panes.len()}else{(current+panes.len()-1)%panes.len()};
                    self.switch_group(panes[next]);
                }
            }
            "split_editor" => {
                if !self.tabs.is_empty() {
                    self.create_split_group(self.active_tab);
                    self.notify("Split editor opened");
                }
            }
            "split_editor_right" => {
                if !self.tabs.is_empty() {
                    self.create_split_group_dir(
                        self.active_tab,
                        crate::app::SplitDirection::Horizontal,
                    );
                    self.notify("Split editor right");
                }
            }
            "split_editor_down" => {
                if !self.tabs.is_empty() {
                    self.create_split_group_dir(
                        self.active_tab,
                        crate::app::SplitDirection::Vertical,
                    );
                    self.notify("Split editor down");
                }
            }
            "close_split" => {
                self.close_all_splits();
                self.notify("Split editor closed");
            }
            "close_group_tabs" => {
                self.close_tabs_matching(|_| true);
            }
            _ => return false,
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: execute a view command via the full palette dispatcher.
    fn exec(app: &mut App, id: &str) -> bool {
        app.execute_palette_command(id)
    }

    // ---------- simple boolean toggles ----------

    #[test]
    fn toggle_explorer() {
        let mut app = App::test_new();
        assert!(app.show_explorer);
        exec(&mut app, "toggle_explorer");
        assert!(!app.show_explorer);
        exec(&mut app, "toggle_explorer");
        assert!(app.show_explorer);
    }

    #[test]
    fn toggle_sidebar_alias() {
        let mut app = App::test_new();
        assert!(app.show_explorer);
        exec(&mut app, "toggle_sidebar");
        assert!(!app.show_explorer);
    }

    #[test]
    fn toggle_minimap() {
        let mut app = App::test_new();
        assert!(app.show_minimap);
        exec(&mut app, "toggle_minimap");
        assert!(!app.show_minimap);
        exec(&mut app, "toggle_minimap");
        assert!(app.show_minimap);
    }

    #[test]
    fn toggle_panel() {
        let mut app = App::test_new();
        assert!(!app.show_panel);
        exec(&mut app, "toggle_panel");
        assert!(app.show_panel);
        exec(&mut app, "toggle_panel");
        assert!(!app.show_panel);
    }

    #[test]
    fn toggle_line_numbers() {
        let mut app = App::test_new();
        assert!(app.show_line_numbers);
        exec(&mut app, "toggle_line_numbers");
        assert!(!app.show_line_numbers);
    }

    #[test]
    fn toggle_indent_guides() {
        let mut app = App::test_new();
        assert!(app.show_indent_guides);
        exec(&mut app, "toggle_indent_guides");
        assert!(!app.show_indent_guides);
    }

    #[test]
    fn toggle_breadcrumbs() {
        let mut app = App::test_new();
        assert!(app.show_breadcrumbs);
        exec(&mut app, "toggle_breadcrumbs");
        assert!(!app.show_breadcrumbs);
    }

    #[test]
    fn toggle_diff_layout_ignores_unrelated_editors() {
        let mut app = App::test_new();
        let mut editor = crate::editor::Editor::new();
        editor.file_path = Some("other.txt".into());
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.diff_mode = crate::app::DiffMode::SideBySide;
        app.compare_base_content = Some("before\n".into());
        app.diff_target_path = Some("compared.txt".into());

        exec(&mut app, "toggle_diff_layout");
        assert!(app.diff_mode == crate::app::DiffMode::SideBySide);
        assert!(app.is_menu_cmd_disabled("toggle_diff_layout"));
        app.clear_diff_state();
        exec(&mut app, "toggle_diff_layout");
        assert!(app.diff_mode == crate::app::DiffMode::Off);
        assert!(!app.bg_git_compare_running);
    }

    #[test]
    fn diff_hunk_commands_use_cached_compare_hunks() {
        let base = "zero\nold one\ntwo\nold three\nfour\n";
        let target = "zero\nnew one\ntwo\nnew three\nfour\n";
        let mut app = App::test_new();
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(target);
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.active_tab = 0;
        app.diff_mode = crate::app::DiffMode::SideBySide;
        app.compare_base_content = Some(base.to_string().into());
        app.set_compare_diff_lines(crate::app::compute_file_diff(base, target));
        app.editor_mut()
            .expect("active editor")
            .clear_git_line_status();

        exec(&mut app, "next_diff_hunk");
        assert_eq!(app.editor().expect("active editor").cursor_line, 1);
        exec(&mut app, "next_diff_hunk");
        assert_eq!(app.editor().expect("active editor").cursor_line, 3);
        exec(&mut app, "prev_diff_hunk");
        assert_eq!(app.editor().expect("active editor").cursor_line, 1);
    }

    #[test]
    fn toggle_activity_bar_changes_visibility() {
        let mut app = App::test_new();
        assert!(app.show_activity_bar);
        exec(&mut app, "toggle_activity_bar");
        assert!(!app.show_activity_bar);
        exec(&mut app, "toggle_activity_bar");
        assert!(app.show_activity_bar);
    }

    #[test]
    fn toggle_status_bar() {
        let mut app = App::test_new();
        assert!(app.show_status_bar);
        exec(&mut app, "toggle_status_bar");
        assert!(!app.show_status_bar);
    }

    #[test]
    fn toggle_tab_bar() {
        let mut app = App::test_new();
        assert!(app.show_tab_bar);
        exec(&mut app, "toggle_tab_bar");
        assert!(!app.show_tab_bar);
    }

    #[test]
    fn toggle_menu_bar() {
        let mut app = App::test_new();
        assert!(app.show_menu_bar);
        exec(&mut app, "toggle_menu_bar");
        assert!(!app.show_menu_bar);
    }

    #[test]
    fn toggle_relative_line_numbers() {
        let mut app = App::test_new();
        assert!(!app.relative_line_numbers);
        exec(&mut app, "toggle_relative_lines");
        assert!(app.relative_line_numbers);
    }

    #[test]
    fn toggle_word_count() {
        let mut app = App::test_new();
        assert!(!app.show_word_count);
        exec(&mut app, "toggle_word_count");
        assert!(app.show_word_count);
    }

    #[test]
    fn toggle_sticky_scroll() {
        let mut app = App::test_new();
        assert!(app.sticky_scroll);
        exec(&mut app, "toggle_sticky_scroll");
        assert!(!app.sticky_scroll);
    }

    #[test]
    fn toggle_cursor_blinking() {
        let mut app = App::test_new();
        assert!(app.cursor_blinking);
        exec(&mut app, "toggle_cursor_blinking");
        assert!(!app.cursor_blinking);
    }

    #[test]
    fn toggle_focus_mode() {
        let mut app = App::test_new();
        assert!(!app.focus_mode);
        exec(&mut app, "toggle_focus_mode");
        assert!(app.focus_mode);
    }

    #[test]
    fn toggle_sidebar_position() {
        let mut app = App::test_new();
        assert!(!app.sidebar_right);
        exec(&mut app, "toggle_sidebar_position");
        assert!(app.sidebar_right);
    }

    #[test]
    fn toggle_screencast_mode() {
        let mut app = App::test_new();
        assert!(!app.screencast_mode);
        exec(&mut app, "toggle_screencast_mode");
        assert!(app.screencast_mode);
    }

    #[test]
    fn toggle_error_lens() {
        let mut app = App::test_new();
        assert!(!app.error_lens);
        exec(&mut app, "toggle_error_lens");
        assert!(app.error_lens);
        assert!(app.notification_history.last().unwrap().0.contains("TODO/FIXME/HACK/BUG/NOTE"));
        exec(&mut app, "toggle_error_lens");
        assert!(!app.error_lens);
    }

    #[test]
    fn toggle_indent_rainbow() {
        let mut app = App::test_new();
        assert!(!app.indent_rainbow);
        exec(&mut app, "toggle_indent_rainbow");
        assert!(app.indent_rainbow);
    }

    #[test]
    fn toggle_color_preview() {
        let mut app = App::test_new();
        assert!(!app.show_color_preview);
        exec(&mut app, "toggle_color_preview");
        assert!(app.show_color_preview);
        exec(&mut app, "toggle_color_preview");
        assert!(!app.show_color_preview);
    }

    #[test]
    fn toggle_bracket_pair_colorization() {
        let mut app = App::test_new();
        assert!(app.bracket_pair_colorization);
        exec(&mut app, "toggle_bracket_pair_colorization");
        assert!(!app.bracket_pair_colorization);
    }

    // ---------- compound / stateful commands ----------

    #[test]
    fn toggle_maximized_hides_and_restores() {
        let mut app = App::test_new();
        // defaults: explorer=true, panel=false → not maximized
        exec(&mut app, "toggle_maximized");
        assert!(!app.show_explorer);
        assert!(!app.show_panel);
        // toggle again → restores both
        exec(&mut app, "toggle_maximized");
        assert!(app.show_explorer);
        assert!(app.show_panel);
    }

    #[test]
    fn toggle_zen_saves_and_restores_state() {
        let mut app = App::test_new();
        assert!(!app.zen_mode);
        let pre_explorer = app.show_explorer;
        let pre_minimap = app.show_minimap;
        app.show_activity_bar = false;

        exec(&mut app, "toggle_zen");
        assert!(app.zen_mode);
        assert!(!app.show_explorer);
        assert!(!app.show_minimap);
        assert!(!app.show_panel);
        assert!(!app.show_tab_bar);
        assert!(!app.show_status_bar);
        assert!(!app.show_activity_bar);

        exec(&mut app, "toggle_zen");
        assert!(!app.zen_mode);
        assert_eq!(app.show_explorer, pre_explorer);
        assert_eq!(app.show_minimap, pre_minimap);
        assert!(!app.show_activity_bar);
    }

    #[test]
    fn toggle_cursor_style_cycles() {
        use crate::app::CursorStyle;
        let mut app = App::test_new();
        assert!(matches!(app.cursor_style, CursorStyle::Line));
        exec(&mut app, "toggle_cursor_style");
        assert!(matches!(app.cursor_style, CursorStyle::Block));
        exec(&mut app, "toggle_cursor_style");
        assert!(matches!(app.cursor_style, CursorStyle::Underline));
        exec(&mut app, "toggle_cursor_style");
        assert!(matches!(app.cursor_style, CursorStyle::Line));
    }

    // ---------- ruler commands ----------

    #[test]
    fn ruler_80_toggles() {
        let mut app = App::test_new();
        let had_80 = app.rulers.contains(&80);
        exec(&mut app, "ruler_80");
        assert_eq!(app.rulers.contains(&80), !had_80);
        exec(&mut app, "ruler_80");
        assert_eq!(app.rulers.contains(&80), had_80);
    }

    #[test]
    fn ruler_120_adds_and_removes() {
        let mut app = App::test_new();
        assert!(!app.rulers.contains(&120));
        exec(&mut app, "ruler_120");
        assert!(app.rulers.contains(&120));
        exec(&mut app, "ruler_120");
        assert!(!app.rulers.contains(&120));
    }

    #[test]
    fn ruler_clear() {
        let mut app = App::test_new();
        exec(&mut app, "ruler_120");
        assert!(!app.rulers.is_empty());
        exec(&mut app, "ruler_clear");
        assert!(app.rulers.is_empty());
    }

    // ---------- unknown command ----------

    #[test]
    fn unknown_command_returns_false() {
        let mut app = App::test_new();
        assert!(!app.execute_view_command("nonexistent_command_xyz"));
    }

    // ---------- LSP-related toggles clear cached data ----------

    #[test]
    fn toggle_inlay_hints_clears_cache() {
        let mut app = App::test_new();
        assert!(app.show_inlay_hints);
        exec(&mut app, "toggle_inlay_hints");
        assert!(!app.show_inlay_hints);
        assert!(app.lsp_inlay_hints.is_empty());
    }

    #[test]
    fn toggle_code_lens_clears_cache() {
        let mut app = App::test_new();
        assert!(app.show_code_lenses);
        exec(&mut app, "toggle_code_lens");
        assert!(!app.show_code_lenses);
        assert!(app.lsp_code_lenses.is_empty());
    }

    #[test]
    fn toggle_semantic_tokens_clears_cache() {
        let mut app = App::test_new();
        assert!(app.show_semantic_tokens);
        exec(&mut app, "toggle_semantic_tokens");
        assert!(!app.show_semantic_tokens);
        assert!(app.lsp_semantic_tokens.is_empty());
    }

    #[test]
    fn toggle_on_type_formatting() {
        let mut app = App::test_new();
        assert!(!app.on_type_formatting_enabled);
        exec(&mut app, "toggle_on_type_formatting");
        assert!(app.on_type_formatting_enabled);
    }

    // ---------- show_notifications ----------

    #[test]
    fn show_notifications_opens_panel() {
        let mut app = App::test_new();
        assert!(!app.show_panel);
        exec(&mut app, "show_notifications");
        assert!(app.show_panel);
        assert_eq!(app.panel_tab, 3);
    }
}
