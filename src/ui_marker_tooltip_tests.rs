use super::*;
use ropey::Rope;

fn at(text: &str, column: usize) -> Option<(usize, String)> {
    preview(Rope::from_str(text).slice(..), column)
}

#[test]
fn marker_hover_is_local_and_handles_repeated_markers_and_unicode_columns() {
    let text = "é界 TODO: first; TODO: second\r\n";
    for column in 3..7 {
        assert_eq!(at(text, column).unwrap().1, " ⚠ Todo: first; TODO: second ");
    }
    for column in 16..20 {
        assert_eq!(at(text, column).unwrap().1, " ⚠ Todo: second ");
    }
    for column in [0, 1, 2, 7, 8, 15, 20, 27, 100_000] {
        assert!(at(text, column).is_none(), "{column}");
    }
    for &(marker, label) in &MARKERS {
        for column in 0..marker.len() {
            assert_eq!(at(marker, column).unwrap().1, format!(" ⚠ {label} "));
        }
        assert!(at(&format!("prefix{marker}"), 6).is_none());
        assert!(at(&format!("{marker}suffix"), 0).is_none());
        // Keep the existing ASCII-alphanumeric boundary rule for hover text.
        assert!(at(&format!("_{marker}_"), 1).is_some());
        assert!(at(&format!("{marker}:  \t\r\n"), 0).is_some());
    }
}

#[test]
fn large_hover_previews_only_copy_displayed_text() {
    let prefix = "界".repeat(400_000);
    let rope = Rope::from_str(&format!("{prefix} TODO: {}", "content ".repeat(100_000)));
    let (marker, text) = preview(rope.line(0), 400_003).unwrap();
    assert_eq!(marker, 0);
    assert_eq!(text.chars().count(), PREVIEW_CHARS);
    assert!(text.starts_with(" ⚠ Todo: content content "));
    assert!(preview(rope.line(0), 200_000).is_none());
}

#[test]
fn tooltip_anchors_to_the_pointer_in_a_scrolled_pane() {
    let mut app = App::test_new();
    app.tabs = vec![crate::app::Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    let editor = app.editor_mut().unwrap();
    editor.rope = Rope::from_str(&format!("{}TODO: message", "x ".repeat(40_000)));
    editor.horizontal_scroll = 79_995;
    app.hovered_editor_pos = Some((0, 80_001));
    app.mouse_col = 14;
    app.mouse_row = 5;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|f| draw(f, &app, Rect::new(10, 3, 60, 12)))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(15, 6)].symbol(), "⚠");
}

#[test]
#[ignore = "manual optimized performance measurement"]
fn benchmark_local_marker_hover() {
    use std::{hint::black_box, time::Instant};
    let rope = Rope::from_str(&format!(
        "{} TODO: useful description",
        "generated_value ".repeat(200_000)
    ));
    for column in [500_000, 3_200_002] {
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..9 {
            for fast in [round % 2 == 0, round % 2 != 0] {
                let now = Instant::now();
                for _ in 0..30 {
                    if fast {
                        black_box(preview(rope.line(0), column));
                    } else {
                        let text = rope.line(0).to_string();
                        for &(marker, _) in &MARKERS {
                            black_box(text.find(marker));
                        }
                    }
                }
                samples[usize::from(fast)].push(now.elapsed() / 30);
            }
        }
        for values in &mut samples {
            values.sort();
        }
        eprintln!(
            "hover at {column}: full-line copy/search={:?}, local={:?}",
            samples[0][4], samples[1][4]
        );
    }
}
