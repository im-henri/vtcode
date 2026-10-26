use super::*;
use ropey::Rope;
use unicode_width::UnicodeWidthChar;

fn glyphs(text: &str, tab_size: usize) -> std::rc::Rc<RenderLineGlyphs> {
    let rope = Rope::from_str(text);
    let view = HighlightedLine::new(None, text, text.chars().count(), PLAIN_STYLE);
    RenderLineCache::default().line(0, (0, 0, tab_size), &rope, &view, None, usize::MAX, false)
}

#[test]
fn compact_column_maps_preserve_every_cell_in_mixed_unicode_and_tabs() {
    for text in [
        "",
        "plain ASCII",
        "éα plain",
        "\t\t\t",
        "a\tb\tcc\t",
        "abc界def",
        "é\t🙂",
        "\t界é\t\0Z",
        "界\t\t é👩‍💻",
    ] {
        for tab_size in [1, 2, 3, 4, 8, 19] {
            let glyphs = glyphs(text, tab_size);
            let mut visual = vec![0];
            let mut cells = vec![0];
            for ch in text.chars() {
                let logical = *visual.last().unwrap();
                let width = if ch == '\t' {
                    tab_size - logical % tab_size
                } else {
                    1
                };
                visual.push(logical + width);
                cells.push(
                    cells.last().unwrap()
                        + if ch == '\t' {
                            width
                        } else {
                            ch.width().unwrap_or(0)
                        },
                );
            }
            for start in 0..cells.len() {
                assert_eq!(
                    glyphs.visual_column(start),
                    visual[start],
                    "{text:?}, tab={tab_size}"
                );
                assert_eq!(
                    glyphs.cell_column(start),
                    cells[start],
                    "{text:?}, tab={tab_size}"
                );
                for end in start..cells.len() {
                    for cell in (0..=cells[end] + 1).chain([usize::MAX]) {
                        let target = cells[start].saturating_add(cell);
                        let expected = (start..end).find(|&column| target < cells[column + 1]);
                        assert_eq!(
                            glyphs.source_at_cell(start, end, cell),
                            (expected.unwrap_or(end), expected.is_some()),
                            "{text:?}, tab={tab_size}, range={start}..{end}, cell={cell}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn ordinary_lines_allocate_only_actual_column_exceptions() {
    for text in ["", "plain source ", "café Greek αβγ", "x\ty"] {
        let glyphs = glyphs(text, 1);
        assert!(glyphs.tab_columns.is_empty());
        assert!(glyphs.cell_columns.is_empty());
    }
    let tabbed = glyphs(&format!("\t{}\tend", "value ".repeat(500)), 4);
    assert!(tabbed.cell_columns.is_empty());
    assert_eq!(tabbed.tab_columns.len(), 2);
    assert!(tabbed.tab_columns.capacity() <= 4);
    let wide = glyphs(&"界".repeat(1000), 4);
    assert!(wide.tab_columns.is_empty());
    assert_eq!(wide.cell_columns.len(), 1001);
}

#[test]
fn coordinate_lookup_remains_exact_across_edit_undo_redo_and_tab_changes() {
    use crate::editor::Editor;
    let mut editor = Editor::test_with_text("abc\t界 é🙂\txyz");
    for change in 0..6 {
        let text = editor.rope.to_string();
        let view = HighlightedLine::new(None, &text, text.chars().count(), PLAIN_STYLE);
        editor.render_line_cache.borrow_mut().line(
            0,
            (
                editor.content_version,
                editor.wrap_revision,
                editor.tab_size,
            ),
            &editor.rope,
            &view,
            None,
            usize::MAX,
            false,
        );
        match change {
            0 => {
                editor.cursor_col = 3;
                editor.insert_char('界');
            }
            1 => editor.undo(),
            2 => editor.redo(),
            3 => editor.tab_size = 8,
            4 => editor.rope = Rope::from_str("界\t é🙂\tabcxyz"),
            _ => {}
        }
        let mut cold = Editor::test_with_text(&editor.rope.to_string());
        cold.tab_size = editor.tab_size;
        for start in 0..=editor.line_len(0) {
            for end in start..=editor.line_len(0) {
                assert_eq!(
                    editor.cell_at_source_column(0, start, end),
                    cold.cell_at_source_column(0, start, end)
                );
                for cell in 0..40 {
                    assert_eq!(
                        editor.source_column_at_cell(0, start, end, cell),
                        cold.source_column_at_cell(0, start, end, cell)
                    );
                }
            }
        }
    }
}
