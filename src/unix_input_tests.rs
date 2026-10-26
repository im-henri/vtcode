use super::*;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

const PROBE: &str = "VTCODE_KEYBOARD_PTY_TEST";
const READY: &str = "VTCODE_KEYBOARD_READY";
const PASTE: &str = "keep\x08\x7f café";

#[test]
fn unix_pty_shortcuts_and_editor_actions() {
    if let Ok(erase) = std::env::var(PROBE) {
        let erase: u8 = erase.parse().unwrap();
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .unwrap();
        // This test runs in its own PTY: never alter the caller's terminal.
        let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
        let mut mode = unsafe {
            assert_eq!(libc::tcgetattr(tty.as_raw_fd(), mode.as_mut_ptr()), 0);
            mode.assume_init()
        };
        mode.c_cc[libc::VERASE] = erase;
        unsafe {
            assert_eq!(libc::tcsetattr(tty.as_raw_fd(), libc::TCSANOW, &mode), 0);
        }
        crossterm::terminal::enable_raw_mode().unwrap();
        let protocol = KeyboardProtocol::enable(&mut io::stdout()).unwrap();
        println!("{READY}");
        io::stdout().flush().unwrap();
        let action_cases = crate::keyboard_protocol_tests::vt_cases().len();
        for (index, (sequence, mut expected)) in crate::keyboard_protocol_tests::extended_key_cases()
            .into_iter()
            .enumerate()
        {
            if erase == 8 && index < 2 {
                expected.modifiers = if index == 0 {
                    KeyModifiers::NONE
                } else {
                    KeyModifiers::CONTROL
                };
            }
            assert!(
                event::poll(Duration::from_secs(5)).unwrap(),
                "{sequence:?} timed out"
            );
            let actual = event::read().unwrap();
            assert_eq!(
                actual,
                Event::Key(expected),
                "erase={erase}, sequence={sequence:?}"
            );
            // The larger corpus checks decoder fidelity, including arbitrary
            // Alt combinations and key-up reports. The action helper describes
            // only the explicit shortcut scenarios in the original corpus.
            if index < action_cases { crate::keyboard_protocol_tests::check_key(expected); }
        }
        assert!(event::poll(Duration::from_secs(5)).unwrap());
        assert_eq!(event::read().unwrap(), Event::Paste(PASTE.into()));
        for (case, (sequence, expected)) in
            crate::keyboard_protocol_tests::fragmented_shortcut_sequences()
                .into_iter()
                .enumerate()
        {
            for split in 1..sequence.len() {
                println!("VTCODE_FRAGMENT_{case}_{split}_READY");
                io::stdout().flush().unwrap();
                // Poll between fragments: a short SSH read must neither emit
                // Escape nor block the event loop waiting for more bytes.
                assert!(
                    !event::poll(Duration::from_millis(10)).unwrap(),
                    "premature key at {sequence:?}, split {split}"
                );
                println!("VTCODE_FRAGMENT_{case}_{split}_CONTINUE");
                io::stdout().flush().unwrap();
                assert!(event::poll(Duration::from_secs(5)).unwrap());
                let actual = event::read().unwrap();
                assert_eq!(actual, Event::Key(expected), "{sequence:?}, split {split}");
                crate::keyboard_protocol_tests::check_key(expected);
            }
        }
        println!("VTCODE_ESCAPE_READY");
        io::stdout().flush().unwrap();
        // A real standalone Escape must still arrive without a subsequent key.
        assert!(event::poll(Duration::from_secs(2)).unwrap());
        assert_eq!(
            event::read().unwrap(),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        );
        assert!(!event::poll(Duration::ZERO).unwrap());
        drop(protocol);
        restore_keyboard_protocol(); // Idempotent cleanup, including the panic hook.
        crossterm::terminal::disable_raw_mode().unwrap();
        unsafe {
            assert_eq!(libc::tcgetattr(tty.as_raw_fd(), &mut mode), 0);
        }
        assert_eq!(mode.c_cc[libc::VERASE], erase);
        return;
    }

    for erase in [127, 8] {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "unix_input::tests::unix_pty_shortcuts_and_editor_actions",
            "--nocapture",
        ]);
        command.env(PROBE, erase.to_string());
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 || tx.send(bytes[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut sent = false;
        let fragments: Vec<_> = crate::keyboard_protocol_tests::fragmented_shortcut_sequences()
            .into_iter()
            .enumerate()
            .flat_map(|(case, (sequence, _))| {
                (1..sequence.len()).flat_map(move |split| {
                    [
                        (
                            format!("VTCODE_FRAGMENT_{case}_{split}_READY"),
                            &sequence[..split],
                        ),
                        (
                            format!("VTCODE_FRAGMENT_{case}_{split}_CONTINUE"),
                            &sequence[split..],
                        ),
                    ]
                })
            })
            .chain(std::iter::once(("VTCODE_ESCAPE_READY".into(), "\x1b")))
            .collect();
        let mut next_fragment = 0;
        let status = loop {
            if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(20)) {
                output.extend(bytes);
            }
            if !sent && String::from_utf8_lossy(&output).contains(READY) {
                for (sequence, _) in crate::keyboard_protocol_tests::extended_key_cases() {
                    writer.write_all(sequence.as_bytes()).unwrap();
                }
                writer
                    .write_all(format!("\x1b[200~{PASTE}\x1b[201~").as_bytes())
                    .unwrap();
                writer.flush().unwrap();
                sent = true;
            }
            if let Some((marker, fragment)) = fragments.get(next_fragment)
                && String::from_utf8_lossy(&output).contains(marker)
            {
                writer.write_all(fragment.as_bytes()).unwrap();
                writer.flush().unwrap();
                next_fragment += 1;
            }
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("PTY test timed out: {}", String::from_utf8_lossy(&output));
            }
        };
        reader.join().unwrap();
        output.extend(rx.try_iter().flatten());
        let output = String::from_utf8_lossy(&output);
        assert!(status.success(), "erase={erase}: {output}");
        assert_eq!(output.matches("\x1b[>1u").count(), 1, "{output}");
        assert_eq!(output.matches("\x1b[<1u").count(), 1, "{output}");
    }
}
