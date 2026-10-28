//! Translate only protocol URI fields. Source text, Markdown, commands and
//! arbitrary string payloads are never scanned or rewritten as file paths.
use super::*;
use serde_json::Value;

pub(super) fn uri_path(uri: &url::Url) -> Option<Vec<u8>> {
    let encoded = uri.path().as_bytes();
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%' {
            let text = std::str::from_utf8(encoded.get(index + 1..index + 3)?).ok()?;
            bytes.push(u8::from_str_radix(text, 16).ok()?);
            index += 3;
        } else {
            bytes.push(encoded[index]);
            index += 1;
        }
    }
    Some(bytes)
}

impl workspace::Workspace {
    /// Resolve a node file URI before native client path conversion. In
    /// particular a Linux URI must never become a Windows drive/UNC path.
    pub(crate) fn file_uri_key(&self, text: &str) -> Option<PathBuf> {
        self.lsp_uri(text, false)
            .and_then(|uri| url::Url::parse(&uri).ok())
            .and_then(|uri| uri.to_file_path().ok())
            .filter(|key| self.remote_path(key).is_ok())
    }

    pub(crate) fn lsp_uri(&self, text: &str, outbound: bool) -> Option<String> {
        let uri = url::Url::parse(text).ok()?;
        if uri.scheme() != "file" {
            return None;
        }
        if outbound {
            let local = uri.to_file_path().ok()?;
            let path = self.remote_path(&local).ok()?;
            let mut encoded = if path.1 {
                String::from("file://")
            } else {
                self.client
                    .hello()
                    .root_uri
                    .trim_end_matches('/')
                    .to_owned()
            };
            if path.1 || !path.0.is_empty() {
                encoded.push('/');
            }
            let drive = self.windows_paths() && path.is_absolute()
                && path.0.get(1) == Some(&b':') && path.0[0].is_ascii_alphabetic();
            let drive_root = drive && path.0.len() == 2;
            for (index, byte) in path.0.into_iter().enumerate() {
                if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte)
                    || (drive && index == 1 && byte == b':') {
                    encoded.push(byte as char);
                } else {
                    encoded.push_str(&format!("%{byte:02X}"));
                }
            }
            if drive_root { encoded.push('/'); }
            url::Url::parse(&encoded).ok().map(Into::into)
        } else {
            let root_uri = url::Url::parse(&self.client.hello().root_uri).ok()?;
            let hosted = uri
                .host_str()
                .is_some_and(|host| !host.is_empty() && host != "localhost");
            if hosted && uri.host_str() != root_uri.host_str() {
                // Never leave a remote server's file://host/... URI available
                // for the Windows client to interpret as a native UNC path.
                return Some(format!(
                    "vtcode-remote-unsupported:{}",
                    URL_SAFE_NO_PAD.encode(text.as_bytes())
                ));
            }
            let mut bytes = uri_path(&uri)?;
            // Directory URLs (including file:///C:/) carry a trailing slash;
            // RemotePath stores components, with no trailing separator.
            while bytes.len() > 1 && bytes.last() == Some(&b'/') { bytes.pop(); }
            let mut root = uri_path(&root_uri)?;
            while root.last() == Some(&b'/') {
                root.pop();
            }
            let relative = (uri.host_str() == root_uri.host_str())
                .then(|| bytes.strip_prefix(root.as_slice()))
                .flatten()
                .and_then(|tail| {
                    if tail.is_empty() {
                        Some(tail)
                    } else {
                        tail.strip_prefix(b"/")
                    }
                });
            if hosted && relative.is_none() {
                // A known UNC workspace maps only its own subtree. A path on
                // another share cannot lose its host and become a local path.
                return Some(format!(
                    "vtcode-remote-unsupported:{}",
                    URL_SAFE_NO_PAD.encode(text.as_bytes())
                ));
            }
            let path = match relative {
                Some(relative) => RemotePath::new(relative.to_vec()).ok()?,
                None => RemotePath::absolute(&bytes).ok()?,
            };
            url::Url::from_file_path(self.view_key(&path))
                .ok()
                .map(Into::into)
        }
    }
    pub(crate) fn rewrite_lsp(&self, value: &mut Value, outbound: bool) {
        match value {
            Value::Object(object) => {
                if outbound
                    && object.get("method").and_then(Value::as_str) == Some("initialize")
                    && let Some(Value::Object(params)) = object.get_mut("params")
                {
                    // A local PID has no meaning on the remote host. Process
                    // lifetime is owned by the headless service's process group.
                    params.insert("processId".into(), Value::Null);
                }
                for (key, value) in object {
                    if matches!(
                        key.as_str(),
                        "uri"
                            | "rootUri"
                            | "oldUri"
                            | "newUri"
                            | "targetUri"
                            | "target"
                            | "scopeUri"
                    ) {
                        if let Some(text) =
                            value.as_str().and_then(|text| self.lsp_uri(text, outbound))
                        {
                            *value = Value::String(text);
                        }
                    } else if key == "rootPath"
                        && outbound
                        && value.as_str() == self.view_root.to_str()
                    {
                        *value = Value::String(self.client.hello().root.clone());
                    } else if key == "changes"
                        && let Value::Object(changes) = value
                    {
                        let old = std::mem::take(changes);
                        for (uri, mut edits) in old {
                            self.rewrite_lsp(&mut edits, outbound);
                            changes.insert(self.lsp_uri(&uri, outbound).unwrap_or(uri), edits);
                        }
                    } else if key != "text" && key != "newText" && key != "documentation" {
                        self.rewrite_lsp(value, outbound);
                    }
                }
            }
            Value::Array(items) => {
                for value in items {
                    self.rewrite_lsp(value, outbound);
                }
            }
            _ => {}
        }
    }
}

pub(crate) enum LspProcess {
    Local(std::process::Child),
    Remote(process::Killer),
}
impl LspProcess {
    pub(crate) fn id(&self) -> u32 {
        match self {
            Self::Local(child) => child.id(),
            Self::Remote(child) => 0x8000_0000 | (child.id as u32 & 0x7fff_ffff),
        }
    }
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Local(child) => child.try_wait(),
            Self::Remote(child) => Ok(child.ended().then(exit_status)),
        }
    }
    pub(crate) fn kill(&mut self) -> io::Result<()> {
        match self {
            Self::Local(child) => child.kill(),
            Self::Remote(child) => {
                child.kill();
                Ok(())
            }
        }
    }
    pub(crate) fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        match self {
            Self::Local(child) => child.wait(),
            Self::Remote(child) => {
                child.kill();
                Ok(exit_status())
            }
        }
    }
}
fn exit_status() -> std::process::ExitStatus {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}
