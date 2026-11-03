use super::*;

fn target(name: &str) -> Target {
    Target {
        name: name.into(),
        host: "user@dev-alias".into(),
        ..Default::default()
    }
}

#[test]
fn missing_catalog_is_read_only_and_saved_targets_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".vtcode/remote-targets.json");
    let mut store = Store::load(&path).unwrap();
    assert!(store.targets.is_empty());
    assert!(!path.parent().unwrap().exists());
    let mut first = target("Development λ");
    first.folder = "/work/project with 'quotes' and $variables".into();
    store.save(vec![target("Zebra"), first.clone()]).unwrap();
    let loaded = Store::load(&path).unwrap();
    assert_eq!(loaded.targets[0], first);
    assert_eq!(loaded.find("DEVELOPMENT λ").unwrap(), &first);
    assert!(
        loaded
            .find("missing")
            .unwrap_err()
            .contains("No saved remote target")
    );
    let crate::cli::Command::Remote { args, .. } = first.invocation().unwrap() else {
        panic!()
    };
    let words = shell_words::split(args.last().unwrap().to_str().unwrap()).unwrap();
    assert_eq!(words, ["vtcode", "--headless", "--", &first.folder]);
}

#[test]
fn custom_commands_remain_opaque_and_ssh_prefix_is_tokenized_once() {
    let mut value = target("Job");
    value.folder = "/unused/default-folder".into();
    value.remote_command = "submitcmd vtcode --headless '/project λ' ; literal $HOME".into();
    value.ssh_command = "'/path with spaces/ssh' -e none -tt -p 2222".into();
    let crate::cli::Command::Remote { program, args } = value.invocation().unwrap() else {
        panic!()
    };
    assert_eq!(program, "/path with spaces/ssh");
    assert_eq!(args.last().unwrap().to_str().unwrap(), value.remote_command);
    assert_eq!(args[args.len() - 2].to_str().unwrap(), value.host);
    assert_eq!(args[3], "-p");
    assert_eq!(args[4], "2222");
}

#[test]
fn invalid_and_duplicate_targets_never_replace_the_catalog() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("remote-targets.json");
    let mut store = Store::load(&path).unwrap();
    store.save(vec![target("Original")]).unwrap();
    let original = fs::read(&path).unwrap();
    for mut invalid in [target(""), target(" Leading"), target("line\nbreak")] {
        assert!(store.save(vec![invalid.clone()]).is_err());
        invalid.name = "Valid".into();
        invalid.host = "-oProxyCommand=bad".into();
        assert!(store.save(vec![invalid]).is_err());
    }
    assert!(store.save(vec![target("Same"), target("same")]).is_err());
    let mut invalid = target("Nul");
    invalid.remote_command = "bad\0command".into();
    assert!(store.save(vec![invalid]).is_err());
    let mut invalid = target("SSH");
    invalid.ssh_command = "ssh 'unterminated".into();
    assert!(store.save(vec![invalid]).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn malformed_unsupported_and_unknown_fields_are_never_silently_discarded() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("remote-targets.json");
    for text in [
        "{",
        r#"{"version":2,"targets":[]}"#,
        r#"{"version":1,"targets":[],"future":true}"#,
        r#"{"version":1,"targets":[{"name":"bad","host":"host","typo":true}]}"#,
    ] {
        fs::write(&path, text).unwrap();
        assert!(Store::load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn a_stale_window_cannot_overwrite_another_windows_targets() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("remote-targets.json");
    let mut first = Store::load(&path).unwrap();
    let mut second = Store::load(&path).unwrap();
    first.save(vec![target("First")]).unwrap();
    assert!(
        second
            .save(vec![target("Second")])
            .unwrap_err()
            .contains("changed in another window")
    );
    assert_eq!(Store::load(&path).unwrap().targets, vec![target("First")]);
    fs::write(&path, "malformed external edit").unwrap();
    assert!(first.save(vec![]).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "malformed external edit");
}

#[test]
fn editing_individual_targets_merges_unrelated_changes_and_rejects_stale_edits() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("remote-targets.json");
    let mut first = Store::load(&path).unwrap();
    first.save(vec![target("Alpha"), target("Beta")]).unwrap();
    let mut second = Store::load(&path).unwrap();
    let original = target("Alpha");
    let mut edited = original.clone();
    edited.folder = "~/project".into();
    second.upsert(None, target("Gamma")).unwrap();
    first.upsert(Some(&original), edited.clone()).unwrap();
    assert_eq!(
        first.targets.len(),
        3,
        "Preserve the other window's added target"
    );
    assert_eq!(second.remove(&original), Err(UpdateError::Conflict));
    assert_eq!(
        second.upsert(Some(&original), target("Renamed")),
        Err(UpdateError::Conflict)
    );
    assert_eq!(
        second.upsert(None, target("ALPHA")),
        Err(UpdateError::NameInUse)
    );
    // A stale catalog can still remove an entry that nobody changed.
    second.remove(&target("Beta")).unwrap();
    assert_eq!(
        Store::load(&path).unwrap().targets,
        vec![edited, target("Gamma")]
    );
    let before = fs::read(&path).unwrap();
    fs::write(&path, b"invalid external edit").unwrap();
    assert!(first.upsert(None, target("Delta")).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"invalid external edit");
    assert!(!before.is_empty());
}

#[test]
fn home_folders_expand_only_home_and_keep_the_rest_literal() {
    for (folder, expected) in [
        ("~", "\"$HOME\""),
        ("~/project", "\"$HOME\"/project"),
        ("~/project $USER", "\"$HOME\"/'project $USER'"),
        ("~someone/project", "'~someone/project'"),
    ] {
        let mut value = target("Home");
        value.folder = folder.into();
        let crate::cli::Command::Remote { args, .. } = value.invocation().unwrap() else {
            panic!()
        };
        assert_eq!(
            args.last().unwrap().to_str().unwrap(),
            format!("vtcode --headless -- {expected}")
        );
    }
}
