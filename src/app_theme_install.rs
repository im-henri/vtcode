use crate::app::App;

impl App {
    pub(crate) fn theme_install_prompt_visible(&self) -> bool {
        self.palette.install_theme_mode || self.palette.install_extension_mode || self.palette.install_syntax_mode
    }

    pub(crate) fn install_prompt_running(&self) -> bool {
        if self.palette.install_syntax_mode {
            self.palette.syntax_install_running
        } else {
            self.palette.theme_install_running
        }
    }

    pub(crate) fn open_theme_install_prompt(&mut self) {
        self.cancel_extension_version_flow();
        self.restore_palette_theme_preview();
        self.clear_drag_states();
        self.palette.active = true;
        self.palette.install_theme_mode = true;
        self.palette.install_extension_mode = false;
        self.palette.install_syntax_mode = false;
        self.palette.theme_install_error = None;
        self.palette.theme_vsix_url = None;
        self.palette.query.clear();
        self.palette.cursor = 0;
        self.palette.selection_anchor = None;
        self.palette.filtered.clear();
        self.palette.selected = 0;
        self.palette.scroll_offset = 0;
    }

    pub(crate) fn theme_vsix_link(&self) -> Option<String> {
        if !self.palette.install_theme_mode || self.palette.install_extension_mode {
            return None;
        }
        crate::theme_download::package_url(self.palette.query.trim().trim_matches('"')).ok()
    }

    pub(crate) fn show_theme_vsix_link(&mut self) {
        if let Some(url) = self.theme_vsix_link() {
            self.clear_drag_states();
            self.text_input_regions.clear();
            self.palette.theme_vsix_url = Some(url);
        }
    }

    pub(crate) fn copy_theme_vsix_link(&mut self) {
        self.copy_theme_vsix_link_with(|url| {
            crate::clipboard::set_text(url).map_err(|error| error.to_string())
        });
    }

    fn copy_theme_vsix_link_with(&mut self, copy: impl FnOnce(&str) -> Result<(), String>) {
        let Some(url) = self.palette.theme_vsix_url.clone() else {
            return;
        };
        match copy(&url) {
            Ok(()) => {
                self.push_clipboard_history(&url);
                self.modal.error = None;
                self.notify(
                    "VSIX link copied. Download it in a browser, then install the local .vsix file.",
                );
            }
            Err(error) => {
                self.modal.error = Some(format!(
                    "Could not copy VSIX link: {error}. Use Open in browser to download it."
                ));
            }
        }
    }

    pub(crate) fn start_theme_install(&mut self) {
        if self.palette.install_syntax_mode { self.start_syntax_install(); return; }
        if self.palette.install_extension_mode { self.start_extension_install(); return; }
        if self.palette.theme_install_running {
            return;
        }
        self.restore_palette_theme_preview();
        self.palette.install_theme_mode = true;
        let source = self.palette.query.trim().to_string();
        if source.is_empty() {
            self.palette.theme_install_error = Some(
                "Enter a vscodethemes.com theme link or a local theme file/directory path.".into(),
            );
            return;
        }
        self.palette.theme_install_running = true;
        self.palette.theme_install_error = None;
        let sender = self.background.sender.clone();
        let root = crate::portable::theme_extensions_dir();
        // Downloading, file reading, unpacking and validation stay off the UI thread.
        if self
            .background
            .maintenance_sender()
            .send(Box::new(move || {
                let result = crate::theme_install::install(&source, &root);
                let _ = sender.send(crate::background::BackgroundResult::ThemeInstalled(result));
            }))
            .is_err()
        {
            self.palette.theme_install_running = false;
            self.palette.theme_install_error =
                Some("Theme installer worker is unavailable.".into());
        }
    }

    pub(crate) fn finish_theme_install(
        &mut self,
        result: Result<Vec<crate::app_theme::ThemeFile>, String>,
    ) {
        self.palette.theme_install_running = false;
        let current = self.palette.active && self.palette.install_theme_mode;
        match result {
            Ok(themes) => {
                let themes = self.filter_removed_themes(themes);
                let count = themes.len();
                for theme in &themes {
                    if let Some(syntax) = &theme.syntect_theme {
                        self.highlighter
                            .register_theme(&theme.syntax_theme, syntax.clone());
                    }
                }
                crate::app_theme::merge_themes(&mut self.loaded_themes, themes);
                self.refresh_palette_commands();
                if !current {
                    self.notify(format!("Installed {count} theme variants."));
                    return;
                }
                self.palette.install_theme_mode = false;
                self.palette.theme_install_error = None;
                self.palette.theme_vsix_url = None;
                self.restore_palette_theme_preview();
                self.palette.active = true;
                self.palette.query = "Preferences: Color Theme".into();
                self.palette.cursor = self.palette.query.len();
                self.palette.selection_anchor = None;
                self.palette.selected = 0;
                self.palette.scroll_offset = 0;
                self.update_palette_filter();
                self.notify(format!(
                    "Installed {count} theme variants. Choose a color theme."
                ));
            }
            Err(error) => {
                let error = if self.theme_vsix_link().is_some() {
                    format!("{error}. Choose Get VSIX link to download in a browser or on another device, then install the local .vsix file.")
                } else {
                    error
                };
                if current {
                    self.palette.theme_install_error = Some(error);
                } else {
                    self.notify(format!("Theme installation failed: {error}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn pasting_a_gallery_link_does_not_open_the_install_prompt() {
        let mut app = App::test_new();
        app.open_palette();
        assert!(app.paste_into_focused_text_input("https://vscodethemes.com/e/GulajavaMinistudio.mayukaithemevsc/mayukai-mirage-gruvbox-darktooth"));
        assert!(!app.theme_install_prompt_visible());
        assert!(app.palette.filtered.is_empty());
        crate::input_pickers::handle_palette_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.palette.theme_install_running);
        crate::input_pickers::handle_palette_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.palette.active);
        assert!(!app.palette.theme_install_running);
    }

    #[test]
    fn successful_local_install_opens_the_theme_picker() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("theme.json");
        std::fs::write(
            &source,
            r##"{"name":"Offline Test","colors":{"editor.background":"#123456"}}"##,
        )
        .unwrap();
        let installed =
            crate::theme_install::install(source.to_str().unwrap(), &temp.path().join("installed"))
                .unwrap();
        let key = installed[0].config_key.clone();
        let mut app = App::test_new();
        app.open_theme_install_prompt();
        app.palette.query = "https://vscodethemes.com/e/example.theme/dark".into();
        app.show_theme_vsix_link();
        app.palette.theme_install_running = true;
        app.finish_theme_install(Ok(installed));
        assert!(app.palette.active);
        assert!(!app.theme_install_prompt_visible());
        assert!(!app.palette.theme_install_running);
        assert!(app.palette.theme_install_error.is_none());
        assert!(app.palette.theme_vsix_url.is_none());
        assert_eq!(app.palette.query, "Preferences: Color Theme");
        assert!(
            app.loaded_themes
                .iter()
                .any(|theme| theme.config_key == key)
        );
    }

    #[test]
    fn empty_install_and_worker_failure_keep_input_available_for_retry() {
        let mut app = App::test_new();
        app.open_theme_install_prompt();
        app.start_theme_install();
        assert!(app.palette.theme_install_error.is_some());
        assert!(!app.palette.theme_install_running);
        app.palette.query = "missing.vsix".into();
        app.palette.theme_install_running = true;
        app.finish_theme_install(Err("File not found".into()));
        assert!(app.palette.active);
        assert_eq!(app.palette.query, "missing.vsix");
        assert!(!app.palette.theme_install_running);
    }

    #[test]
    fn vsix_link_can_be_copied_offline_and_survives_clipboard_failure() {
        let mut app = App::test_new();
        app.open_theme_install_prompt();
        app.palette.query = "  \"https://www.vscodethemes.com/e/akamud.vscode-theme-onedark/atom-one-dark?language=py#preview\"  ".into();
        let source = app.palette.query.clone();
        let url = "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/akamud/vsextensions/vscode-theme-onedark/latest/vspackage";
        app.show_theme_vsix_link();
        assert_eq!(app.palette.theme_vsix_url.as_deref(), Some(url));
        assert!(!app.palette.theme_install_running);
        app.copy_theme_vsix_link_with(|value| {
            assert_eq!(value, url);
            Err("Clipboard unavailable".into())
        });
        assert!(
            app.modal.error.as_deref().unwrap().contains("Clipboard unavailable")
        );
        assert_eq!(app.palette.theme_vsix_url.as_deref(), Some(url));
        app.copy_theme_vsix_link_with(|value| {
            assert_eq!(value, url);
            Ok(())
        });
        assert!(app.modal.error.is_none());
        assert_eq!(app.clipboard_history.back().map(String::as_str), Some(url));
        assert_eq!(app.palette.query, source);
        assert!(!app.palette.theme_install_running);
        app.close_palette();
        app.open_theme_install_prompt();
        assert!(app.palette.theme_vsix_url.is_none());
    }

    #[test]
    fn theme_link_prompt_supports_mouse_and_keyboard_submission_and_retry() {
        use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{Terminal, backend::TestBackend};
        for mouse in [false, true] {
            let mut app = App::test_new();
            app.execute_palette_command("install_color_theme");
            // A malformed gallery URL exercises the real background error path
            // without requiring a network connection or installing user data.
            let link = "https://vscodethemes.com/e/missing-extension-id/dark";
            assert!(app.paste_into_focused_text_input(link));
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
            assert!(app.palette.theme_install_running);
            terminal
                .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("Downloading and installing theme variants"));
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                !app.palette.theme_install_running
            });
            assert!(
                app.palette
                    .theme_install_error
                    .as_deref()
                    .unwrap()
                    .contains("Use a theme link")
            );
            assert_eq!(app.palette.query, link);
            assert!(app.theme_install_prompt_visible());
        }
    }

    #[test]
    fn installer_renders_input_progress_and_retry_errors_at_small_sizes() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::test_new();
        app.open_theme_install_prompt();
        for (width, height) in [(100, 30), (40, 12), (12, 5), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for running in [false, true] {
                app.palette.theme_install_running = running;
                app.palette.theme_install_error = Some("File not found. Retry with Enter.".into());
                terminal
                    .draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app))
                    .unwrap();
                if width == 100 {
                    let text: String = terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|c| c.symbol())
                        .collect();
                    assert!(text.contains("Install Color Theme"));
                    assert!(text.contains(if running {
                        "Installing theme"
                    } else {
                        "File not found"
                    }));
                }
            }
        }
    }
}
