use super::*;
use crate::editor::Editor;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

const WIDTH: u16 = 10;
const HEIGHT: u16 = 40;
const VISIBLE: usize = 40;

fn editor(lines: usize, overview: bool) -> Editor {
    let mut editor = Editor::test_with_text(&"    let value = compute(123);\n".repeat(lines));
    editor.scroll_past_end = false;
    editor.viewport_height = VISIBLE;
    editor.minimap_overview.set(overview);
    editor
}

fn render(editor: &Editor, scroll: usize, changes: &ChangeLineIndex) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal
        .draw(|frame| {
            draw_minimap(
                frame,
                MinimapParams {
                    area: Rect::new(0, 0, WIDTH, HEIGHT),
                    editor,
                    scroll,
                    visible_lines: VISIBLE,
                    search_matches: &[],
                    diagnostic_lines: &[],
                    diagnostic_index: None,
                    change_index: Some(changes),
                    diff_changes: None,
                    colors: crate::app_theme::EditorTheme::Dark.colors(),
                    hovered: false,
                    active: false,
                },
            )
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// Rows whose right edge carries a change marker cell.
fn marked_rows(buffer: &Buffer) -> Vec<usize> {
    let track = crate::app_theme::EditorTheme::Dark.colors().ui.bg_minimap;
    let slider = crate::app_theme::EditorTheme::Dark
        .colors()
        .ui
        .bg_minimap_slider;
    (0..HEIGHT)
        .filter(|&row| {
            let bg = buffer[(WIDTH - 1, row)].bg;
            bg != track && bg != slider
        })
        .map(usize::from)
        .collect()
}

#[test]
fn idle_minimap_shows_the_whole_file_and_every_isolated_change() {
    let lines = 20_000;
    // Single-line changes far apart: each is a tiny fraction of an overview
    // row, yet each must still mark its row rather than being sampled away.
    let changes: Vec<(usize, char)> = [7, 3_333, 9_001, 14_500, 19_998]
        .into_iter()
        .zip(['M', 'A', 'D', 'M', 'A'])
        .collect();
    let index = ChangeLineIndex::from_changes(&changes);
    let expected: Vec<usize> = changes
        .iter()
        .map(|&(line, _)| line * usize::from(HEIGHT) / lines)
        .collect();

    let overview = editor(lines, true);
    assert_eq!(marked_rows(&render(&overview, 0, &index)), expected);

    // The detail window near the top only reaches the first change.
    let detail = editor(lines, false);
    assert_eq!(marked_rows(&render(&detail, 0, &index)), vec![7 / 4]);
}

#[test]
fn clicks_and_drags_follow_the_painted_overview() {
    let mut overview = editor(10_000, true);
    render(&overview, 0, &ChangeLineIndex::default());
    let height = usize::from(HEIGHT);
    let layout = MinimapLayout::for_editor(&overview, height);
    let end = overview.visible_line_count();
    assert_eq!(layout.window, MinimapWindow { start: 0, end });
    let target = layout.click_scroll(20);
    assert!((4_900..=5_200).contains(&target), "{target}");
    // A drag begun on the overview keeps mapping rows to the whole file.
    let drag = MinimapDrag::new(&overview, 0, height);
    drag.drag(&mut overview, 30);
    assert!((7_400..=7_600).contains(&overview.visible_scroll_offset()));
}

#[test]
fn zoom_steps_shrink_monotonically_and_always_contain_the_viewport() {
    for total in [50_usize, 400, 1_000, 25_000, 1_000_000] {
        for height in [3_usize, 24, 60] {
            for visible in [10_usize, 40] {
                if visible > total {
                    continue;
                }
                let max_scroll = total - visible;
                for scroll in [0, 1, max_scroll / 3, max_scroll / 2, max_scroll] {
                    let detail = minimap_window(total, scroll, visible, height);
                    let mut previous = usize::MAX;
                    for step in 0..=20 {
                        let zoom = step as f32 / 20.0;
                        let window =
                            zoomed_minimap_window(total, false, scroll, visible, height, zoom);
                        assert!(window.len() <= previous, "{total} {scroll} {zoom}");
                        assert!(window.start <= scroll && window.end >= scroll + visible);
                        assert!(window.len() >= detail.len().min(total));
                        previous = window.len();
                    }
                    assert_eq!(
                        zoomed_minimap_window(total, false, scroll, visible, height, 1.0),
                        detail
                    );
                }
            }
        }
    }
}

#[test]
fn whole_file_overview_of_a_huge_file_decodes_only_sampled_lines() {
    let lines = 300_000;
    let overview = editor(lines, true);
    let changes: Vec<(usize, char)> = (0..lines).step_by(7).map(|line| (line, 'M')).collect();
    let index = ChangeLineIndex::from_changes(&changes);
    render(&overview, 0, &index);
    let first = overview.minimap_cache.borrow().builds;
    assert!(first <= 4 * usize::from(HEIGHT), "{first}");
    // Repainting the resting overview decodes nothing new.
    render(&overview, 0, &index);
    assert_eq!(overview.minimap_cache.borrow().builds, first);
    assert_eq!(
        marked_rows(&render(&overview, 0, &index)).len(),
        usize::from(HEIGHT)
    );
}

#[test]
#[ignore = "manual release timing of resting, transition and detail minimap frames"]
fn benchmark_overview_transition_frames() {
    use std::time::{Duration, Instant};
    let lines = 1_000_000;
    let changes: Vec<(usize, char)> = (0..lines).step_by(13).map(|line| (line, 'M')).collect();
    let index = ChangeLineIndex::from_changes(&changes);
    let time = |editor: &Editor, scroll: usize, frames: usize| {
        let started = Instant::now();
        for _ in 0..frames {
            std::hint::black_box(render(editor, scroll, &index));
        }
        started.elapsed() / frames as u32
    };
    let detail = editor(lines, false);
    time(&detail, 500_000, 3);
    let detail_frame = time(&detail, 500_000, 200);
    let overview = editor(lines, true);
    time(&overview, 0, 3);
    let resting = time(&overview, 0, 200);
    // One gesture: move once, then paint until the zoom has settled again.
    let mut transition = Duration::ZERO;
    let mut frames = 0;
    let gesture = Instant::now();
    render(&overview, 500_000, &index);
    while gesture.elapsed() < zoom::ZOOM_IN + zoom::HOLD {
        let started = Instant::now();
        std::hint::black_box(render(&overview, 500_000, &index));
        if overview.minimap_zoom.get().painted() < 1.0 {
            transition += started.elapsed();
            frames += 1;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    while overview.minimap_zoom.get().painted() > 0.0 {
        let started = Instant::now();
        std::hint::black_box(render(&overview, 500_000, &index));
        transition += started.elapsed();
        frames += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    eprintln!(
        "1M lines 10x40: detail={detail_frame:?} resting overview={resting:?} \
         transition={:?}/frame over {frames} frames (total {transition:?})",
        transition / frames.max(1) as u32
    );
}
