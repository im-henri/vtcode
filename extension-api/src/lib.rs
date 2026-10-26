pub const API_VERSION: &str = "2";
pub mod editor;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "vtcode")]
unsafe extern "C" {
    fn host_log(ptr: *const u8, len: i32);
    fn host_set_status(ptr: *const u8, len: i32);
    fn host_register_command(id_ptr: *const u8, id_len: i32, title_ptr: *const u8, title_len: i32);
    fn host_register_view(
        id_ptr: *const u8,
        id_len: i32,
        title_ptr: *const u8,
        title_len: i32,
        content_ptr: *const u8,
        content_len: i32,
    );
    fn host_register_view_key_handler(
        id_ptr: *const u8,
        id_len: i32,
        export_ptr: *const u8,
        export_len: i32,
    );
    fn host_register_view_mouse_handler(
        id_ptr: *const u8,
        id_len: i32,
        export_ptr: *const u8,
        export_len: i32,
    );
    fn host_register_view_scroll_handler(
        id_ptr: *const u8,
        id_len: i32,
        export_ptr: *const u8,
        export_len: i32,
    );
    fn host_set_view_content(id_ptr: *const u8, id_len: i32, content_ptr: *const u8, content_len: i32);
    fn host_open_view(id_ptr: *const u8, id_len: i32);
    fn host_register_explorer_context_command(
        id_ptr: *const u8,
        id_len: i32,
        title_ptr: *const u8,
        title_len: i32,
    );
    fn host_register_binary_open_command(
        id_ptr: *const u8,
        id_len: i32,
        title_ptr: *const u8,
        title_len: i32,
    );
    fn host_register_file_open_command(
        id_ptr: *const u8,
        id_len: i32,
        title_ptr: *const u8,
        title_len: i32,
        extensions_ptr: *const u8,
        extensions_len: i32,
    );
    fn host_context_target_path_len() -> i32;
    fn host_read_context_target_path(ptr: *mut u8, max_len: i32) -> i32;
    fn host_context_target_size() -> i32;
    fn host_read_context_target(offset: i32, ptr: *mut u8, max_len: i32) -> i32;
    fn host_active_file_path_len() -> i32;
    fn host_read_active_file_path(ptr: *mut u8, max_len: i32) -> i32;
    fn host_active_buffer_size() -> i32;
    fn host_read_active_buffer(offset: i32, ptr: *mut u8, max_len: i32) -> i32;
    fn host_cursor_line() -> i32;
    fn host_cursor_col() -> i32;
    fn host_selection_start_line() -> i32;
    fn host_selection_start_col() -> i32;
    fn host_selection_end_line() -> i32;
    fn host_selection_end_col() -> i32;
    fn host_workspace_root_len() -> i32;
    fn host_read_workspace_root(ptr: *mut u8, max_len: i32) -> i32;
    fn host_workspace_files_size() -> i32;
    fn host_read_workspace_files(offset: i32, ptr: *mut u8, max_len: i32) -> i32;
    fn host_current_view_event_len() -> i32;
    fn host_read_current_view_event(ptr: *mut u8, max_len: i32) -> i32;

    // Overlay (floating panel) API
    fn host_register_overlay(
        id_ptr: *const u8, id_len: i32,
        title_ptr: *const u8, title_len: i32,
        content_ptr: *const u8, content_len: i32,
        x: i32, y: i32, width: i32, height: i32,
    );
    fn host_set_overlay_content(id_ptr: *const u8, id_len: i32, content_ptr: *const u8, content_len: i32);
    fn host_close_overlay(id_ptr: *const u8, id_len: i32);
    fn host_register_overlay_mouse_handler(
        id_ptr: *const u8, id_len: i32,
        export_ptr: *const u8, export_len: i32,
    );
    fn host_register_overlay_key_handler(
        id_ptr: *const u8, id_len: i32,
        export_ptr: *const u8, export_len: i32,
    );
    fn host_register_tick_handler(export_ptr: *const u8, export_len: i32, interval_ms: i32);

    // Editor buffer modification API
    fn host_insert_text(line: i32, col: i32, text_ptr: *const u8, text_len: i32) -> i32;
    fn host_delete_range(start_line: i32, start_col: i32, end_line: i32, end_col: i32) -> i32;
    fn host_replace_selection(text_ptr: *const u8, text_len: i32) -> i32;

    // Diagnostics API
    fn host_publish_diagnostic(
        file_ptr: *const u8, file_len: i32,
        line: i32, col: i32,
        end_line: i32, end_col: i32,
        severity: i32,
        message_ptr: *const u8, message_len: i32,
    ) -> i32;
    fn host_clear_diagnostics(file_ptr: *const u8, file_len: i32) -> i32;

    // Settings persistence API
    fn host_save_setting(key_ptr: *const u8, key_len: i32, value_ptr: *const u8, value_len: i32) -> i32;
    fn host_load_setting(key_ptr: *const u8, key_len: i32, value_ptr: *mut u8, max_len: i32) -> i32;
}

#[cfg(target_arch = "wasm32")]
fn read_host_string(len: i32, read: impl FnOnce(*mut u8, i32) -> i32) -> Option<String> {
    if len <= 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let written = read(buf.as_mut_ptr(), len);
    if written <= 0 {
        return None;
    }
    buf.truncate(written as usize);
    String::from_utf8(buf).ok()
}

#[cfg(target_arch = "wasm32")]
fn read_host_bytes(len: i32, read: impl Fn(i32, *mut u8, i32) -> i32) -> Option<Vec<u8>> {
    if len < 0 {
        return None;
    }
    if len == 0 {
        return Some(Vec::new());
    }
    let mut out = vec![0u8; len as usize];
    let mut offset = 0usize;
    while offset < out.len() {
        let max_chunk = (out.len() - offset).min(16 * 1024);
        let chunk = read(offset as i32, out[offset..].as_mut_ptr(), max_chunk as i32);
        if chunk <= 0 {
            break;
        }
        offset += chunk as usize;
    }
    out.truncate(offset);
    Some(out)
}

#[cfg(target_arch = "wasm32")]
pub fn log(message: &str) {
    if message.is_empty() {
        return;
    }
    unsafe { host_log(message.as_ptr(), message.len() as i32) };
}

#[cfg(target_arch = "wasm32")]
pub fn set_status(status: &str) {
    unsafe { host_set_status(status.as_ptr(), status.len() as i32) };
}

#[cfg(target_arch = "wasm32")]
pub fn register_command(id: &str, title: &str) {
    if id.is_empty() || title.is_empty() {
        return;
    }
    unsafe {
        host_register_command(
            id.as_ptr(),
            id.len() as i32,
            title.as_ptr(),
            title.len() as i32,
        )
    };
}

#[cfg(target_arch = "wasm32")]
pub fn register_view(id: &str, title: &str, content: &str) {
    if id.is_empty() || title.is_empty() {
        return;
    }
    unsafe {
        host_register_view(
            id.as_ptr(),
            id.len() as i32,
            title.as_ptr(),
            title.len() as i32,
            content.as_ptr(),
            content.len() as i32,
        )
    };
}

#[cfg(target_arch = "wasm32")]
fn register_view_handler(
    id: &str,
    export_name: &str,
    register: unsafe extern "C" fn(*const u8, i32, *const u8, i32),
) {
    if id.is_empty() || export_name.trim().is_empty() {
        return;
    }
    unsafe { register(id.as_ptr(), id.len() as i32, export_name.as_ptr(), export_name.len() as i32) };
}

#[cfg(target_arch = "wasm32")]
pub fn register_view_key_handler(id: &str, export_name: &str) {
    register_view_handler(id, export_name, host_register_view_key_handler);
}

#[cfg(target_arch = "wasm32")]
pub fn register_view_mouse_handler(id: &str, export_name: &str) {
    register_view_handler(id, export_name, host_register_view_mouse_handler);
}

#[cfg(target_arch = "wasm32")]
pub fn register_view_scroll_handler(id: &str, export_name: &str) {
    register_view_handler(id, export_name, host_register_view_scroll_handler);
}

#[cfg(target_arch = "wasm32")]
pub fn set_view_content(id: &str, content: &str) {
    if id.is_empty() {
        return;
    }
    unsafe { host_set_view_content(id.as_ptr(), id.len() as i32, content.as_ptr(), content.len() as i32) };
}


#[cfg(target_arch = "wasm32")]
pub fn open_view(id: &str) {
    if id.is_empty() {
        return;
    }
    unsafe { host_open_view(id.as_ptr(), id.len() as i32) };
}

#[cfg(target_arch = "wasm32")]
pub fn register_explorer_context_command(id: &str, title: &str) {
    if id.is_empty() || title.is_empty() {
        return;
    }
    unsafe {
        host_register_explorer_context_command(
            id.as_ptr(),
            id.len() as i32,
            title.as_ptr(),
            title.len() as i32,
        )
    };
}

#[cfg(target_arch = "wasm32")]
pub fn register_binary_open_command(id: &str, title: &str) {
    if id.is_empty() || title.is_empty() {
        return;
    }
    unsafe {
        host_register_binary_open_command(
            id.as_ptr(),
            id.len() as i32,
            title.as_ptr(),
            title.len() as i32,
        )
    };
}

#[cfg(target_arch = "wasm32")]
pub fn register_file_open_command(id: &str, title: &str, extensions: &str) {
    if id.is_empty() || title.is_empty() || extensions.trim().is_empty() {
        return;
    }
    unsafe {
        host_register_file_open_command(
            id.as_ptr(),
            id.len() as i32,
            title.as_ptr(),
            title.len() as i32,
            extensions.as_ptr(),
            extensions.len() as i32,
        )
    };
}

#[cfg(target_arch = "wasm32")]
pub fn context_target_path() -> Option<String> {
    read_host_string(unsafe { host_context_target_path_len() }, |ptr, max_len| unsafe {
        host_read_context_target_path(ptr, max_len)
    })
}

#[cfg(target_arch = "wasm32")]
pub fn context_target_bytes() -> Option<Vec<u8>> {
    read_host_bytes(unsafe { host_context_target_size() }, |offset, ptr, max_len| unsafe {
        host_read_context_target(offset, ptr, max_len)
    })
}

#[cfg(target_arch = "wasm32")]
pub fn active_file_path() -> Option<String> {
    read_host_string(unsafe { host_active_file_path_len() }, |ptr, max_len| unsafe {
        host_read_active_file_path(ptr, max_len)
    })
}

#[cfg(target_arch = "wasm32")]
pub fn active_buffer_bytes() -> Option<Vec<u8>> {
    read_host_bytes(unsafe { host_active_buffer_size() }, |offset, ptr, max_len| unsafe {
        host_read_active_buffer(offset, ptr, max_len)
    })
}

#[cfg(target_arch = "wasm32")]
pub fn active_buffer() -> Option<String> {
    let bytes = active_buffer_bytes()?;
    String::from_utf8(bytes).ok()
}

#[cfg(target_arch = "wasm32")]
pub fn cursor_position() -> Option<(usize, usize)> {
    let line = unsafe { host_cursor_line() };
    let col = unsafe { host_cursor_col() };
    if line < 0 || col < 0 {
        return None;
    }
    Some((line as usize, col as usize))
}

#[cfg(target_arch = "wasm32")]
pub fn selection_range() -> Option<(usize, usize, usize, usize)> {
    let start_line = unsafe { host_selection_start_line() };
    let start_col = unsafe { host_selection_start_col() };
    let end_line = unsafe { host_selection_end_line() };
    let end_col = unsafe { host_selection_end_col() };
    if start_line < 0 || start_col < 0 || end_line < 0 || end_col < 0 {
        return None;
    }
    Some((
        start_line as usize,
        start_col as usize,
        end_line as usize,
        end_col as usize,
    ))
}

#[cfg(target_arch = "wasm32")]
pub fn workspace_root() -> Option<String> {
    read_host_string(unsafe { host_workspace_root_len() }, |ptr, max_len| unsafe {
        host_read_workspace_root(ptr, max_len)
    })
}

#[cfg(target_arch = "wasm32")]
pub fn workspace_files() -> Option<Vec<String>> {
    let bytes = read_host_bytes(unsafe { host_workspace_files_size() }, |offset, ptr, max_len| unsafe {
        host_read_workspace_files(offset, ptr, max_len)
    })?;
    let text = String::from_utf8(bytes).ok()?;
    if text.is_empty() {
        return Some(Vec::new());
    }
    Some(text.lines().map(str::to_string).collect())
}

#[cfg(target_arch = "wasm32")]
pub fn current_view_event() -> Option<String> {
    read_host_string(unsafe { host_current_view_event_len() }, |ptr, max_len| unsafe {
        host_read_current_view_event(ptr, max_len)
    })
}

/// Read the current event only if its encoded JSON fits the caller's budget.
#[cfg(target_arch = "wasm32")]
pub fn current_view_event_limited(max_bytes: usize) -> Result<Option<String>, String> {
    let len = unsafe { host_current_view_event_len() };
    if len > 0 && len as usize > max_bytes {
        return Err(format!("Encoded input exceeds the {} KiB event budget. Paste a smaller selection.", max_bytes / 1024));
    }
    Ok(read_host_string(len, |ptr, max_len| unsafe { host_read_current_view_event(ptr, max_len) }))
}

/// Register a floating overlay panel that renders on top of the editor.
#[cfg(target_arch = "wasm32")]
pub fn register_overlay(id: &str, title: &str, content: &str, x: u16, y: u16, width: u16, height: u16) {
    if id.is_empty() || title.is_empty() {
        return;
    }
    unsafe {
        host_register_overlay(
            id.as_ptr(), id.len() as i32,
            title.as_ptr(), title.len() as i32,
            content.as_ptr(), content.len() as i32,
            x as i32, y as i32, width as i32, height as i32,
        )
    };
}

/// Update the content of an existing overlay.
#[cfg(target_arch = "wasm32")]
pub fn set_overlay_content(id: &str, content: &str) {
    if id.is_empty() { return; }
    unsafe {
        host_set_overlay_content(
            id.as_ptr(), id.len() as i32,
            content.as_ptr(), content.len() as i32,
        )
    };
}

/// Close and remove an overlay.
#[cfg(target_arch = "wasm32")]
pub fn close_overlay(id: &str) {
    if id.is_empty() { return; }
    unsafe { host_close_overlay(id.as_ptr(), id.len() as i32) };
}

/// Register a mouse handler for an overlay. The handler export receives events
/// via `current_view_event()` as JSON with fields: kind, view_column, view_row, width, height.
#[cfg(target_arch = "wasm32")]
pub fn register_overlay_mouse_handler(id: &str, export_name: &str) {
    if id.is_empty() || export_name.is_empty() { return; }
    unsafe {
        host_register_overlay_mouse_handler(
            id.as_ptr(), id.len() as i32,
            export_name.as_ptr(), export_name.len() as i32,
        )
    };
}

/// Register a keyboard handler for an overlay. The handler export receives events
/// via `current_view_event()` as JSON with fields: code, text, shift, control, alt.
#[cfg(target_arch = "wasm32")]
pub fn register_overlay_key_handler(id: &str, export_name: &str) {
    if id.is_empty() || export_name.is_empty() { return; }
    unsafe {
        host_register_overlay_key_handler(
            id.as_ptr(), id.len() as i32,
            export_name.as_ptr(), export_name.len() as i32,
        )
    };
}

/// Register a tick handler that is called periodically at the given interval (milliseconds).
/// The handler export name is called with no arguments; use `current_view_event()` if needed.
#[cfg(target_arch = "wasm32")]
pub fn register_tick_handler(export_name: &str, interval_ms: u32) {
    if export_name.is_empty() { return; }
    unsafe {
        host_register_tick_handler(
            export_name.as_ptr(), export_name.len() as i32,
            interval_ms as i32,
        )
    };
}

/// Stop a previously registered timer. Extensions should stop timers when idle.
#[cfg(target_arch = "wasm32")]
pub fn unregister_tick_handler(export_name: &str) { register_tick_handler(export_name, 0); }

/// Insert text at the given line and column position in the active editor.
/// Returns 0 on success, -1 on error.
#[cfg(target_arch = "wasm32")]
pub fn insert_text(line: usize, col: usize, text: &str) -> i32 {
    if text.is_empty() { return -1; }
    unsafe {
        host_insert_text(line as i32, col as i32, text.as_ptr(), text.len() as i32)
    }
}

/// Delete a range of text in the active editor.
/// Returns 0 on success, -1 on error.
#[cfg(target_arch = "wasm32")]
pub fn delete_range(start_line: usize, start_col: usize, end_line: usize, end_col: usize) -> i32 {
    unsafe {
        host_delete_range(start_line as i32, start_col as i32, end_line as i32, end_col as i32)
    }
}

/// Replace the current selection in the active editor with the given text.
/// If no selection exists, inserts at cursor position.
/// Returns 0 on success, -1 on error.
#[cfg(target_arch = "wasm32")]
pub fn replace_selection(text: &str) -> i32 {
    unsafe {
        host_replace_selection(text.as_ptr(), text.len() as i32)
    }
}

/// Publish a diagnostic for a file. Severity: 1=error, 2=warning, 3=info, 4=hint.
/// Returns 0 on success, -1 on error.
#[cfg(target_arch = "wasm32")]
pub fn publish_diagnostic(file: &str, line: usize, col: usize, end_line: usize, end_col: usize, severity: u8, message: &str) -> i32 {
    if file.is_empty() || message.is_empty() {
        return -1;
    }
    unsafe {
        host_publish_diagnostic(
            file.as_ptr(), file.len() as i32,
            line as i32, col as i32,
            end_line as i32, end_col as i32,
            severity as i32,
            message.as_ptr(), message.len() as i32,
        )
    }
}

/// Clear all diagnostics published by this extension for the given file.
/// Returns 0 on success, -1 on error.
#[cfg(target_arch = "wasm32")]
pub fn clear_diagnostics(file: &str) -> i32 {
    if file.is_empty() {
        return -1;
    }
    unsafe {
        host_clear_diagnostics(file.as_ptr(), file.len() as i32)
    }
}

/// Save a setting value for this extension. Settings persist in the portable directory under .vtcode/workspaces/<id>/extension-settings.json.
/// Returns 0 on success, -1 on error.
#[cfg(target_arch = "wasm32")]
pub fn save_setting(key: &str, value: &str) -> i32 {
    if key.is_empty() {
        return -1;
    }
    unsafe {
        host_save_setting(
            key.as_ptr(), key.len() as i32,
            value.as_ptr(), value.len() as i32,
        )
    }
}

/// Load a setting value for this extension. Returns None if the setting doesn't exist.
#[cfg(target_arch = "wasm32")]
pub fn load_setting(key: &str) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    // First call with max_len=0 to get the size
    let size = unsafe {
        host_load_setting(key.as_ptr(), key.len() as i32, std::ptr::null_mut(), 0)
    };
    if size <= 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let written = unsafe {
        host_load_setting(
            key.as_ptr(), key.len() as i32,
            buf.as_mut_ptr(), size,
        )
    };
    if written <= 0 {
        return None;
    }
    buf.truncate(written as usize);
    String::from_utf8(buf).ok()
}
