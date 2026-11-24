//! Vim mode in every workbench view: `:` opens the command line and the
//! Command Palette shortcuts work wherever the keyboard is.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{ActivePane, App, SidebarView, Tab, TabKind};
use crate::editor::Editor;

fn base() -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("abc\n"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.terminal_has_focus = true;
    app.quick_suggestions = false;
    super::app_host::set_enabled(&mut app, true);
    app
}

fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, mods));
}

fn keys(app: &mut App, text: &str) {
    for key in super::keys::parse(text, &[]) {
        crate::input_keyboard::handle_keyboard_event(app, key.to_event());
    }
}

fn draw(app: &mut App) {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
}

fn with_tab(kind: TabKind) -> App {
    let mut app = base();
    let mut tab = Tab::new_file(Editor::new());
    tab.kind = kind;
    tab.editor.read_only = true;
    app.tabs.push(tab);
    app.active_tab = 1;
    app
}

fn views() -> Vec<(&'static str, App)> {
    let mut out = vec![("editor", base())];
    for (name, kind) in [
        ("commit details", TabKind::CommitDetails),
        ("source control tab", TabKind::SourceControl),
        ("diff tab", TabKind::Diff(7)),
        ("markdown preview", TabKind::MarkdownPreview),
        ("extension info", TabKind::ExtensionInfo),
        ("git revision", TabKind::GitRevision),
    ] {
        out.push((name, with_tab(kind)));
    }
    out.push(("settings tab", with_tab(TabKind::Settings)));
    for (name, view) in [
        ("explorer", SidebarView::Explorer),
        ("source control sidebar", SidebarView::SourceControl),
        ("extensions sidebar", SidebarView::Extensions),
        ("remote sidebar", SidebarView::Remote),
    ] {
        let mut app = base();
        app.show_explorer = true;
        app.sidebar_view = view;
        app.active_pane = ActivePane::Explorer;
        out.push((name, app));
    }
    for (name, tab) in [("problems", 1), ("output", 2), ("notifications", 3)] {
        let mut app = base();
        app.show_panel = true;
        app.panel_tab = tab;
        app.active_pane = ActivePane::Panel;
        out.push((name, app));
    }
    let mut empty = base();
    empty.tabs.clear();
    out.push(("no tabs", empty));
    out
}

#[test]
fn colon_opens_the_command_line_in_every_view() {
    let mut failed = Vec::new();
    for (name, mut app) in views() {
        draw(&mut app);
        keys(&mut app, ":");
        if app.vim.cmdline.is_none() {
            failed.push(format!("{name}: ':' did not open the command line"));
            continue;
        }
        let before = app.relative_line_numbers;
        keys(&mut app, "set rnu!<CR>");
        if app.relative_line_numbers == before || app.vim.cmdline.is_some() {
            failed.push(format!("{name}: ':set rnu!' did not run"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn palette_shortcuts_work_in_every_view() {
    let mut failed = Vec::new();
    for (name, stroke) in [
        (
            "Ctrl+Shift+P",
            (
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ),
        (
            "Ctrl+Shift+p",
            (
                KeyCode::Char('p'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ),
        ("F1", (KeyCode::F(1), KeyModifiers::NONE)),
    ] {
        for (view, mut app) in views() {
            draw(&mut app);
            press(&mut app, stroke.0, stroke.1);
            if !app.palette.active {
                failed.push(format!("{view}: {name}"));
            }
        }
        // Insert mode too.
        let mut app = base();
        keys(&mut app, "i");
        press(&mut app, stroke.0, stroke.1);
        if !app.palette.active {
            failed.push(format!("insert mode: {name}"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

fn shift_ctrl(c: char) -> (KeyCode, KeyModifiers) {
    (
        KeyCode::Char(c),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )
}

#[test]
fn ctrl_shift_workbench_shortcuts_work_in_every_view_and_mode() {
    let checks: [(&str, char, fn(&App) -> bool); 3] = [
        ("Source Control", 'G', |a| {
            a.sidebar_view == SidebarView::SourceControl && a.show_explorer
        }),
        ("Search in Files", 'F', |a| {
            a.sidebar_view == SidebarView::Search && a.show_explorer
        }),
        ("Extensions", 'X', |a| {
            a.sidebar_view == SidebarView::Extensions && a.show_explorer
        }),
    ];
    let mut failed = Vec::new();
    for (what, c, ok) in checks {
        let (code, mods) = shift_ctrl(c);
        for ((view, mut app), (_, mut plain)) in views().into_iter().zip(views()) {
            super::app_host::set_enabled(&mut plain, false);
            draw(&mut app);
            draw(&mut plain);
            press(&mut app, code, mods);
            press(&mut plain, code, mods);
            if ok(&app) != ok(&plain) {
                failed.push(format!(
                    "{view}: {what} (vim {}, plain {})",
                    ok(&app),
                    ok(&plain)
                ));
            }
        }
        for (mode, prefix) in [
            ("insert", "i"),
            ("visual", "v"),
            ("visual line", "V"),
            ("operator", "d"),
        ] {
            let mut app = base();
            keys(&mut app, prefix);
            let mut plain = base();
            super::app_host::set_enabled(&mut plain, false);
            press(&mut app, code, mods);
            press(&mut plain, code, mods);
            if ok(&app) != ok(&plain) {
                failed.push(format!(
                    "{mode} mode: {what} (vim {}, plain {})",
                    ok(&app),
                    ok(&plain)
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn palette_opens_from_visual_and_operator_pending_modes() {
    let mut failed = Vec::new();
    for (mode, prefix) in [
        ("visual", "v"),
        ("visual line", "V"),
        ("visual block", "<C-v>"),
        ("operator", "d"),
        ("replace", "R"),
    ] {
        let mut app = base();
        keys(&mut app, prefix);
        let (code, mods) = shift_ctrl('P');
        press(&mut app, code, mods);
        if !app.palette.active {
            failed.push(mode);
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn commit_details_takes_vim_keys() {
    let mut app = with_tab(TabKind::CommitDetails);
    app.commit_details.data.files = (0..4)
        .map(|i| crate::commit_details::CommitFile {
            status: 'M',
            path: format!("f{i}.rs"),
            ..Default::default()
        })
        .collect();
    app.commit_details.selected = 0;
    draw(&mut app);
    keys(&mut app, "j");
    assert_eq!(app.commit_details.selected, 1, "j moves down");
    keys(&mut app, "G");
    assert_eq!(app.commit_details.selected, 3, "G goes to the last file");
    keys(&mut app, "k");
    assert_eq!(app.commit_details.selected, 2, "k moves up");
    keys(&mut app, "gg");
    assert_eq!(app.commit_details.selected, 0, "gg goes to the top");
    // `:` types in the filter once it has the keyboard.
    keys(&mut app, "/");
    assert_eq!(
        app.commit_details.focus,
        crate::commit_details::Focus::Filter
    );
    keys(&mut app, "a:b");
    assert!(app.vim.cmdline.is_none());
    assert_eq!(app.commit_details.query, "a:b");
    keys(&mut app, "<Esc><Esc>");
    // `:q` closes the commit tab.
    let tabs = app.tabs.len();
    keys(&mut app, ":");
    assert!(
        app.vim.cmdline.is_some(),
        "focus {:?}",
        app.commit_details.focus
    );
    keys(&mut app, "q");
    assert_eq!(
        app.vim
            .cmdline
            .as_ref()
            .map(|c| c.text.iter().collect::<String>()),
        Some("q".to_string())
    );
    keys(&mut app, "<CR>");
    assert_eq!(
        app.tabs.len(),
        tabs - 1,
        ":q closes Commit Details: {:?}",
        app.vim.message.as_ref().map(|m| &m.text)
    );
    assert!(app.vim.cmdline.is_none());
}

#[test]
fn ctrl_w_moves_out_of_views_without_a_buffer() {
    let mut failed = Vec::new();
    for (name, kind) in [
        ("commit details", TabKind::CommitDetails),
        ("settings", TabKind::Settings),
        ("source control tab", TabKind::SourceControl),
        ("markdown preview", TabKind::MarkdownPreview),
    ] {
        let mut app = with_tab(kind);
        app.show_explorer = true;
        app.sidebar_view = SidebarView::Explorer;
        draw(&mut app);
        keys(&mut app, "<C-w>h");
        if app.active_pane != ActivePane::Explorer {
            failed.push(name);
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

fn graph_app(from_tab: bool) -> App {
    let mut app = base();
    app.source_control_enabled = true;
    app.git_log = (0..3)
        .map(|i| crate::app::GitCommitInfo {
            hash: format!("{i:07}"),
            full_hash: format!("{i:040}"),
            graph: "●".into(),
            message: format!("Commit {i}"),
            author: "Ada".into(),
            time_ago: "now".into(),
            branches: vec![],
        })
        .collect();
    if from_tab {
        app.open_source_control_tab(false);
    }
    draw(&mut app);
    // What a click on a commit in the graph does.
    app.open_source_control_commit_details(0);
    draw(&mut app);
    app
}

fn check_colon_and_palette(app: &mut App, place: &str) {
    keys(app, ":");
    assert!(
        app.vim.cmdline.is_some(),
        "{place}: ':' opens the command line"
    );
    let before = app.relative_line_numbers;
    keys(app, "set rnu!<CR>");
    assert_ne!(
        app.relative_line_numbers, before,
        "{place}: the command ran"
    );
    let (code, mods) = shift_ctrl('P');
    press(app, code, mods);
    assert!(
        app.palette.active,
        "{place}: Ctrl+Shift+P opens the palette"
    );
    keys(app, "<Esc>");
    assert!(!app.palette.active);
    press(app, KeyCode::F(1), KeyModifiers::NONE);
    assert!(app.palette.active, "{place}: F1 opens the palette");
    keys(app, "<Esc>");
    draw(app);
}

#[test]
fn commit_details_opened_from_the_graph_take_colon_and_the_palette() {
    // Source Control in the sidebar: the commit opens as the active tab.
    let mut app = graph_app(false);
    assert!(app.is_commit_details_tab());
    check_colon_and_palette(&mut app, "commit details tab");
    keys(&mut app, ":q<CR>");
    assert!(!app.is_commit_details_tab(), ":q closes it");

    // Source Control as a tab: the commit opens in a preview split and the
    // graph keeps the keyboard.
    let mut app = graph_app(true);
    check_colon_and_palette(&mut app, "graph after a click");
    let (group, _) = app
        .tab_kind_location(TabKind::CommitDetails)
        .expect("the preview shows the commit");
    app.switch_group(group);
    app.active_pane = ActivePane::Editor;
    draw(&mut app);
    assert!(app.is_commit_details_tab());
    check_colon_and_palette(&mut app, "commit preview split");
    keys(&mut app, ":q<CR>");
    assert!(
        app.tab_kind_location(TabKind::CommitDetails).is_none(),
        ":q closes the preview"
    );
}
