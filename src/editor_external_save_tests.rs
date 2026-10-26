use super::{Editor, write_background_save};
use std::{
    fs,
    time::{Duration, UNIX_EPOCH},
};

fn open(path: &std::path::Path, text: &str) -> Editor {
    fs::write(path, text).unwrap();
    let mut editor = Editor::new();
    editor.open_file(path).unwrap();
    editor
}

#[test]
fn external_save_conflict_preserves_disk_buffer_selection_and_undo_redo() {
    for background in [false, true] {
        for external in ["different\n", "", "original\nextra\n"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("file.txt");
            let mut editor = open(&path, "original\n");
            editor.insert_char('X');
            editor.break_undo_group();
            editor.insert_char('Y');
            editor.undo(); // A failed save must keep the redo branch as well.
            editor.selection_anchor = Some((0, 0));
            let text = editor.rope.clone();
            let version = editor.content_version;
            let cursor = (editor.cursor_line, editor.cursor_col);
            let request = editor.background_save_request(true, true, false).unwrap();
            fs::write(&path, external).unwrap();
            let error = if background {
                write_background_save(&request).unwrap_err()
            } else {
                editor.save_file_with_options(true, true).unwrap_err()
            };
            assert!(error.to_string().contains("changed on disk"));
            assert_eq!(fs::read_to_string(&path).unwrap(), external);
            assert!(editor.rope.is_instance(&text));
            assert_eq!(editor.content_version, version);
            assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
            assert_eq!(editor.selection_anchor, Some((0, 0)));
            assert!(editor.modified && editor.can_undo() && editor.can_redo());
            editor.redo();
            assert_eq!(editor.rope, "XYoriginal\n");
            editor.undo();
            editor.undo();
            assert_eq!(editor.rope, "original\n");
            assert_eq!(
                fs::read_dir(dir.path()).unwrap().count(),
                1,
                "no save temp file leaks"
            );
        }
    }
}

#[test]
fn external_save_checks_bytes_even_when_size_and_timestamp_match() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut editor = open(&path, "original\n");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    editor.insert_char('X');
    fs::write(&path, "external\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert!(editor.save_file_with_options(false, false).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");
}

#[test]
fn external_save_allows_metadata_only_changes_and_identical_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut editor = open(&path, "original\n");
    editor.insert_char('X');
    fs::remove_file(&path).unwrap();
    fs::write(&path, "original\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(100)))
        .unwrap();
    editor.save_file_with_options(false, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "Xoriginal\n");
    assert!(!editor.modified);
    editor.undo();
    assert!(editor.modified);
    editor.redo();
    assert!(!editor.modified);
}

#[test]
fn external_save_refuses_deleted_files_without_recreating_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut editor = open(&path, "original\n");
    editor.insert_char('X');
    fs::remove_file(&path).unwrap();
    assert!(editor.save_file_with_options(false, false).is_err());
    assert!(!path.exists());
    assert_eq!(editor.rope, "Xoriginal\n");
    assert!(editor.modified);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn external_save_checks_encoding_and_line_endings_as_well_as_text() {
    for external in [b"\xEF\xBB\xBFtext\n".as_slice(), b"text\r\n", b"text\r"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let mut editor = open(&path, "text\n");
        editor.insert_char('X');
        fs::write(&path, external).unwrap();
        assert!(editor.save_file_with_options(false, false).is_err());
        assert_eq!(fs::read(&path).unwrap(), external);
    }
}

#[test]
fn external_save_baseline_round_trips_all_encodings_and_mixed_line_endings() {
    for encoding in [
        "UTF-8",
        "UTF-8 with BOM",
        "UTF-16 LE",
        "UTF-16 BE",
        "ASCII",
        "ISO-8859-1",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let text = match encoding {
            "ASCII" => "one\n\r\nthree\rfour\t",
            "ISO-8859-1" => "café\n\r\ntrois\rquatre\t",
            _ => "café界😀\n\r\nthree\rfour\t",
        }
        .repeat(8000); // spans both Rope chunks and the comparison read buffer
        let mut editor = Editor::test_with_text(&text);
        editor.file_path = Some(path.clone());
        editor.encoding = encoding;
        editor.save_file_with_options(false, false).unwrap();
        editor.insert_char('X');
        editor.encoding = "UTF-8"; // compare using the old encoding, write using the new one
        editor.save_file_with_options(false, false).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("X{text}"),
            "{encoding}"
        );
        editor.undo();
        assert!(editor.modified);
    }
}

#[test]
fn external_save_empty_document_baseline_is_not_a_missing_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut editor = open(&path, "");
    editor.insert_char('X');
    editor.save_file_with_options(false, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "X");
}

#[test]
fn external_save_reload_updates_the_checkpoint_without_losing_unsaved_edit_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut editor = open(&path, "original\n");
    editor.insert_char('X');
    fs::write(&path, "external\n").unwrap();
    assert!(editor.save_file_with_options(false, false).is_err());
    editor.reload_from_disk().unwrap();
    assert_eq!(editor.rope, "external\n");
    assert!(!editor.modified);
    editor.undo();
    assert_eq!(editor.rope, "Xoriginal\n");
    assert!(editor.modified);
    editor.redo();
    assert_eq!(editor.rope, "external\n");
    assert!(!editor.modified);
    editor.insert_char('Y');
    editor.save_file_with_options(false, false).unwrap();
    assert!(!editor.modified);
    assert_eq!(fs::read_to_string(&path).unwrap(), editor.rope.to_string());
}

#[test]
fn external_save_new_document_cannot_clobber_a_newly_created_destination() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut editor = Editor::test_with_text("my new text");
    editor.file_path = Some(path.clone());
    let request = editor.background_save_request(false, false, true).unwrap();
    fs::write(&path, "created elsewhere").unwrap();
    assert!(write_background_save(&request).is_err());
    // Retrying ordinary Save must not turn a failed Save As into overwrite permission.
    assert!(editor.save_file_with_options(false, false).is_err());
    assert!(
        write_background_save(&editor.background_save_request(false, false, false).unwrap())
            .is_err()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "created elsewhere");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn external_save_replacement_lock_cannot_fall_back_to_truncating_the_original() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let mut editor = open(&path, "original\n");
    editor.insert_char('X');
    // A reader permits reads/writes but prevents replacing or deleting the file.
    let reader = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(&path)
        .unwrap();
    assert!(editor.save_file_with_options(false, false).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
    assert_eq!(editor.rope, "Xoriginal\n");
    assert!(editor.modified && editor.can_undo());
    drop(reader);
    editor.save_file_with_options(false, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "Xoriginal\n");
}

#[cfg(windows)]
#[test]
fn external_save_new_files_remain_replaceable_during_concurrent_reads() {
    use std::{io::{self, Read}, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread};
    let directory = tempfile::tempdir().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4).map(|_| {
        let root = directory.path().to_path_buf();
        let stop = stop.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(entries) = fs::read_dir(&root) {
                    for entry in entries.flatten() {
                        if let Ok(mut file) = fs::File::open(entry.path()) {
                            let mut bytes = [0; 32];
                            let _ = file.read(&mut bytes);
                        }
                    }
                }
            }
        })
    }).collect();
    // Read both the destination and temporary names, as concurrent indexers,
    // watchers and other applications can. Stop/join even on an I/O failure.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        for index in 0..512 {
            let path = directory.path().join(format!("file-{index}.txt"));
            let mut editor = Editor::test_with_text("original\n");
            editor.file_path = Some(path.clone());
            editor.save_file_with_options(false, false)?;
            editor.insert_char('X');
            if let Err(error) = editor.save_file_with_options(false, false) {
                assert_eq!(fs::read_to_string(&path)?, "original\n");
                assert_eq!(editor.rope, "Xoriginal\n");
                assert!(editor.modified && editor.can_undo());
                return Err(io::Error::new(error.kind(), format!("resaving file {index}: {error}")));
            }
            assert_eq!(fs::read_to_string(&path)?, "Xoriginal\n");
            assert!(!editor.modified);
            fs::remove_file(path)?;
        }
        Ok(())
    }));
    stop.store(true, Ordering::Relaxed);
    for reader in readers { reader.join().unwrap(); }
    result.unwrap().unwrap();
}
