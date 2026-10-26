use super::*;
use crate::app_theme::EditorTheme;

fn render(source: &str, width: usize) -> Vec<Row> {
    PreparedDocument::new(source).render(width, Palette::from(&EditorTheme::Dark.colors()), None)
}

#[test]
fn remote_markdown_file_links_resolve_on_the_node_without_native_filesystem_access() {
    use crate::remote::{self, RemotePath};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let remote = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let key = remote.view_key(&RemotePath::new(b"docs/readme.md".to_vec()).unwrap());
    for (target, expected) in [
        ("./sibling%20file.md", b"docs/sibling file.md".as_slice()),
        ("../src/code.rs#example", b"src/code.rs".as_slice()),
        ("../../outside.txt", b"outside.txt".as_slice()),
    ] {
        let uri = preview_link_url(target, Some(&key), Some(&remote)).unwrap();
        let mapped = remote.file_uri_key(uri.as_str()).unwrap();
        let path = remote.remote_path(&mapped).unwrap();
        if target.starts_with("../../") {
            assert!(
                path.display().starts_with('/'),
                "A parent link must be resolved outside the node workspace"
            );
            assert!(path.display().as_bytes().ends_with(expected));
        } else {
            assert_eq!(path, RemotePath::new(expected.to_vec()).unwrap());
        }
    }
    let uri = preview_link_url("../src/code.rs", Some(&key), Some(&remote)).unwrap();
    assert_eq!(
        remote.file_uri_key(uri.as_str()),
        Some(remote.view_key(&RemotePath::new(b"src/code.rs".to_vec()).unwrap()))
    );
    for invalid in [
        "file://other-server/share/file.rs",
        "file:///bad%00name.rs",
        "https://example.test/file.rs",
    ] {
        assert!(remote.file_uri_key(invalid).is_none(), "{invalid}");
    }
    assert_eq!(
        preview_link_url("https://example.test/", Some(&key), Some(&remote))
            .unwrap()
            .as_str(),
        "https://example.test/"
    );
}

#[test]
fn remote_markdown_directory_link_reveals_explorer_and_keeps_preview_tabs() {
    use crate::remote::{self, RemotePath};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("not-yet-listed/nested")).unwrap();
    let remote = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut app = App::new_deferred(remote.view_root.clone());
    app.remote_workspace = Some(remote.clone());
    app.source_control_enabled = false;
    let mut editor =
        crate::editor::Editor::test_with_text("[Open folder](../not-yet-listed/nested)\n");
    editor.file_path = Some(remote.view_key(&RemotePath::new(b"docs/readme.md".to_vec()).unwrap()));
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    app.toggle_markdown_preview_tab();
    let before = app.tabs.len();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 25)).unwrap();
    terminal
        .draw(|frame| draw_markdown_preview(frame, &mut app, Rect::new(0, 0, 90, 25)))
        .unwrap();
    let view = app
        .editor()
        .unwrap()
        .markdown_preview_scroll
        .as_ref()
        .unwrap();
    let (index, link) = view
        .links
        .iter()
        .enumerate()
        .find_map(|(index, links)| links.first().map(|link| (index, link)))
        .unwrap();
    let (col, row) = (
        view.body.x + link.cells.start as u16,
        view.body.y + index as u16,
    );
    assert!(app.markdown_preview_link_click(col, row));
    app.wait_for_background(std::time::Duration::from_secs(10), |app| {
        app.active_pane == crate::app::ActivePane::Explorer
    });
    assert_eq!(app.tabs.len(), before);
    assert!(
        app.tabs
            .iter()
            .any(|tab| tab.kind == crate::app::TabKind::MarkdownPreview)
    );
}
fn text(rows: &[Row]) -> String {
    rows.iter()
        .map(|row| {
            row.line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn metadata_is_not_a_heading_and_scripts_math_and_ids_render() {
    let source = "---\ntitle: Metadata, not a heading\ntags: [test]\n---\n# Actual {#custom}\n\n$H_2O$ and $x^2$; $E = mc^2$.\n\nStandalone scripts: ~2~ and ^2^.\n\n$$\n\\frac{1}{2} + \\sqrt{\\pi}\n$$\n\nThe price is $15.99.";
    let doc = PreparedDocument::new(source);
    let output = text(&doc.render(80, Palette::from(&EditorTheme::Dark.colors()), None));
    assert!(
        !output.contains("Metadata") && !output.contains("{#custom}"),
        "{output}"
    );
    assert!(output.contains("H₂O and x²; E = mc²."), "{output}");
    assert!(output.contains("(1)/(2) + √(π)"), "{output}");
    assert!(output.contains("Standalone scripts: ₂ and ²."), "{output}");
    assert!(output.contains("$15.99"));
    assert_eq!(doc.anchors.get("custom"), Some(&4));
}

#[test]
fn escaped_math_delimiters_and_admonitions_preserve_source_locations() {
    let source = "!!! warning \"Careful\"\n    Avoid **mistakes**.\n\n:::tip[Try this]\nUse \\(a^2 + b^2\\).\n:::\n\n\\[\n\\begin{bmatrix}\n1 & 2 \\\\\n3 & 4\n\\end{bmatrix}\n\\]\n\n# Target\nAfter";
    let doc = PreparedDocument::new(source);
    let rows = doc.render(70, Palette::from(&EditorTheme::Dark.colors()), None);
    let output = text(&rows);
    for expected in [
        "WARNING",
        "Careful",
        "Avoid mistakes.",
        "TIP",
        "Try this",
        "a² + b²",
        "1   2",
        "3   4",
        "Target",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
    let target = source.lines().position(|line| line == "# Target").unwrap();
    assert_eq!(doc.anchors.get("target"), Some(&target));
    assert_eq!(
        rows.iter()
            .find(|row| text(std::slice::from_ref(row)).contains("After"))
            .unwrap()
            .source,
        target + 1
    );
    assert!(
        rows.iter().all(|row| row.source < source.lines().count()),
        "{rows:?}"
    );
}

#[test]
fn html_is_readable_styled_and_comments_are_hidden_but_fences_stay_literal() {
    let source = "Inline <strong>bold</strong>, <em>italic</em>, H<sub>2</sub>O, x<sup>2</sup>, <kbd>Ctrl</kbd>.<br>Next\n\n<details>\n<summary>More info</summary>\n<p>Details &amp; &#169;</p>\n</details>\n\n<!-- hidden\nmultiline comment -->\n\n<script>do_not_show()</script>\n\n```html\n<!-- visible comment -->\n<script>literal()</script>\n```";
    let rows = render(source, 80);
    let output = text(&rows);
    for expected in [
        "Inline bold, italic, H₂O, x², Ctrl.",
        "Next",
        "▾ More info",
        "Details & ©",
        "<!-- visible comment -->",
        "<script>literal()</script>",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
    assert!(
        !output.contains("multiline comment") && !output.contains("do_not_show"),
        "{output}"
    );
    let spans: Vec<_> = rows.iter().flat_map(|row| &row.line.spans).collect();
    assert!(
        spans
            .iter()
            .any(|span| span.content == "bold" && span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(
        spans
            .iter()
            .any(|span| span.content == "italic"
                && span.style.add_modifier.contains(Modifier::ITALIC))
    );
}

#[test]
fn code_tabs_use_display_columns_and_prose_spaces_collapse() {
    let rows = render(
        "A    B and `C   D`.\n\n```text\na\tb\t界\tx\n```\n\n<pre>a\tb\n  indented</pre>",
        80,
    );
    let output = text(&rows);
    assert!(output.contains("A B and C   D."), "{output}");
    assert!(output.contains("a   b   界  x"), "{output}");
    assert!(output.contains("a   b\n  indented"), "{output}");
}

#[test]
fn links_survive_wrapping_tables_unicode_and_punctuation() {
    let source = "[**界面 docs**](#duplicate-1), https://example.com/a_(b). www.example.org and name@example.com. `https://literal.test`\n\n| Link | Value |\n|--|--|\n| [reference](file%20name.md) | data |\n\n# Duplicate\n# Duplicate\n\nText[^a]\n\n[^a]: Details";
    let doc = PreparedDocument::new(source);
    assert_eq!(doc.anchors.get("duplicate"), Some(&6));
    assert_eq!(doc.anchors.get("duplicate-1"), Some(&7));
    for width in [1, 8, 18, 40, 80] {
        let rows = doc.render(width, Palette::from(&EditorTheme::Dark.colors()), None);
        for row in &rows {
            assert!(row.line.width() <= width);
            for link in &row.links {
                assert!(link.cells.end <= row.line.width());
            }
        }
        let targets: Vec<_> = rows
            .iter()
            .flat_map(|row| row.links.iter().map(|link| link.target.as_ref()))
            .collect();
        for target in [
            "#duplicate-1",
            "https://example.com/a_(b)",
            "https://www.example.org",
            "mailto:name@example.com",
            "file%20name.md",
            "#fn-a",
        ] {
            assert!(
                targets.contains(&target),
                "{target} at width {width}: {targets:?}"
            );
        }
        assert!(!targets.iter().any(|target| target.contains("literal.test")));
        let mut view = PreviewScroll::default();
        view.update_text(preview_text_rows(&rows), Rect::new(3, 2, width as u16, 10));
        view.links = rows.iter().map(|row| row.links.clone()).collect();
        for (index, row) in rows.iter().enumerate() {
            view.row = index;
            for link in &row.links {
                assert_eq!(
                    view.link_at(3 + link.cells.start as u16, 2).as_deref(),
                    Some(link.target.as_ref())
                );
            }
        }
    }
}

#[test]
fn ctrl_click_anchor_navigates_without_changing_the_markdown_source() {
    let source = format!(
        "[Jump](#target)\n\n{}\n# Target\nHere\n{}",
        "Paragraph.\n\n".repeat(35),
        "Tail\n\n".repeat(20)
    );
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::new();
    editor.rope = ropey::Rope::from_str(&source);
    editor.file_path = Some(std::env::current_dir().unwrap().join("preview-link.md"));
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    app.toggle_markdown_preview_tab();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 25)).unwrap();
    terminal
        .draw(|frame| draw_markdown_preview(frame, &mut app, Rect::new(0, 0, 90, 25)))
        .unwrap();
    let view = app
        .editor()
        .unwrap()
        .markdown_preview_scroll
        .as_ref()
        .unwrap();
    let (index, link) = view
        .links
        .iter()
        .enumerate()
        .find_map(|(i, links)| links.first().map(|link| (i, link)))
        .unwrap();
    let (col, row) = (
        view.body.x + link.cells.start as u16,
        view.body.y + index as u16,
    );
    crate::mouse_selection::handle_editor_area_left_click(
        &mut app,
        col,
        row,
        crossterm::event::KeyModifiers::CONTROL,
    );
    let view = app
        .editor()
        .unwrap()
        .markdown_preview_scroll
        .as_ref()
        .unwrap();
    assert!(view.row > 30);
    assert!(view.selected_text().is_none());
    assert_eq!(app.editor().unwrap().rope.to_string(), source);
}

#[test]
fn mermaid_rendering_and_fallback_are_cached_and_stay_within_the_pane() {
    let source = "# Diagrams\n\n```mermaid\nflowchart TD\nA[Input] --> B{Valid?}\nB -->|Yes| C[Output]\nB -->|No| D[Error]\n```\n\n```mermaid\nsequenceDiagram\nparticipant A as Alice\nparticipant B as Bob\nA->>B: Hello\nB-->>A: Welcome\n```\n\n```mermaid\npie title Languages\n\"Rust\": 70\n\"Python\": 30\n```\n\n```mermaid\nunsupportedDiagram\nkeep this source\n```";
    let doc = PreparedDocument::new(source);
    assert_eq!(doc.diagrams.len(), 3);
    let wide = text(&doc.render(90, Palette::from(&EditorTheme::Dark.colors()), None));
    for expected in [
        "Flowchart",
        "Input",
        "Valid?",
        "Output",
        "Yes",
        "No",
        "Alice",
        "Bob",
        "Hello",
        "70.0%",
        "30.0%",
        "unsupportedDiagram",
        "keep this source",
    ] {
        assert!(wide.contains(expected), "{expected}: {wide}");
    }
    for width in [90, 1, 6, 15, 35, 60, 100] {
        let rows = doc.render(width, Palette::from(&EditorTheme::Dark.colors()), None);
        assert!(
            rows.iter().all(|row| row.line.width() <= width),
            "width {width}"
        );
        assert_eq!(format!("{rows:?}"), format!("{:?}", render(source, width)));
    }
}

#[test]
fn pathological_nesting_and_embedded_images_have_bounded_output() {
    let source = format!(
        "{}deep text\n\n![alt](data:image/png;base64,{})",
        "> ".repeat(2000),
        "a".repeat(10_000)
    );
    let rows = render(&source, 60);
    let output = text(&rows);
    assert!(output.contains("deep text"));
    assert!(output.contains("embedded image"));
    assert!(!output.contains("base64"));
    assert!(rows.len() < 1000);
    assert!(text(&render(&format!("{}deep list", "- ".repeat(200)), 60)).contains("deep list"));
}

#[test]
fn preview_defers_offscreen_highlighting_and_keeps_scroll_and_selection_maps() {
    PREVIEWS.with_borrow_mut(Vec::clear);
    let source = format!(
        "# Intro\n\n{}\n```rust\nfn main() {{ let text = \"café 世界 👩‍💻\"; }}\n```\n\n{}\n```python\ndef test():\n    return 1\n```",
        "Paragraph.\n\n".repeat(30),
        "More text.\n\n".repeat(30)
    );
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::new();
    editor.rope = ropey::Rope::from_str(&source);
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 20)).unwrap();
    terminal
        .draw(|f| draw_markdown_preview(f, &mut app, Rect::new(0, 0, 90, 20)))
        .unwrap();
    let (first, second, source_rows, text_rows) = PREVIEWS.with_borrow(|cache| {
        let entry = &cache[0];
        assert!(
            entry.document.highlighted.borrow().is_empty(),
            "opening a document must not highlight offscreen code"
        );
        (
            entry.code_views[0].clone(),
            entry.code_views[1].block,
            Rc::clone(&entry.source_rows),
            Rc::clone(&entry.text_rows),
        )
    });
    let editor = app.editor_mut().unwrap();
    let view = editor.markdown_preview_scroll.as_mut().unwrap();
    view.set_row(first.rows.start);
    view.select_all();
    let selected = view.selected_text();
    editor.scroll_offset = view.source_scroll;
    terminal
        .draw(|f| draw_markdown_preview(f, &mut app, Rect::new(0, 0, 90, 20)))
        .unwrap();
    let code = PREVIEWS.with_borrow(|cache| {
        let entry = &cache[0];
        assert!(entry.document.highlighted.borrow().contains(&first.block));
        assert!(!entry.document.highlighted.borrow().contains(&second));
        assert!(Rc::ptr_eq(&source_rows, &entry.source_rows));
        assert!(Rc::ptr_eq(&text_rows, &entry.text_rows));
        let (syntaxes, themes) = app.highlighter.shared_sets();
        let expected = render_document(
            &source,
            entry.width,
            entry.palette,
            Some((&syntaxes, themes.themes.get(&app.syntax_theme))),
        );
        assert_eq!(
            format!("{:?}", &entry.rows[first.rows.clone()]),
            format!("{:?}", &expected[first.rows.clone()])
        );
        Rc::clone(&entry.document.code.borrow()[&first.block])
    });
    assert_eq!(
        app.editor()
            .unwrap()
            .markdown_preview_scroll
            .as_ref()
            .unwrap()
            .selected_text(),
        selected
    );
    terminal
        .draw(|f| draw_markdown_preview(f, &mut app, Rect::new(0, 0, 90, 20)))
        .unwrap();
    PREVIEWS.with_borrow(|cache| {
        assert!(Rc::ptr_eq(
            &code,
            &cache[0].document.code.borrow()[&first.block]
        ))
    });
}

/// Exercise the user's temporary fixture without making it a build dependency.
#[test]
#[ignore = "manual full-document stress test and performance measurement"]
fn markdown_stress_document() {
    let source = std::fs::read_to_string("markdown_renderer_stress_test.md").unwrap();
    let start = std::time::Instant::now();
    let document = PreparedDocument::new(&source);
    let parse = start.elapsed();
    assert_eq!(document.diagrams.len(), 3);
    let palette = Palette::from(&EditorTheme::Dark.colors());
    let highlighter = crate::highlight::Highlighter::new();
    let (syntaxes, themes) = highlighter.shared_sets();
    let syntax = Some((&*syntaxes, themes.themes.get("dark-plus")));
    let start = std::time::Instant::now();
    let plain = document.render(100, palette, None);
    println!(
        "First layout without offscreen highlighting: {:.2} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    let start = std::time::Instant::now();
    let wide = document.render(100, palette, syntax);
    assert_eq!(preview_text_rows(&plain), preview_text_rows(&wide));
    let cold = start.elapsed();
    let output = text(&wide);
    assert!(output.contains("Markdown source"));
    assert!(output.contains("Load test.md"));
    assert!(output.contains("CommonMark · 50 (50.0%)"));
    assert!(!output.contains("HTML comment: this text should not be visible"));
    assert!(output.contains("End of Markdown renderer stress test."));
    let start = std::time::Instant::now();
    for width in [1, 8, 20, 40, 60, 80, 100] {
        let rows = document.render(width, palette, syntax);
        assert!(
            rows.iter().all(|row| row.line.width() <= width),
            "width {width}"
        );
        assert!(rows.iter().all(|row| row.source < source.lines().count()));
        assert_eq!(
            format!("{rows:?}"),
            format!("{:?}", render_document(&source, width, palette, syntax)),
            "width {width}"
        );
    }
    println!(
        "{} bytes: parse {:.2} ms; first highlighted layout {:.2} ms; seven cold/warm comparisons {:.2} ms",
        source.len(),
        parse.as_secs_f64() * 1000.0,
        cold.as_secs_f64() * 1000.0,
        start.elapsed().as_secs_f64() * 1000.0
    );
    let start = std::time::Instant::now();
    for width in [40, 50, 60, 70, 80, 90, 100] {
        std::hint::black_box(document.render(width, palette, syntax));
    }
    println!(
        "Warm reflow: {:.2} ms/width",
        start.elapsed().as_secs_f64() * 1000.0 / 7.0
    );
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::new();
    editor.rope = ropey::Rope::from_str(&source);
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 40)).unwrap();
    let start = std::time::Instant::now();
    terminal
        .draw(|f| draw_markdown_preview(f, &mut app, Rect::new(0, 0, 110, 40)))
        .unwrap();
    println!(
        "Preview first frame: {:.2} ms (offscreen code remains unhighlighted)",
        start.elapsed().as_secs_f64() * 1000.0
    );
    let start = std::time::Instant::now();
    for _ in 0..100 {
        let editor = app.editor_mut().unwrap();
        let view = editor.markdown_preview_scroll.as_mut().unwrap();
        view.scroll(1);
        editor.scroll_offset = view.source_scroll;
        terminal
            .draw(|f| draw_markdown_preview(f, &mut app, Rect::new(0, 0, 110, 40)))
            .unwrap();
    }
    println!(
        "Cached scrolling: {:.3} ms/frame (110 × 40 test terminal)",
        start.elapsed().as_secs_f64() * 10.0
    );
    std::fs::create_dir_all("scratch/markdown-check").unwrap();
    std::fs::write("scratch/markdown-check/stress-preview.txt", &output).unwrap();
    let styled: Vec<_> = wide.iter().map(|row| row.line.spans.iter().map(|span| serde_json::json!({"text":span.content,"fg":format!("{:?}", span.style.fg),"bg":format!("{:?}", span.style.bg),"modifiers":format!("{:?}", span.style.add_modifier)})).collect::<Vec<_>>()).collect();
    std::fs::write(
        "scratch/markdown-check/stress-spans.json",
        serde_json::to_string(&styled).unwrap(),
    )
    .unwrap();
}
