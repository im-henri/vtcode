use crate::app::App;
use crate::extension_versions::ExtensionVersions;
use ratatui::layout::Rect;

pub(crate) struct VersionPicker {
    pub catalog: ExtensionVersions,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
    pub list_area: Rect,
}

impl VersionPicker {
    pub(crate) fn selected_version(&self) -> Option<&crate::extension_versions::ReleaseVersion> {
        self.filtered
            .get(self.selected)
            .and_then(|index| self.catalog.versions.get(*index))
    }
}

impl App {
    pub(crate) fn open_extension_install_prompt(&mut self) {
        self.open_theme_install_prompt();
        self.palette.install_theme_mode = false;
        self.palette.install_extension_mode = true;
    }

    pub(crate) fn start_extension_install(&mut self) {
        if self.palette.theme_install_running {
            return;
        }
        if self.palette.extension_versions.is_some() {
            self.install_selected_extension_version();
        } else {
            self.refresh_extension_versions();
        }
    }

    pub(crate) fn cancel_extension_version_flow(&mut self) {
        if self.palette.install_extension_mode {
            self.palette.extension_install_request =
                self.palette.extension_install_request.wrapping_add(1);
            self.palette.extension_versions = None;
            if !self.palette.extension_installing {
                self.palette.theme_install_running = false;
            }
        }
    }

    pub(crate) fn refresh_extension_versions(&mut self) {
        if self.palette.theme_install_running {
            return;
        }
        let input = self.palette.extension_versions.as_ref().map_or_else(
            || self.palette.query.trim().to_string(),
            |picker| picker.catalog.input.clone(),
        );
        if let Err(error) = crate::extension_install::ExtensionSource::parse(&input) {
            self.palette.theme_install_error = Some(error);
            return;
        }
        self.palette.extension_install_request =
            self.palette.extension_install_request.wrapping_add(1);
        let request = self.palette.extension_install_request;
        self.palette.theme_install_running = true;
        self.palette.theme_install_error = None;
        let root = crate::portable::data_dir().join("extensions");
        let sender = self.background.sender.clone();
        if self
            .background
            .maintenance_sender()
            .send(Box::new(move || {
                let result = crate::extension_versions::discover(&input, &root);
                let _ = sender.send(
                    crate::background::BackgroundResult::ExtensionVersionsDiscovered {
                        request,
                        result,
                    },
                );
            }))
            .is_err()
        {
            self.palette.theme_install_running = false;
            self.palette.theme_install_error =
                Some("Extension version lookup worker is unavailable.".into());
        }
    }

    pub(crate) fn finish_extension_versions(
        &mut self,
        request: u64,
        result: Result<ExtensionVersions, String>,
    ) {
        if request != self.palette.extension_install_request
            || !self.palette.active
            || !self.palette.install_extension_mode
        {
            return;
        }
        self.palette.theme_install_running = false;
        match result {
            Ok(catalog) => {
                let old_ref = self
                    .palette
                    .extension_versions
                    .as_ref()
                    .and_then(|picker| picker.selected_version())
                    .map(|version| version.name.clone());
                if self.palette.extension_versions.is_none() {
                    self.palette.query.clear();
                    self.palette.cursor = 0;
                    self.palette.selection_anchor = None;
                }
                let preferred = old_ref
                    .as_ref()
                    .and_then(|reference| {
                        catalog
                            .versions
                            .iter()
                            .position(|version| &version.name == reference)
                    })
                    .unwrap_or_else(|| catalog.preferred_index());
                let filtered = (0..catalog.versions.len()).collect();
                self.palette.extension_versions = Some(VersionPicker {
                    catalog,
                    filtered,
                    selected: preferred,
                    scroll: 0,
                    list_area: Rect::default(),
                });
                if !self.palette.query.is_empty() {
                    self.filter_extension_versions();
                    if let Some(reference) = old_ref
                        && let Some(picker) = &mut self.palette.extension_versions
                        && let Some(position) = picker
                            .filtered
                            .iter()
                            .position(|index| picker.catalog.versions[*index].name == reference)
                    {
                        picker.selected = position;
                    }
                }
                self.palette.theme_install_error = None;
                self.modal.focus = 0;
                self.text_input_regions.clear();
            }
            Err(error) => self.palette.theme_install_error = Some(error),
        }
    }

    pub(crate) fn filter_extension_versions(&mut self) {
        let query = self.palette.query.to_lowercase();
        if let Some(picker) = &mut self.palette.extension_versions {
            picker.filtered = picker
                .catalog
                .versions
                .iter()
                .enumerate()
                .filter(|(_, version)| {
                    version.name.to_lowercase().contains(&query)
                        || version.category().contains(&query)
                        || version.sha256.contains(&query)
                })
                .map(|(index, _)| index)
                .collect();
            picker.selected = 0;
            picker.scroll = 0;
        }
    }

    pub(crate) fn move_extension_version_selection(&mut self, delta: isize) {
        if self.palette.theme_install_running {
            return;
        }
        if let Some(picker) = &mut self.palette.extension_versions {
            picker.selected = picker
                .selected
                .saturating_add_signed(delta)
                .min(picker.filtered.len().saturating_sub(1));
        }
    }

    pub(crate) fn back_to_extension_link(&mut self) {
        if self.palette.extension_installing {
            self.close_palette();
            return;
        }
        if let Some(picker) = self.palette.extension_versions.take() {
            self.palette.query = picker.catalog.input;
            self.palette.cursor = self.palette.query.len();
            self.palette.selection_anchor = None;
        }
        self.palette.extension_install_request =
            self.palette.extension_install_request.wrapping_add(1);
        self.palette.theme_install_running = false;
        self.palette.theme_install_error = None;
        self.modal.focus = 0;
        self.text_input_regions.clear();
    }

    fn install_selected_extension_version(&mut self) {
        let Some((source, version)) = self.palette.extension_versions.as_ref().and_then(|picker| {
            picker
                .selected_version()
                .map(|version| (picker.catalog.source.clone(), version.clone()))
        }) else {
            return;
        };
        if self.extension_manager.has_unsaved_documents() {
            self.palette.theme_install_error = Some("Save or close modified extension documents before installing or updating extensions.".into());
            return;
        }
        self.palette.theme_install_running = true;
        self.palette.extension_installing = true;
        self.palette.theme_install_error = None;
        self.palette.extension_install_request =
            self.palette.extension_install_request.wrapping_add(1);
        let request = self.palette.extension_install_request;
        let root = crate::portable::data_dir().join("extensions");
        let sender = self.background.sender.clone();
        if self
            .background
            .maintenance_sender()
            .send(Box::new(move || {
                let result = crate::extension_install::install_version(&source, &version, &root);
                let _ = sender.send(crate::background::BackgroundResult::ExtensionInstalled {
                    request,
                    result,
                });
            }))
            .is_err()
        {
            self.palette.theme_install_running = false;
            self.palette.extension_installing = false;
            self.palette.theme_install_error =
                Some("Extension installer worker is unavailable.".into());
        }
    }

    pub(crate) fn finish_extension_install(
        &mut self,
        request: u64,
        result: Result<Vec<crate::extension_install::InstalledExtension>, String>,
    ) {
        self.palette.theme_install_running = false;
        self.palette.extension_installing = false;
        let current = request == self.palette.extension_install_request
            && self.palette.active
            && self.palette.install_extension_mode;
        match result {
            Ok(installed) => {
                let names = installed
                    .iter()
                    .map(|extension| format!("{} {}", extension.name, extension.version))
                    .collect::<Vec<_>>()
                    .join(", ");
                if current {
                    self.palette.theme_install_error = None;
                    self.close_palette();
                }
                self.request_extension_scan();
                self.notify(format!("Installed {names}. Loading extensions…"));
            }
            Err(error) => {
                if current {
                    self.palette.theme_install_error = Some(error);
                } else {
                    self.notify_error(format!("Extension installation failed: {error}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn catalog() -> ExtensionVersions {
        let hash = "c".repeat(64);
        let releases: Vec<_> = ["v0.1.0", "v0.2.0", "v0.3.0-rc.1"].into_iter().enumerate()
            .map(|(index, tag)| serde_json::json!({"id": index + 100, "tag_name": tag, "draft": false,
                "prerelease": tag.contains("rc"), "assets": [{"id": index + 1, "name": "vtcode-extension.zip",
                    "size": 100, "state": "uploaded", "digest": format!("sha256:{hash}")}]})).collect();
        let mut catalog = crate::extension_versions::parse_releases(
            "https://github.com/im-henri/vtcode-ext-hex-editor",
            &serde_json::to_vec(&releases).unwrap(),
        )
        .unwrap();
        catalog
            .installed
            .push(crate::extension_install::InstalledExtension {
                id: "vtcode.hex-editor".into(),
                name: "Hex Editor".into(),
                version: "0.1.0".into(),
                repository: catalog.source.repository.clone(),
                tag: "v0.1.0".into(),
                asset_id: 1,
                sha256: hash,
            });
        catalog
    }

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
    }

    fn show_versions(app: &mut App) {
        app.open_extension_install_prompt();
        app.palette.query = catalog().input.clone();
        app.palette.theme_install_running = true;
        app.finish_extension_versions(app.palette.extension_install_request, Ok(catalog()));
        assert!(!app.palette.theme_install_running);
        assert_eq!(
            app.palette
                .extension_versions
                .as_ref()
                .unwrap()
                .selected_version()
                .unwrap()
                .name,
            "v0.2.0"
        );
    }

    #[test]
    fn extension_version_picker_supports_filter_mouse_keyboard_and_back_without_installing() {
        let mut app = App::test_new();
        show_versions(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal
            .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for text in [
            "Choose Extension Version",
            "Latest stable: v0.2.0",
            "Hex Editor 0.1.0",
            "installed",
            "release",
            "prerelease",
        ] {
            assert!(rendered.contains(text), "missing {text}: {rendered}");
        }
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            app.palette
                .extension_versions
                .as_ref()
                .unwrap()
                .selected_version()
                .unwrap()
                .name,
            "v0.1.0"
        );
        assert!(app.paste_into_focused_text_input("rc.1"));
        assert_eq!(
            app.palette
                .extension_versions
                .as_ref()
                .unwrap()
                .filtered
                .len(),
            1
        );
        assert_eq!(
            app.palette
                .extension_versions
                .as_ref()
                .unwrap()
                .selected_version()
                .unwrap()
                .name,
            "v0.3.0-rc.1"
        );
        key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        terminal
            .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
            .unwrap();
        let list = app.palette.extension_versions.as_ref().unwrap().list_area;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: list.x,
                row: list.y + 1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(
            app.palette
                .extension_versions
                .as_ref()
                .unwrap()
                .selected_version()
                .unwrap()
                .name,
            "v0.1.0"
        );
        // The real install action must still respect running/dirty extension documents.
        app.extension_manager
            .busy_extension_ids
            .insert("busy".into());
        let install = app
            .modal
            .buttons
            .iter()
            .find(|(_, index)| *index == 0)
            .unwrap()
            .0;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: install.x,
                row: install.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(
            app.palette
                .theme_install_error
                .as_deref()
                .unwrap()
                .contains("Save or close")
        );
        assert!(!app.palette.extension_installing);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.palette.extension_versions.is_none());
        assert_eq!(app.palette.query, catalog().input);
        assert!(app.palette.install_extension_mode);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.palette.active);
    }

    #[test]
    fn cancelled_and_stale_version_lookups_cannot_reopen_or_replace_the_picker() {
        let mut app = App::test_new();
        app.open_extension_install_prompt();
        let old = app.palette.extension_install_request;
        app.palette.theme_install_running = true;
        app.close_palette();
        app.finish_extension_versions(old, Ok(catalog()));
        assert!(!app.palette.active);
        assert!(!app.palette.theme_install_running);
        assert!(app.palette.extension_versions.is_none());
        show_versions(&mut app);
        app.finish_extension_versions(old, Err("stale failure".into()));
        assert!(app.palette.theme_install_error.is_none());
        assert!(app.palette.extension_versions.is_some());
        app.palette.query = "v0.".into();
        app.filter_extension_versions();
        app.move_extension_version_selection(1);
        app.finish_extension_versions(app.palette.extension_install_request, Ok(catalog()));
        assert_eq!(
            app.palette
                .extension_versions
                .as_ref()
                .unwrap()
                .selected_version()
                .unwrap()
                .name,
            "v0.1.0"
        );
        app.finish_extension_install(
            app.palette.extension_install_request,
            Err("Tag changed; refresh the list".into()),
        );
        assert!(app.palette.extension_versions.is_some());
        assert!(
            app.palette
                .theme_install_error
                .as_deref()
                .unwrap()
                .contains("refresh")
        );
    }

    #[test]
    fn extension_versions_render_releases_prereleases_errors_and_empty_filters_at_small_sizes() {
        let mut app = App::test_new();
        show_versions(&mut app);
        for (width, height) in [(110, 32), (60, 18), (30, 9), (12, 5), (1, 1)] {
            for busy in [false, true] {
                for error in [false, true] {
                    app.palette.theme_install_running = busy;
                    app.palette.theme_install_error =
                        error.then(|| "Tag changed. Refresh to choose again.".into());
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal
                        .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
                        .unwrap();
                    if width == 110
                        && !busy
                        && !error
                        && let Some(directory) = std::env::var_os("VTCODE_HEX_VISUAL_DIR")
                    {
                        let directory = std::path::PathBuf::from(directory);
                        std::fs::create_dir_all(&directory).unwrap();
                        let buffer = terminal.backend().buffer();
                        let cells = buffer.content.iter().map(|cell| serde_json::json!({
                                "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg)
                            })).collect::<Vec<_>>();
                        std::fs::write(
                            directory.join("hex-version-picker.json"),
                            serde_json::to_vec(&serde_json::json!({
                                "width": width, "height": height, "cells": cells
                            }))
                            .unwrap(),
                        )
                        .unwrap();
                    }
                }
            }
        }
        app.palette.theme_install_running = false;
        app.palette.query = "no such version".into();
        app.filter_extension_versions();
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal
            .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
            .unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.palette.extension_installing);
        assert!(
            app.palette
                .extension_versions
                .as_ref()
                .unwrap()
                .selected_version()
                .is_none()
        );
    }

    #[test]
    fn extension_version_picker_keeps_controls_readable_across_themes() {
        let mut app = App::test_new();
        show_versions(&mut app);
        let mut failures = Vec::new();
        for theme in crate::ui_control_visibility_tests::audit_themes() {
            app.colors = theme.to_theme_colors();
            let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
            terminal
                .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
                .unwrap();
            crate::ui_control_visibility_tests::check_region(
                terminal.backend().buffer(),
                app.modal.area,
                &theme.name,
                &mut failures,
                &app.colors.ui,
            );
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    #[test]
    fn extension_install_is_discoverable_without_installed_extensions_and_retries_invalid_links() {
        let mut app = App::test_new();
        assert!(
            app.extension_palette_commands()
                .iter()
                .any(|(command, _)| command.id == "extensions_install")
        );
        app.execute_palette_command("extensions_install");
        assert!(app.palette.install_extension_mode);
        app.palette.query = "file:///tmp/repo".into();
        app.start_extension_install();
        assert!(!app.palette.theme_install_running);
        assert!(app.palette.theme_install_error.is_some());
        app.finish_extension_install(
            app.palette.extension_install_request,
            Err("Git unavailable".into()),
        );
        assert_eq!(app.palette.query, "file:///tmp/repo");
        assert!(app.palette.install_extension_mode);
    }

    #[test]
    fn extension_link_form_supports_keyboard_mouse_retry_and_small_terminals() {
        use crossterm::event::{
            KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };
        use ratatui::{Terminal, backend::TestBackend};
        for mouse in [false, true] {
            let mut app = App::test_new();
            app.open_extension_install_prompt();
            assert!(app.paste_into_focused_text_input("file:///unsupported"));
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
                .unwrap();
            if mouse {
                let button = app.modal.buttons[0].0;
                crate::mouse::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: button.x,
                        row: button.y,
                        modifiers: KeyModifiers::NONE,
                    },
                );
            } else {
                crate::input_keyboard::handle_keyboard_event(
                    &mut app,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                );
            }
            assert!(app.palette.theme_install_error.is_some());
            assert_eq!(app.palette.query, "file:///unsupported");
            for (width, height) in [(100, 30), (40, 12), (12, 5), (1, 1)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                for running in [false, true] {
                    app.palette.theme_install_running = running;
                    terminal
                        .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
                        .unwrap();
                    if width == 100 {
                        let rendered: String = terminal
                            .backend()
                            .buffer()
                            .content
                            .iter()
                            .map(|cell| cell.symbol())
                            .collect();
                        assert!(rendered.contains("Install Extension"));
                        assert!(rendered.contains("GitHub repository"));
                    }
                }
            }
        }
    }
}
