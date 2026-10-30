//! Built-in file-icon theme, independent of the workbench color theme.
//!
//! As in VS Code's file-icon themes, a file association owns both its glyph and
//! color. UI foregrounds, selection overrides and contrast substitutions must
//! not recolor these icons. Keep the existing palette here rather than in UI
//! renderers; product icons and surrounding text still use workbench colors.

use ratatui::style::Color;
use std::path::Path;

/// Resolve explicit associations before built-in filename and extension icons.
pub(crate) fn for_path(
    path: &Path,
    associations: &crate::file_associations::Associations,
) -> (&'static str, Color) {
    crate::file_associations::resolve(associations, path).map_or_else(
        || for_name(&path.file_name().unwrap_or_default().to_string_lossy()),
        |(_, language)| for_language(language),
    )
}

/// Open documents also honor temporary language modes and historical filenames.
pub(crate) fn for_editor(editor: &crate::editor::Editor) -> (&'static str, Color) {
    if let Some(language) = editor.syntax_language_token() {
        return for_language(language);
    }
    let name = editor
        .syntax_file_path()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| editor.file_name());
    for_name(name)
}

/// Reuse the icon theme's glyph and color for the selected language. Languages
/// without a themed icon use the neutral icon rather than the original type.
pub(crate) fn for_language(language: &str) -> (&'static str, Color) {
    let name = match crate::file_associations::language_id(language).as_str() {
        "rust" => "file.rs",
        "python" => "file.py",
        "javascript" | "javascriptreact" | "jsx" => "file.js",
        "typescript" | "typescriptreact" | "tsx" => "file.ts",
        "html" => "file.html",
        "css" | "scss" | "sass" | "less" => "file.css",
        "json" | "jsonc" | "jsonwithcomments" | "json5" => "file.json",
        "markdown" | "mdx" => "file.md",
        "toml" | "toml(cargo)" | "yaml" | "ini" | "properties" | "editorconfig" => "file.toml",
        "shell" | "shellscript" | "powershell" | "batch" | "batchfile" | "bat" | "fish" | "zsh" => {
            "file.sh"
        }
        "go" => "file.go",
        "c" | "objective-c" => "file.c",
        "cpp" | "objective-cpp" => "file.cpp",
        "assembly" | "asm" | "asmx86_64" | "mipsassembly" | "risc-v" | "riscv" => "file.asm",
        "java" => "file.java",
        "ruby" => "file.rb",
        "php" => "file.php",
        "swift" => "file.swift",
        "kotlin" => "file.kt",
        "xml" | "xsl" => "file.xml",
        "sql" => "file.sql",
        "dockerfile" | "docker" => "Dockerfile",
        "makefile" | "make" => "Makefile",
        "gitconfig" | "gitignore" | "ignore" => ".gitignore",
        "env" | "dotenv" => ".env",
        "csv" => "file.csv",
        "webassembly" | "wasm" | "wat" => "file.wasm",
        _ => "",
    };
    for_name(name)
}

pub(crate) fn for_name(name: &str) -> (&'static str, Color) {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if name.eq_ignore_ascii_case("dockerfile")
        || name
            .get(..11)
            .is_some_and(|s| s.eq_ignore_ascii_case("dockerfile."))
    {
        return ("Dk ", Color::Rgb(56, 150, 230));
    }
    if name.eq_ignore_ascii_case("makefile") || name.eq_ignore_ascii_case("gnumakefile") {
        return ("Mk ", Color::Rgb(229, 160, 63));
    }
    if [".gitignore", ".gitmodules", ".gitattributes"]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
    {
        return ("Gt ", Color::Rgb(241, 80, 47));
    }
    if name.eq_ignore_ascii_case(".env")
        || name
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case(".env."))
    {
        return ("En ", Color::Rgb(234, 195, 55));
    }

    // VS Code file associations are case insensitive. Comparing the short
    // suffixes directly avoids allocating a lowercase filename on every draw.
    let ext = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    const ASSOCIATIONS: &[(&[&str], &str, Color)] = &[
        (&["rs"], "Rs ", Color::Rgb(222, 165, 132)),
        (&["py"], "Py ", Color::Rgb(55, 118, 171)),
        (&["js", "jsx", "mjs"], "JS ", Color::Rgb(241, 224, 90)),
        (&["ts", "tsx"], "TS ", Color::Rgb(49, 120, 198)),
        (&["html", "htm"], "<> ", Color::Rgb(228, 77, 38)),
        (
            &["css", "scss", "sass", "less"],
            "#  ",
            Color::Rgb(86, 156, 214),
        ),
        (&["json", "jsonc"], "{} ", Color::Rgb(253, 214, 69)),
        (&["md", "markdown"], "Md ", Color::Rgb(55, 160, 230)),
        (
            &["toml", "yaml", "yml", "ini", "cfg", "conf"],
            "Cf ",
            Color::Rgb(160, 160, 160),
        ),
        (
            &["sh", "bash", "zsh", "fish", "ps1", "bat", "cmd"],
            ">_ ",
            Color::Rgb(77, 168, 77),
        ),
        (&["go"], "Go ", Color::Rgb(0, 173, 216)),
        (&["lock"], "Lk ", Color::Rgb(130, 130, 130)),
        (&["c", "h"], "C  ", Color::Rgb(85, 85, 255)),
        (&["cpp", "hpp", "cxx", "cc"], "C+ ", Color::Rgb(0, 122, 204)),
        (&["asm", "s"], "As ", Color::Rgb(205, 73, 69)),
        (&["ok"], "≡  ", Color::Rgb(150, 150, 150)),
        (&["java"], "Jv ", Color::Rgb(176, 114, 25)),
        (&["rb"], "Rb ", Color::Rgb(204, 52, 45)),
        (&["php"], "Ph ", Color::Rgb(119, 123, 180)),
        (&["swift"], "Sw ", Color::Rgb(240, 81, 56)),
        (&["kt", "kts"], "Kt ", Color::Rgb(127, 82, 255)),
        (&["xml", "svg"], "<> ", Color::Rgb(227, 98, 9)),
        (&["sql"], "DB ", Color::Rgb(86, 156, 214)),
        (&["docker", "dockerfile"], "Dk ", Color::Rgb(56, 150, 230)),
        (
            &["gitignore", "gitmodules", "gitattributes"],
            "Gt ",
            Color::Rgb(241, 80, 47),
        ),
        (&["env"], "En ", Color::Rgb(234, 195, 55)),
        (&["csv"], "Cv ", Color::Rgb(77, 168, 77)),
        (
            &["png", "jpg", "jpeg", "gif", "bmp", "ico", "webp"],
            "Im ",
            Color::Rgb(160, 110, 200),
        ),
        (
            &["zip", "tar", "gz", "rar", "7z"],
            "Zp ",
            Color::Rgb(175, 120, 60),
        ),
        (&["pdf"], "Pd ", Color::Rgb(200, 50, 50)),
        (&["wasm"], "Wa ", Color::Rgb(101, 79, 240)),
    ];
    ASSOCIATIONS
        .iter()
        .find(|(extensions, _, _)| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
        .map_or(("·  ", Color::Rgb(160, 160, 160)), |(_, glyph, color)| {
            (*glyph, *color)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_names_and_ids_share_the_file_icon_theme() {
        for (language, filename) in [
            ("Rust", "file.rs"),
            ("C++", "file.cpp"),
            ("cpp", "file.cpp"),
            ("Python", "file.py"),
            ("JavaScript (Babel)", "file.js"),
            ("TypeScript React", "file.tsx"),
            ("Shell Script (Bash)", "file.sh"),
            ("Shell", "file.sh"),
            ("Batch File", "file.bat"),
            ("MIPS Assembly", "file.s"),
            ("RISC-V", "file.s"),
            ("JSON with Comments", "file.jsonc"),
            ("Makefile", "Makefile"),
            ("Dockerfile", "Dockerfile"),
            ("Plain Text", "unknown"),
            ("plaintext", "unknown"),
            ("A language without a themed icon", "unknown"),
        ] {
            assert_eq!(for_language(language), for_name(filename), "{language}");
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(for_language(language).0),
                3,
                "associations preserve tree and tab hit geometry"
            );
        }
    }

    #[test]
    fn explicit_rules_replace_builtin_extensions_and_special_filename_icons() {
        let associations = [
            ("*.rs", "C++"),
            ("*.custom", "Python"),
            ("special.rs", "JavaScript"),
            ("Dockerfile.*", "Rust"),
            (".gitignore", "Plain Text"),
        ]
        .map(|(pattern, language)| (pattern.into(), language.into()))
        .into_iter()
        .collect();
        for (filename, expected) in [
            ("src/main.rs", "file.cpp"),
            ("src/special.rs", "file.js"),
            ("model.custom", "file.py"),
            ("Dockerfile.rs", "file.rs"),
            (".gitignore", "unknown"),
            ("unmatched.py", "file.py"),
            ("MAIN.RS", "file.rs"),
        ] {
            assert_eq!(
                for_path(Path::new(filename), &associations),
                for_name(expected),
                "{filename}"
            );
        }
    }

    #[test]
    fn unsaved_documents_honor_associations_and_temporary_language_choices() {
        let mut editor = crate::editor::Editor::test_with_text("// source");
        editor.suggested_file_name = Some("model.rs".into());
        editor
            .language_associations
            .insert("*.rs".into(), "C++".into());
        assert_eq!(for_editor(&editor), for_name("file.cpp"));
        editor.language_override = Some("Python".into());
        assert_eq!(for_editor(&editor), for_name("file.py"));
        editor.language_override = None;
        editor.language_associations.clear();
        assert_eq!(for_editor(&editor), for_name("file.rs"));
        editor.language_override = Some("Plain Text".into());
        assert_eq!(for_editor(&editor), for_name("unknown"));
    }

    #[test]
    fn associations_are_case_insensitive_and_filename_matches_take_precedence() {
        for (name, reference) in [
            ("CHECK.PY", "check.py"),
            ("C:\\work\\CHECK.PY", "check.py"),
            ("/work/组件/CHECK.PY", "check.py"),
            ("SCRIPT.PS1", "script.ps1"),
            ("HEADER.HPP", "header.hpp"),
            (".GITIGNORE", ".gitignore"),
            ("C:\\work\\.GITATTRIBUTES", ".gitattributes"),
            ("DOCKERFILE", "file.dockerfile"),
            ("Dockerfile.PY", "file.dockerfile"),
            ("GNUMAKEFILE", "Makefile"),
            (".ENV.PY", "file.env"),
            ("/work/.env.local", "file.env"),
            ("README", "unknown.extension"),
            ("组件", "unknown.extension"),
        ] {
            assert_eq!(for_name(name), for_name(reference), "{name}");
        }
    }
}
