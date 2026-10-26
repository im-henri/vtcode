//! Windows Terminal sends VT paste delimiters as synthetic KEY_EVENT_RECORDs
//! (virtual key code zero). Decode their UTF-16 before normal key translation,
//! which otherwise drops ESC/control characters and applies typing semantics.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm_winapi::{InputRecord, KeyEventRecord};
use winapi::um::winuser::VK_MENU;

use crate::event::sys::windows::parse::handle_key_event;
use crate::event::{Event, KeyboardEnhancementFlags};
use super::super::paste::PasteBuffer;

#[path = "../sys/unix/parse.rs"]
mod vt_parse;

// The shared VT parser also recognizes terminal query responses. Windows
// queries use native APIs, so only ordinary input events are forwarded here.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VtEvent {
    Event(Event),
    CursorPosition(u16, u16),
    KeyboardEnhancementFlags(KeyboardEnhancementFlags),
    PrimaryDeviceAttributes,
}

const START: &[u8] = b"\x1b[200~";
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(100);
const INPUT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct PasteReader {
    transport: Vec<KeyEventRecord>,
    prefix: Vec<u8>,
    text: Option<PasteBuffer>,
    saw_win32: bool,
    expect_win32: bool,
    last_input: Option<Instant>,
    ready: VecDeque<Event>,
}

impl PasteReader {
    pub(super) fn receiving(&self) -> bool { self.text.is_some() }

    pub(super) fn allocation_failed(&mut self) {
        if let Some(text) = &mut self.text {
            if let Some(event) = text.fail(crate::event::PasteFailure::AllocationFailed) {
                self.ready.push_back(event);
            }
        }
    }

    pub(super) fn begin_batch(&mut self) {
        self.last_input = Some(Instant::now());
        self.check_cancellation();
    }

    fn check_cancellation(&mut self) {
        if let Some(text) = &mut self.text {
            if crate::event::take_paste_cancellation() {
                if let Some(event) = text.fail(crate::event::PasteFailure::Cancelled) {
                    self.ready.push_back(event);
                }
            }
        }
    }

    pub(super) fn set_win32_input_expected(&mut self, expected: bool) {
        self.expect_win32 = expected;
    }

    pub(super) fn pop(&mut self) -> Option<Event> {
        self.ready.pop_front()
    }

    pub(super) fn push(&mut self, record: KeyEventRecord, surrogate: &mut Option<u16>) {
        // Production stamps each native read batch once, not every UTF-16
        // unit in a multi-million-character paste. Direct parser users still
        // initialize the short escape-sequence timeout here.
        self.last_input.get_or_insert_with(Instant::now);
        // W32IM keeps the original virtual key, scan code, modifiers, release
        // and repeat count inside CSI Vk;Sc;Uc;Kd;Cs;Rc_. Unwrap it before paste
        // processing: even the paste delimiters/payload can be wrapped records.
        // OpenConsole can mix raw text/delimiters and W32IM control keys in a
        // single paste. Always decode transport before collecting paste text;
        // the start delimiter does not determine the format of later records.
        if record.virtual_key_code != 0 {
            self.flush_transport(surrogate);
            self.push_decoded(record, surrogate);
            return;
        }
        if !record.key_down {
            return;
        }
        if self.transport.is_empty() && record.u_char != 27 {
            self.push_decoded(record, surrogate);
            return;
        }
        let unit = record.u_char;
        self.transport.push(record);
        let len = self.transport.len();
        // Previous units have already passed this check. Re-scanning the
        // complete prefix on every unit makes Win32-encoded paste expensive.
        let candidate = len == 1 || (self.transport[1].u_char == 91
            && (len == 2 || unit == 59 || (48..=57).contains(&unit)));
        if candidate && len < 96 {
            return;
        }
        if len >= 3 && self.transport[1].u_char == 91 && self.transport[len - 1].u_char == 95 {
            if let Some(decoded) = decode_win32(self.transport[2..len - 1].iter().map(|r| r.u_char)) {
                self.transport.clear();
                self.saw_win32 = true;
                self.push_decoded(decoded, surrogate);
                return;
            }
        }
        self.flush_transport(surrogate);
    }

    fn flush_transport(&mut self, surrogate: &mut Option<u16>) {
        for record in std::mem::take(&mut self.transport) {
            self.push_decoded(record, surrogate);
        }
    }

    fn push_decoded(&mut self, record: KeyEventRecord, surrogate: &mut Option<u16>) {
        let synthetic = record.virtual_key_code == 0;
        // A physical Escape cancels an interrupted/oversized paste. A literal
        // ESC in synthetic paste data still belongs to the payload.
        if self.text.is_some() && record.virtual_key_code == 0x1b && record.key_down {
            if let Some(event) = self.text.as_mut().unwrap().fail(crate::event::PasteFailure::Cancelled) {
                self.ready.push_back(event);
            }
            *surrogate = None;
            return;
        }
        if self.text.is_some() || synthetic {
            // Synthetic release records must not duplicate the payload.
            // ConPTY synthesizes modifier presses around punctuation/control
            // characters. Those records have no text and are not paste data.
            // A text-bearing Alt release completes Unicode composition. Older
            // ConPTY uses these for unmapped characters and emoji surrogates.
            let alt_code =
                !record.key_down && record.virtual_key_code == VK_MENU as u16 && record.u_char != 0;
            if (record.key_down || alt_code) && (synthetic || record.u_char != 0) {
                for _ in 0..record.repeat_count.max(1) {
                    self.push_unit(record.u_char, surrogate);
                }
            }
        } else {
            // A real Escape/shortcut is never delayed while waiting for paste.
            self.parse(false);
            if let Some(event) = handle_key_event(record, surrogate) {
                self.ready.push_back(event);
            }
        }
    }

    fn push_unit(&mut self, unit: u16, surrogate: &mut Option<u16>) {
        if let Some(high) = surrogate.take() {
            if (0xD800..=0xDBFF).contains(&high) && (0xDC00..=0xDFFF).contains(&unit) {
                let scalar = 0x10000 + ((high as u32 - 0xD800) << 10) + (unit as u32 - 0xDC00);
                self.push_char(char::from_u32(scalar).unwrap());
                return;
            }
            // An unmatched high surrogate must not consume the following unit.
            self.push_char(char::REPLACEMENT_CHARACTER);
        }
        if (0xD800..=0xDBFF).contains(&unit) {
            *surrogate = Some(unit);
            return;
        }
        self.push_char(char::from_u32(unit as u32).unwrap_or(char::REPLACEMENT_CHARACTER));
    }

    fn push_char(&mut self, ch: char) {
        if let Some(text) = &mut self.text {
            for byte in ch.encode_utf8(&mut [0; 4]).bytes() {
                if let Some(event) = text.push(byte) { self.ready.push_back(event); }
            }
            if text.finished() { self.text = None; }
            return;
        }
        // Keep the second ESC available as the start of the next sequence.
        if ch == '\x1b' && self.prefix == b"\x1b" {
            self.parse(false);
        }
        self.prefix
            .extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
        if self.prefix.len() > 128 {
            self.prefix.clear();
            return;
        }
        if self.prefix == START {
            self.prefix.clear();
            crate::event::take_paste_cancellation();
            self.text = Some(PasteBuffer::default());
        } else {
            self.parse(true);
        }
    }

    fn idle_limit(&self) -> Option<Duration> {
        if let Some(text) = &self.text {
            return text.waiting().then_some(Duration::from_millis(200));
        }
        if self.text.is_none() && self.transport.is_empty() && self.prefix.is_empty() {
            return None;
        }
        let bare_escape = self.transport.len() == 1 || self.prefix == b"\x1b";
        Some(
            if self.text.is_none() && !self.saw_win32 && !self.expect_win32 && bare_escape {
                ESCAPE_TIMEOUT
            } else {
                INPUT_TIMEOUT
            },
        )
    }

    pub(super) fn wait_timeout(&self, timeout: Option<Duration>) -> Option<Duration> {
        if let Some(text) = &self.text {
            return if text.waiting() {
                Some(timeout.map_or(Duration::from_millis(200), |wait| wait.min(Duration::from_millis(200))))
            } else { timeout };
        }
        let remaining = self.idle_limit().zip(self.last_input).map(|(limit, last)| {
            // Windows' wait API rounds down to milliseconds. Round up here to
            // avoid spinning for the final fractional millisecond.
            limit.saturating_sub(last.elapsed()) + Duration::from_millis(1)
        });
        match (timeout, remaining) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub(super) fn flush_escape(&mut self, surrogate: &mut Option<u16>) {
        // A slow or stalled paste is not a malformed key sequence. Wait for
        // its delimiter or cancellation; never reinterpret its tail as keys.
        if self.text.is_some() {
            self.check_cancellation();
            return;
        }
        let Some((limit, last)) = self.idle_limit().zip(self.last_input) else {
            return;
        };
        if last.elapsed() < limit {
            return;
        }
        // Queue exhaustion alone does not mean a sequence ended: even the
        // very first Win32 packet can span reads. Only resolve a legacy bare
        // Escape after a grace period; discard other incomplete sequences.
        if self.text.is_none() && !self.saw_win32 && !self.expect_win32 {
            if self.transport.len() == 1 {
                self.flush_transport(surrogate);
            }
            if self.prefix == b"\x1b" {
                self.parse(false);
            }
        }
        self.reset(surrogate);
    }

    fn reset(&mut self, surrogate: &mut Option<u16>) {
        self.transport.clear();
        self.prefix.clear();
        self.text = None;
        self.last_input = None;
        *surrogate = None;
    }

    fn parse(&mut self, more: bool) {
        match vt_parse::parse_event(&self.prefix, more) {
            Ok(Some(event)) => {
                self.prefix.clear();
                if let VtEvent::Event(event) = event {
                    self.ready.push_back(event);
                }
            }
            Ok(None) => {}
            Err(_) => self.prefix.clear(),
        }
    }
}

fn decode_win32(units: impl IntoIterator<Item = u16>) -> Option<KeyEventRecord> {
    // Six bounded decimal fields: parse directly from the input records.
    // Do not allocate a UTF-16 vector and a String for every encoded key.
    let mut values = [0u16; 6];
    let mut field = 0;
    let mut value: Option<u16> = None;
    for unit in units {
        if unit == 59 {
            if field == 5 { return None; }
            values[field] = value.take().unwrap_or(0);
            field += 1;
        } else if (48..=57).contains(&unit) {
            value = Some(value.unwrap_or(0).checked_mul(10)?.checked_add(unit - 48)?);
        } else {
            return None;
        }
    }
    if field != 5 { return None; }
    values[5] = value.unwrap_or(1);
    if values[3] > 1 { return None; }
    let mut native = winapi::um::wincon::INPUT_RECORD::default();
    native.EventType = winapi::um::wincon::KEY_EVENT;
    // SAFETY: EventType selects KeyEvent; initialize its fields before conversion.
    unsafe {
        let key = native.Event.KeyEvent_mut();
        key.wVirtualKeyCode = values[0];
        key.wVirtualScanCode = values[1];
        *key.uChar.UnicodeChar_mut() = values[2];
        key.bKeyDown = i32::from(values[3]);
        key.dwControlKeyState = u32::from(values[4]);
        key.wRepeatCount = values[5];
    }
    match InputRecord::from(native) {
        InputRecord::KeyEvent(record) => Some(record),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(unit: u16, vk: u16) -> KeyEventRecord {
        decode_win32(
            format!("{vk};0;{unit};1;0;1")
                .encode_utf16()
                .collect::<Vec<_>>()
                .into_iter(),
        )
        .unwrap()
    }

    fn feed(reader: &mut PasteReader, text: &str, surrogate: &mut Option<u16>) {
        for unit in text.encode_utf16() {
            reader.push(key(unit, 0), surrogate);
        }
    }

    fn expire(reader: &mut PasteReader, surrogate: &mut Option<u16>) {
        reader.last_input = Some(Instant::now() - INPUT_TIMEOUT - Duration::from_millis(1));
        reader.flush_escape(surrogate);
    }

    #[test]
    fn win32_decimal_fields_are_bounded_and_preserve_empty_parameter_defaults() {
        let empty = decode_win32(";;;;;".encode_utf16()).unwrap();
        assert_eq!(empty.repeat_count, 1);
        assert!(!empty.key_down);
        let largest = decode_win32("65535;65535;65535;1;65535;65535".encode_utf16()).unwrap();
        assert_eq!(largest.repeat_count, 65535);
        assert_eq!(largest.u_char, 65535);
        for malformed in ["", "0;0;0;1;0", "0;0;0;1;0;1;", "0;0;0;2;0;1", "0;0;65536;1;0;1", "0;0;-1;1;0;1", "0;0;🦀;1;0;1"] {
            assert!(decode_win32(malformed.encode_utf16()).is_none(), "{malformed}");
        }
    }

    #[test]
    fn first_packet_survives_every_split_and_empty_read() {
        let packet = "\x1b[80;25;16;1;24;1_";
        for split in 1..packet.len() {
            let mut reader = PasteReader::default();
            let mut surrogate = None;
            feed(&mut reader, &packet[..split], &mut surrogate);
            reader.flush_escape(&mut surrogate);
            assert_eq!(reader.pop(), None, "split {split}");
            feed(&mut reader, &packet[split..], &mut surrogate);
            assert_eq!(
                reader.pop(),
                Some(Event::Key(KeyEvent::new(
                    KeyCode::Char('P'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ))),
                "split {split}"
            );
            assert_eq!(reader.pop(), None);
        }
    }

    #[test]
    fn requested_win32_mode_preserves_first_escape_beyond_legacy_timeout() {
        let mut reader = PasteReader::default();
        reader.set_win32_input_expected(true);
        let mut surrogate = None;
        feed(&mut reader, "\x1b", &mut surrogate);
        reader.last_input = Some(Instant::now() - Duration::from_millis(500));
        reader.flush_escape(&mut surrogate);
        assert_eq!(reader.pop(), None);
        feed(&mut reader, "[80;25;16;1;24;1_", &mut surrogate);
        assert_eq!(
            reader.pop(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )))
        );
    }

    #[test]
    fn first_paste_marker_survives_every_split() {
        for split in 1..START.len() {
            let mut reader = PasteReader::default();
            let mut surrogate = None;
            feed(&mut reader, &"\x1b[200~"[..split], &mut surrogate);
            reader.flush_escape(&mut surrogate);
            assert_eq!(reader.pop(), None);
            feed(
                &mut reader,
                &format!("{}text\x1b[201~", &"\x1b[200~"[split..]),
                &mut surrogate,
            );
            assert_eq!(reader.pop(), Some(Event::Paste("text".into())));
        }
    }

    #[test]
    fn legacy_escape_has_a_bounded_wait() {
        let mut reader = PasteReader::default();
        let mut surrogate = None;
        feed(&mut reader, "\x1b", &mut surrogate);
        reader.flush_escape(&mut surrogate);
        assert_eq!(reader.pop(), None);
        assert!(reader.wait_timeout(None).unwrap() <= ESCAPE_TIMEOUT + Duration::from_millis(1));
        expire(&mut reader, &mut surrogate);
        assert_eq!(reader.pop(), Some(Event::Key(KeyCode::Esc.into())));
        assert_eq!(reader.wait_timeout(None), None);
    }

    #[test]
    fn physical_escape_cancels_paste_and_drains_its_tail_before_shortcuts_resume() {
        let mut reader = PasteReader::default();
        let mut surrogate = None;
        feed(&mut reader, "\x1b[200~unfinished", &mut surrogate);
        feed(&mut reader, "\x1b[27;1;27;1;0;1_", &mut surrogate);
        assert!(matches!(reader.pop(), Some(Event::PasteStatus(crate::event::PasteStatus::Failed {
            reason: crate::event::PasteFailure::Cancelled, ..
        }))));
        feed(&mut reader, "\x03\x11\x1b[201~", &mut surrogate);
        assert_eq!(reader.pop(), None);
        feed(&mut reader, "\x1b[80;25;16;1;24;1_", &mut surrogate);
        assert_eq!(
            reader.pop(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )))
        );
        assert_eq!(reader.pop(), None);
    }

    #[test]
    fn truncated_transport_recovers_without_expiring_a_slow_paste() {
        for input in ["\x1b[80;25;"] {
            let mut reader = PasteReader::default();
            let mut surrogate = None;
            feed(&mut reader, input, &mut surrogate);
            assert!(reader.wait_timeout(None).is_some());
            expire(&mut reader, &mut surrogate);
            assert_eq!(reader.pop(), None);
            feed(&mut reader, "x\x1b[200~next\x1b[201~", &mut surrogate);
            assert_eq!(reader.pop(), Some(Event::Key(KeyCode::Char('x').into())));
            assert_eq!(reader.pop(), Some(Event::Paste("next".into())));
        }
        let mut reader = PasteReader::default();
        let mut surrogate = None;
        feed(&mut reader, "\x1b[200~unfinished", &mut surrogate);
        expire(&mut reader, &mut surrogate);
        assert_eq!(reader.pop(), None);
        assert_eq!(reader.wait_timeout(None), Some(Duration::from_millis(200)));
        feed(&mut reader, " tail\x1b[201~", &mut surrogate);
        assert_eq!(reader.pop(), Some(Event::Paste("unfinished tail".into())));
    }

    #[test]
    fn failed_paste_is_drained_without_emitting_keys() {
        let mut reader = PasteReader::default();
        let mut surrogate = None;
        reader.text = Some(PasteBuffer::default());
        reader.allocation_failed();
        assert!(matches!(reader.pop(), Some(Event::PasteStatus(crate::event::PasteStatus::Failed { .. }))));
        reader.allocation_failed();
        assert_eq!(reader.pop(), None);
        feed(&mut reader, "more data and \x03\x13\x1b[20z", &mut surrogate);
        assert_eq!(reader.pop(), None);
        feed(
            &mut reader,
            "\x1b[201~\x1b[200~next\x1b[201~",
            &mut surrogate,
        );
        assert_eq!(reader.pop(), Some(Event::Paste("next".into())));
        assert_eq!(reader.pop(), None);
    }

    #[test]
    fn paste_above_the_old_size_limit_is_accepted() {
        let mut reader = PasteReader::default();
        let mut surrogate = None;
        reader.text = Some(PasteBuffer::default());
        let length = 10 * 1024 * 1024 + 1;
        for _ in 0..length { reader.push_unit(b'x' as u16, &mut surrogate); }
        feed(&mut reader, "\x1b[201~", &mut surrogate);
        let mut progress = 0;
        loop {
            match reader.pop() {
                Some(Event::PasteStatus(crate::event::PasteStatus::Receiving { .. })) => progress += 1,
                Some(Event::Paste(text)) => { assert_eq!(text.len(), length); break; }
                result => panic!("expected completed paste, got {result:?}"),
            }
        }
        assert!(progress > 0 && progress < 100);
        assert_eq!(reader.pop(), None);
    }

    #[test]
    fn malformed_utf16_keeps_following_text_and_valid_pairs() {
        for (units, expected) in [
            (vec![0xd800, b'x' as u16], "\u{fffd}x"),
            (vec![0xd800, 0xd83e, 0xdd80], "\u{fffd}🦀"),
            (vec![0xdc00, b'x' as u16], "\u{fffd}x"),
            (vec![0xd83e, 0xdd80, b'x' as u16], "🦀x"),
        ] {
            let mut reader = PasteReader::default();
            let mut surrogate = None;
            for unit in units {
                reader.push(key(unit, 0), &mut surrogate);
            }
            for ch in expected.chars() {
                assert_eq!(reader.pop(), Some(Event::Key(KeyCode::Char(ch).into())));
            }
            assert_eq!(reader.pop(), None);
        }
    }

    #[test]
    fn encoded_literal_packets_are_not_decoded_twice() {
        let mut reader = PasteReader::default();
        let mut surrogate = None;
        let payload = "literal \x1b[13;28;13;1;0;1_ text";
        for unit in format!("\x1b[200~{payload}\x1b[201~").encode_utf16() {
            feed(
                &mut reader,
                &format!("\x1b[0;0;{unit};1;0;1_"),
                &mut surrogate,
            );
        }
        assert_eq!(reader.pop(), Some(Event::Paste(payload.into())));
    }
}
