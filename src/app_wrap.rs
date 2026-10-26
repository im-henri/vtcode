use rustc_hash::FxHashMap;

use crate::app::{App, EditorGroup, SplitNode, Tab, TabKind};
use crate::background::{BackgroundWorkers, CancellationToken, WrapLayoutTarget};
use crate::editor::Editor;
use crate::editor_wrap::WrapRowOffsets;

pub(crate) struct PendingWrapLayout {
    target: WrapLayoutTarget,
    cancel: CancellationToken,
    visible: bool,
}

fn wrap_width(editor: &Editor) -> usize {
    editor.effective_wrap_width()
}

impl WrapLayoutTarget {
    fn from_editor(editor: &Editor) -> Self {
        Self {
            editor_id: editor.navigation_id,
            content_version: editor.content_version,
            wrap_revision: editor.wrap_revision,
            len_bytes: editor.rope.len_bytes(),
            wrap_width: wrap_width(editor),
            tab_size: editor.tab_size,
            folded_ranges: editor.folded_ranges.iter().map(|(&a, &b)| (a, b)).collect(),
        }
    }

    fn matches(&self, editor: &Editor) -> bool {
        editor.word_wrap
            && !editor.large_file_mode
            && !editor.bg_loading
            && self.editor_id == editor.navigation_id
            && self.content_version == editor.content_version
            && self.wrap_revision == editor.wrap_revision
            && self.len_bytes == editor.rope.len_bytes()
            && self.wrap_width == wrap_width(editor)
            && self.tab_size == editor.tab_size
            && editor
                .folded_ranges
                .iter()
                .map(|(&a, &b)| (a, b))
                .eq(self.folded_ranges.iter().copied())
    }
}

fn queue_editor_layout(
    tab: &mut Tab,
    pending: &mut FxHashMap<u64, PendingWrapLayout>,
    background: &mut BackgroundWorkers,
) {
    if !matches!(
        tab.kind,
        TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails
    ) {
        return;
    }
    let editor = &mut tab.editor;
    if !editor.word_wrap || editor.large_file_mode || editor.bg_loading {
        return;
    }
    editor.refresh_wrap_caches();
    // Warm panes need no snapshot, owned request key, or fold/path copies.
    if !editor.needs_background_wrap_layout(wrap_width(editor)) {
        return;
    }
    if let Some(job) = pending.get_mut(&editor.navigation_id)
        && job.target.matches(editor)
    {
        job.visible = true;
        return;
    }
    if let Some(job) = pending.remove(&editor.navigation_id) {
        job.cancel.cancel();
    }
    let target = WrapLayoutTarget::from_editor(editor);
    let request = target.clone();
    let rope = editor.rope.clone();
    let sender = background.sender.clone();
    let activity = background
        .activity
        .start("Wrapping", editor.file_path.as_deref());
    let cancel = background.submit_interactive(move |cancel| {
        let _activity = activity.cancellable(cancel.clone()).running();
        let Some(offsets) = Editor::build_wrap_row_offsets(
            &rope,
            request.wrap_width,
            request.tab_size,
            &request.folded_ranges,
            || cancel.is_cancelled(),
        ) else {
            return;
        };
        let Some(offsets) =
            WrapRowOffsets::from_offsets_cancellable(offsets, || cancel.is_cancelled())
        else {
            return;
        };
        if !cancel.is_cancelled() {
            let _ = sender.send(crate::background::BackgroundResult::WrapLayout {
                target: request,
                offsets,
            });
        }
    });
    pending.insert(
        editor.navigation_id,
        PendingWrapLayout {
            target,
            cancel,
            visible: true,
        },
    );
}

fn queue_split_layouts(
    node: &SplitNode,
    groups: &mut [EditorGroup],
    active_group: usize,
    pending: &mut FxHashMap<u64, PendingWrapLayout>,
    background: &mut BackgroundWorkers,
) {
    match node {
        SplitNode::Leaf(index) if *index != active_group => {
            if let Some(group) = groups.get_mut(*index)
                && let Some(tab) = group.tabs.get_mut(group.active_tab)
            {
                queue_editor_layout(tab, pending, background);
            }
        }
        SplitNode::Split { children, .. } => {
            for child in children {
                queue_split_layouts(child, groups, active_group, pending, background);
            }
        }
        _ => {}
    }
}

impl App {
    /// Prepare exact layouts for every visible editor without moving focus or
    /// collecting the split tree into a temporary list on each resize frame.
    pub(crate) fn refresh_wrap_layout_async(&mut self) {
        for job in self.pending_wrap_layouts.values_mut() {
            job.visible = false;
        }
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            queue_editor_layout(tab, &mut self.pending_wrap_layouts, &mut self.background);
        }
        if let Some(tree) = &self.split_tree {
            queue_split_layouts(
                tree,
                &mut self.groups,
                self.active_group,
                &mut self.pending_wrap_layouts,
                &mut self.background,
            );
        }
        self.pending_wrap_layouts.retain(|_, job| {
            if !job.visible {
                job.cancel.cancel();
            }
            job.visible
        });
    }

    pub(crate) fn apply_wrap_layout_result(
        &mut self,
        target: WrapLayoutTarget,
        offsets: WrapRowOffsets,
    ) -> bool {
        if !self
            .pending_wrap_layouts
            .get(&target.editor_id)
            .is_some_and(|job| job.target == target)
        {
            return false;
        }
        self.pending_wrap_layouts.remove(&target.editor_id);
        // The editor may have moved since submission. Group/tab positions and
        // save metadata do not change its layout; exact snapshot properties do.
        let editor = self
            .tabs
            .iter_mut()
            .chain(
                self.groups
                    .iter_mut()
                    .enumerate()
                    .filter(|(index, _)| *index != self.active_group)
                    .flat_map(|(_, group)| group.tabs.iter_mut()),
            )
            .map(|tab| &mut tab.editor)
            .find(|editor| editor.navigation_id == target.editor_id);
        let Some(editor) = editor.filter(|editor| target.matches(editor)) else {
            return false;
        };
        let top_line = editor.scroll_offset;
        if !editor.install_wrap_layout(target.wrap_width, offsets) {
            return false;
        }
        editor.wrap_scroll_offset = editor.wrapped_row_for_position(top_line, 0, target.wrap_width);
        editor.sync_line_scroll_from_wrap(target.wrap_width);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SplitDirection;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn configured_column_workers_and_installed_layout_use_the_same_effective_width() {
        let mut app = split_fixture();
        app.editor_mut().unwrap().word_wrap_column = 40;
        draw(&mut app, 180);
        let (target, offsets) = manual_request(&mut app);
        assert_eq!(target.wrap_width, 40);
        assert!(app.apply_wrap_layout_result(target.clone(), offsets));
        draw(&mut app, 150);
        let editor = app.editor().unwrap();
        assert_eq!(editor.effective_wrap_width(), 40);
        assert!(target.matches(editor));
        assert!(editor.wrap_layout_ready(40));
        app.refresh_wrap_layout_async();
        assert!(!app.pending_wrap_layouts.contains_key(&target.editor_id));
    }

    fn split_fixture() -> App {
        let mut app = App::test_new();
        app.show_explorer = false;
        app.show_minimap = false;
        for _ in 0..2 {
            let mut editor =
                Editor::test_with_text(&format!("{}\n", "word ".repeat(25)).repeat(3_000));
            editor.word_wrap = true;
            app.tabs.push(Tab::new_file(editor));
        }
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        app
    }

    fn draw(app: &mut App, width: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    }

    fn wait_for_layouts(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        app.refresh_wrap_layout_async();
        while !app.pending_wrap_layouts.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "visible layouts did not complete"
            );
            app.poll_background_results();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn resizing_refreshes_all_visible_large_wrapped_panes_without_focus_changes() {
        let mut app = split_fixture();
        let focus = (app.active_group, app.active_tab);
        for width in [160, 104, 188, 92, 160] {
            draw(&mut app, width);
            wait_for_layouts(&mut app);
            let editor = app.editor().unwrap();
            assert!(editor.wrap_layout_ready(wrap_width(editor)));
            for (index, group) in app.groups.iter().enumerate() {
                if index != app.active_group {
                    let editor = &group.tabs[group.active_tab].editor;
                    assert!(
                        editor.wrap_layout_ready(wrap_width(editor)),
                        "inactive group {index}"
                    );
                }
            }
            assert_eq!((app.active_group, app.active_tab), focus);
        }
    }

    fn manual_request(app: &mut App) -> (WrapLayoutTarget, WrapRowOffsets) {
        let editor = app.editor().unwrap();
        let target = WrapLayoutTarget::from_editor(editor);
        let offsets = Editor::build_wrap_row_offsets(
            &editor.rope,
            target.wrap_width,
            target.tab_size,
            &target.folded_ranges,
            || false,
        )
        .unwrap()
        .into();
        app.pending_wrap_layouts.insert(
            target.editor_id,
            PendingWrapLayout {
                target: target.clone(),
                cancel: CancellationToken::new(),
                visible: true,
            },
        );
        (target, offsets)
    }

    #[test]
    fn wrap_results_follow_editor_identity_across_focus_and_tab_moves() {
        let mut app = split_fixture();
        draw(&mut app, 160);
        let (target, offsets) = manual_request(&mut app);
        let id = target.editor_id;
        // Move the in-flight editor to another tab index and group.
        let tab = app.tabs.remove(0);
        app.groups[0].tabs.insert(0, tab);
        app.groups[0].active_tab = 0;
        assert!(app.apply_wrap_layout_result(target, offsets));
        assert_eq!(app.groups[0].tabs[0].editor.navigation_id, id);
        assert!(
            app.groups[0].tabs[0]
                .editor
                .wrap_layout_ready(wrap_width(&app.groups[0].tabs[0].editor))
        );
    }

    #[test]
    fn stale_wrap_results_do_not_clear_newer_width_requests() {
        let mut app = split_fixture();
        draw(&mut app, 160);
        let (old, offsets) = manual_request(&mut app);
        app.editor_mut().unwrap().viewport_width -= 1;
        let (current, current_offsets) = manual_request(&mut app);
        assert!(!app.apply_wrap_layout_result(old, offsets));
        assert_eq!(app.pending_wrap_layouts.len(), 1);
        assert!(app.apply_wrap_layout_result(current, current_offsets));
    }

    #[test]
    fn wrap_results_reject_edits_folds_and_replaced_editors() {
        for action in 0..5 {
            let mut app = split_fixture();
            draw(&mut app, 160);
            let (target, offsets) = manual_request(&mut app);
            match action {
                0 => app.editor_mut().unwrap().insert_char('x'),
                1 => {
                    app.editor_mut().unwrap().folded_ranges.insert(0, 10);
                }
                2 => {
                    let editor = app.editor_mut().unwrap();
                    // Shared-buffer synchronization can replace a snapshot
                    // with equal version and byte length. A paint can consume
                    // the dirty range before the old worker result arrives.
                    editor.rope = ropey::Rope::from_str(&editor.rope.to_string().replace(' ', "x"));
                    editor.mark_syntax_dirty_range(0, editor.line_count());
                    editor.refresh_wrap_caches();
                }
                3 => {
                    let editor = app.editor_mut().unwrap();
                    let rope = ropey::Rope::from_str(&editor.rope.to_string().replace(' ', "x"));
                    let bytes = rope.len_bytes() as u64;
                    editor.apply_bg_loaded(rope, "UTF-8", bytes, u64::MAX);
                    editor.refresh_wrap_caches();
                }
                _ => {
                    app.tabs[0] = Tab::new_file(Editor::test_with_text("replacement"));
                }
            }
            assert!(
                !app.apply_wrap_layout_result(target, offsets),
                "action {action}"
            );
        }
    }

    #[test]
    fn superseded_widths_and_hidden_panes_cancel_their_workers() {
        let mut app = split_fixture();
        draw(&mut app, 160);
        app.refresh_wrap_layout_async();
        let previous: Vec<_> = app
            .pending_wrap_layouts
            .values()
            .map(|job| job.cancel.clone())
            .collect();
        assert_eq!(previous.len(), 2);
        app.editor_mut().unwrap().viewport_width -= 1;
        app.split_tree = None;
        app.refresh_wrap_layout_async();
        assert!(previous.iter().all(CancellationToken::is_cancelled));
        assert_eq!(app.pending_wrap_layouts.len(), 1);
    }

    #[test]
    fn non_code_views_skip_wrap_work_while_unfocused_files_still_refresh() {
        let mut app = split_fixture();
        draw(&mut app, 160);
        let inactive_id = app.groups[0].tabs[0].editor.navigation_id;
        for kind in [
            TabKind::Settings,
            TabKind::MarkdownPreview,
            TabKind::Chat(0),
        ] {
            app.tabs[0].kind = kind;
            app.refresh_wrap_layout_async();
            assert_eq!(app.pending_wrap_layouts.len(), 1);
            assert!(app.pending_wrap_layouts.contains_key(&inactive_id));
        }
        wait_for_layouts(&mut app);
        let editor = &app.groups[0].tabs[0].editor;
        assert!(editor.wrap_layout_ready(wrap_width(editor)));
    }
}
