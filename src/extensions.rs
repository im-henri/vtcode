use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use wasmtime::{Config, Engine};

// Re-export manifest types so existing `use crate::extensions::...` paths keep working.
pub(crate) use crate::extension_manifest::{
    EditorContext, EditorContextSnapshot, ExtensionContextCommand, ExtensionDiagnostic,
    ExtensionOverlay, ExtensionRecordView, ExtensionState, ExtensionTickHandler, ExtensionView,
    TextEditRequest,
};

use crate::extension_manifest::{self};
use crate::extension_wasm::ExtensionRecord;

/// Input routing needs a stable list across callbacks, but never needs to copy
/// an overlay's potentially large text or title on each click or keystroke.
pub(crate) struct OverlayTarget {
    pub id: String,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub mouse_handler: bool,
    pub key_handler: bool,
}

// Re-export ExtensionRuntimeBox so existing `use crate::extensions::ExtensionRuntimeBox` works.
pub(crate) use crate::extension_wasm::ExtensionRuntimeBox;

pub(crate) fn new_extension_engine() -> Engine {
    let mut config = Config::new();
    config.consume_fuel(true);
    // The bounded asset worker already supplies concurrency. Wasmtime's
    // process-wide compilation pool would otherwise compete with input on all
    // cores during startup, outside our worker-priority policy.
    config.parallel_compilation(false);
    Engine::new(&config).expect("valid Wasmtime extension configuration")
}

// Fixtures reuse immutable code, while every activation still creates a fresh
// Store, Instance and HostState. Exact byte keys keep reload changes visible.
#[cfg(test)]
pub(crate) fn test_extension_engine() -> &'static Engine {
    static ENGINE: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(new_extension_engine)
}

#[cfg(test)]
pub(crate) fn test_extension_module(
    engine: &Engine,
    bytes: &[u8],
) -> wasmtime::Result<wasmtime::Module> {
    if !Engine::same(engine, test_extension_engine()) {
        return wasmtime::Module::new(engine, bytes);
    }
    static MODULES: std::sync::OnceLock<std::sync::Mutex<HashMap<Vec<u8>, wasmtime::Module>>> =
        std::sync::OnceLock::new();
    let mut modules = MODULES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(module) = modules.get(bytes) {
        return Ok(module.clone());
    }
    let module = wasmtime::Module::new(engine, bytes)?;
    modules.insert(bytes.to_vec(), module.clone());
    Ok(module)
}

pub(crate) struct ExtensionManager {
    pub(crate) engine: std::sync::Arc<std::sync::OnceLock<Engine>>,
    pub(crate) search_roots: Vec<PathBuf>,
    pub(crate) records: Vec<ExtensionRecord>,
    pub(crate) index_by_id: HashMap<String, usize>,
    pub(crate) settings_file: PathBuf,
    pub(crate) busy_extension_ids: HashSet<String>,
    pub(crate) pending_view_events:
        std::collections::VecDeque<crate::extension_handlers::PendingViewEvent>,
    pub(crate) view_resize_requests: HashMap<(String, String), (u64, u64)>,
    pub(crate) tick_yield_to_input: HashSet<String>,
}

impl ExtensionManager {
    pub(crate) fn new(project_root: &Path) -> Self {
        let search_roots = vec![
            crate::portable::data_dir().join("extensions"),
            crate::portable::workspace_dir(project_root).join("extensions"),
            project_root.join("extensions"),
            project_root.join(".vtcode").join("extensions"),
        ];
        let settings_file =
            crate::portable::workspace_dir(project_root).join("extension-settings.json");
        Self::with_paths(search_roots, settings_file)
    }

    fn with_paths(search_roots: Vec<PathBuf>, settings_file: PathBuf) -> Self {
        Self {
            engine: Default::default(),
            search_roots,
            records: Vec::new(),
            index_by_id: HashMap::new(),
            settings_file,
            busy_extension_ids: HashSet::new(),
            pending_view_events: std::collections::VecDeque::new(),
            view_resize_requests: HashMap::new(),
            tick_yield_to_input: HashSet::new(),
        }
    }

    /// An owned, empty manager for initial background activation. The live
    /// manager stays available to the UI until the completed snapshot arrives.
    pub(crate) fn initial_scan_manager(&self) -> Self {
        let mut manager = Self::with_paths(self.search_roots.clone(), self.settings_file.clone());
        manager.engine = self.engine.clone();
        manager
    }

    #[cfg(test)]
    pub(crate) fn scan_and_sync(&mut self) -> Vec<String> {
        let (discovered, notices) = extension_manifest::discover_extensions(&self.search_roots);
        self.sync_discovered(discovered, notices)
    }

    pub(crate) fn sync_discovered(
        &mut self,
        discovered: Vec<extension_manifest::DiscoveredExtension>,
        mut notices: Vec<String>,
    ) -> Vec<String> {
        let mut old_records: HashMap<String, ExtensionRecord> = std::mem::take(&mut self.records)
            .into_iter()
            .map(|record| (record.manifest.id.clone(), record))
            .collect();
        let mut next_records = Vec::new();

        for found in discovered {
            if let Some(mut existing) = old_records.remove(&found.manifest.id) {
                if Self::record_has_unsaved_documents(&existing) {
                    next_records.push(existing);
                    continue;
                }
                let changed = extension_manifest::record_changed(
                    &existing.manifest_path,
                    &existing.wasm_path,
                    &existing.manifest,
                    existing.manifest_mtime,
                    existing.wasm_mtime,
                    &found,
                );
                existing.manifest = found.manifest;
                existing.manifest_path = found.manifest_path;
                existing.wasm_path = found.wasm_path;
                existing.manifest_mtime = found.manifest_mtime;
                existing.wasm_mtime = found.wasm_mtime;

                if !existing.manifest.enabled {
                    if existing.runtime.is_some() {
                        let _ = Self::deactivate_record(&mut existing); // Error OK: deactivation failure is non-fatal
                    }
                    existing.user_unloaded = false;
                    existing.state = ExtensionState::Unloaded;
                    existing.status_text = "Disabled in manifest".to_string();
                    existing.commands.clear();
                    existing.views.clear();
                    existing.overlays.clear();
                    existing.tick_handlers.clear();
                    existing.explorer_context_commands.clear();
                    existing.binary_open_commands.clear();
                    existing.file_open_commands.clear();
                    existing.requested_open_view = None;
                    existing.last_error = None;
                } else if existing.user_unloaded {
                    if existing.runtime.is_some() {
                        let _ = Self::deactivate_record(&mut existing); // Error OK: deactivation failure is non-fatal
                    }
                    existing.state = ExtensionState::Unloaded;
                    existing.status_text = "Unloaded by user".to_string();
                    existing.commands.clear();
                    existing.views.clear();
                    existing.overlays.clear();
                    existing.tick_handlers.clear();
                    existing.explorer_context_commands.clear();
                    existing.binary_open_commands.clear();
                    existing.file_open_commands.clear();
                    existing.requested_open_view = None;
                    existing.last_error = None;
                } else if changed
                    || (existing.runtime.is_none()
                        && !(existing.manifest.main.is_empty()
                            && existing.state == ExtensionState::Loaded))
                    || existing.state != ExtensionState::Loaded
                {
                    if existing.runtime.is_some() {
                        let _ = Self::deactivate_record(&mut existing); // Error OK: deactivation failure is non-fatal
                    }
                    match Self::activate_record(&self.engine, &mut existing, &self.settings_file) {
                        Ok(()) => {
                            notices.push(format!("Loaded extension: {}", existing.manifest.name))
                        }
                        Err(err) => {
                            let ext_id = existing.manifest.id.clone();
                            Self::set_record_error(&mut existing, &err, "Load failed");
                            notices.push(format!("Extension load failed ({ext_id}): {err}"));
                        }
                    }
                }
                next_records.push(existing);
            } else {
                let mut record = ExtensionRecord {
                    manifest: found.manifest,
                    manifest_path: found.manifest_path,
                    wasm_path: found.wasm_path,
                    user_unloaded: false,
                    state: ExtensionState::Unloaded,
                    status_text: "Discovered".to_string(),
                    commands: Vec::new(),
                    views: HashMap::new(),
                    explorer_context_commands: Vec::new(),
                    binary_open_commands: Vec::new(),
                    file_open_commands: Vec::new(),
                    requested_open_view: None,
                    logs: Vec::new(),
                    last_error: None,
                    overlays: HashMap::new(),
                    tick_handlers: Vec::new(),
                    manifest_mtime: found.manifest_mtime,
                    wasm_mtime: found.wasm_mtime,
                    runtime: None,
                    text_edit_requests: Vec::new(),
                    published_diagnostics: HashMap::new(),
                    diagnostics_version: 0,
                };
                if record.manifest.enabled {
                    match Self::activate_record(&self.engine, &mut record, &self.settings_file) {
                        Ok(()) => {
                            notices.push(format!("Loaded extension: {}", record.manifest.name))
                        }
                        Err(err) => {
                            let ext_id = record.manifest.id.clone();
                            Self::set_record_error(&mut record, &err, "Load failed");
                            notices.push(format!("Extension load failed ({ext_id}): {err}"));
                        }
                    }
                } else {
                    record.status_text = "Disabled in manifest".to_string();
                }
                next_records.push(record);
            }
        }

        for (_, mut removed) in old_records {
            if Self::record_has_unsaved_documents(&removed) {
                notices.push(format!(
                    "Kept {} loaded: save or close its modified documents before removing it",
                    removed.manifest.name
                ));
                next_records.push(removed);
                continue;
            }
            let removed_name = removed.manifest.name.clone();
            let _ = Self::deactivate_record(&mut removed); // Error OK: deactivation failure is non-fatal
            notices.push(format!("Unloaded removed extension: {removed_name}"));
        }

        next_records.sort_by_cached_key(|r| r.manifest.name.to_lowercase());
        self.records = next_records;
        self.rebuild_index();
        // Discovery may replace a guest and register a fresh view at the same
        // terminal size. Let that view receive its initial viewport request.
        self.view_resize_requests.clear();
        self.tick_yield_to_input
            .retain(|id| self.index_by_id.contains_key(id));
        notices
    }

    /// Return whether applying a discovery snapshot could change extension
    /// runtime or UI state. The periodic scanner usually returns an identical
    /// snapshot; rebuilding records, commands, and keybindings for that case
    /// only burns CPU and used to provoke an idle redraw every second.
    pub(crate) fn discovery_requires_sync(
        &self,
        discovered: &[extension_manifest::DiscoveredExtension],
    ) -> bool {
        if discovered.len() != self.records.len() {
            return true;
        }

        discovered.iter().any(|found| {
            let Some(existing) = self
                .records
                .iter()
                .find(|record| record.manifest.id == found.manifest.id)
            else {
                return true;
            };
            if extension_manifest::record_changed(
                &existing.manifest_path,
                &existing.wasm_path,
                &existing.manifest,
                existing.manifest_mtime,
                existing.wasm_mtime,
                found,
            ) {
                return true;
            }

            // Stable disabled, manually unloaded, and failed extensions stay
            // untouched until their files change or the user explicitly
            // retries them. Only repair an internally inconsistent loaded
            // record whose declared runtime disappeared.
            existing.state == ExtensionState::Loaded
                && existing.runtime.is_none()
                && !found.manifest.main.is_empty()
        })
    }

    pub(crate) fn scan_roots(&self) -> Vec<PathBuf> {
        self.search_roots.clone()
    }

    pub(crate) fn path_may_affect_discovery(&self, path: &Path) -> bool {
        self.search_roots
            .iter()
            .any(|root| path.starts_with(root) || root.starts_with(path))
    }

    pub(crate) fn has_busy_runtimes(&self) -> bool {
        !self.busy_extension_ids.is_empty()
    }

    pub(crate) fn views(&self) -> Vec<ExtensionRecordView> {
        self.records
            .iter()
            .map(|record| ExtensionRecordView {
                id: record.manifest.id.clone(),
                name: record.manifest.name.clone(),
                version: record.manifest.version.clone(),
                repository: record.manifest.repository.clone(),
                description: record.manifest.description.clone(),
                state: record.state,
                status_text: record.status_text.clone(),
                commands: record.commands.clone(),
                views: record.views.clone(),
                _overlays: record.overlays.clone(),
                last_error: record.last_error.clone(),
                logs: record.logs.clone(),
                enabled: record.manifest.enabled,
            })
            .collect()
    }

    /// Get LSP server configs from all enabled extensions, with bundle-resolved commands.
    pub(crate) fn lsp_configs(&self) -> Vec<crate::lsp::LspServerConfig> {
        self.records
            .iter()
            .filter(|r| r.manifest.enabled)
            .filter_map(|r| {
                let mut config = r.manifest.lsp.clone()?;
                let ext_dir = r.manifest_path.parent()?;
                let bundle = r.manifest.bundle.as_ref();

                // Check for bundled nodeCommand (node binary included in extension)
                if let Some(node_path) = crate::extension_manifest::resolve_bundled_command(
                    ext_dir,
                    bundle,
                    crate::extension_manifest::BundleField::NodeCommand,
                ) {
                    let _ = crate::extension_manifest::ensure_executable(&node_path);
                    let server_js = crate::extension_manifest::resolve_bundled_command(
                        ext_dir,
                        bundle,
                        crate::extension_manifest::BundleField::Command,
                    )?;
                    config.command = node_path.to_string_lossy().into_owned();
                    let prepend = crate::extension_manifest::bundled_prepend_args(bundle);
                    let mut new_args = vec![server_js.to_string_lossy().into_owned()];
                    new_args.extend(prepend);
                    config.args = new_args;
                    return Some(config);
                }

                // Resolve command from bundle
                if let Some(resolved) = crate::extension_manifest::resolve_bundled_command(
                    ext_dir,
                    bundle,
                    crate::extension_manifest::BundleField::Command,
                ) {
                    let _ = crate::extension_manifest::ensure_executable(&resolved);
                    let path_str = resolved.to_string_lossy().into_owned();
                    let prepend = crate::extension_manifest::bundled_prepend_args(bundle);

                    // JS/MJS files need node from PATH to run
                    let is_js = path_str.ends_with(".js") || path_str.ends_with(".mjs");
                    if is_js {
                        config.command = "node".to_string();
                        let mut new_args = vec![path_str];
                        new_args.extend(prepend);
                        config.args = new_args;
                    } else {
                        config.command = path_str;
                        if !prepend.is_empty() {
                            let mut new_args = prepend;
                            new_args.extend(config.args);
                            config.args = new_args;
                        }
                    }
                }
                Some(config)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn error_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| record.state == ExtensionState::Error)
            .count()
    }

    pub(crate) fn get_view(&self, extension_id: &str, view_id: &str) -> Option<ExtensionView> {
        let index = self.index_by_id.get(extension_id).copied()?;
        let record = self.records.get(index)?;
        record.views.get(view_id).cloned()
    }

    #[cfg(test)]
    pub(crate) fn all_overlays(&self) -> Vec<(String, ExtensionOverlay)> {
        self.overlays().map(|(id, overlay)| (id.to_owned(), overlay.clone())).collect()
    }

    pub(crate) fn overlays(&self) -> impl Iterator<Item = (&str, &ExtensionOverlay)> {
        self.records.iter().filter(|record| record.state == ExtensionState::Loaded)
            .flat_map(|record| record.overlays.values().map(|overlay| (record.manifest.id.as_str(), overlay)))
    }

    pub(crate) fn overlay_targets(&self) -> Vec<(String, OverlayTarget)> {
        self.overlays().map(|(id, overlay)| (id.to_owned(), OverlayTarget {
            id: overlay.id.clone(), x: overlay.x, y: overlay.y,
            width: overlay.width, height: overlay.height,
            mouse_handler: overlay.mouse_handler.is_some(),
            key_handler: overlay.key_handler.is_some(),
        })).collect()
    }

    /// Borrow handlers and consult open tabs only for extensions with cosmetic
    /// view ticks. Empty managers and background-only tasks never scan tabs.
    pub(crate) fn tick_handlers<'a>(
        &'a self,
        mut has_open_view: impl FnMut(&str) -> bool + 'a,
    ) -> impl Iterator<Item = (&'a str, &'a ExtensionTickHandler)> + 'a {
        self.records.iter()
            .filter(|record| record.state == ExtensionState::Loaded && !record.tick_handlers.is_empty())
            .flat_map(move |record| {
                let visible = record.views.is_empty()
                    || record.tick_handlers.iter().all(|handler| handler.background)
                    || has_open_view(&record.manifest.id);
                record.tick_handlers.iter().filter(move |handler| handler.background || visible)
                    .map(|handler| (record.manifest.id.as_str(), handler))
            })
    }

    /// Cosmetic ticks require an open view. Explicit background tasks continue
    /// until unregistered, including final cleanup after a view closes.
    #[cfg(test)]
    pub(crate) fn visible_tick_handlers(
        &self,
        visible_ext_ids: &rustc_hash::FxHashSet<String>,
    ) -> Vec<(String, ExtensionTickHandler)> {
        let mut result = Vec::new();
        for record in &self.records {
            if record.state != ExtensionState::Loaded {
                continue;
            }
            if record.tick_handlers.is_empty() {
                continue;
            }
            let has_views = !record.views.is_empty();
            for handler in &record.tick_handlers {
                if !handler.background
                    && has_views
                    && !visible_ext_ids.contains(&record.manifest.id)
                {
                    continue;
                }
                result.push((record.manifest.id.clone(), handler.clone()));
            }
        }
        result
    }

    pub(crate) fn extension_diagnostics_for_file(
        &self,
        file_path: &str,
    ) -> Vec<ExtensionDiagnostic> {
        let mut result = Vec::new();
        for record in &self.records {
            if record.state != ExtensionState::Loaded {
                continue;
            }
            if let Some(diags) = record.published_diagnostics.get(file_path) {
                result.extend(diags.iter().cloned());
            }
        }
        result
    }

    /// Cheap aggregate revision for render caches. Each record increments its
    /// revision only when published diagnostics change.
    pub(crate) fn diagnostics_version(&self) -> u64 {
        self.records
            .iter()
            .fold(self.records.len() as u64, |version, record| {
                version.rotate_left(7)
                    ^ record
                        .diagnostics_version
                        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            })
    }

    pub(crate) fn explorer_context_commands(&self) -> Vec<ExtensionContextCommand> {
        let mut commands = Vec::new();
        for record in &self.records {
            if record.state != ExtensionState::Loaded {
                continue;
            }
            for (command_id, title) in &record.explorer_context_commands {
                commands.push(ExtensionContextCommand {
                    extension_id: record.manifest.id.clone(),
                    command_id: command_id.clone(),
                    title: title.clone(),
                });
            }
        }
        commands
    }

    pub(crate) fn binary_open_commands(&self) -> Vec<ExtensionContextCommand> {
        let mut commands = Vec::new();
        for record in &self.records {
            if record.state != ExtensionState::Loaded {
                continue;
            }
            for (command_id, title) in &record.binary_open_commands {
                commands.push(ExtensionContextCommand {
                    extension_id: record.manifest.id.clone(),
                    command_id: command_id.clone(),
                    title: title.clone(),
                });
            }
        }
        commands
    }

    pub(crate) fn file_open_commands_for_path(&self, path: &Path) -> Vec<ExtensionContextCommand> {
        let Some(extension) = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
        else {
            return Vec::new();
        };

        let mut commands = Vec::new();
        for record in &self.records {
            if record.state != ExtensionState::Loaded {
                continue;
            }
            for (command_id, title, extensions) in &record.file_open_commands {
                if extensions.iter().any(|registered| registered == &extension) {
                    commands.push(ExtensionContextCommand {
                        extension_id: record.manifest.id.clone(),
                        command_id: command_id.clone(),
                        title: title.clone(),
                    });
                }
            }
        }
        commands
    }

    // Lifecycle methods (reload_all, reload_extension, unload_extension, load_extension,
    // activate_record, deactivate_record, set_record_error, pull_runtime_state)
    // are in extension_lifecycle.rs

    fn rebuild_index(&mut self) {
        self.index_by_id.clear();
        for (index, record) in self.records.iter().enumerate() {
            self.index_by_id.insert(record.manifest.id.clone(), index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ExtensionManager;
    use crate::extension_manifest::{EditorContext, ExtensionManifest, ExtensionState};
    use crate::extension_wasm::{
        ExtensionRecord, HostState, apply_invocation_context, clear_invocation_context,
        collect_workspace_file_bytes, sanitize_contributed_views,
    };
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_project_root() -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        path.push(format!("vtcode-ext-test-{}-{}", std::process::id(), stamp));
        path
    }

    #[test]
    fn marks_extension_error_when_wasm_missing() {
        let project_root = unique_temp_project_root();
        let extension_dir = project_root.join("extensions").join("broken-ext");
        std::fs::create_dir_all(&extension_dir).expect("create test extension dir");

        let manifest = r#"{
  "id": "test.broken",
  "name": "Broken Extension",
  "version": "0.1.0",
  "description": "missing wasm",
  "repository": "https://github.com/im-henri/vtcode",
  "apiVersion": "1",
  "main": "missing.wasm",
  "enabled": true
}"#;
        std::fs::write(extension_dir.join("extension.json"), manifest)
            .expect("write test manifest");

        let mut manager = ExtensionManager::new(&project_root);
        let notices = manager.scan_and_sync();
        assert!(notices.iter().any(|line| line.contains("test.broken")));
        assert_eq!(manager.error_count(), 1);
        let views = manager.views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].state, ExtensionState::Error);
        assert!(
            views[0]
                .last_error
                .as_ref()
                .is_some_and(|err| err.contains("WASM entry not found"))
        );

        let _ = std::fs::remove_dir_all(project_root); // Error OK: dir may not exist
    }

    #[test]
    fn scan_does_not_create_empty_extension_roots() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(&project_root).expect("create test project root");

        let mut manager = ExtensionManager::new(&project_root);
        let notices = manager.scan_and_sync();

        assert!(notices.is_empty());
        assert!(!project_root.join("extensions").exists());
        assert!(!project_root.join(".vtcode").exists());

        let _ = std::fs::remove_dir_all(project_root); // Error OK: dir may not exist
    }

    #[test]
    fn wasm_command_round_trip_with_host_imports_and_settings() {
        let temporary = tempfile::tempdir().unwrap();
        let project_root = temporary.path().join("workspace with spaces 世界");
        let extension_dir = project_root.join("extensions/test-probe");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::write(
            extension_dir.join("extension.json"),
            r#"{
            "id":"test.probe", "name":"Host Probe", "version":"0.1.0",
            "repository": "https://github.com/im-henri/vtcode",
  "apiVersion":"1", "main":"probe.wat", "enabled":true
        }"#,
        )
        .unwrap();
        // Wasmtime accepts WAT as well as binary WASM. This fixture exercises
        // actual guest memory and host imports without an external compiler.
        std::fs::write(
            extension_dir.join("probe.wat"),
            r#"(module
            (import "vtcode" "host_register_command" (func $register (param i32 i32 i32 i32)))
            (import "vtcode" "host_set_status" (func $status (param i32 i32)))
            (import "vtcode" "host_log" (func $log (param i32 i32)))
            (import "vtcode" "host_save_setting" (func $save (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "probe")
            (data (i32.const 16) "Host Probe")
            (data (i32.const 32) "activated")
            (data (i32.const 48) "ran")
            (data (i32.const 64) "lastRun")
            (func (export "vtcode_activate") (result i32)
                i32.const 0 i32.const 5 i32.const 16 i32.const 10 call $register
                i32.const 32 i32.const 9 call $status
                i32.const 0)
            (func (export "vtcode_run_command") (param i32) (result i32)
                i32.const 48 i32.const 3 call $status
                i32.const 48 i32.const 3 call $log
                i32.const 64 i32.const 7 i32.const 48 i32.const 3 call $save)
        )"#,
        )
        .unwrap();
        let mut app = crate::app::App::new(project_root);
        app.extension_manager.scan_and_sync();
        let views = app.extension_manager.views();
        assert_eq!(views.len(), 1, "{views:?}");
        assert_eq!(views[0].state, ExtensionState::Loaded, "{views:?}");
        assert_eq!(views[0].status_text, "activated");
        assert_eq!(views[0].commands[0].id, "probe");
        app.extension_manager
            .invoke_command_async("test.probe", "probe", &mut app.background, None, None)
            .unwrap();
        app.wait_for_background(std::time::Duration::from_secs(10), |app| {
            !app.extension_manager
                .busy_extension_ids
                .contains("test.probe")
                && !app.background.has_active_jobs()
                && !app.background.has_pending_results()
        });
        let views = app.extension_manager.views();
        assert_eq!(views[0].status_text, "ran");
        assert!(views[0].logs.iter().any(|line| line == "ran"));
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&app.extension_manager.settings_file).unwrap())
                .unwrap();
        assert_eq!(settings["test.probe"]["lastRun"], "ran");
        app.extension_manager.unload_extension(0).unwrap();
        assert_eq!(
            app.extension_manager.views()[0].state,
            ExtensionState::Unloaded
        );
        app.extension_manager.reload_extension(0).unwrap();
        assert_eq!(app.extension_manager.views()[0].status_text, "activated");
    }

    #[test]
    fn manually_unloaded_extension_stays_unloaded_on_rescan() {
        let project_root = unique_temp_project_root();
        let extension_dir = project_root.join("extensions").join("test-sample");
        std::fs::create_dir_all(&extension_dir).expect("create test extension dir");

        let copied_wasm = extension_dir.join("sample.wasm");
        // Minimal module exporting `vtcode_activate: () -> i32`. Keeping the
        // fixture inline avoids coupling core tests to any bundled extension.
        let minimal_extension_wasm = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
            0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type section
            0x03, 0x02, 0x01, 0x00, // function section
            0x07, 0x13, 0x01, 0x0f, b'v', b't', b'c', b'o', b'd', b'e', b'_', b'a', b'c', b't',
            b'i', b'v', b'a', b't', b'e', 0x00, 0x00, // export section
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0b, // code section
        ];
        std::fs::write(&copied_wasm, minimal_extension_wasm).expect("write sample wasm");

        let manifest = r#"{
  "id": "test.sample",
  "name": "Test Sample",
  "version": "0.1.0",
  "description": "test extension",
  "repository": "https://github.com/im-henri/vtcode",
  "apiVersion": "1",
  "main": "sample.wasm",
  "enabled": true
}"#;
        std::fs::write(extension_dir.join("extension.json"), manifest)
            .expect("write test manifest");

        let mut manager = ExtensionManager::new(&project_root);
        let _ = manager.scan_and_sync();
        assert_eq!(manager.views()[0].state, ExtensionState::Loaded);

        manager.unload_extension(0).expect("unload extension");
        assert_eq!(manager.views()[0].state, ExtensionState::Unloaded);

        let _ = manager.scan_and_sync();
        assert_eq!(manager.views()[0].state, ExtensionState::Unloaded);

        let _ = std::fs::remove_dir_all(project_root); // Error OK: dir may not exist
    }

    #[test]
    fn marks_extension_error_when_api_version_is_unsupported() {
        let project_root = unique_temp_project_root();
        let extension_dir = project_root.join("extensions").join("future-ext");
        std::fs::create_dir_all(&extension_dir).expect("create test extension dir");

        let manifest = r#"{
  "id": "test.future",
  "name": "Future Extension",
  "version": "0.1.0",
  "description": "unsupported api version",
  "repository": "https://github.com/im-henri/vtcode",
  "apiVersion": "999",
  "main": "missing.wasm",
  "enabled": true
}"#;
        std::fs::write(extension_dir.join("extension.json"), manifest)
            .expect("write test manifest");

        let mut manager = ExtensionManager::new(&project_root);
        let notices = manager.scan_and_sync();
        assert!(notices.iter().any(|line| line.contains("test.future")));
        assert_eq!(manager.error_count(), 1);
        let views = manager.views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].state, ExtensionState::Error);
        assert!(
            views[0]
                .last_error
                .as_ref()
                .is_some_and(|err| err.contains("Unsupported apiVersion"))
        );

        let _ = std::fs::remove_dir_all(project_root); // Error OK: dir may not exist
    }

    #[test]
    fn sanitize_contributed_views_empty() {
        let result = sanitize_contributed_views(&HashMap::new());
        assert!(result.is_empty());
    }

    #[test]
    fn sanitize_contributed_views_filters_invalid() {
        use crate::extension_manifest::ExtensionView;
        let mut views = HashMap::new();
        views.insert(
            "".to_string(),
            ExtensionView {
                id: "".to_string(),
                title: "Good".to_string(),
                content: "c".to_string(),
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
            },
        );
        views.insert(
            "ok".to_string(),
            ExtensionView {
                id: "ok".to_string(),
                title: "  ".to_string(),
                content: "c".to_string(),
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
            },
        );
        views.insert(
            "valid".to_string(),
            ExtensionView {
                id: "valid".to_string(),
                title: "Title".to_string(),
                content: "c".to_string(),
                key_handler: Some("key".to_string()),
                mouse_handler: None,
                scroll_handler: None,

                resize_handler: None,
                close_handler: None,
                hover_handler: None,
                context_menu_handler: None,
                pointer_capture: false,
                input_pending: false,

                frame: None,
            },
        );
        let result = sanitize_contributed_views(&views);
        assert_eq!(result.len(), 1);
        let valid = result.get("valid").expect("valid view should exist");
        assert_eq!(valid.id, "valid");
        assert_eq!(valid.key_handler.as_deref(), Some("key"));
    }

    #[test]
    fn sanitize_contributed_views_dedup_latest_wins() {
        use crate::extension_manifest::ExtensionView;
        let mut views = HashMap::new();
        views.insert(
            "v1".to_string(),
            ExtensionView {
                id: "v1".to_string(),
                title: "Second".to_string(),
                content: "new".to_string(),
                key_handler: Some("new_key".to_string()),
                mouse_handler: Some("mouse".to_string()),
                scroll_handler: Some("scroll".to_string()),
                resize_handler: None,
                close_handler: None,
                hover_handler: None,
                context_menu_handler: None,
                pointer_capture: false,
                input_pending: false,
                frame: None,
            },
        );
        let result = sanitize_contributed_views(&views);
        assert_eq!(result.len(), 1);
        let v1 = result.get("v1").expect("v1 view should exist");
        assert_eq!(v1.title, "Second");
        assert_eq!(v1.content, "new");
        assert_eq!(v1.key_handler.as_deref(), Some("new_key"));
        assert_eq!(v1.mouse_handler.as_deref(), Some("mouse"));
        assert_eq!(v1.scroll_handler.as_deref(), Some("scroll"));
    }

    #[test]
    fn extension_state_label() {
        assert_eq!(ExtensionState::Loaded.label(), "Loaded");
        assert_eq!(ExtensionState::Unloaded.label(), "Unloaded");
        assert_eq!(ExtensionState::Error.label(), "Error");
    }

    #[test]
    fn file_open_commands_filter_by_extension_case_insensitive() {
        let project_root = unique_temp_project_root();
        let mut manager = ExtensionManager::new(&project_root);
        manager.records.push(ExtensionRecord {
            manifest: ExtensionManifest {
                id: "test.file-open".to_string(),
                name: "File Open Test".to_string(),
                version: "0.1.0".to_string(),
                repository: "https://github.com/im-henri/vtcode".into(),
                description: "test".to_string(),
                main: "test.wasm".to_string(),
                enabled: true,
                api_version: "1".to_string(),
                lsp: None,
                bundle: None,
            },
            manifest_path: project_root
                .join("extensions")
                .join("test")
                .join("extension.json"),
            wasm_path: project_root
                .join("extensions")
                .join("test")
                .join("test.wasm"),
            user_unloaded: false,
            state: ExtensionState::Loaded,
            status_text: "Loaded".to_string(),
            commands: Vec::new(),
            views: HashMap::new(),
            explorer_context_commands: Vec::new(),
            binary_open_commands: Vec::new(),
            file_open_commands: vec![(
                "test.open".to_string(),
                "Open Test File".to_string(),
                vec!["vcd".to_string()],
            )],
            requested_open_view: None,
            logs: Vec::new(),
            last_error: None,
            overlays: HashMap::new(),
            tick_handlers: Vec::new(),
            manifest_mtime: None,
            wasm_mtime: None,
            runtime: None,
            text_edit_requests: Vec::new(),
            published_diagnostics: HashMap::new(),
            diagnostics_version: 0,
        });
        manager.rebuild_index();

        let matching = manager.file_open_commands_for_path(Path::new("trace.VCD"));
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].command_id, "test.open");

        let non_matching = manager.file_open_commands_for_path(Path::new("trace.txt"));
        assert!(non_matching.is_empty());

        let _ = std::fs::remove_dir_all(project_root); // Error OK: dir may not exist
    }

    #[test]
    fn apply_invocation_context_populates_editor_fields() {
        let mut host = HostState::default();

        apply_invocation_context(
            &mut host,
            Some("C:\\temp\\ctx.txt".to_string()),
            Some(vec![1, 2, 3]),
            Some(EditorContext {
                remote_workspace: None,
                active_file_path: Some("C:\\temp\\active.txt".to_string()),
                active_buffer: Some(crate::extension_buffer::Buffer::from_rope(
                    &ropey::Rope::from_str("hello"),
                )),
                active_modified: false,
                modified_file_paths: Vec::new(),
                cursor_line: Some(3),
                cursor_col: Some(7),
                selection_range: Some((1, 2, 3, 4)),
                workspace_root: Some("C:\\temp".to_string()),
            }),
        );

        assert_eq!(
            host.command_context_path.as_deref(),
            Some("C:\\temp\\ctx.txt")
        );
        assert_eq!(host.command_context_bytes.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(
            host.active_file_path.as_deref(),
            Some("C:\\temp\\active.txt")
        );
        let buffer = host.active_buffer.as_ref().unwrap();
        assert_eq!(buffer.bytes(), b"hello");
        assert_eq!(host.cursor_line, 3);
        assert_eq!(host.cursor_col, 7);
        assert_eq!(
            (
                host.selection_start_line,
                host.selection_start_col,
                host.selection_end_line,
                host.selection_end_col,
            ),
            (1, 2, 3, 4)
        );
        assert_eq!(host.workspace_root.as_deref(), Some("C:\\temp"));

        clear_invocation_context(&mut host);
        assert!(host.command_context_path.is_none());
        assert!(host.command_context_bytes.is_none());
        assert!(host.active_file_path.is_none());
        assert!(host.active_buffer.is_none());
        assert_eq!(host.cursor_line, -1);
        assert_eq!(host.cursor_col, -1);
        assert_eq!(host.selection_start_line, -1);
        assert_eq!(host.selection_end_col, -1);
        assert!(host.workspace_root.is_none());
    }

    #[test]
    fn collect_workspace_file_bytes_skips_hidden_and_build_dirs() {
        let project_root = unique_temp_project_root();
        let visible_dir = project_root.join("src");
        let hidden_dir = project_root.join(".git");
        let target_dir = project_root.join("target");
        std::fs::create_dir_all(&visible_dir).expect("create visible dir");
        std::fs::create_dir_all(&hidden_dir).expect("create hidden dir");
        std::fs::create_dir_all(&target_dir).expect("create target dir");
        std::fs::write(visible_dir.join("main.rs"), "fn main() {}").expect("write main.rs");
        std::fs::write(project_root.join("README.md"), "hello").expect("write README.md");
        std::fs::write(hidden_dir.join("config"), "hidden").expect("write hidden config");
        std::fs::write(target_dir.join("artifact"), "skip").expect("write target artifact");

        let payload = String::from_utf8(collect_workspace_file_bytes(&project_root))
            .expect("workspace files should be valid UTF-8");
        let files: Vec<&str> = payload.lines().collect();
        let expected_nested = Path::new("src")
            .join("main.rs")
            .to_string_lossy()
            .into_owned();

        assert_eq!(files, vec!["README.md", expected_nested.as_str()]);

        let _ = std::fs::remove_dir_all(project_root); // Error OK: dir may not exist
    }
}
