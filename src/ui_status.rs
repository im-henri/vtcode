//! Quiet, responsive workbench status and an on-demand file-details surface.
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, TabKind},
    editor::Editor,
    ui_performance::clip_middle,
};

pub(crate) const HOVER_DELAY: Duration = Duration::from_millis(500);
const SAVED_LIFETIME: Duration = Duration::from_millis(1500);

struct ItemDefinition {
    id: &'static str,
    label: &'static str,
    pinned: bool,
}

const ITEMS: &[ItemDefinition] = &[
    ItemDefinition {
        id: "git_branch",
        label: "Git branch",
        pinned: true,
    },
    ItemDefinition {
        id: "problems",
        label: "Problems",
        pinned: true,
    },
    ItemDefinition {
        id: "goto_line",
        label: "Cursor position",
        pinned: true,
    },
    ItemDefinition {
        id: "language",
        label: "Language",
        pinned: true,
    },
    ItemDefinition {
        id: "tab_size",
        label: "Indentation",
        pinned: false,
    },
    ItemDefinition {
        id: "encoding",
        label: "Encoding",
        pinned: false,
    },
    ItemDefinition {
        id: "line_ending",
        label: "Line endings",
        pinned: false,
    },
    ItemDefinition {
        id: "word_wrap",
        label: "Word wrap",
        pinned: false,
    },
    ItemDefinition {
        id: "file_size",
        label: "File size",
        pinned: false,
    },
    ItemDefinition {
        id: "line_count",
        label: "Lines",
        pinned: false,
    },
    ItemDefinition {
        id: "scroll",
        label: "Scroll position",
        pinned: false,
    },
    ItemDefinition {
        id: "language_server",
        label: "Language server",
        pinned: false,
    },
    ItemDefinition {
        id: "completion_index",
        label: "VTComplete file learning",
        pinned: true,
    },
    ItemDefinition {
        id: "performance",
        label: "Performance",
        pinned: true,
    },
    ItemDefinition {
        id: "read_only",
        label: "Read-only",
        pinned: true,
    },
    ItemDefinition {
        id: "overwrite_mode",
        label: "Overwrite mode",
        pinned: true,
    },
    ItemDefinition {
        id: "selection",
        label: "Selection",
        pinned: true,
    },
    ItemDefinition {
        id: "cursors",
        label: "Multiple cursors",
        pinned: true,
    },
    ItemDefinition {
        id: "word_count",
        label: "Word count",
        pinned: false,
    },
    ItemDefinition {
        id: "word_occ",
        label: "Word occurrences",
        pinned: false,
    },
    ItemDefinition {
        id: "search_results",
        label: "Search results",
        pinned: false,
    },
    ItemDefinition {
        id: "macro",
        label: "Macro recording",
        pinned: true,
    },
    ItemDefinition {
        id: "saved",
        label: "Save confirmation",
        pinned: true,
    },
];

fn is_pinned(app: &App, id: &str) -> bool {
    app.status_bar_pins.get(id).copied().unwrap_or_else(|| {
        // Keep the existing word-count setting useful until a placement is
        // chosen explicitly in the new customization surface.
        if id == "word_count" {
            app.show_word_count
        } else {
            ITEMS
                .iter()
                .find(|item| item.id == id)
                .is_some_and(|item| item.pinned)
        }
    })
}

/// The exact count scans the whole document. Customization lists placement
/// choices, not counts, and must not turn hidden analysis back on.
pub(crate) fn word_occurrences_needed(app: &App) -> bool {
    (app.show_status_bar && !app.zen_mode && is_pinned(app, "word_occ"))
        || (app.status_details.open && !app.status_details.customize && !obscured(app))
}

#[derive(Default)]
pub(crate) struct DetailsState {
    pub open: bool,
    pub customize: bool,
    pub selected: usize,
    pub area: Rect,
    pub close: Rect,
    pub customize_button: Rect,
    pub rows: Vec<(Rect, usize)>,
    scrollbar: Rect,
    scrollbar_drag: bool,
    scroll: usize,
    follow_selection: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverTarget {
    Status(&'static str),
    Activity(usize),
    Panel(u8),
}

#[derive(Default)]
pub(crate) struct HoverState {
    target: Option<HoverTarget>,
    pub started: Option<Instant>,
    pub shown: bool,
}

pub(crate) fn hover_due(app: &App) -> bool {
    !app.chrome_hover.shown
        && app
            .chrome_hover
            .started
            .is_some_and(|start| start.elapsed() >= HOVER_DELAY)
}

fn editor(app: &App) -> Option<&Editor> {
    app.tabs
        .get(app.active_tab)
        .filter(|tab| {
            matches!(
                tab.kind,
                TabKind::File | TabKind::Diff(_) | TabKind::GitRevision
            )
        })
        .map(|tab| &tab.editor)
}

fn obscured(app: &App) -> bool {
    app.completion_index_ui.open
        || app.change_dir_active
        || app.context_menu_active
        || app.menu_open
        || app.tab_context_active
        || app.explorer_context_active
        || app.sc_context_active
        || app.shortcuts_active
        || app.settings_active
        || app.hunk_popup_active
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
}

pub(crate) fn open_details(app: &mut App) {
    app.close_palette();
    app.clear_drag_states();
    crate::ui_performance_details::release_focus(app);
    app.reset_hover_states();
    app.status_details = DetailsState {
        open: true,
        ..DetailsState::default()
    };
}

pub(crate) fn open_customization(app: &mut App, item: Option<&str>) {
    open_details(app);
    app.status_details.customize = true;
    app.status_details.selected = item
        .and_then(|id| ITEMS.iter().position(|item| item.id == id))
        .unwrap_or_default();
    app.status_details.follow_selection = true;
}

fn toggle_customization(app: &mut App) {
    app.status_details.customize = !app.status_details.customize;
    app.status_details.selected = 0;
    app.status_details.scroll = 0;
    app.status_details.scrollbar_drag = false;
    app.status_details.follow_selection = true;
}

fn toggle_pin(app: &mut App, index: usize) {
    if let Some(item) = ITEMS.get(index) {
        let pinned = !is_pinned(app, item.id);
        app.set_status_bar_item_pinned(item.id, pinned);
    }
}

fn scroll_details_to_pointer(app: &mut App, y: u16) {
    let scrollbar = app.status_details.scrollbar;
    let max_scroll = detail_rows(app)
        .len()
        .saturating_sub(scrollbar.height as usize);
    let offset = y
        .saturating_sub(scrollbar.y)
        .min(scrollbar.height.saturating_sub(1));
    app.status_details.scroll =
        max_scroll * offset as usize / usize::from(scrollbar.height.saturating_sub(1).max(1));
    app.status_details.follow_selection = false;
}

fn dismiss(app: &mut App) {
    app.status_details = DetailsState::default();
    app.chrome_hover = HoverState::default();
}

/// Explicit action IDs keep translations and abbreviated labels independent of
/// pointer behavior. Only actions present in the final layout get hit regions.
pub(crate) fn activate(app: &mut App, action: &str) {
    match action {
        "file_details" => {
            if app.status_details.open {
                dismiss(app);
            } else {
                open_details(app);
            }
        }
        "git_branch" => {
            dismiss(app);
            app.execute_palette_command("source_control");
        }
        "problems" => {
            dismiss(app);
            app.panel_tab = 1;
            app.show_panel = true;
            app.terminal_panel_focused = false;
        }
        "goto_line" | "selection" | "scroll" => {
            dismiss(app);
            app.open_goto_line();
        }
        "word_wrap" => {
            app.execute_palette_command("toggle_word_wrap");
        }
        "read_only" => {
            // Historical and diff buffers remain immutable.
            if app
                .tabs
                .get(app.active_tab)
                .is_some_and(|tab| tab.kind == TabKind::File)
                && editor(app).is_some_and(|editor| editor.encoding != "Binary")
            {
                app.execute_palette_command("toggle_read_only");
            }
        }
        "overwrite_mode" => {
            if let Some(editor) = app.editor_mut() {
                editor.overwrite_mode = false;
            }
        }
        "macro" => {
            app.execute_palette_command("macro_stop");
        }
        "performance" => {
            dismiss(app);
            crate::ui_performance_details::toggle(app);
        }
        "language_server" => {
            dismiss(app);
            app.open_lsp_settings_page();
        }
        "completion_index" => {
            dismiss(app);
            crate::ui_vtcomplete_index::open(app);
        }
        "tab_size" | "encoding" | "line_ending" | "language" => {
            dismiss(app);
            app.open_palette();
            app.palette.query = match action {
                "tab_size" => "Indentation",
                "encoding" => "Encoding",
                "line_ending" => "Line Ending",
                _ => "Language: ",
            }
            .into();
            app.palette.cursor = app.palette.query.len();
            app.update_palette_filter();
        }
        _ => {}
    }
}

struct StatusItem {
    text: String,
    compact: Option<String>,
    action: &'static str,
    color: Color,
    priority: u8,
    right: bool,
}

impl StatusItem {
    fn new(
        text: impl Into<String>,
        action: &'static str,
        color: Color,
        priority: u8,
        right: bool,
    ) -> Self {
        Self {
            text: text.into(),
            compact: None,
            action,
            color,
            priority,
            right,
        }
    }
    fn compact(mut self, text: impl Into<String>) -> Self {
        self.compact = Some(text.into());
        self
    }
    fn width(&self) -> usize {
        UnicodeWidthStr::width(self.text.as_str()) + 2
    }
    fn minimum_width(&self) -> usize {
        self.compact.as_deref().map_or(self.width(), |text| {
            (UnicodeWidthStr::width(text) + 2).min(self.width())
        })
    }
}

fn completion_status_item(
    status: &crate::completion_index::IndexStatus,
    foreground: Color,
) -> StatusItem {
    // Editor accents can equal statusBar.background in imported themes. This
    // is a status-bar control in every phase, so use its foreground role; the
    // label communicates activity, pause and limits without relying on color.
    StatusItem::new(
        crate::ui_vtcomplete_index::status_label(status),
        "completion_index",
        foreground,
        4,
        false,
    )
    .compact("VTComplete")
}

fn cache_selection(app: &mut App) {
    let selection = editor(app).and_then(|editor| {
        let (start, end, _, _) = editor.clamped_selection_indices()?;
        let slice = editor.rope.slice(start..end);
        Some((slice.len_chars(), slice.len_lines().max(1)))
    });
    if let Some((chars, lines)) = selection {
        // Cache the displayed counts, which remain valid across tab switches
        // and reloads even when document versions and selection offsets match.
        if app.cached_sel_stats_key != selection || app.cached_sel_stats.is_none() {
            app.cached_sel_stats = Some(if lines > 1 {
                format!("({chars} selected, {lines} lines)")
            } else {
                format!("({chars} selected)")
            });
            app.cached_sel_stats_key = selection;
        }
    } else {
        app.cached_sel_stats = None;
        app.cached_sel_stats_key = None;
    }
}

pub(crate) fn draw_bar(f: &mut Frame, app: &mut App, area: Rect) {
    app.status_bar_items.clear();
    if area.is_empty() {
        return;
    }
    cache_selection(app);
    let area = Rect::new(area.x, area.y, area.width, 1);
    let bg = app.colors.bg_statusbar;
    let fg = app
        .colors
        .ui
        .readable_foreground(app.colors.fg_statusbar, bg, 4.5);
    f.render_widget(Block::default().style(Style::default().fg(fg).bg(bg)), area);

    let performance_pinned = is_pinned(app, "performance");
    let details_width = if performance_pinned {
        if area.width >= 6 { 3 } else { 0 }
    } else {
        area.width.min(3)
    };
    let badge = (performance_pinned && area.width > details_width).then(|| {
        crate::ui_performance::status_badge(
            app,
            usize::from(area.width - details_width),
            Instant::now(),
        )
    });
    let badge_width = badge.as_ref().map_or(0, |badge| badge.width);
    let badge_area = Rect::new(area.right() - badge_width, area.y, badge_width, 1);
    let details_area = Rect::new(badge_area.x - details_width, area.y, details_width, 1);
    let budget = usize::from(area.width - badge_width - details_width);
    let mut items = Vec::with_capacity(ITEMS.len());
    let file_activity = editor(app).and_then(|editor| crate::ui_loading::editor_label(app, editor));

    if is_pinned(app, "git_branch") && !app.cached_git_branch.is_empty() {
        let branch_width = (budget / 3).clamp(5, 24);
        let branch = format!(
            "⑂ {}",
            clip_middle(&app.cached_git_branch, branch_width.saturating_sub(2))
        );
        items.push(StatusItem::new(branch, "git_branch", fg, 7, false));
    }
    let errors = app.cached_error_count;
    let warnings = app.cached_warning_count;
    let (diagnostics, color) = match (errors, warnings) {
        (0, 0) => ("✓".into(), fg),
        (0, warnings) => (format!("⚠ {warnings}"), app.colors.fg_warning),
        (errors, 0) => (format!("⊘ {errors}"), app.colors.fg_error),
        (errors, warnings) => (format!("⊘ {errors}  ⚠ {warnings}"), app.colors.fg_error),
    };
    items.push(
        StatusItem::new(
            diagnostics,
            "problems",
            color,
            if errors + warnings > 0 { 1 } else { 8 },
            false,
        )
        .compact(if errors + warnings > 0 {
            format!("! {}", errors + warnings)
        } else {
            "✓".into()
        }),
    );
    if app
        .saving_flash
        .is_some_and(|start| start.elapsed() < SAVED_LIFETIME)
        && file_activity.is_none()
    {
        items.push(StatusItem::new(
            "✓ Saved",
            "saved",
            app.colors.fg_success,
            9,
            false,
        ));
    } else {
        app.saving_flash = None;
    }

    if app.macro_recording {
        items.push(StatusItem::new("● REC", "macro", app.colors.fg_error, 0, true).compact("REC"));
    }
    if app.is_formatting() {
        items.push(StatusItem::new("Formatting…", "formatting", app.colors.fg_accent, 0, true));
    } else if let Some((message, _)) = &app.formatting.feedback {
        items.push(StatusItem::new(message, "formatting", app.colors.fg_success, 0, true));
    }
    if let Some(label) = file_activity {
        items.push(StatusItem::new(label, "file_details", app.colors.fg_accent, 0, false));
    }
    if is_pinned(app, "completion_index") {
        let status = app.completion_index.status();
        items.push(completion_status_item(&status, fg));
    }
    if let Some(editor) = editor(app) {
        if editor.read_only {
            items.push(
                StatusItem::new("Read-only", "read_only", app.colors.fg_warning, 0, true)
                    .compact("RO"),
            );
        }
        if editor.overwrite_mode {
            items.push(StatusItem::new(
                "OVR",
                "overwrite_mode",
                app.colors.fg_warning,
                0,
                true,
            ));
        }
        if !editor.extra_cursors.is_empty() {
            items.push(StatusItem::new(
                format!("{} cursors", editor.extra_cursors.len() + 1),
                "cursors",
                fg,
                3,
                true,
            ));
        } else if let Some(selection) = &app.cached_sel_stats {
            items.push(
                StatusItem::new(selection.trim_matches(['(', ')']), "selection", fg, 3, true)
                    .compact(
                        selection
                            .trim_matches(['(', ')'])
                            .split(',')
                            .next()
                            .unwrap_or_default(),
                    ),
            );
        }
        if is_pinned(app, "word_count")
            && let Some(count) = &app.cached_word_count
        {
            items.push(StatusItem::new(count, "word_count", fg, 6, true));
        }
        if is_pinned(app, "word_occ")
            && let Some(occurrences) = &app.cached_word_occ
        {
            items.push(StatusItem::new(occurrences, "word_occ", fg, 6, true));
        }
        if is_pinned(app, "search_results")
            && let Some(results) = search_results(app)
        {
            items.push(StatusItem::new(results, "search_results", fg, 4, true));
        }
        items.push(
            StatusItem::new(
                format!(
                    "Ln {}, Col {}",
                    editor.cursor_line + 1,
                    editor.cursor_col + 1
                ),
                "goto_line",
                fg,
                2,
                true,
            )
            .compact(format!(
                "{}:{}",
                editor.cursor_line + 1,
                editor.cursor_col + 1
            )),
        );
        items.push(StatusItem::new(
            editor.language_name(),
            "language",
            fg,
            5,
            true,
        ));
    }

    let metadata_ids = [
        "tab_size",
        "encoding",
        "line_ending",
        "word_wrap",
        "file_size",
        "line_count",
        "scroll",
        "language_server",
    ];
    for row in metadata_ids
        .into_iter()
        .filter(|id| is_pinned(app, id))
        .filter_map(|id| file_detail_row(app, id))
    {
        let text = match row.id {
            "word_wrap" => format!("Wrap: {}", row.value),
            "line_count" => format!("{} lines", row.value),
            "language_server" => match row.value.as_str() {
                "Connected" => "LSP ✓",
                "Starting…" => "LSP …",
                "Failed to start" => "LSP !",
                _ => "LSP —",
            }.into(),
            _ => row.value,
        };
        items.push(StatusItem::new(text, row.id, fg, 6, true));
    }

    let mut candidates: Vec<usize> = (0..items.len())
        .filter(|&index| matches!(items[index].action, "formatting" | "file_details")
            || is_pinned(app, items[index].action))
        .collect();
    candidates.sort_by_key(|&index| items[index].priority);
    let mut visible = vec![false; items.len()];
    let mut remaining = budget;
    // Fit compact controls first. Giving an early item its full label before
    // considering the rest made pinned controls disappear as selection counts
    // or cursor positions grew, even when their compact labels would all fit.
    for &index in &candidates {
        let width = items[index].minimum_width();
        if width <= remaining {
            visible[index] = true;
            remaining -= width;
        }
    }
    // Spend only spare space on full labels, in the same priority order.
    for index in candidates {
        if !visible[index] {
            continue;
        }
        let item = &mut items[index];
        let extra = item.width() - item.minimum_width();
        if extra <= remaining {
            remaining -= extra;
        } else if let Some(compact) = item.compact.take() {
            item.text = compact;
        }
    }
    let right_width: usize = items
        .iter()
        .enumerate()
        .filter(|(index, item)| visible[*index] && item.right)
        .map(|(_, item)| item.width())
        .sum();
    let mut left_x = area.x;
    let mut right_x = details_area.x.saturating_sub(right_width as u16);
    for (index, item) in items.iter().enumerate() {
        if !visible[index] {
            continue;
        }
        let x = if item.right {
            &mut right_x
        } else {
            &mut left_x
        };
        let item_area = Rect::new(*x, area.y, item.width() as u16, 1);
        draw_item(f, app, item_area, &item.text, item.action, item.color);
        *x += item_area.width;
    }
    if details_width > 0 {
        draw_item(f, app, details_area, "…", "file_details", fg);
    }
    if let Some(badge) = badge {
        app.status_bar_items
            .push((badge_area.x, badge_area.width, "performance"));
        crate::ui_performance::draw_badge(f, app, badge_area, badge);
    }

    let gradient = &app.colors.gradients.bg_statusbar;
    if gradient.start != gradient.end {
        let positions =
            crate::app_theme::GradientPrecomputed::new(area.width, 1, gradient.angle_deg);
        for x in area.x..area.right() {
            if let Some(cell) = f.buffer_mut().cell_mut((x, area.y))
                && cell.bg == bg
            {
                cell.set_bg(crate::app_theme::lerp_color(
                    gradient.start,
                    gradient.end,
                    positions.t_at(x - area.x, 0),
                ));
            }
        }
        crate::ui_colors::ensure_control_row_contrast(f.buffer_mut(), area, &app.colors.ui);
    }
}

fn draw_item(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    text: &str,
    action: &'static str,
    fg: Color,
) {
    let index = app.status_bar_items.len();
    let normal_bg = app.colors.bg_statusbar;
    let hovered = app.hovered_status_item == Some(index)
        || (action == "file_details" && app.status_details.open);
    let bg = if hovered {
        if app.colors.bg_statusbar_item == normal_bg && !app.colors.ui.preserve_theme_colors {
            crate::app_theme::lerp_color(normal_bg, fg, 0.12)
        } else {
            app.colors.bg_statusbar_item
        }
    } else {
        normal_bg
    };
    let fg = if hovered {
        app.colors.ui.fg_statusbar_hover.unwrap_or(fg)
    } else {
        fg
    };
    let style = Style::default()
        .fg(app.colors.ui.readable_foreground(fg, bg, 4.5))
        .bg(bg);
    let label = if area.width < 3 {
        text.to_string()
    } else {
        format!(" {text} ")
    };
    f.render_widget(Paragraph::new(label).style(style), area);
    app.status_bar_items.push((area.x, area.width, action));
}

struct DetailRow {
    id: &'static str,
    label: &'static str,
    value: String,
    action: Option<&'static str>,
}

impl DetailRow {
    fn new(id: &'static str, value: impl Into<String>, action: Option<&'static str>) -> Self {
        Self {
            id,
            label: ITEMS
                .iter()
                .find(|item| item.id == id)
                .map_or(id, |item| item.label),
            value: value.into(),
            action,
        }
    }
}

const FILE_DETAIL_IDS: &[&str] = &[
    "language",
    "tab_size",
    "encoding",
    "line_ending",
    "word_wrap",
    "read_only",
    "file_size",
    "line_count",
    "scroll",
    "language_server",
];

fn file_detail_rows(app: &App) -> Vec<DetailRow> {
    FILE_DETAIL_IDS
        .iter()
        .filter_map(|&id| file_detail_row(app, id))
        .collect()
}

/// Build only requested metadata. Pinning one secondary field must not format
/// every field or inspect every language-server client on each redraw.
fn file_detail_row(app: &App, id: &'static str) -> Option<DetailRow> {
    let editor = editor(app)?;
    let (value, action) = match id {
        "language" => (editor.language_name().to_string(), Some("language")),
        "tab_size" => (
            format!(
                "{}: {}",
                if editor.use_tabs {
                    "Tab size"
                } else {
                    "Spaces"
                },
                editor.tab_size
            ),
            Some("tab_size"),
        ),
        "encoding" => (editor.encoding.into(), Some("encoding")),
        "line_ending" => (editor.cached_line_ending.into(), Some("line_ending")),
        "word_wrap" => (
            if editor.word_wrap { "On" } else { "Off" }.into(),
            Some("word_wrap"),
        ),
        "read_only" => (
            if editor.read_only { "On" } else { "Off" }.into(),
            (app.tabs[app.active_tab].kind == TabKind::File && editor.encoding != "Binary")
                .then_some("read_only"),
        ),
        "file_size" => {
            let bytes = editor.rope.len_bytes();
            (
                if bytes < 1024 {
                    format!("{bytes} B")
                } else if bytes < 1024 * 1024 {
                    format!("{:.1} KB", bytes as f64 / 1024.0)
                } else {
                    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
                },
                None,
            )
        }
        "line_count" => (editor.line_count().to_string(), None),
        "scroll" => {
            let last_line = editor.line_count().saturating_sub(1);
            (
                if editor.cursor_line == 0 {
                    "Top".into()
                } else if editor.cursor_line >= last_line {
                    "Bottom".into()
                } else {
                    format!(
                        "{}%",
                        editor.cursor_line.saturating_mul(100) / last_line.max(1)
                    )
                },
                Some("scroll"),
            )
        }
        "language_server" => {
            (
                if !app.lsp_enabled {
                    "Disabled"
                } else {
                    editor.file_path.as_deref().map_or("Save a file to connect", |path| app.lsp_manager.file_status(path))
                }
                .into(),
                Some("language_server"),
            )
        }
        _ => return None,
    };
    Some(DetailRow::new(id, value, action))
}

fn search_results(app: &App) -> Option<String> {
    if !app.search_active || app.search_query.is_empty() {
        return None;
    }
    Some(if app.search_error.is_some() {
        "Invalid regex".into()
    } else if app.bg_file_search_running || app.file_search_refresh_pending {
        "Searching…".into()
    } else if app.search_matches.is_empty() {
        "No results".into()
    } else {
        format!("{} of {}", app.search_current + 1, app.search_matches.len())
    })
}

fn all_detail_rows(app: &App) -> Vec<DetailRow> {
    let mut rows = file_detail_rows(app);
    let index = app.completion_index.status();
    rows.push(DetailRow::new(
        "completion_index",
        crate::ui_vtcomplete_index::status_label(&index),
        Some("completion_index"),
    ));
    if let Some(editor) = editor(app) {
        rows.push(DetailRow::new(
            "goto_line",
            format!(
                "Ln {}, Col {}",
                editor.cursor_line + 1,
                editor.cursor_col + 1
            ),
            Some("goto_line"),
        ));
        if editor.overwrite_mode {
            rows.push(DetailRow::new(
                "overwrite_mode",
                "OVR",
                Some("overwrite_mode"),
            ));
        }
        if !editor.extra_cursors.is_empty() {
            rows.push(DetailRow::new(
                "cursors",
                (editor.extra_cursors.len() + 1).to_string(),
                None,
            ));
        } else if let Some(selection) = &app.cached_sel_stats {
            rows.push(DetailRow::new(
                "selection",
                selection.trim_matches(['(', ')']),
                Some("selection"),
            ));
        }
        if let Some(count) = &app.cached_word_count {
            rows.push(DetailRow::new("word_count", count, None));
        }
        if let Some(occurrences) = &app.cached_word_occ {
            rows.push(DetailRow::new("word_occ", occurrences, None));
        }
        if let Some(results) = search_results(app) {
            rows.push(DetailRow::new("search_results", results, None));
        }
    }
    if !app.cached_git_branch.is_empty() {
        rows.push(DetailRow::new(
            "git_branch",
            &app.cached_git_branch,
            Some("git_branch"),
        ));
    }
    rows.push(DetailRow::new(
        "problems",
        format!(
            "{} errors, {} warnings",
            app.cached_error_count, app.cached_warning_count
        ),
        Some("problems"),
    ));
    if app.macro_recording {
        rows.push(DetailRow::new("macro", "Recording", Some("macro")));
    }
    if app
        .saving_flash
        .is_some_and(|start| start.elapsed() < SAVED_LIFETIME)
    {
        rows.push(DetailRow::new("saved", "Saved", None));
    }
    rows.push(DetailRow::new(
        "performance",
        if app.background.activity.busy_since(Instant::now()).is_some() {
            "Working"
        } else {
            "Idle"
        },
        Some("performance"),
    ));
    rows
}

fn detail_rows(app: &App) -> Vec<DetailRow> {
    if app.status_details.customize {
        return ITEMS
            .iter()
            .map(|item| {
                DetailRow::new(
                    item.id,
                    if is_pinned(app, item.id) {
                        "Bottom bar"
                    } else {
                        "Dropdown"
                    },
                    None,
                )
            })
            .collect();
    }
    // A pinned item that could not fit remains reachable in the dropdown.
    all_detail_rows(app)
        .into_iter()
        .filter(|row| !app.status_bar_items.iter().any(|&(_, _, id)| id == row.id))
        .collect()
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.status_details.open {
        return false;
    }
    if obscured(app) {
        dismiss(app);
        return false;
    }
    let rows = detail_rows(app);
    let count = rows.len();
    app.status_details.selected = app.status_details.selected.min(count.saturating_sub(1));
    app.status_details.follow_selection = true;
    match key.code {
        KeyCode::Esc => dismiss(app),
        KeyCode::Char('c') if key.modifiers.is_empty() => toggle_customization(app),
        KeyCode::Up | KeyCode::BackTab => {
            if count > 0 {
                app.status_details.selected = (app.status_details.selected + count - 1) % count;
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            let reverse = key.modifiers.contains(KeyModifiers::SHIFT);
            if count > 0 {
                app.status_details.selected =
                    (app.status_details.selected + if reverse { count - 1 } else { 1 }) % count;
            }
        }
        KeyCode::Home => app.status_details.selected = 0,
        KeyCode::End => app.status_details.selected = count.saturating_sub(1),
        KeyCode::Enter | KeyCode::Char(' ') => {
            if app.status_details.customize {
                toggle_pin(app, app.status_details.selected);
            } else if let Some(action) = rows
                .get(app.status_details.selected)
                .and_then(|row| row.action)
            {
                activate(app, action);
            }
        }
        _ if !key.modifiers.is_empty() || matches!(key.code, KeyCode::F(_)) => {
            dismiss(app);
            return false;
        }
        _ => {}
    }
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let point = (mouse.column, mouse.row).into();
    if mouse.kind == MouseEventKind::Down(MouseButton::Right)
        && !obscured(app)
        && app.status_bar_area.is_some_and(|area| area.contains(point))
    {
        let item = app
            .status_bar_items
            .iter()
            .find(|&&(x, width, _)| mouse.column >= x && mouse.column < x + width)
            .map(|&(_, _, id)| id);
        open_customization(app, item);
        return true;
    }
    if !app.status_details.open {
        return false;
    }
    if obscured(app) {
        dismiss(app);
        return false;
    }
    let hit = app
        .status_details
        .rows
        .iter()
        .find(|(area, _)| area.contains(point))
        .map(|(_, index)| *index);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if !app.status_details.area.contains(point) || app.status_details.close.contains(point)
            {
                dismiss(app);
            } else if app.status_details.customize_button.contains(point) {
                toggle_customization(app);
            } else if app.status_details.scrollbar.contains(point) {
                app.status_details.scrollbar_drag = true;
                scroll_details_to_pointer(app, mouse.row);
            } else if let Some(index) = hit {
                app.status_details.selected = index;
                if app.status_details.customize {
                    toggle_pin(app, index);
                } else if let Some(action) = detail_rows(app).get(index).and_then(|row| row.action)
                {
                    activate(app, action);
                }
            }
        }
        MouseEventKind::Moved => {
            if let Some(index) = hit {
                app.status_details.selected = index;
            }
            app.mouse_hover_changed = true;
        }
        MouseEventKind::Drag(MouseButton::Left) if app.status_details.scrollbar_drag => {
            scroll_details_to_pointer(app, mouse.row);
        }
        MouseEventKind::Up(MouseButton::Left) => app.status_details.scrollbar_drag = false,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            app.status_details.follow_selection = false;
            let count = detail_rows(app).len();
            app.status_details.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                app.status_details.scroll.saturating_sub(1)
            } else {
                (app.status_details.scroll + 1).min(count.saturating_sub(1))
            };
        }
        _ => {}
    }
    true
}

fn draw_details(f: &mut Frame, app: &mut App) {
    let screen = f.area();
    app.status_details.rows.clear();
    app.status_details.scrollbar = Rect::default();
    if screen.width < 8 || screen.height < 4 {
        app.status_details.area = Rect::default();
        app.status_details.close = Rect::default();
        app.status_details.customize_button = Rect::default();
        return;
    }
    let rows = detail_rows(app);
    let customize = app.status_details.customize;
    app.status_details.selected = app
        .status_details
        .selected
        .min(rows.len().saturating_sub(1));
    let margin = u16::from(screen.width >= 24);
    let width = screen
        .width
        .saturating_sub(2 * margin)
        .min(if customize { 54 } else { 52 });
    let height = (rows.len().max(1) as u16 + 4).min(screen.height.saturating_sub(1));
    let bottom = app
        .status_bar_area
        .map_or(screen.bottom(), |bar| bar.y)
        .max(screen.y + height);
    let area = Rect::new(
        screen.right() - width - margin,
        bottom - height,
        width,
        height,
    );
    app.status_details.area = area;
    let surface = crate::ui_widgets::widget_background(f, &app.colors, area);
    let fg = app.colors.ui.readable_foreground(
        app.colors
            .ui
            .widget_foreground
            .over(surface)
            .unwrap_or(app.colors.fg_text),
        surface,
        4.5,
    );
    let dim = app.colors.ui.description_color(surface);
    let normal = Style::default().fg(fg).bg(surface);
    let border = app
        .colors
        .ui
        .widget_border
        .border(surface)
        .unwrap_or(surface);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(normal);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let close = Rect::new(
        inner.right().saturating_sub(3),
        inner.y,
        3.min(inner.width),
        1,
    );
    app.status_details.close = close;
    let button_width = if inner.width >= 36 {
        if customize { 6 } else { 11 }
    } else if inner.width >= 10 {
        3
    } else {
        0
    };
    let customize_button = Rect::new(
        close.x.saturating_sub(button_width),
        inner.y,
        button_width,
        1,
    );
    app.status_details.customize_button = customize_button;
    let title_width = inner.width.saturating_sub(4 + button_width);
    let title = if customize {
        " Customize status bar"
    } else if editor(app).is_some() {
        " File details"
    } else {
        " Status details"
    };
    f.render_widget(
        Paragraph::new(clip_middle(title, title_width as usize))
            .style(normal.add_modifier(Modifier::BOLD)),
        Rect::new(inner.x, inner.y, title_width, 1),
    );
    f.render_widget(
        Paragraph::new(" × ").style(app.colors.ui.toolbar_style(
            fg,
            surface,
            close.contains((app.mouse_col, app.mouse_row).into()),
            false,
            false,
        )),
        close,
    );
    if button_width > 0 {
        let label = match (customize, button_width > 3) {
            (true, true) => " Done ",
            (false, true) => " Customize ",
            (true, false) => " ✓ ",
            (false, false) => " ⚙ ",
        };
        f.render_widget(
            Paragraph::new(label).style(app.colors.ui.toolbar_style(
                fg,
                surface,
                customize_button.contains((app.mouse_col, app.mouse_row).into()),
                false,
                false,
            )),
            customize_button,
        );
    }
    if inner.height <= 2 {
        return;
    }
    let subtitle = if customize {
        "Pinned items show when available."
    } else {
        editor(app).map_or("Workspace status", Editor::file_name)
    };
    f.render_widget(
        Paragraph::new(clip_middle(
            subtitle,
            inner.width.saturating_sub(2) as usize,
        ))
        .style(normal.fg(dim)),
        Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1),
    );
    let list_height = inner.height.saturating_sub(2);
    let list = Rect::new(inner.x, inner.y + 2, inner.width, list_height);
    // Keyboard selection stays visible; scrolling can still reveal metadata.
    let max_scroll = rows.len().saturating_sub(list.height as usize);
    let show_scrollbar = max_scroll > 0;
    app.status_details.scroll = app.status_details.scroll.min(max_scroll);
    let selected = app.status_details.selected;
    if app.status_details.follow_selection && selected < app.status_details.scroll {
        app.status_details.scroll = selected;
    } else if app.status_details.follow_selection
        && selected >= app.status_details.scroll + list.height as usize
    {
        app.status_details.scroll = selected + 1 - list.height as usize;
    }
    app.status_details.follow_selection = false;
    for (offset, (index, row)) in rows
        .iter()
        .enumerate()
        .skip(app.status_details.scroll)
        .take(list.height as usize)
        .enumerate()
    {
        let row_area = Rect::new(
            list.x,
            list.y + offset as u16,
            list.width.saturating_sub(u16::from(show_scrollbar)),
            1,
        );
        let selected = index == app.status_details.selected;
        let style = app.colors.ui.list_row_style(
            app.colors.ui.widget_foreground,
            surface,
            selected,
            true,
            false,
        );
        let row_bg = style.bg.unwrap_or(surface);
        let row_fg = app
            .colors
            .ui
            .readable_foreground(style.fg.unwrap_or(fg), row_bg, 4.5);
        let prefix = if customize {
            if is_pinned(app, row.id) {
                " [✓] "
            } else {
                " [ ] "
            }
        } else {
            " "
        };
        let label_width = 18.min(row_area.width.saturating_sub(5) as usize / 2 + 4);
        let value_width = (row_area.width as usize).saturating_sub(
            label_width + UnicodeWidthStr::width(prefix) + if customize { 2 } else { 3 },
        );
        let label = clip_middle(row.label, label_width);
        let value = if customize && value_width < 10 {
            clip_middle(
                if is_pinned(app, row.id) {
                    "Bar"
                } else {
                    "Menu"
                },
                value_width,
            )
        } else {
            clip_middle(&row.value, value_width)
        };
        let fill = label_width.saturating_sub(UnicodeWidthStr::width(label.as_str()));
        let line = Line::from(vec![
            Span::raw(prefix),
            Span::raw(label),
            Span::raw(crate::helpers::spaces(fill + 1)),
            Span::styled(
                value,
                Style::default().fg(if selected {
                    row_fg
                } else {
                    app.colors.ui.description_color(row_bg)
                }),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).style(style.fg(row_fg).bg(row_bg)),
            row_area,
        );
        if !customize && row.action.is_some() {
            let arrow = Rect::new(row_area.right().saturating_sub(2), row_area.y, 1, 1);
            f.render_widget(
                Paragraph::new("›").style(style.fg(row_fg).bg(row_bg)),
                arrow,
            );
        }
        app.status_details.rows.push((row_area, index));
    }
    if show_scrollbar {
        let scrollbar = Rect::new(list.right() - 1, list.y, 1, list.height);
        app.status_details.scrollbar = scrollbar;
        let hovered = scrollbar.contains((app.mouse_col, app.mouse_row).into());
        let (track, thumb) =
            app.colors
                .ui
                .scrollbar_colors(surface, hovered, app.status_details.scrollbar_drag);
        crate::ui_widgets::VerticalScrollbar::new(
            scrollbar.x,
            scrollbar.y,
            scrollbar.height,
            rows.len(),
            app.status_details.scroll,
        )
        .symbols('█', '│')
        .colors(thumb, track, track)
        .render(f);
    }
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(clip_middle(
                "All available items are on the bar.",
                list.width.saturating_sub(2) as usize,
            ))
            .style(normal.fg(dim)),
            Rect::new(list.x + 1, list.y, list.width.saturating_sub(2), 1),
        );
    }
}

pub(crate) fn draw_overlays(f: &mut Frame, app: &mut App) {
    if obscured(app) || !app.terminal_has_focus {
        dismiss(app);
        return;
    }
    if app.status_details.open {
        app.chrome_hover = HoverState::default();
        draw_details(f, app);
        return;
    }
    if app.background.activity.details_open && !app.performance_view.pinned {
        app.chrome_hover = HoverState::default();
        return;
    }
    draw_hover(f, app);
}

fn draw_hover(f: &mut Frame, app: &mut App) {
    let point = (app.mouse_col, app.mouse_row).into();
    let target = app
        .status_bar_area
        .filter(|area| area.contains(point))
        .and_then(|_| {
            app.status_bar_items.iter().find(|&&(x, width, action)| {
                action != "performance" && app.mouse_col >= x && app.mouse_col < x + width
            })
        })
        .map(|&(_, _, action)| HoverTarget::Status(action))
        .or_else(|| {
            app.activity_bar_area
                .filter(|area| area.contains(point))
                .and(app.hovered_activity.map(HoverTarget::Activity))
        })
        .or_else(|| {
            app.panel_tab_areas
                .iter()
                .find(|(area, _)| area.contains(point))
                .map(|(_, tab)| HoverTarget::Panel(*tab))
        });
    if target != app.chrome_hover.target {
        app.chrome_hover = HoverState {
            target,
            started: target.map(|_| Instant::now()),
            shown: false,
        };
    }
    if !app.chrome_hover.shown && !hover_due(app) {
        return;
    }
    let Some(target) = target else {
        return;
    };
    let text = match target {
        HoverTarget::Status("git_branch") => {
            format!("{} · Open Source Control", app.cached_git_branch)
        }
        HoverTarget::Status("problems") => format!(
            "{} errors, {} warnings · Open Problems",
            app.cached_error_count, app.cached_warning_count
        ),
        HoverTarget::Status("goto_line") => {
            hint_with_shortcut(app, "Go to Line / Column", "goto_line")
        }
        HoverTarget::Status("file_details") => {
            "Status details · Hidden items and bottom-bar customization".into()
        }
        HoverTarget::Status("read_only") => {
            if file_detail_rows(app)
                .iter()
                .any(|row| row.action == Some("read_only"))
            {
                "Read-only mode · Click to toggle".into()
            } else {
                "Read-only document".into()
            }
        }
        HoverTarget::Status("overwrite_mode") => {
            "Overwrite mode · Click to return to Insert".into()
        }
        HoverTarget::Status("language") => "Change language mode or configure file associations".into(),
        HoverTarget::Status("tab_size") => "Change indentation · Right-click to customize".into(),
        HoverTarget::Status("encoding") => "Change file encoding · Right-click to customize".into(),
        HoverTarget::Status("line_ending") => {
            "Change line endings · Right-click to customize".into()
        }
        HoverTarget::Status("word_wrap") => "Toggle word wrap · Right-click to customize".into(),
        HoverTarget::Status("file_size") => "File size · Right-click to customize".into(),
        HoverTarget::Status("line_count") => {
            "Document line count · Right-click to customize".into()
        }
        HoverTarget::Status("language_server") => {
            "Configure language servers · Click to open LSP settings · Right-click to customize".into()
        }
        HoverTarget::Status("completion_index") => {
            let status = app.completion_index.status();
            format!(
                "VTComplete file learning · {} indexed files · {} · Click to manage learning and RAM use",
                status.indexed_files,
                crate::ui_vtcomplete_index::phase_label(status.phase)
            )
        }
        HoverTarget::Status("macro") => "Recording macro · Click to stop".into(),
        HoverTarget::Status("selection") => app.cached_sel_stats.clone().unwrap_or_default(),
        HoverTarget::Status("cursors") => "Multiple cursors · Esc to return to one cursor".into(),
        HoverTarget::Status("word_count") => "Document word count".into(),
        HoverTarget::Status("scroll") => "Position in document · Click to go to a line".into(),
        HoverTarget::Status("word_occ") => "Occurrences of the current word".into(),
        HoverTarget::Status("search_results") => "Current file search results".into(),
        HoverTarget::Status("saved") => "File saved".into(),
        HoverTarget::Status(_) => return,
        HoverTarget::Activity(index) => {
            let (label, command) = match index {
                0 => ("Explorer".into(), "toggle_explorer"),
                1 => ("Search".into(), "global_search"),
                2 => (
                    format!(
                        "Source Control · {} changes",
                        app.source_control_files.len()
                    ),
                    "source_control",
                ),
                4 => ("Extensions".into(), "extensions"),
                5 => ("Outline".into(), "outline"),
                6 => ("Settings".into(), "settings"),
                _ => return,
            };
            hint_with_shortcut(app, &label, command)
        }
        HoverTarget::Panel(index) => ["", "Problems", "Output", "Notifications"]
            .get(index as usize)
            .unwrap_or(&"")
            .to_string(),
    };
    app.chrome_hover.shown = true;
    crate::ui_widgets::Tooltip::hover(vec![Line::raw(text)], &app.colors)
        .max_width(64)
        .render(f, app.mouse_col.saturating_add(1), app.mouse_row);
}

fn hint_with_shortcut(app: &App, label: &str, command: &str) -> String {
    let shortcut = app.shortcut_for_command(command);
    if shortcut.is_empty() {
        label.into()
    } else {
        format!("{label} · {shortcut}")
    }
}

#[cfg(test)]
#[path = "ui_status_tests.rs"]
mod tests;
