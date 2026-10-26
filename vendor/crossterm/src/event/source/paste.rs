//! Linear, size-unlimited bracketed-paste collection. Keep UTF-8 bytes once,
//! retain only a possible closing delimiter, and move the allocation into the
//! completed event. Allocation failure drains the rest as data, never shortcuts.
use crate::event::{Event, PasteFailure, PasteStatus};

const END: &[u8] = b"\x1b[201~";
const PROGRESS_BYTES: usize = 512 * 1024;

#[derive(Debug)]
pub(super) struct PasteBuffer {
    bytes: Vec<u8>,
    pending: usize,
    received: usize,
    next_progress: usize,
    failed: bool,
    finished: bool,
}

impl Default for PasteBuffer {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            pending: 0,
            received: 0,
            next_progress: PROGRESS_BYTES,
            failed: false,
            finished: false,
        }
    }
}

impl PasteBuffer {
    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    pub(super) fn waiting(&self) -> bool {
        !self.failed && !self.finished
    }

    pub(super) fn fail(&mut self, reason: PasteFailure) -> Option<Event> {
        if self.failed {
            return None;
        }
        self.failed = true;
        self.bytes = Vec::new();
        Some(Event::PasteStatus(PasteStatus::Failed {
            bytes: self.received,
            reason,
        }))
    }

    pub(super) fn push(&mut self, byte: u8) -> Option<Event> {
        if byte == END[self.pending] {
            self.pending += 1;
            if self.pending != END.len() {
                return None;
            }
            self.pending = 0;
            self.finished = true;
            if self.failed {
                return None;
            }
            let bytes = std::mem::take(&mut self.bytes);
            let text = match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
            };
            return Some(Event::Paste(text));
        }
        let pending = self.pending;
        self.pending = usize::from(byte == END[0]);
        let extra = pending + usize::from(self.pending == 0);
        self.received = self.received.saturating_add(extra);
        if self.failed {
            return None;
        }
        if self.bytes.try_reserve(extra).is_err() {
            return self.fail(PasteFailure::AllocationFailed);
        }
        self.bytes.extend_from_slice(&END[..pending]);
        if self.pending == 0 {
            self.bytes.push(byte);
        }
        if self.received >= self.next_progress {
            self.next_progress = self.received.saturating_add(PROGRESS_BYTES);
            return Some(Event::PasteStatus(PasteStatus::Receiving {
                bytes: self.received,
            }));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiters_fragments_literal_escapes_and_unicode_are_lossless() {
        for text in [
            "",
            "é🦀\r\n\tend",
            "\x1b\x1b[20oops\x1b[201X",
            "\x03\x11\x1b[200~literal",
        ] {
            let mut paste = PasteBuffer::default();
            for byte in text.bytes() {
                assert!(paste.push(byte).is_none());
            }
            let mut result = None;
            for &byte in END {
                result = paste.push(byte).or(result);
            }
            assert_eq!(result, Some(Event::Paste(text.into())));
            assert!(paste.finished());
        }
    }

    #[test]
    fn million_line_paste_exceeds_old_limit_and_reuses_its_utf8_allocation() {
        let line = "row é🦀0123456789\n";
        let mut paste = PasteBuffer::default();
        let mut updates = 0;
        for _ in 0..1_000_000 {
            for byte in line.bytes() {
                if let Some(event) = paste.push(byte) {
                    assert!(matches!(
                        event,
                        Event::PasteStatus(PasteStatus::Receiving { .. })
                    ));
                    updates += 1;
                }
            }
        }
        let allocation = paste.bytes.as_ptr();
        let mut result = None;
        for &byte in END {
            result = paste.push(byte).or(result);
        }
        let Some(Event::Paste(text)) = result else {
            panic!("completed paste")
        };
        assert_eq!(text.len(), line.len() * 1_000_000);
        assert_eq!(text.as_ptr(), allocation);
        assert_eq!(text.lines().count(), 1_000_000);
        assert!(updates > 10 && updates < 100);
    }

    #[test]
    fn failed_or_cancelled_payload_is_drained_without_shortcut_replay() {
        for reason in [
            PasteFailure::AllocationFailed,
            PasteFailure::Cancelled,
            PasteFailure::Interrupted,
        ] {
            let mut paste = PasteBuffer::default();
            paste.push(b'a');
            assert_eq!(
                paste.fail(reason),
                Some(Event::PasteStatus(PasteStatus::Failed { bytes: 1, reason }))
            );
            assert!(paste.fail(reason).is_none());
            for byte in b"\x03\x11\x1b[200~still data" {
                assert!(paste.push(*byte).is_none());
            }
            for &byte in END {
                assert!(paste.push(byte).is_none());
            }
            assert!(paste.finished());
            assert!(paste.bytes.is_empty());
        }
    }
}
