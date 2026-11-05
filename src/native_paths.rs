//! Lexical native path comparisons and lossless path persistence.
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "value", rename_all = "camelCase")]
pub(crate) enum NativePath {
    Utf8(String),
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl NativePath {
    pub(crate) fn from_path(path: &Path) -> Self {
        if let Some(text) = path.to_str() {
            return Self::Utf8(text.to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }

    pub(crate) fn to_path_buf(&self) -> Option<PathBuf> {
        Some(match self {
            Self::Utf8(text) => PathBuf::from(text),
            #[cfg(unix)]
            Self::Unix(bytes) => {
                use std::os::unix::ffi::OsStrExt;
                PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
            }
            #[cfg(windows)]
            Self::Windows(units) => {
                use std::os::windows::ffi::OsStringExt;
                PathBuf::from(std::ffi::OsString::from_wide(units))
            }
            _ => return None,
        })
    }
}

pub(crate) mod optional_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredPath {
        Text(PathBuf),
        Native(NativePath),
    }

    pub(crate) fn serialize<S: Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let Some(path) = path else {
            return serializer.serialize_none();
        };
        // Keep the existing string representation for Unicode names.
        match path.to_str() {
            Some(text) => serializer.serialize_some(text),
            None => serializer.serialize_some(&NativePath::from_path(path)),
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Ok(match Option::<StoredPath>::deserialize(deserializer)? {
            Some(StoredPath::Text(path)) => Some(path),
            // A session copied from another OS can still recover the text as
            // an untitled tab when its path has no native representation.
            Some(StoredPath::Native(path)) => path.to_path_buf(),
            None => None,
        })
    }
}

/// Accept Win32 and extended-length spellings of the same Windows prefix.
/// The returned suffix borrows the original path, including nonexistent files.
pub(crate) fn strip_prefix<'a>(path: &'a Path, base: &Path) -> Option<&'a Path> {
    strip_prefix_matching(path, base, |left, right| left.eq_ignore_ascii_case(right))
}

/// Compare path entries by their native spelling, allowing alternate drive and
/// UNC prefixes on Windows.
pub(crate) fn strip_spelling_prefix<'a>(path: &'a Path, base: &Path) -> Option<&'a Path> {
    strip_prefix_matching(path, base, |left, right| left == right)
}

pub(crate) fn same_spelling(left: &Path, right: &Path) -> bool {
    strip_spelling_prefix(left, right).is_some_and(|relative| relative.as_os_str().is_empty())
}

/// Find candidates for filesystem verification. The directory controls
/// whether case aliases actually refer to the same entry.
pub(crate) fn strip_possible_alias_prefix<'a>(path: &'a Path, base: &Path) -> Option<&'a Path> {
    strip_prefix_matching(path, base, possible_alias_name)
}

fn possible_alias_name(left: &OsStr, right: &OsStr) -> bool {
    left.eq_ignore_ascii_case(right)
        || (!(left.is_ascii() && right.is_ascii())
            && left.to_str().zip(right.to_str()).is_some_and(|(left, right)| {
                left.chars().flat_map(char::to_lowercase)
                    .eq(right.chars().flat_map(char::to_lowercase))
            }))
}

#[cfg(windows)]
pub(crate) fn possible_case_alias(left: &Path, right: &Path) -> bool {
    left.file_name().zip(right.file_name()).is_some_and(|(left, right)| possible_alias_name(left, right))
        && strip_possible_alias_prefix(left, right).is_some_and(|rest| rest.as_os_str().is_empty())
        && !same_spelling(left, right)
}

fn strip_prefix_matching<'a>(
    path: &'a Path,
    base: &Path,
    _entry_matches: impl Fn(&OsStr, &OsStr) -> bool,
) -> Option<&'a Path> {
    if let Ok(relative) = path.strip_prefix(base) {
        return Some(relative);
    }
    #[cfg(windows)]
    {
        let mut parts = path.components();
        for expected in base.components() {
            if !same_component(parts.next()?, expected, &_entry_matches) {
                return None;
            }
        }
        Some(parts.as_path())
    }
    #[cfg(not(windows))]
    None
}

pub(crate) fn same(left: &Path, right: &Path) -> bool {
    strip_prefix(left, right).is_some_and(|relative| relative.as_os_str().is_empty())
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::{fs::File, io, path::{Path, PathBuf}};
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    pub(crate) use vtcode::file_source::windows_final_path as final_path;

    pub(crate) fn open_entry(path: &Path, access: u32) -> io::Result<File> {
        File::options()
            .access_mode(access)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    pub(crate) fn entry_path(path: &Path, buffer: &mut Vec<u16>) -> io::Result<PathBuf> {
        final_path(&open_entry(path, FILE_READ_ATTRIBUTES)?, buffer)
    }
}

#[cfg(all(test, windows))]
pub(crate) fn enable_case_sensitive_test_directory(path: &Path) -> bool {
    let output = std::process::Command::new("fsutil.exe")
        .args(["file", "setCaseSensitiveInfo"])
        .arg(path)
        .arg("enable")
        .output()
        .expect("run fsutil on a disposable directory");
    if !output.status.success() {
        eprintln!(
            "Case-sensitive test directory unavailable: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    output.status.success()
}

#[cfg(windows)]
fn same_component(
    left: std::path::Component<'_>,
    right: std::path::Component<'_>,
    entry_matches: &impl Fn(&OsStr, &OsStr) -> bool,
) -> bool {
    use std::path::{Component, Prefix};
    let same_name = |left: &std::ffi::OsStr, right: &std::ffi::OsStr| {
        let normalize = |byte: &u8| {
            if *byte == b'/' {
                b'\\'
            } else {
                byte.to_ascii_lowercase()
            }
        };
        left.as_encoded_bytes()
            .iter()
            .map(normalize)
            .eq(right.as_encoded_bytes().iter().map(normalize))
    };
    match (left, right) {
        (Component::Prefix(left), Component::Prefix(right)) => match (left.kind(), right.kind()) {
            (
                Prefix::Disk(left) | Prefix::VerbatimDisk(left),
                Prefix::Disk(right) | Prefix::VerbatimDisk(right),
            ) => left.eq_ignore_ascii_case(&right),
            (
                Prefix::UNC(ls, lp) | Prefix::VerbatimUNC(ls, lp),
                Prefix::UNC(rs, rp) | Prefix::VerbatimUNC(rs, rp),
            ) => same_name(ls, rs) && same_name(lp, rp),
            _ => same_name(left.as_os_str(), right.as_os_str()),
        },
        (Component::Normal(left), Component::Normal(right)) => entry_matches(left, right),
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_prefix_comparison_keeps_component_boundaries_and_original_suffix() {
        assert_eq!(
            strip_prefix(
                Path::new("workspace/missing/file.txt"),
                Path::new("workspace")
            ),
            Some(Path::new("missing/file.txt"))
        );
        assert!(
            strip_prefix(
                Path::new("workspace-other/file.txt"),
                Path::new("workspace")
            )
            .is_none()
        );
        assert!(same(
            Path::new("workspace/file"),
            Path::new("workspace/file")
        ));
        assert!(!same(
            Path::new("workspace/file/child"),
            Path::new("workspace/file")
        ));
        #[cfg(unix)]
        assert!(!same(
            Path::new("workspace/FILE"),
            Path::new("workspace/file")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn native_prefix_comparison_accepts_drive_and_unc_extended_spellings_without_disk_io() {
        for (left, right) in [
            (r"\\?\C:\not-existing\folder", r"c:/NOT-existing/folder"),
            (r"\\?\UNC\server\share\folder", r"\\SERVER\share\folder"),
        ] {
            assert!(same(Path::new(left), Path::new(right)));
            assert!(same(Path::new(right), Path::new(left)));
            for (base, path) in [
                (Path::new(left), Path::new(right).join("100%file.txt")),
                (Path::new(right), Path::new(left).join("100%file.txt")),
            ] {
                assert_eq!(strip_prefix(&path, base), Some(Path::new("100%file.txt")));
            }
        }
        assert!(!same(Path::new(r"C:\folder"), Path::new(r"D:\folder")));
        assert!(!same(
            Path::new(r"\\?\UNC\server\share\folder"),
            Path::new(r"\\server\other\folder")
        ));
    }
}
