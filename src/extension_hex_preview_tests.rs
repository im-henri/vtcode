//! TEMPORARY harness: render the hex editor from HEX_WASM over a large file
//! and time input. Not committed.
use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::path::Path;
use std::time::{Duration, Instant};

fn app(root: &Path) -> App {
    let wasm = std::fs::read(std::env::var("HEX_WASM").expect("HEX_WASM")).unwrap();
    let dir = std::path::PathBuf::from(std::env::var("HEX_WASM").unwrap());
    let manifest = std::fs::read(dir.parent().unwrap().join("extension.json")).unwrap();
    let package = root.join("extensions/hex-editor");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("extension.json"), manifest).unwrap();
    std::fs::write(package.join("hex-editor.wasm"), wasm).unwrap();
    let mut app = App::new(root.to_path_buf());
    app.extension_manager.search_roots = vec![root.join("extensions")];
    app.extension_manager.settings_file = root.join("state/extension-settings.json");
    app.extension_manager.scan_and_sync();
    assert_eq!(app.extension_manager.error_count(), 0);
    app
}

fn drain(app: &mut App) {
    for _ in 0..100000 {
        app.extension_manager
            .dispatch_view_events(&mut app.background);
        app.wait_for_background(Duration::from_secs(60), |app| {
            !app.extension_manager.has_busy_runtimes()
        });
        if app.extension_manager.pending_view_events.is_empty() {
            return;
        }
    }
    panic!("queue did not drain");
}

fn dump(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn draw(terminal: &mut Terminal<TestBackend>, app: &mut App) {
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    assert!(app.handle_active_extension_view_key(code, modifiers));
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

/// Ticks the search/replace background task until the editor stops asking.
fn run_tasks(app: &mut App) -> u32 {
    let mut ticks = 0;
    loop {
        let handlers: Vec<String> = app.extension_manager.records[0]
            .tick_handlers
            .iter()
            .map(|handler| handler.export_name.clone())
            .collect();
        if handlers.is_empty() {
            return ticks;
        }
        app.extension_manager
            .invoke_tick_handlers_async("vtcode.hex-editor", handlers, &mut app.background, None)
            .unwrap();
        drain(app);
        ticks += 1;
        assert!(ticks < 1_000_000);
    }
}

#[test]
#[ignore]
fn hex_preview() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("big.bin");
    let size: u64 = std::env::var("HEX_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256 * 1024 * 1024);
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&path).unwrap();
        let mut block = vec![0u8; 1024 * 1024];
        for (i, byte) in block.iter_mut().enumerate() {
            *byte = if i % 7 == 0 { 0 } else { (i * 31 % 251) as u8 };
        }
        block[..16].copy_from_slice(b"VTCODE HEX DEMO\n");
        let mut left = size;
        while left > 0 {
            let n = left.min(block.len() as u64) as usize;
            file.write_all(&block[..n]).unwrap();
            left -= n as u64;
        }
    }
    let mut app = app(root.path());
    let (w, h) = (
        std::env::var("HEX_W")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(150u16),
        std::env::var("HEX_H")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(45u16),
    );
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let t = Instant::now();
    assert!(app.run_extension_command_with_optional_context(
        "vtcode.hex-editor",
        "hex.open",
        Some(&path)
    ));
    drain(&mut app);
    draw(&mut terminal, &mut app);
    drain(&mut app);
    draw(&mut terminal, &mut app);
    eprintln!("open: {:?}", t.elapsed());
    eprintln!("{}", dump(&terminal));
    let area = app.code_area.unwrap();

    let t = Instant::now();
    for _ in 0..50 {
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        drain(&mut app);
    }
    eprintln!(
        "50 x PageDown: {:?} ({:?}/key)",
        t.elapsed(),
        t.elapsed() / 50
    );
    let t = Instant::now();
    for _ in 0..50 {
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            area.x + 20,
            area.y + 10,
        );
        drain(&mut app);
    }
    eprintln!(
        "50 x wheel: {:?} ({:?}/tick)",
        t.elapsed(),
        t.elapsed() / 50
    );
    let t = Instant::now();
    for _ in 0..50 {
        mouse(
            &mut app,
            MouseEventKind::Moved,
            area.x + 20 + (t.elapsed().as_micros() % 30) as u16,
            area.y + 10,
        );
        drain(&mut app);
    }
    eprintln!("50 x hover: {:?}", t.elapsed());
    // Hover over the toolbar and the grid to show hover states.
    mouse(&mut app, MouseEventKind::Moved, area.x + 30, area.y + 8);
    drain(&mut app);
    draw(&mut terminal, &mut app);
    drain(&mut app);
    draw(&mut terminal, &mut app);
    eprintln!("{}", dump(&terminal));

    // Find a missing pattern: worst case, scans the whole file.
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    drain(&mut app);
    for ch in "ZQXJ".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    drain(&mut app);
    draw(&mut terminal, &mut app);
    eprintln!("{}", dump(&terminal));
    let t = Instant::now();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    drain(&mut app);
    let ticks = run_tasks(&mut app);
    let elapsed = t.elapsed();
    eprintln!(
        "find missing in {} MiB: {:?} over {ticks} task calls ({:.0} MiB/s busy; at 16 ms/tick wall ≈ {:.1}s)",
        size >> 20,
        elapsed,
        size as f64 / 1048576.0 / elapsed.as_secs_f64(),
        ticks as f64 * 0.016
    );
    draw(&mut terminal, &mut app);
    drain(&mut app);
    draw(&mut terminal, &mut app);
    eprintln!("{}", dump(&terminal));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    drain(&mut app);
    // Drag the scrollbar thumb (if any) to the middle.
    let right = area.x + area.width - 1;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        right,
        area.y + 4,
    );
    drain(&mut app);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        right,
        area.y + area.height / 2,
    );
    drain(&mut app);
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        right,
        area.y + area.height / 2,
    );
    drain(&mut app);
    draw(&mut terminal, &mut app);
    drain(&mut app);
    draw(&mut terminal, &mut app);
    eprintln!("{}", dump(&terminal));
}
