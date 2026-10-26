//! Lexical native path comparisons: no allocation, metadata, or canonicalization.
use std::path::Path;

/// Accept Win32 and extended-length spellings of the same Windows prefix.
/// The returned suffix borrows the original path, including nonexistent files.
pub(crate) fn strip_prefix<'a>(path: &'a Path, base: &Path) -> Option<&'a Path> {
    if let Ok(relative) = path.strip_prefix(base) {
        return Some(relative);
    }
    #[cfg(windows)]
    {
        let mut parts = path.components();
        for expected in base.components() {
            if !same_component(parts.next()?, expected) {
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
fn same_component(left: std::path::Component<'_>, right: std::path::Component<'_>) -> bool {
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
        _ => same_name(left.as_os_str(), right.as_os_str()),
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
