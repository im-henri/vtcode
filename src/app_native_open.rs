//! Resolve ambiguous Windows names on the file worker before reusing a tab.
use super::file_load_result;
use crate::{
    app::{App, Tab, TabKind},
    app_navigation::{NavigationHistory, NavigationSelection},
    background::BackgroundResult,
    editor::Editor,
    native_paths::{possible_case_alias, same_spelling, windows::entry_path},
};
use std::path::{Path, PathBuf};

pub(crate) struct Request {
    pub(crate) path: PathBuf,
    pub(crate) document: u64,
    pub(crate) skip_open_prompt: bool,
    pub(crate) preserve_existing_preview: bool,
    pub(crate) activate_existing_group: bool,
    pub(crate) deferred_preview: bool,
}

impl Request {
    fn matches(&self, tab: &Tab) -> bool {
        tab.kind == TabKind::File
            && tab.editor.bg_loading
            && tab.editor.save_identity == self.document
            && tab.editor.pending_session_restore.is_none()
            && tab.editor.file_path.as_deref() == Some(self.path.as_path())
    }
}

pub(crate) struct Candidate {
    pub(crate) path: PathBuf,
    pub(crate) document: u64,
    pub(crate) version: Option<u64>,
}

impl Candidate {
    fn matches(&self, tab: &Tab) -> bool {
        tab.kind == TabKind::File
            && !tab.editor.load_failed
            && tab.editor.save_identity == self.document
            && tab.editor.file_path.as_deref() == Some(self.path.as_path())
    }
}

impl App {
    pub(super) fn native_open_candidates(&self, path: &Path) -> Vec<Candidate> {
        if crate::remote::workspace::is_view_key(path) {
            return Vec::new();
        }
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == TabKind::File && !tab.editor.load_failed)
            .filter_map(|tab| {
                let editor = &tab.editor;
                let candidate = editor.file_path.as_ref()?;
                possible_case_alias(path, candidate).then(|| Candidate {
                    path: candidate.clone(),
                    document: editor.save_identity,
                    version: (!editor.bg_loading).then_some(editor.content_version),
                })
            })
            .collect()
    }

    pub(super) fn queue_native_file_open(&mut self, request: Request, candidates: Vec<Candidate>) {
        if candidates.is_empty() {
            if request.deferred_preview {
                self.finish_native_file_open_distinct_preview(&request);
            }
            self.queue_background_file_load(request.path, request.skip_open_prompt);
            return;
        }

        let generation = self.workspace_generation;
        let sender = self.background.sender.clone();
        let activity_path = request.path.clone();
        self.background
            .read_for_file("file-load", &activity_path, true, move || {
                let mut buffer = vec![0; 512];
                let alias = entry_path(&request.path, &mut buffer)
                    .ok()
                    .and_then(|resolved| {
                        candidates.into_iter().find(|source| {
                            same_spelling(&resolved, &source.path)
                                || entry_path(&source.path, &mut buffer)
                                    .is_ok_and(|source_path| same_spelling(&resolved, &source_path))
                        })
                    });
                if let Some(source) = alias {
                    let _ = sender.send(BackgroundResult::NativeOpenResolved {
                        generation,
                        request,
                        source: Some(source),
                    });
                    return;
                }
                let skip_open_prompt = request.skip_open_prompt;
                let path = if request.deferred_preview {
                    let path = request.path.clone();
                    let _ = sender.send(BackgroundResult::NativeOpenResolved {
                        generation,
                        request,
                        source: None,
                    });
                    path
                } else {
                    request.path
                };
                let loaded = crate::editor::load_text_file(&path);
                let _ = sender.send(file_load_result(generation, path, loaded, skip_open_prompt));
            });
    }

    pub(crate) fn finish_native_file_open_distinct_preview(&mut self, request: &Request) -> bool {
        self.promote_modified_preview_tabs();
        let Some(tab) = self
            .tabs
            .get_mut(self.active_tab)
            .filter(|tab| request.matches(tab))
        else {
            return false;
        };
        if !self.enable_preview_tabs || tab.pinned || tab.has_unsaved_changes() {
            return false;
        }
        tab.preview = true;
        self.replace_other_preview_tab();
        true
    }

    fn retry_native_file_open(&mut self, request: Request) {
        let candidates = self.native_open_candidates(&request.path);
        self.queue_native_file_open(request, candidates);
    }

    pub(crate) fn finish_native_file_open_alias(
        &mut self,
        request: Request,
        source: Candidate,
    ) -> bool {
        if !self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| request.matches(tab))
        {
            return false;
        }
        let Some(tab) = self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .find(|tab| source.matches(tab))
        else {
            self.retry_native_file_open(request);
            return false;
        };
        tab.editor.refresh_document();
        if !source.matches(tab) {
            self.retry_native_file_open(request);
            return false;
        }
        let mut shared = Editor::new();
        shared.copy_document_from(&tab.editor);
        shared.bg_loading = tab.editor.bg_loading;
        shared.pending_session_restore = tab.editor.pending_session_restore.clone();

        let active_request = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|tab| request.matches(tab));
        if active_request
            && request.activate_existing_group
            && !self.tabs.iter().any(|tab| source.matches(tab))
            && let Some(group) = self.groups.iter().enumerate().position(|(index, group)| {
                self.group_in_current_desk(index)
                    && group.tabs.iter().any(|tab| source.matches(tab))
            })
        {
            // Keep the selected location when the original tab is in another pane.
            let placeholder = self.tabs.remove(self.active_tab);
            crate::tab_collection::removed(&mut self.tab_mru_order, self.active_tab);
            self.active_tab = self
                .tab_mru_order
                .first()
                .copied()
                .unwrap_or_else(|| self.active_tab.min(self.tabs.len().saturating_sub(1)));
            self.navigation
                .move_editor(placeholder.editor.navigation_id, group);
            self.switch_group(group);
            self.tabs.push(placeholder);
            self.active_tab = self.tabs.len() - 1;
        }
        let mut removed = Vec::new();
        complete_group(
            &mut self.tabs,
            &mut self.active_tab,
            &mut self.tab_mru_order,
            &mut self.navigation,
            &request,
            &shared,
            &mut removed,
        );
        for group in &mut self.groups {
            complete_group(
                &mut group.tabs,
                &mut group.active_tab,
                &mut group.tab_mru_order,
                &mut self.navigation,
                &request,
                &shared,
                &mut removed,
            );
        }
        for view in removed {
            self.desks.membership.remove(&view);
            self.desks.selected_tabs.remove(&view);
        }
        self.resolve_workspace_edit_open_alias(&request, &source);
        let mut seen = false;
        self.recent_files.retain_mut(|path| {
            if same_spelling(path, &request.path) {
                path.clone_from(&source.path);
            }
            !same_spelling(path, &source.path) || !std::mem::replace(&mut seen, true)
        });
        if active_request {
            self.tab_scroll_manual_anchor = None;
            self.touch_tab_mru(self.active_tab);
            self.ensure_active_tab_visible();
            self.refresh_source_control_for_navigation();
            self.refresh_git_line_status_async();
            if !shared.bg_loading {
                self.status_message = format!("Opened: {}", self.display_path(&source.path));
            }
        }
        true
    }
}

fn complete_group(
    tabs: &mut Vec<Tab>,
    active: &mut usize,
    mru: &mut Vec<usize>,
    navigation: &mut NavigationHistory,
    request: &Request,
    shared: &Editor,
    removed: &mut Vec<u64>,
) {
    if !tabs.iter().any(|tab| request.matches(tab)) {
        return;
    }
    let actual_path = shared.file_path.as_deref().expect("verified file path");
    if let Some(target) = tabs.iter().position(|tab| {
        tab.kind == TabKind::File && tab.editor.save_identity == shared.save_identity
    }) {
        let selection_to_resolve =
            if let Some(placeholder) = tabs.get(*active).filter(|tab| request.matches(tab)) {
                let pending_selection = NavigationSelection::capture(&placeholder.editor);
                let cursor = placeholder.editor.pending_cursor_position;
                let utf16 = placeholder.editor.pending_cursor_utf16;
                let center = placeholder.editor.pending_cursor_center;
                let selection = placeholder.editor.pending_navigation_selection;
                let review = placeholder.editor.ai_review.requested_navigation;
                let editor = &mut tabs[target].editor;
                if cursor.is_some() {
                    editor.pending_cursor_position = cursor;
                    editor.pending_cursor_utf16 = utf16;
                    editor.pending_cursor_center = center;
                    editor.pending_navigation_selection = selection;
                }
                if review.is_some() {
                    editor.ai_review.requested_navigation = review;
                }
                if !editor.bg_loading {
                    editor.apply_pending_file_navigation();
                }
                *active = target;
                Some(pending_selection)
            } else {
                None
            };
        let view = tabs[target].editor.navigation_id;
        for placeholder in tabs.iter().filter(|tab| request.matches(tab)) {
            navigation.redirect_file_open(placeholder.editor.navigation_id, view, actual_path);
        }
        if let Some(pending_selection) = selection_to_resolve
            && !tabs[target].editor.bg_loading
        {
            navigation.resolve_loaded_editor(&tabs[target].editor, pending_selection);
        }
        let pinned = tabs.iter().any(|tab| request.matches(tab) && tab.pinned);
        tabs[target].pinned |= pinned;
        if pinned || !request.preserve_existing_preview {
            tabs[target].preview = false;
        }
        crate::tab_collection::retain(tabs, active, mru, |_, tab| {
            if request.matches(tab) {
                removed.push(tab.editor.navigation_id);
                false
            } else {
                true
            }
        });
    } else {
        for tab in tabs.iter_mut().filter(|tab| request.matches(tab)) {
            let editor = &mut tab.editor;
            let pending = NavigationSelection::capture(editor);
            editor.copy_document_from(shared);
            editor.bg_loading = shared.bg_loading;
            editor.pending_session_restore = shared.pending_session_restore.clone();
            navigation.redirect_file_open(editor.navigation_id, editor.navigation_id, actual_path);
            if !editor.bg_loading {
                editor.restore_reopen_view();
                editor.apply_pending_file_navigation();
                navigation.resolve_loaded_editor(editor, pending);
            }
            if tab.has_unsaved_changes() {
                tab.preview = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn settle(app: &mut App) {
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.tabs.iter().all(|tab| !tab.editor.bg_loading)
        });
    }

    #[test]
    fn case_distinct_files_open_and_save_independently() {
        let directory = tempfile::tempdir().unwrap();
        if !crate::native_paths::enable_case_sensitive_test_directory(directory.path()) {
            return;
        }
        let upper = directory.path().join("Sample.txt");
        let lower = directory.path().join("sample.txt");
        std::fs::write(&upper, "upper\n").unwrap();
        std::fs::write(&lower, "lower\n").unwrap();
        let mut app = App::test_new();
        app.format_on_save = false;
        app.open_file(&upper);
        app.open_file(&lower);
        settle(&mut app);

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].editor.rope, "upper\n");
        assert_eq!(
            app.editor().unwrap().file_path.as_deref(),
            Some(lower.as_path())
        );
        assert_eq!(app.editor().unwrap().rope, "lower\n");
        app.editor_mut().unwrap().insert_char('!');
        app.save_current_file();
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.editor().unwrap().modified
        });
        assert_eq!(std::fs::read_to_string(upper).unwrap(), "upper\n");
        assert_eq!(std::fs::read_to_string(lower).unwrap(), "!lower\n");
    }

    #[test]
    fn verified_case_alias_reuses_unsaved_text_and_pending_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("Mixed.txt");
        let alias = directory.path().join("mixed.txt");
        std::fs::write(&original, "one\ntwo\n").unwrap();
        if !alias.exists() {
            return;
        }
        let mut app = App::test_new();
        app.open_file(&original);
        settle(&mut app);
        app.editor_mut().unwrap().insert_char('!');
        let document = app.editor().unwrap().save_identity;
        app.open_file(&alias);
        app.editor_mut().unwrap().set_cursor(1, 2);
        settle(&mut app);

        assert_eq!(app.tabs.len(), 1);
        let editor = app.editor().unwrap();
        assert_eq!(editor.save_identity, document);
        assert_eq!(editor.file_path.as_deref(), Some(original.as_path()));
        assert_eq!(editor.rope, "!one\ntwo\n");
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
        assert!(editor.modified);
    }

    #[test]
    fn case_alias_can_resolve_while_the_original_tab_is_loading() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("Mixed.txt");
        let alias = directory.path().join("mixed.txt");
        std::fs::write(&original, "one\ntwo\n").unwrap();
        if !alias.exists() {
            return;
        }
        let mut app = App::test_new();
        app.open_file(&original);
        app.open_file(&alias);
        app.editor_mut().unwrap().set_cursor(1, 2);
        settle(&mut app);

        assert_eq!(app.tabs.len(), 1);
        let editor = app.editor().unwrap();
        assert_eq!(editor.file_path.as_deref(), Some(original.as_path()));
        assert_eq!(editor.rope, "one\ntwo\n");
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    }

    #[test]
    fn case_alias_focuses_the_original_tab_in_another_pane() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("Mixed.txt");
        let alias = directory.path().join("mixed.txt");
        let other = directory.path().join("other.txt");
        std::fs::write(&original, "one\ntwo\n").unwrap();
        std::fs::write(&other, "other\n").unwrap();
        if !alias.exists() {
            return;
        }
        let mut app = App::test_new();
        app.open_file(&original);
        app.open_file(&other);
        settle(&mut app);
        app.create_split_group(0);
        let original_group = app.active_group;
        let document = app.editor().unwrap().save_identity;
        app.switch_group(0);
        app.open_file(&alias);
        app.editor_mut().unwrap().set_cursor(1, 2);
        settle(&mut app);

        assert_eq!(app.active_group, original_group);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.groups[0].tabs.len(), 1);
        let editor = app.editor().unwrap();
        assert_eq!(editor.save_identity, document);
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    }

    #[test]
    fn search_preview_alias_keeps_the_original_view() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("Mixed.txt");
        let alias = directory.path().join("mixed.txt");
        std::fs::write(&original, "one\ntwo\n").unwrap();
        if !alias.exists() {
            return;
        }
        let mut app = App::test_new();
        app.enable_preview_tabs = true;
        app.open_file_from_search(&original);
        settle(&mut app);
        let view = app.editor().unwrap().navigation_id;
        app.open_file_from_search(&alias);
        settle(&mut app);

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.editor().unwrap().navigation_id, view);
        assert!(app.tabs[0].preview);
    }

    #[test]
    fn distinct_case_sensitive_preview_replaces_the_previous_preview() {
        let directory = tempfile::tempdir().unwrap();
        if !crate::native_paths::enable_case_sensitive_test_directory(directory.path()) {
            return;
        }
        let upper = directory.path().join("Mixed.txt");
        let lower = directory.path().join("mixed.txt");
        std::fs::write(&upper, "upper\n").unwrap();
        std::fs::write(&lower, "lower\n").unwrap();
        let mut app = App::test_new();
        app.enable_preview_tabs = true;
        app.open_file_preview(&upper);
        settle(&mut app);
        app.open_file_preview(&lower);
        settle(&mut app);

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.editor().unwrap().file_path.as_deref(),
            Some(lower.as_path())
        );
        assert_eq!(app.editor().unwrap().rope, "lower\n");
        assert!(app.tabs[0].preview);
    }
}
