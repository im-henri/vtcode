use super::{commit_fixture_files, init_git_fixture, make_clean_file_tab};
use crate::app::{App, DiffMode, Tab, TabKind};
use crate::editor::Editor;
use ratatui::{Terminal, backend::TestBackend};
use std::{path::PathBuf, time::Duration};

const BASE: &str = "fn demo() {\n    let message = \"before\";\n    // comment\n}\n";
const INDEX: &str = "fn demo() {\n    let message = \"staged\";\n    // comment\n}\n";
const WORKING: &str = "fn demo() {\n    let message = \"working\";\n    // comment\n}\n";

fn fixture() -> (tempfile::TempDir, PathBuf, App) {
    let (dir, path, mut app) = fixture_with_source(BASE, INDEX, WORKING);
    app.execute_editor_command(&crate::app_menu::language_command_id("Rust"));
    (dir, path, app)
}

fn fixture_with_source(
    base: &str,
    index_text: &str,
    working: &str,
) -> (tempfile::TempDir, PathBuf, App) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script");
    std::fs::write(&path, base).unwrap();
    init_git_fixture(dir.path());
    commit_fixture_files(dir.path(), &["script"], "initial");
    std::fs::write(&path, index_text).unwrap();
    let repo = git2::Repository::open(dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("script")).unwrap();
    index.write().unwrap();
    std::fs::write(&path, working).unwrap();
    let mut app = App::new(dir.path().to_owned());
    app.tabs = vec![make_clean_file_tab(&path, working)];
    app.active_tab = 0;
    (dir, path, app)
}

fn settle_diff(app: &mut App) {
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
            && !app.diff_tabs.refresh_running
            && app.should_render_diff_for_active_editor()
    });
}

fn assert_syntax_in_both_layouts(app: &mut App) {
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    app.syntax_theme = "dark-plus".into();
    app.editor_mut().unwrap().scroll_offset = 0;
    app.diff_mode = DiffMode::Inline;
    let mut terminal = Terminal::new(TestBackend::new(140, 12)).unwrap();
    terminal
        .draw(|frame| crate::ui_diff_display::draw_inline_diff(frame, app, frame.area()))
        .unwrap();
    app.refresh_visible_highlights_async();
    app.wait_for_background(Duration::from_secs(5), |app| {
        let editor = app.editor().unwrap();
        app.diff_render_cache.rows.iter().all(|row| {
            [
                (row.left_line, row.left_highlight_hash()),
                (row.right_line, row.right_highlight_hash()),
            ]
            .into_iter()
            .all(|(line, hash)| {
                line.is_none_or(|line| {
                    app.highlighter
                        .highlighted_fragment_by_hash_if_cached(
                            hash,
                            line,
                            editor.syntax_file_path(),
                            editor.syntax_language_token(),
                            &app.syntax_theme,
                        )
                        .is_some()
                })
            })
        })
    });
    for mode in [DiffMode::Inline, DiffMode::SideBySide] {
        app.diff_mode = mode;
        terminal
            .draw(|frame| match mode {
                DiffMode::Inline => {
                    crate::ui_diff_display::draw_inline_diff(frame, app, frame.area())
                }
                _ => crate::ui_diff_display::draw_side_by_side_diff(frame, app, frame.area()),
            })
            .unwrap();
        let mut areas = vec![(
            app.code_area.unwrap(),
            if mode == DiffMode::SideBySide { 2 } else { 0 },
        )];
        if mode == DiffMode::SideBySide {
            areas.push((app.diff_left_code_area.unwrap(), 0));
        }
        for (area, prefix) in areas {
            let buffer = terminal.backend().buffer();
            let keyword = &buffer[(area.x + prefix, area.y)];
            let name = &buffer[(area.x + prefix + 3, area.y)];
            assert_eq!(keyword.symbol(), "f");
            assert_eq!(name.symbol(), "d");
            assert_ne!(
                keyword.fg, name.fg,
                "the selected language must color each diff pane"
            );
        }
    }
}

#[test]
fn extensionless_source_control_diff_inherits_language_in_both_layouts_and_panes() {
    for staged in [false, true] {
        for split in [false, true] {
            let (_dir, path, mut app) = fixture();
            if split {
                app.tabs.push(Tab::new_file(Editor::new()));
                app.create_split_group(1);
            }
            app.open_source_control_diff_tab(&path, false, staged);
            settle_diff(&mut app);
            assert_eq!(app.editor().unwrap().syntax_language_token(), Some("Rust"));
            assert_syntax_in_both_layouts(&mut app);
        }
    }
}

fn assert_python_diff_colors(app: &mut App, source_line: usize, colored: [bool; 2]) {
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    app.syntax_theme = "dark-plus".into();
    app.editor_mut().unwrap().set_cursor(source_line, 0);
    app.editor_mut().unwrap().scroll_offset = source_line;
    let mut terminal = Terminal::new(TestBackend::new(140, 12)).unwrap();
    for mode in [DiffMode::Inline, DiffMode::SideBySide] {
        app.diff_mode = mode;
        let draw = |frame: &mut ratatui::Frame, app: &mut App| match mode {
            DiffMode::Inline => crate::ui_diff_display::draw_inline_diff(frame, app, frame.area()),
            _ => crate::ui_diff_display::draw_side_by_side_diff(frame, app, frame.area()),
        };
        terminal.draw(|frame| draw(frame, app)).unwrap();
        app.refresh_visible_highlights_async();
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let mut areas = vec![(
            app.code_area.unwrap(),
            if mode == DiffMode::SideBySide { 2 } else { 0 },
            colored[1],
        )];
        if mode == DiffMode::SideBySide {
            areas.push((app.diff_left_code_area.unwrap(), 0, colored[0]));
        }
        for (area, prefix, colored) in areas {
            let buffer = terminal.backend().buffer();
            let y = (area.y..area.bottom())
                .find(|&y| {
                    (0..9)
                        .map(|x| buffer[(area.x + prefix + x, y)].symbol())
                        .collect::<String>()
                        == "def demo("
                })
                .expect("the Python function is visible");
            let keyword = &buffer[(area.x + prefix, y)];
            let name = &buffer[(area.x + prefix + 4, y)];
            assert_eq!(keyword.symbol(), "d");
            assert_eq!(name.symbol(), "d");
            assert_eq!(
                keyword.fg != name.fg,
                colored,
                "def must use the document's Python grammar"
            );
        }
    }
}

#[test]
fn extensionless_diff_detects_python_from_shebang_before_distant_first_render() {
    let prefix = format!("#!/usr/bin/env python3\n{}", "# header\n".repeat(2879));
    for crlf in [false, true] {
        let source = |value| {
            let text = format!(
                "{prefix}def demo():\n    return {value}\n{}",
                "# footer\n".repeat(100)
            );
            if crlf {
                text.replace('\n', "\r\n")
            } else {
                text
            }
        };
        let (_dir, path, mut app) = fixture_with_source(&source(1), &source(2), &source(3));
        // Open directly from Source Control without a live file or a language override.
        app.tabs.clear();
        for staged in [false, true] {
            app.open_source_control_diff_tab(&path, false, staged);
            settle_diff(&mut app);
            assert_eq!(app.editor().unwrap().language_name(), "Python");
            assert_eq!(app.editor().unwrap().syntax_language_token(), None);
            assert_python_diff_colors(&mut app, 2880, [true, true]);
        }
    }
}

fn python_source(header: &str, value: u32) -> String {
    format!(
        "{header}\ndef demo():\n    return {value}\n{}",
        "# footer\n".repeat(30)
    )
}

#[test]
fn inferred_diff_language_respects_overrides_and_associations_and_recovers_on_auto_detect() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into());
    let path = dir.path().join("script");
    let before = python_source("#!/usr/bin/env python3", 1);
    let after = python_source("#!/usr/bin/env python3", 2);
    app.language_associations
        .insert("script".into(), "Plain Text".into());
    app.open_acp_diff(&vtcode::acp::protocol::Diff::new(path, &after).old_text(&before));
    assert_python_diff_colors(&mut app, 1, [false, false]);
    app.execute_editor_command(&crate::app_menu::language_command_id("Python"));
    assert_python_diff_colors(&mut app, 1, [true, true]);
    app.execute_editor_command("lang_auto");
    assert_python_diff_colors(&mut app, 1, [false, false]);
    app.editor_mut().unwrap().language_associations.clear();
    app.execute_editor_command("lang_auto");
    assert_eq!(app.editor().unwrap().language_name(), "Python");
    assert_python_diff_colors(&mut app, 1, [true, true]);
    app.execute_editor_command(&crate::app_menu::language_command_id("Plain Text"));
    assert_python_diff_colors(&mut app, 1, [false, false]);
    app.execute_editor_command("lang_auto");
    assert_python_diff_colors(&mut app, 1, [true, true]);
}

#[test]
fn diff_revisions_detect_their_own_headers_and_ignore_late_colors_from_another_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into());
    let path = dir.path().join("script");
    let python = "#!/usr/bin/env python3";
    let plain = "# no language selected";
    app.open_acp_diff(
        &vtcode::acp::protocol::Diff::new(&path, python_source(python, 2))
            .old_text(python_source(plain, 1)),
    );
    assert_python_diff_colors(&mut app, 1, [false, true]);
    let row = app
        .diff_render_cache
        .rows
        .iter()
        .find(|row| row.right_line == Some(1))
        .unwrap();
    let old_hash = row.right_highlight_hash();
    let old_spans = app
        .highlighter
        .highlighted_fragment_by_hash_if_cached(old_hash, 1, Some(&path), None, &app.syntax_theme)
        .unwrap()
        .as_ref()
        .clone();

    app.open_acp_diff(
        &vtcode::acp::protocol::Diff::new(&path, python_source(plain, 2))
            .old_text(python_source(python, 1)),
    );
    assert_python_diff_colors(&mut app, 1, [true, false]);
    // A delayed result for the same path, line number and row text must not
    // repaint the target using the other revision's language.
    app.background
        .sender
        .send(crate::background::BackgroundResult::HighlightFragments {
            registry_generation: app.highlighter.registry_generation(),
            file_path: path,
            theme_key: app.syntax_theme.clone(),
            language: None,
            entries: vec![(1, old_hash, old_spans)],
        })
        .unwrap();
    app.poll_background_results();
    assert_python_diff_colors(&mut app, 1, [true, false]);
}

#[test]
fn compact_commit_diff_detects_modeline_even_when_first_line_is_hidden() {
    let (_dir, path, mut app) = fixture_with_source("", "", "");
    let header = format!("# -*- mode: python -*-\n{}", "# header\n".repeat(80));
    let before = format!("{header}{}", python_source("# function", 1));
    let after = format!("{header}{}", python_source("# function", 2));
    let mut snapshot = commit_diff(path);
    let lines = crate::app::compute_file_diff(&before, &after);
    snapshot.target = ropey::Rope::from_str(&after);
    snapshot.prepared.base_content = before.into();
    snapshot.prepared.render_cache = crate::app::DiffRenderCache::new(&lines);
    snapshot.prepared.target_line_markers =
        crate::app_git_display::target_line_markers_from_compare_diff(
            &lines,
            snapshot.target.len_lines(),
        );
    snapshot.prepared.diff_lines = lines;
    let mut tab = Tab::new_file(app.new_editor());
    tab.kind = TabKind::CommitDetails;
    app.tabs.push(tab);
    app.active_tab = app.tabs.len() - 1;
    app.install_commit_diff(snapshot);
    app.set_commit_diff_context(true, 2);
    assert!(
        app.diff_render_cache
            .rows
            .iter()
            .all(|row| row.right_line != Some(0))
    );
    assert_python_diff_colors(&mut app, 82, [true, true]);
}

#[test]
fn diff_language_detection_headers_are_bounded_and_preserved_without_target_text() {
    let header = format!("#!/usr/bin/env python3 {}\r", "λ".repeat(10_000));
    let cache = crate::app::DiffRenderCache::new(&[
        ('-', header),
        ('-', "def demo():".into()),
        ('-', "    return 1".into()),
    ]);
    assert_eq!(
        cache.syntax_first_lines[0].chars().count(),
        crate::highlight::MAX_SYNTAX_DETECTION_CHARS + 1
    );
    assert_eq!(cache.syntax_first_lines[1], "\n");
    assert_eq!(
        cache.with_context(0).syntax_first_lines,
        cache.syntax_first_lines
    );

    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(dir.path().into());
    let path = dir.path().join("script");
    app.open_acp_diff(
        &vtcode::acp::protocol::Diff::new(&path, "")
            .old_text(python_source("#!/usr/bin/env python3", 1)),
    );
    app.syntax_theme = "dark-plus".into();
    app.refresh_visible_highlights_async();
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    let row = &app.diff_render_cache.rows[1];
    let spans = app
        .highlighter
        .highlighted_fragment_by_hash_if_cached(
            row.left_highlight_hash(),
            1,
            Some(&path),
            None,
            &app.syntax_theme,
        )
        .unwrap();
    assert!(spans.iter().any(
        |(style, text)| text == "def" && style.foreground != spans.last().unwrap().0.foreground
    ));
}

#[test]
fn language_changes_and_auto_detection_update_existing_diffs_without_changing_text() {
    let (_dir, path, mut app) = fixture();
    app.open_source_control_diff_tab(&path, false, true);
    settle_diff(&mut app);
    app.create_split_group(app.active_tab);
    let diff_group = app.active_group;
    app.switch_group(0);
    app.duplicate_tab(0);
    let unrelated = path.with_file_name("other");
    app.tabs.push(make_clean_file_tab(&unrelated, WORKING));
    app.tabs.last_mut().unwrap().editor.language_override = Some("Python".into());
    app.active_tab = 0;
    for language in [Some("Python"), Some("Rust"), None] {
        app.execute_editor_command(
            &language.map_or_else(|| "lang_auto".into(), crate::app_menu::language_command_id),
        );
        app.sync_document_views(false);
        for tab in app
            .tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| &group.tabs))
        {
            if tab.editor.file_path.as_ref() == Some(&path) {
                assert_eq!(tab.editor.language_override.as_deref(), language);
                assert_eq!(
                    tab.editor.rope,
                    if matches!(tab.kind, TabKind::Diff(_)) {
                        INDEX
                    } else {
                        WORKING
                    }
                );
            } else if tab.editor.file_path.as_ref() == Some(&unrelated) {
                assert_eq!(tab.editor.language_override.as_deref(), Some("Python"));
            }
        }
    }
    app.switch_group(diff_group);
    app.execute_editor_command(&crate::app_menu::language_command_id("Rust"));
    assert_syntax_in_both_layouts(&mut app);
    app.switch_group(0);
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Rust")
    );
}

fn commit_diff(path: PathBuf) -> crate::commit_details::CommitDiff {
    let lines = crate::app::compute_file_diff(BASE, INDEX);
    let target = ropey::Rope::from_str(INDEX);
    crate::commit_details::CommitDiff {
        path,
        labels: crate::app::DiffLabels::new("Parent", "Commit"),
        prepared: crate::background::PreparedDiffRefresh {
            base_content: BASE.into(),
            render_cache: crate::app::DiffRenderCache::new(&lines),
            target_line_markers: crate::app_git_display::target_line_markers_from_compare_diff(
                &lines,
                target.len_lines(),
            ),
            diff_lines: lines,
        },
        target,
    }
}

#[test]
fn commit_details_keep_selected_language_on_refresh_but_not_on_other_files() {
    let (_dir, path, mut app) = fixture();
    let mut tab = Tab::new_file(app.new_editor());
    tab.kind = TabKind::CommitDetails;
    app.tabs.push(tab);
    app.active_tab = 1;
    app.install_commit_diff(commit_diff(path.clone()));
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Rust")
    );
    assert_syntax_in_both_layouts(&mut app);
    app.execute_editor_command(&crate::app_menu::language_command_id("Plain Text"));
    app.install_commit_diff(commit_diff(path.clone()));
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Plain Text")
    );
    app.install_commit_diff(commit_diff(path.with_file_name("other")));
    assert_eq!(app.editor().unwrap().language_override, None);
    app.install_commit_diff(commit_diff(path));
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Plain Text")
    );
}

#[test]
fn extensionless_diff_uses_file_associations_and_allows_an_explicit_override() {
    let (_dir, path, mut app) = fixture();
    app.execute_editor_command("lang_auto");
    app.language_associations
        .insert("script".into(), "Rust".into());
    app.open_source_control_diff_tab(&path, false, false);
    settle_diff(&mut app);
    assert_eq!(app.editor().unwrap().language_override, None);
    assert_syntax_in_both_layouts(&mut app);
    app.execute_editor_command(&crate::app_menu::language_command_id("Plain Text"));
    assert_eq!(
        app.editor().unwrap().syntax_language_token(),
        Some("Plain Text")
    );
    app.execute_editor_command("lang_auto");
    assert_eq!(app.editor().unwrap().syntax_language_token(), Some("Rust"));
    assert_syntax_in_both_layouts(&mut app);
}

#[test]
fn agent_and_commit_snapshot_diffs_inherit_extensionless_file_language() {
    let (_dir, path, mut app) = fixture();
    app.open_acp_diff(&vtcode::acp::protocol::Diff::new(path.clone(), INDEX).old_text(BASE));
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Rust")
    );
    assert_syntax_in_both_layouts(&mut app);
    let oid = commit_fixture_files(path.parent().unwrap(), &["script"], "update");
    app.commit_details.data.hash = oid.to_string();
    app.commit_details.data.files = vec![crate::commit_details::CommitFile::new('M', "script")];
    app.open_source_control_commit_details_file_diff(0);
    let diff_path = app.editor().unwrap().file_path.as_ref().unwrap();
    assert_eq!(diff_path, &path);
    assert!(App::same_open_path(diff_path, &path));
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Rust")
    );
    assert_syntax_in_both_layouts(&mut app);
}

#[test]
fn diff_rendering_uses_the_same_syntax_path_as_the_highlight_worker() {
    let (_dir, path, mut app) = fixture();
    app.open_source_control_diff_tab(&path, false, true);
    settle_diff(&mut app);
    // Historical editors give their syntax cache a separate identity so that
    // highlighting an old revision cannot evict the working file's colors.
    app.editor_mut().unwrap().blame.revision =
        Some(std::sync::Arc::new(crate::git_blame::Revision {
            workdir: path.parent().unwrap().to_owned(),
            path: "script".into(),
            working_file: path.clone(),
            hash: "revision".into(),
            date: String::new(),
            summary: String::new(),
            syntax_path: path.parent().unwrap().join("history/revision/script"),
        }));
    assert_syntax_in_both_layouts(&mut app);
}

#[test]
fn extensionless_blame_diff_and_historical_file_inherit_selected_language() {
    let (_dir, path, mut app) = fixture();
    let oid = commit_fixture_files(path.parent().unwrap(), &["script"], "update");
    let commit = crate::git_blame::Commit {
        hash: oid.to_string(),
        path: "script".into(),
        author: String::new(),
        email: String::new(),
        age: String::new(),
        date: String::new(),
        committed_at: String::new(),
        summary: "update".into(),
        message: "update".into(),
    };
    let history = crate::git_blame::prepare_history(path, &commit, 0, 1024 * 1024).unwrap();
    let revision = history.revision.clone();
    app.open_blame_history(history);
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Rust")
    );
    assert_syntax_in_both_layouts(&mut app);
    let snapshot = crate::git_blame::prepare_snapshot(revision, 0, 1024 * 1024).unwrap();
    app.open_blame_file(snapshot);
    assert_eq!(
        app.editor().unwrap().language_override.as_deref(),
        Some("Rust")
    );
}

#[cfg(windows)]
#[test]
fn windows_language_paths_match_git_slashes_and_editor_case_without_io() {
    use std::path::Path;
    for (left, right) in [
        (r"C:\project\script", "C:/project/script"),
        (r"C:\Project\SCRIPT", "c:/project/script"),
        (r"C:\project\.\script", "C:/project//script"),
        (r"\\server\share\script", "//SERVER/share/script"),
    ] {
        assert!(App::same_open_path(Path::new(left), Path::new(right)));
        assert!(App::same_open_path(Path::new(right), Path::new(left)));
    }
    for (left, right) in [
        (r"C:\project\script", r"C:\project\script2"),
        (r"C:\project\script", r"D:\project\script"),
        (r"C:\project", r"C:\project\script"),
        (r"C:\project\script", r"C:\other\script"),
    ] {
        assert!(!App::same_open_path(Path::new(left), Path::new(right)));
        assert!(!App::same_open_path(Path::new(right), Path::new(left)));
    }
}
