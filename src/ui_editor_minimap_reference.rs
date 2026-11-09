//! Uncached Paragraph renderer used as a pixel/style oracle in tests.
use super::*;

pub(super) fn draw_minimap_reference(f: &mut Frame, p: MinimapParams) {
    let MinimapParams {
        area,
        editor,
        scroll,
        visible_lines,
        search_matches,
        diagnostic_lines,
        diagnostic_index,
        change_index,
        diff_changes,
        colors,
        hovered,
        active,
    } = p;
    let height = area.height as usize;
    let width = area.width as usize;
    let total_lines = editor.rope.len_lines();
    let visible_total = editor.visible_line_count();
    if height == 0 || width == 0 || total_lines == 0 {
        return;
    }

    let layout = MinimapLayout::new(
        visible_total,
        editor.visible_row_for_line(scroll),
        visible_lines,
        height,
        editor.scroll_past_end,
    );
    let window = layout.window;
    let view_start_row = layout.view_start;
    let view_end_row = layout.view_end;
    let selection_lines = editor.selection_range().map(|(sl, _, el, _)| (sl, el));

    // Pre-build a pool of spaces for padding instead of " ".repeat() per row
    let spaces_pool: &str = &crate::helpers::spaces(width.max(1));

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    let mut glyphs = Vec::new();
    let mut raster = Vec::new();
    let mut spans: Vec<Span> = Vec::with_capacity(width + 1);
    let mut batch_buf = String::with_capacity(width);
    let marker_max_width = if width == 0 { 0 } else { width.clamp(1, 3) };
    let marker_str_detail = crate::helpers::blocks(marker_max_width);
    let track_bg = colors.ui.bg_minimap;
    let slider_bg = if active {
        colors.ui.bg_minimap_slider_active
    } else if hovered {
        colors.ui.bg_minimap_slider_hover
    } else {
        colors.ui.bg_minimap_slider
    };
    for row in 0..height {
        let (source_row, end_row) = minimap_source_range(window, row, height);
        let source_line = editor.line_for_visible_row(source_row.min(visible_total));
        let row_end_line = editor.line_for_visible_row(end_row.min(visible_total));
        let in_viewport = row >= view_start_row && row < view_end_row;
        let has_search_match = has_search_match_in_range(search_matches, source_line, row_end_line);
        let has_bookmark = editor
            .bookmarks
            .range(source_line..row_end_line)
            .next()
            .is_some();
        let diff_status = if let Some(changes) = diff_changes {
            change_index.map_or_else(
                || minimap_change_in_range(changes, source_line, row_end_line),
                |index| {
                    index
                        .best_in_range(source_line, row_end_line)
                        .unwrap_or(' ')
                },
            )
        } else if editor.large_file_mode {
            ' '
        } else if editor.has_unsaved_change_in_range(source_line, row_end_line) {
            'N'
        } else {
            change_index.map_or_else(
                || minimap_change_in_range(editor.git_change_lines(), source_line, row_end_line),
                |index| {
                    index
                        .best_in_range(source_line, row_end_line)
                        .unwrap_or(' ')
                },
            )
        };
        let has_selection = selection_overlaps_range(selection_lines, source_line, row_end_line);
        let diag_severity = diagnostic_index.map_or_else(
            || worst_diagnostic_in_range(diagnostic_lines, source_line, row_end_line),
            |index| index.best_in_range(source_line, row_end_line),
        );

        let bg = if has_selection {
            colors.ui.bg_minimap_selection
        } else if in_viewport {
            slider_bg
        } else {
            track_bg
        };

        if source_line < total_lines {
            spans.clear();
            raster.clear();
            raster.resize(width, cache::MiniCell { glyph: Glyph::Blank, dots: 0 });
            for (source, dots) in minimap_samples(source_row, end_row) {
                if source >= visible_total { continue; }
                cache::MinimapCache::decode(&mut glyphs, &editor.rope,
                    editor.line_for_visible_row(source), width, editor.tab_size);
                for (cell, &glyph) in raster.iter_mut().zip(&glyphs) { cell.add(glyph, dots); }
            }
            let diff_colors = minimap_diff_colors(diff_status, bg, &colors);
            let override_bg = if has_search_match {
                Some(dim_color(colors.ui.fg_minimap_find, 100, bg))
            } else if let Some((_, rbg)) = diff_colors {
                Some(rbg)
            } else {
                None
            };
            let row_bg = override_bg.unwrap_or(bg);
            // Diagnostic markers take priority over diff markers
            let effective_marker = if let Some(sev) = diag_severity {
                Some((diagnostic_marker_color(sev, &colors), row_bg))
            } else {
                diff_colors
            };
            let marker_width = if effective_marker.is_some() {
                if width == 0 { 0 } else { width.clamp(1, 3) }
            } else {
                0
            };
            let content_width = width.saturating_sub(marker_width);
            let mut batch_style = Style::default().bg(row_bg);
            batch_buf.clear();
            let mut rendered_cols = 0usize;
            for cell in raster.iter().take(content_width) {
                let style = if cell.glyph == Glyph::Blank { Style::default().bg(row_bg) } else {
                    let fg = if has_search_match { colors.ui.fg_minimap_find }
                    else if has_bookmark { colors.fg_bookmark }
                    else { match cell.glyph {
                        Glyph::String => dim_color(colors.fg_text, 150, row_bg),
                        Glyph::Number => dim_color(colors.fg_text, 160, row_bg),
                        Glyph::Punctuation => dim_color(colors.fg_text, 140, row_bg),
                        _ if in_viewport => colors.fg_text,
                        _ => colors.fg_dim,
                    }};
                    Style::default().fg(fg).bg(row_bg)
                };
                if style != batch_style && !batch_buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut batch_buf), batch_style));
                }
                batch_style = style;
                batch_buf.push(cell.symbol());
                rendered_cols += 1;
            }
            if !batch_buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut batch_buf), batch_style));
            }
            if rendered_cols < content_width {
                let pad = content_width - rendered_cols;
                spans.push(Span::styled(
                    &spaces_pool[..pad.min(spaces_pool.len())],
                    Style::default().bg(row_bg),
                ));
            }
            if let Some((marker_fg, _)) = effective_marker {
                spans.push(Span::styled(
                    marker_str_detail,
                    Style::default()
                        .fg(marker_fg)
                        .bg(row_bg)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(std::mem::take(&mut spans)));
        } else {
            lines.push(Line::from(Span::styled(
                spaces_pool,
                Style::default().bg(bg),
            )));
        }
    }
    let p = Paragraph::new(lines);
    f.render_widget(p, area);
    draw_minimap_viewport_border(f, area, view_start_row, view_end_row, hovered, &colors);
}

pub(super) fn draw_scrollbar_reference(
    f: &mut Frame,
    p: ScrollbarParams,
    source_to_display: Option<&[usize]>,
) {
    let ScrollbarParams {
        area,
        total_lines,
        visible_lines,
        scroll,
        search_matches,
        diagnostic_lines,
        diagnostic_index,
        change_rows,
        cursor_line,
        colors,
        hovered,
        active,
    } = p;
    let height = area.height as usize;
    let width = area.width as usize;
    if height == 0 || total_lines <= visible_lines {
        let bg = Paragraph::new("").style(Style::default().bg(colors.ui.bg_overview));
        f.render_widget(bg, area);
        return;
    }

    // Search and diagnostic vectors are sorted by line. Query one source range
    // per terminal row instead of rebuilding full-document row maps on every
    // scroll/click frame.
    let cursor_row = cursor_line
        .min(total_lines.saturating_sub(1))
        .saturating_mul(height)
        / total_lines;
    let thumb_size = ((visible_lines as u128 * height as u128 / total_lines as u128) as usize)
        .max(1).min(height);
    let max_scroll = total_lines.saturating_sub(visible_lines);
    let thumb_pos = if max_scroll > 0 {
        (scroll.min(max_scroll) as u128 * (height - thumb_size) as u128 / max_scroll as u128) as usize
    } else {
        0
    };

    let (track, thumb) = colors
        .ui
        .scrollbar_colors(colors.ui.bg_overview, hovered, active);
    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for i in 0..height {
        let source_start = i.saturating_mul(total_lines).div_ceil(height);
        let source_end = (i + 1)
            .saturating_mul(total_lines)
            .div_ceil(height)
            .min(total_lines);
        let in_thumb = i >= thumb_pos && i < thumb_pos + thumb_size;
        let (query_start, query_end) =
            source_to_display.map_or((source_start, source_end), |mapping| {
                (
                    mapping.partition_point(|&row| row < source_start),
                    mapping.partition_point(|&row| row < source_end),
                )
            });
        let includes_fallback = source_start == 0 && source_end > 0;
        let fallback_start = source_to_display
            .filter(|_| includes_fallback)
            .map(<[usize]>::len);
        let has_match = has_search_match_in_range(search_matches, query_start, query_end)
            || fallback_start.is_some_and(|start| {
                search_matches
                    .last()
                    .is_some_and(|&(line, _, _)| line >= start)
            });
        let is_cursor = i == cursor_row;
        let change = change_rows.row(i);
        let diag_sev = diagnostic_index.map_or_else(
            || worst_diagnostic_in_range(diagnostic_lines, query_start, query_end),
            |index| index.best_in_range(query_start, query_end),
        );
        let fallback_severity = fallback_start.and_then(|start| {
            diagnostic_index.map_or_else(
                || {
                    let first = diagnostic_lines.partition_point(|&(line, _)| line < start);
                    diagnostic_lines[first..]
                        .iter()
                        .map(|&(_, severity)| severity)
                        .min()
                },
                |index| index.best_in_range_inner(start, usize::MAX, true),
            )
        });
        let diag_sev = match (diag_sev, fallback_severity) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let marker = if let Some(sev) = diag_sev {
            let c = match sev {
                1 => colors.ui.fg_overview_error,
                2 => colors.ui.fg_overview_warning,
                3 => colors.ui.fg_overview_info,
                _ => colors.fg_dim,
            };
            Some(('█', '▐', c))
        } else if has_match {
            let c = colors.ui.fg_overview_find;
            Some(('█', '▐', c))
        } else if let Some(c) = change.status().and_then(|status| crate::ui_colors::overview_marker_color(status, &colors)) {
            Some((change.symbol(), change.narrow_symbol(), c))
        } else {
            None
        };
        // Share the visual rules while retaining Paragraph allocation and
        // painting as the independent direct-cell performance reference.
        let cells = scrollbar_row_cells(width, in_thumb, is_cursor, marker, &colors, track, thumb);
        let spans: Vec<_> = cells.into_iter().zip([width / 2, width - width / 2])
            .filter(|&(_, length)| length > 0)
            .map(|((symbol, style), length)| Span::styled(std::iter::repeat_n(symbol, length).collect::<String>(), style))
            .collect();
        lines.push(Line::from(spans));
    }
    let p = Paragraph::new(lines);
    f.render_widget(p, area);
}
