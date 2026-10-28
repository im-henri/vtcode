//! Stateful native TextMate tokenization. A checkpoint owns its session: scope
//! IDs must never be interpreted by a different session after a worker resumes.
use crate::textmate::{Style, Syntax, Theme};
use rustc_hash::FxHashMap;
use std::sync::{Arc, Mutex};

struct Shared {
    session: irosashi::Session,
    theme: Theme,
    styles: FxHashMap<irosashi::ScopeListId, (Style, bool)>,
    initial: irosashi::StateStack,
}

#[derive(Clone)]
pub(crate) struct State {
    shared: Arc<Mutex<Shared>>,
    stack: irosashi::StateStack,
    first: bool,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextMateState")
            .field("depth", &self.stack.depth())
            .field("first", &self.first)
            .finish()
    }
}
impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
            && self.first == other.first
            && self.stack == other.stack
    }
}
impl Eq for State {}

impl State {
    pub(crate) fn new(syntax: &Syntax, theme: &Theme) -> Self {
        let session = syntax.engine.session(&syntax.id).unwrap_or_else(|_| {
            syntax
                .engine
                .session("vtcode-plain-text")
                .expect("validated Plain Text grammar")
        });
        let stack = session.initial_state();
        Self {
            shared: Arc::new(Mutex::new(Shared {
                session,
                theme: theme.clone(),
                initial: stack.clone(),
                styles: FxHashMap::default(),
            })),
            stack,
            first: true,
        }
    }

    pub(crate) fn initial(&self) -> Self {
        let stack = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .initial
            .clone();
        Self {
            shared: Arc::clone(&self.shared),
            stack,
            first: true,
        }
    }

    pub(crate) fn line<'t>(
        &mut self,
        text: &'t str,
        retain: bool,
        comments: &mut Vec<(usize, usize)>,
    ) -> Vec<(Style, &'t str)> {
        let bare = text.trim_end_matches(['\n', '\r']);
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let parsed = shared.session.tokenize_line(
            bare,
            &self.stack,
            self.first,
            irosashi::TokenizeOptions {
                max_line_length: Some(crate::highlight::MAX_SYNTAX_LINE_CHARS * 4),
            },
        );
        self.stack = parsed.state;
        self.first = false;
        if !retain {
            return Vec::new();
        }
        let mut spans = Vec::with_capacity(parsed.tokens.len());
        let mut column = 0;
        for (index, token) in parsed.tokens.iter().enumerate() {
            let (style, comment) = if let Some(style) = shared.styles.get(&token.scopes) {
                *style
            } else {
                let scopes = shared.session.scope_names(token.scopes);
                let style = shared.theme.matcher.style(
                    &scopes,
                    Style {
                        foreground: shared.theme.foreground,
                        background: shared.theme.background,
                        ..Style::default()
                    },
                );
                let comment = scopes
                    .iter()
                    .any(|scope| *scope == "comment" || scope.starts_with("comment."));
                shared.styles.insert(token.scopes, (style, comment));
                (style, comment)
            };
            let end = if index + 1 == parsed.tokens.len() {
                text.len()
            } else {
                token.end.min(bare.len())
            };
            let start = token.start.min(end);
            let part = &text[start..end];
            let next = column + part.chars().count();
            if comment && next > column {
                if let Some((_, previous)) = comments.last_mut().filter(|(_, end)| *end == column) {
                    *previous = next;
                } else {
                    comments.push((column, next));
                }
            }
            column = next;
            spans.push((style, part));
        }
        if spans.is_empty() {
            spans.push((
                Style {
                    foreground: shared.theme.foreground,
                    background: shared.theme.background,
                    ..Style::default()
                },
                text,
            ));
        }
        spans
    }
}

/// The same incremental engine is used by Markdown fences and command previews.
pub(crate) struct HighlightLines {
    state: State,
}
impl HighlightLines {
    pub(crate) fn new(syntax: &Syntax, theme: &Theme) -> Self {
        Self {
            state: State::new(syntax, theme),
        }
    }
    pub(crate) fn highlight_line<'a>(&mut self, text: &'a str) -> Vec<(Style, &'a str)> {
        self.state.line(text, true, &mut Vec::new())
    }
}
