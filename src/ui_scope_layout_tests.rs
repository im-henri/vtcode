use super::*;
use crate::editor::Editor;

fn assert_same_shape(actual: &SourceShape, expected: &SourceShape) {
    assert_eq!(actual.rows.len(), expected.rows.len());
    for (a, b) in actual.rows.iter().zip(&expected.rows) {
        assert_eq!(
            (a.line, a.start, a.end, a.origin),
            (b.line, b.start, b.end, b.origin)
        );
        assert_eq!(a.columns, b.columns);
    }
    for (a, b) in actual.profiles.iter().zip(&expected.profiles) {
        assert_eq!((a.left, a.right, &a.gaps), (b.left, b.right, &b.gaps));
    }
}

#[test]
fn edits_reuse_unchanged_scope_rows_and_match_fresh_tabs_wraps_and_previews() {
    for wrap in [None, Some(18)] {
        for preview in [None, Some(false), Some(true)] {
            for diff in [None, Some((2, false)), Some((2, true))] {
                let source = format!(
                    "fn f() {{\r\n{}}}\r\nafter\n",
                    "\t let 界 = red; // #ff0000\r\n".repeat(40)
                );
                let mut editor = Editor::test_with_text(&source);
                let original = editor.rope.clone();
                let pair = ((0, 7), (41, 0));
                let mut geometry = Geometry::new(Rect::new(4, 3, 80, 50), true);
                geometry.wrap_width = wrap;
                geometry.color_preview = preview;
                geometry.diff_projection = diff;
                let mut view = View::default();
                view.shape(&editor, &geometry, pair, true).unwrap();
                let columns = Arc::clone(&view.shapes[0].shape.rows[0].columns);
                let initial = view.decoded_rows();
                editor
                    .rope
                    .insert(editor.rope.line_to_char(20) + 3, "longer ");
                view.shape(&editor, &geometry, pair, true).unwrap();
                assert_eq!(view.decoded_rows() - initial, 1);
                assert!(Arc::ptr_eq(&columns, &view.shapes[0].shape.rows[0].columns));
                let fresh =
                    SourceShape::new(&editor, &geometry, pair, None, &mut ShapeWork::default())
                        .unwrap();
                assert_same_shape(&view.shapes[0].shape, &fresh);

                // Undo and text replacement need no dirty-range metadata.
                editor.rope = original.clone();
                view.shape(&editor, &geometry, pair, true).unwrap();
                assert_eq!(view.decoded_rows() - initial, 2);
                let fresh =
                    SourceShape::new(&editor, &geometry, pair, None, &mut ShapeWork::default())
                        .unwrap();
                assert_same_shape(&view.shapes[0].shape, &fresh);
                editor.rope.insert(editor.rope.len_chars(), "outside scope");
                view.shape(&editor, &geometry, pair, true).unwrap();
                assert_eq!(view.decoded_rows() - initial, 2);

                // Inserting a line changes the scope endpoints and layout.
                editor
                    .rope
                    .insert(editor.rope.line_to_char(20), "    inserted();\n");
                let shifted = (pair.0, (42, 0));
                view.shape(&editor, &geometry, shifted, true).unwrap();
                let fresh =
                    SourceShape::new(&editor, &geometry, shifted, None, &mut ShapeWork::default())
                        .unwrap();
                assert_same_shape(&view.shapes.last().unwrap().shape, &fresh);
                editor.tab_size = 8;
                view.shape(&editor, &geometry, shifted, true).unwrap();
                let fresh =
                    SourceShape::new(&editor, &geometry, shifted, None, &mut ShapeWork::default())
                        .unwrap();
                assert_same_shape(&view.shapes.last().unwrap().shape, &fresh);
            }
        }
    }
}

#[test]
#[ignore = "manual optimized-profile scope edit benchmark"]
fn benchmark_scope_edit_layout() {
    use std::{hint::black_box, time::Instant};
    for lines in [30, 500] {
        let mut editor = Editor::test_with_text(&format!(
            "fn f() {{\n{}}}\n",
            "    let result = calculate(input);\n".repeat(lines)
        ));
        let geometry = Geometry::new(Rect::new(4, 3, 160, 50), true);
        let pair = ((0, 7), (lines + 1, 0));
        let mut view = View::default();
        view.shape(&editor, &geometry, pair, true).unwrap();
        editor
            .rope
            .insert(editor.rope.line_to_char(lines / 2) + 5, "x");
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..9 {
            for reuse in [round % 2 == 0, round % 2 != 0] {
                let started = Instant::now();
                for _ in 0..200 {
                    black_box(
                        SourceShape::new(
                            &editor,
                            &geometry,
                            pair,
                            reuse.then_some(&view.shapes[0]),
                            &mut ShapeWork::default(),
                        )
                        .unwrap(),
                    );
                }
                samples[usize::from(reuse)].push(started.elapsed() / 200);
            }
        }
        for samples in &mut samples {
            samples.sort();
        }
        eprintln!(
            "scope edit layout {lines} lines: full={:?}, reused={:?}",
            samples[0][4], samples[1][4]
        );
    }
}

#[test]
fn oversized_source_contours_reject_from_metadata_before_decoding_rows() {
    for source in [
        format!("{{\n{}}}\n", "    body();\n".repeat(MAX_SOURCE_ROWS)),
        format!("{{\n    {};\n}}\n", "x".repeat(MAX_SOURCE_CHARS)),
    ] {
        let editor = Editor::test_with_text(&source);
        let pair = ((0, 0), (editor.rope.len_lines() - 2, 0));
        let geometry = Geometry::new(Rect::new(0, 0, 80, 20), true);
        let mut view = View::default();
        for _ in 0..240 {
            assert!(view.shape(&editor, &geometry, pair, true).is_none());
        }
        assert_eq!(
            view.decoded_rows(),
            0,
            "budget rejection must not decode the first row"
        );
        assert_eq!(
            view.shape_builds, 1,
            "oversized-source rejection is retained"
        );

        let editor = Editor::test_with_text("{\n    body();\n}\n");
        assert!(
            view.shape(&editor, &geometry, ((0, 0), (2, 0)), true)
                .is_some()
        );
        assert_eq!(
            view.decoded_rows(),
            3,
            "small source contours still decode normally"
        );
    }
}
