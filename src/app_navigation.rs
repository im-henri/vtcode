//! Workbench navigation, shared by the mouse, keyboard, menu and palette.
//!
//! VS Code's default selection stack has 50 stops. It compares the *start* of
//! the primary selection, merges ordinary moves less than ten lines apart,
//! and preserves explicit jumps between different lines even at that distance.
//! Behavioral tests: app_navigation_tests.rs.

use std::path::{Path, PathBuf};

use crate::app::{ActivePane, App, TabKind};
use crate::editor::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NavigationSelection {
    pub cursor: (usize, usize),
    pub anchor: Option<(usize, usize)>,
}

impl NavigationSelection {
    pub(crate) fn capture(editor: &Editor) -> Option<Self> {
        if editor.bg_loading && editor.pending_cursor_position.is_none() {
            return None;
        }
        let mut selection = editor.pending_navigation_selection.unwrap_or(Self {
            cursor: editor
                .pending_cursor_position
                .unwrap_or((editor.cursor_line, editor.cursor_col)),
            anchor: editor.selection_anchor,
        });
        // A click, or a range clamped after an edit, can leave an empty anchor.
        // It is the same visible location as a caret without a selection.
        selection.anchor = selection
            .anchor
            .filter(|anchor| *anchor != selection.cursor);
        Some(selection)
    }

    fn start_line(self) -> usize {
        self.anchor
            .map_or(self.cursor.0, |anchor| anchor.0.min(self.cursor.0))
    }

    pub(crate) fn restore(self, editor: &mut Editor) {
        if editor.bg_loading {
            editor.pending_cursor_utf16 = false;
            editor.pending_cursor_position = Some(self.cursor);
            editor.pending_navigation_selection = Some(self);
            return;
        }
        editor.clear_selection();
        editor.extra_cursors.clear();
        editor.extra_cursor_anchors.clear();
        let outside_viewport = self.cursor.0 < editor.scroll_offset
            || self.cursor.0
                >= editor
                    .scroll_offset
                    .saturating_add(editor.viewport_height.max(1));
        editor.set_cursor(self.cursor.0, self.cursor.1);
        editor.selection_anchor = self.anchor.map(|(line, col)| {
            let line = line.min(editor.line_count().saturating_sub(1));
            (line, col.min(editor.line_len(line)))
        });
        if outside_viewport {
            editor.center_cursor();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NavigationLocation {
    group: usize,
    editor_id: u64,
    kind: TabKind,
    // Only file editors can be reopened after disposal. Untitled and tool
    // editors retain their identity while open, without inventing disk paths.
    path: Option<PathBuf>,
    selection: Option<NavigationSelection>,
}

impl NavigationLocation {
    fn same_editor(&self, other: &Self) -> bool {
        self.group == other.group
            && self.kind == other.kind
            && (self.editor_id == other.editor_id
                || self
                    .path
                    .as_ref()
                    .zip(other.path.as_ref())
                    .is_some_and(|(a, b)| App::same_open_path(a, b)))
    }
}

#[derive(Default)]
pub(crate) struct NavigationHistory {
    entries: Vec<NavigationLocation>,
    index: Option<usize>,
    // Nested workbench commands and file opens must not record their own
    // intermediate state while restoring a history location.
    pub(crate) restoring: bool,
    jump_pending: bool,
    pending_file: Option<PendingNavigation>,
    #[cfg(test)]
    reference_caret_recording: bool,
}

struct PendingNavigation {
    editor_id: u64,
    path: PathBuf,
    forward: bool,
    fallback: Option<NavigationLocation>,
}

impl NavigationHistory {
    fn replaces_selection(&self, old: Option<NavigationSelection>, new: Option<NavigationSelection>) -> bool {
        match (old, new) {
            (None, _) => true,
            (Some(old), Some(new)) => {
                let distance = old.start_line().abs_diff(new.start_line());
                distance == 0 || (!self.jump_pending && distance < 10)
            }
            _ => false,
        }
    }

    fn record_current_selection(&mut self, selection: Option<NavigationSelection>) -> bool {
        let Some(index) = self.index else { return false; };
        let previous = self.entries[index].selection;
        if previous == selection { return true; }
        #[cfg(test)]
        if self.reference_caret_recording { return false; }
        if selection.is_none() { return true; }
        if !self.replaces_selection(previous, selection) { return false; }
        self.entries[index].selection = selection;
        self.jump_pending = false;
        true
    }

    pub(crate) fn can_navigate(&self, forward: bool) -> bool {
        let Some(index) = self.index else { return false; };
        let Some(current) = self.entries.get(index) else { return false; };
        let candidates = if forward {
            &self.entries[index + 1..]
        } else {
            &self.entries[..index]
        };
        // Reconciliation can merge adjacent stops. Match navigation's skip
        // behavior so those duplicates do not enable an ineffective button.
        candidates.iter().any(|entry| {
            !current.same_editor(entry) || current.selection != entry.selection
        })
    }

    pub(crate) fn resolve_loaded_editor(
        &mut self,
        editor: &Editor,
        pending_selection: Option<NavigationSelection>,
    ) {
        let actual = NavigationSelection::capture(editor);
        // Loading may finish after the user switches tabs or walks elsewhere
        // in history. Resolve only stops belonging to this exact deferred
        // selection, never whichever stop happens to be current now.
        for entry in &mut self.entries {
            if entry.editor_id == editor.navigation_id && entry.selection == pending_selection {
                entry.selection = actual;
            }
        }
        if self
            .pending_file
            .as_ref()
            .is_some_and(|pending| pending.editor_id == editor.navigation_id)
        {
            self.pending_file = None;
        }
    }

    fn record(&mut self, location: NavigationLocation) {
        if self.restoring {
            return;
        }
        if let Some(index) = self.index {
            let current = &self.entries[index];
            if current == &location {
                return;
            }
            if current.same_editor(&location) && location.selection.is_none() {
                // Reopening the current file temporarily has no selection.
                // That placeholder is not a new visit: preserve the known
                // location and Forward branch until its contents arrive.
                return;
            }
            let replace = current.same_editor(&location)
                && self.replaces_selection(current.selection, location.selection);
            if replace {
                // Small moves after Back preserve the forward branch.
                self.entries[index] = location;
                self.jump_pending = false;
                return;
            }
            self.entries.truncate(index + 1);
        }
        self.entries.push(location);
        if self.entries.len() > 50 {
            self.entries.remove(0);
        }
        self.index = Some(self.entries.len() - 1);
        self.jump_pending = false;
    }

    fn retain(&mut self, mut keep: impl FnMut(&NavigationLocation) -> bool) {
        let mut retained_before: usize = 0;
        let old_index = self.index;
        let mut i = 0;
        self.entries.retain(|entry| {
            let retained = keep(entry);
            if retained && old_index.is_some_and(|index| i <= index) {
                retained_before += 1;
            }
            i += 1;
            retained
        });
        self.index = retained_before.checked_sub(1);
        if self.index.is_none() && !self.entries.is_empty() {
            self.index = Some(0);
        }
    }

    pub(crate) fn remap_groups(&mut self, map: &rustc_hash::FxHashMap<usize, usize>) {
        self.retain(|entry| map.contains_key(&entry.group));
        for entry in &mut self.entries {
            entry.group = map[&entry.group];
        }
    }

    pub(crate) fn merge_groups(&mut self) {
        for entry in &mut self.entries {
            entry.group = 0;
        }
    }

    pub(crate) fn move_editor(&mut self, editor_id: u64, group: usize) {
        for entry in &mut self.entries {
            if entry.editor_id == editor_id {
                entry.group = group;
            }
        }
    }

    pub(crate) fn rename_path(&mut self, from: &Path, to: &Path) {
        for entry in &mut self.entries {
            if let Some(path) = &entry.path {
                if App::same_open_path(path, from) {
                    entry.path = Some(to.to_path_buf());
                } else if let Ok(relative) = path.strip_prefix(from) {
                    entry.path = Some(to.join(relative));
                }
            }
        }
    }

    pub(crate) fn remove_path(&mut self, path: &Path) {
        self.retain(|entry| {
            !entry.path.as_ref().is_some_and(|candidate| {
                App::same_open_path(candidate, path) || candidate.starts_with(path)
            })
        });
    }
}

impl App {
    fn navigation_location(&self) -> Option<NavigationLocation> {
        let tab = self.tabs.get(self.active_tab)?;
        let editor = &tab.editor;
        let selection = matches!(tab.kind, TabKind::File | TabKind::GitRevision | TabKind::Diff(_))
            .then(|| NavigationSelection::capture(editor))
            .flatten();
        Some(NavigationLocation {
            group: self.active_group,
            editor_id: editor.navigation_id,
            kind: tab.kind,
            path: (tab.kind == TabKind::File)
                .then(|| editor.file_path.clone())
                .flatten(),
            selection,
        })
    }

    pub(crate) fn record_navigation(&mut self) {
        self.cancel_stale_remote_location();
        // Go to Line previews are transient; Enter records its original source
        // and final target, and Escape contributes no navigation stop.
        if self.navigation.restoring || self.goto_line_active {
            return;
        }
        // The event loop records navigation after ordinary redraws, mouse
        // movement and edits too. Ordinary caret moves only change the
        // current stop's selection: do not clone its path and reconcile all
        // fifty stops on every arrow key or mouse selection sample.
        let same_view = self.navigation.pending_file.is_none()
            && self.navigation.index.and_then(|index| self.navigation.entries.get(index))
                .zip(self.tabs.get(self.active_tab))
                .is_some_and(|(entry, tab)| {
                    entry.group == self.active_group
                        && entry.editor_id == tab.editor.navigation_id
                        && entry.kind == tab.kind
                        && entry.path.as_deref() == (tab.kind == TabKind::File)
                            .then_some(tab.editor.file_path.as_deref()).flatten()
                });
        if same_view {
            let tab = &self.tabs[self.active_tab];
            let selection = matches!(tab.kind, TabKind::File | TabKind::GitRevision | TabKind::Diff(_))
                .then(|| NavigationSelection::capture(&tab.editor)).flatten();
            if self.navigation.record_current_selection(selection) { return; }
        }
        let location = self.navigation_location();
        if self
            .navigation
            .pending_file
            .as_ref()
            .is_some_and(|pending| {
                !location.as_ref().is_some_and(|location| {
                    location.editor_id == pending.editor_id
                        && self
                            .navigation
                            .index
                            .and_then(|index| self.navigation.entries.get(index))
                            .is_some_and(|entry| {
                                entry.same_editor(location) && entry.selection == location.selection
                            })
                })
            })
        {
            // A user action supersedes a pending Back/Forward. Its later load
            // result must neither overwrite history nor navigate on failure.
            self.navigation.pending_file = None;
        }
        if let Some(location) = location {
            for entry in &mut self.navigation.entries {
                if entry.editor_id == location.editor_id || entry.same_editor(&location) {
                    entry.editor_id = location.editor_id;
                    if entry.path != location.path {
                        entry.path.clone_from(&location.path);
                    }
                    entry.group = location.group;
                }
            }
            self.navigation.record(location);
        }
    }

    pub(crate) fn finish_navigation(&mut self) {
        self.record_navigation();
        self.navigation.jump_pending = false;
    }

    /// Mark the origin of an explicit command (definition, symbol, search,
    /// line, reference, etc.). The next changed location is its destination.
    pub(crate) fn push_nav_history(&mut self) {
        if self.navigation.restoring {
            return;
        }
        if let Some(location) = self.navigation_location() {
            self.navigation.record(location);
        }
        self.navigation.jump_pending = true;
    }

    // Reconcile live view identities before walking history. Reordering tabs
    // does not affect identity; moving a view also moves all of its old stops.
    pub(crate) fn reconcile_navigation(&mut self) {
        let views: rustc_hash::FxHashMap<_, _> = self
            .tabs
            .iter()
            .map(|tab| (self.active_group, tab))
            .chain(
                self.groups
                    .iter()
                    .enumerate()
                    .filter(|(group, _)| *group != self.active_group)
                    .flat_map(|(group, state)| state.tabs.iter().map(move |tab| (group, tab))),
            )
            .map(|(group, tab)| {
                (tab.editor.navigation_id, (group, tab.kind, tab.editor.file_path.as_ref()))
            })
            .collect();
        for entry in &mut self.navigation.entries {
            if let Some((group, kind, path)) = views.get(&entry.editor_id)
            {
                entry.group = *group;
                entry.kind = *kind;
                let path = (*kind == TabKind::File).then_some(*path).flatten();
                if entry.path.as_ref() != path {
                    entry.path = path.cloned();
                }
            }
        }
        self.navigation.retain(|entry| {
            entry.path.is_some() || views.contains_key(&entry.editor_id)
        });
    }

    pub(crate) fn navigate_back(&mut self) {
        self.navigate_history(false);
    }

    pub(crate) fn navigate_forward(&mut self) {
        self.navigate_history(true);
    }

    fn navigate_history(&mut self, forward: bool) {
        if self.goto_line_active {
            self.cancel_goto_line();
        }
        self.reconcile_navigation();
        self.finish_navigation();
        let Some(index) = self.navigation.index else {
            return;
        };
        let current = self.navigation.entries[index].clone();
        let fallback = self.navigation_location();
        let mut target = index;
        loop {
            let next = if forward {
                (target + 1 < self.navigation.entries.len()).then_some(target + 1)
            } else {
                target.checked_sub(1)
            };
            let Some(next) = next else { return };
            target = next;
            let candidate = &self.navigation.entries[target];
            // Removing an intervening view can leave identical neighbors.
            // Skip those stops instead of requiring a no-op button press.
            if current.same_editor(candidate) && current.selection == candidate.selection {
                self.navigation.index = Some(target);
                continue;
            }
            self.restore_history_index(target, forward, fallback.clone());
            // Edits, reloads, and loading placeholders can make distinct saved
            // positions resolve to the same live caret. Keep walking within
            // this press instead of stopping on each now-invisible location.
            let unchanged = self.navigation_location().is_some_and(|actual| {
                current.same_editor(&actual) && current.selection == actual.selection
            });
            if !unchanged {
                return;
            }
        }
    }

    fn restore_history_index(
        &mut self,
        target: usize,
        forward: bool,
        fallback: Option<NavigationLocation>,
    ) {
        self.navigation.index = Some(target);
        let location = self.navigation.entries[target].clone();
        self.navigation.restoring = true;
        self.restore_navigation_location(&location);
        self.navigation.restoring = false;
        self.navigation.pending_file =
            self.editor()
                .filter(|editor| editor.bg_loading)
                .and_then(|editor| {
                    Some(PendingNavigation {
                        editor_id: editor.navigation_id,
                        path: editor.file_path.clone()?,
                        forward,
                        fallback,
                    })
                });
        // Keep the actual clamped position/newly opened editor identity as the
        // current state without adding a stop or truncating forward history.
        if let Some(actual) = self.navigation_location() {
            for entry in &mut self.navigation.entries {
                if entry.same_editor(&location) {
                    entry.editor_id = actual.editor_id;
                }
            }
            self.navigation.entries[target] = actual;
        }
    }

    /// Skip a history target that disappeared or cannot be read. Disk I/O stays
    /// on the existing loader; an unsuccessful reopen is never a history stop.
    pub(crate) fn skip_failed_navigation(&mut self, path: &Path) -> bool {
        let active_request = self
            .navigation
            .pending_file
            .as_ref()
            .is_some_and(|pending| {
                Self::same_open_path(&pending.path, path)
                    && self
                        .editor()
                        .is_some_and(|editor| editor.navigation_id == pending.editor_id)
            });
        if !active_request {
            self.navigation.remove_path(path);
            return false;
        }
        let pending = self.navigation.pending_file.take().unwrap();
        let before = self.navigation.index.map_or(0, |index| {
            self.navigation.entries[..index]
                .iter()
                .filter(|entry| {
                    !entry
                        .path
                        .as_ref()
                        .is_some_and(|candidate| Self::same_open_path(candidate, path))
                })
                .count()
        });
        self.navigation.remove_path(path);
        // Keep the group alive until the next target is restored. Closing the
        // temporary placeholder through force_close_tab would dispose an empty
        // split and erase that group's remaining history in the middle of Back.
        self.tabs.remove(self.active_tab);
        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
        self.tab_mru_order.clear();
        let target = if pending.forward {
            (before < self.navigation.entries.len()).then_some(before)
        } else {
            before.checked_sub(1)
        }
        .or_else(|| {
            pending.fallback.as_ref().and_then(|fallback| {
                self.navigation
                    .entries
                    .iter()
                    .position(|entry| entry.same_editor(fallback))
            })
        });
        if let Some(target) = target {
            self.restore_history_index(target, pending.forward, pending.fallback);
        } else if let Some(fallback) = pending.fallback.filter(|location| {
            !location
                .path
                .as_ref()
                .is_some_and(|candidate| Self::same_open_path(candidate, path))
        }) {
            self.navigation.restoring = true;
            self.restore_navigation_location(&fallback);
            self.navigation.restoring = false;
        }
        self.notify_warn(format!(
            "Skipped unavailable navigation target: {}",
            path.display()
        ));
        true
    }

    fn restore_navigation_location(&mut self, location: &NavigationLocation) {
        self.sync_duplicate_file_tabs();
        self.auto_save_on_focus_change();
        self.clear_transient_editor_state();
        self.clear_drag_states();
        self.dismiss_vtcomplete();
        self.lsp_completion_active = false;
        self.suspend_diff_tab();
        self.switch_group(location.group);
        let index = self.tabs.iter().position(|tab| {
            tab.editor.navigation_id == location.editor_id
                || (tab.kind == location.kind
                    && tab.kind == TabKind::File
                    && tab
                        .editor
                        .file_path
                        .as_ref()
                        .zip(location.path.as_ref())
                        .is_some_and(|(a, b)| Self::same_open_path(a, b)))
        });
        if let Some(index) = index {
            self.active_tab = index;
        } else if let Some(path) = &location.path {
            self.open_file_preview(path);
        } else {
            return;
        }
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.search_input_focused = false;
        self.search_pre_cursor = None;
        self.global_search_active = false;
        self.goto_line_active = false;
        self.quick_open_active = false;
        self.close_palette();
        self.context_menu_active = false;
        self.tab_context_active = false;
        self.menu_open = false;
        self.menu_active = false;
        if let Some(selection) = location.selection {
            if let Some(editor) = self.editor_mut() {
                selection.restore(editor);
            }
        }
        self.sync_diff_tab();
        self.touch_tab_mru(self.active_tab);
        self.tab_scroll_manual_anchor = None;
        self.ensure_active_tab_visible();
        self.refresh_git_line_status_async();
        self.cursor_visible = true;
        self.last_blink = std::time::Instant::now();
    }
}

#[cfg(test)]
#[path = "app_navigation_tests.rs"]
mod tests;
