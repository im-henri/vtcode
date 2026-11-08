//! Large, complete status reads use Git's parallel index/worktree scanner.
//! Small repositories and literal watcher scopes stay in-process in libgit2.
use crate::{app::GitFileChange, background::CancellationToken};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const NATIVE_STATUS_MIN_ENTRIES: usize = 4_096;
/// Index entries without cached stat data (size 0, as `git reset`, unstage
/// and checkout of changed paths leave them) make libgit2 hash the worktree
/// file to confirm every status. Git's scanner does that ~13x faster
/// (3,000 files after Unstage All: 4.2 s per refresh vs 0.3 s).
const UNCERTAIN_STAT_MIN_ENTRIES: usize = 256;

#[cfg(test)]
thread_local! { static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(crate) fn prefer_native(repo: &git2::Repository) -> bool {
    repo.index().is_ok_and(|index| index.len() >= NATIVE_STATUS_MIN_ENTRIES
        || index.iter().filter(|entry| entry.file_size == 0)
            .take(UNCERTAIN_STAT_MIN_ENTRIES).count() >= UNCERTAIN_STAT_MIN_ENTRIES)
}

/// None means Git is not installed; the caller can use bundled libgit2.
pub(crate) fn read<P>(
    repo: &git2::Repository,
    include_untracked: bool,
    cancel: &CancellationToken,
    format_path: impl Fn(&Path) -> P,
) -> Result<Option<Vec<GitFileChange<P>>>, String> {
    #[cfg(test)]
    READS.set(READS.get() + 1);
    crate::app_git::ignore_working_copies(repo).map_err(|error| error.to_string())?;
    let mut command = command(repo)?;
    command
        .args(["status", "--porcelain=v1", "-z", "--renames"])
        .arg(if include_untracked {
            "--untracked-files=normal"
        } else {
            "--untracked-files=no"
        });
    let Some(output) = run_output(&mut command, cancel).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let mut files = parse_status(&output, Path::to_path_buf)?;
    files.retain(|file| {
        file.status != '?'
            || !(crate::formatters::is_temporary_path(&file.path)
                || crate::fs_ops::is_copy_temporary_path(&file.path))
            || !repo.status_should_ignore(&file.path).unwrap_or(false)
    });
    let directories: Vec<_> = files
        .iter()
        .filter(|file| file.directory)
        .map(|file| file.path.clone())
        .collect();
    if !directories.is_empty() && !cancel.is_cancelled() {
        let mut command = command_for_untracked(repo)?;
        // ls-files supports extra ignore rules without replacing the user's
        // excludes file. Refine only new directory summaries: a parent that
        // contains nothing but an editor working copy must disappear too.
        // Clean/modified repositories need just the single status command.
        // Bound arguments on Windows. A very large set of new directories
        // uses one complete untracked pass instead of exceeding argv limits.
        let scoped = directories.iter().map(|path| path.as_os_str().as_encoded_bytes().len() + 3).sum::<usize>() <= 8 * 1024;
        if scoped { command.arg("--").args(&directories); }
        let Some(output) = run_output(&mut command, cancel).map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        files.retain(|file| if scoped { !file.directory } else { file.status != '?' });
        for path in records(&output)? {
            if cancel.is_cancelled() {
                break;
            }
            let directory = path.ends_with(b"/");
            let path = native_path(if directory {
                &path[..path.len() - 1]
            } else {
                path
            })?;
            files.push(GitFileChange {
                path,
                original_path: None,
                status: '?',
                staged: false,
                directory,
            });
        }
    }
    Ok(Some(
        files
            .into_iter()
            .map(|file| GitFileChange {
                path: format_path(&file.path),
                original_path: file.original_path.as_deref().map(&format_path),
                status: file.status,
                staged: file.staged,
                directory: file.directory,
            })
            .collect(),
    ))
}

fn command(repo: &git2::Repository) -> Result<Command, String> {
    let mut command = crate::git_repositories::git_command_for(repo)?;
    command
        .arg("--no-optional-locks")
        .env_remove("GIT_INDEX_FILE");
    Ok(command)
}

fn command_for_untracked(repo: &git2::Repository) -> Result<Command, String> {
    let mut command = command(repo)?;
    command
        .arg("--literal-pathspecs")
        .args([
            "ls-files",
            "-z",
            "--others",
            "--directory",
            "--no-empty-directory",
            "--exclude-standard",
        ])
        .arg(format!(
            "--exclude={}*/",
            crate::formatters::TEMP_DIR_PREFIX
        ))
        .arg(format!("--exclude={}", crate::fs_ops::COPY_TEMP_GIT_IGNORE));
    Ok(command)
}

// Drain both pipes while waiting, including output larger than a pipe buffer.
// The existing process owner kills descendants (filters/fsmonitor hooks too),
// so switching workspaces or disabling source control stops the actual scan.
fn run_output(command: &mut Command, cancel: &CancellationToken) -> io::Result<Option<Vec<u8>>> {
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = match vtcode::acp::LocalProcess::spawn(command) {
        Ok(process) => process,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut stdout = process.child.stdout.take().unwrap();
    let mut stderr = process.child.stderr.take().unwrap();
    std::thread::scope(|scope| {
        let stdout = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let status = loop {
            if cancel.is_cancelled() {
                process.kill();
                let _ = process.child.wait();
                break Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            match process.try_wait_preserving_children() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    process.kill();
                    let _ = process.child.wait();
                    break Err(error);
                }
            }
        };
        let stdout = stdout
            .join()
            .map_err(|_| io::Error::other("Git stdout reader failed"))??;
        let stderr = stderr
            .join()
            .map_err(|_| io::Error::other("Git stderr reader failed"))??;
        if !status?.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&stderr).trim().to_owned(),
            ));
        }
        Ok(Some(stdout))
    })
}

fn records(bytes: &[u8]) -> Result<impl Iterator<Item = &[u8]>, String> {
    if !bytes.is_empty() && !bytes.ends_with(b"\0") {
        return Err("Incomplete Git status output".into());
    }
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty()))
}

fn native_path(bytes: &[u8]) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(Path::new(std::ffi::OsStr::from_bytes(bytes)).to_owned())
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|error| format!("Invalid Git path: {error}"))
    }
}

fn parse_status<P>(
    bytes: &[u8],
    format_path: impl Fn(&Path) -> P,
) -> Result<Vec<GitFileChange<P>>, String> {
    let mut records = records(bytes)?;
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err("Invalid Git status record".into());
        }
        if &record[..2] == b"??" {
            let directory = record.ends_with(b"/");
            let path = native_path(&record[3..record.len() - usize::from(directory)])?;
            files.push(GitFileChange {
                path: format_path(&path),
                original_path: None,
                status: '?',
                staged: false,
                directory,
            });
            continue;
        }
        let path = native_path(&record[3..])?;
        let original = if matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C') {
            Some(native_path(
                records.next().ok_or("Missing Git rename source")?,
            )?)
        } else {
            None
        };
        if matches!(
            &record[..2],
            b"DD" | b"AU" | b"UD" | b"UA" | b"DU" | b"AA" | b"UU"
        ) {
            files.push(GitFileChange {
                path: format_path(&path),
                original_path: None,
                status: '!',
                staged: false,
                directory: false,
            });
            continue;
        }
        for (column, staged) in [(0, true), (1, false)] {
            let status = match record[column] {
                b'A' => 'A',
                b'D' => 'D',
                b'M' | b'T' => 'M',
                b'R' | b'C' => 'R',
                b' ' => continue,
                _ => return Err("Unknown Git status code".into()),
            };
            files.push(GitFileChange {
                path: format_path(&path),
                original_path: (status == 'R')
                    .then(|| original.as_deref().map(&format_path))
                    .flatten(),
                status,
                staged,
                directory: false,
            });
        }
    }
    Ok(files)
}

#[cfg(test)]
#[path = "git_status_tests.rs"]
mod tests;
