use super::refresh_blame_cache;
use crate::app::{App, Tab};
use crate::background::BackgroundResult;
use crate::editor::Editor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn loading_app() -> (tempfile::TempDir, App, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let repository = git2::Repository::init(root.path()).unwrap();
    let path = root.path().join("file.c");
    let text = (0..40)
        .map(|i| format!("int value_{i} = {i};\n"))
        .collect::<String>();
    std::fs::write(&path, &text).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("file.c")).unwrap();
    let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::new(
        "Blame Tester",
        "test@example.com",
        &git2::Time::new(1_577_836_800, 0),
    )
    .unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "initial file",
            &tree,
            &[],
        )
        .unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.tabs = vec![Tab::new_file(Editor::new_loading(&path, 0))];
    app.active_tab = 0;
    app.terminal_has_focus = true;
    (root, app, path, text)
}

fn loaded(app: &mut App, path: &Path, text: &str) {
    app.background
        .sender
        .send(BackgroundResult::FileLoaded {
            workspace_generation: 0,
            path: path.into(),
            rope: ropey::Rope::from_str(text),
            encoding: "UTF-8",
            file_size: text.len() as u64,
            last_disk_modified: std::fs::metadata(path).unwrap().modified().ok(),
            session_restores: Vec::new(),
            skip_open_prompt: true,
        })
        .unwrap();
    app.poll_background_results();
    assert!(!app.editor().unwrap().bg_loading);
}

fn paint_editor(app: &mut App, width: u16, height: u16) {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_inline_blame::begin_frame(app);
            crate::ui_editor::draw_editor(frame, app, frame.area());
        })
        .unwrap();
}

fn loaded_visible_idle(app: &mut App, path: &Path, text: &str) {
    loaded(app, path, text);
    app.show_minimap = false;
    let editor = app.editor_mut().unwrap();
    editor.word_wrap = false;
    editor.scroll_offset = editor.cursor_line.saturating_sub(5);
    paint_editor(app, 100, 20);
    app.last_user_activity = Instant::now() - Duration::from_secs(2);
}

fn finish_blame(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.bg_blame_running {
        assert!(Instant::now() < deadline, "blame worker did not finish");
        app.poll_background_results();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn blame_demand_integrates_with_the_event_wait_without_idle_polling() {
    use ratatui::layout::Rect;
    let mut app = App::test_new();
    app.terminal_has_focus = true;
    app.idle_quiescent = true;
    app.auto_save = crate::app::AutoSaveMode::Off;
    let mut editor = Editor::test_with_text("value\n");
    let path = app.explorer.root.join("file.c");
    editor.file_path = Some(path.clone());
    editor.blame.inline_candidate = Some((Rect::new(10, 2, 20, 1), 0));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.last_user_activity = Instant::now();
    assert!(
        super::next_event_wait(&app, false, false)
            .is_some_and(|wait| !wait.is_zero() && wait <= Duration::from_secs(2))
    );
    app.last_user_activity -= Duration::from_secs(2);
    assert_eq!(
        super::next_event_wait(&app, false, false),
        Some(Duration::ZERO)
    );
    app.bg_blame_running = true;
    assert_eq!(super::next_event_wait(&app, false, false), None);
    app.bg_blame_running = false;
    app.menu_open = true;
    assert_eq!(super::next_event_wait(&app, false, false), None);
    app.menu_open = false;
    app.terminal_has_focus = false;
    assert_eq!(super::next_event_wait(&app, false, false), None);
    app.terminal_has_focus = true;
    let editor = app.editor().unwrap();
    app.blame_documents.insert(
        editor.navigation_id,
        std::sync::Arc::new(crate::git_blame::Document {
            path: path.clone(),
            revision: None,
            rope: editor.rope.clone(),
            key: None,
            lines: vec![crate::git_blame::Line::Uncommitted; editor.rope.len_lines()],
            commit_lines: vec![None; editor.rope.len_lines()],
            notice: None,
        }),
    );
    app.blame_requested_path = Some(path);
    assert_eq!(super::next_event_wait(&app, false, false), None);
}

#[test]
fn blame_opening_and_active_input_do_not_start_analysis() {
    let (_root, mut app, path, text) = loading_app();
    loaded(&mut app, &path, &text);
    let now = Instant::now();
    app.last_user_activity = now;
    refresh_blame_cache(&mut app);
    assert!(!app.bg_blame_running);
    assert!(
        crate::git_blame::next_wait(&app, now).is_none(),
        "No consumer before painting"
    );

    paint_editor(&mut app, 100, 20);
    assert!(app.editor().unwrap().blame.inline_candidate.is_some());
    assert_eq!(
        crate::git_blame::next_wait(&app, now),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        crate::git_blame::next_wait(&app, now + Duration::from_millis(1_999)),
        Some(Duration::from_millis(1))
    );
    assert_eq!(
        crate::git_blame::next_wait(&app, now + Duration::from_secs(2)),
        Some(Duration::ZERO)
    );
    for _ in 0..30 {
        // Editing/navigation keeps moving the existing deadline, without
        // sending a request or queuing intermediate document snapshots.
        app.last_user_activity = Instant::now();
        refresh_blame_cache(&mut app);
        assert!(!app.bg_blame_running);
        assert!(app.blame_documents.is_empty());
    }
    app.last_user_activity -= Duration::from_secs(2);
    refresh_blame_cache(&mut app);
    assert!(app.bg_blame_running);
    assert!(
        crate::git_blame::next_wait(&app, Instant::now()).is_none(),
        "No timer while the worker is running"
    );
    finish_blame(&mut app);
    assert!(crate::git_blame::cached(&app, app.editor().unwrap()).is_some());
    assert!(
        crate::git_blame::next_wait(&app, Instant::now()).is_none(),
        "A cached file has no recurring timer"
    );
}

#[test]
fn blame_rapid_tab_browsing_only_analyzes_the_file_left_visible() {
    let (_root, mut app, path, text) = loading_app();
    loaded(&mut app, &path, &text);
    let mut other = Editor::test_with_text(&text);
    other.file_path = Some(path);
    app.tabs.push(Tab::new_file(other));
    for index in (0..30).map(|index| index % 2) {
        app.active_tab = index;
        app.last_user_activity = Instant::now();
        paint_editor(&mut app, 100, 20);
        refresh_blame_cache(&mut app);
        assert!(!app.bg_blame_running);
    }
    app.last_user_activity -= Duration::from_secs(2);
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert_eq!(app.blame_documents.len(), 1);
    assert!(crate::git_blame::cached(&app, &app.tabs[0].editor).is_none());
    assert!(crate::git_blame::cached(&app, &app.tabs[1].editor).is_some());
}

#[test]
fn blame_invisible_annotations_do_not_request_work_or_wakeups() {
    let (_root, mut app, path, text) = loading_app();
    loaded(&mut app, &path, &text);
    app.last_user_activity -= Duration::from_secs(10);
    app.show_minimap = false;
    for (scroll, wrap, width) in [(20, false, 100), (0, false, 12), (0, true, 100)] {
        let editor = app.editor_mut().unwrap();
        editor.scroll_offset = scroll;
        editor.word_wrap = wrap;
        paint_editor(&mut app, width, 20);
        assert!(
            app.editor().unwrap().blame.inline_candidate.is_none(),
            "scroll={scroll}, wrap={wrap}, width={width}"
        );
        assert!(crate::git_blame::next_wait(&app, Instant::now()).is_none());
        refresh_blame_cache(&mut app);
        assert!(!app.bg_blame_running);
    }
    app.editor_mut().unwrap().word_wrap = false;
    paint_editor(&mut app, 100, 20);
    assert!(app.editor().unwrap().blame.inline_candidate.is_some());
    // A modal covers the editor; closing it reuses the pending demand.
    app.exit_confirm_active = true;
    assert!(crate::git_blame::next_wait(&app, Instant::now()).is_none());
    refresh_blame_cache(&mut app);
    assert!(!app.bg_blame_running);
    app.exit_confirm_active = false;
    assert_eq!(
        crate::git_blame::next_wait(&app, Instant::now()),
        Some(Duration::ZERO)
    );
    app.editor_mut().unwrap().folded_ranges.insert(0, 5);
    paint_editor(&mut app, 100, 20);
    assert!(
        app.editor().unwrap().blame.inline_candidate.is_none(),
        "The fold summary covers the annotation"
    );
    assert!(crate::git_blame::next_wait(&app, Instant::now()).is_none());
    app.editor_mut().unwrap().folded_ranges.clear();
    paint_editor(&mut app, 100, 20);
    // A tab switch must retire the previous frame's demand before the next
    // file's renderer supplies its own visibility.
    app.tabs[0].kind = crate::app::TabKind::Settings;
    assert!(crate::git_blame::next_wait(&app, Instant::now()).is_none());
    app.tabs[0].kind = crate::app::TabKind::File;
    crate::ui_inline_blame::begin_frame(&mut app);
    assert!(crate::git_blame::next_wait(&app, Instant::now()).is_none());
}

#[test]
fn blame_explicit_toggle_can_load_a_cold_file_without_waiting() {
    let (_root, mut app, path, text) = loading_app();
    loaded(&mut app, &path, &text);
    app.last_user_activity = Instant::now();
    crate::editor_blame::toggle(&mut app);
    assert!(app.editor().unwrap().blame.enabled);
    assert_eq!(
        crate::git_blame::next_wait(&app, Instant::now()),
        Some(Duration::ZERO)
    );
    refresh_blame_cache(&mut app);
    assert!(app.bg_blame_running);
    finish_blame(&mut app);
    assert!(
        crate::git_blame::cached(&app, app.editor().unwrap())
            .unwrap()
            .notice
            .is_none()
    );
    crate::editor_blame::toggle(&mut app);
    assert!(!app.editor().unwrap().blame.enabled);
    refresh_blame_cache(&mut app);
    assert!(!app.bg_blame_running);
}

#[test]
fn blame_explicit_split_pane_has_priority_and_hidden_desks_do_not_load() {
    use crate::app::TabDropZone;
    let (_root, mut app, path, text) = loading_app();
    // Use separate documents: two views of the same file correctly share
    // edits, so editing either would also invalidate the visible view's blame.
    let other_path = path.with_file_name("other.c");
    std::fs::write(&other_path, &text).unwrap();
    let repo = git2::Repository::open(path.parent().unwrap()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("other.c")).unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let signature = git2::Signature::now("Blame Tester", "test@example.com").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "another file",
        &tree,
        &[&parent],
    )
    .unwrap();
    loaded_visible_idle(&mut app, &path, &text);
    let first = app.editor().unwrap().navigation_id;
    let mut other = Editor::test_with_text(&text);
    other.file_path = Some(other_path);
    other.blame.enabled = true;
    let second = other.navigation_id;
    app.tabs.push(Tab::new_file(other));
    app.create_split_group_at(1, TabDropZone::Right);
    app.switch_group(0);
    paint_editor(&mut app, 100, 20);
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert!(app.blame_documents.contains_key(&second));
    assert!(!app.blame_documents.contains_key(&first));
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert!(app.blame_documents.contains_key(&first));
    app.switch_group(1);
    app.editor_mut().unwrap().insert_char('x');
    app.switch_group(0);
    // Removing a group from the visible tree simulates a hidden desk. Even an
    // enabled rail there is not a consumer of newly computed blame.
    app.split_tree = Some(crate::app::SplitNode::Leaf(0));
    assert!(crate::git_blame::next_wait(&app, Instant::now()).is_none());
    refresh_blame_cache(&mut app);
    assert!(!app.bg_blame_running);
}

#[test]
fn blame_loading_placeholder_does_not_hide_annotations_after_file_load() {
    let (_root, mut app, path, text) = loading_app();
    app.editor_mut().unwrap().pending_cursor_position = Some((31, 0));
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    loaded_visible_idle(&mut app, &path, &text);
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert!(
        crate::ui_dialogs::get_cached_blame_line(&app, app.editor().unwrap(), 31)
            .is_some_and(|line| line.contains("Blame Tester")),
        "the annotation must appear without moving the cursor or waiting for a watcher event"
    );
    assert_eq!(app.editor().unwrap().cursor_line, 31);
    app.editor_mut().unwrap().scroll_offset = 24;
    app.show_minimap = false;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            crate::ui_editor::draw_editor(frame, &mut app, area);
        })
        .unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Blame Tester"), "{rendered}");
}

#[test]
fn blame_file_loaded_after_quiet_period_refreshes_without_further_user_input() {
    let (_root, mut app, path, text) = loading_app();
    app.idle_quiescent = true;
    loaded_visible_idle(&mut app, &path, &text);
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert!(
        crate::ui_dialogs::get_cached_blame_line(&app, app.editor().unwrap(), 31)
            .is_some_and(|line| line.contains("Blame Tester"))
    );
    assert!(
        app.idle_quiescent,
        "loading blame should not restart cosmetic timers"
    );
}

#[test]
fn blame_late_placeholder_result_is_discarded_and_replaced_without_input() {
    let (_root, mut app, path, text) = loading_app();
    loaded_visible_idle(&mut app, &path, &text);
    app.idle_quiescent = true;
    app.blame_requested_path = Some(path.clone());
    app.bg_blame_running = true;
    app.background
        .sender
        .send(BackgroundResult::BlameCache {
            navigation_id: app.editor().unwrap().navigation_id,
            file_path: path.clone(),
            document: std::sync::Arc::new(crate::git_blame::Document {
                revision: None,
                key: Some(crate::background::BlameCacheKey {
                    file_path: path,
                    head_id: "earlier request".into(),
                    disk_modified: None,
                    disk_len: text.len() as u64,
                    line_count: 1,
                }),
                path: app.editor().unwrap().file_path.clone().unwrap(),
                rope: ropey::Rope::from_str("Loading file..."),
                lines: vec![crate::git_blame::Line::Uncommitted],
                commit_lines: vec![None],
                notice: None,
            }),
        })
        .unwrap();
    app.poll_background_results();
    assert!(app.blame_cache.is_none());
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert!(
        crate::ui_dialogs::get_cached_blame_line(&app, app.editor().unwrap(), 31)
            .is_some_and(|line| line.contains("Blame Tester"))
    );
}

#[test]
fn blame_idle_cache_does_not_keep_spawning_git_work_and_refocus_loads_pending_blame() {
    let (_root, mut app, path, text) = loading_app();
    app.terminal_has_focus = false;
    app.idle_quiescent = true;
    loaded_visible_idle(&mut app, &path, &text);
    refresh_blame_cache(&mut app);
    assert!(!app.bg_blame_running);
    assert!(app.blame_cache.is_none());
    app.terminal_has_focus = true;
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    let key = app.blame_cache_key.clone();
    assert!(key.is_some());
    for _ in 0..20 {
        refresh_blame_cache(&mut app);
        assert!(
            !app.bg_blame_running,
            "a quiet unchanged editor must not keep probing Git"
        );
    }
    assert_eq!(app.blame_cache_key, key);
    assert!(app.idle_quiescent);
}

#[test]
fn closing_a_tab_preserves_valid_blame_and_reclaims_closed_view_resources() {
    let (_root, mut app, path, text) = loading_app();
    loaded_visible_idle(&mut app, &path, &text);
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    let first_id = app.editor().unwrap().navigation_id;
    let first = app.blame_documents[&first_id].clone();
    let mut editor = Editor::test_with_text("another file\n");
    let other_path = path.with_file_name("other.c");
    editor.file_path = Some(other_path.clone());
    let other_id = editor.navigation_id;
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 1;
    paint_editor(&mut app, 100, 20);
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert!(app.blame_documents.contains_key(&other_id));
    app.force_close_tab(1);
    assert!(!app.blame_documents.contains_key(&other_id));
    refresh_blame_cache(&mut app);
    assert!(!app.bg_blame_running);
    assert!(std::sync::Arc::ptr_eq(
        &first,
        &app.blame_documents[&first_id]
    ));
    assert!(std::sync::Arc::ptr_eq(
        &first,
        &app.blame_cache.as_ref().unwrap().1
    ));

    // Actual Git/content invalidations still refresh an otherwise matching
    // snapshot. Navigation must not turn those signals into a stale cache hit.
    app.blame_requested_path = None;
    refresh_blame_cache(&mut app);
    assert!(app.bg_blame_running);
    finish_blame(&mut app);
    app.editor_mut().unwrap().insert_char('x');
    refresh_blame_cache(&mut app);
    assert!(app.bg_blame_running);
    finish_blame(&mut app);
    app.editor_mut().unwrap().undo();
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    app.force_close_tab(0);
    refresh_blame_cache(&mut app);
    assert!(!app.bg_blame_running);
    assert!(app.blame_documents.is_empty());
    assert!(app.blame_cache.is_none());
    app.background
        .sender
        .send(BackgroundResult::BlameCache {
            navigation_id: first_id,
            file_path: path,
            document: first,
        })
        .unwrap();
    app.poll_background_results();
    assert!(
        app.blame_documents.is_empty(),
        "A late reply cannot retain the closed file"
    );
    assert!(app.blame_cache.is_none());
}

#[test]
fn blame_same_length_reload_refreshes_annotations_while_idle_without_watcher() {
    let (root, mut app, path, text) = loading_app();
    loaded_visible_idle(&mut app, &path, &text);
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    let previous_key = app.blame_cache_key.clone().unwrap();

    let repository = git2::Repository::open(root.path()).unwrap();
    let updated = text.replace("int value_31 = 31;", "int value_31 = 99;");
    std::fs::write(&path, updated).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("file.c")).unwrap();
    let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let signature = git2::Signature::now("Updated Tester", "test@example.com").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "update line",
            &tree,
            &[&parent],
        )
        .unwrap();

    app.idle_quiescent = true;
    app.background
        .sender
        .send(BackgroundResult::FileReloaded {
            targets: app
                .tabs
                .iter()
                .chain(app.groups.iter().flat_map(|group| &group.tabs))
                .map(|tab| (tab.editor.navigation_id, tab.editor.content_version))
                .collect(),
            workspace_generation: 0,
            path: path.clone(),
            result: Ok(crate::editor::load_text_file(&path).unwrap()),
        })
        .unwrap();
    app.poll_background_results();
    refresh_blame_cache(&mut app);
    finish_blame(&mut app);
    assert!(
        crate::ui_dialogs::get_cached_blame_line(&app, app.editor().unwrap(), 31)
            .is_some_and(|line| line.contains("Updated Tester"))
    );
    assert_eq!(
        app.blame_cache_key.as_ref().unwrap().line_count,
        previous_key.line_count
    );
    assert!(app.idle_quiescent);
}
