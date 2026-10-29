//! A per-file annotation rail and inline commit details. Source text,
//! syntax colors, selection, folds, review rows and scrolling remain the editor's.
use crate::{
    app::{ActivePane, App, TabKind},
    editor::WrappedRowInfo,
    git_blame,
};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct InlineDetails {
    pub line: usize,
    pub height: u16,
}

struct DetailScrollbar {
    top: i32,
    height: u16,
    thumb_top: i32,
    thumb_height: u16,
}

#[derive(Default)]
pub(crate) struct View {
    pub revision: Option<std::sync::Arc<git_blame::Revision>>,
    pub previous_revision: Option<std::sync::Arc<git_blame::Revision>>,
    pub previous_notice: Option<String>,
    pub enabled: bool,
    pub selected: Option<usize>,
    pub button: Option<Rect>,
    pub hits: Vec<(Rect, usize)>,
    // Visible space for the current line's annotation, even before blame has
    // been loaded. Rendering supplies demand; it never starts Git work.
    pub inline_candidate: Option<(Rect, usize)>,
    pub inline_annotation: Option<(Rect, usize)>,
    pub close: Option<Rect>,
    pub copy: Option<Rect>,
    pub previous: Option<Rect>,
    pub open_commit: Option<Rect>,
    pub open_file: Option<Rect>,
    pub revision_previous: Option<Rect>,
    pub working_file: Option<Rect>,
    pub diff_previous: Option<Rect>,
    pub diff_committed: Option<Rect>,
    pub panel: Option<Rect>,
    pub detail_content: Option<Rect>,
    pub detail_scrollbar: Option<Rect>,
    pub detail_thumb: Option<Rect>,
    detail_track: Option<DetailScrollbar>,
    detail_drag: Option<u16>,
    detail_mouse_down: bool,
    clicked_annotation: Option<usize>,
    pub detail_scroll: u16,
    pub detail_max_scroll: u16,
    pub rows: Vec<Option<WrappedRowInfo>>,
    pub history_request: u64,
    pub history_loading: bool,
}

impl View {
    fn clear_details(&mut self) {
        self.open_commit = None;
        self.close = None;
        self.copy = None;
        self.previous = None;
        self.open_file = None;
        self.panel = None;
        self.detail_content = None;
        self.detail_scrollbar = None;
        self.detail_thumb = None;
        self.detail_track = None;
    }

    fn dismiss_details(&mut self) {
        self.selected = None;
        self.detail_scroll = 0;
        self.detail_max_scroll = 0;
        self.detail_drag = None;
        self.clear_details();
    }

    fn drag_details(&mut self, row: u16) {
        let (Some(track), Some(grab)) = (self.detail_track.as_ref(), self.detail_drag) else {
            return;
        };
        let travel = track.height.saturating_sub(track.thumb_height);
        let position =
            (i32::from(row) - track.top - i32::from(grab)).clamp(0, i32::from(travel)) as u16;
        self.detail_scroll = (u32::from(position) * u32::from(self.detail_max_scroll)
            + u32::from(travel) / 2)
            .checked_div(u32::from(travel))
            .unwrap_or(0) as u16;
    }

    fn contains_header(&self, point: ratatui::layout::Position) -> bool {
        [
            self.button,
            self.revision_previous,
            self.working_file,
            self.diff_previous,
            self.diff_committed,
        ]
        .into_iter()
        .flatten()
        .any(|area| area.contains(point))
    }

    fn clear_obscured_header(&mut self, area: Rect) {
        for control in [
            &mut self.button,
            &mut self.revision_previous,
            &mut self.working_file,
            &mut self.diff_previous,
            &mut self.diff_committed,
        ] {
            if control.is_some_and(|control| !control.intersection(area).is_empty()) {
                *control = None;
            }
        }
    }
}

fn eligible(app: &App) -> bool {
    app.source_control_enabled && app.tabs.get(app.active_tab).is_some_and(|tab| {
        matches!(tab.kind, TabKind::File | TabKind::GitRevision)
            && tab.editor.file_path.is_some()
            && tab.editor.encoding != "Binary"
    }) && !app.should_render_diff_for_active_editor()
}

pub(crate) fn has_details(app: &App) -> bool {
    eligible(app)
        && app.editor().is_some_and(|editor| {
            editor.blame.enabled
                && editor.blame.selected.is_some_and(|line| {
                    !editor
                        .folded_ranges
                        .iter()
                        .any(|(&start, &end)| start < line && line < end)
                        && git_blame::cached(app, editor).is_some_and(|doc| {
                            doc.lines
                                .get(line)
                                .is_some_and(|line| !matches!(line, git_blame::Line::Empty))
                        })
                })
        })
}

fn detail_text(app: &App, width: u16) -> Option<(String, String, bool)> {
    let editor = app.editor()?;
    let document = git_blame::cached(app, editor)?;
    match document.lines.get(editor.blame.selected?)? {
        git_blame::Line::Committed(commit) => Some((
            format!(
                "Commit details · {}",
                commit.hash.get(..7).unwrap_or(&commit.hash)
            ),
            commit_details_message(commit, width),
            true,
        )),
        git_blame::Line::Uncommitted => Some((
            "Uncommitted changes".into(),
            "This line belongs to your current working changes and has not been committed.".into(),
            false,
        )),
        git_blame::Line::Empty => None,
    }
}

pub(crate) fn inline_layout(app: &App, source: Rect) -> Option<InlineDetails> {
    if !has_details(app) {
        return None;
    }
    let width = source.width.saturating_sub(2);
    let max_height = source.height.saturating_sub(2).min(16);
    if width < 10 || max_height < 6 {
        return None;
    }
    let (_, message, committed) = detail_text(app, width.saturating_sub(5))?;
    let action_height = if committed {
        detail_actions(width.saturating_sub(4))
            .last()
            .map_or(0, |area| area.y + 1)
    } else {
        0
    };
    let height = (wrapped_message(&message, width.saturating_sub(5)).len()
        + usize::from(action_height + u16::from(committed) + 2))
    .min(usize::from(max_height)) as u16;
    Some(InlineDetails {
        line: app.editor()?.blame.selected?,
        height,
    })
}

fn history_document(app: &App) -> Option<&std::sync::Arc<git_blame::Document>> {
    let editor = app.editor()?;
    app.blame_documents
        .get(&editor.navigation_id)
        .filter(|doc| {
            editor.file_path.as_ref() == Some(&doc.path) && doc.revision == editor.blame.revision
        })
}

fn available(app: &App) -> bool {
    history_document(app).is_some_and(|doc| doc.key.is_some() && doc.notice.is_none())
}

pub(crate) fn toggle(app: &mut App) {
    if !eligible(app) {
        app.notify("Git blame is available in a file editor.");
        return;
    }
    if let Some(notice) = history_document(app).and_then(|doc| doc.notice.clone()) {
        app.notify(notice);
        return;
    }
    let editor = app.editor_mut().unwrap();
    editor.blame.enabled = !editor.blame.enabled;
    editor.blame.dismiss_details();
    if editor.blame.history_loading {
        editor.blame.history_request = editor.blame.history_request.wrapping_add(1);
        editor.blame.history_loading = false;
    }
}

/// Reserve one stable toggle in the existing file breadcrumb, not another row.
pub(crate) fn header(f: &mut Frame, app: &mut App, mut area: Rect) -> Rect {
    if let Some(editor) = app.editor_mut() {
        editor.blame.button = None;
        editor.blame.revision_previous = None;
        editor.blame.working_file = None;
        editor.blame.diff_previous = None;
        editor.blame.diff_committed = None;
    }
    if area.height == 0 || !app.source_control_enabled {
        return area;
    }
    let files = app
        .tabs
        .get(app.active_tab)
        .and_then(|tab| app.diff_tabs.blame_files_for_tab(tab))
        .cloned();
    if let Some((previous, _)) = files {
        if area.width >= 26 {
            let committed = header_action(f, app, &mut area, " Open file ↗ ", true);
            app.editor_mut().unwrap().blame.diff_committed = Some(committed);
            if area.width >= 30 {
                let before =
                    header_action(f, app, &mut area, " Previous file ↗ ", previous.is_some());
                app.editor_mut().unwrap().blame.diff_previous = Some(before);
            }
        }
        return area;
    }
    if !eligible(app) || area.width < 16 {
        return area;
    }
    // An unknown file is actionable: clicking is an explicit request to load
    // blame. Do not probe Git merely to enable the breadcrumb button.
    let ready = history_document(app).is_none_or(|doc| doc.notice.is_none());
    if !ready {
        app.editor_mut().unwrap().blame.enabled = false;
    }
    let button = Rect::new(area.right() - 9, area.y, 9, 1);
    let enabled = app.editor().unwrap().blame.enabled;
    let hovered = button.contains((app.mouse_col, app.mouse_row).into());
    let style = crate::ui_widgets::action_button_style(
        &app.colors,
        app.colors.ui.bg_breadcrumb,
        enabled,
        hovered && ready,
        ready,
    );
    f.render_widget(
        Paragraph::new(if enabled {
            " Blame ✓ "
        } else if ready {
            " Blame   "
        } else {
            " Blame — "
        })
        .style(style),
        button,
    );
    app.editor_mut().unwrap().blame.button = Some(button);
    area.width -= 9;
    let revision = app.editor().unwrap().blame.revision.clone();
    if let Some(revision) = revision {
        if area.width >= 40 {
            let working = header_action(f, app, &mut area, " Working file ", true);
            app.editor_mut().unwrap().blame.working_file = Some(working);
        }
        if area.width >= 26 {
            let has_parent = app.editor().unwrap().blame.previous_revision.is_some();
            let previous = header_action(f, app, &mut area, " Previous ‹ ", has_parent);
            app.editor_mut().unwrap().blame.revision_previous = Some(previous);
        }
        if area.width >= 43 {
            let label = format!(" {} · {} ", revision.short_hash(), revision.date);
            let width = label.width() as u16;
            let context = Rect::new(area.right() - width, area.y, width, 1);
            f.render_widget(
                Paragraph::new(label)
                    .style(app.colors.ui.codelens_style(app.colors.ui.bg_breadcrumb)),
                context,
            );
            area.width -= width;
        }
    }
    area
}

fn header_action(f: &mut Frame, app: &App, area: &mut Rect, label: &str, enabled: bool) -> Rect {
    let width = (label.width() as u16).min(area.width);
    let button = Rect::new(area.right() - width, area.y, width, 1);
    let hovered = enabled && button.contains((app.mouse_col, app.mouse_row).into());
    f.render_widget(
        Paragraph::new(label).style(crate::ui_widgets::action_button_style(
            &app.colors,
            app.colors.ui.bg_breadcrumb,
            false,
            hovered,
            enabled,
        )),
        button,
    );
    area.width -= width;
    button
}

fn clear_hits(app: &mut App) {
    if let Some(editor) = app.editor_mut() {
        editor.blame.hits.clear();
        editor.blame.clear_details();
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    clear_hits(app);
    if let Some(editor) = app.editor_mut() {
        // A hidden breadcrumb or a changed pane layout cannot leave a control
        // over the source text at its previous screen position.
        editor.blame.clear_obscured_header(area);
    }
    if !eligible(app) || !app.editor().is_some_and(|editor| editor.blame.enabled) || area.is_empty()
    {
        crate::editor_ai_review::draw(f, app, area);
        tooltip(f, app);
        return;
    }
    let document = git_blame::cached(app, app.editor().unwrap()).cloned();
    let selected = app.editor().unwrap().blame.selected.filter(|&line| {
        document.as_ref().is_some_and(|doc| {
            doc.lines
                .get(line)
                .is_some_and(|line| !matches!(line, git_blame::Line::Empty))
        })
    });
    let notice = document.as_ref().and_then(|doc| doc.notice.as_deref());
    let rail_width = if notice.is_some() {
        0
    } else {
        (area.width / 3).min(44)
    };
    let body = Rect::new(
        area.x + rail_width,
        area.y,
        area.width.saturating_sub(rail_width),
        area.height,
    );
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(app.colors.bg_editor)),
        area,
    );
    crate::editor_ai_review::draw(f, app, body);
    let Some(code) = app.code_area else {
        return;
    };
    if app.editor().unwrap().ai_review.active {
        // The blame rail scrolls through the same virtual rows as the source.
        app.editor_mut().unwrap().ai_review.area = area;
    }
    let rail = Rect::new(area.x, code.y, rail_width, code.height);
    let editor = app.editor().unwrap();
    let rows = if editor.ai_review.active {
        editor.ai_review.source_rows.clone()
    } else {
        editor.blame.rows.clone()
    };
    let cursor = editor.cursor_line;
    let detail_rows = editor.ai_review.blame_rows.clone();
    let mut hits = Vec::new();
    for row in 0..rail.height {
        let row_area = Rect::new(rail.x, rail.y + row, rail.width, 1);
        let source = rows.get(row as usize).and_then(|row| *row);
        let current = source.is_some_and(|source| source.line_idx == cursor);
        let expanded = source.is_some_and(|source| Some(source.line_idx) == selected);
        let hover = row_area.contains((app.mouse_col, app.mouse_row).into());
        let background = if current || hover || expanded {
            app.colors.bg_line_highlight
        } else {
            app.colors.bg_dark
        };
        let mut style = app
            .colors
            .ui
            .codelens_style(background)
            .remove_modifier(Modifier::ITALIC);
        if hover {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if expanded {
            style = style.add_modifier(Modifier::BOLD);
        }
        f.render_widget(Paragraph::new("").style(style), row_area);
        let Some(source) = source else {
            if rail.width >= 3 && detail_rows.iter().any(|&(screen, _)| screen == row) {
                f.render_widget(
                    Paragraph::new("│").style(style),
                    Rect::new(rail.right() - 3, row_area.y, 1, 1),
                );
            }
            continue;
        };
        let Some(line) = document
            .as_ref()
            .and_then(|doc| doc.lines.get(source.line_idx))
        else {
            if row == 0 {
                let label = document
                    .as_ref()
                    .and_then(|doc| doc.notice.as_deref())
                    .unwrap_or("Loading blame…");
                let text = crate::ui_pickers::truncate_to_width(
                    label,
                    rail.width.saturating_sub(2) as usize,
                );
                f.render_widget(
                    Paragraph::new(text).style(style),
                    Rect::new(rail.x + 1, rail.y, rail.width.saturating_sub(2), 1),
                );
            }
            continue;
        };
        if matches!(line, git_blame::Line::Empty) {
            continue;
        }
        hits.push((row_area, source.line_idx));
        let label = if source.is_first || hover {
            match line {
                git_blame::Line::Committed(commit) => format!("{} {}", commit.date, commit.summary),
                git_blame::Line::Uncommitted => "Uncommitted".into(),
                git_blame::Line::Empty => String::new(),
            }
        } else {
            String::new()
        };
        let label = crate::ui_pickers::truncate_to_width(
            &label,
            rail.width.saturating_sub(if expanded { 3 } else { 2 }) as usize,
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::raw(" "), Span::raw(label)])).style(style),
            Rect::new(rail.x, rail.y + row, rail.width.saturating_sub(1), 1),
        );
        if expanded && rail.width >= 3 && source.is_first {
            f.render_widget(
                Paragraph::new("▾").style(style),
                Rect::new(rail.right() - 2, row_area.y, 1, 1),
            );
        }
    }
    for y in rail.y..rail.bottom() {
        if rail.width > 0 {
            f.render_widget(
                Paragraph::new("│").style(
                    Style::default()
                        .fg(app.colors.ui.fg_line_number)
                        .bg(app.colors.bg_dark),
                ),
                Rect::new(rail.right() - 1, y, 1, 1),
            );
        }
    }
    app.editor_mut().unwrap().blame.hits = hits;
    if let Some(gutter) = app.gutter_area {
        let paint = Rect::new(
            gutter.x,
            code.y,
            code.right().saturating_sub(gutter.x),
            code.height,
        );
        if let Some(inline) = inline_layout(app, paint) {
            details(f, app, paint, inline, &detail_rows);
        }
    }
    if app.editor().unwrap().blame.panel.is_none() {
        app.editor_mut().unwrap().blame.detail_drag = None;
    }
    tooltip(f, app);
}

fn tooltip(f: &mut Frame, app: &mut App) {
    if app.rendering_inactive_editor {
        return;
    }
    let point = (app.mouse_col, app.mouse_row).into();
    if let Some(editor) = app.editor_mut()
        && editor.blame.clicked_annotation.is_some_and(|clicked| {
            !editor
                .blame
                .hits
                .iter()
                .any(|(area, line)| *line == clicked && area.contains(point))
        })
    {
        editor.blame.clicked_annotation = None;
    }
    let Some(editor) = app.editor() else {
        return;
    };
    let message = if editor
        .blame
        .revision_previous
        .is_some_and(|area| area.contains(point))
    {
        Some(editor.blame.previous_notice.clone().unwrap_or_else(|| {
            if editor.blame.previous_revision.is_some() {
                "Open the previous file version with its own line history.".into()
            } else {
                "This file has no earlier version.".into()
            }
        }))
    } else if editor
        .blame
        .working_file
        .is_some_and(|area| area.contains(point))
    {
        Some("Return to the working file, keeping your unsaved edits.".into())
    } else if editor
        .blame
        .diff_previous
        .is_some_and(|area| area.contains(point))
    {
        let existed = app
            .tabs
            .get(app.active_tab)
            .and_then(|tab| app.diff_tabs.blame_files_for_tab(tab))
            .is_some_and(|(previous, _)| previous.is_some());
        Some(if existed {
            "Open the previous file version and continue exploring its blame.".into()
        } else {
            "The file did not exist before this commit.".into()
        })
    } else if editor
        .blame
        .diff_committed
        .is_some_and(|area| area.contains(point))
        || editor
            .blame
            .open_file
            .is_some_and(|area| area.contains(point))
    {
        Some("Open this committed file with blame, without changing the working file.".into())
    } else {
        None
    };
    if let Some(message) = message {
        crate::ui_widgets::Tooltip::hover(vec![Line::raw(message)], &app.colors)
            .prefer_below()
            .render(f, app.mouse_col, app.mouse_row);
        return;
    }
    let document = git_blame::cached(app, editor);
    if !app.rendering_inactive_editor
        && let Some(button) = app.editor().unwrap().blame.button
        && button.contains((app.mouse_col, app.mouse_row).into())
    {
        let lines = if available(app) {
            vec![
                Line::raw("Show each line's commit date and short title."),
                Line::raw("Click an annotation for commit details and actions."),
            ]
        } else {
            vec![Line::raw(
                history_document(app)
                    .and_then(|doc| doc.notice.as_deref())
                    .unwrap_or(if editor.blame.enabled {
                        "Loading Git blame…"
                    } else {
                        "Load Git blame for this file."
                    }),
            )]
        };
        crate::ui_widgets::Tooltip::hover(lines, &app.colors)
            .prefer_below()
            .render(f, button.x, button.y);
    } else if !app.rendering_inactive_editor
        && editor.blame.panel.is_none()
        && let Some((_, line)) = app
            .editor()
            .unwrap()
            .blame
            .hits
            .iter()
            .find(|(area, _)| area.contains((app.mouse_col, app.mouse_row).into()))
        && editor.blame.clicked_annotation != Some(*line)
        && let Some(git_blame::Line::Committed(commit)) =
            document.as_ref().and_then(|doc| doc.lines.get(*line))
    {
        crate::ui_widgets::Tooltip::hover(
            vec![
                Line::raw(format!("{} · {}", commit.date, commit.summary)),
                Line::raw(format!(
                    "{} · {}",
                    commit.author,
                    commit.hash.chars().take(7).collect::<String>()
                )),
                Line::raw("Click for details, Open Commit, Compare, Open File and Copy Hash."),
            ],
            &app.colors,
        )
        .prefer_below()
        .render(f, app.mouse_col, app.mouse_row);
    }
}

pub(crate) fn wrapped_message(message: &str, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for line in message.split('\n') {
        let mut text = String::new();
        let mut columns = 0;
        for grapheme in line.graphemes(true) {
            let size = grapheme.width();
            if columns + size > width as usize && !text.is_empty() {
                rows.push(Line::raw(std::mem::take(&mut text)));
                columns = 0;
            }
            text.push_str(grapheme);
            columns += size;
        }
        rows.push(Line::raw(text));
    }
    rows
}

const DETAIL_ACTIONS: [&str; 4] = ["Open Commit", "Compare", "Open File", "Copy Hash"];

pub(crate) fn commit_details_message(commit: &git_blame::Commit, width: u16) -> String {
    let author = if commit.email.is_empty() {
        commit.author.clone()
    } else {
        format!("{} <{}>", commit.author, commit.email)
    };
    let mut text = String::new();
    for (label, value) in [
        ("Hash", commit.hash.as_str()),
        ("Author", author.as_str()),
        ("Committed", commit.committed_at.as_str()),
    ] {
        if 11 + value.width() <= width as usize {
            text.push_str(&format!("{label:<11}{value}\n"));
        } else {
            text.push_str(&format!("{label}\n{value}\n"));
        }
    }
    text.push('\n');
    text.push_str(if commit.message.trim().is_empty() {
        &commit.summary
    } else {
        commit.message.trim_end()
    });
    text
}

/// Keep all actions available in a narrow pane by wrapping the action row.
fn detail_actions(width: u16) -> Vec<Rect> {
    if width == 0 {
        return Vec::new();
    }
    let mut areas = Vec::new();
    let (mut x, mut y) = (0, 0);
    for label in DETAIL_ACTIONS {
        let size = (label.width() as u16 + 2).min(width);
        if x > 0 && x + size > width {
            x = 0;
            y += 1;
        }
        areas.push(Rect::new(x, y, size, 1));
        x += size + 1;
    }
    areas
}

/// Translate card-local controls through the visible part of its virtual rows.
fn inline_rect(local: Rect, x: u16, origin_y: i32, visible: Rect) -> Option<Rect> {
    let top = (origin_y + i32::from(local.y)).max(i32::from(visible.y));
    let bottom = (origin_y + i32::from(local.bottom())).min(i32::from(visible.bottom()));
    if local.is_empty() || top >= bottom {
        return None;
    }
    let result = Rect::new(x + local.x, top as u16, local.width, (bottom - top) as u16)
        .intersection(visible);
    (!result.is_empty()).then_some(result)
}

fn details(
    f: &mut Frame,
    app: &mut App,
    source: Rect,
    inline: InlineDetails,
    visible_rows: &[(u16, u16)],
) {
    let Some(&(first_row, first_offset)) = visible_rows.first() else {
        return;
    };
    let width = source.width.saturating_sub(2);
    let Some((title, message, committed)) = detail_text(app, width.saturating_sub(5)) else {
        return;
    };
    let visible = Rect::new(
        source.x + 1,
        source.y + first_row,
        width,
        visible_rows.len() as u16,
    );
    let origin_y = i32::from(visible.y) - i32::from(first_offset);
    let area = Rect::new(0, 0, width, inline.height);
    let mut cells = Buffer::empty(area);
    let background = crate::ui_widgets::widget_background(f, &app.colors, visible);
    let foreground = app.colors.ui.widget_text_color(background);
    let border = app
        .colors
        .ui
        .widget_border
        .border(background)
        .unwrap_or(background);
    let style = Style::default().bg(background).fg(foreground);
    let title = crate::ui_pickers::truncate_to_width(&title, width.saturating_sub(7) as usize);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style.fg(border))
        .style(style)
        .title(Span::styled(
            format!(" {title} "),
            style.add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    block.render(area, &mut cells);
    let close = Rect::new(area.right() - 4, area.y, 3, 1);
    let close_hit = inline_rect(close, visible.x, origin_y, visible);
    Paragraph::new(" × ")
        .style(crate::ui_widgets::action_button_style(
            &app.colors,
            background,
            false,
            close_hit.is_some_and(|area| area.contains((app.mouse_col, app.mouse_row).into())),
            true,
        ))
        .render(close, &mut cells);
    let actions = if committed {
        detail_actions(width.saturating_sub(4))
    } else {
        Vec::new()
    };
    let action_height = actions.last().map_or(0, |area| area.y + 1);
    let separator = u16::from(committed);
    for (index, relative) in actions.iter().enumerate() {
        let button = Rect::new(
            inner.x + 1 + relative.x,
            inner.y + relative.y,
            relative.width,
            1,
        );
        let hit = inline_rect(button, visible.x, origin_y, visible);
        let label = DETAIL_ACTIONS[index];
        let text = if button.width as usize >= label.width() + 2 {
            format!(" {label} ")
        } else {
            crate::ui_pickers::truncate_to_width(label, button.width as usize)
        };
        Paragraph::new(text)
            .style(crate::ui_widgets::action_button_style(
                &app.colors,
                background,
                false,
                hit.is_some_and(|area| area.contains((app.mouse_col, app.mouse_row).into())),
                true,
            ))
            .render(button, &mut cells);
        let view = &mut app.editor_mut().unwrap().blame;
        match index {
            0 => view.open_commit = hit,
            1 => view.previous = hit,
            2 => view.open_file = hit,
            _ => view.copy = hit,
        }
    }
    if committed {
        Paragraph::new("─".repeat(inner.width as usize))
            .style(style.fg(border))
            .render(
                Rect::new(inner.x, inner.y + action_height, inner.width, 1),
                &mut cells,
            );
    }
    let content = Rect::new(
        inner.x + 1,
        inner.y + action_height + separator,
        inner.width.saturating_sub(3),
        inner.height.saturating_sub(action_height + separator),
    );
    let rows = wrapped_message(&message, content.width);
    let max_scroll = rows
        .len()
        .saturating_sub(content.height as usize)
        .min(u16::MAX as usize) as u16;
    let scroll = app.editor().unwrap().blame.detail_scroll.min(max_scroll);
    let total_rows = rows.len();
    Paragraph::new(rows)
        .style(style)
        .scroll((scroll, 0))
        .render(content, &mut cells);
    if max_scroll > 0 && content.height > 0 {
        let scrollbar = Rect::new(inner.right() - 1, content.y, 1, content.height);
        let (_, size) = crate::inline_diff::scrollbar_thumb(
            total_rows,
            content.height as usize,
            scrollbar.height as usize,
            1,
        );
        let top =
            usize::from(scroll) * (scrollbar.height as usize - size) / usize::from(max_scroll);
        let thumb = Rect::new(scrollbar.x, scrollbar.y + top as u16, 1, size as u16);
        let scrollbar_hit = inline_rect(scrollbar, visible.x, origin_y, visible);
        let (track, slider) = app.colors.ui.scrollbar_colors(
            background,
            scrollbar_hit.is_some_and(|area| area.contains((app.mouse_col, app.mouse_row).into())),
            app.editor().unwrap().blame.detail_drag.is_some(),
        );
        for row in scrollbar.y..scrollbar.bottom() {
            let active = thumb.contains((scrollbar.x, row).into());
            Paragraph::new(if active { "█" } else { "│" })
                .style(style.bg(track).fg(if active { slider } else { border }))
                .render(Rect::new(scrollbar.x, row, 1, 1), &mut cells);
        }
        let view = &mut app.editor_mut().unwrap().blame;
        view.detail_scrollbar = scrollbar_hit;
        view.detail_thumb = inline_rect(thumb, visible.x, origin_y, visible);
        // Keep the full geometry, even when the editor clips the card. Pointer
        // dragging then retains the right grip and reaches both message ends.
        view.detail_track = Some(DetailScrollbar {
            top: origin_y + i32::from(scrollbar.y),
            height: scrollbar.height,
            thumb_top: origin_y + i32::from(thumb.y),
            thumb_height: thumb.height,
        });
    }
    for &(screen, offset) in visible_rows {
        for column in 0..width {
            f.buffer_mut()[(visible.x + column, source.y + screen)] =
                cells[(column, offset)].clone();
        }
    }
    let view = &mut app.editor_mut().unwrap().blame;
    view.close = close_hit;
    view.panel = Some(visible);
    view.detail_content = inline_rect(content, visible.x, origin_y, visible);
    view.detail_scroll = scroll;
    view.detail_max_scroll = max_scroll;
}

pub(crate) fn hovered(app: &App, column: u16, row: u16) -> bool {
    let point = (column, row).into();
    app.tabs
        .get(app.active_tab)
        .into_iter()
        .chain(
            app.groups
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != app.active_group)
                .filter_map(|(_, group)| group.tabs.get(group.active_tab)),
        )
        .any(|tab| {
            tab.editor.blame.contains_header(point)
                || tab
                    .editor
                    .blame
                    .hits
                    .iter()
                    .any(|(area, _)| area.contains(point))
                || tab
                    .editor
                    .blame
                    .panel
                    .is_some_and(|area| area.contains(point))
        })
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let point = (mouse.column, mouse.row).into();
    // A gesture started in the card owns its drag/release even outside its
    // bounds; otherwise dragging its scrollbar can select surrounding source.
    if let Some(view) = app.editor_mut().map(|editor| &mut editor.blame) {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            view.detail_mouse_down = false;
            view.detail_drag = None;
        } else if view.detail_mouse_down {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    view.drag_details(mouse.row);
                    return true;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    view.detail_mouse_down = false;
                    view.detail_drag = None;
                    return true;
                }
                _ => {}
            }
        }
    }
    // Tabs retain their controls across splits and moves. Focus the pane owning
    // this rail before acting; inactive copies of the active group are ignored.
    if let Some(index) = app
        .groups
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != app.active_group)
        .find_map(|(index, group)| {
            group
                .tabs
                .get(group.active_tab)
                .filter(|tab| {
                    tab.editor.blame.contains_header(point)
                        || tab
                            .editor
                            .blame
                            .hits
                            .iter()
                            .any(|(area, _)| area.contains(point))
                        || tab
                            .editor
                            .blame
                            .panel
                            .is_some_and(|area| area.contains(point))
                })
                .map(|_| index)
        })
    {
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
        ) || (matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) && app.groups[index].tabs[app.groups[index].active_tab]
            .editor
            .blame
            .panel
            .is_some_and(|area| area.contains(point)))
        {
            app.switch_group(index);
        } else {
            return false;
        }
    }
    let Some(editor) = app.editor() else {
        return false;
    };
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) && editor.blame.contains_header(point)
    {
        app.terminal_panel_focused = false;
        app.search_input_focused = false;
        app.search_pre_cursor = None;
        app.active_pane = ActivePane::Editor;
        let editor = app.editor().unwrap();
        if editor.blame.button.is_some_and(|area| area.contains(point)) {
            toggle(app);
        } else if editor
            .blame
            .revision_previous
            .is_some_and(|area| area.contains(point))
        {
            if let Some(revision) = editor.blame.previous_revision.clone() {
                git_blame::open_revision(app, revision, true);
            }
        } else if editor
            .blame
            .working_file
            .is_some_and(|area| area.contains(point))
        {
            if let Some(revision) = editor.blame.revision.clone() {
                app.open_file(&revision.working_file);
            }
        } else {
            let previous = editor
                .blame
                .diff_previous
                .is_some_and(|area| area.contains(point));
            let revision = app
                .tabs
                .get(app.active_tab)
                .and_then(|tab| app.diff_tabs.blame_files_for_tab(tab))
                .and_then(|(before, after)| {
                    if previous {
                        before.clone()
                    } else {
                        Some(after.clone())
                    }
                });
            if let Some(revision) = revision {
                git_blame::open_revision(app, revision, previous);
            }
        }
        return true;
    }
    if !eligible(app) {
        return false;
    }
    let editor = app.editor().unwrap();
    if matches!(
        mouse.kind,
        MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
    ) && (editor.blame.button.is_some_and(|area| area.contains(point))
        || editor.blame.panel.is_some_and(|area| area.contains(point))
        || editor
            .blame
            .hits
            .iter()
            .any(|(area, _)| area.contains(point)))
    {
        app.terminal_panel_focused = false;
        app.search_input_focused = false;
        app.search_pre_cursor = None;
        app.active_pane = ActivePane::Editor;
        app.drag_start_pos = None;
        app.drag_has_moved = false;
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            app.editor_mut().unwrap().blame.detail_mouse_down = true;
        }
    }
    let editor = app.editor().unwrap();
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        if editor
            .blame
            .detail_scrollbar
            .is_some_and(|area| area.contains(point))
        {
            let on_thumb = editor
                .blame
                .detail_thumb
                .is_some_and(|thumb| thumb.contains(point));
            let track = editor.blame.detail_track.as_ref().unwrap();
            let grab = if on_thumb {
                (i32::from(mouse.row) - track.thumb_top).max(0) as u16
            } else {
                track.thumb_height / 2
            };
            let view = &mut app.editor_mut().unwrap().blame;
            view.detail_drag = Some(grab);
            if !on_thumb {
                view.drag_details(mouse.row);
            }
            return true;
        }
        if editor.blame.open_commit.is_some_and(|area| area.contains(point)) {
            let details = git_blame::cached(app, editor).and_then(|doc| {
                let git_blame::Line::Committed(commit) = doc.lines.get(editor.blame.selected?)? else {
                    return None;
                };
                let root = doc.revision.as_ref().map(|revision| revision.workdir.clone())
                    .or_else(|| doc.path.parent().map(|parent| parent.to_owned()))?;
                Some((root, crate::commit_details::CommitData {
                    hash: commit.hash.clone(),
                    message: commit.message.clone(),
                    author: commit.author.clone(),
                    email: commit.email.clone(),
                    date: commit.committed_at.clone(),
                    time_ago: commit.age.clone(),
                    ..Default::default()
                }))
            });
            if let Some((root, data)) = details {
                let view = &mut app.editor_mut().unwrap().blame;
                view.dismiss_details();
                view.detail_mouse_down = false;
                app.open_commit_details(root, data);
            }
            return true;
        }
        if editor
            .blame
            .open_file
            .is_some_and(|area| area.contains(point))
        {
            if let Some(line) = editor.blame.selected {
                git_blame::open_line_file(app, line);
            }
            return true;
        }
        if editor.blame.close.is_some_and(|area| area.contains(point)) {
            let unavailable =
                git_blame::cached(app, editor).is_some_and(|doc| doc.notice.is_some());
            app.editor_mut().unwrap().blame.dismiss_details();
            if unavailable {
                app.editor_mut().unwrap().blame.enabled = false;
            }
            return true;
        }
        if editor.blame.copy.is_some_and(|area| area.contains(point)) {
            let hash = git_blame::cached(app, editor)
                .and_then(|doc| doc.lines.get(editor.blame.selected?))
                .and_then(|line| {
                    if let git_blame::Line::Committed(commit) = line {
                        Some(commit.hash.clone())
                    } else {
                        None
                    }
                });
            if let Some(hash) = hash {
                let result = crate::clipboard::set_text(hash);
                if result.is_ok() {
                    app.notify("Copied commit hash");
                } else {
                    app.notify_error("Could not copy commit hash");
                }
            }
            return true;
        }
        if editor
            .blame
            .previous
            .is_some_and(|area| area.contains(point))
        {
            if let Some(line) = editor.blame.selected {
                git_blame::open_previous(app, line);
            }
            return true;
        }
    }
    if matches!(
        mouse.kind,
        MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
    ) && let Some(line) = editor
        .blame
        .hits
        .iter()
        .find(|(area, _)| area.contains(point))
        .map(|(_, line)| *line)
    {
        app.active_pane = ActivePane::Editor;
        let editor = app.editor_mut().unwrap();
        editor.blame.clicked_annotation = Some(line);
        // Reading blame should preserve the caret, selection and folded rows.
        // History actions use the selected blame line independently of them.
        if editor.blame.selected == Some(line) {
            editor.blame.dismiss_details();
        } else {
            editor.blame.dismiss_details();
            editor.blame.selected = Some(line);
        }
        return true;
    }
    let editor = app.editor().unwrap();
    if editor.blame.panel.is_some_and(|area| area.contains(point)) {
        match mouse.kind {
            MouseEventKind::ScrollDown if editor.blame.detail_max_scroll > 0 => {
                let view = &mut app.editor_mut().unwrap().blame;
                view.detail_scroll = view
                    .detail_scroll
                    .saturating_add(1)
                    .min(view.detail_max_scroll);
                return true;
            }
            MouseEventKind::ScrollUp if editor.blame.detail_max_scroll > 0 => {
                let view = &mut app.editor_mut().unwrap().blame;
                view.detail_scroll = view.detail_scroll.saturating_sub(1);
                return true;
            }
            MouseEventKind::Down(_) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.code == KeyCode::Esc
        && app.active_pane == ActivePane::Editor
        && app
            .editor()
            .is_some_and(|editor| editor.blame.selected.is_some() || editor.blame.panel.is_some())
    {
        let unavailable =
            git_blame::cached(app, app.editor().unwrap()).is_some_and(|doc| doc.notice.is_some());
        let view = &mut app.editor_mut().unwrap().blame;
        view.dismiss_details();
        if unavailable {
            view.enabled = false;
        }
        return true;
    }
    false
}

#[cfg(test)]
#[path = "editor_blame_tests.rs"]
mod tests;
