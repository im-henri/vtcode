use super::*;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{backend::TestBackend, buffer::Buffer, style::Modifier, Terminal};

fn row_text(buffer: &Buffer, area: Rect) -> String {
    (area.x..area.right()).map(|x| buffer[(x, area.y)].symbol()).collect()
}

#[test]
fn a_sole_repository_cannot_collapse_and_closing_another_reveals_its_details() {
    for right in [false, true] {
        let mut fixture = Fixture::new();
        let app = &mut fixture.app;
        app.sidebar_right = right;
        app.source_control_commit_msg = "keep this draft".into();
        draw(app, 48, 28);
        let header = app.repositories.headers.iter().find(|(_, root)| root == &fixture.root).unwrap().0;
        click(app, header);
        assert!(!app.repositories.expanded, "multiple repositories can still collapse");
        draw(app, 48, 28);
        assert!(app.source_control_commit_area.is_none());
        let close = app.repositories.closes.iter().find(|(_, root)| root == &fixture.nested).unwrap().0;
        click(app, close);
        assert!(app.repositories.expanded, "closing an inactive project must reveal the remaining project");
        wait(app, |app| !app.bg_git_status_running && !app.bg_git_status_pending);
        for _ in 0..3 {
            draw(app, 48, 28);
            assert_eq!(app.repositories.headers.len(), 1);
            assert!(app.source_control_commit_area.is_some());
            let header = app.repositories.headers[0].0;
            click(app, header);
            assert!(app.repositories.expanded);
            assert_eq!(app.source_control_root(), fixture.root);
            assert_eq!(app.source_control_commit_msg, "keep this draft");
        }
        let screen = draw(app, 48, 28);
        assert!(!screen.contains("Active"));
        let mut terminal = Terminal::new(TestBackend::new(48, 28)).unwrap();
        terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, app, frame.area())).unwrap();
        let header = row_text(terminal.backend().buffer(), app.repositories.headers[0].0);
        assert!(!header.contains('▾') && !header.contains('▸'), "a sole repository must not advertise collapsing: {header:?}");
        crate::ui_source_control::handle_key(app, KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(app.source_control_input_mode, "keyboard input must still reach the details");
        let close = app.repositories.closes[0].0;
        click(app, close);
        assert!(app.repositories.active.is_none(), "closing the sole repository remains available");
    }
}

#[test]
fn discovery_removing_an_inactive_repository_reveals_the_collapsed_remaining_project() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    draw(app, 48, 28);
    let header = app.repositories.headers.iter().find(|(_, root)| root == &fixture.root).unwrap().0;
    click(app, header);
    assert!(!app.repositories.expanded);
    assert!(app.apply_repository_scan(fixture.root.clone(), app.workspace_generation,
        Discovery { roots: vec![fixture.root.clone()], parent: None, workspace: fixture.root.clone() }, Vec::new()));
    assert!(app.repositories.expanded);
    draw(app, 48, 28);
    assert_eq!(app.repositories.headers.len(), 1);
    assert!(app.source_control_commit_area.is_some());
}

#[test]
fn active_repository_is_distinct_when_collapsed_unfocused_narrow_or_switched_with_the_keyboard() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    // Give the theme a visibly distinct selected row and badge. The same roles
    // must survive collapsing the details, editor focus, and a narrow sidebar.
    app.colors.ui.list_active_background = crate::theme_workbench::ColorLayer::rgba(20, 70, 120, 255);
    app.colors.ui.badge_background = crate::theme_workbench::ColorLayer::rgba(30, 90, 150, 255);
    for right in [false, true] {
        app.sidebar_right = right;
        for width in [20, 40, 80] {
            for expanded in [true, false] {
                app.repositories.expanded = expanded;
                app.active_pane = crate::app::ActivePane::Editor;
                let mut terminal = Terminal::new(TestBackend::new(width, 28)).unwrap();
                terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, app, frame.area())).unwrap();
                let buffer = terminal.backend().buffer();
                for (area, root) in &app.repositories.headers {
                    let text = row_text(buffer, *area);
                    let active = Some(root) == app.repositories.active.as_ref();
                    assert_eq!(text.starts_with('▌'), active, "{text:?}");
                    assert_eq!(buffer[(area.x + 2, area.y)].modifier.contains(Modifier::BOLD), active);
                    if active {
                        assert_eq!(buffer[(area.x + 2, area.y)].bg, ratatui::style::Color::Rgb(20, 70, 120));
                        assert!(text.contains(if expanded { '▾' } else { '▸' }));
                        if width >= 40 { assert!(text.contains("Active"), "{text:?}"); }
                    } else { assert!(!text.contains("Active")); }
                    let close = app.repositories.closes.iter().find(|(_, close_root)| close_root == root).unwrap().0;
                    assert_eq!(buffer[(close.x, close.y)].bg, buffer[(area.x + 2, area.y)].bg);
                    assert!(close.right() <= width);
                }
            }
        }
    }
    crate::ui_source_control::handle_key(app, KeyCode::Down, KeyModifiers::ALT);
    assert_eq!(app.source_control_root(), fixture.nested);
    let mut terminal = Terminal::new(TestBackend::new(48, 28)).unwrap();
    terminal.draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, app, frame.area())).unwrap();
    for (area, root) in &app.repositories.headers {
        let text = row_text(terminal.backend().buffer(), *area);
        assert_eq!(text.contains("Active"), root == &fixture.nested, "{text:?}");
    }
    let active = app.repositories.headers.iter().find(|(_, root)| root == &fixture.nested).unwrap().0;
    app.mouse_col = active.x;
    app.mouse_row = active.y;
    terminal.draw(|frame| crate::ui_source_control::draw_action_tooltip(frame, app)).unwrap();
    let tooltip: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(tooltip.contains("Active repository"));
    assert!(tooltip.contains("Click to collapse"));
}
