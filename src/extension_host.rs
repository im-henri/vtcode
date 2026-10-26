use std::path::Path;

use wasmtime::{Caller, Linker};

use crate::extension_manifest::{
    parse_file_open_extensions, ExtensionDiagnostic, ExtensionTickHandler, TextEditRequest,
};
use crate::extension_wasm::{
    collect_workspace_file_bytes, host_overlay_entry_mut,
    host_view_entry_mut, load_extension_setting, read_guest_string, save_extension_setting,
    write_guest_bytes, HostState,
};

/// Clamp a `usize` length to `i32` range for WASM interface boundaries.
fn len_as_i32(len: usize) -> i32 {
    len.min(i32::MAX as usize) as i32
}
pub(crate) fn define_host_functions(linker: &mut Linker<HostState>) -> Result<(), String> {
    crate::extension_editor_host::define(linker)?;
    linker
        .func_wrap(
            "vtcode",
            "host_log",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
                if let Some(message) = read_guest_string(&mut caller, ptr, len) {
                    caller.data_mut().logs.push(message);
                }
            },
        )
        .map_err(|err| format!("Failed defining host_log: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_set_status",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
                if let Some(status) = read_guest_string(&mut caller, ptr, len) {
                    caller.data_mut().status = Some(status);
                }
            },
        )
        .map_err(|err| format!("Failed defining host_set_status: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_command",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             title_ptr: i32,
             title_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let title = read_guest_string(&mut caller, title_ptr, title_len);
                if let (Some(id), Some(title)) = (id, title) {
                    caller.data_mut().commands.push((id, title));
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_command: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_view",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             title_ptr: i32,
             title_len: i32,
             content_ptr: i32,
             content_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let title = read_guest_string(&mut caller, title_ptr, title_len);
                let content = read_guest_string(&mut caller, content_ptr, content_len);
                if let (Some(id), Some(title), Some(content)) = (id, title, content) {
                    if id.trim().is_empty() || title.trim().is_empty() {
                        return;
                    }
                    let host = caller.data_mut();
                    let view = host_view_entry_mut(host, &id);
                    view.title = title;
                    view.content = content;
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_view: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_view_key_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let export = read_guest_string(&mut caller, export_ptr, export_len);
                if let (Some(id), Some(export)) = (id, export) {
                    if id.trim().is_empty() {
                        return;
                    }
                    host_view_entry_mut(caller.data_mut(), &id).key_handler =
                        Some(export.trim().to_string()).filter(|value| !value.is_empty());
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_view_key_handler: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_view_mouse_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let export = read_guest_string(&mut caller, export_ptr, export_len);
                if let (Some(id), Some(export)) = (id, export) {
                    if id.trim().is_empty() {
                        return;
                    }
                    host_view_entry_mut(caller.data_mut(), &id).mouse_handler =
                        Some(export.trim().to_string()).filter(|value| !value.is_empty());
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_view_mouse_handler: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_view_scroll_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let export = read_guest_string(&mut caller, export_ptr, export_len);
                if let (Some(id), Some(export)) = (id, export) {
                    if id.trim().is_empty() {
                        return;
                    }
                    host_view_entry_mut(caller.data_mut(), &id).scroll_handler =
                        Some(export.trim().to_string()).filter(|value| !value.is_empty());
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_view_scroll_handler: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_set_view_content",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             content_ptr: i32,
             content_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let content = read_guest_string(&mut caller, content_ptr, content_len);
                if let (Some(id), Some(content)) = (id, content) {
                    if let Some(existing) =
                        caller.data_mut().views.get_mut(&id)
                    {
                        existing.content = content;
                    }
                }
            },
        )
        .map_err(|err| format!("Failed defining host_set_view_content: {err}"))?;


    linker
        .func_wrap(
            "vtcode",
            "host_open_view",
            |mut caller: Caller<'_, HostState>, id_ptr: i32, id_len: i32| {
                if let Some(id) = read_guest_string(&mut caller, id_ptr, id_len) {
                    if !id.trim().is_empty() {
                        caller.data_mut().requested_open_view = Some(id);
                    }
                }
            },
        )
        .map_err(|err| format!("Failed defining host_open_view: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_explorer_context_command",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             title_ptr: i32,
             title_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let title = read_guest_string(&mut caller, title_ptr, title_len);
                if let (Some(id), Some(title)) = (id, title) {
                    if id.trim().is_empty() || title.trim().is_empty() {
                        return;
                    }
                    let host = caller.data_mut();
                    if let Some(existing) = host
                        .explorer_context_commands
                        .iter_mut()
                        .find(|(existing_id, _)| *existing_id == id)
                    {
                        existing.1 = title;
                    } else {
                        host.explorer_context_commands.push((id, title));
                    }
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_explorer_context_command: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_binary_open_command",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             title_ptr: i32,
             title_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let title = read_guest_string(&mut caller, title_ptr, title_len);
                if let (Some(id), Some(title)) = (id, title) {
                    if id.trim().is_empty() || title.trim().is_empty() {
                        return;
                    }
                    let host = caller.data_mut();
                    if let Some(existing) = host
                        .binary_open_commands
                        .iter_mut()
                        .find(|(existing_id, _)| *existing_id == id)
                    {
                        existing.1 = title;
                    } else {
                        host.binary_open_commands.push((id, title));
                    }
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_binary_open_command: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_file_open_command",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             title_ptr: i32,
             title_len: i32,
             ext_ptr: i32,
             ext_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let title = read_guest_string(&mut caller, title_ptr, title_len);
                let extensions = read_guest_string(&mut caller, ext_ptr, ext_len);
                if let (Some(id), Some(title), Some(extensions)) = (id, title, extensions) {
                    if id.trim().is_empty() || title.trim().is_empty() {
                        return;
                    }
                    let parsed_extensions = parse_file_open_extensions(&extensions);
                    if parsed_extensions.is_empty() {
                        return;
                    }
                    let host = caller.data_mut();
                    if let Some(existing) = host
                        .file_open_commands
                        .iter_mut()
                        .find(|(existing_id, _, _)| *existing_id == id)
                    {
                        existing.1 = title;
                        existing.2 = parsed_extensions;
                    } else {
                        host.file_open_commands.push((id, title, parsed_extensions));
                    }
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_file_open_command: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_context_target_path_len",
            |caller: Caller<'_, HostState>| -> i32 {
                caller
                    .data()
                    .command_context_path
                    .as_ref()
                    .map(|path| len_as_i32(path.len()))
                    .unwrap_or(-1)
            },
        )
        .map_err(|err| format!("Failed defining host_context_target_path_len: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_read_context_target_path",
            |mut caller: Caller<'_, HostState>, ptr: i32, max_len: i32| -> i32 {
                if ptr < 0 || max_len <= 0 {
                    return -1;
                }
                let bytes = {
                    let Some(path) = caller.data().command_context_path.as_ref() else {
                        return -1;
                    };
                    path.as_bytes().to_vec()
                };
                let write_len = bytes.len().min(max_len as usize);
                if write_guest_bytes(&mut caller, ptr as usize, &bytes[..write_len]).is_err() {
                    return -1;
                }
                write_len as i32
            },
        )
        .map_err(|err| format!("Failed defining host_read_context_target_path: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_context_target_size",
            |caller: Caller<'_, HostState>| -> i32 {
                let host = caller.data();
                host.command_context_bytes.as_ref().map(|bytes| len_as_i32(bytes.len()))
                    .or_else(|| host.command_context_path.as_ref()
                        .and_then(|path| std::fs::metadata(path).ok())
                        .map(|metadata| metadata.len().min(i32::MAX as u64) as i32))
                    .unwrap_or(-1)
            },
        )
        .map_err(|err| format!("Failed defining host_context_target_size: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_read_context_target",
            |mut caller: Caller<'_, HostState>, offset: i32, ptr: i32, max_len: i32| -> i32 {
                if offset < 0 || ptr < 0 || max_len <= 0 {
                    return -1;
                }
                if max_len > 1024 * 1024 { return -1; }
                let slice = if let Some(bytes) = caller.data().command_context_bytes.as_ref() {
                    let start = offset as usize;
                    if start >= bytes.len() {
                        return 0;
                    }
                    let end = start.saturating_add(max_len as usize).min(bytes.len());
                    bytes[start..end].to_vec()
                } else {
                    use std::io::{Read, Seek, SeekFrom};
                    let Some(path) = caller.data().command_context_path.as_ref() else { return -1; };
                    let Ok(mut file) = std::fs::File::open(path) else { return -1; };
                    if file.seek(SeekFrom::Start(offset as u64)).is_err() { return -1; }
                    let mut bytes = vec![0; max_len as usize];
                    let Ok(count) = file.read(&mut bytes) else { return -1; };
                    bytes.truncate(count);
                    bytes
                };
                if write_guest_bytes(&mut caller, ptr as usize, &slice).is_err() {
                    return -1;
                }
                len_as_i32(slice.len())
            },
        )
        .map_err(|err| format!("Failed defining host_read_context_target: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_active_file_path_len",
            |caller: Caller<'_, HostState>| -> i32 {
                caller
                    .data()
                    .active_file_path
                    .as_ref()
                    .map(|path| len_as_i32(path.len()))
                    .unwrap_or(-1)
            },
        )
        .map_err(|err| format!("Failed defining host_active_file_path_len: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_read_active_file_path",
            |mut caller: Caller<'_, HostState>, ptr: i32, max_len: i32| -> i32 {
                if ptr < 0 || max_len <= 0 {
                    return -1;
                }
                let bytes = {
                    let Some(path) = caller.data().active_file_path.as_ref() else {
                        return -1;
                    };
                    path.as_bytes().to_vec()
                };
                let write_len = bytes.len().min(max_len as usize);
                if write_guest_bytes(&mut caller, ptr as usize, &bytes[..write_len]).is_err() {
                    return -1;
                }
                write_len as i32
            },
        )
        .map_err(|err| format!("Failed defining host_read_active_file_path: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_active_buffer_size",
            |caller: Caller<'_, HostState>| -> i32 {
                caller
                    .data()
                    .active_buffer
                    .as_ref()
                    .map(|buffer| len_as_i32(buffer.len()))
                    .unwrap_or(-1)
            },
        )
        .map_err(|err| format!("Failed defining host_active_buffer_size: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_read_active_buffer",
            |mut caller: Caller<'_, HostState>, offset: i32, ptr: i32, max_len: i32| -> i32 {
                if offset < 0 || ptr < 0 || max_len <= 0 {
                    return -1;
                }
                let Some(buffer) = caller.data().active_buffer.clone() else {
                    return -1;
                };
                let start = offset as usize;
                if start >= buffer.len() {
                    return 0;
                }
                let end = start.saturating_add(max_len as usize).min(buffer.len());
                let Some(memory) = caller
                    .get_export("memory")
                    .and_then(|export| export.into_memory())
                else {
                    return -1;
                };
                // Validate the whole range before writing any guest bytes.
                let Some(destination_end) = (ptr as usize).checked_add(end - start) else {
                    return -1;
                };
                let Some(destination) = memory.data_mut(&mut caller)
                    .get_mut(ptr as usize..destination_end) else { return -1; };
                buffer.copy_range_into(start, destination);
                len_as_i32(end - start)
            },
        )
        .map_err(|err| format!("Failed defining host_read_active_buffer: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_cursor_line",
            |caller: Caller<'_, HostState>| -> i32 { caller.data().cursor_line },
        )
        .map_err(|err| format!("Failed defining host_cursor_line: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_cursor_col",
            |caller: Caller<'_, HostState>| -> i32 { caller.data().cursor_col },
        )
        .map_err(|err| format!("Failed defining host_cursor_col: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_selection_start_line",
            |caller: Caller<'_, HostState>| -> i32 { caller.data().selection_start_line },
        )
        .map_err(|err| format!("Failed defining host_selection_start_line: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_selection_start_col",
            |caller: Caller<'_, HostState>| -> i32 { caller.data().selection_start_col },
        )
        .map_err(|err| format!("Failed defining host_selection_start_col: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_selection_end_line",
            |caller: Caller<'_, HostState>| -> i32 { caller.data().selection_end_line },
        )
        .map_err(|err| format!("Failed defining host_selection_end_line: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_selection_end_col",
            |caller: Caller<'_, HostState>| -> i32 { caller.data().selection_end_col },
        )
        .map_err(|err| format!("Failed defining host_selection_end_col: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_workspace_root_len",
            |caller: Caller<'_, HostState>| -> i32 {
                caller
                    .data()
                    .workspace_root
                    .as_ref()
                    .map(|root| len_as_i32(root.len()))
                    .unwrap_or(-1)
            },
        )
        .map_err(|err| format!("Failed defining host_workspace_root_len: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_read_workspace_root",
            |mut caller: Caller<'_, HostState>, ptr: i32, max_len: i32| -> i32 {
                if ptr < 0 || max_len <= 0 {
                    return -1;
                }
                let bytes = {
                    let Some(root) = caller.data().workspace_root.as_ref() else {
                        return -1;
                    };
                    root.as_bytes().to_vec()
                };
                let write_len = bytes.len().min(max_len as usize);
                if write_guest_bytes(&mut caller, ptr as usize, &bytes[..write_len]).is_err() {
                    return -1;
                }
                write_len as i32
            },
        )
        .map_err(|err| format!("Failed defining host_read_workspace_root: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_workspace_files_size",
            |mut caller: Caller<'_, HostState>| -> i32 {
                let host = caller.data_mut();
                if host.workspace_files.is_none() {
                    let Some(root) = host.workspace_root.clone() else {
                        return -1;
                    };
                    host.workspace_files = Some(collect_workspace_file_bytes(Path::new(&root)));
                }
                host.workspace_files
                    .as_ref()
                    .map(|bytes| len_as_i32(bytes.len()))
                    .unwrap_or(-1)
            },
        )
        .map_err(|err| format!("Failed defining host_workspace_files_size: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_read_workspace_files",
            |mut caller: Caller<'_, HostState>, offset: i32, ptr: i32, max_len: i32| -> i32 {
                if offset < 0 || ptr < 0 || max_len <= 0 {
                    return -1;
                }
                let slice = {
                    let host = caller.data_mut();
                    if host.workspace_files.is_none() {
                        let Some(root) = host.workspace_root.clone() else {
                            return -1;
                        };
                        host.workspace_files = Some(collect_workspace_file_bytes(Path::new(&root)));
                    }
                    let Some(bytes) = host.workspace_files.as_ref() else {
                        return -1;
                    };
                    let start = offset as usize;
                    if start >= bytes.len() {
                        return 0;
                    }
                    let end = start.saturating_add(max_len as usize).min(bytes.len());
                    bytes[start..end].to_vec()
                };
                if write_guest_bytes(&mut caller, ptr as usize, &slice).is_err() {
                    return -1;
                }
                len_as_i32(slice.len())
            },
        )
        .map_err(|err| format!("Failed defining host_read_workspace_files: {err}"))?;

    linker
        .func_wrap("vtcode", "host_current_view_event_len", |caller: Caller<'_, HostState>| {
            caller
                .data()
                .current_view_event
                .as_ref()
                .map(|value| len_as_i32(value.len()))
                .unwrap_or(0)
        })
        .map_err(|err| format!("Failed defining host_current_view_event_len: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_read_current_view_event",
            |mut caller: Caller<'_, HostState>, ptr: i32, max_len: i32| -> i32 {
                let Some(bytes) = caller.data().current_view_event.clone() else {
                    return 0;
                };
                if ptr < 0 || max_len <= 0 {
                    return 0;
                }
                let max_len = max_len as usize;
                let slice = &bytes[..bytes.len().min(max_len)];
                if write_guest_bytes(&mut caller, ptr as usize, slice).is_err() {
                    return 0;
                }
                len_as_i32(slice.len())
            },
        )
        .map_err(|err| format!("Failed defining host_read_current_view_event: {err}"))?;

    // --- Overlay host functions ---

    linker
        .func_wrap(
            "vtcode",
            "host_register_overlay",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             title_ptr: i32,
             title_len: i32,
             content_ptr: i32,
             content_len: i32,
             x: i32,
             y: i32,
             width: i32,
             height: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let title = read_guest_string(&mut caller, title_ptr, title_len);
                let content = read_guest_string(&mut caller, content_ptr, content_len);
                if let (Some(id), Some(title), Some(content)) = (id, title, content) {
                    if id.trim().is_empty() || title.trim().is_empty() {
                        return;
                    }
                    let host = caller.data_mut();
                    let overlay = host_overlay_entry_mut(host, &id);
                    overlay.title = title;
                    overlay.content = content;
                    overlay.x = x.max(0) as u16;
                    overlay.y = y.max(0) as u16;
                    overlay.width = width.max(1) as u16;
                    overlay.height = height.max(1) as u16;
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_overlay: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_set_overlay_content",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             content_ptr: i32,
             content_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let content = read_guest_string(&mut caller, content_ptr, content_len);
                if let (Some(id), Some(content)) = (id, content) {
                    if let Some(existing) =
                        caller.data_mut().overlays.get_mut(&id)
                    {
                        existing.content = content;
                    }
                }
            },
        )
        .map_err(|err| format!("Failed defining host_set_overlay_content: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_close_overlay",
            |mut caller: Caller<'_, HostState>, id_ptr: i32, id_len: i32| {
                if let Some(id) = read_guest_string(&mut caller, id_ptr, id_len) {
                    if !id.trim().is_empty() {
                        caller.data_mut().requested_close_overlays.push(id);
                    }
                }
            },
        )
        .map_err(|err| format!("Failed defining host_close_overlay: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_overlay_mouse_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let export = read_guest_string(&mut caller, export_ptr, export_len);
                if let (Some(id), Some(export)) = (id, export) {
                    if id.trim().is_empty() {
                        return;
                    }
                    host_overlay_entry_mut(caller.data_mut(), &id).mouse_handler =
                        Some(export.trim().to_string()).filter(|value| !value.is_empty());
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_overlay_mouse_handler: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_overlay_key_handler",
            |mut caller: Caller<'_, HostState>,
             id_ptr: i32,
             id_len: i32,
             export_ptr: i32,
             export_len: i32| {
                let id = read_guest_string(&mut caller, id_ptr, id_len);
                let export = read_guest_string(&mut caller, export_ptr, export_len);
                if let (Some(id), Some(export)) = (id, export) {
                    if id.trim().is_empty() {
                        return;
                    }
                    host_overlay_entry_mut(caller.data_mut(), &id).key_handler =
                        Some(export.trim().to_string()).filter(|value| !value.is_empty());
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_overlay_key_handler: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_register_tick_handler",
            |mut caller: Caller<'_, HostState>,
             export_ptr: i32,
             export_len: i32,
             interval_ms: i32| {
                if let Some(export) = read_guest_string(&mut caller, export_ptr, export_len) {
                    if export.trim().is_empty() {
                        return;
                    }
                    caller.data_mut().tick_handlers.retain(|handler| handler.export_name != export);
                    if interval_ms <= 0 { return; }
                    caller.data_mut().tick_handlers.push(ExtensionTickHandler {
                        export_name: export.trim().to_string(),
                        interval_ms: interval_ms as u32,
                        background: false,
                    });
                }
            },
        )
        .map_err(|err| format!("Failed defining host_register_tick_handler: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_insert_text",
            |mut caller: Caller<'_, HostState>, line: i32, col: i32, text_ptr: i32, text_len: i32| -> i32 {
                let text = read_guest_string(&mut caller, text_ptr, text_len);
                if let Some(text) = text {
                    if line >= 0 && col >= 0 {
                        caller.data_mut().text_edit_requests.push(TextEditRequest::InsertText {
                            line: line as usize,
                            col: col as usize,
                            text,
                        });
                        return 0;
                    }
                }
                -1
            },
        )
        .map_err(|err| format!("Failed defining host_insert_text: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_delete_range",
            |mut caller: Caller<'_, HostState>, start_line: i32, start_col: i32, end_line: i32, end_col: i32| -> i32 {
                if start_line >= 0 && start_col >= 0 && end_line >= 0 && end_col >= 0 {
                    caller.data_mut().text_edit_requests.push(TextEditRequest::DeleteRange {
                        start_line: start_line as usize,
                        start_col: start_col as usize,
                        end_line: end_line as usize,
                        end_col: end_col as usize,
                    });
                    0
                } else {
                    -1
                }
            },
        )
        .map_err(|err| format!("Failed defining host_delete_range: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_replace_selection",
            |mut caller: Caller<'_, HostState>, text_ptr: i32, text_len: i32| -> i32 {
                let text = read_guest_string(&mut caller, text_ptr, text_len);
                if let Some(text) = text {
                    caller.data_mut().text_edit_requests.push(TextEditRequest::ReplaceSelection { text });
                    0
                } else {
                    -1
                }
            },
        )
        .map_err(|err| format!("Failed defining host_replace_selection: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_publish_diagnostic",
            |mut caller: Caller<'_, HostState>,
             file_ptr: i32, file_len: i32,
             line: i32, col: i32,
             end_line: i32, end_col: i32,
             severity: i32,
             message_ptr: i32, message_len: i32| -> i32 {
                let file = read_guest_string(&mut caller, file_ptr, file_len);
                let message = read_guest_string(&mut caller, message_ptr, message_len);
                if let (Some(file), Some(message)) = (file, message) {
                    caller.data_mut().extension_diagnostics.push(ExtensionDiagnostic {
                        file,
                        line: line.max(0) as usize,
                        col: col.max(0) as usize,
                        end_line: end_line.max(0) as usize,
                        end_col: end_col.max(0) as usize,
                        severity: (severity.clamp(1, 4)) as u8,
                        message,
                    });
                    0
                } else {
                    -1
                }
            },
        )
        .map_err(|err| format!("Failed defining host_publish_diagnostic: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_clear_diagnostics",
            |mut caller: Caller<'_, HostState>, file_ptr: i32, file_len: i32| -> i32 {
                if let Some(file) = read_guest_string(&mut caller, file_ptr, file_len) {
                    caller.data_mut().clear_diagnostics_files.push(file);
                    0
                } else {
                    -1
                }
            },
        )
        .map_err(|err| format!("Failed defining host_clear_diagnostics: {err}"))?;

    // Settings persistence
    linker
        .func_wrap(
            "vtcode",
            "host_save_setting",
            |mut caller: Caller<'_, HostState>,
             key_ptr: i32,
             key_len: i32,
             value_ptr: i32,
             value_len: i32| -> i32 {
                let key = match read_guest_string(&mut caller, key_ptr, key_len) {
                    Some(k) if !k.trim().is_empty() => k,
                    _ => return -1,
                };
                let value = match read_guest_string(&mut caller, value_ptr, value_len) {
                    Some(v) => v,
                    _ => return -1,
                };
                let ext_id = caller.data().extension_id.clone();
                let settings_path = match caller.data().settings_file_path.clone() {
                    Some(p) => p,
                    None => return -1,
                };
                save_extension_setting(&settings_path, &ext_id, &key, &value)
            },
        )
        .map_err(|err| format!("Failed defining host_save_setting: {err}"))?;

    linker
        .func_wrap(
            "vtcode",
            "host_load_setting",
            |mut caller: Caller<'_, HostState>,
             key_ptr: i32,
             key_len: i32,
             value_ptr: i32,
             max_len: i32| -> i32 {
                let key = match read_guest_string(&mut caller, key_ptr, key_len) {
                    Some(k) if !k.trim().is_empty() => k,
                    _ => return -1,
                };
                let ext_id = caller.data().extension_id.clone();
                let settings_path = match caller.data().settings_file_path.clone() {
                    Some(p) => p,
                    None => return -1,
                };
                match load_extension_setting(&settings_path, &ext_id, &key) {
                    Some(value) => {
                        if max_len <= 0 || value_ptr < 0 {
                            return len_as_i32(value.len());
                        }
                        let bytes = value.as_bytes();
                        let write_len = bytes.len().min(max_len as usize);
                        if write_guest_bytes(&mut caller, value_ptr as usize, &bytes[..write_len]).is_err() {
                            return -1;
                        }
                        write_len as i32
                    }
                    None => 0,
                }
            },
        )
        .map_err(|err| format!("Failed defining host_load_setting: {err}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::len_as_i32;

    #[test]
    fn extensions_cannot_import_ssh_capabilities() {
        let engine = wasmtime::Engine::default();
        let mut linker = wasmtime::Linker::new(&engine);
        super::define_host_functions(&mut linker).unwrap();
        let mut store = wasmtime::Store::new(&engine, super::HostState::default());
        for (name, params) in [
            ("host_open_remote_terminal", "i32 i32"),
            ("host_ssh_execute", "i32 i32 i32 i32"),
            ("host_read_ssh_result", "i32 i32"),
        ] {
            let module = wasmtime::Module::new(
                &engine,
                format!(r#"(module (import "vtcode" "{name}" (func (param {params}) (result i32))))"#),
            )
            .unwrap();
            let error = linker.instantiate(&mut store, &module).unwrap_err();
            assert!(error.to_string().contains(name));
        }
    }

    #[test]
    fn zero() {
        assert_eq!(len_as_i32(0), 0);
    }

    #[test]
    fn small_value() {
        assert_eq!(len_as_i32(42), 42);
    }

    #[test]
    fn i32_max() {
        assert_eq!(len_as_i32(i32::MAX as usize), i32::MAX);
    }

    #[test]
    fn above_i32_max() {
        assert_eq!(len_as_i32(i32::MAX as usize + 1), i32::MAX);
    }

    #[test]
    fn large_usize() {
        assert_eq!(len_as_i32(usize::MAX), i32::MAX);
    }
}
