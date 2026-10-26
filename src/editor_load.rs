//! Bounded text decoding for opens and external reloads.
use super::FILE_READ_BUFFER_BYTES;
use ropey::{Rope, RopeBuilder};
use std::{io::{self, BufRead, Read}, path::Path};

const HEADER_BYTES: usize = 8192;

pub(super) fn read_text_stream_cancellable(
    file: impl Read, path: &Path, file_size: u64, cancel: &crate::background::CancellationToken,
) -> io::Result<(Rope, &'static str)> {
    struct Reader<'a, R> { file: R, cancel: &'a crate::background::CancellationToken }
    impl<R: Read> Read for Reader<'_, R> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            // Interrupted is retried by decoders. Use a terminal error here;
            // the caller converts it back to cancellation after decoding exits.
            if self.cancel.is_cancelled() { return Err(io::Error::other("File open cancelled")); }
            self.file.read(bytes)
        }
    }
    let result = read_text_stream(Reader { file, cancel }, path, file_size);
    if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
    result
}

pub(super) fn read_text_stream(
    mut file: impl Read, path: &Path, file_size: u64,
) -> io::Result<(Rope, &'static str)> {
    let mut header = Vec::with_capacity(file_size.min(HEADER_BYTES as u64) as usize);
    // A stale size must not hide a BOM or binary header in a file that grew.
    file.by_ref().take(HEADER_BYTES as u64).read_to_end(&mut header)?;
    let (skip, encoding) = if header.starts_with(&[0xef, 0xbb, 0xbf]) {
        (3, "UTF-8 with BOM")
    } else if header.starts_with(&[0xff, 0xfe]) {
        (2, "UTF-16 LE")
    } else if header.starts_with(&[0xfe, 0xff]) {
        (2, "UTF-16 BE")
    } else if header.contains(&0) {
        let size_str = if file_size > 1024 * 1024 {
            format!("{:.1} MB", file_size as f64 / (1024.0 * 1024.0))
        } else if file_size > 1024 {
            format!("{:.1} KB", file_size as f64 / 1024.0)
        } else { format!("{file_size} bytes") };
        return Ok((Rope::from_str(&format!(
            "Binary file - cannot display\n\nFile: {}\nSize: {}\n\nThis file appears to be binary and cannot be edited as text.",
            path.display(), size_str,
        )), "Binary"));
    } else { (0, "UTF-8") };
    let utf16 = matches!(encoding, "UTF-16 LE" | "UTF-16 BE");
    if header.len() < HEADER_BYTES {
        // EOF was observed while reading the header. Ordinary small source
        // files need neither a second read nor a large streaming buffer.
        if utf16 {
            return Ok((read_utf16(&header[skip..], (header.len() - skip).max(2), encoding == "UTF-16 LE")?, encoding));
        }
        let text = String::from_utf8_lossy(&header[skip..]);
        let lossy = matches!(text, std::borrow::Cow::Owned(_));
        return Ok((Rope::from_str(&text), if lossy && skip == 0 { "UTF-8 (lossy)" } else { encoding }));
    }
    let capacity = file_size.clamp(HEADER_BYTES as u64, FILE_READ_BUFFER_BYTES as u64) as usize;
    if utf16 {
        let reader = header[skip..].chain(file);
        Ok((read_utf16(reader, capacity, encoding == "UTF-16 LE")?, encoding))
    } else {
        // One streaming builder handles valid and malformed UTF-8. In
        // particular, an invalid byte near EOF must not discard a gigabyte
        // of decoded text and reread the file. Valid blocks append directly
        // from this bounded buffer without Rope::from_reader's extra copy.
        let reader = header[skip..].chain(file);
        let (rope, lossy) = read_utf8(io::BufReader::with_capacity(capacity, reader))?;
        Ok((rope, if lossy && skip == 0 { "UTF-8 (lossy)" } else { encoding }))
    }
}

fn read_retry(reader: &mut impl Read, bytes: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

pub(super) fn read_utf8(mut reader: impl BufRead) -> io::Result<(Rope, bool)> {
    let mut decoder = Utf8Decoder {
        builder: RopeBuilder::new(), partial: [0; 4], pending: 0, lossy: false,
    };
    loop {
        // Ordinary UTF-8 is validated and appended directly from the input
        // buffer. Keep malformed text and boundary repair off this hot path.
        let bytes = match reader.fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if bytes.is_empty() {
            if decoder.pending != 0 { decoder.builder.append("\u{fffd}"); decoder.lossy = true; }
            return Ok((decoder.builder.finish(), decoder.lossy));
        }
        if decoder.pending == 0 && let Ok(text) = std::str::from_utf8(bytes) {
            decoder.builder.append(text);
        } else {
            decoder.append_fragments(bytes);
        }
        let used = bytes.len();
        reader.consume(used);
    }
}

struct Utf8Decoder {
    builder: RopeBuilder,
    partial: [u8; 4],
    pending: usize,
    lossy: bool,
}

impl Utf8Decoder {
    #[cold]
    fn append_fragments(&mut self, bytes: &[u8]) {
        let mut start = 0;
        while self.pending != 0 && start < bytes.len() {
            let previous = self.pending;
            self.partial[self.pending] = bytes[start];
            self.pending += 1;
            match std::str::from_utf8(&self.partial[..self.pending]) {
                Ok(text) => {
                    self.builder.append(text);
                    self.pending = 0;
                    start += 1;
                }
                Err(error) => match error.error_len() {
                    None => start += 1,
                    Some(length) => {
                        self.builder.append("\u{fffd}");
                        self.lossy = true;
                        self.pending = 0;
                        // A non-continuation byte belongs to the next scalar.
                        start += length.saturating_sub(previous);
                    }
                },
            }
        }
        while start < bytes.len() {
            match std::str::from_utf8(&bytes[start..]) {
                Ok(text) => { self.builder.append(text); break; }
                Err(error) => {
                    let valid_end = start + error.valid_up_to();
                    self.builder.append(std::str::from_utf8(&bytes[start..valid_end]).unwrap());
                    match error.error_len() {
                        Some(length) => {
                            self.lossy = true;
                            self.builder.append("\u{fffd}");
                            start = valid_end + length;
                        }
                        None => {
                            self.pending = bytes.len() - valid_end;
                            self.partial[..self.pending].copy_from_slice(&bytes[valid_end..]);
                            break;
                        }
                    }
                }
            }
        }
    }
}
fn read_utf16(mut reader: impl Read, capacity: usize, little: bool) -> io::Result<Rope> {
    let mut bytes = vec![0; capacity + 1];
    let mut pending = 0;
    let mut high = None;
    let mut text = String::with_capacity(capacity * 3 / 2 + 4);
    let mut builder = RopeBuilder::new();
    loop {
        let count = read_retry(&mut reader, &mut bytes[pending..pending + capacity])?;
        if count == 0 {
            if high.is_some() { builder.append("\u{fffd}"); }
            // Match the existing decoder: an incomplete final byte is ignored.
            return Ok(builder.finish());
        }
        let end = pending + count;
        for pair in bytes[..end & !1].chunks_exact(2) {
            let unit = if little { u16::from_le_bytes([pair[0], pair[1]]) }
                else { u16::from_be_bytes([pair[0], pair[1]]) };
            if let Some(previous) = high.take() {
                if (0xdc00..=0xdfff).contains(&unit) {
                    let scalar = 0x10000 + ((u32::from(previous) - 0xd800) << 10) + u32::from(unit) - 0xdc00;
                    text.push(char::from_u32(scalar).unwrap());
                    continue;
                }
                text.push('\u{fffd}');
            }
            match unit {
                0xd800..=0xdbff => high = Some(unit),
                0xdc00..=0xdfff => text.push('\u{fffd}'),
                _ => text.push(char::from_u32(u32::from(unit)).unwrap()),
            }
        }
        builder.append(&text);
        text.clear();
        pending = end & 1;
        if pending != 0 { bytes[0] = bytes[end - 1]; }
    }
}
