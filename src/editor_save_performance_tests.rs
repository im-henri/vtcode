use super::*;

fn reference_preparation(source: &Rope, trim: bool, newline: bool, crlf: bool) -> String {
    let mut result = String::new();
    for line in source.lines() {
        let text = line.to_string();
        let (body, ending) = if let Some(body) = text.strip_suffix("\r\n") {
            (body, "\r\n")
        } else if let Some(body) = text.strip_suffix('\n') {
            (body, "\n")
        } else if let Some(body) = text.strip_suffix('\r') {
            (body, "\r")
        } else {
            (text.as_str(), "")
        };
        result.push_str(if trim {
            body.trim_end_matches([' ', '\t'])
        } else {
            body
        });
        result.push_str(ending);
    }
    if newline && !result.ends_with(['\r', '\n']) {
        result.push_str(if crlf { "\r\n" } else { "\n" });
    }
    if !crlf {
        return result;
    }
    let mut normalized = String::new();
    let mut previous_cr = false;
    for ch in result.chars() {
        if ch == '\n' && !previous_cr {
            normalized.push('\r');
        }
        normalized.push(ch);
        previous_cr = ch == '\r';
    }
    normalized
}

#[test]
fn save_preparation_preserves_unicode_and_whitespace_across_rope_boundaries() {
    let mut fixtures = vec![
        "".into(),
        " \t".into(),
        "\n\r\r\n\n".into(),
        "\r \n".into(),
        "α \t\r\n界\t\ré😀  \nlast\t".into(),
    ];
    for size in [1023, 1024, 1025, 8191, 8192, 8193, 70_000] {
        fixtures.push(format!(
            "α{}\r\n{}\nlast  ",
            " \t".repeat(size),
            "界".repeat(size)
        ));
        fixtures.push(format!("start\r{}\nend", " \t".repeat(size)));
        fixtures.push(format!(
            "{}\t\r{}  \n\t{}",
            "ä".repeat(size),
            "x\t\n".repeat(size / 3),
            " ".repeat(size)
        ));
    }
    // Deterministic mixtures of separators, Unicode, blank rows and long lines.
    let mut seed = 17u64;
    let spaces = " ".repeat(2049);
    let parts = ["x", "😀界", " ", "\t", "\r", "\n", "\r\n", spaces.as_str()];
    let mut random = String::new();
    for _ in 0..2000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        random.push_str(parts[(seed >> 32) as usize % parts.len()]);
    }
    fixtures.push(random);
    for text in fixtures {
        let source = Rope::from_str(&text);
        for trim in [false, true] {
            for newline in [false, true] {
                for crlf in [false, true] {
                    let expected = reference_preparation(&source, trim, newline, crlf);
                    let actual = save::prepare_rope(&source, trim, newline, crlf);
                    assert!(
                        actual == expected,
                        "{} bytes, trim={trim}, newline={newline}, crlf={crlf}",
                        text.len()
                    );
                    if expected == text {
                        assert!(
                            actual.is_instance(&source),
                            "unchanged preparation must share the snapshot"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn save_preparation_shares_large_untouched_regions_around_local_cleanup() {
    let source = Rope::from_str(&format!(
        "{}dirty  \n{}",
        "αβγ\n".repeat(10_000),
        "tail\n".repeat(10_000)
    ));
    let prepared = save::prepare_rope(&source, true, true, false);
    assert_eq!(prepared, source.to_string().replace("dirty  \n", "dirty\n"));
    let shared = source
        .chunks()
        .map(|s| s.as_ptr())
        .collect::<std::collections::HashSet<_>>();
    let shared_bytes: usize = prepared
        .chunks()
        .filter(|s| shared.contains(&s.as_ptr()))
        .map(str::len)
        .sum();
    assert!(shared_bytes > source.len_bytes() * 9 / 10);
}

#[test]
fn save_encoders_batch_output_and_preserve_exact_bytes() {
    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        writes: usize,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !bytes.is_empty() {
                self.writes += 1;
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    for encoding in [
        "UTF-8",
        "UTF-8 with BOM",
        "UTF-16 LE",
        "UTF-16 BE",
        "ASCII",
        "ISO-8859-1",
    ] {
        let text = match encoding {
            "ASCII" => "abc\r\n123\nend\r",
            "ISO-8859-1" => "caféÿ\r\n123\nend\r",
            _ => "café界😀\r\n123\nend\r",
        }
        .repeat(20_000);
        let rope = Rope::from_str(&text);
        let mut output = Output::default();
        save::write_encoded(&rope, encoding, &mut output).unwrap();
        let expected: Vec<u8> = match encoding {
            "UTF-16 LE" => [0xFF, 0xFE]
                .into_iter()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect(),
            "UTF-16 BE" => [0xFE, 0xFF]
                .into_iter()
                .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
                .collect(),
            "UTF-8 with BOM" => [0xEF, 0xBB, 0xBF].into_iter().chain(text.bytes()).collect(),
            "ISO-8859-1" => text.chars().map(|ch| ch as u8).collect(),
            _ => text.into_bytes(),
        };
        assert_eq!(output.bytes, expected, "{encoding}");
        assert!(
            output.writes < output.bytes.len() / 128,
            "{encoding}: {} writes for {} bytes",
            output.writes,
            output.bytes.len()
        );
    }
}

#[test]
fn unchanged_save_verifies_disk_without_replacing_file_or_changing_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("original.txt");
    fs::write(&path, "saved\n").unwrap();
    let timestamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234567);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(timestamp))
        .unwrap();
    let mut editor = Editor::new();
    editor.open_file(&path).unwrap();
    // A second name for the same inode detects unnecessary atomic replacement.
    let witness = dir.path().join("witness.txt");
    fs::hard_link(&path, &witness).unwrap();
    for cleanup in [false, true] {
        if cleanup {
            editor.cursor_col = 5;
            editor.insert_char(' ');
        }
        let request = editor.background_save_request(true, true, false).unwrap();
        let saved = write_background_save(&request).unwrap();
        assert!(!saved.disk_written);
        assert_eq!(saved.transformed_lines.is_some(), cleanup);
        assert_eq!(
            saved.metadata.as_ref().unwrap().modified().unwrap(),
            timestamp
        );
        editor.apply_background_save(
            request.source_content_version,
            &request.rope,
            saved.rope,
            request.encoding,
            Some(timestamp),
            Some(6),
            saved.transformed_lines,
            u64::MAX,
        );
        assert!(!editor.modified);
        if cleanup {
            editor.undo();
            assert_eq!(editor.rope, "saved \n");
            assert!(editor.modified);
            editor.redo();
            assert!(!editor.modified);
        }
    }
    // Modify through the witness to prove the unchanged save kept the inode.
    // Same length and timestamp must still fail the exact byte check.
    fs::write(&witness, "other\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(timestamp))
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "other\n");
    assert!(
        write_background_save(&editor.background_save_request(false, false, false).unwrap())
            .is_err()
    );
    assert_eq!(editor.rope, "saved\n");
}

#[test]
fn equal_text_with_a_new_encoding_still_writes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("original.txt");
    fs::write(&path, "saved\n").unwrap();
    let mut editor = Editor::new();
    editor.open_file(&path).unwrap();
    editor.set_encoding("UTF-8 with BOM");
    let saved =
        write_background_save(&editor.background_save_request(false, false, false).unwrap())
            .unwrap();
    assert!(saved.disk_written);
    assert!(saved.transformed_lines.is_none());
    assert_eq!(fs::read(&path).unwrap(), b"\xEF\xBB\xBFsaved\n");
}

#[test]
#[ignore = "manual optimized save timing; filesystem cache and host load affect results"]
fn benchmark_save_pipeline() {
    use std::{hint::black_box, time::Instant};

    fn median(mut values: Vec<std::time::Duration>) -> f64 {
        values.sort_unstable();
        values[values.len() / 2].as_secs_f64() * 1000.0
    }

    for (name, text, trim, crlf) in [
        (
            "clean LF",
            "let value = 123;\n".repeat(100_000),
            true,
            false,
        ),
        (
            "trim LF",
            "let value = 123;  \t\n".repeat(100_000),
            true,
            false,
        ),
        (
            "trim + CRLF",
            "let value = 123;  \t\n".repeat(100_000),
            true,
            true,
        ),
    ] {
        let mut editor = Editor::test_with_text(&text);
        editor.cached_line_ending = if crlf { "CRLF" } else { "LF" };
        let mut times = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            black_box(editor.prepared_save_rope(trim, true));
            times.push(start.elapsed());
        }
        println!(
            "SAVE_PREP {name}: {:.3} ms ({} bytes)",
            median(times),
            text.len()
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("save.txt");
    for (name, lines, encoding, edited, trim) in [
        ("small edited UTF-8", 1_000, "UTF-8", true, false),
        ("large edited UTF-8", 400_000, "UTF-8", true, false),
        ("large unchanged UTF-8", 400_000, "UTF-8", false, false),
        ("large edited UTF-16", 400_000, "UTF-16 LE", true, false),
        ("large edited ASCII", 400_000, "ASCII", true, false),
        ("large trim UTF-8", 100_000, "UTF-8", true, true),
    ] {
        let text = "let value = 123;  \t\n".repeat(lines);
        let mut editor = Editor::test_with_text(&text);
        editor.encoding = encoding;
        editor.file_path = Some(path.clone());
        editor.record_saved_snapshot(editor.rope.clone());
        let mut original = Vec::new();
        editor
            .write_encoded_content(&editor.rope, &mut original)
            .unwrap();
        if edited {
            editor.insert_char('X');
        }
        let request = editor.background_save_request(trim, false, false).unwrap();
        for loaded in [false, true] {
            let mut times = Vec::new();
            for _ in 0..7 {
                fs::write(&path, &original).unwrap();
                // A normal open editor has already read its disk baseline.
                // Also retain the first-read case: on some Windows hosts,
                // opening a freshly written file dominates all save work.
                if loaded {
                    black_box(fs::read(&path).unwrap());
                }
                let start = Instant::now();
                black_box(write_background_save(&request).unwrap());
                times.push(start.elapsed());
            }
            println!(
                "SAVE_TOTAL {name}, loaded={loaded}: {:.3} ms ({} input bytes)",
                median(times),
                original.len()
            );
        }
    }
}
