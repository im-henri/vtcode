//! Infer a conservative C-style default from installed grammar declarations.
//! Inspect grammar metadata once per registry, never document text or colors.
use super::Syntax;
use crate::textmate::{GrammarSource, SyntaxRegistry};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) struct Automatic {
    registry: Arc<SyntaxRegistry>,
    profiles: BTreeMap<String, Syntax>,
}

impl std::fmt::Debug for Automatic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Automatic").field("profiles", &self.profiles).finish()
    }
}
impl PartialEq for Automatic {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.registry, &other.registry) }
}
impl Eq for Automatic {}

impl Automatic {
    pub(crate) fn new(registry: Arc<SyntaxRegistry>) -> Option<Arc<Self>> {
        let mut profiles = BTreeMap::new();
        for source in registry.sources.values() {
            // Existing language parsers retain their own string/comment rules.
            // Injection grammars describe embedded fragments, not file syntax.
            if Syntax::for_language(&source.id).is_some()
                || Syntax::for_language(&source.name).is_some()
                || !source.grammar["scopeName"].as_str().is_some_and(|scope| scope.starts_with("source."))
                || source.grammar.get("injectionSelector").is_some()
                || source.grammar.get("injectTo").is_some()
            { continue; }
            let mut features = Features::default();
            features.visit(&source.grammar);
            if features.braces && features.line_comments && features.block_comments {
                profiles.insert(source.id.clone(), Syntax::Cpp);
            }
        }
        (!profiles.is_empty()).then(|| Arc::new(Self { registry, profiles }))
    }

    pub(crate) fn for_editor(&self, editor: &crate::editor::Editor) -> Option<(&GrammarSource, Syntax)> {
        // Use the highlighting resolver so language overrides/associations win
        // over extensions, and overlapping custom file types stay consistent.
        let syntax = crate::highlight::select_syntax(&self.registry, editor.syntax_file_path(), editor.syntax_language_token(), "");
        Some((self.registry.sources.get(&syntax.id)?, *self.profiles.get(&syntax.id)?))
    }

    pub(crate) fn for_language(&self, language: &str) -> Option<Syntax> {
        let syntax = self.registry.find_syntax_by_token(language)?;
        self.profiles.get(&syntax.id).copied()
    }
}

#[derive(Default)]
struct Features {
    braces: bool,
    line_comments: bool,
    block_comments: bool,
}
impl Features {
    fn visit(&mut self, value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                let name = value["name"].as_str().unwrap_or_default();
                let begin = value["begin"].as_str().unwrap_or_default();
                let end = value["end"].as_str().unwrap_or_default();
                self.braces |= matches!(begin, "\\{" | "{") && matches!(end, "\\}" | "}");
                self.line_comments |= name.starts_with("comment.line.") && begin == "//";
                self.block_comments |= name.starts_with("comment.block.") && begin == "/\\*" && end == "\\*/";
                for child in object.values() { self.visit(child); }
            }
            serde_json::Value::Array(array) => {
                for child in array { self.visit(child); }
            }
            _ => {}
        }
    }
}
