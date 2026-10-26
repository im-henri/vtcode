//! Windows SSH needs a private console for native input and authentication.
//! Structured stdout/stderr bypass ConPTY entirely through a protected pipe:
//! a terminal renderer is never used as the service's byte transport.
use super::*;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use windows_sys::Win32::System::Console::{
    INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
};
#[path = "remote_console_channel.rs"]
mod channel;
pub(crate) use channel::run_host;

pub(super) fn terminate_job(job: &OwnedHandle) {
    unsafe {
        windows_sys::Win32::System::JobObjects::TerminateJobObject(job.as_raw_handle(), 1);
    }
}
struct Cancel {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    armed: bool,
}
impl Drop for Cancel {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.killer.kill();
        }
    }
}

pub(super) fn spawn(
    program: &std::ffi::OsStr,
    args: &[OsString],
    size: ratatui::layout::Size,
    cwd: &Path,
) -> io::Result<Transport> {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            cols: size.width.max(2),
            rows: size.height.saturating_sub(2).max(2),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(io::Error::other)?;
    let mut input = channel::Pipe::new()?;
    let mut data = channel::Pipe::new_reader()?;
    let host_args: Vec<_> = [
        input.name.clone(),
        data.name.clone(),
        "--".into(),
        program.to_owned(),
    ]
    .into_iter()
    .chain(args.iter().cloned())
    .collect();
    let mut command = portable_pty::CommandBuilder::new(std::env::current_exe()?);
    command.cwd(cwd);
    #[cfg(not(test))]
    {
        command.arg("--headless-console-host");
        command.args(&host_args);
    }
    #[cfg(test)]
    {
        command.args([
            "--ignored",
            "--exact",
            "remote::launch::console::tests::console_host_fixture",
            "--nocapture",
        ]);
        command.env("VT_HEADLESS_HOST_ARGS", serde_json::to_string(&host_args)?);
    }
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(io::Error::other)?;
    drop(pair.slave);
    let mut cancel = Cancel {
        killer: child.clone_killer(),
        armed: true,
    };
    let handle = child
        .as_raw_handle()
        .ok_or_else(|| io::Error::other("Missing private console handle"))?;
    let pid = child
        .process_id()
        .ok_or_else(|| io::Error::other("Missing private console process ID"))?;
    let job = channel::child_job(handle)?;
    input.connect(handle, pid)?;
    data.connect(handle, pid)?;
    let reader = data.into_writer()?;
    let mut input = input.into_writer()?;
    input.write_all(&[1])?;
    // Only the helper's own console diagnostics use ConPTY. SSH's stdout and
    // stderr are already connected directly to the verified data pipe.
    let console_output = Some(pair.master.try_clone_reader().map_err(io::Error::other)?);
    let mut ascii = [(0, 0, 0); 128];
    for (byte, template) in ascii.iter_mut().enumerate() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = match byte as u8 {
            b'\n' => KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            b'\r' => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            27 => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            byte => KeyEvent::new(KeyCode::Char(char::from(byte)), KeyModifiers::NONE),
        };
        crate::input_terminal::for_each_win32_key(key, |vk, unit, _, control| {
            *template = (vk, unit, control);
        });
    }
    let writer = Records {
        pipe: input,
        records: vec![INPUT_RECORD::default(); channel::MAX_RECORDS],
        ascii,
    };
    cancel.armed = false;
    Ok(Transport {
        child,
        master: pair.master,
        reader: Box::new(reader),
        writer: Box::new(writer),
        job: Some(job),
        console_output,
    })
}

struct Records {
    pipe: channel::PipeWriter,
    records: Vec<INPUT_RECORD>,
    ascii: [(u16, u16, u32); 128],
}
impl Records {
    fn clear(&mut self) {
        // Initialize padding in the actual retained allocation before exposing
        // the local ABI byte frame. No uninitialized memory crosses the pipe.
        unsafe {
            std::ptr::write_bytes(self.records.as_mut_ptr(), 0, self.records.len());
        }
    }
    fn set(&mut self, index: usize, vk: u16, unit: u16, down: u8, control: u32) {
        self.records[index].EventType = KEY_EVENT as u16;
        self.records[index].Event.KeyEvent = KEY_EVENT_RECORD {
            bKeyDown: i32::from(down),
            wRepeatCount: 1,
            wVirtualKeyCode: vk,
            wVirtualScanCode: 0,
            uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
            dwControlKeyState: control,
        };
    }
    fn send(&mut self, count: usize) -> io::Result<()> {
        channel::write_records(
            channel::record_bytes(&self.records[..count]),
            &mut self.pipe,
        )
    }
}
impl Write for Records {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.clear();
        let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
        let mut count = 0;
        let mut consumed = 0;
        for ch in text.chars() {
            if count + ch.len_utf16() > self.records.len() {
                break;
            }
            if ch.is_ascii() {
                let (vk, unit, control) = self.ascii[ch as usize];
                self.set(count, vk, unit, 1, control);
                count += 1;
            } else {
                for &unit in ch.encode_utf16(&mut [0; 2]).iter() {
                    self.set(count, 0, unit, 1, 0);
                    count += 1;
                }
            }
            consumed += ch.len_utf8();
        }
        self.send(count)?;
        Ok(consumed)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.pipe.flush()
    }
}
impl LoginWriter for Records {
    fn key(&mut self, key: crossterm::event::KeyEvent, _: bool, _: bool) -> io::Result<()> {
        self.clear();
        let mut count = 0;
        crate::input_terminal::for_each_win32_key(key, |vk, unit, down, control| {
            self.set(count, vk, unit, down, control);
            count += 1;
        });
        self.send(count)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "Internal private console process"]
    fn console_host_fixture() {
        let args = std::env::var("VT_HEADLESS_HOST_ARGS").unwrap();
        let args: Vec<std::ffi::OsString> = serde_json::from_str(&args).unwrap();
        std::process::exit(super::run_host(&args).unwrap());
    }
}
