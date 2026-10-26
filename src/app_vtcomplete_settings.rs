//! One set of project completion preferences shared by the Settings tab and
//! the index popup. Neither surface keeps a second copy of these values.
use crate::{
    app::App,
    completion_index::{IndexSettings, IndexSpeed},
};

/// User-facing modes combine the worker switch and its pause state. Pausing
/// retains learned suggestions; turning learning off also stops using them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LearningMode {
    Off,
    Paused,
    On,
}

impl LearningMode {
    pub(crate) fn from_settings(settings: &IndexSettings) -> Self {
        if !settings.enabled {
            Self::Off
        } else if settings.paused {
            Self::Paused
        } else {
            Self::On
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Paused => "Paused",
            Self::On => "On",
        }
    }
}

/// Accept former setting IDs while exposing VTComplete IDs in current UI.
pub(crate) fn canonical_id(id: &str) -> &str {
    id.strip_prefix("autocomplete_")
        .and_then(|suffix| {
            FIELDS
                .iter()
                .find(|field| field.0.strip_prefix("vtcomplete_") == Some(suffix))
        })
        .map_or(id, |field| field.0)
}

pub(crate) fn dropdown_options(id: &str) -> Option<&'static [&'static str]> {
    let id = canonical_id(id);
    match id {
        "vtcomplete_learning_mode" => Some(&["Off", "Paused", "On"]),
        "vtcomplete_sources" => Some(&["Files you open", "Entire workspace"]),
        "vtcomplete_index_speed" => Some(&["Low CPU", "Balanced", "Faster"]),
        _ => None,
    }
}

pub(crate) fn number_range(id: &str) -> Option<(u64, u64, &'static str)> {
    let id = canonical_id(id);
    match id {
        "vtcomplete_min_prefix" => Some((1, 128, "characters")),
        "vtcomplete_max_files" => Some((1, 100_000, "files")),
        "vtcomplete_max_file_mb" => Some((1, 1024, "MiB per source file")),
        "vtcomplete_memory_mb" => Some((4, 256, "MiB RAM")),
        _ => None,
    }
}

pub(crate) fn number_error(id: &str, value: u64) -> Option<String> {
    let (min, max, unit) = number_range(id)?;
    (!(min..=max).contains(&value)).then(|| format!("Enter {min}–{max} {unit}."))
}

pub(crate) const FIELDS: &[(&str, &str, &str, &str)] = &[
    (
        "vtcomplete_min_prefix",
        "Minimum Prefix Characters",
        "number",
        "Characters needed for automatic word suggestions. Structural triggers can show context suggestions before a word starts. Saved for this project.",
    ),
    (
        "vtcomplete_context_triggers",
        "Context Suggestions",
        "checkbox",
        "Suggest identifiers after references, member access, namespace separators and other code punctuation, including & in Rust. Comments and strings stay quiet. Saved for this project.",
    ),
    (
        "vtcomplete_complete_calls",
        "Complete Function Calls",
        "checkbox",
        "Append () when repeated calling patterns support it, placing the caret inside the parentheses. Saved for this project.",
    ),
    (
        "vtcomplete_use_history",
        "Learn from Accepted Completions",
        "checkbox",
        "Prioritize completions you have chosen in the same surrounding context during this session. Saved for this project.",
    ),
    (
        "vtcomplete_learning_mode",
        "File Learning",
        "dropdown",
        "On by default: automatically learn files you open and keep the active file ready. Paused retains existing suggestions without updates. Off also stops using learned-file suggestions. Current-buffer suggestions and snippets remain available in every mode. Saved for this workspace.",
    ),
    (
        "vtcomplete_sources",
        "Learn from Files",
        "dropdown",
        "Files you open is the default; no workspace scan. Entire workspace opts into gradual file discovery when learning is On. Ignore rules and resource limits apply. Changing scope does not start learning or erase previously indexed files. Saved for this workspace.",
    ),
    (
        "vtcomplete_index_speed",
        "File Learning CPU Use",
        "dropdown",
        "Controls CPU use for file learning and cache loading, not prediction. Low CPU takes longer; Faster uses more CPU. Sleeps when idle. Saved for this workspace.",
    ),
    (
        "vtcomplete_max_files",
        "Maximum Indexed Files",
        "number",
        "Maximum number of source files kept in the index at once, from 1 to 100000. All files share the separate index RAM budget. Saved for this workspace.",
    ),
    (
        "vtcomplete_max_file_mb",
        "Source File Size Limit (MiB per file)",
        "number",
        "Maximum on-disk size of each source file read for learning, from 1 to 1024 MiB. Larger source files are skipped. This is not the size of the generated disk cache or its RAM use. Saved for this workspace.",
    ),
    (
        "vtcomplete_memory_mb",
        "Index RAM Budget (MiB)",
        "number",
        "Estimated RAM budget shared by all indexed files, from 4 to 256 MiB. Includes retained words, examples and indexing overhead. This is not disk space or total editor RAM; editor buffers are separate. Saved for this workspace.",
    ),
    (
        "vtcomplete_status",
        "Manage VTComplete Learning",
        "action",
        "Manage learning, view indexed files, remove their learned data, and inspect source-file and RAM limits.",
    ),
    (
        "vtcomplete_cancel",
        "Discard Pending File Updates",
        "action",
        "Pause file learning and discard pending updates and discovery work. Already learned data stays available.",
    ),
    (
        "vtcomplete_rescan",
        "Refresh Learned Files",
        "action",
        "When file learning is On, check known files for changes. Entire workspace also discovers new files. Start learning first if it is paused or off.",
    ),
];

pub(crate) fn field(
    id: &str,
) -> Option<&'static (&'static str, &'static str, &'static str, &'static str)> {
    let id = canonical_id(id);
    FIELDS.iter().find(|field| field.0 == id)
}

fn display(settings: &IndexSettings, id: &str) -> String {
    let id = canonical_id(id);
    let on_off = |value| if value { "On" } else { "Off" }.to_string();
    match id {
        "vtcomplete_min_prefix" => settings.min_prefix_chars.to_string(),
        "vtcomplete_context_triggers" => on_off(settings.context_triggers),
        "vtcomplete_complete_calls" => on_off(settings.complete_calls),
        "vtcomplete_use_history" => on_off(settings.use_history),
        "vtcomplete_learning_mode" => LearningMode::from_settings(settings).label().into(),
        "vtcomplete_sources" => if settings.project_wide {
            "Entire workspace"
        } else {
            "Files you open"
        }
        .into(),
        "vtcomplete_index_speed" => match settings.speed {
            IndexSpeed::Low => "Low CPU",
            IndexSpeed::Balanced => "Balanced",
            IndexSpeed::Fast => "Faster",
        }
        .into(),
        "vtcomplete_max_files" => settings.max_files.to_string(),
        "vtcomplete_max_file_mb" => settings.max_file_bytes.div_ceil(1024 * 1024).to_string(),
        "vtcomplete_memory_mb" => settings.max_memory_mb.to_string(),
        "vtcomplete_status" => "Manage…".into(),
        "vtcomplete_cancel" => "Discard Pending Updates".into(),
        "vtcomplete_rescan" => "Refresh Files".into(),
        _ => String::new(),
    }
}

impl App {
    pub(crate) fn completion_dropdown_options(&self, id: &str) -> Option<&'static [&'static str]> {
        let id = canonical_id(id);
        if id == "vtcomplete_sources" && self.remote_workspace.is_some() {
            Some(&["Files you open"])
        } else {
            dropdown_options(id)
        }
    }

    pub(crate) fn completion_setting_items(&self) -> Vec<(String, String, String)> {
        let settings = self.completion_index.settings();
        FIELDS
            .iter()
            .map(|&(id, label, _, _)| (label.into(), display(&settings, id), id.into()))
            .collect()
    }

    pub(crate) fn completion_setting_modified(&self, id: &str) -> bool {
        field(id).is_some_and(|field| field.2 != "action")
            && display(&self.completion_index.settings(), id)
                != display(&IndexSettings::default(), id)
    }

    pub(crate) fn change_completion_setting(&mut self, id: &str) -> bool {
        let id = canonical_id(id);
        let mut settings = self.completion_index.settings();
        match id {
            "vtcomplete_context_triggers" => settings.context_triggers = !settings.context_triggers,
            "vtcomplete_complete_calls" => settings.complete_calls = !settings.complete_calls,
            "vtcomplete_use_history" => settings.use_history = !settings.use_history,
            "vtcomplete_status" => {
                crate::ui_vtcomplete_index::open(self);
                return true;
            }
            "vtcomplete_cancel" => {
                self.discard_completion_updates();
                return true;
            }
            "vtcomplete_rescan" => {
                if !self.refresh_completion_files() {
                    self.notify("Start file learning before refreshing files");
                }
                return true;
            }
            _ => return false,
        }
        self.completion_index.configure(settings);
        true
    }

    pub(crate) fn set_completion_number(&mut self, id: &str, value: u64) -> bool {
        let id = canonical_id(id);
        let mut settings = self.completion_index.settings();
        match id {
            "vtcomplete_min_prefix" => settings.min_prefix_chars = value.clamp(1, 128) as usize,
            "vtcomplete_max_files" => settings.max_files = value.clamp(1, 100_000) as usize,
            "vtcomplete_max_file_mb" => {
                settings.max_file_bytes = value.clamp(1, 1024) as usize * 1024 * 1024
            }
            "vtcomplete_memory_mb" => settings.max_memory_mb = value.clamp(4, 256) as usize,
            _ => return false,
        }
        self.completion_index.configure(settings);
        true
    }

    pub(crate) fn set_completion_dropdown(&mut self, id: &str, value: &str) -> bool {
        let id = canonical_id(id);
        if id == "vtcomplete_sources"
            && self.remote_workspace.is_some()
            && value != "Files you open"
        {
            return false;
        }
        let mut settings = self.completion_index.settings();
        match (id, value) {
            ("vtcomplete_learning_mode", "Off") => {
                settings.enabled = false;
                settings.paused = true;
            }
            ("vtcomplete_learning_mode", "Paused") => {
                settings.enabled = true;
                settings.paused = true;
            }
            ("vtcomplete_learning_mode", "On") => {
                settings.enabled = true;
                settings.paused = false;
            }
            ("vtcomplete_sources", "Files you open") => settings.project_wide = false,
            ("vtcomplete_sources", "Entire workspace") => settings.project_wide = true,
            ("vtcomplete_index_speed", "Low CPU") => settings.speed = IndexSpeed::Low,
            ("vtcomplete_index_speed", "Balanced") => settings.speed = IndexSpeed::Balanced,
            ("vtcomplete_index_speed", "Faster") => settings.speed = IndexSpeed::Fast,
            _ => return false,
        }
        self.completion_index.configure(settings);
        if id == "vtcomplete_learning_mode" && value == "On" {
            self.completion_index.rescan();
        }
        true
    }

    pub(crate) fn discard_completion_updates(&mut self) {
        let mut settings = self.completion_index.settings();
        settings.paused = true;
        self.completion_index.configure(settings);
        self.completion_index.cancel();
    }

    pub(crate) fn refresh_completion_files(&mut self) -> bool {
        if LearningMode::from_settings(&self.completion_index.settings()) != LearningMode::On {
            return false;
        }
        if self.completion_index.buffer_only() {
            self.completion_index_sync.refresh_buffers();
        }
        self.completion_index.rescan();
        true
    }

    pub(crate) fn reset_completion_setting(&mut self, id: &str) -> bool {
        let id = canonical_id(id);
        let defaults = IndexSettings::default();
        if self.set_completion_number(
            id,
            match id {
                "vtcomplete_min_prefix" => defaults.min_prefix_chars as u64,
                "vtcomplete_max_files" => defaults.max_files as u64,
                "vtcomplete_max_file_mb" => defaults.max_file_bytes as u64 / 1024 / 1024,
                "vtcomplete_memory_mb" => defaults.max_memory_mb as u64,
                _ => 0,
            },
        ) {
            return true;
        }
        if dropdown_options(id).is_some() {
            return self.set_completion_dropdown(id, &display(&defaults, id));
        }
        if field(id).is_some_and(|field| field.2 == "checkbox") {
            if self.completion_setting_modified(id) {
                self.change_completion_setting(id);
            }
            return true;
        }
        false
    }

    pub(crate) fn open_completion_settings(&mut self) {
        self.completion_index_ui.open = false;
        self.open_settings_tab();
        self.settings_scope = crate::settings::SettingsScope::Workspace;
        self.settings_editor_search.clear();
        self.settings_editor_search_cursor = 0;
        self.settings_editor_search_selection_anchor = None;
        self.select_settings_page(1, Some("VTComplete"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_setting_ids_update_the_same_vtcomplete_preferences() {
        let mut app = App::test_new();
        for &(id, _, kind, _) in FIELDS {
            let legacy = id.replacen("vtcomplete_", "autocomplete_", 1);
            assert_eq!(canonical_id(&legacy), id);
            assert_eq!(App::setting_field_kind(&legacy), kind);
            assert_eq!(App::settings_section_for_id(&legacy), Some("VTComplete"));
        }
        assert_eq!(canonical_id("autocomplete_unknown"), "autocomplete_unknown");
        assert!(app
            .completion_setting_items()
            .iter()
            .all(|(_, _, id)| id.starts_with("vtcomplete_")));

        app.apply_settings_dropdown_value("autocomplete_index_speed", "Low CPU");
        assert_eq!(app.completion_index.settings().speed, IndexSpeed::Low);
        app.begin_numeric_setting_edit("autocomplete_max_files", "20000");
        app.settings_value_buffer = "12345".into();
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.completion_index.settings().max_files, 12345);
        app.toggle_setting("autocomplete_context_triggers");
        assert!(!app.completion_index.settings().context_triggers);

        for id in [
            "autocomplete_index_speed",
            "autocomplete_max_files",
            "autocomplete_context_triggers",
        ] {
            assert!(app.reset_completion_setting(id));
            assert!(!app.completion_setting_modified(id));
        }
    }

    #[test]
    fn learning_modes_scope_and_discard_share_one_unambiguous_state() {
        let mut app = App::test_new();
        assert!(!IndexSettings::default().paused);
        assert!(!IndexSettings::default().project_wide);
        for (label, enabled, paused) in [
            ("On", true, false),
            ("Paused", true, true),
            ("Off", false, true),
        ] {
            assert!(app.set_completion_dropdown("vtcomplete_learning_mode", label));
            let settings = app.completion_index.settings();
            assert_eq!((settings.enabled, settings.paused), (enabled, paused));
            assert_eq!(display(&settings, "vtcomplete_learning_mode"), label);
        }
        app.set_completion_dropdown("vtcomplete_sources", "Entire workspace");
        assert_eq!(
            LearningMode::from_settings(&app.completion_index.settings()),
            LearningMode::Off
        );
        assert!(app.completion_index.settings().project_wide);
        assert!(!app.refresh_completion_files());
        app.set_completion_dropdown("vtcomplete_learning_mode", "On");
        app.discard_completion_updates();
        assert_eq!(
            LearningMode::from_settings(&app.completion_index.settings()),
            LearningMode::Paused
        );
        assert!(app.reset_completion_setting("vtcomplete_sources"));
        assert!(!app.completion_index.settings().project_wide);
        assert!(app.reset_completion_setting("vtcomplete_learning_mode"));
        assert_eq!(
            LearningMode::from_settings(&app.completion_index.settings()),
            LearningMode::On
        );
        assert!(!app.set_completion_dropdown("vtcomplete_learning_mode", "invalid"));
    }

    #[test]
    fn settings_reject_out_of_range_ram_instead_of_silently_clamping() {
        let mut app = App::test_new();
        let before = app.completion_index.settings().max_memory_mb;
        app.begin_numeric_setting_edit("vtcomplete_memory_mb", &before.to_string());
        app.settings_value_buffer = "1024".into();
        assert!(!app.commit_numeric_setting_edit());
        assert!(app.settings_value_editing);
        assert_eq!(app.completion_index.settings().max_memory_mb, before);
        app.settings_value_buffer = "16".into();
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.completion_index.settings().max_memory_mb, 16);
    }

    #[test]
    fn all_project_controls_are_searchable_editable_and_share_popup_preferences() {
        let mut app = App::test_new();
        app.open_completion_settings();
        assert_eq!(app.settings_selected_section(1), Some("VTComplete"));
        let items = app.settings_items();
        let visible = app.settings_filtered_indices_with_items(1, &items);
        let shared_suggestions = items
            .iter()
            .position(|(_, _, id)| id == "quick_suggestions")
            .unwrap();
        assert!(!visible.contains(&shared_suggestions));
        assert_eq!(
            App::settings_section_for_id("quick_suggestions"),
            Some("Suggestions")
        );
        for &(id, _, kind, _) in FIELDS {
            let index = items
                .iter()
                .position(|(_, _, setting)| setting == id)
                .unwrap();
            assert!(
                visible.contains(&index),
                "{id} must be visible on the VTComplete page"
            );
            assert_eq!(App::setting_field_kind(id), kind);
            assert!(!App::setting_description(id).is_empty());
        }
        app.select_settings_page(1, Some("Suggestions"));
        assert!(app
            .settings_filtered_indices_with_items(1, &items)
            .contains(&shared_suggestions));
        app.begin_numeric_setting_edit("vtcomplete_max_file_mb", "100");
        app.settings_value_buffer = "512".into();
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(
            app.completion_index.settings().max_file_bytes,
            512 * 1024 * 1024
        );
        app.toggle_setting("vtcomplete_context_triggers");
        assert!(!app.completion_index.settings().context_triggers);
        app.apply_settings_dropdown_value("vtcomplete_index_speed", "Low CPU");
        assert_eq!(app.completion_index.settings().speed, IndexSpeed::Low);
        assert!(app.reset_setting_in_selected_scope("vtcomplete_context_triggers"));
        assert!(app.completion_index.settings().context_triggers);
        assert!(app.reset_setting_in_selected_scope("vtcomplete_max_file_mb"));
        assert_eq!(
            app.completion_index.settings().max_file_bytes,
            100 * 1024 * 1024
        );
    }
}
