//! VS Code's semantic-token scope probe matcher (MIT, Microsoft Corporation).
//! Port of textMateScopeMatcher.ts and colorThemeData.ts::nameMatcher.
//! Syntax token themes themselves are resolved by the native TextMate engine.
#[derive(Clone, Debug)]
enum Expr {
    Names(Vec<String>),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}
impl Expr {
    fn score(&self, scopes: &[&str]) -> i32 {
        match self {
            Self::Names(names) => {
                if scopes.len() < names.len() {
                    return -1;
                }
                let mut score = -1;
                for name in names {
                    let Some((i, _)) = scopes.iter().enumerate().rev().find(|(_, s)| {
                        **s == name
                            || s.strip_prefix(name.as_str())
                                .is_some_and(|s| s.starts_with('.'))
                    }) else {
                        return -1;
                    };
                    score = ((i + 1) * 0x10000 + name.len()) as i32;
                }
                score
            }
            Self::Not(e) => {
                if e.score(scopes) < 0 {
                    0
                } else {
                    -1
                }
            }
            Self::And(es) => es.iter().map(|e| e.score(scopes)).min().unwrap_or(-1),
            Self::Or(es) => es.iter().map(|e| e.score(scopes)).max().unwrap_or(-1),
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Selector(Vec<Expr>);
impl Selector {
    pub(crate) fn new(input: &str) -> Self {
        static TOKENS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"([LR]:|[\w\.:][\w\.:\-]*|[,|\-()])").expect("scope selector tokens")
        });
        let tokens = TOKENS
            .find_iter(input)
            .map(|m| m.as_str().to_owned())
            .collect();
        let mut parser = Parser { tokens, offset: 0 };
        let mut expressions = Vec::new();
        while parser.peek().is_some() {
            if matches!(parser.peek(), Some("L:" | "R:")) {
                parser.offset += 1;
            }
            if let Some(e) = parser.conjunction() {
                expressions.push(e);
            }
            if parser.peek() != Some(",") {
                break;
            }
            parser.offset += 1;
        }
        Self(expressions)
    }
    pub(crate) fn score(&self, scopes: &[&str]) -> Option<i32> {
        self.0
            .iter()
            .map(|e| e.score(scopes))
            .max()
            .filter(|s| *s >= 0)
    }
}
struct Parser {
    tokens: Vec<String>,
    offset: usize,
}
impl Parser {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.offset).map(String::as_str)
    }
    fn operand(&mut self) -> Option<Expr> {
        match self.peek()? {
            "-" => {
                self.offset += 1;
                Some(Expr::Not(Box::new(self.operand()?)))
            }
            "(" => {
                self.offset += 1;
                let mut alternatives = vec![self.conjunction()?];
                while matches!(self.peek(), Some("|" | ",")) {
                    while matches!(self.peek(), Some("|" | ",")) {
                        self.offset += 1;
                    }
                    if let Some(e) = self.conjunction() {
                        alternatives.push(e);
                    } else {
                        break;
                    }
                }
                if self.peek() == Some(")") {
                    self.offset += 1;
                }
                Some(Expr::Or(alternatives))
            }
            "," | "|" | ")" => None,
            _ => {
                let mut names = Vec::new();
                while self
                    .peek()
                    .is_some_and(|s| !matches!(s, "-" | "(" | ")" | "," | "|"))
                {
                    names.push(self.tokens[self.offset].clone());
                    self.offset += 1;
                }
                Some(Expr::Names(names))
            }
        }
    }
    fn conjunction(&mut self) -> Option<Expr> {
        let mut operands = vec![self.operand()?];
        while let Some(e) = self.operand() {
            operands.push(e);
        }
        Some(Expr::And(operands))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_selectors_handle_paths_exclusions_groups_and_priorities() {
        let stack = [
            "source.rust",
            "meta.function.rust",
            "variable.parameter.rust",
        ];
        assert!(
            Selector::new("source.rust variable.parameter")
                .score(&stack)
                .is_some()
        );
        assert!(
            Selector::new("(source.rust | source.js) variable - comment")
                .score(&stack)
                .is_some()
        );
        assert!(
            Selector::new("variable - (source.rust | comment)")
                .score(&stack)
                .is_none()
        );
        assert_eq!(
            Selector::new("L:variable.parameter").score(&stack),
            Some(3 * 0x10000 + 18)
        );
    }
}
