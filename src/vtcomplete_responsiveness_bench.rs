//! Opt-in UI replay using real source, actual completion workers and a single
//! CPU on Windows. TestBackend measures application painting, not a terminal.
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

#[cfg(windows)]
struct Affinity(usize);
#[cfg(windows)]
impl Affinity {
    fn single_cpu() -> Self {
        unsafe {
            use windows_sys::Win32::System::Threading::{
                GetCurrentProcess, GetProcessAffinityMask, SetProcessAffinityMask,
            };
            let mut process = 0;
            let mut system = 0;
            assert_ne!(
                GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system),
                0
            );
            let first = process & process.wrapping_neg();
            assert_ne!(SetProcessAffinityMask(GetCurrentProcess(), first), 0);
            Self(process)
        }
    }
}
#[cfg(windows)]
impl Drop for Affinity {
    fn drop(&mut self) {
        unsafe {
            use windows_sys::Win32::System::Threading::{
                GetCurrentProcess, SetProcessAffinityMask,
            };
            let _ = SetProcessAffinityMask(GetCurrentProcess(), self.0);
        }
    }
}

fn summary(values: &mut [f64]) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    let at = |percent| values[(values.len() - 1) * percent / 100];
    serde_json::json!({"samples":values.len(),"p50Ms":at(50),"p95Ms":at(95),"maxMs":at(100)})
}

#[test]
#[ignore = "isolated single-CPU foreground replay with real source and optional completion pressure"]
fn benchmark_vtcomplete_foreground_responsiveness() {
    #[cfg(windows)]
    let _affinity = Affinity::single_cpu();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let names = [
        "app_vtcomplete.rs",
        "vtcomplete_worker.rs",
        "completion_index.rs",
        "completion_context.rs",
        "completion_examples.rs",
        "vtcomplete_scoring.rs",
        "completion_patterns.rs",
        "completion_storage.rs",
    ];
    let texts: Vec<_> = names
        .iter()
        .map(|name| std::fs::read_to_string(source.join(name)).unwrap())
        .collect();
    let mut rows = Vec::new();
    for optional_work in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let mut app = App::new_deferred(root.clone());
        app.settings_persistence_enabled = false;
        app.show_explorer = false;
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.cursor_blinking = false;
        app.quick_suggestions = optional_work;
        for effect in crate::animation_config::AnimationEffect::ALL {
            app.animation_settings.set(effect, false);
        }
        app.tabs.clear();
        for (n, text) in texts.iter().enumerate() {
            let path = root.join(names[n]);
            let text = format!("{text}\nprint_value print_line\npri");
            std::fs::write(&path, &text).unwrap();
            let mut editor = Editor::test_with_text(&text);
            editor.file_path = Some(path);
            editor.language_override = Some("Rust".into());
            editor.cursor_line = editor.rope.len_lines().saturating_sub(1);
            editor.cursor_col = 3;
            app.tabs.push(Tab::new_file(editor));
        }
        app.active_tab = 0;
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        if optional_work {
            app.completion_index = crate::completion_index::CompletionIndex::new_for_test(
                root.clone(),
                temporary.path().join("cache"),
            );
            app.poll_completion_index();
            // Queue 128 real-source snapshots, well beyond the visible tab set.
            for n in 0..128 {
                let text = &texts[n % texts.len()];
                if !app.completion_index.observe_buffer(
                    root.join(format!("queued{n}.rs")),
                    ropey::Rope::from_str(text),
                    true,
                ) {
                    break;
                }
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let (_send, receive) = std::sync::mpsc::channel();
        let mut pump = crate::app_event_loop::TerminalEventPump::from_receiver(receive);
        let mut dispatch_ms = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut frame_ms = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for n in 0..300 {
            let case = n % 5;
            let foreground = app.vtcomplete_worker.activity().enter();
            let started = Instant::now();
            match case {
                0 => {
                    crate::app_event_loop::handle_keyboard_input(
                        &mut app,
                        &mut pump,
                        KeyEvent::new(
                            if n % 2 == 0 {
                                KeyCode::Up
                            } else {
                                KeyCode::Down
                            },
                            KeyModifiers::NONE,
                        ),
                    );
                }
                1 => {
                    let area = app.code_area.unwrap();
                    crate::app_event_loop::handle_mouse_input(
                        &mut app,
                        &mut pump,
                        MouseEvent {
                            kind: if n % 2 == 0 {
                                MouseEventKind::ScrollUp
                            } else {
                                MouseEventKind::ScrollDown
                            },
                            column: area.x + 2,
                            row: area.y + 2,
                            modifiers: KeyModifiers::NONE,
                        },
                    );
                }
                2 => app.open_file(&root.join(names[(n / 5) % names.len()])),
                3 => {
                    crate::app_event_loop::handle_keyboard_input(
                        &mut app,
                        &mut pump,
                        KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL),
                    );
                }
                _ => {
                    crate::app_event_loop::handle_keyboard_input(
                        &mut app,
                        &mut pump,
                        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                    );
                }
            }
            dispatch_ms[case].push(started.elapsed().as_secs_f64() * 1000.);
            if optional_work {
                app.update_vtcomplete_explicit();
            }
            // Production polling must defer its optional inventory work too.
            app.poll_completion_index();
            let frame = Instant::now();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            frame_ms[case].push(frame.elapsed().as_secs_f64() * 1000.);
            drop(foreground);
        }
        let mut resumed = true;
        if optional_work {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.vtcomplete_cancel_token.is_some() && Instant::now() < deadline {
                app.poll_background_results();
                std::thread::sleep(Duration::from_millis(2));
            }
            resumed = app.vtcomplete_cancel_token.is_none();
            assert!(
                resumed,
                "the latest completion must finish after input settles"
            );
            app.completion_index.cancel();
        }
        let cases: Vec<_> = ["cursor", "scroll", "open-existing-file", "tab", "typing"]
            .into_iter()
            .enumerate()
            .map(|(n, case)| {
                serde_json::json!({"case":case,"dispatch":summary(&mut dispatch_ms[n]),
                "paint":summary(&mut frame_ms[n])})
            })
            .collect();
        rows.push(serde_json::json!({"optionalCompletionWork":optional_work,"resumedAfterInput":resumed,"cases":cases}));
    }
    let mut inventory = inventory_app(10_000);
    let mut inventory_ms = Vec::new();
    loop {
        let now = Instant::now();
        inventory.poll_completion_index();
        inventory_ms.push(now.elapsed().as_secs_f64() * 1000.);
        if inventory.completion_index_next_wait().is_none() {
            break;
        }
    }
    let report = serde_json::json!({"platform":std::env::consts::OS,"singleCpu":cfg!(windows),
        "sourceFiles":names,"sourceBytes":texts.iter().map(String::len).sum::<usize>(),"runs":rows,
        "tenThousandTabInventory":summary(&mut inventory_ms),"inventoryVisitLimit":INVENTORY_PER_TURN,
        "description":"Current implementation, completion disabled versus queued completion/index work; real sources, actual input dispatch and TestBackend paint, 300 operations per run. Opening revisits an existing file. Setup, disk loading and terminal/compositor time excluded; 10,000-tab inventory is a separate isolated service measurement."});
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(path) = std::env::var_os("VTCODE_RESPONSIVENESS_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
