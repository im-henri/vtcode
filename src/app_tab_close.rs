use crate::app::{ActivePane, App, Tab, TabKind};

struct ClosedTab {
    navigation_id: u64,
    kind: TabKind,
    file: Option<(std::path::PathBuf, usize, usize)>,
    view: Option<crate::app::ExtensionViewTabRef>,
}

impl ClosedTab {
    fn new(tab: &Tab) -> Self {
        Self {
            navigation_id: tab.editor.navigation_id,
            kind: tab.kind,
            file: (tab.kind == TabKind::File)
                .then(|| {
                    tab.editor
                        .file_path
                        .clone()
                        .map(|path| (path, tab.editor.cursor_line, tab.editor.cursor_col))
                })
                .flatten(),
            view: tab.extension_view.clone(),
        }
    }
}

impl App {
    pub(crate) fn tab_operation_pending(&self, tab: &Tab) -> bool {
        self.pending_save_as.contains_key(&tab.editor.save_identity) || tab.editor.file_path.as_ref().is_some_and(|path| {
            self.pending_file_saves.contains_key(path) || self.deferred_file_saves.contains_key(path)
        }) || tab.extension_view.as_ref().is_some_and(|view| {
            self.extension_manager
                .view_input_pending(&view.extension_id, &view.view_id)
        })
    }

    pub(crate) fn close_tab(&mut self, idx: usize) {
        self.sync_duplicate_file_tabs();
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        if tab.pinned {
            self.notify("Tab is pinned — unpin first");
        } else if self.tab_operation_pending(tab) {
            self.notify("Wait for the current save or extension operation before closing this tab");
        } else if tab.has_unsaved_changes() && !self.tabs.iter().chain(self.groups.iter().flat_map(|g|&g.tabs)).any(|peer|
            peer.editor.navigation_id!=tab.editor.navigation_id && peer.kind==TabKind::File && peer.editor.save_identity==tab.editor.save_identity) {
            self.confirm_close_active = true;
            self.confirm_close_tab = idx;
        } else {
            self.force_close_tab(idx);
        }
    }

    /// Plan against the original indices, stopping at the first deferred close.
    /// Keeping the visible editor last also avoids intermediate navigation stops.
    fn plan_tab_closes(&self, matches: impl Fn(usize) -> bool) -> (Vec<usize>, Option<usize>) {
        let mut close = Vec::new();
        for idx in (0..self.tabs.len())
            .rev()
            .filter(|&i| i != self.active_tab)
            .chain((self.active_tab < self.tabs.len()).then_some(self.active_tab))
        {
            let tab = &self.tabs[idx];
            if tab.pinned || !matches(idx) {
                continue;
            }
            if tab.has_unsaved_changes() || self.tab_operation_pending(tab) {
                return (close, Some(idx));
            }
            close.push(idx);
        }
        (close, None)
    }

    pub(crate) fn close_tabs_matching(&mut self, matches: impl Fn(usize) -> bool) {
        let (close, blocked) = self.plan_tab_closes(matches);
        let blocked = blocked.map(|idx| idx - close.iter().filter(|&&i| i < idx).count());
        self.close_tab_batch(vec![(self.active_group, close)]);
        if let Some(idx) = blocked {
            self.close_tab(idx);
        }
    }

    pub(crate) fn force_close_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.close_tab_batch(vec![(self.active_group, vec![idx])]);
        }
    }

    pub(crate) fn close_saved_tabs_matching(&mut self, matches: impl Fn(usize) -> bool) {
        let close = self
            .tabs
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, tab)| {
                (matches(index)
                    && !tab.pinned
                    && !tab.has_unsaved_changes()
                    && !self.tab_operation_pending(tab))
                .then_some(index)
            })
            .collect();
        self.close_tab_batch(vec![(self.active_group, close)]);
    }

    fn saved_tab_closes(&self, include_active: bool) -> Vec<(usize, Vec<usize>)> {
        let eligible = |tab: &Tab| {
            !tab.pinned && !tab.has_unsaved_changes() && !self.tab_operation_pending(tab)
        };
        let mut plan = Vec::new();
        if include_active {
            plan.push((
                self.active_group,
                self.tabs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, tab)| eligible(tab).then_some(i))
                    .collect(),
            ));
        }
        for (group, state) in self.groups.iter().enumerate().rev() {
            if group == self.active_group || !self.group_in_current_desk(group) {
                continue;
            }
            plan.push((
                group,
                state
                    .tabs
                    .iter()
                    .enumerate()
                    .rev()
                    .filter_map(|(i, tab)| eligible(tab).then_some(i))
                    .collect(),
            ));
        }
        plan
    }

    pub(crate) fn close_saved_tabs(&mut self) {
        let count = self.close_tab_batch(self.saved_tab_closes(true));
        self.notify(format!("Closed {count} saved tabs"));
    }

    pub(crate) fn close_all_tabs(&mut self) {
        // Preserve the existing command's policy: keep unsaved inactive views,
        // and prompt for unsaved tabs in the current pane.
        let (close, blocked) = self.plan_tab_closes(|_| true);
        let blocked = blocked.map(|idx| idx - close.iter().filter(|&&i| i < idx).count());
        let mut plan = self.saved_tab_closes(false);
        plan.push((self.active_group, close));
        self.close_tab_batch(plan);
        if let Some(idx) = blocked {
            self.close_tab(idx);
        }
    }

    /// Tear down a set of views without repeatedly shifting the remaining
    /// editors, rebuilding navigation, or scanning all views for every file.
    fn close_tab_batch(&mut self, plan: Vec<(usize, Vec<usize>)>) -> usize {
        let count: usize = plan.iter().map(|(_, indices)| indices.len()).sum();
        if count == 0 {
            return 0;
        }
        let changing_editor = plan.iter().any(|(group, indices)| {
            *group == self.active_group && indices.contains(&self.active_tab)
        });
        if changing_editor {
            self.record_navigation();
            self.clear_transient_editor_state();
            self.blame_cache = None;
            self.blame_cache_key = None;
            self.suspend_diff_tab();
            if self.diff_mode != crate::app::DiffMode::Off {
                self.clear_diff_state();
            }
            self.clear_file_search_results();
        }
        let mut closed = Vec::with_capacity(count);
        for (group, indices) in plan {
            if indices.is_empty() {
                continue;
            }
            let (tabs, active, mru) = if group == self.active_group {
                (
                    &mut self.tabs,
                    &mut self.active_tab,
                    &mut self.tab_mru_order,
                )
            } else {
                let group = &mut self.groups[group];
                (
                    &mut group.tabs,
                    &mut group.active_tab,
                    &mut group.tab_mru_order,
                )
            };
            if indices.len() == 1 {
                let index = indices[0];
                closed.push(ClosedTab::new(&tabs[index]));
                tabs.remove(index);
                crate::tab_collection::removed(mru, index);
                if index < *active {
                    *active -= 1;
                } else if index == *active {
                    *active = mru.first().copied().unwrap_or(*active);
                }
                *active = (*active).min(tabs.len().saturating_sub(1));
                continue;
            }
            let mut remove = vec![false; tabs.len()];
            for index in indices {
                closed.push(ClosedTab::new(&tabs[index]));
                remove[index] = true;
            }
            crate::tab_collection::retain(tabs, active, mru, |i, _| !remove[i]);
        }

        let mut paths = rustc_hash::FxHashSet::default();
        let mut views = rustc_hash::FxHashMap::default();
        let mut preview_closed = false;
        for tab in closed {
            // Do not retain the closed view's rope and attribution until a
            // future analysis result happens to prune the document cache.
            self.blame_documents.remove(&tab.navigation_id);
            self.desks.membership.remove(&tab.navigation_id);
            self.desks.selected_tabs.remove(&tab.navigation_id);
            let still_open = self.tabs.iter().chain(self.groups.iter().flat_map(|g|&g.tabs)).any(|t|t.kind==tab.kind);
            match tab.kind {
                TabKind::Chat(key) if !still_open => {
                    self.chat.layout.editors.remove(&key);
                    if self.chat.layout.active == Some(key) {
                        self.chat.layout.active = None;
                        self.chat.switch_connection(self.chat.layout.dock_agent);
                        let selected = self.chat.dock_selected.clone();
                        self.chat.select_tab(selected);
                        self.active_pane = ActivePane::Editor;
                    }
                }
                TabKind::Settings => self.settings_editor_active = false,
                TabKind::CommitDetails if !still_open => self.reset_commit_details(),
                TabKind::Diff(id) if !still_open => self.forget_diff_tab(id),
                TabKind::MarkdownPreview => preview_closed = true,
                _ => {}
            }
            if let Some((path, line, col)) = tab.file {
                paths.insert(path.clone());
                self.closed_tab_paths.push_back((path, line, col));
                if self.closed_tab_paths.len() > 20 {
                    self.closed_tab_paths.pop_front();
                }
            }
            if let Some(view) = tab.view {
                views.insert((view.extension_id.clone(), view.view_id.clone()), view);
            }
        }
        if !self.desks.groups.is_empty() {
            let used: rustc_hash::FxHashSet<_> = self.desks.membership.values().copied().collect();
            self.desks.groups.retain(|id,_|used.contains(id));
        }
        // One remaining-document index for the entire batch, with no filesystem
        // work. Keep the path identity used by the LSP manager and open tabs.
        if paths.len() == 1 {
            self.close_lsp_document_if_unused(paths.iter().next().unwrap());
        } else if !paths.is_empty() {
            let open: rustc_hash::FxHashSet<_> = self
                .tabs
                .iter()
                .chain(self.groups.iter().flat_map(|g| &g.tabs))
                .filter(|tab| tab.kind == TabKind::File)
                .filter_map(|tab| tab.editor.file_path.as_deref())
                .collect();
            for path in paths {
                if !open.contains(path.as_path()) {
                    self.lsp_manager.did_close(&path);
                }
            }
        }
        if self.split_active()
            && (self.tabs.is_empty()
                || self
                    .groups
                    .iter()
                    .enumerate()
                    .any(|(i, group)| i != self.active_group && group.tabs.is_empty()))
        {
            self.close_empty_groups();
        }
        if preview_closed {
            self.markdown_preview = self
                .tabs
                .iter()
                .chain(self.groups.iter().flat_map(|g| &g.tabs))
                .any(|tab| tab.kind == TabKind::MarkdownPreview);
        }
        if !self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|g| &g.tabs))
            .any(|tab| tab.kind == TabKind::File)
        {
            self.search_active = false;
            self.search_input_focused = false;
            self.hovered_search_toggle = None;
            self.search_pre_cursor = None;
            self.search_selection_range = None;
        }
        self.confirm_close_active = false;
        self.tab_switcher_active = false;
        self.tab_scroll_manual_anchor = None;
        self.ensure_active_tab_visible();
        self.reconcile_navigation();
        if changing_editor {
            self.record_navigation();
        }
        for view in views.into_values() {
            self.notify_extension_view_closed(&view);
        }
        self.prune_remote_file_changes();
        count
    }
}
