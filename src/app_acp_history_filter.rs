//! Workspace preferences containing opaque IDs only, never chat transcripts.
use std::{collections::BTreeSet, io::Write, path::PathBuf};
use vtcode::acp::{AgentConfig, protocol as p};

#[derive(Default)]
pub(super) struct HistoryFilter {
    path: Option<PathBuf>,
    hidden: BTreeSet<String>,
}

impl HistoryFilter {
    pub(super) fn load(directory: PathBuf, agent: &AgentConfig) -> Result<Self, String> {
        // Use a stable identity without persisting executable arguments or env
        // values, which can contain secrets. Display-name edits preserve it.
        let identity = serde_json::to_vec(&(&agent.command, &agent.args, &agent.env))
            .map_err(|e| e.to_string())?;
        let mut hash = 0x6c62272e07bb014262b821756295c58du128;
        for byte in identity {
            hash = (hash ^ u128::from(byte)).wrapping_mul(0x0000000001000000000000000000013b);
        }
        let path = directory
            .join("chat-history-filters")
            .join(format!("{hash:032x}.json"));
        let hidden = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Cannot read chat history preferences: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => return Err(format!("Cannot read chat history preferences: {e}")),
        };
        Ok(Self {
            path: Some(path),
            hidden,
        })
    }

    pub(super) fn contains(&self, id: &p::SessionId) -> bool {
        self.hidden.contains(&id.to_string())
    }

    pub(super) fn set(&mut self, id: &p::SessionId, hidden: bool) -> Result<(), String> {
        if self.contains(id) == hidden {
            return Ok(());
        }
        let mut next = self.hidden.clone();
        let changed = if hidden {
            next.insert(id.to_string())
        } else {
            next.remove(&id.to_string())
        };
        if !changed {
            return Ok(());
        }
        if let Some(path) = &self.path {
            let parent = path.parent().expect("history preferences parent");
            let save = || -> Result<(), Box<dyn std::error::Error>> {
                std::fs::create_dir_all(parent)?;
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                temporary.write_all(&serde_json::to_vec(&next)?)?;
                temporary.persist(path)?;
                Ok(())
            };
            save().map_err(|e| format!("Cannot save chat history preferences: {e}"))?;
        }
        self.hidden = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acp_chat_history_preferences_keep_only_ids_and_are_scoped_to_agent_and_workspace() {
        let root = tempfile::tempdir().unwrap();
        let mut agent = AgentConfig::new("Agent", "mock-agent");
        agent.env.insert("SECRET".into(), "private-value".into());
        let id = p::SessionId::new("opaque-id");
        let mut filter = HistoryFilter::load(root.path().into(), &agent).unwrap();
        filter.set(&id, true).unwrap();
        let text = std::fs::read_to_string(filter.path.as_ref().unwrap()).unwrap();
        assert_eq!(text, "[\"opaque-id\"]");
        let mut reloaded = HistoryFilter::load(root.path().into(), &agent).unwrap();
        assert!(reloaded.contains(&id));
        reloaded.set(&id, false).unwrap();
        assert!(
            !HistoryFilter::load(root.path().into(), &agent)
                .unwrap()
                .contains(&id)
        );
        filter.set(&p::SessionId::new("another-id"), true).unwrap();
        agent.args.push("another-account".into());
        assert!(
            !HistoryFilter::load(root.path().into(), &agent)
                .unwrap()
                .contains(&p::SessionId::new("another-id"))
        );
        assert!(
            !HistoryFilter::load(root.path().join("other-workspace"), &agent)
                .unwrap()
                .contains(&p::SessionId::new("another-id"))
        );
    }
}
