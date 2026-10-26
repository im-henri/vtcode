//! The same stateful editor/Git/Explorer workflow runs over loopback on both
//! platforms and, opt-in, against a real SSH service and scheduler PTY.
use super::*;
use crate::{app::App, background::CancellationToken, explorer::ExplorerSortMode};
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};

fn path(name: &str) -> RemotePath {
    RemotePath::new(name.as_bytes().to_vec()).unwrap()
}
fn create(workspace: &workspace::Workspace, name: &str, text: &str) {
    workspace
        .client
        .call_file(Command::Save {
            path: path(name),
            base: None,
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: 0,
                text: text.into(),
            },
        })
        .unwrap();
}
fn git(workspace: &workspace::Workspace, repository: &str, task: crate::git_tasks::Task) {
    let Reply::GitAction(_) = workspace
        .client
        .call(Command::GitAction {
            repo: path(repository),
            task: task
                .try_map_paths(|name| RemotePath::new(name.into_bytes()))
                .unwrap(),
        })
        .unwrap()
    else {
        panic!("Git action reply");
    };
}
fn index(workspace: &workspace::Workspace, name: &str) -> String {
    let Reply::GitSources(sources) = workspace
        .client
        .call(Command::GitSources {
            file: path(name),
            repository: Some(path("core-workflow")),
            staged: false,
        })
        .unwrap()
    else {
        panic!("Git source reply");
    };
    sources.index.unwrap().to_string()
}
#[track_caller]
fn wait(app: &mut App, ready: impl Fn(&App) -> bool) {
    app.wait_for_background(Duration::from_secs(90), ready);
}
#[track_caller]
fn peek(app: &mut App, key: &Path, line: usize, status: char) {
    app.open_hunk_popup(0, 0, key.to_path_buf(), line, status);
    wait(app, |app| app.hunk_peek.is_some());
}

pub(super) fn run(workspace: &workspace::Workspace) -> serde_json::Value {
    use crate::git_tasks::Task;
    let started = Instant::now();
    for directory in [
        "core-workflow",
        "core-workflow/target",
        "core-workflow/nested",
    ] {
        workspace
            .client
            .call(Command::CreateDirectory {
                path: path(directory),
            })
            .unwrap();
    }
    git(workspace, "core-workflow", Task::Init);
    let name = "core-workflow/file[1] λ.rs";
    let original = "a\nbase\nc\nd\ne\nbase2\ng\n";
    let changed = "a\nchanged\nc\nd\ne\nchanged2\ng\n";
    create(workspace, name, original);
    create(workspace, "core-workflow/.gitignore", "target/\n");
    create(workspace, "core-workflow/.gitattributes", "* -text\n");
    git(
        workspace,
        "core-workflow",
        Task::Commit {
            message: "Disposable core workflow baseline".into(),
            stage_all: true,
        },
    );
    git(workspace, "core-workflow/nested", Task::Init);
    create(workspace, "core-workflow/nested/child.rs", "nested\n");
    create(workspace, "core-workflow/target/cache.rs", "ignored\n");
    let discovery_start = Instant::now();
    let Reply::GitStatus { discovery, scans } = workspace
        .client
        .call(Command::GitStatus {
            include_untracked: true,
            known: None,
            parent_open: false,
            closed: vec![],
        })
        .unwrap()
    else {
        panic!("Git status reply");
    };
    let discovery_ms = discovery_start.elapsed().as_secs_f64() * 1000.;
    let (_, view_scans) = workspace
        .git_snapshot(
            true,
            Some(discovery.clone()),
            false,
            vec![],
            None,
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(
        view_scans
            .iter()
            .any(|scan| scan.files.iter().any(|file| file.path == "nested")),
        "directory status maps to a valid workbench path"
    );
    for root in [path("core-workflow"), path("core-workflow/nested")] {
        assert!(
            discovery.roots.contains(&root),
            "missing nested repository: {discovery:?}"
        );
        assert!(scans.iter().any(|scan| scan.root == root));
    }
    let expanded = [
        "core-workflow",
        "core-workflow/target",
        "core-workflow/nested",
    ]
    .into_iter()
    .map(|name| workspace.view_key(&path(name)))
    .collect();
    let tree_start = Instant::now();
    let (tree, _) = workspace
        .explorer_tree(
            &expanded,
            ExplorerSortMode::Name,
            None,
            true,
            &CancellationToken::new(),
        )
        .unwrap();
    let explorer_ms = tree_start.elapsed().as_secs_f64() * 1000.;
    for name in ["core-workflow/target", "core-workflow/target/cache.rs"] {
        assert_eq!(
            tree.iter()
                .find(|entry| entry.path == workspace.view_key(&path(name)))
                .unwrap()
                .git_status,
            crate::explorer::GitStatus::Ignored
        );
    }
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.source_control_enabled = true;
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    let key = workspace.view_key(&path(name));
    app.open_file(&key);
    wait(&mut app, |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.rope.to_string(), original);
    editor.selection_anchor = Some((0, 0));
    editor.set_cursor_without_scrolling(editor.rope.len_lines() - 1, 0);
    editor.paste_plain_with_text(changed);
    assert!(app.editor().unwrap().modified);
    peek(&mut app, &key, 1, 'N');
    app.execute_hunk_popup_action(0); // undo only the first unsaved hunk
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "a\nbase\nc\nd\ne\nchanged2\ng\n"
    );
    assert_eq!(workspace.load(&key).unwrap().rope.to_string(), original);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), changed);
    app.editor_mut().unwrap().redo();
    app.editor_mut().unwrap().undo();
    app.save_current_file();
    app.editor_mut().unwrap().insert_char('λ');
    let later = app.editor().unwrap().rope.to_string();
    wait(&mut app, |app| app.pending_file_saves.is_empty());
    assert_eq!(app.editor().unwrap().rope.to_string(), later);
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), changed);
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    peek(&mut app, &key, 1, 'U');
    app.execute_hunk_popup_action(0); // stage first hunk only
    wait(&mut app, |app| !app.bg_git_action_running);
    assert_eq!(index(workspace, name), "a\nchanged\nc\nd\ne\nbase2\ng\n");
    peek(&mut app, &key, 1, 'S');
    app.execute_hunk_popup_action(0); // unstage
    wait(&mut app, |app| !app.bg_git_action_running);
    assert_eq!(index(workspace, name), original);
    peek(&mut app, &key, 1, 'U');
    let loaded = workspace.load(&key).unwrap();
    let external = changed.replace("changed\n", "external\n");
    workspace
        .client
        .call_file(Command::Save {
            path: path(name),
            base: Some(loaded.remote_checkpoint.unwrap().revision.clone()),
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: loaded.rope.len_chars(),
                text: external.clone(),
            },
        })
        .unwrap();
    app.execute_hunk_popup_action(0); // stale preview must not stage replacement
    wait(&mut app, |app| !app.bg_git_action_running);
    assert_eq!(index(workspace, name), original);
    app.queue_all_clean_file_reloads();
    wait(&mut app, |app| {
        app.editor()
            .is_some_and(|editor| editor.rope.to_string() == external)
    });
    peek(&mut app, &key, 1, 'U');
    app.execute_hunk_popup_action(1); // discard first saved hunk, retain second
    let discarded = "a\nbase\nc\nd\ne\nchanged2\ng\n";
    wait(&mut app, |app| !app.bg_git_action_running);
    assert_eq!(
        workspace.load(&key).unwrap().rope.to_string(),
        discarded,
        "{:?}",
        app.notification_history
    );
    wait(&mut app, |app| {
        !app.bg_git_action_running
            && app
                .editor()
                .is_some_and(|editor| editor.rope.to_string() == discarded)
    });
    assert_eq!(workspace.load(&key).unwrap().rope.to_string(), discarded);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), external);
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(!app.editor().unwrap().modified);

    // Keep actual repository scans and directory refreshes outstanding while
    // local input, undo and rendering run. No frame performs a remote call.
    let background_workspace = workspace.clone();
    let scans = std::thread::spawn(move || {
        for _ in 0..4 {
            let (tree, _) = background_workspace
                .explorer_tree(
                    &expanded,
                    ExplorerSortMode::Name,
                    None,
                    true,
                    &CancellationToken::new(),
                )
                .unwrap();
            assert!(tree.iter().any(|entry| entry.name == "cache.rs"));
            background_workspace
                .client
                .call(Command::GitStatus {
                    include_untracked: true,
                    known: Some(discovery.clone()),
                    parent_open: false,
                    closed: vec![path("core-workflow/nested")],
                })
                .unwrap();
        }
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut samples = Vec::new();
    for _ in 0..128 {
        let sample = Instant::now();
        let editor = app.editor_mut().unwrap();
        editor.insert_char('🦀');
        editor.undo();
        editor.redo();
        editor.undo();
        editor.move_down();
        editor.move_up();
        editor.scroll_down(2);
        editor.scroll_up(2);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        app.poll_background_results();
        samples.push(sample.elapsed().as_secs_f64() * 1000.);
    }
    scans.join().unwrap();
    assert_eq!(app.editor().unwrap().rope.to_string(), discarded);
    assert!(!app.editor().unwrap().modified);
    samples.sort_by(f64::total_cmp);
    serde_json::json!({"discovery_ms": discovery_ms, "explorer_ms": explorer_ms, "input_draw_p95_ms": samples[samples.len()*95/100], "input_draw_max_ms": samples[samples.len()-1], "samples": samples.len(), "wall_ms": started.elapsed().as_secs_f64()*1000.})
}

#[test]
fn remote_git_explorer_and_editor_combination_keeps_hunks_saves_and_undo_consistent() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace::Workspace::new(super::tests::connection(
        directory.path(),
        Default::default(),
        Default::default(),
    ));
    println!("VTCODE_CORE_WORKFLOW {}", run(&workspace));
    workspace.client.call(Command::Shutdown).unwrap();
}

/// Real POSIX names that collapse to the same lossy Unicode display must remain
/// separate through status -> workbench view key -> action -> native index.
pub(super) fn raw_git_paths(workspace: &workspace::Workspace) {
    use crate::app::GitFileChange;
    use crate::git_tasks::Task;
    let repository = path("git-identities");
    workspace
        .client
        .call(Command::CreateDirectory {
            path: repository.clone(),
        })
        .unwrap();
    git(workspace, "git-identities", Task::Init);
    create(workspace, "git-identities/.gitattributes", "* -text\n");
    let names = [
        b"byte-\xff.rs".as_slice(),
        b"byte-\xfe.rs",
        "byte-�.rs".as_bytes(),
        br"literal\name [1].rs",
        b"trailing.rs ",
        b"line\nname.rs",
    ];
    let mut keys = Vec::new();
    for (number, name) in names.iter().enumerate() {
        let file = repository.join_relative(name).unwrap();
        workspace
            .client
            .call_file(Command::Save {
                path: file.clone(),
                base: None,
                encoding: "UTF-8".into(),
                change: Change {
                    start: 0,
                    end: 0,
                    text: format!("original {number}\n"),
                },
            })
            .unwrap();
        keys.push(workspace.view_key(&file));
    }
    let mut app = App::new_deferred(workspace.view_root.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = true;
    let root = workspace.view_key(&repository);
    let scan = |app: &mut App| {
        // Use the ordinary UI controller: it binds the repository collection
        // to its workspace before applying status and choosing a repository.
        app.repositories.rescan = true;
        app.refresh_source_control_async();
        wait(app, |app| app.repositories.discovered && !app.bg_git_status_running && !app.bg_git_status_pending);
        if app.repositories.active.as_deref() != Some(root.as_path()) { app.select_source_control_repository(&root); }
        assert_eq!(app.source_control_root(), root);
    };
    let change = |app: &App, key: &Path, staged: bool| -> GitFileChange {
        app.source_control_files
            .iter()
            .find(|file| file.staged == staged && root.join(&file.path) == key)
            .cloned()
            .expect("exact raw filename in status")
    };
    let indexed = |key: &Path| {
        let Reply::GitSources(sources) = workspace
            .client
            .call(Command::GitSources {
                file: workspace.remote_path(key).unwrap(),
                repository: Some(repository.clone()),
                staged: false,
            })
            .unwrap()
        else {
            panic!();
        };
        sources.index.map(|text| text.to_string())
    };
    scan(&mut app);
    for (name, key) in names.iter().zip(&keys) {
        let file = change(&app, key, false);
        assert_eq!(
            workspace
                .remote_path(&root.join(file.path))
                .unwrap()
                .relative_to(&repository)
                .unwrap(),
            RemotePath::new(name.to_vec()).unwrap()
        );
    }
    let raw = change(&app, &keys[0], false);
    app.git_stage_changes(vec![raw.clone()]);
    wait(&mut app, |app| !app.bg_git_action_running);
    assert_eq!(indexed(&keys[0]).as_deref(), Some("original 0\n"), "{:?}; root={:?}; expected={root:?}", app.notification_history, app.source_control_root());
    for key in keys.iter().skip(1) {
        assert!(
            indexed(key).is_none(),
            "a neighbouring name must not be staged"
        );
    }
    let mut staged = raw;
    staged.staged = true;
    app.git_unstage_changes(vec![staged]);
    wait(&mut app, |app| !app.bg_git_action_running);
    assert!(indexed(&keys[0]).is_none());
    git(
        workspace,
        "git-identities",
        Task::Commit {
            message: "Raw-path baseline".into(),
            stage_all: true,
        },
    );
    let loaded = workspace.load(&keys[0]).unwrap();
    workspace
        .client
        .call_file(Command::Save {
            path: workspace.remote_path(&keys[0]).unwrap(),
            base: Some(loaded.remote_checkpoint.unwrap().revision.clone()),
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: loaded.rope.len_chars(),
                text: "changed raw file\n".into(),
            },
        })
        .unwrap();
    scan(&mut app);
    app.git_stage_changes(vec![change(&app, &keys[0], false)]);
    wait(&mut app, |app| !app.bg_git_action_running);
    assert_eq!(indexed(&keys[0]).as_deref(), Some("changed raw file\n"));
    scan(&mut app);
    app.git_discard_changes(vec![change(&app, &keys[0], true)]);
    wait(&mut app, |app| !app.bg_git_action_running);
    assert_eq!(
        workspace.load(&keys[0]).unwrap().rope.to_string(),
        "original 0\n"
    );
    assert_eq!(indexed(&keys[0]).as_deref(), Some("original 0\n"));
    for (number, key) in keys.iter().enumerate().skip(1) {
        assert_eq!(
            workspace.load(key).unwrap().rope.to_string(),
            format!("original {number}\n")
        );
    }
    let added = repository.join_relative(b"new-\xfd.rs").unwrap();
    workspace
        .client
        .call_file(Command::Save {
            path: added.clone(),
            base: None,
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: 0,
                text: "temporary\n".into(),
            },
        })
        .unwrap();
    let added_key = workspace.view_key(&added);
    scan(&mut app);
    app.git_discard_changes(vec![change(&app, &added_key, false)]);
    wait(&mut app, |app| !app.bg_git_action_running);
    assert!(workspace.load(&added_key).is_err());
    assert_eq!(
        workspace.load(&keys[2]).unwrap().rope.to_string(),
        "original 2\n"
    );

    let renamed = repository.join_relative(b"renamed-\xff.rs").unwrap();
    workspace
        .client
        .call(Command::Rename {
            from: workspace.remote_path(&keys[0]).unwrap(),
            to: renamed.clone(),
        })
        .unwrap();
    git(workspace, "git-identities", Task::StageAll);
    scan(&mut app);
    let renamed_key = workspace.view_key(&renamed);
    let rename = change(&app, &renamed_key, true);
    assert_eq!(rename.status, 'R');
    assert_eq!(root.join(rename.original_path.as_ref().unwrap()), keys[0]);
    app.git_unstage_changes(vec![rename]);
    wait(&mut app, |app| !app.bg_git_action_running);
    assert!(indexed(&renamed_key).is_none());
    assert_eq!(indexed(&keys[0]).as_deref(), Some("original 0\n"));
    println!(
        "VTCODE_RAW_GIT_PATHS: distinct raw/Unicode names, stage/unstage, staged and untracked discard, rename and neighbouring file preservation passed"
    );
}

#[cfg(unix)]
#[test]
fn remote_git_status_and_mutations_preserve_non_utf8_and_literal_filename_identity() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace::Workspace::new(super::tests::connection(
        directory.path(),
        Default::default(),
        Default::default(),
    ));
    raw_git_paths(&workspace);
    workspace.client.call(Command::Shutdown).unwrap();
}
