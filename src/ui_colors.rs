use ratatui::style::{Color, Style};
use rustc_hash::FxHashMap as CssColorMap;
use std::sync::LazyLock;

use crate::ui::hsl_to_rgb;

/// Keep document change markers consistent in the gutter and overviews.
pub(crate) fn change_marker_color(
    status: char,
    colors: &crate::app_theme::ThemeColors,
) -> Option<Color> {
    match status {
        'N' => Some(colors.ui.fg_gutter_added),
        'U' | 'A' | 'M' => Some(colors.ui.fg_gutter_modified),
        'D' => Some(colors.ui.fg_gutter_deleted),
        'S' => Some(colors.fg_success),
        _ => None,
    }
}

pub(crate) fn minimap_marker_color(
    status: char,
    colors: &crate::app_theme::ThemeColors,
) -> Option<Color> {
    match status {
        'N' => Some(colors.ui.fg_minimap_added),
        'U' | 'A' | 'M' => Some(colors.ui.fg_minimap_modified),
        'D' => Some(colors.ui.fg_minimap_deleted),
        'S' => Some(colors.fg_success),
        _ => None,
    }
}

pub(crate) fn overview_marker_color(
    status: char,
    colors: &crate::app_theme::ThemeColors,
) -> Option<Color> {
    match status {
        'N' => Some(colors.ui.fg_overview_added),
        'U' | 'A' | 'M' => Some(colors.ui.fg_overview_modified),
        'D' => Some(colors.ui.fg_overview_deleted),
        'S' => Some(colors.fg_success),
        _ => None,
    }
}

/// Add `amount` to every RGB channel (clamped at 255). Non-RGB variants pass through.
pub(crate) fn lighten(color: Color, amount: u8) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(
            r.saturating_add(amount),
            g.saturating_add(amount),
            b.saturating_add(amount),
        ),
        other => other,
    }
}

/// TextMate token colors can also carry alpha. Ignoring it makes translucent
/// comments and embedded-language colors unexpectedly bright or saturated.
pub(crate) fn syntect_color(color: syntect::highlighting::Color, surface: Color) -> Color {
    crate::theme_workbench::ColorLayer::rgba(color.r, color.g, color.b, color.a)
        .over(surface)
        .unwrap_or(Color::Rgb(color.r, color.g, color.b))
}

/// The input is an eight-bit channel, so decoding needs only 256 exact values.
pub(crate) fn srgb_linear_channel(channel: u8) -> f32 {
    static LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
        std::array::from_fn(|channel| {
            let value = channel as f32 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    });
    LINEAR[usize::from(channel)]
}

/// Encode linear light to the same rounded byte as the sRGB transfer function.
/// A 12-bit bucket narrows the result to two adjacent bytes; the exact float
/// threshold selects between them without a power function for every pixel.
pub(crate) fn srgb_from_linear(value: f32) -> u8 {
    struct Encoding {
        limits: [f32; 255],
        buckets: [u8; 4097],
    }
    fn encode(value: f32) -> u8 {
        let srgb = if value <= 0.0031308 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round().clamp(0.0, 255.0) as u8
    }
    static ENCODING: LazyLock<Encoding> = LazyLock::new(|| Encoding {
        buckets: std::array::from_fn(|index| encode(index as f32 / 4096.0)),
        limits: std::array::from_fn(|index| {
            // Positive f32 bit patterns have the same order as their values.
            // Find the first float that rounds to the next sRGB byte using the
            // original transfer function, including its floating-point rounding.
            let mut low = 0u32;
            let mut high = 1.0f32.to_bits();
            while low < high {
                let middle = low + (high - low) / 2;
                if encode(f32::from_bits(middle)) > index as u8 {
                    high = middle;
                } else {
                    low = middle + 1;
                }
            }
            f32::from_bits(low)
        }),
    });
    let table = &*ENCODING;
    let result = table.buckets[(value.clamp(0.0, 1.0) * 4096.0) as usize];
    result + u8::from(result < 255 && value >= table.limits[usize::from(result)])
}

/// Relative luminance contrast for resolved RGB theme colors.
pub(crate) fn contrast_ratio(a: Color, b: Color) -> Option<f32> {
    let luminance = |color| {
        let Color::Rgb(r, g, b) = color else {
            return None;
        };
        let linear = srgb_linear_channel;
        Some(0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b))
    };
    let a = luminance(a)?;
    let b = luminance(b)?;
    Some((a.max(b) + 0.05) / (a.min(b) + 0.05))
}

/// Legacy native palettes can opt into contrast fallbacks. An empty palette
/// preserves imported VS Code roles, including intentionally muted colors.
pub(crate) fn ensure_contrast(
    color: Color,
    background: Color,
    minimum: f32,
    palette: &[Color],
) -> Color {
    if palette.is_empty() { return color; }
    let Some(mut best_ratio) = contrast_ratio(color, background) else {
        return color;
    };
    if best_ratio >= minimum {
        return color;
    }
    let mut best = color;
    for &candidate in palette {
        let Some(ratio) = contrast_ratio(candidate, background) else {
            continue;
        };
        if ratio >= minimum {
            return candidate;
        }
        if ratio > best_ratio {
            best = candidate;
            best_ratio = ratio;
        }
    }
    best
}

/// Keep disabled text visible but less prominent than enabled text on the
/// same surface. Description colors can otherwise invert the two states.
pub(crate) fn disabled_text_color(preferred: Color, enabled: Color, background: Color, palette: &[Color]) -> Color {
    if palette.is_empty() { return preferred; }
    let Some(enabled_contrast) = contrast_ratio(enabled, background) else {
        return preferred;
    };
    if contrast_ratio(preferred, background)
        .is_some_and(|ratio| ratio >= 1.5 && ratio <= enabled_contrast * 0.65)
    {
        preferred
    } else {
        crate::app_theme::lerp_color(background, enabled, 0.5)
    }
}

/// Terminal block carets need both a visible block and readable text inside it.
/// Resolve against the surface being painted, which may differ from the editor.
pub(crate) fn block_cursor_style(
    block: Color,
    text: Color,
    surface: Color,
    palette: &[Color],
) -> Style {
    let block = ensure_contrast(block, surface, 3.0, palette);
    Style::default()
        .bg(block)
        .fg(ensure_contrast(text, block, 4.5, palette))
}

/// Cache contrast per foreground/background pair. Diff syntax and selected
/// terminal text reuse a small palette, so this does no luminance work per cell.
pub(crate) struct TextContrastCache {
    palette: Option<[Color; 14]>,
    colors: rustc_hash::FxHashMap<(Color, Color), Color>,
}

impl TextContrastCache {
    pub(crate) fn new(colors: &crate::theme_workbench::WorkbenchColors) -> Self {
        Self {
            palette: colors.contrast_fallbacks(),
            colors: rustc_hash::FxHashMap::default(),
        }
    }

    pub(crate) fn foreground(&mut self, foreground: Color, background: Color) -> Color {
        let Some(palette) = &self.palette else { return foreground; };
        *self
            .colors
            .entry((foreground, background))
            .or_insert_with(|| ensure_contrast(foreground, background, 4.5, palette))
    }
}

/// Control rows can receive a gradient after their spans have been styled.
/// Recheck their text against the final background, without touching editor
/// syntax or terminal output. A row typically reuses only a few color pairs.
pub(crate) fn ensure_control_row_contrast(
    buffer: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    colors: &crate::theme_workbench::WorkbenchColors,
) {
    ensure_control_row_contrast_except(buffer, area, colors, &[]);
}

/// File-icon theme colors retain their identity on gradient tab bars. Their
/// spans are supplied by the renderer, so ordinary text of the same color is
/// still checked and product icons continue to use the workbench palette.
pub(crate) fn ensure_control_row_contrast_except(
    buffer: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    colors: &crate::theme_workbench::WorkbenchColors,
    file_icons: &[ratatui::layout::Rect],
) {
    if colors.preserve_theme_colors { return; }
    let mut cache = TextContrastCache::new(colors);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if file_icons.iter().any(|icon| x >= icon.x && x < icon.right() && y >= icon.y && y < icon.bottom()) {
                continue;
            }
            if let Some(cell) = buffer.cell_mut((x, y))
                && !cell.symbol().trim().is_empty()
            {
                cell.set_fg(cache.foreground(cell.fg, cell.bg));
            }
        }
    }
}

pub(crate) struct TerminalSelectionStyle {
    background: Color,
    foreground: Option<Color>,
    cache: TextContrastCache,
}

impl TerminalSelectionStyle {
    pub(crate) fn new(colors: &crate::theme_workbench::WorkbenchColors) -> Self {
        Self {
            background: colors.readable_foreground(
                colors.bg_terminal_selection,
                colors.bg_terminal,
                1.5,
            ),
            foreground: colors.fg_terminal_selection,
            cache: TextContrastCache::new(colors),
        }
    }

    pub(crate) fn apply(&mut self, style: Style, fallback: Color) -> Style {
        let foreground = self.foreground.or(style.fg).unwrap_or(fallback);
        style
            .bg(self.background)
            .fg(self.cache.foreground(foreground, self.background))
            .remove_modifier(ratatui::style::Modifier::DIM)
    }
}

/// A field selection is softer than a caret, but must still be distinguishable
/// from unselected text, including in themes with identical input/list colors.
pub(crate) fn input_selection_style(
    text: Color,
    background: Color,
    surface: Color,
    palette: &[Color],
) -> Style {
    let background = ensure_contrast(background, surface, 1.5, palette);
    Style::default()
        .bg(background)
        .fg(ensure_contrast(text, background, 4.5, palette))
}

/// Workbench fields retain their foreground and selection opacity until the
/// final field surface is known, including specialized settings controls.
pub(crate) fn input_selection_from_layers(
    foreground: crate::theme_workbench::ColorLayer,
    fill: crate::theme_workbench::ColorLayer,
    surface: Color,
    fallback: Color,
    palette: &[Color],
) -> Style {
    let background = fill.over(surface).unwrap_or(surface);
    let background = if fill.is_transparent() { background }
        else { ensure_contrast(background, surface, 1.5, palette) };
    let foreground = foreground.over(background).unwrap_or(fallback);
    Style::default().bg(background).fg(ensure_contrast(foreground, background, 4.5, palette))
}

/// Keep the selection distinct on both ordinary and highlighted lines. Resolve
/// once per draw, and cache text adjustments per syntax color, not per glyph.
pub(crate) struct EditorSelectionStyle {
    background: Color,
    pending_surfaces: Option<[Color; 2]>,
    foreground: Option<Color>,
    default_foreground: Color,
    foregrounds: rustc_hash::FxHashMap<Color, Color>,
    last_foreground: Option<(Color, Color)>,
    palette: Option<[Color; 14]>,
    copy_feedback: Option<f32>,
    copy_visible: bool,
}

impl EditorSelectionStyle {
    pub(crate) fn new(colors: &crate::app_theme::ThemeColors) -> Self {
        Self {
            background: colors.bg_selection,
            pending_surfaces: (!colors.ui.preserve_theme_colors)
                .then_some([colors.bg_editor, colors.bg_line_highlight]),
            foreground: colors.ui.fg_editor_selection,
            default_foreground: colors.fg_text,
            foregrounds: rustc_hash::FxHashMap::default(),
            last_foreground: None,
            palette: colors.ui.contrast_fallbacks(),
            copy_feedback: None,
            copy_visible: false,
        }
    }

    fn prepare_background(&mut self) {
        // Most rows have no selected cells. A low-contrast native palette can
        // require hundreds of color checks, so defer that work until a selected
        // cell is actually painted (including virtual space and diff selections).
        let Some(surfaces) = self.pending_surfaces.take() else { return; };
        let preferred = self.background;
        let visible = |candidate| {
            surfaces
                .into_iter()
                .all(|surface| contrast_ratio(candidate, surface).is_none_or(|ratio| ratio >= 1.6))
        };
        self.background = if visible(preferred) {
            preferred
        } else {
            // Try both directions: darkening against a light current line must
            // not make the selection disappear into a darker editor surface.
            contrast_colors(&self.palette)
                .iter().copied()
                .filter_map(|target| {
                    (1..=32)
                        .map(|step| {
                            crate::app_theme::lerp_color(preferred, target, step as f32 / 32.0)
                        })
                        .find(|&candidate| visible(candidate))
                })
                .min_by_key(|&candidate| match (preferred, candidate) {
                    (Color::Rgb(r, g, b), Color::Rgb(cr, cg, cb)) => {
                        (i32::from(r) - i32::from(cr)).pow(2)
                            + (i32::from(g) - i32::from(cg)).pow(2)
                            + (i32::from(b) - i32::from(cb)).pow(2)
                    }
                    _ => 0,
                })
                .unwrap_or(preferred)
        };
    }

    pub(crate) fn with_copy_feedback(mut self, surface: Color, fade: f32) -> Self {
        self.prepare_background();
        self.background = crate::app_theme::lerp_color(self.background, surface, fade);
        self.copy_feedback = Some(fade);
        self
    }

    pub(crate) fn copy_feedback_visible(&self) -> bool { self.copy_visible }

    #[inline]
    pub(crate) fn apply(&mut self, style: Style) -> Style {
        if self.pending_surfaces.is_some() {
            self.prepare_background();
        }
        self.copy_visible |= self.copy_feedback.is_some();
        let mut foreground = self
            .foreground
            .or(style.fg)
            .unwrap_or(self.default_foreground);
        if let Some(fade) = self.copy_feedback && self.foreground.is_some() {
            foreground = crate::app_theme::lerp_color(foreground,
                style.fg.unwrap_or(self.default_foreground), fade);
        }
        // Imported themes preserve their colors, so no contrast cache is needed.
        // Native syntax commonly repeats a foreground for an entire token.
        let foreground = if let Some(palette) = &self.palette {
            if let Some((_, output)) = self.last_foreground.filter(|&(input, _)| input == foreground) {
                output
            } else {
                let output = *self.foregrounds.entry(foreground)
                    .or_insert_with(|| ensure_contrast(foreground, self.background, 4.5, palette));
                self.last_foreground = Some((foreground, output));
                output
            }
        } else { foreground };
        style.bg(self.background).fg(foreground)
    }
}

#[cfg(test)]
#[path = "ui_selection_style_tests.rs"]
mod selection_style_tests;

pub(crate) fn contrast_colors(palette: &Option<[Color; 14]>) -> &[Color] {
    palette.as_ref().map_or(&[], |palette| palette.as_slice())
}

/// Line length above which inline color detection is skipped for performance.
const COLOR_DETECT_MAX_LINE_LEN: usize = 2000;

/// Common CSS named colors - only the most distinctive ones that won't match
/// normal code identifiers. Format: (name, r, g, b)
static CSS_COLOR_MAP: LazyLock<CssColorMap<&'static str, (u8, u8, u8)>> = LazyLock::new(|| {
    let mut m = CssColorMap::default();
    for &(name, r, g, b) in CSS_NAMED_COLORS {
        m.insert(name, (r, g, b));
    }
    m
});

const CSS_NAMED_COLORS: &[(&str, u8, u8, u8)] = &[
    ("aliceblue", 240, 248, 255), ("antiquewhite", 250, 235, 215),
    ("aqua", 0, 255, 255), ("aquamarine", 127, 255, 212),
    ("azure", 240, 255, 255), ("beige", 245, 245, 220),
    ("bisque", 255, 228, 196), ("blanchedalmond", 255, 235, 205),
    ("blueviolet", 138, 43, 226), ("brown", 165, 42, 42),
    ("burlywood", 222, 184, 135), ("cadetblue", 95, 158, 160),
    ("chartreuse", 127, 255, 0), ("chocolate", 210, 105, 30),
    ("coral", 255, 127, 80), ("cornflowerblue", 100, 149, 237),
    ("cornsilk", 255, 248, 220), ("crimson", 220, 20, 60),
    ("cyan", 0, 255, 255), ("darkblue", 0, 0, 139),
    ("darkcyan", 0, 139, 139), ("darkgoldenrod", 184, 134, 11),
    ("darkgray", 169, 169, 169), ("darkgreen", 0, 100, 0),
    ("darkkhaki", 189, 183, 107), ("darkmagenta", 139, 0, 139),
    ("darkolivegreen", 85, 107, 47), ("darkorange", 255, 140, 0),
    ("darkorchid", 153, 50, 204), ("darkred", 139, 0, 0),
    ("darksalmon", 233, 150, 122), ("darkseagreen", 143, 188, 143),
    ("darkslateblue", 72, 61, 139), ("darkslategray", 47, 79, 79),
    ("darkturquoise", 0, 206, 209), ("darkviolet", 148, 0, 211),
    ("deeppink", 255, 20, 147), ("deepskyblue", 0, 191, 255),
    ("dimgray", 105, 105, 105), ("dodgerblue", 30, 144, 255),
    ("firebrick", 178, 34, 34), ("floralwhite", 255, 250, 240),
    ("forestgreen", 34, 139, 34), ("fuchsia", 255, 0, 255),
    ("gainsboro", 220, 220, 220), ("ghostwhite", 248, 248, 255),
    ("gold", 255, 215, 0), ("goldenrod", 218, 165, 32),
    ("greenyellow", 173, 255, 47), ("honeydew", 240, 255, 240),
    ("hotpink", 255, 105, 180), ("indianred", 205, 92, 92),
    ("indigo", 75, 0, 130), ("ivory", 255, 255, 240),
    ("khaki", 240, 230, 140), ("lavender", 230, 230, 250),
    ("lavenderblush", 255, 240, 245), ("lawngreen", 124, 252, 0),
    ("lemonchiffon", 255, 250, 205), ("lightblue", 173, 216, 230),
    ("lightcoral", 240, 128, 128), ("lightcyan", 224, 255, 255),
    ("lightgoldenrodyellow", 250, 250, 210), ("lightgray", 211, 211, 211),
    ("lightgreen", 144, 238, 144), ("lightpink", 255, 182, 193),
    ("lightsalmon", 255, 160, 122), ("lightseagreen", 32, 178, 170),
    ("lightskyblue", 135, 206, 250), ("lightslategray", 119, 136, 153),
    ("lightsteelblue", 176, 196, 222), ("lightyellow", 255, 255, 224),
    ("limegreen", 50, 205, 50), ("linen", 250, 240, 230),
    ("magenta", 255, 0, 255), ("maroon", 128, 0, 0),
    ("mediumaquamarine", 102, 205, 170), ("mediumblue", 0, 0, 205),
    ("mediumorchid", 186, 85, 211), ("mediumpurple", 147, 112, 219),
    ("mediumseagreen", 60, 179, 113), ("mediumslateblue", 123, 104, 238),
    ("mediumspringgreen", 0, 250, 154), ("mediumturquoise", 72, 209, 204),
    ("mediumvioletred", 199, 21, 133), ("midnightblue", 25, 25, 112),
    ("mintcream", 245, 255, 250), ("mistyrose", 255, 228, 225),
    ("moccasin", 255, 228, 181), ("navajowhite", 255, 222, 173),
    ("navy", 0, 0, 128), ("oldlace", 253, 245, 230),
    ("olive", 128, 128, 0), ("olivedrab", 107, 142, 35),
    ("orange", 255, 165, 0), ("orangered", 255, 69, 0),
    ("orchid", 218, 112, 214), ("palegoldenrod", 238, 232, 170),
    ("palegreen", 152, 251, 152), ("paleturquoise", 175, 238, 238),
    ("palevioletred", 219, 112, 147), ("papayawhip", 255, 239, 213),
    ("peachpuff", 255, 218, 185), ("peru", 205, 133, 63),
    ("pink", 255, 192, 203), ("plum", 221, 160, 221),
    ("powderblue", 176, 224, 230), ("purple", 128, 0, 128),
    ("rebeccapurple", 102, 51, 153), ("rosybrown", 188, 143, 143),
    ("royalblue", 65, 105, 225), ("saddlebrown", 139, 69, 19),
    ("salmon", 250, 128, 114), ("sandybrown", 244, 164, 96),
    ("seagreen", 46, 139, 87), ("seashell", 255, 245, 238),
    ("sienna", 160, 82, 45), ("silver", 192, 192, 192),
    ("skyblue", 135, 206, 235), ("slateblue", 106, 90, 205),
    ("slategray", 112, 128, 144), ("snow", 255, 250, 250),
    ("springgreen", 0, 255, 127), ("steelblue", 70, 130, 180),
    ("teal", 0, 128, 128), ("thistle", 216, 191, 216),
    ("tomato", 255, 99, 71), ("turquoise", 64, 224, 208),
    ("violet", 238, 130, 238), ("wheat", 245, 222, 179),
    ("whitesmoke", 245, 245, 245), ("yellowgreen", 154, 205, 50),
];

/// Check if byte is a word boundary character (not alphanumeric or underscore)
fn is_word_boundary(b: u8) -> bool {
    !b.is_ascii_alphanumeric() && b != b'_'
}

/// Find inline hex color values (#RGB, #RRGGBB) and CSS color functions in a line.
/// Returns (column, r, g, b) for each color found.
pub(crate) fn find_inline_colors(line: &str, check_named: bool) -> Vec<(usize, u8, u8, u8)> {
    let mut results = Vec::new();
    if line.len() > COLOR_DETECT_MAX_LINE_LEN {
        return results; // skip very long lines for performance
    }
    let bytes = line.as_bytes();
    let is_ascii = line.is_ascii();
    // Only translate offsets of actual colors. Each pass visits matches in
    // byte order, so Unicode columns need at most one scan and no lookup table.
    let (mut byte, mut column) = (0, 0);
    let mut col_at = |i: usize| -> usize {
        if is_ascii { return i; }
        if i < byte { byte = 0; column = 0; }
        column += line[byte..i].chars().count();
        byte = i;
        column
    };
    // Reuse the next ')' across candidates, including an unterminated suffix.
    let mut closing = 0;
    let mut close_at = |start: usize| {
        if closing < start {
            closing = memchr::memchr(b')', &bytes[start..]).map_or(bytes.len(), |end| start + end);
        }
        (closing < bytes.len()).then_some(closing)
    };

    for i in memchr::memchr3_iter(b'#', b'r', b'h', bytes) {
        let b = bytes[i];
        if b == b'#' {
            // Try #RRGGBBAA (8 hex digits) or #RRGGBB (6 hex digits)
            if (i + 8 < bytes.len() && bytes[i+1..i+9].iter().all(|b| b.is_ascii_hexdigit())
                && (i + 9 >= bytes.len() || !bytes[i+9].is_ascii_hexdigit()))
               || (i + 6 < bytes.len() && bytes[i+1..i+7].iter().all(|b| b.is_ascii_hexdigit())
                && (i + 7 >= bytes.len() || !bytes[i+7].is_ascii_hexdigit()))
            {
                if let (Ok(r), Ok(g), Ok(b_val)) = (
                    u8::from_str_radix(std::str::from_utf8(&bytes[i+1..i+3]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&bytes[i+3..i+5]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&bytes[i+5..i+7]).unwrap_or(""), 16),
                ) {
                    results.push((col_at(i), r, g, b_val));
                }
            }
            // Try #RGB (3 hex digits)
            else if i + 3 < bytes.len() && bytes[i+1..i+4].iter().all(|b| b.is_ascii_hexdigit())
                && (i + 4 >= bytes.len() || !bytes[i+4].is_ascii_hexdigit())
            {
                let r = bytes[i+1];
                let g = bytes[i+2];
                let b_ch = bytes[i+3];
                if let (Ok(rv), Ok(gv), Ok(bv)) = (
                    u8::from_str_radix(std::str::from_utf8(&[r]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&[g]).unwrap_or(""), 16),
                    u8::from_str_radix(std::str::from_utf8(&[b_ch]).unwrap_or(""), 16),
                ) {
                    results.push((col_at(i), rv * 17, gv * 17, bv * 17));
                }
            }
        }
        // Detect rgb(r, g, b) and rgba(r, g, b, a)
        if b == b'r' && i + 4 < bytes.len() {
            let is_rgba = i + 5 < bytes.len() && &bytes[i..i+5] == b"rgba(";
            let is_rgb = !is_rgba && &bytes[i..i+4] == b"rgb(";
            if is_rgb || is_rgba {
                let start = if is_rgba { i + 5 } else { i + 4 };
                if let Some(end) = close_at(start) {
                    let inner = &line[start..end];
                    // Parse CSV without allocating a Vec
                    let mut parts = inner.splitn(4, ',');
                    if let (Some(p0), Some(p1), Some(p2)) = (parts.next(), parts.next(), parts.next()) {
                        if let (Ok(r), Ok(g), Ok(bv)) = (
                            p0.trim().parse::<u8>(),
                            p1.trim().parse::<u8>(),
                            p2.trim().parse::<u8>(),
                        ) {
                            results.push((col_at(i), r, g, bv));
                        }
                    }
                }
            }
        }
        // Detect hsl(h, s%, l%) and hsla(h, s%, l%, a)
        if b == b'h' && i + 4 < bytes.len() {
            let is_hsla = i + 5 < bytes.len() && &bytes[i..i+5] == b"hsla(";
            let is_hsl = !is_hsla && &bytes[i..i+4] == b"hsl(";
            if is_hsl || is_hsla {
                let start = if is_hsla { i + 5 } else { i + 4 };
                if let Some(end) = close_at(start) {
                    let inner = &line[start..end];
                    // Parse CSV without allocating a Vec
                    let mut parts = inner.splitn(4, ',');
                    if let (Some(p0), Some(p1), Some(p2)) = (parts.next(), parts.next(), parts.next()) {
                        let h_str = p0.trim();
                        let s_str = p1.trim().trim_end_matches('%');
                        let l_str = p2.trim().trim_end_matches('%');
                        if let (Ok(h), Ok(s), Ok(l)) = (
                            h_str.parse::<f32>(),
                            s_str.parse::<f32>(),
                            l_str.parse::<f32>(),
                        ) {
                            let (r, g, bv) = hsl_to_rgb(h, s / 100.0, l / 100.0);
                            results.push((col_at(i), r, g, bv));
                        }
                    }
                }
            }
        }
    }
    if check_named {
        // Extract words and compare case-insensitively without allocating
        // a full lowercase copy of the line.
        let mut word_start = None;
        for i in 0..=bytes.len() {
            let is_boundary = i == bytes.len() || is_word_boundary(bytes[i]);
            if is_boundary {
                if let Some(start) = word_start {
                    let word = &line[start..i];
                    // CSS color names are ASCII, so we can lowercase just the word
                    let mut buf = [0u8; 24]; // longest CSS color name is 20 chars
                    if word.len() <= buf.len() && word.is_ascii() {
                        let wbytes = word.as_bytes();
                        buf[..word.len()].copy_from_slice(wbytes);
                        buf[..word.len()].make_ascii_lowercase();
                        if let Ok(lower_word) = std::str::from_utf8(&buf[..word.len()]) {
                            if let Some(&(r, g, b)) = CSS_COLOR_MAP.get(lower_word) {
                                results.push((col_at(start), r, g, b));
                            }
                        }
                    }
                    word_start = None;
                }
            } else if word_start.is_none() {
                word_start = Some(i);
            }
        }
    }
    results
}

#[cfg(test)]
#[path = "ui_color_scan_tests.rs"]
mod scan_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // ── lighten ──────────────────────────────────────────────────────

    #[test]
    fn lighten_rgb_basic() {
        assert_eq!(lighten(Color::Rgb(100, 100, 100), 10), Color::Rgb(110, 110, 110));
    }

    #[test]
    fn lighten_rgb_saturates_at_255() {
        assert_eq!(lighten(Color::Rgb(250, 250, 250), 20), Color::Rgb(255, 255, 255));
    }

    #[test]
    fn lighten_zero_amount() {
        assert_eq!(lighten(Color::Rgb(42, 42, 42), 0), Color::Rgb(42, 42, 42));
    }

    #[test]
    fn lighten_non_rgb_passes_through() {
        assert_eq!(lighten(Color::Red, 50), Color::Red);
        assert_eq!(lighten(Color::Reset, 50), Color::Reset);
    }

    #[test]
    fn lighten_black_to_gray() {
        assert_eq!(lighten(Color::Rgb(0, 0, 0), 128), Color::Rgb(128, 128, 128));
    }

    // ── is_word_boundary ─────────────────────────────────────────────

    #[test]
    fn word_boundary_space() {
        assert!(is_word_boundary(b' '));
    }

    #[test]
    fn word_boundary_punctuation() {
        assert!(is_word_boundary(b','));
        assert!(is_word_boundary(b'.'));
        assert!(is_word_boundary(b';'));
        assert!(is_word_boundary(b'('));
    }

    #[test]
    fn not_word_boundary_alpha() {
        assert!(!is_word_boundary(b'a'));
        assert!(!is_word_boundary(b'Z'));
    }

    #[test]
    fn not_word_boundary_digit() {
        assert!(!is_word_boundary(b'0'));
        assert!(!is_word_boundary(b'9'));
    }

    #[test]
    fn not_word_boundary_underscore() {
        assert!(!is_word_boundary(b'_'));
    }

    // ── find_inline_colors: hex ──────────────────────────────────────

    #[test]
    fn hex_6_digit() {
        let colors = find_inline_colors("#ff8000", false);
        assert_eq!(colors, vec![(0, 255, 128, 0)]);
    }

    #[test]
    fn hex_6_digit_uppercase() {
        let colors = find_inline_colors("#FF8000", false);
        assert_eq!(colors, vec![(0, 255, 128, 0)]);
    }

    #[test]
    fn hex_3_digit() {
        let colors = find_inline_colors("#f80", false);
        assert_eq!(colors, vec![(0, 255, 136, 0)]);
    }

    #[test]
    fn hex_8_digit_rgba() {
        let colors = find_inline_colors("#ff8000ff", false);
        assert_eq!(colors.len(), 1);
        assert_eq!(colors[0], (0, 255, 128, 0));
    }

    #[test]
    fn hex_multiple_on_line() {
        let colors = find_inline_colors("color: #ff0000; border: #00ff00;", false);
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0], (7, 255, 0, 0));
        assert_eq!(colors[1], (24, 0, 255, 0));
    }

    #[test]
    fn hex_black_and_white() {
        let colors = find_inline_colors("#000000 #ffffff", false);
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0], (0, 0, 0, 0));
        assert_eq!(colors[1], (8, 255, 255, 255));
    }

    #[test]
    fn no_colors_in_plain_text() {
        assert!(find_inline_colors("hello world", false).is_empty());
    }

    #[test]
    fn hex_not_enough_digits() {
        // #ab is only 2 hex digits, not valid
        assert!(find_inline_colors("#ab ", false).is_empty());
    }

    // ── find_inline_colors: rgb/rgba ─────────────────────────────────

    #[test]
    fn rgb_function() {
        let colors = find_inline_colors("rgb(255, 128, 0)", false);
        assert_eq!(colors, vec![(0, 255, 128, 0)]);
    }

    #[test]
    fn rgba_function() {
        let colors = find_inline_colors("rgba(255, 0, 128, 0.5)", false);
        assert_eq!(colors, vec![(0, 255, 0, 128)]);
    }

    #[test]
    fn rgb_no_spaces() {
        let colors = find_inline_colors("rgb(10,20,30)", false);
        assert_eq!(colors, vec![(0, 10, 20, 30)]);
    }

    // ── find_inline_colors: hsl ──────────────────────────────────────

    #[test]
    fn hsl_function() {
        let colors = find_inline_colors("hsl(0, 100%, 50%)", false);
        assert_eq!(colors.len(), 1);
        assert_eq!(colors[0].1, 255); // red
        assert_eq!(colors[0].2, 0);   // green
        assert_eq!(colors[0].3, 0);   // blue
    }

    #[test]
    fn hsla_function() {
        let colors = find_inline_colors("hsla(120, 100%, 50%, 1.0)", false);
        assert_eq!(colors.len(), 1);
        assert_eq!(colors[0].2, 255); // green channel should be max
    }

    // ── find_inline_colors: named colors ─────────────────────────────

    #[test]
    fn named_color_found() {
        let colors = find_inline_colors("color: tomato;", true);
        assert_eq!(colors.len(), 1);
        assert_eq!(colors[0], (7, 255, 99, 71));
    }

    #[test]
    fn named_color_case_insensitive() {
        let colors = find_inline_colors("Crimson", true);
        assert_eq!(colors.len(), 1);
        assert_eq!(colors[0], (0, 220, 20, 60));
    }

    #[test]
    fn named_color_not_found_when_disabled() {
        let colors = find_inline_colors("tomato", false);
        assert!(colors.is_empty());
    }

    #[test]
    fn named_color_multiple() {
        let colors = find_inline_colors("gold and coral", true);
        assert_eq!(colors.len(), 2);
    }

    // ── find_inline_colors: edge cases ───────────────────────────────

    #[test]
    fn very_long_line_skipped() {
        let long_line = "x".repeat(2001);
        assert!(find_inline_colors(&long_line, false).is_empty());
    }

    #[test]
    fn empty_line() {
        assert!(find_inline_colors("", false).is_empty());
    }

    #[test]
    fn mixed_formats() {
        let line = "#ff0000 rgb(0, 255, 0) hsl(240, 100%, 50%)";
        let colors = find_inline_colors(line, false);
        assert_eq!(colors.len(), 3);
    }
}

