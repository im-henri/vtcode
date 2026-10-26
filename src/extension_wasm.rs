use std::collections::HashMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;
use wasmtime::{Caller, Extern, Instance, Store};

use crate::extension_manifest::{
    EditorContext, ExtensionCommand, ExtensionDiagnostic, ExtensionOverlay, ExtensionState,
    ExtensionTickHandler, ExtensionView, TextEditRequest,
};

pub(crate) const MAX_GUEST_STRING_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_LOG_ENTRIES: usize = 64;
pub(crate) const EXTENSION_CALL_FUEL: u64 = 10_000_000;
// Background tasks run on workers and need room for bounded decoding or
// indexing. UI callbacks keep the smaller budget, and task fuel still prevents
// a guest from monopolizing a worker indefinitely.
const EXTENSION_TASK_FUEL: u64 = 250_000_000;

pub(crate) fn should_skip_workspace_entry(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_str().unwrap_or("");
    if name.starts_with('.') {
        return true;
    }
    entry.file_type().is_dir()
        && matches!(
            name,
            "target"
                | "node_modules"
                | "__pycache__"
                | ".venv"
                | "venv"
                | ".tox"
                | "dist"
                | "build"
                | ".mypy_cache"
                | ".pytest_cache"
        )
}

pub(crate) fn collect_workspace_file_bytes(root: &Path) -> Vec<u8> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| !should_skip_workspace_entry(entry))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        files.push(relative);
    }
    files.sort_unstable();
    files.join("\n").into_bytes()
}

pub(crate) struct HostState {
    pub(crate) remote_workspace: Option<crate::remote::workspace::Workspace>,
    pub(crate) limits: wasmtime::StoreLimits,
    pub(crate) documents: Vec<crate::extension_documents::BinaryDocument>,
    pub(crate) document_openers: HashMap<u32, String>,
    pub(crate) current_command_id: Option<String>,
    pub(crate) last_host_error: String,
    pub(crate) clipboard_text: String,
    pub(crate) current_view_event_text: Option<String>,
    pub(crate) clipboard_write: Option<String>,
    pub(crate) logs: Vec<String>,
    pub(crate) status: Option<String>,
    pub(crate) commands: Vec<(String, String)>,
    pub(crate) views: HashMap<String, ExtensionView>,
    pub(crate) explorer_context_commands: Vec<(String, String)>,
    pub(crate) binary_open_commands: Vec<(String, String)>,
    pub(crate) file_open_commands: Vec<(String, String, Vec<String>)>,
    pub(crate) command_context_path: Option<String>,
    pub(crate) command_context_bytes: Option<Vec<u8>>,
    pub(crate) active_file_path: Option<String>,
    pub(crate) active_buffer: Option<crate::extension_buffer::Buffer>,
    pub(crate) active_modified: bool,
    pub(crate) modified_file_paths: Vec<std::path::PathBuf>,
    pub(crate) cursor_line: i32,
    pub(crate) cursor_col: i32,
    pub(crate) selection_start_line: i32,
    pub(crate) selection_start_col: i32,
    pub(crate) selection_end_line: i32,
    pub(crate) selection_end_col: i32,
    pub(crate) workspace_root: Option<String>,
    pub(crate) workspace_files: Option<Vec<u8>>,
    pub(crate) requested_open_view: Option<String>,
    pub(crate) current_view_event: Option<Vec<u8>>,
    pub(crate) overlays: HashMap<String, ExtensionOverlay>,
    pub(crate) tick_handlers: Vec<ExtensionTickHandler>,
    pub(crate) task_deadline: Option<std::time::Instant>,
    pub(crate) requested_close_overlays: Vec<String>,
    pub(crate) text_edit_requests: Vec<TextEditRequest>,
    pub(crate) extension_diagnostics: Vec<ExtensionDiagnostic>,
    pub(crate) clear_diagnostics_files: Vec<String>,
    pub(crate) extension_id: String,
    pub(crate) settings_file_path: Option<PathBuf>,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            remote_workspace: None,
            limits: wasmtime::StoreLimitsBuilder::new()
                .memory_size(128 * 1024 * 1024)
                .instances(1)
                .memories(1)
                .tables(4)
                .table_elements(100_000)
                .build(),
            documents: Vec::new(),
            document_openers: HashMap::new(),
            current_command_id: None,
            last_host_error: String::new(),
            clipboard_text: String::new(),
            current_view_event_text: None,
            clipboard_write: None,
            logs: Vec::new(),
            status: None,
            commands: Vec::new(),
            views: HashMap::new(),
            explorer_context_commands: Vec::new(),
            binary_open_commands: Vec::new(),
            file_open_commands: Vec::new(),
            command_context_path: None,
            command_context_bytes: None,
            active_file_path: None,
            active_buffer: None,
            active_modified: false,
            modified_file_paths: Vec::new(),
            cursor_line: -1,
            cursor_col: -1,
            selection_start_line: -1,
            selection_start_col: -1,
            selection_end_line: -1,
            selection_end_col: -1,
            workspace_root: None,
            workspace_files: None,
            requested_open_view: None,
            current_view_event: None,
            overlays: HashMap::new(),
            tick_handlers: Vec::new(),
            task_deadline: None,
            requested_close_overlays: Vec::new(),
            text_edit_requests: Vec::new(),
            extension_diagnostics: Vec::new(),
            clear_diagnostics_files: Vec::new(),
            extension_id: String::new(),
            settings_file_path: None,
        }
    }
}

pub(crate) fn clear_invocation_context(host: &mut HostState) {
    host.command_context_path = None;
    host.command_context_bytes = None;
    host.current_command_id = None;
    host.active_file_path = None;
    host.active_buffer = None;
    host.active_modified = false;
    host.modified_file_paths.clear();
    host.cursor_line = -1;
    host.cursor_col = -1;
    host.selection_start_line = -1;
    host.selection_start_col = -1;
    host.selection_end_line = -1;
    host.selection_end_col = -1;
    host.workspace_root = None;
    host.remote_workspace = None;
    host.workspace_files = None;
    host.requested_open_view = None;
    host.current_view_event = None;
    host.current_view_event_text = None;
}

pub(crate) fn apply_invocation_context(
    host: &mut HostState,
    command_context_path: Option<String>,
    command_context_bytes: Option<Vec<u8>>,
    editor_context: Option<EditorContext>,
) {
    clear_invocation_context(host);
    host.command_context_path = command_context_path;
    host.command_context_bytes = command_context_bytes;

    let Some(context) = editor_context else {
        return;
    };

    host.active_file_path = context.active_file_path;
    host.active_buffer = context.active_buffer;
    host.active_modified = context.active_modified;
    host.modified_file_paths = context.modified_file_paths;
    host.cursor_line = context
        .cursor_line
        .and_then(|line| i32::try_from(line).ok())
        .unwrap_or(-1);
    host.cursor_col = context
        .cursor_col
        .and_then(|col| i32::try_from(col).ok())
        .unwrap_or(-1);
    if let Some((sl, sc, el, ec)) = context.selection_range {
        host.selection_start_line = i32::try_from(sl).unwrap_or(-1);
        host.selection_start_col = i32::try_from(sc).unwrap_or(-1);
        host.selection_end_line = i32::try_from(el).unwrap_or(-1);
        host.selection_end_col = i32::try_from(ec).unwrap_or(-1);
    }
    host.workspace_root = context.workspace_root;
    host.remote_workspace = context.remote_workspace;
}

pub(crate) struct ExtensionRuntime {
    pub(crate) store: Store<HostState>,
    pub(crate) instance: Instance,
}

#[derive(Clone, Copy)]
pub(crate) enum ViewEventHandlerKind {
    Key,
    Mouse,
    Scroll,
    Resize,
    Close,
    Hover,
    ContextMenu,
}

pub(crate) fn host_view_entry_mut<'a>(host: &'a mut HostState, id: &str) -> &'a mut ExtensionView {
    host.views
        .entry(id.to_string())
        .or_insert_with(|| ExtensionView {
            id: id.to_string(),
            title: String::new(),
            content: String::new(),
            key_handler: None,
            mouse_handler: None,
            scroll_handler: None,
            resize_handler: None,
            close_handler: None,
            hover_handler: None,
            context_menu_handler: None,
            pointer_capture: false,
            input_pending: false,
            frame: None,
        })
}

pub(crate) fn host_overlay_entry_mut<'a>(
    host: &'a mut HostState,
    id: &str,
) -> &'a mut ExtensionOverlay {
    host.overlays
        .entry(id.to_string())
        .or_insert_with(|| ExtensionOverlay {
            id: id.to_string(),
            title: String::new(),
            content: String::new(),
            x: 0,
            y: 0,
            width: 40,
            height: 10,
            mouse_handler: None,
            key_handler: None,
        })
}

/// Opaque wrapper for passing ExtensionRuntime through channels.
/// Wasmtime's Store<T> is Send when T is Send.
pub(crate) struct ExtensionRuntimeBox(pub(crate) Option<ExtensionRuntime>);

// Safety: HostState is Send (all fields are Send), Store<HostState> is Send, Instance is Send+Sync
unsafe impl Send for ExtensionRuntimeBox {}

impl ExtensionRuntimeBox {
    pub(crate) fn new(runtime: ExtensionRuntime) -> Self {
        Self(Some(runtime))
    }
    pub(crate) fn take(&mut self) -> Option<ExtensionRuntime> {
        self.0.take()
    }
}

pub(crate) struct ExtensionRecord {
    pub(crate) manifest: crate::extension_manifest::ExtensionManifest,
    pub(crate) manifest_path: PathBuf,
    pub(crate) wasm_path: PathBuf,
    pub(crate) user_unloaded: bool,
    pub(crate) state: ExtensionState,
    pub(crate) status_text: String,
    pub(crate) commands: Vec<ExtensionCommand>,
    pub(crate) views: HashMap<String, ExtensionView>,
    pub(crate) explorer_context_commands: Vec<(String, String)>,
    pub(crate) binary_open_commands: Vec<(String, String)>,
    pub(crate) file_open_commands: Vec<(String, String, Vec<String>)>,
    pub(crate) requested_open_view: Option<String>,
    pub(crate) logs: Vec<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) overlays: HashMap<String, ExtensionOverlay>,
    pub(crate) tick_handlers: Vec<ExtensionTickHandler>,
    pub(crate) manifest_mtime: Option<std::time::SystemTime>,
    pub(crate) wasm_mtime: Option<std::time::SystemTime>,
    pub(crate) runtime: Option<ExtensionRuntime>,
    pub(crate) text_edit_requests: Vec<TextEditRequest>,
    pub(crate) published_diagnostics: HashMap<String, Vec<ExtensionDiagnostic>>,
    pub(crate) diagnostics_version: u64,
}

pub(crate) fn sanitize_contributed_views(
    views: &HashMap<String, ExtensionView>,
) -> HashMap<String, ExtensionView> {
    let mut result: HashMap<String, ExtensionView> = HashMap::new();
    for view in views.values() {
        if view.id.trim().is_empty() || view.title.trim().is_empty() {
            continue;
        }
        result.insert(view.id.clone(), view.clone());
    }
    result
}

pub(crate) fn call_noarg_export(
    store: &mut Store<HostState>,
    instance: &Instance,
    export: &str,
    required: bool,
) -> Result<Option<i32>, String> {
    store.data_mut().task_deadline = None;
    // Production stores use a fuel-enabled engine. Ignore the error for unit
    // test stores built with Engine::default(); those guests are tiny and
    // deterministic.
    let task = export == "vtcode_prepare_document_action" || store
        .data()
        .tick_handlers
        .iter()
        .any(|handler| handler.background && handler.export_name == export);
    // Input snapshots can contain megabytes of escaped clipboard text. Give
    // bounded decoding work proportional fuel while keeping the task cap.
    let event_bytes = store.data().current_view_event.as_ref().map_or(0, Vec::len) as u64;
    let _ = store.set_fuel(if task {
        EXTENSION_TASK_FUEL
    } else {
        EXTENSION_CALL_FUEL.saturating_add(event_bytes.saturating_mul(64)).min(EXTENSION_TASK_FUEL)
    });
    if instance.get_func(&mut *store, export).is_none() {
        if required {
            return Err(format!("Missing required export `{export}`"));
        }
        return Ok(None);
    }

    store.data_mut().task_deadline = task.then(|| {
        std::time::Instant::now() + std::time::Duration::from_millis(6)
    });
    let result = if let Ok(func) = instance.get_typed_func::<(), i32>(&mut *store, export) {
        func
            .call(&mut *store, ())
            .map(Some)
            .map_err(|err| format!("Failed calling `{export}`: {err:#}"))
    } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut *store, export) {
        func.call(&mut *store, ())
            .map(|()| Some(0))
            .map_err(|err| format!("Failed calling `{export}`: {err:#}"))
    } else {
        Err(format!(
            "Export `{export}` has unsupported signature; expected () -> i32 or () -> ()"
        ))
    };
    store.data_mut().task_deadline = None;
    result
}

pub(crate) fn call_i32_arg_export(
    store: &mut Store<HostState>,
    instance: &Instance,
    export: &str,
    arg: i32,
) -> Result<i32, String> {
    store.data_mut().task_deadline = None;
    let _ = store.set_fuel(EXTENSION_CALL_FUEL);
    if instance.get_func(&mut *store, export).is_none() {
        return Err(format!("Missing required export `{export}`"));
    }
    if let Ok(func) = instance.get_typed_func::<i32, i32>(&mut *store, export) {
        let code = func
            .call(&mut *store, arg)
            .map_err(|err| format!("Failed calling `{export}`: {err:#}"))?;
        return Ok(code);
    }
    if let Ok(func) = instance.get_typed_func::<i32, ()>(&mut *store, export) {
        func.call(&mut *store, arg)
            .map_err(|err| format!("Failed calling `{export}`: {err:#}"))?;
        return Ok(0);
    }
    Err(format!(
        "Export `{export}` has unsupported signature; expected (i32) -> i32 or (i32) -> ()"
    ))
}

pub(crate) fn save_extension_setting(
    settings_path: &Path,
    ext_id: &str,
    key: &str,
    value: &str,
) -> i32 {
    let mut all_settings: serde_json::Value = if settings_path.exists() {
        match std::fs::read_to_string(settings_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or(serde_json::json!({})),
            Err(_) => serde_json::json!({}),
        }
    } else {
        serde_json::json!({})
    };

    let ext_obj = all_settings
        .as_object_mut()
        .expect("settings root should be an object")
        .entry(ext_id.to_string())
        .or_insert_with(|| serde_json::json!({}));

    if let Some(obj) = ext_obj.as_object_mut() {
        obj.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    if let Some(parent) = settings_path.parent() {
        let _ = std::fs::create_dir_all(parent); // Error OK: dir may already exist
    }
    match std::fs::write(
        settings_path,
        serde_json::to_string_pretty(&all_settings).unwrap_or_default(),
    ) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

pub(crate) fn load_extension_setting(
    settings_path: &Path,
    ext_id: &str,
    key: &str,
) -> Option<String> {
    let content = std::fs::read_to_string(settings_path).ok()?;
    let all_settings: serde_json::Value = serde_json::from_str(&content).ok()?;
    let ext_obj = all_settings.get(ext_id)?;
    let value = ext_obj.get(key)?;
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

pub(crate) fn read_guest_string(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Option<String> {
    if ptr < 0 || len < 0 {
        return None;
    }
    let start = ptr as usize;
    let length = len as usize;
    if length > MAX_GUEST_STRING_BYTES {
        return None;
    }

    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
        return None;
    };

    let mut bytes = vec![0u8; length];
    if memory.read(caller, start, &mut bytes).is_err() {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub(crate) fn write_guest_bytes(
    caller: &mut Caller<'_, HostState>,
    ptr: usize,
    bytes: &[u8],
) -> Result<(), ()> {
    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
        return Err(());
    };
    memory.write(caller, ptr, bytes).map_err(|_| ())
}
