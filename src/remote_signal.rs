//! A hangup must unwind the service normally so its owned processes are killed.
//! Polling the input and a signal pipe adds no timer or idle wakeups.
use std::{
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::net::UnixStream,
    },
};

pub(super) struct Input {
    input: File,
    wake: UnixStream,
    handlers: Vec<signal_hook::SigId>,
    ended: bool,
}

impl Input {
    pub(super) fn new() -> io::Result<Self> {
        // File reads are unbuffered: polling STDIN while using std::io::Stdin's
        // hidden buffer could wait despite bytes already in that buffer.
        // SAFETY: fcntl duplicates the borrowed input; File owns only that copy.
        let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let input = unsafe { File::from_raw_fd(fd) };
        let (wake, writer) = UnixStream::pair()?;
        let mut result = Self {
            input,
            wake,
            handlers: Vec::new(),
            ended: false,
        };
        for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
            // signal-hook performs only an async-signal-safe, nonblocking write
            // in the handler. Cleanup runs on the ordinary service thread.
            result.handlers.push(signal_hook::low_level::pipe::register(
                signal,
                writer.try_clone()?,
            )?);
        }
        Ok(result)
    }
}

impl Read for Input {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.ended {
            return Ok(0);
        }
        let mut descriptors = [
            libc::pollfd {
                fd: self.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.input.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: both live descriptors and the two-element buffer stay
            // owned throughout poll. No other thread reads the service input.
            if unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if descriptors[0].revents != 0 {
                self.ended = true;
                return Ok(0);
            }
            if descriptors[1].revents != 0 {
                return self.input.read(out);
            }
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            signal_hook::low_level::unregister(handler);
        }
    }
}
