//! Extension lifecycle management.
//!
//! Contains methods for loading, unloading, reloading, and scanning extensions.
//! Extracted from `extensions.rs`.

use std::path::Path;

use wasmtime::{Engine, Linker, Store};

use crate::extension_manifest::{
    ExtensionCommand, ExtensionDiagnostic, ExtensionState, SUPPORTED_API_VERSION, TextEditRequest,
};
use crate::extension_wasm::{
    ExtensionRecord, ExtensionRuntime, HostState, MAX_LOG_ENTRIES, call_noarg_export,
    clear_invocation_context, sanitize_contributed_views,
};
use crate::extensions::ExtensionManager;

impl ExtensionManager {
    pub(crate) fn record_has_unsaved_documents(record: &ExtensionRecord) -> bool {
        record
            .views
            .values()
            .any(|view| view.frame.as_ref().is_some_and(|frame| frame.dirty))
            || record.runtime.as_ref().is_some_and(|runtime| {
                runtime
                    .store
                    .data()
                    .documents
                    .iter()
                    .any(|document| document.info().dirty)
            })
    }

    pub(crate) fn has_unsaved_documents(&self) -> bool {
        self.has_busy_runtimes()
            || !self.pending_view_events.is_empty()
            || self.records.iter().any(Self::record_has_unsaved_documents)
    }

    pub(crate) fn reload_all(&mut self) -> Vec<String> {
        let mut notices = Vec::new();
        for index in 0..self.records.len() {
            if !self.records[index].manifest.enabled || self.records[index].user_unloaded {
                continue;
            }
            match self.reload_extension(index) {
                Ok(msg) => notices.push(msg),
                Err(err) => notices.push(err),
            }
        }
        notices
    }

    pub(crate) fn reload_extension(&mut self, index: usize) -> Result<String, String> {
        if self.record_is_busy(index) {
            return Err("Save or close modified extension documents and wait for pending operations before reloading".into());
        }
        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| "No extension selected".to_string())?;
        record.user_unloaded = false;
        if record.runtime.is_some() {
            let _ = Self::deactivate_record(record); // Error OK: deactivation failure is non-fatal
        }
        let id = record.manifest.id.clone();
        let result = match Self::activate_record(&self.engine, record, &self.settings_file) {
            Ok(()) => Ok(format!("Reloaded extension: {}", record.manifest.name)),
            Err(err) => {
                Self::set_record_error(record, &err, "Reload failed");
                Err(err)
            }
        };
        self.remember_disabled(&id, false, result)
    }

    pub(crate) fn unload_extension(&mut self, index: usize) -> Result<String, String> {
        if self.record_is_busy(index) {
            return Err("Save or close modified extension documents and wait for pending operations before unloading".into());
        }
        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| "No extension selected".to_string())?;
        record.user_unloaded = true;
        Self::deactivate_record(record)?;
        record.state = ExtensionState::Unloaded;
        record.status_text = "Unloaded by user".to_string();
        let message = format!("Unloaded extension: {}", record.manifest.name);
        let id = record.manifest.id.clone();
        self.remember_disabled(&id, true, Ok(message))
    }

    fn record_is_busy(&self, index: usize) -> bool {
        self.records.get(index).is_some_and(|record| {
            Self::record_has_unsaved_documents(record)
                || record.views.values().any(|view| view.input_pending)
                || self.busy_extension_ids.contains(&record.manifest.id)
                || self
                    .pending_view_events
                    .iter()
                    .any(|event| event.extension_id == record.manifest.id)
        })
    }

    /// Save the user's enable/disable choice so the next session keeps it.
    /// An enable whose activation fails still counts: the user asked for it.
    fn remember_disabled(
        &mut self,
        id: &str,
        disabled: bool,
        result: Result<String, String>,
    ) -> Result<String, String> {
        let changed = if disabled {
            self.disabled_ids.insert(id.to_owned())
        } else {
            self.disabled_ids.remove(id)
        };
        if !changed {
            return result;
        }
        match crate::extension_state::write_disabled(&self.state_file, &self.disabled_ids) {
            Ok(()) => result,
            Err(error) => {
                let note = format!(" (could not save this for the next session: {error})");
                result
                    .map(|message| message + &note)
                    .map_err(|message| message + &note)
            }
        }
    }

    /// The installed directory of an extension the editor manages, if any.
    /// Extensions in the project's own folders are project files and stay.
    pub(crate) fn removal_dir(&self, record: &ExtensionRecord) -> Option<std::path::PathBuf> {
        let dir = record.manifest_path.parent()?;
        let root = dir.parent()?;
        self.managed_roots
            .iter()
            .any(|managed| managed == root)
            .then(|| dir.to_path_buf())
    }

    /// Unload an editor-installed extension and delete its directory.
    pub(crate) fn remove_extension(&mut self, index: usize) -> Result<String, String> {
        if self.record_is_busy(index) {
            return Err("Save or close modified extension documents and wait for pending operations before removing".into());
        }
        let record = self
            .records
            .get(index)
            .ok_or_else(|| "No extension selected".to_string())?;
        let dir = self
            .removal_dir(record)
            .ok_or("Only extensions installed by VTCode can be removed here")?;
        let root = dir
            .parent()
            .ok_or("Invalid extension directory")?
            .to_path_buf();
        if std::fs::symlink_metadata(&dir)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("The installed extension directory cannot be a symlink.".into());
        }
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".install-lock"))
            .map_err(|error| error.to_string())?;
        let _lock = vtcode::file_source::ExclusiveFileLock::try_acquire(lock_file).map_err(
            |_| "An extension installation is running. Wait for it to finish, then retry.",
        )?;
        let staging = tempfile::Builder::new()
            .prefix(".remove-")
            .tempdir_in(&root)
            .map_err(|error| error.to_string())?;
        let mut record = self.records.remove(index);
        let name = record.manifest.name.clone();
        let id = record.manifest.id.clone();
        let _ = Self::deactivate_record(&mut record); // Error OK: the files are deleted next
        // Move the directory out of discovery's reach first (staged files sit
        // too deep to be found), so a partial delete never leaves half an
        // extension installed.
        if let Err(error) = std::fs::rename(&dir, staging.path().join("extension")) {
            // Still installed: keep it listed, unloaded until enabled again.
            record.state = ExtensionState::Unloaded;
            record.status_text = "Removal failed".to_string();
            record.user_unloaded = true;
            self.records.insert(index, record);
            self.rebuild_index();
            return Err(format!(
                "Could not remove {name}; close files using it and retry: {error}"
            ));
        }
        self.rebuild_index();
        self.view_resize_requests
            .retain(|(owner, _), _| owner != &id);
        self.tick_yield_to_input.remove(&id);
        drop(staging); // Deletes the moved files.
        self.remember_disabled(&id, false, Ok(format!("Removed extension: {name}")))
    }

    pub(crate) fn load_extension(&mut self, index: usize) -> Result<String, String> {
        if self
            .records
            .get(index)
            .is_some_and(|record| record.runtime.is_some())
        {
            return self.reload_extension(index);
        }
        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| "No extension selected".to_string())?;
        record.user_unloaded = false;
        let id = record.manifest.id.clone();
        let result = match Self::activate_record(&self.engine, record, &self.settings_file) {
            Ok(()) => Ok(format!("Loaded extension: {}", record.manifest.name)),
            Err(err) => {
                Self::set_record_error(record, &err, "Load failed");
                Err(err)
            }
        };
        self.remember_disabled(&id, false, result)
    }

    pub(crate) fn set_record_error(record: &mut ExtensionRecord, error: &str, status: &str) {
        record.runtime = None;
        record.commands.clear();
        record.views.clear();
        record.overlays.clear();
        record.tick_handlers.clear();
        record.explorer_context_commands.clear();
        record.binary_open_commands.clear();
        record.file_open_commands.clear();
        record.requested_open_view = None;
        record.state = ExtensionState::Error;
        record.status_text = status.to_string();
        record.last_error = Some(error.to_string());
    }

    pub(crate) fn activate_record(
        engine: &std::sync::OnceLock<Engine>,
        record: &mut ExtensionRecord,
        settings_file: &Path,
    ) -> Result<(), String> {
        // Validate bundled binaries — fail activation if declared but missing
        if let Some(ref bundle) = record.manifest.bundle {
            let platform = crate::extension_manifest::current_platform();
            if let Some(platform_bundle) = bundle.platforms.get(&platform) {
                if let Some(ext_dir) = record.manifest_path.parent() {
                    if let Some(ref cmd_path) = platform_bundle.command {
                        let resolved = ext_dir.join(cmd_path);
                        if !resolved.exists() {
                            let msg = format!(
                                "Bundled binary not found for {platform}: {}",
                                resolved.display()
                            );
                            Self::set_record_error(record, "Missing bundled binary", &msg);
                            return Err(msg);
                        }
                        let _ = crate::extension_manifest::ensure_executable(&resolved);
                    }
                    if let Some(ref node_path) = platform_bundle.node_command {
                        let resolved = ext_dir.join(node_path);
                        if !resolved.exists() {
                            let msg = format!(
                                "Bundled node binary not found for {platform}: {}",
                                resolved.display()
                            );
                            Self::set_record_error(record, "Missing bundled binary", &msg);
                            return Err(msg);
                        }
                        let _ = crate::extension_manifest::ensure_executable(&resolved);
                    }
                }
            } else {
                let msg = format!("No bundle available for platform {platform}");
                Self::set_record_error(record, "Unsupported platform", &msg);
                return Err(msg);
            }
        }

        // LSP-only extensions don't require WASM
        if record.manifest.lsp.is_some() && record.manifest.main.is_empty() {
            record.state = ExtensionState::Loaded;
            record.last_error = None;
            record.status_text = "LSP extension".to_string();
            return Ok(());
        }
        if !crate::extension_manifest::supports_api(&record.manifest.api_version) {
            return Err(format!(
                "Unsupported apiVersion `{}`; supported version is `{SUPPORTED_API_VERSION}`",
                record.manifest.api_version
            ));
        }
        if !record.wasm_path.exists() {
            return Err(format!(
                "WASM entry not found: {}",
                record.wasm_path.display()
            ));
        }

        // Empty workspaces and LSP-only extensions never need a Wasm engine.
        let engine = engine.get_or_init(|| {
            #[cfg(not(test))]
            {
                crate::extensions::new_extension_engine()
            }
            #[cfg(test)]
            {
                crate::extensions::test_extension_engine().clone()
            }
        });
        #[cfg(not(test))]
        let module = crate::extension_module_cache::load(engine, &record.wasm_path);
        #[cfg(test)]
        let module = std::fs::read(&record.wasm_path)
            .map_err(wasmtime::Error::from)
            .and_then(|bytes| crate::extensions::test_extension_module(engine, &bytes));
        let module = module.map_err(|err| format!("Failed compiling module: {err}"))?;
        let mut linker = Linker::new(engine);
        crate::extension_host::define_host_functions(&mut linker)?;

        let mut store = Store::new(
            engine,
            HostState {
                extension_id: record.manifest.id.clone(),
                settings_file_path: Some(settings_file.to_path_buf()),
                ..Default::default()
            },
        );
        store.limiter(|host| &mut host.limits);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|err| format!("Failed instantiating module: {err}"))?;

        let activate_code =
            call_noarg_export(&mut store, &instance, "vtcode_activate", true)?.unwrap_or(0);
        if activate_code != 0 {
            return Err(format!(
                "vtcode_activate returned error code {activate_code}"
            ));
        }

        let mut commands = Vec::new();
        for (idx, (id, title)) in store.data().commands.iter().enumerate() {
            if id.trim().is_empty() {
                continue;
            }
            commands.push(ExtensionCommand {
                id: id.clone(),
                title: title.clone(),
                runtime_index: idx as i32,
            });
        }

        record.commands = commands;
        record.views = sanitize_contributed_views(&store.data().views);
        record.overlays = store.data().overlays.clone();
        record.tick_handlers = store.data().tick_handlers.clone();
        record.explorer_context_commands = store.data().explorer_context_commands.clone();
        record.binary_open_commands = store.data().binary_open_commands.clone();
        record.file_open_commands = store.data().file_open_commands.clone();
        record.requested_open_view = None;
        record.state = ExtensionState::Loaded;
        record.last_error = None;
        record.status_text = store
            .data()
            .status
            .clone()
            .unwrap_or_else(|| "Loaded".to_string());
        record.runtime = Some(ExtensionRuntime { store, instance });
        Self::pull_runtime_state(record);
        Ok(())
    }

    pub(crate) fn deactivate_record(record: &mut ExtensionRecord) -> Result<(), String> {
        if Self::record_has_unsaved_documents(record) {
            return Err("Save or close modified extension documents before unloading".into());
        }
        if let Some(runtime) = record.runtime.as_mut() {
            // Error OK: deactivate callback is optional and may not be exported
            let _ = call_noarg_export(
                &mut runtime.store,
                &runtime.instance,
                "vtcode_deactivate",
                false,
            )?;
            record.status_text = "Unloaded".to_string();
        }
        record.runtime = None;
        record.commands.clear();
        record.views.clear();
        record.overlays.clear();
        record.tick_handlers.clear();
        record.explorer_context_commands.clear();
        record.binary_open_commands.clear();
        record.file_open_commands.clear();
        record.requested_open_view = None;
        if !record.published_diagnostics.is_empty() {
            record.published_diagnostics.clear();
            record.diagnostics_version = record.diagnostics_version.wrapping_add(1);
        }
        record.state = ExtensionState::Unloaded;
        Ok(())
    }

    pub(crate) fn pull_runtime_state(record: &mut ExtensionRecord) {
        let Some(runtime) = record.runtime.as_mut() else {
            return;
        };
        let (
            status,
            new_logs,
            views,
            overlays,
            tick_handlers,
            requested_close_overlays,
            explorer_context_commands,
            binary_open_commands,
            file_open_commands,
            requested_open_view,
            text_edit_requests,
            ext_diags,
            clear_diag_files,
        ) = {
            let host = runtime.store.data_mut();
            for view in host.views.values_mut() {
                if let Some(frame) = view.frame.as_mut() {
                    if let Some(document) = frame
                        .document
                        .and_then(|id| host.documents.get(id.wrapping_sub(1) as usize))
                    {
                        let info = document.info();
                        if frame.dirty != info.dirty || frame.path.as_ref() != Some(&info.path) {
                            let frame = std::sync::Arc::make_mut(frame);
                            frame.dirty = info.dirty;
                            frame.path = Some(info.path);
                        }
                    }
                }
            }
            let status = host.status.take();
            let mut new_logs = Vec::new();
            new_logs.append(&mut host.logs);
            let views = sanitize_contributed_views(&host.views);
            let overlays = host.overlays.clone();
            let tick_handlers = host.tick_handlers.clone();
            let requested_close_overlays = std::mem::take(&mut host.requested_close_overlays);
            let explorer_context_commands = host.explorer_context_commands.clone();
            let binary_open_commands = host.binary_open_commands.clone();
            let file_open_commands = host.file_open_commands.clone();
            let requested_open_view = host.requested_open_view.take();
            let text_edit_requests: Vec<TextEditRequest> =
                host.text_edit_requests.drain(..).collect();
            let ext_diags: Vec<ExtensionDiagnostic> =
                host.extension_diagnostics.drain(..).collect();
            let clear_diag_files: Vec<String> = host.clear_diagnostics_files.drain(..).collect();
            clear_invocation_context(host);
            (
                status,
                new_logs,
                views,
                overlays,
                tick_handlers,
                requested_close_overlays,
                explorer_context_commands,
                binary_open_commands,
                file_open_commands,
                requested_open_view,
                text_edit_requests,
                ext_diags,
                clear_diag_files,
            )
        };
        if let Some(status) = status {
            record.status_text = status;
        }
        record.views = views;
        record.overlays = overlays;
        record.tick_handlers = tick_handlers;
        for close_id in &requested_close_overlays {
            record.overlays.remove(close_id);
        }
        record.explorer_context_commands = explorer_context_commands;
        record.binary_open_commands = binary_open_commands;
        record.file_open_commands = file_open_commands;
        record.requested_open_view = requested_open_view;
        if !text_edit_requests.is_empty() {
            record.text_edit_requests.extend(text_edit_requests);
        }
        let diagnostics_changed = !clear_diag_files.is_empty() || !ext_diags.is_empty();
        for file in clear_diag_files {
            record.published_diagnostics.remove(&file);
        }
        for diag in ext_diags {
            record
                .published_diagnostics
                .entry(diag.file.clone())
                .or_default()
                .push(diag);
        }
        if diagnostics_changed {
            record.diagnostics_version = record.diagnostics_version.wrapping_add(1);
        }
        for line in new_logs {
            record.logs.push(line);
        }
        if record.logs.len() > MAX_LOG_ENTRIES {
            let drop_count = record.logs.len() - MAX_LOG_ENTRIES;
            record.logs.drain(0..drop_count);
        }
    }
}
