//! Host operations for file-backed custom editors. A guest can open only the
//! file explicitly supplied by the workbench, never arbitrary filesystem paths.
use crate::extension_documents::BinaryDocument;
use crate::extension_wasm::{HostState, host_view_entry_mut, read_guest_string, write_guest_bytes};
use wasmtime::{Caller, Linker};

pub(crate) fn check_text_conflict(host: &HostState, path: &std::path::Path) -> Result<(), String> {
    let normalized = crate::extension_documents::normalized_file_path(path)?;
    if host.modified_file_paths.iter().any(|candidate| {
        crate::extension_documents::normalized_file_path(candidate)
            .is_ok_and(|candidate| candidate == normalized)
    }) {
        return Err(
            "Save or discard the text editor's changes to this file before using its custom editor"
                .into(),
        );
    }
    Ok(())
}

fn fail(host: &mut HostState, error: impl ToString) -> i32 {
    host.last_host_error = error.to_string();
    -1
}

fn write_payload(caller: &mut Caller<'_, HostState>, ptr: i32, max: i32, bytes: &[u8]) -> i32 {
    if ptr < 0 || max < 0 {
        return fail(caller.data_mut(), "Invalid guest memory range");
    }
    let bytes = &bytes[..bytes.len().min(max as usize)];
    match write_guest_bytes(caller, ptr as usize, bytes) {
        Ok(()) => bytes.len() as i32,
        Err(()) => fail(caller.data_mut(), "Invalid guest memory range"),
    }
}

pub(crate) fn define(linker: &mut Linker<HostState>) -> Result<(), String> {
    linker.func_wrap("vtcode", "host_view_event_text", |mut caller: Caller<'_, HostState>, ptr: i32, max: i32| -> i32 {
        if caller.data().current_view_event_text.is_none() {
            let Some(bytes) = caller.data().current_view_event.as_deref() else {
                return fail(caller.data_mut(), "No current input event");
            };
            let mut event = match serde_json::from_slice::<serde_json::Value>(bytes) {
                Ok(event) => event,
                Err(error) => return fail(caller.data_mut(), error),
            };
            let text = match event.get_mut("text").map(serde_json::Value::take) {
                Some(serde_json::Value::String(text)) => text,
                _ => String::new(),
            };
            caller.data_mut().current_view_event_text = Some(text);
        }
        let text = caller.data_mut().current_view_event_text.take().unwrap();
        let result = if max == 0 { text.len() as i32 } else { write_payload(&mut caller, ptr, max, text.as_bytes()) };
        caller.data_mut().current_view_event_text = Some(text);
        result
    }).map_err(|error| error.to_string())?;
    linker.func_wrap("vtcode", "host_set_view_input_pending",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32, pending: i32| {
            if let Some(id) = read_guest_string(&mut caller, ptr, len) {
                host_view_entry_mut(caller.data_mut(), &id).input_pending = pending != 0;
            }
        }).map_err(|error| error.to_string())?;
    linker.func_wrap("vtcode", "host_open_text_document",
        |mut caller: Caller<'_, HostState>, name_ptr: i32, name_len: i32, ptr: i32, len: i32| -> i32 {
            let Some(name) = read_guest_string(&mut caller, name_ptr, name_len) else {
                return fail(caller.data_mut(), "Invalid document name");
            };
            if name.is_empty() || name.len() > 255 || name.contains(['/', '\\', ':'])
                || name.chars().any(char::is_control) || matches!(name.as_str(), "." | "..") {
                return fail(caller.data_mut(), "Use a filename without a directory for the export");
            }
            if ptr < 0 || !(0..=32 * 1024 * 1024).contains(&len) {
                return fail(caller.data_mut(), "Generated text is limited to 32 MiB");
            }
            // Borrow guest memory and allocate only the final host String.
            let text = {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return fail(caller.data_mut(), "Missing guest memory");
                };
                let bytes = match memory.data(&caller).get(ptr as usize..ptr as usize + len as usize) {
                    Some(bytes) => bytes,
                    None => return fail(caller.data_mut(), "Invalid guest memory range"),
                };
                match std::str::from_utf8(bytes) {
                    Ok(text) => text.to_owned(),
                    Err(_) => return fail(caller.data_mut(), "Generated text must be valid UTF-8"),
                }
            };
            let pending = caller.data().text_edit_requests.iter().filter_map(|request| {
                if let crate::extension_manifest::TextEditRequest::OpenDocument { text, .. } = request {
                    Some(text.len())
                } else { None }
            }).sum::<usize>();
            if pending + text.len() > 32 * 1024 * 1024 {
                return fail(caller.data_mut(), "Pending generated text exceeds 32 MiB");
            }
            caller.data_mut().text_edit_requests.push(crate::extension_manifest::TextEditRequest::OpenDocument { name, text });
            0
        }).map_err(|error| error.to_string())?;
    linker.func_wrap("vtcode", "host_task_should_yield", |caller: Caller<'_, HostState>| -> i32 {
        let time_up = caller.data().task_deadline
            .is_none_or(|deadline| std::time::Instant::now() >= deadline);
        let fuel_low = caller.get_fuel().is_ok_and(|fuel| {
            fuel < 2 * crate::extension_wasm::EXTENSION_CALL_FUEL
        });
        i32::from(time_up || fuel_low)
    }).map_err(|error| error.to_string())?;
    linker.func_wrap("vtcode", "host_document_open", |mut caller: Caller<'_, HostState>| -> i32 {
        let path = caller.data().command_context_path.as_ref()
            .or(caller.data().active_file_path.as_ref()).cloned();
        let Some(path) = path else { return fail(caller.data_mut(), "Open a file first"); };
        if caller.data().active_modified && caller.data().active_file_path.as_ref() == Some(&path) {
            return fail(caller.data_mut(), "Save the text editor's changes before reopening this file in a custom editor");
        }
        let canonical = match crate::extension_documents::normalized_file_path(std::path::Path::new(&path)) {
            Ok(path) => path, Err(error) => return fail(caller.data_mut(), error),
        };
        if let Err(error) = check_text_conflict(caller.data(), &canonical) { return fail(caller.data_mut(), error); }
        if let Some(index) = caller.data().documents.iter().position(|doc| {
            std::path::Path::new(&doc.info().path) == canonical
        }) { return (index + 1) as i32; }
        let recovered = caller.data().settings_file_path.as_ref().and_then(|path| {
            crate::extension_recovery::read(&crate::extension_recovery::directory(path)).into_iter()
                .find(|record| record.extension_id == caller.data().extension_id && record.original_path == canonical)
        });
        let remote = caller.data().remote_workspace.as_ref().filter(|_| crate::remote::workspace::is_view_key(&canonical));
        let document = match (recovered, remote) {
            (Some(record), Some(remote)) => BinaryDocument::restore_remote(&record, remote),
            (None, Some(remote)) => BinaryDocument::open_remote(&canonical, remote),
            (Some(record), None) => BinaryDocument::restore(&record),
            (None, None) => BinaryDocument::open(&canonical),
        };
        match document {
            Ok(document) => {
                caller.data_mut().documents.push(document);
                let id = caller.data().documents.len() as u32;
                if let Some(command) = caller.data().current_command_id.clone() { caller.data_mut().document_openers.insert(id, command); }
                id as i32
            }
            Err(error) => fail(caller.data_mut(), error),
        }
    }).map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_document_info",
            |mut caller: Caller<'_, HostState>, id: i32, ptr: i32, max: i32| -> i32 {
                let Some(document) = caller.data().documents.get(id.wrapping_sub(1) as usize)
                else {
                    return fail(caller.data_mut(), "Unknown document");
                };
                let json = match serde_json::to_vec(&document.info()) {
                    Ok(json) => json,
                    Err(error) => return fail(caller.data_mut(), error),
                };
                if max == 0 {
                    return json.len() as i32;
                }
                write_payload(&mut caller, ptr, max, &json)
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_document_read",
            |mut caller: Caller<'_, HostState>, id: i32, offset: i64, ptr: i32, max: i32| -> i32 {
                if offset < 0 || ptr < 0 || !(0..=1024 * 1024).contains(&max) {
                    return fail(caller.data_mut(), "Invalid read range");
                }
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return fail(caller.data_mut(), "Missing guest memory");
                };
                let (memory, host) = memory.data_and_store_mut(&mut caller);
                let start = ptr as usize;
                let Some(out) = memory.get_mut(start..start + max as usize) else {
                    return fail(host, "Invalid guest memory range");
                };
                let Some(document) = host.documents.get(id.wrapping_sub(1) as usize)
                else {
                    return fail(host, "Unknown document");
                };
                // Validate the complete guest buffer before I/O, then fill it
                // directly. No temporary host Vec or second payload copy.
                match document.read(offset as u64, out) {
                    Ok(count) => count as i32,
                    Err(error) => fail(host, error),
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_document_changed",
            |mut caller: Caller<'_, HostState>, id: i32, offset: i64, ptr: i32, max: i32| -> i32 {
                if offset < 0 || !(0..=1024 * 1024).contains(&max) {
                    return fail(caller.data_mut(), "Invalid changed-byte range");
                }
                let Some(document) = caller.data().documents.get(id.wrapping_sub(1) as usize)
                else {
                    return fail(caller.data_mut(), "Unknown document");
                };
                match document.changed(offset as u64, max as usize) {
                    Ok(flags) => write_payload(&mut caller, ptr, max, &flags),
                    Err(error) => fail(caller.data_mut(), error),
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_document_edit",
            |mut caller: Caller<'_, HostState>,
             id: i32,
             offset: i64,
             deleted: i64,
             ptr: i32,
             len: i32|
             -> i32 {
                if offset < 0 || deleted < 0 || !(0..=1024 * 1024).contains(&len) {
                    return fail(caller.data_mut(), "Invalid edit range");
                }
                let bytes = if len == 0 {
                    Vec::new()
                } else {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return fail(caller.data_mut(), "Missing guest memory");
                    };
                    let mut bytes = vec![0; len as usize];
                    if ptr < 0 || memory.read(&caller, ptr as usize, &mut bytes).is_err() {
                        return fail(caller.data_mut(), "Invalid guest memory range");
                    }
                    bytes
                };
                let Some(document) = caller
                    .data_mut()
                    .documents
                    .get_mut(id.wrapping_sub(1) as usize)
                else {
                    return fail(caller.data_mut(), "Unknown document");
                };
                match document.edit(offset as u64, deleted as u64, &bytes) {
                    Ok(()) => 0,
                    Err(error) => fail(caller.data_mut(), error),
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_document_action",
            |mut caller: Caller<'_, HostState>, id: i32, action: i32| -> i32 {
                if matches!(action, 3 | 9)
                    && let Some(document) = caller.data().documents.get(id.wrapping_sub(1) as usize)
                    && let Err(error) = check_text_conflict(
                        caller.data(),
                        std::path::Path::new(&document.info().path),
                    )
                {
                    return fail(caller.data_mut(), error);
                }
                let Some(document) = caller
                    .data_mut()
                    .documents
                    .get_mut(id.wrapping_sub(1) as usize)
                else {
                    return fail(caller.data_mut(), "Unknown document");
                };
                match action {
                    1 => i32::from(document.undo()),
                    2 => i32::from(document.redo()),
                    3 => match document.save() {
                        Ok(()) => 0,
                        Err(error) => fail(caller.data_mut(), error),
                    },
                    5 => {
                        document.amend_next_edit();
                        0
                    }
                    6 => match document.begin_transaction() {
                        Ok(()) => 0,
                        Err(error) => fail(caller.data_mut(), error),
                    },
                    7 | 8 => match document.end_transaction(action == 7) {
                        Ok(()) => 0,
                        Err(error) => fail(caller.data_mut(), error),
                    },
                    9 => match document.reload_if_clean() {
                        Ok(()) => 0,
                        Err(error) => fail(caller.data_mut(), error),
                    },
                    _ => fail(caller.data_mut(), "Unknown document action"),
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_register_task_handler",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32, interval: i32| {
                if let Some(export) = read_guest_string(&mut caller, ptr, len)
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty())
                {
                    caller
                        .data_mut()
                        .tick_handlers
                        .retain(|handler| handler.export_name != export);
                    if interval > 0 {
                        caller.data_mut().tick_handlers.push(
                            crate::extension_manifest::ExtensionTickHandler {
                                export_name: export,
                                interval_ms: interval.max(10) as u32,
                                background: true,
                            },
                        );
                    }
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_last_error",
            |mut caller: Caller<'_, HostState>, ptr: i32, max: i32| -> i32 {
                let error = caller.data().last_host_error.as_bytes().to_vec();
                if max == 0 {
                    return error.len() as i32;
                }
                write_payload(&mut caller, ptr, max, &error)
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_clipboard_read",
            |mut caller: Caller<'_, HostState>, ptr: i32, max: i32| -> i32 {
                let bytes = caller.data().clipboard_text.as_bytes().to_vec();
                if max == 0 {
                    return bytes.len() as i32;
                }
                write_payload(&mut caller, ptr, max, &bytes)
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_clipboard_write",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let Some(text) = read_guest_string(&mut caller, ptr, len) else {
                    return fail(caller.data_mut(), "Invalid clipboard text");
                };
                caller.data_mut().clipboard_text = text.clone();
                caller.data_mut().clipboard_write = Some(text);
                0
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_register_view_resize_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                if let (Some(id), Some(export)) = (
                    read_guest_string(&mut caller, id_ptr, id_len),
                    read_guest_string(&mut caller, export_ptr, export_len),
                ) {
                    host_view_entry_mut(caller.data_mut(), &id).resize_handler = Some(export);
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_register_view_close_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                if let (Some(id), Some(export)) = (
                    read_guest_string(&mut caller, id_ptr, id_len),
                    read_guest_string(&mut caller, export_ptr, export_len),
                ) {
                    host_view_entry_mut(caller.data_mut(), &id).close_handler = Some(export);
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_register_view_hover_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                if let (Some(id), Some(export)) = (
                    read_guest_string(&mut caller, id_ptr, id_len),
                    read_guest_string(&mut caller, export_ptr, export_len),
                ) {
                    host_view_entry_mut(caller.data_mut(), &id).hover_handler = Some(export);
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_register_view_context_menu_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                if let (Some(id), Some(export)) = (
                    read_guest_string(&mut caller, id_ptr, id_len),
                    read_guest_string(&mut caller, export_ptr, export_len),
                ) {
                    host_view_entry_mut(caller.data_mut(), &id).context_menu_handler = Some(export);
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_set_view_pointer_capture",
            |mut caller: Caller<'_, HostState>, id_ptr: i32, id_len: i32, enabled: i32| {
                if let Some(id) = read_guest_string(&mut caller, id_ptr, id_len) {
                    host_view_entry_mut(caller.data_mut(), &id).pointer_capture = enabled != 0;
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "vtcode",
            "host_set_view_frame",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             ptr: i32,
             len: i32|
             -> i32 {
                let Some(id) = read_guest_string(&mut caller, id_ptr, id_len) else {
                    return fail(caller.data_mut(), "Invalid view id");
                };
                if !(0..=2 * 1024 * 1024).contains(&len) {
                    return fail(caller.data_mut(), "View frame exceeds 2 MiB");
                }
                let Some(json) = read_guest_string(&mut caller, ptr, len) else {
                    return fail(caller.data_mut(), "Invalid view frame");
                };
                match serde_json::from_str::<crate::extension_surface::ViewFrame>(&json) {
                    Ok(mut frame) => {
                        if frame.lines.len() > 512
                            || frame.lines.iter().any(|line| line.len() > 1024)
                            || frame.lines.iter().map(Vec::len).sum::<usize>() > 32768
                            || frame
                                .lines
                                .iter()
                                .flatten()
                                .any(|span| span.text.chars().any(char::is_control))
                        {
                            return fail(
                                caller.data_mut(),
                                "View frame exceeds the terminal surface limit",
                            );
                        }
                        if let Some(id) = frame.document {
                            let Some(document) =
                                caller.data().documents.get(id.wrapping_sub(1) as usize)
                            else {
                                return fail(caller.data_mut(), "Unknown view document");
                            };
                            let info = document.info();
                            frame.dirty = info.dirty;
                            frame.path = Some(info.path);
                        } else {
                            frame.dirty = false;
                            frame.path = None;
                        }
                        let view = host_view_entry_mut(caller.data_mut(), &id);
                        if let Some(title) = frame.title.as_ref().filter(|title| {
                            !title.trim().is_empty() && !title.chars().any(char::is_control)
                        }) {
                            view.title = title.clone();
                        }
                        view.frame = Some(std::sync::Arc::new(frame));
                        0
                    }
                    Err(error) => fail(caller.data_mut(), error),
                }
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_editor_api_rejects_invalid_memory_ranges_and_forged_document_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.bin");
        std::fs::write(&path, [1, 2, 3]).unwrap();
        let engine = wasmtime::Engine::default();
        let mut linker = Linker::new(&engine);
        define(&mut linker).unwrap();
        let module = wasmtime::Module::new(&engine, r#"(module
          (import "vtcode" "host_document_read" (func $read (param i32 i64 i32 i32) (result i32)))
          (import "vtcode" "host_document_edit" (func $edit (param i32 i64 i64 i32 i32) (result i32)))
          (import "vtcode" "host_set_view_frame" (func $frame (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "view")
          (func (export "read") (param i32 i64 i32 i32) (result i32) local.get 0 local.get 1 local.get 2 local.get 3 call $read)
          (func (export "edit") (param i32 i64 i64 i32 i32) (result i32) local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $edit)
          (func (export "frame") (param i32) (result i32) i32.const 0 i32.const 4 i32.const 100 local.get 0 call $frame))"#).unwrap();
        let mut host = HostState::default();
        host.documents.push(BinaryDocument::open(&path).unwrap());
        let mut store = wasmtime::Store::new(&engine, host);
        store.limiter(|host| &mut host.limits);
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let read = instance
            .get_typed_func::<(i32, i64, i32, i32), i32>(&mut store, "read")
            .unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        memory.write(&mut store, 1024, &[99; 16]).unwrap();
        assert_eq!(read.call(&mut store, (1, 0, 1024, 16)).unwrap(), 3);
        let mut bytes = [0; 16];
        memory.read(&store, 1024, &mut bytes).unwrap();
        assert_eq!(&bytes[..3], &[1, 2, 3]);
        assert_eq!(&bytes[3..], &[99; 13], "only the returned prefix is written");
        assert_eq!(read.call(&mut store, (1, 3, 1024, 16)).unwrap(), 0);
        assert_eq!(read.call(&mut store, (1, 1, 1024, 16)).unwrap(), 2);
        memory.read(&store, 1024, &mut bytes[..2]).unwrap();
        assert_eq!(&bytes[..2], &[2, 3], "random reads reposition the source");
        assert_eq!(read.call(&mut store, (1, 0, 65_536, 0)).unwrap(), 0);
        for arguments in [
            (1, -1, 0, 1),
            (1, 0, -1, 1),
            (1, 0, 65_535, 2),
            (1, 0, 0, 1_048_577),
            (0, 0, 0, 1),
            (1, 4, 0, 1),
        ] {
            assert_eq!(read.call(&mut store, arguments).unwrap(), -1);
        }
        let edit = instance
            .get_typed_func::<(i32, i64, i64, i32, i32), i32>(&mut store, "edit")
            .unwrap();
        for arguments in [
            (1, 0, 1, 65_535, 2),
            (1, -1, 0, 0, 1),
            (1, i64::MAX, i64::MAX, 0, 1),
        ] {
            assert_eq!(edit.call(&mut store, arguments).unwrap(), -1);
        }
        assert!(!store.data().documents[0].info().dirty);
        assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
        let frame = instance
            .get_typed_func::<i32, i32>(&mut store, "frame")
            .unwrap();
        for (payload, expected) in [
            (r#"{"document":99,"lines":[]}"#, -1),
            (r#"{"lines":[[{"text":"\u001b[31m"}]]}"#, -1),
            (
                r#"{"document":1,"dirty":true,"path":"/forged","lines":[]}"#,
                0,
            ),
        ] {
            memory.write(&mut store, 100, payload.as_bytes()).unwrap();
            assert_eq!(
                frame.call(&mut store, payload.len() as i32).unwrap(),
                expected
            );
        }
        let surface = store.data().views["view"].frame.as_ref().unwrap();
        assert!(!surface.dirty);
        assert_eq!(
            surface.path.as_deref(),
            Some(store.data().documents[0].info().path.as_str())
        );
    }

    #[test]
    fn custom_editor_cooperative_yield_checks_time_fuel_and_invocation_scope() {
        use crate::extension_wasm::{call_i32_arg_export, call_noarg_export, EXTENSION_CALL_FUEL};
        use std::time::{Duration, Instant};

        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let mut linker = Linker::new(&engine);
        define(&mut linker).unwrap();
        linker.func_wrap("test", "deadline_present", |caller: Caller<'_, HostState>| -> i32 {
            i32::from(caller.data().task_deadline.is_some())
        }).unwrap();
        let module = wasmtime::Module::new(&engine, r#"(module
          (import "vtcode" "host_task_should_yield" (func $yield (result i32)))
          (import "test" "deadline_present" (func $deadline (result i32)))
          (func (export "query") (result i32) call $yield)
          (func (export "work") (result i32) call $deadline)
          (func (export "void") call $deadline drop)
          (func (export "trap") unreachable)
          (func (export "bad") (param f64))
          (func (export "input") (param i32) (result i32) call $yield))"#).unwrap();
        let mut host = HostState::default();
        for name in ["work", "void", "trap", "bad"] {
            host.tick_handlers.push(crate::extension_manifest::ExtensionTickHandler {
                export_name: name.into(), interval_ms: 16, background: true,
            });
        }
        let mut store = wasmtime::Store::new(&engine, host);
        store.set_fuel(100_000_000).unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let query = instance.get_typed_func::<(), i32>(&mut store, "query").unwrap();
        assert_eq!(query.call(&mut store, ()).unwrap(), 1, "outside a task");
        store.data_mut().task_deadline = Some(Instant::now() + Duration::from_secs(3600));
        assert_eq!(query.call(&mut store, ()).unwrap(), 0);
        store.data_mut().task_deadline = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(query.call(&mut store, ()).unwrap(), 1, "expired slice");
        store.data_mut().task_deadline = Some(Instant::now() + Duration::from_secs(3600));
        store.set_fuel(EXTENSION_CALL_FUEL).unwrap();
        assert_eq!(query.call(&mut store, ()).unwrap(), 1, "reserve fuel to return safely");

        assert_eq!(call_noarg_export(&mut store, &instance, "work", true).unwrap(), Some(1));
        assert!(store.get_fuel().unwrap() > 200_000_000);
        assert!(store.data().task_deadline.is_none());
        assert_eq!(call_noarg_export(&mut store, &instance, "void", true).unwrap(), Some(0));
        assert!(store.data().task_deadline.is_none());
        for name in ["trap", "bad"] {
            assert!(call_noarg_export(&mut store, &instance, name, true).is_err());
            assert!(store.data().task_deadline.is_none(), "clear the slice after {name}");
        }
        assert_eq!(call_i32_arg_export(&mut store, &instance, "input", 0).unwrap(), 1);
        assert!(store.get_fuel().unwrap() < EXTENSION_CALL_FUEL);
        assert!(store.data().task_deadline.is_none());
        store.data_mut().current_view_event = Some(vec![b'x'; 128 * 1024]);
        assert_eq!(call_noarg_export(&mut store, &instance, "query", true).unwrap(), Some(1));
        assert!(store.get_fuel().unwrap() > EXTENSION_CALL_FUEL);
        store.data_mut().current_view_event = None;
    }
    #[test]
    fn generated_documents_validate_names_utf8_ranges_and_staged_input() {
        let engine = wasmtime::Engine::default();
        let mut linker = Linker::new(&engine);
        define(&mut linker).unwrap();
        let module = wasmtime::Module::new(&engine, r#"(module
          (import "vtcode" "host_open_text_document" (func $open (param i32 i32 i32 i32) (result i32)))
          (import "vtcode" "host_set_view_input_pending" (func $pending (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "open") (param i32 i32 i32 i32) (result i32)
            local.get 0 local.get 1 local.get 2 local.get 3 call $open)
          (func (export "pending") (param i32)
            i32.const 0 i32.const 4 local.get 0 call $pending))"#).unwrap();
        let mut store = wasmtime::Store::new(&engine, HostState::default());
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let open = instance.get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "open").unwrap();
        memory.write(&mut store, 0, b"report.csv").unwrap();
        memory.write(&mut store, 256, b"a,b\n1,2\n").unwrap();
        assert_eq!(open.call(&mut store, (0, 10, 256, 8)).unwrap(), 0);
        assert!(matches!(&store.data().text_edit_requests[0],
            crate::extension_manifest::TextEditRequest::OpenDocument { name, text } if name == "report.csv" && text == "a,b\n1,2\n"));
        for name in ["../outside.csv", "C:\\outside.csv", "", ".", "..", "bad\nname"] {
            memory.write(&mut store, 0, name.as_bytes()).unwrap();
            assert_eq!(open.call(&mut store, (0, name.len() as i32, 256, 8)).unwrap(), -1);
        }
        memory.write(&mut store, 0, b"good.csv").unwrap();
        for arguments in [(0, 8, -1, 8), (0, 8, 65_535, 8), (0, 8, 256, -1), (0, 8, 256, 32 * 1024 * 1024 + 1)] {
            assert_eq!(open.call(&mut store, arguments).unwrap(), -1);
        }
        memory.write(&mut store, 256, &[0xff]).unwrap();
        assert_eq!(open.call(&mut store, (0, 8, 256, 1)).unwrap(), -1);
        assert_eq!(store.data().text_edit_requests.len(), 1);
        memory.write(&mut store, 0, b"grid").unwrap();
        let pending = instance.get_typed_func::<i32, ()>(&mut store, "pending").unwrap();
        pending.call(&mut store, 1).unwrap(); assert!(store.data().views["grid"].input_pending);
        pending.call(&mut store, 0).unwrap(); assert!(!store.data().views["grid"].input_pending);
    }
}
