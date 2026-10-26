//! Cooperative scheduling for work that must finish on the UI thread.
//!
//! Services share a wall-clock budget: being descheduled on a busy machine
//! consumes it too. Rotate after each service so a flood cannot starve the
//! next one, and let queued input interrupt the turn.
use std::time::{Duration, Instant};

pub(crate) const TURN_BUDGET: Duration = Duration::from_millis(4);
const INPUT_SERVICE_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Service {
    LanguageServers,
    Workers,
    Terminals,
    Agents,
    Extensions,
}

const SERVICES: [Service; 5] = [
    Service::LanguageServers,
    Service::Workers,
    Service::Terminals,
    Service::Agents,
    Service::Extensions,
];

pub(crate) struct Scheduler {
    next: usize,
    remaining: usize,
    last_service: Option<Instant>,
    observed_wake: Option<u64>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self { next: 0, remaining: SERVICES.len(), last_service: None, observed_wake: None }
    }
}

pub(crate) struct Turn {
    pub(crate) deadline: Instant,
    visited: usize,
    complete: bool,
}

impl Turn {
    pub(crate) fn new(now: Instant) -> Self {
        Self { deadline: now + TURN_BUDGET, visited: 0, complete: false }
    }

    pub(crate) fn deferred(&self) -> bool {
        !self.complete
    }
}

impl Scheduler {
    pub(crate) fn observe_wake(&mut self, generation: u64) -> u64 {
        // A cycle can span several input turns. A producer already polled in
        // an earlier turn may publish again before this one starts; keep the
        // original ticket until every service has been visited.
        *self.observed_wake.get_or_insert(generation)
    }

    pub(crate) fn next(&mut self, turn: &mut Turn, now: Instant, input_pending: bool) -> Option<Service> {
        if !turn.deferred() || (turn.visited > 0 && now >= turn.deadline) {
            return None;
        }
        // During a sustained input backlog, allow one service periodically.
        // A completion, file-open result or PTY must still make progress.
        if input_pending && (turn.visited > 0 || self.last_service
            .is_none_or(|last| now.saturating_duration_since(last) < INPUT_SERVICE_INTERVAL))
        {
            self.last_service.get_or_insert(now);
            return None;
        }
        let service = SERVICES[self.next];
        self.next = (self.next + 1) % SERVICES.len();
        self.last_service = Some(now);
        turn.visited += 1;
        self.remaining -= 1;
        if self.remaining == 0 {
            self.remaining = SERVICES.len();
            self.observed_wake = None;
            turn.complete = true;
        }
        Some(service)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_share_one_budget_and_resume_in_order() {
        let now = Instant::now();
        let mut scheduler = Scheduler::default();
        let mut turn = Turn::new(now);
        assert_eq!(scheduler.next(&mut turn, now, false), Some(Service::LanguageServers));
        assert_eq!(scheduler.next(&mut turn, now + Duration::from_millis(3), false), Some(Service::Workers));
        assert_eq!(scheduler.next(&mut turn, now + TURN_BUDGET, false), None);
        assert!(turn.deferred());
        let mut turn = Turn::new(now + TURN_BUDGET);
        assert_eq!(scheduler.next(&mut turn, now + TURN_BUDGET, false), Some(Service::Terminals));
    }

    #[test]
    fn arriving_input_interrupts_work_without_reordering_services() {
        let now = Instant::now();
        let mut scheduler = Scheduler::default();
        let mut turn = Turn::new(now);
        assert!(scheduler.next(&mut turn, now, false).is_some());
        assert_eq!(scheduler.next(&mut turn, now, true), None);
        let mut turn = Turn::new(now);
        assert_eq!(scheduler.next(&mut turn, now, true), None);
        assert_eq!(scheduler.next(&mut turn, now, false), Some(Service::Workers));
    }

    #[test]
    fn continuous_input_gets_priority_without_starving_any_service() {
        let start = Instant::now();
        let mut scheduler = Scheduler::default();
        assert_eq!(scheduler.next(&mut Turn::new(start), start, true), None);
        for (index, expected) in SERVICES.into_iter().cycle().take(15).enumerate() {
            let now = start + INPUT_SERVICE_INTERVAL * (index as u32 + 1);
            let mut turn = Turn::new(now);
            assert_eq!(scheduler.next(&mut turn, now, true), Some(expected));
            assert_eq!(scheduler.next(&mut turn, now, true), None);
            let now = now + Duration::from_millis(1);
            assert_eq!(scheduler.next(&mut Turn::new(now), now, true), None);
        }
    }

    #[test]
    fn quiet_services_do_not_create_an_idle_poll_timer() {
        let now = Instant::now();
        let mut scheduler = Scheduler::default();
        let mut turn = Turn::new(now);
        for service in SERVICES {
            assert_eq!(scheduler.next(&mut turn, now, false), Some(service));
        }
        assert_eq!(scheduler.next(&mut turn, now, false), None);
        assert!(!turn.deferred());
    }

    #[test]
    fn slow_quiet_cycle_eventually_sleeps_instead_of_restarting_forever() {
        let mut now = Instant::now();
        let mut scheduler = Scheduler::default();
        for (index, service) in SERVICES.into_iter().enumerate() {
            let mut turn = Turn::new(now);
            assert_eq!(scheduler.next(&mut turn, now, false), Some(service));
            now += Duration::from_millis(50);
            assert_eq!(scheduler.next(&mut turn, now, false), None);
            assert_eq!(turn.deferred(), index + 1 < SERVICES.len());
        }
    }

    #[test]
    fn descheduled_turn_still_makes_bounded_progress() {
        let now = Instant::now();
        let mut scheduler = Scheduler::default();
        let mut turn = Turn::new(now);
        let later = now + Duration::from_secs(1);
        assert!(scheduler.next(&mut turn, later, false).is_some());
        assert!(scheduler.next(&mut turn, later, false).is_none());
        assert!(turn.deferred());
    }

    #[test]
    fn partial_cycle_does_not_forget_a_wake_from_an_already_polled_service() {
        let now = Instant::now();
        let mut scheduler = Scheduler::default();
        assert_eq!(scheduler.observe_wake(7), 7);
        let mut turn = Turn::new(now);
        assert_eq!(scheduler.next(&mut turn, now, false), Some(Service::LanguageServers));
        assert!(scheduler.next(&mut turn, now + TURN_BUDGET, false).is_none());
        // An LSP response arrives while input is being handled.
        assert_eq!(scheduler.observe_wake(8), 7);
        let mut turn = Turn::new(now + TURN_BUDGET);
        for _ in 0..4 { assert!(scheduler.next(&mut turn, now + TURN_BUDGET, false).is_some()); }
        assert!(!turn.deferred());
        // Waiting against the old ticket sees generation 8 immediately. Only
        // the next full cycle may acknowledge that generation.
        assert_eq!(scheduler.observe_wake(8), 8);
    }
}
