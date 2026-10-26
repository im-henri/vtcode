use super::*;
use crate::{
    app::{GitFileChange, Tab},
    editor::Editor,
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const TEXT: &str = "outside before\n<<<<<<< HEAD\ncurrent value\n||||||| base\nancestor value\n=======\nincoming value\n>>>>>>> topic\noutside after\n";

fn indexed(app: &mut App) {
    let editor = app.editor_mut().unwrap();
    editor.merge_conflicts.index = Some(Arc::new(Index::update(None, editor.rope.clone())));
    editor.merge_conflicts.pending = false;
}

fn app() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(TEXT);
    editor.saved_rope = Some(editor.rope.clone());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_minimap = false;
    app.active_pane = ActivePane::Editor;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    indexed(&mut app);
    app
}

fn paint(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::editor_ai_review::draw(frame, app, frame.area()))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    (buffer.area.y..buffer.area.bottom())
        .map(|y| {
            (buffer.area.x..buffer.area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn click(app: &mut App, hit: &Hit) {
    assert!(app.handle_editor_review_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.area.x,
        row: hit.area.y,
        modifiers: KeyModifiers::NONE
    }));
}

#[test]
fn merge_conflict_inline_matches_reference_layout_and_preserves_source_rows() {
    for wrap in [false, true] {
        let mut app = app();
        app.editor_mut().unwrap().word_wrap = wrap;
        let buffer = paint(&mut app, 120, 28);
        let rendered = text(&buffer);
        crate::ui_control_visibility_tests::snapshot(&buffer, "Merge conflict - inline");
        for label in [
            "Accept Current Change",
            "Accept Incoming Change",
            "Accept Both Changes",
            "Compare Changes",
            "HEAD (Current Change)",
            "topic (Incoming Change)",
            "Resolve in Merge Editor",
        ] {
            assert!(rendered.contains(label), "missing {label}:\n{rendered}");
        }
        let editor = app.editor().unwrap();
        let code = app.code_area.unwrap();
        let header = editor
            .ai_review
            .source_rows
            .iter()
            .position(|row| row.is_some_and(|row| row.line_idx == 1))
            .unwrap();
        let action = editor
            .ai_review
            .merge_hits
            .iter()
            .find(|hit| hit.action == "current")
            .unwrap();
        assert_eq!(
            action.area.y + 1,
            code.y + header as u16,
            "CodeLens is directly above the marker"
        );
        assert!(
            editor.ai_review.source_rows[header - 1].is_none(),
            "action rows have no source line number"
        );
        for (line, kind) in [(2, 0), (4, 2), (6, 1)] {
            let row = editor
                .ai_review
                .source_rows
                .iter()
                .position(|row| row.is_some_and(|row| row.line_idx == line))
                .unwrap();
            assert_eq!(
                buffer[(code.x + 3, code.y + row as u16)].bg,
                app.colors.ui.merge_contents[kind]
                    .over(app.colors.bg_editor)
                    .unwrap()
            );
        }
        assert!(
            editor.merge_conflicts.review.is_none(),
            "ordinary conflicts open inline"
        );
        assert_eq!(
            editor.rope.to_string(),
            TEXT,
            "decorations never enter the document"
        );
    }
}

#[test]
fn merge_conflict_inline_buttons_act_on_their_own_region_and_support_undo() {
    for action in ["current", "incoming", "both"] {
        let mut app = app();
        paint(&mut app, 100, 26);
        let hit = app
            .editor()
            .unwrap()
            .ai_review
            .merge_hits
            .iter()
            .find(|hit| hit.action == action)
            .unwrap()
            .clone();
        click(&mut app, &hit);
        let resolved = app.editor().unwrap().rope.to_string();
        assert!(!resolved.contains("ancestor"));
        assert!(!resolved.contains("<<<<<<<"));
        assert_eq!(resolved.contains("current value"), action != "incoming");
        assert_eq!(resolved.contains("incoming value"), action != "current");
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), TEXT);
        app.editor_mut().unwrap().redo();
        assert_eq!(app.editor().unwrap().rope.to_string(), resolved);
    }
}

#[test]
fn merge_conflict_stale_buttons_cannot_resolve_a_shifted_conflict() {
    let mut app = app();
    paint(&mut app, 100, 26);
    let hit = app
        .editor()
        .unwrap()
        .ai_review
        .merge_hits
        .iter()
        .find(|hit| hit.action == "incoming")
        .unwrap()
        .clone();
    app.editor_mut().unwrap().insert_char('X');
    let edited = app.editor().unwrap().rope.clone();
    click(&mut app, &hit);
    assert!(app.editor().unwrap().rope.is_instance(&edited));
    indexed(&mut app);
    app.merge_action("incoming", Some(1000));
    assert!(app.editor().unwrap().rope.is_instance(&edited));
}

#[test]
fn merge_conflict_three_way_uses_the_same_result_buffer_and_returns_to_inline() {
    let mut app = app();
    let identity = app.editor().unwrap().save_identity;
    let original = app.editor().unwrap().rope.clone();
    paint(&mut app, 120, 32);
    let hit = app
        .editor()
        .unwrap()
        .ai_review
        .merge_hits
        .iter()
        .find(|hit| hit.action == "merge_editor")
        .unwrap()
        .clone();
    click(&mut app, &hit);
    let buffer = paint(&mut app, 120, 32);
    let rendered = text(&buffer);
    crate::ui_control_visibility_tests::snapshot(&buffer, "Merge conflict - three way");
    for label in [
        "Merge Editor",
        "Current · HEAD",
        "Incoming · topic",
        "Result",
        "Back to Inline",
        "current value",
        "incoming value",
    ] {
        assert!(rendered.contains(label), "missing {label}:\n{rendered}");
    }
    assert!(
        app.editor()
            .unwrap()
            .merge_conflicts
            .review
            .as_ref()
            .unwrap()
            .source
            .is_instance(&original)
    );
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert_eq!(app.tabs.len(), 1);
    let hit = app
        .editor()
        .unwrap()
        .ai_review
        .merge_hits
        .iter()
        .find(|hit| hit.action == "both")
        .unwrap()
        .clone();
    click(&mut app, &hit);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), TEXT);
    indexed(&mut app);
    app.merge_action("close_merge_editor", None);
    paint(&mut app, 120, 32);
    assert!(app.editor().unwrap().merge_conflicts.review.is_none());
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert_eq!(app.editor().unwrap().rope.to_string(), TEXT);
}

#[test]
fn merge_conflict_layout_handles_narrow_short_wrapped_and_offset_frames() {
    for (width, height) in [(0, 0), (1, 1), (15, 8), (35, 18), (60, 20), (100, 30)] {
        for wrap in [false, true] {
            let mut app = app();
            app.editor_mut().unwrap().word_wrap = wrap;
            for merge_editor in [false, true] {
                if merge_editor {
                    app.merge_action("merge_editor", Some(1));
                }
                let _ = paint(&mut app, width, height);
                for hit in &app.editor().unwrap().ai_review.merge_hits {
                    assert!(hit.area.right() <= width && hit.area.bottom() <= height);
                }
            }
        }
    }
}

#[test]
fn merge_conflict_navigation_wraps_and_undo_keeps_independent_decisions() {
    let mut app = app();
    app.editor_mut().unwrap().rope.append(Rope::from_str(TEXT));
    indexed(&mut app);
    app.merge_action("next_conflict", None);
    assert_eq!(app.editor().unwrap().cursor_line, 1);
    app.merge_action("next_conflict", None);
    assert_eq!(app.editor().unwrap().cursor_line, 10);
    app.merge_action("next_conflict", None);
    assert_eq!(app.editor().unwrap().cursor_line, 1);
    app.merge_action("previous_conflict", None);
    assert_eq!(app.editor().unwrap().cursor_line, 10);
    app.merge_action("current", None);
    indexed(&mut app);
    app.merge_action("current", Some(1));
    assert!(!app.editor().unwrap().rope.to_string().contains("<<<<<<<"));
    app.editor_mut().unwrap().undo();
    assert_eq!(
        Index::update(None, app.editor().unwrap().rope.clone())
            .regions
            .len(),
        1
    );
    app.editor_mut().unwrap().undo();
    assert_eq!(
        Index::update(None, app.editor().unwrap().rope.clone())
            .regions
            .len(),
        2
    );
}

fn finish(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done(app) && Instant::now() < deadline {
        app.poll_background_results();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(done(app));
}

#[test]
fn merge_conflict_compare_runs_on_worker_and_opens_a_read_only_two_way_diff() {
    let mut app = app();
    let source = app.editor().unwrap().rope.clone();
    app.merge_action("compare", Some(1));
    finish(&mut app, |app| app.tabs.len() == 2);
    assert!(app.editor().unwrap().read_only);
    assert_eq!(app.editor().unwrap().rope.to_string(), "incoming value\n");
    assert_eq!(app.compare_base_content.as_deref(), Some("current value\n"));
    assert!(app.tabs[0].editor.rope.is_instance(&source));
    assert!(matches!(app.tabs[1].kind, TabKind::Diff(_)));
}

#[test]
fn merge_conflict_background_index_is_idle_when_unchanged_and_updates_after_edits() {
    let mut app = app();
    app.editor_mut().unwrap().merge_conflicts = Default::default();
    app.prepare_merge_conflicts();
    app.prepare_merge_conflicts();
    finish(&mut app, |app| {
        !app.editor().unwrap().merge_conflicts.pending
    });
    let index = app.editor().unwrap().merge_conflicts.index.clone().unwrap();
    for _ in 0..20 {
        app.prepare_merge_conflicts();
    }
    assert!(Arc::ptr_eq(
        &index,
        app.editor()
            .unwrap()
            .merge_conflicts
            .index
            .as_ref()
            .unwrap()
    ));
    assert!(!app.editor().unwrap().merge_conflicts.pending);
    app.editor_mut().unwrap().insert_char('X');
    app.prepare_merge_conflicts();
    finish(&mut app, |app| {
        !app.editor().unwrap().merge_conflicts.pending
    });
    assert!(
        app.editor()
            .unwrap()
            .merge_conflicts
            .current(&app.editor().unwrap().rope)
            .is_some()
    );
}

// A Repository caches its Index in memory. Mutations run through separate
// repository handles, so assertions must explicitly reload the on-disk index.
fn fresh_index(repo: &git2::Repository) -> git2::Index {
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    index
}

fn conflicted_repo() -> (tempfile::TempDir, git2::Repository) {
    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    std::fs::write(directory.path().join("conflict.txt"), "base\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("conflict.txt")).unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
        .unwrap();
    drop(tree);
    let entries = [1, 2, 3].map(|stage| {
        let mut entry = index.get_path(Path::new("conflict.txt"), 0).unwrap();
        entry.flags = (entry.flags & !0x3000) | (stage << 12);
        entry.id = repo.blob(format!("stage {stage}\n").as_bytes()).unwrap();
        entry
    });
    index.remove_path(Path::new("conflict.txt")).unwrap();
    for entry in entries {
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
    std::fs::write(directory.path().join("conflict.txt"), TEXT).unwrap();
    (directory, repo)
}

#[test]
fn merge_conflict_git_status_is_visible_even_when_untracked_is_hidden() {
    let (directory, _repo) = conflicted_repo();
    let status = crate::app_git::collect_source_control_status_with_paths(
        directory.path().into(),
        || {},
        &crate::background::CancellationToken::new(),
        false,
        |path| path.to_string_lossy().into_owned(),
    );
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].status, '!');
    assert!(!status.files[0].staged);
    let layout = crate::source_control_rows::Layout::new(
        &status.files,
        crate::source_control_rows::Visibility { untracked: false },
        [false; 4],
    );
    assert_eq!(layout.count(crate::source_control_rows::Group::Merge), 1);
    assert_eq!(layout.file_count(), 1);
}

#[test]
fn merge_conflict_staging_rejects_markers_preserves_index_and_stages_resolved_results() {
    for all in [false, true] {
        let (directory, repo) = conflicted_repo();
        let original_index = std::fs::read(repo.path().join("index")).unwrap();
        let stage = || {
            if all {
                crate::git_tasks::execute::<String>(
                    directory.path(),
                    crate::git_tasks::Task::StageAll,
                )
            } else {
                crate::app_git::changes::stage(
                    directory.path(),
                    &[GitFileChange {
                        path: "conflict.txt".to_string(),
                        status: '!',
                        staged: false,
                        original_path: None,
                    }],
                )
            }
        };
        let error = stage().unwrap_err();
        assert!(error.contains("conflict markers"), "{error}");
        assert_eq!(
            std::fs::read(repo.path().join("index")).unwrap(),
            original_index
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("conflict.txt")).unwrap(),
            TEXT
        );
        std::fs::write(directory.path().join("conflict.txt"), "resolved\n").unwrap();
        stage().unwrap();
        let index = fresh_index(&repo);
        assert!(!index.has_conflicts());
        let entry = index.get_path(Path::new("conflict.txt"), 0).unwrap();
        assert_eq!(repo.find_blob(entry.id).unwrap().content(), b"resolved\n");
    }
}

fn mouse(app: &mut App, kind: MouseEventKind, area: Rect) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn merge_conflict_source_panes_own_mouse_and_narrow_toolbar_can_return_to_inline() {
    let mut app = app();
    app.editor_mut().unwrap().rope =
        Rope::from_str(&TEXT.replace("current value", &"current line\n".repeat(30)));
    indexed(&mut app);
    app.merge_action("merge_editor", Some(1));
    paint(&mut app, 100, 30);
    let preview = app
        .editor()
        .unwrap()
        .merge_conflicts
        .review
        .as_ref()
        .unwrap()
        .chrome
        .preview;
    let rope = app.editor().unwrap().rope.clone();
    let cursor = (
        app.editor().unwrap().cursor_line,
        app.editor().unwrap().cursor_col,
    );
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Middle),
        MouseEventKind::Up(MouseButton::Middle),
    ] {
        mouse(
            &mut app,
            kind,
            Rect::new(preview.x + 3, preview.y + 1, 1, 1),
        );
    }
    assert!(!app.dragging_text);
    assert!(app.editor().unwrap().extra_cursors.is_empty());
    assert!(app.editor().unwrap().selection_anchor.is_none());
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        cursor
    );
    assert!(app.editor().unwrap().rope.is_instance(&rope));
    let result_scroll = app.editor().unwrap().scroll_offset;
    mouse(&mut app, MouseEventKind::ScrollDown, preview);
    assert_eq!(
        app.editor()
            .unwrap()
            .merge_conflicts
            .review
            .as_ref()
            .unwrap()
            .scroll,
        3
    );
    assert_eq!(app.editor().unwrap().scroll_offset, result_scroll);
    mouse(&mut app, MouseEventKind::ScrollUp, preview);
    assert_eq!(
        app.editor()
            .unwrap()
            .merge_conflicts
            .review
            .as_ref()
            .unwrap()
            .scroll,
        0
    );
    paint(&mut app, 20, 16);
    let back = app
        .editor()
        .unwrap()
        .merge_conflicts
        .review
        .as_ref()
        .unwrap()
        .chrome
        .buttons
        .iter()
        .find(|hit| hit.action == "close_merge_editor")
        .unwrap()
        .area;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), back);
    assert!(app.editor().unwrap().merge_conflicts.review.is_none());
    assert!(app.editor().unwrap().rope.is_instance(&rope));
}

#[test]
fn merge_conflict_toolbar_in_an_inactive_split_targets_that_view_only() {
    let mut other = app();
    let mut app = app();
    app.merge_action("merge_editor", Some(1));
    other.merge_action("merge_editor", Some(1));
    let mut group = crate::app::EditorGroup::new();
    group.tabs = std::mem::take(&mut other.tabs);
    app.groups = vec![crate::app::EditorGroup::new(), group];
    let original = app.editor().unwrap().rope.clone();
    let mut terminal = Terminal::new(TestBackend::new(160, 32)).unwrap();
    terminal
        .draw(|frame| {
            crate::editor_ai_review::draw(frame, &mut app, Rect::new(0, 0, 80, 32));
            crate::ui_helpers::draw_split_editor_for_group(
                frame,
                &mut app,
                Rect::new(80, 0, 80, 32),
                1,
            );
        })
        .unwrap();
    let source_hit = app.groups[1].tabs[0]
        .editor
        .merge_conflicts
        .review
        .as_ref()
        .unwrap()
        .chrome
        .buttons
        .iter()
        .find(|hit| hit.action == "incoming")
        .unwrap()
        .area;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        source_hit,
    );
    assert_eq!(app.active_group, 1);
    assert!(!app.editor().unwrap().rope.to_string().contains("<<<<<<<"));
    assert!(
        app.editor()
            .unwrap()
            .rope
            .to_string()
            .contains("incoming value")
    );
    assert!(app.groups[0].tabs[0].editor.rope.is_instance(&original));
    assert!(app.groups[0].tabs[0].editor.undo_stack.is_empty());
}

#[test]
fn merge_conflict_duplicate_views_share_the_result_and_undo_history() {
    let mut app = app();
    app.editor_mut().unwrap().file_path = Some("shared-conflict.txt".into());
    app.duplicate_tab(0);
    indexed(&mut app);
    app.merge_action("merge_editor", Some(1));
    app.merge_action("both", Some(1));
    app.sync_duplicate_file_tabs();
    assert!(
        app.tabs[0]
            .editor
            .rope
            .is_instance(&app.tabs[1].editor.rope)
    );
    app.active_tab = 0;
    app.editor_mut().unwrap().undo();
    app.sync_duplicate_file_tabs();
    assert_eq!(app.tabs[1].editor.rope.to_string(), TEXT);
    assert!(
        app.tabs[1].editor.merge_conflicts.review.is_some(),
        "layout stays local to its view"
    );
    assert!(app.tabs[0].editor.merge_conflicts.review.is_none());
    app.active_tab = 1;
    app.editor_mut().unwrap().redo();
    app.sync_duplicate_file_tabs();
    assert!(!app.tabs[0].editor.rope.to_string().contains("<<<<<<<"));
}

#[test]
fn merge_conflict_compare_result_never_steals_focus_after_switching_files() {
    let mut app = app();
    app.merge_action("compare", Some(1));
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("another document")));
    app.active_tab = 1;
    finish(&mut app, |app| {
        !app.tabs[0].editor.merge_conflicts.comparing
    });
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    assert_eq!(app.editor().unwrap().rope.to_string(), "another document");
}

#[test]
fn merge_conflict_read_only_documents_offer_comparison_without_accepting_changes() {
    let mut app = app();
    app.editor_mut().unwrap().read_only = true;
    paint(&mut app, 120, 30);
    assert!(
        !app.editor()
            .unwrap()
            .ai_review
            .merge_hits
            .iter()
            .any(|hit| matches!(hit.action, "current" | "incoming" | "both"))
    );
    assert!(
        app.editor()
            .unwrap()
            .ai_review
            .merge_hits
            .iter()
            .any(|hit| hit.action == "compare")
    );
    app.merge_action("merge_editor", Some(1));
    paint(&mut app, 120, 30);
    assert!(
        !app.editor()
            .unwrap()
            .merge_conflicts
            .review
            .as_ref()
            .unwrap()
            .chrome
            .buttons
            .iter()
            .any(|hit| matches!(hit.action, "current" | "incoming" | "both"))
    );
    app.merge_action("both", Some(1));
    assert_eq!(app.editor().unwrap().rope.to_string(), TEXT);
    assert!(app.editor().unwrap().undo_stack.is_empty());
}

#[test]
fn merge_conflict_deleted_files_can_be_staged_as_resolved_deletions() {
    for all in [false, true] {
        let (directory, repo) = conflicted_repo();
        std::fs::remove_file(directory.path().join("conflict.txt")).unwrap();
        if all {
            crate::git_tasks::execute::<String>(directory.path(), crate::git_tasks::Task::StageAll)
                .unwrap();
        } else {
            crate::app_git::changes::stage(
                directory.path(),
                &[GitFileChange {
                    path: "conflict.txt".to_string(),
                    status: '!',
                    staged: false,
                    original_path: None,
                }],
            )
            .unwrap();
        }
        assert!(!fresh_index(&repo).has_conflicts());
        assert!(
            fresh_index(&repo)
                .get_path(Path::new("conflict.txt"), 0)
                .is_none()
        );
    }
}

#[test]
fn merge_conflict_real_git_merge_resolves_saves_and_stages_from_both_views() {
    for three_way in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let path = directory.path().join("file.rs");
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let commit = |text: &str, parent: Option<&git2::Commit<'_>>| {
            std::fs::write(&path, text).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("file.rs")).unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            index.write().unwrap();
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                "test",
                &tree,
                &parent.into_iter().collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let base = repo.find_commit(commit("base\n", None)).unwrap();
        repo.branch("incoming", &base, false).unwrap();
        commit("current\n", Some(&base));
        let mut index = git2::Index::new().unwrap();
        index.read_tree(&base.tree().unwrap()).unwrap();
        let mut incoming = index.get_path(Path::new("file.rs"), 0).unwrap();
        incoming.id = repo.blob(b"incoming\n").unwrap();
        index.add(&incoming).unwrap();
        let tree = repo.find_tree(index.write_tree_to(&repo).unwrap()).unwrap();
        let theirs = repo
            .commit(
                Some("refs/heads/incoming"),
                &sig,
                &sig,
                "incoming",
                &tree,
                &[&base],
            )
            .unwrap();
        let theirs = repo.find_annotated_commit(theirs).unwrap();
        repo.merge(&[&theirs], None, None).unwrap();
        assert!(repo.index().unwrap().has_conflicts());
        let on_disk = std::fs::read_to_string(&path).unwrap();
        let accepted = if on_disk.contains("\r\n") {
            "incoming\r\n"
        } else {
            "incoming\n"
        };
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.open_file(&path).unwrap();
        app.tabs = vec![Tab::new_file(editor)];
        app.active_pane = ActivePane::Editor;
        app.prepare_merge_conflicts();
        finish(&mut app, |app| {
            !app.editor().unwrap().merge_conflicts.pending
        });
        let line = app
            .editor()
            .unwrap()
            .merge_conflicts
            .current(&app.editor().unwrap().rope)
            .unwrap()
            .regions[0]
            .start;
        if three_way {
            app.merge_action("merge_editor", Some(line));
        }
        paint(&mut app, 120, 32);
        let hit = app
            .editor()
            .unwrap()
            .ai_review
            .merge_hits
            .iter()
            .find(|hit| hit.action == "incoming")
            .unwrap()
            .clone();
        click(&mut app, &hit);
        assert_eq!(app.editor().unwrap().rope.to_string(), accepted);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            on_disk,
            "decisions never save implicitly"
        );
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), on_disk);
        app.editor_mut().unwrap().redo();
        app.editor_mut()
            .unwrap()
            .save_file_with_options(false, false)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), accepted);
        crate::app_git::changes::stage(
            directory.path(),
            &[GitFileChange {
                path: "file.rs".to_string(),
                status: '!',
                staged: false,
                original_path: None,
            }],
        )
        .unwrap();
        assert!(!fresh_index(&repo).has_conflicts());
    }
}

#[test]
fn merge_conflict_remote_status_and_staging_use_the_same_safety_checks() {
    use crate::remote::{Command, RemotePath, Reply};
    let (directory, repo) = conflicted_repo();
    let client = crate::remote::tests::connection(directory.path(), Arc::default(), Arc::default());
    let Reply::GitStatus { scans, .. } = client
        .call(Command::GitStatus {
            include_untracked: false,
            known: None,
            parent_open: false,
            closed: vec![],
        })
        .unwrap()
    else {
        panic!("Git status reply");
    };
    assert_eq!(scans.len(), 1);
    assert_eq!(scans[0].files.len(), 1);
    assert_eq!(scans[0].files[0].status, '!');
    let error = client
        .call(Command::GitAction {
            repo: RemotePath::default(),
            task: crate::git_tasks::Task::Stage(scans[0].files.clone()),
        })
        .unwrap_err();
    assert!(error.to_string().contains("conflict markers"));
    assert!(repo.index().unwrap().has_conflicts());
    std::fs::write(directory.path().join("conflict.txt"), "resolved\n").unwrap();
    client
        .call(Command::GitAction {
            repo: RemotePath::default(),
            task: crate::git_tasks::Task::StageAll,
        })
        .unwrap();
    assert!(!fresh_index(&repo).has_conflicts());
}

#[test]
fn merge_conflict_footer_follows_all_wrapped_marker_rows_and_ordinary_clicks_map_to_source() {
    let mut app = app();
    app.editor_mut().unwrap().rope = Rope::from_str(&TEXT.replace(
        ">>>>>>> topic",
        &format!(">>>>>>> topic {}", "very long 界 branch ".repeat(5)),
    ));
    app.editor_mut().unwrap().word_wrap = true;
    indexed(&mut app);
    let _ = paint(&mut app, 36, 42);
    let editor = app.editor().unwrap();
    let code = app.code_area.unwrap();
    let last_marker_row = editor
        .ai_review
        .source_rows
        .iter()
        .rposition(|row| row.is_some_and(|row| row.line_idx == 7))
        .unwrap();
    let footer = editor
        .ai_review
        .merge_hits
        .iter()
        .find(|hit| hit.action == "merge_editor")
        .unwrap()
        .area;
    assert_eq!(footer.y, code.y + last_marker_row as u16 + 1);
    for line in [0, 1, 2, 4, 6, 8] {
        let row = app
            .editor()
            .unwrap()
            .ai_review
            .source_rows
            .iter()
            .position(|row| row.is_some_and(|row| row.line_idx == line))
            .unwrap();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            Rect::new(code.x + 1, code.y + row as u16, 1, 1),
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            Rect::new(code.x + 1, code.y + row as u16, 1, 1),
        );
        assert_eq!(app.editor().unwrap().cursor_line, line);
    }
}

#[test]
fn merge_conflict_source_control_counts_and_open_action_include_the_merge_group() {
    let (directory, _repo) = conflicted_repo();
    let mut app = App::new_deferred(directory.path().to_owned());
    app.refresh_source_control_async();
    finish(&mut app, |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_change_count(), 1);
    assert_eq!(
        app.source_control_layout()
            .count(crate::source_control_rows::Group::Merge),
        1
    );
    assert_eq!(
        app.repositories.entries[0]
            .change_count(crate::source_control_rows::Visibility { untracked: false }),
        1
    );
    app.open_source_control_diff(0);
    finish(&mut app, |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    assert_eq!(app.tabs[app.active_tab].kind, TabKind::File);
    assert!(!app.editor().unwrap().read_only);
    assert_eq!(app.editor().unwrap().rope.to_string(), TEXT);
}

#[test]
fn merge_conflict_visible_analysis_skips_hidden_tabs_and_paint_never_queues_work() {
    let mut app = app();
    app.editor_mut().unwrap().merge_conflicts = Default::default();
    let mut hidden = Tab::new_file(Editor::test_with_text(TEXT));
    hidden.editor.file_path = Some("hidden.txt".into());
    app.tabs.push(hidden);
    paint(&mut app, 100, 28);
    assert!(
        !app.editor().unwrap().merge_conflicts.pending,
        "paint is independent of analysis scheduling"
    );
    app.prepare_visible_merge_conflicts();
    finish(&mut app, |app| {
        !app.editor().unwrap().merge_conflicts.pending
    });
    assert!(
        app.tabs[0]
            .editor
            .merge_conflicts
            .current(&app.tabs[0].editor.rope)
            .is_some()
    );
    assert!(app.tabs[1].editor.merge_conflicts.index.is_none());
    assert!(!app.tabs[1].editor.merge_conflicts.pending);
}

#[test]
fn merge_conflict_partial_manual_edits_are_not_reported_as_resolved() {
    let mut app = app();
    app.merge_action("merge_editor", Some(1));
    let editor = app.editor_mut().unwrap();
    let range = crate::merge_conflicts::line_chars(&editor.rope, 7..8);
    editor.rope.remove(range);
    editor.content_version += 1;
    indexed(&mut app);
    let buffer = paint(&mut app, 120, 30);
    assert!(text(&buffer).contains("Conflict markers remain"));
    assert!(!text(&buffer).contains("All conflicts resolved"));
}

#[cfg(unix)]
#[test]
fn merge_conflict_non_utf8_paths_remain_visible_and_can_be_safely_staged() {
    use std::os::unix::ffi::OsStrExt;
    let (directory, repo) = conflicted_repo();
    let name = b"conflict-\xff.txt";
    let native = Path::new(std::ffi::OsStr::from_bytes(name));
    std::fs::rename(
        directory.path().join("conflict.txt"),
        directory.path().join(native),
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    let entries = [1, 2, 3].map(|stage| {
        let mut entry = index.get_path(Path::new("conflict.txt"), stage).unwrap();
        entry.path = name.to_vec();
        entry.flags = entry.flags & !0xfff | name.len() as u16;
        entry
    });
    index.remove_path(Path::new("conflict.txt")).unwrap();
    for entry in entries {
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
    let status = crate::app_git::collect_source_control_status_with_paths(
        directory.path().into(),
        || {},
        &crate::background::CancellationToken::new(),
        false,
        |path| path.as_os_str().as_bytes().to_vec(),
    );
    let conflicts: Vec<_> = status
        .files
        .iter()
        .filter(|file| file.status == '!')
        .collect();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, name);
    let error =
        crate::git_tasks::execute::<String>(directory.path(), crate::git_tasks::Task::StageAll)
            .unwrap_err();
    assert!(error.contains("conflict markers"));
    assert!(fresh_index(&repo).has_conflicts());
    std::fs::write(directory.path().join(native), "resolved\n").unwrap();
    crate::git_tasks::execute::<String>(directory.path(), crate::git_tasks::Task::StageAll)
        .unwrap();
    let index = fresh_index(&repo);
    assert!(!index.has_conflicts());
    assert!(index.get_path(native, 0).is_some());
}

#[test]
fn merge_conflict_resolved_submodule_is_staged_without_reading_it_as_a_text_file() {
    for (all, registered) in [(false, false), (true, false), (false, true), (true, true)] {
        let (directory, repo) = conflicted_repo();
        let path = directory.path().join("conflict.txt");
        std::fs::remove_file(&path).unwrap();
        let child = git2::Repository::init(&path).unwrap();
        let tree = child
            .find_tree(child.index().unwrap().write_tree().unwrap())
            .unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        let commit = child
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "submodule",
                &tree,
                &[],
            )
            .unwrap();
        if registered {
            std::fs::write(
                directory.path().join(".gitmodules"),
                "[submodule \"child\"]\npath = conflict.txt\nurl = ../child\n",
            )
            .unwrap();
        }
        let mut index = repo.index().unwrap();
        for stage in [1, 2, 3] {
            let mut entry = index.get_path(Path::new("conflict.txt"), stage).unwrap();
            entry.mode = 0o160000;
            entry.id = commit;
            index.add(&entry).unwrap();
        }
        index.write().unwrap();
        if all {
            crate::git_tasks::execute::<String>(directory.path(), crate::git_tasks::Task::StageAll)
                .unwrap();
        } else {
            crate::app_git::changes::stage(
                directory.path(),
                &[GitFileChange {
                    path: "conflict.txt".to_string(),
                    status: '!',
                    staged: false,
                    original_path: None,
                }],
            )
            .unwrap();
        }
        let index = fresh_index(&repo);
        assert!(!index.has_conflicts());
        let entry = index
            .get_path(Path::new("conflict.txt"), 0)
            .unwrap_or_else(|| panic!("Missing submodule: all={all}, registered={registered}"));
        assert_eq!(entry.mode, 0o160000);
        assert_eq!(entry.id, commit);
    }
}

#[test]
fn merge_conflict_file_replaced_by_a_directory_preserves_children_when_staging_all() {
    for all in [false, true] {
        let (directory, repo) = conflicted_repo();
        let path = directory.path().join("conflict.txt");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("child.txt"), "child\n").unwrap();
        if all {
            crate::git_tasks::execute::<String>(directory.path(), crate::git_tasks::Task::StageAll)
                .unwrap();
        } else {
            crate::app_git::changes::stage(
                directory.path(),
                &[GitFileChange {
                    path: "conflict.txt".to_string(),
                    status: '!',
                    staged: false,
                    original_path: None,
                }],
            )
            .unwrap();
        }
        let index = fresh_index(&repo);
        assert!(!index.has_conflicts());
        assert!(index.get_path(Path::new("conflict.txt"), 0).is_none());
        assert_eq!(
            index
                .get_path(Path::new("conflict.txt/child.txt"), 0)
                .is_some(),
            all
        );
        assert_eq!(
            std::fs::read_to_string(path.join("child.txt")).unwrap(),
            "child\n"
        );
    }
}
