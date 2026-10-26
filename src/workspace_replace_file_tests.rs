use super::*;

fn options(query: &str, replacement: &str, regex: bool) -> Options {
    Options {
        query: query.into(),
        replacement: replacement.into(),
        case_sensitive: true,
        regex,
        whole_word: false,
        preserve_case: false,
    }
}

fn encoded(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "UTF-16 LE" => [
            vec![0xff, 0xfe],
            text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ]
        .concat(),
        "UTF-16 BE" => [
            vec![0xfe, 0xff],
            text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        ]
        .concat(),
        "UTF-8 with BOM" => [vec![0xef, 0xbb, 0xbf], text.as_bytes().to_vec()].concat(),
        _ => text.as_bytes().to_vec(),
    }
}

#[test]
fn workspace_replace_file_preserves_mixed_newlines_boms_encodings_and_unrelated_bytes() {
    let root = tempfile::tempdir().unwrap();
    let source = "foo\r\nkeep λ\nfoo\runtouched😀";
    let expected = "猫\r\nkeep λ\n猫\runtouched😀";
    let options = options("foo", "猫", false);
    let program = SnapshotReplacer::workspace(&options).unwrap();
    for encoding in ["UTF-8", "UTF-8 with BOM", "UTF-16 LE", "UTF-16 BE"] {
        let path = root.path().join("source");
        std::fs::write(&path, encoded(source, encoding)).unwrap();
        let result = file(
            &path,
            vec![(1, 0, 3), (3, 0, 3)],
            &program,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(result, (2, true), "{encoding}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            encoded(expected, encoding),
            "{encoding}"
        );
    }
}

#[test]
fn workspace_replace_file_rejects_stale_overlapping_invalid_and_duplicate_empty_ranges_atomically()
{
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    for (query, source, matches) in [
        ("(foo)", "bar\nfoo\n", vec![(1, 0, 3), (2, 0, 3)]),
        ("(foo)", "foobar", vec![(1, 0, 6)]),
        ("(foo)", "foo", vec![(1, 0, 3), (1, 1, 3)]),
        ("(foo)", "foo", vec![(0, 0, 3)]),
        ("(foo)", "foo", vec![(1, usize::MAX, usize::MAX)]),
        ("(?=foo)", "foo", vec![(1, 0, 0), (1, 0, 0)]),
    ] {
        std::fs::write(&path, source).unwrap();
        let options = options(query, "$1!", true);
        let program = SnapshotReplacer::workspace(&options).unwrap();
        assert!(
            file(&path, matches, &program, &CancellationToken::new()).is_err(),
            "{query}, {source}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
}

#[test]
fn workspace_replace_file_rejects_nontext_and_readonly_and_verifies_noops_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let options = options("foo", "bar", false);
    let program = SnapshotReplacer::workspace(&options).unwrap();
    for data in [
        b"foo\0tail".as_slice(),
        b"foo\xfftail",
        b"\xef\xbb\xbffoo\xfftail",
        b"\xff\xfe\x66\0\x6f\0\x6f\0\0\xd8",
    ] {
        std::fs::write(&path, data).unwrap();
        assert!(file(&path, vec![(1, 0, 3)], &program, &CancellationToken::new()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), data);
    }
    std::fs::write(&path, "foo\n").unwrap();
    let original = std::fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&path, readonly).unwrap();
    let result = file(&path, vec![(1, 0, 3)], &program, &CancellationToken::new());
    std::fs::set_permissions(&path, original).unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"foo\n");
    let options = self::options("foo", "foo", false);
    let program = SnapshotReplacer::workspace(&options).unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        file(&path, vec![(1, 0, 3)], &program, &CancellationToken::new()).unwrap(),
        (1, false)
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        file(
            &root.path().join("missing"),
            vec![(1, 0, 3)],
            &program,
            &cancel
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn workspace_replace_compiled_program_keeps_capture_lookaround_and_zero_width_behavior() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    for (source, query, replacement, range, expected) in [
        ("xfoo!", "(?<=x)(foo)(?=!)", "$1猫", (1, 1, 4), "xfoo猫!"),
        (
            "foo\r\nbar\nkeep",
            "(foo)\\n(bar)",
            "$2\\n$1",
            (1, 0, 8),
            "bar\r\nfoo\nkeep",
        ),
        ("foo", "(?=(foo))", "$1!", (1, 0, 0), "foo!foo"),
    ] {
        std::fs::write(&path, source).unwrap();
        let options = options(query, replacement, true);
        let program = SnapshotReplacer::workspace(&options).unwrap();
        assert_eq!(
            file(&path, vec![range], &program, &CancellationToken::new()).unwrap(),
            (1, true)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected, "{query}");
    }
}

#[test]
fn workspace_replace_static_templates_keep_preserve_case_literals_and_inserted_newlines() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    for regex in [false, true] {
        std::fs::write(&path, "Foo foo FOO").unwrap();
        let options = Options {
            case_sensitive: false,
            preserve_case: true,
            ..options("foo", "bar", regex)
        };
        let program = SnapshotReplacer::workspace(&options).unwrap();
        file(
            &path,
            vec![(1, 0, 3), (1, 4, 7), (1, 8, 11)],
            &program,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Bar bar BAR");
    }
    for (regex, replacement, expected) in [
        (false, "$1", "$1\r\nlast\n"),
        (true, r"bar\nbaz", "bar\r\nbaz\r\nlast\n"),
    ] {
        std::fs::write(&path, "foo\r\nlast\n").unwrap();
        let options = options("foo", replacement, regex);
        let program = SnapshotReplacer::workspace(&options).unwrap();
        file(&path, vec![(1, 0, 3)], &program, &CancellationToken::new()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }
}
