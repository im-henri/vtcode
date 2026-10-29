use crate::{app::App, app_theme::ThemeColors};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Color};

fn palette(colors: serde_json::Value) -> ThemeColors {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("palette.json");
    std::fs::write(&path, serde_json::json!({"name":"Coverage", "colors":colors}).to_string()).unwrap();
    crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark")).unwrap().to_theme_colors()
}

#[test]
fn theme_coverage_extension_charts_layer_on_their_actual_surfaces() {
    use crate::extension_surface::{ViewFrame, ViewSpan};
    let colors = palette(serde_json::json!({
        "editor.background":"#101010", "editor.foreground":"#abcdef",
        "sideBar.background":"#303030", "descriptionForeground":"#ffffff80", "editor.selectionForeground":"#ffffff80",
        "charts.green":"#102030", "charts.blue":"#203040", "charts.yellow":"#304050",
        "charts.purple":"#ffffff80", "charts.red":"#405060", "charts.orange":"#607080",
        "terminal.ansiCyan":"#506070", "terminal.ansiBrightGreen":"#708090"
    }));
    let frame = ViewFrame { lines: vec![
        (1..=8).map(|index| ViewSpan { text: index.to_string(), role: format!("chart{index}"), ..Default::default() }).collect(),
        vec![ViewSpan { text:"m".into(), role:"muted".into(), ..Default::default() },
            ViewSpan { text:"m".into(), role:"muted".into(), background:"panel".into(), ..Default::default() },
            ViewSpan { text:"p".into(), role:"chart4".into(), background:"panel".into(), ..Default::default() },
            ViewSpan { text:"s".into(), role:"selection".into(), background:"panel".into(), ..Default::default() }],
    ], ..Default::default() };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 4));
    crate::extension_surface::render_frame(&mut buffer, Rect::new(1, 1, 10, 3), &frame, &colors);
    for (column, rgb) in [(16,32,48), (32,48,64), (48,64,80), (136,136,136),
        (64,80,96), (80,96,112), (96,112,128), (112,128,144)].into_iter().enumerate() {
        assert_eq!(buffer[(column as u16 + 1, 1)].fg, Color::Rgb(rgb.0, rgb.1, rgb.2));
    }
    assert_eq!(buffer[(1, 2)].fg, Color::Rgb(136,136,136));
    assert_eq!(buffer[(2, 2)].fg, Color::Rgb(152,152,152));
    assert_eq!(buffer[(3, 2)].fg, Color::Rgb(152,152,152));
    assert_eq!(buffer[(4, 2)].fg, Color::Rgb(152,152,152));
    assert_eq!(buffer[(10, 3)].fg, Color::Rgb(171,205,239), "blank cells also use the theme");
    crate::ui_control_visibility_tests::snapshot(&buffer, "coverage extension charts");
}

#[test]
fn theme_coverage_extension_selection_cursor_and_search_have_their_own_colors() {
    use crate::extension_surface::{ViewFrame, ViewSpan};
    let colors = palette(serde_json::json!({
        "editor.background":"#101010", "editor.foreground":"#112233",
        "editor.selectionBackground":"#223344", "editor.selectionForeground":"#ffeedd",
        "editorCursor.foreground":"#334455", "editorCursor.background":"#aabbcc",
        "editor.findMatchHighlightBackground":"#445566", "editor.findMatchHighlightForeground":"#ccbbaa"
    }));
    let frame = ViewFrame { lines: vec![vec![
        ViewSpan { text:"s".into(), role:"muted".into(), background:"selection".into(), ..Default::default() },
        ViewSpan { text:"c".into(), role:"cursor".into(), background:"cursor".into(), ..Default::default() },
        ViewSpan { text:"f".into(), background:"match".into(), ..Default::default() },
    ]], ..Default::default() };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 5, 2));
    crate::extension_surface::render_frame(&mut buffer, Rect::new(0, 0, 5, 2), &frame, &colors);
    for (x, foreground, background) in [
        (0, Color::Rgb(255,238,221), Color::Rgb(34,51,68)),
        (1, Color::Rgb(170,187,204), Color::Rgb(51,68,85)),
        (2, Color::Rgb(204,187,170), Color::Rgb(68,85,102)),
    ] {
        assert_eq!((buffer[(x,0)].fg, buffer[(x,0)].bg), (foreground, background));
    }
}

#[test]
fn theme_coverage_desks_use_quick_input_roles_and_themed_group_colors() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.colors = palette(serde_json::json!({
        "editor.background":"#101010", "editor.foreground":"#111213", "sideBar.background":"#303030",
        "quickInput.background":"#123456", "quickInput.foreground":"#abcdef",
        "quickInputList.focusBackground":"#334455", "quickInputList.focusForeground":"#ffeedd",
        "editorWidget.border":"#667788", "editorGroupHeader.tabsBackground":"#202020",
        "focusBorder":"#ff000080", "charts.blue":"#010203", "charts.green":"#040506",
        "charts.yellow":"#070809", "charts.purple":"#ffffff80", "terminal.ansiCyan":"#0a0b0c"
    }));
    for (slot, expected) in [(144,16,16), (1,2,3), (4,5,6), (7,8,9), (144,144,144), (10,11,12)].into_iter().enumerate() {
        assert_eq!(crate::ui_desks::group_color(&app, slot as u8), Color::Rgb(expected.0, expected.1, expected.2));
    }
    app.new_desk("Theme desk", false);
    assert!(crate::ui_desks::command(&mut app, "desk_switch"));
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| crate::ui_desks::draw(frame, &mut app)).unwrap();
    let area = app.desks.popup.area;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(area.x,area.y)].fg, Color::Rgb(102,119,136));
    assert_eq!(buffer[(area.x + 1,area.y)].bg, Color::Rgb(18,52,86));
    assert_eq!(buffer[(area.x + 2,area.y + 2)].bg, Color::Rgb(51,68,85));
    assert_eq!(buffer[(area.x + 2,area.y + 2)].fg, Color::Rgb(255,238,221));
    crate::ui_control_visibility_tests::snapshot(buffer, "coverage desks quick input");
}

#[test]
fn theme_coverage_problem_icons_are_independent_of_editor_diagnostics() {
    use lsp_types::DiagnosticSeverity as Severity;
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.colors = palette(serde_json::json!({
        "foreground":"#abcdef", "editor.background":"#101010", "panel.background":"#202020", "editorError.foreground":"#ff0000",
        "problemsErrorIcon.foreground":"#ffffff80", "problemsWarningIcon.foreground":"#00ff00",
        "problemsInfoIcon.foreground":"#0000ff"
    }));
    app.bg_problems_analysis_running = true;
    app.lsp_manager.diagnostics.insert(root.path().join("source.rs"),
        [Severity::ERROR, Severity::WARNING, Severity::INFORMATION].into_iter().enumerate().map(|(line, severity)|
            crate::lsp::LspDiagnostic { line, col:0, end_line:line, end_col:1, severity, message:"A diagnostic".into() }).collect());
    app.lsp_manager.diagnostics_version += 1;
    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal.draw(|frame| crate::ui_panels::draw_panel_problems(frame, &mut app, frame.area())).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(app.colors.ui.diagnostic(Severity::ERROR), Color::Rgb(255,0,0));
    assert_eq!(buffer[(1,2)].fg, Color::Rgb(144,144,144));
    assert_eq!(buffer[(1,3)].fg, Color::Rgb(0,255,0));
    assert_eq!(buffer[(1,4)].fg, Color::Rgb(0,0,255));
    for row in 2..=4 { assert_eq!(buffer[(3,row)].fg, Color::Rgb(171,205,239)); }
    crate::ui_control_visibility_tests::snapshot(buffer, "coverage problem icon roles");
}

#[test]
fn theme_coverage_translucent_diagnostics_follow_gutter_and_lens_surfaces() {
    let colors = palette(serde_json::json!({"editor.background":"#000000", "editorError.foreground":"#ffffff80"}));
    let severity = lsp_types::DiagnosticSeverity::ERROR;
    assert_eq!(colors.ui.diagnostic(severity), Color::Rgb(128,128,128));
    assert_eq!(colors.ui.diagnostic_on(severity, Color::Rgb(32,32,32)), Color::Rgb(144,144,144));
    assert_eq!(colors.ui.diagnostic_on(severity, Color::Rgb(64,64,64)), Color::Rgb(160,160,160));
}

#[test]
fn theme_coverage_terminal_reverse_video_resolves_defaults_after_theme_changes() {
    let mut grid = crate::terminal::TermGrid::new(12, 2);
    let mut parser = vte::Parser::new();
    for &byte in b"\x1b[7mA\x1b[31mB\x1b[39mC\x1b[27mD\x1b[31;44;7m\x1b7\x1b[0m\x1b8E" {
        parser.advance(&mut grid, byte);
    }
    let generation = grid.cells[0].generation();
    let mut rows = crate::ui_terminal_rows::RowCache::default();
    for key in ["dark", "light", "catppuccin-mocha", "github-dark-high-contrast"] {
        let colors = crate::app_theme_loader::builtin_theme(key).unwrap().to_theme_colors();
        let mut buffer = Buffer::empty(Rect::new(0,0,12,1));
        rows.paint(&mut buffer, &grid, Rect::new(0,0,12,1), &colors.ui);
        assert_eq!((buffer[(0,0)].fg, buffer[(0,0)].bg), (colors.ui.bg_terminal, colors.ui.fg_terminal), "{key}");
        assert_eq!((buffer[(1,0)].fg, buffer[(1,0)].bg), (colors.ui.bg_terminal, colors.ui.ansi[1]), "{key}");
        assert_eq!((buffer[(2,0)].fg, buffer[(2,0)].bg), (colors.ui.bg_terminal, colors.ui.fg_terminal), "{key}");
        assert_eq!((buffer[(3,0)].fg, buffer[(3,0)].bg), (colors.ui.fg_terminal, colors.ui.bg_terminal), "{key}");
        assert_eq!((buffer[(4,0)].fg, buffer[(4,0)].bg), (colors.ui.ansi[4], colors.ui.ansi[1]), "{key}");
        assert_eq!(grid.cells[0].generation(), generation, "theme changes must not rewrite terminal content");
    }
}

#[test]
fn theme_coverage_ssh_palette_uses_saved_builtin_installed_and_custom_colors() {
    use crate::settings::Settings;
    let base = tempfile::tempdir().unwrap();
    let mut settings = Settings { theme:Some("catppuccin-mocha".into()), ..Default::default() };
    settings.workbench_color_customizations = serde_json::from_value(serde_json::json!({
        "terminal.background":"#123456", "[Catppuccin Mocha]":{"terminal.foreground":"#abcdef"}
    })).unwrap();
    let colors = crate::startup::connection_colors(&settings, base.path());
    assert_eq!(colors.ui.bg_terminal, Color::Rgb(18,52,86));
    assert_eq!(colors.ui.fg_terminal, Color::Rgb(171,205,239));
    let extension = base.path().join(".vtcode/theme-extensions/audit");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(extension.join("package.json"), r#"{"publisher":"audit","name":"connection","contributes":{"themes":[{"label":"Connection","uiTheme":"vs-dark","path":"theme.json"}]}}"#).unwrap();
    std::fs::write(extension.join("theme.json"), r##"{"colors":{"terminal.background":"#112233","terminal.foreground":"#ddeeff"}}"##).unwrap();
    let imported = crate::vscode_theme::load_extension_themes(&base.path().join(".vtcode/theme-extensions")).remove(0);
    settings.theme = Some(imported.config_key.clone());
    settings.workbench_color_customizations.clear();
    let colors = crate::startup::connection_colors(&settings, base.path());
    assert_eq!(colors.ui.bg_terminal, Color::Rgb(17,34,51));
    assert_eq!(colors.ui.fg_terminal, Color::Rgb(221,238,255));
    assert_eq!(settings.theme.as_deref(), Some(imported.config_key.as_str()));
}
