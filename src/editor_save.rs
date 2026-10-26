use super::{BackgroundSaveRequest, save_temp_path};
use ropey::Rope;
use std::borrow::Cow;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

/// A save needs immutable text and options, not an Editor, undo history, or UI caches.
#[derive(Debug)]
pub(crate) struct SavedFile {
    pub rope: Rope,
    pub metadata: Option<fs::Metadata>,
    pub transformed_lines: Option<(usize, usize)>,
    pub disk_written: bool,
}

pub(crate) fn write(request: &BackgroundSaveRequest) -> io::Result<SavedFile> {
    // Repeated saves of an exact normalized checkpoint need no cleanup scan.
    // Disk validation below remains mandatory, even when preparation is reused.
    let rope = request.prepare_text();
    // Compute exact invalidation bounds on the worker. A final newline or a
    // local whitespace cleanup must not discard the whole editor's caches.
    let transformed_lines = (!rope.is_instance(&request.rope)).then(|| {
        let (old, new) =
            crate::rope_changes::changed_lines(&request.rope, &rope, &|| false).unwrap();
        (
            old.start.min(new.start),
            old.end.max(new.end).saturating_sub(1),
        )
    });
    // Ctrl+S after Undo or repeated saves still check the actual disk bytes.
    // Once verified, identical content needs no temp file, write, or rename.
    if !request.fail_if_exists
        && let Some(expected) = &request.expected_disk
        && expected.encoding == request.encoding
        && crate::rope_changes::equal(&expected.rope, &rope)
    {
        return Ok(SavedFile {
            metadata: Some(expected.validate(&request.path)?),
            rope,
            transformed_lines,
            disk_written: false,
        });
    }
    // Resolve links before choosing the adjacent temporary file. Replacing the
    // link itself would leave the document we opened unchanged on disk.
    let (destination, metadata) = save_destination(&request.path, request.fail_if_exists)?;
    let temp_path = save_temp_path(&destination);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if metadata.is_some() {
        use std::os::unix::fs::OpenOptionsExt;
        // Keep pending contents private until the destination's permissions
        // have been rechecked. New files retain the normal umask behavior.
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = metadata;
    let file = options.open(&temp_path)?;
    let result = (|| {
        // One sequential write per large block, rather than the 8 KiB default.
        // Small files do not reserve a large buffer.
        let capacity = rope
            .len_bytes()
            .clamp(8 * 1024, super::FILE_READ_BUFFER_BYTES);
        let mut writer = io::BufWriter::with_capacity(capacity, file);
        write_encoded(&rope, request.encoding, &mut writer)?;
        writer.flush()?;
        // Keep this check immediately before publishing, after potentially slow I/O.
        let (current_destination, mut metadata) =
            save_destination(&request.path, request.fail_if_exists)?;
        if current_destination != destination {
            return Err(conflict(&request.path));
        }
        if let Some(expected) = &request.expected_disk {
            metadata = Some(expected.validate(&destination)?);
        }
        #[cfg(unix)]
        if let Some(metadata) = metadata {
            use std::os::unix::fs::PermissionsExt;
            // Rename alone only checks the directory's permissions. Opening
            // without truncation also honors the file's access permissions.
            let _ = fs::OpenOptions::new().write(true).open(&destination)?;
            // Restore ordinary permissions, including executable bits, but
            // never carry setuid/setgid privileges onto newly written code.
            writer
                .get_ref()
                .set_permissions(fs::Permissions::from_mode(
                    metadata.permissions().mode() & 0o777,
                ))?;
        }
        #[cfg(not(unix))]
        let _ = metadata;
        drop(writer);
        if request.fail_if_exists {
            // Publish a new Windows file in one operation. A hard-link/unlink
            // sequence can leave its destination unreplaceable when file reads
            // overlap publication, even after those readers have closed.
            #[cfg(windows)]
            crate::fs_ops::rename_no_replace(&temp_path, &destination)?;
            #[cfg(not(windows))]
            fs::hard_link(&temp_path, &destination)?;
        } else {
            fs::rename(&temp_path, &destination)?;
        }
        Ok(SavedFile {
            rope,
            metadata: fs::metadata(&request.path).ok(),
            transformed_lines,
            disk_written: true,
        })
    })();
    // Rename already consumed the temp path on success; avoid a failed delete
    // syscall (and its remote-filesystem round trip) in the common case.
    if result.is_err() || (!cfg!(windows) && request.fail_if_exists) {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn save_destination(
    path: &Path,
    fail_if_exists: bool,
) -> io::Result<(Cow<'_, Path>, Option<fs::Metadata>)> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if fail_if_exists && metadata.is_some() {
        // A dangling link is still an existing destination, not a new file.
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The destination already exists",
        ));
    }
    let (destination, metadata) = if metadata
        .as_ref()
        .is_some_and(|m| m.file_type().is_symlink())
    {
        let destination = fs::canonicalize(path)?;
        let metadata = fs::metadata(&destination)?;
        (Cow::Owned(destination), Some(metadata))
    } else {
        (Cow::Borrowed(path), metadata)
    };
    if let Some(metadata) = &metadata {
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The destination is not a regular file",
            ));
        }
        if metadata.permissions().readonly() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The destination file is read-only",
            ));
        }
    }
    Ok((destination, metadata))
}

/// Encodes already-normalized text. Newline conversion belongs to preparation,
/// so baseline validation and writing use exactly the same byte representation.
pub(super) fn write_encoded(
    rope: &Rope,
    encoding: &str,
    writer: &mut impl Write,
) -> io::Result<()> {
    match encoding {
        "UTF-16 LE" | "UTF-16 BE" => {
            let little = encoding == "UTF-16 LE";
            writer.write_all(if little { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] })?;
            let mut buffer = [0u8; 8192];
            let mut used = 0;
            for chunk in rope.chunks() {
                for unit in chunk.encode_utf16() {
                    buffer[used..used + 2].copy_from_slice(&if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                    used += 2;
                    if used == buffer.len() {
                        writer.write_all(&buffer)?;
                        used = 0;
                    }
                }
            }
            writer.write_all(&buffer[..used])
        }
        "ASCII" | "ISO-8859-1" => {
            let max = if encoding == "ASCII" { 0x7F } else { 0xFF };
            let mut buffer = [0u8; 8192];
            let mut used = 0;
            for chunk in rope.chunks() {
                if chunk.is_ascii() {
                    if used > 0 {
                        writer.write_all(&buffer[..used])?;
                        used = 0;
                    }
                    writer.write_all(chunk.as_bytes())?;
                    continue;
                }
                for ch in chunk.chars() {
                    if ch as u32 > max {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "{encoding} cannot represent U+{:04X}; choose UTF-8 to save this text without losing characters",
                                ch as u32
                            ),
                        ));
                    }
                    buffer[used] = ch as u8;
                    used += 1;
                    if used == buffer.len() {
                        writer.write_all(&buffer)?;
                        used = 0;
                    }
                }
            }
            writer.write_all(&buffer[..used])
        }
        _ => {
            if encoding == "UTF-8 with BOM" {
                writer.write_all(&[0xEF, 0xBB, 0xBF])?;
            }
            rope.write_to(writer)
        }
    }
}

/// Applies ordered byte edits in one pass. Short gaps feed a single RopeBuilder;
/// long untouched regions retain the original Rope nodes, including history's
/// shared leaves. There is no tree search/mutation or allocation for each line.
struct Rewrite<'a> {
    source: &'a Rope,
    result: Rope,
    builder: Option<ropey::RopeBuilder>,
    chunks: ropey::iter::Chunks<'a>,
    chunk: &'a str,
    chunk_start: usize,
    copied: usize,
}

impl<'a> Rewrite<'a> {
    fn new(source: &'a Rope) -> Self {
        let mut chunks = source.chunks();
        let chunk = chunks.next().unwrap_or_default();
        Self {
            source,
            result: Rope::new(),
            builder: None,
            chunks,
            chunk,
            chunk_start: 0,
            copied: 0,
        }
    }

    fn flush(&mut self) {
        if let Some(builder) = self.builder.take() {
            self.result.append(builder.finish());
        }
    }

    fn seek(&mut self, end: usize) {
        if end > self.chunk_start + self.chunk.len() {
            let (mut chunks, start, _, _) = self.source.chunks_at_byte(end);
            self.chunk = chunks.next().unwrap_or_default();
            self.chunks = chunks;
            self.chunk_start = start;
        }
        self.copied = end;
    }

    fn copy_to(&mut self, end: usize) {
        if end - self.copied >= 4096 {
            self.flush();
            self.result
                .append(Rope::from(self.source.byte_slice(self.copied..end)));
            self.seek(end);
            return;
        }
        while self.copied < end {
            if self.copied == self.chunk_start + self.chunk.len() {
                self.chunk_start = self.copied;
                self.chunk = self.chunks.next().unwrap_or_default();
            }
            let to = end.min(self.chunk_start + self.chunk.len());
            self.builder
                .get_or_insert_with(ropey::RopeBuilder::new)
                .append(&self.chunk[self.copied - self.chunk_start..to - self.chunk_start]);
            self.copied = to;
        }
    }

    fn replace(&mut self, start: usize, end: usize, replacement: &str) {
        self.copy_to(start);
        self.builder
            .get_or_insert_with(ropey::RopeBuilder::new)
            .append(replacement);
        self.seek(end);
    }

    fn finish(mut self) -> Rope {
        self.copy_to(self.source.len_bytes());
        self.flush();
        self.result
    }
}

pub(crate) fn prepare_rope(source: &Rope, trim: bool, final_newline: bool, crlf: bool) -> Rope {
    let mut rewrite = None;
    if trim || crlf {
        let mut offset = 0;
        let mut trailing_from = 0;
        let mut trailing_after_cr = false;
        for chunk in source.chunks() {
            let bytes = chunk.as_bytes();
            // Rope chunks never split CRLF. ASCII separators are also UTF-8
            // boundaries, so scanning does not decode the document's text.
            for newline in memchr::memchr2_iter(b'\r', b'\n', bytes) {
                let is_lf = bytes[newline] == b'\n';
                if is_lf && newline > 0 && bytes[newline - 1] == b'\r' {
                    continue;
                }
                let mut after_cr = false;
                if trim {
                    let local = bytes[..newline]
                        .iter()
                        .rposition(|b| !matches!(b, b' ' | b'\t'))
                        .map_or(0, |i| i + 1);
                    let start = if local == 0 {
                        trailing_from
                    } else {
                        offset + local
                    };
                    // Trimming can join a lone CR to a formerly separate LF,
                    // even across a long run of whitespace spanning chunks.
                    after_cr = if local == 0 {
                        trailing_after_cr
                    } else {
                        bytes[local - 1] == b'\r'
                    };
                    if start < offset + newline {
                        rewrite.get_or_insert_with(|| Rewrite::new(source)).replace(
                            start,
                            offset + newline,
                            "",
                        );
                    }
                }
                if crlf && is_lf && !after_cr {
                    rewrite.get_or_insert_with(|| Rewrite::new(source)).replace(
                        offset + newline,
                        offset + newline + 1,
                        "\r\n",
                    );
                }
            }
            if trim && let Some(last) = bytes.iter().rposition(|b| !matches!(b, b' ' | b'\t')) {
                trailing_from = offset + last + 1;
                trailing_after_cr = bytes[last] == b'\r';
            }
            offset += chunk.len();
        }
        if trim && trailing_from < source.len_bytes() {
            rewrite.get_or_insert_with(|| Rewrite::new(source)).replace(
                trailing_from,
                source.len_bytes(),
                "",
            );
        }
    }
    let mut rope = rewrite.map_or_else(|| source.clone(), Rewrite::finish);
    if final_newline
        && (rope.len_chars() == 0 || !matches!(rope.char(rope.len_chars() - 1), '\r' | '\n'))
    {
        rope.insert(rope.len_chars(), if crlf { "\r\n" } else { "\n" });
    }
    rope
}

/// The bytes last read or successfully written, independent of watcher state.
/// Cloning this on the UI thread only shares the Rope's backing storage.
#[derive(Clone)]
pub(crate) struct DiskSaveBaseline {
    pub rope: Rope,
    pub encoding: &'static str,
}

#[derive(Debug)]
struct ExternalChange(String);
impl std::fmt::Display for ExternalChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
}
impl std::error::Error for ExternalChange {}

pub(crate) fn conflict_message(message: String) -> io::Error {
    io::Error::other(ExternalChange(message))
}
pub(crate) fn is_conflict(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|error| error.is::<ExternalChange>())
}
pub(crate) fn conflict(path: &Path) -> io::Error {
    conflict_message(format!(
        "{} changed on disk. Saving would overwrite changes made outside VTCode. Your unsaved edits remain open.", path.display()
    ))
}

impl DiskSaveBaseline {
    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        self.encoding == other.encoding && self.rope.is_instance(&other.rope)
    }

    pub(super) fn validate(&self, path: &Path) -> io::Result<fs::Metadata> {
        self.validate_inner(path, None)
    }

    pub(crate) fn validate_cancellable(&self, path: &Path, cancel: &crate::background::CancellationToken) -> io::Result<fs::Metadata> {
        self.validate_inner(path, Some(cancel))
    }

    fn validate_inner(&self, path: &Path, cancel: Option<&crate::background::CancellationToken>) -> io::Result<fs::Metadata> {
        if cancel.is_some_and(|cancel| cancel.is_cancelled()) { return Err(io::ErrorKind::Interrupted.into()); }
        let (file, before) = crate::fs_ops::open_document_source(path, false).map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) {
                conflict(path)
            } else {
                error
            }
        })?;
        let encoded_len = match self.encoding {
            "UTF-16 LE" | "UTF-16 BE" => self.rope.len_utf16_cu() * 2 + 2,
            "ISO-8859-1" => self.rope.len_chars(),
            "UTF-8 with BOM" => self.rope.len_bytes() + 3,
            _ => self.rope.len_bytes(),
        };
        if before.len() != encoded_len as u64 {
            return Err(conflict(path));
        }
        let mut comparison = CompareBytes {
            reader: io::BufReader::with_capacity(
                encoded_len.clamp(8192, super::FILE_READ_BUFFER_BYTES),
                file,
            ),
            path,
            cancel,
        };
        // Use the same encoder as Save, without save-time newline conversion.
        // Comparison streams bytes; it never allocates another document Rope.
        write_encoded(&self.rope, self.encoding, &mut comparison)?;
        if cancel.is_some_and(|cancel| cancel.is_cancelled()) { return Err(io::ErrorKind::Interrupted.into()); }
        if !comparison.reader.fill_buf()?.is_empty() {
            return Err(conflict(path));
        }
        let after = std::fs::metadata(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                conflict(path)
            } else {
                error
            }
        })?;
        if !same_source_metadata(&before, &after) {
            return Err(conflict(path));
        }
        Ok(after)
    }
}

fn same_source_metadata(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // An atomic replacement or a writer restoring mtime must not pass the
        // post-read guard merely because size and mtime happen to match.
        before.dev() == after.dev() && before.ino() == after.ino()
            && before.ctime() == after.ctime() && before.ctime_nsec() == after.ctime_nsec()
    }
    #[cfg(not(unix))]
    true
}

#[cfg(all(test, unix))]
#[test]
fn external_replacement_cannot_reuse_equal_size_and_mtime_after_baseline_read() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.txt");
    std::fs::write(&path, "before").unwrap();
    let before = std::fs::metadata(&path).unwrap();
    std::fs::rename(&path, root.path().join("old.txt")).unwrap();
    std::fs::write(&path, "AFTER!").unwrap();
    std::fs::File::options().write(true).open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap())).unwrap();
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert!(!same_source_metadata(&before, &after));
}

struct CompareBytes<'a> {
    reader: io::BufReader<std::fs::File>,
    path: &'a Path,
    cancel: Option<&'a crate::background::CancellationToken>,
}

impl Write for CompareBytes<'_> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let length = bytes.len();
        while !bytes.is_empty() {
            if self.cancel.is_some_and(|cancel| cancel.is_cancelled()) { return Err(io::ErrorKind::Interrupted.into()); }
            let actual = self.reader.fill_buf()?;
            let count = actual.len().min(bytes.len());
            if count == 0 || actual[..count] != bytes[..count] {
                return Err(conflict(self.path));
            }
            self.reader.consume(count);
            bytes = &bytes[count..];
        }
        Ok(length)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
