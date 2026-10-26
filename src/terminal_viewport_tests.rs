use super::*;
use crossterm::event::Event;
use ratatui::{
    backend::{ClearType, TestBackend, WindowSize},
    buffer::Cell,
    layout::Position,
};
use std::{
    cell::Cell as Counter,
    rc::Rc,
    time::{Duration, Instant},
};

struct CountingBackend {
    inner: TestBackend,
    queries: Rc<Counter<usize>>,
    delay: Duration,
}

impl CountingBackend {
    fn new(width: u16, height: u16, delay: Duration) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            queries: Rc::default(),
            delay,
        }
    }
}

impl Backend for CountingBackend {
    type Error = <TestBackend as Backend>::Error;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }
    fn clear_region(&mut self, region: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(region)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.queries.set(self.queries.get() + 1);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.inner.size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

fn paint(terminal: &mut Terminal<CountingBackend>, text: &str) {
    terminal
        .draw(|frame| {
            frame.render_widget(ratatui::widgets::Paragraph::new(text), frame.area());
        })
        .unwrap();
}

#[test]
fn steady_frames_query_the_host_once_and_keep_identical_fullscreen_cells() {
    let mut event_driven =
        event_driven_terminal(CountingBackend::new(120, 40, Duration::ZERO)).unwrap();
    let mut reference = Terminal::new(CountingBackend::new(120, 40, Duration::ZERO)).unwrap();
    for text in ["first\nsecond", "changed\n界 e\u{301}", "", "last"] {
        paint(&mut event_driven, text);
        paint(&mut reference, text);
        assert_eq!(
            event_driven.backend().inner.buffer(),
            reference.backend().inner.buffer()
        );
    }
    assert_eq!(event_driven.backend().queries.get(), 1);
    assert_eq!(reference.backend().queries.get(), 5);
}

#[test]
fn resize_packets_update_buffers_without_queries_and_focus_repairs_a_missed_resize() {
    let mut terminal =
        event_driven_terminal(CountingBackend::new(120, 40, Duration::ZERO)).unwrap();
    for (width, height) in [(80, 20), (160, 50), (1, 1), (0, 0), (120, 40)] {
        terminal.backend_mut().inner.resize(width, height);
        assert_eq!(
            apply_resize(&mut terminal, &Event::Resize(width, height)).unwrap(),
            Some(Size::new(width, height))
        );
        paint(&mut terminal, "hello");
        assert_eq!(terminal.get_frame().area(), Rect::new(0, 0, width, height));
        assert_eq!(
            apply_resize(&mut terminal, &Event::Resize(width, height)).unwrap(),
            None
        );
    }
    assert_eq!(terminal.backend().queries.get(), 1);
    terminal.backend_mut().inner.resize(100, 30);
    assert_eq!(
        apply_resize(&mut terminal, &Event::FocusGained).unwrap(),
        Some(Size::new(100, 30))
    );
    paint(&mut terminal, "after focus");
    assert_eq!(terminal.get_frame().area(), Rect::new(0, 0, 100, 30));
    assert_eq!(terminal.backend().queries.get(), 2);
    assert_eq!(
        apply_resize(&mut terminal, &Event::FocusLost).unwrap(),
        None
    );
    assert_eq!(terminal.backend().queries.get(), 2);
}

#[test]
#[ignore = "Measure per-frame terminal queries with a simulated slow console host"]
fn benchmark_event_driven_terminal_geometry() {
    for fixed in [false, true] {
        let backend = CountingBackend::new(160, 50, Duration::from_millis(2));
        let mut terminal = if fixed {
            event_driven_terminal(backend)
        } else {
            Terminal::new(backend)
        }
        .unwrap();
        let started = Instant::now();
        for _ in 0..120 {
            paint(&mut terminal, "unchanged text");
        }
        eprintln!(
            "event-driven geometry={fixed}: frames=120 host_queries={} elapsed={:?}",
            terminal.backend().queries.get(),
            started.elapsed()
        );
    }
}
