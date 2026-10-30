use super::*;
use crate::merge_conflicts::Index;
use ratatui::{Terminal, backend::TestBackend, style::Color};
use std::time::Duration;

fn merged_repository() -> (tempfile::TempDir, git2::Repository, String) {
    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    let path = directory.path().join("file.txt");
    let original: String = (0..120)
        .map(|line| format!("unchanged line {line}\n"))
        .collect();
    let ours = original
        .replace("unchanged line 25\n", "current conflict 25\n")
        .replace("unchanged line 85\n", "current conflict 85\n");
    let theirs = original
        .replace("unchanged line 25\n", "incoming conflict 25\n")
        .replace("unchanged line 85\n", "incoming conflict 85\n")
        .replace("unchanged line 110\n", "incoming independent change\n");
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    let commit = |text: &str, parent: Option<&git2::Commit<'_>>| {
        std::fs::write(&path, text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        index.write().unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "test",
            &tree,
            &parent.into_iter().collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let base = repo.find_commit(commit(&original, None)).unwrap();
    commit(&ours, Some(&base));
    let mut index = git2::Index::new().unwrap();
    index.read_tree(&base.tree().unwrap()).unwrap();
    let mut entry = index.get_path(Path::new("file.txt"), 0).unwrap();
    entry.id = repo.blob(theirs.as_bytes()).unwrap();
    index.add(&entry).unwrap();
    let tree = repo.find_tree(index.write_tree_to(&repo).unwrap()).unwrap();
    let incoming = repo
        .commit(
            Some("refs/heads/incoming"),
            &signature,
            &signature,
            "incoming",
            &tree,
            &[&base],
        )
        .unwrap();
    repo.merge(
        &[&repo.find_annotated_commit(incoming).unwrap()],
        None,
        None,
    )
    .unwrap();
    assert!(repo.index().unwrap().has_conflicts());
    let resolved = ours.replace("unchanged line 110\n", "incoming independent change\n");
    drop(tree);
    drop(base);
    (directory, repo, resolved)
}

fn refresh(app: &mut App) {
    app.refresh_git_line_status_async();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_diff_running && !app.bg_git_diff_pending
    });
}

fn assert_sparse_conflict_markers(app: &mut App) {
    let editor = app.editor().unwrap();
    let index = Index::update(None, editor.rope.clone());
    assert_eq!(index.regions.len(), 2);
    let mut unchanged = 0;
    for (line, text) in editor.rope.lines().enumerate() {
        if index
            .regions
            .iter()
            .any(|region| (region.start..=region.end).contains(&line))
        {
            continue;
        }
        let expected = if text.to_string().starts_with("incoming independent") {
            'U'
        } else {
            ' '
        };
        assert_eq!(
            editor.line_diff_status(line),
            expected,
            "line {line}: {text}"
        );
        unchanged += usize::from(expected == ' ');
    }
    assert!(
        unchanged > 100,
        "unchanged lines between and outside conflicts stay neutral"
    );
    assert!(
        !editor.git_line_status().contains(&'S'),
        "unmerged stages are not staged edits"
    );
    for region in &index.regions {
        assert_eq!(editor.line_diff_status(region.start), 'U');
        assert_eq!(editor.line_diff_status(region.end), 'U');
    }
    app.editor_mut().unwrap().merge_conflicts.index = Some(Arc::new(index));

    // Exercise the actual inline conflict renderer, including its virtual rows.
    app.show_minimap = true;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    app.colors.ui.fg_gutter_modified = Color::Rgb(1, 2, 3);
    app.colors.ui.fg_minimap_modified = Color::Rgb(4, 5, 6);
    app.colors.ui.fg_overview_modified = Color::Rgb(7, 8, 9);
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal
        .draw(|frame| crate::editor_ai_review::draw(frame, app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let gutter = app.gutter_area.unwrap();
    let code = app.code_area.unwrap();
    let editor = app.editor().unwrap();
    let start = editor
        .merge_conflicts
        .current(&editor.rope)
        .unwrap()
        .regions[0]
        .start;
    for (line, changed) in [(0, false), (start, true)] {
        let row = editor
            .ai_review
            .source_rows
            .iter()
            .position(|row| row.is_some_and(|row| row.line_idx == line))
            .unwrap();
        assert_eq!(
            (gutter.x..gutter.right())
                .any(|x| buffer[(x, code.y + row as u16)].fg == app.colors.ui.fg_gutter_modified),
            changed
        );
    }
    for (area, color) in [
        (app.minimap_area.unwrap(), app.colors.ui.fg_minimap_modified),
        (
            app.scrollbar_area.unwrap(),
            app.colors.ui.fg_overview_modified,
        ),
    ] {
        let colored = (area.y..area.bottom())
            .filter(|&y| (area.x..area.right()).any(|x| buffer[(x, y)].fg == color))
            .count();
        assert!(
            colored > 0 && colored < usize::from(area.height) / 2,
            "only actual change regions should color the overview: {colored} of {}",
            area.height
        );
    }
}

#[test]
fn unmerged_file_has_sparse_gutter_minimap_and_scrollbar_markers_locally_and_remotely() {
    let (directory, repo, _) = merged_repository();
    let original_index = std::fs::read(repo.path().join("index")).unwrap();
    let text = std::fs::read_to_string(directory.path().join("file.txt")).unwrap();
    for remote in [false, true] {
        let mut app = App::new_deferred(directory.path().to_path_buf());
        let path = if remote {
            let workspace = crate::remote::workspace::Workspace::new(
                crate::remote::tests::connection(directory.path(), Arc::default(), Arc::default()),
            );
            let key =
                workspace.view_key(&crate::remote::RemotePath::new(b"file.txt".to_vec()).unwrap());
            workspace.client.set_wake(app.background.wake_handle());
            app.remote_workspace = Some(workspace);
            key
        } else {
            directory.path().join("file.txt")
        };
        let mut editor = Editor::test_with_text(&text);
        editor.file_path = Some(path);
        editor.saved_rope = Some(editor.rope.clone());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.source_control_enabled = true;
        refresh(&mut app);
        assert_sparse_conflict_markers(&mut app);
    }
    assert_eq!(
        std::fs::read(repo.path().join("index")).unwrap(),
        original_index
    );
}

#[test]
fn unmerged_line_baselines_handle_deleted_sides_and_keep_real_head_and_index_separate() {
    use crate::remote::{Command, RemotePath, Reply};
    let (directory, repo, _) = merged_repository();
    let path = Path::new("file.txt");
    let entries: Vec<_> = (1..=3)
        .map(|stage| repo.index().unwrap().get_path(path, stage).unwrap())
        .collect();
    let head = crate::app_git::head_file_content(&repo, path, false).unwrap();
    let client = crate::remote::tests::connection(directory.path(), Arc::default(), Arc::default());
    for (stages, baseline) in [
        (vec![1, 2, 3], Some(2)),
        (vec![2, 3], Some(2)),
        (vec![1, 3], Some(1)),
        (vec![1, 2], Some(2)),
        (vec![3], Some(3)),
        (vec![], None),
    ] {
        let mut index = repo.index().unwrap();
        index.remove_path(path).unwrap();
        for &stage in &stages {
            index.add(&entries[stage - 1]).unwrap();
        }
        index.write().unwrap();
        let expected = baseline.map(|stage| {
            String::from_utf8(
                repo.find_blob(entries[stage - 1].id)
                    .unwrap()
                    .content()
                    .to_vec(),
            )
            .unwrap()
        });
        let (content, conflicted) = crate::app_git::line_diff_index_content(&repo, path).unwrap();
        assert_eq!(content.as_deref(), expected.as_deref(), "stages {stages:?}");
        assert_eq!(conflicted, baseline.is_some());
        assert!(
            crate::app_git::index_file_content(&repo, path)
                .unwrap()
                .is_none()
        );
        let Reply::GitSources(sources) = client
            .call(Command::GitSources {
                file: RemotePath::new(b"file.txt".to_vec()).unwrap(),
                repository: None,
                staged: false,
            })
            .unwrap()
        else {
            panic!("Git sources reply")
        };
        assert_eq!(sources.conflicted, conflicted);
        assert_eq!(sources.index, content);
        assert_eq!(
            sources.head, head,
            "explicit HEAD comparison keeps its actual source"
        );
    }
}

#[test]
fn resolved_unmerged_file_keeps_only_real_changes_before_and_after_staging() {
    let (directory, _repo, resolved) = merged_repository();
    let path = directory.path().join("file.txt");
    std::fs::write(&path, &resolved).unwrap();
    let mut app = App::new_deferred(directory.path().to_path_buf());
    let mut editor = Editor::test_with_text(&resolved);
    editor.file_path = Some(path);
    editor.saved_rope = Some(editor.rope.clone());
    app.tabs = vec![Tab::new_file(editor)];
    app.source_control_enabled = true;
    for staged in [false, true] {
        if staged {
            crate::app_git::changes::stage(
                directory.path(),
                &[GitFileChange {
                    directory: false,
                    path: "file.txt".to_string(),
                    status: '!',
                    staged: false,
                    original_path: None,
                }],
            )
            .unwrap();
        }
        refresh(&mut app);
        assert_eq!(
            app.editor().unwrap().git_change_lines(),
            &[(110, if staged { 'S' } else { 'U' })]
        );
    }
}
