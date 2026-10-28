//! The editor's single syntax system: native TextMate grammars and VS Code themes.
//! Registries and themes are immutable snapshots shared with highlighting workers.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

pub(crate) use irosashi::FontStyle;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}
impl Color {
    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub fn parse(value: &str) -> Option<Self> {
        let hex = value.trim().strip_prefix('#')?;
        if !hex.is_ascii() {
            return None;
        }
        let byte = |i| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let nibble = |i| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|n| n * 17);
        let (r, g, b, a) = match hex.len() {
            3 => (nibble(0)?, nibble(1)?, nibble(2)?, 255),
            4 => (nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?),
            6 => (byte(0)?, byte(2)?, byte(4)?, 255),
            8 => (byte(0)?, byte(2)?, byte(4)?, byte(6)?),
            _ => return None,
        };
        Some(Self { r, g, b, a })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Style {
    pub foreground: Color,
    pub background: Color,
    pub font_style: FontStyle,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            foreground: Color::BLACK,
            background: Color::WHITE,
            font_style: FontStyle::empty(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Theme {
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) matcher: Arc<crate::textmate_theme::Matcher>,
    raw: Arc<Value>,
}
impl PartialEq for Theme {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl Theme {
    #[cfg(test)]
    pub(crate) fn token_rules(&self) -> &[Value] {
        self.raw["tokenColors"]
            .as_array()
            .map_or(&[], Vec::as_slice)
    }
    pub(crate) fn scope_foreground(&self, scopes: &[&str]) -> Color {
        self.matcher
            .style(
                scopes,
                Style {
                    foreground: self.foreground,
                    background: self.background,
                    ..Style::default()
                },
            )
            .foreground
    }
    pub(crate) fn new(
        name: &str,
        foreground: &str,
        background: &str,
        rules: &[Value],
    ) -> Result<Self, String> {
        // VS Code supplies editor defaults itself and ignores unscoped token rules.
        let rules: Vec<_> = rules
            .iter()
            .filter(|r| match &r["scope"] {
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                _ => false,
            })
            .cloned()
            .collect();
        let matcher = Arc::new(crate::textmate_theme::Matcher::new(&rules));
        let raw = json!({"name": name, "colors": {"editor.foreground":foreground,"editor.background":background}, "tokenColors": rules});
        Ok(Self {
            foreground: Color::parse(foreground).unwrap_or(Color::BLACK),
            background: Color::parse(background).unwrap_or(Color::WHITE),
            matcher,
            raw: Arc::new(raw),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ThemeSet {
    pub(crate) themes: BTreeMap<String, Theme>,
}
impl ThemeSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// A source grammar, without translation or loss of TextMate constructs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct GrammarSource {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) filenames: Vec<String>,
    pub(crate) grammar: Value,
}

#[derive(Clone)]
pub(crate) struct Syntax {
    pub(crate) name: String,
    pub(crate) scope: String,
    pub(crate) hidden: bool,
    pub(crate) id: String,
    pub(crate) engine: Arc<irosashi::Highlighter>,
}
impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("name", &self.name)
            .field("scope", &self.scope)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SyntaxRegistry {
    syntaxes: Vec<Syntax>,
    pub(crate) sources: BTreeMap<String, GrammarSource>,
}

const BUNDLED_OVERRIDES: &[(&str, &[u8])] = &[
    (
        "vtcode-plain-text",
        br#"{"scopeName":"text.plain","patterns":[]}"#,
    ),
    (
        "rust",
        include_bytes!("../grammars/Rust/syntaxes/rust.tmLanguage.json"),
    ),
    (
        "python",
        include_bytes!("../grammars/Python/MagicPython.tmLanguage.json"),
    ),
    (
        "regexp",
        include_bytes!("../grammars/Python/MagicRegExp.tmLanguage.json"),
    ),
    (
        "git-commit",
        include_bytes!("../grammars/Git/git-commit.tmLanguage.json"),
    ),
    (
        "git-rebase",
        include_bytes!("../grammars/Git/git-rebase.tmLanguage.json"),
    ),
    (
        "ignore",
        include_bytes!("../grammars/Git/ignore.tmLanguage.json"),
    ),
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LanguageMetadata {
    name: String,
    aliases: Vec<String>,
    extensions: Vec<String>,
    filenames: Vec<String>,
}
fn language_metadata() -> &'static BTreeMap<String, LanguageMetadata> {
    static METADATA: OnceLock<BTreeMap<String, LanguageMetadata>> = OnceLock::new();
    METADATA.get_or_init(|| {
        serde_json::from_str(include_str!("language_metadata.json"))
            .expect("TextMate language metadata")
    })
}
fn catalog_builder() -> irosashi::HighlighterBuilder {
    let mut builder = irosashi::Highlighter::builder();
    for (id, meta) in language_metadata() {
        builder = builder.alias(&meta.name.to_ascii_lowercase(), id);
        for alias in &meta.aliases {
            builder = builder.alias(&alias.to_ascii_lowercase(), id);
        }
        for extension in &meta.extensions {
            builder = builder.extension(extension, id);
        }
        for filename in &meta.filenames {
            builder = builder.filename(filename, id);
        }
    }
    builder
}
fn embedded_engine() -> Arc<irosashi::Highlighter> {
    static ENGINE: OnceLock<Arc<irosashi::Highlighter>> = OnceLock::new();
    Arc::clone(ENGINE.get_or_init(|| {
        let mut builder = catalog_builder();
        for (id, bytes) in BUNDLED_OVERRIDES {
            builder = builder
                .grammar(id, *bytes)
                .expect("bundled TextMate grammar");
        }
        Arc::new(builder.build().expect("bundled TextMate registry"))
    }))
}

impl SyntaxRegistry {
    pub(crate) fn embedded() -> Self {
        Self::from_engine(embedded_engine(), BTreeMap::new())
    }
    fn from_engine(
        engine: Arc<irosashi::Highlighter>,
        sources: BTreeMap<String, GrammarSource>,
    ) -> Self {
        let registry = engine.registry();
        let mut syntaxes: Vec<_> = registry
            .languages()
            .map(|id| {
                let meta = registry.grammar_meta(id).expect("registered grammar");
                let source = sources.get(id);
                Syntax {
                    name: source
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| display_name(id)),
                    scope: meta.scope_name.clone(),
                    hidden: !meta.inject_to.is_empty() || id == "regexp",
                    id: id.to_owned(),
                    engine: Arc::clone(&engine),
                }
            })
            .collect();
        syntaxes.retain(|s| s.id != "vtcode-plain-text");
        syntaxes.push(Syntax {
            name: "Plain Text".into(),
            scope: "text.plain".into(),
            hidden: false,
            id: "vtcode-plain-text".into(),
            engine,
        });
        Self { syntaxes, sources }
    }
    pub(crate) fn syntaxes(&self) -> &[Syntax] {
        &self.syntaxes
    }
    pub(crate) fn find_syntax_plain_text(&self) -> &Syntax {
        self.syntaxes.last().expect("Plain Text")
    }
    fn by_id(&self, id: &str) -> Option<&Syntax> {
        self.syntaxes.iter().find(|s| s.id == id)
    }
    pub(crate) fn find_syntax_by_name(&self, name: &str) -> Option<&Syntax> {
        self.syntaxes
            .iter()
            .rev()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }
    pub(crate) fn find_syntax_by_scope(&self, scope: &str) -> Option<&Syntax> {
        self.syntaxes.iter().find(|s| s.scope == scope)
    }
    pub(crate) fn find_syntax_by_token(&self, name: &str) -> Option<&Syntax> {
        self.find_syntax_by_name(name)
            .or_else(|| {
                let registry = self.find_syntax_plain_text().engine.registry();
                self.by_id(registry.resolve_language(&name.to_ascii_lowercase())?)
            })
            .or_else(|| self.find_syntax_by_extension(name))
    }
    pub(crate) fn find_syntax_by_extension(&self, extension: &str) -> Option<&Syntax> {
        let engine = &self.find_syntax_plain_text().engine;
        engine
            .detect_language(extension)
            .or_else(|| engine.detect_language(&format!("file.{extension}")))
            .and_then(|id| self.by_id(&id))
    }
    pub(crate) fn find_syntax_by_first_line(&self, line: &str) -> Option<&Syntax> {
        // Emacs/Vim modelines describe a language mode independently of its
        // grammar format. Keep extensionless documents and compact diffs usable.
        static MODELINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(
            r"(?i)-\*-.*?\bmode:\s*([\w+.-]+)|-\*-\s*([\w+.-]+)\s*-\*-|\b(?:vi|vim|ex):.*?(?:ft|filetype|syntax)\s*=\s*([\w+.-]+)"
        ).expect("language modelines")
        });
        if let Some(captures) = MODELINE.captures(line) {
            if let Some(syntax) = captures
                .iter()
                .skip(1)
                .flatten()
                .find_map(|m| self.find_syntax_by_token(m.as_str()))
            {
                return Some(syntax);
            }
        }
        self.find_syntax_plain_text()
            .engine
            .detect_language_by_first_line(line)
            .and_then(|id| self.by_id(&id))
    }
    pub(crate) fn into_builder(self) -> SyntaxRegistryBuilder {
        SyntaxRegistryBuilder {
            sources: self.sources,
        }
    }
}

#[derive(Default)]
pub(crate) struct SyntaxRegistryBuilder {
    sources: BTreeMap<String, GrammarSource>,
}
impl SyntaxRegistryBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn add(&mut self, mut source: GrammarSource) {
        // A grammar installed under a new file/name replaces the same scope.
        let scope = source.grammar["scopeName"].as_str().unwrap_or_default();
        if let Some(old) = self
            .sources
            .values()
            .find(|s| s.grammar["scopeName"].as_str() == Some(scope))
        {
            source.id = old.id.clone();
        } else {
            let engine = embedded_engine();
            let registry = engine.registry();
            if let Some(id) = registry.languages().find(|id| {
                registry
                    .grammar_meta(id)
                    .is_some_and(|m| m.scope_name == scope)
            }) {
                source.id = id.to_owned();
            }
        }
        self.sources.insert(source.id.clone(), source);
    }
    pub(crate) fn build(self) -> Result<SyntaxRegistry, String> {
        if self.sources.is_empty() {
            return Ok(SyntaxRegistry::embedded());
        }
        let mut builder = catalog_builder();
        let sources = self.sources;
        for (id, bytes) in BUNDLED_OVERRIDES {
            if !sources.contains_key(*id) {
                builder = builder.grammar(id, *bytes).map_err(|e| e.to_string())?;
            }
        }
        for source in sources.values() {
            builder = builder
                .grammar(&source.id, source.grammar.to_string().into_bytes())
                .map_err(|e| e.to_string())?;
            builder = builder.alias(&source.name.to_ascii_lowercase(), &source.id);
            for alias in &source.aliases {
                builder = builder.alias(&alias.to_ascii_lowercase(), &source.id);
            }
            for filename in &source.filenames {
                builder = builder.filename(filename, &source.id);
            }
            for ext in source.grammar["fileTypes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                builder = builder.extension(ext, &source.id);
            }
        }
        let engine = Arc::new(builder.build().map_err(|e| e.to_string())?);
        for id in sources.keys() {
            engine
                .registry()
                .grammar(id)
                .map_err(|e| format!("{id}: {e}"))?;
        }
        Ok(SyntaxRegistry::from_engine(engine, sources))
    }
}

fn display_name(id: &str) -> String {
    language_metadata()
        .get(id)
        .map_or_else(|| id.to_owned(), |meta| meta.name.clone())
}
