//! Local-only record channel to a helper in SSH's real console. This avoids
//! interpreting raw terminal keys a second time in ConPTY's VT input parser.
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::process::Command;
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::*,
    Security::*,
    Storage::FileSystem::*,
    System::{Console::*, IO::*, JobObjects::*, Pipes::*, Threading::*},
};

pub(super) const MAX_RECORDS: usize = 4096;
pub(super) const RECORD_SIZE: usize = std::mem::size_of::<INPUT_RECORD>();

pub(super) struct Pipe {
    pub name: OsString,
    file: File,
}
pub(super) struct PipeWriter {
    file: File,
    event: OwnedHandle,
}

impl Pipe {
    pub fn into_writer(self) -> io::Result<PipeWriter> {
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(PipeWriter {
            file: self.file,
            event: unsafe { OwnedHandle::from_raw_handle(event) },
        })
    }
    pub fn new() -> io::Result<Self> {
        Self::new_direction(PIPE_ACCESS_OUTBOUND)
    }
    pub fn new_reader() -> io::Result<Self> {
        Self::new_direction(PIPE_ACCESS_INBOUND)
    }
    fn new_direction(direction: u32) -> io::Result<Self> {
        use std::hash::BuildHasher;
        let nonce = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
        let name = OsString::from(format!(
            r"\\.\pipe\vtcode-headless-{}-{nonce:016x}",
            std::process::id()
        ));
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<_> = name.encode_wide().chain([0]).collect();
        let sddl: Vec<_> = "D:P(A;;GA;;;OW)\0".encode_utf16().collect();
        let mut descriptor = std::ptr::null_mut();
        unsafe {
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let security = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let handle = CreateNamedPipeW(
                path.as_ptr(),
                direction | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                &security,
            );
            LocalFree(descriptor);
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                name,
                file: File::from_raw_handle(handle),
            })
        }
    }

    pub fn connect(&mut self, child: RawHandle, pid: u32) -> io::Result<()> {
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        let mut operation = OVERLAPPED {
            hEvent: event.as_raw_handle(),
            ..Default::default()
        };
        unsafe {
            if ConnectNamedPipe(self.file.as_raw_handle(), &mut operation) == 0 {
                match GetLastError() {
                    ERROR_PIPE_CONNECTED => {}
                    ERROR_IO_PENDING => {
                        let handles = [event.as_raw_handle(), child];
                        if WaitForMultipleObjects(2, handles.as_ptr(), 0, 10_000) != 0 {
                            CancelIoEx(self.file.as_raw_handle(), &operation);
                            let mut count = 0;
                            GetOverlappedResult(
                                self.file.as_raw_handle(),
                                &operation,
                                &mut count,
                                1,
                            );
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "SSH console helper did not connect",
                            ));
                        }
                    }
                    _ => return Err(io::Error::last_os_error()),
                }
            }
            let mut actual = 0;
            if GetNamedPipeClientProcessId(self.file.as_raw_handle(), &mut actual) == 0
                || actual != pid
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Unexpected SSH console pipe client",
                ));
            }
        }
        Ok(())
    }
}

impl Write for PipeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut operation = OVERLAPPED {
            hEvent: self.event.as_raw_handle(),
            ..Default::default()
        };
        let mut written = 0;
        unsafe {
            ResetEvent(self.event.as_raw_handle());
            if WriteFile(
                self.file.as_raw_handle(),
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                &mut operation,
            ) == 0
            {
                if GetLastError() != ERROR_IO_PENDING {
                    return Err(io::Error::last_os_error());
                }
                if GetOverlappedResult(self.file.as_raw_handle(), &operation, &mut written, 1) == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(written as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for PipeWriter {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut operation = OVERLAPPED {
            hEvent: self.event.as_raw_handle(),
            ..Default::default()
        };
        let mut read = 0;
        unsafe {
            ResetEvent(self.event.as_raw_handle());
            if ReadFile(
                self.file.as_raw_handle(),
                bytes.as_mut_ptr(),
                bytes.len().min(u32::MAX as usize) as u32,
                &mut read,
                &mut operation,
            ) == 0
            {
                let error = GetLastError();
                if error == ERROR_BROKEN_PIPE || error == ERROR_NO_DATA {
                    return Ok(0);
                }
                if error != ERROR_IO_PENDING {
                    return Err(io::Error::last_os_error());
                }
                if GetOverlappedResult(self.file.as_raw_handle(), &operation, &mut read, 1) == 0 {
                    let error = GetLastError();
                    if error == ERROR_BROKEN_PIPE || error == ERROR_NO_DATA {
                        return Ok(0);
                    }
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(read as usize)
    }
}

pub(super) fn child_job(child: RawHandle) -> io::Result<OwnedHandle> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(job);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
            || AssignProcessToJobObject(job.as_raw_handle(), child) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

pub(super) fn write_records(bytes: &[u8], output: &mut impl Write) -> io::Result<()> {
    if !bytes.len().is_multiple_of(RECORD_SIZE) || bytes.len() > MAX_RECORDS * RECORD_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid console record frame",
        ));
    }
    output.write_all(&((bytes.len() / RECORD_SIZE) as u32).to_le_bytes())?;
    output.write_all(bytes)
}

/// Storage is zero initialized before the Windows read, including padding.
pub(super) fn record_bytes(records: &[INPUT_RECORD]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(records.as_ptr().cast(), std::mem::size_of_val(records)) }
}

pub(crate) fn run_host(args: &[OsString]) -> io::Result<i32> {
    let separator = 2;
    if args.len() < separator + 2 || args[separator] != "--" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid console host arguments",
        ));
    }
    let mut pipe = OpenOptions::new().read(true).open(&args[0])?;
    let output = OpenOptions::new().write(true).open(&args[1])?;
    // Retain a local pipe handle until SSH's exit is known. Otherwise EOF can
    // make the workbench clean up the helper before it reports SSH's status.
    // These bytes never enter the SSH connection or reach the headless node.
    let mut status_output = output.try_clone()?;
    // The parent verifies our kernel process identity and installs crash cleanup
    // before allowing us to create SSH or accept any credentials/input.
    let mut start = [0];
    pipe.read_exact(&mut start)?;
    if start != [1] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid console host startup",
        ));
    }
    // The local workbench's keyboard protocol must not become SSH's byte
    // encoding. Configure this private console explicitly before SSH starts.
    let console_mode = crate::windows_input::ConsoleInputMode::save()?;
    console_mode.prepare_bridge()?;
    let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
    let mut command = Command::new(&args[separator + 1]);
    command.args(&args[separator + 2..]);
    command.stderr(std::process::Stdio::from(output.try_clone()?));
    command.stdout(std::process::Stdio::from(output));
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = writeln!(status_output, "\nVTCode local transport launch failed: {error}");
            return Err(error);
        }
    };
    let raw = child.as_raw_handle();
    let killer = match unsafe { std::os::windows::io::BorrowedHandle::borrow_raw(raw) }
        .try_clone_to_owned()
    {
        Ok(handle) => handle,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let reading = std::thread::Builder::new()
        .name("ssh-console-records".into())
        .spawn(move || {
            let mut records = [INPUT_RECORD::default(); MAX_RECORDS];
            let result = (|| -> io::Result<()> {
                loop {
                    let mut header = [0; 4];
                    pipe.read_exact(&mut header)?;
                    let count = u32::from_le_bytes(header) as usize;
                    if count > MAX_RECORDS {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Oversized console record frame",
                        ));
                    }
                    let bytes = unsafe {
                        std::slice::from_raw_parts_mut(
                            records.as_mut_ptr().cast(),
                            count * RECORD_SIZE,
                        )
                    };
                    pipe.read_exact(bytes)?;
                    if count == 0 {
                        continue;
                    }
                    // Console input has no writable-space notification. Bound
                    // its queued records during bulk transfer; adaptive waits
                    // run only while SSH is backpressured, never on the UI.
                    let mut delay = 1;
                    loop {
                        let mut pending = 0;
                        if unsafe {
                            GetNumberOfConsoleInputEvents(input.as_raw_handle(), &mut pending)
                        } == 0
                        {
                            return Err(io::Error::last_os_error());
                        }
                        if pending < (MAX_RECORDS * 2) as u32 {
                            break;
                        }
                        if unsafe { WaitForSingleObject(killer.as_raw_handle(), delay) }
                            == WAIT_OBJECT_0
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "SSH process exited",
                            ));
                        }
                        delay = (delay * 2).min(16);
                    }
                    let mut written = 0;
                    if unsafe {
                        WriteConsoleInputW(
                            input.as_raw_handle(),
                            records.as_ptr(),
                            count as u32,
                            &mut written,
                        )
                    } == 0
                        || written as usize != count
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
            })();
            if result.is_err() {
                unsafe {
                    TerminateProcess(killer.as_raw_handle(), 1);
                }
            }
        });
    if let Err(error) = reading {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let code = child.wait()?.code().unwrap_or(1);
    writeln!(status_output, "\nVTCode local transport exited with code {code}")?;
    status_output.flush()?;
    Ok(code)
}
