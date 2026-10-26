use super::*;
use std::{hint::black_box, io::Write, time::Instant};

/// Deliberately opt-in: writes a real 1 GiB file without allocating its text in
/// the fixture. Run each case in a fresh process for useful resident-memory data.
#[test]
#[ignore = "manual optimized 1 GiB file workflow and memory measurement"]
fn benchmark_gib_file_workflow() {
    let mib = std::env::var("VTCODE_LARGE_FILE_MIB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1024);
    let case = std::env::var("VTCODE_LARGE_FILE_CASE").unwrap_or_else(|_| "log".into());
    let bytes = mib * 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("large.log");
    let mut chunk = vec![b' '; 64 * 1024];
    if case != "indent" {
        chunk.fill(b'x');
        if case != "long" {
            for row in chunk.chunks_mut(128) {
                row[..27].copy_from_slice(b"2026-10-23 INFO run output ");
                *row.last_mut().unwrap() = b'\n';
            }
        }
    }
    let mut file = fs::File::create(&path).unwrap();
    for offset in (0..bytes).step_by(chunk.len()) {
        file.write_all(&chunk[..chunk.len().min(bytes - offset)])
            .unwrap();
    }
    if case == "lossy" {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::End(-2)).unwrap();
        file.write_all(&[255]).unwrap();
    }
    drop(file);
    let memory = || {
        crate::process_usage::read_current_process()
            .resident_bytes
            .unwrap_or(0)
    };
    let base_memory = memory();
    let start = Instant::now();
    let loaded = load_text_file(&path).unwrap();
    eprintln!(
        "GIB {case} bytes={bytes} load_ms={:.3} resident_delta_mib={:.1}",
        start.elapsed().as_secs_f64() * 1000.0,
        memory().saturating_sub(base_memory) as f64 / 1048576.0
    );
    assert_eq!(
        loaded.rope.len_bytes(),
        bytes + usize::from(case == "lossy") * 2
    );
    let mut editor = Editor::new_loading(&path, loaded.file_size);
    let start = Instant::now();
    editor.apply_bg_loaded(
        loaded.rope,
        loaded.encoding,
        loaded.file_size,
        LARGE_FILE_THRESHOLD,
    );
    eprintln!(
        "GIB {case} install_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert!(editor.large_file_mode || bytes <= LARGE_FILE_THRESHOLD as usize);
    assert!(
        editor
            .saved_rope
            .as_ref()
            .unwrap()
            .is_instance(&editor.rope)
    );
    let saved = editor.rope.clone();
    let start = Instant::now();
    let snapshots: Vec<_> = (0..500).map(|_| editor.rope.clone()).collect();
    eprintln!(
        "GIB {case} 500_snapshots_us={:.3} resident_delta_mib={:.1}",
        start.elapsed().as_secs_f64() * 1e6,
        memory().saturating_sub(base_memory) as f64 / 1048576.0
    );
    assert!(snapshots.iter().all(|rope| rope.is_instance(&saved)));
    let mut latencies = Vec::new();
    for index in 0..21 {
        editor.cursor_line = editor.line_count().saturating_sub(1) * index / 20;
        editor.cursor_col = editor.line_len(editor.cursor_line).min(60);
        let start = Instant::now();
        editor.insert_char('!');
        editor.undo();
        editor.redo();
        editor.undo();
        latencies.push(start.elapsed().as_secs_f64() * 1000.0);
        assert!(editor.rope.is_instance(&saved));
        assert!(!editor.modified);
        editor.redo_stack.clear();
    }
    latencies.sort_by(f64::total_cmp);
    eprintln!(
        "GIB {case} edit_undo_redo_median_ms={:.3} max_ms={:.3}",
        latencies[10], latencies[20]
    );
    if case == "lossy" {
        return;
    } // Original malformed bytes intentionally differ.
    let request = editor.background_save_request(false, false, false).unwrap();
    let start = Instant::now();
    let result = write_background_save(&request).unwrap();
    assert!(!result.disk_written);
    eprintln!(
        "GIB {case} unchanged_save_ms={:.3} resident_delta_mib={:.1}",
        start.elapsed().as_secs_f64() * 1000.0,
        memory().saturating_sub(base_memory) as f64 / 1048576.0
    );
    black_box((snapshots, result));

    editor.cursor_line = editor.line_count().saturating_sub(1) / 2;
    editor.cursor_col = editor.line_len(editor.cursor_line).min(60);
    editor.insert_char('!');
    let request = editor.background_save_request(false, false, false).unwrap();
    let start = Instant::now();
    let result = write_background_save(&request).unwrap();
    eprintln!(
        "GIB {case} edited_save_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert!(result.disk_written);
    assert!(result.rope.is_instance(&request.rope));
    assert_eq!(fs::metadata(&path).unwrap().len(), bytes as u64 + 1);
    editor.record_saved_snapshot(result.rope);
    assert!(!editor.modified);

    let mut peer = Editor::new();
    peer.copy_document_from(&editor);
    let mut shared_times = Vec::new();
    for index in 0..21 {
        editor.cursor_line = editor.line_count().saturating_sub(1) * index / 20;
        editor.cursor_col = editor.line_len(editor.cursor_line).min(60);
        let start = Instant::now();
        editor.insert_char('!');
        peer.refresh_document();
        editor.undo();
        peer.refresh_document();
        shared_times.push(start.elapsed().as_secs_f64() * 1000.0);
        assert!(peer.rope.is_instance(&editor.rope));
    }
    shared_times.sort_by(f64::total_cmp);
    eprintln!(
        "GIB {case} shared_edit_undo_median_ms={:.3} max_ms={:.3}",
        shared_times[10], shared_times[20]
    );

    let mut app = crate::app::App::test_new();
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.show_minimap = true;
    let area = ratatui::layout::Rect::new(0, 0, 180, 60);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 60)).unwrap();
    let mut frames = Vec::new();
    for index in 0..41 {
        let editor = &mut app.tabs[0].editor;
        editor.scroll_offset = editor.line_count().saturating_sub(60) * index / 40;
        editor.cursor_line = editor.scroll_offset;
        editor.selection_anchor = Some((editor.cursor_line, 0));
        editor.cursor_col = editor.line_len(editor.cursor_line).min(60);
        let start = Instant::now();
        terminal
            .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, area))
            .unwrap();
        frames.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    frames.sort_by(f64::total_cmp);
    eprintln!(
        "GIB {case} scrolling_frame_median_ms={:.3} max_ms={:.3}",
        frames[20], frames[40]
    );
    black_box(terminal.backend().buffer());
}

#[test]
fn indentation_sampling_ignores_pathological_prefixes_and_still_detects_ordinary_lines() {
    let mut editor = Editor::test_with_text(&format!(
        "{}value\n  first\n  second\n  third\n",
        " ".repeat(1_000_000),
    ));
    editor.detect_indentation();
    assert!(!editor.use_tabs);
    assert_eq!(editor.tab_size, 2);
    // A whitespace-only line must not outweigh the actual indentation samples.
    editor.rope = Rope::from_str(&format!(
        "{}\n\tfirst\n\tsecond\n\tthird\n",
        " ".repeat(1_000_000)
    ));
    editor.detect_indentation();
    assert!(editor.use_tabs);
}

#[test]
fn large_saved_history_and_duplicate_views_retain_shared_text_leaves() {
    let mut editor = Editor::test_with_text(&"log output line\n".repeat(300_000));
    editor.saved_rope = Some(editor.rope.clone());
    editor.update_large_file_mode(1024 * 1024);
    let saved = editor.rope.clone();
    let leaf = |rope: &Rope| rope.chunk_at_byte(rope.len_bytes() / 2).0.as_ptr();
    let shared_leaf = leaf(&saved);
    let mut peer = Editor::new();
    peer.copy_document_from(&editor);
    for _ in 0..50 {
        editor.break_undo_group();
        editor.insert_char('!');
        peer.refresh_document();
        assert_eq!(leaf(&editor.rope), shared_leaf);
        assert!(peer.rope.is_instance(&editor.rope));
        assert!(peer.shares_document(&editor));
    }
    assert_eq!(editor.undo_stack.len(), 50);
    assert!(
        editor
            .undo_stack
            .read()
            .iter()
            .all(|state| leaf(&state.rope) == shared_leaf)
    );
    for _ in 0..50 {
        editor.undo();
    }
    peer.refresh_document();
    assert!(peer.rope.is_instance(&saved));
    assert!(!peer.modified);
}

#[test]
fn shared_views_enter_large_file_mode_without_retaining_wrap_or_fold_layouts() {
    let mut editor = Editor::test_with_text(&"source line\n".repeat(300));
    editor.saved_rope = Some(editor.rope.clone());
    let mut peer = Editor::new();
    peer.copy_document_from(&editor);
    peer.word_wrap = true;
    peer.folded_ranges.insert(2, 200);
    peer.syntax_dirty_from = None;
    peer.syntax_dirty_through = None;
    crate::editor_document::edit!(editor, {
        editor.update_large_file_mode(1024);
    });
    editor.cursor_line = 150;
    editor.paste_plain_with_text("new\nline");
    editor.cursor_line = 250;
    editor.insert_char('!');
    peer.refresh_document();
    assert!(peer.large_file_mode);
    assert!(!peer.word_wrap);
    assert!(peer.folded_ranges.is_empty());
    assert!(peer.cached_row_offsets.is_none());
    assert!(peer.rope.is_instance(&editor.rope));
    assert_eq!(peer.syntax_dirty_from, Some(0));
    assert!(peer.syntax_dirty_through.unwrap() >= peer.line_count() - 1);
    editor.undo();
    editor.undo();
    peer.refresh_document();
    assert!(!peer.modified);
    assert!(peer.rope.is_instance(peer.saved_rope.as_ref().unwrap()));
}

#[test]
fn large_decoder_accepts_forward_only_streams_and_preserves_read_errors() {
    struct Forward {
        input: io::Cursor<Vec<u8>>,
        consumed: usize,
        fail: bool,
    }
    impl io::Read for Forward {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.fail && self.input.position() as usize == self.input.get_ref().len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source read failed",
                ));
            }
            let count = io::Read::read(&mut self.input, bytes)?;
            self.consumed += count;
            Ok(count)
        }
    }
    let mut bytes = "log output λ🙂\r\n".repeat(20_000).into_bytes();
    bytes.extend_from_slice(&[0xff, 0xf0, 0x9f]);
    let expected = String::from_utf8_lossy(&bytes).into_owned();
    let mut source = Forward {
        input: io::Cursor::new(bytes),
        consumed: 0,
        fail: false,
    };
    let (rope, encoding) = read_text_stream(&mut source, Path::new("not-a-file"), 0).unwrap();
    assert_eq!(rope, expected);
    assert_eq!(encoding, "UTF-8 (lossy)");
    assert_eq!(source.consumed, source.input.get_ref().len());
    source.input.set_position(0);
    source.fail = true;
    let error = read_text_stream(&mut source, Path::new("not-a-file"), 0).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "source read failed");
}

#[test]
fn line_ending_detection_uses_long_line_tails_and_preserves_the_ten_line_sample() {
    for ending in ["", "\n", "\r\n", "\r"] {
        let rope = Rope::from_str(&format!("{}{ending}", "x".repeat(2_000_000)));
        assert_eq!(
            rope_line_ending(&rope),
            if ending == "\r\n" { "CRLF" } else { "LF" }
        );
    }
    let rope = Rope::from_str(&format!("{}\r\n", "\r".repeat(10)));
    assert_eq!(rope_line_ending(&rope), "LF");
    let rope = Rope::from_str(&format!("{}x\r\n", "\r".repeat(9)));
    assert_eq!(rope_line_ending(&rope), "CRLF");
}
