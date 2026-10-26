//! Document differencing, serialization and pipe writes stay on the LSP writer.
//! Rope snapshots share unchanged storage with the editor and with each other.
use super::*;
use ropey::Rope;

#[derive(Default)]
pub(super) struct LspWriter {
    documents: HashMap<String, Rope>,
    incremental: bool,
    disabled: bool,
}

#[derive(Serialize)]
struct Notification<T> {
    jsonrpc: &'static str,
    method: &'static str,
    params: T,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenDocument<'a> {
    uri: &'a str,
    language_id: &'a str,
    version: i32,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenParams<'a> {
    text_document: OpenDocument<'a>,
}

#[derive(Serialize)]
struct DocumentVersion<'a> {
    uri: &'a str,
    version: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangeParams<'a> {
    text_document: DocumentVersion<'a>,
    content_changes: [TextDocumentContentChangeEvent; 1],
}

impl LspWriter {
    /// Reuses the JSON buffer; avoids constructing/copying a document-sized
    /// serde_json::Value and then copying the payload into a framing string.
    pub(super) fn encode(
        &mut self,
        message: LspOutbound,
        buffer: &mut Vec<u8>,
    ) -> serde_json::Result<bool> {
        buffer.clear();
        match message {
            LspOutbound::Json(json) => buffer.extend_from_slice(json.as_bytes()),
            LspOutbound::SyncKind(kind) => {
                self.incremental = kind == TextDocumentSyncKind::INCREMENTAL;
                self.disabled = kind == TextDocumentSyncKind::NONE;
                return Ok(false);
            }
            LspOutbound::DidOpen {
                uri,
                language_id,
                version,
                text,
            } => {
                serde_json::to_writer(
                    buffer,
                    &Notification {
                        jsonrpc: "2.0",
                        method: "textDocument/didOpen",
                        params: OpenParams {
                            text_document: OpenDocument {
                                uri: &uri,
                                language_id: &language_id,
                                version,
                                text: text.to_string(),
                            },
                        },
                    },
                )?;
                self.documents.insert(uri, text);
            }
            LspOutbound::DidChange { uri, version, text } => {
                if self.disabled {
                    return Ok(false);
                }
                let change = match self.documents.get(&uri).filter(|_| self.incremental) {
                    Some(previous) => incremental_change(previous, &text),
                    None => TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: text.to_string(),
                    },
                };
                serde_json::to_writer(
                    buffer,
                    &Notification {
                        jsonrpc: "2.0",
                        method: "textDocument/didChange",
                        params: ChangeParams {
                            text_document: DocumentVersion { uri: &uri, version },
                            content_changes: [change],
                        },
                    },
                )?;
                self.documents.insert(uri, text);
            }
            LspOutbound::DidClose { uri } => {
                self.documents.remove(uri.as_str());
                serde_json::to_writer(
                    buffer,
                    &Notification {
                        jsonrpc: "2.0",
                        method: "textDocument/didClose",
                        params: DidCloseTextDocumentParams {
                            text_document: TextDocumentIdentifier { uri },
                        },
                    },
                )?;
            }
        }
        Ok(true)
    }
}

/// A single replacement covers arbitrary edits, undo, reload and formatting.
/// Shared chunks are skipped by pointer, so ordinary typing does not inspect
/// every byte or allocate a copy of the entire document to compute a diff.
pub(super) fn incremental_change(previous: &Rope, next: &Rope) -> TextDocumentContentChangeEvent {
    if previous.is_instance(next) {
        return TextDocumentContentChangeEvent {
            range: Some(Range::default()),
            range_length: None,
            text: String::new(),
        };
    }
    let limit = previous.len_bytes().min(next.len_bytes());
    let prefix = common_bytes(previous.chunks(), next.chunks(), limit, false);
    // A mismatching UTF-8 sequence may share its first bytes.
    let mut prefix = previous.char_to_byte(previous.byte_to_char(prefix));
    if splits_crlf(previous, prefix) || splits_crlf(next, prefix) {
        prefix -= 1;
    }
    let mut suffix = common_bytes(
        previous.chunks_at_byte(previous.len_bytes()).0.reversed(),
        next.chunks_at_byte(next.len_bytes()).0.reversed(),
        limit - prefix,
        true,
    );
    while suffix > 0 {
        let old_end = previous.len_bytes() - suffix;
        let new_end = next.len_bytes() - suffix;
        if previous.char_to_byte(previous.byte_to_char(old_end)) == old_end
            && next.char_to_byte(next.byte_to_char(new_end)) == new_end
            && !splits_crlf(previous, old_end)
            && !splits_crlf(next, new_end)
        {
            break;
        }
        suffix -= 1;
    }
    let position = |byte| {
        let line = previous.byte_to_line(byte);
        let character = previous.byte_to_char(byte);
        Position::new(
            line as u32,
            (previous.char_to_utf16_cu(character)
                - previous.char_to_utf16_cu(previous.line_to_char(line))) as u32,
        )
    };
    TextDocumentContentChangeEvent {
        range: Some(Range::new(
            position(prefix),
            position(previous.len_bytes() - suffix),
        )),
        range_length: None,
        text: next
            .byte_slice(prefix..next.len_bytes() - suffix)
            .to_string(),
    }
}

fn splits_crlf(rope: &Rope, byte: usize) -> bool {
    byte > 0 && byte < rope.len_bytes() && rope.byte(byte - 1) == b'\r' && rope.byte(byte) == b'\n'
}

fn common_bytes<'a>(
    mut left: impl Iterator<Item = &'a str>,
    mut right: impl Iterator<Item = &'a str>,
    limit: usize,
    reverse: bool,
) -> usize {
    let mut a: &[u8] = &[];
    let mut b: &[u8] = &[];
    let mut count = 0;
    while count < limit {
        if a.is_empty() {
            a = left.next().unwrap_or_default().as_bytes();
        }
        if b.is_empty() {
            b = right.next().unwrap_or_default().as_bytes();
        }
        let n = a.len().min(b.len()).min(limit - count);
        if n == 0 {
            break;
        }
        let (x, y) = if reverse {
            (&a[a.len() - n..], &b[b.len() - n..])
        } else {
            (&a[..n], &b[..n])
        };
        #[cfg(test)]
        if !std::ptr::eq(x.as_ptr(), y.as_ptr()) {
            COMPARED_BYTES.with(|count| count.set(count.get() + n));
        }
        if !std::ptr::eq(x.as_ptr(), y.as_ptr()) && x != y {
            return count
                + if reverse {
                    x.iter()
                        .rev()
                        .zip(y.iter().rev())
                        .take_while(|(a, b)| a == b)
                        .count()
                } else {
                    x.iter().zip(y).take_while(|(a, b)| a == b).count()
                };
        }
        count += n;
        if reverse {
            a = &a[..a.len() - n];
            b = &b[..b.len() - n];
        } else {
            a = &a[n..];
            b = &b[n..];
        }
    }
    count
}

#[cfg(test)]
thread_local! { static COMPARED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
#[path = "lsp_transport_tests.rs"]
mod tests;
