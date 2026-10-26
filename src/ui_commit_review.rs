//! File cards in a single scrolling review. There are no nested vertical
//! scroll regions: the native diff renderer paints only each visible slice.
use super::*;
use crate::commit_review::{Card, Geometry};

pub(super) fn draw_controls(f: &mut Frame, app: &App, view: &InspectorLayout) {
    let details = &app.commit_details;
    let context = details.diff_context;
    let bg = app.colors.bg_editor;
    let count = details
        .context_draft
        .clone()
        .unwrap_or_else(|| context.lines.to_string());
    let count = if count.is_empty() { "_".into() } else { count };
    let mode = if crate::commit_review::mode(app, view.files.width) == crate::app::DiffMode::Inline
    {
        "Side by Side"
    } else {
        "Inline"
    };
    for (action, area, label) in [
        (
            15,
            view.expand_all,
            if view.expand_all.width < 10 {
                "+All"
            } else {
                "Expand all"
            }
            .into(),
        ),
        (
            16,
            view.collapse_all,
            if view.collapse_all.width < 10 {
                "−All"
            } else {
                "Collapse all"
            }
            .into(),
        ),
        (9, view.diff_toggle, mode.to_string()),
        (10, view.diff_full, "Full file".to_string()),
        (11, view.diff_changes, "Changes".to_string()),
        (12, view.context_less, "−".to_string()),
        (13, view.context_more, "+".to_string()),
        (14, view.context_count, count),
        (6, view.hunk_previous, "↑".to_string()),
        (7, view.hunk_next, "↓".to_string()),
    ] {
        let selected =
            action == 10 && !context.changes_only || action == 11 && context.changes_only;
        let focused = details.focus == Focus::Action(action);
        let style = app.colors.ui.button_style(
            bg,
            selected,
            focused || area.contains((app.mouse_col, app.mouse_row).into()),
            enabled(app, action),
        );
        row(
            f,
            area,
            if area.width <= 5 {
                format!(" {label}")
            } else {
                format!(" {label} ")
            },
            if focused {
                style.add_modifier(Modifier::UNDERLINED)
            } else {
                style
            },
        );
    }
    row(
        f,
        view.context_label,
        "± ",
        Style::default()
            .fg(app.colors.ui.description_color(bg))
            .bg(bg),
    );
}

/// Anchor the viewport by file and local row when asynchronous data replaces
/// estimated heights above it. Loading a previous card must not move the code
/// the user is reading.
fn cards(app: &mut App, area: Rect, filtered: &[usize]) -> Vec<Card> {
    let review = &mut app.commit_details.review;
    let mut start = 0usize;
    let mut positions = Vec::with_capacity(filtered.len());
    for &file in filtered {
        let height = review.height(file);
        positions.push((file, start, height));
        start = start.saturating_add(height);
    }
    review.total = start;
    if let Some(anchor) = review.visible.first()
        && let Some((_, current, _)) = positions.iter().find(|(file, _, _)| *file == anchor.file)
        && !review.reveal
    {
        review.scroll = review
            .scroll
            .saturating_add(*current)
            .saturating_sub(anchor.start);
    }
    if review.reveal {
        if let Some((_, start, _)) = positions
            .iter()
            .find(|(file, _, _)| Some(*file) == filtered.get(app.commit_details.selected).copied())
        {
            review.scroll = *start;
        }
        review.reveal = false;
    }
    review.scroll = review
        .scroll
        .min(review.total.saturating_sub(area.height as usize));
    let first = review.scroll;
    let end = first.saturating_add(area.height as usize);
    let mut visible = Vec::new();
    for (file, start, height) in positions {
        if start.saturating_add(height) <= first {
            continue;
        }
        if start >= end {
            break;
        }
        let y = area.y + start.saturating_sub(first) as u16;
        // A sticky header identifies a long diff while its beginning is above
        // the viewport. It belongs to the card, never a floating modal popup.
        let header = Rect::new(
            area.x,
            y,
            area.width.saturating_sub(1),
            1.min(area.bottom().saturating_sub(y)),
        );
        let bottom = area.y.saturating_add(
            (start.saturating_add(height).saturating_sub(first)).min(area.height as usize) as u16,
        );
        let body_y = header.bottom().min(bottom);
        let body = Rect::new(
            area.x + u16::from(area.width > 2),
            body_y,
            area.width.saturating_sub(3),
            bottom.saturating_sub(body_y).saturating_sub(1),
        );
        let open = if header.width >= 12 {
            Rect::new(header.right() - 3, y, 3, header.height)
        } else {
            Rect::default()
        };
        visible.push(Card {
            file,
            start,
            header,
            body,
            open,
            first_row: first.saturating_sub(start.saturating_add(1)),
            ..Default::default()
        });
    }
    visible
}

fn header(f: &mut Frame, app: &App, card: &Card, selected: bool) {
    let file = &app.commit_details.data.files[card.file];
    let bg = app.colors.bg_editor;
    let style = app.colors.ui.list_row_style(
        app.colors.ui.widget_foreground,
        bg,
        selected,
        app.commit_details.focus == Focus::Files,
        card.header.contains((app.mouse_col, app.mouse_row).into()),
    );
    let row_bg = style.bg.unwrap_or(bg);
    let fg = style.fg.unwrap_or(app.colors.fg_text);
    let status = app.colors.ui.git_status_color(file.status, row_bg);
    let stats = if file.binary {
        "binary".into()
    } else {
        format!("+{} −{}", file.added, file.deleted)
    };
    let stats_width = if card.header.width >= 25 {
        stats.width() as u16 + 2
    } else {
        0
    };
    let text_width = card
        .header
        .width
        .saturating_sub(5 + card.open.width + stats_width);
    let expanded = !app.commit_details.review.collapsed.contains(&card.file);
    let path = if let Some(old) = &file.old_path {
        format!("{old} → {}", file.path)
    } else {
        file.path.clone()
    };
    let spans = vec![
        Span::styled(if expanded { " ▾ " } else { " ▸ " }, style),
        Span::styled(
            file.status.to_string(),
            style.fg(if selected { fg } else { status }),
        ),
        Span::styled(" ", style),
        Span::styled(
            display_path(&path, text_width as usize),
            style.add_modifier(Modifier::BOLD),
        ),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)).style(style), card.header);
    if stats_width > 0 {
        let area = Rect::new(
            card.header.right() - card.open.width - stats_width,
            card.header.y,
            stats_width,
            card.header.height,
        );
        let spans = if file.binary {
            vec![Span::styled(" binary ", style)]
        } else {
            vec![
                Span::styled(
                    format!(" +{}", file.added),
                    style.fg(if selected {
                        fg
                    } else {
                        app.colors.ui.git_status_color('A', row_bg)
                    }),
                ),
                Span::styled(
                    format!(" −{} ", file.deleted),
                    style.fg(if selected {
                        fg
                    } else {
                        app.colors.ui.git_status_color('D', row_bg)
                    }),
                ),
            ]
        };
        f.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
    }
    row(
        f,
        card.open,
        if app.commit_details.review.errors.contains_key(&card.file) {
            " ↻ "
        } else {
            " ↗ "
        },
        app.colors.ui.button_style(
            row_bg,
            false,
            card.open.contains((app.mouse_col, app.mouse_row).into()),
            !file.binary,
        ),
    );
}

pub(super) fn draw(f: &mut Frame, app: &mut App, view: &InspectorLayout) {
    let area = view.files;
    if area.is_empty() {
        return;
    }
    let filtered = app.commit_details.filtered_files();
    let bg = app.colors.bg_editor;
    let muted = Style::default()
        .fg(app.colors.ui.description_color(bg))
        .bg(bg);
    let message = if app.commit_details.loading {
        Some("Loading commit changes…")
    } else if app.commit_details.error.is_some() {
        Some("Could not load commit. Click to retry · R")
    } else if app.commit_details.data.files.is_empty() {
        Some("No file changes in this commit")
    } else if filtered.is_empty() {
        Some("No matching files. Esc clears the filter.")
    } else {
        None
    };
    if let Some(message) = message {
        let mut lines = wrapped(message, area.width as usize);
        if let Some(error) = &app.commit_details.error {
            lines.extend(wrapped(error, area.width as usize));
        }
        f.render_widget(
            Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()).style(muted),
            area,
        );
        app.commit_details.review.visible.clear();
        return;
    }
    let mode = crate::commit_review::mode(app, area.width);
    let context = app.commit_details.diff_context;
    if app.commit_details.review.mode != Some(mode) {
        app.commit_details.review.mode = Some(mode);
        app.commit_details.review.heights.clear();
    }
    app.prepare_commit_review_context();
    if let Some(file) = app.commit_details.diff_file
        && app.commit_details.diff_full_cache.is_some()
    {
        let height = crate::commit_review::rows(&app.diff_render_cache, mode)
            .max(1)
            .saturating_add(4);
        app.commit_details.review.heights.insert(file, height);
    }
    for (&file, view) in &mut app.commit_details.review.cached {
        let height = view.review_height(context, mode);
        app.commit_details.review.heights.insert(file, height);
    }
    // Native keyboard/search navigation keeps the target row in the outer
    // viewport; ordinary wheel scrolling never follows an unchanged caret.
    if app.commit_details.focus == Focus::Diff
        && let (Some(file), Some(editor)) = (app.commit_details.diff_file, app.editor())
        && app
            .commit_details
            .review
            .caret
            .is_some_and(|previous| previous != (file, editor.cursor_line))
    {
        let row = if mode == crate::app::DiffMode::SideBySide {
            app.diff_render_cache
                .aligned_row_for_target_line(editor.cursor_line)
        } else {
            app.diff_render_cache
                .inline_row_for_target_line(editor.cursor_line)
        };
        let start: usize = filtered
            .iter()
            .take_while(|index| **index != file)
            .map(|index| app.commit_details.review.height(*index))
            .sum();
        let target = start.saturating_add(row).saturating_add(2);
        let review = &mut app.commit_details.review;
        if target < review.scroll.saturating_add(2) {
            review.scroll = target.saturating_sub(2);
        } else if target
            >= review
                .scroll
                .saturating_add(area.height as usize)
                .saturating_sub(2)
        {
            review.scroll = target
                .saturating_add(3)
                .saturating_sub(area.height as usize);
        }
    }
    let mut visible = cards(app, area, &filtered);
    let selected = app.commit_details.selected_file();
    let original = app.commit_details.diff_file;
    let editor_area = app.editor_area;
    let inactive = app.rendering_inactive_editor;
    let cursor = app.cursor_visible;
    let mut selected_geometry = Geometry::default();
    let mut selected_body = Rect::default();
    for card in &mut visible {
        let file_selected = selected == Some(card.file);
        header(f, app, card, file_selected);
        if app.commit_details.review.collapsed.contains(&card.file) {
            continue;
        }
        if card.body.is_empty() {
            continue;
        }
        let ready = app.activate_commit_review_file(card.file);
        if ready {
            app.prepare_commit_review_context();
            app.diff_mode = mode;
            app.commit_details.review.paint_row = Some(card.first_row);
            app.cursor_visible = cursor && file_selected && app.commit_details.focus == Focus::Diff;
            app.rendering_inactive_editor = inactive || !file_selected;
            if mode == crate::app::DiffMode::SideBySide {
                crate::ui_diff_display::draw_side_by_side_diff(f, app, card.body);
            } else {
                crate::ui_diff_display::draw_inline_diff(f, app, card.body);
            }
            // Use one scrollbar for the review, not one thumb per file.
            if let Some(bar) = app.scrollbar_area.take() {
                f.render_widget(Block::default().style(Style::default().bg(bg)), bar);
            }
            card.geometry = Geometry::capture(app);
            if file_selected {
                selected_geometry = card.geometry;
                selected_body = card.body;
            }
            // Each card owns its request key/cancellation token when suspended.
            // Identical frames neither allocate a job nor re-highlight a file.
            app.refresh_current_editor_highlights_async();
            app.commit_details.review.paint_row = None;
            app.rendering_inactive_editor = inactive;
            app.cursor_visible = cursor;
        } else {
            let file = &app.commit_details.data.files[card.file];
            let text = if file.binary {
                "Binary file changed · no text diff"
            } else if let Some(error) = app.commit_details.review.errors.get(&card.file) {
                error
            } else if app.commit_details.review.pending == Some(card.file) {
                "Loading diff…"
            } else {
                "Diff will load when visible…"
            };
            f.render_widget(
                Paragraph::new(
                    wrapped(text, card.body.width as usize)
                        .into_iter()
                        .map(Line::from)
                        .collect::<Vec<_>>(),
                )
                .style(muted),
                card.body,
            );
        }
        if area.width > 2 {
            let border = app.colors.ui.readable_foreground(
                app.colors
                    .ui
                    .widget_border
                    .border(bg)
                    .unwrap_or(app.colors.fg_dim),
                bg,
                4.5,
            );
            for y in card.header.bottom()..card.body.bottom() {
                for x in [area.x, area.right() - 2] {
                    if let Some(cell) = f.buffer_mut().cell_mut((x, y)) {
                        cell.set_symbol("│")
                            .set_style(Style::default().fg(border).bg(bg));
                    }
                }
            }
        }
    }
    if let Some(file) = selected.filter(|file| app.commit_details.review.cached.contains_key(file))
    {
        app.activate_commit_review_file(file);
    } else if let Some(file) =
        original.filter(|file| app.commit_details.review.cached.contains_key(file))
    {
        app.activate_commit_review_file(file);
    }
    selected_geometry.install(app);
    app.editor_area = editor_area;
    app.diff_mode = mode;
    app.commit_details.review.caret = app
        .commit_details
        .diff_file
        .zip(app.editor().map(|editor| editor.cursor_line));
    app.commit_details.layout.as_mut().unwrap().patch = selected_body;
    app.commit_details.review.visible = visible;
    app.commit_details.diff_loading = app.commit_details.review.pending.is_some()
        && app.commit_details.review.pending == selected;
    app.commit_details.diff_error =
        selected.and_then(|file| app.commit_details.review.errors.get(&file).cloned());
    // A single in-flight file read bounds CPU, disk and SSH traffic, even when
    // expanding thousands of cards. Collapsed and offscreen files stay lazy.
    if app.commit_details.review.pending.is_none() && app.source_control_enabled {
        let next = app.commit_details.review.visible.iter().find_map(|card| {
            (!app.commit_details.review.collapsed.contains(&card.file)
                && !app.commit_details.data.files[card.file].binary
                && !app.commit_details.review.cached.contains_key(&card.file)
                && !(app.commit_details.diff_file == Some(card.file)
                    && app.commit_details.diff_full_cache.is_some())
                && !app.commit_details.review.errors.contains_key(&card.file))
            .then_some(card.file)
        });
        if let Some(file) = next {
            app.queue_commit_review_file(file);
        }
    }
    app.evict_commit_review_files();
    if app.commit_details.review.total > area.height as usize {
        let (track, thumb) = app.colors.ui.scrollbar_colors(
            bg,
            app.mouse_col == area.right() - 1 && (area.y..area.bottom()).contains(&app.mouse_row),
            app.commit_details
                .scrollbar_drag
                .is_some_and(|(focus, _)| focus == Focus::Files),
        );
        crate::ui_widgets::VerticalScrollbar::new(
            area.right() - 1,
            area.y,
            area.height,
            app.commit_details.review.total,
            app.commit_details.review.scroll,
        )
        .symbols('┃', '│')
        .colors(thumb, track, bg)
        .render(f);
    }
}

pub(super) fn toggle(app: &mut App, file: usize) {
    let review = &mut app.commit_details.review;
    // Pin the toggled header when collapsing its long body; otherwise a large
    // offset could jump past it to an unrelated file.
    if let Some(card) = review.visible.iter().find(|card| card.file == file) {
        review.scroll = review.scroll.min(card.start);
    }
    review.toggle(file);
}

pub(super) fn click_card(app: &mut App, col: u16, row: u16) -> bool {
    let point = (col, row).into();
    let card = app
        .commit_details
        .review
        .visible
        .iter()
        .find(|card| card.header.contains(point) || card.body.contains(point))
        .copied();
    let Some(card) = card else {
        return false;
    };
    let filtered = app.commit_details.filtered_files();
    let Some(position) = filtered.iter().position(|file| *file == card.file) else {
        // A filter edit can precede the next paint. Never let an old header's
        // Open action resolve to the first file in the newly filtered list.
        return true;
    };
    if card.body.contains(point) && app.commit_details.review.collapsed.contains(&card.file) {
        return true;
    }
    app.commit_details.selected = position;
    if card.open.contains(point) {
        if app
            .commit_details
            .review
            .errors
            .remove(&card.file)
            .is_some()
        {
            app.queue_commit_review_file(card.file);
        } else if !app.commit_details.data.files[card.file].binary {
            activate(app, 4);
        }
    } else if card.header.contains(point) {
        app.commit_details.focus = Focus::Files;
        toggle(app, card.file);
    } else {
        app.commit_details.focus = Focus::Diff;
        app.commit_details.review.caret = None;
        if app.activate_commit_review_file(card.file) {
            card.geometry.install(app);
        }
    }
    true
}
