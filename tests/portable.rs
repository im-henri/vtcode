use std::path::Path;
use std::process::Command;

fn install(binary: &Path, cwd: &Path, profile: &Path, source: &Path) -> std::process::Output {
    Command::new(binary)
        .arg("--install-theme")
        .arg(source)
        .current_dir(cwd)
        .env("HOME", profile)
        .env("USERPROFILE", profile)
        .env("APPDATA", profile.join("AppData/Roaming"))
        .env("LOCALAPPDATA", profile.join("AppData/Local"))
        .env("TMPDIR", profile.join("tmp"))
        .env("TMP", profile.join("tmp"))
        .env("TEMP", profile.join("tmp"))
        .output()
        .unwrap()
}

#[test]
fn theme_installation_follows_a_relocated_executable_and_never_falls_back() {
    let temp = tempfile::tempdir().unwrap();
    let portable = temp.path().join("portable copy");
    let cwd = temp.path().join("unrelated workspace");
    let profile = temp.path().join("user profile");
    for dir in [&portable, &cwd, &profile] {
        std::fs::create_dir(dir).unwrap();
    }
    let source = temp.path().join("theme.json");
    std::fs::write(
        &source,
        r##"{"name":"Portable Test","colors":{"editor.background":"#123456"}}"##,
    )
    .unwrap();
    let filename = Path::new(env!("CARGO_BIN_EXE_vtcode")).file_name().unwrap();
    let binary = portable.join(filename);
    std::fs::copy(env!("CARGO_BIN_EXE_vtcode"), &binary).unwrap();
    // Invalid/unsupported links must fail before creating portable data or
    // attempting a download. Valid vscodethemes.com theme links are supported.
    for url in [
        "https://vscodethemes.com/e/not-an-extension/dark",
        "https://github.com/example/theme",
        "https://example.com/theme.vsix",
    ] {
        let output = install(&binary, &cwd, &profile, Path::new(url));
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Use a theme link"), "{error}");
        assert!(!portable.join(".vtcode").exists());
    }
    let output = install(&binary, &cwd, &profile, &source);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(portable.join(".vtcode/theme-extensions").is_dir());
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&profile).unwrap().count(), 0);
    assert!(
        !std::fs::read_dir(portable.join(".vtcode/theme-extensions"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".install-"))
    );

    let moved = temp.path().join("moved copy");
    std::fs::rename(&portable, &moved).unwrap();
    let output = install(&moved.join(filename), &cwd, &profile, &source);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!portable.exists());
    assert!(moved.join(".vtcode/theme-extensions").is_dir());

    // An unavailable portable data directory is an error, never a reason to
    // write into the profile, current workspace, or system temporary folder.
    let blocked = temp.path().join("blocked copy");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::copy(moved.join(filename), blocked.join(filename)).unwrap();
    std::fs::write(blocked.join(".vtcode"), "blocking file").unwrap();
    let output = install(&blocked.join(filename), &cwd, &profile, &source);
    assert!(!output.status.success());
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&profile).unwrap().count(), 0);
}
