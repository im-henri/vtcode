use super::*;
use crate::{
    app::{Tab, TabDropZone},
    background::BackgroundResult,
    editor::Editor,
};
use crossterm::event::KeyModifiers;
use ratatui::{Terminal, backend::TestBackend};
use ropey::Rope;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const SOURCE: &str = concat!(
    "#include <stdio.h>\n\n",
    "int sum_array(const int *arr, size_t size) {\n",
    "    int sum = 0;\n",
    "    for (size_t i = 0; i < size; ++i) {\n",
    "        sum += arr[i];\n",
    "    }\n",
    "    return sum;\n",
    "}\n\n",
    "int main(void) {\n",
    "    const int values[] = {1, 2, 3};\n",
    "    printf(\"Sum: %d\\n\", sum_array(values, 3));\n",
    "    return 0;\n",
    "}\n",
);

fn commit(repository: &git2::Repository, text: &str, author: &str, message: &str) -> git2::Oid {
    // Write the exact blob without checkout filters so newline tests cover both
    // LF and CRLF history, independently of the machine's Git configuration.
    let blob = repository.blob(text.as_bytes()).unwrap();
    let mut builder = repository.treebuilder(None).unwrap();
    builder.insert("example.c", blob, 0o100644).unwrap();
    let tree = repository.find_tree(builder.write().unwrap()).unwrap();
    let parent = repository
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    let signature = git2::Signature::new(
        author,
        "author@example.com",
        &git2::Time::new(1_770_000_000, 0),
    )
    .unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap()
}

fn fixture(text: &str) -> (tempfile::TempDir, PathBuf, git2::Oid) {
    let root = tempfile::tempdir().unwrap();
    let repository = git2::Repository::init(root.path()).unwrap();
    let path = root.path().join("example.c");
    std::fs::write(&path, text).unwrap();
    let id = commit(
        &repository,
        text,
        "Alex Morgan",
        "Add array summation\n\nKeep the function small and reusable.",
    );
    (root, path, id)
}

fn loaded_app(path: &Path, text: &str) -> App {
    let mut app = App::new(path.parent().unwrap().into());
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(path.into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.show_breadcrumbs = true;
    app.editor_mut().unwrap().word_wrap = false;
    app.terminal_has_focus = true;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    install(&mut app);
    app
}

fn install(app: &mut App) {
    let editor = app.editor().unwrap();
    let doc = git_blame::prepare(editor.file_path.clone().unwrap(), editor.rope.clone(), None);
    app.blame_documents
        .insert(editor.navigation_id, Arc::new(doc));
}

fn open_blame_commit_tab(remote_mode: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo_root = root.path().join("repo");
    let repo = git2::Repository::init(&repo_root).unwrap();
    commit(&repo, SOURCE, "Alex Morgan", "Initial source");
    let committed = SOURCE.replace("sum += arr[i];", "sum += arr[i] * 2;");
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let parent_tree = parent.tree().unwrap();
    let mut builder = repo.treebuilder(Some(&parent_tree)).unwrap();
    builder.insert("example.c", repo.blob(committed.as_bytes()).unwrap(), 0o100644).unwrap();
    builder.insert("other.rs", repo.blob(b"fn also_changed() {}\n").unwrap(), 0o100644).unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let signature = git2::Signature::now("Jamie Lee", "jamie@example.test").unwrap();
    let message = "Double each array item\n\nUpdate the other file in the same commit.";
    let selected = repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &[&parent]).unwrap();
    let working = committed.replace("{1, 2, 3}", "{2, 4, 6}");
    commit(&repo, &working, "Sam Rivera", "Change the entrypoint");
    let file = repo_root.join("example.c");
    std::fs::write(&file, &working).unwrap();
    let other_root = root.path().join("other");
    let other_repo = git2::Repository::init(&other_root).unwrap();
    commit(&other_repo, SOURCE, "Another Author", "Unrelated repository");

    let remote = remote_mode.then(|| crate::remote::workspace::Workspace::new(
        crate::remote::tests::connection(root.path(), Arc::default(), Arc::default()),
    ));
    let file = remote.as_ref().map_or(file, |remote| remote.target_key("repo/example.c").unwrap());
    let workspace = remote.as_ref().map_or_else(|| root.path().to_owned(), |remote| remote.view_root.clone());
    let other_root = remote.as_ref().map_or(other_root, |remote| remote.target_key("other").unwrap());
    let mut app = App::new_deferred(workspace.clone());
    app.remote_workspace = remote;
    app.source_control_enabled = true;
    app.repositories.workspace = Some(workspace);
    app.repositories.active = Some(other_root.clone());
    app.repositories.discovered = true;
    let mut editor = Editor::test_with_text(&working);
    editor.file_path = Some(file.clone());
    editor.blame.enabled = true;
    editor.cursor_line = 11;
    editor.cursor_col = 4;
    editor.word_wrap = false;
    let rope = editor.rope.clone();
    let doc = match &app.remote_workspace {
        Some(remote) => remote.prepare_blame(file, rope.clone(), None, &app.source_control_cancel),
        None => git_blame::prepare(file, rope.clone(), None),
    };
    assert!(doc.notice.is_none(), "{:?}", doc.notice);
    assert!(matches!(&doc.lines[5], git_blame::Line::Committed(commit) if commit.hash == selected.to_string()));
    assert!(matches!(&doc.lines[11], git_blame::Line::Committed(commit) if commit.hash != selected.to_string()));
    app.blame_documents.insert(editor.navigation_id, Arc::new(doc));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app.git_log.clear();
    let mut terminal = Terminal::new(TestBackend::new(170, 36)).unwrap();
    render(&mut app, &mut terminal);
    let hit = app.editor().unwrap().blame.hits.iter().find(|(_, line)| *line == 5).unwrap().0;
    pointer(&mut app, hit, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let open = app.editor().unwrap().blame.open_commit.unwrap();
    capture(&terminal, if remote_mode { "remote-open-commit" } else { "open-commit" });
    pointer(&mut app, open, MouseEventKind::Down(MouseButton::Left));
    assert!(app.is_commit_details_tab());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.commit_details.data.hash, selected.to_string());
    assert!(app.git_log.is_empty(), "blame must not require or modify graph history");
    assert_eq!(app.source_control_root(), other_root);
    assert!(app.tabs[0].editor.blame.selected.is_none());
    assert!(!app.tabs[0].editor.blame.detail_mouse_down);
    assert_eq!((app.tabs[0].editor.cursor_line, app.tabs[0].editor.cursor_col), (11, 4));
    assert!(app.tabs[0].editor.rope.is_instance(&rope));
    app.wait_for_background(Duration::from_secs(10), |app| !app.commit_details.loading);
    assert!(app.commit_details.error.is_none(), "{:?}", app.commit_details.error);
    assert_eq!(app.commit_details.data.message, message);
    assert_eq!(app.commit_details.data.author, "Jamie Lee");
    let paths: Vec<_> = app.commit_details.data.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["example.c", "other.rs"]);
    app.request_commit_diff();
    app.wait_for_background(Duration::from_secs(10), |app| !app.commit_details.diff_loading);
    assert!(app.commit_details.diff_error.is_none(), "{:?}", app.commit_details.diff_error);
    assert!(app.commit_details.diff_full_cache.is_some());
    terminal.draw(|frame| {
        let area = frame.area();
        crate::ui_chrome_tabs::draw_tab_bar(frame, &mut app, Rect::new(0, 0, area.width, 1));
        crate::ui_commit_details::draw_in_area(frame, &mut app, Rect::new(0, 1, area.width, area.height - 1));
    }).unwrap();
    let text = output(&terminal);
    assert!(text.contains("Commit details") && text.contains("Double each array item"), "{text}");
    assert!(text.contains("example.c") && text.contains("other.rs"), "{text}");
    capture(&terminal, if remote_mode { "remote-full-commit" } else { "full-commit" });

    // Reopening from blame reuses the full tab and preserves its state.
    let generation = app.commit_details.generation;
    app.commit_details.query = "other".into();
    app.active_tab = 0;
    app.editor_mut().unwrap().blame.selected = Some(5);
    render(&mut app, &mut terminal);
    let open = app.editor().unwrap().blame.open_commit.unwrap();
    pointer(&mut app, open, MouseEventKind::Down(MouseButton::Left));
    assert!(app.is_commit_details_tab());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.commit_details.generation, generation);
    assert_eq!(app.commit_details.query, "other");
    if !remote_mode {
        app.open_source_control_commit_details_file_diff(0);
        assert_eq!(app.editor().unwrap().rope.to_string(), committed);
        assert_eq!(app.compare_base_content.as_deref(), Some(SOURCE));
        assert_eq!(app.diff_target_path.as_ref().unwrap().canonicalize().unwrap(), repo_root.join("example.c").canonicalize().unwrap());
    }
}

#[test]
fn blame_opens_full_commit_tab_outside_loaded_history_and_active_repository() {
    open_blame_commit_tab(false);
}

#[test]
fn remote_blame_opens_full_commit_tab_in_the_blamed_files_repository() {
    open_blame_commit_tab(true);
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal
        .draw(|frame| {
            let area = frame.area();
            crate::ui_chrome_tabs::draw_tab_bar(
                frame,
                app,
                Rect::new(area.x, area.y, area.width, 1),
            );
            crate::ui_chrome_tabs::draw_breadcrumb(
                frame,
                app,
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
            app.editor_area = Some(area);
            crate::ui_editor::draw_editor(
                frame,
                app,
                Rect::new(
                    area.x,
                    area.y + 2,
                    area.width,
                    area.height.saturating_sub(2),
                ),
            );
        })
        .unwrap();
}

fn output(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn pointer(app: &mut App, area: Rect, kind: MouseEventKind) {
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

fn author(document: &git_blame::Document, line: usize) -> Option<&str> {
    match document.lines.get(line)? {
        git_blame::Line::Committed(commit) => Some(&commit.author),
        _ => None,
    }
}

#[test]
fn blame_retains_the_complete_commit_message_for_hover_and_details() {
    let (root, path, _) = fixture("before\n");
    let repo = git2::Repository::open(root.path()).unwrap();
    let message = format!("Full commit message\n\n{}\nFinal paragraph beyond 8192 characters.", "More explanation.\n".repeat(600));
    commit(&repo, "after\n", "Alex Morgan", &message);
    let document = git_blame::prepare(path, Rope::from_str("after\n"), None);
    let git_blame::Line::Committed(commit) = &document.lines[0] else { panic!("committed line"); };
    assert_eq!(commit.message.trim_end(), message);
}

#[test]
fn blame_history_local_edits_and_line_shifts_keep_the_correct_authors() {
    let (root, path, first) = fixture("first\nsecond\nthird\nfourth\n");
    let repo = git2::Repository::open(root.path()).unwrap();
    let second = commit(
        &repo,
        "first\nnew second\nthird\nfourth\n",
        "Jamie Lee",
        "Change second line",
    );
    let committed = git_blame::prepare(
        path.clone(),
        Rope::from_str("first\nnew second\nthird\nfourth\n"),
        None,
    );
    assert_eq!(
        author(&committed, 1),
        Some("Jamie Lee"),
        "committed baseline: {:?}",
        committed.lines
    );
    let disk = std::fs::read(&path).unwrap();
    let index = std::fs::read(root.path().join(".git/index")).ok();
    let rope = Rope::from_str("first\ninserted locally\nnew second\nfourth\n");
    let document = git_blame::prepare(path.clone(), rope.clone(), None);
    assert!(document.notice.is_none(), "{:?}", document.notice);
    assert_eq!(author(&document, 0), Some("Alex Morgan"));
    assert!(matches!(document.lines[1], git_blame::Line::Uncommitted));
    assert_eq!(
        author(&document, 2),
        Some("Jamie Lee"),
        "buffer blame: {:?}",
        document.lines
    );
    assert!(
        matches!(&document.lines[0], git_blame::Line::Committed(commit) if commit.hash == first.to_string())
    );
    assert!(
        matches!(&document.lines[2], git_blame::Line::Committed(commit) if commit.hash == second.to_string())
    );
    assert_eq!(author(&document, 3), Some("Alex Morgan"));
    assert!(document.rope.is_instance(&rope));
    assert_eq!(
        std::fs::read(path).unwrap(),
        disk,
        "blame must never write the file"
    );
    assert_eq!(repo.head().unwrap().target(), Some(second));
    assert_eq!(
        std::fs::read(root.path().join(".git/index")).ok(),
        index,
        "blame must never modify staging"
    );
}

#[test]
fn blame_checkout_newlines_do_not_mark_unchanged_text_as_local_edits() {
    for committed in ["first\nsecond\nthird\n", "first\r\nsecond\r\nthird\r\n"] {
        let (_root, path, _) = fixture(committed);
        for buffer in ["first\nsecond\nthird\n", "first\r\nsecond\r\nthird\r\n"] {
            let document = git_blame::prepare(path.clone(), Rope::from_str(buffer), None);
            assert!(document.notice.is_none(), "{:?}", document.notice);
            for line in 0..3 {
                assert_eq!(
                    author(&document, line),
                    Some("Alex Morgan"),
                    "{committed:?}, {buffer:?}"
                );
            }
        }
    }
}

#[test]
fn blame_empty_bom_mixed_endings_and_unicode_separators_are_safe() {
    let (_root, path, _) = fixture("\u{feff}one\r\ntwo\nthree\u{2028}four\n");
    for buffer in [
        "one\ntwo\nthree\u{2028}four\n",
        "one\r\ntwo\r\nthree\u{2028}four\r\n",
    ] {
        let rope = Rope::from_str(buffer);
        let doc = git_blame::prepare(path.clone(), rope.clone(), None);
        assert!(doc.notice.is_none(), "{:?}", doc.notice);
        assert_eq!(doc.lines.len(), rope.len_lines());
        // VS Code recognizes CR/LF line ends; U+2028 stays inside the line.
        assert_eq!(rope.len_lines(), 4);
        for line in 0..3 {
            assert_eq!(author(&doc, line), Some("Alex Morgan"));
        }
        assert!(matches!(doc.lines[3], git_blame::Line::Empty));
    }
    let doc = git_blame::prepare(path.clone(), Rope::from_str(""), None);
    assert!(doc.notice.is_none());
    assert!(matches!(doc.lines.as_slice(), [git_blame::Line::Empty]));
    let doc = git_blame::prepare(path, Rope::from_str("entire file replaced"), None);
    assert!(matches!(
        doc.lines.as_slice(),
        [git_blame::Line::Uncommitted]
    ));
}

#[test]
fn blame_popup_compare_opens_the_previous_revision_without_touching_unsaved_work() {
    let (root, path, _) = fixture(SOURCE);
    let repository = git2::Repository::open(root.path()).unwrap();
    let after = SOURCE.replace("return sum;", "return sum + 1;");
    let id = commit(
        &repository,
        &after,
        "Jamie Lee",
        "Correct the summation result",
    );
    let latest = format!("// a later committed header\n{after}");
    let head = commit(&repository, &latest, "Taylor Green", "Document the example");
    let unsaved = format!("// my unsaved header\n{latest}");
    let mut app = loaded_app(&path, &unsaved);
    app.editor_mut().unwrap().modified = true;
    let source = app.editor().unwrap().rope.clone();
    let disk = std::fs::read(&path).unwrap();
    let doc = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    let line = doc.lines.iter().position(|line| matches!(line, git_blame::Line::Committed(commit) if commit.hash == id.to_string())).unwrap();
    assert_eq!(
        doc.commit_lines[line],
        Some(7),
        "navigation must undo both committed and local line shifts"
    );
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(140, 28)).unwrap();
    render(&mut app, &mut terminal);
    let hit = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, source)| *source == line)
        .unwrap()
        .0;
    pointer(&mut app, hit, MouseEventKind::Down(MouseButton::Left));
    assert!(!app.editor().unwrap().blame.history_loading);
    assert_eq!(app.tabs.len(), 1);
    render(&mut app, &mut terminal);
    let compare = app.editor().unwrap().blame.previous.unwrap();
    pointer(&mut app, compare, MouseEventKind::Down(MouseButton::Left));
    assert!(app.editor().unwrap().blame.history_loading);
    app.wait_for_background(Duration::from_secs(10), |app| {
        matches!(app.tabs[app.active_tab].kind, TabKind::Diff(_))
    });
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.compare_base_content.as_deref(), Some(SOURCE));
    assert_eq!(app.editor().unwrap().rope.to_string(), after);
    assert!(app.editor().unwrap().read_only);
    assert_eq!(app.editor().unwrap().cursor_line, 7);
    assert!(app.active_diff_uses_snapshot_target());
    assert!(app.tabs[0].editor.modified && app.tabs[0].editor.rope.is_instance(&source));
    assert_eq!(std::fs::read(&path).unwrap(), disk);
    assert_eq!(repository.head().unwrap().target(), Some(head));
    assert!(app.diff_labels.as_ref().is_some());
    app.save_current_file();
    assert!(app.pending_file_saves.is_empty());
    app.execute_palette_command("read_only");
    app.execute_palette_command("read_only");
    assert!(
        app.editor().unwrap().read_only,
        "a historical snapshot cannot be unlocked with the normal editor setting"
    );
    assert_eq!(std::fs::read(&path).unwrap(), disk);
    app.refresh_visible_highlights_async();
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    terminal
        .draw(|frame| crate::ui_diff_display::draw_side_by_side_diff(frame, &mut app, frame.area()))
        .unwrap();
    let text = output(&terminal);
    assert!(
        text.contains("Previous version") && text.contains("Correct the summation result"),
        "{text}"
    );
    capture(&terminal, "previous-version");
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "i" && cell.fg != app.colors.fg_text),
        "historical revisions must retain syntax highlighting"
    );

    app.active_tab = 0;
    app.sync_diff_tab();
    render(&mut app, &mut terminal);
    git_blame::open_previous(&mut app, line);
    app.wait_for_background(Duration::from_secs(10), |app| {
        matches!(app.tabs[app.active_tab].kind, TabKind::Diff(_))
    });
    assert_eq!(
        app.tabs.len(),
        2,
        "the same commit should reuse its comparison tab"
    );
    assert!(app.tabs[0].editor.rope.is_instance(&source));
}

#[test]
fn blame_previous_version_follows_a_rename_and_handles_an_initial_commit() {
    let (root, path, _) = fixture(SOURCE);
    let repository = git2::Repository::open(root.path()).unwrap();
    let doc = git_blame::prepare(path.clone(), Rope::from_str(SOURCE), None);
    let git_blame::Line::Committed(first) = &doc.lines[0] else {
        panic!("missing first commit")
    };
    let first = git_blame::prepare_history(path.clone(), first, 0, 1024 * 1024).unwrap();
    assert!(first.diff.base_content.is_empty());
    assert!(first.before_label.contains("file did not exist"));

    let after = SOURCE.replace("return sum;", "return sum + 1;");
    let mut builder = repository.treebuilder(None).unwrap();
    builder
        .insert(
            "renamed.c",
            repository.blob(after.as_bytes()).unwrap(),
            0o100644,
        )
        .unwrap();
    let tree = repository.find_tree(builder.write().unwrap()).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let signature = git2::Signature::now("Jamie Lee", "author@example.com").unwrap();
    let id = repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Rename and correct",
            &tree,
            &[&parent],
        )
        .unwrap();
    let renamed = root.path().join("renamed.c");
    std::fs::write(&renamed, &after).unwrap();
    let doc = git_blame::prepare(renamed.clone(), Rope::from_str(&after), None);
    let info = doc
        .lines
        .iter()
        .find_map(|line| match line {
            git_blame::Line::Committed(commit) if commit.hash == id.to_string() => Some(commit),
            _ => None,
        })
        .unwrap();
    let history = git_blame::prepare_history(renamed, info, 7, 1024 * 1024).unwrap();
    assert_eq!(&*history.diff.base_content, SOURCE);
    assert_eq!(history.contents.to_string(), after);
    std::fs::remove_file(&path).unwrap();
    let snapshot = git_blame::prepare_snapshot(history.revision, 7, 1024 * 1024).unwrap();
    let previous =
        git_blame::prepare_snapshot(snapshot.previous_revision.unwrap(), 7, 1024 * 1024).unwrap();
    assert_eq!(previous.revision.path, PathBuf::from("example.c"));
    assert_eq!(previous.document.rope.to_string(), SOURCE);
    assert!(previous.document.notice.is_none());
    assert!(previous.previous_revision.is_none());
    let Err(message) = git_blame::prepare_history(path, info, 0, 1) else {
        panic!("the size limit must be enforced")
    };
    assert!(message.contains("limit"));
}

#[test]
fn blame_late_history_results_do_not_take_focus_from_another_tab() {
    let (_root, path, _) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    let doc = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    let git_blame::Line::Committed(info) = &doc.lines[0] else {
        panic!("missing commit")
    };
    let history = git_blame::prepare_history(path, info, 0, 1024 * 1024).unwrap();
    let nav = app.editor().unwrap().navigation_id;
    app.editor_mut().unwrap().blame.history_request = 42;
    app.editor_mut().unwrap().blame.history_loading = true;
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("another editor")));
    app.active_tab = 1;
    app.background
        .sender
        .send(BackgroundResult::BlameHistory {
            navigation_id: nav,
            generation: 42,
            result: Ok(history),
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(app.active_tab, 1);
    assert_eq!(app.tabs.len(), 2);
    assert!(!app.tabs[0].editor.blame.history_loading);
    assert_eq!(app.editor().unwrap().rope.to_string(), "another editor");
}

#[test]
fn blame_dates_use_the_commit_timezone_and_cancelled_navigation_stays_cancelled() {
    let (root, path, _) = fixture(SOURCE);
    let repository = git2::Repository::open(root.path()).unwrap();
    let tree = repository.head().unwrap().peel_to_tree().unwrap();
    let signature =
        git2::Signature::new("Date Tester", "date@example.com", &git2::Time::new(0, -60)).unwrap();
    let id = repository
        .commit(
            None,
            &signature,
            &signature,
            "Timezone boundary",
            &tree,
            &[],
        )
        .unwrap();
    repository.set_head_detached(id).unwrap();
    let mut app = loaded_app(&path, SOURCE);
    let doc = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    let git_blame::Line::Committed(info) = &doc.lines[0] else {
        panic!("missing commit")
    };
    assert_eq!(info.date, "1969-12-31");
    assert_eq!(info.committed_at, "1969-12-31 23:00:00 -01:00");
    let history = git_blame::prepare_history(path, info, 0, 1024 * 1024).unwrap();
    let nav = app.editor().unwrap().navigation_id;
    app.editor_mut().unwrap().blame.history_request = 7;
    app.editor_mut().unwrap().blame.history_loading = true;
    toggle(&mut app);
    app.background
        .sender
        .send(BackgroundResult::BlameHistory {
            navigation_id: nav,
            generation: 7,
            result: Ok(history),
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(app.tabs.len(), 1);
    assert!(!app.editor().unwrap().blame.history_loading);
}

#[test]
fn blame_cold_breadcrumb_loads_on_click_and_explains_the_action() {
    let (_root, path, _) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    app.blame_documents.clear();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    render(&mut app, &mut terminal);
    let button = app.editor().unwrap().blame.button.unwrap();
    pointer(&mut app, button, MouseEventKind::Moved);
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains("Load Git blame for this file."));
    assert!(!output(&terminal).contains("Blame —"));
    assert!(!app.bg_blame_running);
    pointer(&mut app, button, MouseEventKind::Down(MouseButton::Left));
    assert!(app.editor().unwrap().blame.enabled);
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains("Loading blame…"));
    git_blame::refresh(&mut app);
    assert!(app.bg_blame_running);
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_blame_running);
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains("Add array summation"));
    assert!(app.editor().unwrap().blame.enabled);
}

#[test]
fn blame_unsupported_files_explain_the_reason_and_do_not_retry_at_idle() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("example.c");
    std::fs::write(&path, SOURCE).unwrap();
    let mut app = loaded_app(&path, SOURCE);
    assert!(
        git_blame::cached(&app, app.editor().unwrap())
            .unwrap()
            .notice
            .as_ref()
            .unwrap()
            .contains("Git repository")
    );
    app.blame_requested_path = Some(path.clone());
    for _ in 0..5 {
        git_blame::refresh(&mut app);
        assert!(!app.bg_blame_running);
    }
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    render(&mut app, &mut terminal);
    let button = app.editor().unwrap().blame.button.unwrap();
    let style = terminal.backend().buffer()[(button.x + 1, button.y)].style();
    pointer(&mut app, button, MouseEventKind::Moved);
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains("Git repository"));
    assert_eq!(
        terminal.backend().buffer()[(button.x + 1, button.y)].style(),
        style
    );
    pointer(&mut app, button, MouseEventKind::Down(MouseButton::Left));
    assert!(!app.editor().unwrap().blame.enabled);
    assert!(app.editor().unwrap().blame.panel.is_none());

    let repository = git2::Repository::init(root.path()).unwrap();
    let document = git_blame::prepare(path.clone(), Rope::from_str(SOURCE), None);
    assert!(document.notice.unwrap().contains("no committed history"));
    commit(&repository, "tracked\n", "Jamie Lee", "Create history");
    let document = git_blame::prepare(
        root.path().join("untracked.c"),
        Rope::from_str(SOURCE),
        None,
    );
    assert!(document.notice.unwrap().contains("no committed history"));
}

#[test]
fn blame_stale_same_size_and_version_results_cannot_replace_the_current_buffer() {
    let (_root, path, _) = fixture("before\n");
    let mut app = loaded_app(&path, "before\n");
    let document = git_blame::cached(&app, app.editor().unwrap())
        .unwrap()
        .clone();
    let nav = app.editor().unwrap().navigation_id;
    let version = app.editor().unwrap().content_version;
    app.editor_mut().unwrap().rope = Rope::from_str("after!\n");
    assert_eq!(app.editor().unwrap().content_version, version);
    assert!(git_blame::cached(&app, app.editor().unwrap()).is_none());
    app.bg_blame_running = true;
    app.background
        .sender
        .send(BackgroundResult::BlameCache {
            navigation_id: nav,
            file_path: path,
            document,
        })
        .unwrap();
    app.poll_background_results();
    assert!(!app.bg_blame_running);
    assert!(git_blame::cached(&app, app.editor().unwrap()).is_none());
    toggle(&mut app);
    git_blame::refresh(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_blame_running);
    assert!(matches!(
        git_blame::cached(&app, app.editor().unwrap())
            .unwrap()
            .lines[0],
        git_blame::Line::Uncommitted
    ));
}

#[test]
fn blame_toggle_and_commit_inspector_do_not_change_source_or_other_tabs() {
    let (_root, path, id) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("another editor")));
    let before = app.editor().unwrap().rope.clone();
    let mut terminal = Terminal::new(TestBackend::new(110, 25)).unwrap();
    render(&mut app, &mut terminal);
    let button = app.editor().unwrap().blame.button.unwrap();
    pointer(&mut app, button, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().blame.button, Some(button));
    assert!(!app.tabs[1].editor.blame.enabled);
    let hit = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 5)
        .unwrap()
        .0;
    let code = app.code_area.unwrap();
    let scroll = app.editor().unwrap().scroll_offset;
    let cursor = app.editor().unwrap().cursor_line;
    pointer(&mut app, hit, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let text = output(&terminal);
    assert!(text.contains(&id.to_string()), "{text}");
    assert!(text.contains("Add array summation"));
    assert!(text.contains("Alex Morgan") && text.contains("author@example.com"));
    assert!(text.contains("Open Commit") && text.contains("Compare") && text.contains("Open File") && text.contains("Copy Hash"));
    assert_eq!(
        app.code_area,
        Some(code),
        "a popup must not resize the source"
    );
    assert_eq!(app.editor().unwrap().scroll_offset, scroll);
    assert_eq!(app.tabs.len(), 2, "opening details cannot navigate");
    let panel = app.editor().unwrap().blame.panel.unwrap();
    assert!(panel.x >= hit.right() && panel.y > code.y && panel.bottom() < code.bottom());
    assert!(app.editor().unwrap().blame.copy.is_some());
    assert_eq!(app.editor().unwrap().cursor_line, cursor);
    assert!(app.editor().unwrap().rope.is_instance(&before));
    pointer(&mut app, hit, MouseEventKind::Down(MouseButton::Left));
    assert!(app.editor().unwrap().blame.selected.is_none());
    assert!(app.editor().unwrap().blame.panel.is_none());
    assert!(app.editor().unwrap().blame.copy.is_none());
    assert!(app.editor().unwrap().blame.previous.is_none());
    assert!(app.editor().unwrap().blame.open_file.is_none());
    assert!(app.editor().unwrap().blame.open_commit.is_none());
    render(&mut app, &mut terminal);
    assert!(!output(&terminal).contains("Commit details"));
    pointer(&mut app, hit, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.editor().unwrap().blame.selected.is_none());
    assert!(app.editor().unwrap().blame.panel.is_none());
    pointer(&mut app, button, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().blame.button, Some(button));
    assert!(app.editor().unwrap().rope.is_instance(&before));
}

#[test]
fn blame_annotations_follow_folds_and_wrapped_source_rows() {
    let (_root, path, _) = fixture(SOURCE);
    let text = SOURCE.replace(
        "    int sum = 0;",
        &format!("    // {}", "a long wrapped source comment ".repeat(8)),
    );
    let mut app = loaded_app(&path, &text);
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(72, 28)).unwrap();
    app.editor_mut().unwrap().folded_ranges.insert(4, 7);
    render(&mut app, &mut terminal);
    let lines: Vec<_> = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .map(|(_, line)| *line)
        .collect();
    assert!(lines.contains(&4) && lines.contains(&7));
    assert!(!lines.contains(&5) && !lines.contains(&6));
    let folds = app.editor().unwrap().folded_ranges.clone();
    let code = app.code_area;
    let cursor = app.editor().unwrap().cursor_line;
    let fold_hit = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 4)
        .unwrap()
        .0;
    pointer(&mut app, fold_hit, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().folded_ranges, folds);
    assert_eq!(app.editor().unwrap().cursor_line, cursor);
    assert_eq!(app.code_area, code);
    let card = app.editor().unwrap().blame.panel.unwrap();
    assert_eq!(card.y, fold_hit.bottom());
    assert_eq!(
        app.editor()
            .unwrap()
            .blame
            .hits
            .iter()
            .find(|(_, line)| *line == 7)
            .unwrap()
            .0
            .y,
        card.bottom(),
        "the next visible source line follows the card after a folded block"
    );
    pointer(&mut app, fold_hit, MouseEventKind::Down(MouseButton::Left));
    assert!(app.editor().unwrap().blame.selected.is_none());
    app.editor_mut().unwrap().unfold_all();
    app.editor_mut().unwrap().word_wrap = true;
    render(&mut app, &mut terminal);
    let source = &app.editor().unwrap().blame.rows;
    assert!(
        source
            .iter()
            .filter(|row| row.is_some_and(|row| row.line_idx == 3))
            .count()
            > 1
    );
    for (area, line) in &app.editor().unwrap().blame.hits {
        let row = (area.y - app.code_area.unwrap().y) as usize;
        assert_eq!(source[row].unwrap().line_idx, *line);
    }
    let before = app.editor().unwrap().rope.clone();
    let wrapped_hit = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 3)
        .unwrap()
        .0;
    pointer(
        &mut app,
        wrapped_hit,
        MouseEventKind::Down(MouseButton::Left),
    );
    render(&mut app, &mut terminal);
    let editor = app.editor().unwrap();
    let card = editor.blame.panel.unwrap();
    let wrapped_rows: Vec<_> = editor
        .ai_review
        .source_rows
        .iter()
        .enumerate()
        .filter_map(|(row, source)| {
            source
                .filter(|source| source.line_idx == 3)
                .map(|source| (row, source))
        })
        .collect();
    assert!(wrapped_rows.len() > 1);
    let (last_row, last_segment) = wrapped_rows.last().unwrap();
    assert_eq!(last_segment.end_col, editor.line_len(3));
    assert_eq!(card.y, app.code_area.unwrap().y + *last_row as u16 + 1);
    assert_eq!(
        editor
            .blame
            .hits
            .iter()
            .find(|(_, line)| *line == 4)
            .unwrap()
            .0
            .y,
        card.bottom(),
        "details follow the entire wrapped line without splitting its source text"
    );
    assert!(editor.rope.is_instance(&before));
    capture(&terminal, "inline-wrapped-line");
}

#[test]
fn blame_annotation_clicks_in_split_panes_focus_the_owning_file() {
    let (_root, path, _) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    toggle(&mut app);
    let first = app.editor().unwrap().navigation_id;
    let mut other = Editor::test_with_text(SOURCE);
    other.file_path = Some(path.clone());
    other.blame.enabled = true;
    let doc = git_blame::prepare(path, other.rope.clone(), None);
    app.blame_documents
        .insert(other.navigation_id, Arc::new(doc));
    app.tabs.push(Tab::new_file(other));
    app.create_split_group_at(1, TabDropZone::Right);
    app.switch_group(0);
    let mut terminal = Terminal::new(TestBackend::new(140, 28)).unwrap();
    terminal
        .draw(|f| {
            crate::ui_helpers::draw_split_editor_for_group(
                f,
                &mut app,
                Rect::new(70, 0, 70, 28),
                1,
            );
            crate::ui_chrome_tabs::draw_breadcrumb(f, &mut app, Rect::new(0, 1, 70, 1));
            crate::ui_editor::draw_editor(f, &mut app, Rect::new(0, 2, 70, 26));
        })
        .unwrap();
    assert_eq!(app.editor().unwrap().navigation_id, first);
    let other = &app.groups[1].tabs[0].editor;
    assert!(
        other.blame.button.is_some(),
        "inactive panes must retain their own toggle"
    );
    let hit = other
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 3)
        .unwrap()
        .0;
    pointer(&mut app, hit, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(app.active_group, 1);
    assert_eq!(app.editor().unwrap().blame.selected, Some(3));
    app.switch_group(0);
    assert!(app.editor().unwrap().blame.enabled);
    assert!(app.editor().unwrap().blame.selected.is_none());
}

#[test]
fn blame_ai_review_virtual_rows_have_no_misattributed_authors() {
    use vtcode::acp::rollback::{FileReview, InlineChunk};
    let (_root, path, _) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    app.chat.rollback.enabled = true;
    app.chat.rollback.inline_reviews = Arc::new(vec![FileReview {
        path,
        text: SOURCE.into(),
        chunks: vec![
            InlineChunk {
                edit: 1,
                index: 0,
                range: Some(0..SOURCE.find('\n').unwrap() + 1),
                original: "removed line\nremoved next line\n".into(),
                line: 1,
                issue: None,
            },
            InlineChunk {
                edit: 2,
                index: 0,
                range: Some(
                    SOURCE.find("    return sum;\n").unwrap()
                        ..SOURCE.find("    return sum;\n").unwrap() + "    return sum;\n".len(),
                ),
                original: "    return 0;\n".into(),
                line: 8,
                issue: None,
            },
        ],
    }]);
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    render(&mut app, &mut terminal);
    let editor = app.editor().unwrap();
    assert!(editor.ai_review.active);
    assert!(editor.ai_review.source_rows.iter().any(Option::is_none));
    for (area, line) in &editor.blame.hits {
        let row = (area.y - app.code_area.unwrap().y) as usize;
        assert_eq!(editor.ai_review.source_rows[row].unwrap().line_idx, *line);
    }
    let following_action = editor
        .ai_review
        .hits
        .iter()
        .find(|hit| hit.chunk == Some((2, 0)))
        .unwrap()
        .area;
    let anchor = editor
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 6)
        .unwrap()
        .0;
    pointer(&mut app, anchor, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let editor = app.editor().unwrap();
    let card = editor.blame.panel.unwrap();
    let shifted = editor
        .ai_review
        .hits
        .iter()
        .find(|hit| hit.chunk == Some((2, 0)))
        .unwrap()
        .area;
    assert_eq!(shifted.y, following_action.y + card.height);
    assert_eq!(
        shifted.y,
        card.bottom(),
        "AI actions for the next line follow the blame card"
    );
    assert!(
        !editor
            .ai_review
            .hits
            .iter()
            .any(|hit| !hit.area.intersection(card).is_empty())
    );
    for (area, line) in &editor.blame.hits {
        let row = (area.y - app.code_area.unwrap().y) as usize;
        assert_eq!(editor.ai_review.source_rows[row].unwrap().line_idx, *line);
    }
    capture(&terminal, "inline-with-ai-review");
}

#[test]
fn blame_inspector_scroll_is_bounded_and_unicode_text_is_preserved() {
    let (_root, path, _) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    let editor = app.editor().unwrap();
    let mut doc = git_blame::prepare(path, editor.rope.clone(), None);
    if let git_blame::Line::Committed(commit) = &doc.lines[0] {
        doc.lines[0] = git_blame::Line::Committed(Arc::new(git_blame::Commit {
            hash: commit.hash.clone(),
            author: commit.author.clone(),
            age: commit.age.clone(),
            date: commit.date.clone(),
            committed_at: commit.committed_at.clone(),
            summary: commit.summary.clone(),
            path: commit.path.clone(),
            email: commit.email.clone(),
            message: (0..30)
                .map(|i| format!("Line {i} · UTF-8 你好 é\n"))
                .collect(),
        }));
    }
    app.blame_documents
        .insert(editor.navigation_id, Arc::new(doc));
    toggle(&mut app);
    app.editor_mut().unwrap().blame.selected = Some(0);
    let mut terminal = Terminal::new(TestBackend::new(64, 22)).unwrap();
    render(&mut app, &mut terminal);
    let panel = app.editor().unwrap().blame.panel.unwrap();
    let content = app.editor().unwrap().blame.detail_content.unwrap();
    let scrollbar = app.editor().unwrap().blame.detail_scrollbar.unwrap();
    let code = app.code_area.unwrap();
    let cursor = app.editor().unwrap().cursor_line;
    let source = app.editor().unwrap().rope.clone();
    let controls = (
        app.editor().unwrap().blame.open_commit,
        app.editor().unwrap().blame.previous,
        app.editor().unwrap().blame.open_file,
        app.editor().unwrap().blame.copy,
    );
    assert!(controls.0.is_some() && controls.1.is_some() && controls.2.is_some() && controls.3.is_some());
    assert_eq!(scrollbar.y, content.y);
    assert_eq!(scrollbar.height, content.height);
    assert_eq!(
        terminal.backend().buffer()[(scrollbar.x, scrollbar.y)].symbol(),
        "█"
    );
    capture(&terminal, "scrollable-details");
    for _ in 0..100 {
        pointer(&mut app, panel, MouseEventKind::ScrollDown);
    }
    render(&mut app, &mut terminal);
    assert_eq!(
        app.editor().unwrap().blame.detail_scroll,
        app.editor().unwrap().blame.detail_max_scroll
    );
    assert!(output(&terminal).contains("Line 29"));
    assert_eq!(app.code_area, Some(code));
    assert_eq!(
        (
            app.editor().unwrap().blame.open_commit,
            app.editor().unwrap().blame.previous,
            app.editor().unwrap().blame.open_file,
            app.editor().unwrap().blame.copy
        ),
        controls,
        "scrolling the message cannot move the actions"
    );
    capture(&terminal, "scrolled-details");
    let thumb = app.editor().unwrap().blame.detail_thumb.unwrap();
    let max_scroll = app.editor().unwrap().blame.detail_max_scroll;
    pointer(&mut app, thumb, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(
        app.editor().unwrap().blame.detail_scroll,
        max_scroll,
        "grabbing must not jump"
    );
    let outside = Rect::new(code.x, panel.y.saturating_sub(2), 1, 1);
    pointer(&mut app, outside, MouseEventKind::Drag(MouseButton::Left));
    pointer(&mut app, outside, MouseEventKind::Up(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().blame.detail_scroll, 0);
    assert!(
        app.editor().unwrap().selection_anchor.is_none(),
        "popup dragging cannot select source"
    );
    assert_eq!(app.editor().unwrap().cursor_line, cursor);
    assert!(app.editor().unwrap().rope.is_instance(&source));
    let end = Rect::new(scrollbar.x, scrollbar.bottom() - 1, 1, 1);
    pointer(&mut app, end, MouseEventKind::Down(MouseButton::Left));
    pointer(&mut app, end, MouseEventKind::Up(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().blame.detail_scroll, max_scroll);
    assert!(output(&terminal).contains("Line 29"));
    for _ in 0..100 {
        pointer(&mut app, panel, MouseEventKind::ScrollUp);
    }
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().blame.detail_scroll, 0);
    let rows = wrapped_message("你好 é 👨‍👩‍👧‍👦", 4);
    assert!(rows.iter().all(|row| row.width() <= 4));
    assert_eq!(
        rows.iter()
            .flat_map(|row| &row.spans)
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "你好 é 👨‍👩‍👧‍👦"
    );
}

#[test]
fn blame_inline_details_switch_commits_and_clear_actions_for_uncommitted_lines() {
    let (root, path, original) = fixture(SOURCE);
    let repo = git2::Repository::open(root.path()).unwrap();
    let after = SOURCE.replace("return sum;", "return sum + 1;");
    let newer = commit(&repo, &after, "Jamie Lee", "Correct the summation result");
    let mut app = loaded_app(&path, &format!("// unsaved\n{after}"));
    let source = app.editor().unwrap().rope.clone();
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(110, 25)).unwrap();
    render(&mut app, &mut terminal);
    let hit = |app: &App, line| {
        app.editor()
            .unwrap()
            .blame
            .hits
            .iter()
            .find(|(_, source)| *source == line)
            .unwrap()
            .0
    };
    let first = hit(&app, 1);
    pointer(&mut app, first, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains(&original.to_string()));
    let second = hit(&app, 8);
    pointer(&mut app, second, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains(&newer.to_string()));
    assert!(output(&terminal).contains("Jamie Lee"));
    assert_eq!(app.editor().unwrap().blame.detail_scroll, 0);
    let local = hit(&app, 0);
    pointer(&mut app, local, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains("Uncommitted changes"));
    assert!(app.editor().unwrap().blame.panel.is_some());
    assert!(app.editor().unwrap().blame.previous.is_none());
    assert!(app.editor().unwrap().blame.open_file.is_none());
    assert!(app.editor().unwrap().blame.open_commit.is_none());
    assert!(app.editor().unwrap().blame.copy.is_none());
    let close = app.editor().unwrap().blame.close.unwrap();
    pointer(&mut app, close, MouseEventKind::Down(MouseButton::Left));
    pointer(&mut app, close, MouseEventKind::Up(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert!(app.editor().unwrap().blame.panel.is_none());
    assert_eq!(app.tabs.len(), 1);
    assert!(app.editor().unwrap().rope.is_instance(&source));
    pointer(&mut app, first, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let code = app.code_area.unwrap();
    let outside = Rect::new(code.x + 2, code.y, 1, 1);
    assert!(
        !app.editor()
            .unwrap()
            .blame
            .panel
            .unwrap()
            .contains((outside.x, outside.y).into())
    );
    pointer(&mut app, outside, MouseEventKind::Down(MouseButton::Left));
    pointer(&mut app, outside, MouseEventKind::Up(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert!(
        app.editor().unwrap().blame.panel.is_some(),
        "inline details stay open while using source"
    );
    assert!(app.editor().unwrap().rope.is_instance(&source));
}

#[test]
fn blame_details_insert_rows_below_the_line_and_shift_source_and_annotations_together() {
    let (_root, path, _) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(110, 25)).unwrap();
    render(&mut app, &mut terminal);
    app.refresh_visible_highlights_async();
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    render(&mut app, &mut terminal);
    let before = app.editor().unwrap().rope.clone();
    let disk = std::fs::read(&path).unwrap();
    let code = app.code_area.unwrap();
    let original_hits = app.editor().unwrap().blame.hits.clone();
    let anchor = original_hits.iter().find(|(_, line)| *line == 5).unwrap().0;
    let following = original_hits.iter().find(|(_, line)| *line == 6).unwrap().0;
    let read_row = |terminal: &Terminal<TestBackend>, y| {
        (code.x..code.right())
            .map(|x| terminal.backend().buffer()[(x, y)].clone())
            .collect::<Vec<_>>()
    };
    let source_below = read_row(&terminal, following.y);
    let source_above = read_row(&terminal, anchor.y);
    pointer(&mut app, anchor, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let panel = app.editor().unwrap().blame.panel.unwrap();
    assert_eq!(
        panel.y,
        anchor.bottom(),
        "the card belongs immediately below the selected line"
    );
    let shifted = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 6)
        .unwrap()
        .0;
    assert_eq!(shifted.y, following.y + panel.height);
    assert_eq!(
        read_row(&terminal, shifted.y),
        source_below,
        "inserted rows preserve syntax and source cells"
    );
    assert_eq!(read_row(&terminal, anchor.y), source_above);
    for y in panel.y..panel.bottom() {
        let row = (y - code.y) as usize;
        assert!(app.editor().unwrap().ai_review.source_rows[row].is_none());
        assert!(
            !app.editor()
                .unwrap()
                .blame
                .hits
                .iter()
                .any(|(area, _)| area.y == y)
        );
    }
    capture(&terminal, "inline-details");
    assert!(app.editor().unwrap().rope.is_instance(&before));
    assert_eq!(std::fs::read(&path).unwrap(), disk);
    pointer(&mut app, anchor, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(app.editor().unwrap().blame.hits, original_hits);
    assert_eq!(read_row(&terminal, following.y), source_below);
    assert!(app.editor().unwrap().blame.panel.is_none());
    assert!(!app.editor().unwrap().ai_review.active);
    assert!(app.editor().unwrap().rope.is_instance(&before));
}

#[test]
fn blame_inline_scrolling_clips_controls_and_keeps_source_selection_and_scrollbar_correct() {
    let text: String = (0..80)
        .map(|line| format!("int value_{line:02} = {line};\n"))
        .collect();
    let (root, path, _) = fixture(&text);
    let repo = git2::Repository::open(root.path()).unwrap();
    let message: String = (0..40)
        .map(|line| format!("Details line {line:02}\n"))
        .collect();
    repo.head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .amend(Some("HEAD"), None, None, None, Some(&message), None)
        .unwrap();
    let mut app = loaded_app(&path, &text);
    let source = app.editor().unwrap().rope.clone();
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();
    render(&mut app, &mut terminal);
    let anchor = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 3)
        .unwrap()
        .0;
    pointer(&mut app, anchor, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let code = app.code_area.unwrap();
    let hit_for = |app: &App, line| {
        app.editor()
            .unwrap()
            .blame
            .hits
            .iter()
            .find(|(_, source)| *source == line)
            .unwrap()
            .0
    };
    let following = hit_for(&app, 4);
    let click = Rect::new(code.x + 4, following.y, 1, 1);
    pointer(&mut app, click, MouseEventKind::Down(MouseButton::Left));
    pointer(&mut app, click, MouseEventKind::Up(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(
        app.editor().unwrap().cursor_line,
        4,
        "clicks below the card address real source"
    );
    assert_eq!(app.editor().unwrap().blame.selected, Some(3));
    let start = Rect::new(code.x, hit_for(&app, 2).y, 1, 1);
    let panel = app.editor().unwrap().blame.panel.unwrap();
    pointer(&mut app, start, MouseEventKind::Down(MouseButton::Left));
    let through = Rect::new(code.x + 3, panel.y + 2, 1, 1);
    pointer(&mut app, through, MouseEventKind::Drag(MouseButton::Left));
    assert_eq!(
        app.editor().unwrap().cursor_line,
        4,
        "dragging through inserted rows selects real text"
    );
    let end = Rect::new(code.x + 3, hit_for(&app, 6).y, 1, 1);
    pointer(&mut app, end, MouseEventKind::Drag(MouseButton::Left));
    pointer(&mut app, end, MouseEventKind::Up(MouseButton::Left));
    assert_eq!(app.editor().unwrap().cursor_line, 6);
    assert_eq!(app.editor().unwrap().selection_anchor, Some((2, 0)));
    assert!(app.editor().unwrap().rope.is_instance(&source));
    render(&mut app, &mut terminal);
    let rail = Rect::new(0, code.y, 1, 1);
    for _ in 0..3 {
        pointer(&mut app, rail, MouseEventKind::ScrollDown);
    }
    render(&mut app, &mut terminal);
    assert!(app.editor().unwrap().blame.panel.is_some());
    assert!(
        app.editor().unwrap().blame.close.is_none(),
        "offscreen actions have no live hit targets"
    );
    assert!(app.editor().unwrap().blame.previous.is_none());
    let thumb = app.editor().unwrap().blame.detail_thumb.unwrap();
    pointer(&mut app, thumb, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(
        app.editor().unwrap().blame.detail_scroll,
        0,
        "grabbing a clipped thumb must not jump"
    );
    let bottom = Rect::new(thumb.x, code.bottom() + 2, 1, 1);
    pointer(&mut app, bottom, MouseEventKind::Drag(MouseButton::Left));
    pointer(&mut app, bottom, MouseEventKind::Up(MouseButton::Left));
    render(&mut app, &mut terminal);
    assert_eq!(
        app.editor().unwrap().blame.detail_scroll,
        app.editor().unwrap().blame.detail_max_scroll
    );
    assert!(output(&terminal).contains("Details line 39"));
    capture(&terminal, "inline-clipped-details");
    for _ in 0..100 {
        pointer(&mut app, rail, MouseEventKind::ScrollDown);
    }
    render(&mut app, &mut terminal);
    assert!(app.editor().unwrap().blame.panel.is_none());
    assert!(
        output(&terminal).contains("value_79"),
        "expanded rows cannot hide the file's end"
    );
    for _ in 0..6 {
        crate::input::handle_editor_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
        render(&mut app, &mut terminal);
    }
    assert!(app.editor().unwrap().blame.close.is_some());
    assert!(app.editor().unwrap().blame.previous.is_some());
    assert_eq!(app.editor().unwrap().blame.selected, Some(3));
    assert!(app.editor().unwrap().rope.is_instance(&source));
}

#[test]
fn blame_inline_reveal_reflows_when_a_long_line_adds_the_horizontal_scrollbar() {
    let text: String = (0..80)
        .map(|line| {
            if line == 30 {
                format!("// {}\n", "long text ".repeat(30))
            } else {
                format!("int value_{line:02} = {line};\n")
            }
        })
        .collect();
    let (_root, path, _) = fixture(&text);
    let mut app = loaded_app(&path, &text);
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(110, 25)).unwrap();
    render(&mut app, &mut terminal);
    assert!(app.h_scrollbar_area.is_none());
    let anchor = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, line)| *line == 20)
        .unwrap()
        .0;
    pointer(&mut app, anchor, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let code = app.code_area.unwrap();
    let scrollbar = app
        .h_scrollbar_area
        .expect("the newly visible long line needs a scrollbar");
    let editor = app.editor().unwrap();
    let panel = editor.blame.panel.unwrap();
    assert!(panel.bottom() <= code.bottom());
    assert!(panel.bottom() <= scrollbar.y);
    assert_eq!(editor.ai_review.source_rows.len(), code.height as usize);
    for area in [
        editor.blame.close,
        editor.blame.open_commit,
        editor.blame.previous,
        editor.blame.open_file,
        editor.blame.copy,
        editor.blame.detail_scrollbar,
    ]
    .into_iter()
    .flatten()
    {
        assert!(area.y >= code.y && area.bottom() <= code.bottom());
    }
    assert_eq!(editor.rope.to_string(), text);
    capture(&terminal, "inline-reflowed-viewport");
}

fn capture(terminal: &Terminal<TestBackend>, name: &str) {
    let Some(directory) = std::env::var_os("VTCODE_CHAT_PRESENTATION_CAPTURE") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})).collect();
    std::fs::write(directory.join(format!("blame-{name}.json")), serde_json::to_vec(&serde_json::json!({"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
}

#[test]
fn blame_layout_keeps_syntax_colors_and_theme_roles_across_pane_sizes() {
    let (_root, path, _) = fixture(SOURCE);
    for theme in ["dark", "light", "high-contrast"] {
        let mut app = loaded_app(&path, SOURCE);
        app.set_theme_by_key(theme);
        let mut terminal = Terminal::new(TestBackend::new(110, 25)).unwrap();
        render(&mut app, &mut terminal);
        app.refresh_visible_highlights_async();
        app.wait_for_background(Duration::from_secs(15), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        render(&mut app, &mut terminal);
        let old_code = app.code_area.unwrap();
        let before = terminal.backend().buffer()[(old_code.x, old_code.y)].fg;
        toggle(&mut app);
        render(&mut app, &mut terminal);
        let code = app.code_area.unwrap();
        assert_eq!(
            terminal.backend().buffer()[(code.x, code.y)].fg,
            before,
            "{theme}: toggling blame must preserve syntax colors"
        );
        assert!(output(&terminal).contains("Add array summation"));
        let doc = git_blame::cached(&app, app.editor().unwrap()).unwrap();
        let date = match &doc.lines[0] {
            git_blame::Line::Committed(commit) => &commit.date,
            _ => panic!("missing commit"),
        };
        assert!(date.parse::<chrono::NaiveDate>().is_ok());
        assert!(
            output(&terminal).matches(date).count() >= SOURCE.lines().count(),
            "every source line must show its commit date"
        );
        capture(&terminal, &format!("{theme}-rail"));
        app.editor_mut().unwrap().blame.selected = Some(5);
        render(&mut app, &mut terminal);
        capture(&terminal, &format!("{theme}-details"));
        for (width, height) in [(1, 3), (8, 8), (24, 14), (40, 20)] {
            terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            render(&mut app, &mut terminal);
            let bounds = terminal.backend().buffer().area;
            for (area, _) in &app.editor().unwrap().blame.hits {
                assert_eq!(area.intersection(bounds), *area);
            }
            let view = &app.editor().unwrap().blame;
            for area in [
                view.panel,
                view.close,
                view.open_commit,
                view.previous,
                view.open_file,
                view.copy,
                view.detail_content,
                view.detail_scrollbar,
                view.detail_thumb,
            ]
            .into_iter()
            .flatten()
            {
                assert_eq!(area.intersection(bounds), area);
            }
            if width >= 24 {
                assert!(view.open_commit.is_some() && view.previous.is_some() && view.open_file.is_some() && view.copy.is_some());
                capture(&terminal, &format!("{theme}-narrow-details-{width}"));
            }
        }
    }
}

#[test]
fn blame_files_walk_multiple_revisions_and_return_to_unsaved_work() {
    let (root, path, first) = fixture(SOURCE);
    let repo = git2::Repository::open(root.path()).unwrap();
    let second_text = SOURCE.replace("return sum;", "return sum + 1;");
    let second = commit(
        &repo,
        &second_text,
        "Jamie Lee",
        "Correct the summation result",
    );
    let third_text = SOURCE.replace("return sum;", "return sum + 2;");
    let third = commit(
        &repo,
        &third_text,
        "Taylor Green",
        "Revise the summation result",
    );
    let latest = format!("// a later committed header\n{third_text}");
    let head = commit(&repo, &latest, "Alex Morgan", "Document the example");
    let unsaved = format!("// my unsaved header\n{latest}");
    let mut app = loaded_app(&path, &unsaved);
    app.editor_mut().unwrap().modified = true;
    let source = app.editor().unwrap().rope.clone();
    let disk = std::fs::read(&path).unwrap();
    let index = std::fs::read(root.path().join(".git/index")).ok();
    let line = git_blame::cached(&app, app.editor().unwrap()).unwrap().lines.iter()
        .position(|line| matches!(line, git_blame::Line::Committed(info) if info.hash == third.to_string())).unwrap();
    toggle(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(140, 28)).unwrap();
    render(&mut app, &mut terminal);
    let hit = app
        .editor()
        .unwrap()
        .blame
        .hits
        .iter()
        .find(|(_, row)| *row == line)
        .unwrap()
        .0;
    pointer(&mut app, hit, MouseEventKind::Down(MouseButton::Left));
    render(&mut app, &mut terminal);
    let open = app.editor().unwrap().blame.open_file.unwrap();
    pointer(&mut app, open, MouseEventKind::Down(MouseButton::Left));
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.tabs[app.active_tab].kind == TabKind::GitRevision
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), third_text);
    assert_eq!(app.editor().unwrap().cursor_line, 7);
    assert!(app.editor().unwrap().blame.enabled);
    for (id, text) in [(second, second_text.as_str()), (first, SOURCE)] {
        render(&mut app, &mut terminal);
        let previous = app.editor().unwrap().blame.revision_previous.unwrap();
        pointer(&mut app, previous, MouseEventKind::Down(MouseButton::Left));
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.editor()
                .unwrap()
                .blame
                .revision
                .as_ref()
                .is_some_and(|revision| revision.hash == id.to_string())
        });
        assert_eq!(app.editor().unwrap().rope.to_string(), text);
        assert!(app.editor().unwrap().read_only);
        assert!(
            git_blame::cached(&app, app.editor().unwrap())
                .unwrap()
                .lines
                .iter()
                .all(|line| !matches!(line, git_blame::Line::Uncommitted))
        );
    }
    render(&mut app, &mut terminal);
    capture(&terminal, "revision-file");
    assert!(app.editor().unwrap().blame.previous_revision.is_none());
    let previous = app.editor().unwrap().blame.revision_previous.unwrap();
    let count = app.tabs.len();
    pointer(&mut app, previous, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(app.tabs.len(), count);
    assert!(!app.editor().unwrap().blame.history_loading);
    app.execute_palette_command("read_only");
    app.execute_palette_command("read_only");
    assert!(app.editor().unwrap().read_only);
    app.execute_palette_command("save");
    assert!(
        app.notification_history
            .last()
            .unwrap()
            .0
            .contains("cannot be saved")
    );
    app.execute_palette_command("save_as");
    assert!(!app.save_as_active);
    assert_eq!(std::fs::read(&path).unwrap(), disk);
    assert_eq!(std::fs::read(root.path().join(".git/index")).ok(), index);
    assert_eq!(repo.head().unwrap().target(), Some(head));
    app.navigate_back();
    assert_eq!(
        app.editor().unwrap().blame.revision.as_ref().unwrap().hash,
        second.to_string()
    );
    app.navigate_forward();
    assert_eq!(
        app.editor().unwrap().blame.revision.as_ref().unwrap().hash,
        first.to_string()
    );
    render(&mut app, &mut terminal);
    let working = app.editor().unwrap().blame.working_file.unwrap();
    pointer(&mut app, working, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(app.tabs[app.active_tab].kind, TabKind::File);
    assert!(app.editor().unwrap().rope.is_instance(&source));
    assert!(app.editor().unwrap().modified);
}

#[test]
fn blame_comparison_opens_either_file_and_reuses_revision_tabs() {
    let (root, path, first) = fixture(SOURCE);
    let repo = git2::Repository::open(root.path()).unwrap();
    let after = SOURCE.replace("return sum;", "return sum + 1;");
    let second = commit(&repo, &after, "Jamie Lee", "Correct the summation result");
    let mut app = loaded_app(&path, &after);
    let document = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    let git_blame::Line::Committed(info) = &document.lines[7] else {
        panic!("missing changed line");
    };
    let history = git_blame::prepare_history(path, info, 7, 1024 * 1024).unwrap();
    app.open_blame_history(history);
    let comparison = app.active_tab;
    let mut terminal = Terminal::new(TestBackend::new(140, 28)).unwrap();
    render(&mut app, &mut terminal);
    assert!(output(&terminal).contains("Previous file"));
    assert!(output(&terminal).contains("Open file"));
    let before = app.editor().unwrap().blame.diff_previous.unwrap();
    let after_button = app.editor().unwrap().blame.diff_committed.unwrap();
    pointer(&mut app, before, MouseEventKind::Down(MouseButton::Left));
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.tabs[app.active_tab].kind == TabKind::GitRevision
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), SOURCE);
    assert_eq!(
        app.editor().unwrap().blame.revision.as_ref().unwrap().hash,
        first.to_string()
    );
    app.active_tab = comparison;
    app.sync_diff_tab();
    render(&mut app, &mut terminal);
    pointer(
        &mut app,
        after_button,
        MouseEventKind::Down(MouseButton::Left),
    );
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.tabs[app.active_tab].kind == TabKind::GitRevision
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), after);
    assert_eq!(
        app.editor().unwrap().blame.revision.as_ref().unwrap().hash,
        second.to_string()
    );
    let count = app.tabs.len();
    app.active_tab = comparison;
    app.sync_diff_tab();
    render(&mut app, &mut terminal);
    pointer(
        &mut app,
        after_button,
        MouseEventKind::Down(MouseButton::Left),
    );
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.tabs[app.active_tab].kind == TabKind::GitRevision
    });
    assert_eq!(app.tabs.len(), count);
}

#[test]
fn blame_historical_blame_survives_head_changes_and_disk_reloads() {
    let (root, path, first) = fixture(SOURCE);
    let repo = git2::Repository::open(root.path()).unwrap();
    let mut app = loaded_app(&path, SOURCE);
    let doc = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    let git_blame::Line::Committed(info) = &doc.lines[0] else {
        panic!("missing commit");
    };
    let history = git_blame::prepare_history(path.clone(), info, 0, 1024 * 1024).unwrap();
    let revision = history.revision;
    app.open_blame_file(git_blame::prepare_snapshot(revision.clone(), 0, 1024 * 1024).unwrap());
    let snapshot = app.editor().unwrap().rope.clone();
    let replacement = SOURCE.replace("return sum;", "return sum + 1;");
    std::fs::write(&path, &replacement).unwrap();
    commit(&repo, &replacement, "Jamie Lee", "A later change");
    app.background
        .sender
        .send(BackgroundResult::FileReloaded {
            targets: app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs))
                .map(|tab| (tab.editor.navigation_id, tab.editor.content_version)).collect(),
            workspace_generation: 0,
            path: path.clone(),
            result: Ok(crate::editor::load_text_file(&path).unwrap()),
        })
        .unwrap();
    app.poll_background_results();
    assert!(app.editor().unwrap().rope.is_instance(&snapshot));
    app.blame_documents.clear();
    git_blame::refresh(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_blame_running);
    let doc = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    assert_eq!(doc.key.as_ref().unwrap().head_id, first.to_string());
    assert!(doc.lines.iter().all(
        |line| matches!(line, git_blame::Line::Committed(info) if info.hash == first.to_string())
            || matches!(line, git_blame::Line::Empty)
    ));
    assert_eq!(app.editor().unwrap().rope.to_string(), SOURCE);
    assert_eq!(std::fs::read_to_string(path).unwrap(), replacement);
}

#[test]
fn blame_late_snapshot_results_and_cancelled_requests_do_not_navigate() {
    let (_root, path, _) = fixture(SOURCE);
    let mut app = loaded_app(&path, SOURCE);
    let doc = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    let git_blame::Line::Committed(info) = &doc.lines[0] else {
        panic!("missing commit");
    };
    let revision = git_blame::prepare_history(path, info, 0, 1024 * 1024)
        .unwrap()
        .revision;
    let nav = app.editor().unwrap().navigation_id;
    app.editor_mut().unwrap().blame.history_request = 42;
    app.editor_mut().unwrap().blame.history_loading = true;
    app.active_pane = ActivePane::Chat;
    let snapshot = git_blame::prepare_snapshot(revision.clone(), 0, 1024 * 1024).unwrap();
    assert!(!git_blame::finish_snapshot(&mut app, nav, 42, Ok(snapshot)));
    assert_eq!(app.tabs.len(), 1);
    assert!(!app.editor().unwrap().blame.history_loading);
    app.active_pane = ActivePane::Editor;
    app.editor_mut().unwrap().blame.history_request = 43;
    app.editor_mut().unwrap().blame.history_loading = true;
    toggle(&mut app);
    let snapshot = git_blame::prepare_snapshot(revision, 0, 1024 * 1024).unwrap();
    assert!(!git_blame::finish_snapshot(&mut app, nav, 43, Ok(snapshot)));
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn blame_revision_highlights_are_isolated_from_the_working_file() {
    let (root, path, _) = fixture(SOURCE);
    let repo = git2::Repository::open(root.path()).unwrap();
    let mut app = loaded_app(&path, SOURCE);
    app.set_theme_by_key("dark");
    let document = git_blame::cached(&app, app.editor().unwrap()).unwrap();
    let git_blame::Line::Committed(info) = &document.lines[0] else {
        panic!("missing commit");
    };
    let original = git_blame::prepare_history(path.clone(), info, 0, 1024 * 1024)
        .unwrap()
        .revision;
    let comment = format!("/*\n{SOURCE}");
    commit(&repo, &comment, "Jamie Lee", "Start a multiline comment");
    let doc = git_blame::prepare(path.clone(), Rope::from_str(&comment), None);
    let git_blame::Line::Committed(info) = &doc.lines[0] else {
        panic!("missing comment commit");
    };
    let commented = git_blame::prepare_history(path.clone(), info, 0, 1024 * 1024)
        .unwrap()
        .revision;
    let mut terminal = Terminal::new(TestBackend::new(140, 28)).unwrap();
    let warm = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
        render(app, terminal);
        app.refresh_visible_highlights_async();
        app.wait_for_background(Duration::from_secs(15), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        render(app, terminal);
    };
    warm(&mut app, &mut terminal);
    let code = app.code_area.unwrap();
    let original_color = terminal.backend().buffer()[(code.x + 1, code.y)].fg;
    assert_ne!(original_color, app.colors.fg_text);
    app.open_blame_file(git_blame::prepare_snapshot(commented, 1, 1024 * 1024).unwrap());
    warm(&mut app, &mut terminal);
    let code = app.code_area.unwrap();
    let commented_color = terminal.backend().buffer()[(code.x + 1, code.y + 1)].fg;
    assert_ne!(commented_color, original_color);
    app.open_blame_file(git_blame::prepare_snapshot(original, 0, 1024 * 1024).unwrap());
    warm(&mut app, &mut terminal);
    let code = app.code_area.unwrap();
    assert_eq!(
        terminal.backend().buffer()[(code.x + 1, code.y)].fg,
        original_color
    );
    capture(&terminal, "revision-colored-file");
    app.active_tab = 0;
    warm(&mut app, &mut terminal);
    let code = app.code_area.unwrap();
    assert_eq!(
        terminal.backend().buffer()[(code.x + 1, code.y)].fg,
        original_color
    );
    for width in [1, 16, 35, 65] {
        app.active_tab = 2;
        terminal = Terminal::new(TestBackend::new(width, 18)).unwrap();
        render(&mut app, &mut terminal);
        let bounds = terminal.backend().buffer().area;
        let view = &app.editor().unwrap().blame;
        for control in [view.button, view.working_file, view.revision_previous]
            .into_iter()
            .flatten()
        {
            assert_eq!(control.intersection(bounds), control);
        }
        if width >= 35 {
            assert!(
                view.revision_previous.is_some(),
                "history navigation must fit a narrow pane"
            );
        }
    }
}

#[test]
fn blame_navigation_keeps_unicode_separators_within_a_git_line() {
    let text = "first\nsecond\u{2028}third\nfourth\n";
    let (root, path, _) = fixture(text);
    let repo = git2::Repository::open(root.path()).unwrap();
    let latest = format!("a newer header\n{text}");
    commit(&repo, &latest, "Jamie Lee", "Add a header");
    let mut app = loaded_app(&path, &latest);
    git_blame::open_line_file(&mut app, 2);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.tabs[app.active_tab].kind == TabKind::GitRevision
    });
    assert_eq!(app.editor().unwrap().cursor_line, 1);
    assert_eq!(app.editor().unwrap().rope.line(1).to_string(), "second\u{2028}third\n");
    app.active_tab = 0;
    git_blame::open_previous(&mut app, 2);
    app.wait_for_background(Duration::from_secs(10), |app| {
        matches!(app.tabs[app.active_tab].kind, TabKind::Diff(_))
    });
    assert_eq!(app.editor().unwrap().cursor_line, 1);
}
