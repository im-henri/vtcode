//! Filesystem transactions execute beside the files, never as a sequence of
//! per-entry SSH round trips. The workbench receives typed path identities.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Task {
    Create {
        path: RemotePath,
        directory: bool,
    },
    Rename {
        from: RemotePath,
        to: RemotePath,
    },
    Paste {
        sources: Vec<RemotePath>,
        directory: RemotePath,
        cut: bool,
    },
    Delete {
        paths: Vec<RemotePath>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Outcome {
    message: String,
    renames: Vec<(RemotePath, RemotePath)>,
    deleted: Vec<RemotePath>,
    check_deleted: Vec<RemotePath>,
    clear_clipboard: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    completed_cut_paths: Vec<RemotePath>,
    warning: bool,
    open_file: Option<RemotePath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pasted_paths: Vec<RemotePath>,
}

pub(crate) fn execute(
    root: &Path,
    task: Task,
    cancel: &crate::background::CancellationToken,
    progress: impl FnMut(crate::file_deletion::Progress, bool),
) -> io::Result<Outcome> {
    let mut result = crate::background::FilesystemActionOutcome::default();
    let mut clear_clipboard = false;
    match task {
        Task::Create { path, directory } => {
            let native = path.resolve(root)?;
            if directory {
                std::fs::create_dir(&native)?;
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&native)?;
                result.open_file = Some(native);
            }
            result.message = format!("Created {}", path.display());
        }
        Task::Rename { from, to } => {
            if from.0.is_empty() {
                return Err(io::Error::other("Cannot rename the remote workspace root"));
            }
            let source = from.resolve(root)?;
            let destination = to.resolve(root)?;
            crate::fs_ops::rename_no_replace(&source, &destination)?;
            result.message = format!("Renamed to {}", to.display());
            result.renames.push((source, destination));
        }
        Task::Delete { paths } => {
            if paths.iter().any(|path| path.0.is_empty()) {
                return Err(io::Error::other("Cannot delete the remote workspace root"));
            }
            let paths = paths
                .iter()
                .map(|path| path.resolve(root))
                .collect::<io::Result<_>>()?;
            result = crate::file_deletion::perform(paths, cancel, progress);
        }
        Task::Paste {
            sources,
            directory,
            cut,
        } => {
            if cut && sources.iter().any(|path| path.0.is_empty()) {
                return Err(io::Error::other("Cannot move the remote workspace root"));
            }
            let directory = directory.resolve(root)?;
            let sources = sources
                .iter()
                .map(|path| path.resolve(root))
                .collect::<io::Result<_>>()?;
            result =
                crate::fs_ops::paste_items(sources, &directory, cut, || cancel.is_cancelled())?;
            clear_clipboard = cut && !result.warning;
        }
    }
    let convert = |path: PathBuf| git::from_native(&path, root);
    Ok(Outcome {
        message: result.message,
        renames: result
            .renames
            .into_iter()
            .map(|(from, to)| Ok((convert(from)?, convert(to)?)))
            .collect::<io::Result<_>>()?,
        deleted: result
            .deleted
            .into_iter()
            .map(convert)
            .collect::<io::Result<_>>()?,
        check_deleted: result
            .check_deleted_paths
            .into_iter()
            .map(convert)
            .collect::<io::Result<_>>()?,
        warning: result.warning,
        clear_clipboard,
        // For a complete cut the client already knows the source list. Keep
        // the existing flag for older clients and send paths only on partial cuts.
        completed_cut_paths: if clear_clipboard {
            Vec::new()
        } else {
            result
                .completed_cut_paths
                .into_iter()
                .map(convert)
                .collect::<io::Result<_>>()?
        },
        open_file: result.open_file.map(convert).transpose()?,
        pasted_paths: result
            .pasted_paths
            .into_iter()
            .map(convert)
            .collect::<io::Result<_>>()?,
    })
}

impl workspace::Workspace {
    pub(crate) fn filesystem(
        &self,
        task: Task,
        cancel: &crate::background::CancellationToken,
        progress: impl FnMut(crate::file_deletion::Progress),
    ) -> io::Result<crate::background::FilesystemActionOutcome> {
        let cut_sources = match &task {
            Task::Paste {
                sources, cut: true, ..
            } => sources.clone(),
            _ => Vec::new(),
        };
        let Reply::Filesystem(outcome) =
            self.client
                .call_progress(Command::Filesystem(task), cancel, progress)?
        else {
            return Err(io::Error::other("Invalid remote filesystem reply"));
        };
        self.invalidate_directories();
        let completed_cut_paths = if outcome.clear_clipboard {
            if outcome.warning {
                // Older agents set the flag for partial moves as well. Their
                // rename list still identifies files we can safely consume.
                outcome
                    .renames
                    .iter()
                    .map(|(from, _)| self.view_key(from))
                    .collect()
            } else {
                cut_sources.iter().map(|path| self.view_key(path)).collect()
            }
        } else {
            outcome
                .completed_cut_paths
                .iter()
                .map(|path| self.view_key(path))
                .collect()
        };
        Ok(crate::background::FilesystemActionOutcome {
            message: outcome.message,
            renames: outcome
                .renames
                .into_iter()
                .map(|(from, to)| (self.view_key(&from), self.view_key(&to)))
                .collect(),
            deleted: outcome
                .deleted
                .iter()
                .map(|path| self.view_key(path))
                .collect(),
            check_deleted_paths: outcome
                .check_deleted
                .iter()
                .map(|path| self.view_key(path))
                .collect(),
            completed_cut_paths,
            cut_clipboard: None,
            warning: outcome.warning,
            open_file: outcome.open_file.as_ref().map(|path| self.view_key(path)),
            pasted_paths: outcome
                .pasted_paths
                .iter()
                .map(|path| self.view_key(path))
                .collect(),
            finish_rename: None,
        })
    }
}
