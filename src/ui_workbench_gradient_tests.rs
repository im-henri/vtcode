use super::*;
use crate::{app::Tab, app_theme::ThemeGradientSpec, editor::Editor};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};
use std::time::Instant;

fn paint(area: Rect, gradient: ThemeGradientSpec, reference: bool) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let background = Color::Rgb(20, 25, 30);
    terminal
        .draw(|frame| {
            for (index, cell) in frame.buffer_mut().content.iter_mut().enumerate() {
                cell.set_char('x')
                    .set_fg(Color::Yellow)
                    .set_style(Style::default().add_modifier(Modifier::ITALIC));
                cell.set_bg(if index % 3 == 0 {
                    Color::Blue
                } else {
                    background
                });
            }
            if reference {
                apply_area_gradient_reference(frame, area, background, gradient);
            } else {
                apply_area_gradient(frame, area, background, gradient);
            }
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn workbench_gradient_tiles_match_original_colors_and_preserve_overlays() {
    for area in [
        Rect::new(0, 0, 80, 20),
        Rect::new(12, 5, 70, 20),
        Rect::new(95, 28, 30, 10),
        Rect::new(2, 2, 300, 300),
        Rect::default(),
    ] {
        for angle_deg in [0.0, 35.0, 90.0, 179.0, -45.0] {
            for end in [
                Color::Rgb(60, 80, 95),
                Color::Rgb(20, 25, 30),
                Color::Indexed(42),
            ] {
                let gradient = ThemeGradientSpec {
                    start: Color::Rgb(20, 25, 30),
                    end,
                    angle_deg,
                };
                assert_eq!(paint(area, gradient, false), paint(area, gradient, true));
            }
        }
    }
}

fn fixture() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(&"let sample = 42; // 世界\n".repeat(200));
    editor.file_path = Some("workspace/example.rs".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.show_explorer = true;
    app.show_status_bar = true;
    app.show_menu_bar = true;
    app.colors.gradients.bg_editor = ThemeGradientSpec {
        start: app.colors.bg_editor,
        end: Color::Rgb(40, 65, 80),
        angle_deg: 35.0,
    };
    app.colors.gradients.bg_sidebar = ThemeGradientSpec {
        start: app.colors.bg_sidebar,
        end: Color::Rgb(40, 30, 70),
        angle_deg: 70.0,
    };
    app
}

#[test]
fn workbench_theme_resize_and_input_frames_keep_identical_cells() {
    let mut app = fixture();
    for (width, height) in [(180, 60), (80, 24), (28, 8), (200, 70)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        for step in 0..4 {
            match step {
                1 => app.editor_mut().unwrap().cursor_line += 5,
                2 => app.editor_mut().unwrap().selection_anchor = Some((0, 0)),
                3 => app.colors.gradients.bg_sidebar.end = Color::Rgb(70, 40, 10),
                _ => {}
            }
            REFERENCE_WORKBENCH_GRADIENT.set(true);
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let expected = terminal.backend().buffer().clone();
            REFERENCE_WORKBENCH_GRADIENT.set(false);
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(*terminal.backend().buffer(), expected);
        }
    }
}

#[test]
#[ignore = "manual alternating full-workbench gradient redraw comparison"]
fn benchmark_workbench_gradient_redraw() {
    let mut app = fixture();
    let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let mut times = [Vec::new(), Vec::new()];
    for batch in 0..9 {
        for reference in [batch % 2 == 0, batch % 2 != 0] {
            REFERENCE_WORKBENCH_GRADIENT.set(reference);
            let started = Instant::now();
            for _ in 0..100 {
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                std::hint::black_box(terminal.backend().buffer());
            }
            times[usize::from(!reference)].push(started.elapsed() / 100);
        }
    }
    REFERENCE_WORKBENCH_GRADIENT.set(false);
    for times in &mut times {
        times.sort_unstable();
    }
    eprintln!(
        "180x60 workbench gradient redraw: original={:?}, cached={:?}",
        times[0][4], times[1][4]
    );
}
