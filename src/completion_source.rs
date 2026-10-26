//! Lexical buffer identity and exact syntax names for user-choice history.
//! Learned retrieval does not use syntax names, extensions, or file relationships.
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Kind {
    pub(crate) family: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) kind: Kind,
    pub(crate) file: u64,
}

impl Kind {
    pub(crate) fn new(path: Option<&Path>, language: &str) -> Self {
        let syntax = language.trim().to_lowercase();
        let name = if matches!(
            syntax.as_str(),
            "" | "unknown" | "plain text" | "plaintext" | "text"
        ) {
            path.and_then(Path::extension)
                .and_then(|part| part.to_str())
                .unwrap_or_default()
                .to_lowercase()
        } else {
            syntax
        };
        Self {
            family: if name.is_empty() {
                0
            } else {
                crate::completion_context::hash_parts(30, &[&name])
            },
        }
    }
}

fn identity(path: &Path) -> String {
    // Lexical normalization avoids I/O, resolves ./ and ../, and keeps distinct
    // roots/directories distinct while preserving the catalog's path spelling.
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if parts.last().is_some_and(|part: &String| part != "..") => {
                parts.pop();
            }
            Component::ParentDir => parts.push("..".into()),
            _ => parts.push(component.as_os_str().to_string_lossy().into_owned()),
        }
    }
    parts.join("/")
}

impl Source {
    pub(crate) fn new(path: Option<&Path>, language: &str) -> Self {
        Self {
            kind: Kind::new(path, language),
            file: path.map_or(0, |path| {
                crate::completion_context::hash_parts(33, &[&identity(path)])
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_normalizes_lexically_without_merging_independent_files() {
        let source = |path| Source::new(Some(Path::new(path)), "");
        assert_eq!(
            source("project/src/../src/thing.c").file,
            source("project/src/thing.c").file
        );
        assert_ne!(
            source("one/thing.custom").file,
            source("two/thing.custom").file
        );
        assert_ne!(source("one/thing.c").file, source("one/thing.h").file);
    }
    #[test]
    fn selected_syntax_is_an_exact_history_identity_without_alias_tables() {
        let kind = |name| Kind::new(None, name);
        assert_eq!(kind("My Custom Syntax"), kind("my custom syntax"));
        assert_ne!(kind("C"), kind("C++"));
        assert_eq!(kind("Unknown"), Kind::default());
    }
}
