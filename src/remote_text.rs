//! Text transfers have bounded messages, not a maximum file size. Rope snapshots
//! remain off the UI thread and a small pipeline hides per-request link latency.
use super::*;
use ropey::{Rope, RopeBuilder};
use std::{collections::HashMap, ops::Range};

pub(crate) const CHUNK_CHARS: usize = 128 * 1024;
pub(crate) const INLINE_BYTES: usize = 512 * 1024;

/// Structured worker results share the same bounded transport as text files.
/// Serializing directly into a Rope avoids a second result-sized byte buffer.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Data {
    Inline(String),
    Source {
        transfer: u64,
        characters: usize,
        bytes: usize,
    },
}
impl Data {
    pub(super) fn transfer(&self) -> Option<u64> {
        match self {
            Self::Source { transfer, .. } => Some(*transfer),
            _ => None,
        }
    }
    pub(super) fn encode(
        store: &std::sync::Mutex<Store>,
        value: &impl Serialize,
    ) -> io::Result<Self> {
        Self::from_rope(store, Self::encode_rope(value)?)
    }
    pub(super) fn encode_rope(value: &impl Serialize) -> io::Result<Rope> {
        struct Writer {
            builder: RopeBuilder,
            tail: Vec<u8>,
        }
        impl io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len();
                if self.tail.is_empty() {
                    match std::str::from_utf8(bytes) {
                        Ok(text) => self.builder.append(text),
                        Err(error) if error.error_len().is_none() => {
                            self.builder.append(
                                std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap(),
                            );
                            self.tail.extend_from_slice(&bytes[error.valid_up_to()..]);
                        }
                        Err(error) => return Err(io::Error::other(error)),
                    }
                } else {
                    // Only a split UTF-8 scalar is retained between writes.
                    let needed = (4 - self.tail.len()).min(bytes.len());
                    for (index, byte) in bytes[..needed].iter().enumerate() {
                        self.tail.push(*byte);
                        match std::str::from_utf8(&self.tail) {
                            Ok(text) => {
                                self.builder.append(text);
                                self.tail.clear();
                                self.write_all(&bytes[index + 1..])?;
                                return Ok(count);
                            }
                            Err(error) if error.error_len().is_none() => {}
                            Err(error) => return Err(io::Error::other(error)),
                        }
                    }
                }
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer {
            builder: RopeBuilder::new(),
            tail: Vec::new(),
        };
        serde_json::to_writer(&mut writer, value)?;
        if !writer.tail.is_empty() {
            return Err(io::Error::other("Incomplete structured UTF-8 result"));
        }
        Ok(writer.builder.finish())
    }
    pub(super) fn from_rope(store: &std::sync::Mutex<Store>, rope: Rope) -> io::Result<Self> {
        Ok(if rope.len_bytes() <= INLINE_BYTES {
            Self::Inline(rope.to_string())
        } else {
            let characters = rope.len_chars();
            let bytes = rope.len_bytes();
            let transfer = store.lock().unwrap().read(rope)?;
            Self::Source {
                transfer,
                characters,
                bytes,
            }
        })
    }
    pub(super) fn decode<T: serde::de::DeserializeOwned>(
        self,
        workspace: &workspace::Workspace,
    ) -> io::Result<T> {
        self.decode_cancellable(workspace, None)
    }
    pub(super) fn decode_cancellable<T: serde::de::DeserializeOwned>(
        self,
        workspace: &workspace::Workspace,
        cancel: Option<&crate::background::CancellationToken>,
    ) -> io::Result<T> {
        match self {
            Self::Inline(text) => serde_json::from_str(&text).map_err(io::Error::other),
            Self::Source {
                transfer,
                characters,
                bytes,
            } => {
                let rope =
                    workspace.receive_text(transfer, characters, bytes, cancel, |_, _| {})?;
                Self::decode_rope(&rope)
            }
        }
    }
    pub(super) fn decode_rope<T: serde::de::DeserializeOwned>(rope: &Rope) -> io::Result<T> {
        struct Reader<'a> {
            chunks: ropey::iter::Chunks<'a>,
            current: &'a [u8],
        }
        impl io::Read for Reader<'_> {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if output.is_empty() {
                    return Ok(0);
                }
                while self.current.is_empty() {
                    let Some(chunk) = self.chunks.next() else {
                        return Ok(0);
                    };
                    self.current = chunk.as_bytes();
                }
                let count = output.len().min(self.current.len());
                output[..count].copy_from_slice(&self.current[..count]);
                self.current = &self.current[count..];
                Ok(count)
            }
        }
        serde_json::from_reader(std::io::BufReader::with_capacity(
            32 * 1024,
            Reader {
                chunks: rope.chunks(),
                current: &[],
            },
        ))
        .map_err(io::Error::other)
    }
}

pub(crate) fn progress(
    sender: crate::background::BackgroundResultSender,
    generation: u64,
    path: PathBuf,
    saving: bool,
) -> impl FnMut(u64, u64) {
    throttled_progress(move |completed, total| {
        let _ = sender.send(
            crate::background::BackgroundResult::RemoteTransferProgress {
                generation,
                path: path.clone(),
                saving,
                completed,
                total,
            },
        );
    })
}

pub(crate) fn throttled_progress(mut emit: impl FnMut(u64, u64)) -> impl FnMut(u64, u64) {
    let mut last = None::<std::time::Instant>;
    move |completed, total| {
        let now = std::time::Instant::now();
        if completed == 0
            || completed == total
            || last
                .is_none_or(|last| now.duration_since(last) >= std::time::Duration::from_millis(80))
        {
            last = Some(now);
            emit(completed, total);
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Command {
    Read {
        transfer: u64,
        start: usize,
        end: usize,
    },
    BeginSave {
        path: RemotePath,
        base: Option<Revision>,
        start: usize,
        end: usize,
        characters: usize,
        bytes: usize,
        encoding: String,
    },
    BeginBlob {
        characters: usize,
        bytes: usize,
    },
    FinishBlob {
        transfer: u64,
    },
    Append {
        transfer: u64,
        offset: usize,
        text: String,
    },
    Finish {
        transfer: u64,
    },
    Close {
        transfer: u64,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Reply {
    Opened {
        transfer: u64,
        characters: usize,
        bytes: usize,
        encoding: String,
        revision: Revision,
        identity: RemotePath,
        size: u64,
        modified: Option<std::time::SystemTime>,
    },
    Chunk(String),
    Upload {
        transfer: u64,
    },
    Source {
        transfer: u64,
        characters: usize,
        bytes: usize,
    },
}

pub(super) struct Save {
    pub root: PathBuf,
    pub path: RemotePath,
    pub source: Option<service::Document>,
    pub range: Range<usize>,
    pub encoding: &'static str,
}
enum Transfer {
    Read(Rope),
    Upload {
        builder: RopeBuilder,
        characters: usize,
        bytes: usize,
        expected_chars: usize,
        expected_bytes: usize,
        save: Option<Save>,
    },
}
#[derive(Default)]
pub(super) struct Store {
    next: u64,
    transfers: HashMap<u64, Transfer>,
    contexts: context::Snapshots,
}
impl Store {
    pub(super) fn context(&mut self, snapshot: context::Snapshot) -> io::Result<u64> {
        let id = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Context ID overflow"))?;
        self.contexts.insert(id, snapshot)?;
        self.next = id;
        Ok(id)
    }
    pub(super) fn context_snapshot(
        &self,
        id: u64,
    ) -> io::Result<std::sync::Arc<context::Snapshot>> {
        self.contexts.get(id)
    }
    fn put(&mut self, transfer: Transfer) -> io::Result<u64> {
        if self.transfers.len() >= 8 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Remote file transfer slots are busy",
            ));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Text transfer ID overflow"))?;
        self.transfers.insert(self.next, transfer);
        Ok(self.next)
    }
    pub(super) fn read(&mut self, rope: Rope) -> io::Result<u64> {
        self.put(Transfer::Read(rope))
    }
    pub(super) fn begin(
        &mut self,
        save: Option<Save>,
        characters: usize,
        bytes: usize,
    ) -> io::Result<u64> {
        if characters > bytes || bytes > characters.saturating_mul(4) {
            return Err(io::Error::other("Invalid text transfer length"));
        }
        self.put(Transfer::Upload {
            builder: RopeBuilder::new(),
            characters: 0,
            bytes: 0,
            expected_chars: characters,
            expected_bytes: bytes,
            save,
        })
    }
    pub(super) fn chunk(&self, transfer: u64, range: Range<usize>) -> io::Result<String> {
        let Some(Transfer::Read(rope)) = self.transfers.get(&transfer) else {
            return Err(io::Error::other("Text source is closed"));
        };
        if range.start > range.end || range.end > rope.len_chars() || range.len() > CHUNK_CHARS {
            return Err(io::Error::other("Invalid text chunk range"));
        }
        Ok(rope.slice(range).to_string())
    }
    pub(super) fn append(&mut self, transfer: u64, offset: usize, text: &str) -> io::Result<()> {
        let Some(Transfer::Upload {
            builder,
            characters,
            bytes,
            expected_chars,
            expected_bytes,
            ..
        }) = self.transfers.get_mut(&transfer)
        else {
            return Err(io::Error::other("Text upload is closed"));
        };
        let chars = text.chars().count();
        if offset != *characters
            || chars > CHUNK_CHARS
            || text.len() > expected_bytes.saturating_sub(*bytes)
            || chars > expected_chars.saturating_sub(*characters)
        {
            return Err(io::Error::other("Invalid or out-of-order text upload"));
        }
        builder.append(text);
        *bytes += text.len();
        *characters += chars;
        Ok(())
    }
    pub(super) fn finish(&mut self, transfer: u64) -> io::Result<(Option<Save>, Rope)> {
        let Some(Transfer::Upload {
            builder,
            characters,
            bytes,
            expected_chars,
            expected_bytes,
            save,
        }) = self.transfers.remove(&transfer)
        else {
            return Err(io::Error::other("Text upload is closed"));
        };
        if characters != expected_chars || bytes != expected_bytes {
            return Err(io::Error::other(
                "Text upload is incomplete; destination was not changed",
            ));
        }
        Ok((save, builder.finish()))
    }
    pub(super) fn finish_blob(&mut self, transfer: u64) -> io::Result<Reply> {
        let (save, rope) = self.finish(transfer)?;
        if save.is_some() {
            return Err(io::Error::other("Save transfer cannot become a tool input"));
        }
        let reply = Reply::Source {
            transfer,
            characters: rope.len_chars(),
            bytes: rope.len_bytes(),
        };
        self.transfers.insert(transfer, Transfer::Read(rope));
        Ok(reply)
    }
    pub(super) fn snapshot(&self, transfer: u64) -> io::Result<Rope> {
        let Some(Transfer::Read(rope)) = self.transfers.get(&transfer) else {
            return Err(io::Error::other("Text source is closed"));
        };
        Ok(rope.clone())
    }
    pub(super) fn close(&mut self, transfer: u64) {
        self.transfers.remove(&transfer);
        self.contexts.close(transfer);
    }
}

struct Close<'a> {
    client: &'a client::Client,
    transfer: u64,
}
impl Drop for Close<'_> {
    fn drop(&mut self) {
        self.client.close_text(self.transfer);
    }
}
pub(super) struct Blob {
    pub transfer: u64,
    client: client::Client,
    _permit: client::TextPermit,
}
impl Drop for Blob {
    fn drop(&mut self) {
        self.client.close_text(self.transfer);
    }
}
impl workspace::Workspace {
    pub(crate) fn format_document(
        &self,
        path: &Path,
        config: crate::formatters::Config,
        source: &Rope,
        lines: Option<(usize, usize)>,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<String> {
        let path = self.remote_path(path)?;
        let operation = self.client.file_operation("Format document", &path);
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                let blob = if source.len_bytes() > INLINE_BYTES {
                    Some(self.send_blob(source, cancel)?)
                } else {
                    None
                };
                let command = match &blob {
                    Some(blob) => super::Command::FormatSource {
                        transfer: blob.transfer,
                        path,
                        config,
                        lines,
                    },
                    None => super::Command::Format {
                        path,
                        config,
                        text: source.to_string(),
                        lines,
                    },
                };
                match self.client.call_cancellable(command, cancel)? {
                    super::Reply::Formatted(text) => Ok(text),
                    super::Reply::Text(Reply::Source {
                        transfer,
                        characters,
                        bytes,
                    }) => self
                        .receive_text(transfer, characters, bytes, Some(cancel), |_, _| {})
                        .map(|rope| rope.to_string()),
                    _ => Err(io::Error::other("Invalid remote formatter reply")),
                }
            },
            |text| format!("{} result bytes", text.len()),
        )
    }
    pub(super) fn send_blob(
        &self,
        rope: &Rope,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<Blob> {
        let permit = self.client.text_permit_cancellable(Some(cancel))?;
        let reply = self.client.call_cancellable(
            super::Command::Text(Command::BeginBlob {
                characters: rope.len_chars(),
                bytes: rope.len_bytes(),
            }),
            cancel,
        )?;
        let super::Reply::Text(Reply::Upload { transfer }) = reply else {
            return Err(io::Error::other("Invalid tool upload reply"));
        };
        let operation =
            self.client
                .transfer_operation("Upload tool input", transfer, rope.len_bytes());
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                if let Some(operation) = &operation {
                    operation.progress(0, rope.len_bytes() as u64);
                }
                let guard = Blob {
                    transfer,
                    client: self.client.clone(),
                    _permit: permit,
                };
                let commands = (0..rope.len_chars()).step_by(CHUNK_CHARS).map(|offset| {
                    super::Command::Text(Command::Append {
                        transfer,
                        offset,
                        text: rope
                            .slice(offset..offset.saturating_add(CHUNK_CHARS).min(rope.len_chars()))
                            .to_string(),
                    })
                });
                let mut acknowledged_chars = 0;
                self.client
                    .pipeline_cancellable(commands, Some(cancel), |reply| {
                        if matches!(reply, super::Reply::Done) {
                            acknowledged_chars =
                                (acknowledged_chars + CHUNK_CHARS).min(rope.len_chars());
                            if let Some(operation) = &operation {
                                operation.progress(
                                    rope.char_to_byte(acknowledged_chars) as u64,
                                    rope.len_bytes() as u64,
                                );
                            }
                            Ok(())
                        } else {
                            Err(io::Error::other("Invalid tool upload acknowledgement"))
                        }
                    })?;
                let completed = self.client.call_cancellable(
                    super::Command::Text(Command::FinishBlob { transfer }),
                    cancel,
                )?;
                if !matches!(completed, super::Reply::Text(Reply::Source { transfer: id, characters, bytes })
            if id == transfer && characters == rope.len_chars() && bytes == rope.len_bytes())
                {
                    return Err(io::Error::other("Invalid completed tool upload"));
                }
                Ok(guard)
            },
            |_| format!("{} bytes uploaded", rope.len_bytes()),
        )
    }
    pub(super) fn receive_text(
        &self,
        transfer: u64,
        characters: usize,
        bytes: usize,
        cancel: Option<&crate::background::CancellationToken>,
        mut progress: impl FnMut(u64, u64),
    ) -> io::Result<Rope> {
        let operation = self
            .client
            .transfer_operation("Download text", transfer, bytes);
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                if let Some(operation) = &operation {
                    operation.progress(0, bytes as u64);
                }
                let _close = Close {
                    client: &self.client,
                    transfer,
                };
                let mut builder = RopeBuilder::new();
                let mut completed = 0;
                let commands = (0..characters).step_by(CHUNK_CHARS).map(|start| {
                    super::Command::Text(Command::Read {
                        transfer,
                        start,
                        end: start.saturating_add(CHUNK_CHARS).min(characters),
                    })
                });
                self.client
                    .pipeline_cancellable(commands, cancel, |reply| {
                        let super::Reply::Text(Reply::Chunk(text)) = reply else {
                            return Err(io::Error::other("Invalid text chunk reply"));
                        };
                        builder.append(&text);
                        completed += text.len() as u64;
                        if let Some(operation) = &operation {
                            operation.progress(completed, bytes as u64);
                        }
                        progress(completed, bytes as u64);
                        Ok(())
                    })?;
                let rope = builder.finish();
                if rope.len_bytes() != bytes || rope.len_chars() != characters {
                    return Err(io::Error::other("Remote file transfer was incomplete"));
                }
                Ok(rope)
            },
            |rope| format!("{} bytes received", rope.len_bytes()),
        )
    }
    pub(super) fn send_text_save(
        &self,
        path: RemotePath,
        base: Option<Revision>,
        old_range: Range<usize>,
        rope: &Rope,
        new_range: Range<usize>,
        encoding: &str,
        mut progress: impl FnMut(u64, u64),
    ) -> io::Result<super::Reply> {
        let _permit = self.client.text_permit()?;
        let inserted = rope.slice(new_range.clone());
        let characters = inserted.len_chars();
        let bytes = inserted.len_bytes();
        let reply = self
            .client
            .call_file(super::Command::Text(Command::BeginSave {
                path,
                base,
                start: old_range.start,
                end: old_range.end,
                characters,
                bytes,
                encoding: encoding.into(),
            }))?;
        let super::Reply::Text(Reply::Upload { transfer }) = reply else {
            return Err(io::Error::other("Invalid text upload reply"));
        };
        let operation = self
            .client
            .transfer_operation("Upload file changes", transfer, bytes);
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                if let Some(operation) = &operation {
                    operation.progress(0, bytes as u64);
                }
                let _close = Close {
                    client: &self.client,
                    transfer,
                };
                let commands = (0..characters).step_by(CHUNK_CHARS).map(|offset| {
                    super::Command::Text(Command::Append {
                        transfer,
                        offset,
                        text: inserted
                            .slice(offset..offset.saturating_add(CHUNK_CHARS).min(characters))
                            .to_string(),
                    })
                });
                let mut acknowledged_chars = 0usize;
                self.client.pipeline(commands, |reply| {
                    if !matches!(reply, super::Reply::Done) {
                        return Err(io::Error::other("Invalid text upload acknowledgement"));
                    }
                    acknowledged_chars = acknowledged_chars
                        .saturating_add(CHUNK_CHARS)
                        .min(characters);
                    if let Some(operation) = &operation {
                        operation.progress(
                            inserted.char_to_byte(acknowledged_chars) as u64,
                            bytes as u64,
                        );
                    }
                    progress(
                        inserted.char_to_byte(acknowledged_chars) as u64,
                        bytes as u64,
                    );
                    Ok(())
                })?;
                if let Some(operation) = &operation {
                    operation.stage("Commit file / await save");
                }
                self.client
                    .call_file(super::Command::Text(Command::Finish { transfer }))
            },
            |_| format!("{bytes} change bytes uploaded and saved"),
        )
    }
}
