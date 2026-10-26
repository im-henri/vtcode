use std::path::Path;
use std::sync::Arc;

use crate::extension_manifest::{EditorContext, EditorContextSnapshot};
use crate::extension_wasm::{
    ExtensionRecord, ExtensionRuntime, ExtensionRuntimeBox, MAX_LOG_ENTRIES, ViewEventHandlerKind,
    apply_invocation_context, call_i32_arg_export, call_noarg_export, clear_invocation_context,
    sanitize_contributed_views,
};
use crate::extensions::{ExtensionManager, ExtensionState, TextEditRequest};

pub(crate) struct PendingViewEvent {
    pub(crate) extension_id: String,
    view_id: String,
    kind: ViewEventHandlerKind,
    payload: String,
    context: EditorContextSnapshot,
    clipboard: String,
    drag_motion: Option<[bool; 4]>,
    document_action: Option<DocumentWorkbenchAction>,
}

pub(crate) enum DocumentWorkbenchAction {
    Save,
    SaveForExit,
    KeepForExit,
    DiscardForExit,
    SaveClose,
    DiscardClose,
    SaveAs {
        path: std::path::PathBuf,
        replace: bool,
    },
}

impl ExtensionManager {
    /// Rich custom editors consume input immediately and execute their guest
    /// callbacks on workers. Queueing preserves every keystroke during saves.
    pub(crate) fn queue_view_event(
        &mut self,
        extension_id: &str,
        view_id: &str,
        kind: ViewEventHandlerKind,
        payload: String,
        context: EditorContextSnapshot,
        clipboard: String,
    ) -> bool {
        let Some(view) = self.get_view(extension_id, view_id) else {
            return false;
        };
        if view.frame.is_none() {
            return false;
        }
        let handler = match kind {
            ViewEventHandlerKind::Key => &view.key_handler,
            ViewEventHandlerKind::Mouse => &view.mouse_handler,
            ViewEventHandlerKind::Scroll => &view.scroll_handler,
            ViewEventHandlerKind::Resize => &view.resize_handler,
            ViewEventHandlerKind::Close => &view.close_handler,
            ViewEventHandlerKind::Hover => &view.hover_handler,
            ViewEventHandlerKind::ContextMenu => &view.context_menu_handler,
        };
        if handler.is_none() {
            return false;
        }
        let mut drag_motion = None;
        let payload = match serde_json::from_str::<serde_json::Value>(&payload) {
            Ok(mut value) => {
                if matches!(kind, ViewEventHandlerKind::Resize)
                    && let (Some(width), Some(height)) =
                        (value["width"].as_u64(), value["height"].as_u64())
                {
                    let key = (extension_id.to_owned(), view_id.to_owned());
                    // Remember delivered requests as well as queued ones. A
                    // guest may publish later or clamp its surface dimensions;
                    // neither is a new viewport resize.
                    if self.view_resize_requests.get(&key) == Some(&(width, height)) {
                        return true;
                    }
                    self.view_resize_requests.insert(key, (width, height));
                }
                if matches!(kind, ViewEventHandlerKind::Close) {
                    self.view_resize_requests
                        .remove(&(extension_id.to_owned(), view_id.to_owned()));
                }
                if view.pointer_capture
                    && matches!(kind, ViewEventHandlerKind::Mouse)
                    && value["kind"] == "drag"
                {
                    drag_motion = Some([
                        value["shift"] == true,
                        value["control"] == true,
                        value["alt"] == true,
                        value["pointerInside"] != false,
                    ]);
                }
                value["viewId"] = serde_json::Value::String(view_id.into());
                value.to_string()
            }
            Err(_) => return false,
        };
        if matches!(kind, ViewEventHandlerKind::Resize) {
            self.pending_view_events.retain(|event| {
                event.extension_id != extension_id
                    || event.view_id != view_id
                    || !matches!(event.kind, ViewEventHandlerKind::Resize)
            });
        }
        // Passive moves and opted-in captured drags keep the freshest position.
        // Press/release, modifiers, keys, scroll and other views are barriers.
        if self.pending_view_events.back().is_some_and(|event| {
            event.extension_id == extension_id
                && event.view_id == view_id
                && (matches!(kind, ViewEventHandlerKind::Hover)
                    && matches!(event.kind, ViewEventHandlerKind::Hover)
                    || drag_motion.is_some() && event.drag_motion == drag_motion)
        }) {
            self.pending_view_events.pop_back();
        }
        self.pending_view_events.push_back(PendingViewEvent {
            extension_id: extension_id.into(),
            view_id: view_id.into(),
            kind,
            payload,
            context,
            clipboard,
            drag_motion,
            document_action: None,
        });
        true
    }

    pub(crate) fn queue_document_action(
        &mut self,
        extension_id: &str,
        view_id: &str,
        action: DocumentWorkbenchAction,
        context: EditorContextSnapshot,
    ) -> bool {
        if !self
            .get_view(extension_id, view_id)
            .and_then(|view| view.frame)
            .is_some_and(|frame| frame.document.is_some())
        {
            return false;
        }
        if !self.queue_view_event(
            extension_id,
            view_id,
            ViewEventHandlerKind::Key,
            r#"{"code":"documentChanged"}"#.into(),
            context,
            String::new(),
        ) {
            return false;
        }
        self.pending_view_events
            .back_mut()
            .expect("queued event")
            .document_action = Some(action);
        true
    }

    pub(crate) fn view_input_pending(&self, extension_id: &str, view_id: &str) -> bool {
        self.busy_extension_ids.contains(extension_id)
            || self.get_view(extension_id, view_id).is_some_and(|view| view.input_pending)
            || self
                .pending_view_events
                .iter()
                .any(|event| event.extension_id == extension_id && event.view_id == view_id)
    }

    pub(crate) fn dispatch_view_events(
        &mut self,
        background: &mut crate::background::BackgroundWorkers,
    ) {
        let count = self.pending_view_events.len();
        for _ in 0..count {
            let Some(event) = self.pending_view_events.pop_front() else {
                break;
            };
            let Some(&index) = self.index_by_id.get(&event.extension_id) else {
                continue;
            };
            if self.busy_extension_ids.contains(&event.extension_id) {
                self.pending_view_events.push_back(event);
                continue;
            }
            let Some(record) = self.records.get_mut(index) else {
                continue;
            };
            let Some(view) = record.views.get(&event.view_id) else {
                continue;
            };
            let handler = match event.kind {
                ViewEventHandlerKind::Key => view.key_handler.clone(),
                ViewEventHandlerKind::Mouse => view.mouse_handler.clone(),
                ViewEventHandlerKind::Scroll => view.scroll_handler.clone(),
                ViewEventHandlerKind::Resize => view.resize_handler.clone(),
                ViewEventHandlerKind::Close => view.close_handler.clone(),
                ViewEventHandlerKind::Hover => view.hover_handler.clone(),
                ViewEventHandlerKind::ContextMenu => view.context_menu_handler.clone(),
            };
            let (Some(handler), Some(runtime)) = (handler, record.runtime.take()) else {
                continue;
            };
            self.tick_yield_to_input.remove(&event.extension_id);
            self.busy_extension_ids.insert(event.extension_id.clone());
            let sender = background.sender.clone();
            background.spawn("extension-input", move || {
                let mut runtime = ExtensionRuntimeBox::new(runtime);
                let guest = runtime.0.as_mut().expect("owned extension runtime");
                apply_invocation_context(
                    guest.store.data_mut(),
                    None,
                    None,
                    Some(event.context.into_context()),
                );
                guest.store.data_mut().current_view_event = Some(event.payload.into_bytes());
                guest.store.data_mut().clipboard_text = event.clipboard;
                let mut close_view = None;
                let mut exit_action = false;
                let workbench_action = event.document_action.is_some();
                let document_result = (|| -> Result<(), String> {
                    let Some(action) = event.document_action else {
                        return Ok(());
                    };
                    let id = guest
                        .store
                        .data()
                        .views
                        .get(&event.view_id)
                        .and_then(|view| view.frame.as_ref())
                        .and_then(|frame| frame.document)
                        .ok_or("Custom editor has no file document")?;
                    // Flush staged input before Save, Save As, recovery, or discard.
                    if let Some(code) = call_noarg_export(
                        &mut guest.store, &guest.instance, "vtcode_prepare_document_action", false,
                    )? && code != 0 {
                        return Err("The custom editor could not finish its pending edits".into());
                    }
                    let recovery_dir = guest
                        .store
                        .data()
                        .settings_file_path
                        .as_ref()
                        .map(|path| crate::extension_recovery::directory(path));
                    let opener = guest.store.data().document_openers.get(&id).cloned();
                    let extension_id = guest.store.data().extension_id.clone();
                    let save_path = match &action {
                        DocumentWorkbenchAction::SaveAs { path, .. } => Some(path.clone()),
                        DocumentWorkbenchAction::Save
                        | DocumentWorkbenchAction::SaveForExit
                        | DocumentWorkbenchAction::SaveClose => guest
                            .store
                            .data()
                            .documents
                            .get(id as usize - 1)
                            .map(|document| std::path::PathBuf::from(document.info().path)),
                        _ => None,
                    };
                    exit_action = matches!(
                        action,
                        DocumentWorkbenchAction::SaveForExit
                            | DocumentWorkbenchAction::KeepForExit
                            | DocumentWorkbenchAction::DiscardForExit
                    );
                    if let Some(path) = save_path {
                        crate::extension_editor_host::check_text_conflict(
                            guest.store.data(),
                            &path,
                        )?;
                    }
                    let document = guest
                        .store
                        .data_mut()
                        .documents
                        .get_mut(id as usize - 1)
                        .ok_or("Unknown custom editor document")?;
                    match action {
                        DocumentWorkbenchAction::Save => document.save()?,
                        DocumentWorkbenchAction::SaveForExit => {
                            exit_action = true;
                            document.save()?;
                        }
                        DocumentWorkbenchAction::KeepForExit => {
                            exit_action = true;
                            document.keep_for_session(
                                &recovery_dir.ok_or("Recovery storage is unavailable")?,
                                &extension_id,
                                &opener.ok_or("This document has no registered reopen command")?,
                                id,
                            )?;
                        }
                        DocumentWorkbenchAction::DiscardForExit => {
                            exit_action = true;
                            document.discard_recovery()?;
                        }
                        DocumentWorkbenchAction::SaveClose => {
                            document.save()?;
                            close_view = Some(event.view_id.clone());
                        }
                        DocumentWorkbenchAction::DiscardClose => {
                            document.reload()?;
                            close_view = Some(event.view_id.clone());
                        }
                        DocumentWorkbenchAction::SaveAs { path, replace } => {
                            document.save_as(&path, replace)?
                        }
                    }
                    Ok(())
                })();
                let render_result =
                    call_noarg_export(&mut guest.store, &guest.instance, &handler, true).and_then(
                        |code| match code {
                            Some(0 | 1) | None => Ok(()),
                            Some(code) => Err(format!("Custom editor handler returned {code}")),
                        },
                    );
                if document_result.is_err() || render_result.is_err() {
                    for document in &mut guest.store.data_mut().documents {
                        document.abort_transaction();
                    }
                    guest.store.data_mut().status = document_result.as_ref().err().or(render_result.as_ref().err()).cloned();
                }
                let result = if workbench_action {
                    document_result
                } else {
                    document_result.and(render_result)
                };
                let _ = sender.send(
                    crate::background::BackgroundResult::ExtensionViewEventDone {
                        extension_index: index,
                        extension_id: event.extension_id,
                        result,
                        runtime,
                        close_view,
                        exit_action,
                    },
                );
            });
        }
    }

    pub(crate) fn take_clipboard_writes(&mut self) -> Vec<String> {
        self.records
            .iter_mut()
            .filter_map(|record| {
                record
                    .runtime
                    .as_mut()
                    .and_then(|runtime| runtime.store.data_mut().clipboard_write.take())
            })
            .collect()
    }
    pub(crate) fn invoke_overlay_mouse_handler(
        &mut self,
        extension_id: &str,
        overlay_id: &str,
        payload: &str,
        ctx: Option<EditorContext>,
    ) -> Result<Option<bool>, String> {
        let index = self
            .index_by_id
            .get(extension_id)
            .copied()
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let handler_export = {
            let record = self
                .records
                .get(index)
                .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
            if record.state != ExtensionState::Loaded || record.runtime.is_none() {
                return Err(format!("Extension is not loaded: {}", record.manifest.name));
            }
            let overlay = record
                .overlays
                .get(overlay_id)
                .ok_or_else(|| format!("Unknown extension overlay: {extension_id}/{overlay_id}"))?;
            overlay.mouse_handler.clone()
        };
        let Some(handler_export) = handler_export else {
            return Ok(None);
        };
        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let code = {
            let runtime = record
                .runtime
                .as_mut()
                .ok_or_else(|| format!("Extension is not loaded: {}", record.manifest.name))?;
            {
                let host = runtime.store.data_mut();
                apply_invocation_context(host, None, None, ctx);
                host.current_view_event = Some(payload.as_bytes().to_vec());
            }
            call_noarg_export(&mut runtime.store, &runtime.instance, &handler_export, true)?
                .unwrap_or(0)
        };
        Self::pull_runtime_state(record);
        match code {
            0 => Ok(Some(false)),
            1 => Ok(Some(true)),
            other => Err(format!(
                "Overlay mouse handler `{handler_export}` failed in extension {extension_id} with code {other}"
            )),
        }
    }

    pub(crate) fn invoke_overlay_key_handler(
        &mut self,
        extension_id: &str,
        overlay_id: &str,
        payload: &str,
        ctx: Option<EditorContext>,
    ) -> Result<Option<bool>, String> {
        let index = self
            .index_by_id
            .get(extension_id)
            .copied()
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let handler_export = {
            let record = self
                .records
                .get(index)
                .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
            if record.state != ExtensionState::Loaded || record.runtime.is_none() {
                return Err(format!("Extension is not loaded: {}", record.manifest.name));
            }
            let overlay = record
                .overlays
                .get(overlay_id)
                .ok_or_else(|| format!("Unknown extension overlay: {extension_id}/{overlay_id}"))?;
            overlay.key_handler.clone()
        };
        let Some(handler_export) = handler_export else {
            return Ok(None);
        };
        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let code = {
            let runtime = record
                .runtime
                .as_mut()
                .ok_or_else(|| format!("Extension is not loaded: {}", record.manifest.name))?;
            {
                let host = runtime.store.data_mut();
                apply_invocation_context(host, None, None, ctx);
                host.current_view_event = Some(payload.as_bytes().to_vec());
            }
            call_noarg_export(&mut runtime.store, &runtime.instance, &handler_export, true)?
                .unwrap_or(0)
        };
        Self::pull_runtime_state(record);
        match code {
            0 => Ok(Some(false)),
            1 => Ok(Some(true)),
            other => Err(format!(
                "Overlay key handler `{handler_export}` failed in extension {extension_id} with code {other}"
            )),
        }
    }

    pub(crate) fn close_overlay(&mut self, extension_id: &str, overlay_id: &str) {
        let Some(&index) = self.index_by_id.get(extension_id) else {
            return;
        };
        if let Some(record) = self.records.get_mut(index) {
            record.overlays.remove(overlay_id);
            if let Some(runtime) = record.runtime.as_mut() {
                runtime.store.data_mut().overlays.remove(overlay_id);
            }
        }
    }

    pub(crate) fn update_overlay_position(
        &mut self,
        extension_id: &str,
        overlay_id: &str,
        x: u16,
        y: u16,
    ) {
        let Some(&index) = self.index_by_id.get(extension_id) else {
            return;
        };
        if let Some(record) = self.records.get_mut(index) {
            if let Some(overlay) = record.overlays.get_mut(overlay_id) {
                overlay.x = x;
                overlay.y = y;
            }
            if let Some(runtime) = record.runtime.as_mut() {
                if let Some(overlay) = runtime.store.data_mut().overlays.get_mut(overlay_id) {
                    overlay.x = x;
                    overlay.y = y;
                }
            }
        }
    }

    pub(crate) fn invoke_view_key_handler(
        &mut self,
        extension_id: &str,
        view_id: &str,
        event_json: &str,
        editor_context: Option<EditorContext>,
    ) -> Result<Option<bool>, String> {
        self.invoke_view_event_handler(
            extension_id,
            view_id,
            ViewEventHandlerKind::Key,
            event_json,
            editor_context,
        )
    }

    pub(crate) fn invoke_view_mouse_handler(
        &mut self,
        extension_id: &str,
        view_id: &str,
        event_json: &str,
        editor_context: Option<EditorContext>,
    ) -> Result<Option<bool>, String> {
        self.invoke_view_event_handler(
            extension_id,
            view_id,
            ViewEventHandlerKind::Mouse,
            event_json,
            editor_context,
        )
    }

    pub(crate) fn invoke_view_scroll_handler(
        &mut self,
        extension_id: &str,
        view_id: &str,
        event_json: &str,
        editor_context: Option<EditorContext>,
    ) -> Result<Option<bool>, String> {
        self.invoke_view_event_handler(
            extension_id,
            view_id,
            ViewEventHandlerKind::Scroll,
            event_json,
            editor_context,
        )
    }

    pub(crate) fn take_requested_open_view(&mut self, extension_id: &str) -> Option<String> {
        let index = self.index_by_id.get(extension_id).copied()?;
        self.records
            .get_mut(index)
            .and_then(|record| record.requested_open_view.take())
    }

    pub(crate) fn take_pending_text_edits(&mut self) -> Vec<TextEditRequest> {
        let mut all = Vec::new();
        for record in &mut self.records {
            all.append(&mut record.text_edit_requests);
        }
        all
    }

    /// Run an extension command in a background thread.
    /// Takes the runtime out of the record, spawns a thread, and sends results back via channel.
    pub(crate) fn invoke_command_async(
        &mut self,
        extension_id: &str,
        command_id: &str,
        background: &mut crate::background::BackgroundWorkers,
        context_path: Option<std::path::PathBuf>,
        editor_context: Option<EditorContextSnapshot>,
    ) -> Result<(), String> {
        let index = self
            .index_by_id
            .get(extension_id)
            .copied()
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        if record.state != ExtensionState::Loaded || record.runtime.is_none() {
            return Err(format!("Extension is not loaded: {}", record.manifest.name));
        }
        let command = record
            .commands
            .iter()
            .find(|c| c.id == command_id)
            .cloned()
            .ok_or_else(|| format!("Unknown command: {command_id}"))?;

        // Take the runtime out - it will be returned via the channel
        let runtime = record
            .runtime
            .take()
            .ok_or_else(|| format!("Extension is not loaded: {}", record.manifest.name))?;

        let ext_id = record.manifest.id.clone();
        self.busy_extension_ids.insert(ext_id.clone());
        let cmd_title = command.title.clone();
        let project_root = self
            .search_roots
            .first()
            .and_then(|root| root.parent())
            .map(Path::to_path_buf);
        let sender = background.sender.clone();

        background.spawn("ext-command", move || {
            let mut runtime_box = ExtensionRuntimeBox::new(runtime);
            let result = if let Some(rt) = runtime_box.0.as_mut() {
                let resolved_context_path = context_path.map(|path| {
                    if path.is_absolute() {
                        path
                    } else if let Some(root) = project_root.as_ref() {
                        root.join(path)
                    } else {
                        path
                    }
                });
                let effective_context_path = resolved_context_path
                    .as_ref()
                    .and_then(|path| path.canonicalize().ok())
                    .or(resolved_context_path);
                let context_path_for_host = effective_context_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned());
                {
                    let host = rt.store.data_mut();
                    apply_invocation_context(
                        host,
                        context_path_for_host,
                        None,
                        editor_context.map(EditorContextSnapshot::into_context),
                    );
                    host.current_command_id = Some(command.id.clone());
                }
                match call_i32_arg_export(
                    &mut rt.store,
                    &rt.instance,
                    "vtcode_run_command",
                    command.runtime_index,
                ) {
                    Ok(0) => Ok(format!("Ran extension command: {}", cmd_title)),
                    Ok(_) => Err(format!(
                        "Command `{}` failed in extension {}",
                        command.id, ext_id
                    )),
                    Err(e) => Err(e),
                }
            } else {
                Err(format!("Extension runtime unavailable for {}", ext_id))
            };
            // Error OK: receiver dropped means no listener
            let _ = sender.send(crate::background::BackgroundResult::ExtensionCommandDone {
                extension_index: index,
                extension_id: ext_id,
                result,
                runtime: runtime_box,
            });
        });

        Ok(())
    }

    /// Run all due tick exports for one extension on a worker. Grouping them
    /// keeps the single Wasmtime store owned by one task and avoids dropping
    /// callbacks when an extension contributes more than one timer.
    pub(crate) fn invoke_tick_handlers_async(
        &mut self,
        extension_id: &str,
        exports: Vec<String>,
        background: &mut crate::background::BackgroundWorkers,
        editor_context: Option<EditorContextSnapshot>,
    ) -> Result<(), String> {
        if exports.is_empty() {
            return Ok(());
        }
        let index = self
            .index_by_id
            .get(extension_id)
            .copied()
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        if record.state != ExtensionState::Loaded || record.runtime.is_none() {
            return Err(format!("Extension is busy: {}", record.manifest.name));
        }
        let runtime = record
            .runtime
            .take()
            .ok_or_else(|| format!("Extension is busy: {}", record.manifest.name))?;
        let ext_id = record.manifest.id.clone();
        self.tick_yield_to_input.insert(ext_id.clone());
        self.busy_extension_ids.insert(ext_id.clone());
        let sender = background.sender.clone();
        background.spawn("ext-tick", move || {
            let mut runtime_box = ExtensionRuntimeBox::new(runtime);
            let result = if let Some(runtime) = runtime_box.0.as_mut() {
                {
                    let host = runtime.store.data_mut();
                    apply_invocation_context(
                        host,
                        None,
                        None,
                        editor_context.map(EditorContextSnapshot::into_context),
                    );
                }
                let mut result = Ok(());
                for export in exports {
                    match call_noarg_export(
                        &mut runtime.store,
                        &runtime.instance,
                        &export,
                        true,
                    ) {
                        Ok(Some(0) | None) => {}
                        Ok(Some(code)) => {
                            result = Err(format!(
                                "Tick handler `{export}` failed in extension {ext_id} with code {code}"
                            ));
                            break;
                        }
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
                if result.is_err() {
                    for document in &mut runtime.store.data_mut().documents { document.abort_transaction(); }
                }
                result
            } else {
                Err(format!("Extension runtime unavailable for {ext_id}"))
            };
            let _ = sender.send(crate::background::BackgroundResult::ExtensionTickDone {
                extension_index: index,
                result,
                runtime: runtime_box,
            });
        });
        Ok(())
    }

    /// Restore completed work and report whether it published a visual change.
    /// Capturing input and advancing a loader usually leave the frame intact.
    pub(crate) fn restore_runtime(
        &mut self,
        index: usize,
        mut runtime_box: ExtensionRuntimeBox,
    ) -> bool {
        if let Some(record) = self.records.get_mut(index) {
            self.busy_extension_ids.remove(&record.manifest.id);
            if let Some(rt) = runtime_box.take() {
                let host = rt.store.data();
                let changed = host.status.as_ref()
                    .is_some_and(|status| *status != record.status_text)
                    || !host.logs.is_empty()
                    || record.views.len() != host.views.len()
                    || host.views.iter().any(|(id, view)| {
                        record.views.get(id).is_none_or(|old| {
                            old.title != view.title
                                || old.content != view.content
                                || match (&old.frame, &view.frame) {
                                    (Some(old), Some(new)) => !Arc::ptr_eq(old, new),
                                    (None, None) => false,
                                    _ => true,
                                }
                        })
                    })
                    || record.overlays != host.overlays
                    || !host.requested_close_overlays.is_empty()
                    || host.requested_open_view.is_some()
                    || !host.text_edit_requests.is_empty()
                    || host.clipboard_write.is_some();
                Self::pull_runtime_state_from_runtime(record, rt);
                return changed;
            }
        }
        false
    }

    /// Pull state from a runtime and restore it to the record.
    fn pull_runtime_state_from_runtime(
        record: &mut ExtensionRecord,
        mut runtime: ExtensionRuntime,
    ) {
        let host = runtime.store.data_mut();
        if let Some(status) = host.status.take() {
            record.status_text = status;
        }
        let new_logs: Vec<String> = host.logs.drain(..).collect();
        record.views = sanitize_contributed_views(&host.views);
        record.overlays = host.overlays.clone();
        record.tick_handlers = host.tick_handlers.clone();
        let requested_close_overlays = std::mem::take(&mut host.requested_close_overlays);
        for close_id in &requested_close_overlays {
            record.overlays.remove(close_id);
        }
        record.explorer_context_commands = host.explorer_context_commands.clone();
        record.binary_open_commands = host.binary_open_commands.clone();
        record.file_open_commands = host.file_open_commands.clone();
        record.requested_open_view = host.requested_open_view.take();
        if !host.text_edit_requests.is_empty() {
            record
                .text_edit_requests
                .append(&mut host.text_edit_requests);
        }
        clear_invocation_context(host);
        for line in new_logs {
            record.logs.push(line);
        }
        if record.logs.len() > MAX_LOG_ENTRIES {
            let drop_count = record.logs.len() - MAX_LOG_ENTRIES;
            record.logs.drain(0..drop_count);
        }
        record.runtime = Some(runtime);
    }

    fn invoke_view_event_handler(
        &mut self,
        extension_id: &str,
        view_id: &str,
        kind: ViewEventHandlerKind,
        event_json: &str,
        editor_context: Option<EditorContext>,
    ) -> Result<Option<bool>, String> {
        let index = self
            .index_by_id
            .get(extension_id)
            .copied()
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let handler_export = {
            let record = self
                .records
                .get(index)
                .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
            if record.state != ExtensionState::Loaded || record.runtime.is_none() {
                return Err(format!("Extension is not loaded: {}", record.manifest.name));
            }
            let view = record
                .views
                .get(view_id)
                .ok_or_else(|| format!("Unknown extension view: {extension_id}/{view_id}"))?;
            match kind {
                ViewEventHandlerKind::Key => view.key_handler.clone(),
                ViewEventHandlerKind::Mouse => view.mouse_handler.clone(),
                ViewEventHandlerKind::Scroll => view.scroll_handler.clone(),
                ViewEventHandlerKind::Resize => view.resize_handler.clone(),
                ViewEventHandlerKind::Close => view.close_handler.clone(),
                ViewEventHandlerKind::Hover => view.hover_handler.clone(),
                ViewEventHandlerKind::ContextMenu => view.context_menu_handler.clone(),
            }
        };
        let Some(handler_export) = handler_export else {
            return Ok(None);
        };

        let record = self
            .records
            .get_mut(index)
            .ok_or_else(|| format!("Unknown extension id: {extension_id}"))?;
        let code = {
            let runtime = record
                .runtime
                .as_mut()
                .ok_or_else(|| format!("Extension is not loaded: {}", record.manifest.name))?;
            {
                let host = runtime.store.data_mut();
                apply_invocation_context(host, None, None, editor_context);
                host.current_view_event = Some(event_json.as_bytes().to_vec());
            }
            call_noarg_export(&mut runtime.store, &runtime.instance, &handler_export, true)?
                .unwrap_or(0)
        };
        Self::pull_runtime_state(record);

        match code {
            0 => Ok(Some(false)),
            1 => Ok(Some(true)),
            other => Err(format!(
                "View event handler `{handler_export}` failed in extension {extension_id} with code {other}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_manifest::{ExtensionManifest, ExtensionOverlay};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_manifest(id: &str) -> ExtensionManifest {
        ExtensionManifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "1.0.0".to_string(),
            repository: "https://github.com/im-henri/vtcode".into(),
            description: String::new(),
            main: String::new(),
            enabled: true,
            api_version: "0.1".to_string(),
            lsp: None,
            bundle: None,
        }
    }

    fn test_record(id: &str) -> ExtensionRecord {
        ExtensionRecord {
            manifest: test_manifest(id),
            manifest_path: PathBuf::new(),
            wasm_path: PathBuf::new(),
            user_unloaded: false,
            state: ExtensionState::Loaded,
            status_text: String::new(),
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
            manifest_mtime: None,
            wasm_mtime: None,
            runtime: None,
            text_edit_requests: Vec::new(),
            published_diagnostics: HashMap::new(),
            diagnostics_version: 0,
        }
    }

    fn test_manager_with(records: Vec<ExtensionRecord>) -> ExtensionManager {
        let mut index_by_id = HashMap::new();
        for (i, r) in records.iter().enumerate() {
            index_by_id.insert(r.manifest.id.clone(), i);
        }
        ExtensionManager {
            engine: Default::default(),
            search_roots: Vec::new(),
            records,
            index_by_id,
            settings_file: PathBuf::new(),
            busy_extension_ids: std::collections::HashSet::new(),
            pending_view_events: std::collections::VecDeque::new(),
            view_resize_requests: HashMap::new(),
            tick_yield_to_input: std::collections::HashSet::new(),
        }
    }

    #[test]
    fn delivered_resize_does_not_repeat_when_the_guest_publishes_a_smaller_frame() {
        use crate::extension_manifest::ExtensionView;
        use crate::extension_surface::ViewFrame;
        let mut record = test_record("ext1");
        record.views.insert("plot".into(), ExtensionView {
            id: "plot".into(),
            title: "Plot".into(),
            content: String::new(),
            key_handler: None,
            mouse_handler: None,
            scroll_handler: None,
            resize_handler: Some("resize".into()),
            close_handler: None,
            hover_handler: None,
            context_menu_handler: None,
            pointer_capture: false,
            input_pending: false,
            frame: Some(Arc::new(ViewFrame::default())),
        });
        let mut manager = test_manager_with(vec![record]);
        let resize = |manager: &mut ExtensionManager, width| {
            manager.queue_view_event("ext1", "plot", ViewEventHandlerKind::Resize,
                serde_json::json!({ "width": width, "height": 240 }).to_string(),
                EditorContextSnapshot::default(), String::new())
        };
        assert!(resize(&mut manager, 600));
        manager.pending_view_events.pop_front().expect("initial resize is delivered");
        manager.records[0].views.get_mut("plot").unwrap().frame = Some(Arc::new(ViewFrame {
            width: 512, height: 200, ..ViewFrame::default()
        }));
        for _ in 0..20 {
            assert!(resize(&mut manager, 600));
            assert!(manager.pending_view_events.is_empty());
        }
        assert!(resize(&mut manager, 601));
        assert_eq!(manager.pending_view_events.len(), 1, "a changed viewport is delivered");
    }

    #[test]
    fn take_requested_open_view_returns_value() {
        let mut rec = test_record("ext1");
        rec.requested_open_view = Some("my-view".to_string());
        let mut mgr = test_manager_with(vec![rec]);

        let result = mgr.take_requested_open_view("ext1");
        assert_eq!(result, Some("my-view".to_string()));
        // Second call should return None (already taken)
        assert_eq!(mgr.take_requested_open_view("ext1"), None);
    }

    #[test]
    fn take_requested_open_view_unknown_ext() {
        let mut mgr = test_manager_with(vec![]);
        assert_eq!(mgr.take_requested_open_view("nonexistent"), None);
    }

    #[test]
    fn take_pending_text_edits_collects_all() {
        let mut rec1 = test_record("ext1");
        let mut rec2 = test_record("ext2");
        rec1.text_edit_requests.push(TextEditRequest::InsertText {
            line: 0,
            col: 0,
            text: "a".into(),
        });
        rec2.text_edit_requests
            .push(TextEditRequest::ReplaceSelection { text: "b".into() });
        let mut mgr = test_manager_with(vec![rec1, rec2]);

        let edits = mgr.take_pending_text_edits();
        assert_eq!(edits.len(), 2);
        // Second call should be empty
        assert!(mgr.take_pending_text_edits().is_empty());
    }

    #[test]
    fn close_overlay_removes_from_record() {
        let mut rec = test_record("ext1");
        rec.overlays.insert(
            "ov1".to_string(),
            ExtensionOverlay {
                id: "ov1".to_string(),
                title: "Test".to_string(),
                content: String::new(),
                x: 0,
                y: 0,
                width: 10,
                height: 5,
                mouse_handler: None,
                key_handler: None,
            },
        );
        let mut mgr = test_manager_with(vec![rec]);

        assert!(mgr.records[0].overlays.contains_key("ov1"));
        mgr.close_overlay("ext1", "ov1");
        assert!(!mgr.records[0].overlays.contains_key("ov1"));
    }

    #[test]
    fn close_overlay_noop_for_unknown() {
        let mut mgr = test_manager_with(vec![test_record("ext1")]);
        mgr.close_overlay("nonexistent", "ov1"); // should not panic
    }

    #[test]
    fn update_overlay_position_updates_coords() {
        let mut rec = test_record("ext1");
        rec.overlays.insert(
            "ov1".to_string(),
            ExtensionOverlay {
                id: "ov1".to_string(),
                title: "Test".to_string(),
                content: String::new(),
                x: 0,
                y: 0,
                width: 10,
                height: 5,
                mouse_handler: None,
                key_handler: None,
            },
        );
        let mut mgr = test_manager_with(vec![rec]);

        mgr.update_overlay_position("ext1", "ov1", 42, 99);
        let ov = &mgr.records[0].overlays["ov1"];
        assert_eq!(ov.x, 42);
        assert_eq!(ov.y, 99);
    }

    #[test]
    fn take_pending_text_edits_empty() {
        let mut mgr = test_manager_with(vec![test_record("ext1")]);
        assert!(mgr.take_pending_text_edits().is_empty());
    }
}
