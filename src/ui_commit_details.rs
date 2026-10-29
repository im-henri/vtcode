//! Responsive commit inspector: overview, changed files, and the native diff editor.
//! Rendering and input share one layout, including after terminal resizing.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    commit_details::{Divider, Focus, PaneSizes},
    ui_pickers::truncate_to_width,
};

#[cfg(test)]
#[path = "ui_commit_details_tests.rs"]
mod tests;

#[path = "ui_commit_review.rs"]
mod review;

const ACTIONS: [&str; 7] = [
    "‹ Newer",
    "Older ›",
    "Copy SHA",
    "Copy message",
    "Open diff",
    "Focus changes",
    "Reset layout",
];
const COMPACT_ACTIONS: [&str; 7] = ["‹", "›", "SHA", "Message", "Diff", "Focus", "Reset"];

#[derive(Clone, Debug, Default)]
pub(crate) struct InspectorLayout {
    pub area: Rect,
    pub title: Rect,
    pub feedback: Rect,
    pub body: Rect,
    pub primary_divider: Rect,
    pub overview: Rect,
    pub heading: Rect,
    pub filter: Rect,
    pub filter_clear: Rect,
    pub files: Rect,
    pub patch_heading: Rect,
    pub diff_toggle: Rect,
    pub diff_full: Rect,
    pub diff_changes: Rect,
    pub context_label: Rect,
    pub context_less: Rect,
    pub context_count: Rect,
    pub context_more: Rect,
    pub hunk_previous: Rect,
    pub hunk_next: Rect,
    pub patch: Rect,
    pub buttons: Vec<Rect>,
    pub compact: bool,
    pub expand_all: Rect,
    pub collapse_all: Rect,
}

/// Full editor view geometry, shared by rendering, hit testing and resizing.
/// Ratios express user preference; minimum sizes only constrain this frame.
pub(crate) fn layout_in_area(area: Rect, expanded: bool, panes: PaneSizes) -> InspectorLayout {
    let inset = u16::from(area.width > 2);
    let content = Rect::new(
        area.x + inset,
        area.y,
        area.width.saturating_sub(inset * 2),
        area.height,
    );
    let mut result = InspectorLayout {
        area,
        compact: content.width < 118,
        ..Default::default()
    };
    if content.is_empty() {
        return result;
    }
    result.title = Rect::new(content.x, content.y, content.width.min(19), 1);
    let labels = if result.compact {
        COMPACT_ACTIONS
    } else {
        ACTIONS
    };
    let total: u16 = labels
        .iter()
        .map(|label| label.width() as u16 + 3)
        .sum::<u16>()
        .saturating_sub(1);
    let inline = content.width >= total.saturating_add(result.title.width + 2);
    let (mut x, mut y) = if inline {
        (content.right() - total, content.y)
    } else {
        (content.x, content.y.saturating_add(1))
    };
    for (index, label) in labels.iter().enumerate() {
        if content.width < 30 && index >= 2
            || content.width < 50 && index == 5
            || content.width < 60 && index == 6
        {
            result.buttons.push(Rect::default());
            continue;
        }
        let width = (label.width() as u16 + 2).min(content.width);
        if x.saturating_add(width) > content.right() {
            x = content.x;
            y = y.saturating_add(1);
        }
        result.buttons.push(if y < content.bottom() {
            Rect::new(x, y, width, 1)
        } else {
            Rect::default()
        });
        x = x.saturating_add(width + 1);
    }
    let feedback_y = y.saturating_add(1).min(content.bottom());
    result.feedback = Rect::new(
        content.x,
        feedback_y,
        content.width,
        u16::from(feedback_y < content.bottom()),
    );
    let body_y = feedback_y.saturating_add(1).min(content.bottom());
    result.body = Rect::new(
        content.x,
        body_y,
        content.width,
        content.bottom().saturating_sub(body_y),
    );
    let mut y = body_y;
    if !expanded && result.body.height >= 7 {
        let available = result.body.height.saturating_sub(8);
        let height = ((result.body.height as u32 * panes.overview as u32 / 1000) as u16)
            .clamp(2, available.max(2));
        result.overview = Rect::new(content.x, y, content.width, height);
        y = y.saturating_add(height);
        result.primary_divider = Rect::new(content.x, y, content.width, 1);
        y = y.saturating_add(1);
    }
    if y < content.bottom() {
        result.heading = Rect::new(content.x, y, content.width, 1);
        if content.width >= 40 {
            result.expand_all = Rect::new(content.right() - 14, y, 14, 1);
            result.collapse_all = Rect::new(content.right() - 30, y, 15, 1);
        } else if content.width >= 20 {
            result.expand_all = Rect::new(content.right() - 5, y, 5, 1);
            result.collapse_all = Rect::new(content.right() - 11, y, 5, 1);
        }
        y += 1;
    }
    if y < content.bottom().saturating_sub(1) {
        let toggle = if content.width >= 32 { 15 } else { 0 };
        result.filter = Rect::new(content.x, y, content.width.saturating_sub(toggle), 1);
        if result.filter.width >= 12 {
            result.filter_clear = Rect::new(result.filter.right() - 3, y, 3, 1);
        }
        if toggle > 0 {
            result.diff_toggle = Rect::new(content.right() - 14, y, 14, 1);
        }
        y += 1;
    }
    if content.width >= 42 && content.bottom().saturating_sub(y) >= 4 {
        result.patch_heading = Rect::new(content.x, y, content.width, 1);
        result.diff_full = Rect::new(content.x, y, 11, 1);
        result.diff_changes = Rect::new(content.x + 12, y, 9, 1);
        result.context_label = Rect::new(content.x + 22, y, 2, 1);
        result.context_less = Rect::new(content.x + 24, y, 3, 1);
        result.context_count = Rect::new(content.x + 27, y, 4, 1);
        result.context_more = Rect::new(content.x + 31, y, 3, 1);
        if content.width >= 52 {
            result.hunk_previous = Rect::new(content.right() - 7, y, 3, 1);
            result.hunk_next = Rect::new(content.right() - 3, y, 3, 1);
        }
        y += 1;
    }
    result.files = Rect::new(
        content.x,
        y,
        content.width,
        content.bottom().saturating_sub(y),
    );
    result
}

fn current_layout(app: &App) -> InspectorLayout {
    app.commit_details.layout.clone().unwrap_or_else(|| {
        layout_in_area(
            app.commit_details.area,
            app.commit_details.diff_expanded,
            app.commit_details.panes,
        )
    })
}

fn actions(view: &InspectorLayout) -> impl Iterator<Item = (usize, Rect)> + '_ {
    view.buttons
        .iter()
        .copied()
        .enumerate()
        .map(|(i, area)| (if i == 6 { 8 } else { i }, area))
        .chain([
            (9, view.diff_toggle),
            (6, view.hunk_previous),
            (7, view.hunk_next),
            (10, view.diff_full),
            (11, view.diff_changes),
            (12, view.context_less),
            (14, view.context_count),
            (13, view.context_more),
            (15, view.expand_all),
            (16, view.collapse_all),
        ])
}

fn obscured(app: &App) -> bool {
    app.change_dir_active
        || app.context_menu_active
        || app.menu_open
        || app.tab_context_active
        || app.explorer_context_active
        || app.sc_context_active
        || app.shortcuts_active
        || app.settings_active
        || app.hunk_popup_active
        || app.palette.active
        || app.rename.active
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
}

pub(crate) fn focused(app: &App) -> bool {
    app.is_commit_details_tab()
        && app.active_pane == crate::app::ActivePane::Editor
        && !app.docked_terminal_has_focus()
        && !(app.search_active && app.search_input_focused)
        && !obscured(app)
}

fn diff_loaded(app: &App) -> bool {
    !app.commit_details.loading
        && !app.commit_details.diff_loading
        && app.commit_details.error.is_none()
        && app.commit_details.diff_error.is_none()
        && app.commit_details.diff_file == app.commit_details.selected_file()
        && app.commit_details.diff_file.is_some()
}

fn native_diff_ready(app: &App) -> bool {
    diff_loaded(app) && app.should_render_diff_for_active_editor()
}

pub(crate) fn owns_key(app: &App, key: KeyEvent) -> bool {
    if !focused(app) {
        return false;
    }
    if app.commit_details.focus == Focus::Diff && native_diff_ready(app) {
        return (key.modifiers.contains(KeyModifiers::ALT)
            && matches!(
                key.code,
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
            ))
            || !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT)
                && matches!(
                    key.code,
                    KeyCode::Tab
                        | KeyCode::BackTab
                        | KeyCode::Esc
                        | KeyCode::F(7)
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                )
            || key.modifiers.is_empty()
                && matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Char('p' | '/' | '[' | ']')
                );
    }
    if app.commit_details.focus == Focus::Action(14)
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && matches!(key.code, KeyCode::Char('a' | 'A' | 'v' | 'V'))
    {
        return true;
    }
    let ctrl = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
    if ctrl {
        if key.modifiers.contains(KeyModifiers::ALT) {
            return key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char(_));
        }
        return matches!(key.code, KeyCode::Char('c' | 'C'))
            || matches!(key.code, KeyCode::Char('f' | 'F'))
                && !key.modifiers.contains(KeyModifiers::SHIFT)
            || app.commit_details.focus == Focus::Filter
                && matches!(
                    key.code,
                    KeyCode::Char('a' | 'A' | 'x' | 'X' | 'v' | 'V' | 'z' | 'Z')
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Backspace
                        | KeyCode::Delete
                        | KeyCode::Home
                        | KeyCode::End
                );
    }
    matches!(
        key.code,
        KeyCode::Char(_)
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::F(7)
    )
}

fn pane_ratio(panes: &mut PaneSizes) -> &mut u16 {
    &mut panes.overview
}

fn resize_pane(app: &mut App, view: &InspectorLayout, _col: u16, row: u16) {
    if app.commit_details.divider_drag.is_none() {
        return;
    }
    let position = row.saturating_sub(view.body.y);
    let total = view.body.height.saturating_sub(1);
    if total > 0 {
        *pane_ratio(&mut app.commit_details.panes) =
            (position.min(total) as u32 * 1000 / total as u32) as u16;
        app.commit_details.layout = None;
    }
}

pub(crate) fn wrapped(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut row = String::new();
        let mut used = 0;
        for word in paragraph.split_word_bounds() {
            let word_width = UnicodeWidthStr::width(word);
            if used > 0 && used + word_width > width {
                lines.push(row.trim_end().to_owned());
                row.clear();
                used = 0;
            }
            if used == 0 && word.trim().is_empty() {
                continue;
            }
            for g in word.graphemes(true) {
                let w = UnicodeWidthStr::width(g);
                if used + w > width && !row.is_empty() {
                    lines.push(std::mem::take(&mut row));
                    used = 0;
                }
                if w <= width {
                    row.push_str(g);
                    used += w;
                }
            }
        }
        lines.push(row.trim_end().to_owned());
    }
    lines
}

fn enabled(app: &App, action: usize) -> bool {
    match action {
        0 => app.adjacent_commit(true).is_some(),
        1 => app.adjacent_commit(false).is_some(),
        2 => !app.commit_details.data.hash.is_empty(),
        3 => !app.commit_details.loading && app.commit_details.error.is_none(),
        5 if app.commit_details.diff_expanded => true,
        4 | 5 => {
            !app.commit_details.loading
                && app.commit_details.error.is_none()
                && app
                    .commit_details
                    .selected_file()
                    .is_some_and(|i| action == 5 || !app.commit_details.data.files[i].binary)
        }
        6 | 7 => native_diff_ready(app) && !app.diff_render_cache.hunks.is_empty(),
        9 => native_diff_ready(app),
        10 | 11 | 14 => native_diff_ready(app),
        12 => native_diff_ready(app) && app.commit_details.diff_context.lines > 0,
        13 => native_diff_ready(app) && app.commit_details.diff_context.lines < 999,
        8 => true,
        15 | 16 => !app.commit_details.loading && !app.commit_details.data.files.is_empty(),
        _ => false,
    }
}

fn copy_feedback_text(action: usize, copied: bool) -> &'static str {
    match (action, copied) {
        (2, true) => "Copied full commit SHA",
        (_, true) => "Copied commit message",
        (2, false) => "Could not copy SHA: clipboard unavailable",
        (_, false) => "Could not copy message: clipboard unavailable",
    }
}

fn copy_commit_value_with(app: &mut App, action: usize, copy: impl FnOnce(&str) -> bool) {
    if !matches!(action, 2 | 3) || !enabled(app, action) {
        return;
    }
    let value = if action == 2 {
        &app.commit_details.data.hash
    } else {
        &app.commit_details.data.message
    };
    let copied = copy(value);
    let message = copy_feedback_text(action, copied);
    app.status_message = message.into();
    app.commit_details.copy_feedback = Some(crate::commit_details::CopyFeedback {
        action,
        copied,
        expires_at: std::time::Instant::now()
            + std::time::Duration::from_secs(if copied { 3 } else { 5 }),
    });
    if !copied {
        app.notify_error(message);
    }
}

fn activate(app: &mut App, action: usize) {
    if !enabled(app, action) {
        return;
    }
    match action {
        0 | 1 => {
            if let Some(index) = app.adjacent_commit(action == 0) {
                let expanded = app.commit_details.diff_expanded;
                app.open_source_control_commit_details(index);
                app.commit_details.diff_expanded = expanded;
                if expanded {
                    app.commit_details.focus = Focus::Diff;
                }
                let visible = app.sc_graph_height.saturating_sub(1).max(1) as usize;
                app.git_log_scroll = app.git_log_scroll.min(index);
                if index >= app.git_log_scroll + visible {
                    app.git_log_scroll = index + 1 - visible;
                }
                app.prefetch_git_history(visible);
            }
        }
        2 | 3 => {
            copy_commit_value_with(app, action, |value| {
                crate::clipboard::set_text(value).is_ok()
            });
        }
        4 => {
            if let Some(index) = app.commit_details.selected_file() {
                app.open_source_control_commit_details_file_diff(index);
            }
        }
        5 => {
            app.commit_details.diff_expanded = !app.commit_details.diff_expanded;
            app.commit_details.focus = if app.commit_details.diff_expanded {
                Focus::Diff
            } else {
                Focus::Files
            };
            app.commit_details.layout = None;
        }
        6 | 7 => {
            app.execute_palette_command(if action == 6 {
                "prev_diff_hunk"
            } else {
                "next_diff_hunk"
            });
            app.commit_details.focus = Focus::Diff;
        }
        9 => {
            app.execute_palette_command("toggle_diff_layout");
            app.commit_details.focus = Focus::Diff;
        }
        10 | 11 => {
            app.commit_details.context_draft = None;
            app.set_commit_diff_context(action == 11, app.commit_details.diff_context.lines);
        }
        12 | 13 => {
            let lines = app
                .commit_details
                .diff_context
                .lines
                .saturating_add_signed(if action == 12 { -1 } else { 1 })
                .min(999);
            app.commit_details.context_draft = None;
            app.set_commit_diff_context(true, lines);
        }
        14 => {
            if let Some(draft) = app.commit_details.context_draft.take() {
                if let Ok(lines) = draft.parse::<usize>() {
                    app.set_commit_diff_context(true, lines.min(999));
                }
            } else {
                app.commit_details.context_draft = Some(String::new());
            }
            app.commit_details.focus = Focus::Action(14);
        }
        8 => {
            app.commit_details.panes = PaneSizes::default();
            app.commit_details.review.forced_mode = None;
            app.commit_details.diff_expanded = false;
            app.commit_details.layout = None;
            app.commit_details.focus = Focus::Files;
        }
        15 | 16 => {
            if action == 15 {
                app.commit_details.review.collapsed.clear();
            } else {
                app.commit_details.review.collapsed =
                    (0..app.commit_details.data.files.len()).collect();
            }
            app.commit_details.review.reveal = true;
            app.commit_details.focus = Focus::Files;
        }
        _ => {}
    }
}

fn apply_context_draft(app: &mut App) {
    if let Some(draft) = app.commit_details.context_draft.take()
        && let Ok(lines) = draft.parse::<usize>()
    {
        app.set_commit_diff_context(true, lines.min(999));
    }
}

pub(crate) fn paste_context_count(app: &mut App, text: &str) {
    let text = text.trim();
    if app.commit_details.focus == Focus::Action(14)
        && !text.is_empty()
        && text.len() <= 3
        && text.chars().all(|c| c.is_ascii_digit())
    {
        app.commit_details.context_draft = Some(text.into());
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let view = current_layout(app);
    let focus = app.commit_details.focus;
    let selected_before = app.commit_details.selected;
    if focus == Focus::Action(14) {
        match key.code {
            KeyCode::Char('v' | 'V') if ctrl => {
                if let Ok(text) = crate::clipboard::get_text() {
                    paste_context_count(app, &text);
                }
                return;
            }
            KeyCode::Char('a' | 'A') if ctrl => {
                app.commit_details.context_draft = Some(String::new());
                return;
            }
            KeyCode::Char(c) if c.is_ascii_digit() && !ctrl && !alt => {
                let draft = app
                    .commit_details
                    .context_draft
                    .get_or_insert_with(String::new);
                if draft.len() < 3 {
                    draft.push(c);
                }
                return;
            }
            KeyCode::Backspace => {
                let draft = app
                    .commit_details
                    .context_draft
                    .get_or_insert_with(|| app.commit_details.diff_context.lines.to_string());
                draft.pop();
                return;
            }
            KeyCode::Esc if app.commit_details.context_draft.is_some() => {
                app.commit_details.context_draft = None;
                return;
            }
            KeyCode::Up | KeyCode::Down => {
                activate(app, if key.code == KeyCode::Up { 13 } else { 12 });
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => apply_context_draft(app),
            _ => {}
        }
    }
    // Windows reports AltGr text as Ctrl+Alt. Keep it in the filter and
    // prevent the workbench's editor fallback from receiving these characters.
    if key
        .modifiers
        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char(_))
    {
        if focus == Focus::Filter {
            app.commit_details
                .edit_filter(key.code, key.modifiers, None);
            app.request_commit_diff();
        }
        return;
    }
    match key.code {
        KeyCode::Esc if focus == Focus::Filter => {
            if app.commit_details.query.is_empty() {
                app.commit_details.focus = Focus::Files;
            } else {
                app.commit_details.clear_filter();
            }
        }
        KeyCode::Esc if app.commit_details.diff_expanded => {
            activate(app, 5);
        }
        KeyCode::Esc => app.commit_details.focus = Focus::Files,
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
            if alt && key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let amount = if matches!(key.code, KeyCode::Left | KeyCode::Up) {
                -50
            } else {
                50
            };
            let ratio = pane_ratio(&mut app.commit_details.panes);
            *ratio = ratio.saturating_add_signed(amount).clamp(100, 900);
            app.commit_details.layout = None;
        }
        KeyCode::Left if alt => activate(app, 0),
        KeyCode::Right if alt => activate(app, 1),
        KeyCode::Up | KeyCode::Down if alt => {
            let max = app.commit_details.filtered_files().len().saturating_sub(1);
            app.commit_details.selected = if key.code == KeyCode::Up {
                app.commit_details.selected.saturating_sub(1)
            } else {
                (app.commit_details.selected + 1).min(max)
            };
        }
        KeyCode::F(7) => activate(
            app,
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                6
            } else {
                7
            },
        ),
        KeyCode::Char('[' | ']') if focus == Focus::Diff => {
            activate(app, if key.code == KeyCode::Char('[') { 6 } else { 7 })
        }
        KeyCode::Char('c' | 'C') if ctrl && focus != Focus::Filter => activate(
            app,
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                3
            } else {
                2
            },
        ),
        KeyCode::Char('f' | 'F') if ctrl => {
            app.commit_details.diff_expanded = false;
            app.commit_details.layout = None;
            app.commit_details.focus = Focus::Filter;
        }
        KeyCode::Char('/') if focus != Focus::Filter => {
            app.commit_details.diff_expanded = false;
            app.commit_details.layout = None;
            app.commit_details.focus = Focus::Filter;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let mut stops = Vec::new();
            if view.files.height > 0 {
                stops.push(Focus::Files);
            }
            if view.filter.height > 0 {
                stops.push(Focus::Filter);
            }
            if view.overview.height > 0 {
                stops.push(Focus::Message);
            }
            if view.patch.height > 0 {
                stops.push(Focus::Diff);
            }
            for (i, rect) in actions(&view) {
                if rect.width > 0 && enabled(app, i) {
                    stops.push(Focus::Action(i));
                }
            }
            if !stops.is_empty() {
                let i = stops.iter().position(|f| *f == focus).unwrap_or(0);
                let reverse =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                app.commit_details.focus =
                    stops[(i + if reverse { stops.len() - 1 } else { 1 }) % stops.len()];
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') if focus == Focus::Files => {
            if let Some(file) = app.commit_details.selected_file() {
                review::toggle(app, file);
            }
        }
        KeyCode::Left | KeyCode::Right if focus == Focus::Files => {
            if let Some(file) = app.commit_details.selected_file() {
                if key.code == KeyCode::Left {
                    app.commit_details.review.collapsed.insert(file);
                } else {
                    app.commit_details.review.collapsed.remove(&file);
                }
            }
        }
        KeyCode::PageUp | KeyCode::PageDown if matches!(focus, Focus::Files | Focus::Diff) => {
            let review = &mut app.commit_details.review;
            review.scroll = review
                .scroll
                .saturating_add_signed(if key.code == KeyCode::PageUp {
                    -(view.files.height as isize).max(1)
                } else {
                    (view.files.height as isize).max(1)
                })
                .min(review.total.saturating_sub(view.files.height as usize));
        }
        KeyCode::Enter => {
            if let Focus::Action(action) = focus {
                activate(app, action);
            } else if focus == Focus::Filter {
                app.commit_details.focus = Focus::Files;
            } else {
                activate(app, 4);
            }
        }
        KeyCode::Char(' ') if matches!(focus, Focus::Action(_)) => {
            if let Focus::Action(action) = focus {
                activate(app, action);
            }
        }
        KeyCode::Left | KeyCode::Right if matches!(focus, Focus::Action(_)) => {
            let Focus::Action(current) = focus else {
                unreachable!()
            };
            let actions: Vec<_> = actions(&view)
                .filter_map(|(index, area)| {
                    (area.width > 0 && enabled(app, index)).then_some(index)
                })
                .collect();
            if let Some(position) = actions.iter().position(|&index| index == current) {
                let direction = if key.code == KeyCode::Left {
                    actions.len() - 1
                } else {
                    1
                };
                app.commit_details.focus =
                    Focus::Action(actions[(position + direction) % actions.len()]);
            }
        }
        KeyCode::Up | KeyCode::Down if focus == Focus::Filter => {
            app.commit_details.focus = Focus::Files;
        }
        KeyCode::Up | KeyCode::Down if matches!(focus, Focus::Action(_)) => {
            app.commit_details.focus = if app.commit_details.diff_expanded {
                Focus::Diff
            } else {
                Focus::Files
            };
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End
            if focus != Focus::Filter =>
        {
            let details = &mut app.commit_details;
            let (position, total, visible) = match focus {
                Focus::Message => (
                    &mut details.message_scroll,
                    details.message_rows,
                    view.overview.height,
                ),
                _ => {
                    let total = details.filtered_files().len();
                    (&mut details.selected, total, view.files.height)
                }
            };
            let max = if matches!(focus, Focus::Message) {
                total.saturating_sub(visible.max(1) as usize)
            } else {
                total.saturating_sub(1)
            };
            let page = visible.max(1) as usize;
            *position = match key.code {
                KeyCode::Up => position.saturating_sub(1),
                KeyCode::Down => position.saturating_add(1).min(max),
                KeyCode::PageUp => position.saturating_sub(page),
                KeyCode::PageDown => position.saturating_add(page).min(max),
                KeyCode::Home => 0,
                KeyCode::End => max,
                _ => *position,
            };
        }
        KeyCode::Char('p') if focus != Focus::Filter => activate(app, 5),
        KeyCode::Char('r') if focus != Focus::Filter && app.commit_details.error.is_some() => {
            app.load_commit_details()
        }
        KeyCode::Char('r') if focus != Focus::Filter => {
            if let Some(file) = app.commit_details.selected_file()
                && app.commit_details.review.errors.remove(&file).is_some()
            {
                app.queue_commit_review_file(file);
            }
        }
        _ if focus == Focus::Filter => {
            app.commit_details
                .edit_filter(key.code, key.modifiers, None)
        }
        _ => {}
    }
    if selected_before != app.commit_details.selected
        || app.commit_details.focus == Focus::Files && focus != Focus::Files
        || focus == Focus::Files
            && matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End
            )
    {
        app.commit_details.review.reveal = true;
    }
    if app.commit_details.focus == Focus::Diff {
        app.request_commit_diff();
    }
}

pub(crate) fn click(app: &mut App, col: u16, row: u16) {
    let view = current_layout(app);
    let point = (col, row).into();
    if !view.area.contains(point) {
        return;
    }
    for (i, area) in actions(&view) {
        if area.contains(point) {
            if enabled(app, i) {
                app.commit_details.focus = Focus::Action(i);
                activate(app, i);
            }
            return;
        }
    }
    if review::click_card(app, col, row) {
        return;
    }
    if !app.commit_details.query.is_empty() && view.filter_clear.contains(point) {
        app.commit_details.clear_filter();
        app.commit_details.review.scroll = 0;
        app.commit_details.review.reveal = true;
    } else if view.filter.contains(point) {
        app.commit_details.focus = Focus::Filter;
        app.commit_details.query_cursor = app
            .commit_details
            .query_hit
            .as_ref()
            .map_or(app.commit_details.query.len(), |map| map.byte_at(col, row));
        app.commit_details.query_anchor = None;
    } else if view.files.contains(point) {
        if app.commit_details.error.is_some() {
            app.load_commit_details();
            return;
        }
        app.commit_details.focus = Focus::Files;
    } else if view.overview.contains(point) {
        app.commit_details.focus = Focus::Message;
    } else if view.patch.contains(point) {
        app.commit_details.focus = Focus::Diff;
    } else if view.patch_heading.contains(point) {
        activate(app, 5);
    }
}

pub(crate) fn scroll(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    if !matches!(
        kind,
        MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
    ) {
        return;
    }
    let view = current_layout(app);
    let point = (col, row).into();
    let pane = if view.overview.contains(point) {
        Focus::Message
    } else if view.files.contains(point) {
        Focus::Files
    } else {
        return;
    };
    let details = &mut app.commit_details;
    if pane == Focus::Diff {
        return;
    }
    if matches!(
        kind,
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
    ) {
        return;
    }
    let (position, max) = match pane {
        Focus::Message => (
            &mut details.message_scroll,
            details
                .message_rows
                .saturating_sub(view.overview.height as usize),
        ),
        _ => (
            &mut details.review.scroll,
            details
                .review
                .total
                .saturating_sub(view.files.height as usize),
        ),
    };
    *position = if kind == MouseEventKind::ScrollUp {
        position.saturating_sub(3)
    } else {
        position.saturating_add(3).min(max)
    };
}

fn scrollbar_pane(app: &App, view: &InspectorLayout, focus: Focus) -> (Rect, usize, usize) {
    let details = &app.commit_details;
    match focus {
        Focus::Files => (view.files, details.review.total, details.review.scroll),
        Focus::Message => (view.overview, details.message_rows, details.message_scroll),
        _ => (Rect::default(), 0, 0),
    }
}

// Match VerticalScrollbar's geometry so the thumb stays under the pointer.
fn scrollbar_thumb(height: u16, count: usize, offset: usize) -> (u16, u16) {
    let size = ((height as f64 / count.max(1) as f64) * height as f64).max(1.0) as u16;
    let max = count.saturating_sub(height as usize);
    let position = if max > 0 {
        (offset.min(max) as f64 / max as f64 * height.saturating_sub(size) as f64) as u16
    } else {
        0
    };
    (position, size)
}

fn drag_scrollbar(app: &mut App, view: &InspectorLayout, row: u16) {
    let Some((focus, grab)) = app.commit_details.scrollbar_drag else {
        return;
    };
    let (area, count, offset) = scrollbar_pane(app, view, focus);
    let (_, size) = scrollbar_thumb(area.height, count, offset);
    let track = area.height.saturating_sub(size);
    if track == 0 {
        return;
    }
    let top = row.saturating_sub(area.y.saturating_add(grab)).min(track);
    let max = count.saturating_sub(area.height as usize);
    let offset = (top as f64 / track as f64 * max as f64).round() as usize;
    let details = &mut app.commit_details;
    match focus {
        Focus::Files => {
            details.review.scroll = offset;
        }
        Focus::Message => details.message_scroll = offset,
        _ => {}
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.commit_details.visible || obscured(app) {
        return false;
    }
    let view = current_layout(app);
    let captured =
        app.commit_details.divider_drag.is_some() || app.commit_details.scrollbar_drag.is_some();
    // Workbench drags retain ownership across this editor view, including
    // moving tabs and resizing the surrounding editor groups or sidebar.
    if !captured && matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
        return false;
    }
    if !view.area.contains((mouse.column, mouse.row).into())
        && !(captured
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            ))
    {
        return false;
    }
    let Some(group) = app.commit_details_tab_group() else {
        return false;
    };
    if mouse.kind == MouseEventKind::Moved && !view.patch.contains((mouse.column, mouse.row).into())
    {
        app.reset_hover_states();
        app.clear_transient_editor_state();
        app.mouse_hover_changed = true;
    }
    if matches!(mouse.kind, MouseEventKind::Down(_)) || captured {
        app.search_input_focused = false;
        app.auto_save_on_focus_change();
        app.switch_group(group);
        app.sync_diff_tab();
        app.active_pane = crate::app::ActivePane::Editor;
        app.terminal_panel_focused = false;
        if !view
            .context_count
            .contains((mouse.column, mouse.row).into())
        {
            apply_context_draft(app);
        }
    }
    let point = (mouse.column, mouse.row).into();
    if !captured
        && view.files.contains(point)
        && (matches!(
            mouse.kind,
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
        ) || mouse.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ))
    {
        if let Some(card) = app
            .commit_details
            .review
            .visible
            .iter()
            .find(|card| card.body.contains(point))
            .copied()
        {
            let geometry = crate::commit_review::Geometry::capture(app);
            if app.is_commit_details_tab() && app.commit_details.diff_file == Some(card.file) {
                card.geometry.install(app);
            }
            app.pan_commit_review_file(
                card.file,
                mouse.column,
                mouse.row,
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollLeft
                ) {
                    -5
                } else {
                    5
                },
            );
            geometry.install(app);
        }
        return true;
    }
    // Wheel motion belongs to the continuous review even over native code.
    // Shift-wheel and horizontal wheel still reach the native text panning.
    if !captured
        && view.files.contains(point)
        && matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        )
        && !mouse.modifiers.contains(KeyModifiers::SHIFT)
    {
        scroll(app, mouse.kind, mouse.column, mouse.row);
        return true;
    }
    if !captured
        && mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && let Some(card) = app
            .commit_details
            .review
            .visible
            .iter()
            .find(|card| card.header.contains(point) || card.body.contains(point))
            .copied()
    {
        review::click_card(app, mouse.column, mouse.row);
        if card.body.contains(point)
            && app.commit_details.diff_file == Some(card.file)
            && app.commit_details.selected_file() == Some(card.file)
            && !app.commit_details.review.collapsed.contains(&card.file)
            && app.commit_details.diff_full_cache.is_some()
        {
            if let Some(layout) = &mut app.commit_details.layout {
                layout.patch = card.body;
            }
            return false;
        }
        return true;
    }
    if !captured
        && view.patch.contains((mouse.column, mouse.row).into())
        && diff_loaded(app)
        && (group != app.active_group || native_diff_ready(app))
    {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            app.commit_details.focus = Focus::Diff;
        }
        return false;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.commit_details.scrollbar_drag = None;
            app.commit_details.divider_drag = None;
            for (divider, area) in [(Divider::Primary, view.primary_divider)] {
                if !area.contains((mouse.column, mouse.row).into()) {
                    continue;
                }
                let previous = app.commit_details.last_divider_click.take();
                if previous
                    .is_some_and(|(last, time)| last == divider && time.elapsed().as_millis() < 400)
                {
                    *pane_ratio(&mut app.commit_details.panes) =
                        *pane_ratio(&mut PaneSizes::default());
                    app.commit_details.layout = None;
                } else {
                    app.commit_details.divider_drag = Some(divider);
                    app.commit_details.last_divider_click =
                        Some((divider, std::time::Instant::now()));
                }
                return true;
            }
            app.commit_details.last_divider_click = None;
            for focus in [Focus::Files, Focus::Message] {
                let (area, count, offset) = scrollbar_pane(app, &view, focus);
                if count <= area.height as usize
                    || area.width == 0
                    || area.height == 0
                    || mouse.column != area.right() - 1
                    || !(area.y..area.bottom()).contains(&mouse.row)
                {
                    continue;
                }
                let (position, size) = scrollbar_thumb(area.height, count, offset);
                let y = mouse.row - area.y;
                let on_thumb = (position..position + size).contains(&y);
                app.commit_details.scrollbar_drag =
                    Some((focus, if on_thumb { y - position } else { size / 2 }));
                app.commit_details.focus = focus;
                if !on_thumb {
                    drag_scrollbar(app, &view, mouse.row);
                }
                return true;
            }
            click(app, mouse.column, mouse.row);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.commit_details.last_divider_click = None;
            if app.commit_details.divider_drag.is_some() {
                resize_pane(app, &view, mouse.column, mouse.row);
            } else {
                drag_scrollbar(app, &view, mouse.row);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.commit_details.scrollbar_drag = None;
            app.commit_details.divider_drag = None;
        }
        _ => scroll(app, mouse.kind, mouse.column, mouse.row),
    }
    true
}

fn row(f: &mut Frame, area: Rect, text: impl Into<String>, style: Style) {
    if area.width > 0 && area.height > 0 {
        f.render_widget(
            Paragraph::new(truncate_to_width(&text.into(), area.width as usize)).style(style),
            area,
        );
    }
}

fn display_path(path: &str, width: usize) -> String {
    if UnicodeWidthStr::width(path) <= width {
        return path.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut suffix = Vec::new();
    let mut used = 1;
    for grapheme in path.graphemes(true).rev() {
        let cells = UnicodeWidthStr::width(grapheme);
        if used + cells > width {
            break;
        }
        suffix.push(grapheme);
        used += cells;
    }
    format!("…{}", suffix.into_iter().rev().collect::<String>())
}

#[cfg(test)]
pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    draw_in_area(f, app, f.area());
}

pub(crate) fn draw_in_area(f: &mut Frame, app: &mut App, area: Rect) {
    app.commit_details
        .expire_copy_feedback(std::time::Instant::now());
    if !app.rendering_inactive_editor {
        app.sync_diff_tab();
        app.refresh_commit_diff_search_context();
    }
    let view = layout_in_area(
        area,
        app.commit_details.diff_expanded,
        app.commit_details.panes,
    );
    app.commit_details.visible = true;
    app.commit_details.area = area;
    app.code_area = None;
    app.gutter_area = None;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    let count = app.commit_details.filtered_files().len();
    app.commit_details.selected = app.commit_details.selected.min(count.saturating_sub(1));
    app.commit_details.layout = Some(view.clone());
    let focus_visible = match app.commit_details.focus {
        Focus::Files => view.files.height > 0,
        Focus::Diff => view.files.height > 0,
        Focus::Message => view.overview.height > 0,
        Focus::Filter => view.filter.height > 0,
        Focus::Action(index) => actions(&view)
            .any(|(action, area)| action == index && area.width > 0 && enabled(app, action)),
    };
    if !focus_visible {
        app.commit_details.focus = if view.files.height > 0 {
            Focus::Files
        } else if view.patch.height > 0 {
            Focus::Diff
        } else if view.overview.height > 0 {
            Focus::Message
        } else {
            Focus::Files
        };
    }
    let bg = app.colors.bg_editor;
    let fg = app.colors.ui.widget_text_color(bg);
    let muted = app.colors.ui.description_color(bg);
    let base = Style::default().fg(fg).bg(bg);
    let subtle = base.fg(muted);
    let strong = base.add_modifier(Modifier::BOLD);
    let details = &app.commit_details;
    f.render_widget(Block::default().style(base), view.area);
    let hash = details.data.hash.get(..7).unwrap_or(&details.data.hash);
    row(
        f,
        view.title,
        if hash.is_empty() {
            "Commit details".into()
        } else {
            format!("Commit {hash}")
        },
        strong,
    );
    for (divider, area) in [(Divider::Primary, view.primary_divider)] {
        if area.width == 0 || area.height == 0 {
            continue;
        }
        let hovered = area.contains((app.mouse_col, app.mouse_row).into());
        let active = details.divider_drag == Some(divider);
        let color = if active {
            app.colors.ui.focus_border.border(bg)
        } else if hovered {
            app.colors.ui.sash_hover_border.over(bg)
        } else {
            app.colors.ui.widget_border.border(bg)
        }
        .unwrap_or(muted);
        let style = base.fg(app.colors.ui.readable_foreground(color, bg, 4.5));
        row(f, area, "─".repeat(area.width as usize), style);
    }
    let labels = if view.compact {
        COMPACT_ACTIONS
    } else {
        ACTIONS
    };
    for (i, area) in view.buttons.iter().enumerate() {
        let feedback = details
            .copy_feedback
            .as_ref()
            .filter(|feedback| feedback.action == i);
        let label = if let Some(feedback) = feedback {
            match (feedback.copied, view.compact) {
                (true, true) => "✓",
                (true, false) => "✓ Copied",
                (false, true) => "!",
                (false, false) => "Failed",
            }
        } else if i == 5 && details.diff_expanded {
            if view.compact { "Info" } else { "Commit info" }
        } else {
            labels[i]
        };
        let action = if i == 6 { 8 } else { i };
        let focused = details.focus == Focus::Action(action);
        let mut style = app.colors.ui.button_style(
            bg,
            i == 4,
            focused || area.contains((app.mouse_col, app.mouse_row).into()),
            enabled(app, action),
        );
        if let Some(feedback) = feedback {
            let background = style.bg.unwrap_or(bg);
            let foreground = if feedback.copied {
                app.colors
                    .ui
                    .readable_foreground(app.colors.fg_success, background, 4.5)
            } else {
                app.colors.ui.error_text_color(background)
            };
            style = style.fg(foreground).add_modifier(Modifier::BOLD);
        }
        row(
            f,
            *area,
            format!(" {label} "),
            if focused {
                style.add_modifier(Modifier::UNDERLINED)
            } else {
                style
            },
        );
    }
    if let Some(feedback) = &details.copy_feedback {
        let text = copy_feedback_text(feedback.action, feedback.copied);
        let (text, foreground) = if feedback.copied {
            (
                format!("✓ {text}"),
                app.colors
                    .ui
                    .readable_foreground(app.colors.fg_success, bg, 4.5),
            )
        } else {
            (text.into(), app.colors.ui.error_text_color(bg))
        };
        row(f, view.feedback, text, strong.fg(foreground));
    }

    let width = view.overview.width.saturating_sub(1) as usize;
    let mut overview = Vec::new();
    let (subject, body) = details
        .data
        .message
        .split_once('\n')
        .unwrap_or((&details.data.message, ""));
    for line in wrapped(
        if subject.is_empty() {
            "(no commit message)"
        } else {
            subject
        },
        width,
    ) {
        overview.push(Line::from(Span::styled(line, strong)));
    }
    let author = if details.data.email.is_empty() {
        details.data.author.clone()
    } else {
        format!("{} <{}>", details.data.author, details.data.email)
    };
    for line in wrapped(&format!("{author} · {}", details.data.time_ago), width) {
        overview.push(Line::from(Span::styled(line, base)));
    }
    let mut metadata = details.data.date.clone();
    if !details.loading && details.error.is_none() {
        let parent = match details.data.parents.as_slice() {
            [] => "Root commit · compared with empty tree".into(),
            [parent] => format!("Parent {}", parent.get(..7).unwrap_or(parent)),
            parents => format!(
                "Merge · {} parents · compared with {}",
                parents.len(),
                parents[0].get(..7).unwrap_or(&parents[0])
            ),
        };
        if !metadata.is_empty() {
            metadata.push_str(" · ");
        }
        metadata.push_str(&parent);
    }
    for line in wrapped(&metadata, width) {
        overview.push(Line::from(Span::styled(line, subtle)));
    }
    let mut refs = Line::default();
    for reference in &details.data.references {
        let badge = Span::styled(
            format!(
                " {} ",
                truncate_to_width(&reference.name, width.saturating_sub(2))
            ),
            app.colors.ui.graph_reference_style(reference.remote, bg),
        );
        if refs.width() + badge.width() + 1 > width && !refs.spans.is_empty() {
            overview.push(refs);
            refs = Line::default();
        }
        refs.spans.push(badge);
        refs.spans.push(Span::styled(" ", base));
    }
    if !refs.spans.is_empty() {
        overview.push(refs);
    }
    if !body.trim().is_empty() {
        overview.push(Line::from(""));
        for line in wrapped(body.trim(), width) {
            overview.push(Line::from(Span::styled(line, base)));
        }
    }
    app.commit_details.message_rows = overview.len();
    app.commit_details.message_scroll = app
        .commit_details
        .message_scroll
        .min(overview.len().saturating_sub(view.overview.height as usize));
    f.render_widget(
        Paragraph::new(overview).style(base).scroll((
            app.commit_details.message_scroll.min(u16::MAX as usize) as u16,
            0,
        )),
        view.overview,
    );
    let details = &app.commit_details;
    let filtered = details.filtered_files();
    let added: usize = details.data.files.iter().map(|f| f.added).sum();
    let deleted: usize = details.data.files.iter().map(|f| f.deleted).sum();
    let count = if details.query.is_empty() {
        details.data.files.len().to_string()
    } else {
        format!("{}/{}", filtered.len(), details.data.files.len())
    };
    let summary = format!(
        "{}Changed files  {count}",
        if details.focus == Focus::Files {
            "▸ "
        } else {
            "  "
        }
    );
    let summary_stats = format!(" +{added} −{deleted}");
    let stats_width = UnicodeWidthStr::width(summary_stats.as_str()) as u16;
    let summary_width = if view.collapse_all.width > 0 {
        view.collapse_all.x.saturating_sub(view.heading.x)
    } else {
        view.heading.width
    };
    let summary_area = Rect::new(
        view.heading.x,
        view.heading.y,
        summary_width,
        view.heading.height,
    );
    row(f, summary_area, &summary, strong);
    if summary_area.width > UnicodeWidthStr::width(summary.as_str()) as u16 + stats_width + 1 {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("+{added}"),
                    base.fg(app.colors.ui.git_status_color('A', bg)),
                ),
                Span::styled(
                    format!(" −{deleted}"),
                    base.fg(app.colors.ui.git_status_color('D', bg)),
                ),
            ])),
            Rect::new(
                summary_area.right() - stats_width + 1,
                view.heading.y,
                stats_width - 1,
                1,
            ),
        );
    }
    let filter_focused = details.focus == Focus::Filter;
    let input_style = app.colors.ui.input_style(bg);
    let filter_bg = input_style.bg.unwrap_or(bg);
    let filter_fg = input_style.fg.unwrap_or(fg);
    let show_clear = !details.query.is_empty() && view.filter_clear.width > 0;
    let filter_area = Rect::new(
        view.filter.x,
        view.filter.y,
        view.filter.width.saturating_sub(if show_clear {
            view.filter_clear.width
        } else {
            0
        }),
        view.filter.height,
    );
    // Keep the caret visible even for long Unicode queries.
    let prefix = if view.filter.width >= 20 { " / " } else { " " };
    let budget = filter_area.width.saturating_sub(prefix.len() as u16 + 1) as usize;
    let before = &details.query[..details.query_cursor.min(details.query.len())];
    let mut start = 0;
    while UnicodeWidthStr::width(&before[start..]) >= budget.max(1) {
        let Some((offset, g)) = before[start..].grapheme_indices(true).next() else {
            break;
        };
        start += offset + g.len();
    }
    let filter_style = base.bg(filter_bg).fg(filter_fg);
    let selected = App::inline_selection_range(details.query_cursor, details.query_anchor);
    let mut spans = vec![Span::styled(prefix, filter_style)];
    let mut hits = vec![start; prefix.len()];
    let mut used = prefix.len();
    if details.query.is_empty() && !filter_focused {
        spans.push(Span::styled(
            "Filter files…  /",
            filter_style.fg(app.colors.ui.input_placeholder_color(filter_bg)),
        ));
    } else {
        for (offset, g) in details.query[start..].grapheme_indices(true) {
            let w = UnicodeWidthStr::width(g);
            if used + w >= filter_area.width as usize {
                break;
            }
            let byte = start + offset;
            let style = if filter_focused && byte == details.query_cursor {
                app.colors.ui.input_caret_style(filter_bg)
            } else if selected.is_some_and(|(a, b)| byte >= a && byte < b) {
                app.colors.ui.input_selection_style(filter_bg)
            } else {
                filter_style
            };
            spans.push(Span::styled(g.to_owned(), style));
            hits.extend(std::iter::repeat_n(byte, w));
            used += w;
        }
        if filter_focused && details.query_cursor == details.query.len() {
            spans.push(Span::styled(
                " ",
                app.colors.ui.input_caret_style(filter_bg),
            ));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(filter_style),
        filter_area,
    );
    app.commit_details.query_hit = Some(crate::text_input::TextInputHitMap::new(
        filter_area,
        hits,
        app.commit_details.query.len(),
    ));
    if show_clear {
        row(
            f,
            view.filter_clear,
            " × ",
            app.colors.ui.toolbar_layer_style(
                app.colors.ui.widget_foreground,
                filter_bg,
                view.filter_clear
                    .contains((app.mouse_col, app.mouse_row).into()),
                false,
                false,
            ),
        );
    }
    review::draw_controls(f, app, &view);
    review::draw(f, app, &view);
    let details = &app.commit_details;
    for focus in [Focus::Message] {
        let (area, count, offset) = scrollbar_pane(app, &view, focus);
        if count > area.height as usize && area.width > 0 && area.height > 0 {
            let (track, thumb) = app.colors.ui.scrollbar_colors(
                bg,
                app.mouse_col == area.right() - 1
                    && (area.y..area.bottom()).contains(&app.mouse_row),
                details
                    .scrollbar_drag
                    .is_some_and(|(pane, _)| pane == focus),
            );
            crate::ui_widgets::VerticalScrollbar::new(
                area.right() - 1,
                area.y,
                area.height,
                count,
                offset,
            )
            .symbols('┃', '│')
            .colors(thumb, track, bg)
            .render(f);
        }
    }
}
