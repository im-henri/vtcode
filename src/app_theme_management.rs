//! Theme picker identity, confirmed variant removal, and picker action targets.
use crate::{app::App, app_theme::ThemeFile};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};
use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct State {
    pub picker_keys: Vec<String>,
    pub picker_removable: Vec<bool>,
    pub delete_hits: Vec<(Rect, usize)>,
    pub pending: Option<Removal>,
    #[cfg(test)]
    pub storage_root: Option<PathBuf>,
}

pub(crate) struct Removal {
    pub key: String,
    pub name: String,
    pub fallback: Option<String>,
    row: usize,
    scroll: usize,
}

impl App {
    fn theme_storage_root(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(root) = &self.theme_management.storage_root {
            return root.clone();
        }
        crate::portable::theme_extensions_dir()
    }

    pub(crate) fn filter_removed_themes(&mut self, mut themes: Vec<ThemeFile>) -> Vec<ThemeFile> {
        // Test applications never read or write the user's portable installation.
        #[cfg(test)]
        if self.theme_management.storage_root.is_none() {
            return themes;
        }
        match crate::theme_catalog::removed(&self.theme_storage_root()) {
            Ok(removed) => {
                themes.retain(|theme| theme.builtin || !removed.contains(&theme.config_key));
                if self
                    .startup_theme_placeholder
                    .as_ref()
                    .is_some_and(|key| removed.contains(key))
                {
                    let key = self.startup_theme_placeholder.take().unwrap();
                    self.loaded_themes.retain(|theme| theme.config_key != key);
                    self.ensure_bundled_theme("dark");
                    self.set_theme_by_key("dark");
                }
                themes
            }
            Err(error) => {
                log_warn!("{error}");
                Vec::new()
            }
        }
    }

    /// Resolve by stable identity while the picker is open. Names need not be unique.
    pub(crate) fn settings_theme_key(&self, value: &str) -> Option<String> {
        if self.settings_dropdown_setting_id.as_deref() == Some("theme")
            && self
                .settings_dropdown_options
                .get(self.settings_dropdown_selected)
                .is_some_and(|label| label.eq_ignore_ascii_case(value))
            && let Some(key) = self
                .theme_management
                .picker_keys
                .get(self.settings_dropdown_selected)
        {
            return Some(key.clone());
        }
        self.loaded_themes
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(value))
            .map(|theme| theme.config_key.clone())
    }

    pub(crate) fn theme_picker_deletable(&self, row: usize) -> bool {
        self.theme_management
            .picker_removable
            .get(row)
            .copied()
            .unwrap_or(false)
    }

    pub(crate) fn request_theme_removal(&mut self, row: usize) {
        if self.settings_dropdown_setting_id.as_deref() != Some("theme") {
            return;
        }
        let Some(key) = self.theme_management.picker_keys.get(row).cloned() else {
            return;
        };
        let Some(theme) = self
            .loaded_themes
            .iter()
            .find(|theme| theme.config_key == key && !theme.builtin)
        else {
            return;
        };
        let name = theme.name.clone();
        let fallback = (self.committed_theme_key() == key).then(|| {
            if theme.is_light {
                "light"
            } else if theme.high_contrast {
                "high-contrast"
            } else {
                "dark"
            }
            .to_string()
        });
        let scroll = self.settings_dropdown_scroll;
        // Restore a highlighted preview before presenting a destructive action.
        self.close_settings_dropdown();
        self.restore_palette_theme_preview();
        self.clear_drag_states();
        self.active_pane = crate::app::ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.theme_management.pending = Some(Removal {
            key,
            name,
            fallback,
            row,
            scroll,
        });
        self.modal = Default::default();
    }

    fn ensure_bundled_theme(&mut self, key: &str) {
        if !self
            .loaded_themes
            .iter()
            .any(|theme| theme.config_key == key && theme.builtin)
            && let Some(theme) = crate::app_theme_loader::builtin_theme(key)
        {
            crate::app_theme::merge_themes(&mut self.loaded_themes, vec![theme.clone()]);
        }
    }

    pub(crate) fn confirm_theme_removal(&mut self) {
        let Some(pending) = &self.theme_management.pending else {
            return;
        };
        let key = pending.key.clone();
        let name = pending.name.clone();
        // Recheck provenance in case an asynchronous catalog update replaced it.
        if !self
            .loaded_themes
            .iter()
            .any(|theme| theme.config_key == key && !theme.builtin)
        {
            self.cancel_theme_removal();
            return;
        }
        if let Err(error) = crate::theme_catalog::remove(&self.theme_storage_root(), &key) {
            self.modal.error = Some(format!("Could not delete theme: {error}"));
            return;
        }
        self.loaded_themes.retain(|theme| theme.config_key != key);
        // External legacy definitions can override a canonical bundled key.
        // Removing such an override reveals the original embedded definition.
        if crate::app_theme_loader::builtin_theme(&key).is_some() {
            self.ensure_bundled_theme(&key);
        }
        let mut preference_error = None;
        if self.theme_key == key {
            let fallback = self
                .theme_management
                .pending
                .as_ref()
                .and_then(|p| p.fallback.as_deref())
                .unwrap_or("dark")
                .to_string();
            self.ensure_bundled_theme(&fallback);
            self.set_theme_by_key(&fallback);
            use crate::settings::SettingsScope;
            let mut scopes = Vec::new();
            if self.settings_global.theme.as_deref() == Some(&key) {
                scopes.push(SettingsScope::Global);
            }
            if self.settings_workspace.theme.as_deref() == Some(&key) {
                scopes.push(SettingsScope::Workspace);
            }
            if scopes.is_empty() {
                scopes.push(self.settings_scope);
            }
            for scope in scopes {
                if let Err(error) = self.patch_settings_scope(scope, "theme", |settings| {
                    settings.theme = Some(fallback.clone());
                }) {
                    preference_error = Some(error);
                }
            }
            self.set_theme_by_key(&fallback);
        }
        if self.startup_theme_placeholder.as_deref() == Some(&key) {
            self.startup_theme_placeholder = None;
        }
        self.refresh_palette_commands();
        self.cancel_theme_removal();
        if let Some(error) = preference_error {
            self.notify_warn(format!(
                "Deleted theme: {name}. Could not save fallback preference: {error}"
            ));
        } else {
            self.notify(format!("Deleted theme: {name}"));
        }
    }

    pub(crate) fn cancel_theme_removal(&mut self) {
        let Some(pending) = self.theme_management.pending.take() else {
            return;
        };
        self.modal = Default::default();
        if self.settings_editor_active {
            self.open_settings_dropdown("theme", &self.theme_name.clone());
            self.settings_dropdown_selected = pending
                .row
                .min(self.settings_dropdown_options.len().saturating_sub(1));
            self.settings_dropdown_scroll = pending.scroll.min(self.settings_dropdown_selected);
        }
    }

    pub(crate) fn draw_theme_picker_actions(&mut self, f: &mut Frame, area: Rect) {
        self.theme_management.delete_hits.clear();
        if self.settings_dropdown_setting_id.as_deref() != Some("theme") || area.is_empty() {
            return;
        }
        let scrollbar = if self.settings_dropdown_options.len() > area.height as usize {
            2.min(area.width.saturating_sub(1))
        } else {
            0
        };
        let width = area.width.saturating_sub(scrollbar);
        let action_width = 11.min(width.saturating_sub(2));
        for visible in 0..area.height as usize {
            let row = self.settings_dropdown_scroll + visible;
            let Some(name) = self.settings_dropdown_options.get(row) else {
                break;
            };
            let y = area.y + visible as u16;
            let style = f.buffer_mut()[(area.x, y)].style();
            let label_width = width.saturating_sub(action_width);
            let label =
                crate::ui_pickers::truncate_to_width(name, label_width.saturating_sub(2) as usize);
            let padding = width
                .saturating_sub(1 + unicode_width::UnicodeWidthStr::width(label.as_str()) as u16);
            f.render_widget(
                Paragraph::new(format!(
                    " {label}{}",
                    crate::helpers::spaces(padding as usize)
                ))
                .style(style),
                Rect::new(area.x, y, width, 1),
            );
            let action = Rect::new(area.x + label_width, y, action_width, 1);
            if self.theme_picker_deletable(row) {
                let hover = action.contains((self.mouse_col, self.mouse_row).into());
                crate::ui_widgets::draw_dialog_button(
                    f,
                    action,
                    "Delete…",
                    &self.colors,
                    false,
                    hover,
                    true,
                    true,
                );
                self.theme_management.delete_hits.push((action, row));
            } else {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        " Bundled",
                        Style::default().fg(self
                            .colors
                            .ui
                            .description_color(style.bg.unwrap_or(self.colors.bg_editor))),
                    )))
                    .style(style),
                    action,
                );
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

    fn fixture() -> (tempfile::TempDir, App, Vec<ThemeFile>) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("package.json"), r#"{"publisher":"sample","name":"pack","contributes":{"themes":[{"label":"Pack Dark","uiTheme":"vs-dark","path":"dark.json"},{"label":"Pack Light","uiTheme":"vs","path":"light.json"}]}}"#).unwrap();
        std::fs::write(
            source.join("dark.json"),
            r##"{"colors":{"editor.background":"#102030"},"tokenColors":[]}"##,
        )
        .unwrap();
        // A surviving variant includes the removed variant's file.
        std::fs::write(
            source.join("light.json"),
            r##"{"include":"dark.json","colors":{"editor.background":"#eeeeee"}}"##,
        )
        .unwrap();
        let root = temp.path().join(".vtcode/theme-extensions");
        let themes = crate::theme_install::install(source.to_str().unwrap(), &root).unwrap();
        let mut app = App::test_new();
        app.loaded_themes = crate::app_theme_loader::load_builtin_themes();
        crate::app_theme::merge_themes(&mut app.loaded_themes, themes.clone());
        app.theme_management.storage_root = Some(root);
        app.settings_editor_active = true;
        app.active_pane = crate::app::ActivePane::Editor;
        app.settings_category_selected = 2;
        app.settings_category_collapsed[2] = false;
        app.settings_selected = app
            .settings_items()
            .iter()
            .position(|(_, _, id)| id == "theme")
            .unwrap();
        app.set_theme_by_key("dark");
        app.open_settings_dropdown("theme", &app.theme_name.clone());
        (temp, app, themes)
    }

    fn row(app: &App, key: &str) -> usize {
        app.theme_management
            .picker_keys
            .iter()
            .position(|k| k == key)
            .unwrap()
    }

    fn key(app: &mut App, code: KeyCode) {
        assert_eq!(
            crate::ui_modal::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE)),
            Some(false)
        );
    }

    fn render_picker(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| crate::ui_settings::draw_settings_editor(f, app, f.area()))
            .unwrap();
        terminal
    }

    fn click(app: &mut App, area: Rect) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + area.width / 2,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    #[test]
    fn picker_delete_requires_confirmation_defaults_to_cancel_and_restores_preview() {
        let (_temp, mut app, themes) = fixture();
        let theme = &themes[0];
        let target = row(&app, &theme.config_key);
        app.settings_dropdown_selected = target;
        app.preview_settings_dropdown_value("theme", &theme.name);
        assert_eq!(app.theme_key, theme.config_key);
        render_picker(&mut app, 100, 35);
        let rect = app
            .theme_management
            .delete_hits
            .iter()
            .find(|(_, row)| *row == target)
            .unwrap()
            .0;
        click(&mut app, rect);
        assert_eq!(
            crate::ui_modal::active(&app),
            Some(crate::ui_modal::Kind::ThemeDelete)
        );
        assert_eq!(app.theme_key, "dark");
        assert!(
            crate::theme_catalog::removed(&app.theme_storage_root())
                .unwrap()
                .is_empty()
        );
        key(&mut app, KeyCode::Enter); // Safe default: Cancel.
        assert!(app.theme_management.pending.is_none());
        assert!(app.settings_dropdown_open);
        assert_eq!(app.theme_key, "dark");
        assert!(
            app.loaded_themes
                .iter()
                .any(|t| t.config_key == theme.config_key)
        );
        assert!(
            crate::theme_catalog::removed(&app.theme_storage_root())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn delete_key_requests_confirmation_and_is_inert_on_bundled_rows() {
        let (_temp, mut app, themes) = fixture();
        let event = KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE);
        app.settings_dropdown_selected = row(&app, "dark");
        crate::input_dialogs::handle_settings_editor_key(&mut app, event);
        assert!(app.theme_management.pending.is_none());
        app.settings_dropdown_selected = row(&app, &themes[0].config_key);
        crate::input_dialogs::handle_settings_editor_key(&mut app, event);
        assert_eq!(
            crate::ui_modal::active(&app),
            Some(crate::ui_modal::Kind::ThemeDelete)
        );
        key(&mut app, KeyCode::Esc);
        assert!(app.settings_dropdown_open);
        assert!(
            crate::theme_catalog::removed(&app.theme_storage_root())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn confirmed_removal_persists_keeps_shared_dependencies_and_can_be_reinstalled() {
        let (temp, mut app, themes) = fixture();
        let target = themes[0].clone();
        app.request_theme_removal(row(&app, &target.config_key));
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Enter);
        assert!(
            !app.loaded_themes
                .iter()
                .any(|theme| theme.config_key == target.config_key)
        );
        assert!(app.settings_dropdown_open);
        assert!(
            crate::app_theme_loader::load_installed_theme(temp.path(), &target.config_key)
                .is_none()
        );
        let light =
            crate::app_theme_loader::load_installed_theme(temp.path(), &themes[1].config_key)
                .unwrap();
        assert_eq!(light.colors.bg_editor, "#EEEEEE");
        assert!(
            app.theme_storage_root()
                .join("sample.pack/dark.json")
                .is_file()
        );
        // Results captured before confirmation must not resurrect the variant.
        app.install_external_themes(themes.clone());
        app.finish_theme_install(Ok(themes));
        assert!(
            !app.loaded_themes
                .iter()
                .any(|theme| theme.config_key == target.config_key)
        );
        let reinstalled = crate::theme_install::install(
            temp.path().join("source").to_str().unwrap(),
            &app.theme_storage_root(),
        )
        .unwrap();
        app.finish_theme_install(Ok(reinstalled));
        assert!(
            app.loaded_themes
                .iter()
                .any(|theme| theme.config_key == target.config_key)
        );
        assert!(
            crate::app_theme_loader::load_installed_theme(temp.path(), &target.config_key)
                .is_some()
        );
    }

    #[test]
    fn removing_active_light_theme_uses_and_persists_bundled_light_fallback() {
        let (_temp, mut app, themes) = fixture();
        let theme = &themes[1];
        app.close_settings_dropdown();
        app.apply_settings_dropdown_value("theme", &theme.name);
        assert_eq!(app.theme_key, theme.config_key);
        app.open_settings_dropdown("theme", &theme.name);
        app.request_theme_removal(row(&app, &theme.config_key));
        assert_eq!(
            app.theme_management
                .pending
                .as_ref()
                .unwrap()
                .fallback
                .as_deref(),
            Some("light")
        );
        app.confirm_theme_removal();
        assert_eq!(app.theme_key, "light");
        assert_eq!(app.settings_effective.theme.as_deref(), Some("light"));
        app.close_settings_dropdown();
        assert_eq!(app.theme_key, "light");
    }

    #[test]
    fn active_workspace_theme_fallback_does_not_overwrite_an_unrelated_global_choice() {
        let (_temp, mut app, themes) = fixture();
        app.close_settings_dropdown();
        app.settings_global.theme = Some("monokai".into());
        app.settings_workspace.theme = Some(themes[1].config_key.clone());
        app.apply_effective_settings();
        app.open_settings_dropdown("theme", &app.theme_name.clone());
        app.request_theme_removal(row(&app, &themes[1].config_key));
        app.confirm_theme_removal();
        assert_eq!(app.theme_key, "light");
        assert_eq!(app.settings_global.theme.as_deref(), Some("monokai"));
        assert_eq!(app.settings_workspace.theme.as_deref(), Some("light"));
        app.apply_effective_settings();
        assert_eq!(app.theme_key, "light");
    }

    #[test]
    fn bundled_themes_cannot_be_removed_and_inactive_removal_keeps_current_theme() {
        let (_temp, mut app, themes) = fixture();
        for key in ["dark", "light", "high-contrast", "monokai"] {
            let row = row(&app, key);
            assert!(!app.theme_picker_deletable(row));
            app.request_theme_removal(row);
            assert!(app.theme_management.pending.is_none());
        }
        app.request_theme_removal(row(&app, &themes[1].config_key));
        app.confirm_theme_removal();
        assert_eq!(app.theme_key, "dark");
        assert_eq!(app.loaded_themes.iter().filter(|t| t.builtin).count(), 4);
    }

    #[test]
    fn external_override_removal_reveals_the_original_bundled_theme() {
        let (_temp, mut app, themes) = fixture();
        app.close_settings_dropdown();
        let mut override_theme = themes[0].clone();
        override_theme.config_key = "dark".into();
        crate::app_theme::merge_themes(&mut app.loaded_themes, vec![override_theme]);
        app.set_theme_by_key("dark");
        app.open_settings_dropdown("theme", &app.theme_name.clone());
        app.request_theme_removal(row(&app, "dark"));
        assert!(app.theme_management.pending.is_some());
        app.confirm_theme_removal();
        assert_eq!(app.theme_key, "dark");
        assert_eq!(app.theme_name, "Dark+");
        assert!(!app.theme_picker_deletable(row(&app, "dark")));
        assert_eq!(app.loaded_themes.iter().filter(|t| t.builtin).count(), 4);
    }

    #[test]
    fn a_removed_saved_theme_uses_the_bundled_fallback_after_catalog_loading() {
        let (_temp, mut app, themes) = fixture();
        app.close_settings_dropdown();
        let removed = &themes[0].config_key;
        crate::theme_catalog::remove(&app.theme_storage_root(), removed).unwrap();
        app.startup_theme_placeholder = Some(removed.clone());
        app.set_theme_by_key(removed);
        assert!(app.install_external_themes(themes.clone()));
        assert_eq!(app.theme_key, "dark");
        assert!(app.startup_theme_placeholder.is_none());
        assert!(
            !app.loaded_themes
                .iter()
                .any(|theme| &theme.config_key == removed)
        );
    }

    #[test]
    fn scrolling_a_large_theme_picker_preserves_current_theme_and_delete_targets() {
        let (_temp, mut app, themes) = fixture();
        app.close_settings_dropdown();
        for index in 0..60 {
            let mut theme = themes[0].clone();
            theme.config_key = format!("variant-{index}");
            theme.name = format!("Variant {index}");
            app.loaded_themes.push(theme);
        }
        app.open_settings_dropdown("theme", "Dark+");
        render_picker(&mut app, 80, 25);
        let area = app.settings_dropdown_area.unwrap();
        let settings_scroll = app.settings_editor_scroll;
        for _ in 0..20 {
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: area.x + 1,
                    row: area.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
        }
        assert!(app.settings_dropdown_scroll > 0);
        assert_eq!(app.settings_editor_scroll, settings_scroll);
        assert_eq!(app.theme_key, "dark");
        render_picker(&mut app, 80, 25);
        let &(rect, row) = app.theme_management.delete_hits.last().unwrap();
        let key = app.theme_management.picker_keys[row].clone();
        click(&mut app, rect);
        assert_eq!(app.theme_management.pending.as_ref().unwrap().key, key);
    }

    #[test]
    fn failed_removal_keeps_catalog_and_dialog_available_for_retry() {
        let (_temp, mut app, themes) = fixture();
        app.request_theme_removal(row(&app, &themes[0].config_key));
        std::fs::write(
            app.theme_storage_root().join(".removed-themes.json"),
            "broken",
        )
        .unwrap();
        app.confirm_theme_removal();
        assert!(
            app.modal
                .error
                .as_deref()
                .unwrap()
                .contains("Could not delete theme")
        );
        assert!(app.theme_management.pending.is_some());
        assert!(
            app.loaded_themes
                .iter()
                .any(|theme| theme.config_key == themes[0].config_key)
        );
        key(&mut app, KeyCode::Esc);
        assert!(app.settings_dropdown_open);
    }

    #[test]
    fn duplicate_display_names_select_preview_and_delete_by_identity() {
        let (_temp, mut app, themes) = fixture();
        app.close_settings_dropdown();
        for theme in app.loaded_themes.iter_mut().filter(|theme| !theme.builtin) {
            theme.name = "Same name".into();
        }
        app.open_settings_dropdown("theme", "Dark+");
        let target = row(&app, &themes[1].config_key);
        app.settings_dropdown_selected = target;
        app.preview_settings_dropdown_value("theme", "Same name");
        assert_eq!(app.theme_key, themes[1].config_key);
        app.select_settings_dropdown_option(target);
        assert_eq!(app.theme_key, themes[1].config_key);
        app.open_settings_dropdown("theme", "Same name");
        assert_eq!(app.settings_dropdown_selected, target);
        app.request_theme_removal(target);
        app.confirm_theme_removal();
        assert!(
            !app.loaded_themes
                .iter()
                .any(|theme| theme.config_key == themes[1].config_key)
        );
        assert!(
            app.loaded_themes
                .iter()
                .any(|theme| theme.config_key == themes[0].config_key)
        );
    }

    #[test]
    fn picker_and_confirmation_actions_fit_small_dark_and_light_windows() {
        for base in ["dark", "light"] {
            for (width, height) in [(100, 35), (45, 22), (25, 15)] {
                let (_temp, mut app, themes) = fixture();
                app.close_settings_dropdown();
                app.set_theme_by_key(base);
                app.open_settings_dropdown("theme", &app.theme_name.clone());
                let target = row(&app, &themes[1].config_key);
                app.settings_dropdown_selected = target;
                app.settings_dropdown_scroll = target.saturating_sub(1);
                let picker = render_picker(&mut app, width, height);
                export_preview(&picker, &format!("{base}-{width}-picker"));
                let rect = app
                    .theme_management
                    .delete_hits
                    .iter()
                    .find(|(_, row)| *row == target)
                    .unwrap()
                    .0;
                let text: String = (rect.x..rect.right())
                    .map(|x| picker.backend().buffer()[(x, rect.y)].symbol())
                    .collect();
                assert!(text.contains("Delete"), "{base} {width}x{height}: {text:?}");
                click(&mut app, rect);
                let mut modal = Terminal::new(TestBackend::new(width, height)).unwrap();
                modal
                    .draw(|f| crate::ui_modal::draw_active(f, &mut app))
                    .unwrap();
                export_preview(&modal, &format!("{base}-{width}-confirmation"));
                assert_eq!(app.modal.buttons.len(), 2);
                assert_eq!(app.modal.focus, 1);
                for (button, _) in &app.modal.buttons {
                    assert!(
                        modal
                            .backend()
                            .buffer()
                            .area
                            .contains((button.x, button.y).into())
                    );
                }
            }
        }
    }

    fn export_preview(terminal: &Terminal<TestBackend>, name: &str) {
        let Some(directory) = std::env::var_os("VTCODE_THEME_MANAGEMENT_PREVIEW_DIR") else {
            return;
        };
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let buffer = terminal.backend().buffer();
        let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
            "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
            "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
        })).collect();
        let scene = serde_json::json!({ "width": buffer.area.width, "height": buffer.area.height, "cells": cells });
        std::fs::write(
            directory.join(format!("{name}.json")),
            serde_json::to_vec(&scene).unwrap(),
        )
        .unwrap();
    }
}
