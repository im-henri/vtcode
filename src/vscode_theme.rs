use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde_json::{Map, Value};
use syntect::highlighting::{
    Color as SyntectColor, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSet,
};

use crate::app_theme_loader::{ThemeColorsFile, ThemeFile};

#[derive(Default)]
struct ResolvedTheme {
    name: Option<String>,
    kind: Option<String>,
    semantic_highlighting: Option<bool>,
    colors: Map<String, Value>,
    token_rules: Vec<Value>,
    semantic_colors: Map<String, Value>,
    tm_theme: Option<Theme>,
}

pub(crate) fn load_theme_file(
    path: &Path,
    label_override: Option<&str>,
    ui_theme: Option<&str>,
) -> Result<ThemeFile, String> {
    let resolved = resolve_theme(path, 0)?;
    finish_theme(path, label_override, ui_theme, resolved)
}

fn finish_theme(
    path: &Path,
    label_override: Option<&str>,
    ui_theme: Option<&str>,
    resolved: ResolvedTheme,
) -> Result<ThemeFile, String> {
    let fallback_name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("VS Code Theme");
    let name = label_override
        .map(str::to_owned)
        .or(resolved.name.clone())
        .unwrap_or_else(|| fallback_name.to_string());
    let is_light = ui_theme
        .is_some_and(|kind| {
            kind.eq_ignore_ascii_case("vs") || kind.eq_ignore_ascii_case("hc-light")
        })
        .then_some(true)
        .or_else(|| ui_theme.map(|_| false))
        .unwrap_or_else(|| {
            resolved
                .kind
                .as_deref()
                .map(|k| k == "light" || k == "hcLight")
                .unwrap_or_else(|| inferred_light(&resolved.colors))
        });
    let config_key = format!("vscode-{}", slug(&name));
    let high_contrast = ui_theme.is_some_and(|kind| kind.starts_with("hc"))
        || resolved.kind.as_deref().is_some_and(|kind| kind == "hcDark" || kind == "hcLight");
    let mut theme_file = base_theme(is_light, high_contrast)?;
    theme_file.name = name.clone();
    theme_file.config_key = config_key.clone();
    theme_file.syntax_theme = config_key;
    theme_file.gradients = None;
    let effective_colors = crate::vscode_color_registry::resolve(&resolved.colors, is_light, high_contrast);
    apply_editor_defaults(&mut theme_file.colors, &effective_colors);
    apply_workbench_colors(
        &mut theme_file.colors,
        &effective_colors,
        is_light,
        theme_file.high_contrast,
    );
    apply_imported_annotation_colors(&mut theme_file.colors, &effective_colors);
    theme_file.syntect_theme = Some(build_syntect_theme(&name, &theme_file.colors, &resolved));
    theme_file.semantic_color_rules = if resolved.semantic_highlighting == Some(false) {
        Map::new()
    } else {
        resolved.semantic_colors
    };
    theme_file.semantic_colors = Some(build_semantic_colors(
        &theme_file.semantic_color_rules, &theme_file.colors.bg_editor,
    ));
    theme_file.workbench_colors = resolved.colors;
    theme_file.token_colors = resolved.token_rules;
    Ok(theme_file)
}

pub(crate) fn load_extension_themes(extension_root: &Path) -> Vec<ThemeFile> {
    load_extension_themes_matching(extension_root, None)
}

/// Read manifests to locate a saved variant, without parsing every other theme.
pub(crate) fn load_extension_themes_matching(extension_root: &Path, key: Option<&str>) -> Vec<ThemeFile> {
    load_extension_themes_filtered(extension_root, key, &Default::default())
}

pub(crate) fn load_extension_themes_excluding(extension_root: &Path, removed: &std::collections::BTreeSet<String>) -> Vec<ThemeFile> {
    load_extension_themes_filtered(extension_root, None, removed)
}

fn load_extension_themes_filtered(extension_root: &Path, key: Option<&str>, removed: &std::collections::BTreeSet<String>) -> Vec<ThemeFile> {
    let Ok(entries) = std::fs::read_dir(extension_root) else {
        return Vec::new();
    };
    let mut extension_dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            (path.is_dir() && !hidden).then_some(path)
        })
        .collect();
    extension_dirs.sort();

    let mut themes = Vec::new();
    for extension_dir in extension_dirs {
        let package_path = extension_dir.join("package.json");
        let Ok(package_text) = std::fs::read_to_string(&package_path) else {
            continue;
        };
        let Ok(package) = parse_jsonc(&package_text) else {
            continue;
        };
        let publisher = package
            .get("publisher")
            .and_then(Value::as_str)
            .unwrap_or("extension");
        let extension_name = package
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("theme");
        let package_messages = load_package_messages(&extension_dir);
        let Some(contributions) = package
            .get("contributes")
            .and_then(|value| value.get("themes"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for contribution in contributions {
            let Some(relative_path) = contribution.get("path").and_then(Value::as_str) else {
                continue;
            };
            let raw_label = contribution
                .get("label")
                .and_then(Value::as_str)
                .or_else(|| contribution.get("id").and_then(Value::as_str))
                .unwrap_or("VS Code Theme");
            let label = localize_manifest_value(raw_label, &package_messages);
            let config_key = format!("vscode-{}-{}-{}", slug(publisher), slug(extension_name), slug(&label));
            if removed.contains(&config_key) || key.is_some_and(|key| key != config_key) { continue; }
            let ui_theme = contribution.get("uiTheme").and_then(Value::as_str);
            let theme_path = extension_dir.join(relative_path);
            // Manually removed variants can remain in package.json. Discovery
            // runs after the terminal UI starts, so skip unusable contributions
            // without writing warnings over the screen. Explicit installation
            // still validates its source and returns errors to the caller.
            if let Ok(mut theme) = load_theme_file(&theme_path, Some(&label), ui_theme) {
                theme.config_key = config_key;
                theme.syntax_theme = theme.config_key.clone();
                themes.push(theme);
            }
        }
    }
    themes.sort_by(|left, right| left.name.cmp(&right.name));
    themes
}

fn load_package_messages(extension_dir: &Path) -> Map<String, Value> {
    let path = extension_dir.join("package.nls.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Map::new();
    };
    parse_jsonc(&text)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn localize_manifest_value(value: &str, messages: &Map<String, Value>) -> String {
    let Some(key) = value
        .strip_prefix('%')
        .and_then(|value| value.strip_suffix('%'))
    else {
        return value.to_string();
    };
    messages
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(key)
        .to_string()
}

fn resolve_theme(path: &Path, depth: usize) -> Result<ResolvedTheme, String> {
    resolve_with(path, depth, &|p| {
        std::fs::read_to_string(p).map_err(|e| e.to_string())
    })
}

fn resolve_with(
    path: &Path,
    depth: usize,
    read: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<ResolvedTheme, String> {
    if depth >= 16 {
        return Err("theme include nesting is too deep".to_string());
    }
    let content = read(path)?;
    let value = parse_jsonc(&content)?;
    let object = value
        .as_object()
        .ok_or_else(|| "VS Code theme root must be an object".to_string())?;

    if object.contains_key("config_key")
        || !["colors", "tokenColors", "include", "semanticTokenColors"]
            .iter()
            .any(|key| object.contains_key(*key))
    {
        return Err("Expected a VS Code color theme (colors / tokenColors / include)".into());
    }
    let mut resolved = if let Some(include) = object.get("include").and_then(Value::as_str) {
        let include_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(include);
        resolve_with(&include_path, depth + 1, read)?
    } else {
        ResolvedTheme::default()
    };

    if let Some(enabled) = object.get("semanticHighlighting").and_then(Value::as_bool) {
        resolved.semantic_highlighting = Some(enabled);
    }
    if let Some(kind) = object.get("type").and_then(Value::as_str) {
        resolved.kind = Some(kind.into());
    }
    if let Some(name) = object.get("name").and_then(Value::as_str) {
        resolved.name = Some(name.to_string());
    }
    if let Some(colors) = object.get("colors").and_then(Value::as_object) {
        for (key, value) in colors {
            resolved.colors.insert(key.clone(), value.clone());
        }
    }
    if let Some(semantic) = object.get("semanticTokenColors").and_then(Value::as_object) {
        for (key, value) in semantic {
            resolved.semantic_colors.insert(key.clone(), value.clone());
        }
    }
    match object.get("tokenColors") {
        Some(Value::Array(rules)) => resolved.token_rules.extend(rules.iter().cloned()),
        Some(Value::String(relative_path)) => {
            let token_path = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(relative_path);
            if token_path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tmTheme"))
            {
                let loaded = ThemeSet::get_theme(&token_path).map_err(|error| error.to_string())?;
                if let Some(existing) = &mut resolved.tm_theme {
                    existing.scopes.extend(loaded.scopes);
                } else {
                    resolved.tm_theme = Some(loaded);
                }
            } else {
                let text =
                    std::fs::read_to_string(&token_path).map_err(|error| error.to_string())?;
                let token_value = parse_jsonc(&text)?;
                if let Some(rules) = token_value.as_array() {
                    resolved.token_rules.extend(rules.iter().cloned());
                } else if let Some(rules) = token_value.get("tokenColors").and_then(Value::as_array)
                {
                    resolved.token_rules.extend(rules.iter().cloned());
                }
            }
        }
        _ => {}
    }
    Ok(resolved)
}

pub(crate) fn load_embedded_theme(
    file: &str,
    label: &str,
    kind: &str,
) -> Result<ThemeFile, String> {
    let read = |path: &Path| {
        Ok(
            match path.file_name().and_then(|s| s.to_str()).unwrap_or("") {
                "dark_vs.json" => include_str!("../themes/dark_vs.json"),
                "dark_plus.json" => include_str!("../themes/dark_plus.json"),
                "light_vs.json" => include_str!("../themes/light_vs.json"),
                "light_plus.json" => include_str!("../themes/light_plus.json"),
                "hc_black.json" => include_str!("../themes/hc_black.json"),
                "monokai-color-theme.json" => {
                    include_str!("../themes/monokai-color-theme.json")
                }
                _ => return Err("Unknown embedded theme".into()),
            }
            .to_string(),
        )
    };
    let path = Path::new(file);
    finish_theme(path, Some(label), Some(kind), resolve_with(path, 0, &read)?)
}

/// Re-resolve from pristine defaults when settings change. In particular, a
/// `default` override must not leave a previously baked theme color behind.
pub(crate) fn customized_theme(
    theme: &ThemeFile,
    overrides: &std::collections::BTreeMap<String, Value>,
) -> ThemeFile {
    let mut selected = theme.clone();
    selected.workbench_colors =
        crate::theme_customizations::resolve(&theme.workbench_colors, overrides, &theme.name);
    let effective_colors = if theme.syntect_theme.is_some() {
        std::borrow::Cow::Owned(crate::vscode_color_registry::resolve(
            &selected.workbench_colors, selected.is_light, selected.high_contrast))
    } else {
        std::borrow::Cow::Borrowed(&selected.workbench_colors)
    };
    if theme.syntect_theme.is_some() {
        // Imported VS Code themes (including bundled themes) use registered
        // fallbacks. Legacy vtcode themes retain their explicitly authored palette.
        selected.colors = base_theme(theme.is_light, theme.high_contrast)
            .expect("the built-in workbench defaults are valid")
            .colors;
        apply_editor_defaults(
            &mut selected.colors,
            &effective_colors,
        );
    }
    apply_workbench_colors(
        &mut selected.colors,
        &effective_colors,
        selected.is_light,
        selected.high_contrast,
    );
    if theme.syntect_theme.is_some() {
        apply_imported_annotation_colors(&mut selected.colors, &effective_colors);
    }
    if !theme.semantic_color_rules.is_empty() {
        selected.semantic_colors = Some(build_semantic_colors(
            &theme.semantic_color_rules,
            &selected.colors.bg_editor,
        ));
    }
    selected
}

fn base_theme(light: bool, high_contrast: bool) -> Result<ThemeFile, String> {
    let (bg, fg, sidebar, dim, selection, workbench, focus) = match (light, high_contrast) {
        (true, false) => (
            "#ffffff", "#333333", "#f3f3f3", "#616161", "#add6ff", "#616161", "#0090f1",
        ),
        (false, false) => (
            "#1e1e1e", "#bbbbbb", "#252526", "#858585", "#264f78", "#cccccc", "#007fd4",
        ),
        (true, true) => (
            "#ffffff", "#292929", "#ffffff", "#292929", "#0f4a85", "#292929", "#006bbd",
        ),
        (false, true) => (
            "#000000", "#ffffff", "#000000", "#ffffff", "#f3f518", "#ffffff", "#f38518",
        ),
    };
    let pick = |dark, light_color| if light { light_color } else { dark };
    // Keep the string palette outside json!: a large nested object exhausts
    // serde_json's macro recursion depth even though the resulting data is small.
    let colors: Map<String, Value> = [
        ("bg_dark", bg),
        ("bg_editor", bg),
        ("bg_sidebar", sidebar),
        ("bg_tabbar", sidebar),
        ("bg_tab_active", bg),
        (
            "bg_tab_inactive",
            if high_contrast {
                sidebar
            } else {
                pick("#2d2d2d", "#ececec")
            },
        ),
        (
            "bg_statusbar",
            if high_contrast { sidebar } else { "#007acc" },
        ),
        (
            "bg_statusbar_item",
            if high_contrast { sidebar } else { "#007acc" },
        ),
        (
            "fg_statusbar",
            if light && high_contrast {
                "#292929"
            } else {
                "#ffffff"
            },
        ),
        ("bg_line_highlight", bg),
        ("fg_text", fg),
        ("fg_dim", dim),
        ("fg_accent", focus),
        (
            "bg_selected",
            if high_contrast {
                sidebar
            } else {
                pick("#04395e", "#0060c0")
            },
        ),
        (
            "fg_selected",
            if light && high_contrast {
                "#292929"
            } else {
                "#ffffff"
            },
        ),
        (
            "bg_explorer_selected",
            if high_contrast {
                sidebar
            } else {
                pick("#37373d", "#e4e6f1")
            },
        ),
        ("bg_selection", selection),
        ("bg_menu", sidebar),
        ("fg_menu", workbench),
        ("fg_workbench", workbench),
        (
            "bg_activitybar",
            if high_contrast {
                sidebar
            } else {
                pick("#333333", "#2c2c2c")
            },
        ),
        (
            "bg_input",
            if light {
                "#ffffff"
            } else if high_contrast {
                "#000000"
            } else {
                "#3c3c3c"
            },
        ),
        ("fg_input", workbench),
        ("fg_input_placeholder", dim),
        (
            "border_color",
            if high_contrast {
                pick("#6fc3df", "#0f4a85")
            } else {
                pick("#3c3c3c", "#c8c8c8")
            },
        ),
        (
            "bg_hover",
            if high_contrast {
                sidebar
            } else {
                pick("#2a2d2e", "#f0f0f0")
            },
        ),
        ("bg_sticky_header", bg),
        (
            "bg_md_code",
            if high_contrast {
                sidebar
            } else {
                pick("#0a0a0a66", "#dcdcdc66")
            },
        ),
        ("fg_md_code", pick("#d7ba7d", "#a31515")),
        ("fg_md_link", pick("#3794ff", "#006ab1")),
        ("fg_md_heading", fg),
        ("fg_md_blockquote", dim),
        ("fg_error", pick("#f48771", "#a1260d")),
        ("fg_warning", pick("#cca700", "#bf8803")),
        ("fg_info_label", pick("#3794ff", "#0063d3")),
        ("fg_success", pick("#73c991", "#388a34")),
        ("fg_git_added", pick("#81b88b", "#587c0c")),
        ("fg_git_modified", pick("#e2c08d", "#895503")),
        ("fg_git_deleted", pick("#c74e39", "#ad0707")),
        ("fg_diff_added", pick("#81b88b", "#587c0c")),
        ("fg_diff_removed", pick("#c74e39", "#ad0707")),
        (
            "bg_sc_button",
            if high_contrast {
                pick("#000000", "#0f4a85")
            } else {
                pick("#0e639c", "#007acc")
            },
        ),
        (
            "bg_sc_button_secondary",
            if high_contrast {
                sidebar
            } else {
                pick("#2a2d2e", "#f0f0f0")
            },
        ),
        ("bg_cursor", fg),
        ("fg_cursor", bg),
    ]
    .into_iter()
    .map(|(name, color)| (name.to_string(), Value::String(color.to_string())))
    .collect();
    serde_json::from_value(serde_json::json!({
        "name": "", "config_key": "", "syntax_theme": "", "is_light": light,
        "high_contrast": high_contrast, "colors": colors
    }))
    .map_err(|e| e.to_string())
}

/// Null decoration defaults mean no paint. Start those legacy scalar fields on
/// the owning editor; registered, possibly translucent, colors are applied next.
fn apply_editor_defaults(
    target: &mut ThemeColorsFile,
    colors: &Map<String, Value>,
) {
    let editor = resolved_color(colors, "editor.background", &target.bg_editor)
        .unwrap_or_else(|| target.bg_editor.clone());
    target.bg_line_highlight = editor.clone();
    target.bg_selected = target.bg_sidebar.clone();
    target.bg_explorer_selected = Some(target.bg_sidebar.clone());
    target.bg_hover = Some(target.bg_sidebar.clone());
    for field in [
        &mut target.bg_diff_added, &mut target.bg_diff_removed,
        &mut target.bg_diff_added_highlight, &mut target.bg_diff_removed_highlight,
        &mut target.bg_search_match, &mut target.bg_word_highlight,
        &mut target.bg_bracket_match, &mut target.bg_search_range,
        &mut target.bg_fold_indicator, &mut target.bg_md_code,
    ] {
        *field = Some(editor.clone());
    }
    // Cursor background is null by default, inheriting the editor underneath.
    target.fg_cursor = Some(editor.clone());
    target.bg_cursor = Some(
        resolved_color(colors, "editor.foreground", &editor)
            .unwrap_or_else(|| target.fg_text.clone()),
    );
}

fn apply_imported_annotation_colors(target: &mut ThemeColorsFile, colors: &Map<String, Value>) {
    // These vtcode annotations have no dedicated VS Code role. In imported
    // themes they inherit diagnostic colors; native themes keep their palette.
    target.fg_todo = target.fg_warning.clone();
    target.fg_hack = target.fg_warning.clone();
    target.fg_fixme = resolved_color(colors, "editorError.foreground", &target.bg_editor)
        .or_else(|| target.fg_error.clone());
    target.fg_bookmark = Some(target.fg_accent.clone());
    target.fg_overwrite_cursor = target.bg_cursor.clone();
    target.fg_tilde = resolved_color(colors, "editorLineNumber.foreground", &target.bg_dark);
    target.fg_md_heading = Some(target.fg_text.clone());
}

pub(crate) fn apply_workbench_colors(
    target: &mut ThemeColorsFile,
    colors: &Map<String, Value>,
    light: bool,
    high_contrast: bool,
) {
    macro_rules! required {
        ($field:ident, $id:literal, $background:expr) => {
            if let Some(color) = resolved_color(colors, $id, $background) {
                target.$field = color;
            }
        };
    }
    macro_rules! optional {
        ($field:ident, $id:literal, $background:expr) => {
            if let Some(color) = resolved_color(colors, $id, $background) {
                target.$field = Some(color);
            }
        };
    }

    let fallback_editor = target.bg_editor.clone();
    required!(bg_editor, "editor.background", &fallback_editor);
    let editor = target.bg_editor.clone();
    target.bg_dark = editor.clone();
    required!(bg_dark, "editorGutter.background", &editor);
    required!(fg_text, "editor.foreground", &editor);
    required!(bg_line_highlight, "editor.lineHighlightBackground", &editor);

    let fallback_sidebar = target.bg_sidebar.clone();
    required!(bg_sidebar, "sideBar.background", &fallback_sidebar);
    let sidebar = target.bg_sidebar.clone();
    if colors.get("list.activeSelectionBackground").is_some_and(Value::is_null) {
        target.bg_selected = sidebar.clone();
    }
    if colors.get("list.inactiveSelectionBackground").is_some_and(Value::is_null) {
        target.bg_explorer_selected = Some(sidebar.clone());
    }
    if colors.get("list.hoverBackground").is_some_and(Value::is_null) {
        target.bg_hover = Some(sidebar.clone());
    }
    optional!(fg_workbench, "foreground", &sidebar);
    let workbench_fg = target.fg_workbench.clone().unwrap_or_else(|| target.fg_text.clone());
    target.fg_folder_icon = Some(workbench_fg.clone());
    optional!(fg_folder_icon, "icon.foreground", &sidebar);
    target.fg_input = Some(workbench_fg);
    required!(fg_dim, "descriptionForeground", &sidebar);

    let fallback_tabbar = target.bg_tabbar.clone();
    required!(
        bg_tabbar,
        "editorGroupHeader.tabsBackground",
        &fallback_tabbar
    );
    let tabbar = target.bg_tabbar.clone();
    required!(bg_tab_active, "tab.activeBackground", &tabbar);
    required!(bg_tab_inactive, "tab.inactiveBackground", &tabbar);

    let fallback_status = target.bg_statusbar.clone();
    required!(bg_statusbar, "statusBar.background", &fallback_status);
    let status = target.bg_statusbar.clone();
    // Missing hover colors inherit the status surface, not the default blue
    // theme. The renderer supplies a subtle highlight when they are identical.
    target.bg_statusbar_item = status.clone();
    required!(bg_statusbar_item, "statusBarItem.hoverBackground", &status);
    required!(fg_statusbar, "statusBar.foreground", &status);

    required!(fg_accent, "focusBorder", &sidebar);
    optional!(fg_accent_blue, "focusBorder", &sidebar);
    required!(bg_selected, "list.activeSelectionBackground", &sidebar);
    let selected = target.bg_selected.clone();
    required!(fg_selected, "list.activeSelectionForeground", &selected);
    optional!(
        bg_explorer_selected,
        "list.inactiveSelectionBackground",
        &sidebar
    );
    optional!(bg_hover, "list.hoverBackground", &sidebar);
    optional!(border_color, "panel.border", &sidebar);
    optional!(bg_activitybar, "activityBar.background", &sidebar);

    optional!(bg_selection, "editor.selectionBackground", &editor);
    optional!(
        bg_trailing_whitespace,
        "editorWhitespace.foreground",
        &editor
    );
    optional!(bg_bracket_match, "editorBracketMatch.background", &editor);
    optional!(bg_word_highlight, "editor.wordHighlightBackground", &editor);
    optional!(
        bg_search_range,
        "editor.findRangeHighlightBackground",
        &editor
    );
    optional!(
        bg_search_match,
        "editor.findMatchHighlightBackground",
        &editor
    );
    optional!(bg_cursor, "editorCursor.foreground", &editor);
    optional!(fg_cursor, "editorCursor.background", &editor);

    optional!(
        fg_git_modified,
        "gitDecoration.modifiedResourceForeground",
        &sidebar
    );
    optional!(
        fg_git_added,
        "gitDecoration.addedResourceForeground",
        &sidebar
    );
    optional!(
        fg_git_deleted,
        "gitDecoration.deletedResourceForeground",
        &sidebar
    );
    optional!(
        fg_modified_indicator,
        "gitDecoration.modifiedResourceForeground",
        &sidebar
    );
    optional!(bg_diff_added, "diffEditor.insertedLineBackground", &editor);
    optional!(bg_diff_removed, "diffEditor.removedLineBackground", &editor);
    optional!(
        bg_diff_added_highlight,
        "diffEditor.insertedTextBackground",
        &editor
    );
    optional!(
        bg_diff_removed_highlight,
        "diffEditor.removedTextBackground",
        &editor
    );

    // VS Code menus inherit dropdown surfaces and list selection colors.
    // Resolve each foreground/alpha color on the surface that actually uses it.
    let dropdown_default = match (light, high_contrast) {
        (true, _) => "#ffffff",
        (false, true) => "#000000",
        (false, false) => "#3c3c3c",
    };
    let dropdown = resolved_color(colors, "dropdown.background", &editor)
        .unwrap_or_else(|| dropdown_default.into());
    let menu = resolved_color(colors, "menu.background", &editor).unwrap_or(dropdown);
    target.bg_menu = Some(menu.clone());
    let menu_fg_default = match (light, high_contrast) {
        (true, _) => target.fg_workbench.as_deref().unwrap_or("#616161"),
        (false, true) => "#ffffff",
        (false, false) => "#f0f0f0",
    };
    target.fg_menu = Some(
        resolved_color(colors, "menu.foreground", &menu)
            .or_else(|| resolved_color(colors, "dropdown.foreground", &menu))
            .unwrap_or_else(|| menu_fg_default.into()),
    );
    let selection_default = match (light, high_contrast) {
        (_, true) => menu.as_str(),
        (true, false) => "#0060c0",
        (false, false) => "#04395e",
    };
    target.bg_menu_selected = Some(
        resolved_color(colors, "menu.selectionBackground", &menu)
            .or_else(|| resolved_color(colors, "list.activeSelectionBackground", &menu))
            .unwrap_or_else(|| selection_default.into()),
    );

    let fallback_input = target.bg_input.clone().unwrap_or_else(|| sidebar.clone());
    optional!(bg_input, "input.background", &fallback_input);
    let input = target.bg_input.clone().unwrap_or(fallback_input);
    optional!(fg_input, "input.foreground", &input);
    optional!(fg_input_placeholder, "input.placeholderForeground", &input);
    optional!(border_input, "input.border", &input);

    optional!(bg_sticky_header, "editorStickyScroll.background", &editor);
    optional!(bg_fold_indicator, "editor.foldBackground", &editor);
    optional!(bg_merge_current, "merge.currentContentBackground", &editor);
    optional!(
        bg_merge_incoming,
        "merge.incomingContentBackground",
        &editor
    );
    optional!(fg_md_link, "textLink.foreground", &editor);
    optional!(fg_md_code, "textPreformat.foreground", &editor);
    optional!(bg_md_code, "textCodeBlock.background", &editor);
    optional!(fg_md_blockquote, "textBlockQuote.border", &editor);
    optional!(fg_info_label, "textLink.foreground", &sidebar);
    optional!(fg_error, "errorForeground", &sidebar);
    optional!(fg_warning, "editorWarning.foreground", &editor);
    optional!(fg_success, "testing.iconPassed", &sidebar);
    optional!(bg_sc_button, "button.background", &sidebar);
    optional!(
        bg_sc_button_secondary,
        "button.secondaryBackground",
        &sidebar
    );
}

fn build_syntect_theme(name: &str, colors: &ThemeColorsFile, resolved: &ResolvedTheme) -> Theme {
    let mut theme = resolved.tm_theme.clone().unwrap_or_default();
    theme.name = Some(name.to_string());
    theme.settings.foreground = parse_syntect_color(&colors.fg_text);
    theme.settings.background = parse_syntect_color(&colors.bg_editor);
    theme.settings.caret = colors.bg_cursor.as_deref().and_then(parse_syntect_color);
    theme.settings.line_highlight = parse_syntect_color(&colors.bg_line_highlight);
    theme.settings.selection = colors.bg_selection.as_deref().and_then(parse_syntect_color);
    theme.settings.find_highlight = colors
        .bg_search_match
        .as_deref()
        .and_then(parse_syntect_color);

    for rule in &resolved.token_rules {
        let Some(settings) = rule.get("settings").and_then(Value::as_object) else {
            continue;
        };
        let selector = match rule.get("scope") {
            Some(Value::String(scope)) => scope.clone(),
            Some(Value::Array(scopes)) => scopes
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            _ => {
                if !resolved.colors.contains_key("editor.foreground") {
                    if let Some(c) = settings
                        .get("foreground")
                        .and_then(Value::as_str)
                        .and_then(parse_syntect_color)
                    {
                        theme.settings.foreground = Some(c);
                    }
                }
                if !resolved.colors.contains_key("editor.background") {
                    if let Some(c) = settings
                        .get("background")
                        .and_then(Value::as_str)
                        .and_then(parse_syntect_color)
                    {
                        theme.settings.background = Some(c);
                    }
                }
                continue;
            }
        };
        let Ok(scope) = ScopeSelectors::from_str(&selector) else {
            continue;
        };
        let foreground = settings
            .get("foreground")
            .and_then(Value::as_str)
            .and_then(parse_syntect_color);
        let background = settings
            .get("background")
            .and_then(Value::as_str)
            .and_then(parse_syntect_color);
        let font_style = settings
            .get("fontStyle")
            .and_then(Value::as_str)
            .map(parse_font_style);
        theme.scopes.push(ThemeItem {
            scope,
            style: StyleModifier {
                foreground,
                background,
                font_style,
            },
        });
    }
    theme
}

fn build_semantic_colors(
    semantic: &Map<String, Value>,
    editor_background: &str,
) -> rustc_hash::FxHashMap<String, (u8, u8, u8)> {
    let mut result = rustc_hash::FxHashMap::default();
    for (selector, style) in semantic {
        let token_type = selector.trim();
        if token_type.is_empty() {
            continue;
        }
        let foreground = style
            .as_str()
            .or_else(|| style.get("foreground").and_then(Value::as_str));
        let Some(foreground) = foreground else {
            continue;
        };
        if let Some((r, g, b, a)) = parse_rgba(foreground) {
            let (br, bg, bb, _) = parse_rgba(editor_background).unwrap_or((0, 0, 0, 255));
            result.insert(
                token_type.to_string(),
                composite((r, g, b, a), (br, bg, bb)),
            );
        }
    }
    result
}

fn parse_font_style(value: &str) -> FontStyle {
    let mut style = FontStyle::empty();
    for part in value.split_whitespace() {
        match part {
            "bold" => style |= FontStyle::BOLD,
            "italic" => style |= FontStyle::ITALIC,
            "underline" | "strikethrough" => style |= FontStyle::UNDERLINE,
            _ => {}
        }
    }
    style
}

pub(crate) fn parse_syntect_color(value: &str) -> Option<SyntectColor> {
    let (r, g, b, a) = parse_rgba(value)?;
    Some(SyntectColor { r, g, b, a })
}

pub(crate) fn resolved_color(
    colors: &Map<String, Value>,
    id: &str,
    background: &str,
) -> Option<String> {
    let value = colors.get(id)?.as_str()?;
    let (r, g, b, a) = parse_rgba(value)?;
    let (br, bg, bb, _) = parse_rgba(background).unwrap_or((0, 0, 0, 255));
    let (r, g, b) = composite((r, g, b, a), (br, bg, bb));
    Some(format!("#{r:02X}{g:02X}{b:02X}"))
}

fn composite(foreground: (u8, u8, u8, u8), background: (u8, u8, u8)) -> (u8, u8, u8) {
    let alpha = u16::from(foreground.3);
    let blend = |front: u8, back: u8| {
        ((u16::from(front) * alpha + u16::from(back) * (255 - alpha) + 127) / 255) as u8
    };
    (
        blend(foreground.0, background.0),
        blend(foreground.1, background.1),
        blend(foreground.2, background.2),
    )
}

fn parse_rgba(value: &str) -> Option<(u8, u8, u8, u8)> {
    let hex = value.trim().strip_prefix('#')?;
    if !hex.is_ascii() {
        return None;
    }
    let byte = |start: usize| u8::from_str_radix(&hex[start..start + 2], 16).ok();
    let nibble = |index: usize| {
        u8::from_str_radix(&hex[index..index + 1], 16)
            .ok()
            .map(|value| value * 17)
    };
    match hex.len() {
        3 => Some((nibble(0)?, nibble(1)?, nibble(2)?, 255)),
        4 => Some((nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?)),
        6 => Some((byte(0)?, byte(2)?, byte(4)?, 255)),
        8 => Some((byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

fn inferred_light(colors: &Map<String, Value>) -> bool {
    let Some((r, g, b, _)) = colors
        .get("editor.background")
        .and_then(Value::as_str)
        .and_then(parse_rgba)
    else {
        return false;
    };
    (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 > 150
}

pub(crate) fn slug(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            separator = false;
        } else if !separator && !result.is_empty() {
            result.push('-');
            separator = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    if result.is_empty() {
        "theme".to_string()
    } else {
        result
    }
}

pub(crate) fn parse_jsonc(input: &str) -> Result<Value, String> {
    let without_comments = strip_json_comments(input);
    let without_trailing_commas = strip_trailing_commas(&without_comments);
    serde_json::from_str(&without_trailing_commas).map_err(|error| error.to_string())
}

fn strip_json_comments(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            output.push(ch);
            index += 1;
        } else if ch == '/' && chars.get(index + 1) == Some(&'/') {
            index += 2;
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
        } else if ch == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                if chars[index] == '\n' {
                    output.push('\n');
                }
                index += 1;
            }
            index = (index + 2).min(chars.len());
        } else {
            output.push(ch);
            index += 1;
        }
    }
    output
}

fn strip_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            output.push(ch);
        } else if ch == ',' {
            let mut next = index + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            if next < chars.len() && matches!(chars[next], '}' | ']') {
                index += 1;
                continue;
            }
            output.push(ch);
        } else {
            output.push(ch);
        }
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vscode_python_scopes_match_monokai_st3_preview() {
        use serde_json::json;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("theme.json");
        // Relevant Monokai ST3 rules, including the scoped overrides its
        // advertised Python preview depends on. No local installation needed.
        std::fs::write(&path, json!({"colors":{"editor.foreground":"#f8f8f2","editor.background":"#272822"},"tokenColors":[
            {"scope":"string","settings":{"foreground":"#e6db74"}},
            {"scope":"keyword","settings":{"foreground":"#f92672"}},
            {"scope":"storage","settings":{"foreground":"#f92672","fontStyle":""}},
            {"scope":"storage.type","settings":{"foreground":"#66d9ef","fontStyle":"italic"}},
            {"scope":"entity.name.function","settings":{"foreground":"#a6e22e","fontStyle":""}},
            {"scope":"support.function","settings":{"foreground":"#66d9ef"}},
            {"scope":"support.type, support.class","settings":{"foreground":"#66d9ef","fontStyle":"italic"}},
            {"scope":"variable.parameter","settings":{"foreground":"#fd971f","fontStyle":"italic"}},
            {"scope":"entity.name.type.class.python","settings":{"foreground":"#a6e22e"}},
            {"scope":"meta.class.python support.type.python","settings":{"foreground":"#a6e22e"}},
            {"scope":"punctuation.definition.decorator.python","settings":{"foreground":"#f92672"}},
            {"scope":"entity.name.function.decorator.python","settings":{"foreground":"#66d9ef"}}
        ]}).to_string()).unwrap();
        let theme = crate::vscode_theme::load_theme_file(&path,None,None).unwrap().syntect_theme.unwrap();
        assert_python_preview_colors(&theme);
    }

    #[test]
    #[ignore = "requires the user's installed Monokai ST3 package; read-only comparison"]
    fn installed_monokai_st3_matches_screenshot_tokens() {
        let path = crate::portable::theme_extensions_dir().join("AndreyVolosovich.monokai-st3/themes/Monokai-ST3-color-theme.json");
        let theme = load_theme_file(&path, None, None).unwrap().syntect_theme.unwrap();
        assert_python_preview_colors(&theme);
    }

    fn assert_python_preview_colors(theme: &Theme) {
        use syntect::{easy::HighlightLines, highlighting::{Color, FontStyle}};
        let set = crate::syntax_assets::build_syntax_set(&[Path::new(env!("CARGO_MANIFEST_DIR")).join("syntaxes/Python")]).unwrap();
        let syntax = set.find_syntax_by_extension("py").unwrap();
        assert_eq!(syntax.name, "Python");
        let mut parser = HighlightLines::new(syntax, theme);
        let source = "class Foo(object):\n    def __init__(self):\n        print(42)\n\n    @property\n    def foo(self):\n        return 'bar'\ntext = \"\"\"\nclass Hidden\n\"\"\"\nclass Visible:\n";
        let checks = [
            (0,"class",[102,217,239],true), (0,"Foo",[166,226,46],false),
            (0,"object",[166,226,46],true), (0,"(",[248,248,242],false),
            (1,"def",[102,217,239],true), (1,"__init__",[102,217,239],false),
            (1,"self",[253,151,31],true), (4,"@",[249,38,114],false),
            (4,"property",[102,217,239],true), (5,"foo",[166,226,46],false),
            (8,"class",[230,219,116],false), (10,"class",[102,217,239],true)
        ];
        for (line_index,line) in source.lines().enumerate() {
            let line = format!("{line}\n");
            let spans = parser.highlight_line(&line,&set).unwrap();
            let per_byte: Vec<_> = spans.iter().flat_map(|(s,t)| std::iter::repeat_n(*s,t.len())).collect();
            for (_,token,rgb,italic) in checks.iter().filter(|c| c.0 == line_index) {
                let offset = line.find(token).unwrap();
                let actual = per_byte[offset];
                assert_eq!(actual.foreground, Color{r:rgb[0],g:rgb[1],b:rgb[2],a:255}, "{token}: {spans:?}");
                assert_eq!(actual.font_style.contains(FontStyle::ITALIC), *italic, "{token}: font style");
            }
        }
    }

    #[test]
    fn independently_resolves_workbench_colors_on_their_own_surfaces() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("theme.json");
        std::fs::write(&path, r##"{"type":"light", "colors": {
            "editor.background":"#ffffff", "sideBar.background":"#102030",
            "tab.activeForeground":"#123456", "tab.inactiveForeground":"#654321",
            "editorLineNumber.foreground":"#010203", "editorLineNumber.activeForeground":"#040506",
            "editor.findMatchBackground":"#ff000080", "editor.findMatchHighlightBackground":"#00ff0080",
            "terminal.background":"#000000", "terminal.foreground":"#ffffff80", "terminal.ansiRed":"#010101",
            "editorBracketHighlight.foreground6":"#fedcba", "editorError.foreground":"#aa1122"
        }}"##).unwrap();
        let c = load_theme_file(&path, None, None)
            .unwrap()
            .to_theme_colors();
        use ratatui::style::Color::Rgb;
        assert_eq!(c.ui.fg_tab_active, Rgb(18, 52, 86));
        assert_eq!(c.ui.fg_tab_inactive, Rgb(101, 67, 33));
        assert_eq!(c.ui.fg_line_number, Rgb(1, 2, 3));
        assert_eq!(c.ui.fg_line_number_active, Rgb(4, 5, 6));
        assert_eq!(c.ui.bg_find_current, Rgb(255, 127, 127));
        assert_eq!(c.bg_search_match, Rgb(127, 255, 127));
        assert_eq!(c.ui.fg_terminal, Rgb(128, 128, 128));
        assert_eq!(c.ui.ansi[1], Rgb(1, 1, 1));
        assert_eq!(c.ui.brackets[5], Some(Rgb(254, 220, 186)));
        assert_eq!(
            c.ui.diagnostic(lsp_types::DiagnosticSeverity::ERROR),
            Rgb(170, 17, 34)
        );
        assert_eq!(c.ui.terminal_color(Rgb(22, 33, 44), true), Rgb(22, 33, 44));
    }

    #[test]
    fn mayukai_dark_inherits_widget_colors_and_preserves_search_syntax() {
        // Relevant colors from Mayukai Dark (MIT):
        // https://github.com/GulajavaMinistudio/Mayukai-Theme/blob/master/themes/Mayukai-darker-color-theme.json
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mayukai.json");
        std::fs::write(&path, r##"{"type":"dark", "colors": {
            "foreground":"#3d424d", "editor.background":"#080c11", "editor.foreground":"#dadbc0",
            "sideBar.background":"#03080f", "sideBar.foreground":"#b8b0b0",
            "editorWidget.background":"#0d1016", "editorWidget.foreground":"#cbccc6",
            "input.background":"#0d1016", "input.foreground":"#b3b1ad",
            "list.highlightForeground":"#e6b450", "quickInputList.focusBackground":"#01060e",
            "editor.findMatchBackground":"#e6b4500d", "editor.findMatchBorder":"#e6b450",
            "editor.findMatchHighlightBackground":"#e6b4500d", "editor.findMatchHighlightBorder":"#e6b45059"
        }}"##).unwrap();
        let c = load_theme_file(&path, None, None).unwrap().to_theme_colors();
        use ratatui::style::{Color::Rgb, Modifier, Style};
        assert_eq!(c.ui.bg_quick_input, Rgb(13, 16, 22));
        assert_eq!(c.ui.fg_quick_input, Rgb(203, 204, 198));
        assert_eq!(c.ui.fg_sidebar, Rgb(184, 176, 176));
        assert_eq!(c.ui.bg_quick_selected, Rgb(1, 6, 14));
        assert_eq!(c.ui.fg_quick_match, Rgb(230, 180, 80));
        assert_eq!(c.fg_input, Rgb(179, 177, 173));
        assert_eq!(c.ui.bg_find_current, Rgb(19, 21, 20));
        let token = Style::default().fg(Rgb(255, 143, 64)).add_modifier(Modifier::ITALIC);
        for (current, border) in [(true, Rgb(230, 180, 80)), (false, Rgb(85, 71, 39))] {
            let style = c.ui.find_style(token, current, c.bg_search_match);
            assert_eq!(style.fg, token.fg);
            assert_eq!(style.underline_color, Some(border));
            assert!(style.add_modifier.contains(Modifier::ITALIC | Modifier::UNDERLINED));
        }
    }

    #[test]
    fn widget_defaults_are_independent_of_menu_and_editor_colors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("theme.json");
        for (kind, widget, foreground, current) in [
            ("light", "#f3f3f3", "#616161", "#a8ac94"),
            ("dark", "#252526", "#cccccc", "#515c6a"),
        ] {
            std::fs::write(&path, serde_json::json!({"type":kind, "colors": {
                "editor.foreground":"#abcdef", "menu.background":"#ff0000", "menu.foreground":"#00ff00"
            }}).to_string()).unwrap();
            let c = load_theme_file(&path, None, None).unwrap().to_theme_colors();
            assert_eq!(c.ui.bg_quick_input, crate::app_theme_loader::hex_to_color(widget));
            assert_eq!(c.ui.fg_quick_input, crate::app_theme_loader::hex_to_color(foreground));
            assert_eq!(c.ui.bg_find_current, crate::app_theme_loader::hex_to_color(current));
            assert_eq!(c.ui.fg_find, None);
            assert_eq!(c.ui.border_find, None);
        }
    }

    #[test]
    fn guide_levels_and_editor_annotations_have_independent_native_colors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("theme.json");
        let mut colors = serde_json::Map::new();
        for (group, prefix) in ["editorIndentGuide.background", "editorIndentGuide.activeBackground", "editorBracketPairGuide.background", "editorBracketPairGuide.activeBackground"].iter().enumerate() {
            for level in 1..=6 {
                colors.insert(format!("{prefix}{level}"), Value::String(format!("#{:02x}{level:02x}00", group + 1)));
            }
        }
        colors.insert("editorCodeLens.foreground".into(), Value::String("#112233".into()));
        colors.insert("editorRuler.foreground".into(), Value::String("#445566".into()));
        colors.insert("editorGutter.foldingControlForeground".into(), Value::String("#778899".into()));
        std::fs::write(&path, serde_json::json!({"colors": colors}).to_string()).unwrap();
        let c = load_theme_file(&path, None, None).unwrap().to_theme_colors();
        use ratatui::style::Color::Rgb;
        for (group, resolved) in [c.ui.indent_guides, c.ui.indent_guides_active, c.ui.bracket_guides, c.ui.bracket_guides_active].iter().enumerate() {
            for (level, color) in resolved.iter().enumerate() {
                assert_eq!(*color, Rgb(group as u8 + 1, level as u8 + 1, 0));
            }
        }
        assert_eq!(c.ui.codelens_foreground.over(c.bg_editor), Some(Rgb(17, 34, 51)));
        assert_eq!(c.ui.ruler_foreground.over(c.bg_editor), Some(Rgb(68, 85, 102)));
        assert_eq!(c.ui.fg_folding, Rgb(119, 136, 153));
    }

    #[test]
    fn indent_guides_reuse_single_theme_colors_at_every_depth() {
        for theme in crate::app_theme_loader::load_builtin_themes() {
            let colors = theme.to_theme_colors();
            for active in [false, true] {
                let first = colors.ui.indent_guide_color(0, active);
                assert_ne!(first, colors.bg_editor, "{} active={active}", theme.name);
                for depth in 1..18 {
                    assert_eq!(colors.ui.indent_guide_color(depth, active), first, "{} depth={depth} active={active}", theme.name);
                }
            }
        }
    }

    #[test]
    fn indent_guide_palettes_cycle_enabled_slots_including_five_color_palettes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("guides.json");
        let mut colors = serde_json::json!({
            "editor.background": "#111111",
            "editorIndentGuide.background6": "#ffffff00",
            "editorIndentGuide.activeBackground1": "#abcdef",
            "editorIndentGuide.activeBackground2": null,
        });
        let palette = ["#aa0000", "#00aa00", "#0000aa", "#aaaa00", "#00aaaa"];
        for (index, color) in palette.iter().enumerate() {
            colors[format!("editorIndentGuide.background{}", index + 1)] = (*color).into();
        }
        std::fs::write(&path, serde_json::json!({"colors": colors}).to_string()).unwrap();
        let theme = load_theme_file(&path, None, None).unwrap().to_theme_colors();
        for depth in 0..19 {
            assert_eq!(theme.ui.indent_guide_color(depth, false), crate::app_theme_loader::hex_to_color(palette[depth % 5]));
            assert_eq!(theme.ui.indent_guide_color(depth, true), ratatui::style::Color::Rgb(171, 205, 239));
        }
        // Transparent, null and background-matched slots must never create gaps.
        colors["editorIndentGuide.background2"] = "#00aa0000".into();
        colors["editorIndentGuide.background3"] = Value::Null;
        colors["editorIndentGuide.background4"] = "#111111".into();
        std::fs::write(&path, serde_json::json!({"colors": colors}).to_string()).unwrap();
        let theme = load_theme_file(&path, None, None).unwrap().to_theme_colors();
        for depth in 0..12 {
            assert_eq!(theme.ui.indent_guide_color(depth, false), crate::app_theme_loader::hex_to_color(palette[if depth % 2 == 0 { 0 } else { 4 }]));
        }
    }

    #[test]
    fn malformed_unicode_and_private_themes_are_rejected_without_panicking() {
        assert_eq!(parse_rgba("#€€"), None);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("old.json");
        std::fs::write(
            &path,
            r##"{"config_key":"old","colors":{"bg_dark":"#123456"}}"##,
        )
        .unwrap();
        assert!(load_theme_file(&path, None, None).is_err());
    }

    #[test]
    fn jsonc_keeps_urls_and_removes_comments_and_trailing_commas() {
        let value = parse_jsonc(
            r##"{
                // comment
                "url": "https://vscodethemes.com/a//b",
                "colors": { "editor.background": "#123", },
            }"##,
        )
        .unwrap();
        assert_eq!(value["url"], "https://vscodethemes.com/a//b");
        assert_eq!(value["colors"]["editor.background"], "#123");
    }

    #[test]
    fn vscode_color_forms_and_alpha_are_supported() {
        assert_eq!(parse_rgba("#ABC"), Some((170, 187, 204, 255)));
        assert_eq!(parse_rgba("#1234"), Some((17, 34, 51, 68)));
        assert_eq!(parse_rgba("#11223344"), Some((17, 34, 51, 68)));
        assert_eq!(composite((255, 255, 255, 0), (10, 20, 30)), (10, 20, 30));
    }

    #[test]
    fn loads_exact_vscode_jsonc_theme_and_textmate_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.jsonc");
        std::fs::write(
            &path,
            r##"{
                "name": "Sample",
                "colors": {
                    "editor.background": "#102030",
                    "sideBar.background": "#203040",
                    "input.background": "#304050",
                    "editor.findMatchHighlightBackground": "#FFAA0080",
                },
                "tokenColors": [{
                    "scope": ["comment", "punctuation.definition.comment"],
                    "settings": { "foreground": "#AABBCC", "fontStyle": "italic" },
                }],
                "semanticTokenColors": { "type": "#112233" },
            }"##,
        )
        .unwrap();
        let theme = load_theme_file(&path, None, Some("vs-dark")).unwrap();
        assert_eq!(theme.name, "Sample");
        assert_eq!(theme.colors.bg_editor, "#102030");
        assert_eq!(theme.colors.bg_sidebar, "#203040");
        assert_eq!(theme.colors.bg_input.as_deref(), Some("#304050"));
        assert_eq!(theme.syntect_theme.as_ref().unwrap().scopes.len(), 1);
        assert_eq!(
            theme.semantic_colors.as_ref().unwrap()["type"],
            (17, 34, 51)
        );
    }

    #[test]
    fn theme_include_inherits_then_overrides() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("base.json"),
            r##"{"colors":{"editor.background":"#111111","sideBar.background":"#222222"}}"##,
        )
        .unwrap();
        let child = dir.path().join("child.json");
        std::fs::write(
            &child,
            r##"{"include":"./base.json","colors":{"sideBar.background":"#333333"}}"##,
        )
        .unwrap();
        let theme = load_theme_file(&child, Some("Child"), Some("vs-dark")).unwrap();
        assert_eq!(theme.colors.bg_editor, "#111111");
        assert_eq!(theme.colors.bg_sidebar, "#333333");
    }

    #[test]
    fn discovers_themes_from_vscode_extension_manifests() {
        let root = tempfile::tempdir().unwrap();
        let extension = root.path().join("publisher.theme-1.0.0");
        std::fs::create_dir_all(extension.join("themes")).unwrap();
        std::fs::write(
            extension.join("package.json"),
            r##"{
                "name": "theme",
                "publisher": "publisher",
                "contributes": { "themes": [{
                    "label": "Manifest Theme",
                    "uiTheme": "vs-dark",
                    "path": "./themes/dark.json",
                }] },
            }"##,
        )
        .unwrap();
        std::fs::write(
            extension.join("themes").join("dark.json"),
            r##"{"colors":{"editor.background":"#010203"},"tokenColors":[]}"##,
        )
        .unwrap();

        let themes = load_extension_themes(root.path());
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].name, "Manifest Theme");
        assert_eq!(themes[0].colors.bg_editor, "#010203");
        assert!(themes[0].config_key.starts_with("vscode-publisher-theme-"));
    }

    #[test]
    fn resolves_localized_extension_theme_labels() {
        let root = tempfile::tempdir().unwrap();
        let extension = root.path().join("vscode.theme-defaults-1.0.0");
        std::fs::create_dir_all(extension.join("themes")).unwrap();
        std::fs::write(
            extension.join("package.json"),
            r##"{
                "name": "theme-defaults",
                "publisher": "vscode",
                "contributes": { "themes": [{
                    "id": "Dark+",
                    "label": "%darkPlusColorThemeLabel%",
                    "uiTheme": "vs-dark",
                    "path": "./themes/dark_plus.json",
                }] },
            }"##,
        )
        .unwrap();
        std::fs::write(
            extension.join("package.nls.json"),
            r##"{"darkPlusColorThemeLabel":"Dark+"}"##,
        )
        .unwrap();
        std::fs::write(
            extension.join("themes/dark_plus.json"),
            r##"{"colors":{"editor.background":"#1E1E1E"},"tokenColors":[]}"##,
        )
        .unwrap();

        let themes = load_extension_themes(root.path());
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].name, "Dark+");
        assert!(themes[0].config_key.ends_with("-dark"));
    }

}
