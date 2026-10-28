//! VS Code semantic selectors and TextMate scope fallbacks. Resolve styles once
//! per token type/modifier combination, then patch the grammar's existing style.
use crate::lsp::DecodedSemanticToken;
use crate::scope_selector::Selector;
use lsp_types::SemanticTokensLegend;
use ratatui::style::{Color, Modifier, Style};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TokenStyle {
    foreground: Option<Color>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strikethrough: Option<bool>,
}

impl TokenStyle {
    fn parse(value: &Value, background: &str) -> Self {
        let foreground = value
            .as_str()
            .or_else(|| value["foreground"].as_str())
            .and_then(crate::vscode_theme::parse_syntax_color)
            .map(|color| {
                crate::ui_colors::syntax_color(
                    color,
                    crate::app_theme_loader::hex_to_color(background),
                )
            });
        let font = value["fontStyle"].as_str();
        let flag = |name: &str| {
            value[name]
                .as_bool()
                .or_else(|| font.map(|font| font.split_whitespace().any(|part| part == name)))
        };
        Self {
            foreground,
            bold: flag("bold"),
            italic: flag("italic"),
            underline: flag("underline"),
            strikethrough: flag("strikethrough"),
        }
    }

    fn merge(&mut self, scores: &mut [i32; 5], other: Self, score: i32) {
        fn property<T: Copy>(target: &mut Option<T>, rank: &mut i32, value: Option<T>, score: i32) {
            if value.is_some() && score >= *rank {
                *target = value;
                *rank = score;
            }
        }
        property(
            &mut self.foreground,
            &mut scores[0],
            other.foreground,
            score,
        );
        property(&mut self.bold, &mut scores[1], other.bold, score);
        property(&mut self.italic, &mut scores[2], other.italic, score);
        property(&mut self.underline, &mut scores[3], other.underline, score);
        property(
            &mut self.strikethrough,
            &mut scores[4],
            other.strikethrough,
            score,
        );
    }

    fn style(self) -> Option<Style> {
        if self == Self::default() {
            return None;
        }
        let mut style = Style::default();
        if let Some(color) = self.foreground {
            style = style.fg(color);
        }
        for (enabled, modifier) in [
            (self.bold, Modifier::BOLD),
            (self.italic, Modifier::ITALIC),
            (self.underline, Modifier::UNDERLINED),
            (self.strikethrough, Modifier::CROSSED_OUT),
        ] {
            if let Some(enabled) = enabled {
                style = if enabled {
                    style.add_modifier(modifier)
                } else {
                    style.remove_modifier(modifier)
                };
            }
        }
        Some(style)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Rule {
    selector: String,
    style: TokenStyle,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SemanticTheme {
    explicit: Vec<Rule>,
    defaults: Vec<Rule>,
}

impl SemanticTheme {
    pub(crate) fn new(tokens: &[Value], semantic: &Map<String, Value>, background: &str) -> Self {
        let explicit = semantic
            .iter()
            .map(|(selector, value)| Rule {
                selector: selector.clone(),
                style: TokenStyle::parse(value, background),
            })
            .collect();
        let syntax: Vec<_> = tokens
            .iter()
            .filter_map(|rule| {
                let selector = match &rule["scope"] {
                    Value::String(scope) => scope.clone(),
                    Value::Array(scopes) => scopes
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","),
                    _ => return None,
                };
                Some((
                    Selector::new(&selector),
                    TokenStyle::parse(&rule["settings"], background),
                ))
            })
            .collect();
        let mut defaults = Vec::new();
        let mut probe = |selector: &str, scopes: &[&str]| {
            for scope in scopes {
                let stack: Vec<_> = scope.split_whitespace().collect();
                let mut style = TokenStyle::default();
                let mut scores = [-1; 5];
                // TextMate selects foreground and fontStyle independently; later
                // rules win ties. An unstyled probe must not inject editor defaults.
                let mut matches: Vec<_> = syntax
                    .iter()
                    .filter_map(|(matcher, candidate)| {
                        matcher.score(&stack).map(|rank| (rank, candidate))
                    })
                    .collect();
                matches.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                for (rank, (_, candidate)) in matches.into_iter().enumerate() {
                    style.merge(&mut scores, *candidate, rank as i32);
                }
                if style != TokenStyle::default() {
                    defaults.push(Rule {
                        selector: selector.into(),
                        style,
                    });
                    break;
                }
            }
        };
        for (selector, scopes) in DEFAULT_SCOPES {
            probe(selector, scopes);
        }
        for mapping in rust_registry()["semanticTokenScopes"]
            .as_array()
            .into_iter()
            .flatten()
        {
            for (selector, scopes) in mapping["scopes"].as_object().into_iter().flatten() {
                let scopes: Vec<_> = scopes
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                probe(&format!("{selector}:rust"), &scopes);
            }
        }
        Self { explicit, defaults }
    }

    pub(crate) fn style(
        &self,
        token: &DecodedSemanticToken,
        legend: &SemanticTokensLegend,
        language: &str,
    ) -> Option<Style> {
        let name = legend.token_types.get(token.token_type as usize)?.as_str();
        let mut style = TokenStyle::default();
        let mut scores = [-1; 5];
        for rule in &self.explicit {
            if let Some(score) = selector_score(
                &rule.selector,
                name,
                token._token_modifiers,
                legend,
                language,
            ) {
                style.merge(&mut scores, rule.style, score);
            }
        }
        // Explicit semantic properties always outrank TextMate fallback rules.
        for score in &mut scores {
            if *score >= 0 {
                *score = i32::MAX;
            }
        }
        for rule in &self.defaults {
            if let Some(score) = selector_score(
                &rule.selector,
                name,
                token._token_modifiers,
                legend,
                language,
            ) {
                style.merge(&mut scores, rule.style, score);
            }
        }
        style.style()
    }

    #[cfg(test)]
    pub(crate) fn color(&self, selector: &str) -> Option<(u8, u8, u8)> {
        match self
            .explicit
            .iter()
            .rev()
            .find(|r| r.selector == selector)?
            .style
            .foreground?
        {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }
}

#[cfg(test)]
impl FromIterator<(String, (u8, u8, u8))> for SemanticTheme {
    fn from_iter<T: IntoIterator<Item = (String, (u8, u8, u8))>>(values: T) -> Self {
        Self {
            explicit: values
                .into_iter()
                .map(|(selector, (r, g, b))| Rule {
                    selector,
                    style: TokenStyle {
                        foreground: Some(Color::Rgb(r, g, b)),
                        ..Default::default()
                    },
                })
                .collect(),
            defaults: Vec::new(),
        }
    }
}

fn selector_score(
    selector: &str,
    name: &str,
    modifiers: u32,
    legend: &SemanticTokensLegend,
    language: &str,
) -> Option<i32> {
    let (pattern, lang) = selector
        .split_once(':')
        .map_or((selector, None), |(p, l)| (p, Some(l)));
    if lang.is_some_and(|l| !l.eq_ignore_ascii_case(language)) {
        return None;
    }
    let mut parts = pattern.split('.');
    let kind = parts.next()?;
    let mut score = if lang.is_some() { 10 } else { 0 };
    if kind != "*" {
        let mut current = name;
        let mut depth = 0;
        while current != kind {
            current = if current == "member" {
                "method"
            } else if language.eq_ignore_ascii_case("rust") {
                rust_registry()["semanticTokenTypes"]
                    .as_array()?
                    .iter()
                    .find(|value| value["id"] == current)?["superType"]
                    .as_str()?
            } else {
                return None;
            };
            depth += 1;
            if depth >= 100 {
                return None;
            }
        }
        score += 100 - depth;
    }
    for modifier in parts {
        let index = legend
            .token_modifiers
            .iter()
            .position(|m| m.as_str() == modifier)?;
        if index >= 32 || modifiers & (1u32 << index) == 0 {
            return None;
        }
        score += 100;
    }
    Some(score)
}

fn rust_registry() -> &'static Value {
    static RUST: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    RUST.get_or_init(|| {
        serde_json::from_str(include_str!("../grammars/Rust/semantic-tokens.json"))
            .expect("bundled Rust semantic registry")
    })
}

// VS Code tokenClassificationRegistry.ts. Entries are ordered scope probes,
// not replacement colors: each active theme supplies every property.
const DEFAULT_SCOPES: &[(&str, &[&str])] = &[
    ("comment", &["comment"]),
    ("string", &["string"]),
    ("keyword", &["keyword.control"]),
    ("number", &["constant.numeric"]),
    ("regexp", &["constant.regexp"]),
    ("operator", &["keyword.operator"]),
    ("namespace", &["entity.name.namespace"]),
    ("type", &["entity.name.type", "support.type"]),
    ("struct", &["entity.name.type.struct"]),
    ("class", &["entity.name.type.class", "support.class"]),
    ("interface", &["entity.name.type.interface"]),
    ("enum", &["entity.name.type.enum"]),
    ("typeParameter", &["entity.name.type.parameter"]),
    ("function", &["entity.name.function", "support.function"]),
    (
        "method",
        &["entity.name.function.member", "support.function"],
    ),
    ("macro", &["entity.name.function.preprocessor"]),
    (
        "variable",
        &["variable.other.readwrite", "entity.name.variable"],
    ),
    ("parameter", &["variable.parameter"]),
    ("property", &["variable.other.property"]),
    ("enumMember", &["variable.other.enummember"]),
    ("event", &["variable.other.event"]),
    (
        "decorator",
        &["entity.name.decorator", "entity.name.function"],
    ),
    ("variable.readonly", &["variable.other.constant"]),
    ("property.readonly", &["variable.other.constant.property"]),
    ("type.defaultLibrary", &["support.type"]),
    ("class.defaultLibrary", &["support.class"]),
    ("interface.defaultLibrary", &["support.class"]),
    (
        "variable.defaultLibrary",
        &["support.variable", "support.other.variable"],
    ),
    ("variable.defaultLibrary.readonly", &["support.constant"]),
    ("property.defaultLibrary", &["support.variable.property"]),
    (
        "property.defaultLibrary.readonly",
        &["support.constant.property"],
    ),
    ("function.defaultLibrary", &["support.function"]),
    ("member.defaultLibrary", &["support.function"]),
];

#[cfg(test)]
#[path = "semantic_theme_tests.rs"]
mod tests;
