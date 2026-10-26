//! Cross-platform ACP locations are explicit byte paths. Native PathBuf serde
//! never gets to reinterpret a POSIX filename using Windows path semantics.
use super::*;
use std::collections::HashMap;
use vtcode::acp::{
    self as acp,
    locations::{self, Location},
};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Slot {
    index: usize,
    path: RemotePath,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Packet<T> {
    pub value: T,
    slots: Vec<Slot>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Event {
    pub packet: Packet<acp::Event>,
    pub history: Option<u64>,
    patch: Option<Patch>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Patch {
    old_bytes: usize,
    start: usize,
    end: usize,
    text: String,
}

fn node_path(root: &Path, path: &Path) -> io::Result<RemotePath> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    git::from_native(&normalized, root)
}
fn uri_path(value: &str) -> Option<io::Result<PathBuf>> {
    let uri = url::Url::parse(value).ok()?;
    if uri.scheme() != "file" {
        return None;
    }
    if uri
        .host_str()
        .is_some_and(|host| !host.is_empty() && host != "localhost")
    {
        return Some(Err(io::Error::other(
            "A foreign file authority is not a workspace path",
        )));
    }
    Some(
        uri.to_file_path()
            .map_err(|_| io::Error::other("Unsupported agent file URI")),
    )
}
fn pack<T>(
    mut value: T,
    mut visit: impl FnMut(&mut T, &mut dyn FnMut(Location<'_>) -> io::Result<()>) -> io::Result<()>,
    mut map: impl FnMut(&Path) -> io::Result<RemotePath>,
) -> io::Result<Packet<T>> {
    let mut slots = Vec::new();
    let mut index = 0;
    visit(&mut value, &mut |location| {
        let path = match location {
            Location::Path(path) => Some((path.clone(), Some(path), None)),
            Location::Uri(uri) => match uri_path(uri) {
                Some(Ok(path)) => Some((path, None, Some(uri))),
                Some(Err(_)) => {
                    // Keep the resource visible without interpreting another
                    // host's file URI as a native Windows UNC path. An
                    // unsupported resource must not disconnect the agent.
                    *uri = format!(
                        "vtcode-remote-unsupported:{}",
                        URL_SAFE_NO_PAD.encode(uri.as_bytes())
                    );
                    None
                }
                None => None,
            },
        };
        if let Some((path, field, uri)) = path {
            slots.push(Slot {
                index,
                path: map(&path)?,
            });
            if let Some(field) = field {
                field.clear();
            }
            if let Some(uri) = uri {
                uri.clear();
            }
        }
        index += 1;
        Ok(())
    })?;
    Ok(Packet { value, slots })
}
fn unpack<T>(
    mut packet: Packet<T>,
    mut visit: impl FnMut(&mut T, &mut dyn FnMut(Location<'_>) -> io::Result<()>) -> io::Result<()>,
    mut map: impl FnMut(&RemotePath) -> io::Result<PathBuf>,
) -> io::Result<T> {
    let mut slots = packet.slots.into_iter().peekable();
    let mut index = 0;
    visit(&mut packet.value, &mut |location| {
        if slots.peek().is_some_and(|slot| slot.index == index) {
            let slot = slots.next().unwrap();
            let path = map(&slot.path)?;
            match location {
                Location::Path(field) => *field = path,
                Location::Uri(uri) => {
                    *uri = url::Url::from_file_path(path)
                        .map_err(|_| io::Error::other("Invalid hosted agent URI"))?
                        .into()
                }
            }
        } else if matches!(location, Location::Path(_)) {
            return Err(io::Error::other("Missing hosted agent location"));
        }
        index += 1;
        Ok(())
    })?;
    if slots.next().is_some() {
        return Err(io::Error::other("Unexpected hosted agent locations"));
    }
    Ok(packet.value)
}
pub(super) fn command(
    workspace: &workspace::Workspace,
    command: acp::Command,
) -> io::Result<Packet<acp::Command>> {
    pack(
        command,
        |command, map| locations::command(command, &mut |location| map(location)),
        |path| workspace.remote_path(path),
    )
}
pub(super) fn node_command(root: &Path, packet: Packet<acp::Command>) -> io::Result<acp::Command> {
    unpack(
        packet,
        |command, map| locations::command(command, &mut |location| map(location)),
        |path| path.resolve(root),
    )
}

impl Patch {
    fn between(old: &str, new: &str) -> Self {
        let mut start = old
            .bytes()
            .zip(new.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
            start -= 1;
        }
        let mut suffix = old.as_bytes()[start..]
            .iter()
            .rev()
            .zip(new.as_bytes()[start..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix)
        {
            suffix -= 1;
        }
        Self {
            old_bytes: old.len(),
            start,
            end: old.len() - suffix,
            text: new[start..new.len() - suffix].into(),
        }
    }
    fn apply(self, old: &mut String) -> io::Result<()> {
        if self.old_bytes != old.len()
            || self.start > self.end
            || !old.is_char_boundary(self.start)
            || !old.is_char_boundary(self.end)
        {
            return Err(io::Error::other(
                "Agent terminal update does not match its previous snapshot",
            ));
        }
        old.replace_range(self.start..self.end, &self.text);
        Ok(())
    }
}
pub(super) fn event(
    root: &Path,
    mut event: acp::Event,
    history: Option<u64>,
    outputs: &mut HashMap<String, String>,
) -> io::Result<Event> {
    let patch = if let acp::Event::Terminal(terminal) = &mut event {
        let output = std::mem::take(&mut terminal.output);
        let patch = Patch::between(
            outputs.get(&terminal.id).map_or("", String::as_str),
            &output,
        );
        if terminal.released {
            outputs.remove(&terminal.id);
        } else {
            outputs.insert(terminal.id.clone(), output);
        }
        Some(patch)
    } else {
        None
    };
    let packet = pack(
        event,
        |event, map| locations::event(event, &mut |location| map(location)),
        |path| node_path(root, path),
    )?;
    Ok(Event {
        packet,
        history,
        patch,
    })
}
pub(super) fn local_event(
    workspace: &workspace::Workspace,
    wire: Event,
    outputs: &mut HashMap<String, String>,
) -> io::Result<(acp::Event, Option<u64>)> {
    let mut event = unpack(
        wire.packet,
        |event, map| locations::event(event, &mut |location| map(location)),
        |path| Ok(workspace.view_key(path)),
    )?;
    if let acp::Event::Terminal(terminal) = &mut event {
        let old = outputs.entry(terminal.id.clone()).or_default();
        wire.patch
            .ok_or_else(|| io::Error::other("Missing agent terminal update"))?
            .apply(old)?;
        terminal.output = old.clone();
        if terminal.released {
            outputs.remove(&terminal.id);
        }
    } else if wire.patch.is_some() || wire.history.is_some() {
        return Err(io::Error::other("Unexpected terminal metadata"));
    }
    Ok((event, wire.history))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_file_resources_are_disabled_without_breaking_the_event_stream() {
        let update = serde_json::from_value(serde_json::json!({"sessionId":"s", "update":{"sessionUpdate":"agent_message_chunk", "content":{"type":"resource_link", "uri":"file://other-machine/share/secret", "name":"Foreign resource"}}})).unwrap();
        let packet = pack(
            acp::Event::Update(update),
            |event, map| locations::event(event, &mut |location| map(location)),
            |_| panic!("Foreign resources are not native paths"),
        )
        .unwrap();
        let value = serde_json::to_value(packet.value).unwrap();
        assert!(
            value["Update"]["update"]["content"]["uri"]
                .as_str()
                .unwrap()
                .starts_with("vtcode-remote-unsupported:")
        );
    }
    #[test]
    fn hosted_file_resource_uris_roundtrip_without_rewriting_source_text() {
        let root = std::env::temp_dir().join("node with spaces λ");
        let key = PathBuf::from(if cfg!(windows) { "C:/" } else { "/" })
            .join(format!("{}vtcode-remote-fixture", char::from(0u8)))
            .join("project/doc.txt");
        let uri = url::Url::from_file_path(&key).unwrap().to_string();
        let resource: acp::protocol::ContentBlock = serde_json::from_value(
            serde_json::json!({"type":"resource_link", "uri":uri, "name":"File"}),
        )
        .unwrap();
        let command = acp::Command::Prompt {
            session_id: acp::protocol::SessionId::new("s"),
            content: vec![
                resource,
                acp::protocol::ContentBlock::Text(acp::protocol::TextContent::new(
                    "file:///literal/source",
                )),
            ],
        };
        let packet = pack(
            command,
            |command, map| locations::command(command, &mut |location| map(location)),
            |path| {
                assert_eq!(path, key);
                RemotePath::new(b"doc.txt".to_vec())
            },
        )
        .unwrap();
        let command = node_command(&root, packet).unwrap();
        let acp::Command::Prompt { content, .. } = command else {
            unreachable!()
        };
        let value = serde_json::to_value(&content[0]).unwrap();
        assert_eq!(
            url::Url::parse(value["uri"].as_str().unwrap())
                .unwrap()
                .to_file_path()
                .unwrap(),
            root.join("doc.txt")
        );
        assert_eq!(acp::content_text(&content[1]), "file:///literal/source");
    }
    #[test]
    fn terminal_patches_preserve_unicode_and_rolling_truncation() {
        let values = [
            "",
            "λ",
            "🙂λ\r\n",
            "🙂prefix λ\r\n",
            "prefix λ\r\nnew",
            "new",
            "",
            "é",
            "ê",
        ];
        for old in values {
            for new in values {
                let patch = Patch::between(old, new);
                let mut result = old.to_owned();
                patch.apply(&mut result).unwrap();
                assert_eq!(result, new);
            }
        }
        let mut old = "λ".to_owned();
        assert!(
            Patch {
                old_bytes: 2,
                start: 1,
                end: 2,
                text: String::new()
            }
            .apply(&mut old)
            .is_err()
        );
        assert_eq!(old, "λ");
        assert!(
            Patch {
                old_bytes: 3,
                start: 0,
                end: 2,
                text: String::new()
            }
            .apply(&mut old)
            .is_err()
        );
    }
    #[test]
    fn only_typed_locations_are_rewritten_and_malformed_slots_are_rejected() {
        let path = RemotePath::new(b"dir/colon:back\\slash-\xff".to_vec()).unwrap();
        let request = acp::FileRequest::Write(acp::protocol::WriteTextFileRequest::new(
            acp::protocol::SessionId::new("session"),
            PathBuf::from("original"),
            "file:///literal/source λ",
        ));
        let packet = pack(
            request,
            |request, map| locations::file_request(request, &mut |location| map(location)),
            |_| Ok(path.clone()),
        )
        .unwrap();
        let json = serde_json::to_string(&packet).unwrap();
        assert!(!json.contains("original"));
        let packet: Packet<acp::FileRequest> = serde_json::from_str(&json).unwrap();
        let value = unpack(
            packet,
            |request, map| locations::file_request(request, &mut |location| map(location)),
            |value| {
                assert_eq!(*value, path);
                Ok(PathBuf::from("mapped"))
            },
        )
        .unwrap();
        assert_eq!(value.path(), Path::new("mapped"));
        assert!(
            matches!(value, acp::FileRequest::Write(request) if request.content == "file:///literal/source λ")
        );
        let mut packet: Packet<acp::FileRequest> = serde_json::from_str(&json).unwrap();
        packet.slots.clear();
        assert!(
            unpack(
                packet,
                |request, map| locations::file_request(request, &mut |location| map(location)),
                |_| Ok(PathBuf::from("mapped"))
            )
            .is_err()
        );
    }
}
