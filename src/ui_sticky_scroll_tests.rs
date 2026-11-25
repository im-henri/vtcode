use super::*;
use crate::background::StickyScopeLine;
use crate::{app::Tab, editor::Editor};
use ratatui::{Terminal, backend::TestBackend};

fn scope(line: usize, parent: Option<usize>, end: usize) -> StickyScopeLine {
    StickyScopeLine { line, parent, end }
}

/// Rows of an unwrapped, unfolded viewport whose first line is `top`.
fn rows(top: usize, count: usize) -> Vec<(usize, bool)> {
    (top..top + count).map(|line| (line, true)).collect()
}

/// `a` holds lines 1..=20, with `b` (2..=10) and `c` (12..=18) inside.
fn nested() -> Vec<StickyScopeLine> {
    vec![
        scope(0, None, 20),
        scope(2, Some(0), 10),
        scope(12, Some(0), 18),
    ]
}

fn headers(top: usize) -> Vec<usize> {
    select_sticky_scopes(&nested(), &rows(top, 4), 3)
        .into_iter()
        .map(|index| nested()[index].line)
        .collect::<Vec<_>>()
}

#[test]
fn headers_follow_vs_code_and_leave_when_their_block_ends() {
    assert_eq!(headers(0), Vec::<usize>::new());
    // The opener itself scrolled under the first slot.
    assert_eq!(headers(1), [0]);
    assert_eq!(headers(3), [0, 2]);
    // Its last body line reaches the second slot: still shown.
    assert_eq!(headers(9), [0, 2]);
    // Its closing line would sit under the header: pushed off.
    assert_eq!(headers(10), [0]);
    assert_eq!(headers(13), [0, 12]);
    // The old renderer kept `c` here, over code after its closing brace.
    assert_eq!(headers(18), [0]);
    assert_eq!(headers(20), [0]);
    // Past the end of `a`, nothing sticks at all.
    assert_eq!(headers(21), Vec::<usize>::new());
}

#[test]
fn headers_respect_the_limit_and_wrapped_or_missing_rows() {
    let deep: Vec<_> = (0..6)
        .map(|depth| scope(depth, depth.checked_sub(1), 100 - depth))
        .collect();
    assert_eq!(select_sticky_scopes(&deep, &rows(50, 4), 3), [0, 1, 2]);
    // The second segment of a wrapped opener is already "below" it.
    let wrapped = [(2, false), (3, true), (4, true)];
    assert_eq!(select_sticky_scopes(&nested(), &wrapped, 2), [0, 1]);
    // Near the end of the file there may be fewer rows than headers.
    assert_eq!(select_sticky_scopes(&nested(), &rows(5, 1), 3), [0]);
    assert!(select_sticky_scopes(&nested(), &[], 3).is_empty());
    assert_eq!(sticky_header_limit(5, 12), 3);
    assert_eq!(sticky_header_limit(5, 4), 1);
    assert_eq!(sticky_header_limit(5, 3), 0);
}

fn sticky_app(text: &str, scopes: Vec<StickyScopeLine>) -> App {
    let mut app = App::test_new();
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(std::path::PathBuf::from("sticky.rs"));
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 0;
    app.active_pane = crate::app::ActivePane::Editor;
    app.show_minimap = false;
    app.sticky_scroll = true;
    app.cached_sticky_scopes = scopes.into();
    app
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    let area = Rect::new(0, 0, 70, 14);
    terminal
        .draw(|frame| draw_editor(frame, app, area))
        .unwrap();
}

#[test]
fn rendered_headers_end_with_their_block_and_clear_at_the_top() {
    let text = format!(
        "fn first() {{\n{}}}\n{}fn second() {{\n{}}}\n",
        "    one();\n".repeat(10),
        "const AFTER: u8 = 0;\n".repeat(10),
        "    two();\n".repeat(10),
    );
    // first: 0..=10 (`}` at 11), plain code 12..=21, second: 22..=32.
    let mut app = sticky_app(&text, vec![scope(0, None, 10), scope(22, None, 32)]);
    let mut terminal = Terminal::new(TestBackend::new(70, 14)).unwrap();
    app.tabs[0].editor.scroll_offset = 5;
    draw(&mut app, &mut terminal);
    assert_eq!(app.cached_sticky_lines.len(), 1);
    assert_eq!(app.cached_sticky_lines[0].0, 0);
    // Scrolled into the plain code after `first`: nothing may stick.
    app.tabs[0].editor.scroll_offset = 14;
    draw(&mut app, &mut terminal);
    assert!(
        app.cached_sticky_lines.is_empty(),
        "{:?}",
        app.cached_sticky_lines
    );
    app.tabs[0].editor.scroll_offset = 25;
    draw(&mut app, &mut terminal);
    assert_eq!(app.cached_sticky_lines[0].0, 22);
    // Back at the top the stale header must not linger for hover/ghost text.
    app.tabs[0].editor.scroll_offset = 0;
    draw(&mut app, &mut terminal);
    assert!(app.cached_sticky_lines.is_empty());
}

#[test]
fn allman_openers_show_the_declaration_above_them() {
    let text = format!("fn allman()\n{{\n{}}}\n", "    body();\n".repeat(20));
    let mut app = sticky_app(&text, vec![scope(1, None, 21)]);
    let mut terminal = Terminal::new(TestBackend::new(70, 14)).unwrap();
    app.tabs[0].editor.scroll_offset = 6;
    draw(&mut app, &mut terminal);
    // The lone `{` says nothing; the declaration above it is shown instead.
    assert_eq!(app.cached_sticky_lines.len(), 1);
    assert_eq!(app.cached_sticky_lines[0].1, "fn allman()");
}

#[test]
fn clicking_a_header_goes_to_its_line() {
    let text = format!("fn outer() {{\n{}}}\n", "    work();\n".repeat(40));
    let mut app = sticky_app(&text, vec![scope(0, None, 40)]);
    let mut terminal = Terminal::new(TestBackend::new(70, 14)).unwrap();
    app.tabs[0].editor.scroll_offset = 20;
    app.tabs[0].editor.cursor_line = 25;
    draw(&mut app, &mut terminal);
    assert_eq!(app.cached_sticky_lines.len(), 1);
    let code = app.code_area.unwrap();
    assert_eq!(
        crate::mouse_selection::sticky_header_at(&app, code.x + 3, code.y),
        Some(0)
    );
    assert_eq!(
        crate::mouse_selection::sticky_header_at(&app, code.x + 3, code.y + 1),
        None
    );
    crate::mouse::handle_mouse(
        &mut app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: code.x + 3,
            row: code.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
    );
    let editor = &app.tabs[0].editor;
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 0));
    assert!(editor.scroll_offset < 20);
}

#[test]
#[ignore = "manual optimized measurement of sticky scope analysis and header selection"]
fn benchmark_sticky_scopes_on_real_source() {
    use std::time::{Duration, Instant};
    let source = include_str!("ui_editor.rs").repeat(16);
    let rope = ropey::Rope::from_str(&source);
    let openers = source
        .lines()
        .filter(|line| {
            let line = line.trim_end();
            !line.trim().is_empty() && line.ends_with(['{', '(', '[', ':'])
        })
        .count();
    let mut cache = crate::app_event_loop::content_analysis::Cache::default();
    let deadline = || Instant::now() + Duration::from_secs(30);
    let started = Instant::now();
    let scopes = cache.analyze(1, &rope, deadline(), &|| false).unwrap().4;
    let cold = started.elapsed();
    let mut edited = rope.clone();
    edited.insert(
        edited.line_to_char(rope.len_lines() / 2),
        "    let added = 1;\n",
    );
    let started = Instant::now();
    cache.analyze(1, &edited, deadline(), &|| false).unwrap();
    let local = started.elapsed();
    let started = Instant::now();
    crate::app_event_loop::analyze_editor_content(&rope);
    let full = started.elapsed();
    let started = Instant::now();
    let mut shown = 0;
    for top in 0..rope.len_lines() {
        let rows: Vec<_> = (top..top + 6).map(|line| (line, true)).collect();
        shown += select_sticky_scopes(&scopes, &rows, 5).len();
    }
    let per_frame = started.elapsed() / rope.len_lines() as u32;
    eprintln!(
        "sticky scopes: lines={} openers={openers} scopes={} reference={full:?} cold={cold:?} local_edit={local:?} select_per_frame={per_frame:?} shown={shown}",
        rope.len_lines(),
        scopes.len(),
    );
}
