use super::*;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn render(
    mapping: Option<&[usize]>,
    matches: &[(usize, usize, usize)],
    diagnostics: &[(usize, u8)],
    index: Option<&DiagnosticLineIndex>,
    total: usize,
    height: u16,
    hovered: bool,
) -> Buffer {
    static COLORS: std::sync::OnceLock<crate::app_theme::ThemeColors> = std::sync::OnceLock::new();
    let colors = *COLORS.get_or_init(|| crate::app_theme::EditorTheme::Dark.colors());
    let changes = ChangeRows::from_lines(height as usize, total, [(1, 'U'), (5, 'N')]);
    let mut terminal = Terminal::new(TestBackend::new(2, height)).unwrap();
    terminal
        .draw(|frame| {
            let params = ScrollbarParams {
                area: Rect::new(0, 0, 2, height),
                total_lines: total,
                visible_lines: 1,
                scroll: total / 3,
                search_matches: matches,
                diagnostic_lines: diagnostics,
                diagnostic_index: index,
                change_rows: &changes,
                cursor_line: total.saturating_sub(1),
                colors,
                hovered,
                active: !hovered,
            };
            if let Some(mapping) = mapping {
                draw_mapped_scrollbar(frame, params, mapping);
            } else {
                draw_scrollbar(frame, params);
            }
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn mapped_scrollbar_matches_explicit_projection_with_duplicates_gaps_and_stale_tail() {
    for mapping in [vec![], vec![0; 9], vec![2, 2, 5, 6, 6, 8, 11, 11, 19]] {
        let total = 29;
        let source_matches = [
            (0, 2, 3),
            (1, 1, 9),
            (5, 0, 2),
            (6, 0, 1),
            (8, 4, 8),
            (90, 0, 1),
            (usize::MAX, 1, 2),
        ];
        let source_diagnostics = [
            (0, 3),
            (1, 2),
            (2, 4),
            (6, 2),
            (6, 1),
            (8, 4),
            (90, 2),
            (usize::MAX, 1),
        ];
        let index = DiagnosticLineIndex::from_diagnostics(&source_diagnostics);
        let mut projected_matches: Vec<_> = source_matches
            .iter()
            .map(|&(line, start, end)| (mapping.get(line).copied().unwrap_or(0), start, end))
            .collect();
        let mut projected_diagnostics: Vec<_> = source_diagnostics
            .iter()
            .map(|&(line, severity)| (mapping.get(line).copied().unwrap_or(0), severity))
            .collect();
        projected_matches.sort_unstable();
        projected_diagnostics.sort_unstable();
        for height in [0, 1, 2, 7, 31, 60] {
            for hovered in [false, true] {
                let reference = render(
                    None,
                    &projected_matches,
                    &projected_diagnostics,
                    None,
                    total,
                    height,
                    hovered,
                );
                for provider in [None, Some(&index)] {
                    assert_eq!(
                        render(
                            Some(&mapping),
                            &source_matches,
                            &source_diagnostics,
                            provider,
                            total,
                            height,
                            hovered
                        ),
                        reference,
                        "mapping={mapping:?}, height={height}, hovered={hovered}"
                    );
                }
            }
        }
    }
}

#[test]
fn mapped_scrollbar_keeps_valid_legacy_projection_frames_across_dense_compressed_rows() {
    let mapping: Vec<_> = (0..400).map(|line| (line / 9) * 3 + 2).collect();
    let matches: Vec<_> = (0..400).step_by(7).map(|line| (line, 0, 1)).collect();
    let diagnostics: Vec<_> = (0..400).map(|line| (line, (line % 4 + 1) as u8)).collect();
    let index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    let projected_matches: Vec<_> = matches
        .iter()
        .map(|&(line, start, end)| (mapping[line], start, end))
        .collect();
    let projected_diagnostics: Vec<_> = diagnostics
        .iter()
        .map(|&(line, severity)| (mapping[line], severity))
        .collect();
    for total in [138, 500] {
        for height in [1, 2, 7, 31, 120] {
            assert_eq!(
                render(
                    Some(&mapping),
                    &matches,
                    &diagnostics,
                    Some(&index),
                    total,
                    height,
                    false
                ),
                render(
                    None,
                    &projected_matches,
                    &projected_diagnostics,
                    None,
                    total,
                    height,
                    false
                )
            );
        }
    }
}

#[test]
#[ignore = "manual release dense inline overview projection benchmark"]
fn benchmark_mapped_diff_scrollbar() {
    let mapping: Vec<_> = (0..50_000).map(|line| line * 2 + line / 17).collect();
    let matches: Vec<_> = (0..50_000).map(|line| (line, 0, 1)).collect();
    let diagnostics: Vec<_> = (0..80_000)
        .map(|n| (n % 50_000, (2 + n % 3) as u8))
        .collect();
    let mut diagnostics = diagnostics;
    diagnostics.sort_unstable_by_key(|&(line, _)| line);
    let index = DiagnosticLineIndex::from_diagnostics(&diagnostics);
    let colors = crate::app::App::test_new().colors;
    let changes = ChangeRows::default();
    let mut terminals = [
        Terminal::new(TestBackend::new(2, 60)).unwrap(),
        Terminal::new(TestBackend::new(2, 60)).unwrap(),
    ];
    let mut times = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let started = std::time::Instant::now();
            let terminal = &mut terminals[usize::from(optimized)];
            for _ in 0..20 {
                let projected_matches;
                let projected_diagnostics;
                let (search, diags) = if optimized {
                    (&matches[..], &diagnostics[..])
                } else {
                    projected_matches = matches
                        .iter()
                        .map(|&(line, start, end)| (mapping[line], start, end))
                        .collect::<Vec<_>>();
                    projected_diagnostics = diagnostics
                        .iter()
                        .map(|&(line, severity)| (mapping[line], severity))
                        .collect::<Vec<_>>();
                    (&projected_matches[..], &projected_diagnostics[..])
                };
                terminal
                    .draw(|frame| {
                        let params = ScrollbarParams {
                            area: Rect::new(0, 0, 2, 60),
                            total_lines: 105_000,
                            visible_lines: 40,
                            scroll: 30_000,
                            search_matches: search,
                            diagnostic_lines: diags,
                            diagnostic_index: optimized.then_some(&index),
                            change_rows: &changes,
                            cursor_line: 40_000,
                            colors,
                            hovered: false,
                            active: false,
                        };
                        if optimized {
                            draw_mapped_scrollbar(frame, params, &mapping);
                        } else {
                            draw_scrollbar(frame, params);
                        }
                    })
                    .unwrap();
                std::hint::black_box(terminal.backend().buffer());
            }
            times[usize::from(optimized)].push(started.elapsed() / 20);
        }
        assert_eq!(
            terminals[0].backend().buffer(),
            terminals[1].backend().buffer()
        );
    }
    times.iter_mut().for_each(|time| time.sort());
    eprintln!(
        "inline overview / 50k search hits + 80k diagnostics: remap={:?} inverse={:?}",
        times[0][3], times[1][3]
    );
}
