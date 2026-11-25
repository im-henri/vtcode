use super::*;

fn local(root: &Path, active: usize, count: usize) -> Entry {
    let base = crate::portable::base_dir();
    Entry {
        scope: Scope::Local,
        roots: (0..count)
            .map(|index| Location::local(&root.join(index.to_string()), &base))
            .collect(),
        active,
    }
}

#[test]
fn recording_one_scope_keeps_the_others() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("last.json");
    assert_eq!(load(&path, &Scope::Local), None);
    let remote = Entry {
        scope: Scope::Remote("host".into()),
        roots: vec![Location::Remote {
            uri: "file:///srv/project/".into(),
        }],
        active: 0,
    };
    record(&path, &[local(temp.path(), 1, 2), remote.clone()]).unwrap();
    record(&path, &[local(temp.path(), 0, 3)]).unwrap();
    assert_eq!(load(&path, &Scope::Local), Some(local(temp.path(), 0, 3)));
    assert_eq!(load(&path, &remote.scope), Some(remote.clone()));

    // Closing every folder forgets them for that scope only.
    record(&path, &[local(temp.path(), 0, 0)]).unwrap();
    assert_eq!(load(&path, &Scope::Local), None);
    assert_eq!(load(&path, &remote.scope), Some(remote));
}

#[test]
fn malformed_or_mismatched_records_reopen_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("last.json");
    std::fs::write(&path, "{not json").unwrap();
    assert_eq!(load(&path, &Scope::Local), None);
    // A broken record is replaced rather than blocking future launches.
    record(&path, &[local(temp.path(), 0, 1)]).unwrap();
    assert!(load(&path, &Scope::Local).is_some());

    let mut wrong = local(temp.path(), 5, 1);
    record(&path, std::slice::from_ref(&wrong)).unwrap();
    assert_eq!(load(&path, &Scope::Local), None);
    wrong.active = 0;
    wrong.scope = Scope::Remote("host".into());
    record(&path, &[wrong.clone()]).unwrap();
    assert_eq!(load(&path, &wrong.scope), None);
}

#[test]
fn recording_folders_preserves_a_newer_file_format() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("last.json");
    let future = r#"{"version":2,"entries":[]}"#;
    std::fs::write(&path, future).unwrap();
    let error = record(&path, &[local(temp.path(), 0, 1)]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(std::fs::read_to_string(path).unwrap(), future);
}
