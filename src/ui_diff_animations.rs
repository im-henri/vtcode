//! Project editor animations onto the final, revision-specific diff rows.
//! All per-frame geometry comes from indexed visible text windows.

use ratatui::{Frame, layout::Rect};
use std::{rc::Rc, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    animation_config::AnimationEffect,
    animations::Snapshot,
    app::{AlignedDiffRow, App, DiffRenderCache},
    ui_editor_animations::{CursorUnderlay, DiffScopeOptions, Geometry, draw_diff},
};

type SelectionKey = (
    Option<(u8, usize, usize, usize, usize)>,
    Option<(usize, usize, usize, usize)>,
);

struct GeometryKey {
    comparison: Arc<DiffRenderCache>,
    source: ropey::Rope,
    id: u64,
    original: bool,
    aligned: bool,
    cursor: (usize, usize),
    selection: SelectionKey,
    area: Rect,
    first: usize,
    pan: usize,
    primary: Option<CursorUnderlay>,
}

impl GeometryKey {
    fn current(
        app: &App,
        id: u64,
        original: bool,
        aligned: bool,
        primary: Option<CursorUnderlay>,
    ) -> Option<Self> {
        let editor = app.editor()?;
        let (_, cursor) = app.diff_scope_cursor()?;
        let (area, first, pan) = geometry_layout(app, original, aligned)?;
        Some(Self {
            comparison: Arc::clone(&app.diff_render_cache),
            source: editor.rope.clone(),
            id,
            original,
            aligned,
            cursor,
            selection: app.diff_animation_selection_key(),
            area,
            first,
            pan,
            primary,
        })
    }

    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.comparison, &other.comparison)
            && self.source.is_instance(&other.source)
            && self.id == other.id
            && self.original == other.original
            && self.aligned == other.aligned
            && self.cursor == other.cursor
            && self.selection == other.selection
            && self.area == other.area
            && self.first == other.first
            && self.pan == other.pan
            && self.primary == other.primary
    }
}

/// Diff rows are immutable between semantic frames. Keep one shared geometry
/// for the focused comparison side, including its source/selection masks and
/// precomputed row maps, instead of decoding every visible grapheme per tick.
#[derive(Default)]
pub(crate) struct DiffGeometryCache {
    entry: Option<(GeometryKey, Rc<Geometry>, Rect)>,
    #[cfg(test)]
    pub(crate) builds: usize,
    #[cfg(test)]
    pub(crate) reuses: usize,
}

impl DiffGeometryCache {
    pub(crate) fn forget(&mut self, id: u64) {
        if self.entry.as_ref().is_some_and(|(key, _, _)| key.id == id) {
            self.entry = None;
        }
    }
}

fn cached_geometry(
    frame: &mut Frame,
    app: &mut App,
    id: u64,
    original: bool,
    aligned: bool,
    primary: Option<CursorUnderlay>,
) -> Option<(Rc<Geometry>, Rect)> {
    let key = GeometryKey::current(app, id, original, aligned, primary)?;
    if app.rendering_animation_frame
        && let Some((cached, geometry, gutter)) = &app.animations.diff_geometry.entry
        && cached.matches(&key)
    {
        let geometry = Rc::clone(geometry);
        let gutter = *gutter;
        #[cfg(test)]
        {
            app.animations.diff_geometry.reuses += 1;
        }
        return Some((geometry, gutter));
    }
    let (mut geometry, gutter) = geometry(app, original, aligned)?;
    if let Some(primary) = primary {
        let area = primary.0;
        let ch = frame.buffer_mut()[(area.x, area.y)]
            .symbol()
            .chars()
            .next()
            .unwrap_or('\0');
        geometry.record_diff_cursor(key.cursor, primary, ch);
    }
    let geometry = Rc::new(geometry);
    let cache = &mut app.animations.diff_geometry;
    cache.entry = Some((key, Rc::clone(&geometry), gutter));
    #[cfg(test)]
    {
        cache.builds += 1;
    }
    Some((geometry, gutter))
}

fn source_line(row: &AlignedDiffRow, original: bool) -> Option<usize> {
    if original {
        row.left_line
    } else {
        row.right_line
    }
}

fn source_row(cache: &DiffRenderCache, original: bool, line: usize) -> Option<&AlignedDiffRow> {
    let rows = cache.selectable_rows(true, original);
    let index =
        rows.partition_point(|&index| source_line(&cache.rows[index], original).unwrap() < line);
    let row = cache.rows.get(*rows.get(index)?)?;
    (source_line(row, original) == Some(line)).then_some(row)
}

fn omitted_range(cache: &DiffRenderCache, original: bool, row: usize) -> (usize, usize) {
    let rows = cache.selectable_rows(true, original);
    let next = rows.partition_point(|&index| index <= row);
    let end = rows
        .get(next)
        .and_then(|&index| source_line(&cache.rows[index], original))
        .unwrap_or(if original {
            cache.max_left_line
        } else {
            cache.target_content_lines()
        });
    (end.saturating_sub(cache.rows[row].hidden_lines), end)
}

fn geometry_layout(app: &App, original: bool, aligned: bool) -> Option<(Rect, usize, usize)> {
    let editor = app.editor()?;
    let mut area = if aligned && original {
        app.diff_left_code_area?
    } else {
        app.code_area?
    };
    if aligned && !original && !app.compare_diff_lines.is_empty() {
        let inset = area.width.min(2);
        area.x += inset;
        area.width -= inset;
    }
    if area.is_empty() {
        return None;
    }
    let first = if aligned {
        app.aligned_diff_scroll_row(editor.scroll_offset)
    } else {
        app.diff_inline_scroll.row
    };
    let pan = if !aligned {
        app.diff_inline_scroll.column
    } else if original {
        app.diff_left_horizontal_scroll
    } else {
        editor.horizontal_scroll
    };
    Some((area, first, pan))
}

fn geometry(app: &App, original: bool, aligned: bool) -> Option<(Geometry, Rect)> {
    let (_, cursor) = app.diff_scope_cursor()?;
    let (area, first, pan) = geometry_layout(app, original, aligned)?;
    let mut geometry = Geometry::new(area, true);
    geometry.set_diff_projection(pan, !aligned);
    let cache = &app.diff_render_cache;
    for screen_row in 0..area.height as usize {
        let display_row = first.saturating_add(screen_row);
        let (index, row_original) = if aligned {
            (display_row, original)
        } else {
            let Some(entry) = cache.inline_rows.get(display_row) else {
                continue;
            };
            (
                entry.aligned_row,
                if entry.kind == ' ' {
                    original
                } else {
                    entry.kind == '-'
                },
            )
        };
        let Some(row) = cache.rows.get(index) else {
            continue;
        };
        if row.hidden_lines > 0 {
            let (start, end) = omitted_range(cache, original, index);
            geometry.omit_lines(start, end, screen_row);
            geometry.obstacle(screen_row, 0, area.width as usize);
            continue;
        }
        let Some(line) = source_line(row, row_original) else {
            // Alignment hatching belongs to the other revision, not source
            // indentation through which a rail may route.
            geometry.obstacle(screen_row, 0, area.width as usize);
            continue;
        };
        if original == row_original {
            geometry.map_source_row(line, screen_row);
        }
        let char_len = if row_original {
            row.left_char_len()
        } else {
            row.right_char_len()
        };
        let (text, mut column, mut x) = if aligned {
            let text = if row_original {
                row.left_text_window(pan, area.width as usize)
            } else {
                row.right_text_window(pan, area.width as usize)
            };
            (text, pan, 0)
        } else if row_original {
            row.left_inline_text_window(pan, area.width as usize)
        } else {
            row.right_inline_text_window(pan, area.width as usize)
        };
        let selected = app.diff_scope_selection_columns(row_original, display_row, line);
        let visible_start = column;
        let cursor_here = original == row_original && cursor.0 == line && cursor.1 >= visible_start;
        let mut cursor_protected = false;
        for grapheme in text.graphemes(true) {
            let end = column + grapheme.chars().count();
            if grapheme.contains(char::is_control) {
                column = end;
                continue;
            }
            let width = grapheme.width();
            if original == row_original {
                geometry.push_diff(
                    line,
                    column,
                    grapheme.chars().next().unwrap(),
                    screen_row,
                    x,
                    width,
                );
            } else if !grapheme.chars().all(char::is_whitespace) {
                geometry.obstacle(screen_row, x, width);
            }
            if selected.is_some_and(|(from, to)| column < to && end > from) {
                geometry.protect(screen_row, x, width);
            }
            // Diff carets on skipped controls land on the next rendered
            // grapheme; they need protection even without an exact anchor.
            if cursor_here && !cursor_protected && cursor.1 < end {
                geometry.protect(screen_row, x, width.max(1));
                cursor_protected = true;
            }
            column = end;
            x += width;
        }
        if original == row_original && column == char_len {
            geometry.push_diff(line, char_len, '\0', screen_row, x, 1);
            if cursor_here && !cursor_protected && cursor.1 <= column {
                geometry.protect(screen_row, x, 1);
            }
        }
        if column == char_len
            && selected.is_some_and(|(from, to)| from <= char_len && to > char_len)
        {
            geometry.protect(screen_row, x, (area.width as usize).saturating_sub(x));
        }
    }
    // The final blank column of each diff prefix is a dedicated fallback rail;
    // numeric gutters and +/- markers remain outside its bounds.
    let gutter = Rect::new(
        area.x.saturating_sub(1),
        area.y,
        u16::from(area.x > 0),
        area.height,
    );
    Some((geometry, gutter))
}

pub(crate) fn cursor_effects_allowed(app: &App) -> bool {
    !app.is_commit_details_tab() || app.commit_details.focus == crate::commit_details::Focus::Diff
}

pub(crate) fn draw(
    frame: &mut Frame,
    app: &mut App,
    aligned: bool,
    primary: Option<CursorUnderlay>,
) {
    if crate::ui_editor_animations::suspend_for_multiple_cursors(app)
        || !app.animation_settings.editor_effects_enabled()
        || !app.terminal_has_focus
        || app.rendering_inactive_editor
        || app.status_details.open
    {
        return;
    }
    let allow_cursor_effects = cursor_effects_allowed(app);
    let scope_enabled = app.editor().is_some_and(|editor| {
        app.animation_settings.scope_enabled_for(editor)
    });
    if !allow_cursor_effects && !scope_enabled {
        if let Some(id) = crate::diff_scope_source::animation_view_id(app) {
            app.animations.forget_view(id);
        }
        return;
    }
    let Some((original, position)) = app.diff_scope_cursor() else {
        return;
    };
    // A replacement diff can lag behind edits. Never project current source
    // effects onto stale comparison rows.
    if !original
        && app
            .editor()
            .is_none_or(|editor| app.compare_diff_target_version != Some(editor.content_version))
    {
        return;
    }
    let Some(id) = crate::diff_scope_source::animation_view_id(app) else {
        return;
    };
    let Some((geometry, gutter)) = cached_geometry(frame, app, id, original, aligned, primary)
    else {
        return;
    };
    let target = &app.tabs[app.active_tab].editor;
    let first = if aligned {
        app.aligned_diff_scroll_row(target.scroll_offset)
    } else {
        app.diff_inline_scroll.row
    };
    let pan = if !aligned {
        app.diff_inline_scroll.column
    } else if original {
        app.diff_left_horizontal_scroll
    } else {
        target.horizontal_scroll
    };
    // Read-only original motion needs only a stable identity and visible
    // anchors, so it starts immediately without copying or parsing the file.
    let snapshot = if original {
        Snapshot::read_only_motion(id, position, (first, pan, 0), geometry.area)
    } else {
        Snapshot {
            id,
            rope: target.rope.clone(),
            cursor: position,
            offset: target
                .rope
                .line_to_char(target.cursor_line)
                .saturating_add(target.cursor_col)
                .min(target.rope.len_chars()),
            content_version: target.content_version,
            undo_revision: target.animation_undo_revision,
            scroll: (first, pan, 0),
            selection: target.selection_range(),
            area: geometry.area,
        }
    };
    let mut state = std::mem::take(&mut app.diff_scope_source);
    let mut paint = std::mem::take(if original {
        &mut state.original_paint
    } else {
        &mut state.target_paint
    });
    if original && scope_enabled {
        state.set_cursor(position);
    }
    let source = if !scope_enabled {
        None
    } else if original {
        app.compare_base_content.as_ref().and_then(|base| {
            state.editor_and_index_for_rules(
                base,
                target.syntax_file_path(),
                target.syntax_language_token(),
                &target.scope_bubble_rules,
            )
        })
    } else {
        Some((target, app.cached_structural_scope_index.as_deref()))
    };
    let cache = &app.diff_render_cache;
    let full = app
        .is_commit_details_tab()
        .then_some(app.commit_details.diff_full_cache.as_deref())
        .flatten();
    let endpoint_row = |line| {
        source_row(cache, original, line)
            .or_else(|| full.and_then(|cache| source_row(cache, original, line)))
    };
    let cell_column = |(line, column)| {
        endpoint_row(line).map(|row| {
            if original {
                row.left_cell_column(column)
            } else {
                row.right_cell_column(column)
            }
        })
    };
    let line_summary = |line| {
        endpoint_row(line).map(|row| {
            if original {
                row.left_scope_summary()
            } else {
                row.right_scope_summary()
            }
        })
    };
    let mut scoped_settings;
    let settings = if !scope_enabled && app.animation_settings.enabled(AnimationEffect::ScopeConnector) {
        scoped_settings = app.animation_settings.clone();
        scoped_settings.set(AnimationEffect::ScopeConnector, false);
        &scoped_settings
    } else {
        &app.animation_settings
    };
    draw_diff(
        frame,
        &geometry,
        gutter,
        &mut app.animations,
        settings,
        app.colors,
        snapshot,
        allow_cursor_effects,
        &mut paint,
        source.map(|(editor, index)| DiffScopeOptions {
            source: editor,
            index,
            bracket_index: if original {
                None
            } else {
                app.cached_bracket_depth_index.as_deref()
            },
            colorize: app.bracket_pair_colorization,
            selection: if original {
                None
            } else {
                target.selection_range()
            },
            cell_column: Some(&cell_column),
            line_summary: Some(&line_summary),
        }),
    );
    if original {
        state.original_paint = paint;
    } else {
        state.target_paint = paint;
    }
    app.diff_scope_source = state;
}
