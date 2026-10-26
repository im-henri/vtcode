use super::*;
use git2::{Repository, Signature};

fn commit(repo: &Repository, message: &str) -> git2::Oid {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.update_all(["*"], None).unwrap();
    index.write().unwrap();
    let id = index.write_tree().unwrap();
    let tree = repo.find_tree(id).unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let sig = Signature::now("Ada Lovelace", "ada@example.test").unwrap();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        message,
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn commit_inspector_loads_complete_metadata_stats_and_root_diff() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello\nworld\n").unwrap();
    let id = commit(
        &repo,
        "Introduce greeting\n\nKeep the complete message.\n\nSigned-off-by: Ada",
    );
    repo.tag_lightweight("v1", &repo.find_object(id, None).unwrap(), false)
        .unwrap();
    let data = load(dir.path(), &id.to_string()).unwrap();
    assert_eq!(data.hash, id.to_string());
    assert!(data.message.contains("Signed-off-by: Ada"));
    assert_eq!(data.author, "Ada Lovelace");
    assert_eq!(data.email, "ada@example.test");
    assert!(!data.date.is_empty());
    assert!(data.parents.is_empty());
    assert!(data.references.iter().any(|r| r.name == "tag: v1"));
    assert_eq!(
        (
            data.files[0].status,
            data.files[0].added,
            data.files[0].deleted
        ),
        ('A', 2, 0)
    );
    let diff = load_diff(dir.path(), &id.to_string(), &data.files[0], u64::MAX).unwrap();
    assert_eq!(diff.target.to_string(), "hello\nworld\n");
    assert_eq!(diff.prepared.base_content.as_ref(), "");
    assert!(
        diff.prepared
            .diff_lines
            .iter()
            .any(|(kind, text)| *kind == '+' && text == "hello")
    );
}

#[test]
fn commit_inspector_rename_diff_uses_original_path_even_from_nested_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    std::fs::write(
        dir.path().join("before.txt"),
        "first\nsecond\nthird\nfourth\n",
    )
    .unwrap();
    commit(&repo, "Initial");
    std::fs::rename(dir.path().join("before.txt"), dir.path().join("after.txt")).unwrap();
    let id = commit(&repo, "Rename greeting");
    let data = load(dir.path(), &id.to_string()).unwrap();
    assert_eq!(data.files.len(), 1);
    assert_eq!(data.files[0].status, 'R');
    assert_eq!(data.files[0].old_path.as_deref(), Some("before.txt"));
    let diff = load_diff(dir.path(), &id.to_string(), &data.files[0], u64::MAX).unwrap();
    assert_eq!(diff.target.to_string(), diff.prepared.base_content.as_ref());
    assert!(diff.prepared.render_cache.hunks.is_empty());
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let mut app = App::test_new();
    app.explorer.root = dir.path().join("nested");
    app.commit_details.data = data;
    app.open_source_control_commit_details_file_diff(0);
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("first\nsecond\nthird\nfourth\n")
    );
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "first\nsecond\nthird\nfourth\n"
    );
    // Git expands Windows short-name aliases in the repository workdir.
    assert_eq!(
        app.diff_target_path.as_ref().unwrap().canonicalize().unwrap(),
        dir.path().join("after.txt").canonicalize().unwrap()
    );
    assert!(app.editor().unwrap().read_only);
}

#[test]
fn commit_inspector_open_diff_creates_a_persistent_snapshot_diff_tab() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let path = dir.path().join("history.rs");
    let before = (0..100)
        .map(|line| format!("let value_{line} = {line};\n"))
        .collect::<String>();
    let after = before.replace("value_80", "updated_80");
    std::fs::write(&path, &before).unwrap();
    commit(&repo, "Initial");
    std::fs::write(&path, &after).unwrap();
    let hash = commit(&repo, "Change a distant line");
    std::fs::write(&path, "// unrelated working tree changes\n").unwrap();

    for mode in [
        crate::app::DiffMode::SideBySide,
        crate::app::DiffMode::Inline,
    ] {
        let mut app = App::test_new();
        app.explorer.root = dir.path().to_path_buf();
        app.preferred_diff_mode = mode;
        app.commit_details.active = true;
        app.commit_details.data = load(dir.path(), &hash.to_string()).unwrap();
        app.commit_details.diff_file = Some(0);
        let mut tab = Tab::new_file(app.new_editor());
        tab.kind = TabKind::CommitDetails;
        app.tabs.push(tab);
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        app.install_commit_diff(
            load_diff(
                dir.path(),
                &hash.to_string(),
                &app.commit_details.data.files[0],
                u64::MAX,
            )
            .unwrap(),
        );
        app.set_commit_diff_context(true, 3);
        let preview = app.diff_render_cache.clone();
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let button = app.commit_details.layout.as_ref().unwrap().buttons[4];
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: button.x + 1,
                row: button.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        let diff_tab = app.active_tab;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(
            matches!(app.tabs[diff_tab].kind, TabKind::Diff(_)),
            "Open Diff must create a diff tab"
        );
        assert!(
            app.should_render_diff_for_active_editor(),
            "the comparison must survive the first UI frame"
        );
        assert!(app.diff_mode == mode);
        assert_eq!(app.compare_base_content.as_deref(), Some(before.as_str()));
        assert_eq!(app.editor().unwrap().rope.to_string(), after);
        assert_eq!(app.editor().unwrap().cursor_line, 80);
        assert!(app.editor().unwrap().read_only);
        assert_eq!(
            app.compare_diff_base_revision,
            Some(crate::app::DiffBaseRevision::Snapshot)
        );
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            rendered.contains("value_80") && rendered.contains("updated_80"),
            "the diff must paint both revisions"
        );
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(),
            if mode == crate::app::DiffMode::Inline {
                "commit-open-diff-inline"
            } else {
                "commit-open-diff-side-by-side"
            },
        );

        let mut code = Tab::new_file(app.new_editor());
        code.editor.file_path = Some(path.clone());
        code.editor.rope = ropey::Rope::from_str("// unsaved editor contents\n");
        code.editor.modified = true;
        app.tabs.push(code);
        app.active_tab = app.tabs.len() - 1;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(!app.should_render_diff_for_active_editor());
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "// unsaved editor contents\n"
        );
        app.active_tab = 0;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&preview, &app.diff_render_cache));
        assert_eq!(app.diff_render_cache.context_lines, Some(3));
        app.active_tab = diff_tab;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(app.should_render_diff_for_active_editor());
        assert_eq!(app.compare_base_content.as_deref(), Some(before.as_str()));
        assert_eq!(app.editor().unwrap().rope.to_string(), after);
        assert_eq!(app.diff_render_cache.context_lines, None);
        app.close_commit_details_tab();
        app.active_tab = app
            .tabs
            .iter()
            .position(|tab| matches!(tab.kind, TabKind::Diff(_)))
            .unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(app.should_render_diff_for_active_editor());
        assert_eq!(app.compare_base_content.as_deref(), Some(before.as_str()));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "// unrelated working tree changes\n"
        );
    }
}

#[test]
fn commit_inspector_binary_and_deleted_files_have_honest_previews() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("gone.txt"), "removed\n").unwrap();
    commit(&repo, "Initial");
    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
    std::fs::write(dir.path().join("image.bin"), b"\0\x01\x02\0\xff").unwrap();
    let id = commit(&repo, "Delete and add binary");
    let data = load(dir.path(), &id.to_string()).unwrap();
    let deleted = data.files.iter().find(|f| f.status == 'D').unwrap();
    assert_eq!(deleted.deleted, 1);
    let diff = load_diff(dir.path(), &id.to_string(), deleted, u64::MAX).unwrap();
    assert_eq!(diff.prepared.base_content.as_ref(), "removed\n");
    assert_eq!(diff.target.to_string(), "");
    assert_eq!(diff.prepared.render_cache.hunks.len(), 1);
    let binary = data.files.iter().find(|f| f.path == "image.bin").unwrap();
    assert!(binary.binary);
    assert!(
        load_diff(dir.path(), &id.to_string(), binary, u64::MAX)
            .err()
            .unwrap()
            .contains("binary")
    );
}

#[test]
fn commit_inspector_rejects_stale_commit_and_patch_results() {
    let mut app = App::test_new();
    app.commit_details.active = true;
    app.commit_details.generation = 12;
    app.commit_details.data.hash = "current".into();
    assert!(!app.apply_commit_details(DetailsResult {
        root: app.explorer.root.clone(),
        generation: 11,
        result: Ok(CommitData::default())
    }));
    assert!(!app.apply_commit_details(DetailsResult {
        root: PathBuf::from("other-workspace"),
        generation: 12,
        result: Ok(CommitData::default())
    }));
    app.commit_details.diff_file = Some(3);
    assert!(!app.apply_commit_diff(DiffResult {
        root: app.explorer.root.clone(),
        generation: 12,
        file: 2,
        result: Err("stale result".into())
    }));
    app.commit_details.active = false;
    assert!(!app.apply_commit_details(DetailsResult {
        root: app.explorer.root.clone(),
        generation: 12,
        result: Ok(CommitData::default())
    }));
    assert_eq!(app.commit_details.data.hash, "current");
}

#[test]
fn commit_inspector_filter_matches_unicode_and_original_paths() {
    let mut details = CommitDetails::default();
    details.data.files = vec![
        CommitFile {
            status: 'R',
            path: "src/東京.rs".into(),
            old_path: Some("legacy/OldName.rs".into()),
            ..CommitFile::default()
        },
        CommitFile::new('M', "src/other.rs"),
    ];
    details.query = "OLDNAME".into();
    assert_eq!(details.filtered_files(), vec![0]);
    details.query = "東京".into();
    assert_eq!(details.selected_file(), Some(0));
    details.query = "absent".into();
    details.selected = 99;
    assert_eq!(details.selected_file(), None);
}

#[test]
fn commit_inspector_diff_treats_git_pathspec_characters_as_literal_filenames() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("[notes].txt"), "literal name\n").unwrap();
    let id = commit(&repo, "Add file with brackets");
    let data = load(dir.path(), &id.to_string()).unwrap();
    let diff = load_diff(dir.path(), &id.to_string(), &data.files[0], u64::MAX).unwrap();
    assert_eq!(diff.target.to_string(), "literal name\n");
}

#[test]
fn commit_inspector_diff_preserves_complete_revisions_and_line_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("lines.txt"), "first\nold\nlast").unwrap();
    commit(&repo, "Initial");
    std::fs::write(dir.path().join("lines.txt"), "first\nnew\nextra\nlast").unwrap();
    let id = commit(&repo, "Update lines");
    let data = load(dir.path(), &id.to_string()).unwrap();
    let diff = load_diff(dir.path(), &id.to_string(), &data.files[0], u64::MAX).unwrap();
    assert_eq!(diff.prepared.base_content.as_ref(), "first\nold\nlast");
    assert_eq!(diff.target.to_string(), "first\nnew\nextra\nlast");
    let rows = &diff.prepared.render_cache.rows;
    assert!(
        rows.iter()
            .any(|row| row.left_text == "old" && row.left_line == Some(1))
    );
    assert!(
        rows.iter()
            .any(|row| row.right_text == "extra" && row.right_line == Some(2))
    );
}

#[test]
fn commit_inspector_diff_keeps_distant_context_and_checks_both_revision_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let base = (0..2500).map(|i| format!("line {i}\n")).collect::<String>();
    std::fs::write(dir.path().join("lines.txt"), &base).unwrap();
    commit(&repo, "Large original");
    let target = base.replace("line 2400\n", "updated line 2400\n");
    std::fs::write(dir.path().join("lines.txt"), &target).unwrap();
    let id = commit(&repo, "Distant change");
    let data = load(dir.path(), &id.to_string()).unwrap();
    let diff = load_diff(dir.path(), &id.to_string(), &data.files[0], u64::MAX).unwrap();
    assert_eq!(diff.target.to_string(), target);
    assert_eq!(diff.prepared.base_content.as_ref(), base);
    assert_eq!(diff.prepared.render_cache.hunks[0].target_start, 2400);
    assert!(
        load_diff(dir.path(), &id.to_string(), &data.files[0], 128)
            .err()
            .unwrap()
            .contains("large file")
    );
    std::fs::write(dir.path().join("lines.txt"), "short replacement\n").unwrap();
    let id = commit(&repo, "Small target, large original");
    let data = load(dir.path(), &id.to_string()).unwrap();
    assert!(
        load_diff(dir.path(), &id.to_string(), &data.files[0], 128)
            .err()
            .unwrap()
            .contains("large file")
    );
}

#[test]
fn commit_inspector_card_action_opens_the_pinned_historical_diff() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("hello.txt"), "committed content\n").unwrap();
    let id = commit(&repo, "Add greeting");
    std::fs::write(dir.path().join("hello.txt"), "working tree content\n").unwrap();
    let mut app = App::test_new();
    app.explorer.root = dir.path().to_path_buf();
    app.commit_details.active = true;
    app.commit_details.data = load(dir.path(), &id.to_string()).unwrap();
    app.commit_details.diff_file = Some(0);
    let mut tab = Tab::new_file(crate::editor::Editor::new());
    tab.kind = TabKind::CommitDetails;
    tab.editor.read_only = true;
    app.tabs.push(tab);
    app.commit_details.query = "hello".into();
    app.commit_details.query_cursor = 5;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 34)).unwrap();
    terminal.draw(|frame| crate::ui_commit_details::draw(frame, &mut app)).unwrap();
    let open = app.commit_details.review.visible[0].open;
    assert_eq!(app.tabs.len(), 1);
    crate::ui_commit_details::click(&mut app, open.x + 1, open.y);
    assert!(app.commit_details.active);
    assert_eq!(app.tabs.len(), 2);
    assert!(
        app.tabs
            .iter()
            .any(|tab| tab.kind == TabKind::CommitDetails)
    );
    assert_eq!(app.commit_details.query, "hello");
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "committed content\n"
    );
    assert!(app.editor().unwrap().read_only);
}

#[test]
fn commit_inspector_reuses_its_tab_preserves_sizes_and_accepts_results_while_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join("hello.txt"), "first\n").unwrap();
    let first = commit(&repo, "First commit");
    std::fs::write(dir.path().join("hello.txt"), "second\n").unwrap();
    let second = commit(&repo, "Second commit");
    let mut app = App::test_new();
    app.explorer.root = dir.path().to_path_buf();
    app.git_log = [second, first]
        .iter()
        .map(|id| crate::app::GitCommitInfo {
            hash: id.to_string()[..7].into(),
            full_hash: id.to_string(),
            graph: "●".into(),
            message: "Commit subject".into(),
            author: "Ada".into(),
            time_ago: "now".into(),
            branches: vec![],
        })
        .collect();
    app.tabs.push(Tab::new_file(crate::editor::Editor::new()));
    app.tab_tooltip_tab = Some(0);
    app.tab_tooltip_hover_start = Some(std::time::Instant::now());
    app.tab_tooltip_shown = true;
    app.open_source_control_commit_details(0);
    assert!(app.tab_tooltip_tab.is_none());
    assert!(app.tab_tooltip_hover_start.is_none());
    assert!(!app.tab_tooltip_shown);
    assert_eq!(app.tabs.len(), 2);
    assert!(app.is_commit_details_tab());
    let generation = app.commit_details.generation;
    let panes = PaneSizes {
        overview: 470,
    };
    app.commit_details.panes = panes;
    app.commit_details.query = "hello".into();
    for shown in [false, true] {
        app.tab_tooltip_tab = Some(0);
        app.tab_tooltip_hover_start = Some(std::time::Instant::now());
        app.tab_tooltip_shown = shown;
        app.open_source_control_commit_details(0);
        assert!(app.tab_tooltip_tab.is_none());
        assert!(app.tab_tooltip_hover_start.is_none());
        assert!(!app.tab_tooltip_shown);
    }
    assert_eq!(app.commit_details.generation, generation);
    assert_eq!(app.commit_details.query, "hello");
    app.active_tab = 0;
    assert!(app.apply_commit_details(DetailsResult {
        root: dir.path().to_path_buf(),
        generation,
        result: Ok(load(dir.path(), &second.to_string()).unwrap())
    }));
    assert_eq!(
        app.active_tab, 0,
        "background metadata must not activate the tab"
    );
    app.open_source_control_commit_details(1);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.commit_details.data.hash, first.to_string());
    assert_eq!(app.commit_details.panes, panes);
    let generation = app.commit_details.generation;
    app.force_close_tab(app.active_tab);
    assert!(!app.commit_details.active);
    assert!(!app.apply_commit_details(DetailsResult {
        root: dir.path().to_path_buf(),
        generation,
        result: Ok(CommitData::default())
    }));
    app.open_source_control_commit_details(0);
    assert_eq!(app.commit_details.panes, panes);
    assert!(app.commit_details.generation > generation);
}

#[test]
fn commit_inspector_native_diff_results_preserve_other_editors_and_diff_tabs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let path = dir.path().join("hello.rs");
    std::fs::write(&path, "fn first() {}\n").unwrap();
    commit(&repo, "First commit");
    std::fs::write(&path, "fn second() {}\n").unwrap();
    let id = commit(&repo, "Second commit");
    std::fs::write(&path, "fn working_tree() {}\n").unwrap();
    let mut app = App::test_new();
    app.explorer.root = dir.path().to_path_buf();
    app.tabs.push(Tab::new_file(crate::editor::Editor::new()));
    let mut tab = Tab::new_file(crate::editor::Editor::new());
    tab.kind = TabKind::CommitDetails;
    app.tabs.push(tab);
    app.commit_details.active = true;
    app.commit_details.data = load(dir.path(), &id.to_string()).unwrap();
    app.request_commit_diff();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.commit_details.diff_loading
    });
    assert_eq!(app.active_tab, 0, "a completed diff must not steal focus");
    assert!(app.editor().unwrap().rope.to_string().is_empty());
    assert!(!app.should_render_diff_for_active_editor());
    app.active_tab = 1;
    app.sync_diff_tab();
    assert!(app.should_render_diff_for_active_editor());
    assert_eq!(app.editor().unwrap().rope.to_string(), "fn second() {}\n");
    assert_eq!(
        app.editor()
            .unwrap()
            .file_path
            .as_ref()
            .unwrap()
            .canonicalize()
            .unwrap(),
        path.canonicalize().unwrap()
    );
    assert_eq!(app.compare_base_content.as_deref(), Some("fn first() {}\n"));
    let commit_cache = app.diff_render_cache.clone();
    app.background
        .sender
        .send(BackgroundResult::FileReloaded {
            targets: app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs))
                .map(|tab| (tab.editor.navigation_id, tab.editor.content_version)).collect(),
            workspace_generation: 0,
            path: path.clone(),
            result: crate::editor::load_text_file(&path).map_err(|error| error.to_string()),
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "fn second() {}\n",
        "working-tree reloads must not replace a commit snapshot"
    );
    assert!(app.editor().unwrap().read_only);
    app.diff_mode = crate::app::DiffMode::Inline;
    app.diff_left_horizontal_scroll = 11;
    app.editor_mut().unwrap().set_cursor(0, 5);
    app.open_source_control_diff_tab(&path, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    assert_eq!(app.active_tab, 2);
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "fn working_tree() {}\n"
    );
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("fn second() {}\n")
    );
    let working_cache = app.diff_render_cache.clone();
    app.active_tab = 1;
    app.sync_diff_tab();
    assert!(std::sync::Arc::ptr_eq(
        &commit_cache,
        &app.diff_render_cache
    ));
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    assert_eq!(app.diff_left_horizontal_scroll, 11);
    assert_eq!(app.editor().unwrap().cursor_col, 5);
    app.active_tab = 2;
    app.sync_diff_tab();
    let generation = app.commit_details.generation;
    assert!(app.apply_commit_diff(DiffResult {
        root: dir.path().to_path_buf(),
        generation,
        file: 0,
        result: load_diff(
            dir.path(),
            &id.to_string(),
            &app.commit_details.data.files[0],
            u64::MAX
        ),
    }));
    assert!(std::sync::Arc::ptr_eq(
        &working_cache,
        &app.diff_render_cache
    ));
    assert_eq!(app.active_tab, 2);
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "fn working_tree() {}\n"
    );
    app.active_tab = 1;
    app.sync_diff_tab();
    assert_eq!(app.editor().unwrap().rope.to_string(), "fn second() {}\n");
    app.force_close_tab(1);
    assert!(!app.commit_details.active);
    assert_ne!(app.diff_tabs.owner, Some(crate::diff_tabs::COMMIT_DIFF_ID));
    assert!(!app.apply_commit_diff(DiffResult {
        root: dir.path().to_path_buf(),
        generation,
        file: 0,
        result: Err("late result".into()),
    }));
}
