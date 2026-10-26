use super::*;
use crate::{editor::Editor, settings::Settings};

#[test]
fn formatter_source_writes_are_batched_and_flush_failures_are_reported() {
    #[derive(Default)]
    struct Sink { bytes: Vec<u8>, writes: usize, fail: bool }
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail { return Err(std::io::Error::other("write failed")); }
            self.writes += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let text = "variable = 'é😀'\r\n".repeat(100_000);
    let source = ropey::Rope::from_str(&text);
    let mut sink = Sink::default();
    write_source(&source, &mut sink).unwrap();
    assert_eq!(sink.bytes, text.as_bytes());
    assert!(sink.writes * 16 < source.chunks().count(), "{} writes for {} rope chunks", sink.writes, source.chunks().count());
    // This fits entirely in the buffer: errors must surface on the final flush.
    sink.fail = true;
    assert!(write_source(&ropey::Rope::from_str("small"), &mut sink).is_err());
}

#[test]
#[ignore = "formatter performance probe; set VTCODE_BLACK to benchmark the installed executable"]
fn benchmark_external_formatter_pipeline() {
    let command = std::env::var("VTCODE_BLACK").expect("set VTCODE_BLACK");
    let dir = tempfile::tempdir().unwrap();
    for lines in [20, 1_000, 10_000] {
        let source: String = (0..lines).map(|i| format!("value_{i}={{'first':{i},'second':[{i},2,3]}}\n")).collect();
        let path = dir.path().join("example.py");
        std::fs::write(&path, &source).unwrap();
        let started = Instant::now();
        let result = std::process::Command::new(&command).arg(&path).output().unwrap();
        let direct = started.elapsed();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let formatted = std::fs::read_to_string(&path).unwrap();
        let mut editor = Editor::test_with_text(&source);
        let started = Instant::now();
        let changes = prepare(&editor.rope, &formatted);
        let prepare_time = started.elapsed();
        let started = Instant::now();
        changes.apply(&mut editor).unwrap();
        let apply = started.elapsed();
        assert_eq!(editor.rope.to_string(), formatted);
        let config = Config {
            provider: Provider::Command, mode: CommandMode::File, command: command.clone(),
            args: vec!["{file}".into()],
        };
        let started = Instant::now();
        let changes = run(&config, &path, dir.path(), &ropey::Rope::from_str(&source), None,
            Arc::new(AtomicBool::new(false)), Duration::from_secs(60)).unwrap();
        let pipeline = started.elapsed();
        eprintln!("{lines} lines: direct formatter {direct:?}; prepare {prepare_time:?}; apply {apply:?}; VTCode pipeline {pipeline:?}; {} edits", changes.len());
    }
}

#[test]
fn formatter_edits_round_trip_unicode_line_endings_and_undo() {
    for (before, after) in [
        ("", "x = 1\n"),
        ("x=1", "x = 1\n"),
        ("x = 1\n", "x = 1\n"),
        ("a\r\nb\r\n", "a\nb\n"),
        ("a\nb\n", "a\r\nb\r\n"),
        ("a\r\nb", "a\r\nb\r\n"),
        ("\r\n\r\n", "\n"),
        ("é='😀'\r\n\r\ny=2\r\n", "é = \"😀\"\r\n\r\ny = 2\r\n"),
        ("a=1\nb = 2\nc=3\n", "a = 1\nb = 2\nc = 3\n"),
        ("trailing\n\n", "trailing\n"),
        ("old\n", ""),
    ] {
        let mut editor = Editor::test_with_text(before);
        let edits = prepare(&editor.rope, after);
        assert_eq!(edits.is_empty(), before == after, "{before:?} -> {after:?}");
        edits.apply(&mut editor).unwrap();
        assert_eq!(editor.rope.to_string(), after, "{before:?} -> {after:?}");
        if before != after {
            editor.undo();
            assert_eq!(editor.rope.to_string(), before);
        }
    }
}

#[test]
fn formatter_minimal_edits_preserve_caret_in_unchanged_regions() {
    let mut editor = Editor::test_with_text("a=1\n# caret here 😀\nc=3\n");
    editor.cursor_line = 1;
    editor.cursor_col = 10;
    let edits = prepare(&editor.rope, "a = 1\n# caret here 😀\nc = 3\n");
    assert_eq!(edits.len(), 2);
    edits.apply(&mut editor).unwrap();
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 10));
}

#[test]
fn formatter_settings_merge_per_language_and_serialize() {
    let user: Settings = serde_json::from_value(serde_json::json!({
        "formatters": {"python":{"provider":"command","command":"user-formatter","args":["--stdin"]}, "cpp":{"provider":"lsp"}}
    }))
    .unwrap();
    let workspace: Settings = serde_json::from_value(serde_json::json!({
        "formatters": {"python":{"provider":"command","mode":"stdin","command":"local formatter","args":["--stdin", "{file}"]}}
    })).unwrap();
    let merged = user.merge(&workspace);
    let map = merged.formatters.as_ref().unwrap();
    assert_eq!(map["python"].provider, Provider::Command);
    assert_eq!(map["python"].mode, CommandMode::Stdin);
    assert_eq!(map["cpp"].provider, Provider::Lsp);
    let decoded: Settings = serde_json::from_value(serde_json::to_value(&merged).unwrap()).unwrap();
    assert_eq!(decoded.formatters, merged.formatters);
    let command = Config { provider: Provider::Command, command: "formatter".into(), args: vec!["{file}".into()], ..Default::default() };
    assert!(validate("python", &command).is_ok());
    assert!(validate("c", &command).is_ok());
    assert!(
        validate(
            "python",
            &Config {
                provider: Provider::Command,
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn external_formatter_selection_fails_before_launching_a_document_formatter() {
    let dir = tempfile::tempdir().unwrap();
    let error = run(
        &Config { provider: Provider::Command, command: "not-an-installed-tool".into(), args: vec![], ..Default::default() },
        &dir.path().join("file.py"), dir.path(), &ropey::Rope::from_str("x=1\n"),
        Some((1, 1)), Arc::new(AtomicBool::new(false)), Duration::from_secs(1),
    ).unwrap_err();
    assert!(error.contains("range formatting"), "{error}");
}

// Test subprocess I/O using the platform shell as an explicit script interpreter.
// Nothing depends on a separately installed formatter or shell interpolation of
// user-supplied arguments.
fn script(windows: &str, unix: &str) -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join(if cfg!(windows) {
        "formatter.ps1"
    } else {
        "formatter.sh"
    });
    std::fs::write(&script, if cfg!(windows) {
        format!("[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)\n[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n{windows}")
    } else { unix.into() }).unwrap();
    let mut args: Vec<String> = if cfg!(windows) {
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
        ]
    } else {
        Vec::new()
    };
    args.push(script.to_string_lossy().into_owned());
    (
        dir,
        Config {
            provider: Provider::Command,
            mode: CommandMode::Stdin,
            command: if cfg!(windows) {
                "powershell.exe"
            } else {
                "sh"
            }
            .into(),
            args,
        },
    )
}

fn execute(
    config: &Config,
    root: &Path,
    text: &str,
    timeout: Duration,
) -> Result<Prepared, String> {
    run(
        config,
        &root.join("unsaved file.py"),
        root,
        &ropey::Rope::from_str(text),
        None,
        Arc::new(AtomicBool::new(false)),
        timeout,
    )
}

#[test]
fn external_formatter_reads_unsaved_stdin_and_expands_literal_arguments() {
    let (dir, mut config) = script(
        "$text = [Console]::In.ReadToEnd(); [Console]::Out.Write($text + '|' + ($args -join '|'))",
        "cat; printf '|%s|%s|%s' \"$1\" \"$2\" \"$3\"",
    );
    config.args.extend([
        "{file}".into(),
        "{workspace}".into(),
        "literal & $value; with spaces".into(),
    ]);
    let mut editor = Editor::test_with_text("unsaved 😀 text");
    let result = execute(
        &config,
        dir.path(),
        &editor.rope.to_string(),
        Duration::from_secs(10),
    )
    .unwrap();
    result.apply(&mut editor).unwrap();
    let text = editor.rope.to_string();
    let parts: Vec<_> = text.split('|').collect();
    assert_eq!(parts[0], "unsaved 😀 text");
    let copy = Path::new(parts[1]);
    assert_eq!(copy.file_name().unwrap(), "unsaved file.py");
    assert_eq!(copy.parent().unwrap().parent().unwrap(), dir.path());
    assert!(is_temporary_path(copy));
    assert!(!copy.parent().unwrap().exists(), "working copy must be cleaned up");
    assert_eq!(parts[2], dir.path().to_string_lossy());
    assert_eq!(parts[3], "literal & $value; with spaces");
    assert!(!dir.path().join("unsaved file.py").exists());
}

#[test]
fn external_formatter_failures_and_empty_output_never_become_edits() {
    for (win, unix, expected) in [
        (
            "[Console]::In.ReadToEnd() | Out-Null; [Console]::Error.Write('bad configuration'); exit 2",
            "cat >/dev/null; printf 'bad configuration' >&2; exit 2",
            "bad configuration",
        ),
        (
            "[Console]::In.ReadToEnd() | Out-Null",
            "cat >/dev/null",
            "returned no text",
        ),
        (
            "[Console]::In.ReadToEnd() | Out-Null; [Console]::OpenStandardOutput().WriteByte(255)",
            "cat >/dev/null; printf '\\377'",
            "UTF-8",
        ),
        (
            "[Console]::In.ReadToEnd() | Out-Null; [Console]::Out.Write(('x' * 2000000))",
            "cat >/dev/null; head -c 2000000 /dev/zero",
            "size limit",
        ),
    ] {
        let (dir, config) = script(win, unix);
        let error = execute(&config, dir.path(), "unchanged", Duration::from_secs(10)).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    let (dir, config) = script("[Console]::In.ReadToEnd() | Out-Null", "cat >/dev/null");
    let edits = execute(&config, dir.path(), " \t\n", Duration::from_secs(10)).unwrap();
    let mut editor = Editor::test_with_text(" \t\n");
    edits.apply(&mut editor).unwrap();
    assert_eq!(
        editor.rope.len_chars(),
        0,
        "formatters may remove whitespace-only documents"
    );
}

#[test]
fn external_formatter_timeout_and_cancellation_release_pipes() {
    let (dir, config) = script("Start-Sleep -Seconds 20", "sleep 20");
    let started = Instant::now();
    assert!(
        execute(&config, dir.path(), "x", Duration::from_millis(400))
            .unwrap_err()
            .contains("did not finish")
    );
    assert!(started.elapsed() < Duration::from_secs(8));
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancel);
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        signal.store(true, Ordering::Release);
    });
    let error = run(
        &config,
        &dir.path().join("file.py"),
        dir.path(),
        &ropey::Rope::from_str("x"),
        None,
        cancel,
        Duration::from_secs(10),
    )
    .unwrap_err();
    trigger.join().unwrap();
    assert!(error.contains("cancelled"));
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
#[ignore = "requires Black; set VTCODE_BLACK to its executable"]
fn real_black_formatter_external_command_and_project_configuration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pyproject.toml"),
        "[tool.black]\nskip-string-normalization = true\n",
    )
    .unwrap();
    let config = Config {
        provider: Provider::Command,
        mode: CommandMode::Stdin,
        command: std::env::var("VTCODE_BLACK").unwrap_or_else(|_| "black".into()),
        args: ["--quiet", "--stdin-filename", "{file}", "-"].map(String::from).into(),
    };
    let source = "first=  '😀'\r\n\r\nsecond=   2\r\n";
    let mut editor = Editor::test_with_text(source);
    let path = dir.path().join("file with spaces.py");
    let changes = run(
        &config,
        &path,
        dir.path(),
        &editor.rope,
        None,
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(30),
    )
    .unwrap();
    changes.apply(&mut editor).unwrap();
    assert_eq!(
        editor.rope.to_string(),
        "first = '😀'\r\n\r\nsecond = 2\r\n"
    );
    assert!(!path.exists());
    editor.undo();
    let file_config = Config {
        mode: CommandMode::File,
        args: vec!["{file}".into()],
        ..config
    };
    let changes = run(
        &file_config, &path, dir.path(), &editor.rope, None,
        Arc::new(AtomicBool::new(false)), Duration::from_secs(30),
    ).unwrap();
    changes.apply(&mut editor).unwrap();
    assert_eq!(editor.rope.to_string(), "first = '😀'\r\n\r\nsecond = 2\r\n");
    editor.undo();
    assert_eq!(editor.rope.to_string(), source);
    assert!(!path.exists());
    assert_no_working_copies(dir.path());
}

#[test]
fn external_formatter_file_modes_require_an_explicit_file_argument() {
    let mut config = Config {
        provider: Provider::Command,
        command: "formatter".into(),
        ..Default::default()
    };
    assert!(validate("python", &config).unwrap_err().contains("No arguments are added automatically"));
    config.args = vec!["--input={file}".into()];
    for mode in [CommandMode::File, CommandMode::FileStdout, CommandMode::Stdin] {
        config.mode = mode;
        assert!(validate("python", &config).is_ok());
        let encoded = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&encoded).unwrap(), config);
    }
    config.args.clear();
    assert!(validate("python", &config).is_ok(), "stdin need not receive a filename");
    config.mode = CommandMode::FileStdout;
    assert!(validate("python", &config).is_err());
}

fn assert_no_working_copies(root: &Path) {
    assert!(!std::fs::read_dir(root).unwrap().any(|entry| {
        is_temporary_path(&entry.unwrap().path())
    }), "formatter working copies were not removed");
}

#[test]
fn external_formatter_in_place_uses_unsaved_copy_supports_atomic_replace_and_undo() {
    let (dir, mut config) = script(
        r#"$file = $args[0]
if ([IO.Path]::GetFileName($file) -ne 'unsaved file.py') { throw 'filename was changed' }
if ([IO.Directory]::GetParent([IO.Path]::GetDirectoryName($file)).FullName -ne $args[1]) { throw 'wrong workspace argument' }
$text = [IO.File]::ReadAllText($file).Replace('value=', 'value = ')
[IO.File]::WriteAllText($file + '.new', $text, [Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath ($file + '.new') -Destination $file -Force
[Console]::Out.Write('Progress and logs are not document contents')"#,
        r#"test "$(basename "$1")" = "unsaved file.py" || exit 2
test "$(dirname "$(dirname "$1")")" = "$2" || exit 3
sed 's/value=/value = /g' "$1" > "$1.new"
mv "$1.new" "$1"
printf 'Progress and logs are not document contents'"#,
    );
    let root = dir.path().join("space and {workspace}");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("unsaved file.py");
    std::fs::write(&path, "value=1\n").unwrap();
    config.mode = CommandMode::File;
    config.args.extend(["{file}".into(), "{workspace}".into()]);
    let source = "value=2\r\n# unsaved 😀\r\n";
    let mut editor = Editor::test_with_text(source);
    editor.cursor_line = 1;
    editor.cursor_col = 2;
    let changes = execute(&config, &root, source, Duration::from_secs(10)).unwrap();
    changes.apply(&mut editor).unwrap();
    assert_eq!(editor.rope.to_string(), "value = 2\r\n# unsaved 😀\r\n");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value=1\n");
    assert_no_working_copies(&root);
    editor.undo();
    assert_eq!(editor.rope.to_string(), source);
    editor.redo();
    assert_eq!(editor.rope.to_string(), "value = 2\r\n# unsaved 😀\r\n");
}

#[test]
fn external_formatter_file_to_stdout_and_unchanged_in_place_results() {
    let (dir, mut config) = script(
        "[Console]::Out.Write([IO.File]::ReadAllText($args[0]).Replace('value=', 'value = '))",
        r#"sed 's/value=/value = /g' "$1""#,
    );
    config.args.push("{file}".into());
    config.mode = CommandMode::FileStdout;
    let edits = execute(&config, dir.path(), "value=2\n", Duration::from_secs(10)).unwrap();
    let mut editor = Editor::test_with_text("value=2\n");
    edits.apply(&mut editor).unwrap();
    assert_eq!(editor.rope.to_string(), "value = 2\n");
    assert_no_working_copies(dir.path());

    // In-place mode must not mistake log output for the formatted document.
    config.mode = CommandMode::File;
    assert!(execute(&config, dir.path(), "value=2\n", Duration::from_secs(10)).unwrap().is_empty());
    assert_no_working_copies(dir.path());
}

#[test]
fn external_formatter_file_failures_leave_the_original_and_clean_up() {
    for (windows, unix, expected) in [
        ("[IO.File]::WriteAllText($args[0], 'changed'); [Console]::Error.Write('broken tool'); exit 2",
            r#"printf changed > "$1"; printf 'broken tool' >&2; exit 2"#, "broken tool"),
        ("[IO.File]::WriteAllText($args[0], '')", r#": > "$1""#, "emptied"),
        ("[IO.File]::Delete($args[0])", r#"rm "$1""#, "Could not read"),
        ("[IO.File]::WriteAllBytes($args[0], [byte[]]@(255))", r#"printf '\377' > "$1""#, "UTF-8"),
        ("[IO.File]::WriteAllText($args[0], ('x' * 2000000))",
            r#"head -c 2000000 /dev/zero > "$1""#, "size limit"),
    ] {
        let (dir, mut config) = script(windows, unix);
        let path = dir.path().join("unsaved file.py");
        std::fs::write(&path, "on disk").unwrap();
        config.mode = CommandMode::File;
        config.args.push("{file}".into());
        let error = execute(&config, dir.path(), "unsaved text", Duration::from_secs(10)).unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "on disk");
        assert_no_working_copies(dir.path());
    }
    let (dir, mut config) = script("Start-Sleep -Seconds 20", "sleep 20");
    config.mode = CommandMode::File;
    config.args.push("{file}".into());
    assert!(execute(&config, dir.path(), "unsaved text", Duration::from_millis(400))
        .unwrap_err().contains("did not finish"));
    assert_no_working_copies(dir.path());
}

#[test]
fn external_formatter_wrong_stdout_mode_cannot_rewrite_the_original() {
    let (dir, mut config) = script(
        "[Console]::In.ReadToEnd() | Out-Null; [IO.File]::WriteAllText($args[0], 'oops')",
        r#"cat >/dev/null; printf oops > "$1""#,
    );
    let path = dir.path().join("unsaved file.py");
    std::fs::write(&path, "on disk").unwrap();
    config.args.push("{file}".into());
    let error = execute(&config, dir.path(), "unsaved text", Duration::from_secs(10)).unwrap_err();
    assert!(error.contains("Edit file in place"), "{error}");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "on disk");
    assert_no_working_copies(dir.path());
}
