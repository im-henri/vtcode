//! Native clipboard ownership and the terminal clipboard used over SSH.
//! A remote desktop clipboard is not the clipboard of the connecting terminal.
use std::cell::RefCell;
use std::io::{self, IsTerminal, Write};

pub(crate) const MAX_TERMINAL_CLIPBOARD_BYTES: usize = 10 * 1024 * 1024;

#[derive(Default)]
struct TerminalClipboard {
    active: bool,
    text: Option<String>,
}

enum CopyKind {
    Text,
    Line(String),
}

thread_local! {
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    static COPY_KIND: RefCell<Option<CopyKind>> = const { RefCell::new(None) };
    static LAST_WRITE_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
    static TERMINAL_CLIPBOARD: RefCell<TerminalClipboard> = const {
        RefCell::new(TerminalClipboard { active: false, text: None })
    };
}

#[cfg(test)]
thread_local! {
    static TEST_TERMINAL_OUTPUT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

fn remote_session(mut has_variable: impl FnMut(&str) -> bool) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .into_iter()
        .any(&mut has_variable)
}

fn uses_terminal_clipboard() -> bool {
    #[cfg(test)]
    if TEST_TERMINAL_OUTPUT.with(|slot| slot.borrow().is_some()) {
        return true;
    }
    TERMINAL_CLIPBOARD.with(|slot| slot.borrow().active)
        || remote_session(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

fn can_write_terminal_clipboard() -> bool {
    io::stdout().is_terminal() && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
}

/// Send a copy request, never a clipboard-read request. The terminal's Paste
/// command supplies external text through the existing bracketed-paste path.
pub(crate) fn write_osc52(writer: &mut impl Write, text: &str) -> io::Result<()> {
    if text.len() > MAX_TERMINAL_CLIPBOARD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal clipboard text exceeds 10 MiB",
        ));
    }
    writer.write_all(b"\x1b]52;c;")?;
    // Stream the encoding instead of allocating another full-size copy.
    {
        let mut encoder = base64::write::EncoderWriter::new(
            &mut *writer,
            &base64::engine::general_purpose::STANDARD,
        );
        encoder.write_all(text.as_bytes())?;
        encoder.finish()?;
    }
    writer.write_all(b"\x07")?;
    writer.flush()
}

/// Accept clipboard writes from a nested terminal application. Queries never
/// read or expose the host clipboard; malformed requests leave it unchanged.
pub(crate) fn decode_osc52(params: &[&[u8]]) -> Option<String> {
    use base64::Engine;
    if params.len() != 3
        || params[0] != b"52"
        || !params[1].iter().all(|byte| b"cpqs01234567".contains(byte))
        || !(params[1].is_empty() || params[1].contains(&b'c') || params[1].contains(&b's'))
        || params[2].len() > MAX_TERMINAL_CLIPBOARD_BYTES.div_ceil(3) * 4
    {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(params[2])
        .ok()?;
    if bytes.len() > MAX_TERMINAL_CLIPBOARD_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn write_terminal(text: &str) -> Result<(), arboard::Error> {
    // Keep one shared copy even if terminal output fails, so switching tabs
    // cannot lose a cut or cause a paste from the remote machine's clipboard.
    TERMINAL_CLIPBOARD.with(|slot| {
        let mut clipboard = slot.borrow_mut();
        clipboard.active = true;
        clipboard.text = Some(text.to_owned());
    });
    remember_line_copy(text, false);
    let write = || {
        #[cfg(test)]
        if let Some(result) = TEST_TERMINAL_OUTPUT.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .map(|writer| write_osc52(writer, text))
        }) {
            return result;
        }
        if !can_write_terminal_clipboard() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "terminal clipboard output is unavailable",
            ));
        }
        write_osc52(&mut io::stdout().lock(), text)
    };
    write().map_err(|error| arboard::Error::Unknown {
        description: error.to_string(),
    })
}

fn with_clipboard<T>(
    operation: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, arboard::Error> {
    CLIPBOARD.with(|slot| {
        let mut clipboard = slot.borrow_mut();
        if clipboard.is_none() {
            // A failed connection is retried on the next action; headless
            // editing can still use the editor's existing internal clipboard.
            *clipboard = Some(arboard::Clipboard::new()?);
        }
        operation(clipboard.as_mut().unwrap())
    })
}

pub(crate) fn set_text(text: impl AsRef<str>) -> Result<(), arboard::Error> {
    let text = text.as_ref();
    let result = if uses_terminal_clipboard() {
        write_terminal(text)
    } else {
        with_clipboard(|clipboard| clipboard.set_text(text)).or_else(|error| {
            // Headless Unix sessions (including WSL without a display) can
            // still copy through their terminal. Windows keeps its native path.
            if cfg!(unix) && can_write_terminal_clipboard() {
                write_terminal(text)
            } else {
                Err(error)
            }
        })
    };
    if result.is_ok() {
        remember_line_copy(text, false);
    }
    LAST_WRITE_ERROR.with(|slot| *slot.borrow_mut() = result.as_ref().err().map(ToString::to_string));
    result
}

pub(crate) fn last_write_error() -> Option<String> { LAST_WRITE_ERROR.with(|slot| slot.borrow().clone()) }

/// Line-copy semantics belong to the clipboard, not to the source editor tab.
/// Other copy actions clear them; paste also verifies the actual clipboard text.
pub(crate) fn remember_line_copy(text: &str, is_line: bool) {
    COPY_KIND.with(|slot| {
        *slot.borrow_mut() = Some(if is_line {
            CopyKind::Line(if text.contains('\r') {
                text.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                text.to_owned()
            })
        } else {
            CopyKind::Text
        })
    });
}

pub(crate) fn line_copy_status(text: &str) -> Option<bool> {
    COPY_KIND.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|kind| match kind {
                CopyKind::Text => false,
                CopyKind::Line(line) => {
                    if !text.contains('\r') { return line == text; }
                    // Clipboard providers may change CRLF to LF. Compare the
                    // normalized stream without allocating another huge line.
                    let mut bytes = text.bytes().peekable();
                    line.bytes().eq(std::iter::from_fn(|| bytes.next().map(|byte| {
                        if byte == b'\r' {
                            if bytes.peek() == Some(&b'\n') { bytes.next(); }
                            b'\n'
                        } else { byte }
                    })))
                }
            })
    })
}

pub(crate) fn get_text() -> Result<String, arboard::Error> {
    if uses_terminal_clipboard() {
        return TERMINAL_CLIPBOARD.with(|slot| {
            slot.borrow()
                .text
                .clone()
                .ok_or(arboard::Error::ContentNotAvailable)
        });
    }
    with_clipboard(arboard::Clipboard::get_text)
}

/// Remember text supplied by the client terminal without echoing it back as
/// a copy request. Subsequent Ctrl+V also sees external clipboard updates.
pub(crate) fn receive_terminal_paste(text: &str) {
    if uses_terminal_clipboard() {
        TERMINAL_CLIPBOARD.with(|slot| slot.borrow_mut().text = Some(text.to_owned()));
    }
}

/// Drop the handle explicitly when main returns, including early errors.
pub(crate) struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        CLIPBOARD.with(|slot| {
            slot.borrow_mut().take();
        });
        COPY_KIND.with(|slot| {
            slot.borrow_mut().take();
        });
        LAST_WRITE_ERROR.with(|slot| { slot.borrow_mut().take(); });
        TERMINAL_CLIPBOARD.with(|slot| *slot.borrow_mut() = TerminalClipboard::default());
    }
}

#[cfg(test)]
pub(crate) fn test_terminal_clipboard() -> impl Drop {
    struct Restore(TerminalClipboard, Option<CopyKind>, Option<Vec<u8>>, Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TERMINAL_CLIPBOARD.with(|slot| *slot.borrow_mut() = std::mem::take(&mut self.0));
            COPY_KIND.with(|slot| *slot.borrow_mut() = self.1.take());
            TEST_TERMINAL_OUTPUT.with(|slot| *slot.borrow_mut() = self.2.take());
            LAST_WRITE_ERROR.with(|slot| *slot.borrow_mut() = self.3.take());
        }
    }
    Restore(
        TERMINAL_CLIPBOARD.with(|slot| std::mem::take(&mut *slot.borrow_mut())),
        COPY_KIND.with(|slot| slot.borrow_mut().take()),
        TEST_TERMINAL_OUTPUT.with(|slot| slot.replace(Some(Vec::new()))),
        LAST_WRITE_ERROR.with(|slot| slot.borrow_mut().take()),
    )
}

#[cfg(test)]
pub(crate) fn test_terminal_output() -> Vec<u8> {
    TEST_TERMINAL_OUTPUT.with(|slot| slot.borrow().clone().unwrap())
}

#[cfg(test)]
#[path = "clipboard_remote_tests.rs"]
mod remote_tests;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const TEXT: &str = "Linux clipboard: 世界 👋\nsecond line";

    #[test]
    #[ignore = "requires an isolated X11/Wayland display"]
    fn copied_text_remains_available_to_another_process() {
        let _guard = ClipboardGuard;
        set_text(TEXT).unwrap();
        // An independent process must request the selection from our live
        // owner, rather than accidentally reading an editor's internal copy.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "clipboard::tests::external_reader",
                "--ignored",
                "--nocapture",
            ])
            .env("VTCODE_CLIPBOARD_READER", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(get_text().unwrap(), TEXT);
    }

    #[test]
    #[ignore = "subprocess fixture for Linux clipboard ownership"]
    fn external_reader() {
        assert!(std::env::var_os("VTCODE_CLIPBOARD_READER").is_some());
        assert_eq!(arboard::Clipboard::new().unwrap().get_text().unwrap(), TEXT);
    }
}
