use crate::app::{App, Tab};
use crate::background::BackgroundResult;
use crate::editor::Editor;
use ratatui::{backend::TestBackend, style::Color, Terminal};
use std::{path::PathBuf, time::Duration};

fn startup_file() -> (tempfile::TempDir, App, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("startup.c");
    let text = (0..40)
        .map(|line| format!("int value_{line} = {line};\n"))
        .collect::<String>();
    std::fs::write(&path, &text).unwrap();
    let mut app = App::new_deferred(root.path().into());
    // These tests isolate file/theme arrival after the startup registry loads.
    // The deferred constructor itself is covered by startup performance tests.
    app.highlighter = crate::highlight::Highlighter::new();
    app.install_external_themes(crate::app_theme::load_builtin_themes());
    let mut editor = Editor::new_loading(&path, 0);
    editor.pending_cursor_position = Some((31, 0));
    app.tabs = vec![Tab::new_file(editor)];
    app.terminal_has_focus = true;
    assert!(app.set_theme_by_key("dark"));
    (root, app, path, text)
}

fn load(app: &mut App, path: &PathBuf, text: &str) {
    app.background
        .sender
        .send(BackgroundResult::FileLoaded {
            workspace_generation: 0,
            path: path.clone(),
            rope: ropey::Rope::from_str(text),
            encoding: "UTF-8",
            file_size: text.len() as u64,
            last_disk_modified: None,
            session_restores: Vec::new(),
            skip_open_prompt: true,
        })
        .unwrap();
    assert!(app.poll_background_results());
    app.refresh_visible_highlights_async();
}

fn settle(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
}

fn color(app: &mut App) -> Option<Color> {
    let editor = app.editor().unwrap();
    let path = editor.file_path.clone();
    let language = editor.syntax_language_token().map(str::to_string);
    let version = editor.content_version;
    app.highlighter
        .highlighted_line_by_index_if_cached(
            31,
            path.as_deref(),
            language.as_deref(),
            &app.syntax_theme,
            version,
        )
        .and_then(|(spans, _)| spans.first().map(|(style, _)| style.foreground))
        .map(|c| Color::Rgb(c.r, c.g, c.b))
}

fn assert_rendered(app: &mut App, expected: Color) {
    app.show_minimap = false;
    app.sticky_scroll = false;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            crate::ui_editor::draw_editor(frame, app, area);
        })
        .unwrap();
    let editor = app.editor().unwrap();
    let code = app.code_area.unwrap();
    let row = code.y + (editor.cursor_line - editor.scroll_offset) as u16;
    // The first character may carry the block caret's inverted colors.
    let cell = terminal.backend().buffer().cell((code.x + 1, row)).unwrap();
    assert_eq!(cell.symbol(), "n");
    assert_eq!(cell.fg, expected);
    assert_ne!(cell.fg, app.colors.fg_text);
}

fn changed_unselected_theme(app: &App) -> crate::app_theme::ThemeFile {
    let mut theme = app
        .loaded_themes
        .iter()
        .find(|theme| theme.config_key == "monokai")
        .unwrap()
        .clone();
    theme.name = "Updated unselected theme".into();
    theme.textmate_theme=Some(crate::textmate::Theme::new("updated","#0c2238","#272822",&theme.token_colors).unwrap());
    theme
}

#[test]
fn startup_highlights_loaded_file_at_restored_cursor_while_idle() {
    let (_root, mut app, path, text) = startup_file();
    app.refresh_visible_highlights_async();
    assert!(app.visible_highlight_key.is_none());
    app.idle_quiescent = true;
    load(&mut app, &path, &text);
    settle(&mut app);
    assert_eq!(app.editor().unwrap().cursor_line, 31);
    let expected = color(&mut app).expect("loaded file needs syntax colors without switching tabs");
    assert_rendered(&mut app, expected);
    assert!(app.idle_quiescent);
}

#[test]
fn startup_theme_catalog_preserves_open_file_colors_when_unselected_theme_changes() {
    let (_root, mut app, path, text) = startup_file();
    load(&mut app, &path, &text);
    settle(&mut app);
    let expected = color(&mut app).unwrap();
    let external = changed_unselected_theme(&app);
    app.idle_quiescent = true;
    app.background
        .sender
        .send(BackgroundResult::ExternalThemesLoaded(vec![external]))
        .unwrap();
    app.poll_background_results();
    assert_eq!(
        color(&mut app),
        Some(expected),
        "catalog discovery must retain the selected theme's colors"
    );
    app.refresh_visible_highlights_async();
    settle(&mut app);
    assert_rendered(&mut app, expected);
    assert!(app.idle_quiescent);
}

#[test]
fn startup_theme_catalog_reschedules_in_flight_highlighting_without_tab_switch() {
    let (_root, mut app, path, text) = startup_file();
    app.editor_mut().unwrap().apply_bg_loaded(
        ropey::Rope::from_str(&text),
        "UTF-8",
        text.len() as u64,
        u64::MAX,
    );
    app.editor_mut().unwrap().set_cursor(31, 0);
    let rope = app.editor().unwrap().rope.clone();
    let language = app
        .editor()
        .unwrap()
        .syntax_language_token()
        .map(str::to_string);
    let job = app
        .highlighter
        .prepare_job(
            &rope,
            &path,
            0,
            Some((0, rope.len_lines())),
            0..rope.len_lines(),
            language.as_deref(),
            &app.syntax_theme,
        )
        .unwrap();
    app.editor_mut().unwrap().syntax_dirty_from = None;
    app.editor_mut().unwrap().syntax_dirty_through = None;
    app.visible_highlight_key = Some((
        path,
        0,
        0,
        rope.len_lines(),
        app.syntax_theme.clone(),
        language,
        0,
    ));
    let (syntaxes, themes) = app.highlighter.shared_sets();
    let batch = job.run(&rope, &syntaxes, &themes, || false);
    let external = changed_unselected_theme(&app);
    app.idle_quiescent = true;
    app.background
        .sender
        .send(BackgroundResult::ExternalThemesLoaded(vec![external]))
        .unwrap();
    app.poll_background_results();
    app.background
        .sender
        .send(BackgroundResult::HighlightBatch(batch))
        .unwrap();
    app.poll_background_results();
    app.refresh_visible_highlights_async();
    settle(&mut app);
    let expected = color(&mut app)
        .expect("the rejected old worker must have a replacement without user input");
    assert_rendered(&mut app, expected);
    assert!(app.idle_quiescent);
}
