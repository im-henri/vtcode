//! Platform desktop integration. Paths and URLs are never shell command text.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub(crate) enum Request {
    SystemApp(PathBuf),
    Terminal(PathBuf),
    Reveal(PathBuf),
    Url(String),
}

impl Request {
    pub(crate) fn run(self) -> io::Result<String> {
        let message = match self {
            Self::SystemApp(path) => {
                system_app(&path)?;
                format!(
                    "Opened {} in system app",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )
            }
            Self::Terminal(path) => {
                let dir = if path.is_dir() {
                    path.as_path()
                } else {
                    path.parent().unwrap_or(&path)
                };
                terminal(dir)?;
                "Opened terminal".into()
            }
            Self::Reveal(path) => {
                reveal(&path)?;
                "Revealed in native file explorer".into()
            }
            Self::Url(url) => {
                desktop_open(std::ffi::OsStr::new(&url))?;
                "Opened link in browser".into()
            }
        };
        Ok(message)
    }
}

fn spawn_launcher(command: &mut Command) -> io::Result<Child> {
    // Null stdio alone does not prevent a Windows process from attaching to
    // the parent's console and changing its shared input mode.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn launch(command: &mut Command) -> io::Result<()> {
    let mut child = spawn_launcher(command)?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn system_app(path: &Path) -> io::Result<()> {
    desktop_open(std::path::absolute(path)?.as_os_str())
}

fn desktop_open(target: &std::ffi::OsStr) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        windows_open(target, None, None)
    }
    #[cfg(target_os = "macos")]
    {
        launch(Command::new("open").arg(target))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        match launch(Command::new("xdg-open").arg(target)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                launch(Command::new("gio").arg("open").arg(target))
            }
            result => result,
        }
    }
}

fn reveal(path: &Path) -> io::Result<()> {
    let path = std::path::absolute(path)?;
    #[cfg(target_os = "windows")]
    {
        if path.is_file() {
            launch(Command::new("explorer.exe").arg(format!("/select,{}", path.display())))
        } else {
            system_app(&path)
        }
    }
    #[cfg(target_os = "macos")]
    {
        launch(Command::new("open").arg("-R").arg(&path))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        system_app(if path.is_dir() {
            &path
        } else {
            path.parent().unwrap_or(&path)
        })
    }
}

fn terminal(dir: &Path) -> io::Result<()> {
    let dir = std::path::absolute(dir)?;
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "terminal directory does not exist",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        // Windows Terminal chooses the user's default profile. Older Windows
        // installations can open an available PowerShell in its own console.
        match launch(
            Command::new("wt.exe")
                .args(["-w", "new", "new-tab", "--startingDirectory"])
                .arg(&dir)
                .current_dir(&dir),
        ) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            result => return result,
        }
        let mut last = None;
        for program in ["pwsh.exe", "powershell.exe"] {
            match windows_open(std::ffi::OsStr::new(program), Some("-NoExit"), Some(&dir)) {
                Ok(()) => return Ok(()),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap())
    }
    #[cfg(target_os = "macos")]
    {
        launch(Command::new("open").args(["-a", "Terminal"]).arg(&dir))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // Prefer the desktop's configured terminal, then distro alternatives.
        // Each executable receives arguments directly, with no shell parsing.
        let mut candidates = Vec::new();
        let mut xdg = Command::new("xdg-terminal-exec");
        let mut arg = std::ffi::OsString::from("--dir=");
        arg.push(&dir);
        xdg.arg(arg);
        candidates.push(xdg);
        candidates.push(Command::new("x-terminal-emulator"));
        for (program, flag) in [
            ("gnome-terminal", "--working-directory"),
            ("konsole", "--workdir"),
            ("xfce4-terminal", "--working-directory"),
        ] {
            let mut command = Command::new(program);
            command.arg(flag).arg(&dir);
            candidates.push(command);
        }
        candidates.push(Command::new("xterm"));
        for mut command in candidates {
            match launch(command.current_dir(&dir)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                result => return result,
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no desktop terminal found (install or configure xdg-terminal-exec)",
        ))
    }
}

#[cfg(target_os = "windows")]
fn windows_open(
    target: &std::ffi::OsStr,
    parameters: Option<&str>,
    directory: Option<&Path>,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{
            Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    };
    fn wide(value: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
        let mut value: Vec<u16> = value.encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "target contains a null character",
            ));
        }
        value.push(0);
        Ok(value)
    }
    let target = wide(target)?;
    let parameters = parameters
        .map(|s| wide(std::ffi::OsStr::new(s)))
        .transpose()?;
    let directory = directory.map(|p| wide(p.as_os_str())).transpose()?;
    // This runs on a worker. NOASYNC keeps shell/DDE work alive until the
    // launch completes; the editor's event loop never waits for it.
    // SAFETY: all strings are NUL-terminated and live throughout the call,
    // info is initialized, and COM initialization is balanced on this thread.
    unsafe {
        let hr = CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        if hr < 0 {
            return Err(io::Error::other(format!(
                "COM initialization failed: {hr:#x}"
            )));
        }
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpFile: target.as_ptr(),
            lpParameters: parameters.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            lpDirectory: directory.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            nShow: SW_SHOWNORMAL,
            ..Default::default()
        };
        // Native associations accept literal paths/URLs. The default shell
        // execution flags give console applications a separate console.
        let result = if ShellExecuteExW(&mut info) != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
        CoUninitialize();
        result
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    #[test]
    fn desktop_dispatch_preserves_literal_arguments_and_working_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("directory with spaces 世界");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("file $(ignored) ' ; 世界.txt");
        std::fs::write(&file, "probe").unwrap();
        for mode in ["preferred", "fallback", "missing"] {
            let helpers = temporary.path().join(mode);
            std::fs::create_dir(&helpers).unwrap();
            let programs: &[&str] = match mode {
                "preferred" => &["xdg-open", "xdg-terminal-exec"],
                "fallback" => &["gio", "x-terminal-emulator"],
                _ => &[],
            };
            for program in programs {
                let path = helpers.join(program);
                std::fs::write(&path, b"#!/bin/sh\nprintf '%s\\0' \"${0##*/}\" \"$PWD\" \"$@\" >> \"$VTCODE_DESKTOP_LOG\"\nprintf '\\n' >> \"$VTCODE_DESKTOP_LOG\"\n").unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let log = temporary.path().join(format!("{mode}.log"));
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "external_open::linux_tests::desktop_dispatch_fixture", "--nocapture"])
                .env("PATH", &helpers)
                .env("VTCODE_DESKTOP_MODE", mode)
                .env("VTCODE_DESKTOP_FILE", &file)
                .env("VTCODE_DESKTOP_LOG", &log)
                .output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        }
    }

    #[test]
    fn desktop_dispatch_fixture() {
        let Ok(mode) = std::env::var("VTCODE_DESKTOP_MODE") else { return; };
        let file = PathBuf::from(std::env::var_os("VTCODE_DESKTOP_FILE").unwrap());
        let folder = file.parent().unwrap();
        let log = PathBuf::from(std::env::var_os("VTCODE_DESKTOP_LOG").unwrap());
        let url = "https://example.invalid/?q=a b&literal=$(ignored)";
        let requests = [
            Request::SystemApp(file.clone()), Request::Reveal(file.clone()),
            Request::Url(url.into()), Request::Terminal(file.clone()),
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let result = request.run();
            if mode == "missing" {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
                continue;
            }
            result.unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let contents = std::fs::read_to_string(&log).unwrap_or_default();
                let rows: Vec<_> = contents.lines().collect();
                if rows.len() > index {
                    let values: Vec<_> = rows[index].split('\0').filter(|value| !value.is_empty()).collect();
                    if index == 3 {
                        assert_eq!(values[1], folder.to_str().unwrap());
                        if mode == "preferred" {
                            assert_eq!(values[0], "xdg-terminal-exec");
                            assert_eq!(values[2..], [format!("--dir={}", folder.display())]);
                        } else {
                            assert_eq!(values[0], "x-terminal-emulator");
                            assert_eq!(values.len(), 2);
                        }
                    } else {
                        let target = match index { 0 => file.to_str().unwrap(), 1 => folder.to_str().unwrap(), _ => url };
                        if mode == "preferred" {
                            assert_eq!(values[0], "xdg-open");
                            assert_eq!(values[2..], [target]);
                        } else {
                            assert_eq!(values[0], "gio");
                            assert_eq!(values[2..], ["open", target]);
                        }
                    }
                    break;
                }
                assert!(Instant::now() < deadline, "desktop helper did not receive request {index}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(Request::Terminal(folder.join("missing directory/not a file")).run().unwrap_err().kind(), io::ErrorKind::NotFound);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::windows::{io::AsRawHandle, process::CommandExt};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
    }

    #[test]
    fn launcher_preserves_console_input_mode() {
        const PROBE: &str = "VTCODE_CONSOLE_MODE_TEST";
        if std::env::var_os(PROBE).is_none() {
            // Give the probe its own hidden console, even in headless CI. Do
            // not change the test runner's console or open an interactive app.
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "external_open::tests::launcher_preserves_console_input_mode",
                    "--nocapture",
                ])
                .env(PROBE, "1")
                .creation_flags(0x0800_0000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "console probe failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let input = OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .unwrap();
        let handle = input.as_raw_handle();
        let mut original = 0;
        // SAFETY: the handle belongs to the live console file and the output
        // pointer refers to an initialized u32 for the duration of each call.
        assert_ne!(unsafe { GetConsoleMode(handle, &mut original) }, 0);
        // Match the relevant crossterm raw/mouse input flags: no processed,
        // line, echo, or quick-edit input; enable mouse and extended flags.
        let raw = (original & !(0x0001 | 0x0002 | 0x0004 | 0x0040)) | 0x0010 | 0x0080;
        assert_ne!(unsafe { SetConsoleMode(handle, raw) }, 0);

        let status = spawn_launcher(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "external_open::tests::launcher_child_changes_own_console_mode",
                ])
                .env("VTCODE_CONSOLE_CHILD_TEST", "1"),
        )
        .unwrap()
        .wait()
        .unwrap();
        assert!(status.success());
        let mut after = 0;
        assert_ne!(unsafe { GetConsoleMode(handle, &mut after) }, 0);
        assert_ne!(unsafe { SetConsoleMode(handle, original) }, 0);
        assert_eq!(after, raw, "launcher changed the editor's input mode");
    }

    #[test]
    fn launcher_child_changes_own_console_mode() {
        if std::env::var_os("VTCODE_CONSOLE_CHILD_TEST").is_none() {
            return;
        }
        // Model a shell enabling Ctrl+C processing. Opening CONIN$ explicitly
        // also proves that redirecting stdio alone does not isolate a console.
        let input = OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .unwrap();
        let mut mode = 0;
        // SAFETY: input owns a live console handle; mode is writable storage.
        assert_ne!(
            unsafe { GetConsoleMode(input.as_raw_handle(), &mut mode) },
            0
        );
        assert_ne!(
            unsafe { SetConsoleMode(input.as_raw_handle(), mode | 0x0001) },
            0
        );
    }
}
