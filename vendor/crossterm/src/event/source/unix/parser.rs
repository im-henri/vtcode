use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::event::{sys::unix::parse::parse_event, Event, InternalEvent, KeyCode};

// A raw Escape is also the first byte of terminal key, mouse and paste reports.
// A short read (especially over SSH) cannot distinguish them. Only an isolated
// Escape needs this grace period; complete reports and ordinary typing are ready
// immediately. Extended keyboard protocols report Escape unambiguously too.
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(super) struct Parser {
    buffer: Vec<u8>,
    internal_events: VecDeque<InternalEvent>,
    pending_escape: Option<Instant>,
    #[cfg(feature = "bracketed-paste")]
    paste: Option<super::super::paste::PasteBuffer>,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            internal_events: VecDeque::with_capacity(128),
            pending_escape: None,
            #[cfg(feature = "bracketed-paste")]
            paste: None,
        }
    }
}

impl Parser {
    pub(super) fn advance(&mut self, buffer: &[u8]) {
        if buffer.is_empty() {
            return;
        }
        self.pending_escape = None;
        #[cfg(feature = "bracketed-paste")]
        self.check_paste_cancellation();
        for &byte in buffer {
            #[cfg(feature = "bracketed-paste")]
            if let Some(paste) = &mut self.paste {
                if let Some(event) = paste.push(byte) {
                    self.internal_events.push_back(InternalEvent::Event(event));
                }
                if paste.finished() { self.paste = None; }
                continue;
            }
            // Escape followed by another report must retain the second Escape
            // as that report's prefix, rather than swallowing both together.
            if byte == 0x1b && self.buffer == b"\x1b" {
                self.internal_events
                    .push_back(InternalEvent::Event(Event::Key(KeyCode::Esc.into())));
                self.buffer.clear();
            }
            self.buffer.push(byte);
            #[cfg(feature = "bracketed-paste")]
            if self.buffer == b"\x1b[200~" {
                crate::event::take_paste_cancellation();
                self.buffer.clear();
                self.paste = Some(Default::default());
                continue;
            }
            // More bytes may arrive in a later read, even when this read was
            // shorter than the OS buffer or the caller is only polling.
            match parse_event(&self.buffer, true) {
                Ok(Some(event)) => {
                    self.internal_events.push_back(event);
                    self.buffer.clear();
                }
                Ok(None) => {}
                Err(_) => self.buffer.clear(),
            }
        }
        if self.buffer == b"\x1b" {
            self.pending_escape = Some(Instant::now());
        }
    }

    pub(super) fn wait_timeout(&self, timeout: Option<Duration>) -> Option<Duration> {
        #[cfg(feature = "bracketed-paste")]
        if self.paste.as_ref().is_some_and(|paste| paste.waiting()) {
            return Some(timeout.map_or(Duration::from_millis(200), |wait| wait.min(Duration::from_millis(200))));
        }
        let Some(started) = self.pending_escape else {
            return timeout;
        };
        let remaining = ESCAPE_TIMEOUT.saturating_sub(started.elapsed());
        Some(timeout.map_or(remaining, |limit| limit.min(remaining)))
    }

    pub(super) fn flush_escape(&mut self) {
        #[cfg(feature = "bracketed-paste")]
        self.check_paste_cancellation();
        if self
            .pending_escape
            .is_some_and(|started| started.elapsed() >= ESCAPE_TIMEOUT)
        {
            self.pending_escape = None;
            self.buffer.clear();
            self.internal_events
                .push_back(InternalEvent::Event(Event::Key(KeyCode::Esc.into())));
        }
    }

    #[cfg(feature = "bracketed-paste")]
    fn check_paste_cancellation(&mut self) {
        if let Some(paste) = &mut self.paste {
            if crate::event::take_paste_cancellation() {
                if let Some(event) = paste.fail(crate::event::PasteFailure::Cancelled) {
                    self.internal_events.push_back(InternalEvent::Event(event));
                }
            }
        }
    }
}

impl Iterator for Parser {
    type Item = InternalEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.internal_events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Option<InternalEvent> {
        Some(InternalEvent::Event(Event::Key(KeyEvent::new(
            code, modifiers,
        ))))
    }

    #[test]
    fn shift_enter_survives_every_short_read_boundary() {
        for sequence in [b"\x1b[13;2u".as_slice(), b"\x1b[27;2;13~"] {
            for split in 1..sequence.len() {
                let mut parser = Parser::default();
                parser.advance(&sequence[..split]);
                parser.flush_escape();
                assert_eq!(parser.next(), None, "sequence {sequence:?}, split {split}");
                assert_eq!(
                    parser.wait_timeout(Some(Duration::ZERO)),
                    Some(Duration::ZERO)
                );
                parser.advance(&sequence[split..]);
                assert_eq!(parser.next(), key(KeyCode::Enter, KeyModifiers::SHIFT));
                assert_eq!(parser.next(), None);
                assert_eq!(parser.wait_timeout(None), None);
            }
        }
    }

    #[test]
    fn only_a_bare_escape_waits_and_it_expires_once() {
        let mut parser = Parser::default();
        parser.advance(b"a\x1b[13;2u\x1b[27u\x1b");
        assert_eq!(parser.next(), key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(parser.next(), key(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(parser.next(), key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(parser.next(), None);
        assert!(parser.wait_timeout(None).unwrap() <= ESCAPE_TIMEOUT);
        parser.pending_escape = Some(Instant::now() - ESCAPE_TIMEOUT);
        assert_eq!(parser.wait_timeout(None), Some(Duration::ZERO));
        parser.flush_escape();
        parser.flush_escape();
        assert_eq!(parser.next(), key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(parser.next(), None);
        assert_eq!(parser.wait_timeout(None), None);
    }

    #[test]
    fn escape_before_another_report_does_not_swallow_its_prefix() {
        let mut parser = Parser::default();
        parser.advance(b"\x1b\x1b[13;2u");
        assert_eq!(parser.next(), key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(parser.next(), key(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(parser.next(), None);
    }

    #[test]
    fn delayed_sequence_body_does_not_expire_as_escape() {
        let mut parser = Parser::default();
        parser.advance(b"\x1b");
        parser.advance(b"[");
        assert_eq!(parser.wait_timeout(None), None);
        parser.flush_escape();
        assert_eq!(parser.next(), None);
        parser.advance(b"13;2u");
        assert_eq!(parser.next(), key(KeyCode::Enter, KeyModifiers::SHIFT));
    }

    #[test]
    fn alt_enter_and_fragmented_paste_remain_distinct() {
        let mut parser = Parser::default();
        parser.advance(b"\x1b");
        parser.advance(b"\r");
        assert_eq!(parser.next(), key(KeyCode::Enter, KeyModifiers::ALT));
        #[cfg(feature = "bracketed-paste")]
        {
            for &byte in b"\x1b[200~text\x1b[13;2u\x1b[201~" {
                parser.advance(&[byte]);
            }
            assert_eq!(
                parser.next(),
                Some(InternalEvent::Event(Event::Paste("text\x1b[13;2u".into())))
            );
        }
        assert_eq!(parser.next(), None);
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn million_line_paste_survives_tty_sized_reads_and_preserves_following_shortcuts() {
        let mut parser = Parser::default();
        let line = "row é🦀0123456789\n";
        let payload = line.repeat(1_000_000);
        parser.advance(b"\x1b[200~");
        let mut received = 0;
        for chunk in payload.as_bytes().chunks(1024) {
            parser.advance(chunk);
            for event in parser.by_ref() {
                let InternalEvent::Event(Event::PasteStatus(crate::event::PasteStatus::Receiving { bytes })) = event else { panic!("payload became {event:?}") };
                assert!(bytes > received);
                received = bytes;
            }
        }
        assert!(received > 10 * 1024 * 1024);
        parser.advance(b"\x1b[201~\x13");
        assert_eq!(parser.next(), Some(InternalEvent::Event(Event::Paste(payload))));
        assert_eq!(parser.next(), key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(parser.next(), None);
        assert!(parser.buffer.capacity() <= 256, "the ordinary key buffer never stores the payload");
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn cancelled_paste_is_drained_as_data_and_slow_pastes_do_not_expire() {
        let mut parser = Parser::default();
        parser.advance(b"\x1b[200~payload");
        assert_eq!(parser.wait_timeout(None), Some(Duration::from_millis(200)));
        parser.flush_escape();
        assert_eq!(parser.next(), None);
        crate::event::cancel_paste();
        parser.flush_escape();
        assert!(matches!(parser.next(), Some(InternalEvent::Event(Event::PasteStatus(crate::event::PasteStatus::Failed { reason: crate::event::PasteFailure::Cancelled, .. })))));
        assert_eq!(parser.wait_timeout(None), None, "no periodic wakeups while draining discarded input");
        parser.advance(b"\x03\x11\x1b[200~discard\x1b[201~\x13");
        assert_eq!(parser.next(), key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(parser.next(), None);
    }
}
