use super::*;
use crate::app::Tab;
use std::path::Path;

fn fixture(filename: &str, content: &str) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(content);
    editor.file_path = Some(filename.into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app
}
fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < until,
            "language detection did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn settle(app: &mut App) {
    let mut now = Instant::now();
    wait(|| {
        app.poll_language_detection(now);
        if let Some(wait) = app.language_detection_wait(now) {
            now += wait;
        }
        app.editor()
            .unwrap()
            .language_detection
            .as_ref()
            .is_some_and(|observation| {
                observation.checked(app.editor().unwrap(), app.highlighter.registry_generation())
            })
    });
}
fn canonical(app: &App, token: &str) -> String {
    app.highlighter
        .shared_sets()
        .0
        .find_syntax_by_token(token)
        .unwrap()
        .name
        .clone()
}

#[test]
fn fallback_updates_language_and_syntax_without_creating_a_manual_override() {
    let mut app = fixture(
        "nonexistent.language-mystery",
        "def greet(name):\n    print(name)\n",
    );
    settle(&mut app);
    let python = canonical(&app, "python");
    assert_eq!(app.editor().unwrap().language_name(), python);
    assert_eq!(
        app.editor().unwrap().syntax_language_token(),
        Some(python.as_str())
    );
    assert!(app.editor().unwrap().language_override.is_none());
    wait(|| !app.background.has_active_jobs());
    let dispatched = app.language_detector.last_dispatch;
    for _ in 0..100 {
        assert!(!app.poll_language_detection(Instant::now()));
        assert!(app.language_detection_wait(Instant::now()).is_none());
    }
    assert_eq!(
        app.language_detector.last_dispatch, dispatched,
        "idle detection must not schedule work"
    );
}

#[test]
fn registered_names_extensions_manual_modes_and_associations_take_precedence() {
    for filename in [
        "program.py",
        "program.c",
        "program.txt",
        "Makefile",
        "program.zig",
    ] {
        let mut app = fixture(filename, "fn main() { println!(\"hello\"); }\n");
        let expected = if filename.ends_with(".zig") {
            canonical(&app, "zig")
        } else {
            app.editor().unwrap().language_name().to_owned()
        };
        settle(&mut app);
        assert_eq!(app.editor().unwrap().language_name(), expected);
        assert!(app.editor().unwrap().language_override.is_none());
        assert!(
            !app.language_detector.cache.lock().unwrap().initialized,
            "known filenames do not invoke content classification"
        );
    }
    for manual in ["Plain Text", "JSON"] {
        let mut app = fixture("program.language-mystery", "fn main() {}\n");
        app.set_editor_language(Some(manual));
        settle(&mut app);
        assert_eq!(app.editor().unwrap().language_name(), manual);
        assert!(app.editor().unwrap().inferred_language.is_none());
        assert!(app.language_detector.last_dispatch.is_none());
    }
    let mut app = fixture("program.language-mystery", "def greet():\n    pass\n");
    app.editor_mut().unwrap().set_language_associations(
        &std::iter::once(("*.language-mystery".to_owned(), "Plain Text".to_owned())).collect(),
    );
    settle(&mut app);
    assert_eq!(app.editor().unwrap().language_name(), "Plain Text");
    assert!(app.language_detector.last_dispatch.is_none());
}

#[test]
fn work_runs_on_a_background_thread_and_wakes_the_ui() {
    let mut app = fixture(
        "program.language-mystery",
        "use std::io;\nfn main() { println!(\"hello\"); }\n",
    );
    let wake = app.background.wake_handle();
    let before = wake.generation();
    app.poll_language_detection(Instant::now());
    wait(|| app.language_detector.reply.lock().unwrap().is_some());
    assert_ne!(
        app.language_detector
            .reply
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .worker,
        std::thread::current().id()
    );
    wait(|| wake.generation() != before);
    assert!(app.poll_language_detection(Instant::now()));
    assert_eq!(
        app.editor().unwrap().language_name(),
        canonical(&app, "rust")
    );
}

#[test]
fn edits_are_coalesced_and_do_not_create_an_idle_detection_loop() {
    let mut app = fixture("notes.language-mystery", "Ordinary prose\n");
    settle(&mut app);
    let last = app.language_detector.last_dispatch.unwrap();
    // Even the first keystroke after a long idle period waits for quiet;
    // the dispatch-rate limit alone cannot provide this debounce.
    let begin = last + Duration::from_secs(1);
    for index in 0..100 {
        let editor = app.editor_mut().unwrap();
        crate::editor_document::edit!(editor, {
            editor.rope = ropey::Rope::from_str(&format!("def function_{index}():\n    pass\n"));
            editor.content_version += 1;
        });
        app.poll_language_detection(begin + Duration::from_millis(index));
        assert_eq!(app.language_detector.last_dispatch, Some(last));
    }
    assert!(app.language_detector.queued.is_some());
    assert!(
        app.language_detection_wait(begin + Duration::from_millis(100))
            .unwrap()
            >= MIN_INTERVAL
    );
    settle(&mut app);
    assert_eq!(
        app.editor().unwrap().language_name(),
        canonical(&app, "python")
    );
}

#[test]
fn late_results_cannot_replace_a_manual_choice_or_another_document() {
    let mut app = fixture("first.language-mystery", "def greet():\n    pass\n");
    app.editor_mut().unwrap().publish_document();
    let observation =
        Observation::capture(app.editor().unwrap(), app.highlighter.registry_generation());
    let view = app.editor().unwrap().navigation_id;
    app.set_editor_language(Some("Plain Text"));
    *app.language_detector.reply.lock().unwrap() = Some(Reply {
        view,
        observation: observation.clone(),
        language: Some("Python".into()),
        worker: std::thread::current().id(),
    });
    app.poll_language_detection(Instant::now());
    assert_eq!(app.editor().unwrap().language_name(), "Plain Text");
    let mut other = Editor::test_with_text("Some text\n");
    other.file_path = Some("second.language-mystery".into());
    app.tabs.push(Tab::new_file(other));
    app.active_tab = 1;
    *app.language_detector.reply.lock().unwrap() = Some(Reply {
        view,
        observation,
        language: Some("Python".into()),
        worker: std::thread::current().id(),
    });
    app.poll_language_detection(Instant::now());
    assert!(app.editor().unwrap().inferred_language.is_none());
}

#[test]
fn reloads_and_save_as_recheck_content_even_when_the_version_resets() {
    let mut app = fixture("reload.language-mystery", "def greet():\n    pass\n");
    settle(&mut app);
    let editor = app.editor_mut().unwrap();
    editor.apply_bg_loaded(
        ropey::Rope::from_str("{\"items\": [1, 2]}"),
        "UTF-8",
        18,
        u64::MAX,
    );
    assert_eq!(editor.content_version, 0);
    assert!(editor.inferred_language.is_none());
    settle(&mut app);
    assert_eq!(
        app.editor().unwrap().language_name(),
        canonical(&app, "json")
    );
    app.editor_mut().unwrap().file_path = Some("renamed.py".into());
    assert_eq!(app.editor().unwrap().language_name(), "Python");
    settle(&mut app);
    assert_eq!(
        app.editor().unwrap().language_name(),
        canonical(&app, "python")
    );
}

#[test]
fn queued_snapshots_are_bounded_and_untitled_buffers_are_supported() {
    let mut app = fixture("prefix.language-mystery", "Some text\n");
    settle(&mut app);
    let editor = app.editor_mut().unwrap();
    crate::editor_document::edit!(editor, {
        editor.file_path = None;
        editor.rope = ropey::Rope::from_str(&format!(
            "fn main() {{\n{}}}\n",
            "    println!(\"hello\");\n".repeat(100_000)
        ));
        editor.content_version += 1;
    });
    let last = app.language_detector.last_dispatch.unwrap();
    app.poll_language_detection(last);
    let request = app.language_detector.queued.as_ref().unwrap();
    assert!(request.snapshot.retained_bytes() <= language_detection::SAMPLE_BLOCK_BYTES * 2 + 6);
    assert!(request.snapshot.retained_bytes() < app.editor().unwrap().rope.len_bytes());
    settle(&mut app);
    assert_eq!(
        app.editor().unwrap().language_name(),
        canonical(&app, "rust")
    );
    assert!(
        app.editor()
            .unwrap()
            .inferred_language
            .as_ref()
            .unwrap()
            .path
            .is_none()
    );
    assert!(!Path::new("prefix.language-mystery").exists());
}

#[test]
fn remote_preview_buffers_are_classified_without_filesystem_access() {
    let mut app = fixture(
        "placeholder",
        "const std = @import(\"std\");\npub fn main() void {\n    std.debug.print(\"Hello world!\\n\", .{});\n}\n",
    );
    let path = std::path::PathBuf::from("\0vtcode-remote-language-test")
        .join("workspace")
        .join("source.unknown-extension");
    assert!(crate::remote::workspace::is_view_key(&path));
    app.editor_mut().unwrap().file_path = Some(path.clone());
    app.tabs[0].preview = true;
    settle(&mut app);
    assert_eq!(
        app.editor().unwrap().language_name(),
        canonical(&app, "zig")
    );
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&path));
    assert!(app.tabs[0].preview);
    assert!(app.editor().unwrap().language_override.is_none());
}

#[test]
fn grammar_modelines_override_statistical_predictions() {
    let mut app = fixture(
        "source.unknown-extension",
        "// -*- mode: python -*-\nfn main() { println!(\"hello\"); }\n",
    );
    settle(&mut app);
    assert_eq!(
        app.editor().unwrap().language_name(),
        canonical(&app, "python")
    );
    app.set_editor_language(Some("JSON"));
    settle(&mut app);
    assert_eq!(app.editor().unwrap().language_name(), "JSON");
}

#[test]
fn inferred_mode_is_shared_between_views_without_becoming_a_manual_override() {
    let mut app = fixture(
        "shared.language-mystery",
        "def greet(name):\n    print(name)\n",
    );
    let mut other = Editor::new();
    other.copy_document_from(app.editor().unwrap());
    app.tabs.push(Tab::new_file(other));
    settle(&mut app);
    app.tabs[1].editor.refresh_document();
    let expected = canonical(&app, "python");
    for tab in &app.tabs {
        assert_eq!(tab.editor.language_name(), expected);
        assert!(tab.editor.language_override.is_none());
    }
}
