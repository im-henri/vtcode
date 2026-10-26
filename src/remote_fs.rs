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
    warning: bool,
    open_file: Option<RemotePath>,
}

pub(crate) fn execute(
    root: &Path,
    task: Task,
    cancel: &crate::background::CancellationToken,
    progress: impl FnMut(crate::file_deletion::Progress, bool),
) -> io::Result<Outcome> {
    let mut result = crate::background::FilesystemActionOutcome::default();
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
            let directory = directory.resolve(root)?;
            if !directory.is_dir() {
                return Err(io::Error::other("Paste destination is not a directory"));
            }
            let mut done = 0;
            let mut failed = 0;
            let mut last_error = None;
            for path in sources {
                if cancel.is_cancelled() {
                    break;
                }
                let source = path.resolve(root)?;
                let name = source
                    .file_name()
                    .ok_or_else(|| io::Error::other("Cannot copy a filesystem root"))?;
                let mut destination = directory.join(name);
                if cut && source == destination {
                    crate::fs_ops::rename_no_replace(&source, &destination)?;
                    done += 1;
                    continue;
                }
                // symlink_metadata recognizes dangling symlinks as occupied.
                if std::fs::symlink_metadata(&destination).is_ok() {
                    let stem = source.file_stem().unwrap_or(name).to_string_lossy();
                    let extension = source
                        .extension()
                        .map(|s| format!(".{}", s.to_string_lossy()))
                        .unwrap_or_default();
                    let mut found = false;
                    for suffix in 1..=10_000 {
                        destination = directory.join(format!("{stem} ({suffix}){extension}"));
                        if matches!(std::fs::symlink_metadata(&destination), Err(error) if error.kind() == io::ErrorKind::NotFound)
                        {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return Err(io::Error::other("Could not choose an unused copy name"));
                    }
                }
                let operation = if cut {
                    crate::fs_ops::rename_no_replace(&source, &destination)
                } else {
                    crate::fs_ops::copy_no_replace(&source, &destination, || cancel.is_cancelled())
                };
                match operation {
                    Ok(()) => {
                        done += 1;
                        if cut {
                            result.renames.push((source, destination));
                        }
                    }
                    Err(error) => {
                        failed += 1;
                        last_error = Some(error);
                    }
                }
            }
            if done == 0
                && let Some(error) = last_error
            {
                return Err(error);
            }
            result.message = format!(
                "{} {done} items{extra}",
                if cut { "Moved" } else { "Copied" },
                extra = if failed == 0 {
                    String::new()
                } else {
                    format!("; {failed} failed")
                }
            );
            result.warning = failed != 0 || cancel.is_cancelled();
            result.clear_explorer_clipboard = cut;
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
        clear_clipboard: result.clear_explorer_clipboard,
        open_file: result.open_file.map(convert).transpose()?,
    })
}

impl workspace::Workspace {
    pub(crate) fn filesystem(
        &self,
        task: Task,
        cancel: &crate::background::CancellationToken,
        progress: impl FnMut(crate::file_deletion::Progress),
    ) -> io::Result<crate::background::FilesystemActionOutcome> {
        let Reply::Filesystem(outcome) =
            self.client
                .call_progress(Command::Filesystem(task), cancel, progress)?
        else {
            return Err(io::Error::other("Invalid remote filesystem reply"));
        };
        self.invalidate_directories();
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
            clear_explorer_clipboard: outcome.clear_clipboard,
            warning: outcome.warning,
            open_file: outcome.open_file.as_ref().map(|path| self.view_key(path)),
            finish_rename: None,
        })
    }
}
