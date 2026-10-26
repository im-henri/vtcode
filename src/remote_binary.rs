//! Range-backed binary sources. Piece tables and undo live on the client; only
//! missing pages and explicit saves cross the connection. Saves refer to source
//! ranges instead of uploading the unchanged parts of a large binary file.
use super::*;
use crate::extension_documents::{BinaryDocument, DiskStamp};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

pub(crate) const PAGE: usize = 64 * 1024;
const CACHE_PAGES: usize = 512;
const MAX_SOURCES: usize = 1024;
const MAX_PARTS: usize = 65_536;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Command {
    Open {
        path: RemotePath,
    },
    Read {
        source: u64,
        offset: u64,
        count: usize,
    },
    Save {
        base: Option<u64>,
        parts: Vec<Part>,
        destination: Option<(RemotePath, bool)>,
    },
    Close {
        source: u64,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Part {
    Source { source: u64, offset: u64, len: u64 },
    Bytes(String),
}
impl Part {
    pub(crate) fn bytes(bytes: &[u8]) -> Self {
        Self::Bytes(URL_SAFE_NO_PAD.encode(bytes))
    }
}

pub(crate) struct Plan {
    parts: Vec<Part>,
    bytes: Vec<u8>,
    transferred: usize,
}
impl Plan {
    pub(crate) fn new() -> Self {
        Self {
            parts: Vec::new(),
            bytes: Vec::new(),
            transferred: 0,
        }
    }
    fn flush(&mut self) -> Result<(), String> {
        if !self.bytes.is_empty() {
            self.parts.push(Part::bytes(&self.bytes));
            self.bytes.clear();
        }
        if self.parts.len() > MAX_PARTS {
            return Err("Binary save has too many pieces".into());
        }
        Ok(())
    }
    pub(crate) fn source(&mut self, source: u64, offset: u64, len: u64) -> Result<(), String> {
        if len == 0 {
            return Ok(());
        }
        self.flush()?;
        if let Some(Part::Source {
            source: previous,
            offset: start,
            len: size,
        }) = self.parts.last_mut()
            && *previous == source
            && start.checked_add(*size) == Some(offset)
        {
            *size = size.checked_add(len).ok_or("Binary source size overflow")?;
        } else {
            self.parts.push(Part::Source {
                source,
                offset,
                len,
            });
        }
        Ok(())
    }
    pub(crate) fn bytes(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        self.transferred = self.transferred.saturating_add(bytes.len());
        if self.transferred > MAX_PAYLOAD_BYTES / 4 * 3 {
            return Err(
                "Binary edit transfer exceeds limit; your changes remain in the local buffer"
                    .into(),
            );
        }
        while !bytes.is_empty() {
            let count = bytes.len().min(PAGE - self.bytes.len());
            self.bytes.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.bytes.len() == PAGE {
                self.flush()?;
            }
        }
        Ok(())
    }
    pub(crate) fn finish(mut self) -> Result<Vec<Part>, String> {
        self.flush()?;
        Ok(self.parts)
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Reply {
    Opened {
        source: u64,
        stamp: DiskStamp,
        read_only: bool,
    },
    Bytes(String),
    Done,
}

#[derive(Default)]
pub(crate) struct Manager {
    sources: HashMap<u64, BinaryDocument>,
    next: u64,
}
impl Manager {
    pub(super) fn close(&mut self, source: u64) {
        self.sources.remove(&source);
    }
    fn retain(&mut self, document: BinaryDocument) -> io::Result<Reply> {
        if self.sources.len() >= MAX_SOURCES {
            return Err(io::Error::other(
                "Too many retained binary sources; close unused custom editors",
            ));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Binary source ID overflow"))?;
        let reply = Reply::Opened {
            source: self.next,
            stamp: document.stamp().clone(),
            read_only: document.info().read_only,
        };
        self.sources.insert(self.next, document);
        Ok(reply)
    }
    pub(crate) fn execute(&mut self, root: &Path, command: Command) -> io::Result<Reply> {
        match command {
            Command::Open { path } => self.retain(BinaryDocument::open_io(&path.resolve(root)?)?),
            Command::Read {
                source,
                offset,
                count,
            } => {
                if count > PAGE {
                    return Err(io::Error::other("Binary read exceeds the page limit"));
                }
                let document = self
                    .sources
                    .get(&source)
                    .ok_or_else(|| io::Error::other("Binary source is closed"))?;
                let mut bytes = vec![0; count];
                let count = document
                    .read(offset, &mut bytes)
                    .map_err(io::Error::other)?;
                bytes.truncate(count);
                Ok(Reply::Bytes(URL_SAFE_NO_PAD.encode(bytes)))
            }
            Command::Save {
                base,
                parts,
                destination,
            } => {
                // Reserve the reply's source before publishing any mutation.
                if self.sources.len() >= MAX_SOURCES {
                    return Err(io::Error::other("Binary source limit reached"));
                }
                if parts.len() > MAX_PARTS {
                    return Err(io::Error::other("Binary save has too many pieces"));
                }
                let base = match base {
                    Some(base) => self
                        .sources
                        .get(&base)
                        .ok_or_else(|| io::Error::other("Binary save baseline is closed"))?
                        .clone(),
                    None if destination.is_some() => BinaryDocument::unsaved(),
                    None => {
                        return Err(io::Error::other("A new binary file requires a destination"));
                    }
                };
                let mut fragments = Vec::with_capacity(parts.len());
                let mut transferred = 0usize;
                for part in parts {
                    fragments.push(match part {
                        Part::Source {
                            source,
                            offset,
                            len,
                        } => self
                            .sources
                            .get(&source)
                            .ok_or_else(|| io::Error::other("Binary source is closed"))?
                            .fragment(offset, len)
                            .map_err(io::Error::other)?,
                        Part::Bytes(data) => {
                            transferred = transferred.saturating_add(data.len());
                            if transferred > MAX_PAYLOAD_BYTES {
                                return Err(io::Error::other("Binary edit transfer exceeds limit"));
                            }
                            BinaryDocument::bytes_fragment(
                                URL_SAFE_NO_PAD.decode(data).map_err(io::Error::other)?,
                            )
                        }
                    });
                }
                let mut document = base.with_fragments(fragments).map_err(io::Error::other)?;
                if let Some((path, replace)) = destination {
                    document
                        .save_as(&path.resolve(root)?, replace)
                        .map_err(io::Error::other)?;
                } else {
                    document.save().map_err(io::Error::other)?;
                }
                self.retain(document)
            }
            Command::Close { source } => {
                self.close(source);
                Ok(Reply::Done)
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Pages {
    entries: HashMap<(u64, u64), Arc<[u8]>>,
    order: VecDeque<(u64, u64)>,
}
impl Pages {
    fn insert(&mut self, key: (u64, u64), bytes: Arc<[u8]>) {
        if self.entries.contains_key(&key) {
            return;
        }
        while self.entries.len() >= CACHE_PAGES {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
        self.entries.insert(key, bytes);
        self.order.push_back(key);
    }
    pub(super) fn remove_source(&mut self, source: u64) {
        self.entries.retain(|(id, _), _| *id != source);
        self.order.retain(|(id, _)| *id != source);
    }
}

pub(crate) struct Source {
    pub id: u64,
    pub stamp: DiskStamp,
    pub read_only: bool,
    workspace: workspace::Workspace,
    pages: Arc<Mutex<Pages>>,
}
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteBinarySource")
            .field("id", &self.id)
            .field("stamp", &self.stamp)
            .finish()
    }
}
impl Source {
    pub(crate) fn read_cached(&self, offset: u64, out: &mut [u8]) -> bool {
        let Some(end) = offset
            .checked_add(out.len() as u64)
            .filter(|end| *end <= self.stamp.len)
        else {
            return false;
        };
        let pages = self.pages.lock().unwrap();
        let mut at = offset;
        while at < end {
            let Some(bytes) = pages.entries.get(&(self.id, at / PAGE as u64)) else {
                return false;
            };
            let start = (at % PAGE as u64) as usize;
            let count = (end - at).min((bytes.len() - start) as u64) as usize;
            let to = (at - offset) as usize;
            out[to..to + count].copy_from_slice(&bytes[start..start + count]);
            at += count as u64;
        }
        true
    }
    pub(crate) fn seed(&self, page: u64, bytes: Vec<u8>) {
        self.pages
            .lock()
            .unwrap()
            .insert((self.id, page), bytes.into());
    }
    fn opened(workspace: &workspace::Workspace, reply: super::Reply) -> Result<Arc<Self>, String> {
        let super::Reply::Binary(Reply::Opened {
            source,
            stamp,
            read_only,
        }) = reply
        else {
            return Err("Invalid binary source reply".into());
        };
        Ok(Arc::new(Self {
            id: source,
            stamp,
            read_only,
            workspace: workspace.clone(),
            pages: workspace.client.binary_pages(),
        }))
    }
    pub(crate) fn open(workspace: &workspace::Workspace, path: &Path) -> io::Result<Arc<Self>> {
        let path = workspace.remote_path(path)?;
        Self::opened(
            workspace,
            workspace
                .client
                .call(super::Command::Binary(Command::Open { path }))?,
        )
        .map_err(io::Error::other)
    }
    pub(crate) fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), String> {
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|end| *end <= self.stamp.len)
            .ok_or("Binary read is past EOF")?;
        let mut at = offset;
        while at < end {
            let page = at / PAGE as u64;
            let key = (self.id, page);
            let cached = self.pages.lock().unwrap().entries.get(&key).cloned();
            let bytes = match cached {
                Some(bytes) => bytes,
                None => {
                    let reply = self
                        .workspace
                        .client
                        .call(super::Command::Binary(Command::Read {
                            source: self.id,
                            offset: page * PAGE as u64,
                            count: PAGE,
                        }))
                        .map_err(|e| e.to_string())?;
                    let super::Reply::Binary(Reply::Bytes(data)) = reply else {
                        return Err("Invalid binary page reply".into());
                    };
                    let data = URL_SAFE_NO_PAD.decode(data).map_err(|e| e.to_string())?;
                    let expected = (self.stamp.len - page * PAGE as u64).min(PAGE as u64) as usize;
                    if data.len() != expected {
                        return Err("Remote binary page was incomplete".into());
                    }
                    let bytes: Arc<[u8]> = data.into();
                    self.pages.lock().unwrap().insert(key, bytes.clone());
                    bytes
                }
            };
            let start = (at % PAGE as u64) as usize;
            let count = (end - at).min((bytes.len() - start) as u64) as usize;
            let to = (at - offset) as usize;
            out[to..to + count].copy_from_slice(&bytes[start..start + count]);
            at += count as u64;
        }
        Ok(())
    }
    pub(crate) fn save(
        workspace: &workspace::Workspace,
        base: Option<u64>,
        parts: Vec<Part>,
        destination: Option<(&Path, bool)>,
    ) -> Result<Arc<Self>, String> {
        if parts.len() > MAX_PARTS {
            return Err("Binary save has too many pieces".into());
        }
        let destination = destination
            .map(|(path, replace)| workspace.remote_path(path).map(|path| (path, replace)))
            .transpose()
            .map_err(|e| e.to_string())?;
        Self::opened(
            workspace,
            workspace
                .client
                .call(super::Command::Binary(Command::Save {
                    base,
                    parts,
                    destination,
                }))
                .map_err(|e| e.to_string())?,
        )
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.pages.lock().unwrap().remove_source(self.id);
        self.workspace.client.close_binary(self.id);
    }
}
