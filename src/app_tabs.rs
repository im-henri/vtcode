use std::path::Path;

use unicode_width::UnicodeWidthStr;

use crate::app::{ActivePane, App, SourceControlRepositoryState, Tab, TabKind};
use crate::editor::Editor;
use crate::explorer::FileExplorer;

fn loaded_file_result(workspace_generation: u64, path: std::path::PathBuf, loaded: crate::editor::LoadedTextFile, skip_open_prompt: bool) -> crate::background::BackgroundResult {
    let result = crate::background::BackgroundResult::FileLoaded {
        workspace_generation, path, rope: loaded.rope, encoding: loaded.encoding,
        file_size: loaded.file_size, last_disk_modified: loaded.last_disk_modified,
        session_restores: Vec::new(), skip_open_prompt,
    };
    match loaded.remote_checkpoint {
        Some(checkpoint) => crate::background::BackgroundResult::RemoteFileResult { checkpoint, result: Box::new(result) },
        None => result,
    }
}

impl App {
    pub(crate) fn queue_background_file_load(
        &mut self,
        path: std::path::PathBuf,
        skip_open_prompt: bool,
    ) {
        let workspace_generation = self.workspace_generation;
        let sender = self.background.sender.clone();
        let activity_path = path.clone();
        let remote = self.remote_workspace.clone().filter(|_| crate::remote::workspace::is_view_key(&path));
        self.background.read_for_file("file-load", &activity_path, true, move || {
            match remote.map_or_else(|| crate::editor::load_text_file(&path), |remote| remote.load_progress(&path,
                crate::remote::text::progress(sender.clone(), workspace_generation, path.clone(), false))) {
                Ok(loaded) => {
                    let _ = sender.send(loaded_file_result(workspace_generation, path, loaded, skip_open_prompt));
                }
                Err(error) => {
                    let _ = sender.send(crate::background::BackgroundResult::FileLoadFailed {
                        workspace_generation,
                        path,
                        error: error.to_string(),
                        session_restores: Vec::new(),
                    });
                }
            }
        });
    }

    /// Load one disk baseline and hydrate every saved view of that path on the
    /// same worker. Recovery buffers and undo snapshots can be very large, so
    /// none of this work belongs on the startup/UI thread.
    pub(crate) fn queue_background_session_file_load(
        &mut self,
        path: std::path::PathBuf,
        mut session_restores: Vec<crate::session::SessionRestore>,
        foreground: bool,
    ) {
        let project_root = self.explorer.root.clone();
        let workspace_generation = self.workspace_generation;
        let sender = self.background.sender.clone();
        let activity_path = path.clone();
        let remote = self.remote_workspace.clone().filter(|_| crate::remote::workspace::is_view_key(&path));
        self.background.read_for_file("session-file-load", &activity_path, foreground, move || {
            crate::session::hydrate_saved_tabs(&project_root,
                session_restores.iter_mut().map(|restore| &mut restore.tab));
            match remote.as_ref().map_or_else(|| crate::editor::load_text_file(&path), |remote| remote.load_progress(&path,
                crate::remote::text::progress(sender.clone(), workspace_generation, path.clone(), false))) {
                Ok(mut loaded) => {
                    crate::session::share_recovered_disk_snapshot(
                        session_restores.iter_mut().map(|restore| &mut restore.tab), &loaded.rope);
                    if let Some(remote) = &remote { remote.check_recovery(&path, &mut loaded, &session_restores); }
                    let result = crate::background::BackgroundResult::FileLoaded {
                        workspace_generation,
                        path,
                        rope: loaded.rope,
                        encoding: loaded.encoding,
                        file_size: loaded.file_size,
                        last_disk_modified: loaded.last_disk_modified,
                        session_restores,
                        skip_open_prompt: true,
                    };
                    let result = match loaded.remote_checkpoint {
                        Some(checkpoint) => crate::background::BackgroundResult::RemoteFileResult { checkpoint, result: Box::new(result) },
                        None => result,
                    };
                    let _ = sender.send(result);
                }
                Err(error) => {
                    let _ = sender.send(crate::background::BackgroundResult::FileLoadFailed {
                        workspace_generation,
                        path,
                        error: error.to_string(),
                        session_restores,
                    });
                }
            }
        });
    }

    /// Create a new Editor pre-configured with App-level settings like language_associations.
    pub(crate) fn new_editor(&self) -> Editor {
        let mut editor = Editor::new();
        editor.language_associations = self.language_associations.clone();
        editor.scope_bubble_rules = self.animation_settings.scope_languages.clone();
        editor.read_only = self.default_read_only;
        editor.word_wrap = self.default_word_wrap;
        editor.word_wrap_column = self.word_wrap_column;
        editor.whitespace_mode = self.default_whitespace_mode;
        editor.tab_size = self.default_tab_size;
        editor.use_tabs = self.default_use_tabs;
        editor.cursor_surrounding_lines = self.default_cursor_surrounding_lines;
        editor
    }

    /// Snapshot editors keep independent text, but inherit the file's selected
    /// language. Look across panes/desks without probing the filesystem.
    pub(crate) fn language_override_for_path(&self, path: &Path) -> Option<String> {
        self.tabs.get(self.active_tab).into_iter()
            .chain(self.tabs.iter())
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| matches!(
                tab.kind,
                TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails
            ))
            .find_map(|tab| {
                tab.editor.file_path.as_deref()
                    .filter(|other| Self::same_open_path(other, path))
                    .and(tab.editor.language_override.as_ref())
            })
            .cloned()
    }

    pub(crate) fn set_editor_language(&mut self, language: Option<&str>) {
        let Some(current) = self.tabs.get(self.active_tab).filter(|tab| matches!(
            tab.kind,
            TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails
        ))
        else { return; };
        let path = current.editor.file_path.clone();
        let identity = current.editor.save_identity;
        for tab in self.tabs.iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            if !matches!(
                tab.kind,
                TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails
            )
                || !(tab.editor.save_identity == identity || path.as_deref().is_some_and(|path| {
                    tab.editor.file_path.as_deref().is_some_and(|other| Self::same_open_path(other, path))
                }))
            {
                continue;
            }
            // Publish only the language change through a live file's shared
            // document. Diff snapshots must never copy text or undo history.
            tab.editor.refresh_document();
            tab.editor.language_override = language.map(str::to_owned);
            tab.editor.publish_document();
        }
        self.invalidate_visible_highlights();
    }

    /// Keep the caller's path spelling as the I/O target. Every file-opening
    /// caller already supplies the path it intends to open (Explorer entries,
    /// Git changes, quick-open results, LSP locations, and CLI arguments).
    /// Re-resolving a relative target against the Explorer root changes its
    /// meaning and can turn a valid Windows path into a missing one.
    pub(crate) fn open_target_path(path: &Path) -> std::path::PathBuf {
        path.to_path_buf()
    }

    pub(crate) fn same_open_path(left: &Path, right: &Path) -> bool {
        if crate::remote::workspace::is_view_key(left) || crate::remote::workspace::is_view_key(right) { return left == right; }
        crate::native_paths::same(left, right)
    }

    pub(crate) fn open_binary_file_info_tab(&mut self, path: &Path) {
        self.binary_open_prompt_skip_once = Some(Self::open_target_path(path));
        self.open_file(path);
    }

    pub(crate) fn validate_diff_state_for_active_tab(&mut self) {
        self.sync_diff_tab();
        if self.diff_mode != crate::app::DiffMode::Off && !self.active_editor_matches_diff_target()
        {
            self.clear_diff_state();
        }
    }

    pub(crate) fn open_file(&mut self, path: &Path) {
        self.open_file_with_preview(path, false);
    }

    pub(crate) fn open_file_preview(&mut self, path: &Path) {
        self.open_file_with_preview(path, self.enable_preview_tabs);
    }

    fn open_file_with_preview(&mut self, path: &Path, preview: bool) {
        self.open_file_with_source(path, preview, None);
    }

    pub(crate) fn open_file_with_preloaded(&mut self, path: &Path, loaded: crate::editor::LoadedTextFile) {
        self.open_file_with_source(path, false, Some(loaded));
    }

    fn open_file_with_source(&mut self, path: &Path, preview: bool, mut loaded: Option<crate::editor::LoadedTextFile>) {
        self.cancel_remote_location();
        self.record_navigation();
        self.open_file_with_preview_inner(path, preview, &mut loaded);
        self.record_navigation();
        if let Some(unused) = loaded {
            // Opening an already-live/shared buffer, or a pending Save As,
            // must not free a large prefetched Rope on the input thread.
            self.background.spawn("remote-location-drop", move || drop(unused));
        }
    }

    fn open_file_with_preview_inner(&mut self, path: &Path, preview: bool, loaded: &mut Option<crate::editor::LoadedTextFile>) {
        if self.pending_save_as.values().any(|(generation, target)|
            *generation == self.workspace_generation && Self::same_open_path(target, path))
        {
            self.notify("Wait for Save As to finish before opening its destination.");
            return;
        }
        self.promote_modified_preview_tabs();
        self.sync_duplicate_file_tabs();
        self.suspend_diff_tab();
        self.auto_save_on_focus_change();
        let open_path = Self::open_target_path(path);
        // Track in recent files
        self.recent_files
            .retain(|recent| !Self::same_open_path(recent, &open_path));
        self.recent_files.insert(0, open_path.clone());
        if self.recent_files.len() > 20 {
            self.recent_files.truncate(20);
        }

        // Opening a regular file exits any active compare/diff view.
        if self.diff_mode != crate::app::DiffMode::Off || self.pending_git_compare_path.is_some() {
            self.clear_diff_state();
        }

        // Check if file is already open in a tab
        for (i, tab) in self.tabs.iter().enumerate() {
            if tab.kind == TabKind::File {
                let matches = tab
                    .editor
                    .file_path
                    .as_ref()
                    .is_some_and(|fp| Self::same_open_path(fp, &open_path));
                if matches {
                    self.tabs[i].preview = false;
                    self.active_tab = i;
                    self.active_pane = ActivePane::Editor;
                    self.sync_diff_tab();
                    self.touch_tab_mru(i);
                    self.refresh_source_control_for_navigation();
                    self.refresh_git_line_status_async();
                    return;
                }
            }
        }
        // Check other split groups for the same file
        for (group_idx, group) in self.groups.iter().enumerate().filter(|(i,_)| !self.navigation.restoring && self.group_in_current_desk(*i)) {
            for (tab_idx, tab) in group.tabs.iter().enumerate() {
                if tab.kind == TabKind::File {
                    let matches = tab
                        .editor
                        .file_path
                        .as_ref()
                        .is_some_and(|fp| Self::same_open_path(fp, &open_path));
                    if matches {
                        self.switch_group(group_idx);
                        self.active_tab = tab_idx;
                        self.pin_preview_tab();
                        self.active_pane = ActivePane::Editor;
                        self.sync_diff_tab();
                        self.touch_tab_mru(tab_idx);
                        self.refresh_source_control_for_navigation();
                        self.refresh_git_line_status_async();
                        return;
                    }
                }
            }
        }

        // Opening an existing document in another desk creates a view of the
        // live buffer, including unsaved edits; it never reloads the disk file.
        let saved_view = self.closed_file_views.get(&open_path).copied().or_else(|| {
            self.closed_file_views.iter().find(|(path, _)| Self::same_open_path(path, &open_path))
                .map(|(_, position)| *position)
        });
        if let Some(source) = self.groups.iter().flat_map(|g| &g.tabs).find(|tab|
            tab.kind == TabKind::File && !tab.editor.bg_loading
                && tab.editor.file_path.as_ref().is_some_and(|path| Self::same_open_path(path,&open_path))) {
            let mut tab = self.clone_desk_tab(source);
            tab.editor.pending_reopen_view = saved_view;
            tab.editor.restore_reopen_view();
            tab.preview = preview && !tab.has_unsaved_changes();
            self.tabs.push(tab);
            self.active_tab = self.tabs.len()-1;
            self.active_pane = ActivePane::Editor;
            self.touch_tab_mru(self.active_tab);
            self.sync_diff_tab();
            self.ensure_active_tab_visible();
            self.refresh_source_control_for_navigation();
            self.refresh_git_line_status_async();
            return;
        }

        let skip_open_prompt = self
            .binary_open_prompt_skip_once
            .as_ref()
            .is_some_and(|skip| Self::same_open_path(skip, &open_path));
        if skip_open_prompt {
            self.binary_open_prompt_skip_once = None;
        }

        // Every file follows the same asynchronous path. Even an apparently
        // small file can live on a slow filesystem, and metadata/header reads
        // are enough to freeze a terminal frame.
        let mut editor = Editor::new_loading(&open_path, 0);
        editor.pending_reopen_view = saved_view;
        editor.language_associations = self.language_associations.clone();
        editor.scope_bubble_rules = self.animation_settings.scope_languages.clone();
        editor.word_wrap_column = self.word_wrap_column;
        editor.whitespace_mode = self.default_whitespace_mode;
        editor.tab_size = self.default_tab_size;
        editor.use_tabs = self.default_use_tabs;
        editor.cursor_surrounding_lines = self.default_cursor_surrounding_lines;
        self.status_message = format!("Loading: {}...", self.display_path(&open_path));
        let replace_preview = if preview {
            self.tabs.iter().position(|tab| tab.kind == TabKind::File && tab.preview)
        } else {
            None
        };
        let mut tab = Tab::new_file(editor);
        tab.preview = preview;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.active_pane = ActivePane::Editor;
        self.touch_tab_mru(self.active_tab);
        // Add the replacement before closing the old preview so a group with
        // only one tab stays in place. Closing also releases its LSP state.
        if let Some(index) = replace_preview {
            // Focus changed above; closing the now-inactive old preview must
            // not be relied on to clear the previous editor's transient state.
            self.clear_transient_editor_state();
            self.clear_file_search_results();
            self.force_close_tab(index);
            self.active_tab = self.tabs.len() - 1;
            self.touch_tab_mru(self.active_tab);
        }
        self.refresh_source_control_for_navigation();

        if let Some(loaded) = loaded.take() {
            let _ = self.background.sender.send(loaded_file_result(self.workspace_generation, open_path, loaded, skip_open_prompt));
        } else { self.queue_background_file_load(open_path, skip_open_prompt); }
    }

    /// Highlight every visible editor viewport on workers. Scrolling and edits
    /// replace each pane's request key. Stateful TextMate jobs yield checkpoints and
    /// resume in bounded slices, including when large-file mode is active.
    pub(crate) fn refresh_visible_highlights_async(&mut self) {
        if let Some(path) = self.tabs.get(self.active_tab)
            .filter(|tab| tab.editor.bg_loading).and_then(|tab| tab.editor.file_path.as_deref()) {
            self.background.prioritize_file_read(path);
        }
        self.refresh_current_editor_highlights_async();
        let Some(tree) = self.split_tree.take() else { return; };
        tree.visit_leaves(&mut |index| {
            if index == self.active_group
                || index >= self.groups.len()
                || self.groups[index].tabs.is_empty()
            {
                return;
            }
            // Project the other pane's editor and request state without changing
            // focus. Reuse the same bounded worker path for normal and diff views.
            self.swap_group_highlight_context(index);
            let saved_diff = self.begin_inactive_diff_view();
            self.refresh_current_editor_highlights_async();
            self.end_inactive_diff_view(saved_diff);
            self.swap_group_highlight_context(index);
        });
        self.split_tree = Some(tree);
    }

    fn swap_group_highlight_context(&mut self, index: usize) {
        let group = &mut self.groups[index];
        std::mem::swap(&mut self.tabs, &mut group.tabs);
        std::mem::swap(&mut self.active_tab, &mut group.active_tab);
        self.swap_group_highlight_requests(index);
    }

    pub(crate) fn swap_group_highlight_requests(&mut self, index: usize) {
        let group = &mut self.groups[index];
        std::mem::swap(&mut self.visible_highlight_key, &mut group.visible_highlight_key);
        std::mem::swap(&mut self.visible_highlight_cancel_token, &mut group.visible_highlight_cancel_token);
        std::mem::swap(&mut self.provisional_highlight_key, &mut group.provisional_highlight_key);
        std::mem::swap(&mut self.provisional_highlight_cancel_token, &mut group.provisional_highlight_cancel_token);
    }

    pub(crate) fn invalidate_visible_highlights(&mut self) {
        self.visible_highlight_key = None;
        self.provisional_highlight_key = None;
        if let Some(token) = self.visible_highlight_cancel_token.take() {
            token.cancel();
        }
        if let Some(token) = self.provisional_highlight_cancel_token.take() {
            token.cancel();
        }
        for group in &mut self.groups {
            group.visible_highlight_key = None;
            group.provisional_highlight_key = None;
            if let Some(token) = group.visible_highlight_cancel_token.take() {
                token.cancel();
            }
            if let Some(token) = group.provisional_highlight_cancel_token.take() {
                token.cancel();
            }
        }
    }

    pub(crate) fn refresh_current_editor_highlights_async(&mut self) {
        // Rendered views own their highlighting. In particular, a Markdown
        // preview shares its source path but has a different language override;
        // scheduling it here would replace the source editor's document cache.
        let Some(editor) = self.tabs.get(self.active_tab)
            .filter(|tab| matches!(tab.kind, TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails))
            .map(|tab| &tab.editor)
        else {
            self.visible_highlight_key = None;
            self.provisional_highlight_key = None;
            if let Some(token) = self.visible_highlight_cancel_token.take() {
                token.cancel();
            }
            if let Some(token) = self.provisional_highlight_cancel_token.take() {
                token.cancel();
            }
            return;
        };
        if editor.bg_loading {
            self.visible_highlight_key = None;
            self.provisional_highlight_key = None;
            if let Some(token) = self.visible_highlight_cancel_token.take() {
                token.cancel();
            }
            if let Some(token) = self.provisional_highlight_cancel_token.take() {
                token.cancel();
            }
            return;
        }
        let Some(file_path) = editor.syntax_file_path() else {
            return;
        };
        let scroll = editor.scroll_offset;
        let viewport_height = editor.viewport_height.max(50);
        let language = editor.syntax_language_token();
        let content_version = editor.content_version;
        let changed_range = editor
            .syntax_dirty_from
            .map(|start| (start, editor.syntax_dirty_through.unwrap_or(start)));
        let theme_key = self.syntax_theme.as_str();
        let diff_active = self.should_render_diff_for_active_editor();
        let (start, end, source_generation) = if diff_active {
            let start = if self.diff_mode == crate::app::DiffMode::Inline {
                // Deleted rows can span many viewports without moving the
                // target file's scroll offset. Follow the rendered inline row.
                let inline_row = if std::sync::Arc::ptr_eq(
                    &self.diff_inline_scroll.cache,
                    &self.diff_render_cache,
                ) && self.diff_render_cache.target_line_for_inline_row(self.diff_inline_scroll.row) == scroll {
                    self.diff_inline_scroll.row
                } else {
                    self.diff_render_cache.inline_row_for_target_line(scroll)
                };
                self.diff_render_cache.inline_rows.get(inline_row)
                    .map_or(0, |row| row.aligned_row)
            } else {
                self.diff_render_cache.aligned_row_for_target_line(scroll)
            };
            let end = (start + viewport_height + 25).min(self.diff_render_cache.rows.len());
            (
                start,
                end,
                self.diff_refresh_generation
                    .wrapping_add(self.git_compare_generation)
                    .wrapping_add(1),
            )
        } else {
            // Keep a small margin in both directions so reverse scrolling can
            // use already-colored rows as well as forward scrolling.
            let start = scroll.saturating_sub(25).min(editor.rope.len_lines());
            let end = (scroll + viewport_height + 25).min(editor.rope.len_lines());
            (start, end, 0)
        };
        if self.visible_highlight_key.as_ref().is_some_and(|current| {
            current.0 == file_path && current.1 == content_version
                && current.4 == theme_key && current.5.as_deref() == language
                && current.6 == source_generation
                && (current.2 == start && current.3 == end
                    || !diff_active && current.2 <= scroll
                        && current.3 >= (scroll + viewport_height).min(editor.rope.len_lines()))
        })
        {
            // Do not cancel useful work on every wheel tick while its
            // prefetched range still covers the entire visible viewport.
            return;
        }
        // Most event-loop visits reuse the same viewport. Only snapshot the
        // rope and allocate owned request keys after the borrowed check fails.
        let rope = (!diff_active).then(|| editor.rope.clone());
        let diff_cache = diff_active.then(|| std::sync::Arc::clone(&self.diff_render_cache));
        let file_path = file_path.to_path_buf();
        let language = language.map(str::to_owned);
        let theme_key = theme_key.to_owned();
        let key = (file_path.clone(), content_version, start, end, theme_key.clone(), language.clone(), source_generation);
        if let Some(token) = self.visible_highlight_cancel_token.take() {
            token.cancel();
        }
        if self.provisional_highlight_key.as_ref() != Some(&key) {
            if let Some(token) = self.provisional_highlight_cancel_token.take() {
                token.cancel();
            }
            self.provisional_highlight_key = None;
        }
        if diff_active {
            self.visible_highlight_key = Some(key);
            let registry_generation = self.highlighter.registry_generation();
            let (syntax_set, theme_set) = self.highlighter.shared_sets();
            let sender = self.background.sender.clone();
            let Some(diff_cache) = diff_cache else {
                return;
            };
            let activity = self.background.activity.start("Highlighting", Some(&file_path));
            let token = self.background.submit_interactive(move |cancel| {
                let _activity = activity.cancellable(cancel.clone()).running();
                use crate::highlight::Highlighter;
                if cancel.is_cancelled() {
                    return;
                }
                let highlighters = diff_cache.syntax_first_lines.each_ref().map(|first_line| {
                    crate::highlight::FragmentHighlighter::new(
                        first_line, Some(&file_path), language.as_deref(), &theme_key,
                        &syntax_set, &theme_set,
                    )
                });
                let mut entries = Vec::with_capacity(end.saturating_sub(start) * 2);
                let mut highlight = |side: usize, idx: usize, text: &str, hash: u64| {
                    let mut line = text.to_string();
                    Highlighter::normalize_line_for_highlighting(&mut line);
                    let spans = highlighters[side].highlight_line(&line);
                    entries.push((idx, hash, spans));
                };
                for row in &diff_cache.rows[start..end] {
                    if cancel.is_cancelled() {
                        return;
                    }
                    if let Some(idx) = row.left_line {
                        highlight(
                            0,
                            idx,
                            row.left_text_window(0, crate::highlight::MAX_SYNTAX_LINE_CHARS),
                            row.left_highlight_hash(),
                        );
                    }
                    if let Some(idx) = row.right_line {
                        highlight(
                            1,
                            idx,
                            row.right_text_window(0, crate::highlight::MAX_SYNTAX_LINE_CHARS),
                            row.right_highlight_hash(),
                        );
                    }
                }
                let _ = sender.send(crate::background::BackgroundResult::HighlightFragments {
                    registry_generation,
                    file_path,
                    theme_key,
                    language,
                    entries,
                });
            });
            self.visible_highlight_cancel_token = Some(token);
            return;
        }

        let Some(rope) = rope else {
            return;
        };
        let job = self.highlighter.prepare_job(
            &rope,
            &file_path,
            content_version,
            changed_range,
            start..end,
            language.as_deref(),
            &theme_key,
        );
        // The cache now owns propagation of these edits. Only accumulate new
        // edits in the editor, so a later newline shifts the correct suffix.
        if let Some(editor) = self.editor_mut() {
            editor.syntax_dirty_from = None;
            editor.syntax_dirty_through = None;
        }
        let Some(job) = job else {
            self.visible_highlight_key = Some(key);
            return;
        };
        let provisional_job = (self.provisional_highlight_key.as_ref() != Some(&key))
            .then(|| self.highlighter.prepare_provisional_job(&job)).flatten();
        let submit_provisional =
            provisional_job.is_some() && self.provisional_highlight_key.as_ref() != Some(&key);
        self.visible_highlight_key = Some(key.clone());
        let (syntax_set, theme_set) = self.highlighter.shared_sets();
        let sender = self.background.sender.clone();
        if submit_provisional {
            self.provisional_highlight_key = Some(key);
            if let Some(token) = self.provisional_highlight_cancel_token.take() {
                token.cancel();
            }
        }
        if let Some(provisional_job) = provisional_job.filter(|_| submit_provisional) {
            let provisional_rope = rope.clone();
            let provisional_syntax_set = std::sync::Arc::clone(&syntax_set);
            let provisional_theme_set = std::sync::Arc::clone(&theme_set);
            let provisional_sender = sender.clone();
            let activity = self.background.activity.start("Highlighting preview", Some(&file_path));
            let token = self.background.submit_interactive(move |cancel| {
                let _activity = activity.cancellable(cancel.clone()).running();
                let batch = provisional_job.run(
                    &provisional_rope,
                    &provisional_syntax_set,
                    &provisional_theme_set,
                    || cancel.is_cancelled(),
                );
                if !cancel.is_cancelled() {
                    let _ = provisional_sender
                        .send(crate::background::BackgroundResult::HighlightBatch(batch));
                }
            });
            self.provisional_highlight_cancel_token = Some(token);
        }
        let activity = self.background.activity.start("Highlighting", Some(&file_path));
        let token = self.background.submit_interactive(move |cancel| {
            let _activity = activity.cancellable(cancel.clone()).running();
            let batch = job.run(&rope, &syntax_set, &theme_set, || cancel.is_cancelled());
            if !cancel.is_cancelled() {
                let _ = sender.send(crate::background::BackgroundResult::HighlightBatch(batch));
            }
        });
        self.visible_highlight_cancel_token = Some(token);
    }

    /// Keep the current preview open without pinning it in the tab bar.
    pub(crate) fn pin_preview_tab(&mut self) {
        if self.active_tab < self.tabs.len() {
            self.tabs[self.active_tab].preview = false;
        }
    }

    pub(crate) fn set_preview_tabs_enabled(&mut self, enabled: bool) {
        self.enable_preview_tabs = enabled;
        self.promote_modified_preview_tabs();
    }

    /// Edits and pinned tabs must never be replaced by subsequent browsing.
    pub(crate) fn promote_modified_preview_tabs(&mut self) -> bool {
        let mut changed = false;
        for tab in self.tabs.iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            if tab.preview && (!self.enable_preview_tabs || tab.pinned || tab.has_unsaved_changes()) {
                tab.preview = false;
                changed = true;
            }
        }
        changed
    }
    pub(crate) fn touch_tab_mru(&mut self, tab_idx: usize) {
        self.cancel_stale_remote_location();
        if let Some(id)=self.tabs.get(tab_idx).and_then(|t| self.desks.membership.get(&t.editor.navigation_id)).copied()
            && let Some(group)=self.desks.groups.get_mut(&id) { group.collapsed=false; }
        if tab_idx < self.tabs.len() && self.tab_mru_order.first() == Some(&tab_idx) { return; }
        self.tab_mru_order.retain(|&i| i != tab_idx && i < self.tabs.len());
        if tab_idx < self.tabs.len() { self.tab_mru_order.insert(0, tab_idx); }
    }

    pub(crate) fn swap_tabs(&mut self, left: usize, right: usize) {
        let moving=self.tabs[left].editor.navigation_id;
        let destination=self.desks.membership.get(&self.tabs[right].editor.navigation_id).copied();
        if let Some(group)=destination {self.desks.membership.insert(moving,group);self.tabs[left].preview=false;}
        else {self.desks.membership.remove(&moving);}
        self.tabs.swap(left, right);
        crate::tab_collection::swapped(&mut self.tab_mru_order, left, right);
    }

    /// A drop beyond the tab strip's contents leaves any named group. Move the
    /// existing view without swapping other tabs or changing their membership.
    pub(crate) fn move_tab_to_end(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else { return; };
        self.desks.membership.remove(&tab.editor.navigation_id);
        tab.preview = false;
        let last = self.tabs.len() - 1;
        self.tabs[index..].rotate_left(1);
        let remap = |i: usize| {
            if i == index { last } else { i - usize::from(i > index) }
        };
        self.active_tab = remap(self.active_tab);
        for i in &mut self.tab_mru_order {
            *i = remap(*i);
        }
        self.tab_scroll_manual_anchor = None;
        self.ensure_active_tab_visible();
    }

    pub(crate) fn move_tab_between_groups(
        &mut self,
        source_group: usize,
        tab_idx: usize,
        target_group: usize,
    ) -> bool {
        // Validate both endpoints before removing a tab, including unsaved tabs.
        if source_group == target_group
            || (target_group != self.active_group && target_group >= self.groups.len())
        {
            return false;
        }

        let source_len = if source_group == self.active_group {
            self.tabs.len()
        } else if source_group < self.groups.len() {
            self.groups[source_group].tabs.len()
        } else {
            return false;
        };
        if tab_idx >= source_len {
            return false;
        }

        let mut tab = if source_group == self.active_group {
            self.tabs.remove(tab_idx)
        } else {
            self.groups[source_group].tabs.remove(tab_idx)
        };
        tab.preview = false;
        self.desks.membership.remove(&tab.editor.navigation_id);

        if source_group == self.active_group {
            self.navigation.move_editor(tab.editor.navigation_id, target_group);
            self.active_tab = self
                .active_tab
                .saturating_sub(usize::from(tab_idx < self.active_tab))
                .min(self.tabs.len().saturating_sub(1));
            crate::tab_collection::removed(&mut self.tab_mru_order, tab_idx);
        } else if source_group < self.groups.len() {
            self.navigation.move_editor(tab.editor.navigation_id, target_group);
            let source = &mut self.groups[source_group];
            crate::tab_collection::removed(&mut source.tab_mru_order, tab_idx);
            source.active_tab = source
                .active_tab
                .saturating_sub(usize::from(tab_idx < source.active_tab))
                .min(source.tabs.len().saturating_sub(1));
        }

        let result = if target_group == self.active_group {
            self.tabs.push(tab);
            true
        } else if target_group < self.groups.len() {
            self.groups[target_group].tabs.push(tab);
            true
        } else {
            false
        };

        // Clean up empty groups from the split tree
        if result && self.split_active() {
            let source_empty = if source_group == self.active_group {
                self.tabs.is_empty()
            } else {
                source_group < self.groups.len() && self.groups[source_group].tabs.is_empty()
            };
            if source_empty {
                self.close_empty_groups();
            }
        }

        result
    }

    /// The language server owns a document, not an individual split view.
    pub(crate) fn close_lsp_document_if_unused(&mut self, path: &Path) {
        if self.file_tab_location_by_path(path).is_none() {
            self.lsp_manager.did_close(path);
        }
    }

    pub(crate) fn new_file(&mut self) {
        self.record_navigation();
        let editor = self.new_editor();
        self.tabs.push(Tab::new_file(editor));
        self.active_tab = self.tabs.len() - 1;
        self.active_pane = ActivePane::Editor;
        self.touch_tab_mru(self.active_tab);
        self.status_message = String::from("New file");
        self.record_navigation();
    }

    pub(crate) fn can_duplicate_tab(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|tab| {
            matches!(tab.kind, TabKind::File | TabKind::GitRevision | TabKind::MarkdownPreview)
                && !tab.editor.bg_loading
        })
    }

    /// Duplicate the requested buffer, including unsaved text, without first
    /// activating it or reloading its contents from disk.
    pub(crate) fn duplicate_tab(&mut self, index: usize) {
        if !self.can_duplicate_tab(index) {
            self.notify("Only loaded text editor tabs can be duplicated");
            return;
        }
        self.sync_duplicate_file_tabs();
        self.tabs[index].editor.break_undo_group();
        let tab = &self.tabs[index];
        let source = &tab.editor;
        let mut editor = self.new_editor();
        editor.rope = source.rope.clone();
        editor.saved_rope = source.saved_rope.clone();
        editor.saved_encoding = source.saved_encoding;
        editor.file_path = source.file_path.clone();
        if source.file_path.is_some() {
            editor.save_identity = source.save_identity;
        }
        editor.cursor_line = source.cursor_line;
        editor.cursor_col = source.cursor_col;
        editor.read_only = source.read_only;
        editor.word_wrap = source.word_wrap;
        editor.word_wrap_column = source.word_wrap_column;
        editor.modified = source.modified;
        editor.content_version = source.content_version;
        editor.encoding = source.encoding;
        editor.cached_line_ending = source.cached_line_ending;
        editor.last_disk_modified = source.last_disk_modified;
        editor.disk_unavailable = source.disk_unavailable;
        editor.large_file_mode = source.large_file_mode;
        editor.rich_features_timed_out = source.rich_features_timed_out;
        editor.file_size = source.file_size;
        editor.language_override = source.language_override.clone();
        editor.tab_size = source.tab_size;
        editor.use_tabs = source.use_tabs;
        editor.copy_document_from(source);
        let mut duplicate = Tab::new_file(editor);
        duplicate.kind = tab.kind;
        self.tabs.push(duplicate);
        self.clear_transient_editor_state();
        self.active_tab = self.tabs.len() - 1;
        self.active_pane = ActivePane::Editor;
        self.touch_tab_mru(self.active_tab);
        self.validate_diff_state_for_active_tab();
        self.ensure_active_tab_visible();
        self.notify("Tab duplicated");
    }

    /// Open settings as a tab, reusing an existing Settings tab in any pane.
    pub(crate) fn open_settings_tab(&mut self) {
        self.flush_live_settings();
        self.global_search_active = false;
        self.settings_scope = crate::settings::SettingsScope::Global;
        self.settings_focus_control = false;
        self.cancel_numeric_setting_edit();

        // The focused group's tabs live in `self.tabs`; other panes keep
        // theirs in `groups`. Prefer the current pane if both have Settings.
        let existing = self.tabs.iter()
            .position(|tab| tab.kind == TabKind::Settings)
            .map(|index| (self.active_group, index))
            .or_else(|| {
                self.groups.iter().enumerate()
                    .filter(|(group, _)| *group != self.active_group && self.group_in_current_desk(*group))
                    .find_map(|(group, state)| {
                        state.tabs.iter()
                            .position(|tab| tab.kind == TabKind::Settings)
                            .map(|index| (group, index))
                    })
            });
        self.clear_transient_editor_state();
        if let Some((group, index)) = existing {
            self.switch_group(group);
            self.active_tab = index;
        } else {
            let mut editor = self.new_editor();
            editor.file_path = Some(std::path::PathBuf::from("⚙ Settings"));
            editor.read_only = true;
            self.tabs.push(Tab {
                editor,
                pinned: false,
                preview: false,
                kind: TabKind::Settings,
                extension_view: None,
                file_search: Default::default(),
                terminal_idx: None,
            });
            self.active_tab = self.tabs.len() - 1;
        }
        self.active_pane = ActivePane::Editor;
        self.settings_editor_active = true;
        self.touch_tab_mru(self.active_tab);
        self.validate_diff_state_for_active_tab();
        self.tab_scroll_manual_anchor = None;
        self.ensure_active_tab_visible();
    }

    pub(crate) fn unsaved_file_count(&self) -> usize {
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| group.tabs.iter()))
            .filter(|tab| tab.has_unsaved_changes())
            .count()
    }

    /// Open keyboard shortcuts as a tab (or switch to existing)
    pub(crate) fn open_keybindings_tab(&mut self) {
        self.terminal_panel_focused = false;
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| t.kind == TabKind::KeyboardShortcuts)
        {
            self.active_tab = idx;
            self.active_pane = ActivePane::Editor;
            return;
        }
        let mut editor = self.new_editor();
        editor.file_path = Some(std::path::PathBuf::from("⌨ Keyboard Shortcuts"));
        self.tabs.push(Tab {
            editor,
            pinned: false,
            preview: false,
            kind: TabKind::KeyboardShortcuts,
            extension_view: None,
            file_search: Default::default(),
            terminal_idx: None,
        });
        self.active_tab = self.tabs.len() - 1;
        self.active_pane = ActivePane::Editor;
    }

    /// Explicit open pins the page; browsing reuses a dedicated extension preview.
    pub(crate) fn open_extension_info_tab(&mut self, extension_id: &str) {
        self.show_extension_info_tab(extension_id, false);
    }

    pub(crate) fn preview_extension_info_tab(&mut self, extension_id: &str) {
        self.show_extension_info_tab(extension_id, true);
    }

    fn show_extension_info_tab(&mut self, extension_id: &str, preview: bool) {
        if let Some(index) = self.tabs.iter().position(|tab| {
            tab.kind == TabKind::ExtensionInfo
                && crate::ui_extension_info::tab_extension_id(tab).as_deref() == Some(extension_id)
        }) {
            self.active_tab = index;
            if !preview { self.tabs[index].preview = false; }
        } else {
            let views = self.extension_views();
            let name = views.iter().find(|ext| ext.id == extension_id)
                .map_or("Unknown Extension", |ext| ext.name.as_str());
            let mut editor = self.new_editor();
            editor.file_path = Some(std::path::PathBuf::from(format!("Extension: {name} [ext-info:{extension_id}]")));
            let tab = Tab { editor, pinned: false, preview, kind: TabKind::ExtensionInfo, extension_view: None, file_search: Default::default(), terminal_idx: None };
            if let Some(index) = self.tabs.iter().position(|tab| tab.kind == TabKind::ExtensionInfo && tab.preview) {
                self.tabs[index] = tab;
                self.active_tab = index;
            } else {
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
            }
        }
        self.active_pane = ActivePane::Editor;
        self.ensure_active_tab_visible();
    }

    fn markdown_preview_tab_locations(&self) -> Vec<(Option<usize>, usize)> {
        let mut locations = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate() {
            if tab.kind == TabKind::MarkdownPreview {
                locations.push((None, idx));
            }
        }
        for (group_idx, group) in self.groups.iter().enumerate() {
            for (tab_idx, tab) in group.tabs.iter().enumerate() {
                if tab.kind == TabKind::MarkdownPreview {
                    locations.push((Some(group_idx), tab_idx));
                }
            }
        }
        locations
    }

    fn markdown_preview_tab_location_by_source(
        &self,
        source_path: &Path,
    ) -> Option<(Option<usize>, usize)> {
        if let Some(idx) = self.tabs.iter().position(|t| {
            t.kind == TabKind::MarkdownPreview && t.editor.file_path.as_deref() == Some(source_path)
        }) {
            return Some((None, idx));
        }
        for (group_idx, group) in self.groups.iter().enumerate() {
            if let Some(tab_idx) = group.tabs.iter().position(|t| {
                t.kind == TabKind::MarkdownPreview
                    && t.editor.file_path.as_deref() == Some(source_path)
            }) {
                return Some((Some(group_idx), tab_idx));
            }
        }
        None
    }

    pub(crate) fn has_markdown_preview_for_path(&self, path: &Path) -> bool {
        self.markdown_preview_tab_location_by_source(path).is_some()
    }

    fn file_tab_location_by_path(&self, path: &Path) -> Option<(Option<usize>, usize)> {
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| t.kind == TabKind::File && t.editor.file_path.as_deref() == Some(path))
        {
            return Some((None, idx));
        }
        for (group_idx, group) in self.groups.iter().enumerate() {
            if let Some(tab_idx) = group.tabs.iter().position(|t| {
                t.kind == TabKind::File && t.editor.file_path.as_deref() == Some(path)
            }) {
                return Some((Some(group_idx), tab_idx));
            }
        }
        None
    }

    fn tab_at_location(&self, location: (Option<usize>, usize)) -> Option<&Tab> {
        let (group_idx, tab_idx) = location;
        match group_idx {
            None => self.tabs.get(tab_idx),
            Some(gi) => self.groups.get(gi).and_then(|g| g.tabs.get(tab_idx)),
        }
    }

    fn tab_mut_at_location(&mut self, location: (Option<usize>, usize)) -> Option<&mut Tab> {
        let (group_idx, tab_idx) = location;
        match group_idx {
            None => self.tabs.get_mut(tab_idx),
            Some(gi) => self
                .groups
                .get_mut(gi)
                .and_then(|g| g.tabs.get_mut(tab_idx)),
        }
    }

    fn activate_markdown_preview_tab(&mut self, index: usize) {
        self.clear_transient_editor_state();
        self.active_tab = index;
        self.active_pane = ActivePane::Editor;
        self.touch_tab_mru(index);
        self.validate_diff_state_for_active_tab();
        self.tab_scroll_manual_anchor = None;
        self.ensure_active_tab_visible();
    }

    pub(crate) fn toggle_markdown_preview_tab(&mut self) {
        if self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.kind == TabKind::MarkdownPreview)
        {
            self.close_tab(self.active_tab);
            self.notify("Markdown preview closed");
            return;
        }

        let Some(active_tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if active_tab.kind != TabKind::File {
            self.notify("Markdown preview is only available for Markdown files");
            return;
        }

        if !active_tab.editor.is_markdown() {
            self.notify("Markdown preview is only available for Markdown files");
            return;
        }

        let Some(source_path) = active_tab.editor.file_path.clone() else {
            self.notify("Save the Markdown file before opening preview");
            return;
        };
        if active_tab.editor.large_file_mode {
            self.notify("Markdown preview is paused for this large file");
            return;
        }

        // Reuse a preview in this pane. A preview manually moved to another
        // pane must not redirect this action away from the source's pane.
        if let Some((None, index)) = self.markdown_preview_tab_location_by_source(&source_path) {
            self.activate_markdown_preview_tab(index);
            self.sync_markdown_preview_state();
            return;
        }

        let (source_rope, source_file_size, source_scroll, source_version) =
            if let Some(ed) = self.editor() {
                (
                    ed.rope.clone(),
                    ed.file_size,
                    ed.scroll_offset,
                    ed.content_version,
                )
            } else {
                return;
            };

        let mut editor = self.new_editor();
        editor.rope = source_rope;
        editor.file_path = Some(source_path);
        editor.read_only = true;
        editor.modified = false;
        editor.file_size = source_file_size;
        editor.update_large_file_mode(self.large_file_threshold_mb.saturating_mul(1024 * 1024));
        editor.content_version = source_version;
        editor.cached_visible_line_count = None;
        editor.scroll_offset = source_scroll;
        editor.wrap_scroll_offset = 0;
        editor.word_wrap = false;
        editor.horizontal_scroll = 0;
        editor.language_override = Some("Markdown".into());
        self.tabs.push(Tab {
            editor,
            pinned: false,
            preview: false,
            kind: TabKind::MarkdownPreview,
            extension_view: None,
            file_search: Default::default(),
            terminal_idx: None,
        });
        let preview_idx = self.tabs.len() - 1;

        self.markdown_preview = true;
        self.activate_markdown_preview_tab(preview_idx);
        self.notify("Markdown preview opened");
    }

    pub(crate) fn sync_markdown_preview_state(&mut self) -> bool {
        // Preview creation and close paths maintain this flag. Avoid walking
        // every tab on each idle event-loop tick when preview is not in use.
        if !self.markdown_preview {
            return false;
        }
        let preview_locs = self.markdown_preview_tab_locations();
        let mut changed = false;
        let has_preview = !preview_locs.is_empty();
        if self.markdown_preview != has_preview {
            self.markdown_preview = has_preview;
            changed = true;
        }
        if !has_preview {
            return changed;
        }

        for preview_loc in preview_locs {
            let Some(source_path) = self
                .tab_at_location(preview_loc)
                .and_then(|t| t.editor.file_path.clone())
            else {
                continue;
            };
            let Some(source_loc) = self.file_tab_location_by_path(&source_path) else {
                continue;
            };

            let source_version = self
                .tab_at_location(source_loc)
                .map(|t| t.editor.content_version);
            let preview_version = self
                .tab_at_location(preview_loc)
                .map(|t| t.editor.content_version);
            if let (Some(source_version), Some(preview_version)) = (source_version, preview_version)
            {
                if source_version != preview_version {
                    let (source_rope, source_file_size, source_large_file_mode, source_timed_out) =
                        if let Some(source_tab) = self.tab_at_location(source_loc) {
                            (
                                source_tab.editor.rope.clone(),
                                source_tab.editor.file_size,
                                source_tab.editor.large_file_mode,
                                source_tab.editor.rich_features_timed_out,
                            )
                        } else {
                            continue;
                        };
                    if let Some(preview_tab) = self.tab_mut_at_location(preview_loc) {
                        preview_tab.editor.rope = source_rope;
                        preview_tab.editor.fold_hint_cache.get_mut().clear();
                        preview_tab.editor.minimap_cache.get_mut().clear();
                        preview_tab.editor.file_size = source_file_size;
                        preview_tab.editor.large_file_mode = source_large_file_mode;
                        preview_tab.editor.rich_features_timed_out = source_timed_out;
                        preview_tab.editor.content_version = source_version;
                        preview_tab.editor.cached_visible_line_count = None;
                        preview_tab.editor.modified = false;
                        preview_tab.editor.read_only = true;
                        changed = true;
                    }
                }
            }

            // Preview input owns visual rows; translate its pending position back
            // to the source before applying the normal source-to-preview sync.
            let requested_source = self.tab_mut_at_location(preview_loc)
                .and_then(|tab| tab.editor.markdown_preview_scroll.as_mut())
                .and_then(|view| view.pending_source.take());
            if let Some(line) = requested_source {
                if let Some(tab) = self.tab_mut_at_location(source_loc) {
                    let editor = &mut tab.editor;
                    let line = line.min(editor.line_count().saturating_sub(1));
                    if editor.word_wrap {
                        let row =
                            editor.wrapped_row_for_position(line, 0, editor.effective_wrap_width());
                        editor.set_wrap_scroll_offset(row);
                    } else {
                        editor.scroll_offset = line;
                    }
                    changed = true;
                }
            }
            let source_scroll = if let Some(source_tab) = self.tab_at_location(source_loc) {
                source_tab.editor.scroll_offset
            } else {
                continue;
            };
            if let Some(preview_tab) = self.tab_mut_at_location(preview_loc) {
                if requested_source.is_some() {
                    if let Some(view) = &mut preview_tab.editor.markdown_preview_scroll {
                        // Acknowledge the source's clamped position without
                        // snapping back to the first row of a wrapped paragraph.
                        view.source_scroll = source_scroll;
                    }
                }
                if preview_tab.editor.scroll_offset != source_scroll {
                    preview_tab.editor.scroll_offset = source_scroll;
                    changed = true;
                }
                if preview_tab.editor.wrap_scroll_offset != 0 {
                    preview_tab.editor.wrap_scroll_offset = 0;
                    changed = true;
                }
            }
        }

        changed
    }

    pub(crate) fn reopen_closed_tab(&mut self) {
        if let Some((path, _, _)) = self.closed_tab_paths.pop_back() {
            // The loader reports an unavailable path asynchronously. Avoid a
            // foreground filesystem round-trip, especially for remote roots.
            self.open_file(&path);
        } else {
            self.notify("No recently closed tabs");
        }
    }

    /// Drop view state owned by the previous directory after its session has
    /// been saved. Settings and the destination session rebuild the layout.
    fn clear_workspace_view(&mut self) {
        self.explorer_expanding.clear();
        self.logs.reset_views();
        self.remote_connection = Default::default();
        self.side_notice = Default::default();
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        self.cancel_workspace_replace();
        self.cancel_remote_location();
        self.cancel_remote_disk_checks();
        self.clear_workspace_lsp_syncs();
        self.clear_drag_states();
        self.reset_hover_states();
        self.clear_transient_editor_state();
        self.clear_diff_state();
        self.diff_tabs = Default::default();
        self.clear_file_search_results();
        self.clear_global_search();
        self.search_history = Default::default();
        for field in 0..4 { *self.global_search_field_history_mut(field) = Default::default(); }
        self.search_query.clear();
        self.search_cursor = 0;
        self.search_selection_anchor = None;
        self.search_active = false;
        self.search_pre_cursor = None;
        self.global_search_active = false;
        self.global_search_replace_query.clear();
        self.search_include_pattern.clear();
        self.search_exclude_pattern.clear();
        self.global_search_field_cursors = [0; 4];
        self.global_search_field_selection_anchors = [None; 4];
        self.search_focus_field = 0;
        self.global_search_replace_visible = false;
        self.symbol_list_active = false;
        self.source_symbol_work.cancel_picker();
        self.source_symbol_work.cancel_outline();
        self.symbol_list_items.clear();
        self.symbol_list_filtered.clear();
        self.outline_items.clear();
        self.outline_scroll = 0;
        self.outline_selected = 0;
        self.navigation = Default::default();
        self.closed_tab_paths.clear();
        self.closed_file_views.clear();
        self.tab_mru_order.clear();
        self.tab_scroll_offset = 0;
        self.tab_scroll_manual_anchor = None;
        self.active_tab = 0;
        self.active_group = 0;
        self.desks = Default::default();
        self.active_pane = ActivePane::Explorer;
        self.split_tree = None;
        self.split_dividers.clear();
        self.split_vertical = true;
        self.split_ratio = 0.5;
        self.sidebar_view = crate::app::SidebarView::Explorer;
        self.panel_tab = 1;
        self.sc_graph_collapsed = false;
        self.sc_graph_height = 10;
        self.zen_mode = false;
        self.zen_pre_state = None;
        self.markdown_preview = false;
        self.breadcrumb_dropdown_active = false;
        self.breadcrumb_dropdown_generation = self.breadcrumb_dropdown_generation.wrapping_add(1);
        self.breadcrumb_dropdown_items.clear();
        self.breadcrumb_segments.clear();
        self.binary_open_prompt_active = false;
        self.binary_open_prompt_path = None;
        self.binary_open_prompt_commands.clear();
        self.binary_open_prompt_skip_once = None;
        self.confirm_close_active = false;
        self.save_as_active = false;
        self.settings_editor_active = false;
        crate::ui_lsp_settings::activate(self, crate::ui_lsp_settings::Action::Close);
        self.tab_context_active = false;
        self.goto_line_active = false;
        self.dismiss_vtcomplete();
        self.completion_index_ui = Default::default();
        self.lsp_completion_active = false;
        self.lsp_signature_active = false;
        self.lsp_code_action_active = false;
        self.lsp_references_active = false;
        self.extension_pointer_capture = None;
        self.pending_custom_recoveries.clear();
        self.chat.layout.editors.clear();
        self.chat.layout.active = None;
        self.chat.layout.rendering = None;
        self.chat.layout.placements.clear();
        self.chat.layout.surfaces.clear();
        if let Some(token) = self.visible_highlight_cancel_token.take() {
            token.cancel();
        }
        if let Some(token) = self.provisional_highlight_cancel_token.take() {
            token.cancel();
        }
        self.visible_highlight_key = None;
        self.provisional_highlight_key = None;
        self.session_baseline = None;
        self.tabs.clear();
        self.groups.clear();
    }

    pub(crate) fn change_workspace(&mut self, path: &std::path::Path) {
        if let Some(remote) = self.remote_workspace.clone() {
            if path == self.explorer.root { return; }
            self.workspace_open_generation = self.workspace_open_generation.wrapping_add(1);
            let request = self.workspace_open_generation;
            let source = self.workspace_generation;
            let path = path.to_path_buf();
            let sender = self.background.sender.clone();
            self.background.spawn("remote-open-workspace", move || {
                let result = remote.open_workspace(&path).map_err(|error| error.to_string());
                let _ = sender.send(crate::background::BackgroundResult::RemoteWorkspaceOpened { request, source, result });
            });
            return;
        }
        self.change_workspace_ready(path, None);
    }

    pub(crate) fn change_workspace_ready(&mut self, path: &std::path::Path, remote: Option<crate::remote::workspace::Workspace>) {
        if remote.is_some() || path.is_dir() {
            if crate::portable::workspace_dir(path)
                == crate::portable::workspace_dir(&self.explorer.root)
            {
                return;
            }
            if self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
                .any(|tab| tab.kind == TabKind::ExtensionView && tab.has_unsaved_changes())
            {
                self.notify("Save custom editor changes before opening another folder");
                return;
            }
            if self.extension_manager.has_busy_runtimes()
                && self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
                    .any(|tab| tab.kind == TabKind::ExtensionView)
            {
                self.notify("Wait for the custom editor operation before opening another folder");
                return;
            }
            // Save against the old root before replacing either the Explorer
            // or the tabs. Keep the current workspace intact if recovery fails.
            if let Err(error) = self.try_save_session() {
                self.notify(format!("Could not save current workspace: {error}"));
                return;
            }
            self.clear_workspace_view();
            self.remote_workspace = remote;
            // Keep folder switches responsive: canonicalization and directory
            // traversal are completed by the Explorer worker.
            self.explorer = FileExplorer::new_deferred(path.to_path_buf());
            self.cancel_source_control_reads();
            if let Some(cancel) = self.quick_open_cancel_token.take() {
                cancel.cancel();
            }
            self.quick_open_generation = self.quick_open_generation.wrapping_add(1);
            self.quick_open_results.clear();
            self.quick_open_selected = 0;
            self.bg_quick_open_running = false;
            // A new workspace uses a new traversal service immediately. Drop
            // a large old file index off-thread rather than freeing every
            // retained path during the folder-switch input event.
            let previous_quick_open = std::mem::take(&mut self.quick_open_index);
            self.background
                .spawn("quick-open-index-drop", move || drop(previous_quick_open));
            self.repositories = Default::default();
            self.source_control_repository_state = SourceControlRepositoryState::Checking;
            self.source_control_last_refresh = None;
            self.source_control_initialize_area = None;
            self.source_control_files.clear();
            self.source_control_selection = Default::default();
            self.source_control_selected = 0;
            self.source_control_scroll = 0;
            self.set_source_control_visibility(Default::default());
            self.source_control_actions_area = None;
            self.source_control_refresh_area = None;
            self.source_control_input_mode = false;
            self.source_control_commit_msg.clear();
            self.source_control_commit_cursor = 0;
            self.source_control_commit_selection_anchor = None;
            self.sc_context_active = false;
            self.sc_context_target = None;
            self.sc_context_targets.clear();
            self.close_hunk_popup();
            self.sc_discard_confirm_active = false;
            self.sc_discard_request = None;
            self.sc_stage_all_commit_request = None;
            self.close_commit_details_tab();
            self.git_branch_picker_active = false;
            self.resizing_sc_graph = false;
            self.sc_graph_click_start = None;
            if self.source_control_action_menu_open() || self.source_control_filter_menu_open() {
                self.context_menu_active = false;
            }
            self.clear_git_history();
            self.git_log_scroll = 0;
            self.git_log_selected = 0;
            self.cached_git_branch.clear();
            self.bg_git_branch_pending = false;
            self.settings_scope = crate::settings::SettingsScope::Global;
            self.cancel_numeric_setting_edit();
            self.startup_theme_placeholder = None;
            self.initialize_settings();
            let previous_extensions = std::mem::replace(
                &mut self.extension_manager,
                crate::extensions::ExtensionManager::new(path),
            );
            self.background
                .spawn("extension-manager-drop", move || drop(previous_extensions));
            let previous_lsp = std::mem::replace(
                &mut self.lsp_manager,
                crate::lsp::LspManager::new_with_wake(
                    path.to_path_buf(),
                    self.background.wake_handle(),
                ),
            );
            self.lsp_manager.remote_workspace = self.remote_workspace.clone();
            // A language server is allowed a two-second graceful shutdown.
            // Never spend that grace period on the terminal event thread.
            self.background
                .spawn("lsp-shutdown", move || drop(previous_lsp));
            if let Some(session) = crate::session::load_session(path) {
                self.restore_session(&session);
            }
            self.apply_session_settings();
            self.sync_activity_bar_selection();
            self.capture_session_baseline();
            self.extensions_selected = 0;
            self.extensions_scroll = 0;
            self.refresh_palette_commands();
            self.refresh_explorer_async();
            self.request_syntax_assets_async();
            self.request_extension_scan();
            self.refresh_source_control_async();
            self.refresh_git_branch_async();
            self.workspace_watcher_active = false;
            if let Some(remote) = &self.remote_workspace {
                let _ = remote.client.notify(crate::remote::Command::Watch);
            } else {
                self.background.start_file_watcher(path.to_path_buf());
            }
            if self.quick_open_active {
                self.update_quick_open();
            }
            self.notify(format!("Opened folder: {}", self.display_path(path)));
        } else {
            self.notify(format!("Not a directory: {}", path.display()));
        }
    }

    fn tab_ui_width(tab: &Tab) -> usize {
        let display_width = if let Some(revision) = tab
            .editor
            .blame
            .revision
            .as_ref()
            .filter(|_| tab.kind == TabKind::GitRevision)
        {
            revision.tab_title().width()
        } else if tab.kind == TabKind::CommitDetails {
            "Commit details".width()
        } else if tab.kind == TabKind::MarkdownPreview {
            "Preview: ".width() + tab.editor.file_name().width()
        } else if matches!(tab.kind, TabKind::Diff(_)) {
            tab.editor.file_name().width() + " (Diff)".width()
        } else if tab.kind == TabKind::File {
            tab.editor.display_name().width()
        } else {
            tab.editor.file_name().width()
        };
        let mod_len: usize = if tab.has_unsaved_changes() { 2 } else { 0 };
        let pin_len: usize = if tab.pinned { 4 } else { 0 };
        let icon_len: usize = if matches!(tab.kind, TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails) { 2 } else { 0 };
        let deleted_prefix_len: usize = if tab.kind == TabKind::File && tab.editor.disk_unavailable
        {
            1
        } else {
            0
        };
        let close_len: usize = 3;
        1 + pin_len + mod_len + icon_len + deleted_prefix_len + display_width + close_len
    }

    fn placed_tab_width(&self, tab: &Tab) -> usize {
        if let TabKind::Chat(key) = tab.kind {
            let title = self.chat.editor_tab_title(key);
            title.width() + 6 + if tab.pinned { 4 } else { 0 }
        } else { Self::tab_ui_width(tab) }
    }

    fn visible_last_tab_index(
        &self,
        tabs: &[Tab],
        scroll_offset: usize,
        area_width: usize,
    ) -> Option<usize> {
        if tabs.is_empty() || scroll_offset >= tabs.len() || area_width == 0 {
            return None;
        }

        let mut used = if scroll_offset > 0 { 2 } else { 0 }; // "◂ "
        let mut last = scroll_offset;
        for (idx, tab) in tabs.iter().enumerate().skip(scroll_offset) {
            let w = self.placed_tab_width(tab);
            if idx > scroll_offset && used + w > area_width {
                break;
            }
            used += w;
            last = idx;
        }

        if last + 1 < tabs.len() {
            while last > scroll_offset && used + 2 > area_width {
                used = used.saturating_sub(self.placed_tab_width(&tabs[last]));
                last -= 1;
            }
        }

        Some(last)
    }

    /// Tab widths are at least four cells, so removing the leftmost tab can
    /// only advance the last visible index, even when the two-cell scroll
    /// arrow appears. Search offsets instead of revisiting every intervening
    /// viewport when switching between distant tabs.
    fn first_tab_offset_showing(
        &self,
        mut first: usize,
        mut last: usize,
        active: usize,
        width: usize,
    ) -> usize {
        while first < last {
            let middle = first + (last - first) / 2;
            if self
                .visible_last_tab_index(&self.tabs, middle, width)
                .is_some_and(|end| end >= active)
            {
                last = middle;
            } else {
                first = middle + 1;
            }
        }
        first
    }

    /// Ensure active tab is visible by adjusting tab_scroll_offset.
    pub(crate) fn ensure_active_tab_visible(&mut self) {
        if self.tab_strip_scroll.is_some() {
            if crate::ui_chrome_tabs::strip_scroll_position(self, self.active_group).is_some() {
                return;
            }
            self.tab_strip_scroll = None;
        }
        if self.tabs.is_empty() {
            self.tab_scroll_offset = 0;
            self.tab_scroll_manual_anchor = None;
            return;
        }

        if let Some(anchor_tab) = self.tab_scroll_manual_anchor {
            if self.active_tab == anchor_tab {
                self.tab_scroll_offset = self.tab_scroll_offset.min(self.max_tab_scroll_offset());
                return;
            }
            self.tab_scroll_manual_anchor = None;
        }

        if let Some(area) = self.tab_bar_area.filter(|_| !self.desks.membership.is_empty()) {
            let hits=crate::ui_chrome_tabs::TabBarHits::new(self,area);
            if !hits.tabs.iter().any(|(index,_,_)| *index == self.active_tab) {
                self.tab_scroll_offset=self.active_tab;
            }
            return;
        }

        if self.active_tab < self.tab_scroll_offset {
            self.tab_scroll_offset = self.active_tab;
        }

        let area_width = self.tab_bar_area.map_or(0, |a| usize::from(a.width.saturating_sub(crate::ui_desks::controls(self,a,self.active_group).2)));
        if area_width == 0 {
            let max_visible = 8;
            if self.active_tab >= self.tab_scroll_offset + max_visible {
                self.tab_scroll_offset = self.active_tab.saturating_sub(max_visible - 1);
            }
            return;
        }

        if self
            .visible_last_tab_index(&self.tabs, self.tab_scroll_offset, area_width)
            .is_some_and(|last| self.active_tab > last)
        {
            self.tab_scroll_offset = self.first_tab_offset_showing(
                self.tab_scroll_offset + 1,
                self.active_tab.min(self.tabs.len()),
                self.active_tab,
                area_width,
            );
        }

        // When we're already showing the last tab, shift left as far as possible
        // while keeping the active tab visible so the strip doesn't leave trailing gaps.
        if let Some(last_visible) =
            self.visible_last_tab_index(&self.tabs, self.tab_scroll_offset, area_width)
        {
            if last_visible + 1 >= self.tabs.len() {
                if self.tab_scroll_offset > 0
                    && self
                        .visible_last_tab_index(&self.tabs, self.tab_scroll_offset - 1, area_width)
                        .is_some_and(|last| last >= self.active_tab)
                {
                    self.tab_scroll_offset = self.first_tab_offset_showing(
                        0,
                        self.tab_scroll_offset - 1,
                        self.active_tab,
                        area_width,
                    );
                }
            }
        }
    }

    /// Compute the maximum meaningful tab scroll offset for the current tab bar.
    /// Returns the highest offset where the last tab is still visible.
    pub(crate) fn max_tab_scroll_offset(&self) -> usize {
        let area_width = self.tab_bar_area.map_or(0, |a| usize::from(a.width.saturating_sub(crate::ui_desks::controls(self,a,self.active_group).2)));
        self.compute_max_tab_scroll(&self.tabs, area_width)
    }

    fn compute_max_tab_scroll(&self, tabs: &[Tab], area_width: usize) -> usize {
        if tabs.is_empty() || area_width == 0 {
            return 0;
        }
        // Group headings have their own width and collapsed members have none.
        // Let manual scrolling reach every group, without pulling it back to
        // the active file until the user actually selects a different tab.
        if !self.desks.membership.is_empty() && tabs.iter().any(|t| self.tab_group(t).is_some()) {
            return tabs.len().saturating_sub(1);
        }
        let mut used = 0usize;
        for i in (0..tabs.len()).rev() {
            let w = self.placed_tab_width(&tabs[i]);
            let left_arrow = if i > 0 { 2 } else { 0 };
            if used + w + left_arrow > area_width {
                return (i + 1).min(tabs.len().saturating_sub(1));
            }
            used += w;
        }
        0
    }
}

#[cfg(test)]
#[path = "preview_tabs_tests.rs"]
mod preview_tests;

#[cfg(test)]
#[path = "app_tabs_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
#[path = "app_tab_lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
#[path = "app_reopen_tests.rs"]
mod reopen_tests;

#[cfg(test)]
mod tests {
    use crate::app::{App, Tab, TabKind};
    use crate::editor::Editor;
    use std::path::{Path, PathBuf};

    #[test]
    fn markdown_preview_button_opens_and_reuses_a_tab_in_the_clicked_pane() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{Terminal, backend::TestBackend};

        for ((name, language), split) in [
            ("preview-button.md", None),
            ("preview-button.MD", None),
            ("preview-button.txt", Some("Markdown")),
            ("preview-button", Some("Markdown")),
        ].into_iter().flat_map(|case| [false, true].map(|split| (case, split))) {
            let mut app = App::test_new();
            let mut source = Editor::test_with_text("# Unsaved Markdown\n");
            source.file_path = Some(name.into());
            source.modified = true;
            source.content_version = 3;
            let source_id = source.navigation_id;
            app.tabs = vec![Tab::new_file(source)];
            app.active_tab = 0;
            if split {
                app.tabs.insert(0, Tab::new_file(Editor::test_with_text("other pane")));
                app.active_tab = 1;
                app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
            }
            app.active_pane = crate::app::ActivePane::Editor;
            app.show_breadcrumbs = true;
            app.show_panel = false;
            app.show_terminal_sidebar = false;
            if let Some(language) = language {
                app.execute_editor_command(&crate::app_menu::language_command_id(language));
            }
            let group = app.active_group;
            let group_count = app.groups.len();
            let mut terminal = Terminal::new(TestBackend::new(180, 30)).unwrap();
            let click_preview = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
                terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
                let button = app.breadcrumb_markdown_preview_area.expect("preview button");
                for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
                    crate::mouse::handle_mouse(app, MouseEvent {
                        kind, column: button.x + 2, row: button.y, modifiers: KeyModifiers::NONE,
                    });
                }
            };

            click_preview(&mut app, &mut terminal);
            assert_eq!(app.active_group, group);
            assert_eq!(app.groups.len(), group_count);
            assert_eq!(app.tabs.len(), 2);
            assert_eq!(app.active_tab, 1);
            assert_eq!(app.tabs[0].editor.navigation_id, source_id);
            assert!(app.tabs[0].editor.modified);
            assert_eq!(app.tabs[1].kind, TabKind::MarkdownPreview);
            assert!(app.tabs[1].editor.read_only);
            assert_eq!(app.tabs[1].editor.rope.to_string(), "# Unsaved Markdown\n");
            assert_eq!(app.tab_mru_order.first(), Some(&1));

            let preview_id = app.tabs[1].editor.navigation_id;
            app.active_tab = 0;
            app.tabs[0].editor.rope.insert(0, "Updated\n\n");
            app.tabs[0].editor.content_version += 1;
            click_preview(&mut app, &mut terminal);
            assert_eq!(app.active_group, group);
            assert_eq!(app.groups.len(), group_count);
            assert_eq!(app.tabs.len(), 2);
            assert_eq!(app.active_tab, 1);
            assert_eq!(app.tabs[1].editor.navigation_id, preview_id);
            assert_eq!(app.tabs[1].editor.rope.to_string(), "Updated\n\n# Unsaved Markdown\n");

            app.close_tab(app.active_tab);
            assert_eq!(app.active_group, group);
            assert_eq!(app.groups.len(), group_count);
            assert_eq!(app.tabs.len(), 1);
            assert_eq!(app.editor().unwrap().navigation_id, source_id);
            assert!(app.editor().unwrap().modified);
            assert!(!app.markdown_preview);
        }
    }

    #[test]
    fn markdown_preview_keeps_source_highlights_when_focus_changes() {
        let mut app = App::test_new();
        let path = PathBuf::from("colored-preview.md");
        let mut editor = Editor::test_with_text("# Heading\n\n**bold** and `code`\n");
        editor.file_path = Some(path.clone());
        let rope = editor.rope.clone();
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        let job = app.highlighter.prepare_job(
            &rope, &path, 0, None, 0..rope.len_lines(), None, &app.syntax_theme,
        ).unwrap();
        let (syntaxes, themes) = app.highlighter.shared_sets();
        assert!(app.highlighter.apply_batch_with_outcome(job.run(&rope, &syntaxes, &themes, || false)).accepted());
        app.editor_mut().unwrap().syntax_dirty_from = None;
        app.editor_mut().unwrap().syntax_dirty_through = None;
        app.refresh_visible_highlights_async();
        let expected = app.highlighter.highlighted_line_if_cached(
            "# Heading\n", 0, Some(&path), None, &app.syntax_theme,
        ).unwrap();

        app.toggle_markdown_preview_tab();
        assert_eq!(app.tabs[app.active_tab].kind, TabKind::MarkdownPreview);
        app.create_split_group_dir(app.active_tab, crate::app::SplitDirection::Horizontal);
        // The preview carries an explicit Markdown language override; the source
        // uses extension detection. It must never replace the source's cache.
        for group in [1, 0, 1, 0] {
            app.switch_group(group);
            app.refresh_visible_highlights_async();
            app.poll_background_results();
            let actual = app.highlighter.highlighted_line_if_cached(
                "# Heading\n", 0, Some(&path), None, &app.syntax_theme,
            ).expect("opening or focusing a preview must preserve source syntax colors");
            assert_eq!(actual, expected);
            let preview_key = if group == 1 { &app.visible_highlight_key }
                else { &app.groups[1].visible_highlight_key };
            assert!(preview_key.is_none(), "rendered previews do not need source highlight jobs");
        }
    }

    #[test]
    fn theme_preview_recolors_unfocused_file_and_escape_restores_it() {
        use crate::app::{EditorGroup, SplitDirection, SplitNode};
        use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};
        let mut app = App::test_new();
        for (key, name, foreground) in [("preview-a", "Preview A", "#123456"), ("preview-b", "Preview B", "#abcdef")] {
            let mut theme = crate::app_theme::load_builtin_themes().remove(0);
            theme.config_key = key.into();
            theme.syntax_theme = key.into();
            theme.name = name.into();
            theme.workbench_colors.insert("editor.foreground".into(), serde_json::json!(foreground));
            theme.token_colors.clear();
            app.loaded_themes.push(theme);
        }
        let mut settings = Tab::new_file(Editor::new());
        settings.kind = TabKind::Settings;
        app.tabs = vec![settings];
        app.active_tab = 0;
        app.active_group = 0;
        app.settings_editor_active = true;
        let mut file = Editor::test_with_text("ZEBRA\n");
        file.file_path = Some(PathBuf::from("preview.rs"));
        file.viewport_height = 10;
        let mut pane = EditorGroup::new();
        pane.tabs.push(Tab::new_file(file));
        app.groups = vec![EditorGroup::new(), pane];
        app.split_tree = Some(SplitNode::Split { direction: SplitDirection::Horizontal, children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)], ratios: vec![0.5,0.5] });

        fn assert_rendered(app: &mut App, expected: Color) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                app.poll_background_results();
                if app.highlighter.highlighted_line_if_cached("ZEBRA\n", 0, Some(Path::new("preview.rs")), None, &app.syntax_theme).is_some() { break; }
                assert!(std::time::Instant::now() < deadline, "unfocused preview did not receive highlighting");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let mut terminal = Terminal::new(TestBackend::new(50,10)).unwrap();
            terminal.draw(|f| crate::ui_helpers::draw_split_editor_for_group(f, app, Rect::new(0,0,50,10), 1)).unwrap();
            let cell = terminal.backend().buffer().content.iter().find(|c| c.symbol() == "Z").expect("visible source token");
            assert_eq!(cell.fg, expected);
            assert_eq!(app.active_group, 0);
            assert_eq!(app.tabs[app.active_tab].kind, TabKind::Settings);
        }
        app.set_theme_by_key("preview-a");
        assert_rendered(&mut app, Color::Rgb(18,52,86));
        app.open_settings_dropdown("theme", "Preview A");
        app.preview_settings_dropdown_value("theme", "Preview B");
        assert_rendered(&mut app, Color::Rgb(171,205,239));
        // Rapid previews must not leave a stale worker painting the old theme.
        app.preview_settings_dropdown_value("theme", "Preview A");
        app.preview_settings_dropdown_value("theme", "Preview B");
        app.close_settings_dropdown();
        assert_rendered(&mut app, Color::Rgb(18,52,86));
        app.open_palette();
        app.palette.query = "Preferences: Color Theme Preview B".into();
        app.update_palette_filter();
        assert_rendered(&mut app, Color::Rgb(171,205,239));
        crate::input_pickers::handle_palette_key(
            &mut app, crossterm::event::KeyCode::Esc, crossterm::event::KeyModifiers::NONE,
        );
        assert_rendered(&mut app, Color::Rgb(18,52,86));
        // Reapplying the same key with edited color overrides needs a new job.
        app.settings_effective.workbench_color_customizations.insert("editor.foreground".into(), serde_json::json!("#fedcba"));
        app.set_theme_by_key("preview-a");
        assert_rendered(&mut app, Color::Rgb(254,220,186));
    }

    #[test]
    fn visible_python_highlights_match_crlf_editor_lines() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("def greet(name):\r\n    return name\r\n");
        let path = PathBuf::from("sample.py");
        editor.file_path = Some(path.clone());
        editor.viewport_height = 10;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        app.refresh_visible_highlights_async();
        assert!(
            app.highlighter
                .highlighted_line_if_cached(
                    "def greet(name):\n",
                    0,
                    Some(path.as_path()),
                    None,
                    app.syntax_theme.as_str(),
                )
                .is_none()
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            app.poll_background_results();
            if app
                .highlighter
                .highlighted_line_if_cached(
                    "def greet(name):\n",
                    0,
                    Some(path.as_path()),
                    None,
                    app.syntax_theme.as_str(),
                )
                .is_some()
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("CRLF Python highlight batch never matched the rendered LF cache key");
    }

    #[test]
    fn distant_viewport_schedules_one_provisional_pass_and_cancels_it_on_scroll() {
        let mut app = App::test_new();
        let source = "int value = std::vector<int>{1, 2}.size(); // comment\n".repeat(20_000);
        let mut editor = Editor::test_with_text(&source);
        editor.file_path = Some(PathBuf::from("large.cpp"));
        editor.scroll_offset = 10_000;
        editor.viewport_height = 50;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        app.refresh_visible_highlights_async();
        let first_key = app.provisional_highlight_key.clone().unwrap();
        let first_token = app.provisional_highlight_cancel_token.clone().unwrap();
        assert!(!first_token.is_cancelled());

        // Repainting the same viewport must not enqueue or cancel it again.
        app.refresh_visible_highlights_async();
        assert_eq!(app.provisional_highlight_key.as_ref(), Some(&first_key));
        assert!(!first_token.is_cancelled());

        app.editor_mut().unwrap().scroll_offset = 15_000;
        app.refresh_visible_highlights_async();
        assert!(first_token.is_cancelled());
        assert_ne!(app.provisional_highlight_key.as_ref(), Some(&first_key));
    }

    #[test]
    #[ignore = "manual comparative release benchmark"]
    fn benchmark_unchanged_highlight_request_allocations() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&"let value = 1;\n".repeat(1_000));
        editor.file_path = Some(PathBuf::from("workspace/application/src/modules/editor/components/highlight_state.rs"));
        editor.language_override = Some("Rust".into());
        editor.scroll_offset = 100;
        editor.viewport_height = 50;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.refresh_current_editor_highlights_async();
        let mut before = Vec::new(); let mut after = Vec::new();
        for sample in 0..7 {
            for borrowed in [sample % 2 == 0, sample % 2 != 0] {
                let started = std::time::Instant::now();
                for _ in 0..20_000 {
                    if borrowed { app.refresh_current_editor_highlights_async(); }
                    else {
                        // Original unchanged-viewport path created owned keys
                        // and snapshotted the rope before checking for reuse.
                        let editor = app.editor().unwrap();
                        let path = editor.syntax_file_path().unwrap().to_path_buf();
                        let language = editor.syntax_language_token().map(str::to_owned);
                        let theme = app.syntax_theme.clone();
                        let rope = editor.rope.clone();
                        let key = (path.clone(), editor.content_version, 75, 175, theme.clone(), language.clone(), 0);
                        std::hint::black_box((app.visible_highlight_key.as_ref() == Some(&key), rope));
                    }
                }
                let elapsed = started.elapsed();
                if borrowed { after.push(elapsed); } else { before.push(elapsed); }
            }
        }
        before.sort(); after.sort();
        eprintln!("unchanged highlight viewport / 20,000 visits: owned={:?} borrowed={:?}", before[3], after[3]);
    }

    #[test]
    fn completed_edited_highlights_are_not_reset_by_result_polling() {
        let mut app = App::test_new();
        let path = PathBuf::from("completed-edit.rs");
        let mut editor = Editor::test_with_text("let value = 1;\n");
        editor.file_path = Some(path.clone());
        editor.content_version = 1;
        editor.mark_syntax_dirty_range(0, 0);
        let rope = editor.rope.clone();
        app.tabs.push(Tab::new_file(editor));
        let job = app
            .highlighter
            .prepare_job(
                &rope,
                &path,
                1,
                Some((0, 0)),
                0..rope.len_lines(),
                None,
                &app.syntax_theme,
            )
            .unwrap();
        let (syntaxes, themes) = app.highlighter.shared_sets();
        let batch = job.run(&rope, &syntaxes, &themes, || false);
        app.background
            .sender
            .send(crate::background::BackgroundResult::HighlightBatch(batch))
            .unwrap();
        app.poll_background_results();
        assert!(
            app.highlighter
                .highlighted_line_by_index_if_cached(0, Some(&path), None, &app.syntax_theme, 1,)
                .is_some(),
            "result polling must not clear freshly completed colors"
        );
        assert!(app.editor().unwrap().syntax_dirty_from.is_none());
        assert!(
            !app.background.has_active_jobs(),
            "a completed viewport must stay idle"
        );
    }

    #[test]
    fn find_query_changes_reuse_completed_syntax_colors_and_jobs() {
        let mut app = App::test_new();
        let path = PathBuf::from("find-stable.rs");
        let mut editor = Editor::test_with_text("let value = calculate(42);\n");
        editor.file_path = Some(path.clone());
        let rope = editor.rope.clone();
        app.tabs.push(Tab::new_file(editor));
        let job = app
            .highlighter
            .prepare_job(
                &rope,
                &path,
                0,
                None,
                0..rope.len_lines(),
                None,
                &app.syntax_theme,
            )
            .unwrap();
        let (syntaxes, themes) = app.highlighter.shared_sets();
        let batch = job.run(&rope, &syntaxes, &themes, || false);
        app.background
            .sender
            .send(crate::background::BackgroundResult::HighlightBatch(batch))
            .unwrap();
        app.poll_background_results();
        let key = app.visible_highlight_key.clone();
        let before = app
            .highlighter
            .highlighted_line_by_index_if_cached(0, Some(&path), None, &app.syntax_theme, 0)
            .unwrap()
            .0;
        for query in ["v", "va", "value", "calculate", "value"] {
            app.search_query = query.into();
            app.execute_palette_command("find");
            app.refresh_visible_highlights_async();
            assert_eq!(app.visible_highlight_key, key);
            assert!(!app.background.has_active_jobs());
            let after = app
                .highlighter
                .highlighted_line_by_index_if_cached(0, Some(&path), None, &app.syntax_theme, 0)
                .unwrap()
                .0;
            assert!(std::rc::Rc::ptr_eq(&before, &after));
        }
    }

    #[test]
    fn scrolling_inside_prefetch_keeps_the_pending_highlight_worker() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&"let value = 1;\n".repeat(20_000));
        editor.file_path = Some(PathBuf::from("scroll-prefetch.rs"));
        editor.scroll_offset = 10_000;
        editor.viewport_height = 50;
        app.tabs.push(Tab::new_file(editor));
        app.refresh_visible_highlights_async();
        let key = app.visible_highlight_key.clone();
        let token = app.visible_highlight_cancel_token.clone().unwrap();
        for scroll in [10_005, 10_010, 9_995, 10_020] {
            app.editor_mut().unwrap().scroll_offset = scroll;
            app.refresh_visible_highlights_async();
            assert_eq!(app.visible_highlight_key, key);
            assert!(!token.is_cancelled());
        }
    }

    #[test]
    fn converged_syntax_highlighting_stays_dormant_without_changes() {
        let mut app = App::test_new();
        let source = "fn main() { println!(\"ready\"); }\n".repeat(200);
        let path = PathBuf::from("stable.rs");
        let mut editor = Editor::test_with_text(&source);
        editor.file_path = Some(path.clone());
        editor.viewport_height = 60;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        app.refresh_visible_highlights_async();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            app.poll_background_results();
            if app.highlighter.document_state_converged(&path, 0)
                && !app.background.has_active_jobs()
                && !app.background.has_pending_results()
            {
                break;
            }
            std::thread::yield_now();
        }
        assert!(app.highlighter.document_state_converged(&path, 0));
        assert!(!app.background.has_active_jobs());
        assert!(!app.background.has_pending_results());

        let settled_key = app.visible_highlight_key.clone();
        let wake_generation = app.background.wake_handle().generation();
        for _ in 0..100 {
            app.refresh_visible_highlights_async();
        }

        assert_eq!(app.visible_highlight_key, settled_key);
        assert!(!app.background.has_active_jobs());
        assert!(!app.background.has_pending_results());
        assert_eq!(app.background.wake_handle().generation(), wake_generation);
    }

    fn make_tab(name: &str, pinned: bool, modified: bool) -> Tab {
        let mut editor = Editor::new();
        editor.file_path = Some(std::path::PathBuf::from(name));
        editor.modified = modified;
        Tab {
            editor,
            pinned,
            preview: false,
            kind: TabKind::File,
            extension_view: None,
            file_search: Default::default(),
            terminal_idx: None,
        }
    }

    #[test]
    fn open_target_path_preserves_a_relative_workspace_path() {
        let path = Path::new("C_Workplace/CPS_202X_dev/src_cc/main.cpp");
        assert_eq!(App::open_target_path(path), path);
    }

    #[test]
    fn open_target_path_preserves_parent_components() {
        let path = Path::new("workspace/nested/../src/main.rs");
        assert_eq!(App::open_target_path(path), path);
    }

    #[test]
    fn tab_ui_width_basic() {
        let tab = make_tab("main.rs", false, false);
        let w = App::tab_ui_width(&tab);
        // 1 + 0(pin) + 0(mod) + 2(icon) + 0(deleted) + 7(name) + 3(close) = 13
        assert_eq!(w, 13);
    }

    #[test]
    fn tab_ui_width_pinned_modified() {
        let tab = make_tab("a.rs", true, true);
        let w = App::tab_ui_width(&tab);
        // 1 + 4(pin) + 2(mod) + 2(icon) + 0(deleted) + 4(name) + 3(close) = 16
        assert_eq!(w, 16);
    }

    #[test]
    fn tab_ui_width_reserves_cached_deleted_marker() {
        let mut tab = make_tab("main.rs", false, false);
        tab.editor.disk_unavailable = true;
        assert_eq!(App::tab_ui_width(&tab), 14);
    }

    #[test]
    fn visible_last_tab_empty() {
        let result = App::test_new().visible_last_tab_index(&[], 0, 100);
        assert_eq!(result, None);
    }

    #[test]
    fn visible_last_tab_single() {
        let tabs = vec![make_tab("x.rs", false, false)];
        let result = App::test_new().visible_last_tab_index(&tabs, 0, 100);
        assert_eq!(result, Some(0));
    }

    #[test]
    fn markdown_preview_scroll_follows_source_tab() {
        let mut source_editor = Editor::new();
        source_editor.file_path = Some(PathBuf::from("README.md"));
        source_editor.scroll_offset = 0;
        source_editor.content_version = 1;
        let source_tab = Tab::new_file(source_editor);

        let mut preview_editor = Editor::new();
        preview_editor.file_path = Some(PathBuf::from("README.md"));
        preview_editor.scroll_offset = 12;
        preview_editor.content_version = 1;
        let preview_tab = Tab {
            editor: preview_editor,
            pinned: false,
            preview: false,
            kind: TabKind::MarkdownPreview,
            extension_view: None,
            file_search: Default::default(),
            terminal_idx: None,
        };

        let mut app = App::new(PathBuf::from("."));
        app.tabs = vec![source_tab, preview_tab];
        app.active_tab = 1;
        app.markdown_preview = true;

        assert!(app.sync_markdown_preview_state());
        assert_eq!(app.tabs[0].editor.scroll_offset, 0);
        assert_eq!(app.tabs[1].editor.scroll_offset, 0);

        app.tabs[0].editor.scroll_offset = 3;
        app.tabs[1].editor.scroll_offset = 9;
        assert!(app.sync_markdown_preview_state());
        assert_eq!(app.tabs[0].editor.scroll_offset, 3);
        assert_eq!(app.tabs[1].editor.scroll_offset, 3);
    }

    // ── Split view tab operation tests ──────────────────────────────────

    #[test]
    fn create_split_group_dir_creates_two_groups() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        app.active_tab = 1;

        app.create_split_group_dir(1, SplitDirection::Horizontal);

        assert!(app.split_active());
        // The moved tab should now be in the active group
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].editor.file_name(), "b.rs");
        // The other tab stays in group 0
        assert!(
            app.groups
                .iter()
                .any(|g| g.tabs.iter().any(|t| t.editor.file_name() == "a.rs"))
        );
    }

    #[test]
    fn create_split_group_dir_from_single_tab() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![make_tab("only.rs", false, false)];
        app.active_tab = 0;

        app.create_split_group_dir(0, SplitDirection::Vertical);

        assert!(!app.split_active());
        assert!(app.split_tree.is_none());
        // Moving a sole tab within its pane leaves a normal single-pane layout.
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].editor.file_name(), "only.rs");
    }

    #[test]
    fn create_split_group_dir_out_of_bounds_is_noop() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![make_tab("a.rs", false, false)];
        app.active_tab = 0;

        app.create_split_group_dir(5, SplitDirection::Horizontal);

        assert!(!app.split_active());
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn switch_group_swaps_active_state() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        app.active_tab = 0;

        app.create_split_group_dir(1, SplitDirection::Horizontal);
        // b.rs is now in the active group, a.rs in group 0
        let active_before = app.active_group;
        let other = if active_before == 0 { 1 } else { 0 };

        app.switch_group(other);

        assert_eq!(app.active_group, other);
        // The previously active group's tabs are now saved
        assert!(app.tabs.iter().any(|t| t.editor.file_name() == "a.rs"));
    }

    #[test]
    fn switch_group_same_group_is_noop() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        app.create_split_group_dir(1, SplitDirection::Horizontal);

        let group_before = app.active_group;
        app.switch_group(group_before);
        assert_eq!(app.active_group, group_before);
    }

    #[test]
    fn switch_group_out_of_bounds_is_noop() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        app.create_split_group_dir(1, SplitDirection::Horizontal);

        let group_before = app.active_group;
        app.switch_group(99);
        assert_eq!(app.active_group, group_before);
    }

    #[test]
    fn move_tab_between_groups_moves_tab() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
            make_tab("c.rs", false, false),
        ];
        app.active_tab = 0;

        app.create_split_group_dir(2, SplitDirection::Horizontal);
        // c.rs is in new active group, a.rs+b.rs in other group
        let source = app.active_group;
        let target = if source == 0 { 1 } else { 0 };

        // Move c.rs from active group to the other group
        let result = app.move_tab_between_groups(source, 0, target);
        assert!(result);
    }

    #[test]
    fn move_tab_between_groups_empty_source_closes_group() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        app.active_tab = 0;

        app.create_split_group_dir(1, SplitDirection::Horizontal);
        // b.rs is in new active group (1 tab), a.rs is in the other
        let source = app.active_group;
        let target = if source == 0 { 1 } else { 0 };

        // Move the only tab out of the active group
        app.move_tab_between_groups(source, 0, target);

        // Source group should be cleaned up - split should collapse
        assert!(!app.split_active());
    }

    #[test]
    fn move_tab_between_groups_invalid_groups_returns_false() {
        let mut app = App::test_new();
        app.tabs = vec![make_tab("a.rs", false, false)];

        let result = app.move_tab_between_groups(0, 0, 1);
        assert!(!result);
        assert_eq!(app.tabs.len(), 1, "invalid drops must not discard the source tab");
        assert_eq!(app.tabs[0].editor.file_name(), "a.rs");
    }

    #[test]
    fn moving_an_earlier_tab_preserves_the_sources_selected_tab() {
        use crate::app::SplitDirection;
        for active_source in [false, true] {
            let mut app = App::test_new();
            app.tabs = ["a.rs", "b.rs", "c.rs", "destination.rs"].into_iter()
                .map(|name| make_tab(name, false, false)).collect();
            app.active_tab = 1;
            app.create_split_group_dir(3, SplitDirection::Horizontal);
            if active_source { app.switch_group(0); }
            assert!(app.move_tab_between_groups(0, 0, 1));
            app.switch_group(0);
            assert_eq!(app.editor().unwrap().file_name(), "b.rs");
            assert_eq!(app.active_tab, 0);
        }
    }

    #[test]
    fn splitting_an_earlier_tab_preserves_the_sources_selected_tab() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = ["a.rs", "b.rs", "c.rs"].into_iter()
            .map(|name| make_tab(name, false, false)).collect();
        app.active_tab = 1;
        app.create_split_group_dir(0, SplitDirection::Horizontal);
        app.switch_group(0);
        assert_eq!(app.editor().unwrap().file_name(), "b.rs");
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn force_close_tab_in_split_cleans_up_empty_group() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        app.active_tab = 0;

        app.create_split_group_dir(1, SplitDirection::Horizontal);
        // b.rs in active group (single tab), a.rs in other group
        assert!(app.split_active());

        // Close the only tab in active group
        app.force_close_tab(0);

        // Split should collapse since one group is now empty
        assert!(!app.split_active());
        // Remaining tab should be loaded
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].editor.file_name(), "a.rs");
    }

    #[test]
    fn force_close_tab_in_split_keeps_split_with_remaining_tabs() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
            make_tab("c.rs", false, false),
        ];
        app.active_tab = 0;

        // Split off c.rs, leaving a.rs+b.rs in original group
        app.create_split_group_dir(2, SplitDirection::Horizontal);
        // Switch to the group with a.rs+b.rs
        let other = if app.active_group == 0 { 1 } else { 0 };
        app.switch_group(other);

        assert_eq!(app.tabs.len(), 2);
        app.force_close_tab(0);

        // Split should still be active (1 tab in each group)
        assert!(app.split_active());
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn close_empty_groups_exits_split_when_no_groups_remain() {
        let mut app = App::test_new();
        app.tabs = vec![];
        // Manually set up empty split state
        app.groups.push(crate::app::EditorGroup::new());
        app.groups.push(crate::app::EditorGroup::new());
        app.active_group = 0;

        app.close_empty_groups();

        assert!(!app.split_active());
        assert_eq!(app.active_group, 0);
        assert!(app.split_tree.is_none());
    }

    #[test]
    fn close_empty_groups_collapses_to_single_pane() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        app.active_tab = 0;

        app.create_split_group_dir(1, SplitDirection::Horizontal);
        // Now clear one group's tabs manually to simulate
        let other = if app.active_group == 0 { 1 } else { 0 };
        app.groups[other].tabs.clear();

        app.close_empty_groups();

        assert!(!app.split_active());
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn multiple_splits_create_three_groups() {
        use crate::app::SplitDirection;
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
            make_tab("c.rs", false, false),
        ];
        app.active_tab = 0;

        // First split: move b.rs out
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        assert!(app.split_active());

        // Switch back to group with a.rs+c.rs
        let other = if app.active_group == 0 { 1 } else { 0 };
        app.switch_group(other);

        // Second split: move c.rs out (if it's still there)
        if app.tabs.len() > 1 {
            app.create_split_group_dir(1, SplitDirection::Vertical);
        }

        // Should have at least 2 groups
        assert!(app.split_active());
        assert!(app.groups.len() >= 2);
    }

    #[test]
    fn compute_max_tab_scroll_empty() {
        assert_eq!(App::test_new().compute_max_tab_scroll(&[], 100), 0);
    }

    #[test]
    fn compute_max_tab_scroll_all_fit() {
        let tabs = vec![
            make_tab("a.rs", false, false),
            make_tab("b.rs", false, false),
        ];
        // Each tab ~13 chars wide (1+0+0+2+1+4+3=11? let's compute)
        // "a.rs" name width=4, so 1+0+0+2+1+4+3=11 (no deleted prefix though)
        // With 100 chars width, both tabs fit → max scroll = 0
        assert_eq!(App::test_new().compute_max_tab_scroll(&tabs, 100), 0);
    }

    #[test]
    fn compute_max_tab_scroll_overflow() {
        // Create 10 tabs. Each ~14 chars wide. Total ~140. With width=50, max scroll should be > 0
        let tabs: Vec<Tab> = (0..10)
            .map(|i| make_tab(&format!("file{i}.rs"), false, false))
            .collect();
        let max = App::test_new().compute_max_tab_scroll(&tabs, 50);
        // Max scroll should be less than tabs.len()-1 (which would be 9)
        assert!(max < 9, "max scroll {} should be less than 9", max);
        assert!(max > 0, "max scroll should be > 0 since tabs overflow");
        // At max scroll, all remaining tabs should fit
        let mut used = 2usize; // left arrow
        for tab in tabs.iter().skip(max) {
            used += App::tab_ui_width(tab);
        }
        assert!(
            used <= 50,
            "at max scroll {max}, used {used} should fit in 50"
        );
    }

    #[test]
    fn compute_max_tab_scroll_single_wide_tab() {
        let tabs = vec![make_tab("a.rs", false, false)];
        assert_eq!(App::test_new().compute_max_tab_scroll(&tabs, 50), 0);
    }

    #[test]
    fn compute_max_tab_scroll_zero_width() {
        let tabs = vec![make_tab("a.rs", false, false)];
        assert_eq!(App::test_new().compute_max_tab_scroll(&tabs, 0), 0);
    }

    #[test]
    fn close_tab_pinned_stays_open() {
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("pinned.rs", true, false),
            make_tab("normal.rs", false, false),
        ];
        app.active_tab = 0;
        app.close_tab(0);
        // Pinned tab should NOT be closed
        assert_eq!(app.tabs.len(), 2);
        assert!(app.tabs[0].pinned);
    }

    #[test]
    fn edge_case_bulk_tab_menus_stop_at_confirmation_and_keep_pinned_tabs() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        for command in ["tab_close_all", "tab_close_others", "tab_close_right", "tab_close_left"] {
            for mouse in [false, true] {
                let mut app = App::test_new();
                app.tabs = vec![
                    make_tab("first.rs", false, false),
                    make_tab("dirty.rs", false, true),
                    make_tab("pinned.rs", true, false),
                    make_tab("last.rs", false, false),
                ];
                app.tab_context_tab = if command == "tab_close_left" { 3 } else { 0 };
                app.tab_context_active = true;
                app.tab_context_pos = (0, 0);
                let index = App::tab_context_items().iter().position(|item| item.1 == command).unwrap();
                app.tab_context_selected = index;
                if mouse {
                    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40)).unwrap();
                    terminal.draw(|frame| crate::ui_chrome::draw_tab_context_menu(frame, &mut app)).unwrap();
                    let event = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 3, row: index as u16 + 1, modifiers: KeyModifiers::NONE };
                    assert!(crate::mouse_dialogs::handle_mouse_dialogs(&mut app, &event, event.column, event.row));
                } else {
                    assert_eq!(crate::input_dialogs::handle_tab_context_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Some(false));
                }
                assert!(app.confirm_close_active, "{command}, mouse={mouse}");
                assert_eq!(app.tabs[app.confirm_close_tab].editor.file_name(), "dirty.rs");
                assert!(app.tabs.iter().any(|tab| tab.pinned));
                assert_eq!(app.tabs.len(), if command == "tab_close_left" { 4 } else { 3 });
            }
        }
    }

    #[test]
    fn edge_case_close_chords_finish_with_pinned_or_unsaved_tabs() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for pinned in [true, false] {
            for ctrl in [true, false] {
                let mut app = App::test_new();
                app.tabs = vec![make_tab("keep.rs", pinned, !pinned), make_tab("close.rs", false, false)];
                app.active_tab = 0;
                assert!(!crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)));
                assert!(app.chord_pending);
                assert!(!crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Char('w'), if ctrl { KeyModifiers::CONTROL } else { KeyModifiers::NONE })));
                assert!(!app.chord_pending);
                assert_eq!(app.tabs.len(), 1);
                assert_eq!(app.tabs[0].editor.file_name(), "keep.rs");
                assert_eq!(app.confirm_close_active, !pinned);
            }
        }
    }

    #[test]
    fn edge_case_close_group_preserves_unsaved_work_and_other_splits() {
        for modified in [false, true] {
            let mut app = App::test_new();
            app.tabs = vec![make_tab("other.rs", false, false), make_tab("active.rs", false, modified)];
            app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
            assert_eq!(app.editor().unwrap().file_name(), "active.rs");
            app.execute_palette_command("close_group_tabs");
            assert_eq!(app.confirm_close_active, modified);
            assert_eq!(app.tabs.len(), 1);
            assert_eq!(app.editor().unwrap().file_name(), if modified { "active.rs" } else { "other.rs" });
            assert_eq!(app.split_active(), modified);
        }
    }

    #[test]
    fn close_tab_modified_prompts_confirm() {
        let mut app = App::test_new();
        app.tabs = vec![make_tab("dirty.rs", false, true)];
        app.active_tab = 0;
        app.close_tab(0);
        // Modified file tab should trigger confirm dialog, NOT close
        assert_eq!(app.tabs.len(), 1);
        assert!(app.confirm_close_active);
        assert_eq!(app.confirm_close_tab, 0);
    }

    #[test]
    fn close_tab_unmodified_closes_immediately() {
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("clean.rs", false, false),
            make_tab("other.rs", false, false),
        ];
        app.active_tab = 0;
        app.close_tab(0);
        // Unmodified tab should close immediately
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].editor.file_name(), "other.rs");
    }

    #[test]
    fn closing_last_file_dismisses_find_widget() {
        let mut app = App::test_new();
        app.tabs = vec![make_tab("only.rs", false, false)];
        app.search_active = true;
        app.search_input_focused = true;
        app.hovered_search_toggle = Some(2);
        app.search_pre_cursor = Some((4, 2, 1));
        app.search_selection_range = Some((4, 2, 4, 8));

        app.force_close_tab(0);

        assert!(app.tabs.is_empty());
        assert!(!app.search_active);
        assert!(!app.search_input_focused);
        assert_eq!(app.hovered_search_toggle, None);
        assert_eq!(app.search_pre_cursor, None);
        assert_eq!(app.search_selection_range, None);
    }

    #[test]
    fn closing_one_file_keeps_the_remaining_files_own_find_widget() {
        let mut app = App::test_new();
        app.tabs = vec![
            make_tab("first.rs", false, false),
            make_tab("second.rs", false, false),
        ];
        app.tabs[1].file_search.search_active = true;
        app.tabs[1].file_search.search_query = "second query".into();
        app.search_active = true;
        app.search_query = "first query".into();
        app.search_input_focused = false;

        app.force_close_tab(0);

        assert_eq!(app.tabs.len(), 1);
        assert!(app.search_active);
        assert_eq!(app.search_query, "second query");
    }

    #[test]
    fn close_tab_out_of_bounds_noop() {
        let mut app = App::test_new();
        app.tabs = vec![make_tab("a.rs", false, false)];
        app.close_tab(5);
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn close_tab_pinned_and_modified() {
        let mut app = App::test_new();
        app.tabs = vec![make_tab("both.rs", true, true)];
        app.active_tab = 0;
        app.close_tab(0);
        // Pinned check takes precedence over modified check
        assert_eq!(app.tabs.len(), 1);
        assert!(!app.confirm_close_active);
    }
}
