//! Paths typed on the Ex command line use the workspace's filesystem.

use std::io;
use std::path::{Path, PathBuf};

use crate::app::App;
use crate::remote::workspace::Workspace;

pub(crate) enum Matches {
    Ready(Vec<String>),
    Pending(u64),
}

pub(crate) struct Entry {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
}

pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub(crate) fn cwd(app: &App) -> PathBuf {
    let root = super::app_host::vim_cwd(app);
    if root.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        root.to_path_buf()
    }
}

pub(crate) fn label(app: &App, path: &Path) -> String {
    if crate::remote::workspace::is_view_key(path) {
        let mut label = app.display_path_label(path);
        if let Some(remote) = app.remote_workspace.as_ref() {
            // The server sends its canonical Windows path. Convert the common
            // drive and UNC prefixes for display without touching local disk.
            if remote.windows_paths() {
                if let Some(rest) = label.strip_prefix(r"\\?\UNC\") {
                    label = format!(r"\\{rest}");
                } else if let Some(rest) = label.strip_prefix(r"\\?\")
                    && rest.as_bytes().get(1..3) == Some(b":\\")
                {
                    label = rest.to_owned();
                }
                if label.len() > 3 && label.ends_with('\\') {
                    label.pop();
                }
            } else if label.len() > 1 && label.ends_with('/') {
                label.pop();
            }
        }
        label
    } else {
        app.display_path_label(&crate::portable::filesystem_spelling(path))
    }
}

fn escaped_char(ch: char, windows: bool) -> bool {
    matches!(ch, ' ' | '\t' | '|') || (!windows && ch == '\\')
}

pub(crate) fn argument(text: &str) -> &str {
    let text = text.trim_start();
    let end = text.trim_end().len();
    // A completed name can end in an escaped space. Keep it while trimming
    // any whitespace the user typed after the argument.
    if text[..end]
        .bytes()
        .rev()
        .take_while(|&b| b == b'\\')
        .count()
        % 2
        == 1
        && let Some(ch) = text[end..].chars().next()
        && escaped_char(ch, cfg!(windows))
    {
        return &text[..end + ch.len_utf8()];
    }
    &text[..end]
}

fn unescape(arg: &str, windows: bool) -> String {
    let mut chars = arg.chars().peekable();
    let mut text = String::with_capacity(arg.len());
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|&c| escaped_char(c, windows)) {
            text.push(chars.next().unwrap());
        } else {
            text.push(ch);
        }
    }
    text
}

fn windows_paths(remote: Option<&Workspace>) -> bool {
    remote.map_or(cfg!(windows), Workspace::windows_paths)
}

pub(crate) fn resolve(base: &Path, arg: &str, remote: Option<&Workspace>) -> io::Result<PathBuf> {
    let Some(remote) = remote else {
        return Ok(resolve_local(base, arg));
    };
    let windows = remote.windows_paths();
    let mut text = unescape(arg, windows);
    if windows {
        text = text.replace('\\', "/");
        if text.as_bytes().get(1) == Some(&b':') {
            text.insert(0, '/');
        }
    }
    remote.resolve_from_key(base, &text)
}

pub(crate) fn resolve_local(base: &Path, arg: &str) -> PathBuf {
    let text = unescape(arg, cfg!(windows));
    if text == "~"
        && let Some(home) = home()
    {
        return home;
    }
    let home_relative = text
        .strip_prefix("~/")
        .or_else(|| cfg!(windows).then(|| text.strip_prefix("~\\")).flatten());
    if let Some(rest) = home_relative
        && let Some(home) = home()
    {
        return home.join(rest);
    }
    base.join(text)
}

pub(crate) fn parent(path: &Path, remote: Option<&Workspace>) -> Option<PathBuf> {
    if crate::remote::workspace::is_view_key(path) {
        remote?.parent_key(path).ok().flatten()
    } else {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
    }
}

pub(crate) fn entries(dir: &Path, remote: Option<&Workspace>) -> io::Result<Vec<Entry>> {
    if let Some(remote) = remote {
        return Ok(remote
            .list(&remote.remote_path(dir)?)?
            .iter()
            .map(|entry| Entry {
                path: remote.view_key(&entry.path),
                name: entry.path.name(),
                is_dir: entry.is_dir,
            })
            .collect());
    }
    Ok(std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            let path = entry.path();
            Some(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: kind.is_dir() || kind.is_symlink() && path.is_dir(),
                path,
            })
        })
        .collect())
}

pub(crate) fn complete(
    arg: &str,
    directories_only: bool,
    ignore_case: bool,
    base: &Path,
    remote: Option<&Workspace>,
) -> Vec<String> {
    let windows = windows_paths(remote);
    if arg == "~" && remote.is_none() && home().is_some() {
        return vec!["~/".into()];
    }
    let mut end_of_directory = 0;
    let mut chars = arg.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|&(_, c)| escaped_char(c, windows)) {
            chars.next();
        } else if ch == '/' || windows && ch == '\\' {
            end_of_directory = i + 1;
        }
    }
    let (dir_part, file_part) = arg.split_at(end_of_directory);
    let prefix = unescape(file_part, windows);
    let directory = if dir_part.is_empty() {
        Ok(base.to_path_buf())
    } else {
        resolve(base, dir_part, remote)
    };
    let Ok(directory) = directory else {
        return Vec::new();
    };
    let Ok(entries) = entries(&directory, remote) else {
        return Vec::new();
    };

    let ignore_case = windows || ignore_case;
    let folded_prefix = ignore_case.then(|| prefix.to_lowercase());
    let mut items: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            (!directories_only || entry.is_dir)
                && (!entry.name.starts_with('.') || prefix.starts_with('.'))
                && folded_prefix.as_ref().map_or_else(
                    || entry.name.starts_with(&prefix),
                    |prefix| entry.name.to_lowercase().starts_with(prefix),
                )
        })
        .filter_map(|entry| {
            let name = if remote.is_some() {
                &entry.name
            } else {
                entry.path.file_name()?.to_str()?
            };
            if name.chars().any(char::is_control) {
                return None;
            }
            let mut candidate = dir_part.to_owned();
            for ch in name.chars() {
                if escaped_char(ch, windows) {
                    candidate.push('\\');
                }
                candidate.push(ch);
            }
            if entry.is_dir {
                candidate.push('/');
            }
            Some(candidate)
        })
        .collect();
    items.sort();
    // Directory iteration omits these valid path components.
    if matches!(prefix.as_str(), "." | "..") {
        items.insert(0, format!("{dir_part}../"));
        if prefix == "." {
            items.insert(0, format!("{dir_part}./"));
        }
    }
    items
}
