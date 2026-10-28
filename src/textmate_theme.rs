//! VS Code TextMate theme trie (MIT, Microsoft Corporation).
//! Scope inheritance, parent ranking and source-order overrides follow
//! microsoft/vscode-textmate 9.3.2, src/theme.ts. Tokenization is independent.
use crate::textmate::{Color, FontStyle, Style};
use serde_json::Value;
use std::{cmp::Ordering, collections::BTreeMap};

#[derive(Clone, Copy, Debug, Default)]
struct Properties {
    foreground: Option<Color>,
    background: Option<Color>,
    font: Option<FontStyle>,
}
impl Properties {
    fn patch(self, other: Self) -> Self {
        Self {
            foreground: other.foreground.or(self.foreground),
            background: other.background.or(self.background),
            font: other.font.or(self.font),
        }
    }
    fn apply(self, style: &mut Style) {
        if let Some(color) = self.foreground {
            style.foreground = color;
        }
        if let Some(color) = self.background {
            style.background = color;
        }
        if let Some(font) = self.font {
            style.font_style = font;
        }
    }
}
#[derive(Clone, Debug, Default)]
struct Rule {
    depth: usize,
    parents: Vec<String>,
    properties: Properties,
}
impl Rule {
    fn specificity(&self, other: &Self) -> Ordering {
        self.depth
            .cmp(&other.depth)
            .then_with(|| {
                self.parents
                    .iter()
                    .filter(|s| s.as_str() != ">")
                    .map(String::len)
                    .cmp(
                        other
                            .parents
                            .iter()
                            .filter(|s| s.as_str() != ">")
                            .map(String::len),
                    )
            })
            .then_with(|| self.parents.len().cmp(&other.parents.len()))
    }
    fn matches(&self, stack: &[&str]) -> bool {
        let mut scopes = stack.iter().rev();
        let mut parents = self.parents.iter();
        while let Some(parent) = parents.next() {
            let immediate = parent == ">";
            let parent = if immediate {
                let Some(parent) = parents.next() else {
                    return false;
                };
                parent
            } else {
                parent
            };
            let matches = |scope: &&&str| {
                **scope == parent
                    || scope
                        .strip_prefix(parent.as_str())
                        .is_some_and(|tail| tail.starts_with('.'))
            };
            if immediate {
                if !scopes.next().as_ref().is_some_and(matches) {
                    return false;
                }
            } else if !scopes.any(|scope| matches(&scope)) {
                return false;
            }
        }
        true
    }
}
#[derive(Clone, Debug, Default)]
struct Node {
    main: Rule,
    conditional: Vec<Rule>,
    rules: Vec<Rule>,
    children: BTreeMap<String, Node>,
}
impl Node {
    fn insert(&mut self, scope: &str, rule: Rule) {
        if !scope.is_empty() {
            let (head, rest) = scope.split_once('.').unwrap_or((scope, ""));
            if !self.children.contains_key(head) {
                self.children.insert(
                    head.into(),
                    Self {
                        main: self.main.clone(),
                        conditional: self.conditional.clone(),
                        ..Self::default()
                    },
                );
            }
            self.children.get_mut(head).unwrap().insert(rest, rule);
        } else if rule.parents.is_empty() {
            self.main.depth = rule.depth;
            self.main.properties = self.main.properties.patch(rule.properties);
        } else if let Some(previous) = self
            .conditional
            .iter_mut()
            .find(|r| r.parents == rule.parents)
        {
            previous.depth = rule.depth;
            previous.properties = previous.properties.patch(rule.properties);
        } else {
            self.conditional.push(Rule {
                properties: self.main.properties.patch(rule.properties),
                ..rule
            });
        }
    }
    fn finish(&mut self) {
        self.rules = self.conditional.clone();
        self.rules.push(self.main.clone());
        self.rules.sort_by(|a, b| b.specificity(a));
        for child in self.children.values_mut() {
            child.finish();
        }
    }
    fn matching(&self, scope: &str, parents: &[&str]) -> Properties {
        let mut node = self;
        for segment in scope.split('.') {
            let Some(child) = node.children.get(segment) else {
                break;
            };
            node = child;
        }
        node.rules
            .iter()
            .find(|rule| rule.matches(parents))
            .map_or(Properties::default(), |rule| rule.properties)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Matcher {
    root: Node,
}
impl Matcher {
    pub(crate) fn new(rules: &[Value]) -> Self {
        let mut parsed = Vec::new();
        for (index, rule) in rules.iter().enumerate() {
            let scopes: Vec<_> = match &rule["scope"] {
                Value::String(s) => s.trim_matches(',').split(',').collect(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => continue,
            };
            let settings = &rule["settings"];
            let properties = Properties {
                foreground: settings["foreground"].as_str().and_then(Color::parse),
                background: settings["background"].as_str().and_then(Color::parse),
                font: settings["fontStyle"].as_str().map(|font| {
                    let mut flags = FontStyle::empty();
                    for part in font.split(' ') {
                        match part {
                            "bold" => flags |= FontStyle::BOLD,
                            "italic" => flags |= FontStyle::ITALIC,
                            "underline" => flags |= FontStyle::UNDERLINE,
                            "strikethrough" => flags |= FontStyle::STRIKETHROUGH,
                            _ => (),
                        }
                    }
                    flags
                }),
            };
            for scope in scopes {
                let mut parts: Vec<_> = scope.trim().split(' ').collect();
                let target = parts.pop().unwrap_or_default();
                if target.is_empty() {
                    continue;
                }
                let parents: Vec<_> = parts.into_iter().rev().map(str::to_owned).collect();
                parsed.push((target.to_owned(), parents, index, properties));
            }
        }
        parsed.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        let mut root = Node::default();
        for (scope, parents, _, properties) in parsed {
            let depth = scope.split('.').count();
            root.insert(
                &scope,
                Rule {
                    depth,
                    parents,
                    properties,
                },
            );
        }
        root.finish();
        Self { root }
    }
    pub(crate) fn style(&self, scopes: &[&str], mut defaults: Style) -> Style {
        for (index, scope) in scopes.iter().enumerate() {
            self.root
                .matching(scope, &scopes[..index])
                .apply(&mut defaults);
        }
        defaults
    }
}
