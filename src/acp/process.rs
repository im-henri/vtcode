//! Owned process trees. Dropping a connection or terminal always tears down its
//! descendants, including shells that would otherwise keep pipe readers alive.
use std::io;
use std::path::Path;
use std::process::{Child, Command};

/// Background tools have explicit pipe streams. They must not bypass those
/// streams by opening the editor's (or headless protocol's) controlling TTY.
pub fn without_controlling_terminal(command: &mut Command) {
    #[cfg(unix)] {
        use std::os::unix::process::CommandExt;
        // SAFETY: this child-only callback uses async-signal-safe syscalls,
        // closes its descriptor on every path, and touches no shared Rust state.
        unsafe { command.pre_exec(|| {
            let tty = libc::open(c"/dev/tty".as_ptr(),libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            if tty < 0 { return Ok(()); }
            // Detach only this child, never the parent editor or job shell.
            if libc::getsid(0) == libc::getpid() {
                libc::close(tty);
                return Err(io::Error::from_raw_os_error(libc::ENOTTY));
            }
            let result = libc::ioctl(tty,libc::TIOCNOTTY);
            let error = io::Error::last_os_error();
            libc::close(tty);
            if result < 0 { Err(error) } else { Ok(()) }
        }); }
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// Build a child using the same environment for executable lookup and launch.
/// Rust only supplies an omitted .exe suffix on Windows; many CLI installers
/// expose .cmd/.bat launchers instead. Resolve those without a shell command
/// string, retaining std's argument escaping and process-tree ownership.
pub fn command<'a>(
    program: &str,
    cwd: &Path,
    environment: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Command {
    let environment: Vec<_> = environment.into_iter().collect();
    #[cfg(windows)]
    let mut command = {
        let override_value = |name: &str| {
            environment
                .iter()
                .rev()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| std::ffi::OsString::from(value))
        };
        let path = override_value("PATH").or_else(|| {
            // The regular terminal refreshes the saved Windows environment.
            // Use its existing reader so newly installed CLIs also work over
            // ACP, while retaining inherited PATH priority (e.g. a venv).
            let terminal_environment = portable_pty::CommandBuilder::new(program);
            merge_windows_paths(
                std::env::var_os("PATH").as_deref(),
                terminal_environment.get_env("PATH"),
            )
        });
        let path_ext = override_value("PATHEXT").or_else(|| std::env::var_os("PATHEXT"));
        let resolved = windows_program(program.as_ref(), cwd, path.as_deref(), path_ext.as_deref())
            .unwrap_or_else(|| program.into());
        let mut command = Command::new(resolved);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command
    };
    #[cfg(not(windows))]
    let mut command = Command::new(program);
    command
        .current_dir(super::files::agent_path(cwd))
        .envs(environment);
    command
}

#[cfg(windows)]
fn merge_windows_paths(
    inherited: Option<&std::ffi::OsStr>,
    refreshed: Option<&std::ffi::OsStr>,
) -> Option<std::ffi::OsString> {
    use std::{collections::HashSet, path::PathBuf};
    let mut seen = HashSet::new();
    let paths = inherited
        .into_iter()
        .chain(refreshed)
        .flat_map(std::env::split_paths)
        .filter(|path| !path.as_os_str().is_empty())
        .filter(|path| {
            let normalized: PathBuf = path.components().collect();
            seen.insert(
                normalized
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_ascii_lowercase(),
            )
        });
    if inherited.is_none() && refreshed.is_none() {
        None
    } else {
        std::env::join_paths(paths).ok()
    }
}

#[cfg(windows)]
fn windows_program(
    program: &std::ffi::OsStr,
    cwd: &Path,
    search_path: Option<&std::ffi::OsStr>,
    path_ext: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    use std::{ffi::OsStr, path::PathBuf};
    let program = Path::new(program);
    let explicit =
        program.is_absolute() || program.parent().is_some_and(|p| !p.as_os_str().is_empty());
    let candidates: Vec<PathBuf> = if explicit {
        vec![if program.is_absolute() {
            program.to_owned()
        } else {
            cwd.join(program)
        }]
    } else {
        search_path
            .into_iter()
            .flat_map(std::env::split_paths)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| {
                if dir.is_absolute() {
                    dir.join(program)
                } else {
                    cwd.join(dir).join(program)
                }
            })
            .collect()
    };
    let extensions: Vec<_> = path_ext
        .unwrap_or(OsStr::new(".COM;.EXE;.BAT;.CMD"))
        .to_string_lossy()
        .split(';')
        .map(|ext| ext.trim().to_ascii_lowercase())
        // File associations (.ps1, .js, etc.) require an explicit interpreter.
        .filter(|ext| matches!(ext.as_str(), ".exe" | ".com" | ".cmd" | ".bat"))
        .collect();
    for candidate in candidates {
        if candidate.extension().is_some() && candidate.is_file() {
            return Some(candidate);
        }
        if candidate.extension().is_none() {
            for ext in &extensions {
                let mut path = candidate.as_os_str().to_owned();
                path.push(ext);
                let path = PathBuf::from(path);
                if path.is_file() {
                    return Some(path);
                }
            }
            // npm installs an extensionless Unix shell script alongside its
            // Windows launchers. It is a file, but CreateProcess cannot run it.
            // A real Windows binary with no suffix remains usable.
            use std::io::Read;
            let mut signature = [0; 2];
            if std::fs::File::open(&candidate)
                .and_then(|mut file| file.read_exact(&mut signature))
                .is_ok()
                && signature == *b"MZ"
            {
                return Some(candidate);
            }
        }
    }
    None
}

pub struct Process {
    pub child: Child,
    control: std::sync::Arc<std::sync::Mutex<Control>>,
}
struct Control {
    terminated: bool,
    #[cfg(windows)]
    job: usize,
    #[cfg(unix)]
    group: i32,
}
#[derive(Clone)]
pub struct ProcessKiller(std::sync::Weak<std::sync::Mutex<Control>>);
impl ProcessKiller {
    pub fn active(&self) -> bool {
        self.0.upgrade().is_some_and(|control| !control.lock().unwrap_or_else(|e| e.into_inner()).terminated)
    }
    pub fn kill(&self) {
        if let Some(control) = self.0.upgrade() { control.lock().unwrap_or_else(|e| e.into_inner()).kill(); }
    }
}
impl Control {
    fn kill(&mut self) {
        if self.terminated { return; }
        self.terminated = true;
        #[cfg(windows)]
        // SAFETY: Control owns this live job handle until its final reference drops.
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job as _, 1); }
        #[cfg(unix)]
        // SAFETY: the owned child started in this group; reaping is coordinated
        // with this mutex so a later cancellation cannot signal a reused ID.
        {
            #[cfg(target_os = "linux")]
            if child_exit_ready(self.group).is_err() { return; }
            unsafe { libc::kill(-self.group, libc::SIGKILL); }
        }
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        self.kill();
        #[cfg(windows)]
        // SAFETY: the job is uniquely owned by the final Control reference.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job as _); }
    }
}

impl Process {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        without_controlling_terminal(command);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn()?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
            // SAFETY: the job and process handles are owned here. The kernel
            // copies the initialized limits before this stack frame returns.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if job.is_null()
                    || SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&limits) as u32,
                    ) == 0
                    || AssignProcessToJobObject(job, child.as_raw_handle()) == 0
                {
                    let error = io::Error::last_os_error();
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    let mut child = child;
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                Ok(Self {
                    child,
                    control: std::sync::Arc::new(std::sync::Mutex::new(Control { terminated: false, job: job as usize })),
                })
            }
        }
        #[cfg(not(windows))]
        {
            let group = child.id() as i32;
            Ok(Self { child, control: std::sync::Arc::new(std::sync::Mutex::new(Control { terminated: false, #[cfg(unix)] group })) })
        }
    }

    pub fn kill(&mut self) {
        self.control.lock().unwrap_or_else(|e| e.into_inner()).kill();
        let _ = self.child.kill();
    }
    pub fn killer(&self) -> ProcessKiller { ProcessKiller(std::sync::Arc::downgrade(&self.control)) }
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let mut control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(target_os = "linux")]
        if !control.terminated {
            // Keep the dead leader unreaped until its remaining process group
            // is killed. Reaping first permits the kernel to reuse that ID.
            if !child_exit_ready(control.group)? { return Ok(None); }
            control.kill();
        }
        let status = self.child.try_wait()?;
        if status.is_some() { control.kill(); }
        Ok(status)
    }
}

#[cfg(target_os = "linux")]
fn child_exit_ready(pid: libc::pid_t) -> io::Result<bool> {
    loop {
        // SAFETY: waitid fills this initialized buffer, never reaps (WNOWAIT),
        // and checks only this process's specified child, not unrelated jobs.
        let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::waitid(libc::P_PID, pid as _, &mut status, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) } == 0 {
            return Ok(unsafe { status.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod unix_ownership_tests {
    use super::*;
    use std::{io::BufRead, process::Stdio, time::{Duration, Instant}};

    #[test]
    fn observing_exit_kills_remaining_children_before_reaping_the_group_leader() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & printf '%s\\n' \"$!\"; exit 0"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut process = Process::spawn(&mut command).unwrap();
        let killer = process.killer();
        let mut line = String::new();
        std::io::BufReader::new(process.child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let child: u32 = line.trim().parse().unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while process.try_wait().unwrap().is_none() {
            assert!(Instant::now() < end, "Leader must exit without waiting for its background child");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!killer.active());
        loop {
            let live = std::fs::read_to_string(format!("/proc/{child}/stat")).ok()
                .is_some_and(|stat| stat.rsplit_once(')').unwrap().1.split_whitespace().next() != Some("Z"));
            if !live { break; }
            assert!(Instant::now() < end, "Background child survived the observed parent exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(process.try_wait().unwrap().unwrap().success(), "Repeated wait retains the original status");
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn launcher_resolves_path_extensions_in_directory_and_pathext_order() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first folder");
        let second = root.path().join("second folder");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("test-agent"), "#!/bin/sh\nexit 99\n").unwrap();
        std::fs::write(first.join("test-agent.ps1"), "exit 99\n").unwrap();
        for path in [first.join("test-agent.cmd"), second.join("test-agent.exe")] {
            std::fs::write(path, "").unwrap();
        }
        let path = std::env::join_paths([&first, &second]).unwrap();
        let ext = OsStr::new(".EXE;.CMD;.BAT;.PS1");
        assert_eq!(
            windows_program(
                OsStr::new("test-agent"),
                root.path(),
                Some(&path),
                Some(ext)
            ),
            Some(first.join("test-agent.cmd"))
        );
        std::fs::write(first.join("test-agent.exe"), "").unwrap();
        assert_eq!(
            windows_program(
                OsStr::new("test-agent"),
                root.path(),
                Some(&path),
                Some(ext)
            ),
            Some(first.join("test-agent.exe"))
        );
        assert_eq!(
            windows_program(
                OsStr::new("test-agent"),
                root.path(),
                Some(&path),
                Some(OsStr::new(".CMD;.EXE"))
            ),
            Some(first.join("test-agent.cmd"))
        );
    }

    #[test]
    fn launcher_uses_child_path_case_insensitively_and_keeps_arguments_separate() {
        let root = tempfile::tempdir().unwrap();
        let launcher = root.path().join("test-agent.cmd");
        std::fs::write(&launcher, "").unwrap();
        let path = root.path().to_string_lossy();
        let mut child = command(
            "test-agent",
            root.path(),
            [("Path", path.as_ref()), ("pathext", ".CMD")],
        );
        child.args(["argument with spaces", "literal&value"]);
        assert_eq!(child.get_program(), launcher.as_os_str());
        assert_eq!(
            child.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("argument with spaces"),
                OsStr::new("literal&value")
            ]
        );
        assert_eq!(
            child
                .get_envs()
                .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
                .unwrap()
                .1,
            Some(root.path().as_os_str()),
            "an explicit PATH override must not be extended with saved directories",
        );
    }

    #[test]
    fn launcher_skips_unix_only_shims_and_preserves_native_extensionless_binaries() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("unix");
        let second = root.path().join("windows");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("test-agent"), "#!/bin/sh\n").unwrap();
        std::fs::write(second.join("test-agent.exe"), b"MZ").unwrap();
        let path = std::env::join_paths([&first, &second]).unwrap();
        assert_eq!(
            windows_program(OsStr::new("test-agent"), root.path(), Some(&path), None),
            Some(second.join("test-agent.exe")),
        );
        let binary = first.join("native-without-suffix");
        std::fs::write(&binary, b"MZ").unwrap();
        assert_eq!(
            windows_program(
                OsStr::new("native-without-suffix"),
                root.path(),
                Some(&path),
                None
            ),
            Some(binary),
        );
    }

    #[test]
    fn refreshed_path_preserves_inherited_priority_and_appends_new_directories_once() {
        let inherited =
            std::env::join_paths([r"C:\venv\Scripts", r"C:\Tools", r"C:\semi;colon"]).unwrap();
        let saved =
            std::env::join_paths([r"c:\tools\", r"D:\User Tools\世界", r"C:\NewTool"]).unwrap();
        let merged = merge_windows_paths(Some(&inherited), Some(&saved)).unwrap();
        assert_eq!(
            std::env::split_paths(&merged).collect::<Vec<_>>(),
            [
                r"C:\venv\Scripts",
                r"C:\Tools",
                r"C:\semi;colon",
                r"D:\User Tools\世界",
                r"C:\NewTool"
            ]
            .map(std::path::PathBuf::from),
        );
        assert!(merge_windows_paths(None, None).is_none());
        assert_eq!(merge_windows_paths(Some(&inherited), None), Some(inherited));
    }

    #[test]
    #[ignore = "set VTCODE_TEST_INSTALLED_AGENTS to installed CLI names separated by semicolons"]
    fn installed_cli_launchers_run_by_name() {
        use std::{
            io::Read,
            process::Stdio,
            time::{Duration, Instant},
        };
        let names =
            std::env::var("VTCODE_TEST_INSTALLED_AGENTS").expect("supply installed CLI names");
        for name in names.split(';').filter(|name| !name.is_empty()) {
            let mut command = command(name, &std::env::current_dir().unwrap(), []);
            command
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut process =
                Process::spawn(&mut command).unwrap_or_else(|e| panic!("{name}: {e}"));
            let deadline = Instant::now() + Duration::from_secs(30);
            let status = loop {
                if let Some(status) = process.child.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "{name} --version timed out");
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut stdout = String::new();
            let mut stderr = String::new();
            process
                .child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut stdout)
                .unwrap();
            process
                .child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            assert!(status.success(), "{name}: {stderr}");
            assert!(!stdout.trim().is_empty(), "{name} returned no version");
            println!("{name}: {}", stdout.trim());
        }
    }

    #[test]
    fn launcher_handles_explicit_relative_paths_and_never_searches_implicit_cwd() {
        let root = tempfile::tempdir().unwrap();
        let launcher = root.path().join("test-agent.bat");
        std::fs::write(&launcher, "").unwrap();
        assert_eq!(
            windows_program(OsStr::new("./test-agent"), root.path(), None, None),
            Some(launcher.clone())
        );
        assert_eq!(
            windows_program(launcher.as_os_str(), root.path(), None, None),
            Some(launcher)
        );
        assert!(
            windows_program(
                OsStr::new("test-agent"),
                root.path(),
                Some(OsStr::new("")),
                None
            )
            .is_none()
        );
        std::fs::write(root.path().join("only-script.ps1"), "").unwrap();
        assert!(
            windows_program(
                OsStr::new("only-script"),
                root.path(),
                Some(root.path().as_os_str()),
                Some(OsStr::new(".PS1"))
            )
            .is_none()
        );
    }
}
