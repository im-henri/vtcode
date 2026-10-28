//! Settings pages and their shared mouse/keyboard navigation model.

use crate::app::App;

impl App {
    pub(crate) fn settings_editor_has_focus(&self) -> bool {
        self.settings_editor_active
            && self.active_pane == crate::app::ActivePane::Editor
            && !self.docked_terminal_has_focus()
    }

    /// The compact picker exposes the same pages as the expanded tree.
    pub(crate) fn settings_page_options() -> Vec<(usize, Option<&'static str>, String)> {
        let mut pages = Vec::new();
        for (category, &name) in Self::settings_categories().iter().enumerate() {
            pages.push((category, None, name.to_string()));
            pages.extend(
                Self::settings_sections(category)
                    .iter()
                    .map(|&section| (category, Some(section), format!("  {section}"))),
            );
        }
        pages
    }

    pub(crate) fn settings_sections(category: usize) -> &'static [&'static str] {
        match category {
            1 => &[
                "Editing",
                "Cursor",
                "Whitespace",
                "Minimap",
                "Suggestions",
                "VTComplete",
                "Formatting",
            ],
            2 => &["Appearance", "Layout", "Editor Management"],
            3 => &["Code Decorations", "Navigation", "Search", "Focus"],
            4 => &["Saving", "File Associations", "Exclusions", "Large Files"],
            7 => &[
                "Cursor",
                "Editing",
                "Code Structure",
                "Navigation",
                "Shared Frame Rate",
            ],
            _ => &[],
        }
    }

    pub(crate) fn settings_section_for_id(id: &str) -> Option<&'static str> {
        if crate::app_vtcomplete_settings::field(id).is_some() {
            return Some("VTComplete");
        }
        match Self::settings_category_for_id(id) {
            1 => Some(match id {
                "cursor_style" | "cursor_blinking" | "cursor_surrounding_lines" => "Cursor",
                "whitespace" | "comment_whitespace" => "Whitespace",
                "minimap" | "minimap_width" => "Minimap",
                "quick_suggestions" | "hover_enabled" => "Suggestions",
                "format_on_type" | "format_on_save" | "formatters" => "Formatting",
                _ => "Editing",
            }),
            2 => Some(match id {
                "theme" | "line_numbers" | "relative_lines" | "indent_guides" => "Appearance",
                "preview_tabs" | "mouse_back_forward" | "tab_bar" => "Editor Management",
                _ => "Layout",
            }),
            3 => Some(match id {
                "sticky_scroll" | "sticky_scroll_max" | "scroll_past_end" | "scroll_speed" => {
                    "Navigation"
                }
                "focus_mode" | "word_count" | "screencast_mode" => "Focus",
                id if id.starts_with("search_") || id == "max_global_search_results" => "Search",
                _ => "Code Decorations",
            }),
            4 => Some(match id {
                "language_associations" => "File Associations",
                "files_exclude" => "Exclusions",
                "large_file_threshold_mb" | "large_file_analysis_timeout_ms" => "Large Files",
                _ => "Saving",
            }),
            7 => Self::animation_setting_section(id),
            _ => None,
        }
    }

    pub(crate) fn settings_selected_section(&self, category: usize) -> Option<&'static str> {
        self.settings_section_selected
            .filter(|(parent, section)| {
                *parent == category && Self::settings_sections(category).contains(section)
            })
            .map(|(_, section)| section)
    }

    /// Expanded branches stay open when another page is selected.
    pub(crate) fn settings_category_visual_rows(&self) -> Vec<(usize, bool, &'static str)> {
        let mut rows = Vec::new();
        for (category, name) in Self::settings_categories().iter().copied().enumerate() {
            rows.push((category, false, name));
            if !self.settings_category_collapsed[category] {
                rows.extend(
                    Self::settings_sections(category)
                        .iter()
                        .map(|&section| (category, true, section)),
                );
            }
        }
        rows
    }

    pub(crate) fn settings_tree_row_selected(
        &self,
        category: usize,
        child: bool,
        label: &str,
    ) -> bool {
        category == self.settings_category_selected
            && if child {
                self.settings_selected_section(category) == Some(label)
            } else {
                self.settings_selected_section(category).is_none()
                    || self.settings_category_collapsed[category]
            }
    }

    pub(crate) fn reveal_settings_category(&mut self) {
        let Some(area) = self.settings_category_area.filter(|area| area.height > 0) else {
            return;
        };
        let rows = self.settings_category_visual_rows();
        if let Some(index) = rows.iter().position(|&(category, child, label)| {
            self.settings_tree_row_selected(category, child, label)
        }) {
            self.settings_category_scroll = self
                .settings_category_scroll
                .min(index)
                .max(index.saturating_sub(area.height as usize - 1))
                .min(rows.len().saturating_sub(area.height as usize));
        }
    }

    pub(crate) fn select_settings_page(&mut self, category: usize, section: Option<&'static str>) {
        if category >= Self::settings_categories().len() {
            return;
        }
        self.settings_category_selected = category;
        self.settings_section_selected = section
            .filter(|section| Self::settings_sections(category).contains(section))
            .map(|section| (category, section));
        if self.settings_section_selected.is_some() {
            self.settings_category_collapsed[category] = false;
        }
        self.settings_editor_search.clear();
        self.settings_editor_search_cursor = 0;
        self.settings_editor_search_selection_anchor = None;
        self.settings_editor_scroll = 0;
        self.settings_selected = usize::MAX;
        self.ensure_settings_selection_visible();
        self.reveal_settings_category();
    }

    pub(crate) fn move_settings_category(&mut self, down: bool) {
        let rows = self.settings_category_visual_rows();
        let index = rows
            .iter()
            .position(|&(category, child, label)| {
                self.settings_tree_row_selected(category, child, label)
            })
            .unwrap_or(0);
        let next = if down {
            (index + 1).min(rows.len().saturating_sub(1))
        } else {
            index.saturating_sub(1)
        };
        if let Some(&(category, child, label)) = rows.get(next) {
            self.select_settings_page(category, child.then_some(label));
        }
    }

    pub(crate) fn expand_settings_category(&mut self, expand: bool) {
        let category = self.settings_category_selected;
        let sections = Self::settings_sections(category);
        if sections.is_empty() {
            return;
        }
        if expand && self.settings_category_collapsed[category] {
            self.settings_category_collapsed[category] = false;
            self.reveal_settings_category();
        } else if expand {
            if self.settings_selected_section(category).is_none() {
                self.select_settings_page(category, sections.first().copied());
            }
        } else if !self.settings_category_collapsed[category]
            && self.settings_selected_section(category).is_some()
        {
            self.select_settings_page(category, None);
        } else {
            self.settings_category_collapsed[category] = true;
            self.reveal_settings_category();
        }
    }

    /// Track length, thumb length and maximum row offset for the navigation tree.
    pub(crate) fn settings_category_scrollbar(&self) -> Option<(usize, usize, usize)> {
        let height = self.settings_category_area?.height as usize;
        let total = self.settings_category_visual_rows().len();
        if height == 0 || total <= height {
            return None;
        }
        Some((height, (height * height / total).max(1), total - height))
    }

    pub(crate) fn drag_settings_category_scrollbar(&mut self, row: u16) {
        if let Some(area) = self.settings_category_area
            && let Some((height, thumb, max)) = self.settings_category_scrollbar()
        {
            let position = row.saturating_sub(area.y) as usize;
            let top = position.saturating_sub(thumb / 2).min(height - thumb);
            self.settings_category_scroll = top * max / (height - thumb).max(1);
        }
    }
}
