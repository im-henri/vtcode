//! A private, local byte stream between the workbench and one terminal.
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(super) const OPEN_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_PACKET: usize = 32 * 1024 * 1024;

pub(super) struct Opening {
    deadline: Instant,
    cancelled: AtomicBool,
    signal: platform::Signal,
}

impl Opening {
    pub(super) fn new() -> io::Result<Self> {
        Self::with_timeout(OPEN_TIMEOUT)
    }

    pub(super) fn with_timeout(timeout: Duration) -> io::Result<Self> {
        Ok(Self {
            deadline: Instant::now() + timeout,
            cancelled: AtomicBool::new(false),
            signal: platform::Signal::new()?,
        })
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.signal.wake();
    }

    pub(super) fn remaining(&self) -> io::Result<Duration> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "detached terminal launch was cancelled",
            ));
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the detached terminal did not complete its handshake",
                )
            })
    }
}

pub(super) fn read_handshake(
    stream: &mut Stream,
    opening: &Opening,
    bytes: &mut Vec<u8>,
) -> io::Result<()> {
    struct Handshake<'a>(&'a mut Stream, &'a Opening);
    impl Read for Handshake<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            platform::read_opening(self.0, self.1, bytes)
        }
    }
    read_packet(&mut Handshake(stream, opening), bytes)
}

pub(super) fn read_packet(reader: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_PACKET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized window packet",
        ));
    }
    bytes.resize(length, 0);
    reader.read_exact(bytes)
}

pub(super) fn write_packet(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_PACKET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "oversized window packet",
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(bytes)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{
        fs::{self, Permissions},
        os::{
            fd::{AsRawFd, RawFd},
            unix::{
                fs::{FileTypeExt, MetadataExt, PermissionsExt},
                net::{UnixListener, UnixStream},
            },
        },
        path::Path,
    };

    pub(crate) type Stream = UnixStream;

    pub(super) struct Signal {
        receiver: UnixStream,
        sender: UnixStream,
    }

    impl Signal {
        pub(super) fn new() -> io::Result<Self> {
            let (receiver, sender) = UnixStream::pair()?;
            Ok(Self { receiver, sender })
        }

        pub(super) fn wake(&self) {
            disconnect(&self.sender);
        }
    }

    fn wait_readable(fd: RawFd, opening: &Opening) -> io::Result<()> {
        let mut fds = [
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: opening.signal.receiver.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            let millis = opening.remaining()?.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: both descriptors stay owned during the wait.
            let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, millis) };
            if result > 0 {
                opening.remaining()?;
                return Ok(());
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }

    pub(super) fn read_opening(
        stream: &mut Stream,
        opening: &Opening,
        bytes: &mut [u8],
    ) -> io::Result<usize> {
        wait_readable(stream.as_raw_fd(), opening)?;
        stream.read(bytes)
    }
    pub(crate) struct Listener {
        socket: UnixListener,
        directory: tempfile::TempDir,
    }

    impl Listener {
        pub(crate) fn bind() -> io::Result<Self> {
            let directory = tempfile::Builder::new()
                .prefix("vtcode-tab-")
                .permissions(Permissions::from_mode(0o700))
                .tempdir()?;
            let socket = UnixListener::bind(directory.path().join("window"))?;
            socket.set_nonblocking(true)?;
            Ok(Self { socket, directory })
        }

        pub(crate) fn endpoint(&self) -> OsString {
            self.directory.path().join("window").into_os_string()
        }

        pub(crate) fn accept(self, opening: &Opening) -> io::Result<Stream> {
            loop {
                wait_readable(self.socket.as_raw_fd(), opening)?;
                match self.socket.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false)?;
                        return Ok(stream);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    pub(crate) fn connect(endpoint: &OsStr) -> io::Result<Stream> {
        let path = Path::new(endpoint);
        let socket = fs::symlink_metadata(path)?;
        let directory = fs::symlink_metadata(path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid detached window endpoint",
            )
        })?)?;
        // SAFETY: geteuid takes no arguments and reads the current process UID.
        let user = unsafe { libc::geteuid() };
        if !socket.file_type().is_socket()
            || socket.uid() != user
            || !directory.is_dir()
            || directory.uid() != user
            || directory.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the detached window socket must be in a private directory",
            ));
        }
        UnixStream::connect(path)
    }

    pub(crate) fn disconnect(stream: &Stream) {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    pub(crate) fn peer_disconnected(stream: &Stream) -> bool {
        #[cfg(target_os = "linux")]
        let events = libc::POLLRDHUP;
        #[cfg(not(target_os = "linux"))]
        let events = 0;
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // With pending frame bytes, wait for hangup rather than readability.
        (unsafe { libc::poll(&mut fd, 1, 0) }) > 0
            && fd.revents & (events | libc::POLLHUP | libc::POLLERR) != 0
    }

    pub(crate) fn exit_frontend() -> ! {
        // Only the dedicated frontend calls this after its owner has gone.
        unsafe { libc::_exit(0) }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        fs::{File, OpenOptions},
        hash::BuildHasher,
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{
            ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
            ERROR_PIPE_NOT_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_FAILED,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile,
            WriteFile,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe,
            },
            Threading::{
                CreateEventW, GetCurrentProcess, ResetEvent, SetEvent, TerminateProcess,
                WaitForMultipleObjects,
            },
        },
    };

    const PREFIX: &str = r"\\.\pipe\vtcode-tab-";

    pub(super) struct Signal(OwnedHandle);

    impl Signal {
        pub(super) fn new() -> io::Result<Self> {
            Ok(Self(new_event()?))
        }

        pub(super) fn wake(&self) {
            unsafe {
                SetEvent(self.0.as_raw_handle());
            }
        }
    }

    fn new_event() -> io::Result<OwnedHandle> {
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateEventW returned a new owned handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(event) })
    }

    fn wait_opening_io(
        handle: HANDLE,
        operation: &OVERLAPPED,
        opening: &Opening,
    ) -> io::Result<()> {
        let wait = (|| loop {
            let remaining = opening.remaining()?;
            let handles = [opening.signal.0.as_raw_handle(), operation.hEvent];
            // SAFETY: both event handles remain valid throughout the wait.
            let result = unsafe {
                WaitForMultipleObjects(
                    2,
                    handles.as_ptr(),
                    0,
                    remaining.as_millis().min(u128::from(u32::MAX - 1)) as u32 + 1,
                )
            };
            match result {
                WAIT_TIMEOUT => continue,
                WAIT_FAILED => break Err(io::Error::last_os_error()),
                WAIT_OBJECT_0 => break opening.remaining().map(|_| ()),
                result if result == WAIT_OBJECT_0 + 1 => break opening.remaining().map(|_| ()),
                _ => break Err(io::Error::other("unexpected detached terminal wait result")),
            }
        })();
        if wait.is_err() {
            // SAFETY: join the cancelled operation before its buffer or
            // OVERLAPPED can leave scope.
            let mut count = 0;
            unsafe {
                CancelIoEx(handle, operation);
                GetOverlappedResult(handle, operation, &mut count, 1);
            }
        }
        wait
    }

    pub(crate) struct Stream {
        file: File,
        event: OwnedHandle,
    }

    impl Stream {
        fn new(file: File) -> io::Result<Self> {
            Ok(Self {
                file,
                event: new_event()?,
            })
        }

        pub(crate) fn try_clone(&self) -> io::Result<Self> {
            // Separate events allow one reader and one writer to wait concurrently.
            Self::new(self.file.try_clone()?)
        }

        fn operation(&self) -> OVERLAPPED {
            unsafe {
                ResetEvent(self.event.as_raw_handle());
            }
            OVERLAPPED {
                hEvent: self.event.as_raw_handle(),
                ..Default::default()
            }
        }

        fn transfer(
            &mut self,
            opening: Option<&Opening>,
            start: impl FnOnce(&File, *mut u32, *mut OVERLAPPED) -> i32,
        ) -> io::Result<usize> {
            let mut op = self.operation();
            let mut count = 0;
            if start(&self.file, &mut count, &mut op) == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(error);
                }
                if let Some(opening) = opening {
                    wait_opening_io(self.file.as_raw_handle(), &op, opening)?;
                }
                // SAFETY: wait until the OS has released op and the borrowed buffer.
                if unsafe { GetOverlappedResult(self.file.as_raw_handle(), &op, &mut count, 1) }
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(count as usize)
        }

        fn read_with_opening(
            &mut self,
            bytes: &mut [u8],
            opening: Option<&Opening>,
        ) -> io::Result<usize> {
            let len = bytes.len().min(u32::MAX as usize) as u32;
            if len == 0 {
                return Ok(0);
            }
            self.transfer(opening, |file, count, op| unsafe {
                ReadFile(file.as_raw_handle(), bytes.as_mut_ptr(), len, count, op)
            })
        }
    }

    impl Read for Stream {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.read_with_opening(bytes, None)
        }
    }

    pub(super) fn read_opening(
        stream: &mut Stream,
        opening: &Opening,
        bytes: &mut [u8],
    ) -> io::Result<usize> {
        opening.remaining()?;
        stream.read_with_opening(bytes, Some(opening))
    }

    impl Write for Stream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let len = bytes.len().min(u32::MAX as usize) as u32;
            if len == 0 {
                return Ok(0);
            }
            self.transfer(None, |file, count, op| unsafe {
                WriteFile(file.as_raw_handle(), bytes.as_ptr(), len, count, op)
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub(crate) struct Listener {
        stream: Stream,
        name: OsString,
    }

    impl Listener {
        pub(crate) fn bind() -> io::Result<Self> {
            let nonce = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
            let name = OsString::from(format!("{PREFIX}{}-{nonce:016x}", std::process::id()));
            let path: Vec<_> = name.encode_wide().chain([0]).collect();
            let sddl: Vec<_> = "D:P(A;;GA;;;OW)\0".encode_utf16().collect();
            let mut descriptor = null_mut();
            // SAFETY: the non-inheritable pipe belongs to this user and rejects
            // remote clients. The OS copies the descriptor during creation.
            let file = unsafe {
                if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    null_mut(),
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
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    64 * 1024,
                    64 * 1024,
                    0,
                    &security,
                );
                let result = if handle == INVALID_HANDLE_VALUE {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(File::from_raw_handle(handle))
                };
                LocalFree(descriptor);
                result?
            };
            Ok(Self {
                stream: Stream::new(file)?,
                name,
            })
        }

        pub(crate) fn endpoint(&self) -> OsString {
            self.name.clone()
        }

        pub(crate) fn accept(self, opening: &Opening) -> io::Result<Stream> {
            opening.remaining()?;
            let mut op = self.stream.operation();
            let handle = self.stream.file.as_raw_handle();
            // SAFETY: any pending connect is completed or cancelled and joined
            // before its event and OVERLAPPED leave this scope.
            unsafe {
                if ConnectNamedPipe(handle, &mut op) == 0 {
                    let error = io::Error::last_os_error();
                    match error.raw_os_error().map(|code| code as u32) {
                        Some(ERROR_PIPE_CONNECTED) => {}
                        Some(ERROR_IO_PENDING) => {
                            wait_opening_io(handle, &op, opening)?;
                            let mut ignored = 0;
                            if GetOverlappedResult(handle, &op, &mut ignored, 0) == 0 {
                                return Err(io::Error::last_os_error());
                            }
                        }
                        _ => return Err(error),
                    }
                }
            }
            Ok(self.stream)
        }
    }

    pub(crate) fn connect(endpoint: &OsStr) -> io::Result<Stream> {
        if !endpoint
            .to_str()
            .is_some_and(|name| name.starts_with(PREFIX))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid detached window pipe",
            ));
        }
        Stream::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OVERLAPPED)
                .open(endpoint)?,
        )
    }

    pub(crate) fn disconnect(stream: &Stream) {
        // The server owns the pipe instance. Disconnect releases blocked reads
        // and writes even if the terminal frontend is hung.
        unsafe {
            DisconnectNamedPipe(stream.file.as_raw_handle());
        }
    }

    pub(crate) fn peer_disconnected(stream: &Stream) -> bool {
        // The overlapped client handle can peek without consuming frame bytes.
        let result = unsafe {
            PeekNamedPipe(
                stream.file.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        result == 0
            && matches!(
                io::Error::last_os_error()
                    .raw_os_error()
                    .map(|code| code as u32),
                Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED)
            )
    }

    pub(crate) fn exit_frontend() -> ! {
        // Only the dedicated frontend calls this after its owner has gone.
        unsafe {
            TerminateProcess(GetCurrentProcess(), 0);
        }
        std::process::exit(0)
    }
}

pub(super) use platform::{
    Listener, Stream, connect, disconnect, exit_frontend, peer_disconnected,
};
