use std::fmt::Write as FmtWrite;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;
use unicode_segmentation::UnicodeSegmentation;

use crate::helpers::len_as_u16;
use crate::text_input::TextInputHitMap;

// ---------------------------------------------------------------------------
// DropdownItem – one row inside a dropdown / context menu
// ---------------------------------------------------------------------------

/// A single item in a dropdown menu. Supports labels, shortcuts, icons, separators.
pub(crate) struct DropdownItem<'a> {
    pub label: &'a str,
    pub shortcut: &'a str,
    pub separator_after: bool,
    pub icon: &'a str,
    pub icon_color: Option<Color>,
    pub disabled: bool,
}

impl<'a> DropdownItem<'a> {
    pub(crate) fn new(label: &'a str) -> Self {
        Self {
            label,
            shortcut: "",
            separator_after: false,
            icon: "",
            icon_color: None,
            disabled: false,
        }
    }

    pub(crate) fn shortcut(mut self, s: &'a str) -> Self {
        self.shortcut = s;
        self
    }

    pub(crate) fn separator(mut self) -> Self {
        self.separator_after = true;
        self
    }

    pub(crate) fn icon(mut self, icon: &'a str, color: Color) -> Self {
        self.icon = icon;
        self.icon_color = Some(color);
        self
    }

    pub(crate) fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
}

// ---------------------------------------------------------------------------
// DropdownColors – theming knobs
// ---------------------------------------------------------------------------

/// Color scheme for dropdown menus and context menus.
pub(crate) struct DropdownColors {
    pub layers: Option<DropdownLayers>,
    pub contrast_palette: Option<[Color; 14]>,
    /// Background color for the dropdown panel.
    pub bg: Color,
    /// Background color for the currently selected/hovered item.
    pub bg_selected: Color,
    pub fg: Color,
    pub fg_selected: Color,
    pub border_selected: Option<Color>,
    /// Foreground color for keyboard-shortcut hints.
    pub fg_shortcut: Color,
    /// Foreground color for separator lines between items.
    pub fg_separator: Color,
    /// Foreground color for disabled (greyed-out) items.
    pub fg_disabled: Color,
    pub border: Color,
    pub scrollbar_fg: Color,
    pub scrollbar_bg: Color,
}

pub(crate) struct DropdownLayers {
    pub normal: crate::theme_workbench::ControlColors,
    pub selected: crate::theme_workbench::ControlColors,
    pub separator: crate::theme_workbench::ColorLayer,
    pub disabled: crate::theme_workbench::ColorLayer,
    pub shortcut: crate::theme_workbench::ColorLayer,
    pub slider: crate::theme_workbench::ColorLayer,
    pub track: crate::theme_workbench::ColorLayer,
    pub fallback_surface: Color,
}

// ---------------------------------------------------------------------------
// DropdownMenu – reusable dropdown / context‑menu widget
// ---------------------------------------------------------------------------

/// Generic dropdown / context menu widget.
///
/// Renders a bordered popup at the given anchor position, clamped to the
/// terminal boundaries, with a list of items that supports:
///  - keyboard selection highlight
///  - mouse hover highlight
///  - optional shortcut column
///  - separator lines (`"─"` labels or `separator_after`)
pub(crate) struct DropdownMenu<'a> {
    items: &'a [DropdownItem<'a>],
    selected: usize,
    anchor_x: u16,
    anchor_y: u16,
    min_width: u16,
    max_visible: Option<u16>,
    scroll_offset: usize,
    colors: DropdownColors,
    mouse_row: u16,
    mouse_col: u16,
}

impl<'a> DropdownMenu<'a> {
    pub(crate) fn new(items: &'a [DropdownItem<'a>], colors: DropdownColors) -> Self {
        Self {
            items,
            selected: 0,
            anchor_x: 0,
            anchor_y: 0,
            min_width: 16,
            max_visible: None,
            scroll_offset: 0,
            colors,
            mouse_row: u16::MAX,
            mouse_col: u16::MAX,
        }
    }

    pub(crate) fn selected(mut self, idx: usize) -> Self {
        self.selected = idx;
        self
    }

    pub(crate) fn anchor(mut self, x: u16, y: u16) -> Self {
        self.anchor_x = x;
        self.anchor_y = y;
        self
    }

    pub(crate) fn min_width(mut self, w: u16) -> Self {
        self.min_width = w;
        self
    }

    pub(crate) fn max_visible(mut self, n: u16) -> Self {
        self.max_visible = Some(n);
        self
    }

    pub(crate) fn scroll_offset(mut self, offset: usize) -> Self {
        self.scroll_offset = offset;
        self
    }

    pub(crate) fn mouse(mut self, row: u16, col: u16) -> Self {
        self.mouse_row = row;
        self.mouse_col = col;
        self
    }

    /// Compute the total number of visual rows (items + separators).
    fn visual_row_count(&self) -> usize {
        let mut rows = self.items.len();
        for item in self.items {
            if item.separator_after {
                rows += 1;
            }
        }
        rows
    }

    /// Compute the needed content width based on items.
    fn content_width(&self) -> u16 {
        let has_shortcut = self.items.iter().any(|i| !i.shortcut.is_empty());
        let max_icon = self
            .items
            .iter()
            .map(|i| UnicodeWidthStr::width(i.icon))
            .max()
            .unwrap_or(0);
        let max_label = self
            .items
            .iter()
            .map(|i| UnicodeWidthStr::width(i.label))
            .max()
            .unwrap_or(10);
        let max_shortcut = if has_shortcut {
            self.items
                .iter()
                .map(|i| UnicodeWidthStr::width(i.shortcut))
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        let gap = if max_shortcut > 0 { 2 } else { 0 };
        let leading = if max_icon > 0 { max_icon } else { 1 }; // icon or leading space
        (leading + max_label + gap + max_shortcut + 1) as u16
    }

    /// Render the dropdown and return the inner `Rect` (for mouse hit-testing).
    pub(crate) fn render(self, f: &mut Frame) -> Rect {
        self.render_impl(f, None)
    }

    /// Keep selection visible and report the actual first visual row for hits.
    pub(crate) fn render_scrolled(self, f: &mut Frame, scroll: &mut usize) -> Rect {
        self.render_impl(f, Some(scroll))
    }

    fn render_impl(mut self, f: &mut Frame, scroll: Option<&mut usize>) -> Rect {
        if self.items.is_empty() {
            return Rect::default();
        }
        let term = f.area();
        if term.width < 3 || term.height < 3 {
            return Rect::default();
        }
        // Resize events can leave a popup anchored beyond the current frame.
        self.anchor_x = self.anchor_x.min(term.width.saturating_sub(1));
        self.anchor_y = self.anchor_y.min(term.height.saturating_sub(1));

        let visual_rows = self.visual_row_count();
        let max_vis = self.max_visible.unwrap_or(u16::MAX);
        let inner_height = (visual_rows as u16).min(max_vis);
        let needs_scrollbar = visual_rows as u16 > inner_height && inner_height > 0;

        let content_w = self.content_width().max(self.min_width);
        // Full cells are easier to see than a thin glyph in a muted theme.
        let dropdown_width = content_w + 2 + if needs_scrollbar { 2 } else { 0 };
        let dropdown_height = inner_height + 2; // borders

        let dropdown_area = {
            // Smart repositioning: flip above/left when near screen edges
            let x = if self.anchor_x.saturating_add(dropdown_width) > term.width {
                // Would overflow right edge - shift left
                term.width.saturating_sub(dropdown_width)
            } else {
                self.anchor_x
            };

            let (y, h) = if self.anchor_y.saturating_add(dropdown_height) <= term.height {
                // Fits below anchor
                (self.anchor_y, dropdown_height)
            } else if self.anchor_y >= dropdown_height {
                // Flip above anchor
                (self.anchor_y - dropdown_height, dropdown_height)
            } else {
                // Can't fit either way — use whichever side has more space
                if self.anchor_y > term.height.saturating_sub(self.anchor_y) {
                    // More space above
                    let h = self.anchor_y.min(dropdown_height);
                    (self.anchor_y.saturating_sub(h), h)
                } else {
                    // More space below (or equal)
                    let h = term
                        .height
                        .saturating_sub(self.anchor_y)
                        .min(dropdown_height);
                    (self.anchor_y, h)
                }
            };
            Rect::new(x, y, dropdown_width.min(term.width), h.max(2)).intersection(term)
        };

        if let Some(layers) = &self.colors.layers {
            let owner = surface_background(f, dropdown_area, layers.fallback_surface);
            let background = layers.normal.background.over(owner).unwrap_or(owner);
            let foreground = layers.normal.foreground.over(background).unwrap_or(self.colors.fg);
            let selected = layers.selected.background.over(background).unwrap_or(background);
            self.colors.bg = background;
            self.colors.fg = foreground;
            self.colors.bg_selected = selected;
            self.colors.fg_selected = layers.selected.foreground.over(selected).unwrap_or(foreground);
            self.colors.border = layers.normal.border.border(background).unwrap_or(background);
            self.colors.border_selected = layers.selected.border.border(selected);
            self.colors.fg_separator = layers.separator.over(background).unwrap_or(background);
            self.colors.fg_disabled = layers.disabled.over(background).unwrap_or(foreground);
            self.colors.fg_shortcut = layers.shortcut.over(background).unwrap_or(foreground);
            self.colors.scrollbar_bg = layers.track.over(background).unwrap_or(background);
            self.colors.scrollbar_fg = layers.slider.over(self.colors.scrollbar_bg).unwrap_or(self.colors.scrollbar_bg);
        }
        f.render_widget(Clear, dropdown_area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.colors.border))
            .style(Style::default().bg(self.colors.bg));
        let inner = block.inner(dropdown_area);
        f.render_widget(block, dropdown_area);

        if inner.width == 0 || inner.height == 0 {
            return inner;
        }

        let needs_scrollbar = visual_rows > inner.height as usize;
        if let Some(scroll) = scroll {
            let selected = self.selected.min(self.items.len() - 1);
            let selected_row = selected + self.items[..selected].iter().filter(|item| item.separator_after).count();
            let mut offset = (*scroll).min(visual_rows.saturating_sub(inner.height as usize));
            if selected_row < offset { offset = selected_row; }
            else if selected_row >= offset + inner.height as usize {
                offset = selected_row + 1 - inner.height as usize;
            }
            // Begin on an item, keeping separator rows and pointer hits aligned.
            let mut row = 0;
            for item in self.items {
                if row >= offset { offset = row; break; }
                row += 1 + usize::from(item.separator_after);
            }
            self.scroll_offset = offset;
            *scroll = offset;
        }

        // Item content area (narrowed by scrollbar if present)
        let scrollbar_width = if needs_scrollbar { 2.min(inner.width.saturating_sub(1)) } else { 0 };
        let item_width = inner.width.saturating_sub(scrollbar_width);

        let has_shortcut = self.items.iter().any(|i| !i.shortcut.is_empty());

        let palette = crate::ui_colors::contrast_colors(&self.colors.contrast_palette);
        let fg = crate::ui_colors::ensure_contrast(self.colors.fg, self.colors.bg, 4.5, &palette);
        let fg_disabled = crate::ui_colors::disabled_text_color(
            self.colors.fg_disabled, fg, self.colors.bg, palette,
        );
        let bg_selected = if !palette.is_empty() && self.colors.bg_selected == self.colors.bg {
            crate::app_theme::lerp_color(self.colors.bg, fg, 0.18)
        } else {
            self.colors.bg_selected
        };
        let fg_selected = crate::ui_colors::ensure_contrast(self.colors.fg_selected, bg_selected, 4.5, &palette);

        let mut row_y = inner.y;
        let mut visual_idx: usize = 0;
        let mut fmt_buf = String::with_capacity(128);
        for (item_idx, item) in self.items.iter().enumerate() {
            // Skip rows before scroll offset
            if visual_idx < self.scroll_offset {
                visual_idx += 1;
                if item.separator_after {
                    visual_idx += 1;
                }
                continue;
            }
            if row_y >= inner.y + inner.height {
                break;
            }

            // Separator-only row (label == "─")
            if item.label == "─" {
                let sep = "─".repeat(item_width as usize);
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        sep,
                        Style::default().fg(self.colors.fg_separator),
                    ))),
                    Rect::new(inner.x, row_y, item_width, 1),
                );
                row_y += 1;
                visual_idx += 1;
                continue;
            }

            let is_selected = item_idx == self.selected;
            let is_hovered = self.mouse_row == row_y
                && self.mouse_col >= inner.x
                && self.mouse_col < inner.x + inner.width;
            let (bg, fg) = if item.disabled {
                (self.colors.bg, fg_disabled)
            } else if is_selected || is_hovered {
                (bg_selected, fg_selected)
            } else {
                (self.colors.bg, fg)
            };
            let icon_fg = if item.disabled || is_selected || is_hovered {
                fg
            } else {
                crate::ui_colors::ensure_contrast(item.icon_color.unwrap_or(fg), bg, 3.0, &palette)
            };
            let shortcut_fg = if item.disabled || is_selected || is_hovered {
                fg
            } else {
                crate::ui_colors::ensure_contrast(self.colors.fg_shortcut, bg, 3.0, &palette)
            };

            let has_icon = !item.icon.is_empty();
            let icon_w = if has_icon {
                UnicodeWidthStr::width(item.icon)
            } else {
                0
            };

            let line = if has_shortcut && !item.shortcut.is_empty() {
                let shortcut_w = UnicodeWidthStr::width(item.shortcut);
                let label_max = (item_width as usize).saturating_sub(shortcut_w + 2 + icon_w);
                let label_w = UnicodeWidthStr::width(item.label).min(label_max);
                let pad = label_max.saturating_sub(label_w);
                let mut spans = Vec::new();
                if has_icon {
                    spans.push(Span::styled(
                        item.icon,
                        Style::default().fg(icon_fg).bg(bg),
                    ));
                }
                spans.push(Span::styled(
                    {
                        fmt_buf.clear();
                        let _ = write!(fmt_buf, " {}", item.label);
                        for _ in 0..pad {
                            fmt_buf.push(' ');
                        }
                        std::mem::take(&mut fmt_buf)
                    },
                    Style::default().fg(fg).bg(bg),
                ));
                spans.push(Span::styled(
                    {
                        fmt_buf.clear();
                        let _ = write!(fmt_buf, "{} ", item.shortcut);
                        std::mem::take(&mut fmt_buf)
                    },
                    Style::default().fg(shortcut_fg).bg(bg),
                ));
                Line::from(spans)
            } else if has_icon {
                let pad = (item_width as usize)
                    .saturating_sub(UnicodeWidthStr::width(item.label) + icon_w);
                Line::from(vec![
                    Span::styled(
                        item.icon,
                        Style::default().fg(icon_fg).bg(bg),
                    ),
                    Span::styled(
                        {
                            fmt_buf.clear();
                            let _ = write!(fmt_buf, "{}", item.label);
                            for _ in 0..pad {
                                fmt_buf.push(' ');
                            }
                            std::mem::take(&mut fmt_buf)
                        },
                        Style::default().fg(fg).bg(bg),
                    ),
                ])
            } else {
                let pad =
                    (item_width as usize).saturating_sub(UnicodeWidthStr::width(item.label) + 1);
                Line::from(Span::styled(
                    {
                        fmt_buf.clear();
                        let _ = write!(fmt_buf, " {}", item.label);
                        for _ in 0..pad {
                            fmt_buf.push(' ');
                        }
                        std::mem::take(&mut fmt_buf)
                    },
                    Style::default().fg(fg).bg(bg),
                ))
            };

            let mut row_style = Style::default().bg(bg);
            if !item.disabled && (is_selected || is_hovered)
                && let Some(border) = self.colors.border_selected
            {
                row_style = row_style.add_modifier(Modifier::UNDERLINED).underline_color(border);
            }
            f.render_widget(
                Paragraph::new(line).style(row_style),
                Rect::new(inner.x, row_y, item_width, 1),
            );
            row_y += 1;
            visual_idx += 1;

            // Separator after this item
            if item.separator_after && row_y < inner.y + inner.height {
                let sep = "─".repeat(item_width as usize);
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        sep,
                        Style::default().fg(self.colors.fg_separator),
                    ))),
                    Rect::new(inner.x, row_y, item_width, 1),
                );
                row_y += 1;
                visual_idx += 1;
            }
        }

        // Draw scrollbar when items overflow
        if needs_scrollbar && inner.height > 0 {
            let track_x = inner.right() - scrollbar_width;
            let track_h = inner.height as usize;
            let total = visual_rows;
            let thumb_h = ((track_h * track_h) / total).max(1).min(track_h);
            let max_scroll = total.saturating_sub(track_h);
            let thumb_top = if max_scroll > 0 {
                (self.scroll_offset.min(max_scroll) * (track_h - thumb_h)) / max_scroll
            } else {
                0
            };
            for i in 0..track_h {
                let fg_c = if i >= thumb_top && i < thumb_top + thumb_h {
                    self.colors.scrollbar_fg
                } else {
                    self.colors.scrollbar_bg
                };
                f.render_widget(
                    Paragraph::new(Span::styled(
                        " ".repeat(scrollbar_width as usize),
                        Style::default().fg(fg_c).bg(fg_c),
                    )),
                    Rect::new(track_x, inner.y + i as u16, scrollbar_width, 1),
                );
            }
        }

        inner
    }
}

// ---------------------------------------------------------------------------
// Action buttons
// ---------------------------------------------------------------------------

pub(crate) fn action_button_style(
    colors: &crate::app_theme::ThemeColors,
    surface: Color,
    primary: bool,
    hovered: bool,
    enabled: bool,
) -> Style {
    colors.ui.button_style(surface, primary, hovered, enabled)
}

pub(crate) fn draw_button_content(
    f: &mut Frame,
    area: Rect,
    label: &str,
    style: Style,
    highlighted: bool,
    centered: bool,
) {
    if area.is_empty() {
        return;
    }
    let label = if area.width >= 5 {
        let label = crate::ui_pickers::truncate_to_width(
            label.trim(),
            area.width.saturating_sub(4) as usize,
        );
        if highlighted {
            format!("[ {label} ]")
        } else {
            format!("  {label}  ")
        }
    } else {
        crate::ui_pickers::truncate_to_width(label.trim(), area.width as usize)
    };
    let alignment = if centered {
        ratatui::layout::Alignment::Center
    } else {
        ratatui::layout::Alignment::Left
    };
    f.render_widget(
        Paragraph::new(label).alignment(alignment).style(style),
        area,
    );
}

/// Read a surface before replacing its contents. Unpainted test/empty frames
/// fall back to the view that owns the control.
pub(crate) fn surface_background(f: &mut Frame, area: Rect, fallback: Color) -> Color {
    f.buffer_mut().cell((area.x, area.y)).map(|cell| cell.bg)
        .filter(|color| *color != Color::Reset).unwrap_or(fallback)
}

/// Resolve a floating widget's fill before clearing the underlying view. The
/// same widget can open over an editor, terminal, or sidebar surface.
pub(crate) fn widget_background(f: &mut Frame, colors: &crate::app_theme::ThemeColors, area: Rect) -> Color {
    colors.ui.widget_background_color(surface_background(f, area, colors.bg_editor))
}

/// Primary and secondary workbench actions share readable colors and the
/// explicit hover outline used by dialog actions, including in flat themes.
pub(crate) fn draw_action_button(
    f: &mut Frame,
    area: Rect,
    label: &str,
    colors: &crate::app_theme::ThemeColors,
    primary: bool,
    hovered: bool,
    enabled: bool,
) {
    let hovered = hovered && enabled;
    let surface = f.buffer_mut().cell((area.x, area.y)).map(|cell| cell.bg)
        .filter(|color| *color != Color::Reset).unwrap_or(colors.ui.bg_widget);
    draw_button_content(
        f,
        area,
        label,
        colors.ui.button_style(surface, primary, hovered, enabled),
        hovered,
        true,
    );
}

/// Dialog actions use a single focus target for keyboard and pointer input.
/// Brackets identify it even when a theme's button/hover colors are identical.
pub(crate) fn draw_dialog_button(
    f: &mut Frame,
    area: Rect,
    label: &str,
    colors: &crate::app_theme::ThemeColors,
    primary: bool,
    focused: bool,
    destructive: bool,
    enabled: bool,
) {
    let focused = focused && enabled;
    let surface = f.buffer_mut().cell((area.x, area.y)).map(|cell| cell.bg)
        .filter(|color| *color != Color::Reset).unwrap_or(colors.ui.bg_widget);
    let mut style = colors.ui.button_style(surface, primary, focused, enabled);
    if destructive && enabled {
        style = style.fg(colors.ui.error_text_color(style.bg.unwrap()));
    }
    if focused {
        style = style.add_modifier(Modifier::BOLD);
        if let Some(border) = colors.ui.focus_border.border(style.bg.unwrap()) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
    }
    draw_button_content(f, area, label, style, focused, false);
}

// ---------------------------------------------------------------------------
// PopupDialog – centered popup with title, border, and content area
// ---------------------------------------------------------------------------

/// A bordered popup dialog with a title, positioned either centered or near the top of the terminal.
pub(crate) struct PopupDialog<'a> {
    title: &'a str,
    title_color: Color,
    width: u16,
    height: u16,
    bg: Color,
    border_color: Color,
    position: PopupPosition,
}

/// Vertical positioning strategy for a [`PopupDialog`].
pub(crate) enum PopupPosition {
    /// Centered both horizontally and vertically in the terminal.
    Center,
    /// Centered horizontally, placed at a fixed row offset from the top.
    TopCenter(u16),
}

impl<'a> PopupDialog<'a> {
    pub(crate) fn new(title: &'a str) -> Self {
        Self {
            title,
            title_color: Color::Reset,
            width: 56,
            height: 5,
            bg: Color::Reset,
            border_color: Color::Reset,
            position: PopupPosition::Center,
        }
    }

    pub(crate) fn title_color(mut self, c: Color) -> Self {
        self.title_color = c;
        self
    }

    pub(crate) fn size(mut self, w: u16, h: u16) -> Self {
        self.width = w;
        self.height = h;
        self
    }

    pub(crate) fn bg(mut self, c: Color) -> Self {
        self.bg = c;
        self
    }

    pub(crate) fn border_color(mut self, c: Color) -> Self {
        self.border_color = c;
        self
    }

    pub(crate) fn position(mut self, pos: PopupPosition) -> Self {
        self.position = pos;
        self
    }

    pub(crate) fn area(&self, term: Rect) -> Rect {
        let w = self.width.min(term.width.saturating_sub(4));
        let h = self.height.min(term.height.saturating_sub(4));
        let (x, y) = match self.position {
            PopupPosition::Center => (
                (term.width.saturating_sub(w)) / 2,
                (term.height.saturating_sub(h)) / 2,
            ),
            PopupPosition::TopCenter(top_y) => ((term.width.saturating_sub(w)) / 2, top_y),
        };
        Rect::new(term.x.saturating_add(x), term.y.saturating_add(y), w, h).intersection(term)
    }

    pub(crate) fn widget_background(&self, f: &mut Frame, colors: &crate::app_theme::ThemeColors) -> Color {
        let area = self.area(f.area());
        widget_background(f, colors, area)
    }

    /// Render the popup chrome and return the inner area for content rendering.
    pub(crate) fn render(self, f: &mut Frame) -> Rect {
        let popup_area = self.area(f.area());

        f.render_widget(Clear, popup_area);
        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default()
                    .fg(self.title_color)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.border_color))
            .style(Style::default().bg(self.bg));
        let inner = block.inner(popup_area);
        f.render_widget(block, popup_area);
        inner
    }
}

// ---------------------------------------------------------------------------
// Helper: convert &str slices (with "─" separators) to DropdownItems
// ---------------------------------------------------------------------------

/// Build `DropdownItem` list from a slice of `&str` where `"─"` entries become
/// separator rows. This matches the existing convention used by explorer, source
/// control and hunk popup menus.
pub(crate) fn items_from_str_slice<'a>(labels: &'a [&'a str]) -> Vec<DropdownItem<'a>> {
    labels.iter().map(|l| DropdownItem::new(l)).collect()
}

/// Dropdown colors derived from the app theme.
pub(crate) fn themed_dropdown_colors(c: &crate::app_theme::ThemeColors) -> DropdownColors {
    DropdownColors {
        layers: c.ui.preserve_theme_colors.then_some(DropdownLayers {
            // defaultMenuStyles inherits editorWidget.border when menu.border
            // is unset. Explicit transparent borders still suppress it.
            normal: crate::theme_workbench::ControlColors {
                border: c.ui.menu_colors.border.or(c.ui.widget_border), ..c.ui.menu_colors
            }, selected: c.ui.menu_selection_colors,
            separator: c.ui.menu_separator, disabled: c.ui.disabled_foreground,
            shortcut: c.ui.menu_colors.foreground, slider: c.ui.scrollbar_slider,
            track: c.ui.scrollbar_background, fallback_surface: c.bg_editor,
        }),
        contrast_palette: c.ui.contrast_fallbacks(),
        bg: c.bg_menu,
        bg_selected: c.bg_menu_selected,
        fg: c.fg_menu,
        fg_selected: c.ui.fg_menu_selected,
        border_selected: c.ui.menu_selection_border.border(c.bg_menu_selected),
        fg_shortcut: c.fg_menu,
        fg_separator: c.ui.fg_menu_separator,
        fg_disabled: c.ui.fg_menu_disabled,
        border: c.ui.border_menu,
        scrollbar_fg: c.fg_dim,
        scrollbar_bg: c.bg_menu,
    }
}

/// Settings option lists belong to the setting's control, with dropdown/list
/// roles rather than the unrelated application-menu palette.
pub(crate) fn settings_dropdown_colors(c: &crate::app_theme::ThemeColors) -> DropdownColors {
    let control = c.ui.settings_dropdown_list;
    let bg = control.background.over(c.bg_editor).unwrap_or(c.bg_editor);
    let fg = c.ui.readable_foreground(control.foreground.over(bg).unwrap_or(c.fg_input), bg, 4.5);
    let selection = if c.ui.preserve_theme_colors { crate::theme_workbench::ControlColors {
        background: c.ui.quick_focus_background, foreground: c.ui.quick_focus_foreground, border: c.ui.active_contrast_border,
    } } else { crate::theme_workbench::ControlColors {
        background: c.ui.list_active_background, foreground: c.ui.list_active_foreground, border: c.ui.list_focus_outline,
    } };
    let bg_selected = selection.background.over(bg).unwrap_or(bg);
    let fg_selected = c.ui.readable_foreground(
        selection.foreground.over(bg_selected).unwrap_or(fg), bg_selected, 4.5,
    );
    let (scrollbar_bg, scrollbar_fg) = c.ui.scrollbar_colors(bg, false, false);
    DropdownColors {
        layers: c.ui.preserve_theme_colors.then_some(DropdownLayers {
            normal: control,
            selected: selection,
            separator: control.border, disabled: c.ui.disabled_foreground,
            shortcut: c.ui.description_foreground, slider: c.ui.scrollbar_slider,
            track: c.ui.scrollbar_background, fallback_surface: c.bg_editor,
        }),
        contrast_palette: c.ui.contrast_fallbacks(),
        bg,
        bg_selected,
        fg,
        fg_selected,
        border_selected: selection.border.border(bg_selected),
        fg_shortcut: c.ui.description_color(bg),
        fg_separator: control.border.border(bg).unwrap_or(bg),
        fg_disabled: c.ui.description_color(bg),
        border: control.border.border(bg).unwrap_or(bg),
        scrollbar_fg,
        scrollbar_bg,
    }
}

// ---------------------------------------------------------------------------
// InputField – reusable single-line text input with prefix, cursor, and suffix
// ---------------------------------------------------------------------------

/// A single-line text input widget with prefix, cursor, placeholder, and suffix support.
pub(crate) struct InputField<'a> {
    contrast_palette: Option<[Color; 14]>,
    preserve_theme_colors: bool,
    prefix: &'a str,
    prefix_color: Color,
    prefix_layer: Option<crate::theme_workbench::ColorLayer>,
    prefix_is_decoration: bool,
    text: &'a str,
    text_color: Color,
    foreground_layer: Option<crate::theme_workbench::ColorLayer>,
    cursor: &'a str,
    cursor_color: Color,
    cursor_layer: Option<crate::theme_workbench::ColorLayer>,
    cursor_byte_pos: Option<usize>,
    selection_byte_range: Option<(usize, usize)>,
    selection_color: Color,
    selection_bg: Color,
    selection_layers: Option<(crate::theme_workbench::ColorLayer, crate::theme_workbench::ColorLayer)>,
    suffix: &'a str,
    suffix_color: Color,
    placeholder: &'a str,
    placeholder_color: Color,
    placeholder_layer: Option<crate::theme_workbench::ColorLayer>,
    bg: Color,
}

impl<'a> InputField<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self {
            contrast_palette: None,
            preserve_theme_colors: false,
            prefix: "> ",
            prefix_color: Color::Reset,
            prefix_layer: None,
            prefix_is_decoration: false,
            text,
            text_color: Color::Reset,
            foreground_layer: None,
            cursor: "│",
            cursor_color: Color::Reset,
            cursor_layer: None,
            cursor_byte_pos: None,
            selection_byte_range: None,
            selection_color: Color::Reset,
            selection_bg: Color::Reset,
            selection_layers: None,
            suffix: "",
            suffix_color: Color::Reset,
            placeholder: "",
            placeholder_color: Color::Reset,
            placeholder_layer: None,
            bg: Color::Reset,
        }
    }

    pub(crate) fn prefix(mut self, prefix: &'a str, color: Color) -> Self {
        self.prefix = prefix;
        self.prefix_color = color;
        self.prefix_layer = None;
        self.prefix_is_decoration = false;
        self
    }

    /// A themed border marker is decoration, rather than an input text label.
    pub(crate) fn prefix_decoration(mut self, prefix: &'a str, color: Color) -> Self {
        self.prefix = prefix;
        self.prefix_color = color;
        self.prefix_layer = None;
        self.prefix_is_decoration = true;
        self
    }

    /// Initialize every field color from its input theme roles; subsequent
    /// builder options can override individual colors for a specialized field.
    pub(crate) fn theme_colors(self, colors: &crate::app_theme::ThemeColors) -> Self {
        self.theme_control(colors, colors.ui.input_colors, colors.bg_sidebar)
    }

    pub(crate) fn theme_control(mut self, colors: &crate::app_theme::ThemeColors,
        control: crate::theme_workbench::ControlColors, surface: Color) -> Self {
        self.contrast_palette = Some(colors.ui.contrast_palette);
        self.preserve_theme_colors = colors.ui.preserve_theme_colors;
        self.prefix_color = colors.fg_input;
        self.prefix_layer = Some(control.foreground);
        self.text_color = colors.fg_input;
        self.foreground_layer = Some(control.foreground);
        self.cursor_color = colors.fg_input;
        self.cursor_layer = Some(control.foreground);
        self.selection_color = colors.fg_input;
        self.selection_bg = colors.bg_input;
        self.selection_layers = Some((control.foreground, colors.ui.input_selection_background));
        self.suffix_color = colors.fg_dim;
        self.placeholder_color = colors.fg_input_placeholder;
        self.placeholder_layer = Some(colors.ui.input_placeholder);
        self.bg = control.background.over(surface).unwrap_or(surface);
        self
    }

    pub(crate) fn cursor_marker(mut self, cursor: &'a str) -> Self {
        self.cursor = cursor;
        self
    }

    pub(crate) fn cursor_pos(mut self, byte_pos: usize) -> Self {
        self.cursor_byte_pos = Some(byte_pos);
        self
    }

    pub(crate) fn selection_range(mut self, range: Option<(usize, usize)>) -> Self {
        self.selection_byte_range = range;
        self
    }

    pub(crate) fn suffix(mut self, suffix: &'a str, color: Color) -> Self {
        self.suffix = suffix;
        self.suffix_color = color;
        self
    }

    pub(crate) fn placeholder_text(mut self, text: &'a str) -> Self {
        self.placeholder = text;
        self
    }

    pub(crate) fn bg(mut self, c: Color) -> Self {
        self.bg = c;
        self
    }

    /// Render the input field into the given single-line area.
    pub(crate) fn render(self, f: &mut Frame, area: Rect) -> TextInputHitMap {
        let mut spans = Vec::new();

        let cursor_pos = self
            .cursor_byte_pos
            .map(|pos| crate::app::App::clamp_to_grapheme_boundary(self.text, pos));
        let cursor_visible = cursor_pos.is_some() && !self.cursor.is_empty();
        let cursor_needs_cell = cursor_visible
            && ((self.text.is_empty() && self.placeholder.is_empty())
                || (cursor_pos == Some(self.text.len()) && !self.text.is_empty()));
        let reserved = UnicodeWidthStr::width(self.prefix)
            + UnicodeWidthStr::width(self.suffix)
            + usize::from(cursor_needs_cell);
        let max_text_width = (area.width as usize).saturating_sub(reserved).max(1);
        let (view_start, view_end) = if UnicodeWidthStr::width(self.text) <= max_text_width {
            (0, self.text.len())
        } else {
            let cursor = cursor_pos.unwrap_or(0);
            let content_budget = max_text_width.saturating_sub(2).max(1);
            let left_target = content_budget / 2;
            let mut start = cursor;
            let mut used = 0usize;
            for (idx, ch) in self.text[..cursor].grapheme_indices(true).rev() {
                let width = ch.width();
                if used + width > left_target {
                    break;
                }
                start = idx;
                used += width;
            }
            let mut end = cursor;
            for (offset, ch) in self.text[cursor..].grapheme_indices(true) {
                let width = ch.width();
                if used + width > content_budget {
                    break;
                }
                end = cursor + offset + ch.len();
                used += width;
            }
            if end == self.text.len() && used < content_budget {
                for (idx, ch) in self.text[..start].grapheme_indices(true).rev() {
                    let width = ch.width();
                    if used + width > content_budget {
                        break;
                    }
                    start = idx;
                    used += width;
                }
            }
            (start, end)
        };

        let own_colors = [self.text_color, self.selection_color, self.cursor_color, self.bg, self.selection_bg];
        let palette = if self.preserve_theme_colors { &[] }
            else { self.contrast_palette.as_ref().map_or(own_colors.as_slice(), |palette| palette.as_slice()) };
        if !self.prefix.is_empty() {
            let prefix_color = self.prefix_layer.and_then(|color| color.over(self.bg)).unwrap_or(self.prefix_color);
            spans.push(Span::styled(
                self.prefix,
                Style::default().fg(if self.prefix_is_decoration { prefix_color }
                    else { crate::ui_colors::ensure_contrast(prefix_color, self.bg, 4.5, palette) }),
            ));
        }

        let text_color = self.foreground_layer.and_then(|color| color.over(self.bg)).unwrap_or(self.text_color);
        let text_color = crate::ui_colors::ensure_contrast(text_color, self.bg, 4.5, palette);
        let placeholder_color = self.placeholder_layer.and_then(|color| color.over(self.bg)).unwrap_or(self.placeholder_color);
        let placeholder_color = crate::ui_colors::ensure_contrast(placeholder_color, self.bg, 4.5, palette);
        let selection_style = if let Some((foreground, fill)) = self.selection_layers {
            crate::ui_colors::input_selection_from_layers(foreground, fill, self.bg, text_color, palette)
        } else { crate::ui_colors::input_selection_style(self.selection_color, self.selection_bg, self.bg, palette) };
        let cursor_color = self.cursor_layer.and_then(|color| color.over(self.bg)).unwrap_or(self.cursor_color);
        let cursor_style = crate::ui_colors::block_cursor_style(cursor_color, self.bg, self.bg, palette)
            .add_modifier(Modifier::BOLD);
        if self.text.is_empty() && !self.placeholder.is_empty() {
            if cursor_visible {
                let mut placeholder_chars = self.placeholder.chars();
                if let Some(first) = placeholder_chars.next() {
                    spans.push(Span::styled(first.to_string(), cursor_style));
                    let rest = placeholder_chars.collect::<String>();
                    if !rest.is_empty() {
                        spans.push(Span::styled(
                            rest,
                            Style::default().fg(placeholder_color),
                        ));
                    }
                }
            } else {
                spans.push(Span::styled(
                    self.placeholder,
                    Style::default().fg(placeholder_color),
                ));
            }
        } else if let Some(pos) = cursor_pos {
            let selection = self.selection_byte_range.map(|(start, end)| {
                let low = start.min(end);
                let high = start.max(end);
                (
                    crate::app::App::clamp_to_grapheme_boundary(self.text, low),
                    crate::app::App::clamp_to_grapheme_boundary(self.text, high),
                )
            });
            if view_start > 0 {
                spans.push(Span::styled(
                    "…",
                    Style::default().fg(placeholder_color),
                ));
            }
            let mut batch = String::new();
            let mut batch_style: Option<Style> = None;
            for (relative_idx, ch) in self.text[view_start..view_end].grapheme_indices(true) {
                let byte_idx = view_start + relative_idx;
                let selected =
                    selection.is_some_and(|(start, end)| byte_idx >= start && byte_idx < end);
                let style = if cursor_visible && byte_idx == pos {
                    cursor_style
                } else if selected {
                    selection_style
                } else {
                    Style::default().fg(text_color)
                };
                if !batch.is_empty() && batch_style != Some(style) {
                    spans.push(Span::styled(
                        std::mem::take(&mut batch),
                        batch_style.unwrap_or_default(),
                    ));
                }
                batch_style = Some(style);
                batch.push_str(ch);
            }
            if !batch.is_empty() {
                spans.push(Span::styled(batch, batch_style.unwrap_or_default()));
            }
            if cursor_visible && pos == self.text.len() {
                spans.push(Span::styled(" ", cursor_style));
            }
            if view_end < self.text.len() {
                spans.push(Span::styled(
                    "…",
                    Style::default().fg(placeholder_color),
                ));
            }
        } else {
            spans.push(Span::styled(
                self.text,
                Style::default().fg(text_color),
            ));
        }

        if !self.suffix.is_empty() {
            spans.push(Span::styled(
                self.suffix,
                Style::default().fg(crate::ui_colors::ensure_contrast(self.suffix_color, self.bg, 4.5, palette)),
            ));
        }

        // Build the pointer hit map from the same viewport and output order as
        // the spans above.  This keeps click-to-place correct when prefixes,
        // suffixes, Unicode characters, cursor cells, or ellipses are present.
        let mut column_to_byte = Vec::with_capacity(area.width as usize);
        push_hit_width(&mut column_to_byte, UnicodeWidthStr::width(self.prefix), 0);
        if self.text.is_empty() && !self.placeholder.is_empty() {
            push_hit_width(
                &mut column_to_byte,
                UnicodeWidthStr::width(self.placeholder),
                0,
            );
        } else if let Some(pos) = cursor_pos {
            if view_start > 0 {
                push_hit_width(&mut column_to_byte, 1, view_start);
            }
            for (relative_idx, ch) in self.text[view_start..view_end].grapheme_indices(true) {
                let byte_idx = view_start + relative_idx;
                push_hit_grapheme(&mut column_to_byte, ch, byte_idx);
            }
            if cursor_visible && pos == self.text.len() {
                push_hit_width(&mut column_to_byte, 1, pos);
            }
            if view_end < self.text.len() {
                push_hit_width(&mut column_to_byte, 1, view_end);
            }
        } else {
            for (byte_idx, ch) in self.text.grapheme_indices(true) {
                push_hit_grapheme(&mut column_to_byte, ch, byte_idx);
            }
        }
        push_hit_width(
            &mut column_to_byte,
            UnicodeWidthStr::width(self.suffix),
            self.text.len(),
        );

        let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(self.bg));
        f.render_widget(paragraph, area);
        TextInputHitMap::new(area, column_to_byte, self.text.len())
    }
}

fn push_hit_width(columns: &mut Vec<usize>, width: usize, byte: usize) {
    columns.extend(std::iter::repeat_n(byte, width));
}

fn push_hit_grapheme(columns: &mut Vec<usize>, ch: &str, byte: usize) {
    let width = ch.width();
    if width == 0 {
        return;
    }
    let end = byte + ch.len();
    for cell in 0..width {
        columns.push(if cell * 2 < width { byte } else { end });
    }
}

// ---------------------------------------------------------------------------
// Tooltip – smart-positioning popup anchored near a point (above/below)
// ---------------------------------------------------------------------------

/// A bordered tooltip popup that automatically positions itself above or below its anchor point.
pub(crate) struct Tooltip<'a> {
    lines: Vec<Line<'a>>,
    border_color: Color,
    bg: Color,
    title: Option<&'a str>,
    title_style: Style,
    max_width: u16,
    max_height: u16,
    prefer_below: bool,
    avoid_area: Option<Rect>,
    theme: Option<TooltipTheme>,
    layers: Vec<(usize, Option<usize>, crate::theme_workbench::ControlColors)>,
}

struct TooltipTheme {
    colors: crate::theme_workbench::ControlColors,
    palette: Option<[Color; 14]>,
    fallback: Color,
}

impl<'a> Tooltip<'a> {
    pub(crate) fn new(lines: Vec<Line<'a>>, border_color: Color, bg: Color) -> Self {
        Self {
            lines,
            border_color,
            bg,
            title: None,
            title_style: Style::default(),
            max_width: 64,
            max_height: 14,
            prefer_below: false,
            avoid_area: None,
            theme: None,
            layers: Vec::new(),
        }
    }

    pub(crate) fn themed(lines: Vec<Line<'a>>, control: crate::theme_workbench::ControlColors,
        colors: &crate::app_theme::ThemeColors) -> Self {
        let mut tooltip = Self::new(lines, Color::Reset, Color::Reset);
        tooltip.theme = Some(TooltipTheme { colors: control, palette: colors.ui.contrast_fallbacks(), fallback: colors.bg_editor });
        tooltip
    }

    pub(crate) fn hover(lines: Vec<Line<'a>>, colors: &crate::app_theme::ThemeColors) -> Self {
        Self::themed(lines, colors.ui.hover_colors, colors)
    }

    /// Keep semantic colors raw until the tooltip's final placement is known.
    pub(crate) fn span_foreground(mut self, line: usize, span: usize, foreground: crate::theme_workbench::ColorLayer) -> Self {
        self.layers.push((line, Some(span), crate::theme_workbench::ControlColors {
            foreground, ..Default::default()
        }));
        self
    }

    pub(crate) fn line_colors(mut self, line: usize, colors: crate::theme_workbench::ControlColors) -> Self {
        self.layers.push((line, None, colors));
        self
    }

    pub(crate) fn title(mut self, title: &'a str, style: Style) -> Self {
        self.title = Some(title);
        self.title_style = style;
        self
    }

    pub(crate) fn max_width(mut self, w: u16) -> Self {
        self.max_width = w;
        self
    }
    pub(crate) fn max_height(mut self, h: u16) -> Self {
        self.max_height = h;
        self
    }

    /// Prefer placing the tooltip below its anchor, falling back above only
    /// when the available terminal height makes that necessary.
    pub(crate) fn prefer_below(mut self) -> Self {
        self.prefer_below = true;
        self
    }

    /// Language help must not cover completion controls or documentation.
    pub(crate) fn avoid(mut self, area: Option<Rect>) -> Self {
        self.avoid_area = area;
        self
    }

    /// Render the tooltip near `(anchor_x, anchor_y)` using the configured
    /// placement preference and the opposite side as a fallback.
    pub(crate) fn render(mut self, f: &mut Frame, anchor_x: u16, anchor_y: u16) -> Rect {
        let area = f.area();
        let content_w = self
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum::<usize>()
            })
            .max()
            .map(len_as_u16).unwrap_or(20);
        let popup_width = content_w.saturating_add(4).min(self.max_width).min(area.width);
        let popup_height = (len_as_u16(self.lines.len()) + 2)
            .min(self.max_height)
            .min(area.height);

        let below_y = anchor_y.saturating_add(1);
        let below_fits = below_y.saturating_add(popup_height) <= area.bottom();
        let above_fits = anchor_y >= area.y.saturating_add(popup_height);
        let y = if self.prefer_below && below_fits {
            below_y
        } else if above_fits {
            anchor_y - popup_height
        } else if below_fits {
            below_y
        } else {
            area.bottom().saturating_sub(popup_height).max(area.y)
        };
        let x = anchor_x.max(area.x).min(area.right().saturating_sub(popup_width));
        let mut popup_area = Rect::new(x, y, popup_width, popup_height);
        if let Some(avoid) = self.avoid_area.filter(|avoid| !avoid.intersection(popup_area).is_empty()) {
            let alternative = [anchor_y.checked_sub(popup_height), Some(below_y)].into_iter().flatten()
                .map(|y| Rect::new(x, y, popup_width, popup_height))
                .find(|candidate| candidate.y >= area.y && candidate.bottom() <= area.bottom()
                    && candidate.intersection(avoid).is_empty());
            let Some(alternative) = alternative else { return Rect::default(); };
            popup_area = alternative;
        }

        let mut normal = Style::default().bg(self.bg);
        if let Some(theme) = &self.theme {
            let surface = surface_background(f, popup_area, theme.fallback);
            self.bg = theme.colors.background.over(surface).unwrap_or(surface);
            let foreground = theme.colors.foreground.over(self.bg).unwrap_or(surface);
            let foreground = crate::ui_colors::ensure_contrast(foreground, self.bg, 4.5, crate::ui_colors::contrast_colors(&theme.palette));
            normal = Style::default().fg(foreground).bg(self.bg);
            self.border_color = theme.colors.border.border(self.bg).unwrap_or(self.bg);
            for &(line_index, span_index, colors) in &self.layers {
                let Some(line) = self.lines.get_mut(line_index) else { continue; };
                let line_surface = line.style.bg.unwrap_or(self.bg);
                let target = match span_index {
                    Some(index) => {
                        let Some(span) = line.spans.get_mut(index) else { continue; };
                        &mut span.style
                    }
                    None => &mut line.style,
                };
                let surface = target.bg.unwrap_or(line_surface);
                let background = colors.background.over(surface).unwrap_or(surface);
                let foreground = colors.foreground.or(theme.colors.foreground).over(background).unwrap_or(foreground);
                let mut style = Style::default().bg(background)
                    .fg(crate::ui_colors::ensure_contrast(foreground, background, 4.5, crate::ui_colors::contrast_colors(&theme.palette)));
                if let Some(border) = colors.border.border(background) {
                    style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
                }
                *target = target.patch(style);
            }
        }

        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.border_color))
            .style(normal);

        if let Some(t) = self.title {
            block = block.title(Span::styled(format!(" {t} "), normal.patch(self.title_style)));
        }

        let inner = block.inner(popup_area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);
        f.render_widget(Paragraph::new(self.lines).style(normal), inner);
        popup_area
    }
}

// ---------------------------------------------------------------------------
// ToggleButton – inline button with active/hover state (search toggles, etc.)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) fn toggle_button_style(
    active: bool,
    hovered: bool,
    dim_fg: Color,
    accent: Color,
    selected_fg: Color,
    hover_bg: Color,
    palette: &[Color],
) -> Style {
    if active {
        Style::default().fg(crate::ui_colors::ensure_contrast(selected_fg, accent, 4.5, &palette)).bg(accent)
    } else if hovered {
        // Selection foreground belongs on the selected background, not the
        // hover surface (which is often very pale in light themes).
        Style::default().fg(crate::ui_colors::ensure_contrast(dim_fg, hover_bg, 4.5, &palette)).bg(hover_bg)
    } else {
        Style::default().fg(dim_fg)
    }
}

// ---------------------------------------------------------------------------
// CollapsibleHeader – section header with expand/collapse arrow
// ---------------------------------------------------------------------------

/// Builds a collapsible section header line with ▶/▼ arrow and label.
pub(crate) struct CollapsibleHeader<'a> {
    pub label: &'a str,
    pub collapsed: bool,
    pub count: Option<usize>,
    pub fg: Color,
    pub bg: Color,
}

impl<'a> CollapsibleHeader<'a> {
    pub(crate) fn new(label: &'a str, collapsed: bool) -> Self {
        Self {
            label,
            collapsed,
            count: None,
            fg: Color::Reset,
            bg: Color::Reset,
        }
    }

    pub(crate) fn count(mut self, n: usize) -> Self {
        self.count = Some(n);
        self
    }

    pub(crate) fn colors(mut self, fg: Color, bg: Color) -> Self {
        self.fg = fg;
        self.bg = bg;
        self
    }

    /// Build a Line with the arrow and label. Caller pushes to their lines Vec.
    pub(crate) fn to_line(&self) -> Line<'static> {
        let arrow = if self.collapsed { "▶" } else { "▼" };
        let style = Style::default().fg(self.fg).bg(self.bg);
        let mut spans = vec![
            Span::styled(format!(" {arrow} "), style.add_modifier(Modifier::BOLD)),
            Span::styled(self.label.to_string(), style.add_modifier(Modifier::BOLD)),
        ];
        if let Some(n) = self.count {
            spans.push(Span::styled(
                format!(" ({n})"),
                Style::default().fg(self.fg).bg(self.bg),
            ));
        }
        Line::from(spans)
    }
}

// ---------------------------------------------------------------------------
// VerticalScrollbar – reusable scrollbar with a reserved cell width
// ---------------------------------------------------------------------------

/// Draws a vertical scrollbar with thumb/track; one column unless widened.
pub(crate) struct VerticalScrollbar {
    pub x: u16,
    pub top_y: u16,
    pub height: u16,
    pub total_rows: usize,
    pub offset: usize,
    pub thumb_fg: Color,
    pub track_fg: Color,
    pub bg: Color,
    width: u16,
    thumb_symbol: char,
    track_symbol: char,
}

impl VerticalScrollbar {
    pub(crate) fn new(x: u16, top_y: u16, height: u16, total_rows: usize, offset: usize) -> Self {
        Self {
            x,
            top_y,
            height,
            total_rows,
            offset,
            thumb_fg: Color::Reset,
            track_fg: Color::Reset,
            bg: Color::Reset,
            width: 1,
            thumb_symbol: '█',
            track_symbol: '│',
        }
    }

    pub(crate) fn colors(mut self, thumb_fg: Color, track_fg: Color, bg: Color) -> Self {
        self.thumb_fg = thumb_fg;
        self.track_fg = track_fg;
        self.bg = bg;
        self
    }

    pub(crate) fn symbols(mut self, thumb: char, track: char) -> Self {
        self.thumb_symbol = thumb;
        self.track_symbol = track;
        self
    }

    pub(crate) fn width(mut self, width: u16) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn thumb(&self) -> (u16, u16) {
        let thumb_size =
            ((self.height as f64 / self.total_rows as f64) * self.height as f64).max(1.0) as u16;
        let max_offset = self.total_rows.saturating_sub(self.height as usize);
        let thumb_pos = if max_offset > 0 {
            ((self.offset as f64 / max_offset as f64)
                * (self.height.saturating_sub(thumb_size)) as f64) as u16
        } else {
            0
        };
        (thumb_pos, thumb_size)
    }

    pub(crate) fn render(&self, f: &mut Frame) {
        if self.total_rows <= self.height as usize || self.height == 0 {
            return;
        }
        let (thumb_pos, thumb_size) = self.thumb();
        let buf = f.buffer_mut();
        for i in 0..self.height {
            let sy = self.top_y + i;
            for x in self.x..self.x.saturating_add(self.width) {
                let Some(cell) = buf.cell_mut((x, sy)) else { continue; };
                if i >= thumb_pos && i < thumb_pos + thumb_size {
                    cell.set_char(self.thumb_symbol)
                        .set_fg(self.thumb_fg)
                        .set_bg(self.bg);
                } else {
                    cell.set_char(self.track_symbol)
                        .set_fg(self.track_fg)
                        .set_bg(self.bg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn test_colors() -> DropdownColors {
        DropdownColors {
            layers: None,
            contrast_palette: crate::app_theme::load_builtin_themes()[0].to_theme_colors().ui.contrast_fallbacks(),
            bg: Color::Black,
            bg_selected: Color::Blue,
            fg: Color::White,
            fg_selected: Color::Yellow,
            border_selected: None,
            fg_shortcut: Color::Gray,
            fg_separator: Color::DarkGray,
            fg_disabled: Color::DarkGray,
            border: Color::White,
            scrollbar_fg: Color::Gray,
            scrollbar_bg: Color::Black,
        }
    }

    // -----------------------------------------------------------------------
    // DropdownMenu – non-render helpers
    // -----------------------------------------------------------------------

    #[test]
    fn dropdown_menu_visual_row_count_no_separators() {
        let items = vec![
            DropdownItem::new("A"),
            DropdownItem::new("B"),
            DropdownItem::new("C"),
        ];
        let menu = DropdownMenu::new(&items, test_colors());
        assert_eq!(menu.visual_row_count(), 3);
    }

    #[test]
    fn dropdown_menu_visual_row_count_with_separators() {
        let items = vec![
            DropdownItem::new("Cut").separator(),
            DropdownItem::new("Copy").separator(),
            DropdownItem::new("Paste"),
        ];
        let menu = DropdownMenu::new(&items, test_colors());
        // 3 items + 2 separator rows = 5
        assert_eq!(menu.visual_row_count(), 5);
    }

    #[test]
    fn dropdown_menu_visual_row_count_empty() {
        let items: Vec<DropdownItem> = vec![];
        let menu = DropdownMenu::new(&items, test_colors());
        assert_eq!(menu.visual_row_count(), 0);
    }

    #[test]
    fn dropdown_menu_tiny_frames_and_stale_anchors_do_not_panic() {
        let items = [DropdownItem::new("Copy").separator(), DropdownItem::new("Paste")];
        for (width, height) in [(0, 0), (1, 1), (2, 2), (1, 40), (80, 1), (3, 3), (80, 24)] {
            for layered in [false, true] {
                let colors = if layered {
                    themed_dropdown_colors(&crate::app_theme::load_builtin_themes()[0].to_theme_colors())
                } else {
                    test_colors()
                };
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| {
                    let inner = DropdownMenu::new(&items, colors)
                        .anchor(u16::MAX, u16::MAX)
                        .render(frame);
                    assert_eq!(inner.intersection(frame.area()), inner);
                }).unwrap();
            }
        }
    }

    #[test]
    fn dropdown_menu_content_width_basic() {
        let items = vec![DropdownItem::new("Hello")];
        let menu = DropdownMenu::new(&items, test_colors());
        // leading space (1) + "Hello" (5) + trailing (1) = 7
        let w = menu.content_width();
        assert!(w >= 7, "content_width should be at least 7, got {w}");
    }

    #[test]
    fn dropdown_menu_content_width_with_shortcut() {
        let items = vec![DropdownItem::new("Save").shortcut("Ctrl+S")];
        let menu = DropdownMenu::new(&items, test_colors());
        let w = menu.content_width();
        // leading(1) + "Save"(4) + gap(2) + "Ctrl+S"(6) + trailing(1) = 14
        assert!(w >= 14, "content_width should be at least 14, got {w}");
    }

    // -----------------------------------------------------------------------
    // items_from_str_slice helper
    // -----------------------------------------------------------------------

    #[test]
    fn items_from_str_slice_converts_labels() {
        let labels = &["Open", "─", "Close"];
        let items = items_from_str_slice(labels);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "Open");
        assert_eq!(items[1].label, "─");
        assert_eq!(items[2].label, "Close");
    }

    // -----------------------------------------------------------------------
    // InputField
    // -----------------------------------------------------------------------

    #[test]
    fn input_field_default_prompt_and_text_render_with_input_theme_colors() {
        for (foreground, background) in [
            (Color::Rgb(190,230,210), Color::Rgb(20,32,28)),
            (Color::Rgb(24,36,48), Color::Rgb(240,244,248)),
        ] {
            let mut theme = crate::app_theme::load_builtin_themes().remove(0);
            for (id, color) in [("input.foreground", foreground), ("input.background", background)] {
                let Color::Rgb(r, g, b) = color else { unreachable!() };
                theme.workbench_colors.insert(id.into(), format!("#{r:02x}{g:02x}{b:02x}").into());
            }
            let colors = theme.to_theme_colors();
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(16,1)).unwrap();
            terminal.draw(|frame| {
                InputField::new("query").theme_colors(&colors).render(frame, Rect::new(0,0,16,1));
            }).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0,0)].symbol(), ">");
            assert_eq!(buffer[(2,0)].symbol(), "q");
            for x in [0,2,3,4,5,6] {
                assert_eq!(buffer[(x,0)].fg, foreground, "adornment or text bypassed the input theme");
                assert_eq!(buffer[(x,0)].bg, background);
            }
        }
    }

    #[test]
    fn input_field_builder_chain() {
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let field = InputField::new("search text")
            .theme_colors(&colors)
            .prefix("🔍 ", Color::Yellow)
            .cursor_marker("|")
            .cursor_pos(6)
            .selection_range(Some((1, 6)))
            .suffix(" (3 results)", Color::Gray)
            .placeholder_text("Type to search...")
            .bg(Color::Black);

        assert_eq!(field.prefix, "🔍 ");
        assert_eq!(field.prefix_color, Color::Yellow);
        assert_eq!(field.text_color, colors.fg_input);
        assert_eq!(field.cursor, "|");
        assert_eq!(field.cursor_color, colors.fg_input);
        assert_eq!(field.cursor_byte_pos, Some(6));
        assert_eq!(field.selection_byte_range, Some((1, 6)));
        assert_eq!(field.selection_bg, colors.bg_input);
        assert_eq!(field.suffix, " (3 results)");
        assert_eq!(field.suffix_color, Color::Gray);
        assert_eq!(field.placeholder, "Type to search...");
        assert_eq!(field.placeholder_color, colors.fg_input_placeholder);
        assert_eq!(field.bg, Color::Black);
    }

    #[test]
    fn input_field_hit_map_matches_prefix_cursor_and_wide_characters() {
        let mut terminal = Terminal::new(TestBackend::new(12, 2)).unwrap();
        let mut hit_map = None;
        terminal
            .draw(|frame| {
                hit_map = Some(
                    InputField::new("a界b")
                        .prefix("> ", Color::Blue)
                        .cursor_marker("│")
                        .cursor_pos("a界b".len())
                        .render(frame, Rect::new(0, 0, 12, 1)),
                );
            })
            .unwrap();
        let hit_map = hit_map.unwrap();

        assert_eq!(hit_map.byte_at(0, 0), 0); // prefix
        assert_eq!(hit_map.byte_at(2, 0), 0); // before `a`
        assert_eq!(hit_map.byte_at(3, 0), 1); // first cell of `界`
        assert_eq!(hit_map.byte_at(4, 0), 4); // second cell of `界`
        assert_eq!(hit_map.byte_at(5, 0), 4); // before `b`
        assert_eq!(hit_map.byte_at(6, 0), 5); // cursor at end
    }

    #[test]
    fn input_field_viewport_and_mouse_targets_keep_combining_and_emoji_clusters_whole() {
        let text = "ab e\u{301} 👩‍💻 xyz";
        for width in [7, 30] {
            let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
            let mut hit_map = None;
            terminal.draw(|frame| {
                hit_map = Some(InputField::new(text)
                    .prefix("", Color::White)
                    .cursor_marker("│")
                    .cursor_pos(text.find(" xyz").unwrap())
                    .render(frame, Rect::new(0, 0, width, 1)));
            }).unwrap();
            let hit_map = hit_map.unwrap();
            for x in 0..width {
                let byte = hit_map.byte_at(x, 0);
                assert_eq!(byte, crate::app::App::clamp_to_grapheme_boundary(text, byte));
            }
            let buffer = terminal.backend().buffer();
            assert!((0..width).any(|x| buffer.cell((x, 0)).is_some_and(|cell| cell.symbol() == "👩‍💻")),
                "width={width}, cells={:?}", (0..width).map(|x| buffer.cell((x, 0)).unwrap().symbol()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn single_line_input_hit_map_does_not_capture_rows_below_it() {
        let mut terminal = Terminal::new(TestBackend::new(12, 3)).unwrap();
        let mut hit_map = None;
        terminal
            .draw(|frame| {
                hit_map = Some(
                    InputField::new("value")
                        .cursor_pos(5)
                        .render(frame, Rect::new(0, 0, 12, 3)),
                );
            })
            .unwrap();
        let hit_map = hit_map.unwrap();
        assert!(hit_map.contains(1, 0));
        assert!(!hit_map.contains(1, 1));
    }

    #[test]
    fn input_field_block_cursor_overlays_text_without_shifting_it() {
        let mut theme = crate::app_theme::load_builtin_themes().remove(0);
        theme.workbench_colors.insert("input.foreground".into(), "#ffffff".into());
        theme.workbench_colors.insert("input.background".into(), "#000000".into());
        let colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(8, 1)).unwrap();
        let mut hit_map = None;
        terminal
            .draw(|frame| {
                hit_map = Some(
                    InputField::new("abcd")
                        .theme_colors(&colors)
                        .prefix("", Color::Reset)
                        .cursor_marker("│")
                        .cursor_pos(2)
                        .render(frame, Rect::new(0, 0, 8, 1)),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..4)
            .filter_map(|x| buffer.cell((x, 0)))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert_eq!(rendered, "abcd");
        assert_eq!(buffer.cell((2, 0)).map(|cell| cell.bg), Some(Color::Rgb(255, 255, 255)));
        assert_eq!(hit_map.unwrap().byte_at(2, 0), 2);
    }

    // -----------------------------------------------------------------------
    // toggle_button_style
    // -----------------------------------------------------------------------

    #[test]
    fn toggle_button_style_active() {
        let s = toggle_button_style(
            true,
            false,
            Color::Gray,
            Color::Blue,
            Color::White,
            Color::DarkGray, &[Color::White, Color::Black]);
        assert_eq!(s.fg.unwrap(), Color::White);
        assert_eq!(s.bg.unwrap(), Color::Blue);
    }

    #[test]
    fn toggle_button_style_hovered() {
        let s = toggle_button_style(
            false,
            true,
            Color::Gray,
            Color::Blue,
            Color::White,
            Color::DarkGray, &[Color::White, Color::Black]);
        assert_eq!(s.fg.unwrap(), Color::Gray);
        assert_eq!(s.bg.unwrap(), Color::DarkGray);
    }

    #[test]
    fn toggle_button_style_default() {
        let s = toggle_button_style(
            false,
            false,
            Color::Gray,
            Color::Blue,
            Color::White,
            Color::DarkGray, &[Color::White, Color::Black]);
        assert_eq!(s.fg.unwrap(), Color::Gray);
        assert!(s.bg.is_none());
    }

    #[test]
    fn collapsible_header_collapsed() {
        let header = CollapsibleHeader::new("Staged", true)
            .count(3)
            .colors(Color::White, Color::Black);
        let line = header.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("▶"));
        assert!(text.contains("Staged"));
        assert!(text.contains("(3)"));
    }

    #[test]
    fn collapsible_header_expanded() {
        let header = CollapsibleHeader::new("Changes", false);
        let line = header.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("▼"));
        assert!(text.contains("Changes"));
    }

    #[test]
    fn collapsible_header_no_count() {
        let header = CollapsibleHeader::new("GRAPH", true);
        let line = header.to_line();
        assert_eq!(line.spans.len(), 2); // arrow + label, no count span
    }

}
