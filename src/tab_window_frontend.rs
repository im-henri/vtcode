//! The terminal process only forwards input and presents the owner's VT frames.
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};

use crossterm::event::Event;
use serde::{Deserialize, Serialize};

use super::{
    desktop::{self, Host, ScreenPoint},
    transport::{self, Stream},
};
use crate::{app_event_loop::TerminalEventPump, background::UiWake};

#[derive(Debug, Deserialize, Serialize)]
pub(super) enum Input {
    Hello {
        size: (u16, u16),
        host: Option<Host>,
    },
    Event {
        event: Event,
        host: Option<Host>,
    },
}

fn send(stream: &mut Stream, input: &Input) -> io::Result<()> {
    transport::write_packet(stream, &serde_json::to_vec(input)?)
}

pub(super) fn run(endpoint: OsString) -> io::Result<()> {
    let mut input_stream = match transport::connect(&endpoint) {
        Ok(stream) => stream,
        // The source tab or workbench may close before the terminal is started.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut stream = input_stream.try_clone()?;
    let wake = UiWake::default();
    let (frames, incoming) = mpsc::sync_channel(1);
    let (recycle, buffers) = mpsc::sync_channel::<Vec<u8>>(1);
    let (_finished, finishing) = mpsc::channel::<()>();
    let frame_wake = wake.clone();
    let receiving = std::thread::Builder::new()
        .name("window-frames".into())
        .spawn(move || {
            'receive: loop {
                let mut bytes = buffers.try_recv().unwrap_or_default();
                if transport::read_packet(&mut stream, &mut bytes).is_err() {
                    break;
                }
                loop {
                    match frames.try_send(bytes) {
                        Ok(()) => break,
                        Err(mpsc::TrySendError::Full(frame)) => {
                            if transport::peer_disconnected(&stream) {
                                break 'receive;
                            }
                            bytes = frame;
                            // A full terminal is the only case that needs a
                            // timed check. The consumer wakes us when it drains.
                            std::thread::park_timeout(Duration::from_millis(25));
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                    }
                }
                frame_wake.notify();
            }
            drop(frames);
            frame_wake.notify();
            // A terminal with flow control paused can block even the screen
            // cleanup escape sequences. Once the owner has closed, this
            // dedicated process must leave even if its host stops reading.
            if finishing.recv_timeout(Duration::from_secs(1))
                == Err(mpsc::RecvTimeoutError::Timeout)
            {
                transport::exit_frontend();
            }
        })?;

    let mut stdout = io::stdout();
    let screen = crate::TerminalScreen::enter(&mut stdout)?;
    let mut events = TerminalEventPump::start(wake.clone())?;
    let mut size = crossterm::terminal::size()?;
    if send(
        &mut input_stream,
        &Input::Hello {
            size,
            host: desktop::foreground_host(),
        },
    )
    .is_err()
    {
        return Ok(());
    }
    let mut host = None;
    loop {
        let generation = wake.generation();
        match incoming.try_recv() {
            Ok(mut bytes) => {
                receiving.thread().unpark();
                stdout.write_all(&bytes)?;
                stdout.flush()?;
                bytes.clear();
                let _ = recycle.try_send(bytes);
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        #[cfg(windows)]
        events.poll_native_mouse(true);
        let Some(event) = events.try_recv()? else {
            wake.wait_for_change(generation, None);
            continue;
        };
        let sample = match event {
            Event::Key(_) if host.is_none() => desktop::foreground_host(),
            Event::FocusGained
            | Event::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(_),
                ..
            }) => desktop::foreground_host(),
            _ => None,
        };
        let changed_host = sample.filter(|sample| host != Some(*sample));
        host = changed_host.or(host);
        // Reconcile sizes when a terminal was hidden during a resize.
        if event == Event::FocusGained
            && let Ok(new_size) = crossterm::terminal::size()
            && new_size != size
        {
            size = new_size;
            if send(
                &mut input_stream,
                &Input::Event {
                    event: Event::Resize(size.0, size.1),
                    host: changed_host,
                },
            )
            .is_err()
            {
                break;
            }
        } else if let Event::Resize(width, height) = event {
            size = (width, height);
        }
        if send(
            &mut input_stream,
            &Input::Event {
                event,
                host: changed_host,
            },
        )
        .is_err()
        {
            break;
        }
    }
    drop(screen);
    Ok(())
}

pub(super) struct Output {
    pub(super) ready: Arc<AtomicBool>,
    pub(super) frames: SyncSender<Vec<u8>>,
    pub(super) buffers: Receiver<Vec<u8>>,
}

/// Dropping the owner immediately releases blocked readers and writers.
pub(super) struct Connection(Stream);

impl Drop for Connection {
    fn drop(&mut self) {
        transport::disconnect(&self.0);
    }
}

pub(super) struct Ready {
    pub(super) connection: Connection,
    pub(super) output: Output,
    pub(super) size: (u16, u16),
    pub(super) host: Arc<AtomicU64>,
    pub(super) inputs: Receiver<io::Result<Event>>,
}

pub(super) enum Notice {
    Ready(Ready),
    Closed(io::Result<()>),
}

pub(super) struct Launch(pub(super) Arc<transport::Opening>);

impl Drop for Launch {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn serve(
    notify: impl Fn(Notice) -> io::Result<()> + Clone + Send + 'static,
    wake: UiWake,
    point: Option<ScreenPoint>,
    opening: Arc<transport::Opening>,
) -> io::Result<()> {
    let listener = transport::Listener::bind()?;
    opening.remaining()?;
    launch(&listener.endpoint(), point)?;
    accept(listener, notify, wake, opening)
}

pub(super) fn accept(
    listener: transport::Listener,
    notify: impl Fn(Notice) -> io::Result<()> + Clone + Send + 'static,
    wake: UiWake,
    opening: Arc<transport::Opening>,
) -> io::Result<()> {
    let mut reader = listener.accept(&opening)?;
    let connection = Connection(reader.try_clone()?);
    let mut writer = reader.try_clone()?;
    let mut bytes = Vec::new();
    transport::read_handshake(&mut reader, &opening, &mut bytes)?;
    drop(opening);
    let Input::Hello { size, host } = serde_json::from_slice(&bytes)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing detached terminal handshake",
        ));
    };
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(1);
    let (recycle, buffers) = mpsc::sync_channel(1);
    let ready = Arc::new(AtomicBool::new(true));
    let output = Output {
        ready: ready.clone(),
        frames: tx,
        buffers,
    };
    let writer_notice = notify.clone();
    let output_wake = wake.clone();
    std::thread::Builder::new()
        .name("window-output".into())
        .spawn(move || {
            for mut frame in rx {
                if let Err(error) = transport::write_packet(&mut writer, &frame) {
                    let _ = writer_notice(Notice::Closed(Err(error)));
                    return;
                }
                frame.clear();
                let _ = recycle.try_send(frame);
                ready.store(true, Ordering::Release);
                output_wake.notify();
            }
        })?;
    let (input_sender, inputs) = mpsc::sync_channel(256);
    let window_host = Arc::new(AtomicU64::new(host.unwrap_or(0)));
    notify(Notice::Ready(Ready {
        connection,
        output,
        size,
        host: window_host.clone(),
        inputs,
    }))?;
    while transport::read_packet(&mut reader, &mut bytes).is_ok() {
        let Input::Event { event, host } = serde_json::from_slice(&bytes)? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected window handshake",
            ));
        };
        if let Some(value) = host {
            window_host.store(value, Ordering::Release);
        }
        if input_sender.send(Ok(event)).is_err() {
            break;
        }
        wake.notify();
    }
    Ok(())
}

pub(super) fn start(
    wake: UiWake,
    notify: impl Fn(Notice) -> io::Result<()> + Clone + Send + 'static,
    point: Option<ScreenPoint>,
) -> io::Result<Launch> {
    let opening = Arc::new(transport::Opening::new()?);
    let lifetime = Launch(opening.clone());
    std::thread::Builder::new()
        .name("detached-terminal".into())
        .spawn(move || {
            let result = serve(notify.clone(), wake, point, opening);
            let _ = notify(Notice::Closed(result));
        })?;
    Ok(lifetime)
}

#[cfg(windows)]
fn launch(endpoint: &OsStr, point: Option<ScreenPoint>) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

    let exe = std::env::current_exe()?;
    let mut terminal = Command::new("wt.exe");
    terminal.args(["-w", "new"]);
    if let Some(point) = point {
        let (x, y) = desktop::launch_position(point);
        terminal.args(["--pos", &format!("{x},{y}")]);
    }
    terminal.args(["new-tab", "--title", "VTCode — Detached"]);
    if !exe.as_os_str().as_encoded_bytes().contains(&b';')
        && crate::external_open::launch(terminal.arg(&exe).arg("--detached-window").arg(endpoint))
            .is_ok()
    {
        return Ok(());
    }

    // Launch VTCode's console subsystem directly when the Terminal alias is
    // unavailable. Leave stdio attached to the new console.
    let mut child = Command::new(exe)
        .arg("--detached-window")
        .arg(endpoint)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(unix)]
fn launch(endpoint: &OsStr, _point: Option<ScreenPoint>) -> io::Result<()> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no desktop terminal display is available",
        ));
    }
    let exe = std::env::current_exe()?;
    for (terminal, args) in [
        ("xdg-terminal-exec", &["--"][..]),
        ("x-terminal-emulator", &["-e"][..]),
        ("gnome-terminal", &["--window", "--"][..]),
        ("konsole", &["--separate", "-e"][..]),
        ("xterm", &["-e"][..]),
    ] {
        let result = crate::external_open::launch(
            Command::new(terminal)
                .args(args)
                .arg(&exe)
                .arg("--detached-window")
                .arg(endpoint),
        );
        match result {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            result => return result,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no desktop terminal could launch a detached VTCode window",
    ))
}
