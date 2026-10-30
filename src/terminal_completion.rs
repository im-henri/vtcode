//! Shell command completion is signalled by OSC 133/633, never output silence.
use std::time::{Duration, Instant};

const BLINK_DURATION: Duration = Duration::from_millis(1500);
const BLINK_STEP: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Commands {
    started: u64,
    finished: u64,
    running: bool,
    exit_code: Option<i32>,
}

impl Commands {
    #[cfg(test)]
    pub(crate) fn finished_count(&self) -> u64 { self.finished }

    pub(crate) fn osc(&mut self, params: &[&[u8]]) {
        if params.len() < 2 || !matches!(params[0], b"133" | b"633") {
            return;
        }
        match params[1] {
            b"C" if !self.running => {
                self.started = self.started.wrapping_add(1);
                self.running = true;
            }
            b"D" if self.running => {
                self.finished = self.finished.wrapping_add(1);
                self.running = false;
                self.exit_code = params.get(2)
                    .and_then(|code| std::str::from_utf8(code).ok()?.parse().ok());
            }
            _ => {}
        }
    }
}

#[derive(Default)]
pub(crate) struct Notice {
    commands: Commands,
    pub(crate) unread: bool,
    blink_started: Option<Instant>,
    last_pulse: bool,
}

impl Notice {
    pub(crate) fn update(&mut self, commands: Commands, focused: bool, now: Instant) -> bool {
        let changed = commands != self.commands;
        if changed {
            if commands.started != self.commands.started || commands.running {
                self.unread = false;
                self.blink_started = None;
            }
            if commands.finished != self.commands.finished && !commands.running {
                self.unread = !focused;
                self.blink_started = Some(now);
                self.last_pulse = true;
            }
            self.commands = commands;
        }
        let acknowledged = focused && self.acknowledge();
        changed || acknowledged
    }

    pub(crate) fn acknowledge(&mut self) -> bool {
        if !std::mem::take(&mut self.unread) { return false; }
        self.blink_started = None;
        self.last_pulse = false;
        true
    }

    pub(crate) fn visible(&self, now: Instant) -> bool {
        self.unread || self.blink_started.is_some_and(|start|
            now.saturating_duration_since(start) < BLINK_DURATION)
    }

    pub(crate) fn failed(&self) -> bool {
        self.commands.exit_code.is_some_and(|code| code != 0)
    }

    pub(crate) fn pulse(&self, now: Instant, animate: bool) -> bool {
        animate && self.blink_started.is_some_and(|start| {
            let elapsed = now.saturating_duration_since(start);
            elapsed < BLINK_DURATION && elapsed.as_millis() / BLINK_STEP.as_millis() % 2 == 0
        })
    }

    pub(crate) fn advance(&mut self, now: Instant, animate: bool) -> bool {
        let pulse = self.pulse(now, animate);
        let expired = self.blink_started.is_some_and(|start|
            now.saturating_duration_since(start) >= BLINK_DURATION);
        if expired {
            self.blink_started = None;
        }
        let changed = pulse != self.last_pulse || (expired && !self.unread);
        self.last_pulse = pulse;
        changed
    }

    pub(crate) fn next_wait(&self, now: Instant, animate: bool) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.blink_started?);
        if elapsed >= BLINK_DURATION {
            return (!self.unread || self.last_pulse).then_some(Duration::ZERO);
        }
        if animate {
            let phase = Duration::from_nanos((elapsed.as_nanos() % BLINK_STEP.as_nanos()) as u64);
            Some((BLINK_STEP - phase).min(BLINK_DURATION - elapsed))
        } else {
            (!self.unread).then_some(BLINK_DURATION - elapsed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalEmulator;

    #[test]
    fn terminal_completion_ignores_startup_duplicate_and_partial_sequences() {
        let mut terminal = TerminalEmulator::test_with_output(40, 4, "\x1b]633;D;0\x07");
        let mut notice = Notice::default();
        let now = Instant::now();
        assert!(!notice.update(terminal.grid.commands, false, now));
        for chunk in [b"\x1b]13".as_slice(), b"3;C\x1b", b"\\output\x1b]633;C\x07\x1b]633;D;", b"17\x07"] {
            terminal.test_queue_output(chunk);
            terminal.process_while(|| true);
        }
        assert!(notice.update(terminal.grid.commands, false, now));
        assert!(notice.unread && notice.failed());
        assert_eq!(terminal.grid.commands.started, 1);
        assert_eq!(terminal.grid.commands.finished, 1);
        terminal.test_queue_output(b"\x1b]133;D;0\x07");
        terminal.process_while(|| true);
        assert!(!notice.update(terminal.grid.commands, false, now));
        assert!(notice.failed(), "a duplicate finish must not overwrite the result");
    }

    #[test]
    fn terminal_completion_survives_screen_resets_and_alternate_screens() {
        for reset in ["\x1bc", "\x1b[?1049htext\x1b[?1049l"] {
            let terminal = TerminalEmulator::test_with_output(40, 4, &format!("\x1b]633;C\x07{reset}\x1b]633;D;0\x07"));
            let mut notice = Notice::default();
            assert!(notice.update(terminal.grid.commands, false, Instant::now()));
            assert!(notice.unread);
            assert!(!notice.failed());
        }
    }

    #[test]
    fn terminal_completion_blinks_briefly_acknowledges_and_stays_idle() {
        let mut commands = Commands::default();
        let mut notice = Notice::default();
        let start = Instant::now();
        commands.osc(&[b"633", b"C"]);
        commands.osc(&[b"633", b"D", b"0"]);
        notice.update(commands, false, start);
        assert!(notice.pulse(start, true));
        assert_eq!(notice.next_wait(start, true), Some(BLINK_STEP));
        for tick in 1..=6 {
            let now = start + BLINK_STEP * tick;
            notice.advance(now, true);
            assert!(notice.visible(now));
        }
        let later = start + Duration::from_secs(20);
        assert!(notice.unread);
        assert!(!notice.pulse(later, true));
        assert_eq!(notice.next_wait(later, true), None, "no idle animation polling");
        assert!(notice.acknowledge());
        assert!(!notice.visible(later));
        assert!(!notice.acknowledge());

        commands.osc(&[b"633", b"C"]);
        notice.update(commands, false, later);
        assert!(!notice.visible(later), "a new command clears the previous result");
        commands.osc(&[b"633", b"D", b"0"]);
        notice.update(commands, true, later);
        assert!(!notice.unread, "watching the terminal acknowledges completion");
        assert!(notice.visible(later));
        assert!(!notice.pulse(later, false), "respect disabled animations");
        assert_eq!(notice.next_wait(later, false), Some(BLINK_DURATION));
        notice.advance(later + BLINK_DURATION, false);
        assert!(!notice.visible(later + BLINK_DURATION));
        assert_eq!(notice.next_wait(later + BLINK_DURATION, false), None);

        commands.osc(&[b"633", b"C"]);
        commands.osc(&[b"633", b"D", b"0"]);
        notice.update(commands, false, later);
        assert!(notice.acknowledge());
        assert!(!notice.visible(later), "visiting a newly completed terminal also cancels its blink");
    }
}
