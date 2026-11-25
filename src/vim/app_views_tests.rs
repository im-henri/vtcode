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

/// More places the keyboard can be: terminals, the agent chat, Search,
/// the Output panel and the commit message box.
fn more_views() -> Vec<(&'static str, App)> {
    let mut out = Vec::new();
    {
        let mut app = base();
        let pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "");
        app.terminals[0].terminal_pty = Some(pty);
        app.terminals[0].dock = crate::app::TerminalDock::Right;
        app.show_terminal_sidebar = true;
        app.terminal_panel_focused = true;
        out.push(("terminal sidebar", app));
    }
    {
        let mut app = base();
        app.chat.visible = true;
        app.active_pane = ActivePane::Chat;
        out.push(("agent chat", app));
    }
    {
        let mut app = base();
        app.show_explorer = true;
        app.sidebar_view = SidebarView::Search;
        app.global_search_active = true;
        app.active_pane = ActivePane::Explorer;
        out.push(("search box", app));
    }
    {
        let mut app = base();
        app.show_explorer = true;
        app.sidebar_view = SidebarView::Search;
        app.global_search_active = true;
        app.global_search_results_focused = true;
        app.active_pane = ActivePane::Explorer;
        out.push(("search results", app));
    }
    {
        let mut app = base();
        app.show_explorer = true;
        app.sidebar_view = SidebarView::SourceControl;
        app.active_pane = ActivePane::Explorer;
        app.source_control_input_mode = true;
        out.push(("commit message box", app));
    }
    out
}

fn fingerprint(app: &App) -> String {
    format!(
        "palette={} quick_open={} explorer={} view={:?} panel={}/{} shortcuts={} chat={} tab={} pane={:?} tabs={}",
        app.palette.active,
        app.quick_open_active,
        app.show_explorer,
        app.sidebar_view,
        app.show_panel,
        app.panel_tab,
        app.shortcuts_active,
        app.chat.visible,
        app.active_tab,
        app.active_pane,
        app.tabs.len(),
    )
}

/// Vim mode must not change what workbench shortcuts do outside the Vim
/// buffer, and Ctrl+Shift / F-key shortcuts must work the same in it too.
#[test]
fn workbench_shortcuts_match_plain_mode_outside_the_vim_buffer() {
    let c = KeyModifiers::CONTROL;
    let cs = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    let strokes = [
        ("Ctrl+Shift+P", KeyCode::Char('P'), cs, true),
        ("F1", KeyCode::F(1), KeyModifiers::NONE, true),
        ("Ctrl+Shift+G", KeyCode::Char('G'), cs, true),
        ("Ctrl+Shift+F", KeyCode::Char('F'), cs, true),
        ("Ctrl+Shift+X", KeyCode::Char('X'), cs, true),
        ("Ctrl+Shift+M", KeyCode::Char('M'), cs, true),
        ("Ctrl+Shift+I", KeyCode::Char('I'), cs, true),
        ("Ctrl+Shift+/", KeyCode::Char('/'), cs, true),
        ("Ctrl+P", KeyCode::Char('p'), c, true),
        ("Ctrl+B", KeyCode::Char('b'), c, false),
        ("Ctrl+`", KeyCode::Char('`'), c, false),
        ("Ctrl+Tab", KeyCode::Tab, c, false),
    ];
    let mut failed = Vec::new();
    for (name, code, mods, in_editor_too) in strokes {
        let all = views().into_iter().chain(more_views());
        let plain = views().into_iter().chain(more_views());
        for ((view, mut app), (_, mut off)) in all.zip(plain) {
            // Vim buffers (a file, a git revision) own Vim's Ctrl keys.
            if matches!(view, "editor" | "git revision" | "diff tab") && !in_editor_too {
                continue;
            }
            super::app_host::set_enabled(&mut off, false);
            draw(&mut app);
            draw(&mut off);
            press(&mut app, code, mods);
            press(&mut off, code, mods);
            let (a, b) = (fingerprint(&app), fingerprint(&off));
            if a != b {
                failed.push(format!("{view}: {name}\n   vim:   {a}\n   plain: {b}"));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn colon_types_where_text_is_being_typed() {
    for (view, mut app) in more_views() {
        if view == "search results" {
            continue;
        }
        draw(&mut app);
        keys(&mut app, ":");
        assert!(app.vim.cmdline.is_none(), "{view}: ':' is text here");
    }
    let mut app = more_views()
        .into_iter()
        .find(|v| v.0 == "search results")
        .unwrap()
        .1;
    draw(&mut app);
    keys(&mut app, ":");
    assert!(
        app.vim.cmdline.is_some(),
        "search results: ':' opens the command line"
    );
}

fn diff_tab_app(vim: bool) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_bool("core.autocrlf", false).unwrap();
        config.set_str("user.email", "t@example.com").unwrap();
        config.set_str("user.name", "T").unwrap();
    }
    let file = dir.path().join("file.txt");
    let old: String = (0..80).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, &old).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    let new = old.replace("line 40\n", "line forty\n");
    std::fs::write(&file, &new).unwrap();
    let mut app = App::new(dir.path().to_path_buf());
    let mut editor = Editor::test_with_text(&new);
    editor.file_path = Some(file.clone());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.terminal_has_focus = true;
    app.source_control_files = vec![crate::app::GitFileChange {
        directory: false,
        original_path: None,
        path: "file.txt".into(),
        status: 'M',
        staged: false,
    }];
    app.open_source_control_diff(0);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    app.active_pane = ActivePane::Editor;
    super::app_host::set_enabled(&mut app, vim);
    draw(&mut app);
    (dir, app)
}

fn view_of(app: &App) -> (usize, usize) {
    let ed = app.editor().unwrap();
    (ed.cursor_line, ed.scroll_offset)
}

#[test]
fn a_diff_tab_moves_with_vim_keys_like_the_arrows() {
    let (_d, probe) = diff_tab_app(true);
    assert!(matches!(
        probe.tabs[probe.active_tab].kind,
        TabKind::Diff(_)
    ));
    let mut failed = Vec::new();
    for (vim_keys, plain) in [
        ("j", vec![(KeyCode::Down, KeyModifiers::NONE)]),
        ("jjk", vec![(KeyCode::Down, KeyModifiers::NONE)]),
        ("G", vec![(KeyCode::End, KeyModifiers::CONTROL)]),
        ("Ggg", vec![(KeyCode::Home, KeyModifiers::CONTROL)]),
    ] {
        let (_d1, mut app) = diff_tab_app(true);
        let (_d2, mut off) = diff_tab_app(false);
        keys(&mut app, vim_keys);
        for (code, mods) in plain {
            press(&mut off, code, mods);
        }
        if vim_keys == "Ggg" {
            press(&mut off, KeyCode::End, KeyModifiers::CONTROL);
            press(&mut off, KeyCode::Home, KeyModifiers::CONTROL);
        }
        // Vim's last line is the last one with text, not the empty line
        // after the final newline.
        let expected = if vim_keys == "G" { 79 } else { view_of(&off).0 };
        if view_of(&app).0 != expected {
            failed.push(format!(
                "{vim_keys}: vim {:?} plain {:?}",
                view_of(&app),
                view_of(&off)
            ));
        }
    }
    // `:` works there too.
    let (_d, mut app) = diff_tab_app(true);
    keys(&mut app, ":");
    if app.vim.cmdline.is_none() {
        failed.push("':' in a diff tab".into());
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Views reached less often: other tab kinds, Settings and Commit Details
/// with the keyboard off their default field, and a split.
fn rare_views() -> Vec<(&'static str, App, bool)> {
    // (name, app, ':' should open the command line)
    let mut out = Vec::new();
    for (name, kind) in [
        ("amend commit tab", TabKind::AmendCommit),
        ("extension view tab", TabKind::ExtensionView),
        ("remote connection tab", TabKind::RemoteConnection(0)),
    ] {
        out.push((name, with_tab(kind), true));
    }
    for (name, categories, control) in [
        ("settings categories", true, false),
        ("settings control", false, true),
    ] {
        let mut app = with_tab(TabKind::Settings);
        app.settings_focus_categories = categories;
        app.settings_focus_control = control;
        out.push((name, app, true));
    }
    use crate::commit_details::Focus;
    for (name, focus) in [
        ("commit files", Focus::Files),
        ("commit message", Focus::Message),
        ("commit diff", Focus::Diff),
        ("commit action", Focus::Action(0)),
    ] {
        let mut app = with_tab(TabKind::CommitDetails);
        app.commit_details.focus = focus;
        out.push((name, app, true));
    }
    let mut split = with_tab(TabKind::CommitDetails);
    split.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
    out.push(("commit details in a split", split, true));
    let mut split = base();
    split.create_split_group_dir(0, crate::app::SplitDirection::Horizontal);
    out.push(("editor in a split", split, true));
    out
}

#[test]
fn colon_and_shortcuts_in_rare_views() {
    let mut failed = Vec::new();
    for (name, mut app, opens) in rare_views() {
        draw(&mut app);
        keys(&mut app, ":");
        draw(&mut app);
        if app.vim.cmdline.is_some() != opens {
            failed.push(format!(
                "{name}: ':' opened the command line: {}",
                app.vim.cmdline.is_some()
            ));
        }
        if opens && app.vim.cmdline.is_some() {
            let before = app.relative_line_numbers;
            keys(&mut app, "set rnu!<CR>");
            if app.relative_line_numbers == before {
                failed.push(format!("{name}: ':set rnu!' did not run"));
            }
        }
    }
    let cs = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    for (stroke, code, mods) in [
        ("Ctrl+Shift+P", KeyCode::Char('P'), cs),
        ("F1", KeyCode::F(1), KeyModifiers::NONE),
        ("Ctrl+Shift+G", KeyCode::Char('G'), cs),
        ("Ctrl+P", KeyCode::Char('p'), KeyModifiers::CONTROL),
        ("Ctrl+Shift+/", KeyCode::Char('/'), cs),
    ] {
        for ((name, mut app, _), (_, mut off, _)) in rare_views().into_iter().zip(rare_views()) {
            super::app_host::set_enabled(&mut off, false);
            draw(&mut app);
            draw(&mut off);
            press(&mut app, code, mods);
            press(&mut off, code, mods);
            let (a, b) = (fingerprint(&app), fingerprint(&off));
            if a != b {
                failed.push(format!("{name}: {stroke}\n   vim:   {a}\n   plain: {b}"));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn chat_tab_app() -> App {
    use vtcode::acp::{Session, protocol as p};
    let mut app = base();
    app.chat.initialized = Some(p::InitializeResponse::new(vtcode::acp::ProtocolVersion::V1));
    let id = p::SessionId::new("first");
    app.chat
        .sessions
        .insert(id.clone(), Session::new(id.clone()));
    app.chat.open_tabs.push(id.clone());
    app.chat.session_order.push(id.clone());
    app.chat.selected = Some(id.clone());
    app.place_chat_in_editor(id, 0, None);
    draw(&mut app);
    // Back on the file tab, in the editor.
    super::app_host::activate_tab(&mut app, 0);
    draw(&mut app);
    assert_eq!(app.active_pane, ActivePane::Editor);
    app
}

#[test]
fn a_chat_tab_reached_with_vim_commands_takes_typing() {
    for how in ["gt", ":bn<CR>", ":tabnext<CR>"] {
        let mut app = chat_tab_app();
        keys(&mut app, how);
        // No frame in between: the very next key must reach the chat.
        assert!(
            matches!(app.tabs[app.active_tab].kind, TabKind::Chat(_)),
            "{how} switches to the chat tab"
        );
        keys(&mut app, "a:b");
        assert!(app.vim.cmdline.is_none(), "{how}: ':' is text in the chat");
        assert_eq!(app.chat.input, "a:b", "{how}: typing reaches the chat");
        // And back out with Ctrl-\ Ctrl-N, as from the docked chat.
        keys(&mut app, r"<C-\><C-n>");
        assert_ne!(
            app.active_pane,
            ActivePane::Chat,
            "{how}: Ctrl-Backslash Ctrl-N leaves the chat"
        );
    }
}

fn popup_state(app: &mut App) -> String {
    format!(
        "popup={} field={:?} text={:?} palette={} quick_open={} goto={} shortcuts={} file={:?} pane={:?}",
        super::app_host::popup_open(app),
        app.focused_text_input_id(),
        app.focused_text_for_test(),
        app.palette.active,
        app.quick_open_active,
        app.goto_line_active,
        app.shortcuts_active,
        app.tabs.first().map(|t| t.editor.rope.to_string()),
        app.active_pane,
    )
}

/// Every popup behaves as it does without Vim mode: typing goes into its
/// field, Ctrl-U/W still edit it, Esc closes it, and the editor's Vim
/// state comes back as it was.
#[test]
fn popups_take_keys_as_without_vim_mode() {
    let openers: [(&str, fn(&mut App)); 10] = [
        ("palette", |a| press(a, KeyCode::F(1), KeyModifiers::NONE)),
        ("quick open", |a| {
            press(a, KeyCode::Char('p'), KeyModifiers::CONTROL)
        }),
        ("go to line", |a| a.open_goto_line()),
        ("shortcuts", |a| {
            a.execute_palette_command("show_shortcuts");
        }),
        ("clipboard history", |a| {
            a.push_clipboard_history("copied text");
            a.execute_palette_command("clipboard_history");
        }),
        ("workspaces", |a| crate::ui_workspaces::open(a)),
        ("desk switcher", |a| {
            crate::ui_desks::open(a, crate::ui_desks::Mode::Switch)
        }),
        ("new desk", |a| {
            crate::ui_desks::open(a, crate::ui_desks::Mode::New)
        }),
        ("close unsaved tab", |a| {
            a.tabs[0].editor.modified = true;
            a.tabs.push(Tab::new_file(Editor::test_with_text(
                "other
",
            )));
            a.active_tab = 0;
            a.close_tab(0);
        }),
        ("menu bar", |a| {
            // Opened with the mouse; the keyboard drives it from there.
            a.menu_active = true;
            a.menu_open = true;
        }),
    ];
    let sequences: [(&str, &[(KeyCode, KeyModifiers)]); 4] = [
        (
            "type",
            &[
                (KeyCode::Char('a'), KeyModifiers::NONE),
                (KeyCode::Char(':'), KeyModifiers::NONE),
                (KeyCode::Char('j'), KeyModifiers::NONE),
                (KeyCode::Char('k'), KeyModifiers::NONE),
            ],
        ),
        (
            "esc",
            &[
                (KeyCode::Char('a'), KeyModifiers::NONE),
                (KeyCode::Esc, KeyModifiers::NONE),
            ],
        ),
        (
            "palette key",
            &[(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )],
        ),
        (
            "down enter",
            &[
                (KeyCode::Down, KeyModifiers::NONE),
                (KeyCode::Esc, KeyModifiers::NONE),
            ],
        ),
    ];
    let mut failed = Vec::new();
    for (popup, open) in openers {
        for (seq, strokes) in sequences {
            let mut app = base();
            let mut off = base();
            super::app_host::set_enabled(&mut off, false);
            for a in [&mut app, &mut off] {
                draw(a);
                open(a);
                draw(a);
            }
            let opened = (popup_state(&mut app), popup_state(&mut off));
            if !super::app_host::popup_open(&off) {
                failed.push(format!("{popup}: the test did not open it: {}", opened.1));
                continue;
            }
            if opened.0 != opened.1 {
                failed.push(format!(
                    "{popup}: opening\n   vim:   {}\n   plain: {}",
                    opened.0, opened.1
                ));
                continue;
            }
            for &(code, mods) in strokes {
                press(&mut app, code, mods);
                press(&mut off, code, mods);
                draw(&mut app);
                draw(&mut off);
            }
            let (a, b) = (popup_state(&mut app), popup_state(&mut off));
            if a != b {
                failed.push(format!("{popup}: {seq}\n   vim:   {a}\n   plain: {b}"));
            }
            if !super::app_host::popup_open(&app) && app.vim.mode != super::Mode::Normal {
                failed.push(format!("{popup}: {seq}: Vim left in {:?}", app.vim.mode));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn terminal_tab_app(vim: bool) -> (App, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
    let mut app = base();
    let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "");
    let input = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    app.detach_terminal_to_tab();
    super::app_host::set_enabled(&mut app, vim);
    draw(&mut app);
    (app, input)
}

#[test]
fn a_terminal_tab_gets_keys_and_shortcuts_as_without_vim_mode() {
    let (mut app, input) = terminal_tab_app(true);
    assert!(matches!(app.tabs[app.active_tab].kind, TabKind::Terminal));
    assert!(
        app.focused_terminal_index().is_some(),
        "the tab's terminal has the keyboard"
    );
    keys(&mut app, ":ls jk");
    assert!(app.vim.cmdline.is_none());
    assert_eq!(&*input.lock().unwrap(), b":ls jk", "keys reach the shell");
    for (name, code, mods) in [
        (
            "Ctrl+Shift+P",
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        ("F1", KeyCode::F(1), KeyModifiers::NONE),
        ("Ctrl+P", KeyCode::Char('p'), KeyModifiers::CONTROL),
    ] {
        let (mut app, a_in) = terminal_tab_app(true);
        let (mut off, b_in) = terminal_tab_app(false);
        press(&mut app, code, mods);
        press(&mut off, code, mods);
        assert_eq!(fingerprint(&app), fingerprint(&off), "{name}");
        assert_eq!(
            *a_in.lock().unwrap(),
            *b_in.lock().unwrap(),
            "{name}: same bytes to the shell"
        );
    }
    // Leave it and come back with Vim tab commands; typing reaches the shell again.
    let (mut app, input) = terminal_tab_app(true);
    keys(&mut app, r"<C-\>");
    let after_prefix = (
        app.vim_terminal_prefix,
        app.active_tab,
        app.tab_mru_order.clone(),
    );
    keys(&mut app, "<C-n>");
    assert!(
        app.focused_terminal_index().is_none(),
        "Ctrl-Backslash Ctrl-N leaves the terminal: prefix/tab/mru {after_prefix:?} now tab {} pane {:?} sidebar_focus {} in={:?}",
        app.active_tab,
        app.active_pane,
        app.terminal_panel_focused,
        input.lock().unwrap()
    );
    assert!(!matches!(app.tabs[app.active_tab].kind, TabKind::Terminal));
    input.lock().unwrap().clear();
    let terminal_tab = app
        .tabs
        .iter()
        .position(|t| t.kind == TabKind::Terminal)
        .unwrap();
    keys(&mut app, &format!(":b{}<CR>", terminal_tab + 1));
    keys(&mut app, "echo");
    assert_eq!(
        &*input.lock().unwrap(),
        b"echo",
        ":b N onto the terminal tab: typing reaches the shell"
    );
}

#[test]
fn ctrl_w_moves_from_a_terminal_tab_to_another_editor_pane() {
    let (mut app, input) = terminal_tab_app(true);
    app.show_explorer = false;
    let editor = app.active_group;
    app.create_split_group_at(app.active_tab, crate::app::TabDropZone::Right);
    let terminal = app.active_group;
    assert_ne!(editor, terminal);
    draw(&mut app);

    keys(&mut app, "<C-w>h");
    assert_eq!(app.active_group, editor);
    assert!(app.focused_terminal_index().is_none());
    keys(&mut app, "<C-w>l");
    assert_eq!(app.active_group, terminal);
    assert!(app.focused_terminal_index().is_some());
    keys(&mut app, "<C-w>w");
    assert_eq!(app.active_group, editor);
    assert!(app.focused_terminal_index().is_none());
    assert!(input.lock().unwrap().is_empty());
}

#[test]
fn leaving_the_only_tab_a_terminal_moves_to_the_explorer() {
    let (mut app, _input) = terminal_tab_app(true);
    let terminal = app.active_tab;
    app.tabs.retain(|t| t.kind == TabKind::Terminal);
    app.active_tab = 0;
    let _ = terminal;
    keys(&mut app, r"<C-\><C-n>");
    assert!(app.focused_terminal_index().is_none());
    assert_eq!(app.active_pane, ActivePane::Explorer);
    keys(&mut app, ":");
    assert!(app.vim.cmdline.is_some(), "and ':' works from there");
}

fn full_state(app: &mut App) -> String {
    let tabs: Vec<String> = app
        .tabs
        .iter()
        .map(|t| {
            format!(
                "{:?}:{}:{:?}",
                t.kind,
                t.editor.modified,
                t.editor.rope.to_string()
            )
        })
        .collect();
    format!(
        "{} | popup={} field={:?} text={:?} search={} gsearch={} groups={} term_focus={} sidebar_term={} tabs={tabs:?}",
        fingerprint(app),
        super::app_host::popup_open(app),
        app.focused_text_input_id(),
        app.focused_text_for_test(),
        app.search_active,
        app.global_search_active,
        app.groups.len(),
        app.terminal_panel_focused,
        app.show_terminal_sidebar,
    )
}

/// Every default workbench shortcut, in every view, does what it does
/// without Vim mode — except, in a Vim buffer, the keys Vim itself defines.
/// (Bindings that start a real shell are covered by the terminal tests.)
#[test]
fn every_default_shortcut_matches_plain_mode_in_every_view() {
    let bindings: Vec<_> = crate::app::default_keybindings()
        .into_iter()
        .filter(|b| b.chord.is_none() && !matches!(b.id, "toggle_terminal" | "new_terminal"))
        .map(|b| (b.id, b.modifiers, b.code))
        .collect();
    let threads = 6;
    let chunks: Vec<Vec<_>> = (0..threads)
        .map(|t| bindings.iter().copied().skip(t).step_by(threads).collect())
        .collect();
    let results: Vec<(usize, Vec<String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    std::thread::Builder::new()
                        .stack_size(64 << 20)
                        .spawn(move || shortcut_parity(&chunk))
                        .unwrap()
                        .join()
                        .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let checked: usize = results.iter().map(|r| r.0).sum();
    let failed: Vec<String> = results.into_iter().flat_map(|r| r.1).collect();
    eprintln!("checked {checked} shortcut/view pairs");
    assert!(checked > 1000, "only {checked} pairs ran");
    assert!(
        failed.is_empty(),
        "{} differences:
{}",
        failed.len(),
        failed.join(
            "
"
        )
    );
}

fn shortcut_parity(bindings: &[(&'static str, KeyModifiers, KeyCode)]) -> (usize, Vec<String>) {
    type Views = fn() -> Vec<(&'static str, App)>;
    let sets: [Views; 3] = [views, more_views, || {
        rare_views().into_iter().map(|(n, a, _)| (n, a)).collect()
    }];
    let vim_buffers = ["editor", "git revision", "diff tab", "editor in a split"];
    let opts = super::Options::default();
    let mut failed = Vec::new();
    let mut checked = 0;
    for &(id, modifiers, code) in bindings {
        let event = KeyEvent::new(code, modifiers);
        let vim_key = super::keys::Key::from_event(event);
        for set in sets {
            for ((view, mut app), (_, mut off)) in set().into_iter().zip(set()) {
                if vim_buffers.contains(&view)
                    && !vim_key.is_some_and(|k| super::unclaimed(k, &opts))
                {
                    continue;
                }
                super::app_host::set_enabled(&mut off, false);
                draw(&mut app);
                draw(&mut off);
                crate::input_keyboard::handle_keyboard_event(&mut app, event);
                crate::input_keyboard::handle_keyboard_event(&mut off, event);
                checked += 1;
                // Deliberate Vim behaviour: Ctrl-W starts a window command
                // everywhere, and Ctrl-D/U page through result lists.
                if id == "close_tab"
                    || view == "search results"
                        && matches!(id, "select_next_occurrence" | "cursor_undo")
                {
                    continue;
                }
                let (a, b) = (full_state(&mut app), full_state(&mut off));
                if a != b {
                    failed.push(format!(
                        "{view}: {id} ({modifiers:?} {code:?})
   vim:   {a}
   plain: {b}"
                    ));
                }
            }
        }
    }
    (checked, failed)
}

fn hunk_peek_app(vim: bool) -> App {
    let mut app = base();
    let saved: String = (0..120).map(|i| format!("old {i}\n")).collect();
    let current: String = (0..120).map(|i| format!("new {i}\n")).collect();
    app.hunk_peek =
        Some(crate::ui_hunk_peek::HunkPeek::from_texts(&saved, &current, 0, 4).unwrap());
    app.hunk_popup_active = true;
    app.hunk_popup_status = 'M';
    app.hunk_popup_pos = (10, 4);
    super::app_host::set_enabled(&mut app, vim);
    draw(&mut app);
    app
}

#[test]
fn the_hunk_peek_scrolls_and_closes_with_vim_keys() {
    let state = |app: &App| {
        (
            app.hunk_popup_active,
            app.hunk_peek.as_ref().map(|p| (p.scroll, p.selected)),
        )
    };
    let mut failed = Vec::new();
    for (vim_keys, plain) in [
        ("j", vec![KeyCode::Down]),
        ("jjjk", vec![KeyCode::Down, KeyCode::Down]),
        ("<C-d>", vec![KeyCode::PageDown]),
        ("G", vec![KeyCode::End]),
        ("Ggg", vec![KeyCode::End, KeyCode::Home]),
        ("q", vec![KeyCode::Esc]),
    ] {
        let mut app = hunk_peek_app(true);
        let mut off = hunk_peek_app(false);
        keys(&mut app, vim_keys);
        for code in plain {
            press(&mut off, code, KeyModifiers::NONE);
        }
        if state(&app) != state(&off) {
            failed.push(format!(
                "{vim_keys}: vim {:?} plain {:?}",
                state(&app),
                state(&off)
            ));
        }
        // j and Ctrl-D must visibly move (End/Home settle at draw time).
        if matches!(vim_keys, "j" | "<C-d>") && state(&app) == state(&hunk_peek_app(false)) {
            failed.push(format!("{vim_keys}: nothing moved"));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
