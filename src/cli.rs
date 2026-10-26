//! Parse arguments before touching the terminal, workspace, or configuration.
use std::{ffi::OsString, io::Write, path::PathBuf};

pub(crate) const HELP: &str = concat!(
    "VTCode ",
    env!("CARGO_PKG_VERSION"),
    " - native terminal code editor\n\n",
    "Usage:\n",
    "  vtcode [FILE[:LINE[:COLUMN]] | DIRECTORY]\n\n",
    "  vtcode --remote HOST [--remote-command COMMAND] [--ssh-command COMMAND]\n\n",
    "  vtcode --headless [DIRECTORY]\n\n",
    "  vtcode --install-theme SOURCE\n\n",
    "Arguments:\n",
    "  FILE                         Open a file, or create it when saved.\n\n",
    "  FILE:LINE[:COLUMN]           Open at a location (line/column start at 1).\n\n",
    "  DIRECTORY                    Open a project directory.\n",
    "                               With no path, open the current directory.\n\n",
    "Options:\n",
    "  -h, --help                   Show this help and exit without opening the UI.\n\n",
    "  -V, --version                Show the version and exit without opening the UI.\n\n",
    "  --remote HOST               Edit over SSH, using a local editor and buffers.\n",
    "                               HOST is [user@]hostname or an SSH config alias.\n",
    "                               Defaults: ssh -e none -tt HOST 'vtcode --headless'.\n\n",
    "  --remote-command COMMAND    Replace 'vtcode --headless' with this remote\n",
    "                               shell command, including any job submission.\n",
    "                               Specify the remote folder in this command.\n\n",
    "  --ssh-command COMMAND       Replace the entire local SSH command prefix.\n",
    "                               Default: 'ssh -e none -tt'. HOST and the remote\n",
    "                               command are appended as separate arguments.\n",
    "                               Use shell-style quotes around paths with spaces;\n",
    "                               no local shell expansion is performed.\n\n",
    "  --headless [DIRECTORY]       Run the remote workspace service without a UI.\n",
    "                               Defaults to the current directory; used with\n",
    "                               a local VTCode --remote client.\n\n",
    "  --install-theme SOURCE       Install a color theme without opening the UI.\n",
    "                               SOURCE: vscodethemes.com theme link, local VSIX,\n",
    "                               ZIP, extension directory, or JSON/JSONC file.\n\n",
    "  --                           Treat remaining arguments as literal paths.\n",
    "                               Use it before a path starting with '-'.\n\n",
    "Examples:\n",
    "  vtcode .\n\n",
    "  vtcode remote\n\n",
    "  vtcode src/main.rs:42:5\n\n",
    "  vtcode -- -notes.txt\n\n",
    "  vtcode --remote user@host\n\n",
    "  vtcode --remote user@host --remote-command 'vtcode --headless /path/to/project'\n\n",
    "  vtcode --remote user@login-host --remote-command 'submitcmd vtcode --headless /path/to/project'\n\n",
    "  vtcode --remote user@host --ssh-command 'ssh -e none -tt -vv -p 2222'\n",
);

pub(crate) fn write_help(output: &mut impl Write) -> std::io::Result<()> {
    output.write_all(HELP.as_bytes())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Version,
    Editor(Option<OsString>),
    InstallTheme(String),
    Headless(PathBuf),
    Remote {
        program: OsString,
        args: Vec<OsString>,
    },
    #[cfg(windows)]
    ConsoleHost(Vec<OsString>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Editor,
    InstallTheme,
    Headless,
    Remote,
}

#[derive(Default)]
struct RemoteOptions {
    host: Option<OsString>,
    command: Option<OsString>,
    ssh: Option<OsString>,
}
impl RemoteOptions {
    fn has_overrides(&self) -> bool {
        self.command.is_some() || self.ssh.is_some()
    }
    fn finish(self) -> Result<Command, String> {
        let host = self.host.ok_or("missing HOST for --remote")?;
        let text = host
            .to_str()
            .ok_or("the remote host must be valid Unicode")?;
        if text.is_empty()
            || text.starts_with('-')
            || text.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(
                "invalid HOST for --remote; use [user@]hostname or an SSH config alias".into(),
            );
        }
        // Remote shell syntax belongs to the server's shell. Pass it as one
        // opaque argument without inspecting or appending to the command.
        let command = self.command.unwrap_or_else(|| "vtcode --headless".into());
        let (program, mut args) = if let Some(ssh) = self.ssh {
            let ssh = ssh
                .into_string()
                .map_err(|_| "--ssh-command must be valid Unicode")?;
            let mut words = shell_words::split(&ssh)
                .map_err(|error| format!("invalid --ssh-command quoting: {error}"))?
                .into_iter();
            let program = words
                .next()
                .filter(|word| !word.is_empty() && !word.starts_with('-'))
                .ok_or("--ssh-command must include an executable")?;
            (program.into(), words.map(Into::into).collect())
        } else {
            ("ssh".into(), vec!["-e".into(), "none".into(), "-tt".into()])
        };
        args.extend([host, command]);
        Ok(Command::Remote { program, args })
    }
}

fn is_option(arg: &std::ffi::OsStr) -> bool {
    arg != "-" && arg.as_encoded_bytes().starts_with(b"-")
}

fn operand(value: &mut Option<OsString>, arg: OsString) -> Result<(), String> {
    if arg.is_empty() {
        return Err("a file, directory, or theme source cannot be empty".into());
    }
    if value.is_some() {
        return Err(format!(
            "unexpected argument {arg:?}; expected only one path or theme source"
        ));
    }
    *value = Some(arg);
    Ok(())
}

fn option_value(
    flag: &str,
    inline: Option<&str>,
    args: &mut std::iter::Peekable<impl Iterator<Item = OsString>>,
) -> Result<OsString, String> {
    let value: OsString = match inline {
        Some(value) => value.into(),
        None if args.peek().is_some_and(|arg| !is_option(arg)) => args.next().unwrap(),
        None => return Err(format!("missing value for {flag}")),
    };
    if value.is_empty()
        || value.to_str().is_some_and(|value| value.trim().is_empty())
        || value.as_encoded_bytes().contains(&0)
    {
        return Err(format!(
            "{flag} requires a nonempty value without NUL characters"
        ));
    }
    Ok(value)
}

pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    let mut mode = Mode::Editor;
    let mut remote = RemoteOptions::default();
    let mut value = None;
    let mut help = false;
    let mut version = false;
    while let Some(arg) = args.next() {
        let (flag, inline) = arg.to_str().map_or(("", None), |text| {
            text.split_once('=')
                .map_or((text, None), |(flag, value)| (flag, Some(value)))
        });
        match flag {
            "--" if inline.is_none() => {
                for arg in args {
                    operand(&mut value, arg)?;
                }
                break;
            }
            "-h" | "--help" if inline.is_none() => help = true,
            "-V" | "--version" if inline.is_none() => version = true,
            "--remote-command" | "--ssh-command" => {
                if !matches!(mode, Mode::Editor | Mode::Remote) || value.is_some() {
                    return Err(format!(
                        "argument {flag} requires --remote HOST and cannot be combined with another command or path"
                    ));
                }
                let slot = if flag == "--remote-command" {
                    &mut remote.command
                } else {
                    &mut remote.ssh
                };
                if slot.is_some() {
                    return Err(format!("argument {flag} may only be specified once"));
                }
                *slot = Some(option_value(flag, inline, &mut args)?);
            }
            "--remote" => {
                if mode != Mode::Editor || value.is_some() {
                    return Err(format!(
                        "argument {flag} cannot be combined with another command or path"
                    ));
                }
                mode = Mode::Remote;
                // Missing hosts are deferred until after --help/--version.
                if inline.is_some() || args.peek().is_some_and(|arg| !is_option(arg)) {
                    remote.host = Some(option_value(flag, inline, &mut args)?);
                }
            }
            "--headless" | "--install-theme" if inline.is_none() => {
                if mode != Mode::Editor || value.is_some() || remote.has_overrides() {
                    return Err(format!(
                        "argument {arg:?} cannot be combined with another command or path"
                    ));
                }
                mode = if flag == "--headless" {
                    Mode::Headless
                } else {
                    Mode::InstallTheme
                };
            }
            #[cfg(windows)]
            "--headless-console-host"
                if inline.is_none()
                    && mode == Mode::Editor
                    && value.is_none()
                    && !remote.has_overrides() =>
            {
                let args: Vec<_> = args.collect();
                if args.len() < 4
                    || args[2] != "--"
                    || args[0].is_empty()
                    || args[1].is_empty()
                    || args[3].is_empty()
                {
                    return Err("invalid internal console host arguments".into());
                }
                return Ok(if help {
                    Command::Help
                } else if version {
                    Command::Version
                } else {
                    Command::ConsoleHost(args)
                });
            }
            _ if is_option(&arg) => return Err(format!("unknown option {arg:?}")),
            _ => operand(&mut value, arg)?,
        }
    }
    if help {
        return Ok(Command::Help);
    }
    if version {
        return Ok(Command::Version);
    }
    if mode != Mode::Remote && remote.has_overrides() {
        return Err("--remote-command and --ssh-command require --remote HOST".into());
    }
    match mode {
        Mode::Editor => Ok(Command::Editor(value)),
        Mode::Remote => {
            if let Some(value) = value {
                return Err(format!(
                    "unexpected path {value:?} with --remote; specify the remote folder inside --remote-command"
                ));
            }
            remote.finish()
        }
        Mode::Headless => Ok(Command::Headless(
            value.map(PathBuf::from).unwrap_or_else(|| ".".into()),
        )),
        Mode::InstallTheme => {
            let source = value.ok_or("missing SOURCE for --install-theme")?;
            let source = source
                .into_string()
                .map_err(|_| "the theme source must be valid Unicode")?;
            Ok(Command::InstallTheme(source))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parsed(args: &[&str]) -> Command {
        parse(args.iter().map(OsString::from)).unwrap()
    }
    fn remote(program: &str, args: &[&str]) -> Command {
        Command::Remote {
            program: program.into(),
            args: args.iter().map(Into::into).collect(),
        }
    }

    #[test]
    fn ordinary_launches_keep_paths_locations_and_former_command_names() {
        assert_eq!(parsed(&[]), Command::Editor(None));
        for path in [
            ".",
            "remote",
            "bridge",
            "new file.rs",
            "src/main.rs:42:5",
            "C:\\project\\file.rs:8",
            "café/🦀.rs",
            "-",
            "name=value",
        ] {
            assert_eq!(parsed(&[path]), Command::Editor(Some(path.into())));
        }
    }
    #[test]
    fn help_and_version_do_not_launch_remote_commands() {
        for args in [
            vec!["--help"],
            vec!["-h"],
            vec!["file.rs", "--help"],
            vec!["--headless", "--help"],
            vec!["--install-theme", "-h"],
            vec!["--remote", "--help"],
            vec!["--remote", "user@host", "--help"],
            vec![
                "--remote",
                "user@host",
                "--ssh-command",
                "do-not-launch",
                "--help",
            ],
        ] {
            assert_eq!(parsed(&args), Command::Help);
        }
        for args in [
            vec!["--version"],
            vec!["-V"],
            vec!["file.rs", "-V"],
            vec!["--remote", "--version"],
        ] {
            assert_eq!(parsed(&args), Command::Version);
        }
    }
    #[test]
    fn separator_preserves_option_and_command_named_paths() {
        for path in [
            "--help",
            "--headless",
            "--remote",
            "-notes.txt",
            "remote",
            "bridge",
        ] {
            assert_eq!(parsed(&["--", path]), Command::Editor(Some(path.into())));
        }
        assert_eq!(
            parsed(&["--headless", "--", "-project"]),
            Command::Headless("-project".into())
        );
        assert_eq!(
            parsed(&["--install-theme", "--", "-theme.json"]),
            Command::InstallTheme("-theme.json".into())
        );
    }
    #[test]
    fn non_ui_commands_keep_their_operands() {
        assert_eq!(parsed(&["--headless"]), Command::Headless(".".into()));
        assert_eq!(
            parsed(&["--headless", "project folder"]),
            Command::Headless("project folder".into())
        );
        assert_eq!(
            parsed(&[
                "--install-theme",
                "https://vscodethemes.com/e/owner.theme/dark"
            ]),
            Command::InstallTheme("https://vscodethemes.com/e/owner.theme/dark".into())
        );
    }
    #[test]
    fn remote_defaults_append_host_and_one_opaque_launch_command() {
        for host in [
            "henri@192.168.50.144",
            "work-cluster",
            "user@[::1]",
            "host.example",
        ] {
            assert_eq!(
                parsed(&["--remote", host]),
                remote("ssh", &["-e", "none", "-tt", host, "vtcode --headless"])
            );
        }
        assert_eq!(
            parsed(&["--remote=work-cluster"]),
            parsed(&["--remote", "work-cluster"])
        );
        let command =
            "bash -ic 'aapsubmit --queue short /opt/VTCode/vtcode --headless \"/my project/λ🦀\"'";
        assert_eq!(
            parsed(&["--remote", "user@login", "--remote-command", command]),
            remote("ssh", &["-e", "none", "-tt", "user@login", command])
        );
        assert_eq!(
            parsed(&["--remote-command", command, "--remote=user@login"]),
            parsed(&[
                "--remote=user@login",
                &format!("--remote-command={command}")
            ])
        );
    }
    #[test]
    fn ssh_override_replaces_all_defaults_and_never_runs_a_local_shell() {
        let ssh = r#"'C:\Program Files\OpenSSH\ssh.exe' -e none -tt -vv -p 2222 -i 'C:\Keys\my key' -o 'ProxyCommand=ssh -W %h:%p jump'"#;
        let command = "submitcmd --cores 1 vtcode --headless '/my project'";
        assert_eq!(
            parsed(&[
                "--ssh-command",
                ssh,
                "--remote-command",
                command,
                "--remote",
                "user@host"
            ]),
            remote(
                r"C:\Program Files\OpenSSH\ssh.exe",
                &[
                    "-e",
                    "none",
                    "-tt",
                    "-vv",
                    "-p",
                    "2222",
                    "-i",
                    r"C:\Keys\my key",
                    "-o",
                    "ProxyCommand=ssh -W %h:%p jump",
                    "user@host",
                    command
                ]
            )
        );
        assert_eq!(
            parsed(&["--remote=alias", "--ssh-command=wrapper --custom"]),
            remote("wrapper", &["--custom", "alias", "vtcode --headless"])
        );
        assert_eq!(
            parsed(&["--remote", "alias", "--ssh-command", "ssh -v"]),
            remote("ssh", &["-v", "alias", "vtcode --headless"])
        );
        assert_eq!(
            parsed(&[
                "--remote",
                "alias",
                "--ssh-command",
                "ssh '$HOME' ';' '$(touch x)'",
                "--remote-command",
                "printf '%s' \"$HOME\"; exec vtcode --headless"
            ]),
            remote(
                "ssh",
                &[
                    "$HOME",
                    ";",
                    "$(touch x)",
                    "alias",
                    "printf '%s' \"$HOME\"; exec vtcode --headless"
                ]
            )
        );
    }
    #[test]
    fn invalid_flags_missing_values_and_extras_are_rejected() {
        for args in [
            vec!["--unknown"],
            vec!["-x"],
            vec!["file.rs", "--unknown"],
            vec!["--help", "--unknown"],
            vec!["--headless", "--unknown"],
            vec!["--install-theme"],
            vec!["--install-theme", "--unknown"],
            vec!["file.rs", "extra.rs"],
            vec!["--headless", ".", "extra"],
            vec!["--install-theme", "theme.json", "extra"],
            vec!["--headless", "--install-theme", "theme.json"],
            vec!["file.rs", "--headless"],
            vec![""],
            vec!["--headless", ""],
            vec!["--install-theme", ""],
            vec!["--remote"],
            vec!["--remote", "--"],
            vec!["--remote="],
            vec!["--remote", ""],
            vec!["--remote", "-bad-host"],
            vec!["--remote=-bad-host"],
            vec!["--remote", "user@bad host"],
            vec!["--remote", "bad\nhost"],
            vec!["--remote", "host", "local-path"],
            vec!["--remote", "host", "--", "path"],
            vec!["local-path", "--remote", "host"],
            vec!["--remote", "host", "--remote", "other"],
            vec!["--headless", "--remote", "host"],
            vec!["--remote", "host", "--headless"],
            vec!["--remote-command", "vtcode --headless"],
            vec!["--ssh-command", "ssh"],
            vec!["--remote", "host", "--remote-command"],
            vec!["--remote", "host", "--remote-command", ""],
            vec!["--remote", "host", "--remote-command", "   "],
            vec!["--remote", "host", "--remote-command", "bad\0command"],
            vec!["--remote", "host", "--ssh-command"],
            vec!["--remote", "host", "--ssh-command", ""],
            vec!["--remote", "host", "--ssh-command", "   "],
            vec!["--remote", "host", "--ssh-command", "'' -tt"],
            vec!["--remote", "host", "--ssh-command=-vv"],
            vec!["--remote", "host", "--ssh-command", "ssh 'broken"],
            vec![
                "--remote",
                "host",
                "--ssh-command",
                "ssh",
                "--ssh-command",
                "other",
            ],
            vec![
                "--remote",
                "host",
                "--remote-command",
                "one",
                "--remote-command",
                "two",
            ],
            vec!["--remote-command", "cmd", "--headless"],
            vec!["--install-theme", "x", "--ssh-command", "ssh"],
            vec!["remote", "ssh"],
        ] {
            assert!(parse(args.iter().map(OsString::from)).is_err(), "{args:?}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn native_paths_and_remote_command_arguments_do_not_require_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b"name-\xff.rs".to_vec());
        assert_eq!(
            parse([path.clone()]).unwrap(),
            Command::Editor(Some(path.clone()))
        );
        assert_eq!(
            parse(["--headless".into(), path.clone()]).unwrap(),
            Command::Headless(path.clone().into())
        );
        assert_eq!(
            parse([
                "--remote".into(),
                "host".into(),
                "--remote-command".into(),
                path.clone()
            ])
            .unwrap(),
            Command::Remote {
                program: "ssh".into(),
                args: vec![
                    "-e".into(),
                    "none".into(),
                    "-tt".into(),
                    "host".into(),
                    path
                ]
            }
        );
        assert!(
            parse([OsString::from_vec(b"--bad-\xff".to_vec())])
                .unwrap_err()
                .contains("unknown option")
        );
    }
    #[cfg(windows)]
    #[test]
    fn console_host_validates_its_header_and_forwards_child_flags() {
        let args = ["input-pipe", "output-pipe", "--", "ssh", "--help"]
            .map(OsString::from)
            .to_vec();
        assert_eq!(
            parse(std::iter::once("--headless-console-host".into()).chain(args.clone())).unwrap(),
            Command::ConsoleHost(args)
        );
        for args in [
            vec!["--headless-console-host"],
            vec!["--headless-console-host", "a", "b", "ssh"],
            vec!["--headless-console-host", "a", "b", "--", ""],
        ] {
            assert!(parse(args.iter().map(OsString::from)).is_err());
        }
    }
}
