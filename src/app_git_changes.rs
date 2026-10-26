use crate::{app::GitFileChange, background::GitActionOutcome};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

fn message<P: AsRef<Path>>(action: &str, changes: &[GitFileChange<P>]) -> String {
    if changes.len() == 1 {
        format!("{action}: {}", changes[0].path.as_ref().display())
    } else {
        format!("{action} {} files", changes.len())
    }
}

pub(crate) fn stage<P: AsRef<Path>>(root: &Path, changes: &[GitFileChange<P>]) -> Result<GitActionOutcome, String> {
    let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
    let mut index = repo.index().map_err(|error| error.to_string())?;
    for change in changes {
        let path = change.path.as_ref();
        if (1..=3).any(|stage| index.get_path(path, stage).is_some()) {
            check_conflict_file(&repo, path)?;
            stage_resolved_conflict(&repo, &mut index, path)?;
            continue;
        }
        if change.status == 'D' || change.status == '!' && repo.workdir().is_some_and(|root| std::fs::symlink_metadata(root.join(path)).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)) {
            index.remove_path(path)
        } else {
            index.add_path(path)
        }
        .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    index.write().map_err(|error| error.to_string())?;
    Ok(GitActionOutcome {
        message: message("Staged", changes),
        refresh_line_status: true,
        ..Default::default()
    })
}

fn stage_resolved_conflict(repo: &git2::Repository, index: &mut git2::Index, path: &Path) -> Result<(), String> {
    // add_all can treat conflicted gitlinks as deletions; add_path can retain
    // their conflict stages. Remove them explicitly, preserving Git's REUC
    // resolution history, before adding the resolved file/link/commit.
    index.remove_path(path).map_err(|error| error.to_string())?;
    let absent_or_directory = repo.workdir().is_some_and(|root| {
        let full = root.join(path);
        let metadata = std::fs::symlink_metadata(&full);
        metadata.as_ref().is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            || metadata.is_ok_and(|metadata| metadata.is_dir()) && std::fs::symlink_metadata(full.join(".git"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    });
    // A file/directory resolution removes the old file. Stage All's ordinary
    // traversal adds its children; a gitlink is added explicitly above that scan.
    if !absent_or_directory { index.add_path(path).map_err(|error| format!("{}: {error}", path.display()))?; }
    Ok(())
}

fn check_conflict_file(repo: &git2::Repository, path: &Path) -> Result<(), String> {
    let Some(workdir) = repo.workdir() else { return Err("Cannot resolve conflicts in a bare repository".into()); };
    let workdir = std::fs::canonicalize(workdir).map_err(|error| error.to_string())?;
    let full = worktree_path(&workdir, path)?;
    match std::fs::symlink_metadata(&full) {
        // Git stages a link itself, or a submodule's checked-out commit. Neither
        // is a text file to inspect, and a special device/pipe must not be opened.
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => return Ok(()),
        Ok(metadata) if !metadata.is_file() => return Err(format!("{} is not a regular file", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
        Ok(_) => {}
    }
    let file = match std::fs::File::open(&full) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if crate::merge_conflicts::has_markers_in_reader(file).map_err(|error| error.to_string())? {
        return Err(format!("Resolve all conflict markers in {} and save the file before staging", path.display()));
    }
    Ok(())
}

pub(crate) fn prepare_conflicts_for_stage_all(repo: &git2::Repository, index: &mut git2::Index) -> Result<(), String> {
    if !index.has_conflicts() { return Ok(()); }
    let mut paths = BTreeSet::new();
    for conflict in index.conflicts().map_err(|error| error.to_string())? {
        let conflict = conflict.map_err(|error| error.to_string())?;
        for entry in [conflict.our, conflict.their, conflict.ancestor].into_iter().flatten() {
            paths.insert(entry.path);
        }
    }
    let mut checked = Vec::with_capacity(paths.len());
    for path in paths {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            Path::new(std::ffi::OsStr::from_bytes(&path))
        };
        #[cfg(not(unix))]
        let path = std::str::from_utf8(&path).map_err(|_| "Resolve this non-UTF-8 conflict path with Git")?;
        check_conflict_file(repo, path.as_ref())?;
        checked.push(PathBuf::from(path));
    }
    // Validate every path before modifying even the in-memory index. No file
    // is rescanned, and failures leave the on-disk index untouched.
    for path in checked {
        stage_resolved_conflict(repo, index, &path)?;
    }
    Ok(())
}

fn reset_index_paths<'a, P: AsRef<Path> + 'a>(
    repo: &git2::Repository,
    index: &mut git2::Index,
    changes: impl Iterator<Item = &'a GitFileChange<P>>,
) -> Result<(), String> {
    let mut head_index = git2::Index::new().map_err(|error| error.to_string())?;
    match repo.head().and_then(|head| head.peel_to_tree()) {
        Ok(tree) => head_index
            .read_tree(&tree)
            .map_err(|error| error.to_string())?,
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
            ) => {}
        Err(error) => return Err(error.to_string()),
    }
    // Index lookups are literal: brackets or other glob characters in a
    // selected filename must never affect neighboring files.
    for path in changes.flat_map(GitFileChange::native_paths) {
        if let Some(entry) = head_index.get_path(path, 0) {
            index.add(&entry).map_err(|error| error.to_string())?;
        } else if index.get_path(path, 0).is_some() {
            index.remove_path(path).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn unstage<P: AsRef<Path>>(root: &Path, changes: &[GitFileChange<P>]) -> Result<GitActionOutcome, String> {
    let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
    let mut index = repo.index().map_err(|error| error.to_string())?;
    reset_index_paths(&repo, &mut index, changes.iter())?;
    index.write().map_err(|error| error.to_string())?;
    Ok(GitActionOutcome {
        message: message("Unstaged", changes),
        refresh_line_status: true,
        ..Default::default()
    })
}

fn worktree_path(workdir: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Discard target is not a repository-relative file".into());
    }
    let path = workdir.join(relative);
    // Resolve the parent, not the leaf: discarding a symlink removes the link,
    // while a symlinked parent must not redirect deletion outside the worktree.
    let mut parent = path.parent();
    while let Some(directory) = parent {
        match std::fs::canonicalize(directory) {
            Ok(resolved) if resolved.starts_with(workdir) => return Ok(path),
            Ok(_) => return Err(format!("{} is outside the repository", relative.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent = directory.parent()
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Cannot resolve discard target".into())
}

pub(crate) fn discard<P: AsRef<Path>>(root: &Path, changes: &[GitFileChange<P>]) -> Result<GitActionOutcome, String> {
    let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
    let workdir = std::fs::canonicalize(
        repo.workdir()
            .ok_or("Bare repositories cannot discard files")?,
    )
    .map_err(|error| error.to_string())?;
    let mut index = repo.index().map_err(|error| error.to_string())?;
    let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
    for change in changes {
        for relative in change.native_paths() {
            let path = worktree_path(&workdir, relative)?;
            if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir())
                || index
                    .get_path(relative, 0)
                    .is_some_and(|entry| entry.mode == 0o160000)
            {
                return Err(format!("Cannot discard directory or submodule: {}", relative.display()));
            }
            paths.insert(relative.to_owned());
        }
        if !change.staged {
            let tracked = index.get_path(change.path.as_ref(), 0).is_some();
            if tracked == (change.status == '?') {
                return Err(format!(
                    "{} changed staging state; refresh and try again",
                    change.path.as_ref().display()
                ));
            }
        }
    }
    let mut staged = changes.iter().filter(|change| change.staged).peekable();
    if staged.peek().is_some() {
        reset_index_paths(&repo, &mut index, staged)?;
        index.write().map_err(|error| error.to_string())?;
    }
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force().disable_pathspec_match(true);
    let mut restore = false;
    let mut delete = Vec::new();
    for relative in paths {
        if index.get_path(&relative, 0).is_some() {
            checkout.path(&relative);
            restore = true;
        } else {
            delete.push(worktree_path(&workdir, &relative)?);
        }
    }
    if restore {
        // Unstaged edits restore the index version, preserving staged work.
        repo.checkout_index(Some(&mut index), Some(&mut checkout))
            .map_err(|error| error.to_string())?;
    }
    for path in delete {
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("{}: {error}", path.display()));
            }
        }
    }
    Ok(GitActionOutcome {
        message: message("Discarded", changes),
        refresh_explorer: true,
        refresh_line_status: true,
        reload_all_clean_files: true,
        ..Default::default()
    })
}
