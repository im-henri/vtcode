//! Measure continuous pointer age through the actual workbench event loop.
//! A queue-only dispatch benchmark misses service scheduling and repaint order.
use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{TerminalOptions, Viewport, layout::Rect};
use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
    time::Instant,
};

thread_local! {
    static OBSERVER: RefCell<Option<Box<dyn FnMut(&App) -> bool>>> = RefCell::new(None);
    static BEFORE_PAINT: RefCell<Option<Box<dyn FnMut(&App)>>> = RefCell::new(None);
    #[cfg(windows)]
    static LAST_POINTER: std::cell::Cell<std::time::SystemTime> = std::cell::Cell::new(std::time::SystemTime::UNIX_EPOCH);
    #[cfg(windows)]
    static SCREEN_MARKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(windows)]
pub(super) fn pointer_updated() {
    LAST_POINTER.set(std::time::SystemTime::now());
}
#[cfg(windows)]
pub(super) fn last_pointer_updated() -> std::time::SystemTime {
    LAST_POINTER.get()
}

#[cfg(windows)]
pub(super) fn observe(observer: impl FnMut(&App) -> bool + 'static) {
    OBSERVER.with_borrow_mut(|slot| *slot = Some(Box::new(observer)));
}

#[cfg(windows)]
pub(super) fn enable_screen_marker() {
    SCREEN_MARKER.set(true);
}

pub(super) fn before_paint(app: &App) {
    BEFORE_PAINT.with_borrow_mut(|callback| {
        if let Some(callback) = callback {
            callback(app);
        }
    });
}

#[cfg(windows)]
pub(super) fn paint_marker(frame: &mut ratatui::Frame, app: &App) {
    if !SCREEN_MARKER.get() {
        return;
    }
    let point = if app.mouse_col >= 49 && app.mouse_row >= 4 {
        (app.mouse_row - 4) * 80 + app.mouse_col - 49
    } else {
        u16::MAX
    };
    // A title is a control message, not a screen update. Include a consistency
    // check so partial terminal diff writes cannot masquerade as another point.
    let marker = format!("@{point:04X}{:04X}@", !point);
    frame
        .buffer_mut()
        .set_string(0, 0, marker, ratatui::style::Style::default());
}

pub(super) fn presented(app: &App) -> bool {
    OBSERVER.with_borrow_mut(|observer| observer.as_mut().is_some_and(|observer| observer(app)))
}

#[test]
fn input_arriving_during_preparation_updates_the_first_painted_selection() {
    use std::cell::Cell;
    use std::rc::Rc;
    for reference in [true, false] {
        super::REFERENCE_INPUT_BATCHES.set(reference);
        let mut app = super::responsiveness_tests::fixture(1);
        app.source_control_enabled = false;
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(crate::terminal_output::buffered_output(
                Box::new(io::sink()) as Box<dyn io::Write>,
            )),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 160, 50)),
            },
        )
        .unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let area = app.code_area.unwrap();
        let event = |kind, x, y| {
            Event::Mouse(MouseEvent {
                kind,
                column: area.x + x,
                row: area.y + y,
                modifiers: KeyModifiers::NONE,
            })
        };
        let old = (area.x + 3, area.y + 4);
        let fresh = (area.x + 10, area.y + 8);
        let injected = Rc::new(Cell::new(false));
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(Ok(event(MouseEventKind::Down(MouseButton::Left), 2, 2)))
            .unwrap();
        sender
            .send(Ok(event(MouseEventKind::Drag(MouseButton::Left), 3, 4)))
            .unwrap();
        let sent = injected.clone();
        let late = event(MouseEventKind::Drag(MouseButton::Left), 10, 8);
        BEFORE_PAINT.with_borrow_mut(|callback| {
            *callback = Some(Box::new(move |app| {
                if (app.mouse_col, app.mouse_row) == old && !sent.replace(true) {
                    sender.send(Ok(late.clone())).unwrap();
                }
            }))
        });
        let painted = Rc::new(Cell::new(None));
        let painted_result = painted.clone();
        let was_sent = injected.clone();
        OBSERVER.with_borrow_mut(|observer| {
            *observer = Some(Box::new(move |app| {
                if !was_sent.get() {
                    return false;
                }
                painted_result.set(Some((app.mouse_col, app.mouse_row)));
                true
            }))
        });
        let result = run_app(
            &mut terminal,
            &mut app,
            TerminalEventPump::from_receiver(receiver),
            None,
        );
        BEFORE_PAINT.with_borrow_mut(|callback| *callback = None);
        OBSERVER.with_borrow_mut(|observer| *observer = None);
        super::REFERENCE_INPUT_BATCHES.set(false);
        result.unwrap();
        assert!(injected.get());
        assert_eq!(painted.get(), Some(if reference { old } else { fresh }));
        assert_eq!(
            app.editor().unwrap().cursor_line,
            if reference { 4 } else { 8 }
        );
    }
}

struct Output {
    delay: Duration,
    written: bool,
}
impl io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written |= !bytes.is_empty();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.written && !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.written = false;
        Ok(())
    }
}

#[test]
#[ignore = "Continuous mouse input through the complete event loop; optimized --nocapture"]
fn benchmark_continuous_pointer_event_loop() {
    crossterm::style::force_color_output(true);
    for delay in [0, 2, 8] {
        let mut app = super::responsiveness_tests::fixture(1);
        app.source_control_enabled = false;
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(crate::terminal_output::buffered_output(Box::new(Output {
                delay: Duration::from_millis(delay),
                written: false,
            })
                as Box<dyn io::Write>)),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 200, 65)),
            },
        )
        .unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let area = app.code_area.unwrap();
        let wake = app.background.wake_handle();
        let (tx, rx) = std::sync::mpsc::channel();
        let times = Arc::new(Mutex::new(Vec::<(usize, Instant)>::new()));
        let ages = Arc::new(Mutex::new(Vec::new()));
        let times_view = times.clone();
        let ages_view = ages.clone();
        OBSERVER.with_borrow_mut(|observer| {
            *observer = Some(Box::new(move |app| {
                let x = app.mouse_col.saturating_sub(area.x + 3) as usize;
                let y = app.mouse_row.saturating_sub(area.y + 1) as usize;
                let point = y * 80 + x;
                let times = times_view.lock().unwrap();
                if let Some(&(index, sent)) =
                    times.iter().find(|&&(index, _)| index * 37 % 2400 == point)
                {
                    ages_view.lock().unwrap().push((
                        index,
                        sent.elapsed(),
                        times.len() - index - 1,
                    ));
                }
                false
            }));
        });
        let producer = std::thread::spawn(move || {
            let pointer = |kind, index: usize| {
                Event::Mouse(MouseEvent {
                    kind,
                    column: area.x + 3 + (index * 37 % 2400 % 80) as u16,
                    row: area.y + 1 + (index * 37 % 2400 / 80) as u16,
                    modifiers: KeyModifiers::NONE,
                })
            };
            std::thread::sleep(Duration::from_millis(250));
            tx.send(Ok(pointer(MouseEventKind::Down(MouseButton::Left), 0)))
                .unwrap();
            wake.notify();
            for index in 0..600 {
                times.lock().unwrap().push((index, Instant::now()));
                tx.send(Ok(pointer(MouseEventKind::Drag(MouseButton::Left), index)))
                    .unwrap();
                wake.notify();
                std::thread::sleep(Duration::from_millis(1));
            }
            tx.send(Ok(pointer(MouseEventKind::Up(MouseButton::Left), 599)))
                .unwrap();
            wake.notify();
            std::thread::sleep(Duration::from_millis(250));
            tx.send(Err(io::Error::other("probe complete"))).unwrap();
            wake.notify();
        });
        let result = run_app(
            &mut terminal,
            &mut app,
            TerminalEventPump::from_receiver(rx),
            None,
        );
        OBSERVER.with_borrow_mut(|observer| {
            *observer = None;
        });
        producer.join().unwrap();
        assert_eq!(result.unwrap_err().to_string(), "probe complete");
        let ages = ages.lock().unwrap();
        let mut elapsed: Vec<_> = ages.iter().map(|(_, age, _)| *age).collect();
        elapsed.sort();
        let mut behind: Vec<_> = ages.iter().map(|(_, _, behind)| *behind).collect();
        behind.sort();
        assert!(elapsed.len() > 10);
        eprintln!(
            "full event-loop output_delay={delay}ms frames={} age p50={:?} p95={:?} p99={:?}; samples behind p50={} p95={}; final={}",
            ages.len(),
            elapsed[elapsed.len() / 2],
            elapsed[elapsed.len() * 95 / 100],
            elapsed[elapsed.len() * 99 / 100],
            behind[behind.len() / 2],
            behind[behind.len() * 95 / 100],
            ages.last().unwrap().0
        );
        assert_eq!(ages.last().unwrap().0, 599);
    }
}
