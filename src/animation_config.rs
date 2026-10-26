//! Optional terminal animation preferences. Every effect is independent and off by default.

pub(crate) const MIN_ANIMATION_DURATION_MS: u64 = 1;
pub(crate) const MAX_ANIMATION_DURATION_MS: u64 = 5_000;

pub(crate) fn clamp_animation_duration_ms(value: u64) -> u64 {
    value.clamp(MIN_ANIMATION_DURATION_MS, MAX_ANIMATION_DURATION_MS)
}

macro_rules! animation_effects {
    ($(($variant:ident, $id:literal, $key:literal, $label:literal, $description:literal)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(usize)]
        pub(crate) enum AnimationEffect { $($variant),+ }

        impl AnimationEffect {
            pub(crate) const ALL: [Self; 5] = [$(Self::$variant),+];

            pub(crate) fn id(self) -> &'static str {
                match self { $(Self::$variant => $id),+ }
            }
            pub(crate) fn json_key(self) -> &'static str {
                match self { $(Self::$variant => $key),+ }
            }
            pub(crate) fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
            pub(crate) fn description(self) -> &'static str {
                match self { $(Self::$variant => $description),+ }
            }
            pub(crate) fn duration_id(self) -> &'static str {
                match self { $(Self::$variant => concat!($id, "_duration_ms")),+ }
            }
            pub(crate) fn intensity_id(self) -> &'static str {
                match self { $(Self::$variant => concat!($id, "_intensity")),+ }
            }
        }
    };
}

impl AnimationEffect {
    pub(crate) fn duration_json_key(self) -> &'static str {
        match self {
            Self::CursorSmear => "animationCursorSmearDurationMs",
            Self::CursorFire => "animationCursorFireDurationMs",
            Self::WhitespaceReveal => "animationWhitespaceRevealDurationMs",
            Self::ScopeConnector => "animationScopeBubbleDurationMs",
            Self::TabSlide => "animationTabSlideDurationMs",
        }
    }

    pub(crate) fn intensity_json_key(self) -> &'static str {
        match self {
            Self::CursorSmear => "animationCursorSmearIntensity",
            Self::CursorFire => "animationCursorFireIntensity",
            Self::WhitespaceReveal => "animationWhitespaceRevealIntensity",
            Self::ScopeConnector => "animationScopeBubbleIntensity",
            Self::TabSlide => "animationTabSlideIntensity",
        }
    }
}

animation_effects! {
    (CursorSmear, "animation_cursor_smear", "animationCursorSmear", "Cursor Smear", "Animate the cursor as a filled, elastic ribbon with smooth corners, continuous turns, and compact movement while typing."),
    (CursorFire, "animation_cursor_fire", "animationCursorFire", "Fire Hazard", "Follow the cursor with curling flame tongues, fine drifting sparks, and glowing embers that cool along the trail. Works independently from Cursor Smear."),
    (WhitespaceReveal, "animation_whitespace_reveal", "animationWhitespaceReveal", "Whitespace Reveal", "Sweep a filled ribbon smoothly across inserted tabs and spaces, with fractional edges and a soft arrival at each tab stop. Editing remains immediate."),
    (ScopeConnector, "animation_scope_connector", "animationScopeConnector", "Scope Bubble", "Wrap the active block, including Python indentation, brace-free C/C++ statements, preprocessor branches (#if/#elif/#else/#endif), and nested braces, in rounded tracing lines on both sides. The bubble follows whitespace around the code, stays visible inside the block, and fades smoothly between scopes."),
    (TabSlide, "animation_tab_slide", "animationTabSlide", "Sliding Tab Indicator", "Stretch the active underline toward its next tab, then gather it with independent edge springs. Rapid switches preserve position and momentum."),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnimationSettings {
    enabled: [bool; 5],
    durations: [Option<u64>; 5],
    intensities: [Option<u8>; 5],
    /// Legacy values remain the fallback for configurations without per-effect settings.
    pub(crate) duration_ms: u64,
    pub(crate) fps: u64,
    pub(crate) intensity: u8,
    pub(crate) scope_tight: bool,
    pub(crate) scope_fill: bool,
    /// Canonicalized once when preferences are applied, never on each frame.
    pub(crate) scope_disabled_languages: Vec<String>,
    pub(crate) scope_languages: std::sync::Arc<crate::scope_bubble_config::Rules>,
}

impl Default for AnimationSettings {
    fn default() -> Self {
        Self {
            enabled: [false; 5],
            durations: [None; 5],
            intensities: [None; 5],
            duration_ms: 240,
            fps: 60,
            intensity: 55,
            scope_tight: true,
            scope_fill: false,
            scope_disabled_languages: Vec::new(),
            scope_languages: crate::scope_bubble_config::Rules::empty(),
        }
    }
}

impl AnimationSettings {
    pub(crate) fn duration(&self, effect: AnimationEffect) -> u64 {
        clamp_animation_duration_ms(self.durations[effect as usize].unwrap_or(self.duration_ms))
    }

    pub(crate) fn intensity(&self, effect: AnimationEffect) -> u8 {
        self.intensities[effect as usize].unwrap_or(self.intensity)
    }

    pub(crate) fn set_duration(&mut self, effect: AnimationEffect, duration_ms: u64) {
        self.durations[effect as usize] = Some(clamp_animation_duration_ms(duration_ms));
    }

    pub(crate) fn set_intensity(&mut self, effect: AnimationEffect, intensity: u8) {
        self.intensities[effect as usize] = Some(intensity.clamp(5, 100));
    }

    pub(crate) fn enabled(&self, effect: AnimationEffect) -> bool {
        self.enabled[effect as usize]
    }

    pub(crate) fn set(&mut self, effect: AnimationEffect, enabled: bool) {
        self.enabled[effect as usize] = enabled;
    }

    /// The fast path does no language detection at all. Overrides and file
    /// associations take precedence; shebang detection is capped at 256 chars.
    pub(crate) fn scope_enabled_for(&self, editor: &crate::editor::Editor) -> bool {
        if !self.enabled(AnimationEffect::ScopeConnector) {
            return false;
        }
        if editor.scope_bubble_rules.for_editor(editor).is_some_and(|rule|
            rule.syntax == crate::scope_bubble_config::Syntax::Disabled) {
            return false;
        }
        if self.scope_disabled_languages.is_empty() {
            return true;
        }
        let language = crate::scope_bubble_config::language(editor);
        let language = scope_language_key(language);
        !self.scope_disabled_languages.iter().any(|disabled| disabled.eq_ignore_ascii_case(language))
    }

    pub(crate) fn any_enabled(&self) -> bool {
        self.enabled.iter().any(|enabled| *enabled)
    }

    pub(crate) fn editor_effects_enabled(&self) -> bool {
        AnimationEffect::ALL
            .into_iter()
            .any(|effect| effect != AnimationEffect::TabSlide && self.enabled(effect))
    }

    pub(crate) fn effect_for_id(id: &str) -> Option<AnimationEffect> {
        AnimationEffect::ALL
            .into_iter()
            .find(|effect| effect.id() == id)
    }

    pub(crate) fn effect_for_duration_id(id: &str) -> Option<AnimationEffect> {
        AnimationEffect::ALL
            .into_iter()
            .find(|effect| effect.duration_id() == id)
    }

    pub(crate) fn effect_for_intensity_id(id: &str) -> Option<AnimationEffect> {
        AnimationEffect::ALL
            .into_iter()
            .find(|effect| effect.intensity_id() == id)
    }

    /// Apply only explicitly supplied preferences, preserving settings from lower layers.
    pub(crate) fn apply(&mut self, settings: &crate::settings::Settings) {
        if let Some(value) = settings.animation_cursor_smear {
            self.set(AnimationEffect::CursorSmear, value);
        }
        if let Some(value) = settings.animation_cursor_fire {
            self.set(AnimationEffect::CursorFire, value);
        }
        if let Some(value) = settings.animation_whitespace_reveal {
            self.set(AnimationEffect::WhitespaceReveal, value);
        }
        if let Some(value) = settings.animation_scope_connector {
            self.set(AnimationEffect::ScopeConnector, value);
        }
        if let Some(value) = settings.animation_tab_slide {
            self.set(AnimationEffect::TabSlide, value);
        }
        if let Some(value) = settings.animation_duration_ms {
            self.duration_ms = clamp_animation_duration_ms(value);
        }
        if let Some(value) = settings.animation_fps {
            self.fps = value.clamp(15, 120);
        }
        if let Some(value) = settings.animation_intensity {
            self.intensity = value.clamp(5, 100);
        }
        for effect in AnimationEffect::ALL {
            if let Some(value) = settings.animation_effect_duration(effect) {
                self.set_duration(effect, value);
            }
            if let Some(value) = settings.animation_effect_intensity(effect) {
                self.set_intensity(effect, value);
            }
        }
        if let Some(value) = settings.animation_scope_bubble_tight {
            self.scope_tight = value;
        }
        if let Some(value) = settings.animation_scope_bubble_fill {
            self.scope_fill = value;
        }
        if let Some(languages) = &settings.animation_scope_bubble_disabled_languages {
            self.scope_disabled_languages = languages
                .iter()
                .map(|language| scope_language_key(language).to_ascii_lowercase())
                .filter(|language| !language.is_empty())
                .collect();
            self.scope_disabled_languages.sort_unstable();
            self.scope_disabled_languages.dedup();
        }
        if let Some(configs) = &settings.animation_scope_bubble_languages {
            let rules = crate::scope_bubble_config::Rules::compile(configs);
            if rules != self.scope_languages { self.scope_languages = rules; }
        }
    }
}

/// Common display names, syntax tokens and extensions share one preference.
/// Unknown/custom syntax names remain valid and are compared case-insensitively.
pub(crate) const SCOPE_LANGUAGE_ALIASES: &[(&str, &[&str])] = &[
        ("python", &["python 3", "python3", "py", "pyw", "pyi"]),
        ("rust", &["rs"]),
        ("cpp", &["c++", "c/c++ header", "cc", "cxx", "c++ header", "h", "hh", "hpp", "hxx"]),
        ("csharp", &["c#", "cs"]),
        ("fsharp", &["f#", "fs", "fsx"]),
        ("objective-c", &["objc", "m"]),
        ("objective-c++", &["objcpp", "mm"]),
        ("javascript", &["javascript (babel)", "js", "mjs", "cjs"]),
        ("javascriptreact", &["javascript react", "jsx"]),
        ("typescript", &["ts", "mts", "cts"]),
        ("typescriptreact", &["typescript react", "tsx"]),
        ("shellscript", &["shell script", "shell", "bash", "zsh", "sh"]),
        ("fish", &["fish shell"]),
        ("ruby", &["rb", "erb"]),
        ("powershell", &["ps1", "psm1", "pwsh"]),
        ("kotlin", &["kt", "kts"]),
        ("scala", &["sc"]),
        ("elixir", &["ex", "exs"]),
        ("erlang", &["erl", "hrl"]),
        ("haskell", &["hs", "lhs"]),
        ("clojure", &["clj", "cljs"]),
        ("julia", &["jl"]),
        ("fortran", &["fortran modern", "fortran free form", "f", "f90", "f95", "f03", "f08", "for"]),
        ("matlab", &["octave"]),
        ("vim", &["viml", "vimscript", "vim script"]),
        ("php", &["phtml"]),
        ("perl", &["pl", "pm"]),
        ("html", &["htm", "xhtml"]),
        ("xml", &["xsl", "xslt"]),
        ("json", &["json with comments", "jsonc", "json5"]),
        ("glsl", &["vert", "frag"]),
        ("toml", &["toml (cargo)"]),
        ("yaml", &["yml"]),
        ("markdown", &["md", "mdx"]),
        ("terraform", &["tf", "tfvars"]),
        ("graphql", &["gql"]),
        ("protobuf", &["protocol buffers", "proto"]),
        ("plaintext", &["plain text", "txt"]),
    ];

pub(crate) fn scope_language_key(language: &str) -> &str {
    let language = language.trim();

    SCOPE_LANGUAGE_ALIASES.iter().find_map(|(key, aliases)| {
        (key.eq_ignore_ascii_case(language)
            || aliases.iter().any(|alias| alias.eq_ignore_ascii_case(language)))
            .then_some(*key)
    }).unwrap_or(language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_language_preferences_match_names_ids_and_source_overrides() {
        let mut settings = AnimationSettings::default();
        settings.apply(&crate::settings::Settings {
            animation_scope_connector: Some(true),
            animation_scope_bubble_disabled_languages: Some(vec![
                " PYTHON ".into(), "Cpp".into(), "Rust".into(), "python".into(), "".into(),
            ]),
            ..Default::default()
        });
        assert_eq!(settings.scope_disabled_languages, ["cpp", "python", "rust"]);
        let mut editor = crate::editor::Editor::test_with_text("if ready:\n    work()\n");
        for path in ["test.py", "test.PYI", "test.cc", "test.hpp", "test.rs"] {
            editor.file_path = Some(path.into());
            assert!(!settings.scope_enabled_for(&editor), "{path}");
        }
        editor.file_path = Some("test.py".into());
        editor.language_override = Some("JavaScript".into());
        assert!(settings.scope_enabled_for(&editor));
        editor.language_override = Some("c++".into());
        assert!(!settings.scope_enabled_for(&editor));
        editor.language_override = None;
        editor.file_path = Some("script.custom".into());
        editor.language_associations.insert("*.custom".into(), "Python".into());
        assert!(!settings.scope_enabled_for(&editor));
        editor.language_associations.clear();
        editor.file_path = None;
        editor.suggested_file_name = Some("untitled.py".into());
        assert!(!settings.scope_enabled_for(&editor));
        settings.apply(&crate::settings::Settings {
            animation_scope_bubble_disabled_languages: Some(vec![]),
            ..Default::default()
        });
        assert!(settings.scope_enabled_for(&editor));
        settings.set(AnimationEffect::ScopeConnector, false);
        assert!(!settings.scope_enabled_for(&editor));
    }

    #[test]
    fn scope_language_shebang_lookup_is_bounded() {
        let mut settings = AnimationSettings::default();
        settings.set(AnimationEffect::ScopeConnector, true);
        settings.scope_disabled_languages = vec!["python".into()];
        let python = crate::editor::Editor::test_with_text("#!/usr/bin/env python3\nif ready:\n    work()\n");
        assert!(!settings.scope_enabled_for(&python));
        let oversized = crate::editor::Editor::test_with_text(&format!("#!{}python", "x".repeat(1024 * 1024)));
        assert!(settings.scope_enabled_for(&oversized));
    }

    #[test]
    fn all_effects_start_disabled_and_can_be_enabled_independently() {
        let mut settings = AnimationSettings::default();
        assert!(!settings.any_enabled());
        for effect in AnimationEffect::ALL {
            assert!(!settings.enabled(effect));
            settings.set(effect, true);
            assert!(settings.any_enabled());
            assert!(settings.enabled(effect));
            for other in AnimationEffect::ALL {
                assert_eq!(settings.enabled(other), other == effect);
            }
            settings.set(effect, false);
        }
    }

    #[test]
    fn metadata_ids_are_unique_and_resolve_to_their_effects() {
        let mut ids = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        for effect in AnimationEffect::ALL {
            assert!(ids.insert(effect.id()));
            assert!(keys.insert(effect.json_key()));
            assert_eq!(AnimationSettings::effect_for_id(effect.id()), Some(effect));
            assert!(!effect.label().is_empty());
            assert!(!effect.description().is_empty());
        }
        assert_eq!(AnimationSettings::effect_for_id("animation_fps"), None);
    }

    #[test]
    fn partial_settings_preserve_other_effects_and_bound_timing() {
        let mut settings = AnimationSettings::default();
        settings.set(AnimationEffect::WhitespaceReveal, true);
        settings.apply(&crate::settings::Settings {
            animation_cursor_smear: Some(true),
            animation_duration_ms: Some(0),
            animation_fps: Some(u64::MAX),
            animation_intensity: Some(0),
            ..Default::default()
        });
        assert!(settings.enabled(AnimationEffect::WhitespaceReveal));
        assert!(settings.enabled(AnimationEffect::CursorSmear));
        assert!(!settings.enabled(AnimationEffect::ScopeConnector));
        assert_eq!(settings.duration_ms, 1);
        assert_eq!(settings.fps, 120);
        assert_eq!(settings.intensity, 5);
    }

    #[test]
    fn effect_overrides_leave_legacy_fallbacks_and_other_effects_independent() {
        let mut settings = AnimationSettings::default();
        settings.duration_ms = 330;
        settings.intensity = 62;
        settings.set_duration(AnimationEffect::ScopeConnector, 710);
        settings.set_intensity(AnimationEffect::ScopeConnector, 88);
        for effect in AnimationEffect::ALL {
            assert_eq!(
                settings.duration(effect),
                if effect == AnimationEffect::ScopeConnector {
                    710
                } else {
                    330
                }
            );
            assert_eq!(
                settings.intensity(effect),
                if effect == AnimationEffect::ScopeConnector {
                    88
                } else {
                    62
                }
            );
        }
        settings.apply(&crate::settings::Settings {
            animation_duration_ms: Some(450),
            animation_intensity: Some(71),
            animation_cursor_fire_duration_ms: Some(920),
            animation_cursor_fire_intensity: Some(98),
            ..Default::default()
        });
        assert_eq!(settings.duration(AnimationEffect::CursorSmear), 450);
        assert_eq!(settings.intensity(AnimationEffect::CursorSmear), 71);
        assert_eq!(settings.duration(AnimationEffect::CursorFire), 920);
        assert_eq!(settings.intensity(AnimationEffect::CursorFire), 98);
        assert_eq!(settings.duration(AnimationEffect::ScopeConnector), 710);
        assert_eq!(settings.intensity(AnimationEffect::ScopeConnector), 88);
    }

    #[test]
    fn scope_bubble_outline_inherits_and_retired_glow_preferences_are_ignored() {
        let mut settings = AnimationSettings::default();
        assert!(settings.scope_tight);
        settings.apply(&crate::settings::Settings {
            animation_scope_bubble_tight: Some(false),
            animation_scope_bubble_glow: Some(true),
            animation_scope_bubble_glow_duration_ms: Some(0),
            ..Default::default()
        });
        assert!(!settings.scope_tight);
        settings.apply(&crate::settings::Settings::default());
        assert!(!settings.scope_tight);
        settings.apply(&crate::settings::Settings {
            animation_scope_bubble_glow_duration_ms: Some(u64::MAX),
            ..Default::default()
        });
        assert!(!settings.scope_tight);
    }

    #[test]
    fn every_effect_and_legacy_fallback_accept_the_full_duration_range() {
        for (input, expected) in [(0, 1), (1, 1), (5000, 5000), (u64::MAX, 5000)] {
            let mut settings = AnimationSettings::default();
            settings.duration_ms = input;
            for effect in AnimationEffect::ALL {
                assert_eq!(settings.duration(effect), expected);
                settings.set_duration(effect, input);
                assert_eq!(settings.duration(effect), expected);
            }
            let mut preferences = crate::settings::Settings {
                animation_duration_ms: Some(input),
                animation_scope_bubble_glow_duration_ms: Some(input),
                ..Default::default()
            };
            for effect in AnimationEffect::ALL {
                preferences.set_animation_effect_duration(effect, input);
            }
            settings.apply(&preferences);
            assert_eq!(settings.duration_ms, expected);
            for effect in AnimationEffect::ALL {
                assert_eq!(settings.duration(effect), expected);
            }
        }
    }
}
