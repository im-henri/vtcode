use super::{collect_source_control_status, repository_relative_path};
use crate::app::{
    App, GitBranchPickerItem, GitFileChange, SourceControlRepositoryState, Tab,
};
use crate::app_git_display;
use crate::editor::Editor;
use crate::explorer::{FileExplorer, GitStatus};
use lsp_types::{Position, Range, TextEdit};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "app_git_hunk_tests.rs"]
mod hunk_edge_tests;

#[path = "app_git_line_conflict_tests.rs"]
mod line_conflict_tests;

#[path = "app_unsaved_hunk_tests.rs"]
mod unsaved_hunk_tests;

#[path = "app_external_save_tests.rs"]
mod external_save_tests;

#[path = "diff_language_tests.rs"]
mod diff_language_tests;

#[test]
fn a_second_save_with_unchanged_text_still_queues_changed_encoding_and_save_options() {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("text  ");
    let path = temp_file_path("save-encoding");
    editor.file_path = Some(path.clone());
    app.tabs = vec![Tab::new_file(editor)];
    let version = app.tabs[0].editor.content_version;
    app.pending_file_saves.insert(path.clone(), (app.workspace_generation, version.into()));
    app.tabs[0].editor.encoding = "UTF-16 LE";
    let request = app.tabs[0].editor.background_save_request(true, true, false).unwrap();
    app.queue_background_save_job(request);
    let (_, latest) = app.deferred_file_saves.get(&path).expect("the second save must not be dropped");
    assert_eq!(latest.encoding, "UTF-16 LE");
    assert!(latest.trim_trailing && latest.insert_final_newline);
    app.tabs[0].editor.encoding = "UTF-8 with BOM";
    let request = app.tabs[0].editor.background_save_request(false, false, false).unwrap();
    app.queue_background_save_job(request);
    let (_, latest) = &app.deferred_file_saves[&path];
    assert_eq!(latest.encoding, "UTF-8 with BOM");
    assert!(!latest.trim_trailing && !latest.insert_final_newline);
}

#[test]
fn undo_to_clean_text_cannot_close_while_an_older_save_is_still_writing() {
    for close_all in [false, true] {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("saved");
        let path = temp_file_path("close-pending-save");
        editor.file_path = Some(path.clone());
        editor.saved_rope = Some(editor.rope.clone());
        editor.insert_char('X');
        let version = editor.content_version;
        editor.undo();
        assert!(!editor.modified);
        app.tabs = vec![Tab::new_file(editor)];
        app.pending_file_saves.insert(path.clone(), (app.workspace_generation, version.into()));
        if close_all { app.close_saved_tabs(); } else { app.close_tab(0); }
        assert!(app.tabs.iter().any(|tab| tab.editor.file_path.as_ref() == Some(&path)),
            "the tab must retain the undone text until the disk checkpoint is known");
    }
}

#[test]
fn a_save_started_after_the_close_prompt_cannot_race_its_save_or_discard_buttons() {
    for answer in ['y', 'n'] {
        let mut app = App::test_new();
        let path = temp_file_path("close-save-race");
        app.tabs = vec![make_modified_file_tab(&path, "keep edits")];
        app.close_tab(0);
        assert!(app.confirm_close_active);
        app.pending_file_saves.insert(path.clone(), (app.workspace_generation, 0.into()));
        crate::input_pickers::handle_confirm_close_key(&mut app, crossterm::event::KeyCode::Char(answer));
        assert!(app.confirm_close_active);
        assert_eq!(app.tabs[0].editor.rope, "keep edits");
        assert!(!path.exists());
    }
}

#[test]
fn save_and_quit_keeps_documents_open_on_errors_or_pending_writes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for failure in ["missing-parent", "untitled", "encoding", "pending"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(if failure == "missing-parent" { "missing/file.txt" } else { "file.txt" });
        let mut app = App::test_new();
        let mut tab = make_modified_file_tab(&path, "unsaved 界");
        if failure == "untitled" { tab.editor.file_path = None; }
        if failure == "encoding" { tab.editor.encoding = "ASCII"; }
        app.tabs = vec![tab];
        app.exit_confirm_active = true;
        if failure == "pending" { app.pending_file_saves.insert(path.clone(), (app.workspace_generation, 0.into())); }
        assert_eq!(crate::input_dialogs::handle_exit_confirm_key(&mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)), Some(false), "{failure}");
        assert!(app.exit_confirm_active, "{failure}");
        assert!(app.tabs[0].editor.modified);
        assert_eq!(app.tabs[0].editor.rope, "unsaved 界");
        assert!(!path.exists());
        assert!(!app.notification_history.is_empty());
    }
}

#[test]
fn save_and_quit_saves_one_snapshot_across_split_views_with_undoable_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "saved").unwrap();
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("saved");
    editor.file_path = Some(path.clone());
    editor.saved_rope = Some(editor.rope.clone());
    editor.paste_plain_with_text("edit  \n");
    app.tabs = vec![Tab::new_file(editor)];
    app.duplicate_tab(0);
    app.create_split_group(1);
    app.trim_trailing_on_save = true;
    app.insert_final_newline = true;
    crate::input_dialogs::save_text_documents_before_exit(&mut app).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "edit\nsaved\n");
    for tab in app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs)) {
        if tab.kind == crate::app::TabKind::File {
            assert_eq!(tab.editor.rope, "edit\nsaved\n");
            assert!(!tab.editor.modified);
        }
    }
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "edit  \nsaved");
    assert!(app.editor().unwrap().modified);
}

fn temp_file_path(label: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("vtcode-{label}-{unique}.txt"))
}

fn make_modified_file_tab(path: &Path, contents: &str) -> Tab {
    let mut editor = Editor::new();
    editor.file_path = Some(path.to_path_buf());
    editor.rope = ropey::Rope::from_str(contents);
    editor.modified = true;
    Tab::new_file(editor)
}

fn make_clean_file_tab(path: &Path, contents: &str) -> Tab {
    let mut tab = make_modified_file_tab(path, contents);
    tab.editor.modified = false;
    tab
}

#[test]
fn formatter_working_copies_never_appear_in_source_control() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    let copy = dir.path().join(".vtcode-format-test");
    std::fs::create_dir(&copy).unwrap();
    std::fs::write(copy.join("main.py"), "temporary text").unwrap();
    std::fs::write(dir.path().join("main.py"), "value=1\n").unwrap();
    let status = collect_source_control_status(dir.path().to_path_buf());
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].path, "main.py");
    let mut app = App::new(dir.path().to_path_buf());
    app.git_stage_all();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_git_action_running);
    let repo = git2::Repository::open(dir.path()).unwrap();
    let index = repo.index().unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index.get(0).unwrap().path, b"main.py");
    super::commit_repository(dir.path(), "Formatting fixture", true).unwrap();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    assert!(tree.get_path(Path::new(".vtcode-format-test/main.py")).is_err());
    assert!(!dir.path().join(".gitignore").exists());
}

#[test]
fn queued_save_separates_typing_before_and_after_its_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pending.txt");
    std::fs::write(&path, "original").unwrap();
    let mut app = App::new(dir.path().to_path_buf());
    let mut editor = Editor::test_with_text("original");
    editor.file_path = Some(path.clone());
    editor.saved_rope = Some(editor.rope.clone());
    editor.cursor_col = 8;
    editor.insert_char('a');
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    // Hold the job in the real save queue so no worker race or disk write is
    // required to exercise the grouping boundary at request submission.
    app.pending_file_saves.insert(path.clone(), (app.workspace_generation, 0.into()));
    let request = app.tabs[0].editor.background_save_request(false, false, false).unwrap();
    app.queue_background_save_job(request);
    let (_, request) = app.deferred_file_saves.remove(&path).unwrap();
    let editor = &mut app.tabs[0].editor;
    editor.insert_char('b');
    editor.apply_background_save(request.source_content_version, &request.rope, request.rope.clone(), request.encoding, None, None, None, u64::MAX);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "originala");
    assert!(!editor.modified);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "original");
    assert!(editor.modified);
}

fn temp_repo_dir(label: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("vtcode-{label}-{unique}"))
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// Fixture construction and simple object reads do not need an external Git
// process. Application actions below still exercise their real CLI commands.
fn configure_git_fixture(repo: &git2::Repository) {
    let mut config = repo.config().unwrap();
    config.set_bool("core.autocrlf", false).unwrap();
    config.set_str("user.email", "test@example.com").unwrap();
    config.set_str("user.name", "VTCode Test").unwrap();
}

fn init_git_fixture(path: &Path) -> git2::Repository {
    let repo = git2::Repository::init(path).unwrap();
    configure_git_fixture(&repo);
    repo
}

fn commit_fixture_files(path: &Path, files: &[&str], message: &str) -> git2::Oid {
    let repo = git2::Repository::open(path).unwrap();
    let mut index = repo.index().unwrap();
    for file in files {
        index.add_path(Path::new(file)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = repo.signature().unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

fn fixture_oid(path: &Path, revision: &str) -> git2::Oid {
    git2::Repository::open(path)
        .unwrap()
        .revparse_single(revision)
        .unwrap()
        .id()
}

fn fixture_branch(path: &Path) -> String {
    git2::Repository::open(path)
        .unwrap()
        .head()
        .unwrap()
        .shorthand()
        .unwrap()
        .into()
}

fn fixture_stash_count(path: &Path) -> usize {
    let mut repo = git2::Repository::open(path).unwrap();
    let mut count = 0;
    repo.stash_foreach(|_, _, _| {
        count += 1;
        true
    })
    .unwrap();
    count
}

fn make_git_app_with_file(
    label: &str,
    initial: &str,
    updated: &str,
) -> (std::path::PathBuf, std::path::PathBuf, App) {
    let repo_dir = temp_repo_dir(label);
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let file_path = repo_dir.join("file.txt");
    std::fs::write(&file_path, initial).expect("initial file");
    init_git_fixture(&repo_dir);
    commit_fixture_files(&repo_dir, &["file.txt"], "initial");
    std::fs::write(&file_path, updated).expect("updated file");

    let mut app = App::new(repo_dir.clone());
    app.tabs = vec![make_clean_file_tab(&file_path, updated)];
    app.active_tab = 0;
    (repo_dir, file_path, app)
}

#[test]
fn git_remote_branch_and_stash_round_trip_with_spaced_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let repo_dir = temporary.path().join("worktree with spaces 世界");
    let remote = temporary.path().join("bare remote with spaces.git");
    let peer = temporary.path().join("second checkout");
    std::fs::create_dir(&repo_dir).unwrap();
    std::fs::create_dir(&remote).unwrap();
    git2::Repository::init_bare(&remote).unwrap();
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("linux-test");
    let repo = git2::Repository::init_opts(&repo_dir, &options).unwrap();
    configure_git_fixture(&repo);
    let file = repo_dir.join("file.txt");
    std::fs::write(&file, "initial\n").unwrap();
    commit_fixture_files(&repo_dir, &["file.txt"], "initial");
    repo.remote("origin", remote.to_str().unwrap()).unwrap();
    drop(repo);
    let mut app = App::new(repo_dir.clone());
    app.tabs = vec![make_clean_file_tab(&file, "initial\n")];
    app.active_tab = 0;
    let settle = |app: &mut App| {
        app.wait_for_background(std::time::Duration::from_secs(10), |app| {
            !app.bg_git_action_running && !app.bg_git_remote_running
                && !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
    };

    app.execute_palette_command("git_push");
    settle(&mut app);
    assert_eq!(
        fixture_oid(&remote, "refs/heads/linux-test"),
        fixture_oid(&repo_dir, "HEAD")
    );
    let repo = git2::Repository::open(&repo_dir).unwrap();
    let branch = repo
        .find_branch("linux-test", git2::BranchType::Local)
        .unwrap();
    assert_eq!(
        branch.upstream().unwrap().name().unwrap(),
        Some("origin/linux-test")
    );
    drop(branch);
    drop(repo);
    let peer_repo = git2::build::RepoBuilder::new()
        .branch("linux-test")
        .clone(remote.to_str().unwrap(), &peer)
        .unwrap();
    configure_git_fixture(&peer_repo);
    drop(peer_repo);
    std::fs::write(peer.join("file.txt"), "from remote\n").unwrap();
    commit_fixture_files(&peer, &["file.txt"], "peer change");
    run_git(&peer, &["push"]);
    app.execute_palette_command("git_fetch");
    settle(&mut app);
    assert_eq!(
        fixture_oid(&repo_dir, "origin/linux-test"),
        fixture_oid(&peer, "HEAD")
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "initial\n");
    app.execute_palette_command("git_pull");
    settle(&mut app);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "from remote\n");
    assert_eq!(app.tabs[0].editor.rope.to_string(), "from remote\n");

    std::fs::write(&file, "stashed on Linux\n").unwrap();
    app.execute_palette_command("git_stash");
    settle(&mut app);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "from remote\n");
    assert_eq!(fixture_stash_count(&repo_dir), 1);
    app.execute_palette_command("git_stash_pop");
    settle(&mut app);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "stashed on Linux\n");
    assert_eq!(app.tabs[0].editor.rope.to_string(), "stashed on Linux\n");
    assert_eq!(fixture_stash_count(&repo_dir), 0);
    // Clean the worktree before switching branches so checkout can succeed.
    std::fs::write(&file, "from remote\n").unwrap();
    app.create_and_checkout_branch("feature/linux-probe");
    settle(&mut app);
    assert_eq!(fixture_branch(&repo_dir), "feature/linux-probe");
    app.checkout_git_branch_item(GitBranchPickerItem::Local { name: "linux-test".into() });
    settle(&mut app);
    assert_eq!(fixture_branch(&repo_dir), "linux-test");
}

#[test]
fn staged_moves_appear_once_in_source_control() {
    let head = (0..30).map(|i| format!("original line {i}\n")).collect::<String>();
    for edited in [false, true] {
        let (repo_dir, path, app) = make_git_app_with_file("staged-move-status", &head, &head);
        let moved = repo_dir.join("nested/moved.txt");
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::rename(&path, &moved).unwrap();
        if edited {
            std::fs::write(&moved, head.replace("original line 15", "edited line 15")).unwrap();
        }
        run_git(&repo_dir, &["add", "-A"]);
        let snapshot = collect_source_control_status(repo_dir.clone());
        assert_eq!(snapshot.files.len(), 1, "a staged move must be one change");
        let change = &snapshot.files[0];
        assert_eq!(change.status, 'R');
        assert_eq!(change.path, "nested/moved.txt");
        assert_eq!(change.original_path.as_deref(), Some("file.txt"));
        assert!(change.staged);
        drop(app);
        std::fs::remove_dir_all(repo_dir).unwrap();
    }
}

#[test]
fn staging_both_sides_of_a_move_then_unstaging_preserves_worktree_and_other_staged_files() {
    let contents = "this file moves without losing its contents\n";
    let (repo_dir, path, mut app) = make_git_app_with_file("stage-move", contents, contents);
    let moved = repo_dir.join("nested/moved.txt");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(repo_dir.join("keep.txt"), "unrelated staged addition\n").unwrap();
    run_git(&repo_dir, &["add", "keep.txt"]);
    app.source_control_files = collect_source_control_status(repo_dir.clone()).files;
    let deleted = app.source_control_files.iter().position(|f| f.path == "file.txt" && !f.staged).unwrap();
    app.git_stage_file(deleted);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running && app.source_control_files.iter().any(|f| f.path == "file.txt" && f.staged)
    });
    let added = app.source_control_files.iter().position(|f| f.path == "nested" && f.directory && !f.staged).unwrap();
    app.git_stage_file(added);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running && app.source_control_files.iter().any(|f| f.status == 'R')
    });
    let renamed = app.source_control_files.iter().position(|f| f.status == 'R').unwrap();
    app.git_unstage_file(renamed);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running && app.source_control_files.iter().filter(|f| !f.staged).count() == 2
    });
    assert_eq!(git_stdout(&repo_dir, &["diff", "--cached", "--name-only"]).trim(), "keep.txt");
    assert!(!path.exists());
    assert_eq!(std::fs::read_to_string(&moved).unwrap(), contents);
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn staged_move_diffs_use_the_old_head_path_and_new_index_path() {
    let head = (0..30).map(|i| format!("original line {i}\n")).collect::<String>();
    for edited in [false, true] {
        let index = if edited { head.replace("original line 15", "edited line 15") } else { head.clone() };
        let (repo_dir, path, mut app) = make_git_app_with_file("move-diff", &head, &head);
        let moved = repo_dir.join("nested/moved.txt");
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&moved, &index).unwrap();
        run_git(&repo_dir, &["add", "-A"]);
        let worktree = format!("{index}additional unstaged line\n");
        std::fs::write(&moved, &worktree).unwrap();
        app.source_control_files = collect_source_control_status(repo_dir.clone()).files;
        assert_eq!(app.source_control_files.len(), 2);
        assert!(app.source_control_files.iter().all(|f| f.path == "nested/moved.txt"));
        let staged = app.source_control_files.iter().position(|f| f.staged).unwrap();
        assert_eq!(app.source_control_files[staged].status, 'R');
        app.open_source_control_diff(staged);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| app.should_render_diff_for_active_editor());
        assert_eq!(app.compare_base_content.as_deref(), Some(head.as_str()));
        assert_eq!(app.editor().unwrap().rope.to_string(), index);
        assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff(&head, &index));
        let unstaged = app.source_control_files.iter().position(|f| !f.staged).unwrap();
        assert_eq!(app.source_control_files[unstaged].status, 'M');
        assert!(app.source_control_files[unstaged].original_path.is_none());
        app.open_source_control_diff(unstaged);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| app.should_render_diff_for_active_editor());
        assert_eq!(app.compare_base_content.as_deref(), Some(index.as_str()));
        assert_eq!(app.editor().unwrap().rope.to_string(), worktree);
        drop(app);
        std::fs::remove_dir_all(repo_dir).unwrap();
    }
}

#[test]
fn discarding_a_staged_move_restores_the_original_without_touching_other_files() {
    let head = "original contents\n";
    let (repo_dir, path, mut app) = make_git_app_with_file("discard-move", head, head);
    let moved = repo_dir.join("nested/moved.txt");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&path, &moved).unwrap();
    run_git(&repo_dir, &["add", "-A"]);
    let change = collect_source_control_status(repo_dir.clone()).files.remove(0);
    assert_eq!(change.status, 'R');
    std::fs::write(&moved, "later worktree changes\n").unwrap();
    let unrelated = repo_dir.join("nested/unrelated.txt");
    std::fs::write(&unrelated, "keep me\n").unwrap();
    app.git_discard_changes(vec![change]);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_git_action_running);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), head);
    assert!(!moved.exists());
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "keep me\n");
    assert!(git_stdout(&repo_dir, &["diff", "--cached", "--name-only"]).trim().is_empty());
    // Discard completion queues status, Explorer and clean-file reloads.
    // Finish those readers before removing their repository on Windows.
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn unrelated_additions_and_deletions_are_not_reported_as_moves() {
    let (repo_dir, path, app) = make_git_app_with_file("not-a-move", "original\n", "original\n");
    std::fs::remove_file(path).unwrap();
    std::fs::write(repo_dir.join("unrelated.txt"), "completely different file\n").unwrap();
    run_git(&repo_dir, &["add", "-A"]);
    let snapshot = collect_source_control_status(repo_dir.clone());
    assert_eq!(snapshot.files.len(), 2);
    assert!(snapshot.files.iter().any(|f| f.status == 'A'));
    assert!(snapshot.files.iter().any(|f| f.status == 'D'));
    assert!(snapshot.files.iter().all(|f| f.staged && f.original_path.is_none()));
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn source_control_status_distinguishes_a_directory_without_git() {
    let directory = temp_repo_dir("no-repository-status");
    std::fs::create_dir_all(&directory).expect("test directory");

    let status = collect_source_control_status(directory.clone());

    assert!(!status.repository_found);
    assert!(status.files.is_empty());
    assert!(status.workdir.is_none());

    let mut app = App::new(directory.clone());
    app.refresh_source_control_async();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.source_control_repository_state == SourceControlRepositoryState::Missing
    });
    assert!(app.source_control_files.is_empty());
    drop(app);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn initialize_repository_makes_the_directory_discoverable() {
    let directory = temp_repo_dir("initialize-repository");
    std::fs::create_dir_all(&directory).expect("test directory");

    let mut app = App::new(directory.clone());
    app.source_control_repository_state = SourceControlRepositoryState::Missing;
    app.git_initialize_repository();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.source_control_repository_state == SourceControlRepositoryState::Available
            && !app.bg_git_action_running
    });

    let status = collect_source_control_status(directory.clone());

    assert!(status.repository_found);
    assert!(status.files.is_empty());
    assert!(status.workdir.is_some());
    assert!(app.source_control_files.is_empty());
    drop(app);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn stage_all_and_unstage_all_cover_modified_deleted_and_new_files() {
    let repo_dir = temp_repo_dir("stage-and-unstage-all");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    init_git_fixture(&repo_dir);
    std::fs::write(repo_dir.join("modified.txt"), "before\n").expect("modified fixture");
    std::fs::write(repo_dir.join("deleted.txt"), "delete me\n").expect("deleted fixture");
    commit_fixture_files(&repo_dir, &["modified.txt", "deleted.txt"], "initial");
    std::fs::write(repo_dir.join("modified.txt"), "after\n").expect("modify fixture");
    std::fs::remove_file(repo_dir.join("deleted.txt")).expect("delete fixture");
    std::fs::write(repo_dir.join("new.txt"), "new\n").expect("new fixture");

    let mut app = App::new(repo_dir.clone());
    app.refresh_source_control_async();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.source_control_repository_state == SourceControlRepositoryState::Available
            && app.source_control_files.iter().any(|file| !file.staged)
    });

    app.git_stage_all();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running
            && !app.source_control_files.is_empty()
            && app.source_control_files.iter().all(|file| file.staged)
    });
    let staged = git_stdout(&repo_dir, &["diff", "--cached", "--name-status"]);
    assert!(staged.lines().any(|line| line.ends_with("modified.txt")));
    assert!(staged.lines().any(|line| line.ends_with("deleted.txt")));
    assert!(staged.lines().any(|line| line.ends_with("new.txt")));

    app.git_unstage_all();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running
            && !app.source_control_files.is_empty()
            && app.source_control_files.iter().all(|file| !file.staged)
    });
    assert!(
        git_stdout(&repo_dir, &["diff", "--cached", "--name-status"])
            .trim()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(repo_dir.join("modified.txt")).expect("modified worktree file"),
        "after\n"
    );
    assert!(!repo_dir.join("deleted.txt").exists());
    assert!(repo_dir.join("new.txt").exists());

    drop(app);
    std::fs::remove_dir_all(repo_dir).expect("remove temp repo");
}

#[test]
fn unstage_all_clears_the_index_before_the_first_commit() {
    let repo_dir = temp_repo_dir("unstage-all-unborn-head");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    init_git_fixture(&repo_dir);
    std::fs::write(repo_dir.join("new.txt"), "new\n").expect("new fixture");

    let mut app = App::new(repo_dir.clone());
    app.git_stage_all();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running
            && app.source_control_files.iter().any(|file| file.staged)
    });

    app.git_unstage_all();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running
            && app.source_control_files.iter().any(|file| !file.staged)
            && app.source_control_files.iter().all(|file| !file.staged)
    });
    assert!(
        git_stdout(&repo_dir, &["diff", "--cached", "--name-only"])
            .trim()
            .is_empty()
    );
    assert!(repo_dir.join("new.txt").exists());

    drop(app);
    std::fs::remove_dir_all(repo_dir).expect("remove temp repo");
}

#[test]
fn source_control_status_populates_distinct_collapsed_directory_summaries() {
    let repo_dir = temp_repo_dir("collapsed-directory-statuses");
    std::fs::create_dir_all(repo_dir.join("src")).expect("src directory");
    std::fs::create_dir_all(repo_dir.join("docs").join("generated"))
        .expect("nested docs directory");
    init_git_fixture(&repo_dir);

    let modified_path = repo_dir.join("src").join("modified.rs");
    std::fs::write(&modified_path, "before\n").expect("initial tracked file");
    commit_fixture_files(&repo_dir, &["src/modified.rs"], "initial");
    std::fs::write(&modified_path, "after with a different length\n").expect("modify tracked file");
    std::fs::write(
        repo_dir.join("docs").join("generated").join("new.txt"),
        "untracked\n",
    )
    .expect("nested untracked file");

    let status = collect_source_control_status(repo_dir.clone());
    assert!(status.repository_found);
    let snapshot = status
        .files
        .iter()
        .map(|change| format!("{}:{}:{}", change.path, change.status, change.staged))
        .collect::<Vec<_>>();
    assert!(
        status.files.iter().any(|change| {
            Path::new(&change.path) == Path::new("src/modified.rs")
                && change.status == 'M'
                && !change.staged
        }),
        "status snapshot: {snapshot:?}"
    );
    assert!(
        status.files.iter().any(|change| {
            Path::new(&change.path) == Path::new("docs")
                && change.directory
                && change.status == '?'
                && !change.staged
        }),
        "status snapshot: {snapshot:?}"
    );

    let mut explorer = FileExplorer::new(repo_dir.clone());
    explorer.apply_git_repository_changes(
        status
            .files
            .iter()
            .map(|change| (status.workdir.as_deref().unwrap(), change.path.as_str(), change.status, change.directory)),
    );

    let src = explorer
        .entries
        .iter()
        .find(|entry| entry.name == "src")
        .expect("collapsed src directory");
    assert!(!src.expanded);
    assert!(src.git_status_summary.contains(GitStatus::Modified));
    assert!(!src.git_status_summary.contains(GitStatus::Untracked));

    let docs = explorer
        .entries
        .iter()
        .find(|entry| entry.name == "docs")
        .expect("collapsed docs directory");
    assert!(!docs.expanded);
    assert!(docs.git_status_summary.contains(GitStatus::Untracked));
    assert!(!docs.git_status_summary.contains(GitStatus::Modified));

    let root_summary = explorer.root_git_status_summary();
    assert!(root_summary.contains(GitStatus::Modified));
    assert!(root_summary.contains(GitStatus::Untracked));

    let docs_index = explorer
        .entries
        .iter()
        .position(|entry| entry.name == "docs")
        .expect("docs index");
    explorer.selected = docs_index;
    assert_eq!(explorer.toggle_or_open(), None);
    let expanded_docs = explorer
        .entries
        .iter()
        .find(|entry| entry.name == "docs")
        .expect("expanded docs directory");
    assert!(expanded_docs.expanded);
    assert!(expanded_docs
        .git_status_summary
        .contains(GitStatus::Untracked));
    assert!(!expanded_docs
        .git_status_summary
        .contains(GitStatus::Modified));

    assert_eq!(explorer.toggle_or_open(), None);
    let collapsed_docs = explorer
        .entries
        .iter()
        .find(|entry| entry.name == "docs")
        .expect("collapsed docs directory after toggle");
    assert!(!collapsed_docs.expanded);
    assert!(collapsed_docs
        .git_status_summary
        .contains(GitStatus::Untracked));
    assert!(!collapsed_docs
        .git_status_summary
        .contains(GitStatus::Modified));

    drop(explorer);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn binary_files_do_not_enter_git_diff_views() {
    for (head, index, worktree) in [
        ("old\0binary", "new\0binary", "latest\0binary"),
        ("text\n", "text\n", "new\0binary"),
        ("text\n", "new\0binary", "text again\n"),
        ("old\0binary", "text\n", "more text\n"),
    ] {
        let (repo_dir, path, mut app) = make_git_app_with_file("binary-diff", head, index);
        run_git(&repo_dir, &["add", "file.txt"]);
        std::fs::write(&path, worktree).unwrap();
        for staged in [false, true] {
            if (staged && !head.contains('\0') && !index.contains('\0'))
                || (!staged && !index.contains('\0') && !worktree.contains('\0'))
            {
                continue;
            }
            app.open_source_control_diff_tab(&path, false, staged);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.pending_git_compare_path.is_none()
                    && !app.bg_git_compare_running
                    && app.editor().is_some_and(|editor| !editor.bg_loading)
            });
            assert!(!app.should_render_diff_for_active_editor());
            assert!(app.compare_diff_lines.is_empty());
            assert!(app.diff_mode == crate::app::DiffMode::Off);
        }

        // Deleted binary files have no working-tree bytes to inspect.
        if index.contains('\0') {
            std::fs::remove_file(&path).unwrap();
            app = App::new(repo_dir.clone());
            app.open_source_control_diff_tab(&path, true, false);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
            });
            assert!(!app.should_render_diff_for_active_editor());
        }
        if head.contains('\0') {
            run_git(&repo_dir, &["rm", "--cached", "-f", "file.txt"]);
            app.open_source_control_diff_tab(&path, true, true);
            app.request_compare_with_git_async();
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
            });
            assert!(!app.should_render_diff_for_active_editor());
        }
        drop(app);
        let _ = std::fs::remove_dir_all(repo_dir);
    }
}

#[test]
fn binary_files_do_not_enter_commit_diff_views() {
    for (initial, updated) in [("old\0binary", "text\n"), ("text\n", "new\0binary")] {
        let (repo_dir, _, mut app) = make_git_app_with_file("binary-commit-diff", initial, updated);
        run_git(&repo_dir, &["add", "file.txt"]);
        run_git(&repo_dir, &["commit", "-m", "update"]);
        app.commit_details.data.hash = "HEAD".into();
        app.commit_details.data.files = vec![crate::commit_details::CommitFile::new('M', "file.txt")];
        let original_tabs = app.tabs.len();
        app.open_source_control_commit_details_file_diff(0);
        assert_eq!(app.tabs.len(), original_tabs);
        assert!(!app.should_render_diff_for_active_editor());
        assert!(app.compare_diff_lines.is_empty());
        assert_eq!(app.status_message, crate::app_ui_state::BINARY_DIFF_MESSAGE);
        drop(app);
        let _ = std::fs::remove_dir_all(repo_dir);
    }
}

#[test]
fn open_staged_diff_refreshes_after_restaging_the_same_file() {
    let head = "first\noriginal\nlast\n";
    let initial = "first\nstaged\nlast\n";
    let updated = "first\nrestaged\nlast\n";
    let (repo_dir, path, mut app) = make_git_app_with_file("restage-open-diff", head, initial);
    run_git(&repo_dir, &["add", "file.txt"]);
    app.open_source_control_diff_tab(&path, false, true);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    app.editor_mut().unwrap().cursor_line = 2;
    app.diff_mode = crate::app::DiffMode::Inline;
    app.diff_left_horizontal_scroll = 3;

    std::fs::write(&path, updated).unwrap();
    app.source_control_files = collect_source_control_status(repo_dir.clone()).files;
    let unstaged = app.source_control_files.iter().position(|change| !change.staged).unwrap();
    app.git_stage_file(unstaged);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running && !app.bg_git_status_running
            && app.editor().unwrap().rope.to_string() == updated
    });
    assert_eq!(app.compare_base_content.as_deref(), Some(head));
    assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff(head, updated));
    assert_eq!(app.editor().unwrap().cursor_line, 2);
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    assert_eq!(app.diff_left_horizontal_scroll, 3);
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn source_control_diff_refresh_updates_inactive_splits_after_saving_and_staging() {
    use crate::app::{DiffMode, SplitDirection};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    let head = "committed\n";
    let index = "staged\n";
    let worktree = "working\n";
    let saved = "saved changes\n";
    let (repo_dir, path, mut app) = make_git_app_with_file("refresh-split-diffs", head, index);
    run_git(&repo_dir, &["add", "file.txt"]);
    std::fs::write(&path, worktree).unwrap();
    app.open_source_control_diff_tab(&path, false, true);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor() && !app.diff_tabs.refresh_running
    });
    let staged_cache = app.diff_render_cache.clone();
    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor() && !app.diff_tabs.refresh_running
    });
    app.create_split_group_dir(app.active_tab, SplitDirection::Horizontal);
    let diff_group = app.active_group;
    app.switch_group(0);
    app.open_file(&path);
    // Observe the external worktree write before editing and saving it.
    app.editor_mut().unwrap().reload_from_disk().unwrap();
    app.editor_mut().unwrap().rope = ropey::Rope::from_str(saved);
    app.editor_mut().unwrap().content_version += 1;
    app.editor_mut().unwrap().modified = true;
    app.format_on_save = false;
    app.save_current_file();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.pending_file_saves.contains_key(&path)
            && !app.diff_tabs.refresh_running && !app.bg_git_status_running
    });
    assert_eq!(app.tabs[1].editor.rope.to_string(), index, "saving must not overwrite the index view");
    assert_eq!(app.groups[diff_group].tabs[0].editor.rope.to_string(), saved);

    app.show_minimap = false;
    let mut terminal = Terminal::new(TestBackend::new(140, 25)).unwrap();
    terminal.draw(|frame| {
        crate::ui_helpers::draw_split_editor_for_group(frame, &mut app, Rect::new(0, 0, 140, 25), diff_group);
    }).unwrap();
    let displayed: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(displayed.contains("saved changes"), "the inactive split must redraw its refreshed diff");
    assert!(!app.should_render_diff_for_active_editor());

    app.open_source_control_diff_tab(&path, false, true);
    assert!(Arc::ptr_eq(&staged_cache, &app.diff_render_cache));
    let unstaged = app.source_control_files.iter().position(|change| !change.staged).unwrap();
    app.git_stage_file(unstaged);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_action_running && !app.bg_git_status_running && !app.diff_tabs.refresh_running
    });
    assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff(head, saved));
    app.switch_group(diff_group);
    assert!(app.diff_mode == DiffMode::SideBySide);
    assert_eq!(app.compare_base_content.as_deref(), Some(saved));
    assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff(saved, saved));
    assert!(app.diff_render_cache.hunks.is_empty());

    // Removing all staged changes must also refresh a diff whose status row vanished.
    run_git(&repo_dir, &["commit", "-m", "updated"]);
    app.refresh_source_control_async();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_status_running && !app.diff_tabs.refresh_running
    });
    app.switch_group(0);
    assert_eq!(app.compare_base_content.as_deref(), Some(saved));
    assert!(app.diff_render_cache.hunks.is_empty());
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn source_control_diff_refresh_coalesces_index_changes_and_preserves_unchanged_caches() {
    let head = "head\n";
    let index = "index\n";
    let latest = "latest staged\n";
    let worktree = "working\n";
    let (repo_dir, path, mut app) = make_git_app_with_file("refresh-diff-race", head, index);
    run_git(&repo_dir, &["add", "file.txt"]);
    std::fs::write(&path, worktree).unwrap();
    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor() && !app.diff_tabs.refresh_running
    });
    let cache = app.diff_render_cache.clone();
    app.refresh_source_control_async();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_status_running && !app.diff_tabs.refresh_running
    });
    assert!(Arc::ptr_eq(&cache, &app.diff_render_cache));
    let status_rows = |files: Vec<GitFileChange>| files.into_iter()
        .map(|change| (change.path, change.status, change.staged, change.original_path))
        .collect::<Vec<_>>();
    let old_status = status_rows(collect_source_control_status(repo_dir.clone()).files);

    // Queue another request before processing the first worker result.
    app.refresh_source_control_async();
    std::fs::write(&path, latest).unwrap();
    run_git(&repo_dir, &["add", "file.txt"]);
    std::fs::write(&path, worktree).unwrap();
    assert_eq!(old_status, status_rows(collect_source_control_status(repo_dir.clone()).files));
    app.refresh_source_control_async();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_status_running && !app.diff_tabs.refresh_running
    });
    assert_eq!(app.compare_base_content.as_deref(), Some(latest));
    assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff(latest, worktree));

    run_git(&repo_dir, &["reset", "HEAD", "--", "file.txt"]);
    app.refresh_source_control_async();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_status_running && !app.diff_tabs.refresh_running
    });
    assert_eq!(app.compare_base_content.as_deref(), Some(head));
    assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff(head, worktree));

    app.refresh_source_control_async();
    app.force_close_tab(app.active_tab);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_git_status_running && !app.diff_tabs.refresh_running
    });
    assert_eq!(app.tabs.len(), 1);
    assert!(!app.should_render_diff_for_active_editor());
    assert_eq!(app.editor().unwrap().rope.to_string(), index);
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn source_control_diff_refresh_handles_worktree_edits_deletion_and_recreation() {
    let (repo_dir, path, mut app) = make_git_app_with_file("refresh-worktree-diff", "head\n", "first\n");
    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor() && !app.diff_tabs.refresh_running
    });
    for contents in [Some("changed\n"), None, Some("recreated\n")] {
        match contents {
            Some(text) => std::fs::write(&path, text).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        app.background.sender.send(crate::background::BackgroundResult::FileDiskState(
            crate::background::inspect_workspace_path(path.clone()),
        )).unwrap();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.diff_tabs.refresh_running
                && app.editor().unwrap().rope.to_string() == contents.unwrap_or("")
        });
        assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff("head\n", contents.unwrap_or("")));
        assert_eq!(app.editor().unwrap().disk_unavailable, contents.is_none());
        assert!(!app.editor().unwrap().modified);
    }
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn source_control_staged_and_unstaged_diffs_use_separate_revisions_and_tabs() {
    let head = "first\noriginal\nlast\n";
    let index = "first\nstaged\nlast\n";
    let worktree = "first\nunstaged\nlast\nextra\n";
    let (repo_dir, path, mut app) = make_git_app_with_file("mixed-diff", head, index);
    run_git(&repo_dir, &["add", "file.txt"]);
    std::fs::write(&path, worktree).unwrap();
    app.source_control_files = vec![
        GitFileChange { directory: false,
            original_path: None,
            path: "file.txt".into(),
            status: 'M',
            staged: true,
        },
        GitFileChange { directory: false,
            original_path: None,
            path: "file.txt".into(),
            status: 'M',
            staged: false,
        },
    ];

    // Switch while the working-tree file is still loading: its worker must
    // never install working-tree content or a diff in the staged tab.
    app.open_source_control_diff(1);
    let unstaged_tab = app.active_tab;
    let staged_change = app.source_control_files[0].clone();
    app.open_source_control_context_menu(0, 0, staged_change);
    let view_diff = app
        .source_control_context_menu_items()
        .iter()
        .position(|&label| label == "Open Diff")
        .unwrap();
    app.execute_source_control_context(view_diff);
    let staged_tab = app.active_tab;
    assert_ne!(staged_tab, unstaged_tab);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor() && !app.tabs[unstaged_tab].editor.bg_loading
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), index);
    assert_eq!(app.compare_base_content.as_deref(), Some(head));
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff(head, index)
    );
    assert_eq!(
        app.editor().unwrap().git_line_status().len(),
        ropey::Rope::from_str(index).len_lines()
    );
    assert!(
        crate::ui_chrome_tabs::tab_display_name(&app, staged_tab, &app.tabs[staged_tab])
            .contains("Staged Diff")
    );
    let staged_labels = crate::app::DiffLabels::new("HEAD (last commit)", "Index (staged)");
    assert_eq!(app.diff_labels.as_ref(), Some(&staged_labels));
    let staged_cache = app.diff_render_cache.clone();

    app.open_source_control_diff_tab(&path, false, false);
    assert_eq!(app.active_tab, unstaged_tab);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert_eq!(app.compare_base_content.as_deref(), Some(index));
    assert_eq!(app.editor().unwrap().rope.to_string(), worktree);
    assert_eq!(
        app.diff_labels,
        Some(crate::app::DiffLabels::new("Index (staged)", "Working Tree"))
    );
    assert!(
        crate::ui_chrome_tabs::tab_display_name(&app, unstaged_tab, &app.tabs[unstaged_tab])
            .contains("Unstaged Diff")
    );
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff(index, worktree)
    );

    app.open_source_control_diff_tab(&path, false, true);
    assert_eq!(app.active_tab, staged_tab);
    assert!(Arc::ptr_eq(&staged_cache, &app.diff_render_cache));
    assert_eq!(app.diff_labels.as_ref(), Some(&staged_labels));
    // Automatic disk reloads may update the working file, but the staged
    // editor must continue displaying the index snapshot.
    let reloaded = "later worktree edits\n";
    std::fs::write(&path, reloaded).unwrap();
    app.queue_file_reload(path.clone());
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.tabs[0].editor.rope.to_string() == reloaded
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), index);
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff(head, index)
    );
    app.open_source_control_diff_tab(&path, false, false);
    assert_eq!(app.tabs.len(), 3);
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_staged_deletion_ignores_recreated_worktree_file() {
    let head = "committed\n";
    let (repo_dir, path, mut app) = make_git_app_with_file("staged-delete-diff", head, head);
    run_git(&repo_dir, &["rm", "file.txt"]);
    std::fs::write(&path, "recreated\n").unwrap();
    app.open_source_control_diff_tab(&path, true, true);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert_eq!(app.compare_base_content.as_deref(), Some(head));
    assert_eq!(app.editor().unwrap().rope.len_bytes(), 0);
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff(head, "")
    );
    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert_eq!(app.compare_base_content.as_deref(), Some(""));
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff("", "recreated\n")
    );
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_staged_addition_before_first_commit_uses_index_when_file_is_missing() {
    let repo_dir = temp_repo_dir("staged-unborn-diff");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_git_fixture(&repo_dir);
    let path = repo_dir.join("new.txt");
    let index = "staged new file\nsecond line\n";
    std::fs::write(&path, index).unwrap();
    run_git(&repo_dir, &["add", "new.txt"]);
    std::fs::remove_file(&path).unwrap();
    let mut app = App::new(repo_dir.clone());
    app.open_source_control_diff_tab(&path, false, true);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    assert_eq!(app.compare_base_content.as_deref(), Some(""));
    assert_eq!(app.editor().unwrap().rope.to_string(), index);
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff("", index)
    );
    app.open_source_control_diff_tab(&path, true, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert!(app.diff_mode == crate::app::DiffMode::SideBySide);
    assert_eq!(app.compare_base_content.as_deref(), Some(index));
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff(index, "")
    );
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_diff_and_file_tabs_preserve_independent_views() {
    use crate::app::{ActivePane, DiffMode, TabKind};
    let (repo_dir, file_path, mut app) = make_git_app_with_file(
        "separate-diff-tabs", "first\nold\nlast\n", "first\nnew\nlast\n",
    );
    app.source_control_files = vec![GitFileChange { directory: false,
        original_path: None,
        path: "file.txt".into(), status: 'M', staged: false,
    }];
    app.tabs[0].editor.cursor_line = 2;
    app.tabs[0].editor.cursor_col = 3;
    app.tabs[0].editor.selection_anchor = Some((1, 0));
    app.tabs[0].editor.scroll_offset = 1;
    app.tabs[0].editor.insert_char('!');
    let original_rope = app.tabs[0].editor.rope.clone();
    let cursor = (app.tabs[0].editor.cursor_line, app.tabs[0].editor.cursor_col);
    let selection = app.tabs[0].editor.selection_anchor;
    let scroll = app.tabs[0].editor.scroll_offset;

    app.open_source_control_diff(0);
    assert_eq!(app.tabs.len(), 2);
    assert!(matches!(app.tabs[1].kind, TabKind::Diff(_)));
    app.wait_for_background(std::time::Duration::from_secs(5), |app| app.diff_mode == DiffMode::SideBySide);
    assert!(app.should_render_diff_for_active_editor());
    let cache = app.diff_render_cache.clone();
    app.editor_mut().unwrap().scroll_offset = 1;
    app.diff_left_horizontal_scroll = 3;
    app.diff_mode = DiffMode::Inline;

    let change = app.source_control_files.iter().find(|f| f.path == "file.txt").unwrap().clone();
    app.open_source_control_context_menu(0, 0, change);
    let open_file = app.source_control_context_menu_items().iter().position(|&label| label == "Open File").unwrap();
    app.execute_source_control_context(open_file);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 0);
    assert!(app.active_pane == ActivePane::Editor);
    assert!(!app.should_render_diff_for_active_editor());
    assert_eq!(app.tabs[0].editor.rope, original_rope);
    assert_eq!((app.tabs[0].editor.cursor_line, app.tabs[0].editor.cursor_col), cursor);
    assert_eq!(app.tabs[0].editor.selection_anchor, selection);
    assert_eq!(app.tabs[0].editor.scroll_offset, scroll);
    assert!(app.tabs[0].editor.modified);
    assert_eq!(app.unsaved_file_count(), 1);

    app.active_tab = 1;
    app.validate_diff_state_for_active_tab();
    assert!(app.diff_mode == DiffMode::Inline);
    assert!(Arc::ptr_eq(&cache, &app.diff_render_cache));
    assert_eq!(app.diff_left_horizontal_scroll, 3);
    assert_eq!(app.editor().unwrap().scroll_offset, 1);
    assert!(!app.bg_git_compare_running);
    app.open_source_control_diff(0);
    assert_eq!(app.tabs.len(), 2);
    assert!(Arc::ptr_eq(&cache, &app.diff_render_cache));
    app.force_close_tab(1);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&file_path));
    assert_eq!(app.tabs[0].editor.rope, original_rope);
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_diff_tabs_survive_multiple_files_and_split_rendering() {
    use crate::app::{DiffMode, SplitDirection};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    let (repo_dir, path, mut app) = make_git_app_with_file("split-diff-tabs", "old\n", "new\n");
    std::fs::write(repo_dir.join("second.txt"), "second\n").unwrap();
    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| app.diff_mode == DiffMode::SideBySide);
    let first_cache = app.diff_render_cache.clone();
    app.execute_palette_command("toggle_diff_layout");
    assert!(app.diff_mode == DiffMode::Inline);
    app.open_source_control_diff_tab(&repo_dir.join("second.txt"), false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| app.diff_mode == DiffMode::Inline);
    app.execute_palette_command("toggle_diff_layout");
    assert!(app.diff_mode == DiffMode::SideBySide);
    let second_cache = app.diff_render_cache.clone();
    assert!(!Arc::ptr_eq(&first_cache, &second_cache));
    app.active_tab = 1;
    app.validate_diff_state_for_active_tab();
    assert!(Arc::ptr_eq(&first_cache, &app.diff_render_cache));
    assert!(app.diff_mode == DiffMode::Inline);
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    app.sync_diff_tab();
    let first_group = app.active_group;
    app.switch_group(0);
    app.open_file(&path);
    assert!(!app.should_render_diff_for_active_editor());
    app.show_minimap = false;
    let mut terminal = Terminal::new(TestBackend::new(140, 25)).unwrap();
    terminal.draw(|frame| {
        crate::ui_helpers::draw_split_editor_for_group(frame, &mut app, Rect::new(0, 0, 140, 25), first_group);
    }).unwrap();
    let displayed: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(displayed.contains("file.txt (Unstaged Diff)"));
    assert!(displayed.contains("old") && displayed.contains("new"));
    assert!(!app.should_render_diff_for_active_editor());
    app.switch_group(first_group);
    assert!(Arc::ptr_eq(&first_cache, &app.diff_render_cache));
    assert!(app.diff_mode == DiffMode::Inline);
    assert!(app.should_render_diff_for_active_editor());
    app.force_close_tab(0);
    app.sync_diff_tab();
    app.active_tab = app.tabs.iter().position(|tab| tab.editor.file_path.as_deref() == Some(repo_dir.join("second.txt").as_path())).unwrap();
    app.validate_diff_state_for_active_tab();
    assert!(Arc::ptr_eq(&second_cache, &app.diff_render_cache));
    assert!(app.diff_mode == DiffMode::SideBySide);
    let third_path = repo_dir.join("third.txt");
    std::fs::write(&third_path, "third\n").unwrap();
    app.open_source_control_diff_tab(&third_path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert!(app.diff_mode == DiffMode::SideBySide);
    assert!(app.preferred_diff_mode == DiffMode::SideBySide);
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_pending_diff_cannot_replace_normal_file_view() {
    use crate::app::{DiffMode, TabKind};
    let (repo_dir, path, mut app) = make_git_app_with_file("pending-diff-tab", "old\n", "new\n");
    app.source_control_files = vec![GitFileChange { directory: false, original_path: None, path: "file.txt".into(), status: 'M', staged: false }];
    app.open_source_control_diff(0);
    app.open_file(&path);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.tabs[1].editor.bg_loading);
    assert_eq!(app.active_tab, 0);
    assert!(app.tabs[0].kind == TabKind::File);
    assert!(!app.should_render_diff_for_active_editor());
    assert!(app.diff_mode == DiffMode::Off);
    app.active_tab = 1;
    app.validate_diff_state_for_active_tab();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| app.should_render_diff_for_active_editor());
    assert!(app.should_render_diff_for_active_editor());
    assert!(app.editor().unwrap().read_only);
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_large_diff_waits_for_file_then_finishes_in_background() {
    let mut initial = String::new();
    let mut updated = String::new();
    for line in 0..45_000 {
        let original = format!("line {line:05} unchanged content\n");
        initial.push_str(&original);
        if line == 22_500 {
            updated.push_str("line 22500 changed content now\n");
        } else {
            updated.push_str(&original);
        }
    }
    assert!(updated.len() as u64 > crate::editor::BG_LOAD_THRESHOLD);

    let (repo_dir, file_path, mut app) =
        make_git_app_with_file("async-large-diff", &initial, &updated);
    app.tabs.clear();
    app.active_tab = 0;
    app.source_control_files = vec![GitFileChange { directory: false,
        original_path: None,
        path: "file.txt".to_string(),
        status: 'M',
        staged: false,
    }];

    app.open_source_control_diff(0);

    assert!(app.tabs[0].editor.bg_loading);
    assert_eq!(app.pending_git_compare_path.as_ref(), Some(&file_path));
    assert!(matches!(app.diff_mode, crate::app::DiffMode::Off));
    assert!(app.compare_diff_lines.is_empty());
    assert!(!app.bg_git_compare_running);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while (app.tabs[0].editor.bg_loading
        || app.pending_git_compare_path.is_some()
        || app.bg_git_compare_running
        || !matches!(app.diff_mode, crate::app::DiffMode::SideBySide))
        && std::time::Instant::now() < deadline
    {
        app.poll_background_results();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    assert!(!app.tabs[0].editor.bg_loading);
    assert!(!app.tabs[0]
        .editor
        .rope
        .to_string()
        .starts_with("Loading file"));
    assert!(app.pending_git_compare_path.is_none());
    assert!(!app.bg_git_compare_running);
    assert!(
        matches!(app.diff_mode, crate::app::DiffMode::SideBySide),
        "status: {}; notifications: {:?}",
        app.status_message,
        app.notification_history
    );
    assert_eq!(app.compare_base_content.as_deref(), Some(initial.as_str()));
    assert!(app
        .compare_diff_lines
        .iter()
        .any(|(kind, text)| *kind == '+' && text == "line 22500 changed content now"));
    assert_eq!(app.status_message, "Opened diff: file.txt");

    drop(app);
    std::fs::remove_dir_all(repo_dir).expect("remove temp repo");
}

#[test]
fn source_control_untracked_file_diff_uses_a_single_pane() {
    use ratatui::{Terminal, backend::TestBackend};
    let repo_dir = temp_repo_dir("untracked-file-diff");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    init_git_fixture(&repo_dir);
    let file_path = repo_dir.join("new-file.txt");
    let contents = "first new line\nsecond new line\n";
    std::fs::write(&file_path, contents).expect("untracked file");

    let mut app = App::new(repo_dir.clone());
    app.source_control_files = vec![GitFileChange { directory: false,
        original_path: None,
        path: "new-file.txt".to_string(),
        status: '?',
        staged: false,
    }];

    app.open_source_control_diff(0);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });

    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    assert!(app.preferred_diff_mode == crate::app::DiffMode::SideBySide);
    assert_eq!(app.editor().expect("open editor").rope.to_string(), contents);
    assert_eq!(app.compare_base_content.as_deref(), Some(""));
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff("", contents)
    );
    assert!(!app
        .notification_history
        .iter()
        .any(|(message, _, _)| message.contains("No git version found")));

    app.show_minimap = false;
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|frame| {
        crate::ui_chrome::draw_editor_section(frame, &mut app, frame.area());
    }).unwrap();
    assert!(app.diff_split_area.is_none());
    assert!(app.diff_left_code_area.is_none());
    assert!(app.code_area.unwrap().width > 100);
    let displayed: String = terminal.backend().buffer().content.iter()
        .map(|cell| cell.symbol()).collect();
    assert!(displayed.contains("first new line"));
    assert!(displayed.contains("second new line"));

    drop(app);
    std::fs::remove_dir_all(repo_dir).expect("remove temp repo");
}

#[test]
fn source_control_staged_addition_uses_inline_but_unstaged_edits_use_preferred_layout() {
    use crate::app::DiffMode;
    let (repo_dir, existing_path, mut app) =
        make_git_app_with_file("new-file-layout", "existing\n", "modified\n");
    let path = repo_dir.join("new.txt");
    std::fs::write(&path, "staged new file\n").unwrap();
    run_git(&repo_dir, &["add", "new.txt"]);
    std::fs::write(&path, "later unstaged edit\n").unwrap();

    app.open_source_control_diff_tab(&path, false, true);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert!(app.diff_mode == DiffMode::Inline);
    assert!(app.preferred_diff_mode == DiffMode::SideBySide);
    assert_eq!(app.compare_base_content.as_deref(), Some(""));
    assert_eq!(app.editor().unwrap().rope.to_string(), "staged new file\n");

    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert!(app.diff_mode == DiffMode::SideBySide);
    assert_eq!(app.compare_base_content.as_deref(), Some("staged new file\n"));
    assert_eq!(app.editor().unwrap().rope.to_string(), "later unstaged edit\n");

    app.open_source_control_diff_tab(&existing_path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert!(app.diff_mode == DiffMode::SideBySide);

    app.open_source_control_diff_tab(&path, false, true);
    assert!(app.diff_mode == DiffMode::Inline);
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_intent_to_add_file_uses_a_single_pane() {
    let (repo_dir, _, mut app) =
        make_git_app_with_file("intent-to-add-layout", "existing\n", "existing\n");
    let path = repo_dir.join("new.txt");
    std::fs::write(&path, "new contents\n").unwrap();
    run_git(&repo_dir, &["add", "-N", "new.txt"]);
    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    assert_eq!(app.compare_base_content.as_deref(), Some(""));
    assert_eq!(app.editor().unwrap().rope.to_string(), "new contents\n");
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn source_control_changes_to_tracked_empty_files_keep_the_preferred_layout() {
    for staged in [false, true] {
        let (repo_dir, path, mut app) =
            make_git_app_with_file("tracked-empty-layout", "", "added contents\n");
        if staged {
            run_git(&repo_dir, &["add", "file.txt"]);
        }
        app.open_source_control_diff_tab(&path, false, staged);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            app.should_render_diff_for_active_editor()
        });
        assert!(app.diff_mode == crate::app::DiffMode::SideBySide);
        assert_eq!(app.compare_base_content.as_deref(), Some(""));
        assert_eq!(app.editor().unwrap().rope.to_string(), "added contents\n");
        drop(app);
        let _ = std::fs::remove_dir_all(repo_dir);
    }
}

#[test]
fn source_control_deleted_file_diff_uses_an_empty_read_only_target() {
    let repo_dir = temp_repo_dir("deleted-file-diff");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    init_git_fixture(&repo_dir);
    let file_path = repo_dir.join("deleted-file.txt");
    let original = "first deleted line\nsecond deleted line\n";
    std::fs::write(&file_path, original).expect("tracked file");
    commit_fixture_files(&repo_dir, &["deleted-file.txt"], "add deleted fixture");
    std::fs::remove_file(&file_path).expect("delete tracked file");

    let mut app = App::new(repo_dir.clone());
    app.source_control_files = vec![GitFileChange { directory: false,
        original_path: None,
        path: "deleted-file.txt".to_string(),
        status: 'D',
        staged: false,
    }];

    app.open_source_control_diff(0);
    let editor = app.editor().expect("deleted diff target");
    assert!(!editor.bg_loading);
    assert!(editor.read_only);
    assert!(editor.disk_unavailable);
    assert_eq!(editor.rope.len_bytes(), 0);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        matches!(app.diff_mode, crate::app::DiffMode::SideBySide)
    });

    assert_eq!(app.compare_base_content.as_deref(), Some(original));
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff(original, "")
    );
    assert!(app
        .compare_diff_lines
        .iter()
        .all(|(kind, _)| *kind == '-'));
    assert!(!app
        .notification_history
        .iter()
        .any(|(message, _, _)| message.contains("Error loading file")));

    drop(app);
    std::fs::remove_dir_all(repo_dir).expect("remove temp repo");
}

#[test]
fn repository_relative_path_accepts_noncanonical_file_path() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::current_dir()
        .expect("current directory")
        .join("target")
        .join("test-paths")
        .join(format!("vtcode-git-relative-{unique}"));
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).expect("nested directory");
    let file = root.join("file.txt");
    std::fs::write(&file, "contents\n").expect("test file");
    let canonical_root = std::fs::canonicalize(&root).expect("canonical root");
    let noncanonical_file = nested.join("..").join("file.txt");

    let relative = repository_relative_path(&canonical_root, &noncanonical_file);

    assert_eq!(relative.as_deref(), Some(Path::new("file.txt")));
    std::fs::remove_file(&file).expect("delete test file");
    assert_eq!(
        repository_relative_path(&canonical_root, &noncanonical_file).as_deref(),
        Some(Path::new("file.txt")),
        "deleted files retain their normalized Git index path",
    );
    let outside = root.with_file_name(format!("outside-{unique}.txt"));
    std::fs::write(&outside, "outside the worktree\n").unwrap();
    assert_eq!(
        repository_relative_path(&canonical_root, &root.join("..").join(outside.file_name().unwrap())),
        None,
        "a parent traversal must not produce an index path outside the worktree",
    );
    std::fs::remove_file(outside).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn target_markers_include_deleted_lines() {
    let compare = vec![
        ('-', "gone".to_string()),
        (' ', "keep".to_string()),
        ('-', "tail-gone".to_string()),
    ];
    let markers = app_git_display::target_line_markers_from_compare_diff(&compare, 1);
    assert_eq!(markers, vec!['D']);
}

#[test]
fn target_markers_preserve_added_marker_when_delete_maps_same_line() {
    let compare = vec![('+', "new".to_string()), ('-', "old".to_string())];
    let markers = app_git_display::target_line_markers_from_compare_diff(&compare, 1);
    assert_eq!(markers, vec!['U']);
}

#[test]
fn map_worktree_line_to_index_line_accounts_for_insertions() {
    let index_text = "alpha\nbeta\ngamma\n";
    let worktree_text = "intro\nalpha\nbeta\ngamma\n";

    assert_eq!(
        app_git_display::map_worktree_line_to_index_line(index_text, worktree_text, 2),
        Some(1)
    );
}

#[test]
fn git_branch_picker_scroll_keeps_selected_row_visible() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut app = App::test_new();
    app.git_branch_picker_items = (0..15)
        .map(|idx| GitBranchPickerItem::Local {
            name: format!("branch-{idx}"),
        })
        .collect();
    app.git_branch_picker_selected = 14;

    for (width, height) in [(120, 36), (80, 24), (42, 20)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui_pickers::draw_git_branch_picker(frame, &mut app)).unwrap();
        app.sync_git_branch_picker_scroll();
        assert!(app.git_branch_picker_selected >= app.git_branch_picker_scroll_offset);
        assert!(app.git_branch_picker_selected < app.git_branch_picker_scroll_offset + app.git_branch_picker_visible_rows());
        assert!(app.quick_picker.rows.iter().any(|(_, index)| *index == 14));
    }
}

#[test]
fn git_branch_picker_keeps_creation_separate_from_search_results() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut app = App::test_new();
    app.git_branch_picker_query = "feature/new-branch".to_string();

    assert_eq!(app.git_branch_picker_total_rows(), 0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| crate::ui_pickers::draw_git_branch_picker(frame, &mut app)).unwrap();
    assert!(app.quick_picker.rows.is_empty());
    let rendered: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(rendered.contains("New branch"));
}

#[test]
fn save_current_file_falls_back_when_no_formatter_is_available() {
    let path = temp_file_path("save-fallback");
    let mut app = App::test_new();
    app.tabs = vec![make_modified_file_tab(&path, "hello")];
    app.active_tab = 0;
    app.formatters.insert(crate::formatters::language(app.editor().unwrap()), Default::default());
    app.format_on_save = true;

    app.save_current_file();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.pending_file_saves.contains_key(&path)
    });

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    assert!(!app.tabs[0].editor.modified);
    let _ = std::fs::remove_file(path); // Error OK: file may not exist
}

#[test]
fn handle_lsp_format_result_applies_edits_then_saves() {
    let path = temp_file_path("format-save");
    let mut app = App::test_new();
    app.tabs = vec![make_modified_file_tab(&path, "bad\n")];
    app.active_tab = 0;
    app.lsp_format_pending = true;
    app.lsp_format_target_path = Some(path.clone());
    app.lsp_save_after_format_path = Some(path.clone());
    app.lsp_manager.format_dirty = true;
    app.lsp_manager.format_edits = vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(0, 3),
        },
        new_text: "good".to_string(),
    }];

    assert!(app.handle_lsp_format_result());
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.pending_file_saves.contains_key(&path)
    });

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "good\n");
    assert!(!app.tabs[0].editor.modified);
    assert!(!app.lsp_format_pending);
    assert!(app.lsp_format_target_path.is_none());
    assert!(app.lsp_save_after_format_path.is_none());
    assert!(!app.lsp_manager.format_dirty);
    let _ = std::fs::remove_file(path); // Error OK: file may not exist
}

#[test]
fn git_stage_hunk_stages_only_selected_hunk() {
    let (repo_dir, file_path, mut app) = make_git_app_with_file(
        "stage-hunk",
        "one\ntwo\nthree\nfour\n",
        "one\ntwo changed\nthree\nfour changed\n",
    );

    app.git_stage_hunk(&file_path, 1);

    let staged = git_stdout(
        &repo_dir,
        &["diff", "--cached", "--unified=0", "--", "file.txt"],
    );
    let unstaged = git_stdout(&repo_dir, &["diff", "--unified=0", "--", "file.txt"]);
    assert!(
        staged.contains("+two changed"),
        "notifications: {:?}",
        app.notification_history
    );
    assert!(!staged.contains("four changed"));
    assert!(unstaged.contains("+four changed"));
    assert!(!unstaged.contains("two changed"));

    let _ = std::fs::remove_dir_all(repo_dir); // Error OK: dir may not exist
}

#[test]
fn git_line_status_clears_when_an_untracked_file_becomes_ignored() {
    let (repo_dir, _, mut app) = make_git_app_with_file("ignored-lines", "base\n", "base\n");
    let file = repo_dir.join("generated.txt");
    let contents = "generated\ncontents\n";
    std::fs::write(&file, contents).unwrap();
    app.tabs = vec![make_clean_file_tab(&file, contents)];
    app.hunk_popup_file = Some(file.clone());
    app.hunk_popup_line = 0;
    app.hunk_popup_status = 'U';

    for ignored in [false, true, false] {
        std::fs::write(repo_dir.join(".gitignore"), if ignored { "generated.txt\n" } else { "" }).unwrap();
        app.refresh_git_line_status_async();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_git_diff_running && !app.bg_git_diff_pending
        });
        let editor = app.editor().unwrap();
        if ignored {
            assert!(editor.git_line_status().iter().all(|&status| status == ' '));
            assert!(editor.git_change_lines().is_empty());
            assert_eq!(app.load_hunk_peek().err().as_deref(), Some("No changes found"));
        } else {
            assert_eq!(&editor.git_line_status()[..2], &['U', 'U']);
            assert!(app.load_hunk_peek().is_ok());
        }
    }
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn ignored_files_in_new_repositories_have_no_git_markers_or_hunk_preview() {
    for ignore_source in [".gitignore", "generated/.gitignore", ".git/info/exclude"] {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        std::fs::create_dir_all(temp.path().join("generated/nested")).unwrap();
        let file = temp.path().join("generated/nested/output.txt");
        let contents = "generated\ncontents\n";
        std::fs::write(&file, contents).unwrap();
        let pattern = if ignore_source == "generated/.gitignore" { "nested/\n" } else { "generated/\n" };
        std::fs::write(temp.path().join(ignore_source), pattern).unwrap();
        assert!(repo.status_file(Path::new("generated/nested/output.txt")).unwrap().is_ignored());

        let mut app = App::new(temp.path().to_path_buf());
        app.tabs = vec![make_clean_file_tab(&file, contents)];
        app.active_tab = 0;
        app.refresh_git_line_status_async();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_git_diff_running && !app.bg_git_diff_pending
        });
        assert!(app.editor().unwrap().git_change_lines().is_empty(), "{ignore_source}");
        app.hunk_popup_file = Some(file);
        app.hunk_popup_line = 0;
        app.hunk_popup_status = 'U';
        assert_eq!(app.load_hunk_peek().err().as_deref(), Some("No changes found"));
    }
}

#[test]
fn tracked_files_matching_ignore_rules_keep_git_markers_and_hunk_preview() {
    let (repo_dir, file, mut app) = make_git_app_with_file("tracked-ignore-rule", "before\n", "after\n");
    std::fs::write(repo_dir.join(".gitignore"), "*.txt\n").unwrap();
    let added = repo_dir.join("added.txt");
    std::fs::write(&added, "staged addition\n").unwrap();
    run_git(&repo_dir, &["add", "--force", "added.txt"]);

    for (path, contents, marker) in [(&file, "after\n", 'U'), (&added, "staged addition\n", 'S')] {
        app.tabs = vec![make_clean_file_tab(path, contents)];
        app.refresh_git_line_status_async();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_git_diff_running && !app.bg_git_diff_pending
        });
        assert_eq!(app.editor().unwrap().git_line_status()[0], marker);
        app.hunk_popup_file = Some(path.clone());
        app.hunk_popup_line = 0;
        app.hunk_popup_status = marker;
        assert!(app.load_hunk_peek().is_ok());
    }
    drop(app);
    std::fs::remove_dir_all(repo_dir).unwrap();
}

#[test]
fn hunk_peek_previews_and_stages_the_selected_change() {
    let (repo_dir, file_path, mut app) = make_git_app_with_file(
        "peek-stage", "a\nb\nc\nd\ne\nf\ng\nh\ni\n", "a\nB\nc\nd\ne\nf\ng\nH\ni\n",
    );
    app.open_hunk_popup(0, 2, file_path.clone(), 7, 'M');
    app.wait_for_hunk_preview();
    let peek = app.hunk_peek.as_ref().expect("loaded Git preview");
    assert_eq!(peek.selected, 1);
    assert_eq!(peek.hunks.len(), 2);
    assert!(peek.rows[peek.hunks[1].start_row..peek.hunks[1].end_row].iter().any(|r| r.kind == '-' && r.text == "h"));
    assert!(peek.rows[peek.hunks[1].start_row..peek.hunks[1].end_row].iter().any(|r| r.kind == '+' && r.text == "H"));
    app.execute_hunk_popup_action(0);
    let staged = git_stdout(&repo_dir, &["show", ":file.txt"]);
    assert_eq!(staged, "a\nb\nc\nd\ne\nf\ng\nH\ni\n");
    assert_eq!(std::fs::read_to_string(file_path).unwrap(), "a\nB\nc\nd\ne\nf\ng\nH\ni\n");
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn hunk_peek_gutter_buttons_act_on_the_clicked_hunk_and_preserve_other_changes() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    let original = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
    let updated = "a\nB\nc\nd\ne\nf\ng\nH\ni\n";
    let first_only = "a\nB\nc\nd\ne\nf\ng\nh\ni\n";
    let second_only = "a\nb\nc\nd\ne\nf\ng\nH\ni\n";
    for (action, symbol, staged, dirty, expected_index, expected_worktree) in [
        ("stage", "+", false, false, second_only, updated),
        ("discard", "−", false, false, original, first_only),
        ("unstage", "−", true, false, first_only, updated),
        ("dirty-discard", "−", false, true, original, updated),
    ] {
        let (repo_dir, file, mut app) = make_git_app_with_file(action, original, updated);
        if staged { run_git(&repo_dir, &["add", "file.txt"]); }
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.open_hunk_popup(10, 4, file.clone(), 1, if staged { 'S' } else { 'M' });
        app.wait_for_hunk_preview();
        assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 0);
        let mut terminal = Terminal::new(TestBackend::new(120, 60)).unwrap();
        terminal.draw(|frame| {
            crate::ui_editor::draw_editor(frame, &mut app, Rect::new(10, 3, 100, 48));
            crate::ui_hunk_peek::draw(frame, &mut app);
        }).unwrap();
        let area = app.hunk_popup_rect(120, 60);
        let buffer = terminal.backend().buffer();
        let mut hits = Vec::new();
        for row in area.y + 2..area.bottom() - 1 {
            for col in area.x..area.right() {
                if buffer[(col, row)].symbol() == symbol { hits.push((col, row)); }
            }
        }
        assert_eq!(hits.len(), 2, "{action}: both visible hunks have gutter actions");
        if dirty {
            app.editor_mut().unwrap().rope = ropey::Rope::from_str("unsaved changes\n");
            app.editor_mut().unwrap().modified = true;
        }
        let (column, row) = hits[1];
        crate::mouse::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE,
        });
        assert!(!app.hunk_popup_active, "{action}: clicked action was handled");
        assert_eq!(git_stdout(&repo_dir, &["show", ":file.txt"]), expected_index, "{action}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), expected_worktree, "{action}");
        if dirty {
            assert_eq!(app.editor().unwrap().rope.to_string(), "unsaved changes\n");
            assert!(app.notification_history.iter().any(|note| note.0.contains("Save the file")));
        }
        drop(app);
        let _ = std::fs::remove_dir_all(repo_dir);
    }
}

#[test]
fn hunk_peek_unsaved_gutter_click_uses_saved_snapshot_without_writing_the_file() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    let saved = "first();\r\n\told_界();\r\nlast();\r\n";
    let current = "first();\r\n\tnew_界();\r\nlast();\r\n";
    // A non-Git document must work too; missing/changed disk contents must not
    // replace the saved snapshot that produced the editor's unsaved marker.
    for (disk, exact_markers) in [(Some(saved), false), (Some("changed externally\n"), true), (None, true)] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("unsaved.rs");
        if let Some(contents) = disk { std::fs::write(&file, contents).unwrap(); }
        let mut app = App::new(dir.path().to_path_buf());
        let mut editor = Editor::test_with_text(saved);
        editor.file_path = Some(file.clone());
        editor.saved_rope = Some(editor.rope.clone());
        editor.cursor_line = 1;
        editor.cursor_col = 4;
        editor.selection_anchor = Some((1, 1));
        editor.paste_plain_with_text("new");
        if exact_markers {
            let diff = crate::editor::compute_unsaved_diff_markers(editor.saved_rope.as_ref().unwrap(), &editor.rope);
            editor.apply_unsaved_diff_cache(editor.content_version, diff);
        }
        let version = editor.content_version;
        let undo_len = editor.undo_stack.len();
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.show_minimap = false;
        app.sticky_scroll = false;
        let area = Rect::new(10, 3, 110, 25);
        let mut terminal = Terminal::new(TestBackend::new(130, 32)).unwrap();
        terminal.draw(|frame| crate::ui_editor::draw_editor(frame, &mut app, area)).unwrap();
        let gutter = app.gutter_area.unwrap();
        assert_eq!(app.editor().unwrap().line_diff_status(1), 'N');
        crate::mouse::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: gutter.x,
            row: gutter.y + 1, modifiers: KeyModifiers::NONE,
        });
        app.wait_for_hunk_preview();
        let peek = app.hunk_peek.as_ref().unwrap_or_else(|| panic!("{:?}", app.notification_history));
        assert!(peek.rows.iter().any(|row| row.kind == '-' && row.text == "\told_界();"));
        assert!(peek.rows.iter().any(|row| row.kind == '+' && row.text == "\tnew_界();"));
        assert_eq!(app.hunk_popup_items(), ["Revert Unsaved Hunk", "Close Preview"], "saved-buffer changes are not a Git patch");
        terminal.draw(|frame| {
            crate::ui_editor::draw_editor(frame, &mut app, area);
            crate::ui_hunk_peek::draw(frame, &mut app);
        }).unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Unsaved Changes (since last save)"));
        crate::ui_control_visibility_tests::snapshot(buffer, "Unsaved hunk preview");
        app.execute_hunk_popup_action(1);
        let editor = app.editor().unwrap();
        assert!(editor.modified);
        assert_eq!(editor.rope.to_string(), current);
        assert_eq!(editor.content_version, version);
        assert_eq!(editor.undo_stack.len(), undo_len);
        assert_eq!(std::fs::read_to_string(&file).ok().as_deref(), disk);
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), saved, "preview preserves undo");
    }
}

#[test]
fn hunk_peek_unsaved_untitled_document_does_not_require_a_disk_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().to_path_buf());
    let mut editor = Editor::new();
    editor.saved_rope = Some(editor.rope.clone());
    editor.paste_plain_with_text("new document\n");
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.open_hunk_popup(10, 4, None::<std::path::PathBuf>, 0, 'N');
    app.wait_for_hunk_preview();
    let peek = app.hunk_peek.as_ref().expect("untitled preview");
    assert!(peek.rows.iter().any(|row| row.kind == '+' && row.text == "new document"));
    assert!(app.editor().unwrap().modified);
    assert!(app.editor().unwrap().file_path.is_none());
    assert!(!app.save_as_active);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn hunk_peek_unsaved_changes_compare_with_last_save_instead_of_git_index() {
    let (repo_dir, file, mut app) = make_git_app_with_file(
        "peek-unsaved-base", "index\n", "saved\n",
    );
    let editor = app.editor_mut().unwrap();
    editor.saved_rope = Some(editor.rope.clone());
    // Reverting to the index still is an unsaved change relative to disk.
    editor.rope = ropey::Rope::from_str("index\n");
    editor.modified = true;
    app.open_hunk_popup(10, 4, file.clone(), 0, 'N');
    app.wait_for_hunk_preview();
    let peek = app.hunk_peek.as_ref().expect("unsaved preview despite no index difference");
    assert!(peek.rows.iter().any(|row| row.kind == '-' && row.text == "saved"));
    assert!(peek.rows.iter().any(|row| row.kind == '+' && row.text == "index"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "saved\n");
    assert_eq!(git_stdout(&repo_dir, &["show", ":file.txt"]), "index\n");
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn hunk_peek_git_preview_uses_live_buffer_and_keeps_unsaved_actions_protected() {
    let original = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
    let saved = "a\nB\nc\nd\ne\nf\ng\nh\ni\n";
    let current = "a\r\nB\r\nc\r\nd\r\ne\r\nf\r\ng\r\n界\r\ni\r\n";
    let (repo_dir, file, mut app) = make_git_app_with_file("peek-live", original, saved);
    let editor = app.editor_mut().unwrap();
    editor.saved_rope = Some(editor.rope.clone());
    editor.rope = ropey::Rope::from_str(current);
    editor.modified = true;
    for action in [0, 1] {
        app.open_hunk_popup(10, 4, file.clone(), 7, 'M');
        app.wait_for_hunk_preview();
        let peek = app.hunk_peek.as_ref().expect("Git preview includes unsaved content");
        assert_eq!(peek.hunks.len(), 2, "CRLF does not mark every line as changed");
        assert_eq!(peek.selected, 1);
        assert!(peek.rows.iter().any(|row| row.kind == '+' && row.text == "界"));
        app.execute_hunk_popup_action(action);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), saved);
        assert_eq!(git_stdout(&repo_dir, &["show", ":file.txt"]), original);
        assert_eq!(app.editor().unwrap().rope.to_string(), current);
        assert!(app.editor().unwrap().modified);
    }
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn hunk_peek_staged_preview_maps_hunks_through_unsaved_crlf_insertions() {
    let original = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
    let staged = "a\nB\nc\nd\ne\nf\ng\nH\ni\n";
    let current = "inserted\r\na\r\nB\r\nc\r\nd\r\ne\r\nf\r\ng\r\nH\r\ni\r\n";
    let (repo_dir, file, mut app) = make_git_app_with_file("peek-staged-live", original, staged);
    run_git(&repo_dir, &["add", "file.txt"]);
    let editor = app.editor_mut().unwrap();
    editor.saved_rope = Some(editor.rope.clone());
    editor.rope = ropey::Rope::from_str(current);
    editor.modified = true;
    app.open_hunk_popup(10, 4, file.clone(), 8, 'S');
    app.wait_for_hunk_preview();
    let peek = app.hunk_peek.as_ref().expect("staged preview after unsaved insertion");
    assert_eq!(peek.selected, 1);
    assert_eq!(peek.hunks[0].action_line, Some(2));
    assert_eq!(peek.hunks[1].action_line, Some(8));
    assert!(!peek.rows.iter().any(|row| row.text == "inserted"));
    assert!(peek.rows.iter().any(|row| row.kind == '+' && row.text == "H"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), staged);
    assert_eq!(git_stdout(&repo_dir, &["show", ":file.txt"]), staged);
    assert_eq!(app.editor().unwrap().rope.to_string(), current);
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn hunk_peek_opens_from_a_real_gutter_click_with_windows_paths() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    let (repo_dir, file_path, mut app) = make_git_app_with_file(
        "peek-gutter", "first\nold\nlast\n", "first\nnew\nlast\n",
    );
    let canonical = std::fs::canonicalize(&file_path).unwrap();
    for path in [file_path.clone(), canonical] {
        app.close_hunk_popup();
        app.tabs[0].editor.file_path = Some(path);
        app.tabs[0].editor.set_git_line_status(vec![' ', 'M', ' ']);
        app.show_minimap = false;
        app.sticky_scroll = false;
        let area = Rect::new(10, 3, 90, 25);
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|f| crate::ui_editor::draw_editor(f, &mut app, area)).unwrap();
        let gutter = app.gutter_area.unwrap();
        crate::mouse::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: gutter.x,
            row: gutter.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.hunk_popup_active);
        assert!(app.hunk_preview_feedback.as_ref().is_some_and(crate::hunk_preview::Feedback::loading));
        app.wait_for_hunk_preview();
        assert!(app.hunk_peek.is_some(), "Gutter click fell back to menu: {:?}", app.notification_history);
        terminal.draw(|f| {
            crate::ui_editor::draw_editor(f, &mut app, area);
            crate::ui_dialogs::draw_hunk_popup(f, &mut app);
        }).unwrap();
        let buffer = terminal.backend().buffer();
        let displayed: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(displayed.contains("Git Local Changes"));
        assert!(displayed.contains("old") && displayed.contains("new"));
    }
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn hunk_peek_opens_and_stages_from_deleted_line_gutter_markers() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    for (label, original, updated) in [
        ("start", "removed\nfirst\nlast\n", "first\nlast\n"),
        ("middle", "first\nremoved\nlast\n", "first\nlast\n"),
        ("end", "first\nlast\nremoved\n", "first\nlast\n"),
        ("all", "removed\n", ""),
    ] {
        let (repo_dir, _, mut app) = make_git_app_with_file(label, original, updated);
        let markers = app_git_display::compute_line_diff(
            &original.lines().collect::<Vec<_>>(),
            &updated.lines().collect::<Vec<_>>(),
        );
        let line = markers.iter().position(|&status| status == 'D').unwrap();
        app.tabs[0].editor.set_git_line_status(markers);
        app.show_minimap = false;
        app.sticky_scroll = false;
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|f| {
            crate::ui_editor::draw_editor(f, &mut app, Rect::new(10, 3, 90, 25));
        }).unwrap();
        let gutter = app.gutter_area.unwrap();
        crate::mouse::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: gutter.x,
            row: gutter.y + line as u16,
            modifiers: KeyModifiers::NONE,
        });
        app.wait_for_hunk_preview();
        let peek = app.hunk_peek.as_ref().unwrap_or_else(|| {
            panic!("{label}: deletion click did not open preview: {:?}", app.notification_history)
        });
        assert!(peek.rows.iter().any(|row| row.kind == '-' && row.text == "removed"));
        assert_eq!(app.hunk_popup_items(), ["Stage Hunk", "Discard Hunk", "Close Preview"]);
        app.execute_hunk_popup_action(0);
        assert_eq!(git_stdout(&repo_dir, &["show", ":file.txt"]), updated, "{label}");
        drop(app);
        let _ = std::fs::remove_dir_all(repo_dir);
    }
}

#[test]
fn hunk_peek_staged_preview_uses_index_after_worktree_line_shift() {
    let (repo_dir, file_path, mut app) = make_git_app_with_file(
        "peek-staged", "a\nb\nc\nd\ne\n", "a\nb\nC\nd\ne\n",
    );
    run_git(&repo_dir, &["add", "file.txt"]);
    std::fs::write(&file_path, "inserted\na\nb\nC\nd\ne\n").unwrap();
    app.tabs = vec![make_clean_file_tab(&file_path, "inserted\na\nb\nC\nd\ne\n")];
    app.open_hunk_popup(0, 4, file_path.clone(), 3, 'S');
    app.wait_for_hunk_preview();
    let peek = app.hunk_peek.as_ref().expect("staged preview");
    assert!(peek.rows.iter().any(|r| r.kind == '+' && r.text == "C" && r.new_line == Some(3)));
    assert!(!peek.rows.iter().any(|r| r.text == "inserted"));
    app.execute_hunk_popup_action(0);
    assert!(git_stdout(&repo_dir, &["diff", "--cached"]).is_empty());
    assert!(std::fs::read_to_string(file_path).unwrap().contains("inserted\na\nb\nC"));
    drop(app);
    let _ = std::fs::remove_dir_all(repo_dir);
}

#[test]
fn git_unstage_hunk_reverts_only_selected_staged_hunk() {
    let (repo_dir, file_path, mut app) = make_git_app_with_file(
        "unstage-hunk",
        "one\ntwo\nthree\nfour\n",
        "one\ntwo changed\nthree\nfour changed\n",
    );
    app.git_stage_hunk(&file_path, 1);

    app.git_unstage_hunk(&file_path, 1);

    let staged = git_stdout(
        &repo_dir,
        &["diff", "--cached", "--unified=0", "--", "file.txt"],
    );
    let unstaged = git_stdout(&repo_dir, &["diff", "--unified=0", "--", "file.txt"]);
    assert!(staged.trim().is_empty());
    assert!(unstaged.contains("+two changed"));
    assert!(unstaged.contains("+four changed"));

    let _ = std::fs::remove_dir_all(repo_dir); // Error OK: dir may not exist
}

#[test]
fn git_discard_hunk_restores_only_selected_hunk() {
    let (repo_dir, file_path, mut app) = make_git_app_with_file(
        "discard-hunk",
        "one\ntwo\nthree\nfour\n",
        "one\ntwo changed\nthree\nfour changed\n",
    );

    app.git_discard_hunk(&file_path, 1);

    let content = std::fs::read_to_string(&file_path)
        .expect("discarded file")
        .replace("\r\n", "\n");
    assert_eq!(content, "one\ntwo\nthree\nfour changed\n");
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.editor().is_some_and(|editor| {
            editor.rope.to_string().replace("\r\n", "\n") == "one\ntwo\nthree\nfour changed\n"
        })
    });
    let editor = app.editor().expect("open editor");
    assert_eq!(editor.rope.to_string().replace("\r\n", "\n"), content);
    assert!(!editor.modified);

    let _ = std::fs::remove_dir_all(repo_dir); // Error OK: dir may not exist
}

#[test]
fn commit_diff_keeps_diff_target_paths_for_marker_refresh_guard() {
    let repo_dir = temp_repo_dir("commit-diff-markers");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    init_git_fixture(&repo_dir);

    let file_path = repo_dir.join("file.txt");
    std::fs::write(&file_path, "before\n").expect("initial file");
    commit_fixture_files(&repo_dir, &["file.txt"], "initial");

    std::fs::write(&file_path, "after\n").expect("updated file");
    commit_fixture_files(&repo_dir, &["file.txt"], "updated");

    let mut app = App::new(repo_dir.clone());
    app.commit_details.data.hash = fixture_oid(&repo_dir, "HEAD").to_string();
    app.commit_details.data.files = vec![crate::commit_details::CommitFile::new('M', "file.txt")];

    app.open_source_control_commit_details_file_diff(0);

    assert_eq!(app.compare_base_path, None);
    // Git expands Windows short-name aliases in the repository workdir.
    assert_eq!(
        app.diff_target_path.as_ref().unwrap().canonicalize().unwrap(),
        file_path.canonicalize().unwrap()
    );
    assert!(app.active_editor_matches_diff_target());
    let parent = fixture_oid(&repo_dir, "HEAD^").to_string();
    let commit = fixture_oid(&repo_dir, "HEAD").to_string();
    assert_eq!(
        app.diff_labels,
        Some(crate::app::DiffLabels::new(
            format!("{} (parent)", &parent[..7]),
            format!("{} (commit)", &commit[..7]),
        ))
    );

    app.refresh_git_line_status_async();
    assert!(!app.bg_git_diff_running);

    for mode in [crate::app::DiffMode::Inline, crate::app::DiffMode::SideBySide] {
        app.execute_palette_command("toggle_diff_layout");
        app.open_source_control_commit_details_file_diff(0);
        assert!(app.diff_mode == mode);
    }

    let _ = std::fs::remove_dir_all(repo_dir); // Error OK: dir may not exist
}

#[test]
fn commit_diff_refresh_preserves_parent_snapshot_content() {
    let repo_dir = temp_repo_dir("commit-diff-refresh");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    init_git_fixture(&repo_dir);

    let file_path = repo_dir.join("file.txt");
    std::fs::write(&file_path, "before\n").expect("initial file");
    commit_fixture_files(&repo_dir, &["file.txt"], "initial");

    std::fs::write(&file_path, "after\n").expect("updated file");
    commit_fixture_files(&repo_dir, &["file.txt"], "updated");

    std::fs::write(&file_path, "worktree\n").expect("worktree file");

    let mut app = App::new(repo_dir.clone());
    app.commit_details.data.hash = fixture_oid(&repo_dir, "HEAD").to_string();
    app.commit_details.data.files = vec![crate::commit_details::CommitFile::new('M', "file.txt")];

    app.open_source_control_commit_details_file_diff(0);
    app.refresh_compare_diff();

    assert_eq!(app.compare_base_content.as_deref(), Some("before\n"));
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff("before\n", "after\n")
    );

    let _ = std::fs::remove_dir_all(repo_dir); // Error OK: dir may not exist
}

#[test]
fn git_branch_picker_filtered_items_empty_query_returns_all() {
    let mut app = App::test_new();
    app.git_branch_picker_items = vec![
        GitBranchPickerItem::Local {
            name: "main".to_string(),
        },
        GitBranchPickerItem::Local {
            name: "feature/login".to_string(),
        },
        GitBranchPickerItem::Remote {
            remote_ref: "origin/develop".to_string(),
            local_name: "develop".to_string(),
        },
    ];
    app.git_branch_picker_query = "".to_string();

    let filtered = app.git_branch_picker_filtered_items();
    assert_eq!(filtered.len(), 3);
    assert_eq!(filtered[0].target_branch_name(), "main");
    assert_eq!(filtered[1].target_branch_name(), "feature/login");
    assert_eq!(filtered[2].target_branch_name(), "develop");
}

#[test]
fn git_branch_picker_filtered_items_query_returns_matching() {
    let mut app = App::test_new();
    app.git_branch_picker_items = vec![
        GitBranchPickerItem::Local {
            name: "main".to_string(),
        },
        GitBranchPickerItem::Local {
            name: "feature/login".to_string(),
        },
        GitBranchPickerItem::Remote {
            remote_ref: "origin/develop".to_string(),
            local_name: "develop".to_string(),
        },
    ];
    app.git_branch_picker_query = "feature".to_string();

    let filtered = app.git_branch_picker_filtered_items();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].target_branch_name(), "feature/login");
}

#[test]
fn git_branch_picker_create_candidate_returns_none_when_matching_existing() {
    let mut app = App::test_new();
    app.git_branch_picker_items = vec![
        GitBranchPickerItem::Local {
            name: "main".to_string(),
        },
        GitBranchPickerItem::Remote {
            remote_ref: "origin/develop".to_string(),
            local_name: "develop".to_string(),
        },
    ];
    app.git_branch_picker_query = "main".to_string();
    assert_eq!(app.git_branch_picker_create_candidate(), None);

    app.git_branch_picker_query = "develop".to_string();
    assert_eq!(app.git_branch_picker_create_candidate(), None);
}

#[test]
fn git_branch_picker_create_candidate_returns_some_when_no_match() {
    let mut app = App::test_new();
    app.git_branch_picker_items = vec![GitBranchPickerItem::Local {
        name: "main".to_string(),
    }];
    app.git_branch_picker_query = "feature/new".to_string();

    assert_eq!(
        app.git_branch_picker_create_candidate(),
        Some("feature/new".to_string())
    );
}

#[test]
fn parse_git_file_patch_single_hunk() {
    let diff = "\
diff --git a/file.txt b/file.txt
index abc..def 100644
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,4 @@
 line1
+added
 line2
 line3
";
    let patch = app_git_display::parse_git_file_patch(diff).expect("should parse");
    assert_eq!(patch.hunks.len(), 1);
    assert_eq!(patch.hunks[0].old_start, 1);
    assert_eq!(patch.hunks[0].old_count, 3);
    assert_eq!(patch.hunks[0].new_start, 1);
    assert_eq!(patch.hunks[0].new_count, 4);
}

#[test]
fn parse_git_file_patch_multiple_hunks() {
    let diff = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,2 +1,2 @@
-old1
+new1
 same
@@ -10,3 +10,4 @@
 ctx
+inserted
 ctx2
 ctx3
";
    let patch = app_git_display::parse_git_file_patch(diff).expect("should parse");
    assert_eq!(patch.hunks.len(), 2);
    assert_eq!(patch.hunks[0].old_start, 1);
    assert_eq!(patch.hunks[0].old_count, 2);
    assert_eq!(patch.hunks[0].new_start, 1);
    assert_eq!(patch.hunks[0].new_count, 2);
    assert_eq!(patch.hunks[1].old_start, 10);
    assert_eq!(patch.hunks[1].old_count, 3);
    assert_eq!(patch.hunks[1].new_start, 10);
    assert_eq!(patch.hunks[1].new_count, 4);
}

#[test]
fn compare_with_git_populates_line_markers_for_diff_indicators() {
    let repo_dir = temp_repo_dir("compare-with-git-markers");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    init_git_fixture(&repo_dir);

    let file_path = repo_dir.join("file.txt");
    std::fs::write(&file_path, "before\n").expect("initial file");
    commit_fixture_files(&repo_dir, &["file.txt"], "initial");

    let mut app = App::new(repo_dir.clone());
    app.tabs = vec![make_modified_file_tab(&file_path, "after\n")];
    app.active_tab = 0;

    app.execute_palette_command("compare_with_git");
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        matches!(app.diff_mode, crate::app::DiffMode::SideBySide)
    });

    assert_eq!(app.compare_base_content.as_deref(), Some("before\n"));
    assert_eq!(
        app.compare_diff_lines,
        crate::app::compute_file_diff("before\n", "after\n")
    );
    assert!(app
        .editor()
        .expect("open editor")
        .git_line_status()
        .iter()
        .any(|marker| *marker != ' '));

    let _ = std::fs::remove_dir_all(repo_dir); // Error OK: dir may not exist
}
