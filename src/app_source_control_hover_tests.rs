use super::*;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::fs;

fn stage(repo: &git2::Repository, paths: &[&str]) {
    let mut index = repo.index().unwrap();
    for path in paths {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
}

fn commit(repo: &git2::Repository) {
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let author = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &author, &author, "Initial", &tree, &[])
        .unwrap();
}

fn change(path: &str, status: char, staged: bool) -> GitFileChange {
    GitFileChange {
        path: path.into(),
        original_path: None,
        status,
        staged,
        directory: false,
    }
}

#[test]
fn reports_lines_for_renames_deletions_and_untracked_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let repo = git2::Repository::init(root).unwrap();
    fs::write(root.join("before[1].txt"), "a\nb\nc\nd\ne\n").unwrap();
    fs::write(root.join("gone.txt"), "first\nlast").unwrap();
    stage(&repo, &["before[1].txt", "gone.txt"]);
    assert_eq!(
        read_stats(root, &change("gone.txt", 'A', true)).unwrap(),
        ChangeStats::Text {
            added: 2,
            deleted: 0
        },
    );
    commit(&repo);

    fs::rename(root.join("before[1].txt"), root.join("after[1].txt")).unwrap();
    fs::write(root.join("after[1].txt"), "a\nb\nc\nd\ne\nf\n").unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("before[1].txt")).unwrap();
    index.add_path(Path::new("after[1].txt")).unwrap();
    index.write().unwrap();
    let mut renamed = change("after[1].txt", 'R', true);
    renamed.original_path = Some("before[1].txt".into());
    assert_eq!(
        read_stats(root, &renamed).unwrap(),
        ChangeStats::Text {
            added: 1,
            deleted: 0
        }
    );

    fs::remove_file(root.join("gone.txt")).unwrap();
    fs::write(root.join("new.txt"), "first\nlast").unwrap();
    fs::write(root.join("empty.txt"), "").unwrap();
    fs::write(root.join("binary.dat"), b"x\0y").unwrap();
    for (file, expected) in [
        (
            change("gone.txt", 'D', false),
            ChangeStats::Text {
                added: 0,
                deleted: 2,
            },
        ),
        (
            change("new.txt", '?', false),
            ChangeStats::Text {
                added: 2,
                deleted: 0,
            },
        ),
        (
            change("empty.txt", '?', false),
            ChangeStats::Text {
                added: 0,
                deleted: 0,
            },
        ),
        (change("binary.dat", '?', false), ChangeStats::Binary),
    ] {
        assert_eq!(read_stats(root, &file).unwrap(), expected, "{file:?}");
    }
}

fn screen_text(buffer: &Buffer) -> String {
    buffer.content().iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn row_hover_uses_the_staged_or_unstaged_row_and_refreshes_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = "patch.txt";
    let repo = git2::Repository::init(root).unwrap();
    fs::write(root.join(path), "keep\nold\nlast\n").unwrap();
    stage(&repo, &[path]);
    commit(&repo);
    fs::write(root.join(path), "keep\nstaged\nlast\n").unwrap();
    stage(&repo, &[path]);
    fs::write(root.join(path), "keep\nstaged\nextra\nextra2").unwrap();

    let mut app = App::new_deferred(root.to_owned());
    app.show_explorer = true;
    app.sidebar_view = SidebarView::SourceControl;
    app.source_control_repository_state = SourceControlRepositoryState::Available;
    app.sc_graph_collapsed = true;
    app.update_source_control_files(vec![change(path, 'M', true), change(path, 'M', false)]);
    let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
    let draw = |terminal: &mut Terminal<TestBackend>, app: &mut App| {
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    };
    draw(&mut terminal, &mut app);

    for (staged, added) in [(true, 1), (false, 2)] {
        let action = app
            .source_control_file_actions
            .iter()
            .find(|row| row.change.staged == staged)
            .unwrap();
        app.mouse_row = action.area.y;
        app.mouse_col = if staged {
            action.area.x - 1
        } else {
            action.area.right() + 1
        };
        assert_eq!(
            terminal.backend().buffer()[(app.mouse_col, app.mouse_row)].symbol(),
            if staged { " " } else { "M" },
        );
        draw(&mut terminal, &mut app);
        let now = Instant::now();
        poll(&mut app, now);
        assert_eq!(next_wait(&app, now), Some(HOVER_DELAY));
        assert!(summary(&app).is_none());
        poll(&mut app, now + HOVER_DELAY);
        app.wait_for_background(Duration::from_secs(5), |app| summary(app).is_some());
        draw(&mut terminal, &mut app);
        assert!(
            screen_text(terminal.backend().buffer())
                .contains(&format!("+{added} added   -1 removed"))
        );
        assert_eq!(summary(&app).unwrap().0.staged, staged);
    }

    fs::write(root.join(path), "keep\nstaged\nlast\nextra\n").unwrap();
    let now = Instant::now();
    app.source_control_last_refresh = Some(now);
    draw(&mut terminal, &mut app);
    assert!(!screen_text(terminal.backend().buffer()).contains("+2 added"));
    poll(&mut app, now);
    poll(&mut app, now + HOVER_DELAY);
    app.wait_for_background(Duration::from_secs(5), |app| summary(app).is_some());
    draw(&mut terminal, &mut app);
    assert!(screen_text(terminal.backend().buffer()).contains("+1 added   -0 removed"));

    let action = app
        .source_control_file_actions
        .iter()
        .find(|row| !row.change.staged)
        .unwrap();
    app.mouse_col = action.area.x;
    assert!(summary(&app).is_none());
    draw(&mut terminal, &mut app);
    poll(&mut app, Instant::now());
    assert!(next_wait(&app, Instant::now()).is_none());
    let text = screen_text(terminal.backend().buffer());
    assert!(!text.contains("added   -"));
    assert!(text.contains("Open File"));
}

#[test]
fn remote_line_counts_use_repository_relative_paths() {
    use crate::remote::{RemotePath, tests::connection, workspace::Workspace};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("nested repo");
    fs::create_dir(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/patch [1].txt"), "first\nlast").unwrap();
    stage(&repo, &["sub/patch [1].txt"]);
    let remote = Workspace::new(connection(
        temp.path(),
        Default::default(),
        Default::default(),
    ));
    let root_key = remote.view_key(&RemotePath::new(b"nested repo".to_vec()).unwrap());
    assert_eq!(
        remote
            .git_file_stats(
                &root_key,
                change("sub/patch [1].txt", 'A', true),
                &CancellationToken::new()
            )
            .unwrap(),
        ChangeStats::Text {
            added: 2,
            deleted: 0
        },
    );
}
