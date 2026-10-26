//! Exercise the real entry point with redirected stdio: help/errors must never
//! initialize a terminal, start a remote process, or create editor state.
#[path = "../src/cli.rs"]
mod cli;

use std::{
    fs::{self, File},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn run(args: &[&str]) -> (i32, String, String) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let profile = temp.path().join("profile");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&profile).unwrap();
    let stdout = temp.path().join("stdout");
    let stderr = temp.path().join("stderr");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vtcode"))
        .args(args)
        .current_dir(&workspace)
        .env("HOME", &profile)
        .env("USERPROFILE", &profile)
        .env("APPDATA", profile.join("AppData/Roaming"))
        .env("LOCALAPPDATA", profile.join("AppData/Local"))
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("vtcode {args:?} did not exit without a terminal");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = fs::read_to_string(stdout).unwrap();
    let stderr = fs::read_to_string(stderr).unwrap();
    for text in [&stdout, &stderr] {
        assert!(
            !text.contains('\x1b'),
            "terminal escape sequence for {args:?}: {text:?}"
        );
    }
    assert_eq!(
        fs::read_dir(&workspace).unwrap().count(),
        0,
        "workspace writes for {args:?}"
    );
    assert_eq!(
        fs::read_dir(&profile).unwrap().count(),
        0,
        "profile writes for {args:?}"
    );
    (status.code().expect("normal process exit"), stdout, stderr)
}

#[test]
fn help_lists_arguments_without_starting_the_editor() {
    let mut expected = Vec::new();
    cli::write_help(&mut expected).unwrap();
    let expected = String::from_utf8(expected).unwrap();
    for args in [vec!["--help"], vec!["-h"], vec!["file.rs", "--help"]] {
        let (status, stdout, stderr) = run(&args);
        assert_eq!(status, 0, "{stderr}");
        assert_eq!(stdout, expected);
        assert!(stderr.is_empty(), "{stderr}");
        for argument in [
            "FILE:LINE[:COLUMN]",
            "DIRECTORY",
            "--install-theme",
            "--headless",
            "--remote HOST",
            "--remote-command",
            "--ssh-command",
            "--version",
            "--help",
        ] {
            assert!(stdout.contains(argument), "missing help for {argument}");
        }
    }
}

#[test]
fn command_help_never_launches_a_service_or_connection() {
    for args in [
        vec!["--headless", "--help"],
        vec!["--headless", "not-a-directory", "--help"],
        vec!["--install-theme", "--help"],
        vec!["--remote", "--help"],
        vec!["--remote", "user@host", "-h"],
        vec![
            "--remote",
            "user@host",
            "--ssh-command",
            "program-that-must-never-launch",
            "--help",
        ],
        vec![
            "--remote",
            "user@host",
            "--remote-command",
            "vtcode --headless --help",
            "--help",
        ],
    ] {
        let (status, stdout, stderr) = run(&args);
        assert_eq!(status, 0, "{args:?}: {stderr}");
        assert!(stdout.contains("Usage:"));
        assert!(stderr.is_empty(), "{stderr}");
    }
}

#[test]
fn invalid_arguments_exit_with_an_actionable_error_before_ui_startup() {
    for (args, error) in [
        (vec!["--bad-argument"], "unknown option"),
        (vec!["-x"], "unknown option"),
        (vec!["file.rs", "--bad-argument"], "unknown option"),
        (vec!["--help", "--bad-argument"], "unknown option"),
        (vec!["--headless", "--bad-argument"], "unknown option"),
        (vec!["--install-theme"], "missing SOURCE"),
        (vec!["--install-theme", "--bad-argument"], "unknown option"),
        (vec!["--remote"], "missing HOST"),
        (vec!["--remote", "--bad-argument"], "unknown option"),
        (vec!["--remote", "--"], "missing HOST"),
        (vec!["--remote=--bad-host"], "invalid HOST"),
        (
            vec!["--remote", "host", "--remote-command"],
            "missing value",
        ),
        (
            vec!["--remote", "host", "--ssh-command", "ssh 'unclosed"],
            "quoting",
        ),
        (
            vec!["--remote-command", "vtcode --headless"],
            "require --remote",
        ),
        (vec!["--ssh-command", "ssh"], "require --remote"),
        (vec!["--remote", "host", "path"], "remote folder"),
        (vec!["remote", "ssh"], "unexpected argument"),
        (vec!["file.rs", "extra.rs"], "unexpected argument"),
        (vec!["--headless", ".", "extra"], "unexpected argument"),
        (
            vec!["--install-theme", "theme.json", "extra"],
            "unexpected argument",
        ),
        (
            vec!["--headless", "--install-theme", "theme.json"],
            "cannot be combined",
        ),
    ] {
        let (status, stdout, stderr) = run(&args);
        assert_eq!(status, 2, "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(stderr.starts_with("error:"), "{stderr}");
        assert!(stderr.contains(error), "{stderr}");
        assert!(stderr.contains("vtcode --help"), "{stderr}");
    }
}

#[test]
fn runtime_startup_errors_are_plain_messages_with_a_failure_exit_status() {
    let (status, stdout, stderr) = run(&["--headless", "missing-remote-workspace"]);
    assert_eq!(status, 1);
    assert!(stdout.is_empty());
    assert!(stderr.starts_with("error:"), "{stderr}");
    assert!(!stderr.contains("Custom {"), "{stderr}");
    assert!(!stderr.contains("Os {"), "{stderr}");
}

#[test]
fn version_exits_without_starting_the_editor() {
    for args in [vec!["--version"], vec!["-V"]] {
        let (status, stdout, stderr) = run(&args);
        assert_eq!(status, 0, "{stderr}");
        assert_eq!(
            stdout.trim(),
            format!("vtcode {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(stderr.is_empty(), "{stderr}");
    }
}

#[cfg(windows)]
#[test]
fn invalid_internal_arguments_fail_before_opening_console_pipes() {
    let (status, stdout, stderr) = run(&["--headless-console-host"]);
    assert_eq!(status, 2);
    assert!(stdout.is_empty());
    assert!(stderr.contains("invalid internal console host arguments"));
}

#[cfg(unix)]
#[test]
fn windows_only_internal_option_is_an_unknown_option() {
    let (status, stdout, stderr) = run(&["--headless-console-host"]);
    assert_eq!(status, 2);
    assert!(stdout.is_empty());
    assert!(stderr.contains("unknown option"));
}
