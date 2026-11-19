//! A private, local byte stream between the workbench and one terminal.
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    time::Duration,
};

pub(super) const OPEN_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_PACKET: usize = 32 * 1024 * 1024;

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
        os::unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        path::Path,
        time::Instant,
    };

    pub(crate) type Stream = UnixStream;
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

        pub(crate) fn accept(self) -> io::Result<Stream> {
            let deadline = Instant::now() + OPEN_TIMEOUT;
            loop {
                match self.socket.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false)?;
                        return Ok(stream);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "the detached terminal did not connect",
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(15));
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
            ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
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
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
            },
            Threading::{CreateEventW, ResetEvent, WaitForSingleObject},
        },
    };

    const PREFIX: &str = r"\\.\pipe\vtcode-tab-";

    pub(crate) struct Stream {
        file: File,
        event: OwnedHandle,
    }

    impl Stream {
        fn new(file: File) -> io::Result<Self> {
            // SAFETY: a successful call returns the owned event for our I/O.
            let event = unsafe { CreateEventW(null(), 1, 0, null()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                file,
                event: unsafe { OwnedHandle::from_raw_handle(event) },
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
            start: impl FnOnce(&File, *mut u32, *mut OVERLAPPED) -> i32,
        ) -> io::Result<usize> {
            let mut op = self.operation();
            let mut count = 0;
            if start(&self.file, &mut count, &mut op) == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(error);
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
    }

    impl Read for Stream {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let len = bytes.len().min(u32::MAX as usize) as u32;
            if len == 0 {
                return Ok(0);
            }
            self.transfer(|file, count, op| unsafe {
                ReadFile(file.as_raw_handle(), bytes.as_mut_ptr(), len, count, op)
            })
        }
    }

    impl Write for Stream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let len = bytes.len().min(u32::MAX as usize) as u32;
            if len == 0 {
                return Ok(0);
            }
            self.transfer(|file, count, op| unsafe {
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

        pub(crate) fn accept(self) -> io::Result<Stream> {
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
                            let mut ignored = 0;
                            if WaitForSingleObject(op.hEvent, OPEN_TIMEOUT.as_millis() as u32)
                                != WAIT_OBJECT_0
                            {
                                CancelIoEx(handle, &op);
                                GetOverlappedResult(handle, &op, &mut ignored, 1);
                                return Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "the detached terminal did not connect",
                                ));
                            }
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
}

pub(super) use platform::{Listener, Stream, connect, disconnect};
