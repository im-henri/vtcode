use super::*;
use crate::explorer::{FileEntry, GitStatus};
use std::sync::mpsc;

fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.explorer.entries = (0..100)
        .map(|i| {
            let name = format!("file-{i}.txt");
            let path = root.path().join(&name);
            FileEntry {
                path,
                name,
                is_dir: false,
                depth: 0,
                expanded: false,
                git_status: GitStatus::Unmodified,
                git_status_summary: Default::default(),
            }
        })
        .collect();
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Explorer;
    app.terminal_has_focus = true;
    (root, app)
}

#[test]
fn sweeping_the_tree_queues_no_reads_or_idle_work_after_leaving() {
    let (_root, mut app) = fixture();
    let now = Instant::now();
    for index in 0..100 {
        app.hovered_explorer = Some(index);
        let at = now + Duration::from_millis(index as u64);
        poll(&mut app, at);
        assert_eq!(next_wait(&app, at), Some(QUIET));
        assert_eq!(app.background.active_job_count(), 0);
    }
    app.hovered_explorer = None;
    poll(&mut app, now + Duration::from_secs(10));
    assert_eq!(next_wait(&app, now + Duration::from_secs(10)), None);
    assert!(app.explorer_hover.target.is_none());
    assert_eq!(app.background.active_job_count(), 0);
}

#[test]
fn painting_an_uncached_hover_does_not_submit_filesystem_work() {
    let (_root, mut app) = fixture();
    app.hovered_explorer = Some(0);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 35)).unwrap();
    for _ in 0..3 {
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
    }
    assert_eq!(app.background.active_job_count(), 0);
    assert!(app.explorer_hover.active.is_none());
    assert_eq!(app.viewport_area, ratatui::layout::Rect::new(0, 0, 140, 35));
}

#[test]
fn slow_reads_keep_one_slot_and_only_the_latest_hover_can_populate_the_cache() {
    let (_root, mut app) = fixture();
    let latest = app.explorer.entries[99].path.clone();
    std::fs::write(&latest, "one\ntwo\n").unwrap();
    app.background.use_single_read_worker();
    let (started, running) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    app.background
        .maintenance_sender()
        .send(Box::new(move || {
            started.send(()).unwrap();
            let _ = resume.recv();
        }))
        .unwrap();
    running.recv_timeout(Duration::from_secs(5)).unwrap();
    let now = Instant::now();
    app.hovered_explorer = Some(0);
    poll(&mut app, now);
    poll(&mut app, now + QUIET);
    assert_eq!(app.background.active_job_count(), 2);
    for index in 1..100 {
        app.hovered_explorer = Some(index);
        let at = now + Duration::from_secs(index as u64);
        poll(&mut app, at);
        poll(&mut app, at + QUIET);
        assert_eq!(
            app.background.active_job_count(),
            2,
            "one queued read, despite a blocked disk"
        );
        assert_eq!(
            next_wait(&app, at + QUIET),
            None,
            "completion wakes the blocked slot; no spinning timer"
        );
    }
    // An optional metadata call blocked in the OS must not hold up an open,
    // even when there is only one foreground read worker.
    let (read, done) = mpsc::channel();
    let file = latest.clone();
    app.background
        .read_for_file("file-load", &latest, true, move || {
            read.send(std::fs::read_to_string(file).unwrap()).unwrap();
        });
    assert_eq!(
        done.recv_timeout(Duration::from_secs(5)).unwrap(),
        "one\ntwo\n"
    );
    release.send(()).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.explorer_hover.active.is_none()
    });
    assert!(
        app.explorer_hover_info_cache.is_empty(),
        "obsolete reads must not publish stale metadata"
    );
    poll(&mut app, now + Duration::from_secs(101));
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.explorer_hover_info_cache.contains_key(&latest)
    });
    assert_eq!(app.explorer_hover_info_cache.len(), 1);
    assert_eq!(app.explorer_hover_info_cache[&latest].line_count, Some(3));
    assert_eq!(next_wait(&app, Instant::now()), None);
}

#[test]
fn a_hidden_explorer_cannot_leave_its_last_tooltip_over_the_editor() {
    let (_root, mut app) = fixture();
    let path = app.explorer.entries[0].path.clone();
    app.explorer_hover_info_cache.insert(
        path.clone(),
        ExplorerHoverInfo {
            path,
            is_dir: false,
            size: 17,
            modified: None,
            line_count: Some(3),
        },
    );
    app.hovered_explorer = Some(0);
    app.mouse_col = 15;
    app.mouse_row = 10;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 35)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let text = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    assert!(text(&terminal).contains("17 B"));
    app.show_explorer = false;
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(!text(&terminal).contains("17 B"));
}

fn tooltip_scene(is_dir: bool) -> (tempfile::TempDir, App) {
    use crate::{app::Tab, editor::Editor};

    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.source_control_enabled = false;
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Explorer;
    app.show_minimap = false;
    app.show_panel = false;
    app.cursor_blinking = false;
    for effect in crate::animation_config::AnimationEffect::ALL {
        app.animation_settings.set(effect, false);
    }
    let name = "a-long-tooltip-path-with-Unicode-目录-and-details.txt".to_string();
    let path = root.path().join(&name);
    app.explorer.entries = vec![FileEntry {
        path: path.clone(),
        name,
        is_dir,
        depth: 0,
        expanded: false,
        git_status: GitStatus::Unmodified,
        git_status_summary: Default::default(),
    }];
    app.explorer_hover_info_cache.insert(
        path.clone(),
        ExplorerHoverInfo {
            path,
            is_dir,
            size: 17,
            modified: None,
            line_count: Some(3),
        },
    );
    let mut editor = Editor::test_with_text(
        &format!("{}\n", ".vscode/ 界 settings.json ".repeat(30)).repeat(35),
    );
    editor.select_all();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.mouse_col = 16;
    app.mouse_row = 8;
    (root, app)
}

#[test]
fn remote_explorer_and_tab_tooltips_show_the_remote_machines_paths() {
    use crate::remote::{self, RemotePath};
    use ratatui::{Terminal, backend::TestBackend};

    for (native, uri, expected) in [
        ("/home/remote/project space", "file:///home/remote/project%20space/",
            "/home/remote/project space/src/white space λ #%.rs"),
        (r"C:\work\project space", "file:///C:/work/project%20space/",
            r"C:\work\project space\src\white space λ #%.rs"),
        (r"\\server\share\project space", "file://server/share/project%20space/",
            r"\\server\share\project space\src\white space λ #%.rs"),
    ] {
        let hello = remote::Hello {
            version: remote::VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            root: native.into(),
            root_uri: uri.into(),
            session: 42,
        };
        let client = remote::client::Client::attach(
            std::io::Cursor::new(Vec::new()), Vec::new(), hello,
        ).unwrap();
        let workspace = remote::workspace::Workspace::new(client);
        let target = RemotePath::new("src/white space λ #%.rs".as_bytes().to_vec()).unwrap();
        let key = workspace.view_key(&target);
        assert!(key.to_string_lossy().contains("vtcode-remote-"));
        for is_dir in [false, true] {
            for tab_tooltip in [false, true] {
                if is_dir && tab_tooltip { continue; }
                let (_root, mut app) = tooltip_scene(is_dir);
                let old = app.explorer.entries[0].path.clone();
                let mut info = app.explorer_hover_info_cache.remove(&old).unwrap();
                info.path = key.clone();
                app.explorer_hover_info_cache.insert(key.clone(), info);
                app.explorer.entries[0].path = key.clone();
                app.explorer.entries[0].name = "white space λ #%.rs".into();
                app.explorer.root = workspace.view_root.clone();
                app.remote_workspace = Some(workspace.clone());
                if tab_tooltip {
                    app.tabs[0].editor.file_path = Some(key.clone());
                    app.tab_tooltip_tab = Some(0);
                    app.tab_tooltip_hover_start = Some(Instant::now() - Duration::from_secs(1));
                } else {
                    app.hovered_explorer = Some(0);
                }
                let mut terminal = Terminal::new(TestBackend::new(180, 35)).unwrap();
                let completed = terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                let text = completed.buffer.content.iter().map(|cell| cell.symbol()).collect::<String>();
                assert!(text.contains(expected),
                    "root={native}, directory={is_dir}, tab={tab_tooltip}: expected {expected:?}");
                assert!(!text.contains("vtcode-remote-") && !text.contains('\0'));
                assert_eq!(workspace.remote_path(&app.explorer.entries[0].path).unwrap(), target);
                assert_eq!(app.tab_tooltip_shown, tab_tooltip);
                assert_eq!(app.background.active_job_count(), 0,
                    "formatting the tooltip must not request filesystem or network work");
            }
        }
    }
}

#[test]
fn commit_details_tabs_allow_explorer_hover_when_selected_hidden_or_in_another_group() {
    use crate::{app::Tab, editor::Editor};
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let moved = |column, row| MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    for placement in 0..3 {
        let (_root, mut app) = tooltip_scene(false);
        let mut details = Tab::new_file(Editor::new());
        details.kind = TabKind::CommitDetails;
        details.editor.read_only = true;
        app.tabs.push(details);
        app.commit_details.active = true;
        app.terminal_has_focus = true;
        match placement {
            0 => app.active_tab = 1,
            1 => app.active_tab = 0,
            _ => {
                app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
                app.switch_group(0);
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(180, 35)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(app.commit_details.active);
        assert_eq!(app.commit_details.visible, placement != 1);

        let explorer_button =
            crate::ui_chrome::activity_item_regions(app.activity_bar_area.unwrap())
                .into_iter()
                .find(|(_, index)| *index == 0)
                .unwrap()
                .0;
        crate::mouse::handle_mouse(&mut app, moved(explorer_button.x + 1, explorer_button.y));
        assert_eq!(app.hovered_activity, Some(0), "placement={placement}");

        let tree = crate::ui_chrome::explorer_tree_content_area(&app, app.explorer_area.unwrap());
        // The event loop marks each move dirty before mouse routing.
        app.mouse_hover_changed = true;
        crate::mouse::handle_mouse(&mut app, moved(tree.x + 1, tree.y));
        assert_eq!(app.hovered_explorer, Some(0), "placement={placement}");
        assert!(app.mouse_hover_changed);
        assert!(app.hovered_activity.is_none());
        let frame = terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let text: String = frame
            .buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("17 B"),
            "the Explorer tooltip must render; placement={placement}"
        );

        // A real confirmation still owns mouse events above every editor tab.
        app.confirm_close_active = true;
        app.reset_hover_states();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        crate::mouse::handle_mouse(&mut app, moved(explorer_button.x + 1, explorer_button.y));
        assert!(app.hovered_activity.is_none());
        crate::mouse::handle_mouse(&mut app, moved(tree.x + 1, tree.y));
        assert!(app.hovered_explorer.is_none());
    }
}

#[test]
fn explorer_tooltips_clear_background_text_and_selection_across_their_entire_rectangle() {
    use ratatui::{Terminal, backend::TestBackend};
    use unicode_width::UnicodeWidthStr;

    for is_dir in [false, true] {
        for theme in ["monokai", "light"] {
            for width in [80, 160] {
                let (_root, mut app) = tooltip_scene(is_dir);
                app.colors = crate::app_theme_loader::builtin_theme(theme)
                    .unwrap()
                    .to_theme_colors();
                let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                // Completed frames include Unicode continuation cells;
                // TestBackend only records individual terminal writes.
                let before = terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap()
                    .buffer
                    .clone();
                let path_width =
                    UnicodeWidthStr::width(app.explorer.entries[0].path.to_string_lossy().as_ref())
                        + 2;
                let tooltip_width = path_width.min(usize::from(width - 2)) as u16 + 1;
                let x = (app.mouse_col + 2).min(width - tooltip_width);
                let y = app.mouse_row + 1;
                let details = if is_dir {
                    " 📁  "
                } else {
                    " 17 B, 3 lines,  "
                };
                let padding = x + UnicodeWidthStr::width(details) as u16..x + tooltip_width;
                assert!(
                    padding
                        .clone()
                        .any(|col| before[(col, y + 1)].symbol() != " "),
                    "the shorter details row must overlap real editor/Explorer text"
                );

                app.hovered_explorer = Some(0);
                let completed = terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                let buffer = completed.buffer;
                let row: String = (x..x + tooltip_width)
                    .map(|col| buffer[(col, y + 1)].symbol())
                    .collect();
                assert!(
                    row.starts_with(details),
                    "{theme}, directory={is_dir}: {row:?}"
                );
                for col in padding {
                    let cell = &buffer[(col, y + 1)];
                    assert_eq!(
                        cell.symbol(),
                        " ",
                        "background leaked at ({col}, {}) in {theme}",
                        y + 1
                    );
                    assert!(
                        cell.modifier.is_empty(),
                        "underlying text style leaked into tooltip padding"
                    );
                }
                for row in y..y + 2 {
                    let mut covered_until = x;
                    for col in x..x + tooltip_width {
                        if col < covered_until {
                            continue; // Style comes from the leading wide glyph.
                        }
                        assert_eq!(buffer[(col, row)].bg, app.colors.bg_sidebar);
                        covered_until =
                            col + UnicodeWidthStr::width(buffer[(col, row)].symbol()) as u16;
                    }
                }
                assert_eq!(
                    app.background.active_job_count(),
                    0,
                    "painting must not request metadata"
                );
            }
        }
    }
}

#[test]
fn moving_and_dismissing_an_explorer_tooltip_restores_the_covered_editor() {
    use ratatui::{Terminal, backend::TestBackend};

    let (_root, mut app) = tooltip_scene(true);
    app.animation_settings
        .set(crate::animation_config::AnimationEffect::CursorSmear, true);
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    let before = terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap()
        .buffer
        .clone();
    let code = app.code_area.unwrap();
    app.hovered_explorer = Some(0);
    for row in [8, 16, 8] {
        app.mouse_row = row;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        for _ in 0..2 {
            app.rendering_animation_frame = true;
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            app.rendering_animation_frame = false;
        }
    }
    assert!(
        app.animation_frame_cache.replays > 0,
        "exercise tooltips over cached editor frames"
    );
    app.hovered_explorer = None;
    let completed = terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    for row in code.y..code.bottom() {
        for col in code.x..code.right() {
            assert_eq!(
                completed.buffer[(col, row)],
                before[(col, row)],
                "tooltip left a stale cell at ({col}, {row})"
            );
        }
    }
}

#[test]
fn hidden_or_obscured_explorer_does_not_schedule_hover_reads() {
    for mode in 0..5 {
        let (_root, mut app) = fixture();
        let now = Instant::now();
        app.hovered_explorer = Some(0);
        poll(&mut app, now);
        match mode {
            0 => app.terminal_has_focus = false,
            1 => app.show_explorer = false,
            2 => app.sidebar_view = SidebarView::Search,
            3 => app.menu_open = true,
            _ => app.explorer_context_active = true,
        }
        poll(&mut app, now + QUIET);
        assert_eq!(app.background.active_job_count(), 0);
        assert_eq!(next_wait(&app, now + QUIET), None);
    }
}

#[test]
fn metadata_completion_repaints_current_tooltip_and_invalidated_reads_are_rejected() {
    let (_root, mut app) = fixture();
    app.hovered_explorer = Some(0);
    let path = app.explorer.entries[0].path.clone();
    let cancel = CancellationToken::new();
    app.explorer_hover.active = Some(Request {
        path: path.clone(),
        generation: 1,
        cancel,
    });
    let info = || {
        Some(ExplorerHoverInfo {
            path: path.clone(),
            is_dir: false,
            size: 17,
            modified: None,
            line_count: Some(3),
        })
    };
    app.background
        .sender
        .send(BackgroundResult::ExplorerHoverInfo {
            generation: 1,
            info: info(),
        })
        .unwrap();
    assert!(
        app.poll_background_results(),
        "arrival must display the tooltip without another mouse move or cursor blink"
    );
    app.explorer_hover.active = Some(Request {
        path: path.clone(),
        generation: 2,
        cancel: CancellationToken::new(),
    });
    app.explorer_hover.invalidate(Some(&path));
    app.explorer_hover_info_cache.clear();
    app.background
        .sender
        .send(BackgroundResult::ExplorerHoverInfo {
            generation: 2,
            info: info(),
        })
        .unwrap();
    app.poll_background_results();
    assert!(app.explorer_hover_info_cache.is_empty());
    assert!(app.explorer_hover.active.is_none());
}
