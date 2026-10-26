use super::*;

fn close_fixture(mut app: App, root: std::path::PathBuf) {
    // Git invalidations can still be reading this repository after the final
    // assertion. Windows cannot delete it while those workers hold files open.
    app.wait_for_background(std::time::Duration::from_secs(10), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn discard_is_undoable_before_background_reload_and_keeps_each_hunk_separate() {
    let original = "a\nb\nc\nd\ne\nf\n";
    let changed = "a\nB\nc\nD\ne\nF\n";
    let after_first = "a\nB\nc\nd\ne\nF\n";
    let after_second = "a\nB\nc\nd\ne\nf\n";
    let (root, file, mut app) = make_git_app_with_file("discard-undo", original, changed);
    let editor = app.editor_mut().unwrap();
    editor.record_saved_snapshot(editor.rope.clone());
    app.git_stage_hunk(&file, 1);
    app.git_discard_hunk(&file, 3);
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        after_first,
        "discard must install its undo step before returning"
    );
    app.git_discard_hunk(&file, 5);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), after_second);
    assert_eq!(git_stdout(&root, &["show", ":file.txt"]), after_second);
    let history = app.editor().unwrap().undo_stack.len();
    assert_eq!(history, 2);
    // A delayed watcher notification must not add a third, empty undo step.
    app.editor_mut().unwrap().reload_from_disk().unwrap();
    assert_eq!(app.editor().unwrap().undo_stack.len(), history);
    for (redo, expected, dirty) in [
        (false, after_first, true),
        (false, changed, true),
        (true, after_first, true),
        (true, after_second, false),
    ] {
        let editor = app.editor_mut().unwrap();
        if redo {
            editor.redo();
        } else {
            editor.undo();
        }
        assert_eq!(editor.rope.to_string(), expected);
        assert_eq!(editor.modified, dirty);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            after_second,
            "undo/redo edits the buffer, not disk"
        );
        assert_eq!(
            git_stdout(&root, &["show", ":file.txt"]),
            after_second,
            "undo/redo must not change the index"
        );
    }
    app.git_unstage_hunk(&file, 1);
    assert_eq!(git_stdout(&root, &["show", ":file.txt"]), original);
    assert_eq!(
        app.editor().unwrap().undo_stack.len(),
        history,
        "staging does not add text undo steps"
    );
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), after_first);
    close_fixture(app, root);
}

#[test]
fn stage_unstage_discard_round_trips_line_endings_empty_files_and_eof() {
    for (name, original, changed, line) in [
        (
            "crlf",
            "first\r\nold\r\nlast\r\n",
            "first\r\nnew\r\nlast\r\n",
            1,
        ),
        (
            "mixed",
            "first\r\nold\nlast\r\n",
            "first\r\nnew\r\nlast\r\n",
            1,
        ),
        ("unicode", "界\t🙂\nold", "界\t🙂\nnew", 1),
        ("add-newline", "text", "text\n", 0),
        ("remove-newline", "text\n", "text", 0),
        ("empty", "", "added", 0),
        ("delete-all", "removed\n", "", 0),
        ("delete-first", "removed\nkeep\n", "keep\n", 0),
        ("delete-last", "keep\nremoved\n", "keep\n", 0),
        ("insert-first", "keep\n", "added\nkeep\n", 0),
        ("insert-last", "keep\n", "keep\nadded\n", 1),
    ] {
        let (root, file, mut app) = make_git_app_with_file(name, original, changed);
        app.git_stage_hunk(&file, line);
        assert_eq!(
            git_stdout(&root, &["show", ":file.txt"]),
            changed,
            "stage {name}: {:?}",
            app.notification_history
        );
        app.git_unstage_hunk(&file, line);
        assert_eq!(
            git_stdout(&root, &["show", ":file.txt"]),
            original,
            "unstage {name}"
        );
        app.git_discard_hunk(&file, line);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            original,
            "discard {name}"
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            original,
            "buffer {name}"
        );
        app.editor_mut().unwrap().undo();
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            changed,
            "undo {name}"
        );
        assert!(app.editor().unwrap().modified, "undo {name}");
        app.editor_mut().unwrap().redo();
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            original,
            "redo {name}"
        );
        assert!(!app.editor().unwrap().modified, "redo {name}");
        close_fixture(app, root);
    }
}

#[test]
fn stale_preview_never_stages_discards_or_unstages_a_replacement_hunk() {
    for action in ["stage", "discard", "unstage"] {
        let (root, file, mut app) = make_git_app_with_file(action, "base\n", "previewed\n");
        let staged = action == "unstage";
        if staged {
            run_git(&root, &["add", "file.txt"]);
        }
        app.open_hunk_popup(0, 0, file.clone(), 0, if staged { 'S' } else { 'U' });
        assert!(app.hunk_peek.is_some());
        std::fs::write(&file, "newer change\n").unwrap();
        if staged {
            run_git(&root, &["add", "file.txt"]);
        }
        let index = git_stdout(&root, &["show", ":file.txt"]);
        app.execute_hunk_popup_action(usize::from(action == "discard"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "newer change\n",
            "{action}"
        );
        assert_eq!(git_stdout(&root, &["show", ":file.txt"]), index, "{action}");
        assert!(app.editor().unwrap().undo_stack.is_empty());
        assert!(
            app.notification_history
                .iter()
                .any(|note| note.0.contains("changed")),
            "{action}: {:?}",
            app.notification_history
        );
        close_fixture(app, root);
    }
}

#[test]
fn hunk_actions_wait_for_pending_saves_and_git_operations() {
    for busy in ["save", "git"] {
        for action in ["stage", "unstage", "discard"] {
            let (root, file, mut app) = make_git_app_with_file(busy, "old\n", "new\n");
            if action == "unstage" {
                run_git(&root, &["add", "file.txt"]);
            }
            let index = git_stdout(&root, &["show", ":file.txt"]);
            if busy == "save" {
                app.pending_file_saves
                    .insert(file.clone(), (app.workspace_generation, 0.into()));
            } else {
                app.bg_git_action_running = true;
            }
            match action {
                "stage" => app.git_stage_hunk(&file, 0),
                "unstage" => app.git_unstage_hunk(&file, 0),
                _ => app.git_discard_hunk(&file, 0),
            }
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "new\n",
                "{busy}/{action}"
            );
            assert_eq!(
                git_stdout(&root, &["show", ":file.txt"]),
                index,
                "{busy}/{action}"
            );
            assert!(app.editor().unwrap().undo_stack.is_empty());
            close_fixture(app, root);
        }
    }
}

#[test]
fn discard_checks_unsaved_and_read_only_copies_in_other_panes() {
    for protected in ["modified", "read-only"] {
        let (root, file, mut app) = make_git_app_with_file(protected, "old\n", "new\n");
        app.tabs.push(make_clean_file_tab(&file, "new\n"));
        app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
        let other_group = app.active_group;
        app.switch_group(0);
        let other = &mut app.groups[other_group].tabs[0].editor;
        if protected == "modified" {
            other.rope = ropey::Rope::from_str("unsaved\n");
            other.modified = true;
        } else {
            other.read_only = true;
        }
        app.git_discard_hunk(&file, 0);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "new\n",
            "{protected}"
        );
        assert_eq!(app.editor().unwrap().rope.to_string(), "new\n");
        close_fixture(app, root);
    }
}

#[test]
fn hunk_actions_use_literal_filenames_and_stable_patch_options() {
    let original = "a\nb\nc\nd\ne\nf\n";
    let changed = "a\nB\nc\nd\ne\nF\n";
    let first_only = "a\nB\nc\nd\ne\nf\n";
    let (root, _, mut app) = make_git_app_with_file("hunk-options", original, original);
    let file = root.join("file[1].txt");
    let other = root.join("file1.txt");
    std::fs::write(&file, original).unwrap();
    std::fs::write(&other, original).unwrap();
    commit_fixture_files(&root, &["file[1].txt", "file1.txt"], "paths");
    std::fs::write(&file, changed).unwrap();
    std::fs::write(&other, "unrelated change\n").unwrap();
    app.tabs = vec![make_clean_file_tab(&file, changed)];
    let repo = git2::Repository::open(&root).unwrap();
    let mut config = repo.config().unwrap();
    config.set_bool("diff.noprefix", true).unwrap();
    config.set_i32("diff.interHunkContext", 20).unwrap();
    app.open_hunk_popup(0, 0, file.clone(), 1, 'U');
    assert_eq!(app.hunk_peek.as_ref().unwrap().hunks.len(), 2);
    app.execute_hunk_popup_action(0);
    assert_eq!(git_stdout(&root, &["show", ":file[1].txt"]), first_only);
    assert_eq!(git_stdout(&root, &["show", ":file1.txt"]), original);
    assert_eq!(
        std::fs::read_to_string(other).unwrap(),
        "unrelated change\n"
    );
    app.git_unstage_hunk(&file, 1);
    assert_eq!(git_stdout(&root, &["show", ":file[1].txt"]), original);
    app.git_discard_hunk(&file, 5);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), first_only);
    drop(config);
    drop(repo);
    close_fixture(app, root);
}

#[test]
fn hunk_actions_preserve_whitespace_despite_git_apply_configuration() {
    for policy in ["fix", "error-all"] {
        let (root, file, mut app) = make_git_app_with_file("hunk-whitespace", "old \n", "new  \n");
        let repo = git2::Repository::open(&root).unwrap();
        repo.config()
            .unwrap()
            .set_str("apply.whitespace", policy)
            .unwrap();
        app.git_stage_hunk(&file, 0);
        assert_eq!(
            git_stdout(&root, &["show", ":file.txt"]),
            "new  \n",
            "{policy}"
        );
        app.git_unstage_hunk(&file, 0);
        assert_eq!(
            git_stdout(&root, &["show", ":file.txt"]),
            "old \n",
            "{policy}"
        );
        app.git_discard_hunk(&file, 0);
        assert_eq!(std::fs::read_to_string(file).unwrap(), "old \n", "{policy}");
        drop(repo);
        close_fixture(app, root);
    }
}

#[test]
fn discard_then_typing_undo_redo_and_save_keep_correct_checkpoints() {
    let (root, file, mut app) = make_git_app_with_file("hunk-edit-save", "old\n", "new\n");
    app.git_discard_hunk(&file, 0);
    app.editor_mut().unwrap().insert_char('x');
    for (redo, expected, dirty) in [
        (false, "old\n", false),
        (false, "new\n", true),
        (true, "old\n", false),
        (true, "xold\n", true),
    ] {
        let editor = app.editor_mut().unwrap();
        if redo {
            editor.redo();
        } else {
            editor.undo();
        }
        assert_eq!(editor.rope.to_string(), expected);
        assert_eq!(editor.modified, dirty);
    }
    app.editor_mut()
        .unwrap()
        .save_file_with_options(false, false)
        .unwrap();
    for (redo, expected, dirty) in [
        (false, "old\n", true),
        (false, "new\n", true),
        (true, "old\n", true),
        (true, "xold\n", false),
    ] {
        let editor = app.editor_mut().unwrap();
        if redo {
            editor.redo();
        } else {
            editor.undo();
        }
        assert_eq!(editor.rope.to_string(), expected);
        assert_eq!(editor.modified, dirty);
    }
    assert_eq!(git_stdout(&root, &["show", ":file.txt"]), "old\n");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "xold\n");
    close_fixture(app, root);
}
