use super::*;
use crate::explorer::{FileEntry, GitStatus};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::time::Instant;

fn fixture(count: usize) -> App {
    let mut app = App::test_new();
    app.explorer.entries = (0..count).map(|index| {
        let name = format!("file-{index:05}界.rs");
        FileEntry { path: PathBuf::from("workspace").join(&name), name,
            is_dir: index % 13 == 0, depth: 0, expanded: false,
            git_status: GitStatus::Modified, git_status_summary: GitStatus::Modified.into(),
        }
    }).collect();
    app.show_open_editors = false;
    app.explorer.scroll_offset = count.saturating_sub(50);
    app.explorer_multi_selected = app.explorer.entries.iter()
        .enumerate().filter(|(index, _)| index % 5 != 0)
        .map(|(_, entry)| entry.path.clone()).collect();
    app.explorer_selection_anchor = Some(0);
    app
}

fn scene(app: &mut App, indexed: bool, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 55)).unwrap();
    terminal.draw(|frame| draw_explorer_impl(frame, app, Rect::new(0, 0, width, 55), indexed))
        .unwrap().buffer.clone()
}

#[test]
fn explorer_row_style_reuse_preserves_theme_focus_git_hover_and_drop_states() {
    use crate::app_theme::EditorTheme;
    let mut app = fixture(80);
    let statuses = [GitStatus::Unmodified, GitStatus::Modified, GitStatus::Added,
        GitStatus::Deleted, GitStatus::Renamed, GitStatus::Untracked, GitStatus::Ignored];
    for (i, entry) in app.explorer.entries.iter_mut().enumerate() {
        entry.git_status = statuses[i % statuses.len()];
        entry.git_status_summary = entry.git_status.into();
    }
    for theme in [EditorTheme::Dark, EditorTheme::Light] {
        app.colors = theme.colors();
        for step in 0..8 {
            app.active_pane = if step % 2 == 0 { crate::app::ActivePane::Explorer }
                else { crate::app::ActivePane::Editor };
            app.explorer.scroll_offset = step * 3;
            app.explorer.selected = step * 3;
            app.hovered_explorer = Some(step * 3 + 2);
            app.dragging_explorer_path = Some(app.explorer.entries[0].path.clone());
            app.explorer_drop_target_idx = Some(step * 3 + 4);
            for width in [3, 25, 60] {
                REFERENCE_EXPLORER_STYLES.set(false);
                let actual = scene(&mut app, true, width);
                REFERENCE_EXPLORER_STYLES.set(true);
                let expected = scene(&mut app, true, width);
                REFERENCE_EXPLORER_STYLES.set(false);
                assert_eq!(actual, expected, "step={step}, width={width}");
            }
        }
    }
}

#[test]
fn explorer_label_width_reuse_preserves_truncated_and_highlighted_unicode_names() {
    let mut app = fixture(16);
    app.explorer_multi_selected.clear();
    app.explorer_selection_anchor = None;
    for (index, entry) in app.explorer.entries.iter_mut().enumerate() {
        entry.name = ["ordinary_name.rs", "组件_café_e\u{301}_👩‍💻.py", "very long filename with spaces.txt"][index % 3].into();
        entry.is_dir = index % 4 == 0;
        app.explorer.find.results.rows.insert(entry.path.clone(), crate::explorer_find::Match {
            matched: true, indices: vec![0, 2, 5, 8, 10, 11, 12, 50], ..Default::default()
        });
    }
    for width in [1, 3, 8, 19, 32, 60] {
        for rename in [false, true] {
            app.rename.active = rename;
            app.rename.target = Some(app.explorer.entries[3].path.clone());
            app.rename.query = "new name.rs".into();
            REFERENCE_EXPLORER_STYLES.set(false);
            let actual = scene(&mut app, true, width);
            REFERENCE_EXPLORER_STYLES.set(true);
            let expected = scene(&mut app, true, width);
            REFERENCE_EXPLORER_STYLES.set(false);
            assert_eq!(actual, expected, "width={width}, rename={rename}");
        }
    }
}

#[test]
fn explorer_selection_projection_matches_complete_frames_and_stays_viewport_bounded() {
    let mut app = fixture(300);
    app.explorer_multi_selected.push("missing.rs".into());
    app.explorer_multi_selected.push(app.explorer.entries[290].path.clone());
    for width in [1, 5, 19, 60] {
        for offset in [0, 1, 90, 250, 299] {
            for anchor in [None, Some(0)] {
                app.explorer.scroll_offset = offset;
                app.explorer_selection_anchor = anchor;
                app.hovered_explorer = Some(offset + 2);
                app.explorer.selected = offset;
                assert_eq!(scene(&mut app, true, width), scene(&mut app, false, width));
                let rows = app.explorer.viewport_indices(50);
                let projected = visible_explorer_selection(&app, &rows).unwrap();
                assert!(projected.len() <= rows.len());
                for index in rows {
                    let path = &app.explorer.entries[index].path;
                    assert_eq!(projected.get(path).copied().unwrap_or(false),
                        app.explorer_multi_selected.contains(path));
                }
            }
        }
    }
    app.explorer_multi_selected.truncate(32);
    assert!(visible_explorer_selection(&app, &app.explorer.viewport_indices(50)).is_none());
    assert_eq!(scene(&mut app, true, 60), scene(&mut app, false, 60));
    // Replacing one selected path preserves the count, but must repaint the
    // affected rows immediately rather than reusing membership by length.
    app.explorer_multi_selected = app.explorer.entries.iter().take(280)
        .map(|entry| entry.path.clone()).collect();
    app.explorer.scroll_offset = 250;
    scene(&mut app, true, 60);
    app.explorer_multi_selected[279] = app.explorer.entries[299].path.clone();
    assert_eq!(scene(&mut app, true, 60), scene(&mut app, false, 60));
}

#[test]
#[ignore = "comparative release Explorer rendering benchmark"]
fn benchmark_explorer_selection_redraw() {
    for count in [16, 20_000] {
        let mut app = fixture(count);
        assert_eq!(scene(&mut app, true, 60), scene(&mut app, false, 60));
        let mut timings = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for indexed in [sample % 2 == 0, sample % 2 != 0] {
                let mut terminal = Terminal::new(TestBackend::new(60, 55)).unwrap();
                terminal.draw(|frame| draw_explorer_impl(frame, &mut app, Rect::new(0, 0, 60, 55), indexed)).unwrap();
                let start = Instant::now();
                for _ in 0..30 {
                    terminal.draw(|frame| draw_explorer_impl(frame, &mut app, Rect::new(0, 0, 60, 55), indexed)).unwrap();
                }
                timings[usize::from(indexed)].push(start.elapsed() / 30);
            }
        }
        for samples in &mut timings { samples.sort(); }
        eprintln!("Explorer {count} entries complete redraw: reference={:?}, optimized={:?}",
            timings[0][3], timings[1][3]);
    }
}
