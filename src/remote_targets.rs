//! Portable, named SSH launch recipes shared by the workbench and command line.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Target {
    pub name: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub remote_command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ssh_command: String,
}

impl Target {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.validate_fields().map_err(|(_, error)| error)
    }

    pub(crate) fn validate_fields(&self) -> Result<(), (usize, String)> {
        if self.name.trim().is_empty() {
            return Err((0, "Enter a name for this target.".into()));
        }
        if self.name != self.name.trim() || self.name.chars().any(char::is_control) {
            return Err((
                0,
                "Names cannot contain control characters or leading/trailing spaces.".into(),
            ));
        }
        if self.host.is_empty()
            || self.host.starts_with('-')
            || self
                .host
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err((
                1,
                "Enter an SSH host or config alias, for example user@hostname.".into(),
            ));
        }
        if self.folder.chars().any(char::is_control) {
            return Err((
                2,
                "The remote folder cannot contain control characters.".into(),
            ));
        }
        if self.remote_command.contains('\0') {
            return Err((3, "Commands cannot contain NUL characters.".into()));
        }
        if self.ssh_command.contains('\0') {
            return Err((4, "Commands cannot contain NUL characters.".into()));
        }
        self.invocation()
            .map(|_| ())
            .map_err(|error| (4, error.replace("--ssh-command", "SSH command")))
    }

    pub(crate) fn invocation(&self) -> Result<crate::cli::Command, String> {
        // A custom job/shell command stays opaque, just like --remote-command.
        let command = if !self.remote_command.trim().is_empty() {
            self.remote_command.clone()
        } else if self.folder.is_empty() {
            "vtcode --headless".into()
        } else {
            let folder = if self.folder == "~" {
                "\"$HOME\"".into()
            } else if let Some(relative) = self.folder.strip_prefix("~/") {
                format!("\"$HOME\"/{}", shell_words::quote(relative))
            } else {
                shell_words::quote(&self.folder).into_owned()
            };
            format!("vtcode --headless -- {folder}")
        };
        crate::cli::remote_command(
            &self.host,
            Some(&command),
            (!self.ssh_command.trim().is_empty()).then_some(self.ssh_command.as_str()),
        )
    }

    pub(crate) fn launch(&self) -> Result<crate::remote::launch::Launch, String> {
        self.validate()?;
        match self.invocation()? {
            crate::cli::Command::Remote { program, args } => {
                Ok(crate::remote::launch::Launch::new(program, args))
            }
            _ => unreachable!("remote invocation"),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u32,
    targets: Vec<Target>,
}

pub(crate) fn path() -> PathBuf {
    crate::portable::data_dir().join("remote-targets.json")
}

pub(crate) struct Store {
    pub targets: Vec<Target>,
    path: PathBuf,
    source: Option<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UpdateError {
    Conflict,
    NameInUse,
    Storage(String),
}
impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => f.write_str("This target changed in another window. Reload the saved version or save your draft as a copy."),
            Self::NameInUse => f.write_str("A target with this name already exists. Choose a different name."),
            Self::Storage(error) => f.write_str(error),
        }
    }
}

fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match crate::fs_ops::open_document_source(path, false) {
        Ok((mut file, _)) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

impl Store {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let source =
            read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        let mut targets = if let Some(bytes) = &source {
            let catalog: Catalog = serde_json::from_slice(bytes)
                .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
            if catalog.version != 1 {
                return Err(format!(
                    "Unsupported remote targets version {} in {}",
                    catalog.version,
                    path.display()
                ));
            }
            catalog.targets
        } else {
            Vec::new()
        };
        validate_targets(&targets)?;
        targets.sort_by_cached_key(|target| target.name.to_lowercase());
        Ok(Self {
            targets,
            path: path.into(),
            source,
        })
    }

    pub(crate) fn find(&self, name: &str) -> Result<&Target, String> {
        self.targets.iter().find(|target| target.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("No saved remote target named {name:?}. Add it in the Remote sidebar. Targets: {}", self.path.display()))
    }

    /// Lock the read/compare/replace transaction across editor processes. An
    /// outdated editor must refresh rather than overwrite another editor's work.
    pub(crate) fn save(&mut self, targets: Vec<Target>) -> Result<(), String> {
        validate_targets(&targets)?;
        let _lock = self.lock()?;
        if read(&self.path).map_err(|error| error.to_string())? != self.source {
            return Err(
                "Remote targets changed in another window or on disk. Refresh before saving again."
                    .into(),
            );
        }
        self.write(targets)
    }

    /// Merge independent edits while holding the same cross-process lock as
    /// full catalog writes. Only a change to this target causes a conflict.
    pub(crate) fn upsert(
        &mut self,
        original: Option<&Target>,
        target: Target,
    ) -> Result<(), UpdateError> {
        target.validate().map_err(UpdateError::Storage)?;
        self.update(original, Some(target))
    }

    pub(crate) fn remove(&mut self, original: &Target) -> Result<(), UpdateError> {
        self.update(Some(original), None)
    }

    fn update(
        &mut self,
        original: Option<&Target>,
        target: Option<Target>,
    ) -> Result<(), UpdateError> {
        let _lock = self.lock().map_err(UpdateError::Storage)?;
        let mut latest = Self::load(&self.path).map_err(UpdateError::Storage)?;
        let position = if let Some(original) = original {
            Some(
                latest
                    .targets
                    .iter()
                    .position(|saved| saved == original)
                    .ok_or(UpdateError::Conflict)?,
            )
        } else {
            None
        };
        if let Some(target) = target {
            if latest.targets.iter().enumerate().any(|(index, saved)| {
                Some(index) != position && saved.name.eq_ignore_ascii_case(&target.name)
            }) {
                return Err(UpdateError::NameInUse);
            }
            if let Some(index) = position {
                latest.targets[index] = target;
            } else {
                latest.targets.push(target);
            }
        } else if let Some(index) = position {
            latest.targets.remove(index);
        }
        let targets = std::mem::take(&mut latest.targets);
        latest.write(targets).map_err(UpdateError::Storage)?;
        *self = latest;
        Ok(())
    }

    fn lock(&self) -> Result<vtcode::file_source::ExclusiveFileLock, String> {
        let parent = self
            .path
            .parent()
            .ok_or("Remote targets path has no parent")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let lock = crate::fs_ops::open_lock_file(&self.path.with_extension("lock"))
            .map_err(|error| error.to_string())?;
        vtcode::file_source::ExclusiveFileLock::try_acquire(lock).map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                "Remote targets are being saved by another window. Try again.".to_string()
            }
            fs::TryLockError::Error(error) => error.to_string(),
        })
    }

    fn write(&mut self, mut targets: Vec<Target>) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or("Remote targets path has no parent")?;
        targets.sort_by_cached_key(|target| target.name.to_lowercase());
        let catalog = Catalog {
            version: 1,
            targets,
        };
        let mut bytes = serde_json::to_vec_pretty(&catalog).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        let mut temp =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
        temp.write_all(&bytes)
            .and_then(|_| temp.as_file().sync_all())
            .map_err(|error| error.to_string())?;
        temp.persist(&self.path)
            .map_err(|error| format!("Cannot save {}: {error}", self.path.display()))?;
        self.targets = catalog.targets;
        self.source = Some(bytes);
        Ok(())
    }
}

fn validate_targets(targets: &[Target]) -> Result<(), String> {
    let mut names =
        rustc_hash::FxHashSet::with_capacity_and_hasher(targets.len(), Default::default());
    for target in targets {
        target
            .validate()
            .map_err(|error| format!("Target {:?}: {error}", target.name))?;
        if !names.insert(target.name.to_ascii_lowercase()) {
            return Err(format!(
                "A remote target named {:?} already exists. Choose a unique name.",
                target.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "remote_targets_tests.rs"]
mod tests;
