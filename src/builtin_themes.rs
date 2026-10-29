//! Embedded theme definitions and their independent, lazy parse caches.
use crate::app_theme_loader::ThemeFile;
use std::{path::{Component, Path, PathBuf}, sync::OnceLock};

pub(crate) struct Entry {
    pub key: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
    pub file: &'static str,
    source: &'static str,
    parsed: OnceLock<ThemeFile>,
}

impl Entry {
    pub(crate) fn theme(&'static self) -> &'static ThemeFile {
        self.parsed.get_or_init(|| {
            let mut theme = crate::vscode_theme::load_embedded_theme(self.file, self.name, self.kind)
                .expect("valid bundled VS Code theme");
            theme.config_key = self.key.into();
            theme.syntax_theme = if self.key == "dark" { "dark-plus" } else { self.key }.into();
            theme.builtin = true;
            theme
        })
    }
}

macro_rules! themes {
    (@count) => { 0usize };
    (@count $head:literal $(, $tail:literal)*) => { 1usize + themes!(@count $($tail),*) };
    ($(($key:literal, $name:literal, $kind:literal, $file:literal)),* $(,)?) => {
        pub(crate) static THEMES: [Entry; themes!(@count $($key),*)] = [$(Entry {
            key: $key, name: $name, kind: $kind, file: $file,
            source: include_str!(concat!("../themes/", $file)),
            parsed: OnceLock::new(),
        }),*];
    };
}

themes! {
    ("dark", "Dark+", "vs-dark", "dark_plus.json"),
    ("light", "Light+", "vs", "light_plus.json"),
    ("high-contrast", "High Contrast", "hc-black", "hc_black.json"),
    ("monokai", "Monokai", "vs-dark", "monokai-color-theme.json"),
    ("one-dark-pro", "One Dark Pro", "vs-dark", "one-dark-pro/OneDark-Pro.json"),
    ("one-dark-pro-darker", "One Dark Pro Darker", "vs-dark", "one-dark-pro/OneDark-Pro-darker.json"),
    ("dracula", "Dracula Theme", "vs-dark", "dracula/dracula.json"),
    ("dracula-soft", "Dracula Theme Soft", "vs-dark", "dracula/dracula-soft.json"),
    ("github-dark-default", "GitHub Dark Default", "vs-dark", "github/dark-default.json"),
    ("github-dark-dimmed", "GitHub Dark Dimmed", "vs-dark", "github/dark-dimmed.json"),
    ("github-dark-high-contrast", "GitHub Dark High Contrast", "hc-black", "github/dark-high-contrast.json"),
    ("tokyo-night", "Tokyo Night", "vs-dark", "tokyo-night/tokyo-night-color-theme.json"),
    ("tokyo-night-storm", "Tokyo Night Storm", "vs-dark", "tokyo-night/tokyo-night-storm-color-theme.json"),
    ("catppuccin-mocha", "Catppuccin Mocha", "vs-dark", "catppuccin/mocha.json"),
    ("catppuccin-macchiato", "Catppuccin Macchiato", "vs-dark", "catppuccin/macchiato.json"),
    ("catppuccin-frappe", "Catppuccin Frappé", "vs-dark", "catppuccin/frappe.json"),
    ("nord", "Nord", "vs-dark", "nord/nord-color-theme.json"),
    ("night-owl", "Night Owl", "vs-dark", "night-owl/Night Owl-color-theme.json"),
    ("ayu-dark", "Ayu Dark", "vs-dark", "ayu/ayu-dark.json"),
    ("ayu-mirage", "Ayu Mirage", "vs-dark", "ayu/ayu-mirage.json"),
    ("rose-pine", "Rosé Pine", "vs-dark", "rose-pine/rose-pine-color-theme.json"),
    ("rose-pine-moon", "Rosé Pine Moon", "vs-dark", "rose-pine/rose-pine-moon-color-theme.json"),
    ("gruvbox-dark-medium", "Gruvbox Dark Medium", "vs-dark", "gruvbox/gruvbox-dark-medium.json"),
    ("gruvbox-dark-hard", "Gruvbox Dark Hard", "vs-dark", "gruvbox/gruvbox-dark-hard.json"),
    ("shades-of-purple", "Shades of Purple", "vs-dark", "shades-of-purple/shades-of-purple-color-theme.json"),
    ("synthwave-84", "SynthWave '84", "vs-dark", "synthwave-84/synthwave-color-theme.json"),
    ("palenight", "Palenight Theme", "vs-dark", "palenight/palenight.json"),
    ("andromeda", "Andromeda", "vs-dark", "andromeda/Andromeda-color-theme.json"),
    ("solarized-dark", "Solarized Dark", "vs-dark", "solarized-dark-color-theme.json"),
}

/// Resolve the complete bundled path: different packs may reuse file names.
/// Includes remain entirely in memory, including the original Dark+/Light+ bases.
pub(crate) fn source(path: &Path) -> Option<&'static str> {
    let path: PathBuf = path.components().filter(|part| *part != Component::CurDir).collect();
    THEMES.iter().find(|entry| Path::new(entry.file) == path).map(|entry| entry.source)
        .or_else(|| match path.to_str()? {
            "dark_vs.json" => Some(include_str!("../themes/dark_vs.json")),
            "light_vs.json" => Some(include_str!("../themes/light_vs.json")),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_theme_paths_preserve_pack_identity_and_resolve_base_includes() {
        assert_eq!(source(Path::new("./dark_vs.json")), source(Path::new("dark_vs.json")));
        assert!(source(Path::new("dark_vs.json")).is_some());
        assert!(source(Path::new("github/dark-default.json")).is_some());
        assert!(source(Path::new("dark-default.json")).is_none());
        assert!(source(Path::new("catppuccin/dark-default.json")).is_none());
        assert!(source(Path::new("../themes/dark_vs.json")).is_none());
    }

    #[test]
    fn selecting_a_bundled_theme_reuses_its_independent_parse_cache() {
        let entry = THEMES.iter().find(|entry| entry.key == "catppuccin-mocha").unwrap();
        assert!(std::ptr::eq(entry.theme(), entry.theme()));
        assert_eq!(entry.theme().syntax_theme, entry.key);
        assert_eq!(entry.theme().name, entry.name);
    }

    #[test]
    fn bundled_themes_preserve_upstream_workbench_colors_and_token_rules() {
        for entry in &THEMES {
            let raw = crate::vscode_theme::parse_jsonc(entry.source).unwrap();
            let theme = entry.theme();
            if let Some(colors) = raw["colors"].as_object() {
                for (key, value) in colors {
                    assert_eq!(theme.workbench_colors.get(key), Some(value), "{}: {key}", entry.key);
                }
            }
            if let Some(rules) = raw["tokenColors"].as_array() {
                assert!(theme.token_colors.ends_with(rules), "{} token rules", entry.key);
            }
            assert_eq!(theme.high_contrast, entry.kind == "hc-black", "{}", entry.key);
        }
    }
}
