use super::*;

struct DirectoryLink(PathBuf);

impl DirectoryLink {
    fn new(target: &Path, path: PathBuf) -> Self {
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(&path)
                .arg(target)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &path).unwrap();
        Self(path)
    }
}

impl Drop for DirectoryLink {
    fn drop(&mut self) {
        #[cfg(windows)]
        fs::remove_dir(&self.0).unwrap();
        #[cfg(unix)]
        fs::remove_file(&self.0).unwrap();
    }
}

fn track(fixture: &Fixture, path: &str, mode: u32) {
    let repository = git2::Repository::open(fixture.root.path()).unwrap();
    let mut index = repository.index().unwrap();
    index
        .add(&git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: repository.blob(b"tracked baseline\n").unwrap(),
            flags: 0,
            flags_extended: 0,
            path: path.as_bytes().to_vec(),
        })
        .unwrap();
    index.write().unwrap();
}

#[test]
fn tracked_directory_links_do_not_block_checkpoints_or_capture_external_files() {
    let mut fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("settings.json"), "external original\n").unwrap();
    let link = DirectoryLink::new(outside.path(), fixture.root.path().join(".claude"));
    track(&fixture, ".claude", 0o120000);
    fixture.write("a", "original\n");
    let index = fs::read(fixture.root.path().join(".git/index")).unwrap();

    let checkpoint = fixture.begin();
    assert!(
        !fixture.journal.state.active[0]
            .before
            .contains_key(Path::new(".claude"))
    );
    fixture.write("a", "agent edit\n");
    fs::write(link.0.join("settings.json"), "external edit\n").unwrap();
    fixture
        .journal
        .capture_checkpoint(
            checkpoint,
            BTreeSet::new(),
            &[PathBuf::from(".claude/settings.json")].into(),
        )
        .unwrap();
    fixture.finish();
    let summary = fixture.journal.summary("agent", "chat");
    assert_eq!(summary.edits.len(), 1);
    assert_eq!(summary.edits[0].path, Path::new("a"));
    fixture.decline(Selection::All).unwrap();
    assert_eq!(fixture.read("a"), "original\n");
    assert_eq!(
        fs::read_to_string(outside.path().join("settings.json")).unwrap(),
        "external edit\n"
    );
    assert_eq!(
        fs::read(fixture.root.path().join(".git/index")).unwrap(),
        index
    );
    assert!(storage::is_link(&fs::symlink_metadata(&link.0).unwrap()));

    drop(fixture.journal);
    Journal::open(fixture.root.path(), fixture.data.path()).unwrap();
}

#[test]
fn tracked_paths_under_preexisting_directory_links_are_outside_capture_coverage() {
    let mut fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("settings.json"), "external\n").unwrap();
    let link = DirectoryLink::new(outside.path(), fixture.root.path().join(".claude"));
    track(&fixture, ".claude/settings.json", 0o100644);
    let checkpoint = fixture.begin();
    assert!(fixture.journal.state.active[0].before.is_empty());
    fixture
        .journal
        .capture_checkpoint(
            checkpoint,
            BTreeSet::new(),
            &[PathBuf::from(".claude")].into(),
        )
        .unwrap();
    fixture.finish();
    assert!(fixture.journal.state.edits.is_empty());
    assert!(
        storage::checked_path(
            fixture.journal.workspace_root(),
            Path::new(".claude/settings.json")
        )
        .unwrap_err()
        .contains("does not follow links")
    );
    assert!(storage::is_link(&fs::symlink_metadata(&link.0).unwrap()));
}

#[test]
fn replacing_an_already_captured_directory_with_a_link_still_blocks_capture() {
    let mut fixture = Fixture::new();
    fs::create_dir(fixture.root.path().join("config")).unwrap();
    fixture.write("config/a", "original\n");
    let checkpoint = fixture.begin();
    fs::remove_file(fixture.root.path().join("config/a")).unwrap();
    fs::remove_dir(fixture.root.path().join("config")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("a"), "external\n").unwrap();
    let _link = DirectoryLink::new(outside.path(), fixture.root.path().join("config"));
    let error = fixture
        .journal
        .finish_checkpoint(checkpoint, BTreeSet::new())
        .unwrap_err();
    assert!(error.contains("does not follow links"), "{error}");
    assert_eq!(fixture.journal.active(), Some(checkpoint));
    assert_eq!(
        fs::read_to_string(outside.path().join("a")).unwrap(),
        "external\n"
    );
}

#[test]
fn a_tracked_symlink_replaced_by_an_ignored_regular_file_is_still_captured() {
    let mut fixture = Fixture::new();
    fixture.write(".gitignore", ".claude\n");
    fixture.write(".claude", "regular file\n");
    track(&fixture, ".claude", 0o120000);
    fixture.begin();
    fixture.write(".claude", "changed\n");
    fixture.finish();
    fixture.decline(Selection::All).unwrap();
    assert_eq!(fixture.read(".claude"), "regular file\n");
}

#[test]
#[cfg(unix)]
fn tracked_file_and_dangling_symlinks_do_not_block_checkpoints() {
    let mut fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("a"), "external\n").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("a"),
        fixture.root.path().join("linked-file"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("missing"),
        fixture.root.path().join("dangling"),
    )
    .unwrap();
    for path in ["linked-file", "dangling"] {
        track(&fixture, path, 0o120000);
    }
    fixture.write("a", "original\n");
    fixture.begin();
    assert_eq!(fixture.journal.state.active[0].before.len(), 1);
    fixture.write("a", "changed\n");
    fixture.finish();
    fixture.decline(Selection::All).unwrap();
    assert_eq!(fixture.read("a"), "original\n");
    assert_eq!(
        fs::read_to_string(outside.path().join("a")).unwrap(),
        "external\n"
    );
    assert!(
        fs::symlink_metadata(fixture.root.path().join("dangling"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
