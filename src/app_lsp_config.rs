//! Apply settings and extension language servers without blocking on shutdown.
use crate::{
    app::{App, TabKind},
    lsp::LspManager,
    settings::SettingsScope,
};

impl App {
    pub(crate) fn refresh_language_servers(&mut self, restart: bool) {
        let settings = self.settings_for_scope(SettingsScope::Workspace);
        let diagnostic_delay = settings.lsp_diagnostic_delay_ms.unwrap_or(crate::lsp::diagnostics::DEFAULT_DELAY_MS);
        self.lsp_manager.diagnostic_publication.set_delay_ms(diagnostic_delay);
        let mut configs = Vec::new();
        if settings.lsp_enabled.unwrap_or(true) {
            // Explicit preferences take precedence over extension defaults.
            for config in settings
                .lsp_servers
                .unwrap_or_default()
                .into_iter()
                .chain(self.extension_manager.lsp_configs())
            {
                if !configs
                    .iter()
                    .any(|existing: &crate::lsp::LspServerConfig| {
                        existing.language == config.language
                    })
                {
                    configs.push(config);
                }
            }
        }
        let associations: crate::file_associations::Associations = self.language_associations.iter()
            .map(|(pattern, language)| (pattern.clone(), crate::file_associations::language_id(language)))
            .collect();
        if !restart && self.lsp_manager.configs == configs
            && *self.lsp_manager.file_associations == associations {
            return;
        }
        if self.lsp_format_pending {
            self.finish_lsp_formatting(Err("Language servers were restarted or reconfigured while formatting. Try again once the server is ready.".into()));
        }
        let mut manager =
            LspManager::new_with_wake(self.explorer.root.clone(), self.background.wake_handle());
        manager.remote_workspace = self.remote_workspace.clone();
        manager.file_associations = std::sync::Arc::new(associations);
        manager.diagnostic_publication.set_delay_ms(diagnostic_delay);
        manager.diagnostics_version = self.lsp_manager.diagnostics_version.wrapping_add(1);
        manager.completion_revision = self.lsp_manager.completion_revision.wrapping_add(1);
        manager.signature_revision = self.lsp_manager.signature_revision.wrapping_add(1);
        self.editor_diagnostic_cache.clear();
        for config in configs {
            manager.register_config(config);
        }
        let previous = std::mem::replace(&mut self.lsp_manager, manager);
        self.background
            .spawn("lsp-shutdown", move || drop(previous));
        self.clear_lsp_syncs();
        self.pending_lsp_workspace_edits.clear();
        self.lsp_suggestions = Default::default();
        self.clear_lsp_link_hover();
        self.lsp_completion_active = false;
        self.dismiss_lsp_hover();
        self.lsp_signature_active = false;
        self.lsp_code_action_active = false;
        self.lsp_references_active = false;
        self.lsp_format_pending = false;
        self.lsp_format_target_path = None;
        self.lsp_save_after_format_path = None;
        self.lsp_inlay_hints.clear();
        self.lsp_semantic_tokens.clear();
        self.lsp_document_highlights.clear();
        self.lsp_document_multiline_highlights.clear();
        self.lsp_code_lenses.clear();
        self.lsp_code_lenses_raw.clear();
        self.lsp_document_links.clear();
        self.lsp_folding_ranges.clear();
        self.lsp_selection_ranges.clear();
        self.lsp_selection_range_source = None;
        self.lsp_selection_range_cursor = (usize::MAX, usize::MAX);
        self.lsp_outline_available = false;
        self.lsp_inlay_hint_last_scroll = usize::MAX;
        self.lsp_semantic_tokens_last_version = u64::MAX;
        self.lsp_outline_last_version = u64::MAX;
        self.lsp_folding_last_version = u64::MAX;
        self.lsp_code_lens_last_version = u64::MAX;
        self.lsp_document_links_last_version = u64::MAX;
        // Already-open files must connect too, without requiring an edit/reopen.
        let documents: Vec<_> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| {
                tab.kind == TabKind::File
                    && !tab.editor.bg_loading
                    && !tab.editor.large_file_mode
                    && tab.editor.encoding != "Binary"
            })
            .filter_map(|tab| {
                Some((
                    tab.editor.file_path.clone()?,
                    tab.editor.content_version,
                    tab.editor.rope.clone(),
                ))
            })
            .collect();
        for (path, version, rope) in documents {
            self.queue_lsp_document_sync(path, version, rope);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Tab,
        editor::Editor,
        lsp::{LspClient, LspServerConfig},
    };

    fn config(command: &str) -> LspServerConfig {
        LspServerConfig {
            language: "rust".into(),
            command: command.into(),
            args: vec![],
            file_patterns: vec!["*.rs".into()],
            settings: None, init_options: None,
        }
    }

    #[test]
    fn diagnostic_delay_can_be_edited_persisted_and_reset_without_restarting_servers() {
        let mut app = App::test_new();
        let directory = tempfile::tempdir().unwrap();
        app.settings_workspace_path = directory.path().join("workspace.json");
        app.settings_persistence_enabled = true;
        app.settings_global.lsp_servers = Some(vec![config("test-server")]);
        app.settings_global.lsp_diagnostic_delay_ms = Some(1_500);
        app.apply_effective_settings();
        let (client, _server) = LspClient::test_pair("rust", &["*.rs"]);
        app.lsp_manager.clients.push(client);
        let id = "lsp_diagnostic_delay_ms";
        app.settings_scope = SettingsScope::Workspace;
        assert_eq!(app.setting_source_label(id), "Inherited · Global");
        assert_eq!(app.current_settings().lsp_diagnostic_delay_ms, Some(1_500));
        assert_eq!(App::settings_category_for_id(id), 8);
        assert_eq!(App::setting_field_kind(id), "number");
        for (input, expected) in [("2500", 2_500), ("99999", 10_000), ("0", 0)] {
            app.begin_numeric_setting_edit(id, "1500");
            app.settings_value_buffer = input.into();
            assert!(app.commit_numeric_setting_edit());
            assert_eq!(app.lsp_manager.clients.len(), 1, "delay edits must not restart LSP");
            assert_eq!(app.current_settings().lsp_diagnostic_delay_ms, Some(expected));
            assert_eq!(crate::settings::load_from_file(&app.settings_workspace_path).lsp_diagnostic_delay_ms, Some(expected));
            assert_eq!(app.settings_items().iter().find(|item| item.2 == id).unwrap().1, expected.to_string());
        }
        assert!(app.reset_setting_in_selected_scope(id));
        assert_eq!(app.lsp_manager.diagnostic_publication.delay_ms(), 1_500);
        assert_eq!(app.lsp_manager.clients.len(), 1);
        app.settings_global.lsp_diagnostic_delay_ms = None;
        app.apply_effective_settings();
        assert_eq!(app.lsp_manager.diagnostic_publication.delay_ms(), crate::lsp::diagnostics::DEFAULT_DELAY_MS);
        app.settings_global.lsp_diagnostic_delay_ms = Some(2_000);
        app.refresh_language_servers(true);
        assert_eq!(app.lsp_manager.diagnostic_publication.delay_ms(), 2_000, "restarts retain the preference");
    }

    #[test]
    fn saved_servers_apply_to_open_files_and_failures_can_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test.rs");
        let mut app = App::test_new();
        app.explorer.root = root.path().into();
        let mut editor = Editor::test_with_text("fn main() {}\n");
        editor.file_path = Some(path.clone());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.settings_global.lsp_servers = Some(vec![config("vtcode-nonexistent-lsp-test-command")]);
        app.apply_effective_settings();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.lsp_manager.startup_errors.is_empty()
        });
        assert!(
            app.lsp_manager
                .server_status("rust")
                .contains("Failed to start")
        );
        assert_eq!(app.lsp_manager.file_status(&path), "Failed to start");
        assert!(
            !app.lsp_manager.has_config_for_file(&path),
            "failed launches must not repeatedly flatten documents or spawn processes"
        );
        app.refresh_language_servers(true);
        assert!(app.lsp_manager.startup_errors.is_empty());
        assert!(app.lsp_manager.has_config_for_file(&path));
        app.settings_scope = SettingsScope::Workspace;
        app.toggle_setting("lsp_enabled");
        assert!(!app.lsp_enabled);
        assert_eq!(app.settings_workspace.lsp_enabled, Some(false));
        assert!(app.lsp_manager.configs.is_empty());
        app.poll_background_results();
        assert!(app.lsp_manager.clients.is_empty());
        app.toggle_setting("lsp_enabled");
        assert!(app.lsp_enabled);
        assert_eq!(app.lsp_manager.configs.len(), 1);
    }

    #[test]
    fn unrelated_settings_do_not_restart_servers_and_scope_reset_restores_global() {
        let mut app = App::test_new();
        app.settings_global.lsp_servers = Some(vec![config("global-server")]);
        app.settings_workspace.lsp_servers = Some(vec![config("workspace-server")]);
        app.apply_effective_settings();
        assert_eq!(app.lsp_manager.configs[0].command, "workspace-server");
        let (client, _server) = LspClient::test_pair("rust", &["*.rs"]);
        app.lsp_manager.clients.push(client);
        app.toggle_setting("word_wrap");
        assert_eq!(app.lsp_manager.clients.len(), 1);
        app.settings_workspace.lsp_servers = None;
        app.apply_effective_settings();
        assert_eq!(app.lsp_manager.configs[0].command, "global-server");
        assert!(app.lsp_manager.clients.is_empty());
        app.settings_global.lsp_servers = None;
        app.apply_effective_settings();
        assert!(app.lsp_manager.configs.is_empty());
    }

    #[test]
    fn custom_servers_override_extensions_and_removal_restores_the_extension() {
        let root = tempfile::tempdir().unwrap();
        let mut app = App::test_new();
        let manifest: crate::extension_manifest::ExtensionManifest = serde_json::from_value(serde_json::json!({
            "id":"lsp-fixture", "name":"LSP fixture", "version":"1.0.0", "lsp":config("extension-server")
        })).unwrap();
        app.extension_manager.sync_discovered(
            vec![crate::extension_manifest::DiscoveredExtension {
                manifest,
                manifest_path: root.path().join("extension.json"),
                wasm_path: root.path().join("unused.wasm"),
                manifest_mtime: None,
                wasm_mtime: None,
            }],
            vec![],
        );
        app.refresh_language_servers(false);
        assert_eq!(app.lsp_manager.configs.len(), 1);
        assert_eq!(app.lsp_manager.configs[0].command, "extension-server");
        app.settings_global.lsp_servers = Some(vec![config("custom-server")]);
        app.apply_effective_settings();
        assert_eq!(app.lsp_manager.configs.len(), 1);
        assert_eq!(app.lsp_manager.configs[0].command, "custom-server");
        app.settings_global.lsp_servers = None;
        app.apply_effective_settings();
        assert_eq!(app.lsp_manager.configs[0].command, "extension-server");
        app.extension_manager.records[0].manifest.enabled = false;
        app.refresh_language_servers(false);
        assert!(app.lsp_manager.configs.is_empty());
    }

    #[test]
    fn server_settings_accept_legacy_manifest_keys_and_match_compound_suffixes() {
        let config: LspServerConfig = serde_json::from_value(serde_json::json!({
            "language":"typescript", "command":"typescript-language-server",
            "file_patterns":["*.d.ts", "tsconfig.json"], "init_options":{"preferences":{}}
        }))
        .unwrap();
        assert!(config.handles_file(std::path::Path::new("folder/example.d.ts")));
        assert!(config.handles_file(std::path::Path::new("folder/tsconfig.json")));
        assert!(!config.handles_file(std::path::Path::new("example.ts")));
        let json = serde_json::to_value(&config).unwrap();
        assert!(json.get("filePatterns").is_some());
        assert_eq!(
            serde_json::from_value::<LspServerConfig>(json).unwrap(),
            config
        );
    }
}

impl LspManager {
    pub(crate) fn server_status(&self, language: &str) -> String {
        if let Some(error) = self.startup_errors.get(language) {
            return error.clone();
        }
        match self
            .clients
            .iter()
            .find(|client| client.language == language)
        {
            Some(client) if client.initialized => "Connected".into(),
            Some(_) => "Starting…".into(),
            None => "Ready · opens with a matching file".into(),
        }
    }

    pub(crate) fn file_status(&self, path: &std::path::Path) -> &'static str {
        let Some(config) = self.configs.iter().find(|config| config.handles_file(path)) else {
            return "Not configured";
        };
        if self.startup_errors.contains_key(&config.language) {
            return "Failed to start";
        }
        match self.clients.iter().find(|client| client.handles_file(path)) {
            Some(client) if client.initialized => "Connected",
            Some(_) => "Starting…",
            None => "Ready",
        }
    }
}
