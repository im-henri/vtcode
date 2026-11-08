//! Manual end-to-end frame profile over this repository. Each scenario runs the
//! same preparation (`refresh_caches`) and paint as the event loop, through a
//! real Crossterm backend writing into memory, so terminal bytes are measured.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};
use std::time::{Duration, Instant};

use super::{TerminalEventPump, handle_keyboard_input, handle_mouse_input, refresh_caches};
use crate::app::App;

#[derive(Clone, Default)]
struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
impl std::io::Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.borrow_mut().extend_from_slice(bytes); Ok(bytes.len()) }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

/// Encodes every diff with both the previous Crossterm backend and the
/// compact backend, so byte counts and encode times compare identical frames.
struct Dual {
    old: CrosstermBackend<Shared>,
    new: crate::terminal_output::AnsiBackend<Shared>,
    old_bytes: Shared,
    new_bytes: Shared,
    old_time: Duration,
    new_time: Duration,
    width: u16,
    height: u16,
}
impl Dual {
    fn new(width: u16, height: u16) -> Self {
        let (old_bytes, new_bytes) = (Shared::default(), Shared::default());
        Self {
            old: CrosstermBackend::new(old_bytes.clone()),
            new: crate::terminal_output::AnsiBackend::new(new_bytes.clone()),
            old_bytes, new_bytes, old_time: Duration::ZERO, new_time: Duration::ZERO, width, height,
        }
    }
}
impl ratatui::backend::Backend for Dual {
    type Error = std::io::Error;
    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)> {
        let cells: Vec<_> = content.collect();
        let started = Instant::now();
        // The previous CursorBackend always hid the cursor (with a flush).
        self.old.hide_cursor()?;
        self.old.draw(cells.iter().copied())?;
        let middle = Instant::now();
        self.new.draw(cells.iter().copied())?;
        self.new_time += middle.elapsed();
        self.old_time += middle - started;
        Ok(())
    }
    fn hide_cursor(&mut self) -> std::io::Result<()> { self.old.hide_cursor()?; self.new.hide_cursor() }
    fn show_cursor(&mut self) -> std::io::Result<()> { self.old.show_cursor()?; self.new.show_cursor() }
    fn get_cursor_position(&mut self) -> std::io::Result<ratatui::layout::Position> { Ok(Default::default()) }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, position: P) -> std::io::Result<()> {
        let position = position.into();
        self.old.set_cursor_position(position)?;
        self.new.set_cursor_position(position)
    }
    fn clear(&mut self) -> std::io::Result<()> { self.old.clear()?; self.new.clear() }
    fn clear_region(&mut self, region: ratatui::backend::ClearType) -> std::io::Result<()> {
        self.old.clear_region(region)?; self.new.clear_region(region)
    }
    fn size(&self) -> std::io::Result<ratatui::layout::Size> { Ok(ratatui::layout::Size::new(self.width, self.height)) }
    fn window_size(&mut self) -> std::io::Result<ratatui::backend::WindowSize> {
        Ok(ratatui::backend::WindowSize { columns_rows: self.size()?, pixels: Default::default() })
    }
    fn flush(&mut self) -> std::io::Result<()> { self.old.flush()?; self.new.flush() }
}

type Term = Terminal<crate::terminal_output::CursorBackend<Dual>>;

fn terminal(width: u16, height: u16) -> Term {
    let mut terminal = Terminal::with_options(
        crate::terminal_output::test_cursor_backend(Dual::new(width, height)),
        ratatui::TerminalOptions { viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, width, height)) },
    ).unwrap();
    terminal.clear().unwrap();
    terminal
}

struct Sample { prep: Duration, paint: Duration, old_bytes: usize, new_bytes: usize, old_encode: Duration, new_encode: Duration }

fn frame(app: &mut App, terminal: &mut Term) -> Sample {
    let started = Instant::now();
    app.poll_background_results();
    app.sync_duplicate_file_tabs();
    app.ensure_active_tab_visible();
    app.prepare_visible_merge_conflicts();
    refresh_caches(app);
    // As the event loop does each iteration (drives the status activity badge).
    app.background.refresh_activity(Instant::now());
    let prepared = Instant::now();
    let mut widgets = Duration::ZERO;
    let completed = terminal.draw(|f| { let t = Instant::now(); crate::ui::draw(f, app); widgets = t.elapsed(); }).unwrap();
    WIDGETS.with(|w| w.set(w.get() + widgets));
    if std::env::var_os("VTCODE_PROFILE_ROWS").is_some() {
        let buffer = completed.buffer.clone();
        LAST_FRAME.with(|frame| *frame.borrow_mut() = Some(buffer));
    }
    let painted = Instant::now();
    let backend = &mut **terminal.backend_mut();
    let old_bytes = std::mem::take(&mut *backend.old_bytes.0.borrow_mut()).len();
    let new_bytes = std::mem::take(&mut *backend.new_bytes.0.borrow_mut()).len();
    let (old_encode, new_encode) = (std::mem::take(&mut backend.old_time), std::mem::take(&mut backend.new_time));
    app.refresh_wrap_layout_async();
    // Paint excludes the extra comparison encode.
    Sample { prep: prepared - started, paint: (painted - prepared).saturating_sub(old_encode), old_bytes, new_bytes, old_encode, new_encode }
}

fn settle(app: &mut App, terminal: &mut Term) {
    for _ in 0..40 {
        frame(app, terminal);
        app.wait_for_background(Duration::from_secs(20), |app| !app.background.has_pending_results());
        std::thread::sleep(Duration::from_millis(5));
    }
    frame(app, terminal);
}

fn report(name: &str, samples: &mut Vec<Sample>) {
    if samples.is_empty() { return; }
    let p = |mut v: Vec<Duration>, q: f64| { v.sort(); v[((v.len() - 1) as f64 * q) as usize] };
    let total: Vec<_> = samples.iter().map(|s| s.prep + s.paint).collect();
    let n = samples.len();
    let old_bytes = samples.iter().map(|s| s.old_bytes).sum::<usize>() / n;
    let new_bytes = samples.iter().map(|s| s.new_bytes).sum::<usize>() / n;
    let widgets = WIDGETS.with(|w| w.replace(Duration::ZERO)) / n as u32;
    eprintln!(
        "PROFILE {name:<24} n={n:<4} total p50={:>8.1?} p90={:>8.1?} max={:>8.1?} | prep p50={:>8.1?} paint p50={:>8.1?} | bytes old={old_bytes:>6} new={new_bytes:>6} widgets avg={widgets:>8.1?} | encode old={:>7.1?} new={:>7.1?}",
        p(total.clone(), 0.5), p(total.clone(), 0.9), p(total, 1.0),
        p(samples.iter().map(|s| s.prep).collect(), 0.5), p(samples.iter().map(|s| s.paint).collect(), 0.5),
        p(samples.iter().map(|s| s.old_encode).collect(), 0.5), p(samples.iter().map(|s| s.new_encode).collect(), 0.5),
    );
    samples.clear();
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    handle_keyboard_input(app, &mut pump, KeyEvent::new(code, modifiers));
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    handle_mouse_input(app, &mut pump, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
}

fn repo() -> std::path::PathBuf { std::env::current_dir().unwrap() }

#[test]
#[ignore = "manual end-to-end frame profile"]
fn profile_basic_operation_frames() {
    let (width, height) = std::env::var("VTCODE_PROFILE_SIZE").ok()
        .and_then(|size| size.split_once('x').map(|(w, h)| (w.parse().unwrap(), h.parse().unwrap())))
        .unwrap_or((200u16, 55u16));
    let mut app = App::new(repo());
    let mut term = terminal(width, height);
    app.animation_settings = Default::default();
    settle(&mut app, &mut term);
    let files = ["src/app.rs", "src/ui_editor.rs", "src/explorer.rs", "src/highlight.rs", "src/app_tabs.rs", "README.md"];
    for file in files {
        app.open_file(&repo().join(file));
        settle(&mut app, &mut term);
    }
    // Large Rust source as the active tab; reveal it so Explorer has `src/`.
    app.active_tab = 0;
    app.touch_tab_mru(0);
    app.active_pane = crate::app::ActivePane::Editor;
    if app.explorer.reveal_file_centered_deferred(&repo().join(files[0])) { app.refresh_explorer_tree_async(); }
    settle(&mut app, &mut term);
    eprintln!("PROFILE setup: tabs={} active={:?} explorer entries={} lines={}", app.tabs.len(),
        app.editor().and_then(|e| e.file_path.clone()), app.explorer.entries.len(),
        app.editor().map_or(0, |e| e.rope.len_lines()));
    let mut samples = Vec::new();

    for _ in 0..60 { samples.push(frame(&mut app, &mut term)); }
    report("idle full redraw", &mut samples);

    for _ in 0..120 {
        mouse(&mut app, MouseEventKind::ScrollDown, 100, 20);
        samples.push(frame(&mut app, &mut term));
    }
    report("wheel scroll", &mut samples);

    for _ in 0..60 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("page down", &mut samples);

    for _ in 0..120 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("cursor down", &mut samples);

    for index in 0..150 {
        let c = (b'a' + (index % 26) as u8) as char;
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("typing", &mut samples);

    for _ in 0..150 {
        key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("backspace", &mut samples);

    for _ in 0..60 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
        samples.push(frame(&mut app, &mut term));
    }
    report("next tab", &mut samples);

    for index in 0..40 {
        let w = if index % 2 == 0 { width - 13 } else { width };
        term.resize(Rect::new(0, 0, w, height)).unwrap();
        samples.push(frame(&mut app, &mut term));
    }
    report("terminal resize", &mut samples);

    // Explorer sidebar resize by dragging its edge.
    settle(&mut app, &mut term);
    let edge = app.explorer_area.map_or(30, |area| area.right().saturating_sub(1));
    mouse(&mut app, MouseEventKind::Down(crossterm::event::MouseButton::Left), edge, 20);
    for index in 0..80u16 {
        let x = edge + (index % 20);
        mouse(&mut app, MouseEventKind::Drag(crossterm::event::MouseButton::Left), x, 20);
        samples.push(frame(&mut app, &mut term));
    }
    mouse(&mut app, MouseEventKind::Up(crossterm::event::MouseButton::Left), edge, 20);
    report("sidebar drag resize", &mut samples);

    // Explorer navigation.
    settle(&mut app, &mut term);
    app.active_pane = crate::app::ActivePane::Explorer;
    app.explorer.selected = 0;
    app.explorer.scroll_offset = 0;
    for _ in 0..150 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("explorer down", &mut samples);
    if let Some(area) = app.explorer_area {
        for _ in 0..100 {
            mouse(&mut app, MouseEventKind::ScrollDown, area.x + 3, area.y + 5);
            samples.push(frame(&mut app, &mut term));
        }
    }
    report("explorer wheel", &mut samples);
    if let Some(area) = app.explorer_area {
        for index in 0..100 {
            mouse(&mut app, MouseEventKind::Moved, area.x + 3, area.y + 3 + (index % 30));
            samples.push(frame(&mut app, &mut term));
        }
    }
    report("explorer hover", &mut samples);

    // Source control view.
    app.execute_palette_command("source_control");
    settle(&mut app, &mut term);
    for _ in 0..60 { samples.push(frame(&mut app, &mut term)); }
    report("source control redraw", &mut samples);
    app.active_pane = crate::app::ActivePane::Editor;

    // Tabs: move tab right / left.
    for index in 0..60 {
        let code = if index % 2 == 0 { KeyCode::PageDown } else { KeyCode::PageUp };
        key(&mut app, code, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        samples.push(frame(&mut app, &mut term));
    }
    report("move tab", &mut samples);
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
}

#[test]
#[ignore = "manual end-to-end frame profile"]
fn profile_diff_and_source_control_frames() {
    let (width, height) = (200u16, 55u16);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "profile@example.com"]);
    git(&root, &["config", "user.name", "profile"]);
    let source = std::fs::read_to_string(repo().join("src/app.rs")).unwrap();
    for index in 0..40 {
        std::fs::write(root.join(format!("file_{index:02}.rs")), &source).unwrap();
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    // Scattered edits: every 97th line changed, every 211th removed.
    let edited: String = source.lines().enumerate().filter(|(i, _)| i % 211 != 5)
        .map(|(i, line)| if i % 97 == 3 { format!("{line} // edited\n") } else { format!("{line}\n") }).collect();
    for index in 0..40 {
        std::fs::write(root.join(format!("file_{index:02}.rs")), &edited).unwrap();
    }
    let mut app = App::new(root.clone());
    app.animation_settings = Default::default();
    let mut term = terminal(width, height);
    settle(&mut app, &mut term);
    app.execute_palette_command("source_control");
    settle(&mut app, &mut term);
    eprintln!("PROFILE setup: source control files={}", app.source_control_files.len());
    let mut samples = Vec::new();
    for _ in 0..60 { samples.push(frame(&mut app, &mut term)); }
    report("sc idle redraw", &mut samples);

    let started = Instant::now();
    app.open_source_control_diff(0);
    settle(&mut app, &mut term);
    eprintln!("PROFILE open first diff (settled) {:?}", started.elapsed());
    app.active_pane = crate::app::ActivePane::Editor;
    for _ in 0..120 {
        mouse(&mut app, MouseEventKind::ScrollDown, 120, 20);
        samples.push(frame(&mut app, &mut term));
    }
    report("diff wheel scroll", &mut samples);
    for _ in 0..80 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("diff page down", &mut samples);

    // Open diffs for a sequence of files, timing until each is fully shown.
    let mut opens = Vec::new();
    for index in 1..20 {
        let started = Instant::now();
        app.open_source_control_diff(index);
        let mut frames = 0;
        loop {
            frame(&mut app, &mut term);
            frames += 1;
            let ready = app.background.active_job_count() == 0 && !app.background.has_pending_results();
            if ready || frames > 2000 { break; }
            app.wait_for_background(Duration::from_secs(20), |app| app.background.has_pending_results() || app.background.active_job_count() == 0);
        }
        opens.push(started.elapsed());
    }
    opens.sort();
    eprintln!("PROFILE open diff until idle: p50={:?} max={:?}", opens[opens.len() / 2], opens.last().unwrap());

    for _ in 0..40 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
        samples.push(frame(&mut app, &mut term));
    }
    report("diff next tab", &mut samples);

    // Editing the working tree file updates Source Control.
    let started = Instant::now();
    std::fs::write(root.join("file_00.rs"), format!("{edited}// more\n")).unwrap();
    app.refresh_source_control_async();
    loop {
        frame(&mut app, &mut term);
        if app.background.active_job_count() == 0 && !app.background.has_pending_results() { break; }
        app.wait_for_background(Duration::from_secs(20), |app| app.background.has_pending_results() || app.background.active_job_count() == 0);
    }
    eprintln!("PROFILE git status refresh until idle: {:?}", started.elapsed());
}

/// Frames until the screen stops changing and no worker is busy. Returns the
/// time of the last visible change and how many intermediate frames painted.
fn until_stable(app: &mut App, terminal: &mut Term) -> (Duration, usize) {
    let started = Instant::now();
    let (mut quiet, mut changes, mut last_change) = (0, 0, Duration::ZERO);
    let verbose = std::env::var_os("VTCODE_PROFILE_ROWS").is_some();
    let mut previous = verbose.then(|| LAST_FRAME.with(|frame| frame.borrow().clone()).unwrap_or_else(|| terminal.current_buffer_mut().clone()));
    let wake = app.background.wake_handle();
    while started.elapsed() < Duration::from_secs(20) {
        let observed = wake.generation();
        let sample = frame(app, terminal);
        if sample.new_bytes > 0 {
            changes += 1;
            last_change = started.elapsed();
            if let Some(before) = previous.as_mut() {
                // After draw the buffers are swapped; the painted one is the
                // "previous" buffer ratatui diffs against on the next frame.
                let painted = painted_buffer();
                let rows: Vec<u16> = (0..painted.area.height).filter(|&y| (0..painted.area.width)
                    .any(|x| painted[(x, y)] != before[(x, y)])).collect();
                let text = |y: u16| (0..painted.area.width).map(|x| painted[(x, y)].symbol()).collect::<String>();
                let before_text = |y: u16| (0..before.area.width).map(|x| before[(x, y)].symbol()).collect::<String>();
                eprintln!("    frame +{:>8.1?} bytes={:>6} rows={:?} first={:?}", started.elapsed(), sample.new_bytes,
                    rows, rows.first().map(|&y| text(y).trim_end().to_string()));
                if rows.len() == 1 {
                    let y = rows[0];
                    let column = (0..painted.area.width).find(|&x| painted[(x, y)] != before[(x, y)]).unwrap_or(0);
                    let tail = |t: String| t.chars().skip(usize::from(column).saturating_sub(20)).collect::<String>();
                    eprintln!("      at col {column}: was {:?} now {:?}", tail(before_text(y)).trim_end(), tail(text(y)).trim_end());
                }
                *before = painted;
            }
        }
        let idle = app.background.active_job_count() == 0 && !app.background.has_pending_results();
        if idle && sample.new_bytes == 0 {
            quiet += 1;
            if quiet >= 3 { break; }
        } else {
            quiet = 0;
        }
        // Paint as soon as any result arrives, like the event loop does.
        if !idle && !app.background.has_pending_results() {
            wake.wait_for_change(observed, Some(Duration::from_millis(50)));
        }
    }
    (last_change, changes)
}

/// The buffer presented by the most recent draw.
fn painted_buffer() -> ratatui::buffer::Buffer {
    LAST_FRAME.with(|frame| frame.borrow().clone().expect("a painted frame"))
}

thread_local! {
    static WIDGETS: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    static LAST_FRAME: std::cell::RefCell<Option<ratatui::buffer::Buffer>> = const { std::cell::RefCell::new(None) };
}

#[test]
#[ignore = "manual end-to-end action latency profile"]
fn profile_action_latency() {
    let (width, height) = (200u16, 55u16);
    let mut app = App::new(repo());
    let mut term = terminal(width, height);
    app.animation_settings = Default::default();
    if std::env::var_os("VTCODE_PROFILE_NO_ANIMATIONS").is_some() {
        for effect in crate::animation_config::AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    }
    settle(&mut app, &mut term);
    let log = |name: &str, (elapsed, changes): (Duration, usize)| {
        eprintln!("PROFILE latency {name:<34} stable after {elapsed:>9.1?} ({changes} painted frames)");
    };
    for file in ["src/app.rs", "src/ui_editor.rs", "src/explorer.rs", "src/highlight.rs", "src/app_tabs.rs",
        "src/app_git.rs", "src/ui_chat.rs", "src/remote_client.rs"] {
        app.open_file(&repo().join(file));
        log(&format!("open {file}"), until_stable(&mut app, &mut term));
    }
    for _ in 0..4 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
        log("next tab", until_stable(&mut app, &mut term));
    }
    key(&mut app, KeyCode::End, KeyModifiers::CONTROL);
    log("ctrl+end", until_stable(&mut app, &mut term));
    key(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
    log("ctrl+home", until_stable(&mut app, &mut term));
    for _ in 0..3 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        log("page down", until_stable(&mut app, &mut term));
    }
    for _ in 0..3 {
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        log("type x", until_stable(&mut app, &mut term));
    }
    // Explorer: expand and collapse folders.
    app.active_pane = crate::app::ActivePane::Explorer;
    app.explorer.selected = 0;
    for name in ["scripts", "themes", "vendor", "tests"] {
        if let Some(index) = app.explorer.entries.iter().position(|entry| entry.name == name) {
            app.explorer.selected = index;
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            log(&format!("explorer expand {name}"), until_stable(&mut app, &mut term));
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            log(&format!("explorer collapse {name}"), until_stable(&mut app, &mut term));
        }
    }
    app.active_pane = crate::app::ActivePane::Editor;
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    log("quick open", until_stable(&mut app, &mut term));
    for c in "uieditor".chars() {
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        log(&format!("quick open type {c}"), until_stable(&mut app, &mut term));
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    log("quick open accept", until_stable(&mut app, &mut term));
    key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    log("close tab", until_stable(&mut app, &mut term));
}

#[test]
#[ignore = "manual syntax job profile"]
fn profile_highlight_jobs() {
    use crate::highlight::Highlighter;
    let mut highlighter = Highlighter::new();
    let path = repo().join("src/app.rs");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut rope = ropey::Rope::from_str(&text);
    let (syntaxes, themes) = highlighter.shared_sets();
    let converge = |highlighter: &mut Highlighter, rope: &ropey::Rope, version: u64, changed: Option<(usize, usize)>, visible: std::ops::Range<usize>, name: &str| {
        let started = Instant::now();
        let mut jobs = 0;
        let mut first = None;
        let mut changed = changed;
        while let Some(job) = highlighter.prepare_job(rope, &path, version, changed.take(), visible.clone(), Some("rust"), "dark-plus") {
            let batch = job.run(rope, &syntaxes, &themes, || false);
            let lines = batch.entries.len();
            highlighter.apply_batch(batch);
            jobs += 1;
            first.get_or_insert((started.elapsed(), lines));
            if jobs > 1000 { break; }
        }
        eprintln!("PROFILE highlight {name:<32} {:>9.1?} jobs={jobs} first={first:?}", started.elapsed());
    };
    converge(&mut highlighter, &rope, 1, None, 0..105, "cold top viewport");
    converge(&mut highlighter, &rope, 1, None, 95..205, "scroll to 120");
    for (version, offset) in (2..6).zip(0..) {
        let index = rope.line_to_char(145) + offset;
        rope.insert_char(index, 'x');
        converge(&mut highlighter, &rope, version, Some((145, 145)), 95..205, "type at line 145");
    }
    converge(&mut highlighter, &rope, 5, None, 6500..6601, "jump to end (cold)");
    converge(&mut highlighter, &rope, 5, None, 3000..3105, "jump to middle");
    // Throughput: full file from a fresh highlighter.
    let mut fresh = Highlighter::new();
    let started = Instant::now();
    let mut lines = 0;
    for start in (0..rope.len_lines()).step_by(100) {
        while let Some(job) = fresh.prepare_job(&rope, &path, 1, None, start..(start + 100).min(rope.len_lines()), Some("rust"), "dark-plus") {
            let batch = job.run(&rope, &syntaxes, &themes, || false);
            lines += batch.entries.len();
            fresh.apply_batch(batch);
        }
    }
    let elapsed = started.elapsed();
    eprintln!("PROFILE highlight full file {} lines ({lines} entries) in {elapsed:?} = {:.1} us/line",
        rope.len_lines(), elapsed.as_secs_f64() * 1e6 / rope.len_lines() as f64);
}

#[test]
#[ignore = "manual large-workspace stress profile"]
fn profile_large_workspace_stress() {
    let (width, height) = (200u16, 55u16);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "profile@example.com"]);
    git(&root, &["config", "user.name", "profile"]);
    std::fs::create_dir(root.join("big")).unwrap();
    for index in 0..20_000 {
        std::fs::write(root.join("big").join(format!("item_{index:05}.txt")), "x\n").unwrap();
    }
    std::fs::create_dir(root.join("changes")).unwrap();
    for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{}}\n")).unwrap();
    }
    let source = std::fs::read_to_string(repo().join("src/app.rs")).unwrap();
    let huge: String = std::iter::repeat_n(source.as_str(), 9).collect();
    std::fs::write(root.join("huge.rs"), &huge).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{ changed(); }}\n")).unwrap();
    }
    let edited: String = huge.lines().enumerate().filter(|(i, _)| i % 499 != 7)
        .map(|(i, line)| if i % 61 == 3 { format!("{line} // edited\n") } else { format!("{line}\n") }).collect();
    std::fs::write(root.join("huge.rs"), &edited).unwrap();

    let started = Instant::now();
    let mut app = App::new(root.clone());
    app.animation_settings = Default::default();
    for effect in crate::animation_config::AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    let mut term = terminal(width, height);
    let log = |name: &str, (elapsed, changes): (Duration, usize)| {
        eprintln!("PROFILE stress {name:<36} stable after {elapsed:>9.1?} ({changes} painted frames)");
    };
    log("startup", until_stable(&mut app, &mut term));
    eprintln!("PROFILE stress startup wall {:?}", started.elapsed());

    // Explorer: expand a 20k-entry folder, scroll it, collapse it.
    app.active_pane = crate::app::ActivePane::Explorer;
    let big = app.explorer.entries.iter().position(|entry| entry.name == "big").unwrap();
    app.explorer.selected = big;
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    log("explorer expand 20k", until_stable(&mut app, &mut term));
    eprintln!("PROFILE stress explorer entries={}", app.explorer.entries.len());
    let mut samples = Vec::new();
    for _ in 0..100 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("explorer 20k down", &mut samples);
    for _ in 0..30 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("explorer 20k page down", &mut samples);
    if let Some(area) = app.explorer_area {
        for _ in 0..100 {
            mouse(&mut app, MouseEventKind::ScrollDown, area.x + 3, area.y + 5);
            samples.push(frame(&mut app, &mut term));
        }
    }
    report("explorer 20k wheel", &mut samples);
    app.explorer.selected = big;
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    log("explorer collapse 20k", until_stable(&mut app, &mut term));

    // Source control with 3k changes.
    app.execute_palette_command("source_control");
    log("source control 3k", until_stable(&mut app, &mut term));
    eprintln!("PROFILE stress source control files={}", app.source_control_files.len());
    for _ in 0..60 { samples.push(frame(&mut app, &mut term)); }
    report("source control 3k redraw", &mut samples);
    for _ in 0..100 {
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("source control 3k down", &mut samples);
    let started = Instant::now();
    std::fs::write(root.join("changes/file_0000.rs"), "fn again() {}\n").unwrap();
    app.refresh_source_control_async();
    log("source control 3k refresh", until_stable(&mut app, &mut term));
    eprintln!("PROFILE stress source control refresh wall {:?}", started.elapsed());

    // Huge-file diff.
    let huge_index = app.source_control_files.iter().position(|change| change.path.ends_with("huge.rs")).unwrap();
    app.open_source_control_diff(huge_index);
    log("open diff 59k lines", until_stable(&mut app, &mut term));
    app.active_pane = crate::app::ActivePane::Editor;
    for _ in 0..100 {
        mouse(&mut app, MouseEventKind::ScrollDown, 120, 20);
        samples.push(frame(&mut app, &mut term));
    }
    report("huge diff wheel", &mut samples);
    for _ in 0..40 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("huge diff page down", &mut samples);
    key(&mut app, KeyCode::End, KeyModifiers::CONTROL);
    log("huge diff ctrl+end", until_stable(&mut app, &mut term));

    // Huge file editing.
    app.open_file(&root.join("huge.rs"));
    log("open huge.rs", until_stable(&mut app, &mut term));
    for index in 0..60 {
        key(&mut app, KeyCode::Char((b'a' + (index % 26) as u8) as char), KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("huge.rs typing", &mut samples);
    key(&mut app, KeyCode::End, KeyModifiers::CONTROL);
    log("huge.rs ctrl+end", until_stable(&mut app, &mut term));
    for _ in 0..60 {
        key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
        samples.push(frame(&mut app, &mut term));
    }
    report("huge.rs typing at end", &mut samples);
    log("huge.rs settle after typing", until_stable(&mut app, &mut term));
}

#[test]
#[ignore = "manual Explorer expansion phase profile"]
fn profile_explorer_expand_phases() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    git(&root, &["init", "-q"]);
    std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    for folder in ["big", "node_modules"] {
        std::fs::create_dir(root.join(folder)).unwrap();
        for index in 0..20_000 {
            std::fs::write(root.join(folder).join(format!("item_{index:05}.txt")), "x\n").unwrap();
        }
    }
    std::fs::write(root.join("big/tracked.txt"), "x\n").unwrap();
    git(&root, &["add", ".gitignore", "big/tracked.txt"]);
    for (round, folder) in [(0, "big"), (1, "node_modules"), (2, "big"), (3, "node_modules")] {
        let big = root.join(folder);
        let started = Instant::now();
        let count = std::fs::read_dir(&big).unwrap().filter_map(Result::ok)
            .map(|entry| (entry.path(), entry.file_type().ok())).count();
        eprintln!("PROFILE explorer read_dir {count} entries: {:?}", started.elapsed());
        let mut explorer = crate::explorer::FileExplorer::new(root.clone());
        let mut expanded = rustc_hash::FxHashSet::default();
        expanded.insert(big.clone());
        let request = explorer.headless_request(expanded.clone(), crate::explorer::ExplorerSortMode::Name, None,
            crate::background::CancellationToken::new());
        let started = Instant::now();
        let (canonical, entries, matches) = crate::explorer::FileExplorer::build_refresh_entries(request);
        eprintln!("PROFILE explorer {folder} round {round} cold build {} entries: {:?}", entries.len(), started.elapsed());
        let request = explorer.headless_request(expanded, crate::explorer::ExplorerSortMode::Name, None,
            crate::background::CancellationToken::new());
        let started = Instant::now();
        let (canonical2, entries2, matches2) = crate::explorer::FileExplorer::build_refresh_entries(request);
        eprintln!("PROFILE explorer {folder} round {round} warm build {} entries: {:?}", entries2.len(), started.elapsed());
        let started = Instant::now();
        explorer.apply_refresh_entries(canonical, entries, matches);
        eprintln!("PROFILE explorer round {round} apply on UI: {:?}", started.elapsed());
        let started = Instant::now();
        explorer.apply_refresh_entries(canonical2, entries2, matches2);
        eprintln!("PROFILE explorer round {round} re-apply on UI: {:?}", started.elapsed());
    }
}

/// Loopback headless service with per-direction byte and message counters and
/// an optional simulated one-way delay before each message (half the RTT).
#[derive(Clone, Default)]
struct Traffic {
    up_bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    down_bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    up_messages: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    down_messages: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Traffic {
    fn take(&self) -> [usize; 4] {
        use std::sync::atomic::Ordering::Relaxed;
        [self.up_messages.swap(0, Relaxed), self.up_bytes.swap(0, Relaxed),
            self.down_messages.swap(0, Relaxed), self.down_bytes.swap(0, Relaxed)]
    }
}

struct Counted {
    stream: std::net::TcpStream,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    messages: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    marker: &'static [u8],
    delay: Duration,
}

impl std::io::Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use std::sync::atomic::Ordering::Relaxed;
        // Frame: PREFIX:session(16):id(16):sequence:last: (sequence 0 starts a message).
        let starts = memchr::memmem::find_iter(bytes, self.marker)
            .filter(|&at| bytes.get(at + 42) == Some(&b'0') && bytes.get(at + 43) == Some(&b':'))
            .count();
        if starts > 0 {
            self.messages.fetch_add(starts, Relaxed);
            if !self.delay.is_zero() { std::thread::sleep(self.delay); }
        }
        let written = self.stream.write(bytes)?;
        self.bytes.fetch_add(written, Relaxed);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.stream.flush() }
}

fn remote_connection(root: &std::path::Path, traffic: &Traffic, one_way: Duration) -> crate::remote::client::Client {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    stream.set_nodelay(true).unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nodelay(true).unwrap();
    let root = root.to_owned();
    let down = Counted { stream: server.try_clone().unwrap(), bytes: traffic.down_bytes.clone(),
        messages: traffic.down_messages.clone(), marker: b"VTCR30S:", delay: one_way };
    std::thread::spawn(move || { let _ = crate::remote::service::serve(&root, server, down); });
    let up = Counted { stream: stream.try_clone().unwrap(), bytes: traffic.up_bytes.clone(),
        messages: traffic.up_messages.clone(), marker: b"VTCR30C:", delay: one_way };
    crate::remote::client::Client::connect(stream, up).unwrap()
}

/// Like `until_stable`, but also waits out a quiet window: remote requests
/// in flight are not local background jobs.
fn until_stable_remote(app: &mut App, terminal: &mut Term) -> (Duration, usize) {
    let started = Instant::now();
    let (mut last_change, mut changes) = (Duration::ZERO, 0);
    let mut quiet_since = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        let (_, painted) = until_stable(app, terminal);
        if painted > 0 {
            changes += painted;
            last_change = started.elapsed();
            quiet_since = Instant::now();
        }
        if quiet_since.elapsed() >= Duration::from_millis(300) { break; }
        if std::env::var_os("VTCODE_PROFILE_BUSY").is_some() {
            let now = Instant::now();
            app.background.refresh_activity(now);
            let labels: Vec<_> = app.background.activity.active_labels().collect();
            if !labels.is_empty() || app.background.active_job_count() > 0 {
                eprintln!("PROFILE remote   +{:?} active={labels:?} jobs={}", started.elapsed(), app.background.active_job_count());
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    (last_change, changes)
}

#[test]
#[ignore = "manual remote (headless over loopback) operation profile"]
fn profile_remote_operations() {
    let rtt = Duration::from_millis(std::env::var("VTCODE_PROFILE_RTT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
    let files = ["app.rs", "ui_editor.rs", "explorer.rs", "highlight.rs", "app_tabs.rs", "app_git.rs", "remote_client.rs", "terminal.rs"];
    let traffic = Traffic::default();
    let dir = tempfile::tempdir().unwrap();
    // VTCODE_PROFILE_SSH="user@host|remote command" profiles a real SSH link
    // against a workspace prepared on the host with the same fixture.
    let (workspace, _ssh_owner) = if let Ok(spec) = std::env::var("VTCODE_PROFILE_SSH") {
        let (host, command) = spec.split_once('|').expect("host|command");
        let args = ["-o", "BatchMode=yes", "-e", "none", "-tt", host, command].map(std::ffi::OsString::from).to_vec();
        let attaching = Instant::now();
        let (client, owner) = crate::remote::launch::Launch::new("ssh".into(), args)
            .start(ratatui::layout::Size::new(120, 40), Default::default()).unwrap()
            .endpoint_for_test().unwrap();
        eprintln!("PROFILE remote ssh attach {:?}", attaching.elapsed());
        (crate::remote::workspace::Workspace::new(client), Some(owner))
    } else {
        let root = std::fs::canonicalize(dir.path()).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "profile@example.com"]);
        git(&root, &["config", "user.name", "profile"]);
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        for file in files { std::fs::copy(repo().join("src").join(file), root.join("src").join(file)).unwrap(); }
        for index in 0..300 {
            std::fs::write(root.join("src/nested").join(format!("m{index:03}.rs")), format!("pub fn f{index}() {{}}
")).unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "base"]);
        for file in &files[..4] {
            let path = root.join("src").join(file);
            let text = std::fs::read_to_string(&path).unwrap();
            let edited: String = text.lines().enumerate()
                .map(|(i, l)| if i % 150 == 2 { format!("{l} // changed
") } else { format!("{l}
") }).collect();
            std::fs::write(path, edited).unwrap();
        }
        (crate::remote::workspace::Workspace::new(remote_connection(&root, &traffic, rtt / 2)), None)
    };
    let started = Instant::now();
    let mut app = crate::startup::Prepared::load_remote(workspace.view_root.clone())
        .into_app_with_remote(crate::background::UiWake::default(), Some(workspace.clone()));
    app.animation_settings = Default::default();
    for effect in crate::animation_config::AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    let mut term = terminal(200, 55);
    let log = |name: &str, (elapsed, changes): (Duration, usize), traffic: &Traffic| {
        let [up_messages, up, down_messages, down] = traffic.take();
        let link = if std::env::var_os("VTCODE_PROFILE_SSH").is_some() { "ssh".to_string() } else { format!("rtt={rtt:?}") };
        eprintln!("PROFILE remote {link} {name:<28} stable {elapsed:>9.1?} frames={changes:<3} requests={up_messages:<4} up={up:>8}B replies={down_messages:<4} down={down:>9}B");
    };
    let busy = |app: &mut App| {
        let now = Instant::now();
        app.background.refresh_activity(now);
        if let Some(since) = app.background.activity.busy_since(now) {
            eprintln!("PROFILE remote   busy {:?} primary={:?} jobs={} active_jobs={}", now - since,
                app.background.activity.primary(now).map(|activity| activity.label),
                app.background.activity.job_count(), app.background.active_job_count());
        }
    };
    app.refresh_explorer_async();
    app.ensure_initial_source_control_status();
    log("startup", until_stable_remote(&mut app, &mut term), &traffic);
    eprintln!("PROFILE remote startup wall {:?} entries={}", started.elapsed(), app.explorer.entries.len());
    app.active_pane = crate::app::ActivePane::Explorer;
    for folder in ["src", "src/nested"] {
        let key_path = workspace.target_key(folder).unwrap();
        if let Some(index) = app.explorer.entries.iter().position(|entry| entry.path == key_path) {
            app.explorer.selected = index;
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            log(&format!("explorer expand {folder}"), until_stable_remote(&mut app, &mut term), &traffic);
        } else {
            eprintln!("PROFILE remote missing explorer entry {folder}");
        }
    }
    for file in files {
        app.open_file(&workspace.target_key(&format!("src/{file}")).unwrap());
        log(&format!("open {file}"), until_stable_remote(&mut app, &mut term), &traffic);
        busy(&mut app);
    }
    app.active_pane = crate::app::ActivePane::Editor;
    for _ in 0..3 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
        log("next tab", until_stable_remote(&mut app, &mut term), &traffic);
    }
    for _ in 0..3 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        log("page down", until_stable_remote(&mut app, &mut term), &traffic);
    }
    for c in "xyz".chars() {
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        log("type", until_stable_remote(&mut app, &mut term), &traffic);
    }
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    log("save", until_stable_remote(&mut app, &mut term), &traffic);
    key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    log("save again", until_stable_remote(&mut app, &mut term), &traffic);
    app.execute_palette_command("source_control");
    log("source control view", until_stable_remote(&mut app, &mut term), &traffic);
    eprintln!("PROFILE remote source control files={}", app.source_control_files.len());
    for index in 0..app.source_control_files.len().min(3) {
        app.open_source_control_diff(index);
        log("open diff", until_stable_remote(&mut app, &mut term), &traffic);
    }
    key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    log("close tab", until_stable_remote(&mut app, &mut term), &traffic);
    app.open_file(&workspace.target_key("src/app.rs").unwrap());
    log("reopen app.rs", until_stable_remote(&mut app, &mut term), &traffic);
    std::thread::sleep(Duration::from_secs(2));
    log("2s idle", until_stable_remote(&mut app, &mut term), &traffic);
    let now = Instant::now();
    app.background.refresh_activity(now);
    eprintln!("PROFILE remote busy={:?} primary={:?} jobs={} active_jobs={}",
        app.background.activity.busy_since(now).map(|since| now - since),
        app.background.activity.primary(now).map(|activity| (activity.label, activity.path.clone())),
        app.background.activity.job_count(), app.background.active_job_count());
}

/// Time a Source Control action: how long the call itself holds the UI
/// thread, and how long until the action and its refreshes have finished.
fn timed_action(app: &mut App, term: &mut Term, name: &str, action: impl FnOnce(&mut App)) {
    let started = Instant::now();
    action(app);
    let blocked = started.elapsed();
    app.wait_for_background(Duration::from_secs(60), |app| !app.bg_git_action_running);
    let (_, frames) = until_stable(app, term);
    eprintln!("PROFILE sc {name:<28} ui-blocked {blocked:>9.1?} done {:>9.1?} ({frames} frames)", started.elapsed());
}

#[test]
#[ignore = "manual Source Control action profile"]
fn profile_source_control_actions() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "profile@example.com"]);
    git(&root, &["config", "user.name", "profile"]);
    git(&root, &["config", "core.autocrlf", "false"]);
    std::fs::create_dir(root.join("changes")).unwrap();
    for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{}}\n")).unwrap();
    }
    let source = std::fs::read_to_string(repo().join("src/app.rs")).unwrap().replace("\r\n", "\n");
    std::fs::write(root.join("big.rs"), &source).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{ changed(); }}\n")).unwrap();
    }
    let edited: String = source.lines().enumerate()
        .map(|(i, l)| if i % 400 == 10 { format!("{l} // edited\n") } else { format!("{l}\n") }).collect();
    std::fs::write(root.join("big.rs"), &edited).unwrap();

    // As main() configures libgit2.
    if std::env::var_os("VTCODE_PROFILE_STRICT").is_none() { git2::opts::strict_object_creation(false); }
    let mut app = App::new(root.clone());
    app.animation_settings = Default::default();
    for effect in crate::animation_config::AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    let mut term = terminal(200, 55);
    app.execute_palette_command("source_control");
    until_stable(&mut app, &mut term);
    timed_action(&mut app, &mut term, "status refresh (3k)", |app| app.refresh_source_control_async());
    eprintln!("PROFILE sc files={}", app.source_control_files.len());

    let first = app.source_control_files.iter().find(|change| !change.staged).cloned().unwrap();
    timed_action(&mut app, &mut term, "stage one file", |app| app.git_stage_changes(vec![first.clone()]));
    let staged = app.source_control_files.iter().find(|change| change.staged).cloned().unwrap();
    timed_action(&mut app, &mut term, "unstage one file", |app| app.git_unstage_changes(vec![staged.clone()]));
    timed_action(&mut app, &mut term, "stage all (3k)", |app| app.git_stage_all());
    timed_action(&mut app, &mut term, "unstage all (3k)", |app| app.git_unstage_all());

    // Hunk actions on an open, clean file.
    app.open_file(&root.join("big.rs"));
    until_stable(&mut app, &mut term);
    app.refresh_git_line_status_async();
    until_stable(&mut app, &mut term);
    let big = root.join("big.rs");
    timed_action(&mut app, &mut term, "stage hunk", |app| app.git_stage_hunk(&big, 10));
    timed_action(&mut app, &mut term, "unstage hunk", |app| app.git_unstage_hunk(&big, 10));
    timed_action(&mut app, &mut term, "discard hunk", |app| app.git_discard_hunk(&big, 410));

    let victim = app.source_control_files.iter().find(|change| !change.staged && change.path.ends_with("file_0001.rs")).cloned().unwrap();
    timed_action(&mut app, &mut term, "discard one file", |app| app.git_discard_changes(vec![victim.clone()]));
    app.source_control_commit_msg = "profile commit".into();
    let to_commit = app.source_control_files.iter().filter(|change| !change.staged).take(1).cloned().collect::<Vec<_>>();
    timed_action(&mut app, &mut term, "stage for commit", |app| app.git_stage_changes(to_commit.clone()));
    let started = Instant::now();
    app.git_commit();
    let mut last = String::new();
    while started.elapsed() < Duration::from_secs(30) {
        app.poll_background_results();
        refresh_caches(&mut app);
        let now = Instant::now();
        app.background.refresh_activity(now);
        let labels: Vec<_> = app.background.activity.active_labels().map(|(label, age)| format!("{label}:{}ms", age.as_millis())).collect();
        let state = format!("action={} status={} pending={} labels={labels:?} jobs={}", app.bg_git_action_running,
            app.bg_git_status_running, app.bg_git_status_pending, app.background.active_job_count());
        let key = state.split(" labels").next().unwrap().to_string() + &labels.iter().map(|l| l.split(':').next().unwrap()).collect::<Vec<_>>().join(",");
        if key != last { eprintln!("PROFILE sc commit +{:?} {state}", started.elapsed()); last = key; }
        if !app.bg_git_action_running && app.background.active_job_count() == 0 && !app.bg_git_status_running { break; }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("PROFILE sc commit settled {:?}", started.elapsed());
    // The commit task itself, without the UI's follow-up refreshes.
    std::fs::write(root.join("changes/file_0002.rs"), "fn again() {}
").unwrap();
    git(&root, &["add", "changes/file_0002.rs"]);
    let started = Instant::now();
    crate::app_git::commit_repository(&root, "direct commit", false).unwrap();
    eprintln!("PROFILE sc commit task alone {:?}", started.elapsed());
    for (name, args) in [("git log refresh", vec!["log", "--oneline", "-50"]), ("git status", vec!["status", "--porcelain"])] {
        let started = Instant::now();
        git(&root, &args);
        eprintln!("PROFILE sc cli {name} {:?}", started.elapsed());
    }
}

#[test]
#[ignore = "manual stage-all phase profile"]
fn profile_stage_all_phases() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "profile@example.com"]);
    git(&root, &["config", "user.name", "profile"]);
    std::fs::create_dir(root.join("changes")).unwrap();
    for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{}}\n")).unwrap();
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    let modify = |round: usize| for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{ changed({round}); }}\n")).unwrap();
    };
    modify(1);
    let started = Instant::now();
    let repo = git2::Repository::open(&root).unwrap();
    crate::app_git::ignore_working_copies(&repo).unwrap();
    let mut index = repo.index().unwrap();
    eprintln!("PROFILE stage-all open+index {:?}", started.elapsed());
    let started = Instant::now();
    index.add_all(std::iter::once("*"), git2::IndexAddOption::DEFAULT, None).unwrap();
    eprintln!("PROFILE stage-all libgit2 add_all {:?}", started.elapsed());
    let started = Instant::now();
    index.write().unwrap();
    eprintln!("PROFILE stage-all index write {:?}", started.elapsed());
    git(&root, &["reset", "-q"]);
    modify(2);
    let started = Instant::now();
    git(&root, &["add", "-A"]);
    eprintln!("PROFILE stage-all git add -A {:?}", started.elapsed());
    let started = Instant::now();
    let statuses = repo.statuses(None).unwrap().len();
    eprintln!("PROFILE stage-all status after ({statuses}) {:?}", started.elapsed());
    let started = Instant::now();
    let statuses = repo.statuses(None).unwrap().len();
    eprintln!("PROFILE stage-all status again ({statuses}) {:?}", started.elapsed());
}

#[test]
#[ignore = "manual packed stage-all prototype"]
fn profile_packed_stage_all_prototype() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "profile@example.com"]);
    git(&root, &["config", "user.name", "profile"]);
    std::fs::create_dir(root.join("changes")).unwrap();
    for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{}}\n")).unwrap();
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    for index in 0..3_000 {
        std::fs::write(root.join("changes").join(format!("file_{index:04}.rs")), format!("fn f{index}() {{ packed(); }}\n")).unwrap();
    }
    let started = Instant::now();
    let repo = git2::Repository::open(&root).unwrap();
    let odb = repo.odb().unwrap();
    let mempack = odb.add_new_mempack_backend(1000).unwrap();
    let mut index = repo.index().unwrap();
    index.add_all(std::iter::once("*"), git2::IndexAddOption::DEFAULT, None).unwrap();
    let added = started.elapsed();
    let mut pack = git2::Buf::new();
    mempack.dump(&repo, &mut pack).unwrap();
    let mut writer = odb.packwriter().unwrap();
    writer.write_all(&pack).unwrap();
    writer.commit().unwrap();
    drop(writer);
    mempack.reset().unwrap();
    index.write().unwrap();
    eprintln!("PROFILE packed add_all {added:?}, total with pack ({} bytes) {:?}", pack.len(), started.elapsed());
    drop(index); drop(odb); drop(repo);
    let fsck = std::process::Command::new("git").args(["fsck", "--no-dangling"]).current_dir(&root).output().unwrap();
    eprintln!("PROFILE fsck ok={} {}", fsck.status.success(), String::from_utf8_lossy(&fsck.stderr).lines().take(3).collect::<Vec<_>>().join(" | "));
    let staged = std::process::Command::new("git").args(["diff", "--cached", "--stat"]).current_dir(&root).output().unwrap();
    eprintln!("PROFILE staged: {}", String::from_utf8_lossy(&staged.stdout).lines().last().unwrap_or(""));
    let unstaged = std::process::Command::new("git").args(["status", "--porcelain", "--untracked-files=no"]).current_dir(&root).output().unwrap();
    eprintln!("PROFILE worktree-vs-index changes: {}", String::from_utf8_lossy(&unstaged.stdout).lines().filter(|l| l.as_bytes().get(1) == Some(&b'M')).count());

    // Unstage all as the task does (read_tree), then status timings.
    let repo = git2::Repository::open(&root).unwrap();
    let mut index = repo.index().unwrap();
    index.read_tree(&repo.head().unwrap().peel_to_tree().unwrap()).unwrap();
    index.write().unwrap();
    for round in 0..2 {
        let started = Instant::now();
        let count = repo.statuses(None).unwrap().len();
        eprintln!("PROFILE status after read_tree #{round}: {count} in {:?}", started.elapsed());
    }
    let started = Instant::now();
    let mut options = git2::StatusOptions::new();
    options.update_index(true);
    let count = repo.statuses(Some(&mut options)).unwrap().len();
    eprintln!("PROFILE status update_index: {count} in {:?}", started.elapsed());
    let started = Instant::now();
    let count = repo.statuses(None).unwrap().len();
    eprintln!("PROFILE status after update_index: {count} in {:?}", started.elapsed());
}

#[test]
#[ignore = "manual search, Quick Open and startup profile"]
fn profile_search_quick_open_and_startup() {
    let started = Instant::now();
    let mut app = App::new(repo());
    app.animation_settings = Default::default();
    for effect in crate::animation_config::AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    let mut term = terminal(200, 55);
    let constructed = started.elapsed();
    frame(&mut app, &mut term);
    let first_frame = started.elapsed();
    let (settled, _) = until_stable(&mut app, &mut term);
    eprintln!("PROFILE startup App::new {constructed:?}, first frame {first_frame:?}, settled +{settled:?}");

    for query in ["fn ", "refresh_caches", "zzqqxx_not_found", "(?i)cancellationtoken"] {
        app.global_search_query = query.into();
        app.global_search_regex = query.starts_with("(?i)");
        let started = Instant::now();
        app.update_global_search();
        let mut first = None;
        while started.elapsed() < Duration::from_secs(30) {
            app.poll_pending_global_search(Instant::now());
            app.poll_background_results();
            if first.is_none() && app.global_search_results.file_count() > 0 { first = Some(started.elapsed()); }
            if !app.bg_global_search_running && app.global_search_debounce_wait(Instant::now()).is_none() { break; }
            std::thread::sleep(Duration::from_millis(1));
        }
        eprintln!("PROFILE search {query:<24} first results {first:?} done {:?} files={}", started.elapsed(),
            app.global_search_results.file_count());
    }

    let started = Instant::now();
    app.execute_palette_command("quick_open");
    frame(&mut app, &mut term);
    let opened = started.elapsed();
    let (settled, _) = until_stable(&mut app, &mut term);
    eprintln!("PROFILE quick open first frame {opened:?}, settled +{settled:?}");
    for c in "appevent".chars() {
        let started = Instant::now();
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        let (settled, _) = until_stable(&mut app, &mut term);
        eprintln!("PROFILE quick open type {c} {:?} (stable +{settled:?})", started.elapsed());
    }
}
