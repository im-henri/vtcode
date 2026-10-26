//! Built-in file-icon theme, independent of the workbench color theme.
//!
//! As in VS Code's file-icon themes, a file association owns both its glyph and
//! color. UI foregrounds, selection overrides and contrast substitutions must
//! not recolor these icons. Keep the existing palette here rather than in UI
//! renderers; product icons and surrounding text still use workbench colors.

use ratatui::style::Color;

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
    use super::for_name;

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
