//! Preferences for VTComplete word suggestions and GPTComplete inline predictions.
use std::{
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::app::App;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum InlineEagerness {
    Cautious,
    #[default]
    Balanced,
    Eager,
    Always,
}

impl InlineEagerness {
    pub(crate) const ALL: [Self; 4] = [Self::Cautious, Self::Balanced, Self::Eager, Self::Always];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cautious => "Cautious",
            Self::Balanced => "Balanced",
            Self::Eager => "Eager",
            Self::Always => "Always",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Preferences {
    pub(crate) min_prefix_chars: usize,
    pub(crate) context_triggers: bool,
    pub(crate) complete_calls: bool,
    pub(crate) use_history: bool,
    pub(crate) inline_predictions: bool,
    pub(crate) inline_snoozed: bool,
    pub(crate) inline_eagerness: InlineEagerness,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            min_prefix_chars: 1,
            context_triggers: true,
            complete_calls: true,
            use_history: true,
            inline_predictions: true,
            inline_snoozed: false,
            inline_eagerness: InlineEagerness::Balanced,
        }
    }
}

fn settings_path(root: &Path) -> PathBuf {
    let directory = crate::portable::workspace_dir(root);
    if crate::remote::workspace::is_view_key(root) {
        crate::portable::data_dir()
            .join("remote")
            .join(directory.file_name().unwrap())
            .join("autocomplete")
            .join("completion-settings.json")
    } else {
        directory.join("completion-settings.json")
    }
}

impl Preferences {
    fn read(path: &Path) -> Option<Self> {
        use std::io::Read;
        const LIMIT: u64 = 64 * 1024;

        let (file, metadata) = crate::fs_ops::open_document_source(path, false).ok()?;
        if metadata.len() > LIMIT {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > LIMIT {
            return None;
        }
        let mut preferences: Self = serde_json::from_slice(&bytes).ok()?;
        preferences.min_prefix_chars = preferences.min_prefix_chars.clamp(1, 128);
        Some(preferences)
    }

    pub(crate) fn load(root: &Path) -> Self {
        let path = settings_path(root);
        let read = |path: &Path| {
            Self::read(path).or_else(|| Self::read(&path.with_extension("backup.json")))
        };
        read(&path)
            .or_else(|| {
                if crate::remote::workspace::is_view_key(root) {
                    read(&crate::portable::workspace_dir(root).join("completion-settings.json"))
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }

    fn save(self, path: &Path) -> io::Result<()> {
        use std::io::Write;

        let directory = path.parent().expect("completion settings directory");
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&mut file, &self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

/// Accept former setting IDs while exposing VTComplete IDs in current UI.
fn canonical_id(id: &str) -> &str {
    id.strip_prefix("autocomplete_")
        .and_then(|suffix| {
            FIELDS
                .iter()
                .find(|field| field.0.strip_prefix("vtcomplete_") == Some(suffix))
        })
        .map_or(id, |field| field.0)
}

pub(crate) fn number_error(id: &str, value: u64) -> Option<String> {
    (canonical_id(id) == "vtcomplete_min_prefix" && !(1..=128).contains(&value))
        .then(|| "Enter 1–128 characters.".into())
}

pub(crate) const FIELDS: &[(&str, &str, &str, &str)] = &[
    (
        "vtcomplete_min_prefix",
        "Minimum Prefix Characters",
        "number",
        "Characters needed for automatic word suggestions. Context suggestions can appear before a word starts.",
    ),
    (
        "vtcomplete_context_triggers",
        "Context Suggestions",
        "checkbox",
        "Suggest identifiers after code punctuation such as member access, namespace separators and references.",
    ),
    (
        "vtcomplete_complete_calls",
        "Complete Function Calls",
        "checkbox",
        "Add parentheses when repeated calls in the active file support it.",
    ),
    (
        "vtcomplete_use_history",
        "Use Completion History",
        "checkbox",
        "Use recent edits and accepted suggestions in the current file to improve completions.",
    ),
    (
        "vtcomplete_inline_predictions",
        "Inline Predictions",
        "checkbox",
        "Enable GPTComplete for this workspace. Alt+I requests a prediction. Tab accepts it, Ctrl+Right accepts one word, and Esc hides it.",
    ),
    (
        "vtcomplete_inline_eagerness",
        "Inline Prediction Eagerness",
        "dropdown",
        "Choose how readily predictions appear. Balanced is the default. Alt+I is always at least Eager.",
    ),
];

pub(crate) fn field(
    id: &str,
) -> Option<&'static (&'static str, &'static str, &'static str, &'static str)> {
    let id = canonical_id(id);
    FIELDS.iter().find(|field| field.0 == id)
}

fn display(preferences: Preferences, id: &str) -> String {
    let on_off = |value| if value { "On" } else { "Off" }.to_string();
    match canonical_id(id) {
        "vtcomplete_min_prefix" => preferences.min_prefix_chars.to_string(),
        "vtcomplete_context_triggers" => on_off(preferences.context_triggers),
        "vtcomplete_complete_calls" => on_off(preferences.complete_calls),
        "vtcomplete_use_history" => on_off(preferences.use_history),
        "vtcomplete_inline_predictions" => on_off(preferences.inline_predictions),
        "vtcomplete_inline_eagerness" => preferences.inline_eagerness.label().into(),
        _ => String::new(),
    }
}

impl App {
    pub(crate) fn completion_dropdown_options(&self, id: &str) -> Option<&'static [&'static str]> {
        (canonical_id(id) == "vtcomplete_inline_eagerness")
            .then_some(&["Cautious", "Balanced", "Eager", "Always"])
    }

    pub(crate) fn completion_setting_items(&self) -> Vec<(String, String, String)> {
        FIELDS
            .iter()
            .map(|&(id, label, _, _)| {
                (
                    label.into(),
                    display(self.completion_preferences, id),
                    id.into(),
                )
            })
            .collect()
    }

    pub(crate) fn completion_setting_modified(&self, id: &str) -> bool {
        display(self.completion_preferences, id) != display(Preferences::default(), id)
    }

    fn save_completion_preferences(&mut self, mut preferences: Preferences) {
        let inline_changed =
            preferences.inline_predictions != self.completion_preferences.inline_predictions;
        if inline_changed {
            preferences.inline_snoozed = false;
        }
        if preferences == self.completion_preferences {
            return;
        }
        if self.settings_persistence_enabled
            && let Err(error) = preferences.save(&settings_path(&self.explorer.root))
        {
            self.notify(format!("Could not save autocomplete settings ({error})"));
            return;
        }
        let snooze_changed =
            preferences.inline_snoozed != self.completion_preferences.inline_snoozed;
        self.completion_preferences = preferences;
        if inline_changed || snooze_changed {
            self.reset_inline_prediction_state();
        }
    }

    pub(crate) fn set_inline_predictions_enabled(&mut self, enabled: bool) {
        let mut preferences = self.completion_preferences;
        preferences.inline_predictions = enabled;
        self.save_completion_preferences(preferences);
    }

    pub(crate) fn snooze_inline_until_resumed(&mut self) {
        if self.completion_preferences.inline_predictions {
            let mut preferences = self.completion_preferences;
            preferences.inline_snoozed = true;
            self.save_completion_preferences(preferences);
        }
    }

    pub(crate) fn resume_inline_predictions(&mut self) {
        if self.completion_preferences.inline_snoozed {
            let mut preferences = self.completion_preferences;
            preferences.inline_snoozed = false;
            self.save_completion_preferences(preferences);
        } else {
            self.reset_inline_prediction_state();
        }
    }

    pub(crate) fn change_completion_setting(&mut self, id: &str) -> bool {
        let mut preferences = self.completion_preferences;
        let value = match canonical_id(id) {
            "vtcomplete_context_triggers" => &mut preferences.context_triggers,
            "vtcomplete_complete_calls" => &mut preferences.complete_calls,
            "vtcomplete_use_history" => &mut preferences.use_history,
            "vtcomplete_inline_predictions" => &mut preferences.inline_predictions,
            _ => return false,
        };
        *value = !*value;
        self.save_completion_preferences(preferences);
        true
    }

    pub(crate) fn set_completion_number(&mut self, id: &str, value: u64) -> bool {
        if canonical_id(id) != "vtcomplete_min_prefix" {
            return false;
        }
        let mut preferences = self.completion_preferences;
        preferences.min_prefix_chars = value.clamp(1, 128) as usize;
        self.save_completion_preferences(preferences);
        true
    }

    pub(crate) fn set_completion_dropdown(&mut self, id: &str, value: &str) -> bool {
        if canonical_id(id) != "vtcomplete_inline_eagerness" {
            return false;
        }
        let Some(level) = InlineEagerness::ALL
            .into_iter()
            .find(|level| level.label() == value)
        else {
            return false;
        };
        let mut preferences = self.completion_preferences;
        preferences.inline_eagerness = level;
        self.save_completion_preferences(preferences);
        true
    }

    pub(crate) fn reset_completion_setting(&mut self, id: &str) -> bool {
        let defaults = Preferences::default();
        let mut preferences = self.completion_preferences;
        match canonical_id(id) {
            "vtcomplete_min_prefix" => preferences.min_prefix_chars = defaults.min_prefix_chars,
            "vtcomplete_context_triggers" => {
                preferences.context_triggers = defaults.context_triggers
            }
            "vtcomplete_complete_calls" => preferences.complete_calls = defaults.complete_calls,
            "vtcomplete_use_history" => preferences.use_history = defaults.use_history,
            "vtcomplete_inline_predictions" => {
                preferences.inline_predictions = defaults.inline_predictions
            }
            "vtcomplete_inline_eagerness" => {
                preferences.inline_eagerness = defaults.inline_eagerness
            }
            _ => return false,
        }
        self.save_completion_preferences(preferences);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_settings_navigation::{GPTCOMPLETE_SECTION, VTCOMPLETE_SECTION};
    use crate::app_settings_ui::SETTINGS_AUTOCOMPLETE_CATEGORY;

    #[test]
    fn untimed_snooze_and_off_are_saved_for_the_workspace() {
        use crate::app_vtcomplete_inline::Availability;

        let root = tempfile::tempdir().unwrap();
        let mut app = App::new_deferred(root.path().to_owned());
        app.settings_persistence_enabled = true;
        app.snooze_inline_until_resumed();
        assert_eq!(app.inline_availability(), Availability::Snoozed);
        let saved = Preferences::load(root.path());
        assert!(saved.inline_snoozed);
        assert!(saved.inline_predictions);
        app.resume_inline_predictions();
        assert!(!Preferences::load(root.path()).inline_snoozed);

        app.snooze_inline_until_resumed();
        app.set_inline_predictions_enabled(false);
        assert_eq!(app.inline_availability(), Availability::Disabled);
        assert!(!Preferences::load(root.path()).inline_predictions);

        app.set_inline_predictions_enabled(true);
        assert_eq!(app.inline_availability(), Availability::Enabled);
        assert!(Preferences::load(root.path()).inline_predictions);
        std::fs::remove_file(settings_path(root.path())).unwrap();
    }

    #[test]
    fn old_preferences_keep_completion_controls_and_ignore_index_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("completion-settings.json");
        std::fs::write(
            &path,
            r#"{
            "enabled": true, "projectWide": true, "paused": false, "speed": "fast",
            "maxFiles": 20000, "maxMemoryMb": 48, "minPrefixChars": 3,
            "inlinePredictions": false, "inlineEagerness": "eager"
        }"#,
        )
        .unwrap();

        let preferences = Preferences::read(&path).unwrap();
        assert_eq!(preferences.min_prefix_chars, 3);
        assert!(!preferences.inline_predictions);
        assert_eq!(preferences.inline_eagerness, InlineEagerness::Eager);
        preferences.save(&path).unwrap();
        assert_eq!(Preferences::read(&path), Some(preferences));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("projectWide")
        );
    }

    #[test]
    fn autocomplete_pages_group_shared_popup_and_inline_controls() {
        let mut app = App::test_new();
        let items = app.settings_items();
        let category = SETTINGS_AUTOCOMPLETE_CATEGORY;
        for (section, expected) in [
            ("General", &["quick_suggestions"][..]),
            (
                VTCOMPLETE_SECTION,
                &[
                    "vtcomplete_min_prefix",
                    "vtcomplete_context_triggers",
                    "vtcomplete_complete_calls",
                    "vtcomplete_use_history",
                ][..],
            ),
            (
                GPTCOMPLETE_SECTION,
                &[
                    "vtcomplete_inline_predictions",
                    "vtcomplete_inline_eagerness",
                ][..],
            ),
        ] {
            app.select_settings_page(category, Some(section));
            let visible: Vec<_> = app
                .settings_filtered_indices_with_items(category, &items)
                .into_iter()
                .map(|index| items[index].2.as_str())
                .collect();
            assert_eq!(visible, expected, "{section}");
        }
        assert!(!app.set_completion_dropdown("vtcomplete_sources", "Entire workspace"));

        app.select_settings_page(category, Some(VTCOMPLETE_SECTION));
        app.settings_editor_search = "GPTComplete".into();
        let matches: Vec<_> = app
            .settings_filtered_indices_with_items(category, &items)
            .into_iter()
            .map(|index| items[index].2.as_str())
            .collect();
        assert_eq!(
            matches,
            [
                "quick_suggestions",
                "vtcomplete_inline_predictions",
                "vtcomplete_inline_eagerness"
            ]
        );

        app.settings_editor_search.clear();
        app.begin_numeric_setting_edit("autocomplete_min_prefix", "1");
        app.settings_value_buffer = "129".into();
        assert!(!app.commit_numeric_setting_edit());
        app.settings_value_buffer = "3".into();
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.completion_preferences.min_prefix_chars, 3);
        app.toggle_setting("autocomplete_context_triggers");
        assert!(!app.completion_preferences.context_triggers);
        assert!(app.reset_setting_in_selected_scope("vtcomplete_context_triggers"));
        assert!(app.completion_preferences.context_triggers);
    }
}
