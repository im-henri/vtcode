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

#[derive(Debug)]
enum CursorEvent {
    Paint(bool),
    Move(Position, bool),
    Show(Position),
}

struct CountingBackend {
    inner: TestBackend,
    queries: Rc<Counter<usize>>,
    delay: Duration,
    cursor_visible: bool,
    cursor_events: Vec<CursorEvent>,
}

impl CountingBackend {
    fn new(width: u16, height: u16, delay: Duration) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            queries: Rc::default(),
            delay,
            cursor_visible: false,
            cursor_events: Vec::new(),
        }
    }
}

impl Backend for CountingBackend {
    type Error = <TestBackend as Backend>::Error;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.cursor_events.push(CursorEvent::Paint(self.cursor_visible));
        let mut last = None;
        self.inner.draw(content.inspect(|(x, y, _)| last = Some(Position::new(x.saturating_add(1), *y))))?;
        // Like a real terminal, printing the diff leaves the output cursor at
        // the final painted cell, not at the focused input's caret position.
        if let Some(position) = last { self.inner.set_cursor_position(position)?; }
        Ok(())
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_visible = false;
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_visible = true;
        self.cursor_events.push(CursorEvent::Show(self.inner.get_cursor_position()?));
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.cursor_events.push(CursorEvent::Move(position, self.cursor_visible));
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

fn paint<B: Backend>(terminal: &mut Terminal<B>, text: &str) {
    terminal
        .draw(|frame| {
            frame.render_widget(ratatui::widgets::Paragraph::new(text), frame.area());
        })
        .unwrap();
}

#[test]
fn loading_animation_never_exposes_the_output_cursor_while_painting() {
    let mut terminal = event_driven_terminal(CountingBackend::new(80, 12, Duration::ZERO)).unwrap();
    let mut app = crate::app::App::test_new();
    let started = Instant::now();
    for tick in 0..36 {
        let now = started + Duration::from_millis(tick * 80);
        if tick > 0 { assert!(app.loading.advance(true, now)); }
        app.loading.begin_frame();
        // Switch between a native caret (terminal/output/extension), an editor
        // caret painted into cells, and another native input while it animates.
        let caret = match tick / 12 {
            0 => Some(Position::new(50, 8)),
            1 => None,
            _ => Some(Position::new(60, 9)),
        };
        terminal.backend_mut().cursor_events.clear();
        terminal.draw(|frame| {
            crate::ui_loading::draw_header(frame, &mut app, Rect::new(1, 0, 24, 1));
            if let Some(caret) = caret { frame.set_cursor_position(caret); }
        }).unwrap();
        app.loading.finish_frame(now);
        let backend = terminal.backend();
        assert_eq!(backend.cursor_visible, caret.is_some());
        for event in &backend.cursor_events {
            match event {
                CursorEvent::Paint(visible) => assert!(!visible, "tick {tick}: painted with a visible cursor"),
                CursorEvent::Move(position, visible) => {
                    assert!(!visible, "tick {tick}: moved a visible cursor to {position:?}");
                    assert_eq!(Some(*position), caret);
                }
                CursorEvent::Show(position) => assert_eq!(Some(*position), caret,
                    "tick {tick}: cursor appeared at the animation's output position"),
            }
        }
    }
}

#[test]
fn explicit_cursor_visibility_outside_a_frame_and_terminal_cleanup_still_work() {
    let mut terminal = event_driven_terminal(CountingBackend::new(80, 12, Duration::ZERO)).unwrap();
    terminal.show_cursor().unwrap();
    assert!(terminal.backend().cursor_visible);
    terminal.hide_cursor().unwrap();
    assert!(!terminal.backend().cursor_visible);
    terminal.draw(|frame| frame.set_cursor_position((10, 4))).unwrap();
    assert!(terminal.backend().cursor_visible);
    terminal.hide_cursor().unwrap();
    assert!(!terminal.backend().cursor_visible);
    paint(&mut terminal, "editor frame without a native cursor");
    assert!(!terminal.backend().cursor_visible);
    terminal.show_cursor().unwrap();
    assert!(terminal.backend().cursor_visible);
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
            Terminal::new(CursorBackend::new(backend))
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
