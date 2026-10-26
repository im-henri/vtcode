//! Frozen node-side prompt context. References never escape to an ACP agent.
use super::*;
use std::{collections::HashMap, sync::Arc};
use vtcode::acp::{self, protocol as p};

const PREFIX: &str = "vtcode-context:";

pub(in crate::remote) struct Snapshot {
    pub root: PathBuf,
    pub files: Vec<(RemotePath, String)>,
}

#[derive(Default)]
pub(in crate::remote) struct Snapshots {
    values: HashMap<u64, Arc<Snapshot>>,
    bytes: usize,
}
impl Snapshots {
    pub fn insert(&mut self, id: u64, snapshot: Snapshot) -> io::Result<()> {
        let bytes = snapshot
            .files
            .iter()
            .map(|(_, text)| text.len())
            .sum::<usize>();
        if self.values.len() >= 32 || bytes > TOTAL_LIMIT || self.bytes + bytes > 32 * TOTAL_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Pending file context is busy; try again.",
            ));
        }
        self.bytes += bytes;
        self.values.insert(id, Arc::new(snapshot));
        Ok(())
    }
    pub fn get(&self, id: u64) -> io::Result<Arc<Snapshot>> {
        self.values
            .get(&id)
            .cloned()
            .ok_or_else(|| io::Error::other("Prepared file context is closed"))
    }
    pub fn close(&mut self, id: u64) {
        if let Some(snapshot) = self.values.remove(&id) {
            self.bytes -= snapshot
                .files
                .iter()
                .map(|(_, text)| text.len())
                .sum::<usize>();
        }
    }
}

pub(crate) struct Source {
    pub(in crate::remote) client: client::Client,
    pub(in crate::remote) id: u64,
}
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedContext")
            .field("id", &self.id)
            .finish()
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.client.release_context(self.id);
    }
}
impl Source {
    pub(crate) fn reference(&self, index: usize, embedded: bool, label: String) -> p::ContentBlock {
        p::ContentBlock::ResourceLink(p::ResourceLink::new(
            label,
            format!("{PREFIX}{}:{index}:{}", self.id, u8::from(embedded)),
        ))
    }
}

fn reference(block: &p::ContentBlock) -> io::Result<Option<(u64, usize, bool)>> {
    let p::ContentBlock::ResourceLink(link) = block else {
        return Ok(None);
    };
    let Some(value) = link.uri.strip_prefix(PREFIX) else {
        return Ok(None);
    };
    let mut fields = value.split(':');
    let parse = || io::Error::other("Invalid prepared file context reference");
    let id = fields
        .next()
        .ok_or_else(parse)?
        .parse::<u64>()
        .map_err(|_| parse())?;
    let index = fields
        .next()
        .ok_or_else(parse)?
        .parse::<usize>()
        .map_err(|_| parse())?;
    let embedded = match fields.next() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err(parse()),
    };
    if id == 0 || index >= FILE_COUNT || fields.next().is_some() {
        return Err(parse());
    }
    Ok(Some((id, index, embedded)))
}

/// The UI only upgrades weak references. A queued command owns its snapshots
/// through upload, native queue backpressure and the node's acknowledgement.
pub(crate) fn retain(
    client: &client::Client,
    command: &acp::Command,
) -> io::Result<Vec<Arc<Source>>> {
    let acp::Command::Prompt { content, .. } = command else {
        return Ok(Vec::new());
    };
    let mut sources = Vec::new();
    for block in content {
        if let Some((id, _, _)) = reference(block)?
            && !sources.iter().any(|source: &Arc<Source>| source.id == id)
        {
            sources.push(client.retain_context(id)?);
        }
    }
    Ok(sources)
}

/// Resolve frozen memory before locking any agent/file state. No disk reads or
/// requests back to the workbench occur while accepting the prepared prompt.
pub(in crate::remote) fn materialize(
    root: &Path,
    store: &std::sync::Mutex<text::Store>,
    command: &mut acp::Command,
) -> io::Result<()> {
    let acp::Command::Prompt { content, .. } = command else {
        return Ok(());
    };
    let mut snapshots = HashMap::new();
    let mut count = 0;
    let mut bytes = 0;
    for block in content {
        let Some((id, index, embedded)) = reference(block)? else {
            continue;
        };
        let snapshot = match snapshots.entry(id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(store.lock().unwrap().context_snapshot(id)?)
            }
        };
        if snapshot.root != root {
            return Err(io::Error::other(
                "Prepared file context belongs to another workspace",
            ));
        }
        let (path, text) = snapshot
            .files
            .get(index)
            .ok_or_else(|| io::Error::other("Invalid prepared file context index"))?;
        count += 1;
        bytes += text.len();
        if count > FILE_COUNT || bytes > TOTAL_LIMIT {
            return Err(io::Error::other(
                "Prepared file context exceeds the message budget",
            ));
        }
        let native = path.resolve(root)?;
        *block = if embedded {
            let uri = url::Url::from_file_path(native)
                .map_err(|_| io::Error::other("Invalid attached file URI"))?;
            p::ContentBlock::Resource(p::EmbeddedResource::new(
                p::EmbeddedResourceResource::TextResourceContents(
                    p::TextResourceContents::new(text.clone(), uri.to_string())
                        .mime_type("text/plain"),
                ),
            ))
        } else {
            let context = serde_json::json!({"path": native.to_string_lossy(), "text": text});
            p::ContentBlock::Text(p::TextContent::new(format!(
                "Attached file context (current editor contents when open):\n{context}"
            )))
        };
    }
    Ok(())
}
