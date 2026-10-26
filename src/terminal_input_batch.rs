//! Publish already-ready console input before waking the workbench. Waking for
//! each record let the UI paint an old drag endpoint while its newer samples
//! were still being decoded on the input thread. Never wait for another event.
use crossterm::event::Event;
use std::{
    cell::RefCell,
    collections::BinaryHeap,
    io,
    sync::mpsc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_EVENTS: usize = 1_024;
const BUDGET: Duration = Duration::from_millis(1);

struct Stamped {
    sequence: u64,
    decoded_at: Instant,
    physical_press_at: Option<Instant>,
    event: io::Result<Event>,
}

/// Only the UI consumer touches these counters. Keep input-delivery diagnostics
/// beside the queue rather than locking a shared monitor from the reader. This
/// records no text, creates no timer and performs no work while input is idle.
#[derive(Default)]
pub(crate) struct DeliveryTimings {
    pub queue: crate::performance::RenderTiming,
    pub drag_queue: crate::performance::RenderTiming,
    pub press_ingress: crate::performance::RenderTiming,
}

impl DeliveryTimings {
    fn record(&mut self, input: &Stamped, now: Instant) {
        let elapsed = now.saturating_duration_since(input.decoded_at);
        self.queue.record(elapsed);
        if let Some(pressed) = input.physical_press_at {
            self.press_ingress
                .record(input.decoded_at.saturating_duration_since(pressed));
        }
        if matches!(&input.event, Ok(Event::Mouse(mouse))
            if matches!(mouse.kind, crossterm::event::MouseEventKind::Drag(_)))
        {
            self.drag_queue.record(elapsed);
        }
    }
}

thread_local! {
    // The workbench and its performance view run on the same UI thread.
    // Test runners on other threads and headless workers cannot contaminate it.
    static DELIVERY: RefCell<DeliveryTimings> = RefCell::new(DeliveryTimings::default());
}

pub(crate) fn with_delivery_timings<T>(read: impl FnOnce(&DeliveryTimings) -> T) -> T {
    DELIVERY.with_borrow(read)
}

pub(crate) fn reset_delivery_timings() {
    DELIVERY.with_borrow_mut(|timings| *timings = DeliveryTimings::default());
}

enum Batch {
    One(Stamped),
    Ready(Vec<Stamped>),
}

impl Batch {
    fn push(&mut self, event: Stamped) {
        match self {
            Self::Ready(events) => events.push(event),
            Self::One(_) => {
                let Self::One(first) = std::mem::replace(self, Self::Ready(Vec::with_capacity(16)))
                else {
                    unreachable!()
                };
                let Self::Ready(events) = self else {
                    unreachable!()
                };
                events.extend([first, event]);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct Sender(SenderKind);

#[derive(Clone)]
enum SenderKind {
    Queued(mpsc::Sender<Batch>, Arc<AtomicU64>),
    #[cfg(test)]
    Single(mpsc::Sender<io::Result<Event>>),
}

pub(crate) struct Receiver {
    source: ReceiverKind,
    pending: RefCell<Pending>,
}

#[derive(Default)]
struct Pending {
    sequence: u64,
    ready: Option<Segment>,
    deferred: BinaryHeap<Segment>,
}

enum Segment {
    One(Stamped),
    Ready(std::vec::IntoIter<Stamped>),
}

impl Segment {
    fn sequence(&self) -> u64 {
        match self {
            Self::One(event) => event.sequence,
            Self::Ready(events) => events.as_slice()[0].sequence,
        }
    }

    fn take(self) -> (Stamped, Option<Self>) {
        match self {
            Self::One(event) => (event, None),
            Self::Ready(mut events) => {
                let first = events.next().expect("published batches are nonempty");
                (
                    first,
                    (!events.as_slice().is_empty()).then_some(Self::Ready(events)),
                )
            }
        }
    }
}

impl From<Batch> for Segment {
    fn from(batch: Batch) -> Self {
        match batch {
            Batch::One(event) => Self::One(event),
            Batch::Ready(events) => Self::Ready(events.into_iter()),
        }
    }
}

impl PartialEq for Segment {
    fn eq(&self, other: &Self) -> bool {
        self.sequence() == other.sequence()
    }
}
impl Eq for Segment {}
impl PartialOrd for Segment {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Segment {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // A max heap serves the earliest stamp. Wrapping comparisons remain
        // ordered across rollover; a live queue cannot span 2^63 events.
        (other.sequence().wrapping_sub(self.sequence()) as i64).cmp(&0)
    }
}

enum ReceiverKind {
    Queued(mpsc::Receiver<Batch>),
    #[cfg(test)]
    Single(mpsc::Receiver<io::Result<Event>>),
}

pub(crate) fn channel() -> (Sender, Receiver) {
    let (sender, receiver) = mpsc::channel();
    (
        Sender(SenderKind::Queued(sender, Arc::new(AtomicU64::new(0)))),
        Receiver {
            source: ReceiverKind::Queued(receiver),
            pending: RefCell::new(Pending::default()),
        },
    )
}

impl Sender {
    // Native Shift/side-button events share the same ordering boundary as
    // decoded batches; sparse input needs no intermediate Vec allocation.
    pub(crate) fn send(&self, event: io::Result<Event>) -> Result<(), ()> {
        self.publish(Batch::One(self.stamp(event)))
            .then_some(())
            .ok_or(())
    }

    fn stamp(&self, event: io::Result<Event>) -> Stamped {
        self.stamp_at(event, Instant::now())
    }

    fn stamp_at(&self, event: io::Result<Event>, decoded_at: Instant) -> Stamped {
        let sequence = match &self.0 {
            SenderKind::Queued(_, sequence) => sequence.fetch_add(1, Ordering::Relaxed),
            #[cfg(test)]
            SenderKind::Single(_) => 0,
        };
        Stamped {
            sequence,
            decoded_at,
            physical_press_at: None,
            event,
        }
    }

    fn publish(&self, batch: Batch) -> bool {
        match &self.0 {
            SenderKind::Queued(sender, _) => sender.send(batch).is_ok(),
            #[cfg(test)]
            SenderKind::Single(sender) => match batch {
                Batch::One(event) => sender.send(event.event).is_ok(),
                Batch::Ready(events) => events
                    .into_iter()
                    .all(|event| sender.send(event.event).is_ok()),
            },
        }
    }
}

impl Receiver {
    pub(crate) fn try_recv(&self) -> Result<io::Result<Event>, mpsc::TryRecvError> {
        match &self.source {
            ReceiverKind::Queued(receiver) => {
                let mut pending = self.pending.borrow_mut();
                loop {
                    if let Some(ready) = pending.ready.take() {
                        if ready.sequence() == pending.sequence {
                            let (event, rest) = ready.take();
                            pending.ready = rest;
                            pending.sequence = pending.sequence.wrapping_add(1);
                            DELIVERY
                                .with_borrow_mut(|timings| timings.record(&event, Instant::now()));
                            return Ok(event.event);
                        }
                        pending.deferred.push(ready);
                    }
                    if pending
                        .deferred
                        .peek()
                        .is_some_and(|first| first.sequence() == pending.sequence)
                    {
                        pending.ready = pending.deferred.pop();
                        continue;
                    }
                    let next = Segment::from(receiver.try_recv()?);
                    if next.sequence() == pending.sequence {
                        pending.ready = Some(next);
                    } else {
                        pending.deferred.push(next);
                    }
                }
            }
            #[cfg(test)]
            ReceiverKind::Single(receiver) => receiver.try_recv(),
        }
    }
}

#[cfg(test)]
impl From<mpsc::Sender<io::Result<Event>>> for Sender {
    fn from(sender: mpsc::Sender<io::Result<Event>>) -> Self {
        Self(SenderKind::Single(sender))
    }
}

#[cfg(test)]
impl From<mpsc::Receiver<io::Result<Event>>> for Receiver {
    fn from(receiver: mpsc::Receiver<io::Result<Event>>) -> Self {
        Self {
            source: ReceiverKind::Single(receiver),
            pending: RefCell::new(Pending::default()),
        }
    }
}

/// False means the input source or receiver has ended. Filtering belongs to
/// the producer (e.g. duplicate native Shift gestures), before publication.
pub(crate) fn forward(
    first: io::Result<Event>,
    sender: &Sender,
    wake: &crate::background::UiWake,
    filter: impl FnMut(&io::Result<Event>) -> bool,
    next_ready: impl FnMut() -> io::Result<Option<Event>>,
    ordered: impl FnMut() -> Option<Instant>,
) -> bool {
    forward_with_budget(first, sender, wake, filter, next_ready, BUDGET, ordered)
}

pub(crate) fn forward_with_budget(
    first: io::Result<Event>,
    sender: &Sender,
    wake: &crate::background::UiWake,
    mut filter: impl FnMut(&io::Result<Event>) -> bool,
    mut next_ready: impl FnMut() -> io::Result<Option<Event>>,
    budget: Duration,
    mut ordered: impl FnMut() -> Option<Instant>,
) -> bool {
    let started = Instant::now();
    let mut event = first;
    let mut batch: Option<Batch> = None;
    let mut running = true;
    for count in 1..=MAX_EVENTS {
        // Include producer-side filtering and batch publication in queue age.
        // Stamping after filtering would hide time waiting for focus/geometry.
        let decoded_at = Instant::now();
        let failed = event.is_err();
        if !filter(&event) {
            let mut event = sender.stamp_at(event, decoded_at);
            event.physical_press_at = ordered();
            if let Some(batch) = &mut batch {
                batch.push(event);
            } else {
                batch = Some(Batch::One(event));
            }
        }
        if failed {
            running = false;
            break;
        }
        if count == MAX_EVENTS || started.elapsed() >= budget {
            break;
        }
        event = match next_ready() {
            Ok(Some(event)) => Ok(event),
            Ok(None) => break,
            Err(error) => Err(error),
        };
    }
    if let Some(batch) = batch {
        if !sender.publish(batch) {
            return false;
        }
        wake.notify();
    }
    running
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    #[test]
    fn delivery_timings_separate_ingress_queue_and_drag_samples() {
        let at = Instant::now();
        let mut timings = DeliveryTimings::default();
        let mut input = Stamped {
            sequence: 0,
            decoded_at: at + Duration::from_millis(20),
            physical_press_at: Some(at),
            event: Ok(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row: 5,
                modifiers: KeyModifiers::NONE,
            })),
        };
        timings.record(&input, at + Duration::from_millis(50));
        assert_eq!(timings.queue.last_ms, 30.0);
        assert_eq!(timings.press_ingress.last_ms, 20.0);
        assert_eq!(timings.drag_queue.sample_count(), 0);
        input.physical_press_at = None;
        input.event = Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 20,
            row: 8,
            modifiers: KeyModifiers::NONE,
        }));
        timings.record(&input, at + Duration::from_millis(55));
        assert_eq!(timings.drag_queue.last_ms, 35.0);
        input.event = Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        timings.record(&input, at + Duration::from_millis(60));
        assert_eq!(timings.queue.sample_count(), 3);
        assert_eq!(timings.press_ingress.sample_count(), 1);
        assert_eq!(
            timings.drag_queue.sample_count(),
            1,
            "opening the monitor must not displace drag history"
        );
    }

    #[test]
    fn delayed_publication_retains_original_decode_time_and_reset_clears_history() {
        reset_delivery_timings();
        let (sender, receiver) = channel();
        let decoded = Instant::now()
            .checked_sub(Duration::from_millis(40))
            .unwrap();
        let mut input = sender.stamp_at(Ok(Event::FocusGained), decoded);
        input.physical_press_at = decoded.checked_sub(Duration::from_millis(15));
        assert!(sender.publish(Batch::One(input)));
        with_delivery_timings(|timings| assert!(!timings.queue.has_samples()));
        assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::FocusGained);
        with_delivery_timings(|timings| {
            assert!(
                timings.queue.last_ms >= 40.0,
                "publication must not retimestamp old input"
            );
            assert_eq!(timings.press_ingress.last_ms, 15.0);
            assert_eq!(timings.queue.sample_count(), 1);
        });
        assert!(receiver.try_recv().is_err());
        with_delivery_timings(|timings| {
            assert_eq!(
                timings.queue.sample_count(),
                1,
                "empty polling is not a latency sample"
            )
        });
        reset_delivery_timings();
        with_delivery_timings(|timings| {
            assert!(!timings.queue.has_samples());
            assert!(!timings.drag_queue.has_samples());
            assert!(!timings.press_ingress.has_samples());
        });
    }

    #[test]
    fn ready_batches_are_invisible_until_complete_and_share_order_with_native_input() {
        let (sender, receiver) = channel();
        let wake = crate::background::UiWake::default();
        let key = Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let mut next = Some(key.clone());
        assert!(forward_with_budget(
            Ok(Event::FocusGained),
            &sender,
            &wake,
            |_| false,
            || {
                assert!(
                    matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
                    "the UI must not see a historical endpoint from a batch still being decoded"
                );
                Ok(next.take())
            },
            Duration::MAX,
            || None
        ));
        sender.send(Ok(Event::FocusLost)).unwrap();
        assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::FocusGained);
        assert_eq!(receiver.try_recv().unwrap().unwrap(), key);
        assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::FocusLost);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(sender);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn published_batches_drain_before_disconnect_and_keep_the_first_input_error() {
        let (sender, receiver) = channel();
        let wake = crate::background::UiWake::default();
        assert!(!forward_with_budget(
            Ok(Event::FocusGained),
            &sender,
            &wake,
            |_| false,
            || Err(io::Error::other("decoder stopped")),
            Duration::MAX,
            || None
        ));
        drop(sender);
        assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::FocusGained);
        assert_eq!(
            receiver.try_recv().unwrap().unwrap_err().to_string(),
            "decoder stopped"
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn native_events_cannot_overtake_earlier_decoded_input_inside_an_unpublished_batch() {
        for initial in [0, u64::MAX - 1] {
            let (sender, receiver) = channel();
            let SenderKind::Queued(_, sequence) = &sender.0 else {
                unreachable!()
            };
            sequence.store(initial, Ordering::Relaxed);
            receiver.pending.borrow_mut().sequence = initial;
            let mut next = Some(Event::Resize(80, 25));
            assert!(forward_with_budget(
                Ok(Event::FocusGained),
                &sender,
                &Default::default(),
                |_| false,
                || {
                    if next.is_some() {
                        sender.send(Ok(Event::FocusLost)).unwrap();
                        assert!(
                            matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
                            "a native event must wait for the earlier decoded event's publication"
                        );
                    }
                    Ok(next.take())
                },
                Duration::MAX,
                || None
            ));
            assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::FocusGained);
            assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::FocusLost);
            assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::Resize(80, 25));
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
        }
    }

    #[test]
    fn delayed_batches_with_many_native_gaps_merge_without_losing_or_repeating_input() {
        for initial in [0, u64::MAX - 450] {
            let (sender, receiver) = channel();
            let SenderKind::Queued(_, sequence) = &sender.0 else {
                unreachable!()
            };
            sequence.store(initial, Ordering::Relaxed);
            receiver.pending.borrow_mut().sequence = initial;
            let expected: Vec<_> = (0..1_000)
                .map(|index| Event::Resize(index, index % 40))
                .collect();
            let mut messages = Vec::new();
            let mut batch: Option<Batch> = None;
            for (index, event) in expected.iter().cloned().enumerate() {
                let event = sender.stamp(Ok(event));
                if index % 5 == 0 {
                    messages.push(Batch::One(event));
                } else if let Some(batch) = &mut batch {
                    batch.push(event);
                } else {
                    batch = Some(Batch::One(event));
                }
                if index % 31 == 30
                    && let Some(batch) = batch.take()
                {
                    messages.push(batch);
                }
            }
            if let Some(batch) = batch {
                messages.push(batch);
            }
            let mut actual = Vec::new();
            for message in messages.into_iter().rev() {
                assert!(sender.publish(message));
                while let Ok(event) = receiver.try_recv() {
                    actual.push(event.unwrap());
                }
                assert_eq!(actual, expected[..actual.len()]);
            }
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn mouse_backlogs_keep_keys_releases_paste_and_errors_in_order() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Sender::from(tx);
        let wake = crate::background::UiWake::default();
        let mut expected = Vec::new();
        for index in 0..3_000u16 {
            expected.push(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: index % 100,
                row: index % 30,
                modifiers: KeyModifiers::NONE,
            }));
            if index % 511 == 0 {
                expected.extend([
                    Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                    Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Up(MouseButton::Left),
                        column: index % 100,
                        row: index % 30,
                        modifiers: KeyModifiers::NONE,
                    }),
                    Event::Paste("🦀\r\ntext".into()),
                ]);
            }
        }
        let mut source: std::collections::VecDeque<_> = expected.iter().cloned().collect();
        let mut actual = Vec::new();
        while let Some(first) = source.pop_front() {
            let generation = wake.generation();
            assert!(forward(
                Ok(first),
                &tx,
                &wake,
                |_| false,
                || Ok(source.pop_front()),
                || None
            ));
            assert_eq!(
                wake.generation(),
                generation + 1,
                "one wake for the ready batch"
            );
            let batch: Vec<_> = rx.try_iter().map(Result::unwrap).collect();
            assert!(!batch.is_empty() && batch.len() <= MAX_EVENTS);
            actual.extend(batch);
        }
        assert_eq!(actual, expected);
        assert!(!forward(
            Ok(Event::FocusLost),
            &tx,
            &wake,
            |_| false,
            || Err(io::Error::other("reader stopped")),
            || None
        ));
        assert_eq!(rx.recv().unwrap().unwrap(), Event::FocusLost);
        assert_eq!(
            rx.recv().unwrap().unwrap_err().to_string(),
            "reader stopped"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn sparse_or_filtered_input_never_waits_or_manufactures_a_notification() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Sender::from(tx);
        let wake = crate::background::UiWake::default();
        let generation = wake.generation();
        let mut checks = 0;
        assert!(forward(
            Ok(Event::FocusGained),
            &tx,
            &wake,
            |_| true,
            || {
                checks += 1;
                Ok(None)
            },
            || None
        ));
        assert_eq!(checks, 1);
        assert_eq!(wake.generation(), generation);
        assert!(rx.try_recv().is_err());
        assert!(forward(
            Ok(Event::FocusGained),
            &tx,
            &wake,
            |_| false,
            || Ok(None),
            || None
        ));
        assert_eq!(rx.recv().unwrap().unwrap(), Event::FocusGained);
        assert_eq!(wake.generation(), generation + 1);
        drop(rx);
        assert!(!forward(
            Ok(Event::FocusLost),
            &tx,
            &wake,
            |_| false,
            || Ok(None),
            || None
        ));
        assert_eq!(
            wake.generation(),
            generation + 1,
            "a failed publication never wakes the closed UI"
        );
    }
}
