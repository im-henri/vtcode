use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::time::Duration;

struct Fixture {
    app: App,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new(remote: bool, head: &str, index: &str, working: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        init_git_fixture(directory.path());
        std::fs::write(directory.path().join("file.txt"), head).unwrap();
        commit_fixture_files(directory.path(), &["file.txt"], "initial");
        let workspace = remote.then(|| crate::remote::workspace::Workspace::new(
            crate::remote::tests::connection(directory.path(), Default::default(), Default::default()),
        ));
        let root = workspace.as_ref().map_or_else(|| directory.path().to_path_buf(), |workspace| workspace.view_root.clone());
        let file = workspace.as_ref().map_or_else(|| directory.path().join("file.txt"), |workspace| workspace.target_key("file.txt").unwrap());
        let mut app = App::new_deferred(root);
        app.remote_workspace = workspace;
        app.source_control_enabled = true;
        app.active_pane = crate::app::ActivePane::Editor;
        app.tabs = vec![make_clean_file_tab(&file, working)];
        app.editor_mut().unwrap().saved_rope = Some(ropey::Rope::from_str(working));
        app.show_minimap = false;
        app.sticky_scroll = false;
        std::fs::write(directory.path().join("file.txt"), working).unwrap();
        let mut fixture = Self { app, directory };
        fixture.set_index(index);
        fixture.refresh();
        fixture
    }

    fn set_index(&mut self, text: &str) {
        let repo = git2::Repository::open(self.directory.path()).unwrap();
        let mut index = repo.index().unwrap();
        let mut entry = index.get_path(Path::new("file.txt"), 0).unwrap();
        entry.id = repo.blob(text.as_bytes()).unwrap();
        entry.file_size = text.len() as u32;
        index.add(&entry).unwrap();
        index.write().unwrap();
    }

    fn index(&self) -> String {
        let repo = git2::Repository::open(self.directory.path()).unwrap();
        let entry = repo.index().unwrap().get_path(Path::new("file.txt"), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    }

    fn refresh(&mut self) {
        self.app.refresh_git_line_status_async();
        self.app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_diff_running && !app.bg_git_diff_pending);
    }

    fn click(&mut self, line: usize) {
        let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
        terminal.draw(|frame| crate::ui_editor::draw_editor(frame, &mut self.app, Rect::new(10, 3, 110, 32))).unwrap();
        let gutter = self.app.gutter_area.unwrap();
        crate::mouse::handle_mouse(&mut self.app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: crate::ui_editor_highlight::change_gutter_column(gutter).unwrap(),
            row: gutter.y + line as u16, modifiers: KeyModifiers::NONE,
        });
        assert!(self.app.hunk_popup_active, "the gutter must open the preview");
        self.app.wait_for_hunk_preview();
    }

    fn assert_preview(&self, staged: bool, removed: &str, added: Option<&str>) {
        let peek = self.app.hunk_peek.as_ref().unwrap_or_else(|| panic!(
            "Preview failed, status {:?}, notifications {:?}", self.app.hunk_popup_status, self.app.notification_history,
        ));
        assert_eq!(self.app.hunk_popup_status == 'S', staged);
        assert!(peek.rows.iter().any(|row| row.kind == '-' && row.text == removed));
        if let Some(added) = added { assert!(peek.rows.iter().any(|row| row.kind == '+' && row.text == added)); }
        assert_eq!(self.app.hunk_popup_items(), if staged {
            vec!["Revert Hunk", "Close Preview"]
        } else { vec!["Stage Hunk", "Discard Hunk", "Close Preview"] });
    }
}

#[test]
fn staged_deletion_gutter_previews_and_actions_keep_the_index_source() {
    for remote in [false, true] {
        for (head, working) in [
            ("removed\nfirst\nlast\n", "first\nlast\n"),
            ("first\nremoved\nlast\n", "first\nlast\n"),
            ("first\nlast\nremoved\n", "first\nlast\n"),
            ("removed\n", ""),
        ] {
            let mut fixture = Fixture::new(remote, head, working, working);
            let line = fixture.app.editor().unwrap().git_line_status().iter().position(|&status| status == 'D').unwrap();
            fixture.click(line);
            fixture.assert_preview(true, "removed", None);
            fixture.app.execute_hunk_popup_action(0);
            fixture.app.wait_for_git_action();
            fixture.app.wait_for_hunk_preview();
            assert_eq!(fixture.index(), head);
            assert_eq!(std::fs::read_to_string(fixture.directory.path().join("file.txt")).unwrap(), working);
            assert_eq!(fixture.app.editor().unwrap().rope.to_string(), working);
            assert!(!fixture.app.editor().unwrap().modified);
        }
    }
}

#[test]
fn staged_gutter_markers_follow_unstaged_insertions_and_deletions() {
    let head = "a\nb\nc\nd\ne\n";
    let index = "a\nb\nC\nd\ne\n";
    for remote in [false, true] {
        for (working, line) in [("inserted\na\nb\nC\nd\ne\n", 3), ("b\nC\nd\ne\n", 1)] {
            let mut fixture = Fixture::new(remote, head, index, working);
            assert_eq!(fixture.app.editor().unwrap().line_diff_status(line), 'S', "remote={remote}, {working:?}");
            fixture.click(line);
            fixture.assert_preview(true, "c", Some("C"));
        }
    }
}

#[test]
fn staging_an_edit_at_a_staged_deletion_keeps_both_kinds_until_the_index_changes() {
    use crate::line_changes::LineChanges;

    let head = "first\nremoved\nretained\nlast\n";
    let index = "first\nretained\nlast\n";
    let working = "first\nedited retained\nlast\n";
    for remote in [false, true] {
        let mut fixture = Fixture::new(remote, head, index, working);
        assert_eq!(fixture.app.editor().unwrap().line_changes(1), LineChanges::git('D', 'M'));
        fixture.click(1);
        fixture.assert_preview(false, "retained", Some("edited retained"));
        fixture.app.execute_hunk_popup_action(0);
        fixture.app.wait_for_git_action();
        fixture.refresh();
        assert_eq!(fixture.index(), working);
        assert_eq!(fixture.app.editor().unwrap().line_changes(1), LineChanges::git('M', ' '));
    }
}

#[test]
fn gutter_previews_recover_when_the_clicked_hunk_moves_between_index_and_worktree() {
    let head = "a\nb\nc\nd\ne\nf\ng\n";
    let partial = "a\nB\nc\nd\ne\nf\ng\n";
    let working = "a\nB\nc\nd\ne\nF\ng\n";
    for remote in [false, true] {
        for staged in [false, true] {
            let mut fixture = Fixture::new(remote, head, if staged { partial } else { head }, working);
            assert_eq!(fixture.app.editor().unwrap().line_diff_status(1), if staged { 'S' } else { 'U' });
            // Leave the painted markers unchanged, as happens before the
            // asynchronous refresh after a stage/unstage operation completes.
            fixture.set_index(if staged { head } else { partial });
            fixture.click(1);
            fixture.assert_preview(!staged, "b", Some("B"));
            assert_eq!(fixture.app.hunk_peek.as_ref().unwrap().selected, 0);
            fixture.app.execute_hunk_popup_action(0);
            fixture.app.wait_for_git_action();
            fixture.app.wait_for_hunk_preview();
            assert_eq!(fixture.index(), if staged { partial } else { head });
            assert_eq!(std::fs::read_to_string(fixture.directory.path().join("file.txt")).unwrap(), working);
        }
    }
}

#[test]
fn an_outdated_marker_never_opens_an_unrelated_remaining_hunk() {
    let head = "a\nb\nc\nd\ne\nf\ng\n";
    for remote in [false, true] {
        let mut fixture = Fixture::new(remote, head, head, "a\nB\nc\nd\ne\nF\ng\n");
        std::fs::write(fixture.directory.path().join("file.txt"), "a\nb\nc\nd\ne\nF\ng\n").unwrap();
        fixture.click(1);
        assert!(!fixture.app.hunk_popup_active && fixture.app.hunk_peek.is_none());
        assert_eq!(fixture.index(), head);
    }
}

#[test]
fn deletion_source_changes_survive_marker_reuse_and_view_copies() {
    let mut editor = Editor::test_with_text("kept\n");
    let staged = crate::editor::PreparedGitLineStatus::from_diffs(2, &['D'], &[]);
    let unstaged = crate::editor::PreparedGitLineStatus::from_diffs(2, &[], &['D']);
    assert!(editor.set_prepared_git_line_status(staged.clone()));
    assert_eq!(editor.line_diff_status(0), 'D');
    assert_eq!(editor.line_hunk_status(0), 'S');
    let mut other = Editor::test_with_text("kept\n");
    other.set_prepared_git_line_status(editor.prepared_git_line_status());
    assert_eq!(other.line_hunk_status(0), 'S');
    assert!(editor.set_prepared_git_line_status(unstaged));
    assert_eq!(editor.line_diff_status(0), 'D');
    assert_eq!(editor.line_hunk_status(0), 'D');
    assert!(editor.set_prepared_git_line_status(staged));
    editor.clear_git_line_status();
    editor.set_git_line_status(vec!['D', ' ']);
    assert_eq!(editor.line_hunk_status(0), 'D');
}
