use super::*;

fn full_scan_window(text: &str, column: usize, width: usize) -> (&str, usize, usize) {
    if text.is_ascii() && !text.bytes().any(|byte| byte.is_ascii_control()) {
        let start = column.min(text.len());
        return (
            &text[start..start.saturating_add(width).min(text.len())],
            start,
            0,
        );
    }
    let mut cell = 0usize;
    let mut character = 0usize;
    let mut padding = 0usize;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let cells = if grapheme.contains(char::is_control) {
            0
        } else {
            grapheme.width()
        };
        if cell < column {
            padding = cell.saturating_add(cells).saturating_sub(column).min(width);
            cell += cells;
            character += grapheme.chars().count();
            continue;
        }
        let tail = &text[byte..];
        let mut used = padding;
        let mut end = tail.len();
        for (offset, grapheme) in tail.grapheme_indices(true) {
            if !grapheme.contains(char::is_control) {
                used += grapheme.width();
            }
            if used > width {
                end = offset;
                break;
            }
        }
        return (&tail[..end], character, padding);
    }
    ("", character, padding)
}

fn checkpoint_columns(index: &DiffTextIndex, first_cells: usize, last_cell: usize) -> Vec<usize> {
    // Exhaust the short Unicode pattern, then seek on either side of every
    // sparse boundary and EOF. Repeating a full prefix scan at every interior
    // cell of a long repeated pattern adds quadratic work, not new contexts.
    let mut columns: Vec<_> = (0..first_cells.saturating_add(3)).collect();
    for checkpoint in &index.cell_checkpoints {
        columns.extend(checkpoint.cell.saturating_sub(2)..=checkpoint.cell.saturating_add(2));
    }
    columns.extend(last_cell.saturating_sub(3)..=last_cell.saturating_add(3));
    columns.sort_unstable();
    columns.dedup();
    columns
}

#[test]
fn sparse_diff_windows_preserve_graphemes_cells_character_offsets_and_clipped_padding() {
    for source in [
        "",
        "ascii\t text\r\n",
        "界a e\u{301}👩‍💻 🇪🇪 क्‍ष \t\r\n",
        "\u{200b}\u{200b}界\u{200b}x\u{200b}",
        "\r\n\r界\n\u{301}a\u{200d}b",
        "🇪🇪🇫🇮🇸🇪🇦🇧🇨",
    ] {
        for repetitions in [1, 19, 151] {
            let text = source.repeat(repetitions);
            let index = DiffTextIndex::new(&text);
            assert_eq!(index.char_len, text.chars().count());
            let columns = if repetitions == 1 {
                (0..text.width().saturating_add(3)).collect()
            } else {
                checkpoint_columns(&index, source.width(), text.width())
            };
            for column in columns {
                for width in [0, 1, 2, 3, 7, 23] {
                    assert_eq!(
                        index.inline_window(&text, column, width),
                        full_scan_window(&text, column, width),
                        "source={source:?}, repetitions={repetitions}, column={column}, width={width}"
                    );
                }
            }
            assert_eq!(
                index.inline_window(&text, usize::MAX, 7),
                full_scan_window(&text, usize::MAX, 7)
            );
            for (character, (byte, _)) in text
                .char_indices()
                .chain(std::iter::once((text.len(), '\0')))
                .enumerate()
            {
                assert_eq!(
                    index.character_at_byte(&text, byte),
                    character,
                    "UTF-8 boundary {byte}: source={source:?}, repetitions={repetitions}"
                );
            }
        }
    }
}

#[test]
fn sparse_diff_windows_keep_regional_indicator_and_indic_context_across_checkpoints() {
    let atoms = [
        "a",
        "界",
        "\u{200b}",
        "\r\n",
        "e\u{301}",
        "🇪",
        "🇫",
        "👩‍💻",
        "क्",
        "‍ष",
    ];
    let mut seed = 0x8af2_bc51_u64;
    for case in 0..32 {
        let mut text = String::new();
        for _ in 0..800 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            text.push_str(atoms[seed as usize % atoms.len()]);
        }
        let index = DiffTextIndex::new(&text);
        for column in checkpoint_columns(&index, 16, text.width()) {
            for width in [0, 1, 2, 17] {
                assert_eq!(
                    index.inline_window(&text, column, width),
                    full_scan_window(&text, column, width),
                    "case={case}, column={column}, width={width}"
                );
            }
        }
    }
}

#[test]
fn sparse_diff_windows_handle_long_clusters_and_zero_width_runs_without_unbounded_offset_counting()
{
    let text = format!(
        "{}a{}界{}suffix",
        "\u{200b}".repeat(2_000),
        "\u{301}".repeat(20_000),
        "\u{200b}".repeat(2_000)
    );
    let index = DiffTextIndex::new(&text);
    for column in [0, 1, 2, 3, 9, 10] {
        for width in [0, 1, 2, 7, 20] {
            assert_eq!(
                index.inline_window(&text, column, width),
                full_scan_window(&text, column, width)
            );
        }
    }
    assert!(index.cell_checkpoints.len() <= index.char_len / DIFF_TEXT_INDEX_STRIDE + 1);
    let ascii = DiffTextIndex::new(&"a".repeat(50_000));
    assert!(ascii.cell_checkpoints.is_empty());
    assert!(ascii.byte_checkpoints.is_empty());
}

#[test]
#[ignore = "manual release diff Unicode horizontal seek benchmark"]
fn benchmark_sparse_diff_unicode_window() {
    let text = "界e\u{301}👩‍💻🇪🇪 value ".repeat(20_000);
    let index = DiffTextIndex::new(&text);
    let column = text.width() - 180;
    assert_eq!(
        index.inline_window(&text, column, 100),
        full_scan_window(&text, column, 100)
    );
    let mut timings = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let start = std::time::Instant::now();
            for _ in 0..20 {
                let value = if optimized {
                    index.inline_window(&text, column, 100)
                } else {
                    full_scan_window(&text, column, 100)
                };
                std::hint::black_box(value);
            }
            timings[usize::from(optimized)].push(start.elapsed() / 20);
        }
    }
    timings.iter_mut().for_each(|times| times.sort());
    eprintln!(
        "long Unicode diff display-cell window: full={:?} sparse={:?}",
        timings[0][3], timings[1][3]
    );
}
