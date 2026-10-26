//! VS Code's registered defaults, resolved before converting to TUI colors.
//!
//! Defaults are a dependency graph, not independent fallback swatches. Keep
//! floating-point alpha and HSL lightness until every transform has completed.
//! The vendored definitions and their upstream revision live beside this file.
//! Color arithmetic adapted from Microsoft VS Code (MIT); see
//! `THIRD_PARTY_NOTICES.md` for the copyright and permission notice.
use std::sync::LazyLock;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Map, Value};

static DEFAULTS: LazyLock<Map<String, Value>> = LazyLock::new(|| {
    let mut data: Value = serde_json::from_str(include_str!("vscode_color_defaults.json"))
        .expect("vendored VS Code color definitions are valid JSON");
    data["colors"]
        .take()
        .as_object()
        .expect("color definitions are an object")
        .clone()
});

#[derive(Clone, Copy, Debug)]
struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    a: f64,
    // VS Code preserves the originating HSLA when chaining lightness changes.
    hsl: Option<[f64; 3]>,
}

fn rounded(value: f64, scale: f64) -> f64 {
    (value * scale).round() / scale
}

impl Rgba {
    fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self {
            r: r.clamp(0.0, 255.0) as u8,
            g: g.clamp(0.0, 255.0) as u8,
            b: b.clamp(0.0, 255.0) as u8,
            a: rounded(a.clamp(0.0, 1.0), 1000.0),
            hsl: None,
        }
    }

    fn from_hex(value: &str) -> Option<Self> {
        let c = crate::vscode_theme::parse_syntect_color(value)?;
        Some(Self::new(
            c.r.into(),
            c.g.into(),
            c.b.into(),
            f64::from(c.a) / 255.0,
        ))
    }

    fn hex(self) -> String {
        if self.a == 1.0 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                self.r,
                self.g,
                self.b,
                (self.a * 255.0).round() as u8
            )
        }
    }

    fn transparent(self, factor: f64) -> Self {
        Self::new(self.r.into(), self.g.into(), self.b.into(), self.a * factor)
    }

    fn lightness(self, factor: f64) -> Self {
        let [h, s, l] = self.hsl.unwrap_or_else(|| {
            let (r, g, b) = (
                f64::from(self.r) / 255.0,
                f64::from(self.g) / 255.0,
                f64::from(self.b) / 255.0,
            );
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let l = (min + max) / 2.0;
            let chroma = max - min;
            if chroma == 0.0 {
                return [0.0, 0.0, rounded(l, 1000.0)];
            }
            let s = (chroma / if l <= 0.5 { 2.0 * l } else { 2.0 - 2.0 * l }).min(1.0);
            let h = if max == r {
                (g - b) / chroma + if g < b { 6.0 } else { 0.0 }
            } else if max == g {
                (b - r) / chroma + 2.0
            } else {
                (r - g) / chroma + 4.0
            };
            [(h * 60.0).round(), rounded(s, 1000.0), rounded(l, 1000.0)]
        });
        let l = rounded((l * factor).clamp(0.0, 1.0), 1000.0);
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        let channel = |mut t: f64| {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            let value = if s == 0.0 {
                l
            } else if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            };
            (value * 255.0).round()
        };
        let mut color = Self::new(
            channel(h / 360.0 + 1.0 / 3.0),
            channel(h / 360.0),
            channel(h / 360.0 - 1.0 / 3.0),
            self.a,
        );
        color.hsl = Some([h, s, l]);
        color
    }

    fn luminance(self) -> f64 {
        let linear = |channel: u8| {
            let c = f64::from(channel) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        rounded(
            0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b),
            10000.0,
        )
    }

    fn opaque(self, background: Self) -> Self {
        if self.a == 1.0 || background.a != 1.0 {
            return self;
        }
        let channel = |fg: u8, bg: u8| f64::from(bg) - self.a * (f64::from(bg) - f64::from(fg));
        Self::new(
            channel(self.r, background.r),
            channel(self.g, background.g),
            channel(self.b, background.b),
            1.0,
        )
    }

    fn mix(self, other: Self, ratio: f64) -> Self {
        let ratio = ratio.clamp(0.0, 1.0);
        let channel = |a: u8, b: u8| f64::from(a) + (f64::from(b) - f64::from(a)) * ratio;
        Self::new(
            channel(self.r, other.r),
            channel(self.g, other.g),
            channel(self.b, other.b),
            self.a + (other.a - self.a) * ratio,
        )
    }
}

struct Resolver<'a> {
    authored: &'a Map<String, Value>,
    defaults: &'a Map<String, Value>,
    kind: &'static str,
    cache: FxHashMap<String, Option<Rgba>>,
    visiting: FxHashSet<String>,
}

impl<'a> Resolver<'a> {
    fn new(
        authored: &'a Map<String, Value>,
        defaults: &'a Map<String, Value>,
        light: bool,
        high_contrast: bool,
    ) -> Self {
        Self {
            authored,
            defaults,
            kind: match (light, high_contrast) {
                (false, false) => "dark",
                (true, false) => "light",
                (false, true) => "hcDark",
                (true, true) => "hcLight",
            },
            cache: FxHashMap::default(),
            visiting: FxHashSet::default(),
        }
    }

    fn color(&mut self, id: &str) -> Option<Rgba> {
        if let Some(color) = self.cache.get(id) {
            return *color;
        }
        if !self.visiting.insert(id.to_owned()) {
            return None;
        }
        let color = self
            .authored
            .get(id)
            .and_then(Value::as_str)
            .and_then(Rgba::from_hex)
            .or_else(|| self.defaults.get(id).and_then(|value| self.value(value)));
        self.visiting.remove(id);
        self.cache.insert(id.to_owned(), color);
        color
    }

    fn value(&mut self, value: &Value) -> Option<Rgba> {
        match value {
            Value::String(value) => {
                if value.starts_with('#') {
                    Rgba::from_hex(value)
                } else {
                    self.color(value)
                }
            }
            Value::Object(value) => {
                if let Some(rgba) = value.get("rgba").and_then(Value::as_array) {
                    return Some(Rgba::new(
                        rgba.first()?.as_f64()?,
                        rgba.get(1)?.as_f64()?,
                        rgba.get(2)?.as_f64()?,
                        rgba.get(3)?.as_f64()?,
                    ));
                }
                if let Some(value) = value.get(self.kind) {
                    return self.value(value);
                }
                let factor = value.get("factor").and_then(Value::as_f64).unwrap_or(0.0);
                let field = |key| value.get(key).unwrap_or(&Value::Null);
                match value.get("op").and_then(Value::as_str)? {
                    "Transparent" => Some(self.value(field("value"))?.transparent(factor)),
                    "Lighten" => Some(self.value(field("value"))?.lightness(1.0 + factor)),
                    "Darken" => Some(self.value(field("value"))?.lightness(1.0 - factor)),
                    "Opaque" => {
                        let color = self.value(field("value"))?;
                        Some(
                            self.value(field("background"))
                                .map_or(color, |bg| color.opaque(bg)),
                        )
                    }
                    "Mix" => {
                        let clear = Rgba::new(0.0, 0.0, 0.0, 0.0);
                        Some(self.value(field("color")).unwrap_or(clear).mix(
                            self.value(field("with")).unwrap_or(clear),
                            field("ratio").as_f64()?,
                        ))
                    }
                    "OneOf" => field("values")
                        .as_array()?
                        .iter()
                        .find_map(|candidate| self.value(candidate)),
                    "IfDefinedThenElse" => {
                        // Registered defaults do not count as authored colors.
                        let defined = self
                            .authored
                            .get(field("if").as_str()?)
                            .and_then(Value::as_str)
                            .and_then(Rgba::from_hex)
                            .is_some();
                        self.value(field(if defined { "then" } else { "else" }))
                    }
                    "LessProminent" => {
                        let color = self.value(field("value"))?;
                        let transparency = field("transparency").as_f64()?;
                        let Some(bg) = self.value(field("background")) else {
                            return Some(color.transparent(factor * transparency));
                        };
                        let (from, to) = (color.luminance(), bg.luminance());
                        let factor = if factor == 0.0 { 0.5 } else { factor };
                        let factor = if from < to {
                            1.0 + factor * (to - from) / to
                        } else {
                            1.0 - factor * (from - to) / from
                        };
                        Some(color.lightness(factor).transparent(transparency))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// Keep authored values separate so settings removal and `IfDefinedThenElse`
/// can distinguish a user color from a registered default. Null entries stay
/// null: they intentionally request no fill/outline, not an arbitrary swatch.
pub(crate) fn resolve(
    authored: &Map<String, Value>,
    light: bool,
    high_contrast: bool,
) -> Map<String, Value> {
    let mut resolver = Resolver::new(authored, &DEFAULTS, light, high_contrast);
    let mut result = authored.clone();
    for id in DEFAULTS.keys() {
        result.insert(
            id.clone(),
            resolver
                .color(id)
                .map_or(Value::Null, |color| color.hex().into()),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_upstream_color_engine_for_every_registered_role() {
        // Generated by executing the unmodified upstream Color and transform
        // implementations, independently of this Rust resolver.
        let fixtures: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/vscode-color-reference.json"
        ))
        .unwrap();
        for case in fixtures["cases"].as_array().unwrap() {
            let kind = case["kind"].as_str().unwrap();
            let authored = case["authored"].as_object().unwrap();
            let mut resolver = Resolver::new(
                authored,
                &DEFAULTS,
                matches!(kind, "light" | "hcLight"),
                kind.starts_with("hc"),
            );
            let expected = case["expected"].as_object().unwrap();
            assert_eq!(expected.len(), DEFAULTS.len());
            for (id, expected) in expected {
                let actual = resolver
                    .color(id)
                    .map(|c| [f64::from(c.r), f64::from(c.g), f64::from(c.b), c.a]);
                let expected = expected
                    .as_array()
                    .map(|v| std::array::from_fn(|i| v[i].as_f64().unwrap()));
                assert_eq!(actual, expected, "{kind}: {id}; overrides: {authored:?}");
            }
        }
    }

    #[test]
    fn invalid_authored_colors_and_cyclic_references_are_safe() {
        let authored = serde_json::json!({"foreground": "broken", "focusBorder": null})
            .as_object()
            .unwrap()
            .clone();
        let defaults = serde_json::json!({"foreground":"#123456", "a":"b", "b":"a"})
            .as_object()
            .unwrap()
            .clone();
        let mut resolver = Resolver::new(&authored, &defaults, false, false);
        assert_eq!(resolver.color("foreground").unwrap().hex(), "#123456");
        assert!(resolver.color("a").is_none());
        assert!(resolver.color("b").is_none());
        assert!(resolver.color("unregistered").is_none());
    }
}
