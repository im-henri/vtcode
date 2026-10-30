use super::*;

struct Source {
    len: u64,
    position: u64,
    binary: Option<u64>,
    fill: u8,
    read_bytes: u64,
    seeks: Vec<u64>,
    fail_probe: bool,
    cancel_on_seek: Option<crate::background::CancellationToken>,
}

impl Source {
    fn new(len: u64, binary: Option<u64>) -> Self {
        Self {
            len,
            binary,
            position: 0,
            fill: b'x',
            read_bytes: 0,
            seeks: Vec::new(),
            fail_probe: false,
            cancel_on_seek: None,
        }
    }
}

impl Read for Source {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.fail_probe && !self.seeks.is_empty() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let count = (self
            .len
            .saturating_sub(self.position)
            .min(output.len() as u64)) as usize;
        // Fail a regression before a virtual multi-gigabyte file can allocate
        // a multi-gigabyte text model in the test process.
        assert!(self.read_bytes + count as u64 <= 8 * PROBE_THRESHOLD,
            "unexpected full read of the virtual binary test file");
        output[..count].fill(self.fill);
        if let Some(binary) = self
            .binary
            .filter(|&at| at >= self.position && at < self.position + count as u64)
        {
            output[(binary - self.position) as usize] = 0;
        }
        self.read_bytes += count as u64;
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for Source {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.len) + i128::from(offset),
        };
        self.position =
            u64::try_from(position).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.seeks.push(self.position);
        if let Some(cancel) = &self.cancel_on_seek {
            cancel.cancel();
        }
        Ok(self.position)
    }
}

#[test]
fn multi_gigabyte_binary_opens_read_only_bounded_head_middle_and_tail_samples() {
    let len = 8 * 1024 * 1024 * 1024;
    for binary in [0, len / 2, len - 1] {
        let mut source = Source::new(len, Some(binary));
        let (rope, encoding) = read_text_file(&mut source, Path::new("large.bin"), len).unwrap();
        assert_eq!(encoding, "Binary");
        assert!(rope.to_string().contains("Binary file - cannot display"));
        assert!(rope.len_bytes() < 1024);
        assert!(
            source.read_bytes <= 3 * HEADER_BYTES as u64,
            "read {} bytes for binary at {binary}",
            source.read_bytes
        );
    }
}

#[test]
fn obvious_binary_without_nuls_and_utf8_bom_binary_stop_at_the_header() {
    for fill in [0xff, 1] {
        let mut source = Source::new(8 * 1024 * 1024 * 1024, None);
        source.fill = fill;
        let len = source.len;
        assert_eq!(
            read_text_file(&mut source, Path::new("binary"), len)
                .unwrap()
                .1,
            "Binary"
        );
        assert_eq!(source.read_bytes, HEADER_BYTES as u64);
        assert!(source.seeks.is_empty());
    }
    assert_eq!(
        read_text_stream(
            io::Cursor::new(b"\xef\xbb\xbfdata\0binary"),
            Path::new("bom.bin"),
            14
        )
        .unwrap()
        .1,
        "Binary"
    );
}

#[test]
fn binary_between_samples_stops_streaming_before_reading_the_whole_body() {
    let len = 4 * PROBE_THRESHOLD;
    let mut source = Source::new(len, Some(3 * HEADER_BYTES as u64));
    let (rope, encoding) = read_text_file(&mut source, Path::new("late.bin"), len).unwrap();
    assert_eq!(encoding, "Binary");
    assert!(rope.len_bytes() < 1024);
    assert!(source.read_bytes <= 3 * HEADER_BYTES as u64 + FILE_READ_BUFFER_BYTES as u64);
    assert!(source.read_bytes < len);
}

#[test]
fn sampling_resumes_text_at_the_header_and_preserves_unicode_boms_and_lossy_text() {
    let len = 2 * PROBE_THRESHOLD;
    let mut source = Source::new(len, None);
    let (rope, encoding) = read_text_file(&mut source, Path::new("text"), len).unwrap();
    assert_eq!(encoding, "UTF-8");
    assert_eq!(rope.len_bytes() as u64, len);
    assert_eq!(source.read_bytes, len + 2 * HEADER_BYTES as u64);
    let text = "λ 文🙂\r\n".repeat(110_000);
    for bom in [false, true] {
        let mut bytes = text.as_bytes().to_vec();
        if bom {
            bytes.splice(..0, [0xef, 0xbb, 0xbf]);
        }
        let len = bytes.len() as u64;
        let (rope, encoding) =
            read_text_file(io::Cursor::new(bytes), Path::new("text"), len).unwrap();
        assert_eq!(rope, text);
        assert_eq!(encoding, if bom { "UTF-8 with BOM" } else { "UTF-8" });
    }
    let bytes = b"normal text\xff\n";
    let (rope, encoding) = read_text_file(
        io::Cursor::new(bytes),
        Path::new("lossy"),
        bytes.len() as u64,
    )
    .unwrap();
    assert_eq!(encoding, "UTF-8 (lossy)");
    assert_eq!(rope, "normal text\u{fffd}\n");
}

#[test]
fn utf16_text_and_ordinary_log_controls_are_not_classified_as_binary() {
    let text = "λ 文🙂\r\n".repeat(110_000);
    for little in [false, true] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let len = bytes.len() as u64;
        let (rope, encoding) =
            read_text_file(io::Cursor::new(bytes), Path::new("utf16"), len).unwrap();
        assert_eq!(rope, text);
        assert_eq!(encoding, if little { "UTF-16 LE" } else { "UTF-16 BE" });
    }
    assert!(!binary_sample(b"\x1b[31mred\x1b[0m\t log\r\n\x08\x0b\x0c"));
}

#[test]
fn failed_or_cancelled_probes_never_turn_io_failures_into_binary_previews() {
    let mut source = Source::new(2 * PROBE_THRESHOLD, None);
    source.fail_probe = true;
    let len = source.len;
    assert_eq!(
        read_text_file(&mut source, Path::new("denied"), len)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let cancel = crate::background::CancellationToken::new();
    let mut source = Source::new(2 * PROBE_THRESHOLD, None);
    source.cancel_on_seek = Some(cancel.clone());
    let len = source.len;
    assert_eq!(
        read_text_stream_cancellable(&mut source, Path::new("cancelled"), len, &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(source.read_bytes, HEADER_BYTES as u64);
}

#[test]
fn local_binary_with_a_text_header_keeps_the_message_read_only_and_retains_full_size() {
    use std::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large.bin");
    let mut file = std::fs::File::create(&path).unwrap();
    let len = 16 * PROBE_THRESHOLD;
    file.set_len(len).unwrap();
    file.write_all(&vec![b'x'; 4 * HEADER_BYTES]).unwrap();
    drop(file);
    let mut editor = crate::editor::Editor::new();
    editor.open_file(&path).unwrap();
    assert_eq!(editor.encoding, "Binary");
    assert!(editor.read_only);
    assert_eq!(editor.file_size, len);
    assert!(editor.rope.len_bytes() < 1024);
    assert!(
        editor
            .rope
            .to_string()
            .contains("Binary file - cannot display")
    );
    assert!(
        editor
            .background_save_request(false, false, false)
            .is_none()
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), len);
}
