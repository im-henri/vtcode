use super::*;
use std::{
    fs,
    process::Command,
    sync::{Arc, Barrier},
    time::{Duration, SystemTime},
};

const SCRIPTS: [(&str, &str); 5] = [
    ("bashrc", include_str!("terminal_shell/bash.sh")),
    (".zshenv", include_str!("terminal_shell/zshenv")),
    (".zshrc", include_str!("terminal_shell/zshrc")),
    ("fish.fish", include_str!("terminal_shell/fish.fish")),
    (
        "powershell.ps1",
        include_str!("terminal_shell/powershell.ps1"),
    ),
];

#[test]
fn every_shell_reuses_persistent_files_in_the_portable_version_directory() {
    let directory = crate::portable::data_dir()
        .join("shell-integration")
        .join(env!("CARGO_PKG_VERSION"));
    assert_eq!(storage::directory(), directory);
    for shell in ["bash", "zsh", "fish", "powershell", "pwsh"] {
        let first = Integration::prepare(shell, &[]).unwrap();
        let second = Integration::prepare(shell, &[]).unwrap();
        assert_eq!(first.args, second.args);
        assert_eq!(first.env, second.env);
        match shell {
            "bash" => assert_eq!(Path::new(&first.args[1]), directory.join("bashrc")),
            "zsh" => assert!(
                first
                    .env
                    .contains(&("ZDOTDIR".into(), directory.as_os_str().into()))
            ),
            "fish" => assert_eq!(
                first.args[2],
                OsString::from(fish_command(&directory.join("fish.fish")).unwrap())
            ),
            _ => assert_eq!(
                first.args[3],
                OsString::from(powershell_command(&directory.join("powershell.ps1")).unwrap())
            ),
        }
        drop(first);
        drop(second);
    }
    for (name, expected) in SCRIPTS {
        assert_eq!(fs::read_to_string(directory.join(name)).unwrap(), expected);
    }
}

#[test]
fn cached_scripts_are_reused_without_rewriting_and_incomplete_files_are_repaired() {
    let root = tempfile::tempdir().unwrap();
    let directory = root
        .path()
        .join("portable code's λ/.vtcode/shell-integration/test-version");
    let path = storage::install_in(&directory, "bashrc", "complete hook\n").unwrap();
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(
        fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1234)),
    )
    .unwrap();
    drop(file);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        storage::install_in(&directory, "bashrc", "complete hook\n").unwrap(),
        path
    );
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    fs::write(&path, "partial").unwrap();
    storage::install_in(&directory, "bashrc", "complete hook\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "complete hook\n");
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
}

#[test]
fn simultaneous_shell_starts_publish_complete_shared_scripts() {
    let root = tempfile::tempdir().unwrap();
    let directory = Arc::new(root.path().join(".vtcode/shell-integration/test-version"));
    let body = Arc::new("complete shell hook\n".repeat(8192));
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (directory, body, barrier) = (directory.clone(), body.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..8 {
                    let path = storage::install_in(&directory, "bashrc", &body).unwrap();
                    assert_eq!(fs::read_to_string(path).unwrap(), *body);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(fs::read_dir(&*directory).unwrap().count(), 1);
}

#[test]
fn explicit_commands_do_not_install_hooks_and_storage_errors_allow_plain_shell_fallback() {
    let root = tempfile::tempdir().unwrap();
    let directory = root
        .path()
        .join("unused/.vtcode/shell-integration/test-version");
    for shell in ["bash", "zsh", "fish", "powershell", "pwsh", "custom-shell"] {
        let launch =
            Integration::prepare_in(shell, &["explicit-command".into()], &directory).unwrap();
        assert!(launch.args.is_empty() && launch.env.is_empty());
    }
    let launch = Integration::prepare_in("custom-shell", &[], &directory).unwrap();
    assert!(launch.args.is_empty() && launch.env.is_empty());
    assert!(!directory.exists());
    let blocked = root.path().join("blocked");
    fs::write(&blocked, "not a directory").unwrap();
    for shell in ["bash", "zsh", "fish", "powershell", "pwsh"] {
        assert!(Integration::prepare_in(shell, &[], &blocked).is_err());
        let fallback = Integration::prepare_in(shell, &[], &blocked).unwrap_or_default();
        assert!(fallback.args.is_empty() && fallback.env.is_empty());
    }
}

#[test]
fn shell_versions_use_separate_files_that_survive_other_installations() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join(".vtcode/shell-integration/1.0");
    let second = root.path().join(".vtcode/shell-integration/2.0");
    let first = storage::install_in(&first, "bashrc", "old hook").unwrap();
    let second = storage::install_in(&second, "bashrc", "new hook").unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read_to_string(first).unwrap(), "old hook");
    assert_eq!(fs::read_to_string(second).unwrap(), "new hook");
}

#[test]
fn native_powershell_loads_the_shared_file_from_paths_with_shell_metacharacters() {
    let programs = if cfg!(windows) {
        &["powershell.exe", "pwsh.exe"][..]
    } else {
        &["pwsh"][..]
    };
    let root = tempfile::tempdir().unwrap();
    let directory = root
        .path()
        .join("code's λ $() ` folder/.vtcode/shell-integration/test-version");
    for program in programs {
        if Command::new(program).arg("-Version").output().is_err() {
            continue;
        }
        let launch = Integration::prepare_in(program, &[], &directory).unwrap();
        let loader = launch.args.last().unwrap().to_str().unwrap();
        for constrained in [false, true] {
            let mode = if constrained {
                "$ExecutionContext.SessionState.LanguageMode = 'ConstrainedLanguage'; "
            } else {
                ""
            };
            let check = if constrained {
                "if (Test-Path variable:global:__VTCodeCommand) { exit 2 }"
            } else {
                "if (!(Test-Path variable:global:__VTCodeCommand)) { exit 3 }"
            };
            let command = format!(
                "$ErrorActionPreference = 'Stop'; {mode}{loader}; {check}; Write-Output 'VTCode_Shared_Loaded'"
            );
            let mut process = Command::new(program);
            process.args(["-NoLogo", "-NoProfile", "-NonInteractive"]);
            #[cfg(windows)]
            process.args(["-ExecutionPolicy", "Restricted"]);
            let output = process.args(["-Command", &command]).output().unwrap();
            assert!(
                output.status.success(),
                "{program}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("VTCode_Shared_Loaded"));
        }
    }
}

#[cfg(unix)]
#[test]
fn native_fish_and_zsh_load_shared_hooks_and_keep_their_own_profiles() {
    use crate::{terminal::TerminalEmulator, terminal_completion::Notice};
    for program in ["fish", "zsh"] {
        if Command::new(program).arg("--version").output().is_err() {
            continue;
        }
        let home = tempfile::tempdir().unwrap();
        let marker = format!("VTCode_Shared_{program}>");
        let config = home.path().join(".config");
        fs::create_dir_all(config.join("fish")).unwrap();
        fs::write(
            config.join("fish/config.fish"),
            format!("function fish_prompt\n printf '{marker} '\nend\n"),
        )
        .unwrap();
        fs::write(
            home.path().join(".zshrc"),
            format!("PS1='{marker} '\nHISTFILE=\n"),
        )
        .unwrap();
        let mut launch = Integration::prepare(program, &[]).unwrap();
        if program == "zsh" {
            for (key, value) in &mut launch.env {
                if key == "VTCODE_ZDOTDIR_SET" {
                    *value = "1".into();
                }
                if key == "VTCODE_ZDOTDIR" {
                    *value = home.path().as_os_str().into();
                }
            }
            if !launch.env.iter().any(|(key, _)| key == "VTCODE_ZDOTDIR") {
                launch
                    .env
                    .push(("VTCODE_ZDOTDIR".into(), home.path().as_os_str().into()));
            }
        }
        let mut args = vec![
            format!("HOME={}", home.path().display()),
            format!("XDG_CONFIG_HOME={}", config.display()),
            "TERM=xterm-256color".into(),
        ];
        args.extend(
            launch.env.iter().map(|(key, value)| {
                format!("{}={}", key.to_string_lossy(), value.to_string_lossy())
            }),
        );
        args.push(program.into());
        args.extend(
            launch
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        let mut terminal =
            TerminalEmulator::spawn_command_in_dir("env", &args, 120, 24, home.path()).unwrap();
        super::tests::wait_for(&mut terminal, |terminal| {
            terminal.grid.cells.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .contains(&marker)
            })
        });
        for (index, command) in [(1, "true"), (2, "false")] {
            super::tests::enter(&mut terminal, command);
            super::tests::wait_for(&mut terminal, |terminal| {
                terminal.grid.commands.finished_count() == index
            });
        }
        let mut notice = Notice::default();
        notice.update(terminal.grid.commands, false, std::time::Instant::now());
        assert!(notice.unread && notice.failed());
    }
}

#[cfg(unix)]
#[test]
fn native_fish_sources_paths_with_quotes_backslashes_and_spaces() {
    if Command::new("fish").arg("--version").output().is_err() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let directory = root
        .path()
        .join("code's λ \\\\ $() folder/.vtcode/shell-integration/test-version");
    let script = storage::install_in(
        &directory,
        "fish.fish",
        "set -g VTCODE_SHARED_LOADED 'from shared file'\n",
    )
    .unwrap();
    let command = format!(
        "{}; printf '%s' \"$VTCODE_SHARED_LOADED\"",
        fish_command(&script).unwrap()
    );
    let output = Command::new("fish")
        .args(["--no-config", "-c", &command])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"from shared file");
}
