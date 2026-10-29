use crate::app::{App, EditorGroup, SplitDirection, SplitNode, TabDropZone};

impl App {
    /// Validate pane hit maps against the currently visible tab, including
    /// switches that happened since the last frame.
    pub(crate) fn visible_tab_group(&self, id: u64, kind: crate::app::TabKind) -> Option<usize> {
        let matches = |tab: &crate::app::Tab| tab.editor.navigation_id == id && tab.kind == kind;
        if self.tabs.get(self.active_tab).is_some_and(matches) {
            return Some(self.active_group);
        }
        let index = self.groups.iter().enumerate().find_map(|(index, group)| {
            (index != self.active_group
                && group.desk == self.desks.active
                && group.tabs.get(group.active_tab).is_some_and(matches))
            .then_some(index)
        })?;
        let mut visible = false;
        if let Some(tree) = &self.split_tree {
            tree.visit_leaves(&mut |leaf| visible |= leaf == index);
        }
        visible.then_some(index)
    }

    /// Whether there is more than one editor group (i.e. a split view is active)
    pub(crate) fn split_active(&self) -> bool {
        if self.desks.items.len() > 1 || (self.groups.len()==1 && self.groups[0].tabs.is_empty()) {
            self.split_tree.as_ref().is_some_and(|tree| tree.leaves().len() > 1)
        } else { !self.groups.is_empty() }
    }

    /// Save current active group state into the groups vector
    pub(crate) fn save_active_group(&mut self) {
        let g = EditorGroup {
            desk: self.desks.active,
            inactive_chrome_cache: self.groups.get_mut(self.active_group)
                .and_then(|group| group.inactive_chrome_cache.take()),
            visible_highlight_key: self.visible_highlight_key.take(),
            visible_highlight_cancel_token: self.visible_highlight_cancel_token.take(),
            provisional_highlight_key: self.provisional_highlight_key.take(),
            provisional_highlight_cancel_token: self.provisional_highlight_cancel_token.take(),
            tabs: std::mem::take(&mut self.tabs),
            tab_mru_order: std::mem::take(&mut self.tab_mru_order),
            active_tab: self.active_tab,
            tab_scroll_offset: self.tab_scroll_offset,
            tab_strip_scroll: self.tab_strip_scroll.filter(|_| self.tab_scroll_manual_anchor == Some(self.active_tab)),
            code_area: self.code_area,
            tab_bar_area: self.tab_bar_area,
            editor_area: self.editor_area,
            minimap_area: self.minimap_area,
            gutter_area: self.gutter_area,
            gutter_width: self.gutter_width,
            scrollbar_area: self.scrollbar_area,
            h_scrollbar_area: self.h_scrollbar_area,
            hovered_tab: self.hovered_tab,
            hovered_close_btn: self.hovered_close_button,
            breadcrumb_area: self.breadcrumb_area,
            breadcrumb_segments: std::mem::take(&mut self.breadcrumb_segments),
        };
        if self.active_group < self.groups.len() {
            self.groups[self.active_group] = g;
        }
    }

    /// Load group state from the groups vector into the active flat fields
    pub(crate) fn load_group(&mut self, idx: usize) {
        if idx < self.groups.len() {
            let g = &mut self.groups[idx];
            self.visible_highlight_key = g.visible_highlight_key.take();
            self.visible_highlight_cancel_token = g.visible_highlight_cancel_token.take();
            self.provisional_highlight_key = g.provisional_highlight_key.take();
            self.provisional_highlight_cancel_token = g.provisional_highlight_cancel_token.take();
            self.tabs = std::mem::take(&mut g.tabs);
            self.tab_mru_order = std::mem::take(&mut g.tab_mru_order);
            self.active_tab = if self.tabs.is_empty() {
                0
            } else {
                g.active_tab.min(self.tabs.len() - 1)
            };
            self.tab_scroll_offset = g.tab_scroll_offset;
            self.tab_strip_scroll = g.tab_strip_scroll;
            self.tab_scroll_manual_anchor = self.tab_strip_scroll
                .filter(|scroll| self.tabs.get(self.active_tab).is_some_and(|tab| scroll.anchor == tab.editor.navigation_id))
                .map(|_| self.active_tab);
            self.code_area = g.code_area;
            self.tab_bar_area = g.tab_bar_area;
            self.editor_area = g.editor_area;
            self.minimap_area = g.minimap_area;
            self.gutter_area = g.gutter_area;
            self.gutter_width = g.gutter_width;
            self.scrollbar_area = g.scrollbar_area;
            self.h_scrollbar_area = g.h_scrollbar_area;
            self.hovered_tab = g.hovered_tab;
            self.hovered_close_button = g.hovered_close_btn;
            self.breadcrumb_area = g.breadcrumb_area;
            self.breadcrumb_segments = std::mem::take(&mut g.breadcrumb_segments);
            self.settings_editor_active = self.tabs.get(self.active_tab).is_some_and(|t|t.kind==crate::app::TabKind::Settings);
        }
    }

    /// Switch focus to a different editor group
    pub(crate) fn switch_group(&mut self, new_group: usize) {
        if let Some(desk) = self.groups.get(new_group).map(|g| g.desk)
            && desk != self.desks.active {
            self.switch_desk(desk);
        }
        if new_group == self.active_group || new_group >= self.groups.len() {
            return;
        }
        self.cancel_remote_location();
        // Input can change focus before the next render/synchronization pass.
        // Flush the outgoing document and its history before the new view can edit.
        self.sync_duplicate_file_tabs();
        if let Some(editor) = self.editor_mut() { editor.break_undo_group(); }
        self.clear_tab_tooltip();
        self.save_active_group();
        self.active_group = new_group;
        self.load_group(new_group);
        self.sync_diff_tab();
    }

    /// Create a new split group and move a tab to it.
    /// `direction` controls how the new pane is placed relative to the active group.
    pub(crate) fn create_split_group(&mut self, tab_idx: usize) {
        self.create_split_group_dir(
            tab_idx,
            if self.split_vertical {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            },
        );
    }

    pub(crate) fn create_split_group_dir(&mut self, tab_idx: usize, direction: SplitDirection) {
        self.create_split_group_at(
            tab_idx,
            match direction {
                SplitDirection::Horizontal => TabDropZone::Right,
                SplitDirection::Vertical => TabDropZone::Bottom,
            },
        );
    }

    /// Move a tab into a new pane on the requested side of the active group.
    pub(crate) fn create_split_group_at(&mut self, tab_idx: usize, zone: TabDropZone) {
        // Moving the only tab within its own pane cannot create a split.
        if tab_idx >= self.tabs.len() || self.tabs.len() < 2 {
            return;
        }
        let direction = match zone {
            TabDropZone::Left | TabDropZone::Right => SplitDirection::Horizontal,
            TabDropZone::Top | TabDropZone::Bottom => SplitDirection::Vertical,
        };
        let before = matches!(zone, TabDropZone::Left | TabDropZone::Top);
        let was_empty = self.groups.is_empty();
        if was_empty {
            // First split: save current state as group 0
            self.groups.push(EditorGroup::new());
            self.active_group = 0;
        }
        self.save_active_group();
        // Take the tab out of the current group
        let mut tab = self.groups[self.active_group].tabs.remove(tab_idx);
        self.desks.membership.remove(&tab.editor.navigation_id);
        tab.preview = false;
        let source = &mut self.groups[self.active_group];
        crate::tab_collection::removed(&mut source.tab_mru_order, tab_idx);
        source.active_tab = source
            .active_tab
            .saturating_sub(usize::from(tab_idx < source.active_tab))
            .min(source.tabs.len().saturating_sub(1));
        // Create new group with this tab
        let mut new_group = EditorGroup::new();
        new_group.desk = self.desks.active;
        new_group.tabs.push(tab);
        new_group.tab_mru_order.push(0);
        self.groups.push(new_group);
        let new_idx = self.groups.len() - 1;
        self.navigation.move_editor(self.groups[new_idx].tabs[0].editor.navigation_id, new_idx);

        // Build/update split tree
        if was_empty {
            self.split_tree = Some(SplitNode::Leaf(0));
        } else if self.split_tree.is_none() && self.desks.items.len() > 1 {
            self.split_tree = Some(SplitNode::Leaf(self.active_group));
        } else if self.split_tree.is_none() {
            // Restore a tree for sessions that only stored a flat layout.
            let children: Vec<SplitNode> = (0..new_idx).map(SplitNode::Leaf).collect();
            let flat_dir = if self.split_vertical {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            let n = children.len();
            self.split_tree = Some(SplitNode::Split {
                direction: flat_dir,
                children,
                ratios: vec![1.0 / n as f32; n],
            });
        }
        if let Some(tree) = &mut self.split_tree {
            tree.split_leaf(self.active_group, new_idx, direction, before);
            tree.simplify();
        }

        // Clean up empty groups
        self.active_group = new_idx;
        self.compact_groups();
        self.load_group(self.active_group);
        self.sync_diff_tab();
        self.tab_scroll_manual_anchor = None;
    }

    /// Build a map from old group indices to new compacted indices, skipping empty groups.
    fn build_group_index_map(&self) -> rustc_hash::FxHashMap<usize, usize> {
        let mut index_map = rustc_hash::FxHashMap::default();
        let mut new_idx = 0usize;
        for (i, g) in self.groups.iter().enumerate() {
            if !g.tabs.is_empty() {
                index_map.insert(i, new_idx);
                new_idx += 1;
            }
        }
        index_map
    }

    /// Remove empty groups and compact indices, updating split_tree.
    fn compact_groups(&mut self) {
        if self.desks.items.len() > 1 { self.compact_desk_groups(); return; }
        let index_map = self.build_group_index_map();
        if index_map.len() == self.groups.len() {
            return;
        } // nothing to compact

        // Remap active_group
        self.navigation.remap_groups(&index_map);
        self.active_group = index_map.get(&self.active_group).copied().unwrap_or(0);

        // Remove empty leaves from tree and remap indices
        if let Some(ref mut tree) = self.split_tree {
            // Remove leaves that are not in kept
            let all_leaves: Vec<usize> = tree.leaves();
            for leaf_id in all_leaves {
                if !index_map.contains_key(&leaf_id) {
                    tree.remove_leaf(leaf_id);
                }
            }
            tree.remap_indices(&index_map);
            tree.simplify();
        }

        // Compact the groups vec
        let mut kept: Vec<usize> = index_map.keys().copied().collect();
        kept.sort_unstable();
        let new_groups: Vec<EditorGroup> = kept
            .into_iter()
            .map(|i| std::mem::take(&mut self.groups[i]))
            .collect();
        self.groups = new_groups;
    }

    /// Close a group if it has no tabs, removing it from the groups list
    pub(crate) fn close_empty_groups(&mut self) {
        if self.groups.is_empty() {
            return;
        }
        self.save_active_group();
        if self.desks.items.len() > 1 {
            self.compact_desk_groups();
            self.load_group(self.active_group);
            return;
        }

        let index_map = self.build_group_index_map();

        if let Some(ref mut tree) = self.split_tree {
            // Collect empty group indices first, then remove them
            let empty_ids: Vec<usize> = self
                .groups
                .iter()
                .enumerate()
                .filter(|(_, g)| g.tabs.is_empty())
                .map(|(i, _)| i)
                .collect();
            for id in &empty_ids {
                tree.remove_leaf(*id);
            }
            tree.remap_indices(&index_map);
            tree.simplify();
        }

        // Remap active_group
        self.navigation.remap_groups(&index_map);
        self.active_group = index_map.get(&self.active_group).copied().unwrap_or(0);

        self.groups.retain(|g| !g.tabs.is_empty());

        // No groups left: exit split mode entirely
        if self.groups.is_empty() {
            self.active_group = 0;
            self.split_tree = None;
            return;
        }

        // One group left: load it as the single active pane
        if self.groups.len() <= 1 {
            self.split_tree = None;
            self.active_group = 0;
            self.load_group(0);
            self.groups.clear();
            return;
        }

        // Multiple groups remain
        if self.active_group >= self.groups.len() {
            self.active_group = self.groups.len() - 1;
        }
        self.load_group(self.active_group);

        // Verify tree leaf indices are valid after cleanup
        if let Some(ref tree) = self.split_tree {
            let leaves = tree.leaves();
            let valid = leaves.iter().all(|&id| {
                id == self.active_group
                    || (id < self.groups.len() && !self.groups[id].tabs.is_empty())
            });
            if !valid {
                // Tree is inconsistent — rebuild from flat groups
                let n = self.groups.len();
                let direction = SplitDirection::Horizontal;
                let children: Vec<SplitNode> = (0..n).map(SplitNode::Leaf).collect();
                let ratios = vec![1.0 / n as f32; n];
                self.split_tree = Some(SplitNode::Split {
                    direction,
                    children,
                    ratios,
                });
            }
        }
    }

    /// Collapse split mode into a single pane, keeping all open tabs.
    pub(crate) fn close_all_splits(&mut self) {
        if self.desks.items.len() > 1 { self.merge_desk_panes(); return; }
        if self.groups.is_empty() {
            self.active_group = 0;
            self.split_tree = None;
            return;
        }

        self.save_active_group();
        let mut merged_tabs = std::mem::take(&mut self.groups[self.active_group].tabs);
        let mut merged_mru = std::mem::take(&mut self.groups[self.active_group].tab_mru_order);
        for i in 0..self.groups.len() {
            if i != self.active_group {
                let mut other_tabs = std::mem::take(&mut self.groups[i].tabs);
                let offset = merged_tabs.len();
                merged_mru.extend(self.groups[i].tab_mru_order.iter()
                    .filter(|&&index| index < other_tabs.len()).map(|index| index + offset));
                merged_tabs.append(&mut other_tabs);
            }
        }

        self.tabs = merged_tabs;
        self.navigation.merge_groups();
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.tab_mru_order = merged_mru;

        self.groups.clear();
        self.active_group = 0;
        self.split_tree = None;
    }
}

#[cfg(test)]
#[path = "app_layout_tests.rs"]
mod tests;
