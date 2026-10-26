//! Text equivalents for common HTML and TeX. These never execute markup or
//! fetch resources; unknown TeX commands retain their spelling.
use super::Palette;
use ratatui::style::{Modifier, Style};

pub(super) fn image_destination(destination: &str) -> String {
    if destination.starts_with("data:") {
        return "embedded image".into();
    }
    let mut chars = destination.chars();
    let short: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

pub(super) fn script(text: &str, superscript: bool) -> String {
    let from = "0123456789+-=()in";
    let to = if superscript {
        "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁱⁿ"
    } else {
        "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ᵢₙ"
    };
    let converted: Option<String> = text
        .chars()
        .map(|c| {
            from.chars()
                .position(|v| v == c)
                .and_then(|i| to.chars().nth(i))
        })
        .collect();
    converted.unwrap_or_else(|| format!("{}({text})", if superscript { '^' } else { '_' }))
}

pub(super) fn math(source: &str) -> String {
    Math { source, offset: 0 }.expression(0, false)
}

struct Math<'a> {
    source: &'a str,
    offset: usize,
}
impl Math<'_> {
    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }
    fn take(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.offset += c.len_utf8();
        Some(c)
    }
    fn argument(&mut self, depth: usize) -> String {
        while self.peek().is_some_and(char::is_whitespace) {
            self.take();
        }
        if self.peek() == Some('{') {
            self.take();
            self.expression(depth + 1, true)
        } else {
            self.take().map(|c| c.to_string()).unwrap_or_default()
        }
    }
    fn expression(&mut self, depth: usize, group: bool) -> String {
        if depth >= 32 {
            let rest = self.source[self.offset..].to_owned();
            self.offset = self.source.len();
            return rest;
        }
        let mut result = String::new();
        while let Some(ch) = self.take() {
            match ch {
                '}' if group => break,
                '{' => {
                    result.push('{');
                    result.push_str(&self.expression(depth + 1, true));
                    result.push('}');
                }
                '^' | '_' => result.push_str(&script(&self.argument(depth), ch == '^')),
                '\\' => {
                    let start = self.offset;
                    while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                        self.take();
                    }
                    let command = &self.source[start..self.offset];
                    match command {
                        "frac" | "dfrac" | "tfrac" => {
                            let a = self.argument(depth);
                            let b = self.argument(depth);
                            result.push_str(&format!("({a})/({b})"));
                        }
                        "sqrt" => result.push_str(&format!("√({})", self.argument(depth))),
                        "text" | "mathrm" | "mathbf" | "mathit" | "operatorname" => {
                            result.push_str(&self.argument(depth))
                        }
                        "left" | "right" => {}
                        "begin" | "end" => {
                            let env = self.argument(depth);
                            match env.as_str() {
                                "aligned" | "align" | "gathered" | "matrix" => {}
                                "bmatrix" => {
                                    result.push(if command == "begin" { '[' } else { ']' })
                                }
                                "pmatrix" => {
                                    result.push(if command == "begin" { '(' } else { ')' })
                                }
                                _ => result.push_str(&format!("\\{command}{{{env}}}")),
                            }
                        }
                        "" => match self.take() {
                            Some('\\') => result.push('\n'),
                            Some(',' | ';' | ':' | ' ') => result.push(' '),
                            Some('!') => {}
                            Some(c) => result.push(c),
                            None => result.push('\\'),
                        },
                        _ => {
                            let symbol = match command {
                                "alpha" => "α",
                                "beta" => "β",
                                "gamma" => "γ",
                                "delta" => "δ",
                                "epsilon" => "ε",
                                "theta" => "θ",
                                "lambda" => "λ",
                                "mu" => "μ",
                                "pi" => "π",
                                "rho" => "ρ",
                                "sigma" => "σ",
                                "tau" => "τ",
                                "phi" => "φ",
                                "omega" => "ω",
                                "Gamma" => "Γ",
                                "Delta" => "Δ",
                                "Theta" => "Θ",
                                "Sigma" => "Σ",
                                "Omega" => "Ω",
                                "sum" => "∑",
                                "prod" => "∏",
                                "int" => "∫",
                                "infty" => "∞",
                                "partial" => "∂",
                                "nabla" => "∇",
                                "times" => "×",
                                "cdot" => "·",
                                "div" => "÷",
                                "pm" => "±",
                                "le" | "leq" => "≤",
                                "ge" | "geq" => "≥",
                                "ne" | "neq" => "≠",
                                "approx" => "≈",
                                "in" => "∈",
                                "notin" => "∉",
                                "subseteq" => "⊆",
                                "forall" => "∀",
                                "exists" => "∃",
                                "to" | "rightarrow" => "→",
                                "leftarrow" => "←",
                                "Rightarrow" => "⇒",
                                "Leftrightarrow" => "⇔",
                                "quad" => "  ",
                                "qquad" => "    ",
                                _ => "",
                            };
                            if symbol.is_empty() {
                                result.push('\\');
                                result.push_str(command);
                            } else {
                                result.push_str(symbol);
                            }
                        }
                    }
                }
                '&' => result.push(' '),
                _ => result.push(ch),
            }
        }
        result
    }
}

#[derive(Default)]
pub(super) struct Html {
    styles: Vec<(String, Style)>,
    hidden: Option<String>,
    pre: bool,
    comment: bool,
    script: Option<bool>,
}
impl Html {
    pub(super) fn visible(&self) -> bool {
        self.hidden.is_none()
    }
    pub(super) fn literal(&self) -> bool {
        self.pre
            || self
                .styles
                .iter()
                .any(|(tag, _)| matches!(tag.as_str(), "code" | "kbd" | "samp"))
    }
    pub(super) fn text<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.script
            .map_or_else(|| text.into(), |sup| script(text, sup).into())
    }
    pub(super) fn style(&self, base: Style) -> Style {
        self.styles
            .iter()
            .fold(base, |style, (_, patch)| style.patch(*patch))
    }
    pub(super) fn feed(
        &mut self,
        input: &str,
        base: Style,
        palette: Palette,
    ) -> Vec<(String, Style)> {
        let mut out = Vec::new();
        let mut offset = 0;
        while offset < input.len() {
            let tail = &input[offset..];
            if self.comment {
                if let Some(end) = tail.find("-->") {
                    offset += end + 3;
                    self.comment = false;
                    continue;
                }
                break;
            }
            if tail.starts_with("<!--") {
                self.comment = true;
                offset += 4;
                continue;
            }
            if !tail.starts_with('<') {
                let end = tail.find('<').unwrap_or(tail.len());
                if self.visible() {
                    let text = entities(&tail[..end]);
                    let text = if self.pre {
                        super::expand_code_tabs(&text)
                    } else {
                        let mut whitespace = false;
                        text.chars()
                            .filter_map(|c| {
                                if c.is_whitespace() {
                                    let was = whitespace;
                                    whitespace = true;
                                    (!was).then_some(' ')
                                } else {
                                    whitespace = false;
                                    Some(c)
                                }
                            })
                            .collect()
                    };
                    out.push((self.text(&text).into_owned(), self.style(base)));
                }
                offset += end;
                continue;
            }
            // Attribute values may contain '>'; scan quotes before ending a tag.
            let mut quote = None;
            let end = tail.char_indices().skip(1).find_map(|(i, c)| {
                if quote == Some(c) {
                    quote = None;
                } else if quote.is_none() && matches!(c, '\'' | '"') {
                    quote = Some(c);
                } else if quote.is_none() && c == '>' {
                    return Some(i);
                }
                None
            });
            let Some(end) = end else {
                if self.visible() {
                    out.push((tail.into(), self.style(base)));
                }
                break;
            };
            let raw = tail[1..end].trim();
            let closing = raw.starts_with('/');
            let raw = raw.trim_start_matches('/');
            let n = raw
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(raw.len());
            let name = raw[..n].to_ascii_lowercase();
            offset += end + 1;
            if let Some(hidden) = &self.hidden {
                if closing && *hidden == name {
                    self.hidden = None;
                }
                continue;
            }
            if matches!(name.as_str(), "script" | "style" | "head" | "iframe") && !closing {
                self.hidden = Some(name);
                continue;
            }
            if closing {
                if let Some(i) = self.styles.iter().rposition(|(tag, _)| *tag == name) {
                    self.styles.truncate(i);
                }
                if name == "pre" {
                    self.pre = false;
                }
                if matches!(name.as_str(), "sup" | "sub") {
                    self.script = None;
                }
            }
            let style = self.style(base);
            let newline = matches!(
                name.as_str(),
                "p" | "div"
                    | "blockquote"
                    | "details"
                    | "summary"
                    | "figure"
                    | "figcaption"
                    | "table"
                    | "tr"
                    | "pre"
                    | "ul"
                    | "ol"
                    | "li"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
            );
            if newline {
                out.push(("\n".into(), style));
            }
            if name == "br" {
                out.push(("\n".into(), style));
            }
            if name == "hr" {
                out.push(("\n────────\n".into(), style.fg(palette.dim)));
            }
            if name == "img" && !closing {
                let alt = attribute(&raw[n..], "alt").unwrap_or_else(|| "image".into());
                let src = attribute(&raw[n..], "src").unwrap_or_default();
                let caption = if src.is_empty() {
                    format!("[Image: {alt}]")
                } else {
                    format!("[Image: {alt}] ({})", image_destination(&src))
                };
                out.push((caption, style.fg(palette.dim)));
            }
            if matches!(name.as_str(), "td" | "th") && closing {
                out.push((" │ ".into(), style.fg(palette.dim)));
            }
            if closing {
                continue;
            }
            if matches!(name.as_str(), "sup" | "sub") {
                self.script = Some(name == "sup");
            }
            if name == "pre" {
                self.pre = true;
            }
            if name == "li" {
                out.push(("• ".into(), style));
            }
            if name == "summary" {
                out.push(("▾ ".into(), style));
            }
            let patch = match name.as_str() {
                "b" | "strong" | "summary" | "th" => {
                    Some(Style::default().add_modifier(Modifier::BOLD))
                }
                "i" | "em" => Some(Style::default().add_modifier(Modifier::ITALIC)),
                "s" | "del" | "strike" => {
                    Some(Style::default().add_modifier(Modifier::CROSSED_OUT))
                }
                "u" | "ins" => Some(Style::default().add_modifier(Modifier::UNDERLINED)),
                "a" => Some(
                    Style::default()
                        .fg(palette.link)
                        .add_modifier(Modifier::UNDERLINED),
                ),
                "code" | "kbd" | "samp" => Some(
                    Style::default()
                        .fg(palette.inline_code)
                        .bg(palette.inline_bg),
                ),
                "mark" => Some(
                    Style::default()
                        .fg(palette.inline_code)
                        .bg(palette.inline_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(
                    Style::default()
                        .fg(palette.heading)
                        .add_modifier(Modifier::BOLD),
                ),
                _ => None,
            };
            if let Some(patch) = patch
                && self.styles.len() < 64
            {
                self.styles.push((name, patch));
            }
        }
        out
    }
}

fn attribute(mut text: &str, wanted: &str) -> Option<String> {
    while !text.trim_start().is_empty() {
        text = text.trim_start();
        let end = text
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(text.len());
        let name = &text[..end];
        text = text[end..].trim_start();
        let Some(rest) = text.strip_prefix('=') else {
            if end == 0 {
                break;
            }
            continue;
        };
        text = rest.trim_start();
        let (value, rest) = if text.starts_with(['\'', '"']) {
            let quote = text.as_bytes()[0] as char;
            let end = text[1..].find(quote)? + 1;
            (&text[1..end], &text[end + 1..])
        } else {
            let end = text.find(char::is_whitespace).unwrap_or(text.len());
            (&text[..end], &text[end..])
        };
        if name.eq_ignore_ascii_case(wanted) {
            return Some(entities(value));
        }
        text = rest;
    }
    None
}

fn entities(source: &str) -> String {
    let mut result = String::new();
    let mut rest = source;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let end = rest.bytes().take(32).position(|b| b == b';');
        if let Some(end) = end {
            let name = &rest[..end];
            let number = name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|s| u32::from_str_radix(s, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|s| s.parse::<u32>().ok()))
                .and_then(char::from_u32);
            if let Some(c) = number {
                result.push(c);
                rest = &rest[end + 1..];
                continue;
            }
            let replacement = match name {
                "amp" => Some("&"),
                "lt" => Some("<"),
                "gt" => Some(">"),
                "quot" => Some("\""),
                "apos" => Some("'"),
                "nbsp" => Some("\u{a0}"),
                "copy" => Some("©"),
                "reg" => Some("®"),
                "trade" => Some("™"),
                _ => None,
            };
            if let Some(value) = replacement {
                result.push_str(value);
                rest = &rest[end + 1..];
                continue;
            }
        }
        result.push('&');
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn math_keeps_unknown_commands_and_formats_common_notation() {
        assert_eq!(math(r"E=mc^2"), "E=mc²");
        assert_eq!(math(r"\frac{a_1}{\sqrt{\pi}}"), "(a₁)/(√(π))");
        assert_eq!(math(r"\unknown{x}"), r"\unknown{x}");
        assert_eq!(
            math(r"\begin{aligned}a &= b \\ c &= d\end{aligned}"),
            "a  = b \n c  = d"
        );
    }
    #[test]
    fn html_handles_quoted_brackets_entities_and_hides_scripts() {
        let mut html = Html::default();
        let palette = Palette::from(&crate::app_theme::EditorTheme::Dark.colors());
        let text: String = html.feed("<p><strong title='a > b'>Bold</strong> &amp; &#x1F680;<br><img alt='A &amp; B' src='data:abc'> <!--hidden--><script>bad()</script>end</p>", Style::default(), palette).into_iter().map(|(s, _)| s).collect();
        assert!(
            text.contains("Bold & 🚀\n[Image: A & B] (embedded image)"),
            "{text}"
        );
        assert!(!text.contains("hidden") && !text.contains("bad()"));
        assert!(text.contains("end"));
    }
}
