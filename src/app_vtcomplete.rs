use crate::app::App;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CompletionKind {
    #[default]
    Word,
    Function,
    Member,
    Variable,
    Type,
    Value,
    Argument,
    Keyword,
    Snippet,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompletionDetail {
    pub(crate) kind: CompletionKind,
    pub(crate) annotation: &'static str,
    pub(crate) context: crate::completion_context::Context,
}

impl CompletionKind {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Function => "ƒ",
            Self::Member => "m",
            Self::Variable => "v",
            Self::Type => "t",
            Self::Value => "=",
            Self::Argument => "a",
            Self::Keyword => "k",
            Self::Snippet => "s",
            Self::Word => "w",
        }
    }
}
#[cfg(test)]
use crate::app::fuzzy_match_score;

#[derive(Clone, Copy)]
struct VtCompleteCandidate {
    proximity: usize,
    fuzzy_score: i64,
    prefix_match: bool,
    keyword: bool,
}

#[cfg(test)]
fn add_vtcomplete_candidate(
    word_scores: &mut rustc_hash::FxHashMap<String, VtCompleteCandidate>,
    candidate: &str,
    prefix: &str,
    prefix_lower: &str,
    proximity: usize,
    keyword: bool,
) {
    if candidate.len() < 2 || candidate == prefix || candidate.eq_ignore_ascii_case(prefix) {
        return;
    }

    // Scores depend on the word and the query, which stay fixed throughout
    // this request. Repeated occurrences can only improve proximity/priority.
    if let Some(entry) = word_scores.get_mut(candidate) {
        entry.proximity = entry.proximity.min(proximity);
        entry.keyword |= keyword;
        return;
    }

    let candidate_lower = candidate.to_lowercase();
    let prefix_match = candidate.starts_with(prefix) || candidate_lower.starts_with(prefix_lower);
    let Some(fuzzy_score) = fuzzy_match_score(&candidate_lower, prefix_lower) else {
        return;
    };

    word_scores.insert(
        candidate.to_string(),
        VtCompleteCandidate {
            proximity,
            fuzzy_score,
            prefix_match,
            keyword,
        },
    );
}

pub(crate) fn language_keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "Rust" => &[
            "as",
            "async",
            "await",
            "break",
            "const",
            "continue",
            "crate",
            "dyn",
            "else",
            "enum",
            "extern",
            "false",
            "fn",
            "for",
            "if",
            "impl",
            "in",
            "let",
            "loop",
            "match",
            "mod",
            "move",
            "mut",
            "pub",
            "ref",
            "return",
            "self",
            "static",
            "struct",
            "super",
            "trait",
            "true",
            "type",
            "unsafe",
            "use",
            "where",
            "while",
            "yield",
            "String",
            "Vec",
            "Option",
            "Result",
            "Some",
            "None",
            "Ok",
            "Err",
            "println",
            "eprintln",
            "format",
            "vec",
            "todo",
            "unimplemented",
            "unreachable",
            "usize",
            "isize",
            "u8",
            "u16",
            "u32",
            "u64",
            "u128",
            "i8",
            "i16",
            "i32",
            "i64",
            "i128",
            "f32",
            "f64",
            "bool",
            "char",
            "str",
        ],
        "Python" => &[
            "and",
            "as",
            "assert",
            "async",
            "await",
            "break",
            "class",
            "continue",
            "def",
            "del",
            "elif",
            "else",
            "except",
            "finally",
            "for",
            "from",
            "global",
            "if",
            "import",
            "in",
            "is",
            "lambda",
            "nonlocal",
            "not",
            "or",
            "pass",
            "raise",
            "return",
            "try",
            "while",
            "with",
            "yield",
            "True",
            "False",
            "None",
            "self",
            "print",
            "range",
            "len",
            "list",
            "dict",
            "set",
            "tuple",
            "int",
            "float",
            "str",
            "bool",
            "isinstance",
            "enumerate",
        ],
        "JavaScript" | "JavaScript React" => &[
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "debugger",
            "default",
            "delete",
            "do",
            "else",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "let",
            "new",
            "null",
            "of",
            "return",
            "super",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "typeof",
            "undefined",
            "var",
            "void",
            "while",
            "with",
            "yield",
            "console",
            "document",
            "window",
            "Array",
            "Object",
            "Promise",
            "Map",
            "Set",
            "Symbol",
            "RegExp",
            "JSON",
            "Math",
            "Error",
        ],
        "TypeScript" | "TypeScript React" => &[
            "abstract",
            "any",
            "as",
            "async",
            "await",
            "bigint",
            "boolean",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "debugger",
            "declare",
            "default",
            "delete",
            "do",
            "else",
            "enum",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "from",
            "function",
            "get",
            "if",
            "implements",
            "import",
            "in",
            "infer",
            "instanceof",
            "interface",
            "is",
            "keyof",
            "let",
            "module",
            "namespace",
            "never",
            "new",
            "null",
            "number",
            "object",
            "of",
            "override",
            "private",
            "protected",
            "public",
            "readonly",
            "return",
            "set",
            "static",
            "string",
            "super",
            "switch",
            "symbol",
            "this",
            "throw",
            "true",
            "try",
            "type",
            "typeof",
            "undefined",
            "unique",
            "unknown",
            "var",
            "void",
            "while",
            "with",
            "yield",
        ],
        "Go" => &[
            "break",
            "case",
            "chan",
            "const",
            "continue",
            "default",
            "defer",
            "else",
            "fallthrough",
            "for",
            "func",
            "go",
            "goto",
            "if",
            "import",
            "interface",
            "map",
            "package",
            "range",
            "return",
            "select",
            "struct",
            "switch",
            "type",
            "var",
            "append",
            "cap",
            "close",
            "copy",
            "delete",
            "len",
            "make",
            "new",
            "panic",
            "print",
            "println",
            "recover",
            "bool",
            "byte",
            "complex64",
            "complex128",
            "error",
            "float32",
            "float64",
            "int",
            "int8",
            "int16",
            "int32",
            "int64",
            "rune",
            "string",
            "uint",
            "uint8",
            "uint16",
            "uint32",
            "uint64",
            "uintptr",
            "true",
            "false",
            "nil",
            "iota",
        ],
        "C" | "C++" => &[
            "auto",
            "break",
            "case",
            "char",
            "const",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extern",
            "float",
            "for",
            "goto",
            "if",
            "inline",
            "int",
            "long",
            "register",
            "return",
            "short",
            "signed",
            "sizeof",
            "static",
            "struct",
            "switch",
            "typedef",
            "union",
            "unsigned",
            "void",
            "volatile",
            "while",
            "class",
            "namespace",
            "template",
            "virtual",
            "override",
            "nullptr",
            "bool",
            "true",
            "false",
            "new",
            "delete",
            "throw",
            "try",
            "catch",
            "public",
            "private",
            "protected",
            "using",
            "constexpr",
            "noexcept",
            "static_cast",
            "dynamic_cast",
            "const_cast",
            "reinterpret_cast",
            "string",
            "vector",
            "map",
            "set",
            "shared_ptr",
            "unique_ptr",
            "include",
            "define",
            "ifdef",
            "ifndef",
            "endif",
            "pragma",
        ],
        "Java" => &[
            "abstract",
            "assert",
            "boolean",
            "break",
            "byte",
            "case",
            "catch",
            "char",
            "class",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extends",
            "final",
            "finally",
            "float",
            "for",
            "if",
            "implements",
            "import",
            "instanceof",
            "int",
            "interface",
            "long",
            "native",
            "new",
            "package",
            "private",
            "protected",
            "public",
            "return",
            "short",
            "static",
            "strictfp",
            "super",
            "switch",
            "synchronized",
            "this",
            "throw",
            "throws",
            "transient",
            "try",
            "void",
            "volatile",
            "while",
            "true",
            "false",
            "null",
            "String",
            "Integer",
            "Double",
            "Boolean",
            "List",
            "ArrayList",
            "Map",
            "HashMap",
            "Set",
            "HashSet",
            "Optional",
            "System",
            "Override",
        ],
        "HTML" | "XML" => &[
            "div",
            "span",
            "input",
            "button",
            "form",
            "table",
            "thead",
            "tbody",
            "tfoot",
            "section",
            "header",
            "footer",
            "nav",
            "main",
            "article",
            "aside",
            "class",
            "style",
            "href",
            "src",
            "alt",
            "title",
            "type",
            "value",
            "name",
            "placeholder",
            "onclick",
            "onchange",
            "onsubmit",
            "onload",
            "disabled",
            "checked",
            "readonly",
        ],
        "CSS" | "SCSS" | "Less" => &[
            "display",
            "position",
            "width",
            "height",
            "margin",
            "padding",
            "border",
            "background",
            "color",
            "font",
            "text",
            "flex",
            "grid",
            "align",
            "justify",
            "overflow",
            "opacity",
            "transition",
            "transform",
            "animation",
            "cursor",
            "visibility",
            "none",
            "block",
            "inline",
            "relative",
            "absolute",
            "fixed",
            "sticky",
            "center",
            "inherit",
            "initial",
            "important",
            "hover",
            "active",
            "focus",
            "before",
            "after",
            "first",
            "last",
            "media",
            "keyframes",
            "import",
            "include",
            "mixin",
            "extend",
            "variable",
        ],
        _ => &[],
    }
}

#[path = "vtcomplete_worker.rs"]
mod worker;
#[cfg(test)]
use worker::{VtCompleteSnapshot, compute_vtcomplete_suggestions};
pub(crate) use worker::{VtCompleteWorkOutcome, VtCompleteWorker};

/// Queued typing only needs suggestions for the last still-current caret.
/// Keep identities here, not Rope snapshots: intermediate snapshots force
/// copy-on-write edits and wake workers for text that will never be displayed.
#[derive(Default)]
pub(crate) struct InputBatch {
    depth: usize,
    target: Option<InputTarget>,
    #[cfg(test)]
    pub(crate) submissions: usize,
}

#[derive(PartialEq, Eq)]
struct InputTarget {
    group: usize,
    tab: usize,
    document: u64,
    version: u64,
    cursor: (usize, usize),
}

impl App {
    fn vtcomplete_target(&self) -> Option<InputTarget> {
        let editor = self.editor()?;
        Some(InputTarget { group: self.active_group, tab: self.active_tab,
            document: editor.navigation_id, version: editor.content_version,
            cursor: (editor.cursor_line, editor.cursor_col) })
    }

    pub(crate) fn begin_vtcomplete_batch(&mut self) {
        self.vtcomplete_input_batch.depth += 1;
    }

    pub(crate) fn finish_vtcomplete_batch(&mut self) {
        self.vtcomplete_input_batch.depth = self.vtcomplete_input_batch.depth.saturating_sub(1);
        if self.vtcomplete_input_batch.depth == 0
            && let Some(target) = self.vtcomplete_input_batch.target.take()
            && self.vtcomplete_target() == Some(target)
        {
            self.submit_vtcomplete();
        }
    }

    pub(crate) fn dismiss_vtcomplete(&mut self) {
        self.vtcomplete_input_batch.target = None;
        self.vtcomplete_generation = self.vtcomplete_generation.wrapping_add(1);
        if let Some(token) = self.vtcomplete_cancel_token.take() {
            token.cancel();
        }
        self.vtcomplete_suggestions.clear();
        self.vtcomplete_details.clear();
        self.vtcomplete_selected = 0;
        self.vtcomplete_active = false;
        self.vtcomplete_explicit = false;
        self.vtcomplete_navigated = false;
    }

    pub(crate) fn update_vtcomplete(&mut self) {
        if !self.quick_suggestions && !self.vtcomplete_explicit {
            self.dismiss_vtcomplete();
            return;
        }
        self.refresh_vtcomplete();
    }

    pub(crate) fn update_vtcomplete_explicit(&mut self) {
        self.vtcomplete_explicit = true;
        self.refresh_vtcomplete();
    }

    fn refresh_vtcomplete(&mut self) {
        if self.editor().is_none() {
            self.dismiss_vtcomplete();
            return;
        }
        self.vtcomplete_generation = self.vtcomplete_generation.wrapping_add(1);
        if let Some(token) = self.vtcomplete_cancel_token.take() {
            token.cancel();
        }
        // Keep the selected matching row on refinements. Filtering these ten
        // rows also makes an immediate Tab safe before the worker replies.
        let prefix = self.editor().and_then(worker::popup_prefix).or_else(|| {
            (self.vtcomplete_explicit
                && self
                    .editor()
                    .is_some_and(|editor| !worker::has_vtcomplete_prefix(editor)))
            .then(String::new)
        });
        let selected = self
            .vtcomplete_navigated
            .then(|| {
                self.vtcomplete_suggestions
                    .get(self.vtcomplete_selected)
                    .cloned()
            })
            .flatten();
        if let Some(prefix) = prefix {
            self.vtcomplete_suggestions
                .retain(|word| worker::quick_matches(word, &prefix));
            if !self.vtcomplete_navigated
                && let Some(context) = self
                    .vtcomplete_details
                    .values()
                    .next()
                    .map(|detail| detail.context)
                && context.domain == crate::completion_context::Domain::Code
                && !context.member()
                && !context.declaration
                && self.editor().is_some_and(|editor| {
                    if crate::editor_snippets::get_snippet(&prefix, editor.language_name())
                        .is_none()
                    {
                        return false;
                    }
                    let mut current = crate::completion_context::Analyzer::default();
                    current.feed(&worker::editor_context(editor), |_| {});
                    current.flush(|_| {});
                    !current.context().member() && !current.context().declaration
                })
            {
                self.vtcomplete_suggestions.retain(|word| word != &prefix);
                self.vtcomplete_suggestions.insert(0, prefix.clone());
                self.vtcomplete_suggestions.truncate(10);
                self.vtcomplete_details.insert(
                    prefix.clone(),
                    CompletionDetail {
                        kind: CompletionKind::Snippet,
                        annotation: "snippet",
                        context,
                    },
                );
            }
            let surviving = selected.as_ref().and_then(|selected| {
                self.vtcomplete_suggestions
                    .iter()
                    .position(|word| word == selected)
            });
            self.vtcomplete_selected = surviving.unwrap_or(0);
            if surviving.is_none() {
                self.vtcomplete_navigated = false;
            }
            self.vtcomplete_active = !self.vtcomplete_suggestions.is_empty();
        } else {
            self.dismiss_vtcomplete();
            return;
        }

        if self.vtcomplete_input_batch.depth > 0 {
            self.vtcomplete_input_batch.target = self.vtcomplete_target();
            return;
        }
        self.submit_vtcomplete();
    }

    fn submit_vtcomplete(&mut self) {
        let Some(editor) = self.editor() else {
            return;
        };
        if editor.bg_loading || editor.read_only {
            self.dismiss_vtcomplete();
            return;
        }
        if !editor.extra_cursors.is_empty() || editor.selection_anchor.is_some() {
            self.dismiss_vtcomplete();
            return;
        }
        let settings = self.completion_index.settings();
        if !self.vtcomplete_explicit
            && !worker::automatic_trigger(editor, &settings)
        {
            self.dismiss_vtcomplete();
            return;
        }
        let language = editor.language_name().to_string();
        let content_version = editor.content_version;
        let active_group = self.active_group;
        let active_tab = self.active_tab;
        let generation = self.vtcomplete_generation;
        // The shared project index supplies other files. Typing never clones
        // or traverses every open tab, even in a window with thousands of tabs.
        let active = worker::VtCompleteSnapshot {
            source: crate::completion_source::Source::new(editor.file_path.as_deref(), &language),
            rope: editor.rope.clone(),
            document_id: editor.navigation_id,
            cursor_line: editor.cursor_line,
            cursor_col: editor.cursor_col,
            active: true,
        };
        let edit_baseline = settings.use_history
            .then(|| editor.undo_stack.back().and_then(|state| {
                (state.rope.len_bytes() <= 1024 * 1024
                    && state.rope.len_lines() <= 16_384)
                    .then(|| state.rope.clone())
            }))
            .flatten();
        #[cfg(test)]
        { self.vtcomplete_input_batch.submissions += 1; }
        let token = crate::background::CancellationToken::new();
        self.vtcomplete_worker.submit(
            worker::VtCompleteRequest {
                active,
                edit_baseline,
                index: self.completion_index.clone(),
                explicit: self.vtcomplete_explicit,
                language,
                generation,
                active_group,
                active_tab,
                content_version,
                cancel: token.clone(),
                sender: self.background.sender.clone(),
            },
            self.background.completion_sender(),
        );
        self.vtcomplete_cancel_token = Some(token);
    }

    pub(crate) fn apply_vtcomplete(&mut self) {
        if !self.vtcomplete_active || self.vtcomplete_suggestions.is_empty() {
            return;
        }
        if self.editor().is_none_or(|editor| {
            editor.read_only
                || editor.bg_loading
                || !editor.extra_cursors.is_empty()
                || editor.selection_anchor.is_some()
        }) {
            self.dismiss_vtcomplete();
            return;
        }
        let Some(word) = self.vtcomplete_suggestions
            .get(self.vtcomplete_selected)
            .cloned()
        else {
            self.dismiss_vtcomplete();
            return;
        };
        let snippet = self
            .vtcomplete_details
            .get(&word)
            .is_some_and(|detail| detail.kind == CompletionKind::Snippet);
        let number = self.vtcomplete_details.get(&word).is_some_and(|detail| {
            detail.kind == CompletionKind::Value
                && matches!(detail.annotation, "number" | "number sequence")
        });
        if let Some(editor) = self.editor() {
            let context = self
                .vtcomplete_details
                .get(&word)
                .map(|detail| detail.context)
                .unwrap_or_else(|| {
                    let mut analyzer = crate::completion_context::Analyzer::default();
                    analyzer.feed(&worker::editor_context(editor), |_| {});
                    analyzer.flush(|_| {});
                    let mut context = analyzer.context();
                    let source = crate::completion_source::Source::new(
                        editor.file_path.as_deref(),
                        editor.language_name(),
                    );
                    context.source_family = source.kind.family;
                    context.source_scope = source.file;
                    context
                });
            let prefix = worker::popup_prefix(editor).unwrap_or_default();
            self.completion_index.record_choice(
                word.strip_suffix("()").unwrap_or(&word),
                &prefix,
                context,
                self.vtcomplete_explicit || self.vtcomplete_navigated,
            );
        }
        let previous = self.file_search_edit_snapshot();
        self.pin_preview_tab();
        self.record_edit_position();
        if let Some(editor) = self.editor_mut() {
            if number {
                editor.complete_number(&word);
            } else if !snippet || !editor.complete_snippet(&word) {
                editor.complete_word(&word);
            }
        }
        if let Some(previous) = previous
            && self.refresh_document_feedback_after_edit(&previous)
        {
            if let Some(editor) = self.editor_mut() {
                let start = editor.earliest_edit_line();
                let end = editor.latest_edit_line();
                editor.mark_syntax_dirty_range(start, end);
            }
            self.refresh_search_after_edit(&previous);
        }
        self.dismiss_vtcomplete();
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, Tab};
    use crate::editor::Editor;

    fn vtcomplete_app_with_text(text: &str, cursor_col: usize) -> App {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(text);
        editor.cursor_line = 0;
        editor.cursor_col = cursor_col;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app
    }

    #[test]
    fn accepting_a_snippet_candidate_enters_tab_stops_and_undo_restores_the_prefix() {
        let mut app = vtcomplete_app_with_text("f", 1);
        app.editor_mut().unwrap().file_path = Some(std::path::PathBuf::from("test.rs"));
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = vec!["fn".into()];
        app.vtcomplete_details.insert(
            "fn".into(),
            super::CompletionDetail {
                kind: super::CompletionKind::Snippet,
                annotation: "snippet",
                ..Default::default()
            },
        );
        app.apply_vtcomplete();
        assert!(app.editor().unwrap().rope.to_string().starts_with("fn "));
        assert!(app.editor().unwrap().snippet_active);
        assert!(!app.vtcomplete_active);
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), "f");
    }

    #[test]
    fn accepting_a_numeric_prediction_replaces_the_fraction_and_is_undoable() {
        let mut app = vtcomplete_app_with_text("set(-1.25, value)", 6);
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = vec!["-1.75".into()];
        app.vtcomplete_details.insert("-1.75".into(), super::CompletionDetail {
            kind: super::CompletionKind::Value, annotation: "number sequence",
            ..Default::default()
        });
        app.apply_vtcomplete();
        assert_eq!(app.editor().unwrap().rope.to_string(), "set(-1.75, value)");
        assert!(!app.vtcomplete_active);
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), "set(-1.25, value)");
    }

    #[test]
    fn fast_snippet_refinement_checks_current_receiver_before_using_old_popup_context() {
        for (text, expected) in [("fn", true), ("object.fn", false)] {
            let mut app = vtcomplete_app_with_text(text, text.len());
            app.editor_mut().unwrap().file_path = Some(std::path::PathBuf::from("test.rs"));
            app.vtcomplete_suggestions = vec!["fnSomething".into()];
            app.vtcomplete_details
                .insert("fnSomething".into(), super::CompletionDetail::default());
            app.begin_vtcomplete_batch();
            app.update_vtcomplete();
            assert_eq!(
                app.vtcomplete_suggestions.iter().any(|word| word == "fn"),
                expected
            );
        }
    }

    // --- language_keywords tests ---

    #[test]
    fn vtcomplete_repeated_words_preserve_priority_and_distinct_casing() {
        let mut scores = rustc_hash::FxHashMap::default();
        for (word, proximity, keyword) in [
            ("print_line", 1000, false),
            ("print_line", 2, false),
            ("print_line", 100, true),
            ("Print_Line", 5, false),
        ] {
            super::add_vtcomplete_candidate(&mut scores, word, "pri", "pri", proximity, keyword);
        }
        assert_eq!(scores.len(), 2);
        let repeated = scores.get("print_line").unwrap();
        assert_eq!(repeated.proximity, 2);
        assert!(repeated.keyword);
        assert!(repeated.prefix_match);
        assert_eq!(
            repeated.fuzzy_score,
            crate::app::fuzzy_match_score("print_line", "pri").unwrap()
        );
        let other_case = scores.get("Print_Line").unwrap();
        assert_eq!(other_case.proximity, 5);
        assert!(!other_case.keyword);
    }

    #[test]
    #[ignore = "manual performance measurement; run with --release --ignored --nocapture"]
    fn benchmark_vtcomplete_repeated_words() {
        use std::hint::black_box;
        use std::time::Instant;

        let rope = ropey::Rope::from_str(
            &"print_line print_value print_result unrelated\n".repeat(10_000),
        );
        let cancel = crate::background::CancellationToken::new();
        let mut samples = Vec::new();
        for _ in 0..7 {
            let snapshots = vec![super::VtCompleteSnapshot {
                source: Default::default(),
                rope: rope.clone(),
                document_id: 1,
                cursor_line: 0,
                cursor_col: 0,
                active: true,
            }];
            let started = Instant::now();
            let suggestions = super::compute_vtcomplete_suggestions(
                black_box(snapshots),
                "pri",
                "Plain Text",
                &cancel,
            )
            .unwrap();
            samples.push(started.elapsed());
            assert_eq!(suggestions.len(), 3);
            black_box(suggestions);
        }
        samples.sort();
        eprintln!("vtcomplete lines=10000 median={:?}", samples[3]);
    }

    #[test]
    fn language_keywords_rust() {
        let kw = super::language_keywords("Rust");
        assert!(kw.contains(&"fn"));
        assert!(kw.contains(&"let"));
        assert!(kw.contains(&"struct"));
    }

    #[test]
    fn language_keywords_python() {
        let kw = super::language_keywords("Python");
        assert!(kw.contains(&"def"));
        assert!(kw.contains(&"class"));
    }

    #[test]
    fn language_keywords_unknown() {
        let kw = super::language_keywords("BrainFuck");
        assert!(kw.is_empty());
    }

    #[test]
    fn vtcomplete_includes_fuzzy_matches() {
        let mut app = vtcomplete_app_with_text(
            "prl
print_line
",
            3,
        );

        app.update_vtcomplete();
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            app.vtcomplete_active
        });

        assert!(app.vtcomplete_active);
        assert!(
            app.vtcomplete_suggestions
                .iter()
                .any(|suggestion| suggestion == "print_line")
        );
    }

    #[test]
    fn vtcomplete_prefers_prefix_matches_over_fuzzy_matches() {
        let mut app = vtcomplete_app_with_text(
            "pri
pair_right
print_line
",
            3,
        );

        app.update_vtcomplete();
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            app.vtcomplete_active
        });

        assert!(app.vtcomplete_active);
        assert_eq!(
            app.vtcomplete_suggestions.first(),
            Some(&"print_line".to_string())
        );
        assert!(
            app.vtcomplete_suggestions
                .iter()
                .any(|suggestion| suggestion == "pair_right")
        );
    }

    #[test]
    fn explicit_completion_refines_when_automatic_suggestions_are_disabled() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = vtcomplete_app_with_text("pri print_line\n", 3);
        app.quick_suggestions = false;
        crate::input::handle_editor_key(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            app.vtcomplete_active
        });
        assert!(app.vtcomplete_explicit);
        let previous_generation = app.vtcomplete_generation;
        crate::input::handle_editor_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(app.vtcomplete_generation > previous_generation);
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            app.vtcomplete_cancel_token.is_none()
        });
        assert!(app.vtcomplete_active);
        assert!(app.vtcomplete_explicit);
        assert!(
            app.vtcomplete_suggestions
                .iter()
                .any(|word| word == "print_line")
        );
    }

    #[test]
    fn refinements_remove_stale_rows_before_tab_can_accept_them() {
        let mut app = vtcomplete_app_with_text("pri print_line\n", 3);
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = vec!["unrelated".into(), "print_line".into()];
        app.vtcomplete_selected = 0;
        app.vtcomplete_navigated = true;
        app.update_vtcomplete();
        assert_eq!(app.vtcomplete_suggestions, ["print_line"]);
        assert!(!app.vtcomplete_navigated);
        app.apply_vtcomplete();
        assert!(
            app.editor()
                .unwrap()
                .rope
                .to_string()
                .starts_with("print_line ")
        );
    }

    #[test]
    fn vtcomplete_result_requires_exact_editor_source_and_language() {
        for mismatch in 0..6 {
            let mut app = vtcomplete_app_with_text("pri print_line\npri print_other", 3);
            let editor = app.editor().unwrap();
            let result = crate::background::BackgroundResult::VtComplete {
                generation: app.vtcomplete_generation,
                active_group: app.active_group,
                active_tab: app.active_tab,
                document_id: editor.navigation_id,
                cursor_line: editor.cursor_line,
                cursor_col: editor.cursor_col,
                content_version: editor.content_version,
                source_rope: editor.rope.clone(),
                source_identity: crate::completion_source::Source::new(
                    editor.file_path.as_deref(),
                    editor.language_name(),
                ),
                language: editor.language_name().to_owned(),
                work_outcome: super::VtCompleteWorkOutcome::Exact,
                deferred_candidates: 0,
                suggestions: vec!["print_line".into()],
                details: Vec::new(),
            };
            match mismatch {
                0 => {
                    let mut replacement = Editor::test_with_text("pri print_other");
                    replacement.cursor_col = 3;
                    app.tabs[0].editor = replacement;
                }
                1 => app.editor_mut().unwrap().language_override = Some("Rust".into()),
                2 => app.editor_mut().unwrap().rope = ropey::Rope::from_str("pri print_other"),
                3 => app.editor_mut().unwrap().cursor_line = 1,
                5 => app.editor_mut().unwrap().file_path = Some("renamed-file".into()),
                _ => {}
            }
            app.background.sender.send(result).unwrap();
            app.poll_background_results();
            assert_eq!(app.vtcomplete_active, mismatch == 4);
            assert_eq!(!app.vtcomplete_suggestions.is_empty(), mismatch == 4);
        }
    }
}
