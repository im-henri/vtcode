use super::Buffer;
use crate::editor::Editor;
use crate::extension_manifest::{EditorContext, EditorContextSnapshot};
use crate::extension_wasm::{
    HostState, apply_invocation_context, call_noarg_export, clear_invocation_context,
};
use ropey::Rope;
use std::sync::{Arc, Barrier};
use wasmtime::{Instance, Linker, Store};

const GUEST: &str = r#"(module
    (import "vtcode" "host_active_buffer_size" (func $size (result i32)))
    (import "vtcode" "host_read_active_buffer" (func $read (param i32 i32 i32) (result i32)))
    (memory (export "memory") 1)
    (func (export "no_read") (result i32) i32.const 0)
    (func (export "size_only") (result i32) call $size)
    (func (export "read") (param i32 i32 i32) (result i32)
        local.get 0 local.get 1 local.get 2 call $read)
    (func (export "trap") (result i32) unreachable))"#;

struct Guest {
    store: Store<HostState>,
    instance: Instance,
}

impl Guest {
    fn new(buffer: Option<Buffer>) -> Self {
        Self::with_module(buffer, GUEST)
    }

    fn with_module(buffer: Option<Buffer>, source: &str) -> Self {
        let engine = crate::extensions::test_extension_engine();
        let module = crate::extensions::test_extension_module(engine, source.as_bytes()).unwrap();
        let mut linker = Linker::new(engine);
        crate::extension_host::define_host_functions(&mut linker).unwrap();
        let mut store = Store::new(engine, HostState::default());
        store.set_fuel(1_000_000).unwrap();
        apply_invocation_context(
            store.data_mut(),
            None,
            None,
            Some(EditorContext {
                active_buffer: buffer,
                ..Default::default()
            }),
        );
        let instance = linker.instantiate(&mut store, &module).unwrap();
        Self { store, instance }
    }

    fn invoke(&mut self, export: &str) -> i32 {
        call_noarg_export(&mut self.store, &self.instance, export, true)
            .unwrap()
            .unwrap()
    }

    fn read(&mut self, offset: i32, ptr: i32, len: i32) -> i32 {
        self.instance
            .get_typed_func::<(i32, i32, i32), i32>(&mut self.store, "read")
            .unwrap()
            .call(&mut self.store, (offset, ptr, len))
            .unwrap()
    }

    fn memory(&mut self, offset: usize, len: usize) -> Vec<u8> {
        let memory = self.instance.get_memory(&mut self.store, "memory").unwrap();
        memory.data(&self.store)[offset..offset + len].to_vec()
    }
}

#[test]
fn metadata_only_guests_and_context_clones_share_the_rope() {
    let editor = Editor::test_with_text(&"café 🦀\r\n".repeat(100_000));
    let buffer = editor.extension_buffer_snapshot();
    let snapshot = EditorContextSnapshot {
        context: EditorContext {
            active_buffer: Some(buffer.clone()),
            ..Default::default()
        },
    };
    let context = snapshot.clone().into_context();
    assert!(context.active_buffer.as_ref().unwrap().shares(&buffer));
    assert!(
        snapshot
            .into_context()
            .active_buffer
            .unwrap()
            .shares(&buffer)
    );
    assert!(format!("{context:?}").contains("Buffer"));
    let mut guest = Guest::new(context.active_buffer);
    for _ in 0..10 {
        assert_eq!(guest.invoke("no_read"), 0);
        assert_eq!(guest.invoke("size_only"), editor.rope.len_bytes() as i32);
    }
}

#[test]
fn guests_read_raw_unicode_and_crlf_bytes_across_rope_chunks() {
    let text = "ab界e\u{301}🦀\r\n".repeat(200);
    let rope = Rope::from_str(&text);
    assert!(rope.chunks().count() > 1);
    let buffer = Buffer::from_rope(&rope);
    let mut guest = Guest::new(Some(buffer.clone()));
    // One-byte reads intentionally split UTF-8 sequences and rope chunks.
    for offset in 0..text.len() {
        assert_eq!(guest.read(offset as i32, 3, 1), 1);
        assert_eq!(guest.memory(3, 1), &text.as_bytes()[offset..offset + 1]);
    }
    assert_eq!(guest.read(0, 17, i32::MAX), text.len() as i32);
    assert_eq!(guest.memory(17, text.len()), text.as_bytes());
    assert_eq!(guest.read(text.len() as i32 - 2, 65_534, 100), 2);
    assert_eq!(guest.memory(65_534, 2), b"\r\n");
}

#[test]
fn guests_stream_a_large_snapshot_in_small_reads_without_changing_shared_text() {
    let text = "界🦀\r\ne\u{301}".repeat(50_000);
    let rope = Rope::from_str(&text);
    let buffer = Buffer::from_rope(&rope);
    let mut guest = Guest::new(Some(buffer.clone()));
    let mut offset = 0;
    while offset < text.len() {
        let count = 4093.min(text.len() - offset);
        assert_eq!(guest.read(offset as i32, 17, 4093), count as i32);
        assert_eq!(
            guest.memory(17, count),
            &text.as_bytes()[offset..offset + count]
        );
        offset += count;
    }
    assert!(buffer.0.is_instance(&rope));
    assert_eq!(guest.memory(0, 17), [0; 17]);
    assert_eq!(guest.memory(65_530, 6), [0; 6]);
}

#[test]
fn invalid_and_eof_reads_do_not_materialize_or_write_guest_memory() {
    let buffer = Buffer::from_rope(&Rope::from_str("hello"));
    let mut guest = Guest::new(Some(buffer.clone()));
    for (offset, ptr, len, expected) in [
        (-1, 0, 1, -1),
        (0, -1, 1, -1),
        (0, 0, 0, -1),
        (0, 0, -1, -1),
        (0, 65_536, 1, -1),
        (0, 65_532, 5, -1),
        (0, i32::MAX, i32::MAX, -1),
        (5, 0, 1, 0),
        (6, 0, 1, 0),
        (i32::MAX, i32::MAX, i32::MAX, 0),
    ] {
        assert_eq!(guest.read(offset, ptr, len), expected);
    }
    assert_eq!(guest.memory(65_532, 4), [0; 4]);
    assert_eq!(guest.read(0, 65_531, 5), 5);
    assert_eq!(guest.memory(65_531, 5), b"hello");
}

#[test]
fn missing_memory_export_does_not_materialize() {
    let buffer = Buffer::from_rope(&Rope::from_str("hello"));
    let mut guest = Guest::with_module(
        Some(buffer.clone()),
        r#"(module
            (import "vtcode" "host_read_active_buffer" (func $read (param i32 i32 i32) (result i32)))
            (func (export "read") (param i32 i32 i32) (result i32)
                local.get 0 local.get 1 local.get 2 call $read))"#,
    );
    assert_eq!(guest.read(0, 0, 5), -1);
}

#[test]
fn absent_empty_and_cleared_buffers_remain_distinct() {
    let mut guest = Guest::new(None);
    assert_eq!(guest.invoke("size_only"), -1);
    assert_eq!(guest.read(0, 0, 1), -1);
    let empty = Buffer::from_rope(&Rope::new());
    apply_invocation_context(
        guest.store.data_mut(),
        None,
        None,
        Some(EditorContext {
            active_buffer: Some(empty.clone()),
            ..Default::default()
        }),
    );
    assert_eq!(guest.invoke("size_only"), 0);
    assert_eq!(guest.read(0, 0, 1), 0);
    clear_invocation_context(guest.store.data_mut());
    assert_eq!(guest.invoke("size_only"), -1);
    assert_eq!(guest.read(0, 0, 1), -1);
}

#[test]
fn new_invocations_replace_snapshots_even_after_a_guest_trap() {
    let previous = Buffer::from_rope(&Rope::from_str("old"));
    let mut guest = Guest::new(Some(previous.clone()));
    assert!(call_noarg_export(&mut guest.store, &guest.instance, "trap", true).is_err());
    let next = Buffer::from_rope(&Rope::from_str("new text"));
    apply_invocation_context(
        guest.store.data_mut(),
        None,
        None,
        Some(EditorContext {
            active_buffer: Some(next.clone()),
            ..Default::default()
        }),
    );
    assert_eq!(guest.invoke("size_only"), 8);
    assert_eq!(guest.read(0, 0, 100), 8);
    assert_eq!(guest.memory(0, 8), b"new text");
    apply_invocation_context(guest.store.data_mut(), None, None, None);
    assert_eq!(guest.read(0, 0, 1), -1);
}

#[test]
fn concurrent_callbacks_read_shared_leaves_without_a_contiguous_copy() {
    let text = "λ🦀\r\n".repeat(100_000);
    let rope = Rope::from_str(&text);
    let buffer = Buffer::from_rope(&rope);
    let barrier = Arc::new(Barrier::new(4));
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let snapshot = Buffer::from_rope(&rope);
                assert!(snapshot.shares(&buffer));
                let barrier = barrier.clone();
                let expected = &text;
                scope.spawn(move || {
                    barrier.wait();
                    let mut bytes = [0; 4096];
                    // Concurrent readers have independent destinations; each
                    // begins at an arbitrary byte, including inside UTF-8 scalars.
                    for offset in (worker..expected.len()).step_by(4096) {
                        let count = bytes.len().min(expected.len() - offset);
                        snapshot.copy_range_into(offset, &mut bytes[..count]);
                        assert_eq!(
                            &bytes[..count],
                            &expected.as_bytes()[offset..offset + count]
                        );
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    assert!(buffer.shares(&Buffer::from_rope(&rope)));
}

#[test]
fn capturing_new_revision_does_not_change_an_active_reader() {
    let mut rope = Rope::from_str("before");
    let old = Buffer::from_rope(&rope);
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            barrier.wait();
            for _ in 0..1000 {
                assert_eq!(old.bytes(), b"before");
            }
        });
        barrier.wait();
        rope.insert(0, "new ");
        let next = Buffer::from_rope(&rope);
        assert!(!old.shares(&next));
        assert_eq!(next.bytes(), b"new before");
        worker.join().unwrap();
    });
    assert_eq!(old.bytes(), b"before");
}

#[test]
fn shared_views_keep_snapshot_bytes_correct_through_edits_undo_and_redo() {
    let mut active = Editor::test_with_text("α\r\nβ\n");
    let original = active.extension_buffer_snapshot();
    let mut other = Editor::new();
    other.copy_document_from(&active);
    assert!(original.shares(&other.extension_buffer_snapshot()));

    active.set_cursor(1, 1);
    active.insert_char('界');
    let edited = active.extension_buffer_snapshot();
    assert!(!original.shares(&edited));
    assert_eq!(original.bytes(), "α\r\nβ\n".as_bytes());
    assert_eq!(edited.bytes(), "α\r\nβ界\n".as_bytes());
    other.refresh_document();
    assert!(edited.shares(&other.extension_buffer_snapshot()));

    other.undo();
    active.refresh_document();
    let undone = active.extension_buffer_snapshot();
    assert!(undone.shares(&other.extension_buffer_snapshot()));
    assert_eq!(undone.bytes(), original.bytes());
    active.redo();
    other.refresh_document();
    let redone = other.extension_buffer_snapshot();
    assert!(redone.shares(&active.extension_buffer_snapshot()));
    assert_eq!(redone.bytes(), edited.bytes());
    assert_eq!(original.bytes(), "α\r\nβ\n".as_bytes());
}

#[test]
fn reloads_and_separate_documents_with_same_version_cannot_reuse_stale_bytes() {
    let project = tempfile::tempdir().unwrap();
    let path = project.path().join("file.txt");
    std::fs::write(&path, "old disk text").unwrap();
    let mut editor = Editor::test_with_text("old disk text");
    editor.file_path = Some(path.clone());
    let original = editor.extension_buffer_snapshot();
    let other = Editor::test_with_text("different file");
    assert_eq!(editor.content_version, other.content_version);
    let separate = other.extension_buffer_snapshot();
    assert!(!original.shares(&separate));
    assert_eq!(separate.bytes(), b"different file");
    std::fs::write(&path, "new disk text").unwrap();
    editor.reload_from_disk().unwrap();
    let reloaded = editor.extension_buffer_snapshot();
    assert!(!reloaded.shares(&original));
    assert_eq!(reloaded.bytes(), b"new disk text");
    assert_eq!(original.bytes(), b"old disk text");
    assert!(reloaded.shares(&editor.extension_buffer_snapshot()));
}

#[test]
fn caching_a_snapshot_preserves_the_single_view_edit_fast_path() {
    let mut editor = Editor::test_with_text("hello");
    let snapshot = editor.extension_buffer_snapshot();
    editor.insert_char('x');
    assert_eq!(editor.document_revision(), 0);
    assert_eq!(snapshot.bytes(), b"hello");
    assert_eq!(editor.extension_buffer_snapshot().bytes(), b"xhello");
}

#[test]
fn background_load_with_reused_version_replaces_snapshot_but_metadata_does_not() {
    let mut editor = Editor::test_with_text("old text");
    let original = editor.extension_buffer_snapshot();
    let version = editor.content_version;
    editor.apply_bg_loaded(Rope::from_str("new text"), "UTF-8", 8, u64::MAX);
    assert_eq!(version, editor.content_version);
    let loaded = editor.extension_buffer_snapshot();
    assert!(!loaded.shares(&original));
    assert_eq!(loaded.bytes(), b"new text");
    assert_eq!(original.bytes(), b"old text");
    editor.file_path = Some("save-as.txt".into());
    editor.save_identity += 1;
    editor.record_saved_snapshot(editor.rope.clone());
    editor.set_cursor(0, 3);
    assert!(loaded.shares(&editor.extension_buffer_snapshot()));
}

#[test]
fn separate_app_callbacks_share_buffer_across_desk_switches_and_keep_metadata_fresh() {
    let mut app = crate::app::App::test_new();
    app.new_file();
    app.editor_mut().unwrap().rope = Rope::from_str("hello");
    let initial = app.current_extension_editor_context();
    let buffer = initial.active_buffer.as_ref().unwrap();
    let desk = app.new_desk("Copy", true);
    app.editor_mut().unwrap().set_cursor(0, 3);
    let next = app.extension_editor_context_snapshot().into_context();
    assert!(buffer.shares(next.active_buffer.as_ref().unwrap()));
    assert_eq!(initial.cursor_col, Some(0));
    assert_eq!(next.cursor_col, Some(3));
    let mut guest = Guest::new(next.active_buffer);
    assert_eq!(guest.read(0, 0, 5), 5);
    app.switch_desk(0);
    let main = app.current_extension_editor_context();
    assert!(buffer.shares(main.active_buffer.as_ref().unwrap()));
    assert_eq!(buffer.bytes(), main.active_buffer.as_ref().unwrap().bytes());

    app.editor_mut().unwrap().insert_char('界');
    let edited = app
        .current_extension_editor_context()
        .active_buffer
        .unwrap();
    app.switch_desk(desk);
    let copy = app
        .current_extension_editor_context()
        .active_buffer
        .unwrap();
    assert!(edited.shares(&copy));
    assert!(!buffer.shares(&copy));
    assert_eq!(copy.bytes(), "界hello".as_bytes());
    assert_eq!(buffer.bytes(), b"hello");
}

#[test]
#[ignore = "manual optimized buffer preparation benchmark; run on one idle CPU"]
fn benchmark_extension_buffer_preparation() {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    const CALLBACKS: usize = 32;
    let text = "let café = call(123); // extension buffer\r\n".repeat(200_000);
    let rope = Rope::from_str(&text);
    let mut samples = [Vec::new(), Vec::new(), Vec::new()];
    // Alternate order, discard warmups, and exclude fixture destruction.
    // The third mode copies a real 4 KiB range per callback directly from
    // the Rope, including its initial tree lookup and chunk traversal.
    for sample in 0..11 {
        for variant in 0..3 {
            let mode = (variant + sample) % 3;
            let start = Instant::now();
            for _ in 0..CALLBACKS {
                if mode == 0 {
                    let mut bytes = Vec::with_capacity(rope.len_bytes());
                    bytes.extend(black_box(&rope).bytes());
                    black_box(bytes);
                } else {
                    let context = EditorContextSnapshot {
                        context: EditorContext {
                            active_buffer: Some(Buffer::from_rope(black_box(&rope))),
                            ..Default::default()
                        },
                    }
                    .into_context();
                    let buffer = context.active_buffer.as_ref().unwrap();
                    black_box(buffer.len());
                    if mode == 2 {
                        let mut bytes = [0; 4096];
                        buffer.copy_range_into(0, &mut bytes);
                        black_box(bytes);
                    }
                    black_box(context);
                }
            }
            let elapsed = start.elapsed();
            if mode != 0 {
                let buffer = Buffer::from_rope(&rope);
                if mode == 2 {
                    assert_eq!(buffer.bytes(), text.as_bytes());
                }
            }
            if sample >= 2 {
                samples[mode].push(elapsed);
            }
        }
    }
    let median_ms = |times: &mut Vec<Duration>| {
        times.sort();
        times[times.len() / 2].as_secs_f64() * 1_000.0
    };
    println!(
        "{} bytes, {CALLBACKS} callbacks: eager {:.6} ms; lazy metadata {:.6} ms; bounded reads {:.6} ms",
        text.len(),
        median_ms(&mut samples[0]),
        median_ms(&mut samples[1]),
        median_ms(&mut samples[2]),
    );
}

#[test]
#[ignore = "manual optimized memory benchmark; run alone on one idle CPU"]
fn benchmark_memory_extension_partial_read() {
    use std::{io::Read, time::Instant};
    const BYTES: usize = 128 * 1024 * 1024;
    let rope = Rope::from_reader(std::io::repeat(b'a').take(BYTES as u64)).unwrap();
    let buffer = Buffer::from_rope(&rope);
    let mut guest = Guest::new(Some(buffer.clone()));
    let before = crate::process_usage::read_current_process()
        .resident_bytes
        .unwrap();
    let started = Instant::now();
    assert_eq!(guest.read((BYTES / 2) as i32, 0, 4096), 4096);
    let elapsed = started.elapsed();
    let after = crate::process_usage::read_current_process()
        .resident_bytes
        .unwrap();
    assert_eq!(guest.memory(0, 4096), vec![b'a'; 4096]);
    println!(
        "128 MiB document, first 4 KiB extension read: {elapsed:?}; resident increase {} bytes",
        after.saturating_sub(before)
    );
    std::hint::black_box(buffer);
}
