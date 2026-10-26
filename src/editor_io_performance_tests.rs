use super::*;
use std::{hint::black_box, time::Instant};

#[cfg(unix)]
#[test]
fn unchanged_save_rejects_a_replaced_fifo_without_waiting_or_marking_an_empty_document_saved() {
    use std::{
        os::unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, OpenOptionsExt},
        },
        sync::mpsc,
        time::Duration,
    };
    let root = tempfile::tempdir().unwrap();
    let mut stalled = Vec::new();
    for (index, text) in ["", "nonempty"].into_iter().enumerate() {
        let path = root.path().join(format!("source-{index}"));
        std::fs::write(&path, text).unwrap();
        let rope = Rope::from_str(text);
        let request = BackgroundSaveRequest {
            prepared_options: None,
            source_document_id: 1,
            path: path.clone(),
            rope: rope.clone(),
            encoding: "UTF-8",
            line_ending: "LF",
            trim_trailing: false,
            insert_final_newline: false,
            source_content_version: 0,
            fail_if_exists: false,
            expected_disk: Some(DiskSaveBaseline {
                rope,
                encoding: "UTF-8",
            }),
        };
        std::fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = send.send(write_background_save(&request).map(|_| ()));
        });
        let early = receive.recv_timeout(Duration::from_secs(3));
        let blocked = early.is_err();
        if blocked {
            // Unblock the old implementation too, so a failed regression can
            // report its error instead of leaving a stalled save worker.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(writer) => {
                        drop(writer);
                        break;
                    }
                    Err(error)
                        if error.raw_os_error() == Some(libc::ENXIO)
                            && Instant::now() < deadline =>
                    {
                        std::thread::yield_now()
                    }
                    Err(error) => panic!("Could not release FIFO fixture: {error}"),
                }
            }
        }
        let result = match early {
            Ok(result) => result,
            Err(_) => receive.recv_timeout(Duration::from_secs(5)).unwrap(),
        };
        worker.join().unwrap();
        stalled.push(blocked);
        assert!(
            result.is_err(),
            "a pipe is not a successfully saved empty regular file"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_fifo()
        );
    }
    assert_eq!(stalled, [false, false]);
    let path = root.path().join("ordinary");
    std::fs::write(&path, "unchanged").unwrap();
    let rope = Rope::from_str("unchanged");
    let request = BackgroundSaveRequest {
        prepared_options: None,
        source_document_id: 1,
        path,
        rope: rope.clone(),
        encoding: "UTF-8",
        line_ending: "LF",
        trim_trailing: false,
        insert_final_newline: false,
        source_content_version: 0,
        fail_if_exists: false,
        expected_disk: Some(DiskSaveBaseline {
            rope,
            encoding: "UTF-8",
        }),
    };
    assert!(!write_background_save(&request).unwrap().disk_written);
}

#[test]
fn cancelled_file_decoding_stops_utf8_lossy_and_utf16_readers_between_chunks() {
    struct Source {
        bytes: io::Cursor<Vec<u8>>,
        cancel: crate::background::CancellationToken,
    }
    impl io::Read for Source {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = output.len().min(4096);
            let read = io::Read::read(&mut self.bytes, &mut output[..count])?;
            if self.bytes.position() >= 20_000 {
                self.cancel.cancel();
            }
            Ok(read)
        }
    }
    impl io::Seek for Source {
        fn seek(&mut self, from: io::SeekFrom) -> io::Result<u64> {
            io::Seek::seek(&mut self.bytes, from)
        }
    }
    for encoding in ["UTF-8", "lossy", "UTF-16 LE", "UTF-16 BE"] {
        let text = "\tλ 🦀\r\n".repeat(20_000);
        let bytes = if encoding.starts_with("UTF-16") {
            let mut bytes = if encoding.ends_with("LE") {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if encoding.ends_with("LE") {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            bytes
        } else {
            let mut bytes = text.into_bytes();
            if encoding == "lossy" {
                bytes[3] = 255;
            }
            bytes
        };
        let size = bytes.len() as u64;
        let cancel = crate::background::CancellationToken::new();
        let mut source = Source {
            bytes: io::Cursor::new(bytes),
            cancel: cancel.clone(),
        };
        let result = super::load::read_text_stream_cancellable(
            &mut source,
            Path::new("never-open-this-path"),
            size,
            &cancel,
        );
        assert_eq!(
            result.unwrap_err().kind(),
            io::ErrorKind::Interrupted,
            "{encoding}"
        );
        assert!(source.bytes.position() <= 24_096, "{encoding}");
        assert!(source.bytes.position() < size);
    }
}

#[test]
fn native_file_open_rejects_directories_and_decodes_the_inspected_handle() {
    let root = tempfile::tempdir().unwrap();
    assert!(load_text_file(root.path()).is_err());
    let path = root.path().join("original.txt");
    let changed = root.path().join("renamed.txt");
    fs::write(&path, "original\r\nλ\n").unwrap();
    let (file, metadata) = crate::fs_ops::open_document_source(&path, false).unwrap();
    fs::rename(&path, &changed).unwrap();
    fs::write(&path, "replacement\n").unwrap();
    let loaded = decode_text_source(file, metadata, &path, None).unwrap();
    assert_eq!(loaded.rope.to_string(), "original\r\nλ\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "replacement\n");
}

struct Fragmented<'a> {
    bytes: &'a [u8],
    fragment: usize,
    consumed: usize,
    requested: usize,
}

impl std::io::Seek for Fragmented<'_> {
    fn seek(&mut self, _: std::io::SeekFrom) -> io::Result<u64> {
        panic!("valid or already identified encodings must not reread input");
    }
}

impl std::io::Read for Fragmented<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.requested = self.requested.max(output.len());
        let count = output.len().min(self.fragment).min(self.bytes.len());
        output[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        self.consumed += count;
        Ok(count)
    }
}

#[test]
fn streamed_utf8_matches_lossy_reference_across_buffers_and_short_reads() {
    for prefix in [0, 1, 8190, 8191, 8192, FILE_READ_BUFFER_BYTES - 2] {
        let mut payload = vec![b'x'; prefix];
        payload.extend_from_slice("🙂界\r\n".as_bytes());
        payload.extend_from_slice(&[0xf0, 0x9f, b'a', 0xff, 0x80, 0xed, 0xa0, 0x80]);
        payload.extend_from_slice(&[0xe2, 0x82]); // incomplete final scalar
        for bom in [false, true] {
            let mut bytes = if bom {
                vec![0xef, 0xbb, 0xbf]
            } else {
                Vec::new()
            };
            bytes.extend_from_slice(&payload);
            for fragment in [1, 2, 3, 7, 8191, usize::MAX] {
                let mut input = Fragmented {
                    bytes: &bytes,
                    fragment,
                    consumed: 0,
                    requested: 0,
                };
                let (rope, lossy) =
                    super::load::read_utf8(io::BufReader::with_capacity(8192, &mut input)).unwrap();
                assert_eq!(rope, String::from_utf8_lossy(&bytes).as_ref());
                assert!(lossy);
                assert_eq!(input.consumed, bytes.len());
                assert!(input.requested <= FILE_READ_BUFFER_BYTES);
            }
        }
    }
}

#[test]
fn invalid_utf8_decodes_once_without_rewinding_or_reopening_the_source() {
    struct Source {
        bytes: io::Cursor<Vec<u8>>,
        seeks: usize,
        largest_read: usize,
        read_bytes: usize,
    }
    impl std::io::Read for Source {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.largest_read = self.largest_read.max(output.len());
            let count = std::io::Read::read(&mut self.bytes, output)?;
            self.read_bytes += count;
            Ok(count)
        }
    }
    impl std::io::Seek for Source {
        fn seek(&mut self, position: std::io::SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            std::io::Seek::seek(&mut self.bytes, position)
        }
    }
    for invalid in [3, 20_000, 39_999] {
        for bom in [false, true] {
            let mut bytes = vec![b'a'; 40_000];
            bytes[invalid] = 0xff;
            let expected = String::from_utf8_lossy(&bytes).into_owned();
            if bom {
                bytes.splice(..0, [0xef, 0xbb, 0xbf]);
            }
            let size = bytes.len() as u64;
            let mut source = Source {
                bytes: io::Cursor::new(bytes),
                seeks: 0,
                largest_read: 0,
                read_bytes: 0,
            };
            let (rope, encoding) =
                read_text_stream(&mut source, Path::new("must-not-be-opened"), size).unwrap();
            assert_eq!(rope, expected);
            assert_eq!(
                encoding,
                if bom {
                    "UTF-8 with BOM"
                } else {
                    "UTF-8 (lossy)"
                }
            );
            assert_eq!(source.seeks, 0);
            assert_eq!(source.read_bytes, size as usize);
            assert!(source.largest_read <= FILE_READ_BUFFER_BYTES);
        }
    }
}

#[test]
fn streamed_utf16_matches_lossy_reference_with_split_surrogates_and_odd_bytes() {
    for little in [false, true] {
        for prefix in [0, 4093, 4094, 4095, FILE_READ_BUFFER_BYTES / 2 - 1] {
            let mut units = vec![b'a' as u16; prefix];
            units.extend([
                0xd83d,
                0xde00,
                0xd800,
                0xd800,
                b'x' as u16,
                0xdc00,
                0x4e16,
                13,
                10,
                0,
                0xd801,
            ]);
            let expected = String::from_utf16_lossy(&units);
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            bytes.extend(units.iter().flat_map(|unit| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            }));
            for odd in [false, true] {
                if odd {
                    bytes.push(0xff);
                }
                for fragment in [1, 3, 8191, usize::MAX] {
                    let mut input = Fragmented {
                        bytes: &bytes,
                        fragment,
                        consumed: 0,
                        requested: 0,
                    };
                    let (rope, encoding) =
                        read_text_stream(&mut input, Path::new("unused"), bytes.len() as u64)
                            .unwrap();
                    assert_eq!(rope, expected);
                    assert_eq!(encoding, if little { "UTF-16 LE" } else { "UTF-16 BE" });
                    assert_eq!(input.consumed, bytes.len());
                    assert!(input.requested <= FILE_READ_BUFFER_BYTES);
                }
            }
        }
    }
}

#[test]
fn decoder_does_not_trust_stale_file_size_or_swallow_read_errors() {
    for size in [0, 1, 99_999] {
        let (rope, encoding) = read_text_stream(
            io::Cursor::new(b"\xff\xfeh\0i\0"),
            Path::new("unused"),
            size,
        )
        .unwrap();
        assert_eq!(rope, "hi");
        assert_eq!(encoding, "UTF-16 LE");
        assert_eq!(
            read_text_stream(io::Cursor::new(b"bin\0ary"), Path::new("unused"), size)
                .unwrap()
                .1,
            "Binary"
        );
    }
    struct Fails {
        remaining: usize,
        interrupted: bool,
    }
    impl std::io::Read for Fails {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.remaining == 0 {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let count = self.remaining.min(output.len());
            output[..count].fill(b'x');
            self.remaining -= count;
            Ok(count)
        }
    }
    impl std::io::Seek for Fails {
        fn seek(&mut self, _: std::io::SeekFrom) -> io::Result<u64> {
            panic!("filesystem errors must not retry as decoding errors");
        }
    }
    for remaining in [0, 12_000] {
        let result = read_text_stream(
            Fails {
                remaining,
                interrupted: false,
            },
            Path::new("unused"),
            30_000,
        );
        assert_eq!(
            result.err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}

#[test]
fn newline_and_backspace_keep_large_indentation_history_and_mixed_whitespace() {
    for indent in [
        " ".repeat(120_000),
        "\t ".repeat(30_000),
        "\u{2003} ".repeat(30_000),
    ] {
        let source = format!("{indent}value\r\nnext");
        for col in [0, 1, 4] {
            let mut editor = Editor::test_with_text(&source);
            editor.record_saved_snapshot(editor.rope.clone());
            editor.cursor_col = col;
            let prefix: String = indent.chars().take(col).collect();
            let byte = source
                .char_indices()
                .nth(col)
                .map_or(source.len(), |(byte, _)| byte);
            let expected = format!("{prefix}\n{prefix}{}", &source[byte..]);
            editor.insert_newline();
            assert_eq!(editor.rope, expected);
            assert_eq!((editor.cursor_line, editor.cursor_col), (1, col));
            editor.undo();
            assert_eq!(editor.rope, source);
            assert!(!editor.modified);
            editor.redo();
            assert_eq!(editor.rope, expected);
        }
        let mut editor = Editor::test_with_text(&source);
        editor.cursor_col = indent.chars().count();
        editor.record_saved_snapshot(editor.rope.clone());
        let removed = if indent.bytes().all(|b| b == b' ') {
            4
        } else {
            1
        };
        editor.backspace();
        let prefix: String = indent
            .chars()
            .take(indent.chars().count() - removed)
            .collect();
        assert_eq!(editor.rope, format!("{prefix}value\r\nnext"));
        editor.undo();
        assert_eq!(editor.rope, source);
        assert!(!editor.modified);
    }
}

#[test]
fn save_cleanup_preserves_unaffected_caches_and_tracks_exact_undo_range() {
    for (ending, trim, newline) in [
        ("tail  \n", true, false),
        ("tail", false, true),
        ("tail\r \n", true, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("save.rs");
        let source = format!("{}{ending}", "fn f() {\n    work();\n}\n".repeat(1000));
        std::fs::write(&path, &source).unwrap();
        let mut editor = Editor::test_with_text(&source);
        editor.file_path = Some(path);
        editor.record_saved_snapshot(editor.rope.clone());
        for line in 0..60 {
            editor.can_fold_line(line);
        }
        let builds = editor.fold_hint_cache.borrow().builds;
        editor.syntax_dirty_from = None;
        editor.syntax_dirty_through = None;
        let request = editor
            .background_save_request(trim, newline, false)
            .unwrap();
        let saved = write_background_save(&request).unwrap();
        let (start, end) = saved.transformed_lines.unwrap();
        assert_eq!(start, 3000);
        assert!(end >= start && end <= 3002);
        let written = saved.rope.clone();
        editor.apply_background_save(
            request.source_content_version,
            &request.rope,
            saved.rope,
            request.encoding,
            None,
            None,
            saved.transformed_lines,
            u64::MAX,
        );
        assert_eq!(
            editor.syntax_dirty_from,
            Some(3000),
            "cleanup must not invalidate the caret line"
        );
        for line in 0..60 {
            editor.can_fold_line(line);
        }
        assert_eq!(editor.fold_hint_cache.borrow().builds, builds);
        assert!(!editor.modified);
        editor.syntax_dirty_from = None;
        editor.syntax_dirty_through = None;
        editor.undo();
        assert_eq!(editor.rope, source);
        assert_eq!(editor.syntax_dirty_from, Some(3000));
        assert!(editor.modified);
        editor.redo();
        assert_eq!(editor.rope, written);
        assert!(!editor.modified);
    }
}

#[test]
#[ignore = "manual optimized file and editing benchmark; no disk latency included"]
fn benchmark_basic_file_and_edit_work() {
    fn measure(name: &str, rounds: usize, mut run: impl FnMut()) {
        let mut times = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..rounds {
                run();
            }
            times.push(start.elapsed().as_secs_f64() * 1e6 / rounds as f64);
        }
        times.sort_by(f64::total_cmp);
        eprintln!("BASIC {name}: {:.3} us", times[4]);
    }
    for (name, text, utf16, rounds) in [
        (
            "decode small UTF-8",
            "fn main() { println!(\"hello\"); }\n".repeat(30),
            false,
            1000,
        ),
        (
            "decode 8MB UTF-8",
            "let result = calculate(value);\r\n".repeat(260_000),
            false,
            5,
        ),
        (
            "decode 8MB UTF-16",
            "let result = calculate(value);\r\n".repeat(130_000),
            true,
            5,
        ),
        (
            "decode Unicode UTF-16",
            "let café = \"世界🙂\";\r\n".repeat(120_000),
            true,
            5,
        ),
    ] {
        let bytes = if utf16 {
            [
                vec![0xff, 0xfe],
                text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ]
            .concat()
        } else {
            text.as_bytes().to_vec()
        };
        measure(name, rounds, || {
            black_box(
                read_text_stream(
                    io::Cursor::new(bytes.as_slice()),
                    Path::new("unused"),
                    bytes.len() as u64,
                )
                .unwrap(),
            );
        });
    }
    for (name, text, col, edit, rounds) in [
        (
            "Enter normal code",
            "    calculate(value);\n".to_owned(),
            21,
            Editor::insert_newline as fn(&mut Editor),
            3000,
        ),
        (
            "Enter at start of 2MB indentation",
            format!("{}value", " ".repeat(2_000_000)),
            0,
            Editor::insert_newline,
            5,
        ),
        (
            "Enter inside 2MB indentation",
            format!("{}value", " ".repeat(2_000_000)),
            4,
            Editor::insert_newline,
            5,
        ),
        (
            "Backspace in 2MB indentation",
            format!("{}value", " ".repeat(2_000_000)),
            2_000_000,
            Editor::backspace,
            5,
        ),
    ] {
        let source = Rope::from_str(&text);
        let mut editor = Editor::new();
        measure(name, rounds, || {
            editor.rope = source.clone();
            editor.cursor_line = 0;
            editor.cursor_col = col;
            editor.undo_stack.clear();
            editor.break_undo_group();
            edit(&mut editor);
            black_box(&editor.rope);
        });
    }
}

#[test]
#[ignore = "manual optimized comparison of save cleanup cache invalidation"]
fn benchmark_save_cleanup_cache_reuse() {
    let source = Rope::from_str(&format!("{}tail  \n", "    value();\n".repeat(5000)));
    let saved = super::save::prepare_rope(&source, true, false, false);
    let mut samples = [Vec::new(), Vec::new()];
    for sample in 0..15 {
        for local in [sample % 2 == 0, sample % 2 != 0] {
            let mut editor = Editor::new();
            editor.rope = source.clone();
            editor.record_saved_snapshot(source.clone());
            for line in 0..4000 {
                black_box(editor.can_fold_line(line));
            }
            let start = Instant::now();
            editor.apply_background_save(
                editor.content_version,
                &source,
                saved.clone(),
                "UTF-8",
                None,
                None,
                Some(if local {
                    (5000, 5000)
                } else {
                    (0, source.len_lines())
                }),
                u64::MAX,
            );
            for line in 0..60 {
                black_box(editor.can_fold_line(line));
            }
            samples[usize::from(local)].push(start.elapsed());
            assert!(!editor.modified);
            assert_eq!(editor.rope, saved);
        }
    }
    for times in &mut samples {
        times.sort();
    }
    eprintln!(
        "BASIC save cleanup plus 60 cached gutter hints: full={:?}, local={:?}",
        samples[0][7], samples[1][7]
    );
}
