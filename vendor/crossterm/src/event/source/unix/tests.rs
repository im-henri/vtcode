use super::UnixInternalEventSource;
use crate::{
    event::{source::EventSource, Event, InternalEvent, KeyCode, KeyEvent, KeyModifiers},
    terminal::sys::file_descriptor::FileDesc,
};
use std::{io::Write, os::unix::net::UnixStream, sync::mpsc, time::Duration};

fn source() -> (UnixInternalEventSource, UnixStream) {
    let (reader, writer) = UnixStream::pair().unwrap();
    // Blocking, like /dev/tty: incomplete input must return to the poller.
    #[cfg(not(feature = "libc"))]
    let fd = FileDesc::Owned(reader.into());
    #[cfg(feature = "libc")]
    let fd = {
        use std::os::fd::IntoRawFd;
        FileDesc::new(reader.into_raw_fd(), true)
    };
    (
        UnixInternalEventSource::from_file_descriptor(fd).unwrap(),
        writer,
    )
}

fn shift_enter() -> Option<InternalEvent> {
    Some(InternalEvent::Event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::SHIFT,
    ))))
}

#[test]
fn short_ui_poll_keeps_fragment_without_blocking_for_its_suffix() {
    let (mut source, mut writer) = source();
    writer.write_all(b"\x1b").unwrap();
    let (resume, wait) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        // A fallback prevents a broken blocking reader from hanging the test.
        let _ = wait.recv_timeout(Duration::from_secs(1));
        writer.write_all(b"[13;2u").unwrap();
        writer
    });
    let early = source.try_read(Some(Duration::ZERO)).unwrap();
    let _ = resume.send(());
    let _writer = writer.join().unwrap();
    assert_eq!(
        early, None,
        "incomplete Shift+Enter must not emit Escape or wait for its suffix"
    );
    assert_eq!(
        source.try_read(Some(Duration::from_secs(1))).unwrap(),
        shift_enter()
    );
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
}

#[test]
fn standalone_escape_expires_during_a_long_poll() {
    let (mut source, mut writer) = source();
    writer.write_all(b"\x1b").unwrap();
    assert_eq!(
        source.try_read(Some(Duration::from_secs(1))).unwrap(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
    );
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
}

#[test]
fn full_read_preserves_remaining_input_without_another_write() {
    let (mut source, mut writer) = source();
    let mut bytes = vec![b'x'; 1023];
    bytes.extend_from_slice(b"\x1b[13;2u");
    writer.write_all(&bytes).unwrap();
    for _ in 0..1023 {
        assert_eq!(
            source.try_read(Some(Duration::ZERO)).unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Char('x').into())))
        );
    }
    assert_eq!(
        source.try_read(Some(Duration::ZERO)).unwrap(),
        shift_enter()
    );
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
}

#[test]
#[cfg(feature = "bracketed-paste")]
fn full_reads_preserve_one_large_paste_event() {
    let (mut source, mut writer) = source();
    let text = "foo \x1b[13;2u\n".repeat(300);
    writer
        .write_all(format!("\x1b[200~{text}\x1b[201~").as_bytes())
        .unwrap();
    assert_eq!(
        source.try_read(Some(Duration::from_secs(1))).unwrap(),
        Some(InternalEvent::Event(Event::Paste(text)))
    );
    assert_eq!(source.try_read(Some(Duration::ZERO)).unwrap(), None);
}
