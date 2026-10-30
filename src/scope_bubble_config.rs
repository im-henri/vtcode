//! Per-language scope profiles, compiled only when preferences change.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

pub(crate) type Configs = BTreeMap<String, Config>;

#[path = "scope_bubble_automatic.rs"]
mod automatic;
pub(crate) use automatic::Automatic;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Syntax {
    #[default]
    Automatic,
    C,
    Cpp,
    Csharp,
    Css,
    Go,
    Java,
    Javascript,
    Json,
    Python,
    Rust,
    Shell,
    Fish,
    Vim,
    Cmake,
    Php,
    Lua,
    Ruby,
    Julia,
    Matlab,
    Fortran,
    Disabled,
}
impl Syntax {
    pub(crate) const ALL: [Self; 22] = [
        Self::Automatic,
        Self::C,
        Self::Cpp,
        Self::Csharp,
        Self::Css,
        Self::Go,
        Self::Java,
        Self::Javascript,
        Self::Json,
        Self::Python,
        Self::Rust,
        Self::Shell,
        Self::Fish,
        Self::Vim,
        Self::Cmake,
        Self::Php,
        Self::Lua,
        Self::Ruby,
        Self::Julia,
        Self::Matlab,
        Self::Fortran,
        Self::Disabled,
    ];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Csharp => "C#",
            Self::Css => "CSS",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::Javascript => "JavaScript / TypeScript",
            Self::Json => "JSON",
            Self::Python => "Python",
            Self::Rust => "Rust",
            Self::Disabled => "Disabled",
            Self::Shell => "Shell Script",
            Self::Fish => "Fish",
            Self::Vim => "Vim",
            Self::Cmake => "CMake",
            Self::Php => "PHP",
            Self::Lua => "Lua",
            Self::Ruby => "Ruby",
            Self::Julia => "Julia",
            Self::Matlab => "MATLAB",
            Self::Fortran => "Fortran",
        }
    }
    pub(crate) fn for_language(language: &str) -> Option<Self> {
        Some(match language_key(language).as_str() {
            "c" | "objective-c" | "glsl" | "hlsl" => Self::C,
            "cpp" | "objective-c++" => Self::Cpp,
            "csharp" => Self::Csharp,
            "css" | "scss" | "less" => Self::Css,
            "go" => Self::Go,
            "java" => Self::Java,
            "javascript" | "javascriptreact" | "typescript" | "typescriptreact" => Self::Javascript,
            "json" => Self::Json,
            "python" => Self::Python,
            "rust" => Self::Rust,
            "shellscript" => Self::Shell,
            "fish" => Self::Fish,
            "vim" => Self::Vim,
            "cmake" => Self::Cmake,
            "php" => Self::Php,
            "lua" => Self::Lua,
            "ruby" => Self::Ruby,
            "julia" => Self::Julia,
            "matlab" => Self::Matlab,
            "fortran" => Self::Fortran,
            _ => return None,
        })
    }
    pub(crate) fn has_delimiters(self) -> bool {
        matches!(
            self,
            Self::C
                | Self::Cpp
                | Self::Csharp
                | Self::Css
                | Self::Go
                | Self::Java
                | Self::Javascript
                | Self::Json
                | Self::Python
                | Self::Rust
        )
    }
    pub(crate) fn language(self) -> Option<&'static str> {
        match self {
            Self::Automatic => None,
            Self::Javascript => Some("JavaScript"),
            Self::Disabled => Some("Plain Text"),
            _ => Some(self.label()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Delimiter {
    #[serde(rename = "{}")]
    Braces,
    #[serde(rename = "()")]
    Parentheses,
    #[serde(rename = "[]")]
    Brackets,
}
impl Delimiter {
    pub(crate) const ALL: [Self; 3] = [Self::Braces, Self::Parentheses, Self::Brackets];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Braces => "Braces { }",
            Self::Parentheses => "Parentheses ( )",
            Self::Brackets => "Brackets [ ]",
        }
    }
    pub(crate) fn bit(self) -> u8 {
        1 << self as u8
    }
    pub(crate) fn token(ch: char) -> Option<(Self, bool)> {
        Some(match ch {
            '{' => (Self::Braces, true),
            '}' => (Self::Braces, false),
            '(' => (Self::Parentheses, true),
            ')' => (Self::Parentheses, false),
            '[' => (Self::Brackets, true),
            ']' => (Self::Brackets, false),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Config {
    pub syntax: Syntax,
    /// None retains the parser's built-in delimiters; [] explicitly disables them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delimiters: Option<Vec<Delimiter>>,
    pub language_blocks: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            syntax: Syntax::Automatic,
            delimiters: None,
            language_blocks: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    pub syntax: Syntax,
    pub delimiters: Option<u8>,
    pub language_blocks: bool,
}

impl Rule {
    pub(crate) fn syntax_for(self, editor: &crate::editor::Editor) -> Syntax {
        if self.syntax == Syntax::Automatic {
            Syntax::for_language(language(editor)).unwrap_or(Syntax::Automatic)
        } else {
            self.syntax
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Rules {
    pub configs: Configs,
    pub(crate) automatic: Option<Arc<Automatic>>,
    // Sorted once; lookup compares ASCII case without allocating a lowercase name.
    compiled: Vec<(String, Rule)>,
}
impl Rules {
    pub(crate) fn empty() -> Arc<Self> {
        static EMPTY: OnceLock<Arc<Rules>> = OnceLock::new();
        EMPTY.get_or_init(|| Arc::new(Self::default())).clone()
    }
    pub(crate) fn compile(configs: &Configs) -> Arc<Self> {
        if configs.is_empty() {
            return Self::empty();
        }
        let configs: Configs = configs
            .iter()
            .map(|(name, config)| (language_key(name), config.clone()))
            .filter(|(name, _)| !name.is_empty())
            .collect();
        let mut compiled: BTreeMap<String, Rule> = configs
            .iter()
            .map(|(name, config)| {
                (
                    name.clone(),
                    Rule {
                        syntax: config.syntax,
                        language_blocks: config.language_blocks,
                        delimiters: config
                            .delimiters
                            .as_ref()
                            .map(|pairs| pairs.iter().fold(0, |mask, pair| mask | pair.bit())),
                    },
                )
            })
            .collect();
        for (canonical, aliases) in crate::animation_config::SCOPE_LANGUAGE_ALIASES {
            if let Some(rule) = compiled.get(*canonical).copied() {
                for alias in *aliases {
                    compiled.insert((*alias).into(), rule);
                }
            }
        }
        Arc::new(Self {
            configs,
            automatic: None,
            compiled: compiled.into_iter().collect(),
        })
    }

    pub(crate) fn with_automatic(self: &Arc<Self>, automatic: Option<Arc<Automatic>>) -> Arc<Self> {
        if self.automatic == automatic { return self.clone(); }
        Arc::new(Self { configs: self.configs.clone(), compiled: self.compiled.clone(), automatic })
    }
    pub(crate) fn get(&self, language: &str) -> Option<Rule> {
        if self.compiled.is_empty() {
            return None;
        }
        let find = |name: &str| {
            self.compiled
                .binary_search_by(|(key, _)| {
                    key.bytes()
                        .cmp(name.trim().bytes().map(|byte| byte.to_ascii_lowercase()))
                })
                .ok()
                .map(|index| self.compiled[index].1)
        };
        find(language)
    }
    pub(crate) fn for_editor(&self, editor: &crate::editor::Editor) -> Option<Rule> {
        let configured = self.get(fallback_language(editor));
        if configured.is_some_and(|rule| rule.syntax != Syntax::Automatic) { return configured; }
        let automatic = self.automatic.as_ref().and_then(|automatic| automatic.for_editor(editor));
        let configured = configured.or_else(|| automatic.and_then(|(source, _)| {
            self.get(&source.name).or_else(|| self.get(&source.id))
                .or_else(|| source.aliases.iter().find_map(|alias| self.get(alias)))
        }));
        match (configured, automatic) {
            (Some(mut rule), Some((_, syntax))) if rule.syntax == Syntax::Automatic => {
                rule.syntax = syntax;
                Some(rule)
            }
            (None, Some((_, syntax))) => Some(Rule { syntax, delimiters: None, language_blocks: true }),
            (rule, _) => rule,
        }
    }
}

pub(crate) fn language_key(name: &str) -> String {
    crate::animation_config::scope_language_key(name).to_ascii_lowercase()
}

pub(crate) fn language(editor: &crate::editor::Editor) -> &str {
    editor.scope_bubble_rules.automatic.as_ref()
        .and_then(|automatic| automatic.for_editor(editor))
        .map_or_else(|| fallback_language(editor), |(source, _)| source.name.as_str())
}

fn fallback_language(editor: &crate::editor::Editor) -> &str {
    editor.syntax_language_token().unwrap_or_else(|| {
        let path = editor.syntax_file_path();
        let name = path
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str());
        match name {
            Some("CMakeLists.txt") => "cmake",
            Some("Makefile" | "makefile" | "GNUmakefile") => "makefile",
            Some("Dockerfile") => "dockerfile",
            Some(".vimrc" | "vimrc" | ".gvimrc" | "gvimrc") => "vim",
            Some(".bashrc" | ".bash_profile" | ".zshrc") => "shellscript",
            _ => {
                if let Some(ext) = path
                    .and_then(|p| p.extension())
                    .and_then(|ext| ext.to_str())
                {
                    return ext;
                }
                if let Some(first) = editor.rope.get_line(0) {
                    let mut chars = first.chars();
                    if chars.next() == Some('#') && chars.next() == Some('!') {
                        let mut matched = 0;
                        for ch in chars.take(254) {
                            if ch == char::from(b"python"[matched]) {
                                matched += 1;
                            } else {
                                matched = usize::from(ch == 'p');
                            }
                            if matched == 6 {
                                return "python";
                            }
                        }
                    }
                }
                "plain text"
            }
        }
    })
}

/// Workspace entries replace the same language only, preserving other User rules.
pub(crate) fn merge(base: &Option<Configs>, overrides: &Option<Configs>) -> Option<Configs> {
    if base.is_none() && overrides.is_none() {
        return None;
    }
    Some(
        base.iter()
            .chain(overrides.iter())
            .flat_map(|map| map.iter())
            .map(|(name, config)| (language_key(name), config.clone()))
            .collect(),
    )
}
