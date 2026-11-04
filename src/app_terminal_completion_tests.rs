use super::*;
use crate::terminal::TerminalEmulator;
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> (App, Vec<std::sync::mpsc::Sender<()>>) {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("document"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.terminal_has_focus = true;
    app.show_explorer = false;
    app.terminals.clear();
    let mut exits = Vec::new();
    for name in ["Shell", "Build"] {
        let mut pty = TerminalEmulator::test_with_output(80, 24, "");
        exits.push(pty.test_keep_running());
        let mut terminal = TerminalTab::new(name.into(), app.explorer.root.clone());
        terminal.terminal_pty = Some(pty);
        app.terminals.push(terminal);
    }
    app.active_terminal = 0;
    app.show_terminal_sidebar = true;
    app.terminal_sidebar_revealed = true;
    app.terminal_panel_focused = true;
    (app, exits)
}

fn finish(app: &mut App, index: usize, code: i32) {
    app.terminals[index].terminal_pty.as_ref().unwrap()
        .test_queue_output(format!("\x1b]633;C\x07\x1b]633;D;{code}\x07").as_bytes());
    assert!(app.process_terminals(), "a hidden command completion must redraw its tab");
}

fn draw(app: &mut App) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(180, 42)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn terminal_completion_dock_badge_survives_blink_and_clears_on_visit() {
    let (mut app, _exits) = fixture();
    draw(&mut app);
    let tabs_before = app.terminal_sidebar_tab_areas.clone();
    let closes_before = app.terminal_sidebar_close_areas.clone();
    finish(&mut app, 1, 0);
    assert!(app.terminals[1].completion.unread);
    app.advance_terminal_completions(Instant::now() + Duration::from_secs(2));
    let buffer = draw(&mut app);
    let rect = app.terminal_sidebar_tab_areas.iter().find(|(_, index)| *index == 1).unwrap().0;
    let label: String = (rect.x..rect.right()).map(|x| buffer[(x, rect.y)].symbol()).collect();
    assert_eq!(label, "● Build ");
    assert_eq!(app.terminal_sidebar_tab_areas, tabs_before);
    assert_eq!(app.terminal_sidebar_close_areas, closes_before);
    assert_eq!(app.terminals[1].name, "Build");
    assert!(app.next_terminal_completion_wait(Instant::now() + Duration::from_secs(3)).is_none());
    crate::mouse::handle_mouse(&mut app, crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: rect.x + 2, row: rect.y, modifiers: crossterm::event::KeyModifiers::NONE,
    });
    draw(&mut app);
    assert_eq!(app.active_terminal, 1);
    assert!(!app.terminals[1].completion.unread);
    assert!(!app.terminals[1].completion.visible(Instant::now()));
}

#[test]
fn terminal_completion_editor_badge_preserves_tab_and_close_hit_targets() {
    let (mut app, _exits) = fixture();
    app.detach_terminal_to_group(1, app.active_group);
    app.active_tab = 0;
    app.terminal_panel_focused = false;
    draw(&mut app);
    let before_area = app.tab_bar_area.unwrap();
    let before_hits: Vec<_> = (before_area.x..before_area.right())
        .map(|column| crate::mouse::find_tab_at_col(&app, column, before_area)).collect();
    finish(&mut app, 1, 2);
    app.advance_terminal_completions(Instant::now() + Duration::from_secs(2));
    let buffer = draw(&mut app);
    let area = app.tab_bar_area.unwrap();
    let label: String = (area.x..area.right()).map(|x| buffer[(x, area.y)].symbol()).collect();
    assert!(label.contains("●  Build"), "{label}");
    assert!(app.terminals[1].completion.failed());
    let after_hits: Vec<_> = (area.x..area.right())
        .map(|column| crate::mouse::find_tab_at_col(&app, column, area)).collect();
    assert_eq!(before_hits, after_hits);
    app.active_tab = app.tabs.iter().position(|tab| tab.terminal_idx == Some(1)).unwrap();
    draw(&mut app);
    assert!(!app.terminals[1].completion.visible(Instant::now()));
}

#[test]
fn terminal_completion_watching_other_panes_and_window_focus_are_independent() {
    let (mut app, _exits) = fixture();
    finish(&mut app, 0, 0);
    assert!(!app.terminals[0].completion.visible(Instant::now()), "the focused terminal needs no completion alert");
    app.terminal_panel_focused = false;
    finish(&mut app, 0, 0);
    assert!(app.terminals[0].completion.unread, "editing a file must leave the terminal unread");
    app.terminal_panel_focused = true;
    app.terminal_has_focus = false;
    finish(&mut app, 1, 1);
    app.active_terminal = 1;
    draw(&mut app);
    assert!(app.terminals[1].completion.unread, "an unfocused application must not acknowledge a command");
    app.terminal_has_focus = true;
    draw(&mut app);
    assert!(!app.terminals[1].completion.unread);
}

#[test]
fn focused_terminal_successes_and_failures_never_show_a_badge_or_blink() {
    for editor_tab in [false, true] {
        for code in [0, 17] {
            let (mut app, _exits) = fixture();
            if editor_tab { assert!(app.detach_terminal_to_group(0, app.active_group)); }
            draw(&mut app);
            finish(&mut app, 0, code);
            let now = Instant::now();
            assert!(!app.terminals[0].completion.unread);
            assert!(!app.terminals[0].completion.visible(now));
            assert!(!app.terminals[0].completion.pulse(now, true));
            assert_eq!(app.next_terminal_completion_wait(now), None);
            assert_eq!(app.terminals[0].completion.failed(), code != 0);
            let buffer = draw(&mut app);
            let header = if editor_tab { app.tab_bar_area.unwrap() }
                else { app.terminal_sidebar_tab_areas.iter().find(|(_, index)| *index == 0).unwrap().0 };
            let label: String = (header.x..header.right()).map(|x| buffer[(x, header.y)].symbol()).collect();
            assert!(label.contains("Shell"), "{label}");
            assert!(!label.contains('●'), "{label}");
            let background = app.colors.ui.bg_panel;
            assert_eq!(crate::ui_chrome_tabs::terminal_completion_background(&app, Some(0), background, now), background);
            if editor_tab { app.active_tab = 0; }
            else { app.terminal_panel_focused = false; }
            draw(&mut app);
            assert!(!app.terminals[0].completion.visible(Instant::now()), "leaving the terminal must not resurrect an acknowledged command");
            assert_eq!(app.next_terminal_completion_wait(Instant::now()), None);
        }
    }
}

#[test]
fn terminal_completion_waits_for_terminal_focus_while_a_dialog_owns_input() {
    let (mut app, _exits) = fixture();
    app.begin_terminal_rename(0);
    assert!(app.focused_text_input_id().is_some());
    finish(&mut app, 0, 0);
    draw(&mut app);
    assert!(app.terminals[0].completion.unread);
    app.cancel_terminal_rename();
    draw(&mut app);
    assert!(!app.terminals[0].completion.unread);
}
