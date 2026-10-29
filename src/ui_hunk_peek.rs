//! Gutter change peek: a read-only view zone inserted into the rendered editor.
//! Mouse/keyboard input is captured while open, so the underlying editor's
//! document-to-screen mapping never receives coordinates from the shifted rows.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
};
use similar::DiffableStr;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{app::App, app_git_display::GitFilePatch};

pub(crate) struct PeekRow {
    pub kind: char,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub text: String,
    pub highlighted: crate::highlight::HighlightSpans,
    emphasis: Option<Vec<bool>>,
    partner: Option<usize>,
    prepared: bool,
}

pub(crate) struct PeekHunk {
    pub start_row: usize,
    pub end_row: usize,
    pub action_line: Option<usize>,
}

pub(crate) struct UnsavedHunkSnapshot {
    pub document: u64,
    pub version: u64,
    pub saved: ropey::Rope,
    pub current: ropey::Rope,
}

#[derive(Clone, Copy, Default)]
struct PeekLayout {
    panel: Rect,
    body: Rect,
    code: Rect,
    quick_actions: Rect,
    vertical: Rect,
    horizontal: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuickAction {
    area: Rect,
    hunk: usize,
    button: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverTarget {
    Toolbar(usize),
    QuickAction(QuickAction),
    VerticalScroll,
    HorizontalScroll,
}

#[derive(Clone, Copy)]
struct ScrollDrag {
    horizontal: bool,
    grab_offset: usize,
}

pub(crate) struct HunkPeek {
    pub source: GitFilePatch,
    pub unsaved: Option<UnsavedHunkSnapshot>,
    pub remote: Option<crate::remote::hunk::Snapshot>,
    pub hunks: Vec<PeekHunk>,
    pub rows: Vec<PeekRow>,
    pub selected: usize,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub focused_button: Option<usize>,
    digits: usize,
    max_width: usize,
    tab_size: usize,
    layout: Option<PeekLayout>,
    drag: Option<ScrollDrag>,
    hovered: Option<HoverTarget>,
}

fn range_start(start: usize, count: usize) -> usize {
    if count == 0 {
        start
    } else {
        start.saturating_sub(1)
    }
}

fn text_width(text: &str, tab_size: usize) -> usize {
    text.chars().fold(0, |col, ch| {
        col + if ch == '\t' {
            tab_size - col % tab_size
        } else {
            ch.width().unwrap_or(0)
        }
    })
}

impl HunkPeek {
    pub(crate) fn for_line(
        patch: &GitFilePatch,
        target: &str,
        line: usize,
        tab_size: usize,
    ) -> Result<Self, String> {
        let selected = patch.hunks.iter().position(|hunk| {
            crate::app_git_display::line_number_in_hunk(
                line.saturating_add(1), hunk.new_start, hunk.new_count,
            )
        }).ok_or("No change found at this line")?;
        let mut peek = Self::with_tab_size(patch, target, selected, tab_size);
        peek.hunks[selected].action_line = Some(line);
        Ok(peek)
    }

    pub(crate) fn from_texts(
        saved: &str,
        current: &str,
        line: usize,
        tab_size: usize,
    ) -> Result<Self, String> {
        let diff = similar::TextDiff::configure()
            .timeout(std::time::Duration::from_millis(50))
            .diff_lines(saved, current);
        // Retain the diff's ranges directly instead of serializing and parsing
        // a patch. Lone-CR files also need one display row per diff line.
        let hunks = diff.grouped_ops(0).into_iter().map(|ops| {
            let old = ops.first().unwrap().old_range().start..ops.last().unwrap().old_range().end;
            let new = ops.first().unwrap().new_range().start..ops.last().unwrap().new_range().end;
            let old_start = old.start + usize::from(!old.is_empty());
            let new_start = new.start + usize::from(!new.is_empty());
            let mut text = format!("@@ -{},{} +{},{} @@\n", old_start, old.len(), new_start, new.len());
            for change in ops.iter().flat_map(|op| diff.iter_changes(op)) {
                text.push(match change.tag() {
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Equal => ' ',
                });
                text.push_str(change.value().trim_end_matches(['\r', '\n']));
                text.push('\n');
            }
            crate::app_git_display::GitDiffHunk {
                old_start, old_count: old.len(), new_start, new_count: new.len(), text,
            }
        }).collect::<Vec<_>>();
        if hunks.is_empty() { return Err("No changes found".into()); }
        let patch = GitFilePatch { header: String::new(), hunks };
        Self::for_line(&patch, current, line, tab_size)
    }

    #[cfg(test)]
    pub(crate) fn from_patch(patch: &GitFilePatch, target: &str, selected: usize) -> Self {
        Self::with_tab_size(patch, target, selected, 4)
    }

    pub(crate) fn with_tab_size(
        patch: &GitFilePatch,
        target: &str,
        selected: usize,
        tab_size: usize,
    ) -> Self {
        let tab_size = tab_size.max(1);
        let target = target.tokenize_lines();
        let mut rows = Vec::with_capacity(target.len());
        let mut hunks = Vec::with_capacity(patch.hunks.len());
        let mut old = 0;
        let mut new = 0;
        let mut max_width = 0;
        let mut push =
            |rows: &mut Vec<PeekRow>, kind, text: &str, old: &mut usize, new: &mut usize| {
                let text = text.trim_end_matches(['\r', '\n']);
                max_width = max_width.max(text_width(text, tab_size));
                rows.push(PeekRow {
                    kind,
                    old_line: (kind != '+').then_some(*old + 1),
                    new_line: (kind != '-').then_some(*new + 1),
                    text: text.to_string(),
                    highlighted: Vec::new(),
                    emphasis: None,
                    partner: None,
                    prepared: false,
                });
                if kind != '+' {
                    *old += 1;
                }
                if kind != '-' {
                    *new += 1;
                }
            };
        for hunk in &patch.hunks {
            let start = range_start(hunk.new_start, hunk.new_count);
            while new < start.min(target.len()) {
                push(&mut rows, ' ', target[new], &mut old, &mut new);
            }
            old = range_start(hunk.old_start, hunk.old_count);
            let start_row = rows.len();
            let mut removed = Vec::new();
            let mut added = Vec::new();
            for line in hunk.text.lines().skip(1) {
                if let Some(kind @ ('+' | '-' | ' ')) = line.chars().next() {
                    match kind {
                        '-' => removed.push(rows.len()),
                        '+' => added.push(rows.len()),
                        _ => {}
                    }
                    push(&mut rows, kind, &line[1..], &mut old, &mut new);
                }
            }
            // Pair rows now, but defer character diffing until a row is visible.
            for (left, right) in removed.into_iter().zip(added) {
                rows[left].partner = Some(right);
                rows[right].partner = Some(left);
            }
            hunks.push(PeekHunk {
                start_row,
                end_row: rows.len(),
                action_line: Some(hunk.new_start.saturating_sub(1)),
            });
        }
        while new < target.len() {
            push(&mut rows, ' ', target[new], &mut old, &mut new);
        }
        let digits = old.max(new).max(1).to_string().len().max(2);
        let scroll = hunks[selected].start_row.saturating_sub(3);
        Self {
            source: patch.clone(),
            unsaved: None,
            remote: None,
            hunks,
            rows,
            selected,
            scroll,
            horizontal_scroll: 0,
            focused_button: None,
            digits,
            max_width,
            tab_size,
            layout: None,
            drag: None,
            hovered: None,
        }
    }

    fn prepare_emphasis(&mut self, index: usize) {
        if self.rows[index].prepared {
            return;
        }
        self.rows[index].prepared = true;
        let Some(partner) = self.rows[index].partner else {
            return;
        };
        self.rows[partner].prepared = true;
        let (left, right) = if self.rows[index].kind == '-' {
            (index, partner)
        } else {
            (partner, index)
        };
        if self.rows[left].text.len() + self.rows[right].text.len() > 4096 {
            return;
        }
        let diff = similar::TextDiff::configure()
            .timeout(std::time::Duration::from_millis(2))
            .diff_chars(&self.rows[left].text, &self.rows[right].text);
        let mut old_mask = Vec::new();
        let mut new_mask = Vec::new();
        for change in diff.iter_all_changes() {
            let count = change.value().chars().count();
            match change.tag() {
                similar::ChangeTag::Equal => {
                    old_mask.extend(std::iter::repeat_n(false, count));
                    new_mask.extend(std::iter::repeat_n(false, count));
                }
                similar::ChangeTag::Delete => old_mask.extend(std::iter::repeat_n(true, count)),
                similar::ChangeTag::Insert => new_mask.extend(std::iter::repeat_n(true, count)),
            }
        }
        self.rows[left].emphasis = Some(old_mask);
        self.rows[right].emphasis = Some(new_mask);
    }

    fn reveal_selected(&mut self) {
        let hunk = &self.hunks[self.selected];
        // Keep a few context rows visible without scrolling a short change
        // off the bottom of a very small peek viewport.
        let height = self.layout.map_or(12, |l| l.body.height.max(1) as usize);
        let context = height.saturating_sub(hunk.end_row - hunk.start_row).min(3);
        self.scroll = hunk.start_row.saturating_sub(context);
        self.horizontal_scroll = 0;
        self.drag = None;
        self.layout = None;
    }
}

pub(crate) fn peek_rect(app: &App, width: u16, height: u16) -> Rect {
    let bounds = Rect::new(0, 0, width, height);
    let editor = app
        .gutter_area
        .zip(app.code_area)
        .map(|(gutter, code)| {
            Rect::new(
                gutter.x,
                gutter.y,
                code.right().saturating_sub(gutter.x),
                code.height,
            )
        })
        .or(app.editor_area)
        .unwrap_or(bounds)
        .intersection(bounds);
    if editor.is_empty() {
        return Rect::new(editor.x.min(width), editor.y.min(height), 0, 0);
    }
    let rows = app.hunk_peek.as_ref().map_or_else(
        || app.hunk_preview_feedback.as_ref().map_or(1, |feedback| if feedback.loading() { 3 } else { 4 }),
        |peek| peek.rows.len(),
    );
    let panel_height = (rows.saturating_add(3).min(18) as u16)
        .min((editor.height / 2).max(5))
        .min(editor.height);
    let y = app
        .hunk_popup_pos
        .1
        .saturating_add(1)
        .clamp(editor.top(), editor.bottom().saturating_sub(panel_height));
    Rect::new(editor.x, y, editor.width, panel_height)
}

fn make_layout(app: &App, panel: Rect) -> PeekLayout {
    let Some(peek) = &app.hunk_peek else {
        return PeekLayout { panel, ..PeekLayout::default() };
    };
    let numbers = (peek.digits * 2 + 3).min(panel.width as usize) as u16;
    // Reserve the buttons on one row, including for one-line additions and
    // deletions. Tiny panes retain the existing toolbar instead of hiding code.
    let action_width = if matches!(app.hunk_popup_status, 'N' | 'S') { 3 } else { 6 };
    let quick_width = if panel.width >= numbers + action_width + 2 { action_width } else { 0 };
    let gutter = numbers + quick_width;
    let code_width = panel.width.saturating_sub(gutter + 1);
    let horizontal = code_width > 0 && peek.max_width > code_width as usize && panel.height > 4;
    let body = Rect::new(
        panel.x,
        panel.y.saturating_add(2),
        panel.width,
        panel.height.saturating_sub(3 + u16::from(horizontal)),
    );
    let code = Rect::new(panel.x + gutter, body.y, code_width, body.height);
    let quick_actions = Rect::new(panel.x + numbers, body.y, quick_width, body.height);
    let vertical = if body.width > 0 && peek.rows.len() > body.height as usize {
        Rect::new(body.right() - 1, body.y, 1, body.height)
    } else {
        Rect::default()
    };
    let horizontal = if horizontal {
        Rect::new(code.x, body.bottom(), code.width, 1)
    } else {
        Rect::default()
    };
    PeekLayout {
        panel,
        body,
        code,
        quick_actions,
        vertical,
        horizontal,
    }
}

fn input_layout(app: &App) -> PeekLayout {
    app.hunk_peek
        .as_ref()
        .and_then(|p| p.layout)
        .or_else(|| app.hunk_preview_feedback.as_ref()?.area.map(|panel| make_layout(app, panel)))
        .unwrap_or_else(|| make_layout(app, peek_rect(app, u16::MAX, u16::MAX)))
}

fn thumb(total: usize, visible: usize, track: usize, scroll: usize) -> (usize, usize) {
    if track == 0 {
        return (0, 0);
    }
    let length = (track.saturating_mul(visible) / total.max(1)).clamp(1, track);
    let max_scroll = total.saturating_sub(visible);
    let start = scroll.min(max_scroll).saturating_mul(track - length) / max_scroll.max(1);
    (start, length)
}

#[allow(clippy::too_many_arguments)]
fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    total: usize,
    visible: usize,
    scroll: usize,
    horizontal: bool,
    normal: Style,
    thumb_color: Color,
    track_color: Color,
) {
    if area.is_empty() {
        return;
    }
    let track = if horizontal { area.width } else { area.height } as usize;
    let (start, length) = thumb(total, visible, track, scroll);
    let buffer = f.buffer_mut();
    for offset in 0..track {
        let x = area.x + if horizontal { offset as u16 } else { 0 };
        let y = area.y + if horizontal { 0 } else { offset as u16 };
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_symbol(if horizontal { "━" } else { "┃" });
            cell.set_style(normal.fg(if (start..start + length).contains(&offset) {
                thumb_color
            } else {
                track_color
            }));
        }
    }
}

fn scrollbar_down(app: &mut App, layout: PeekLayout, col: u16, row: u16) -> bool {
    let horizontal = layout.horizontal.contains((col, row).into());
    if !horizontal && !layout.vertical.contains((col, row).into()) {
        return false;
    }
    let Some(peek) = app.hunk_peek.as_mut() else {
        return false;
    };
    let (total, visible, track, offset, scroll) = if horizontal {
        (
            peek.max_width,
            layout.code.width as usize,
            layout.horizontal.width as usize,
            col.saturating_sub(layout.horizontal.x) as usize,
            peek.horizontal_scroll,
        )
    } else {
        (
            peek.rows.len(),
            layout.body.height as usize,
            layout.vertical.height as usize,
            row.saturating_sub(layout.vertical.y) as usize,
            peek.scroll,
        )
    };
    let (start, length) = thumb(total, visible, track, scroll);
    if (start..start + length).contains(&offset) {
        peek.drag = Some(ScrollDrag {
            horizontal,
            grab_offset: offset - start,
        });
    } else {
        let value = if horizontal {
            &mut peek.horizontal_scroll
        } else {
            &mut peek.scroll
        };
        *value = if offset < start {
            value.saturating_sub(visible)
        } else {
            value
                .saturating_add(visible)
                .min(total.saturating_sub(visible))
        };
    }
    true
}

fn drag_scrollbar(app: &mut App, layout: PeekLayout, col: u16, row: u16) {
    let Some(peek) = app.hunk_peek.as_mut() else {
        return;
    };
    let Some(drag) = peek.drag else {
        return;
    };
    let (total, visible, track, offset) = if drag.horizontal {
        (
            peek.max_width,
            layout.code.width as usize,
            layout.horizontal.width as usize,
            col.saturating_sub(layout.horizontal.x) as usize,
        )
    } else {
        (
            peek.rows.len(),
            layout.body.height as usize,
            layout.vertical.height as usize,
            row.saturating_sub(layout.vertical.y) as usize,
        )
    };
    let (_, length) = thumb(total, visible, track, 0);
    let travel = track.saturating_sub(length);
    let offset = offset.saturating_sub(drag.grab_offset).min(travel);
    let value = offset.saturating_mul(total.saturating_sub(visible)) / travel.max(1);
    if drag.horizontal {
        peek.horizontal_scroll = value;
    } else {
        peek.scroll = value;
    }
}

fn buttons(app: &App) -> &'static [(&'static str, &'static str)] {
    if let Some(feedback) = &app.hunk_preview_feedback {
        return if feedback.loading() {
            &[("×", "Close Preview")]
        } else {
            &[("Retry", "Retry Preview"), ("×", "Close Preview")]
        };
    }
    if app.hunk_popup_status == 'N' {
        &[
            ("−", "Revert Unsaved Hunk"),
            ("↓", "Next Change"),
            ("↑", "Previous Change"),
            ("×", "Close Preview"),
        ]
    } else if app.hunk_popup_status == 'S' {
        &[
            ("↶", "Revert Hunk"),
            ("↓", "Next Change"),
            ("↑", "Previous Change"),
            ("×", "Close Preview"),
        ]
    } else {
        &[
            ("+", "Stage Hunk"),
            ("↶", "Discard Hunk"),
            ("↓", "Next Change"),
            ("↑", "Previous Change"),
            ("×", "Close Preview"),
        ]
    }
}

fn button_rect(area: Rect, index: usize, count: usize) -> Rect {
    let width = 3.min(area.width / count as u16);
    Rect::new(
        area.right().saturating_sub((count - index) as u16 * width),
        area.y.saturating_add(1),
        width,
        u16::from(area.height > 1),
    )
}

fn button_at(app: &App, area: Rect, col: u16, row: u16) -> Option<usize> {
    let actions = buttons(app);
    (0..actions.len()).find(|&index| {
        let button = if app.hunk_preview_feedback.is_some() {
            feedback_button_rect(area, index, actions)
        } else {
            button_rect(area, index, actions.len())
        };
        button.contains((col, row).into())
    })
}

fn feedback_button_rect(area: Rect, index: usize, actions: &[(&str, &str)]) -> Rect {
    let trailing: u16 = actions[index + 1..].iter().map(|(label, _)| (label.width() + 2) as u16).sum();
    let right = area.right().saturating_sub(trailing).max(area.x);
    let x = right.saturating_sub((actions[index].0.width() + 2) as u16).max(area.x);
    Rect::new(x, area.y.saturating_add(1), right - x, u16::from(area.height >= 4))
}

fn quick_actions(
    peek: &HunkPeek,
    layout: PeekLayout,
    revert_only: bool,
) -> impl Iterator<Item = QuickAction> + '_ {
    let count = if layout.quick_actions.is_empty() {
        0
    } else if revert_only {
        1
    } else {
        2
    };
    let end = peek.scroll.saturating_add(layout.body.height as usize).min(peek.rows.len());
    let first = peek.hunks.partition_point(|hunk| hunk.end_row <= peek.scroll);
    // Inspect only visible hunks/rows. A partially scrolled hunk keeps its
    // controls on its first visible changed row, beside the code they affect.
    peek.hunks.iter().enumerate().skip(first)
        .take_while(move |(_, hunk)| count > 0 && hunk.start_row < end)
        .filter_map(move |(index, hunk)| {
            let start = hunk.start_row.max(peek.scroll);
            let offset = peek.rows[start..hunk.end_row.min(end)].iter()
                .position(|row| row.kind != ' ')?;
            Some((index, layout.body.y + (start + offset - peek.scroll) as u16))
        })
        .flat_map(move |(hunk, y)| (0..count).map(move |button| QuickAction {
            area: Rect::new(layout.quick_actions.x + button as u16 * 3, y, 3, 1),
            hunk,
            button,
        }))
}

fn quick_action_at(app: &App, layout: PeekLayout, col: u16, row: u16) -> Option<QuickAction> {
    if !layout.quick_actions.contains((col, row).into()) {
        return None;
    }
    quick_actions(app.hunk_peek.as_ref()?, layout, matches!(app.hunk_popup_status, 'N' | 'S'))
        .find(|action| action.area.contains((col, row).into()))
}

fn hover_target(app: &App, layout: PeekLayout, col: u16, row: u16) -> Option<HoverTarget> {
    if let Some(button) = button_at(app, layout.panel, col, row) {
        Some(HoverTarget::Toolbar(button))
    } else if let Some(action) = quick_action_at(app, layout, col, row) {
        Some(HoverTarget::QuickAction(action))
    } else if layout.vertical.contains((col, row).into()) {
        Some(HoverTarget::VerticalScroll)
    } else if layout.horizontal.contains((col, row).into()) {
        Some(HoverTarget::HorizontalScroll)
    } else {
        None
    }
}

fn draw_frame(f: &mut Frame, area: Rect, normal: Style, border: Color, pointer_x: u16) {
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new("").style(normal), area);
    for y in [area.y, area.bottom() - 1] {
        f.render_widget(
            Paragraph::new("─".repeat(area.width as usize)).style(normal.fg(border)),
            Rect::new(area.x, y, area.width, 1),
        );
    }
    if area.height >= 3 {
        f.render_widget(
            Paragraph::new("▲").style(normal.fg(border)),
            Rect::new(pointer_x.min(area.right() - 1), area.y, 1, 1),
        );
    }
}

fn preview_filename(path: Option<&std::path::Path>) -> std::borrow::Cow<'_, str> {
    let Some(path) = path else { return "Untitled".into(); };
    if let Some(name) = crate::remote::workspace::display_name(path) {
        return name.into();
    }
    path.file_name().map(|name| name.to_string_lossy()).unwrap_or_else(|| "Untitled".into())
}

fn draw_feedback(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(feedback) = &app.hunk_preview_feedback else { return; };
    let loading = feedback.loading();
    let message = match &feedback.state {
        crate::hunk_preview::FeedbackState::Loading(_) => {
            if app.remote_workspace.is_some() {
                "Loading changes from remote workspace…".to_string()
            } else {
                "Loading changes…".to_string()
            }
        }
        crate::hunk_preview::FeedbackState::Failed(error) => format!("Unable to load change preview.\n{error}"),
    };
    let focused = feedback.focused_button;
    let colors = app.colors;
    let bg = colors.bg_editor;
    let border = colors.ui.peek_border.border(bg).unwrap_or(bg);
    let normal = Style::default().bg(bg).fg(colors.fg_text).remove_modifier(Modifier::DIM);
    let pointer_x = app.code_area.map_or(area.x, |code| code.x);
    draw_frame(f, area, normal, border, pointer_x);
    if area.height < 4 {
        f.render_widget(Paragraph::new(message).style(normal), area);
    } else {
        let actions = buttons(app);
        let first = feedback_button_rect(area, 0, actions);
        let filename = preview_filename(app.hunk_popup_file.as_deref());
        let title = format!(" {filename}:{} · Change Preview", app.hunk_popup_line.saturating_add(1));
        let (title_bg, title_fg, _) = colors.ui.peek_title_colors(bg);
        let title_style = normal.bg(title_bg).fg(title_fg);
        f.render_widget(
            Paragraph::new(title).style(title_style.add_modifier(Modifier::BOLD)),
            Rect::new(area.x, area.y + 1, first.x.saturating_sub(area.x), 1),
        );
        let hovered = button_at(app, area, app.mouse_col, app.mouse_row);
        for (index, (icon, _)) in actions.iter().enumerate() {
            let style = colors.ui.toolbar_style(title_fg, title_bg, hovered == Some(index),
                focused == Some(index), false).remove_modifier(Modifier::DIM);
            f.render_widget(
                Paragraph::new(format!(" {icon} ")).style(style),
                feedback_button_rect(area, index, actions),
            );
        }
        f.render_widget(
            Paragraph::new(message).style(normal).wrap(Wrap { trim: false }),
            Rect::new(area.x + 1, area.y + 2, area.width.saturating_sub(2), area.height - 4),
        );
        let hint = hovered.or(focused).map_or_else(
            || "Esc to close preview",
            |index| if actions[index].1 == "Close Preview" { "Close Preview (Esc)" } else { "Retry Preview" },
        );
        f.render_widget(
            Paragraph::new(format!(" {hint}")).style(normal.fg(colors.ui.readable_foreground(colors.fg_dim, bg, 4.5))),
            Rect::new(area.x, area.bottom() - 2, area.width, 1),
        );
    }
    if loading {
        crate::ui_loading::draw_header(f, app, Rect::new(area.x, area.y, area.width, 1));
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let area = peek_rect(app, f.area().width, f.area().height);
    if let Some(feedback) = &mut app.hunk_preview_feedback {
        feedback.area = Some(area);
    }
    if area.is_empty() {
        return;
    }
    if app.hunk_preview_feedback.is_some() {
        draw_feedback(f, app, area);
        return;
    }
    let layout = make_layout(app, area);
    let (syntaxes, themes) = app.highlighter.shared_sets();
    let Some(peek) = &mut app.hunk_peek else {
        return;
    };
    peek.layout = Some(layout);
    peek.scroll = peek
        .scroll
        .min(peek.rows.len().saturating_sub(layout.body.height as usize));
    peek.horizontal_scroll = peek
        .horizontal_scroll
        .min(peek.max_width.saturating_sub(layout.code.width as usize));
    let end = (peek.scroll + layout.body.height as usize).min(peek.rows.len());
    for index in peek.scroll..end {
        peek.prepare_emphasis(index);
        let row = &mut peek.rows[index];
        if row.highlighted.is_empty()
            && !row.text.is_empty()
            && row.text.len() <= crate::highlight::MAX_SYNTAX_LINE_CHARS
        {
            row.highlighted = crate::highlight::Highlighter::highlight_line_static_pub(
                &row.text,
                app.hunk_popup_file.as_deref(),
                None,
                &app.syntax_theme,
                &syntaxes,
                &themes,
            );
        }
    }
    let Some(peek) = &app.hunk_peek else {
        return;
    };
    // Insert the view zone beneath the clicked line, retaining code below it.
    // Keep the minimap and scrollbar outside this zone, as in VS Code.
    if let Some(code) = app.code_area {
        let bottom = code.bottom().min(f.area().bottom());
        let buffer = f.buffer_mut();
        for y in (area.bottom()..bottom).rev() {
            for x in area.left()..area.right() {
                if let Some(source) = buffer.cell((x, y - area.height)).cloned() {
                    if let Some(cell) = buffer.cell_mut((x, y)) {
                        *cell = source;
                    }
                }
            }
        }
    }
    let colors = app.colors;
    let bg = colors.bg_editor;
    let border = colors.ui.peek_border.border(bg).unwrap_or(bg);
    let normal = Style::default()
        .bg(bg)
        .fg(colors.fg_text)
        .remove_modifier(Modifier::DIM);
    let pointer_x = app.code_area.map_or(area.x, |code| code.x);
    draw_frame(f, area, normal, border, pointer_x);
    if area.height < 3 {
        return;
    }
    let actions = buttons(app);
    let first = button_rect(area, 0, actions.len());
    let title_area = Rect::new(area.x, area.y + 1, first.x.saturating_sub(area.x), 1);
    let filename = preview_filename(app.hunk_popup_file.as_deref());
    let source = if app.hunk_popup_status == 'S' {
        "Index"
    } else if app.editor().is_some_and(|editor| editor.modified) {
        "Unsaved Buffer"
    } else {
        "Working Tree"
    };
    let hovered = button_at(app, area, app.mouse_col, app.mouse_row);
    let quick_hovered = quick_action_at(app, layout, app.mouse_col, app.mouse_row);
    let focus = hovered.or(peek.focused_button);
    let detail = if let Some(action) = quick_hovered {
        let label = if app.hunk_popup_status == 'S' {
            "Unstage Hunk"
        } else {
            actions[action.button].1
        };
        format!("  {label} · change {} of {}", action.hunk + 1, peek.hunks.len())
    } else {
        focus.map_or_else(
            || {
                if app.hunk_popup_status == 'N' {
                    return format!(
                        " Unsaved Changes (since last save) - {} of {} changes",
                        peek.selected + 1, peek.hunks.len(),
                    );
                }
                format!(
                    " Git Local Changes ({source}) - {} of {} changes",
                    peek.selected + 1,
                    peek.hunks.len()
                )
            },
            |i| format!("  {}", actions[i].1),
        )
    };
    let (title_bg, title_fg, title_dim) = colors.ui.peek_title_colors(bg);
    let title_style = normal.bg(title_bg).fg(title_fg);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {filename}"), title_style.add_modifier(Modifier::BOLD)),
            Span::styled(detail, title_style.fg(title_dim)),
        ]))
        .style(title_style),
        title_area,
    );
    for (index, (icon, _)) in actions.iter().enumerate() {
        let style = colors.ui.toolbar_style(title_fg, title_bg, hovered == Some(index),
            peek.focused_button == Some(index), false).remove_modifier(Modifier::DIM);
        f.render_widget(
            Paragraph::new(format!(" {icon} ")).style(style),
            button_rect(area, index, actions.len()),
        );
    }
    let body = layout.body;
    let rows = &peek.rows;
    let scroll = peek.scroll;
    let digits = peek.digits;
    for (offset, row) in rows
        .iter()
        .skip(scroll)
        .take(body.height as usize)
        .enumerate()
    {
        let y = body.y + offset as u16;
        let row_bg = match row.kind {
            '-' => colors.bg_diff_removed,
            '+' => colors.bg_diff_added,
            _ => bg,
        };
        let emphasis_bg = match row.kind {
            '-' => colors.bg_diff_removed_highlight,
            '+' => colors.bg_diff_added_highlight,
            _ => bg,
        };
        let number =
            |n: Option<usize>| n.map_or_else(|| " ".repeat(digits), |n| format!("{n:>digits$}"));
        let gutter = format!(" {} {} ", number(row.old_line), number(row.new_line));
        let gutter_width = (digits * 2 + 3).min(body.width as usize) as u16;
        f.render_widget(
            Paragraph::new(gutter).style(normal.fg(colors.fg_dim)),
            Rect::new(body.x, y, gutter_width, 1),
        );
        let code = Rect::new(
            layout.code.x,
            y,
            layout.code.width,
            1,
        );
        let spans = row_spans(
            row,
            peek.horizontal_scroll,
            code.width as usize,
            normal.bg(row_bg),
            emphasis_bg,
            peek.tab_size,
            &colors.ui,
        );
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(normal.bg(row_bg)),
            code,
        );
    }
    for action in quick_actions(peek, layout, matches!(app.hunk_popup_status, 'N' | 'S')) {
        let stage = !matches!(app.hunk_popup_status, 'N' | 'S') && action.button == 0;
        let fg = if peek.hunks[action.hunk].action_line.is_none() {
            colors.fg_dim
        } else if stage {
            colors.ui.fg_gutter_added
        } else {
            colors.ui.fg_gutter_deleted
        };
        let style = colors.ui.toolbar_style(
            fg, bg, quick_hovered == Some(action),
            peek.selected == action.hunk && peek.focused_button == Some(action.button), false,
        ).add_modifier(Modifier::BOLD).remove_modifier(Modifier::DIM);
        f.render_widget(Paragraph::new(if stage { " + " } else { " − " }).style(style), action.area);
    }
    let vertical_hovered = layout.vertical.contains((app.mouse_col, app.mouse_row).into());
    let (vertical_track, vertical_thumb) = colors.ui.scrollbar_colors(bg, vertical_hovered, peek.drag.is_some_and(|drag| !drag.horizontal));
    draw_scrollbar(
        f,
        layout.vertical,
        rows.len(),
        body.height as usize,
        peek.scroll,
        false,
        normal.bg(vertical_track),
        vertical_thumb,
        vertical_track,
    );
    let horizontal_hovered = layout.horizontal.contains((app.mouse_col, app.mouse_row).into());
    let (horizontal_track, horizontal_thumb) = colors.ui.scrollbar_colors(bg, horizontal_hovered, peek.drag.is_some_and(|drag| drag.horizontal));
    draw_scrollbar(
        f,
        layout.horizontal,
        peek.max_width,
        layout.code.width as usize,
        peek.horizontal_scroll,
        true,
        normal.bg(horizontal_track),
        horizontal_thumb,
        horizontal_track,
    );
    // Scrolling/resizing can put a control under a stationary pointer. Track
    // the hover actually drawn so the next move always clears its highlight.
    if let Some(peek) = &mut app.hunk_peek {
        peek.hovered = hovered.map(HoverTarget::Toolbar)
            .or(quick_hovered.map(HoverTarget::QuickAction))
            .or(vertical_hovered.then_some(HoverTarget::VerticalScroll))
            .or(horizontal_hovered.then_some(HoverTarget::HorizontalScroll));
    }
}

fn row_spans(
    row: &PeekRow,
    scroll: usize,
    width: usize,
    normal: Style,
    emphasis_bg: Color,
    tab_size: usize,
    colors: &crate::theme_workbench::WorkbenchColors,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut batch = String::new();
    let mut batch_style = normal;
    let mut contrast = crate::ui_colors::TextContrastCache::new(colors);
    let mut col = 0;
    let mut index = 0;
    let fallback_fg = normal.fg.unwrap_or(Color::Reset);
    let segments = std::iter::once((fallback_fg, row.text.as_str()))
        .take(usize::from(row.highlighted.is_empty()))
        .chain(row.highlighted.iter().map(|(s, text)| {
            (
                crate::ui_colors::syntax_color(s.foreground, normal.bg.unwrap_or(Color::Reset)),
                text.as_str(),
            )
        }));
    'segments: for (fg, text) in segments {
        for ch in text.chars() {
            if col >= scroll + width {
                break 'segments;
            }
            let w = if ch == '\t' {
                tab_size - col % tab_size
            } else {
                ch.width().unwrap_or(0)
            };
            let end = col + w;
            let emphasized = row
                .emphasis
                .as_ref()
                .map_or(row.kind != ' ', |m| m.get(index).copied().unwrap_or(false));
            let background = if emphasized { emphasis_bg } else { normal.bg.unwrap_or(Color::Reset) };
            let style = normal.fg(contrast.foreground(fg, background)).bg(background);
            if end > scroll || (w == 0 && col > scroll) {
                if style != batch_style && !batch.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut batch), batch_style));
                }
                batch_style = style;
                if ch == '\t' || ch.is_control() || col < scroll || end > scroll + width {
                    let shown = end.min(scroll + width).saturating_sub(col.max(scroll));
                    batch.extend(std::iter::repeat_n(' ', shown));
                } else {
                    batch.push(ch);
                }
            }
            col = end;
            index += 1;
        }
    }
    if !batch.is_empty() {
        spans.push(Span::styled(batch, batch_style));
    }
    spans
}

fn activate(app: &mut App, button: usize) {
    let Some((_, label)) = buttons(app).get(button).copied() else {
        return;
    };
    match label {
        "Next Change" | "Previous Change" => {
            let Some(peek) = app.hunk_peek.as_mut() else {
                return;
            };
            peek.selected = if label == "Next Change" {
                (peek.selected + 1) % peek.hunks.len()
            } else {
                (peek.selected + peek.hunks.len() - 1) % peek.hunks.len()
            };
            peek.reveal_selected();
            if let Some(line) = peek.hunks[peek.selected].action_line {
                app.hunk_popup_line = line;
                let gutter_y = app.gutter_area.map_or(0, |a| a.y);
                if let Some(editor) = app.editor_mut() {
                    let line = line.min(editor.line_count().saturating_sub(1));
                    editor.cursor_line = line;
                    editor.cursor_col = 0;
                    editor.scroll_offset = line.saturating_sub(2);
                    if editor.word_wrap {
                        let row =
                            editor.wrapped_row_for_position(line, 0, editor.effective_wrap_width());
                        editor.set_wrap_scroll_offset(row.saturating_sub(2));
                        app.hunk_popup_pos.1 = gutter_y.saturating_add(row.min(2) as u16);
                    } else {
                        app.hunk_popup_pos.1 = gutter_y.saturating_add(line.min(2) as u16);
                    }
                }
            }
        }
        "Close Preview" => app.close_hunk_popup(),
        "Retry Preview" => app.retry_hunk_preview(),
        _ => {
            if app
                .hunk_peek
                .as_ref()
                .is_some_and(|p| p.hunks[p.selected].action_line.is_none())
            {
                app.notify("This staged change is covered by working tree edits; use Source Control to revert it");
                return;
            }
            if let Some(index) = app
                .hunk_popup_items()
                .iter()
                .position(|item| *item == label)
            {
                app.execute_hunk_popup_action(index);
            }
        }
    }
}

fn scroll(app: &mut App, down: bool, amount: usize, horizontal: bool) {
    let layout = input_layout(app);
    let Some(peek) = app.hunk_peek.as_mut() else {
        return;
    };
    let max = if horizontal {
        peek.max_width.saturating_sub(layout.code.width as usize)
    } else {
        peek.rows.len().saturating_sub(layout.body.height as usize)
    };
    let value = if horizontal {
        &mut peek.horizontal_scroll
    } else {
        &mut peek.scroll
    };
    *value = if down {
        value.saturating_add(amount).min(max)
    } else {
        value.saturating_sub(amount)
    };
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.close_hunk_popup(),
        KeyCode::Tab | KeyCode::BackTab => {
            let count = buttons(app).len();
            let backwards = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            let focus = if let Some(feedback) = &mut app.hunk_preview_feedback {
                &mut feedback.focused_button
            } else if let Some(peek) = &mut app.hunk_peek {
                &mut peek.focused_button
            } else {
                return;
            };
            *focus = Some(match *focus {
                Some(i) if backwards => (i + count - 1) % count,
                Some(i) => (i + 1) % count,
                None if backwards => count - 1,
                None => 0,
            });
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let focused = app.hunk_preview_feedback.as_ref().and_then(|feedback| feedback.focused_button)
                .or_else(|| app.hunk_peek.as_ref().and_then(|peek| peek.focused_button));
            if let Some(i) = focused {
                activate(app, i);
            }
        }
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
            let down = matches!(key.code, KeyCode::Down | KeyCode::PageDown);
            let amount = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                input_layout(app).body.height.max(1) as usize
            } else {
                1
            };
            scroll(app, down, amount, false);
        }
        KeyCode::Home | KeyCode::End => scroll(
            app,
            key.code == KeyCode::End,
            usize::MAX,
            !key.modifiers.contains(KeyModifiers::CONTROL),
        ),
        KeyCode::Left | KeyCode::Right => scroll(app, key.code == KeyCode::Right, 4, true),
        _ => {}
    }
}

pub(crate) fn click(app: &mut App, col: u16, row: u16, width: u16, height: u16) {
    let layout = app
        .hunk_peek
        .as_ref()
        .and_then(|p| p.layout)
        .or_else(|| app.hunk_preview_feedback.as_ref()?.area.map(|panel| make_layout(app, panel)))
        .unwrap_or_else(|| make_layout(app, peek_rect(app, width, height)));
    if scrollbar_down(app, layout, col, row) {
        return;
    }
    if let Some(action) = quick_action_at(app, layout, col, row) {
        if let Some(peek) = app.hunk_peek.as_mut() {
            peek.selected = action.hunk;
            if let Some(line) = peek.hunks[action.hunk].action_line {
                app.hunk_popup_line = line;
            }
        }
        activate(app, action.button);
        return;
    }
    if let Some(i) = button_at(app, layout.panel, col, row) {
        activate(app, i);
    } else if !layout.panel.contains((col, row).into()) {
        app.close_hunk_popup();
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    let layout = input_layout(app);
    match mouse.kind {
        MouseEventKind::Moved => {
            let hovered = hover_target(app, layout, mouse.column, mouse.row);
            if app.hunk_preview_feedback.is_some() {
                let hovered = button_at(app, layout.panel, mouse.column, mouse.row);
                if let Some(feedback) = &mut app.hunk_preview_feedback {
                    app.mouse_hover_changed |= feedback.hovered_button != hovered;
                    feedback.hovered_button = hovered;
                }
            }
            if let Some(peek) = app.hunk_peek.as_mut() {
                app.mouse_hover_changed |= peek.hovered != hovered;
                peek.hovered = hovered;
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            click(
                app,
                mouse.column,
                mouse.row,
                layout.panel.right(),
                layout.panel.bottom(),
            );
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(peek) = app.hunk_peek.as_mut() {
                peek.drag = None;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            drag_scrollbar(app, layout, mouse.column, mouse.row)
        }
        MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => {
            if layout.panel.contains((mouse.column, mouse.row).into()) {
                let horizontal = mouse.modifiers.contains(KeyModifiers::SHIFT)
                    || matches!(
                        mouse.kind,
                        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                    );
                let down = matches!(
                    mouse.kind,
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollRight
                );
                scroll(app, down, app.scroll_speed.max(1), horizontal);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_git_display::parse_git_file_patch;
    use ratatui::{Terminal, backend::TestBackend};

    fn app_with_peek() -> App {
        let mut app = App::new(std::path::PathBuf::from("."));
        let patch = parse_git_file_patch("@@ -2,2 +2,1 @@\n-let old = 1;\n-removed();\n+let new = 2;\n@@ -8 +7 @@\n-last_old();\n+last_new();\n").unwrap();
        app.hunk_peek = Some(HunkPeek::from_patch(
            &patch,
            "first();\nlet new = 2;\nthird();\nfourth();\nfifth();\nsixth();\nlast_new();\nend();\n",
            0,
        ));
        app.hunk_popup_active = true;
        app.hunk_popup_status = 'M';
        app.hunk_popup_line = 1;
        app.hunk_popup_file = Some("example.rs".into());
        app.hunk_popup_pos = (10, 4);
        app.editor_area = Some(Rect::new(10, 3, 90, 26));
        app.gutter_area = Some(Rect::new(10, 3, 6, 26));
        app.code_area = Some(Rect::new(16, 3, 78, 26));
        app
    }

    #[test]
    fn peek_border_and_title_use_their_own_theme_roles_and_surfaces() {
        use crate::theme_workbench::{ColorLayer, WorkbenchColors};
        let mut app = app_with_peek();
        let roles = serde_json::json!({
            "peekView.border":"#ff99cc80", "peekViewTitle.background":"#101e32c0",
            "peekViewTitleLabel.foreground":"#eeffffe0", "peekViewTitleDescription.foreground":"#ddffbbc0",
            "focusBorder":"#00ffaa", "textLink.foreground":"#ff8800"
        });
        app.colors.ui = WorkbenchColors::resolve(roles.as_object().unwrap(), &app.colors, false, false);
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let area = peek_rect(&app, 110, 32);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(area.x, area.y)].fg,
            ColorLayer::rgba(255, 153, 204, 128).over(app.colors.bg_editor).unwrap());
        let title = &buffer[(area.x + 1, area.y + 1)];
        assert_eq!(title.symbol(), "e");
        assert_eq!(title.bg, ColorLayer::rgba(16, 30, 50, 192).over(app.colors.bg_editor).unwrap());
        assert_eq!(title.fg, ColorLayer::rgba(238, 255, 255, 224).over(title.bg).unwrap());
        let description = &buffer[(area.x + 12, area.y + 1)];
        assert_eq!(description.symbol(), "G");
        assert_eq!(description.fg, ColorLayer::rgba(221, 255, 187, 192).over(description.bg).unwrap());
        let title_fg = title.fg;
        crate::ui_control_visibility_tests::snapshot(buffer, "independent peek title roles");

        app.colors.ui.peek_border = ColorLayer::rgba(0, 0, 0, 0);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(area.x, area.y)].fg, app.colors.bg_editor);
        assert_eq!(buffer[(area.x + 1, area.y + 1)].fg, title_fg);
    }

    fn long_peek() -> (App, Terminal<TestBackend>) {
        let mut app = app_with_peek();
        let old: String = (0..200).map(|i| format!("line {i:03}\n")).collect();
        let new = old
            .replace("line 010", &format!("changed\t{}", "x".repeat(300)))
            .replace("line 150", "second change");
        let diff = similar::TextDiff::from_lines(&old, &new)
            .unified_diff()
            .context_radius(0)
            .to_string();
        let patch = parse_git_file_patch(&diff).unwrap();
        app.hunk_peek = Some(HunkPeek::with_tab_size(&patch, &new, 0, 8));
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        (app, terminal)
    }

    #[test]
    fn peek_scrollbars_composite_the_track_and_slider_on_the_peek_background() {
        use crate::theme_workbench::ColorLayer;
        let (mut app, mut terminal) = long_peek();
        app.colors.ui.scrollbar_background = ColorLayer::rgba(80, 100, 120, 128);
        app.colors.ui.scrollbar_slider = ColorLayer::rgba(240, 240, 240, 128);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let layout = app.hunk_peek.as_ref().unwrap().layout.unwrap();
        let track = ColorLayer::rgba(80, 100, 120, 128).over(app.colors.bg_editor).unwrap();
        let thumb = ColorLayer::rgba(240, 240, 240, 128).over(track).unwrap();
        for area in [layout.vertical, layout.horizontal] {
            assert!(!area.is_empty());
            let buffer = terminal.backend().buffer();
            let mut found_thumb = false;
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    assert_eq!(buffer[(x, y)].bg, track);
                    found_thumb |= buffer[(x, y)].fg == thumb;
                }
            }
            assert!(found_thumb);
        }
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers,
            },
        );
    }

    #[test]
    fn peek_wheel_scrolls_past_hunk_using_rendered_geometry() {
        let (mut app, mut terminal) = long_peek();
        let layout = input_layout(&app);
        let initial = app.hunk_peek.as_ref().unwrap().scroll;
        // This is beyond the 80-column terminal-size fallback used by headless
        // sessions. Hit testing must use the geometry from the actual frame.
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            90,
            layout.body.y + 1,
            KeyModifiers::NONE,
        );
        assert!(app.hunk_peek.as_ref().unwrap().scroll > initial);
        assert!(app.hunk_popup_active);
        handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let peek = app.hunk_peek.as_ref().unwrap();
        let layout = peek.layout.unwrap();
        assert_eq!(peek.scroll, peek.rows.len() - layout.body.height as usize);
        let bottom_scroll = peek.scroll;
        let row = layout.body.bottom() - 1;
        let text: String = (layout.code.x..layout.code.right())
            .map(|x| terminal.backend().buffer()[(x, row)].symbol())
            .collect();
        assert!(
            text.contains("line 199"),
            "Can scroll to context far beyond both changes"
        );
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            90,
            layout.body.y,
            KeyModifiers::NONE,
        );
        assert_eq!(app.hunk_peek.as_ref().unwrap().scroll, bottom_scroll);
        let before = app.hunk_peek.as_ref().unwrap().scroll;
        mouse(&mut app, MouseEventKind::ScrollUp, 0, 0, KeyModifiers::NONE);
        assert_eq!(app.hunk_peek.as_ref().unwrap().scroll, before);
        assert!(app.hunk_popup_active);
    }

    #[test]
    fn peek_scrollbar_tracks_page_and_thumbs_drag_to_both_ends() {
        let (mut app, _) = long_peek();
        let layout = input_layout(&app);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            layout.vertical.x,
            layout.vertical.bottom() - 1,
            KeyModifiers::NONE,
        );
        let peek = app.hunk_peek.as_ref().unwrap();
        let (start, _) = thumb(
            peek.rows.len(),
            layout.body.height as usize,
            layout.vertical.height as usize,
            peek.scroll,
        );
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            layout.vertical.x,
            layout.vertical.y + start as u16,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            109,
            31,
            KeyModifiers::NONE,
        );
        let peek = app.hunk_peek.as_ref().unwrap();
        assert_eq!(peek.scroll, peek.rows.len() - layout.body.height as usize);
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            0,
            0,
            KeyModifiers::NONE,
        );
        assert_eq!(app.hunk_peek.as_ref().unwrap().scroll, 0);
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            0,
            0,
            KeyModifiers::NONE,
        );
        assert!(app.hunk_peek.as_ref().unwrap().drag.is_none());
        assert!(app.hunk_popup_active);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            layout.horizontal.x,
            layout.horizontal.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            109,
            layout.horizontal.y,
            KeyModifiers::NONE,
        );
        let peek = app.hunk_peek.as_ref().unwrap();
        assert_eq!(
            peek.horizontal_scroll,
            peek.max_width - layout.code.width as usize
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            109,
            layout.horizontal.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::ScrollUp,
            90,
            layout.body.y,
            KeyModifiers::SHIFT,
        );
        assert!(
            app.hunk_peek.as_ref().unwrap().horizontal_scroll < 308 - layout.code.width as usize
        );
    }

    #[test]
    fn peek_prepares_only_visible_diffs_and_batches_matching_styles() {
        let (app, _) = long_peek();
        let peek = app.hunk_peek.as_ref().unwrap();
        let first = peek.hunks[0].start_row;
        let distant = peek.hunks[1].start_row;
        assert!(peek.rows[first].prepared);
        assert!(!peek.rows[distant].prepared);
        assert!(peek.rows[distant].emphasis.is_none());
        assert!(peek.rows[distant].highlighted.is_empty());
        let row = PeekRow {
            kind: ' ',
            old_line: None,
            new_line: Some(1),
            text: "a\t界e\u{301}xyz".into(),
            highlighted: Vec::new(),
            emphasis: None,
            partner: None,
            prepared: false,
        };
        assert_eq!(text_width(&row.text, 8), 14);
        let spans = row_spans(&row, 0, 80, Style::default(), Color::Red, 8, &crate::theme_workbench::WorkbenchColors::default());
        assert_eq!(
            spans.len(),
            1,
            "One span per style run, rather than one allocation per character"
        );
        assert_eq!(spans[0].content, "a       界e\u{301}xyz");
        let clipped = row_spans(&row, 9, 3, Style::default(), Color::Red, 8, &crate::theme_workbench::WorkbenchColors::default());
        assert_eq!(clipped[0].content, " e\u{301}x");
    }

    #[test]
    fn peek_numbers_context_replacements_and_deletions() {
        let mut app = app_with_peek();
        app.hunk_peek.as_mut().unwrap().prepare_emphasis(1);
        let rows = &app.hunk_peek.as_ref().unwrap().rows;
        assert_eq!((rows[0].old_line, rows[0].new_line), (Some(1), Some(1)));
        assert_eq!(
            (rows[1].kind, rows[1].old_line, rows[1].new_line),
            ('-', Some(2), None)
        );
        assert_eq!(
            (rows[3].kind, rows[3].old_line, rows[3].new_line),
            ('+', None, Some(2))
        );
        assert_eq!((rows[4].old_line, rows[4].new_line), (Some(4), Some(3)));
        assert!(rows[1].emphasis.as_ref().unwrap().iter().any(|v| *v));
        assert!(rows[1].emphasis.as_ref().unwrap().iter().any(|v| !*v));
        let patch = parse_git_file_patch("@@ -1 +0,0 @@\n-deleted\n\\ No newline at end of file\n")
            .unwrap();
        let peek = HunkPeek::from_patch(&patch, "", 0);
        assert_eq!(peek.rows.len(), 1);
        assert_eq!(peek.rows[0].old_line, Some(1));
        assert_eq!(peek.rows[0].new_line, None);
        let patch = parse_git_file_patch("@@ -0,0 +1,2 @@\n+one\n+two\n").unwrap();
        let peek = HunkPeek::from_patch(&patch, "one\ntwo\n", 0);
        assert_eq!(peek.rows[1].new_line, Some(2));
        assert!(peek.rows.iter().all(|r| r.old_line.is_none()));
    }

    #[test]
    fn peek_live_text_handles_insertions_deletions_and_line_endings() {
        for (saved, current, line, removed, added) in [
            ("", "added", 0, None, Some("added")),
            ("removed\n", "", 0, Some("removed"), None),
            ("first\nremoved\n", "first\n", 1, Some("removed"), None),
            ("removed\nfirst\n", "first\n", 0, Some("removed"), None),
            ("first\r\nold_界\r\n", "first\r\nnew_界\r\n", 1, Some("old_界"), Some("new_界")),
        ] {
            let peek = HunkPeek::from_texts(saved, current, line, 4).unwrap();
            assert_eq!(peek.hunks.len(), 1);
            assert_eq!(peek.hunks[0].action_line, Some(line));
            let old: Vec<_> = peek.rows.iter().filter(|row| row.kind == '-').map(|row| row.text.as_str()).collect();
            let new: Vec<_> = peek.rows.iter().filter(|row| row.kind == '+').map(|row| row.text.as_str()).collect();
            assert_eq!(old, removed.into_iter().collect::<Vec<_>>());
            assert_eq!(new, added.into_iter().collect::<Vec<_>>());
        }
        assert_eq!(HunkPeek::from_texts("same\n", "same\n", 0, 4).err().as_deref(), Some("No changes found"));
        assert_eq!(HunkPeek::from_texts("same\nold\n", "same\nnew\n", 0, 4).err().as_deref(), Some("No change found at this line"));
    }

    #[test]
    fn peek_unsaved_changes_can_navigate_without_offering_disk_git_actions() {
        let mut app = app_with_peek();
        app.hunk_popup_status = 'N';
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let peek = app.hunk_peek.as_ref().unwrap();
        let actions: Vec<_> = quick_actions(peek, peek.layout.unwrap(), true).collect();
        assert_eq!(actions.len(), 2, "one revert button per unsaved hunk");
        for action in &actions {
            assert_eq!(terminal.backend().buffer()[(action.area.x + 1, action.area.y)].symbol(), "−");
        }
        assert_eq!(buttons(&app).iter().map(|(_, label)| *label).collect::<Vec<_>>(),
            ["Revert Unsaved Hunk", "Next Change", "Previous Change", "Close Preview"]);
        let next = button_rect(peek.layout.unwrap().panel, 1, 4);
        click(&mut app, next.x + 1, next.y, 110, 32);
        assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 1);
        assert_eq!(app.hunk_popup_line, 6);
        activate(&mut app, 2);
        assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 0);
        assert_eq!(app.hunk_popup_line, 1);
        activate(&mut app, 3);
        assert!(!app.hunk_popup_active);
    }

    #[test]
    fn peek_navigation_and_body_click_do_not_execute_an_action() {
        let mut app = app_with_peek();
        let area = peek_rect(&app, 110, 32);
        click(&mut app, area.x + 20, area.y + 3, 110, 32);
        assert!(app.hunk_popup_active);
        assert_eq!(app.hunk_popup_line, 1);
        let next = button_rect(area, 2, 5);
        click(&mut app, next.x + 1, next.y, 110, 32);
        assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 1);
        assert_eq!(app.hunk_popup_line, 6);
        activate(&mut app, 3);
        assert_eq!(app.hunk_popup_line, 1);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.hunk_popup_active,
            "Enter without a focused toolbar button is harmless"
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.hunk_popup_active);
        assert!(app.hunk_peek.is_none());
    }

    #[test]
    fn peek_scroll_is_bounded_and_keyboard_can_close() {
        let mut app = app_with_peek();
        app.code_area.as_mut().unwrap().height = 10;
        app.gutter_area.as_mut().unwrap().height = 10;
        scroll(&mut app, true, usize::MAX, false);
        let peek = app.hunk_peek.as_ref().unwrap();
        assert_eq!(peek.scroll, peek.rows.len() - 2);
        scroll(&mut app, false, usize::MAX, false);
        assert_eq!(app.hunk_peek.as_ref().unwrap().scroll, 0);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.hunk_popup_active);
    }

    #[test]
    fn peek_quick_actions_fit_one_line_additions_and_deletions_without_hiding_numbers() {
        for (patch, target, old, new) in [
            ("@@ -0,0 +1 @@\n+added\n", "added\n", None, Some(1)),
            ("@@ -1 +0,0 @@\n-removed\n", "", Some(1), None),
        ] {
            let mut app = app_with_peek();
            app.hunk_peek = Some(HunkPeek::from_patch(&parse_git_file_patch(patch).unwrap(), target, 0));
            let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let peek = app.hunk_peek.as_ref().unwrap();
            let layout = peek.layout.unwrap();
            let actions: Vec<_> = quick_actions(peek, layout, false).collect();
            assert_eq!(actions.len(), 2);
            assert_eq!(actions[0].area.y, actions[1].area.y);
            for (action, symbol) in actions.iter().zip(["+", "−"]) {
                assert_eq!(terminal.backend().buffer()[(action.area.x + 1, action.area.y)].symbol(), symbol);
                assert_eq!(action.area.intersection(layout.body), action.area);
                assert!(action.area.intersection(layout.code).is_empty());
            }
            let digits = peek.digits;
            let number = |n: Option<usize>| n.map_or_else(|| " ".repeat(digits), |n| format!("{n:>digits$}"));
            let shown: String = (layout.body.x..layout.quick_actions.x)
                .map(|x| terminal.backend().buffer()[(x, layout.body.y)].symbol()).collect();
            assert_eq!(shown, format!(" {} {} ", number(old), number(new)));
        }
    }

    #[test]
    fn peek_quick_actions_follow_scrolled_hunks_and_hover_only_redraws_changed_controls() {
        let (mut app, mut terminal) = long_peek();
        let first = app.hunk_peek.as_ref().unwrap().hunks[0].start_row;
        app.hunk_peek.as_mut().unwrap().scroll = first + 1;
        app.hunk_peek.as_mut().unwrap().horizontal_scroll = 50;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let peek = app.hunk_peek.as_ref().unwrap();
        let layout = peek.layout.unwrap();
        let actions: Vec<_> = quick_actions(peek, layout, false).collect();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].area.y, layout.body.y, "partial hunk retains its actions");
        let stage = actions[0].area;
        let discard = actions[1].area;
        for (x, y, redraw) in [
            (stage.x, stage.y, true),
            (stage.x + 1, stage.y, false),
            (discard.x + 1, discard.y, true),
        ] {
            app.mouse_hover_changed = false;
            mouse(&mut app, MouseEventKind::Moved, x, y, KeyModifiers::NONE);
            assert_eq!(app.mouse_hover_changed, redraw);
        }
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let title: String = (layout.panel.x..layout.panel.right())
            .map(|x| terminal.backend().buffer()[(x, layout.panel.y + 1)].symbol()).collect();
        assert!(title.contains("Discard Hunk"));
        app.mouse_hover_changed = false;
        mouse(&mut app, MouseEventKind::Moved, layout.code.x, layout.body.y, KeyModifiers::NONE);
        assert!(app.mouse_hover_changed, "leaving removes the hover highlight and label");
        app.mouse_hover_changed = false;
        mouse(&mut app, MouseEventKind::Moved, layout.code.x + 1, layout.body.y, KeyModifiers::NONE);
        assert!(!app.mouse_hover_changed);

        app.hunk_peek.as_mut().unwrap().scroll = first + 3;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let peek = app.hunk_peek.as_ref().unwrap();
        assert_eq!(quick_actions(peek, peek.layout.unwrap(), false).count(), 0, "context has no actions");
        app.hunk_peek.as_mut().unwrap().scroll = app.hunk_peek.as_ref().unwrap().hunks[1].start_row;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let peek = app.hunk_peek.as_ref().unwrap();
        let actions: Vec<_> = quick_actions(peek, peek.layout.unwrap(), false).collect();
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|action| action.hunk == 1));

        app.mouse_col = actions[0].area.x + 1;
        app.mouse_row = actions[0].area.y;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.mouse_hover_changed = false;
        mouse(&mut app, MouseEventKind::Moved, layout.code.x, layout.body.y, KeyModifiers::NONE);
        assert!(app.mouse_hover_changed, "hover painted after scrolling clears on the next move");
    }

    #[test]
    fn peek_staged_quick_action_respects_unmappable_hunks() {
        let mut app = app_with_peek();
        app.hunk_popup_status = 'S';
        app.hunk_peek.as_mut().unwrap().hunks[1].action_line = None;
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let peek = app.hunk_peek.as_ref().unwrap();
        let actions: Vec<_> = quick_actions(peek, peek.layout.unwrap(), true).collect();
        assert_eq!(actions.len(), 2, "one unstage action per visible staged hunk");
        let action = actions[1];
        assert_eq!(terminal.backend().buffer()[(action.area.x + 1, action.area.y)].symbol(), "−");
        mouse(&mut app, MouseEventKind::Moved, action.area.x + 1, action.area.y, KeyModifiers::NONE);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Unstage Hunk"));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), action.area.x + 1, action.area.y, KeyModifiers::NONE);
        assert!(app.hunk_popup_active, "unmappable hunk must not use another hunk's action line");
        assert_eq!(app.hunk_popup_line, 1);
        assert!(app.notification_history.iter().any(|note| note.0.contains("covered by working tree edits")));
    }

    #[test]
    fn peek_layout_stays_inside_editor_on_resize() {
        let mut app = app_with_peek();
        app.hunk_popup_pos.1 = 28;
        for (w, h) in [(110, 32), (80, 24), (18, 8), (12, 4), (1, 1)] {
            let area = peek_rect(&app, w, h);
            assert!(area.right() <= w && area.bottom() <= h);
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            if let Some(peek) = app.hunk_peek.as_ref() {
                let layout = make_layout(&app, area);
                for action in quick_actions(peek, layout, false) {
                    assert_eq!(action.area.intersection(area), action.area);
                    assert!(action.area.intersection(layout.code).is_empty());
                }
            }
        }
    }

    #[test]
    fn peek_render_inserts_rows_and_preserves_minimap() {
        let mut app = app_with_peek();
        let area = peek_rect(&app, 110, 32);
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal
            .draw(|f| {
                for y in 3..29 {
                    f.render_widget(
                        Paragraph::new(format!("  {y:>2}  editor line {y}")),
                        Rect::new(10, y, 84, 1),
                    );
                    f.render_widget(Paragraph::new("MINI"), Rect::new(95, y, 4, 1));
                }
                draw(f, &mut app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = |y: u16| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(text(area.y + 1).contains("example.rs"));
        assert!(text(area.y + 1).contains("1 of 2 changes"));
        assert!(text(area.y + 3).contains("let old = 1;"));
        assert!(text(area.y + 5).contains("let new = 2;"));
        assert!(text(area.bottom()).contains(&format!("editor line {}", area.y)));
        assert_eq!(
            buffer[(area.right() - 2, area.y + 3)].bg,
            app.colors.bg_diff_removed
        );
        assert_eq!(
            buffer[(area.right() - 2, area.y + 5)].bg,
            app.colors.bg_diff_added
        );
        assert_eq!(buffer[(area.x, area.y)].fg, app.colors.ui.peek_border.border(app.colors.bg_editor).unwrap_or(app.colors.bg_editor));
        assert_eq!(buffer[(95, area.y + 3)].symbol(), "M");
        crate::ui_control_visibility_tests::snapshot(buffer, "Hunk peek quick actions");
        if let Ok(path) = std::env::var("VTCODE_HUNK_PEEK_SNAPSHOT") {
            let cells: Vec<_> = (0..32).flat_map(|y| (0..110).map(move |x| (x, y))).map(|(x, y)| {
                let cell = &buffer[(x, y)];
                serde_json::json!({"x": x, "y": y, "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg)})
            }).collect();
            std::fs::write(path, serde_json::to_string(&cells).unwrap()).unwrap();
        }
    }
}
