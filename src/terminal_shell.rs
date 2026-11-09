//! Shared, versioned hooks beside the executable on the machine running the
//! shell. Existing shell profiles remain in control.
use std::{ffi::OsString, io, path::Path};

#[path = "terminal_shell_storage.rs"]
mod storage;

#[cfg(test)]
#[path = "terminal_shell_storage_tests.rs"]
mod storage_tests;

#[derive(Default)]
pub(crate) struct Integration {
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
}

impl Integration {
    pub(crate) fn prepare(program: &str, args: &[String]) -> io::Result<Self> {
        Self::prepare_in(program, args, &storage::directory())
    }

    fn prepare_in(program: &str, args: &[String], directory: &Path) -> io::Result<Self> {
        let mut launch = Self::default();
        // Explicit commands and noninteractive tools must retain their arguments.
        if !args.is_empty() {
            return Ok(launch);
        }
        let name = program.rsplit(['/', '\\']).next().unwrap_or(program).to_ascii_lowercase();
        match name.trim_end_matches(".exe") {
            "pwsh" | "powershell" => {
                let script = storage::install_in(directory, "powershell.ps1", include_str!("terminal_shell/powershell.ps1"))?;
                launch.args = ["-NoLogo", "-NoExit", "-Command", &powershell_command(&script)?]
                    .map(OsString::from).into();
            }
            "bash" => {
                let rc = storage::install_in(directory, "bashrc", include_str!("terminal_shell/bash.sh"))?;
                launch.args = vec!["--rcfile".into(), rc.into_os_string(), "-i".into()];
            }
            "zsh" => {
                storage::install_in(directory, ".zshenv", include_str!("terminal_shell/zshenv"))?;
                storage::install_in(directory, ".zshrc", include_str!("terminal_shell/zshrc"))?;
                if let Some(original) = std::env::var_os("ZDOTDIR") {
                    launch.env.push(("VTCODE_ZDOTDIR_SET".into(), "1".into()));
                    launch.env.push(("VTCODE_ZDOTDIR".into(), original));
                } else {
                    launch.env.push(("VTCODE_ZDOTDIR_SET".into(), "0".into()));
                }
                launch.env.push(("ZDOTDIR".into(), directory.as_os_str().into()));
                launch.args.push("-i".into());
            }
            "fish" => {
                let script = storage::install_in(directory, "fish.fish", include_str!("terminal_shell/fish.fish"))?;
                launch.args = ["-i", "--init-command", &fish_command(&script)?]
                    .map(OsString::from).into();
            }
            _ => {}
        }
        Ok(launch)
    }

    pub(crate) fn apply(&self, command: &mut portable_pty::CommandBuilder) {
        command.args(&self.args);
        for (key, value) in &self.env {
            command.env(key, value);
        }
    }
}

fn script_path(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Shell integration path is not valid Unicode"))
}

fn powershell_command(path: &Path) -> io::Result<String> {
    let path = script_path(path)?.replace('\'', "''");
    // Keep the existing interactive-command loading behavior and language-mode
    // guard; loading a file must not require changing execution policy.
    Ok(format!("if ($ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage') {{ . ([scriptblock]::Create([System.IO.File]::ReadAllText('{path}'))) }}"))
}

fn fish_command(path: &Path) -> io::Result<String> {
    let path = script_path(path)?.replace('\\', "\\\\").replace('\'', "\\'");
    Ok(format!("source '{path}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{terminal::TerminalEmulator, terminal_completion::Notice};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};

    pub(super) fn wait_for(terminal: &mut TerminalEmulator, mut ready: impl FnMut(&TerminalEmulator) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            terminal.process();
            if ready(terminal) { return; }
            let screen = terminal.grid.cells.iter().map(|row| row.iter().map(|cell| cell.ch).collect::<String>().trim_end().to_owned()).collect::<Vec<_>>().join("\n");
            assert!(Instant::now() < deadline, "{} did not become ready ({:?}):\n{screen}", terminal.profile_name, terminal.grid.commands);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn enter(terminal: &mut TerminalEmulator, command: &str) {
        // PSReadLine restores/re-enables Win32 input mode between commands.
        // Let its asynchronous ConPTY mode frame arrive before encoding keys.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut quiet_since = Instant::now();
        while quiet_since.elapsed() < Duration::from_millis(40) && Instant::now() < deadline {
            if terminal.process() { quiet_since = Instant::now(); }
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut input = Vec::new();
        for code in command.chars().map(KeyCode::Char).chain([KeyCode::Enter]) {
            crate::input_terminal::write_key(KeyEvent::new(code, KeyModifiers::NONE),
                terminal.grid.application_cursor_keys, terminal.grid.win32_input_mode, |bytes| input.extend_from_slice(bytes));
        }
        terminal.write(&input);
    }

    fn powershell_test_args(program: &str) -> Vec<String> {
        let launch = Integration::prepare(program, &[]).unwrap();
        let mut args: Vec<String> = launch.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        args.insert(0, "-NoProfile".into());
        // Interactive shell tests must not append commands to user history.
        args.last_mut().unwrap().insert_str(0, "Set-PSReadLineOption -HistorySaveStyle SaveNothing; ");
        args
    }

    #[test]
    fn terminal_completion_preserves_explicit_commands_and_unknown_shells() {
        for (program, args) in [("pwsh.exe", vec!["-Command".into(), "exit 7".into()]), ("bash", vec!["-c".into(), "true".into()]), ("custom-shell", vec![])] {
            let launch = Integration::prepare(program, &args).unwrap();
            assert!(launch.args.is_empty() && launch.env.is_empty());
        }
    }

    #[cfg(windows)]
    #[test]
    fn terminal_completion_native_powershell_silent_failed_and_multiline_commands() {
        // Exercise both modern PowerShell and the system 5.1 fallback. Keep
        // the test independent of locally installed prompt themes/profiles.
        for program in ["pwsh.exe", "powershell.exe"] {
            if !std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|directory| directory.join(program).is_file()) { continue; }
            let mut args = powershell_test_args(program);
            let script = args.last_mut().unwrap();
            script.insert_str(0, "function global:prompt { \"VTCode_Ready_$($global:?)> \" }; ");
            let directory = tempfile::tempdir().unwrap();
            let cwd = directory.path().join("my dir #1;100%");
            let nested = cwd.join("child");
            std::fs::create_dir_all(&nested).unwrap();
            let mut terminal = TerminalEmulator::spawn_command_in_dir(program, &args, 120, 24, &cwd).unwrap();
            wait_for(&mut terminal, |terminal| {
                terminal.grid.cells.iter().any(|row| row.iter().map(|cell| cell.ch).collect::<String>().contains("VTCode_Ready_True>"))
            });
            assert_eq!(terminal.grid.commands.finished_count(), 0, "startup is not a command");
            let mut notice = Notice::default();
            for (index, command, failed) in [(1, "$null", false), (2, "Write-Error 'VTCode_failure'", true), (3, "$null", false)] {
                enter(&mut terminal, command);
                wait_for(&mut terminal, |terminal| terminal.grid.commands.finished_count() == index);
                notice.update(terminal.grid.commands, false, Instant::now());
                assert!(notice.unread, "{program}: {command}");
                assert_eq!(notice.failed(), failed, "{program}: {command}");
                if failed {
                    wait_for(&mut terminal, |terminal| terminal.grid.cells.iter().any(|row|
                        row.iter().map(|cell| cell.ch).collect::<String>().contains("VTCode_Ready_False>")));
                }
                notice.acknowledge();
            }
            enter(&mut terminal, "");
            enter(&mut terminal, "if ($true) {");
            enter(&mut terminal, "$null");
            enter(&mut terminal, "}");
            enter(&mut terminal, "");
            wait_for(&mut terminal, |terminal| terminal.grid.commands.finished_count() >= 4);
            assert_eq!(terminal.grid.commands.finished_count(), 4, "empty lines and continuation prompts are not completed commands");
            enter(&mut terminal, "cd child");
            wait_for(&mut terminal, |terminal| terminal.grid.cwd_uri.as_deref()
                .and_then(|uri| url::Url::parse(uri).ok()).and_then(|uri| uri.to_file_path().ok())
                .is_some_and(|path| crate::app::App::same_open_path(&path, &nested)));
        }
    }

    #[test]
    fn terminal_completion_native_bash_preserves_prompt_hooks_and_ignores_empty_input() {
        let bash = if cfg!(windows) {
            let path = std::path::PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_default()).join("Git/bin/bash.exe");
            if !path.is_file() { return; }
            path
        } else {
            std::path::PathBuf::from("bash")
        };
        let env = if cfg!(windows) { bash.parent().unwrap().parent().unwrap().join("usr/bin/env.exe") } else { "env".into() };
        for array in [false, true] {
            for legacy in [false, true] {
                let home = tempfile::tempdir().unwrap();
                let cwd = home.path().join("my dir #1;100%");
                std::fs::create_dir_all(cwd.join("child")).unwrap();
                let prompt = if array {
                    "PROMPT_COMMAND=('printf \"VTCode_Status:%s\\n\" \"$?\"')"
                } else {
                    "PROMPT_COMMAND='printf \"VTCode_Status:%s\\n\" \"$?\"'"
                };
                std::fs::write(home.path().join(".bashrc"), format!("PS1='VTCode_Ready> '\n{prompt}\n")).unwrap();
                let mut launch = Integration::prepare(bash.to_str().unwrap(), &[]).unwrap();
                if legacy {
                    // Run the history-based path on machines with modern Bash
                    // as well, without changing the user's shell or environment.
                    let rc = std::path::Path::new(&launch.args[1]);
                    let script = std::fs::read_to_string(rc).unwrap()
                        .replace("if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then", "if true; then")
                        .replace("if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then", "if false; then");
                    let private_rc = home.path().join("legacy bashrc");
                    std::fs::write(&private_rc, script).unwrap();
                    launch.args[1] = private_rc.into_os_string();
                }
                let mut args = vec![format!("HOME={}", home.path().to_string_lossy().replace('\\', "/")), bash.to_string_lossy().into_owned()];
                args.extend(launch.args.iter().map(|arg| arg.to_string_lossy().into_owned()));
                let mut terminal = TerminalEmulator::spawn_command_in_dir(env.to_str().unwrap(), &args, 100, 24, &cwd).unwrap();
                terminal.profile_name = format!("Bash array={array}, legacy={legacy}");
                wait_for(&mut terminal, |terminal| terminal.grid.cells.iter().any(|row| row.iter().map(|cell| cell.ch).collect::<String>().contains("VTCode_Ready>")));
                assert_eq!(terminal.grid.commands.finished_count(), 0);
                enter(&mut terminal, "");
                enter(&mut terminal, ":");
                wait_for(&mut terminal, |terminal| terminal.grid.commands.finished_count() >= 1);
                assert_eq!(terminal.grid.commands.finished_count(), 1, "array={array}, legacy={legacy}");
                enter(&mut terminal, "false");
                wait_for(&mut terminal, |terminal| {
                    terminal.grid.commands.finished_count() >= 2 && terminal.grid.cells.iter().any(|row| row.iter().map(|cell| cell.ch).collect::<String>().contains("VTCode_Status:1"))
                });
                assert_eq!(terminal.grid.commands.finished_count(), 2);
                let mut notice = Notice::default();
                notice.update(terminal.grid.commands, false, Instant::now());
                assert!(notice.unread && notice.failed());
                enter(&mut terminal, "if true; then");
                enter(&mut terminal, ":");
                enter(&mut terminal, "fi");
                wait_for(&mut terminal, |terminal| terminal.grid.commands.finished_count() >= 3);
                assert_eq!(terminal.grid.commands.finished_count(), 3, "continuation prompts must not finish commands");
                enter(&mut terminal, "cd child");
                wait_for(&mut terminal, |terminal| terminal.grid.cwd_uri.as_ref()
                    .is_some_and(|uri| uri.ends_with("/my%20dir%20%231%3B100%25/child")));
            }
        }
    }

    #[test]
    fn terminal_completion_remote_shell_signals_cross_the_workspace_connection() {
        let directory = tempfile::tempdir().unwrap();
        let client = crate::remote::tests::connection(directory.path(), Default::default(), Default::default());
        let program = if cfg!(windows) { "powershell.exe" } else { "bash" };
        let args = if cfg!(windows) { powershell_test_args(program) } else { Vec::new() };
        let process = client.spawn(program.into(), args, crate::remote::RemotePath::default(), Some((100, 24)), Default::default()).unwrap();
        let mut terminal = TerminalEmulator::from_remote(process, 100, 24, Default::default()).unwrap();
        // As with a task launched in a new terminal, input can precede the
        // first prompt. Hook installation must not consume that first command.
        terminal.write(if cfg!(windows) { b"$null\r" } else { b":\r" });
        wait_for(&mut terminal, |terminal| terminal.grid.commands.finished_count() >= 1);
        let mut notice = Notice::default();
        notice.update(terminal.grid.commands, false, Instant::now());
        assert!(notice.unread);
        assert!(!notice.failed());
    }
}
