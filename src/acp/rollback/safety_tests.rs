use super::*;
use std::io::Write;

#[test]
fn accepted_checkpoints_remain_reversible_after_git_gc_and_restart() {
    let mut f = Fixture::new();
    f.write("a", "preexisting user work\r\n");
    let checkpoint = f.begin();
    f.write("a", "agent work\r\n");
    f.finish();
    f.journal.accept("agent", "chat", &Selection::All).unwrap();
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root.path())
        .arg("-c")
        .arg(format!("safe.directory={}", root.path().display()))
        .args(["gc", "--prune=now"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    let plan = journal
        .plan("agent", "chat", &Selection::Checkpoint(checkpoint))
        .unwrap();
    journal.apply(plan).unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "preexisting user work\r\n"
    );
    let undo = journal.undo_plan("agent", "chat").unwrap();
    journal.apply(undo).unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "agent work\r\n"
    );
}

fn prepared(f: &mut Fixture) -> Plan {
    prepared_named(f, "test")
}

fn prepared_named(f: &mut Fixture, name: &str) -> Plan {
    let mut plan = f.journal.plan("agent", "chat", &Selection::All).unwrap();
    for (i, item) in plan.replacements.iter_mut().enumerate() {
        if item.before.is_some() {
            item.backup = Some(
                item.path
                    .with_file_name(format!(".vtcode-rollback-{name}-{i}")),
            );
        }
    }
    let mut state = f.journal.state.clone();
    state.transaction = Some(plan.clone());
    f.journal.save(state).unwrap();
    plan
}

fn interrupted(f: &mut Fixture) -> Plan {
    let plan = prepared(f);
    for item in &plan.replacements {
        f.journal.replace(item).unwrap();
    }
    plan
}

#[test]
fn recovery_can_resume_after_restoring_a_racing_original() {
    let mut f = Fixture::new();
    for name in ["a", "z"] {
        f.write(name, "original");
    }
    f.begin();
    for name in ["a", "z"] {
        f.write(name, "agent");
    }
    f.finish();
    let plan = interrupted(&mut f);
    let backup = plan.replacements[0].backup.as_ref().unwrap();
    f.write(backup.to_str().unwrap(), "external writer's final bytes");
    // Recovery restored the actual displaced original, then the process died
    // before it could finish the second file or commit the recovery record.
    f.write("a", "external writer's final bytes");
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    journal.recover().unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "external writer's final bytes"
    );
    assert_eq!(fs::read_to_string(root.path().join("z")).unwrap(), "agent");
    assert!(!journal.interrupted());
}

#[test]
fn recovery_survives_a_restart_at_every_displace_and_install_boundary() {
    let mut f = Fixture::new();
    f.write("a-modified", "original");
    f.write("b-deleted", "deleted by agent");
    fs::write(f.root.path().join("d-binary"), [0, 128, 1]).unwrap();
    f.begin();
    f.write("a-modified", "agent");
    fs::remove_file(f.root.path().join("b-deleted")).unwrap();
    f.write("c-created", "created by agent");
    fs::write(f.root.path().join("d-binary"), [0, 255, 2]).unwrap();
    f.finish();
    let baseline = f.journal.state.clone();
    for stop in 0..=6 {
        // Reuse the immutable captured history, but restore its review state
        // and use fresh backup paths for every interrupted operation.
        f.journal.save(baseline.clone()).unwrap();
        let plan = prepared_named(&mut f, &format!("test-{stop}"));
        let mut step = 0;
        for item in &plan.replacements {
            if let Some(backup) = &item.backup {
                if step == stop {
                    break;
                }
                fs::rename(f.root.path().join(&item.path), f.root.path().join(backup)).unwrap();
                step += 1;
            }
            if let Some(after) = &item.after {
                if step == stop {
                    break;
                }
                f.journal.install(&item.path, after).unwrap();
                step += 1;
            }
        }
        let Fixture {
            root,
            data,
            journal,
        } = f;
        drop(journal);
        let mut journal = Journal::open(root.path(), data.path()).unwrap();
        journal.recover().unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("a-modified")).unwrap(),
            "agent",
            "step {stop}"
        );
        assert!(!root.path().join("b-deleted").exists(), "step {stop}");
        assert_eq!(
            fs::read_to_string(root.path().join("c-created")).unwrap(),
            "created by agent",
            "step {stop}"
        );
        assert_eq!(
            fs::read(root.path().join("d-binary")).unwrap(),
            [0, 255, 2],
            "step {stop}"
        );
        assert!(!journal.interrupted());
        assert!(
            journal
                .summary("agent", "chat")
                .edits
                .iter()
                .all(|e| e.pending() > 0)
        );
        let plan = journal.plan("agent", "chat", &Selection::All).unwrap();
        journal.apply(plan).unwrap();
        let undo = journal.undo_plan("agent", "chat").unwrap();
        journal.apply(undo).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("c-created")).unwrap(),
            "created by agent"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("a-modified")).unwrap(),
            "agent"
        );
        assert!(!root.path().join("b-deleted").exists());
        assert_eq!(fs::read(root.path().join("d-binary")).unwrap(), [0, 255, 2]);
        f = Fixture {
            root,
            data,
            journal,
        };
    }
}

#[cfg(any(unix, windows))]
#[test]
fn replaced_workspace_root_cannot_redirect_checkpoint_access() {
    let holder = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), "outside work").unwrap();
    let link = holder.path().join("workspace");
    fs::create_dir(&link).unwrap();
    let root = link.canonicalize().unwrap();
    fs::remove_dir(&link).unwrap();
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    let result = storage::checked_path(&root, Path::new("keep"));
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
    assert!(
        result.is_err(),
        "a replaced root must not access the link target"
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("keep")).unwrap(),
        "outside work"
    );
}

#[test]
fn successful_restore_retains_original_for_a_writer_with_an_open_handle() {
    let mut f = Fixture::new();
    f.write("a", "original\n");
    f.begin();
    f.write("a", "agent\n");
    f.finish();
    let mut writer = OpenOptions::new()
        .append(true)
        .open(f.root.path().join("a"))
        .unwrap();
    f.decline(Selection::All).unwrap();
    let backup = f.journal.state.completed.last().unwrap().plan.replacements[0]
        .backup
        .clone()
        .unwrap();
    writer.write_all(b"late external work\n").unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    assert_eq!(f.read("a"), "original\n");
    assert_eq!(
        fs::read_to_string(f.root.path().join(backup)).unwrap(),
        "agent\nlate external work\n"
    );
}

#[test]
fn recovery_preflights_missing_snapshots_before_moving_any_working_file() {
    let mut f = Fixture::new();
    for name in ["a", "z"] {
        f.write(name, &format!("original {name}"));
    }
    f.begin();
    for name in ["a", "z"] {
        f.write(name, &format!("agent {name}"));
    }
    f.finish();
    let plan = interrupted(&mut f);
    let last = plan.replacements.last().unwrap();
    fs::remove_file(f.root.path().join(last.backup.as_ref().unwrap())).unwrap();
    let hash = &last.before.as_ref().unwrap().blob.hash;
    let object = f
        .journal
        .repository
        .join("objects")
        .join(&hash[..2])
        .join(&hash[2..]);
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(&object).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&object, permissions).unwrap();
    }
    fs::remove_file(object).unwrap();
    assert!(f.journal.recover().is_err());
    assert!(f.journal.interrupted());
    assert_eq!(f.read("a"), "original a");
    assert_eq!(f.read("z"), "original z");
    assert_eq!(
        f.read(
            plan.replacements[0]
                .backup
                .as_ref()
                .unwrap()
                .to_str()
                .unwrap()
        ),
        "agent a"
    );
}

#[test]
fn recovery_preflights_readonly_files_before_moving_any_working_file() {
    let mut f = Fixture::new();
    for name in ["a", "z"] {
        f.write(name, &format!("original {name}"));
    }
    f.begin();
    for name in ["a", "z"] {
        f.write(name, &format!("agent {name}"));
    }
    f.finish();
    interrupted(&mut f);
    let path = f.root.path().join("z");
    let permissions = fs::metadata(&path).unwrap().permissions();
    let mut readonly = permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    let result = f.journal.recover();
    fs::set_permissions(&path, permissions).unwrap();
    assert!(result.is_err());
    assert_eq!(f.read("a"), "original a");
    assert_eq!(f.read("z"), "original z");
}

#[cfg(windows)]
#[test]
fn user_protection_matches_case_aliases_of_captured_paths() {
    let mut f = Fixture::new();
    f.write("MixedCase.txt", "original");
    f.begin();
    f.write("MixedCase.txt", "user and agent work");
    f.journal
        .finish(BTreeSet::from([PathBuf::from("mixedcase.TXT")]))
        .unwrap();
    assert!(f.decline(Selection::All).is_err());
    assert_eq!(f.read("MixedCase.txt"), "user and agent work");
}

#[test]
fn a_new_nested_repository_is_not_modified_by_old_checkpoints() {
    let mut f = Fixture::new();
    fs::create_dir(f.root.path().join("nested")).unwrap();
    f.write("nested/a", "original");
    f.begin();
    f.write("nested/a", "agent");
    f.finish();
    git2::Repository::init(f.root.path().join("nested")).unwrap();
    assert!(f.decline(Selection::All).is_err());
    assert_eq!(f.read("nested/a"), "agent");
}
