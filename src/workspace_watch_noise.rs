//! Event-triggered duplicate detection, not a filesystem poll.
//! Linux IN_ATTRIB cannot identify which timestamp changed. Cache bounded
//! attributes so access-time noise and timestamp-only Git metadata do not cause
//! repeated status scans. Metadata events never read contents (NOATIME is not
//! reliable on all shared filesystems). Data/structural project events are
//! never suppressed; Git data events may use a bounded content fingerprint.
use super::*;

#[derive(Default)]
pub(crate) struct Filter {
    #[cfg(target_os = "linux")]
    entries: std::collections::HashMap<PathBuf, Fingerprint>,
}

#[cfg(not(target_os = "linux"))]
impl Filter {
    pub(crate) fn filter(&mut self, burst: Burst) -> crate::background::WorkspaceEventBurst { burst.event }
}

#[cfg(target_os = "linux")]
struct Fingerprint {
    attributes: Attributes,
    content: Option<[u8; 32]>,
}

#[cfg(target_os = "linux")]
#[derive(PartialEq, Eq)]
enum Attributes {
    Missing,
    Present {
        device: u64,
        inode: u64,
        size: u64,
        modified: Option<std::time::SystemTime>,
        mode: u32,
        uid: u32,
        gid: u32,
        links: u64,
    },
}

#[cfg(target_os = "linux")]
impl Filter {
    fn changed(&mut self, path: &Path, git: bool, metadata_only: bool) -> bool {
        let Some(attributes) = attributes(path, git) else {
            self.entries.remove(path);
            return true;
        };
        let previous = self.entries.get(path).filter(|previous| previous.attributes == attributes);
        if metadata_only && previous.is_some() { return false; }
        let content = (!metadata_only).then(|| content_fingerprint(path)).flatten();
        let changed = metadata_only || content.is_none()
            || !previous.is_some_and(|previous| previous.content == content);
        if self.entries.len() >= 256 && !self.entries.contains_key(path) { self.entries.clear(); }
        self.entries.insert(path.to_owned(), Fingerprint { attributes, content });
        changed
    }

    pub(crate) fn filter(&mut self, mut burst: Burst) -> crate::background::WorkspaceEventBurst {
        if burst.event.rescan {
            self.entries.clear();
            return burst.event;
        }
        for path in &burst.event.paths {
            // Remember attributes even for unconditional data notifications:
            // a subsequent access-time notification must not start a feedback
            // cycle after Git or the editor reads the changed file.
            if !burst.metadata.contains(path) { self.changed(path, false, true); }
        }
        burst.event.paths.retain(|path| !burst.metadata.contains(path) || self.changed(path, false, true));
        if !burst.git_paths.is_empty() {
            let mut changed = false;
            for path in &burst.git_paths {
                changed |= self.changed(path, true, burst.metadata.contains(path));
            }
            burst.event.git_metadata_changed = changed;
        }
        burst.event
    }
}

#[cfg(target_os = "linux")]
fn attributes(path: &Path, git: bool) -> Option<Attributes> {
    use std::os::unix::fs::MetadataExt;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(Attributes::Missing),
        Err(_) => return None,
    };
    Some(Attributes::Present {
        device: metadata.dev(), inode: metadata.ino(), size: metadata.len(),
        modified: (!git).then(|| metadata.modified().ok()).flatten(),
        mode: metadata.mode(), uid: metadata.uid(), gid: metadata.gid(), links: metadata.nlink(),
    })
}

#[cfg(target_os = "linux")]
fn content_fingerprint(path: &Path) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::{io::Read, os::unix::fs::{MetadataExt, OpenOptionsExt}};
    const MAX_BYTES: u64 = 4 * 1024 * 1024;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES { return None; }
    // Reading for deduplication must not create another access-time update.
    // If NOATIME is unavailable (e.g. ownership/FS restrictions), forward the
    // event instead of falling back to an ordinary read.
    let mut file = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOATIME | libc::O_NOFOLLOW).open(path).ok()?;
    let before = file.metadata().ok()?;
    if !before.is_file() || before.len() > MAX_BYTES { return None; }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 32 * 1024];
    let mut total = 0u64;
    loop {
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if count == 0 { break; }
        total += count as u64;
        if total > MAX_BYTES { return None; }
        digest.update(&buffer[..count]);
    }
    let after = file.metadata().ok()?;
    if total != before.len() || before.len() != after.len()
        || before.mtime() != after.mtime() || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec()
    { return None; }
    Some(digest.finalize().into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use notify::{Event, EventKind, event::{DataChange, MetadataKind, ModifyKind}};
    use std::fs::{File, FileTimes, Permissions};
    use std::os::unix::fs::PermissionsExt;

    fn burst(root: &Path, path: &Path, kind: EventKind) -> Burst {
        let mut burst = Burst::default();
        burst.push(root, Ok(Event::new(kind).add_path(path.to_owned())));
        burst
    }
    fn metadata(root: &Path, path: &Path) -> Burst {
        burst(root, path, EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)))
    }
    fn git_file(root: &Path) -> PathBuf {
        let directory = root.join(".git");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("index");
        std::fs::write(&path, b"first").unwrap();
        path
    }

    #[test]
    fn access_time_noise_does_not_repeat_changes_or_create_another_access() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        std::fs::write(&path, "source").unwrap();
        let mut filter = Filter::default();
        assert!(!filter.filter(metadata(root.path(), &path)).is_empty());
        let accessed = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        File::open(&path).unwrap().set_times(FileTimes::new().set_accessed(accessed)).unwrap();
        assert!(filter.filter(metadata(root.path(), &path)).is_empty());
        assert_eq!(std::fs::metadata(path).unwrap().accessed().unwrap(), accessed);
    }

    #[test]
    fn metadata_filter_preserves_permissions_and_data_events_preserve_same_size_same_mtime_edits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        std::fs::write(&path, "first").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut filter = Filter::default();
        assert!(!filter.filter(metadata(root.path(), &path)).is_empty());
        std::fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        assert!(!filter.filter(metadata(root.path(), &path)).is_empty());
        std::fs::write(&path, "other").unwrap();
        File::open(&path).unwrap().set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(!filter.filter(burst(root.path(), &path, EventKind::Modify(ModifyKind::Data(DataChange::Any)))).is_empty());
        assert!(filter.filter(metadata(root.path(), &path)).is_empty());
    }

    #[test]
    fn data_events_win_over_metadata_noise_in_either_order() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        std::fs::write(&path, "source").unwrap();
        let data = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        let meta = EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any));
        for kinds in [[data, meta], [meta, data]] {
            let mut filter = Filter::default();
            filter.filter(metadata(root.path(), &path));
            let mut combined = Burst::default();
            for kind in kinds { combined.push(root.path(), Ok(Event::new(kind).add_path(path.clone()))); }
            assert_eq!(filter.filter(combined).paths, vec![path.clone()]);
        }
    }

    #[test]
    fn repeated_git_metadata_is_bounded_and_timestamp_only_changes_do_not_rescan() {
        let root = tempfile::tempdir().unwrap();
        let path = git_file(root.path());
        let mut combined = Burst::default();
        for _ in 0..10_000 {
            combined.push(root.path(), Ok(Event::new(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any))).add_path(path.clone())));
        }
        assert_eq!(combined.git_paths.len(), 1);
        assert!(!combined.event.rescan);
        let mut filter = Filter::default();
        assert!(filter.filter(combined).git_metadata_changed);
        File::open(&path).unwrap().set_times(FileTimes::new().set_modified(std::time::UNIX_EPOCH)).unwrap();
        assert!(!filter.filter(metadata(root.path(), &path)).git_metadata_changed);
        std::fs::write(&path, "other").unwrap();
        let data = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        assert!(filter.filter(burst(root.path(), &path, data)).git_metadata_changed);
        assert!(!filter.filter(burst(root.path(), &path, data)).git_metadata_changed);
        std::fs::remove_file(&path).unwrap();
        assert!(filter.filter(metadata(root.path(), &path)).git_metadata_changed);
        assert!(!filter.filter(metadata(root.path(), &path)).git_metadata_changed);
        std::fs::write(&path, "other").unwrap();
        assert!(filter.filter(metadata(root.path(), &path)).git_metadata_changed);
    }

    #[test]
    fn large_git_data_changes_and_symlink_data_changes_are_forwarded_conservatively() {
        let root = tempfile::tempdir().unwrap();
        let path = git_file(root.path());
        File::create(&path).unwrap().set_len(4 * 1024 * 1024 + 1).unwrap();
        let link = root.path().join(".git/link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let mut filter = Filter::default();
        for path in [path, link] {
            for _ in 0..2 {
                assert!(filter.filter(burst(root.path(), &path, EventKind::Modify(ModifyKind::Data(DataChange::Any)))).git_metadata_changed);
            }
        }
    }

    #[test]
    fn metadata_noise_is_deduplicated_without_opening_unreadable_files() {
        let root = tempfile::tempdir().unwrap();
        let path = git_file(root.path());
        std::fs::set_permissions(&path, Permissions::from_mode(0o000)).unwrap();
        let mut filter = Filter::default();
        assert!(filter.filter(metadata(root.path(), &path)).git_metadata_changed);
        assert!(!filter.filter(metadata(root.path(), &path)).git_metadata_changed);
    }

    #[test]
    fn eviction_and_overflow_never_hide_unknown_changes() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(".git");
        std::fs::create_dir(&directory).unwrap();
        let mut filter = Filter::default();
        for index in 0..300 {
            let path = directory.join(index.to_string());
            std::fs::write(&path, "metadata").unwrap();
            assert!(filter.filter(metadata(root.path(), &path)).git_metadata_changed);
        }
        assert!(filter.entries.len() <= 256);
        let path = directory.join("0");
        assert!(filter.filter(metadata(root.path(), &path)).git_metadata_changed);
        assert!(!filter.filter(metadata(root.path(), &path)).git_metadata_changed);
        let mut overflow = Burst::default();
        overflow.event.rescan = true;
        assert!(filter.filter(overflow).rescan);
        assert!(filter.filter(metadata(root.path(), &path)).git_metadata_changed);
    }
}
