use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::ui_dialogs::{file_icon_colored, tab_icon};
use crate::ui_widgets::{DropdownItem, DropdownMenu, themed_dropdown_colors};

#[path = "tab_path_labels.rs"]
mod tab_path_labels;
use tab_path_labels::TabNameDuplicates;
pub(crate) use tab_path_labels::NameCache;

#[path = "tab_strip_scroll.rs"]
mod strip_scroll;
pub(crate) use strip_scroll::{StripScroll, position as strip_scroll_position, scroll_by as scroll_strip_by};

/// Width of each tab-bar scroll indicator ("◂ " / " ▸").
const TAB_SCROLL_INDICATOR_WIDTH: usize = 2;

fn active_editor_is_markdown(app: &App) -> bool {
    app.editor().is_some_and(|editor| editor.is_markdown())
}

#[cfg(test)]
pub(crate) fn tab_display_name<'a>(
    app: &'a App,
    tab_idx: usize,
    tab: &'a crate::app::Tab,
) -> std::borrow::Cow<'a, str> {
    tab_display_name_with_duplicates(app, tab_idx, tab, None)
}

/// Geometry for one input event. Build the duplicate-name index once, then use
/// the same clipped rectangles for hover, close, scrolling and new-tab hits.
/// Keeping this event-local also makes tab renames/reorders immediately visible.
pub(crate) struct TabBarHits {
    pub tabs: Vec<(usize, Rect, Rect)>,
    pub left: Rect,
    pub right: Rect,
    pub new_tab: Rect,
    /// Drop space after the last tab, including the new-tab button. Empty when
    /// more tabs are clipped behind the right scroll indicator.
    pub after_tabs: Rect,
    pub groups: Vec<(u64, Rect)>,
    tab_clips: Vec<usize>,
    group_clips: Vec<(usize, usize)>,
    new_tab_clip: usize,
}

impl TabBarHits {
    pub(crate) fn new(app: &App, area: Rect) -> Self {
        Self::for_group(app, area, app.active_group)
    }

    pub(crate) fn for_group(app: &App, area: Rect, group: usize) -> Self {
        Self::for_group_with_names(app, area, group, None)
    }

    fn for_group_with_names(app: &App, area: Rect, group: usize, names: Option<&TabNameDuplicates>) -> Self {
        if let Some(scroll) = strip_scroll_position(app, group) {
            return strip_scroll::hits(app, area, group, scroll, names);
        }
        let mut hits = Self {
            tabs: Vec::new(),
            groups: Vec::new(),
            left: Rect::default(),
            right: Rect::default(),
            new_tab: Rect::default(),
            after_tabs: Rect::default(),
            tab_clips: Vec::new(),
            group_clips: Vec::new(),
            new_tab_clip: 0,
        };
        if area.is_empty() {
            return hits;
        }
        let (tabs, active, scroll) = if group == app.active_group
            && app.groups.get(group).is_none_or(|g| g.tabs.is_empty())
        {
            (&app.tabs, app.active_tab, app.tab_scroll_offset)
        } else if let Some(g) = app.groups.get(group) {
            (&g.tabs, g.active_tab, g.tab_scroll_offset)
        } else {
            return hits;
        };
        let (_, _, prefix) = crate::ui_desks::controls(app, area, group);
        let mut used = usize::from(prefix);
        let cells = |start: usize, width: usize| {
            Rect::new(
                area.x.saturating_add(crate::helpers::len_as_u16(start)),
                area.y,
                crate::helpers::len_as_u16(width),
                1,
            )
            .intersection(area)
        };
        if scroll > 0 {
            hits.left = cells(used, TAB_SCROLL_INDICATOR_WIDTH);
            used += TAB_SCROLL_INDICATOR_WIDTH;
        }
        let mut last_group = None;
        let mut counts = rustc_hash::FxHashMap::<u64, usize>::default();
        if !app.desks.membership.is_empty() {
            for tab in tabs {
                if let Some(&id) = app.desks.membership.get(&tab.editor.navigation_id) {
                    *counts.entry(id).or_default() += 1;
                }
            }
        }
        let owned_names;
        let duplicates = if let Some(names) = names {
            Some(names)
        } else {
            owned_names = TabNameDuplicates::for_tabs(app, tabs, active, scroll, area.width);
            owned_names.as_ref()
        };
        for (index, tab) in tabs.iter().enumerate().skip(scroll) {
            if used >= usize::from(area.width) {
                break;
            }
            let named = app.tab_group(tab);
            if let Some(named) = named
                && last_group != Some(named.id)
            {
                let count = counts.get(&named.id).copied().unwrap_or(0);
                let remaining = usize::from(area.width).saturating_sub(used);
                let reserve = if index == active || tab.pinned {
                    remaining.min(10)
                } else {
                    0
                };
                let width = (named.name.width() + count.to_string().len() + 6)
                    .min(25)
                    .min(remaining.saturating_sub(reserve));
                hits.groups.push((named.id, cells(used, width)));
                used += width;
            }
            last_group = named.map(|g| g.id);
            if named.is_some_and(|g| g.collapsed) && index != active && !tab.pinned {
                continue;
            }
            let name = tab_display_name_with_duplicates(app, index, tab, duplicates);
            let width = tab_label_width(app, tab, &name);
            if used + width > usize::from(area.width) && !hits.tabs.is_empty() {
                hits.right = cells(used, TAB_SCROLL_INDICATOR_WIDTH);
                used += TAB_SCROLL_INDICATOR_WIDTH;
                break;
            }
            let close = if tab.pinned {
                Rect::default()
            } else {
                cells(used + width - 3, 3)
            };
            hits.tabs.push((index, cells(used, width), close));
            used += width;
        }
        if used + 3 <= usize::from(area.width) {
            hits.new_tab = cells(used, 3);
        }
        if hits.right.is_empty() {
            hits.after_tabs = cells(used, usize::from(area.width).saturating_sub(used));
        }
        hits
    }

    pub(crate) fn tab_at(&self, column: u16) -> Option<(usize, Rect, Rect)> {
        self.tabs
            .iter()
            .copied()
            .find(|(_, rect, _)| Self::contains(*rect, column))
    }

    pub(crate) fn contains(rect: Rect, column: u16) -> bool {
        !rect.is_empty() && column >= rect.x && column < rect.right()
    }
}

fn tab_label_width(app: &App, tab: &crate::app::Tab, display_name: &str) -> usize {
    let icon = chat_tab_marker(app, tab.kind).map_or_else(
        || tab_icon(tab.kind, tab.editor.file_name()), |(icon, _)| icon,
    );
    tab_label_width_with_icon(tab, display_name, icon)
}

fn tab_label_width_with_icon(tab: &crate::app::Tab, display_name: &str, icon: &str) -> usize {
    1 + if tab.pinned { "📌".width() } else { 0 }
        + if tab.has_unsaved_changes() { 2 } else { 0 }
        + icon.width()
        + usize::from(tab.kind == crate::app::TabKind::File && tab.editor.disk_unavailable)
        + display_name.width() + 3
}

#[cfg(test)]
pub(crate) fn tab_render_width(app: &App, index: usize, tab: &crate::app::Tab) -> u16 {
    crate::helpers::len_as_u16(tab_label_width(app, tab, &tab_display_name(app, index, tab)))
}

fn tab_display_name_with_duplicates<'a>(
    app: &'a App,
    tab_idx: usize,
    tab: &'a crate::app::Tab,
    duplicates: Option<&TabNameDuplicates>,
) -> std::borrow::Cow<'a, str> {
    let name = tab.editor.file_name();
    if tab.kind == crate::app::TabKind::ExtensionInfo {
        let id = crate::ui_extension_info::tab_extension_id(tab);
        let name = app.extension_views().into_iter().find(|ext| Some(ext.id.as_str()) == id.as_deref())
            .map_or_else(|| "Extension".to_string(), |ext| format!("Extension: {}", ext.name));
        return std::borrow::Cow::Owned(name);
    }
    if tab.kind == crate::app::TabKind::CommitDetails {
        return std::borrow::Cow::Borrowed("Commit details");
    }
    if let Some(revision) = tab.editor.blame.revision.as_ref().filter(|_| tab.kind == crate::app::TabKind::GitRevision) {
        return role_name_with_context(app, tab, revision.tab_title(), duplicates);
    }
    if let crate::app::TabKind::Chat(key) = tab.kind {
        return std::borrow::Cow::Owned(app.chat.editor_tab_title(key));
    }
    if matches!(tab.kind, crate::app::TabKind::Diff(_)) {
        let label = match app.diff_tabs.staged_for_tab(tab) {
            Some(true) => "Staged Diff",
            Some(false) => "Unstaged Diff",
            None => "Diff",
        };
        if app.repositories.entries.len() > 1
            && let Some(root) = app.diff_tabs.repository_for_tab(tab)
            && let Some(repository) = root.file_name()
        {
            return std::borrow::Cow::Owned(format!("{name} ({label} · {})", repository.to_string_lossy()));
        }
        return std::borrow::Cow::Owned(format!("{name} ({label})"));
    }
    if tab.kind == crate::app::TabKind::MarkdownPreview {
        return role_name_with_context(app, tab, format!("Preview: {name}"), duplicates);
    }
    if tab.kind == crate::app::TabKind::Terminal {
        let terminal_name = tab
            .terminal_idx
            .and_then(|idx| app.terminals.get(idx))
            .map_or("Terminal", |terminal| terminal.name.as_str());
        return std::borrow::Cow::Borrowed(terminal_name);
    }
    if tab_idx == app.active_tab
        && app.tabs.get(tab_idx).is_some_and(|active| std::ptr::eq(active, tab))
        && app.diff_mode != crate::app::DiffMode::Off
        && app.active_editor_matches_diff_target()
    {
        if let Some(label) = &app.diff_tab_label {
            return std::borrow::Cow::Borrowed(label.as_str());
        }
    }
    if tab.kind == crate::app::TabKind::File && tab.editor.file_path.is_none() {
        return tab.editor.display_name();
    }
    let pending = tab.kind == crate::app::TabKind::File
        && tab
            .editor
            .file_path
            .as_ref()
            .is_some_and(|p| app.pending_agent_change(p));
    let review_mark = if pending { " [AI]" } else { "" };
    let context = duplicates.and_then(|index| index.context(tab))
        .unwrap_or_else(|| tab_path_labels::context(app, tab));
    if let Some(context) = context {
        return std::borrow::Cow::Owned(format!("{name} ({context}){review_mark}"));
    }
    if pending { std::borrow::Cow::Owned(format!("{name}{review_mark}")) } else { std::borrow::Cow::Borrowed(name) }
}

fn role_name_with_context<'a>(app: &App, tab: &crate::app::Tab, name: String, duplicates: Option<&TabNameDuplicates>) -> std::borrow::Cow<'a, str> {
    let context = duplicates.and_then(|index| index.context(tab))
        .unwrap_or_else(|| tab_path_labels::context(app, tab));
    std::borrow::Cow::Owned(if let Some(context) = context { format!("{name} ({context})") } else { name })
}

pub(crate) fn chat_tab_marker(app: &App, kind: crate::app::TabKind) -> Option<(&'static str, ratatui::style::Color)> {
    let crate::app::TabKind::Chat(key) = kind else { return None; };
    let view = app.chat.layout.editors.get(&key)?;
    let state = app.chat.agent_state(view.agent)?;
    let session = state.sessions.get(&view.session)?;
    Some(if state.permissions.iter().any(|p| p.request.session_id == view.session) {
        ("! ", app.colors.fg_warning)
    } else if session.busy {
        (app.chat.activity.tab_symbol(), app.colors.fg_accent_blue)
    } else if state.unread_tabs.contains(&view.session) {
        let error = matches!(session.entries.last(), Some(vtcode::acp::ChatEntry::Error(_)));
        ("● ", if error { app.colors.fg_error } else { app.colors.fg_success })
    } else { ("◇ ", app.colors.fg_dim) })
}

pub(crate) fn draw_tab_bar(f: &mut Frame, app: &App, area: Rect) {
    draw_tab_bar_at(f, app, area, std::time::Instant::now());
}

pub(crate) fn draw_tab_bar_for_group(
    f: &mut Frame,
    app: &App,
    area: Rect,
    group: usize,
    now: std::time::Instant,
) {
    if group == app.active_group {
        if app.rendering_animation_frame {
            let cache = app.tab_animation_cache.borrow();
            if let Some(cache) = cache.as_ref().filter(|cache| {
                cache.cells.area == area
                    && cache.scroll == app.tab_scroll_offset
                    && cache.strip_scroll == strip_scroll_position(app, group)
                    && cache.editor == app.editor().map(|editor| editor.navigation_id)
            }) {
                let buffer = f.buffer_mut();
                for y in area.y..area.bottom() {
                    let source = cache.cells.index_of(area.x, y);
                    let target = buffer.index_of(area.x, y);
                    buffer.content[target..target + usize::from(area.width)].clone_from_slice(
                        &cache.cells.content[source..source + usize::from(area.width)],
                    );
                }
                draw_tab_indicator(f, app, area, cache.active, cache.track,
                    cache.strip_scroll.map_or(cache.scroll, |scroll| scroll.columns), now);
                return;
            }
        }
    }
    let (tabs, active, scroll) =
        if group == app.active_group && app.groups.get(group).is_none_or(|g| g.tabs.is_empty()) {
            (&app.tabs, app.active_tab, app.tab_scroll_offset)
        } else {
            (&app.groups[group].tabs, app.groups[group].active_tab, app.groups[group].tab_scroll_offset)
        };
    let strip_scroll = strip_scroll_position(app, group);
    let duplicates = if strip_scroll.is_some() { TabNameDuplicates::for_scroll(app, tabs, active) }
        else { TabNameDuplicates::for_tabs(app, tabs, active, scroll, area.width) };
    let hits = TabBarHits::for_group_with_names(app, area, group, duplicates.as_ref());
    let bg = app.colors.bg_tabbar;
    f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);
    crate::ui_desks::draw_controls(f, app, area, group);
    let mut group_stats = rustc_hash::FxHashMap::<u64, (usize, bool)>::default();
    if !hits.groups.is_empty() {
        for tab in tabs {
            if let Some(&id) = app.desks.membership.get(&tab.editor.navigation_id) {
                let stats = group_stats.entry(id).or_default();
                stats.0 += 1;
                stats.1 |= tab.has_unsaved_changes();
            }
        }
    }
    for (position, (id, rect)) in hits.groups.iter().enumerate() {
        let (clip, width) = hits.group_clips.get(position).copied().unwrap_or((0, usize::from(rect.width)));
        let named = &app.desks.groups[id];
        let (count, dirty) = group_stats.get(id).copied().unwrap_or_default();
        let suffix = format!(" {}{} ", count, if dirty { "●" } else { "" });
        let name = crate::ui_desks::clipped_name(
            &named.name,
            width.saturating_sub(suffix.width() + 2),
        );
        let color = crate::ui_desks::group_color(app, named.color);
        let label = format!(
            "{} {}{}",
            if named.collapsed { "▸" } else { "▾" },
            name,
            suffix
        );
        f.render_widget(
            Paragraph::new(label).scroll((0, crate::helpers::len_as_u16(clip))).style(
                Style::default()
                    .fg(color)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                    .underline_color(color),
            ),
            *rect,
        );
    }
    let mut active_rect = None;
    let mut file_icon_areas = Vec::new();
    for (position, &(i, rect, close)) in hits.tabs.iter().enumerate() {
        let clip = hits.tab_clips.get(position).copied().unwrap_or(0);
        let tab = &tabs[i];
        let is_active = i == active;
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        let selected = app.desks.selected_tabs.contains(&tab.editor.navigation_id);
        let focused = group == app.active_group;
        let tab_bg = if focused {
            if selected || hovered {
                app.colors.ui.bg_tab_hover
            } else if is_active {
                app.colors.bg_tab_active
            } else {
                app.colors.bg_tab_inactive
            }
        } else if hovered {
            app.colors.ui.bg_unfocused_tab_hover
        } else if is_active {
            app.colors.ui.bg_unfocused_tab
        } else {
            app.colors.ui.bg_unfocused_tab_inactive
        };
        let fg = app.colors.ui.readable_foreground(
            if focused {
                if is_active {
                    app.colors.ui.fg_tab_active
                } else {
                    app.colors.ui.fg_tab_inactive
                }
            } else if hovered {
                app.colors.ui.fg_unfocused_tab_hover
            } else if is_active {
                app.colors.ui.fg_unfocused_tab
            } else {
                app.colors.ui.fg_unfocused_tab_inactive
            },
            tab_bg,
            4.5,
        );
        let mut style = Style::default().fg(fg).bg(tab_bg);
        if tab.preview {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if let Some(named) = app.tab_group(tab) {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(crate::ui_desks::group_color(app, named.color));
        } else if is_active
            && let Some(border) = if focused {
                app.colors.ui.border_tab_active
            } else {
                app.colors.ui.border_tab_unfocused
            }
        {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(border);
        }
        let icon = chat_tab_marker(app, tab.kind).map_or_else(
            || tab_icon(tab.kind, tab.editor.file_name()),
            |(icon, _)| icon,
        );
        let mut spans = vec![Span::styled(if tab.pinned { " 📌" } else { " " }, style)];
        if tab.has_unsaved_changes() {
            spans.push(Span::styled(
                "● ",
                style.fg(app.colors.fg_modified_indicator),
            ));
        }
        let icon_style = if matches!(
            tab.kind,
            crate::app::TabKind::File | crate::app::TabKind::GitRevision
        ) {
            style.fg(file_icon_colored(tab.editor.file_name()).1)
        } else {
            style
        };
        if matches!(tab.kind, crate::app::TabKind::File | crate::app::TabKind::GitRevision) {
            let start = if tab.pinned { " 📌".width() } else { 1 } + if tab.has_unsaved_changes() { 2 } else { 0 };
            let first = start.max(clip);
            let end = (start + icon.width()).min(clip + usize::from(rect.width));
            if end > first {
                file_icon_areas.push(Rect::new(rect.x + (first - clip) as u16, rect.y, (end - first) as u16, 1));
            }
        }
        spans.push(Span::styled(icon, icon_style));
        if tab.editor.disk_unavailable && tab.kind == crate::app::TabKind::File {
            spans.push(Span::styled("~", style));
        }
        let display_name = tab_display_name_with_duplicates(app, i, tab, duplicates.as_ref());
        let width = tab_label_width(app, tab, &display_name);
        spans.push(Span::styled(display_name, style));
        spans.push(Span::styled("   ", style));
        f.render_widget(Paragraph::new(Line::from(spans)).scroll((0, crate::helpers::len_as_u16(clip))).style(style), rect);
        if !close.is_empty() && (is_active || hovered) {
            f.render_widget(
                Paragraph::new(" × ").scroll((0, crate::helpers::len_as_u16(clip.saturating_sub(width - 3)))).style(app.colors.ui.toolbar_style(
                    fg,
                    tab_bg,
                    close.contains((app.mouse_col, app.mouse_row).into()),
                    false,
                    false,
                )),
                close,
            );
        }
        if is_active {
            active_rect = Some(rect);
        }
    }
    for (rect, label) in [(hits.left, "◂ "), (hits.right, " ▸"), (hits.new_tab, " + ")] {
        f.render_widget(
            Paragraph::new(label).scroll((0, if rect == hits.new_tab { hits.new_tab_clip as u16 } else { 0 })).style(app.colors.ui.toolbar_style(
                app.colors.ui.fg_tab_active,
                bg,
                rect.contains((app.mouse_col, app.mouse_row).into()),
                false,
                false,
            )),
            rect,
        );
    }
    if strip_scroll.is_some() && area.width > 0 {
        let gradient = &app.colors.gradients.bg_tabbar;
        if gradient.start != gradient.end {
            let computed = crate::app_theme::GradientPrecomputed::new(area.width, 1, gradient.angle_deg);
            for x in area.x..area.right() {
                if let Some(cell) = f.buffer_mut().cell_mut((x, area.y))
                    && (cell.bg == bg || cell.bg == app.colors.bg_tab_inactive)
                {
                    cell.set_bg(crate::app_theme::lerp_color(gradient.start, gradient.end, computed.t_at(x - area.x, 0)));
                }
            }
            crate::ui_colors::ensure_control_row_contrast_except(f.buffer_mut(), area, &app.colors.ui, &file_icon_areas);
        }
    }
    if group == app.active_group {
        let (_, _, prefix) = crate::ui_desks::controls(app, area, group);
        let end = hits.tabs.last().map_or(usize::from(prefix), |(_, r, _)| {
            usize::from(r.right().saturating_sub(area.x))
        });
        if app.animation_settings.any_enabled() && !area.is_empty() {
            let mut slot = app.tab_animation_cache.borrow_mut();
            let cache = slot.get_or_insert_with(|| TabAnimationCache {
                cells: ratatui::buffer::Buffer::empty(area),
                active: active_rect,
                track: (usize::from(prefix), end),
                scroll: app.tab_scroll_offset,
                strip_scroll,
                editor: app.editor().map(|editor| editor.navigation_id),
            });
            cache.cells.resize(area);
            let buffer = f.buffer_mut();
            for y in area.y..area.bottom() {
                let source = buffer.index_of(area.x, y);
                let target = cache.cells.index_of(area.x, y);
                cache.cells.content[target..target + usize::from(area.width)]
                    .clone_from_slice(&buffer.content[source..source + usize::from(area.width)]);
            }
            cache.active = active_rect;
            cache.track = (usize::from(prefix), end);
            cache.scroll = app.tab_scroll_offset;
            cache.strip_scroll = strip_scroll;
            cache.editor = app.editor().map(|editor| editor.navigation_id);
        } else {
            *app.tab_animation_cache.borrow_mut() = None;
        }
        draw_tab_indicator(
            f,
            app,
            area,
            active_rect,
            (usize::from(prefix), end),
            strip_scroll.map_or(app.tab_scroll_offset, |scroll| scroll.columns),
            now,
        );
    }
}

pub(crate) struct TabAnimationCache {
    cells: ratatui::buffer::Buffer,
    active: Option<Rect>,
    track: (usize, usize),
    scroll: usize,
    strip_scroll: Option<StripScroll>,
    editor: Option<u64>,
}

fn draw_tab_bar_at(f: &mut Frame, app: &App, area: Rect, now: std::time::Instant) {
    if app.tabs.iter().any(|tab| app.tab_group(tab).is_some())
        || !app.desks.selected_tabs.is_empty()
        || strip_scroll_position(app, app.active_group).is_some()
    {
        draw_tab_bar_for_group(f, app, area, app.active_group, now);
        return;
    }
    crate::ui_desks::draw_controls(f, app, area, app.active_group);
    let (_, _, prefix) = crate::ui_desks::controls(app, area, app.active_group);
    let area = Rect::new(
        area.x.saturating_add(prefix),
        area.y,
        area.width.saturating_sub(prefix),
        area.height,
    );
    draw_plain_tab_bar_at(f, app, area, now);
}

fn draw_plain_tab_bar_at(f: &mut Frame, app: &App, area: Rect, now: std::time::Instant) {
    if app.rendering_animation_frame {
        let cache = app.tab_animation_cache.borrow();
        if let Some(cache) = cache.as_ref().filter(|cache| {
            cache.cells.area == area
                && cache.scroll == app.tab_scroll_offset
                && cache.strip_scroll.is_none()
                && cache.editor == app.editor().map(|editor| editor.navigation_id)
        }) {
            let buffer = f.buffer_mut();
            for y in area.y..area.bottom() {
                let source = cache.cells.index_of(area.x, y);
                let target = buffer.index_of(area.x, y);
                buffer.content[target..target + usize::from(area.width)].clone_from_slice(
                    &cache.cells.content[source..source + usize::from(area.width)],
                );
            }
            draw_tab_indicator(f, app, area, cache.active, cache.track, cache.scroll, now);
            return;
        }
    }
    let duplicates = TabNameDuplicates::for_viewport(app, area.width);
    draw_tab_bar_labels(f, app, area, now, duplicates.as_ref());
}

fn draw_tab_bar_labels(
    f: &mut Frame,
    app: &App,
    area: Rect,
    now: std::time::Instant,
    duplicates: Option<&TabNameDuplicates>,
) {
    // Hidden tabs do not produce spans. Keep allocation tied to the viewport,
    // including the one partially visible tab and both scroll indicators.
    let visible_capacity = app.tabs.len().min(usize::from(area.width) / 4 + 2);
    let mut spans = Vec::with_capacity(visible_capacity * 6 + 2);
    let mut file_icon_areas = Vec::new();
    let mut active_tab_area = None;
    let has_gradient = app.colors.gradients.bg_tabbar.start != app.colors.gradients.bg_tabbar.end;
    let tab_active_bg = app.colors.bg_tab_active;
    let tab_inactive_bg = app.colors.bg_tab_inactive;
    let tab_bar_bg = app.colors.bg_tabbar;
    let fg_text = app.colors.ui.fg_tab_active;
    let fg_dim = app.colors.ui.fg_tab_inactive;
    let sliding_indicator = app.terminal_has_focus
        && app
            .animation_settings
            .enabled(crate::animation_config::AnimationEffect::TabSlide);

    let available_width = area.width as usize;
    let mut used_width = 0;
    let scroll = app.tab_scroll_offset;
    let tab_track_start = if scroll > 0 { TAB_SCROLL_INDICATOR_WIDTH } else { 0 };
    let mut tab_track_end = tab_track_start;

    // Show scroll indicator if scrolled
    if scroll > 0 {
        let left_style = app.colors.ui.toolbar_style(fg_text, tab_bar_bg, app.hovered_tab_scroll_left, false, false);
        spans.push(Span::styled("◂ ", left_style));
        used_width += TAB_SCROLL_INDICATOR_WIDTH;
    }

    for (visible_count, (i, tab)) in app.tabs.iter().enumerate().skip(scroll).enumerate() {
        let name = tab.editor.file_name();
        let is_file_tab = matches!(tab.kind, crate::app::TabKind::File | crate::app::TabKind::GitRevision);
        let deleted_prefix =
            if is_file_tab && tab.editor.disk_unavailable {
                "~"
            } else {
                ""
            };
        let chat_marker = chat_tab_marker(app, tab.kind);
        let icon = chat_marker.map_or_else(|| tab_icon(tab.kind, name), |(icon, _)| icon);
        let file_icon_color = chat_marker.map(|(_, color)| color);
        let show_close = !tab.pinned
            && (i == app.active_tab || app.hovered_tab == Some(i) || app.dragging_tab == Some(i));
        let close = if tab.pinned {
            "   "
        } else if show_close {
            " × "
        } else {
            "   "
        };

        // Show dedicated name for markdown preview tabs, otherwise normal/diff label.
        let display_name = tab_display_name_with_duplicates(app, i, tab, duplicates);

        let prefix = if tab.pinned { " 📌" } else { " " };
        let modified_width = if tab.has_unsaved_changes() { 2 } else { 0 }; // "● " = 2
        let tab_width = tab_label_width_with_icon(tab, display_name.as_ref(), icon);
        if used_width + tab_width > available_width && visible_count > 0 {
            // Show right scroll indicator
            let right_style = app.colors.ui.toolbar_style(fg_text, tab_bar_bg, app.hovered_tab_scroll_right, false, false);
            spans.push(Span::styled(" ▸", right_style));
            used_width += TAB_SCROLL_INDICATOR_WIDTH;
            break;
        }

        if i == app.active_tab {
            active_tab_area = Some(Rect::new(
                area.x.saturating_add(crate::helpers::len_as_u16(used_width)),
                area.y,
                crate::helpers::len_as_u16(tab_width),
                1,
            ).intersection(area));
        }

        if used_width + 2 < available_width
            && chat_marker.is_some_and(|(icon, _)| icon.trim_end() == app.chat.activity.symbol()) {
            app.chat.activity.show();
        }

        let is_dragging = app.dragging_tab == Some(i);
        let (fg, bg) = if i == app.active_tab {
            (fg_text, tab_active_bg)
        } else if is_dragging {
            // Match the editor drop surface, not the focus-outline color.
            (fg_text, app.colors.ui.bg_editor_drop)
        } else if app.hovered_tab == Some(i) {
            // Brighten the tab slightly on hover
            (app.colors.ui.fg_tab_hover, app.colors.ui.bg_tab_hover)
        } else {
            (fg_dim, tab_inactive_bg)
        };

        let fg = app.colors.ui.readable_foreground(fg, bg, 4.5);
        let mut text_style = if is_file_tab && tab.preview && !tab.pinned && !tab.has_unsaved_changes() {
            Style::default()
                .fg(fg)
                .bg(bg)
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default().fg(fg).bg(bg)
        };
        let border = if i == app.active_tab && sliding_indicator { None }
            else if i == app.active_tab { app.colors.ui.border_tab_active }
            else if app.hovered_tab == Some(i) { app.colors.ui.border_tab_hover } else { None };
        if let Some(border) = border {
            text_style = text_style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        // Split tab into parts for per-element styling
        spans.push(Span::styled(prefix, text_style));
        if tab.has_unsaved_changes() {
            let mod_color = app.colors.ui.readable_foreground(app.colors.fg_modified_indicator, bg, 3.0);
            spans.push(Span::styled("● ", Style::default().fg(mod_color).bg(bg)));
        }
        let file_icon_color = if is_file_tab { Some(file_icon_colored(name).1) } else { file_icon_color };
        if is_file_tab && has_gradient {
            let offset = used_width + prefix.width() + modified_width;
            file_icon_areas.push(Rect::new(area.x.saturating_add(crate::helpers::len_as_u16(offset)),
                area.y, crate::helpers::len_as_u16(icon.width()), 1).intersection(area));
        }
        let icon_style = file_icon_color.map_or(text_style, |color| {
            text_style.fg(if is_file_tab { color } else { app.colors.ui.readable_foreground(color, bg, 3.0) })
        });
        spans.push(Span::styled(icon, icon_style));
        let label = if deleted_prefix.is_empty() { display_name }
            else { std::borrow::Cow::Owned(format!("{deleted_prefix}{display_name}")) };
        spans.push(Span::styled(label, text_style));
        let close_style = if show_close {
            let is_close_hovered = app.hovered_tab == Some(i) && app.hovered_tab_close_btn;
            app.colors.ui.toolbar_style(fg, bg, is_close_hovered, false, false)
        } else {
            Style::default().bg(bg)
        };
        if let Some(border) = app.colors.ui.border_tab {
            spans.push(Span::styled(close, close_style.add_modifier(Modifier::UNDERLINED).underline_color(border)));
        } else { spans.push(Span::styled(close, close_style)); }
        used_width += tab_width;
        tab_track_end = used_width.min(available_width);
    }

    // "+" new tab button
    if used_width + 3 <= available_width {
        let style = app.colors.ui.toolbar_style(fg_text, tab_bar_bg, app.hovered_new_tab_button, false, false);
        spans.push(Span::styled(" + ", style));
        used_width += 3;
    }

    // Fill rest of tab bar
    let remaining = available_width.saturating_sub(used_width);
    spans.push(Span::styled(
        crate::helpers::spaces(remaining),
        Style::default().bg(tab_bar_bg),
    ));

    let line = Line::from(spans);
    let p = Paragraph::new(line);
    f.render_widget(p, area);

    // Apply gradient to tab bar background (cell by cell)
    let tb_grad = &app.colors.gradients.bg_tabbar;
    if tb_grad.start != tb_grad.end && area.width > 0 {
        let buf = f.buffer_mut();
        let g = crate::app_theme::GradientPrecomputed::new(area.width, 1, tb_grad.angle_deg);
        for x in area.x..area.x + area.width {
            let t = g.t_at(x - area.x, 0);
            let grad_bg = crate::app_theme::lerp_color(tb_grad.start, tb_grad.end, t);
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                if cell.bg == tab_bar_bg || cell.bg == tab_inactive_bg {
                    cell.set_bg(grad_bg);
                }
            }
        }
        crate::ui_colors::ensure_control_row_contrast_except(buf, area, &app.colors.ui, &file_icon_areas);
    }

    if app.animation_settings.any_enabled() && !area.is_empty() {
        let mut slot = app.tab_animation_cache.borrow_mut();
        let cache = slot.get_or_insert_with(|| TabAnimationCache {
            cells: ratatui::buffer::Buffer::empty(area),
            active: active_tab_area,
            track: (tab_track_start, tab_track_end),
            scroll,
            strip_scroll: None,
            editor: app.editor().map(|editor| editor.navigation_id),
        });
        cache.cells.resize(area);
        let buffer = f.buffer_mut();
        for y in area.y..area.bottom() {
            let source = buffer.index_of(area.x, y);
            let target = cache.cells.index_of(area.x, y);
            cache.cells.content[target..target + usize::from(area.width)]
                .clone_from_slice(&buffer.content[source..source + usize::from(area.width)]);
        }
        cache.active = active_tab_area;
        cache.track = (tab_track_start, tab_track_end);
        cache.scroll = scroll;
        cache.strip_scroll = None;
        cache.editor = app.editor().map(|editor| editor.navigation_id);
    } else {
        *app.tab_animation_cache.borrow_mut() = None;
    }
    draw_tab_indicator(
        f,
        app,
        area,
        active_tab_area,
        (tab_track_start, tab_track_end),
        scroll,
        now,
    );
}

fn draw_tab_indicator(
    f: &mut Frame,
    app: &App,
    area: Rect,
    active_tab_area: Option<Rect>,
    track: (usize, usize),
    scroll: usize,
    now: std::time::Instant,
) {
    let (tab_track_start, tab_track_end) = track;
    if app.terminal_has_focus
        && let Some(tab_area) = active_tab_area.filter(|rect| !rect.is_empty())
        && let Some(editor) = app.editor()
        && let Some(indicator) = app.animations.tab_indicator(
            editor.navigation_id,
            tab_area,
            area,
            scroll,
            &app.animation_settings,
            now,
        )
    {
        let strength =
            app.animation_settings
                .intensity(crate::animation_config::AnimationEffect::TabSlide) as f32
                / 100.0;
        // Integrate the fractional edge coverage and blend light, rather than
        // RGB channel values. The boundary then moves evenly across dark and
        // light themes instead of dimming and jumping to the next column.
        let indicator_color = app
            .colors
            .ui
            .border_tab_active
            .unwrap_or(app.colors.fg_accent);
        let track_left = area.x.saturating_add(crate::helpers::len_as_u16(tab_track_start)).min(area.right());
        let track_right = area.x.saturating_add(crate::helpers::len_as_u16(tab_track_end)).min(area.right());
        for x in track_left..track_right {
            let coverage = indicator_coverage(x, indicator.left, indicator.right);
            if coverage > 0.0 && let Some(cell) = f.buffer_mut().cell_mut((x, area.y)) {
                let amount = coverage * (0.75 + strength * 0.25);
                cell.set_style(
                    Style::default()
                        .underline_color(indicator_blend(
                            cell.bg,
                            indicator_color,
                            amount,
                        ))
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
        }
    }
}

fn indicator_coverage(column: u16, left: f32, right: f32) -> f32 {
    if !left.is_finite() || !right.is_finite() || right <= left {
        return 0.0;
    }
    ((column as f32 + 1.0).min(right) - (column as f32).max(left)).clamp(0.0, 1.0)
}

fn indicator_blend(
    surface: ratatui::style::Color,
    accent: ratatui::style::Color,
    amount: f32,
) -> ratatui::style::Color {
    use ratatui::style::Color;
    let amount = amount.clamp(0.0, 1.0);
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (surface, accent) else {
        return crate::app_theme::lerp_color(surface, accent, amount);
    };
    let linear = crate::ui_colors::srgb_linear_channel;
    let channel = |a, b| {
        let light = linear(a) * (1.0 - amount) + linear(b) * amount;
        crate::ui_colors::srgb_from_linear(light)
    };
    Color::Rgb(channel(ar, br), channel(ag, bg), channel(ab, bb))
}

pub(crate) fn draw_breadcrumb(f: &mut Frame, app: &mut App, area: Rect) {
    let area = crate::editor_blame::header(f, app, area);
    app.breadcrumb_markdown_preview_area = None;
    app.diff_nav_prev_area = None;
    app.diff_nav_next_area = None;
    app.diff_layout_toggle_area = None;

    if area.is_empty() {
        return;
    }

    let has_diff = app.should_render_diff_for_active_editor();
    let has_cached_diff_changes = has_diff && !app.diff_render_cache.hunks.is_empty();
    let has_changes = has_cached_diff_changes
        || app.editor().is_some_and(|ed| ed.has_git_line_changes());
    let is_markdown = app
        .tabs
        .get(app.active_tab)
        .is_some_and(|t| t.kind == crate::app::TabKind::File)
        && active_editor_is_markdown(app);
    let preview_label = " Preview Markdown ";
    let preview_w = UnicodeWidthStr::width(preview_label) as u16;
    let diff_nav_w: u16 = if has_changes { 6 } else { 0 }; // " ▲ " + " ▼ "
    // Reserve the same width in both modes so repeated clicks stay on the button.
    let diff_layout_w: u16 = if has_diff { 14 } else { 0 };
    let mut path_area = area;
    let mut right_reserved = 0u16;
    if is_markdown && area.width > preview_w + diff_nav_w + diff_layout_w {
        right_reserved += preview_w;
        path_area.width = area.width.saturating_sub(right_reserved);
        app.breadcrumb_markdown_preview_area = Some(Rect::new(
            area.x + area.width - right_reserved,
            area.y,
            preview_w,
            1,
        ));
    } else if is_markdown && area.width > preview_w + diff_layout_w {
        path_area.width = area.width.saturating_sub(preview_w);
        app.breadcrumb_markdown_preview_area = Some(Rect::new(
            path_area.x + path_area.width,
            area.y,
            preview_w,
            1,
        ));
    }
    if has_diff && path_area.width >= diff_layout_w {
        path_area.width = path_area.width.saturating_sub(diff_layout_w);
        app.diff_layout_toggle_area = Some(Rect::new(
            path_area.right(),
            area.y,
            diff_layout_w,
            1,
        ));
    }
    if has_changes && path_area.width > diff_nav_w {
        path_area.width = path_area.width.saturating_sub(diff_nav_w);
        let nav_x = path_area.x + path_area.width;
        app.diff_nav_prev_area = Some(Rect::new(nav_x, area.y, 3, 1));
        app.diff_nav_next_area = Some(Rect::new(nav_x + 3, area.y, 3, 1));
    }
    app.breadcrumb_area = Some(path_area);
    let Some(editor) = app.editor() else {
        return;
    };
    let preview_open_for_active = app.breadcrumb_markdown_preview_area.is_some() && editor
        .file_path
        .as_deref()
        .is_some_and(|p| app.has_markdown_preview_for_path(p));
    let path: String = match editor.file_path.as_ref() {
        Some(p) => {
            let rel = p.strip_prefix(&app.explorer.root).unwrap_or(p);
            rel.to_string_lossy().into_owned()
        }
        None => "untitled".to_owned(),
    };

    // Split path into components and join with " › "
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let dir_parts = if parts.len() > 1 {
        &parts[..parts.len() - 1]
    } else {
        &[] as &[&str]
    };

    // Truncate middle segments if path is too long for the area
    let max_width = (path_area.width as usize).saturating_sub(4); // padding
    let full_width: usize = dir_parts
        .iter()
        .map(|p| UnicodeWidthStr::width(*p))
        .sum::<usize>()
        + if dir_parts.len() > 1 {
            (dir_parts.len() - 1) * 3
        } else {
            0
        }; // " › " separators
           // Build display parts with their original dir_parts index for navigation
    let display_parts: Vec<(&str, usize)> = if full_width > max_width && dir_parts.len() > 2 {
        // Keep first and last segments, replace middle with "…"
        let mut result: Vec<(&str, usize)> = vec![(dir_parts[0], 0)];
        let remaining = max_width.saturating_sub(
            UnicodeWidthStr::width(dir_parts[0])
                + UnicodeWidthStr::width(dir_parts[dir_parts.len() - 1])
                + 6,
        ); // 6 for separators
        if dir_parts.len() > 3
            && remaining
                < dir_parts[1..dir_parts.len() - 1]
                    .iter()
                    .map(|p| UnicodeWidthStr::width(*p) + 3)
                    .sum::<usize>()
        {
            // "…" is not clickable — skip it in segments (no entry pushed)
            result.push(("…", usize::MAX)); // sentinel: not a navigable segment
        } else {
            for (j, p) in dir_parts[1..dir_parts.len() - 1].iter().enumerate() {
                result.push((p, 1 + j));
            }
        }
        result.push((dir_parts[dir_parts.len() - 1], dir_parts.len() - 1));
        result
    } else {
        dir_parts.iter().enumerate().map(|(i, p)| (*p, i)).collect()
    };

    let mut spans = vec![Span::styled(
        "  ",
        Style::default().bg(app.colors.ui.bg_breadcrumb),
    )];
    let mut x_pos = path_area.x + 2; // 2 for initial padding
    app.breadcrumb_segments.clear();

    for (i, (part, orig_idx)) in display_parts.iter().enumerate() {
        let part_width = UnicodeWidthStr::width(*part) as u16;
        let separator_width = if i > 0 { 3u16 } else { 0 };
        // Stop rendering if this segment would overflow the breadcrumb area
        if x_pos + separator_width + part_width > path_area.x + path_area.width {
            break;
        }
        if i > 0 {
            spans.push(Span::styled(
                " › ",
                Style::default()
                    .fg(app.colors.ui.fg_breadcrumb)
                    .bg(app.colors.ui.bg_breadcrumb),
            ));
            x_pos += 3;
        }
        let is_last = i == display_parts.len() - 1;
        let is_hovered = app.mouse_row == path_area.y
            && app.mouse_col >= x_pos
            && app.mouse_col < x_pos + part_width;
        let fg = if is_hovered {
            app.colors.ui.fg_breadcrumb_focus
        } else if is_last {
            app.colors.ui.fg_breadcrumb_focus
        } else {
            app.colors.ui.fg_breadcrumb
        };
        let mut style = Style::default().fg(app.colors.ui.readable_foreground(fg, app.colors.ui.bg_breadcrumb, 4.5)).bg(app.colors.ui.bg_breadcrumb);
        if is_hovered {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        // Only store navigable segments (not "…" ellipsis)
        if *orig_idx != usize::MAX {
            // Store the number of path components to take (orig_idx + 1),
            // so mouse handler can use parts.iter().take(seg_idx) correctly
            app.breadcrumb_segments
                .push((x_pos, part_width, *orig_idx + 1));
        }
        spans.push(Span::styled(*part, style));
        x_pos += part_width;
    }
    // File name with icon
    if let Some(&file_name) = parts.last() {
        if !display_parts.is_empty() || !dir_parts.is_empty() {
            spans.push(Span::styled(
                " › ",
                Style::default()
                    .fg(app.colors.ui.fg_breadcrumb)
                    .bg(app.colors.ui.bg_breadcrumb),
            ));
            x_pos += 3;
        }
        let (icon, icon_color) = file_icon_colored(file_name);
        let mut icon_str = String::with_capacity(icon.len() + 1);
        icon_str.push_str(icon);
        icon_str.push(' ');
        let icon_w = UnicodeWidthStr::width(icon_str.as_str()) as u16;
        let name_w = UnicodeWidthStr::width(file_name) as u16;
        if x_pos + icon_w + name_w <= path_area.x + path_area.width {
            spans.push(Span::styled(
                icon_str,
                Style::default().fg(icon_color).bg(app.colors.ui.bg_breadcrumb),
            ));
            spans.push(Span::styled(
                file_name,
                Style::default()
                    .fg(app.colors.ui.readable_foreground(app.colors.ui.fg_breadcrumb_focus, app.colors.ui.bg_breadcrumb, 4.5))
                    .bg(app.colors.ui.bg_breadcrumb),
            ));
        }
    }
    // Add current symbol name if available (find enclosing symbol at cursor line via binary search)
    let cursor_line = app.editor().map_or(0, |ed| ed.cursor_line);
    let pos = app.outline_items.partition_point(|s| s.line <= cursor_line);
    let symbol_name = if pos > 0 {
        Some(&app.outline_items[pos - 1].name)
    } else {
        None
    };
    if let Some(sym) = symbol_name {
        spans.push(Span::styled(
            " › ",
            Style::default()
                .fg(app.colors.ui.fg_breadcrumb)
                .bg(app.colors.ui.bg_breadcrumb),
        ));
        spans.push(Span::styled(
            sym.as_str(),
            Style::default()
                .fg(app.colors.ui.readable_foreground(app.colors.ui.fg_breadcrumb_focus, app.colors.ui.bg_breadcrumb, 4.5))
                .bg(app.colors.ui.bg_breadcrumb),
        ));
    }

    let total_used: usize = spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let fill = (path_area.width as usize).saturating_sub(total_used);
    spans.push(Span::styled(
        crate::helpers::spaces(fill),
        Style::default().bg(app.colors.ui.bg_breadcrumb),
    ));

    let line = Line::from(spans);
    let p = Paragraph::new(line).style(Style::default().bg(app.colors.ui.bg_breadcrumb));
    if path_area.width > 0 {
        f.render_widget(p, path_area);
    }

    if let Some(button_area) = app.diff_layout_toggle_area {
        let hovered = button_area.contains((app.mouse_col, app.mouse_row).into());
        let label = match app.diff_mode {
            crate::app::DiffMode::SideBySide => "Inline",
            crate::app::DiffMode::Inline => "Side by Side",
            crate::app::DiffMode::Off => "",
        };
        let style = app.colors.ui.toolbar_style(app.colors.ui.fg_breadcrumb,
            app.colors.ui.bg_breadcrumb, hovered, false, false).add_modifier(Modifier::BOLD);
        f.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style),
            button_area,
        );
    }

    // Diff navigation buttons (▲ prev change, ▼ next change)
    if let Some(prev_area) = app.diff_nav_prev_area {
        let hovered = app.mouse_row == prev_area.y
            && app.mouse_col >= prev_area.x
            && app.mouse_col < prev_area.x + prev_area.width;
        let style = app.colors.ui.toolbar_style(app.colors.ui.fg_breadcrumb,
            app.colors.ui.bg_breadcrumb, hovered, false, false);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" ▲ ", style))),
            prev_area,
        );
    }
    if let Some(next_area) = app.diff_nav_next_area {
        let hovered = app.mouse_row == next_area.y
            && app.mouse_col >= next_area.x
            && app.mouse_col < next_area.x + next_area.width;
        let style = app.colors.ui.toolbar_style(app.colors.ui.fg_breadcrumb,
            app.colors.ui.bg_breadcrumb, hovered, false, false);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" ▼ ", style))),
            next_area,
        );
    }

    if let Some(button_area) = app.breadcrumb_markdown_preview_area {
        let hovered = app.mouse_row == button_area.y
            && app.mouse_col >= button_area.x
            && app.mouse_col < button_area.x + button_area.width;
        let mut style = app.colors.ui.toolbar_style(app.colors.ui.fg_breadcrumb,
            app.colors.ui.bg_breadcrumb, hovered, false, preview_open_for_active)
            .add_modifier(Modifier::BOLD);
        if preview_open_for_active { style = style.add_modifier(Modifier::UNDERLINED); }
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(preview_label, style)))
                .style(Style::default().bg(app.colors.ui.bg_breadcrumb)),
            button_area,
        );
    }

    // Draw breadcrumb dropdown if active. Directory metadata is captured by
    // the worker, so this render path never touches the filesystem.
    if app.breadcrumb_dropdown_active {
        let dropdown_x = app
            .breadcrumb_segments
            .get(app.breadcrumb_dropdown_segment)
            .map_or(path_area.x, |&(x, _, _)| x);
        let dropdown_y = path_area.y + 1;
        let bc_items = &app.breadcrumb_dropdown_items;
        let dd_items: Vec<DropdownItem> = if app.breadcrumb_dropdown_loading {
            vec![DropdownItem::new("Loading…").disabled(true)]
        } else if bc_items.is_empty() {
            vec![DropdownItem::new("No items").disabled(true)]
        } else {
            bc_items
                .iter()
                .map(|(path, is_dir)| {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                    if *is_dir {
                        DropdownItem::new(name).icon("📁 ", app.colors.ui.folder_icon_color(app.colors.bg_menu))
                    } else {
                        DropdownItem::new(name).icon("  ", app.colors.ui.icon_color(app.colors.bg_menu))
                    }
                })
                .collect()
        };
        let colors = themed_dropdown_colors(&app.colors);
        DropdownMenu::new(&dd_items, colors)
            .anchor(dropdown_x, dropdown_y)
            .min_width(14)
            .max_visible(13)
            .selected(app.breadcrumb_dropdown_selected)
            .mouse(app.mouse_row, app.mouse_col)
            .render(f);
    }
}

#[cfg(test)]
#[path = "ui_chrome_tabs_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
#[path = "ui_untitled_tabs_tests.rs"]
mod untitled_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DiffMode, Tab};
    use crate::editor::Editor;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn active_editor_is_markdown_with_md_extension() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(std::path::PathBuf::from("readme.md"));
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        assert!(active_editor_is_markdown(&app));
    }

    #[test]
    fn active_editor_is_markdown_with_markdown_extension() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(std::path::PathBuf::from("notes.markdown"));
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        assert!(active_editor_is_markdown(&app));
    }

    #[test]
    fn active_editor_is_markdown_with_mdx_extension() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(std::path::PathBuf::from("component.mdx"));
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        assert!(active_editor_is_markdown(&app));
    }

    #[test]
    fn active_editor_is_not_markdown_for_txt() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(std::path::PathBuf::from("readme.txt"));
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        assert!(!active_editor_is_markdown(&app));
    }

    #[test]
    fn active_editor_is_not_markdown_when_no_tabs() {
        let app = App::test_new();
        assert!(!active_editor_is_markdown(&app));
    }

    #[test]
    fn markdown_preview_button_and_command_follow_selected_language() {
        for name in ["readme.txt", "readme", "readme.md"] {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text("# Heading\n");
            editor.file_path = Some(name.into());
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
            for (language, expected) in [
                (Some("Plain Text"), false),
                (Some("Markdown"), true),
                (Some("Python"), false),
                (None, name.ends_with(".md")),
            ] {
                let command = language.map(crate::app_menu::language_command_id)
                    .unwrap_or_else(|| "lang_auto".into());
                app.execute_editor_command(&command);
                terminal.draw(|frame| draw_breadcrumb(frame, &mut app, frame.area())).unwrap();
                assert_eq!(app.breadcrumb_markdown_preview_area.is_some(), expected,
                    "{name}: {language:?}");
                app.toggle_markdown_preview_tab();
                assert_eq!(app.markdown_preview, expected, "{name}: {language:?}");
                if expected {
                    assert_eq!(app.tabs[app.active_tab].kind, crate::app::TabKind::MarkdownPreview);
                    app.toggle_markdown_preview_tab();
                }
                assert_eq!(app.tabs.len(), 1);
            }
        }
    }

    #[test]
    fn theme_tab_foregrounds_reach_the_rendered_cells() {
        let mut app = App::test_new();
        for name in ["a.txt", "b.txt"] {
            let mut editor = Editor::new();
            editor.file_path = Some(name.into());
            app.tabs.push(Tab::new_file(editor));
        }
        app.colors.ui.fg_tab_active = ratatui::style::Color::Rgb(242,244,246);
        app.colors.ui.fg_tab_inactive = ratatui::style::Color::Rgb(210,220,230);
        let mut terminal = Terminal::new(TestBackend::new(80,1)).unwrap();
        terminal.draw(|frame| draw_tab_bar(frame, &app, Rect::new(0,0,80,1))).unwrap();
        let cells = &terminal.backend().buffer().content;
        assert!(cells.iter().any(|c| c.symbol() == "a" && c.fg == app.colors.ui.fg_tab_active));
        assert!(cells.iter().any(|c| c.symbol() == "b" && c.fg == app.colors.ui.fg_tab_inactive));
    }

    #[test]
    fn active_tab_indicator_glides_and_retargets_without_jumping_or_recoloring_text() {
        use crate::animation_config::AnimationEffect;
        use std::time::{Duration, Instant};

        let mut app = App::test_new();
        for name in ["a.txt", "a_much_longer_document.txt"] {
            let mut editor = Editor::new();
            editor.file_path = Some(name.into());
            app.tabs.push(Tab::new_file(editor));
        }
        app.colors.ui.border_tab = None;
        app.colors.ui.border_tab_active = None;
        app.colors.ui.border_tab_hover = None;
        app.animation_settings.set(AnimationEffect::TabSlide, true);
        let mut terminal = Terminal::new(TestBackend::new(90, 1)).unwrap();
        let area = Rect::new(0, 0, 90, 1);
        let start = Instant::now();
        let underlined = |buffer: &ratatui::buffer::Buffer| {
            (0..90)
                .filter(|&x| buffer[(x, 0)].modifier.contains(Modifier::UNDERLINED))
                .collect::<Vec<_>>()
        };

        terminal
            .draw(|frame| draw_tab_bar_at(frame, &app, area, start))
            .unwrap();
        let first = underlined(terminal.backend().buffer());
        assert!(!first.is_empty());
        assert!(app
            .animations
            .next_frame_wait(&app.animation_settings, start)
            .is_none());

        app.active_tab = 1;
        let switched = start + Duration::from_millis(1);
        terminal
            .draw(|frame| draw_tab_bar_at(frame, &app, area, switched))
            .unwrap();
        assert_eq!(underlined(terminal.backend().buffer()), first);
        assert!(app
            .animations
            .next_frame_wait(&app.animation_settings, switched)
            .is_some());

        let midway = start + Duration::from_millis(90);
        terminal
            .draw(|frame| draw_tab_bar_at(frame, &app, area, midway))
            .unwrap();
        let moving = underlined(terminal.backend().buffer());
        assert_ne!(moving, first);
        assert!(moving.last() > first.last());
        app.active_tab = 0;
        terminal
            .draw(|frame| draw_tab_bar_at(frame, &app, area, midway))
            .unwrap();
        assert_eq!(underlined(terminal.backend().buffer()), moving);

        let settled = start + Duration::from_secs(2);
        app.animations
            .advance_frame(&app.animation_settings, settled);
        terminal
            .draw(|frame| draw_tab_bar_at(frame, &app, area, settled))
            .unwrap();
        let animated = terminal.backend().buffer().clone();
        assert_eq!(underlined(&animated), first);
        assert!(app
            .animations
            .next_frame_wait(&app.animation_settings, settled)
            .is_none());

        app.animation_settings.set(AnimationEffect::TabSlide, false);
        terminal
            .draw(|frame| draw_tab_bar_at(frame, &app, area, settled))
            .unwrap();
        for (with_indicator, plain) in animated
            .content
            .iter()
            .zip(&terminal.backend().buffer().content)
        {
            assert_eq!(with_indicator.symbol(), plain.symbol());
            assert_eq!(with_indicator.fg, plain.fg);
            assert_eq!(with_indicator.bg, plain.bg);
        }
        assert!(underlined(terminal.backend().buffer()).is_empty());
    }

    #[test]
    fn tab_indicator_fractional_edges_conserve_width_and_blend_light_evenly() {
        use ratatui::style::Color;
        for offset in [0.0, 0.01, 0.25, 0.5, 0.75, 0.99] {
            let left = 2.0 + offset;
            let right = left + 6.375;
            let coverage: f32 = (0..20).map(|x| indicator_coverage(x, left, right)).sum();
            assert!((coverage - 6.375).abs() < 0.00001);
        }
        assert_eq!(indicator_coverage(4, 5.0, 5.0), 0.0);
        assert_eq!(indicator_coverage(4, f32::NAN, 9.0), 0.0);
        assert_eq!(indicator_coverage(4, 0.0, f32::INFINITY), 0.0);
        let middle = indicator_blend(Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255), 0.5);
        assert_eq!(middle, Color::Rgb(188, 188, 188));
        assert_eq!(
            indicator_blend(Color::Rgb(12, 34, 56), Color::Rgb(210, 230, 250), 0.0),
            Color::Rgb(12, 34, 56)
        );
        assert_eq!(
            indicator_blend(Color::Rgb(12, 34, 56), Color::Rgb(210, 230, 250), 1.0),
            Color::Rgb(210, 230, 250)
        );
    }

    #[test]
    fn tab_indicator_scroll_resize_and_reorder_snap_to_the_new_layout_without_replaying_motion() {
        use crate::animation_config::AnimationEffect;
        use std::time::{Duration, Instant};
        let mut app = App::test_new();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            let mut editor = Editor::new();
            editor.file_path = Some(name.into());
            app.tabs.push(Tab::new_file(editor));
        }
        app.colors.ui.border_tab_active = None;
        app.animation_settings.set(AnimationEffect::TabSlide, true);
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        let area = Rect::new(0, 0, 80, 1);
        let start = Instant::now();
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, start))
            .unwrap();
        app.active_tab = 2;
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, start))
            .unwrap();
        let moving = start + Duration::from_millis(40);
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, moving))
            .unwrap();
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, moving)
                .is_some()
        );

        app.tab_scroll_offset = 1;
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, moving))
            .unwrap();
        let scrolled = terminal.backend().buffer().clone();
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, moving)
                .is_none()
        );
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, moving + Duration::from_secs(2)))
            .unwrap();
        assert_eq!(&scrolled, terminal.backend().buffer());

        // The same editor can move when tabs are reordered. Its underline
        // belongs to the new slot immediately, with no false tab-switch event.
        app.tab_scroll_offset = 0;
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, moving))
            .unwrap();
        app.tabs.swap(0, 2);
        app.active_tab = 0;
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, moving))
            .unwrap();
        let reordered = terminal.backend().buffer().clone();
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, moving)
                .is_none()
        );
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, moving + Duration::from_secs(2)))
            .unwrap();
        assert_eq!(&reordered, terminal.backend().buffer());

        app.active_tab = 1;
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, moving))
            .unwrap();
        let narrow = Rect::new(0, 0, 40, 1);
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, narrow, moving + Duration::from_millis(20)))
            .unwrap();
        assert!(
            app.animations
                .next_frame_wait(&app.animation_settings, moving + Duration::from_millis(20))
                .is_none()
        );
    }

    #[test]
    fn tab_indicator_motion_preserves_labels_icons_gradients_and_toolbar_targets() {
        use crate::animation_config::AnimationEffect;
        use std::time::{Duration, Instant};
        let mut app = App::test_new();
        for name in ["a.rs", "longer.rs", "c.rs"] {
            let mut editor = Editor::new();
            editor.file_path = Some(name.into());
            app.tabs.push(Tab::new_file(editor));
        }
        app.colors.ui.border_tab_active = None;
        app.colors.ui.border_tab_hover = None;
        app.colors.gradients.bg_tabbar.start = ratatui::style::Color::Rgb(12, 18, 34);
        app.colors.gradients.bg_tabbar.end = ratatui::style::Color::Rgb(46, 58, 82);
        app.animation_settings.set(AnimationEffect::TabSlide, true);
        let mut terminal = Terminal::new(TestBackend::new(90, 3)).unwrap();
        let area = Rect::new(5, 1, 80, 1);
        let start = Instant::now();
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, start))
            .unwrap();
        app.active_tab = 2;
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, start))
            .unwrap();
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, start + Duration::from_millis(38)))
            .unwrap();
        let moving = terminal.backend().buffer().clone();
        assert!(
            moving
                .content
                .iter()
                .any(|cell| cell.modifier.contains(Modifier::UNDERLINED))
        );
        app.animation_settings.set(AnimationEffect::TabSlide, false);
        terminal
            .draw(|f| draw_tab_bar_at(f, &app, area, start + Duration::from_millis(38)))
            .unwrap();
        let plain = terminal.backend().buffer();
        for y in 0..3 {
            for x in 0..90 {
                let animated = &moving[(x, y)];
                let baseline = &plain[(x, y)];
                assert_eq!(animated.symbol(), baseline.symbol(), "symbol at {x},{y}");
                assert_eq!(animated.fg, baseline.fg, "foreground at {x},{y}");
                assert_eq!(animated.bg, baseline.bg, "background at {x},{y}");
                assert_eq!(
                    animated.modifier - Modifier::UNDERLINED,
                    baseline.modifier - Modifier::UNDERLINED
                );
                if y != 1 || x < 5 || x >= 85 || matches!(animated.symbol(), "+" | "◂" | "▸") {
                    assert_eq!(animated, baseline, "outside tab track at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn tab_indicator_handles_zero_width_and_clipped_first_tabs_without_touching_surroundings() {
        use crate::animation_config::AnimationEffect;
        use std::time::Instant;
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some("a_very_long_tab_name.rs".into());
        app.tabs.push(Tab::new_file(editor));
        app.animation_settings.set(AnimationEffect::TabSlide, true);
        let mut terminal = Terminal::new(TestBackend::new(10, 3)).unwrap();
        for width in [0, 1, 4] {
            let area = Rect::new(3, 1, width, 1);
            terminal
                .draw(|f| draw_tab_bar_at(f, &app, area, Instant::now()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..3 {
                for x in 0..10 {
                    if y != 1 || x < area.x || x >= area.right() {
                        assert_eq!(buffer[(x, y)].symbol(), " ");
                        assert!(!buffer[(x, y)].modifier.contains(Modifier::UNDERLINED));
                    }
                }
            }
        }
    }

    #[test]
    fn file_tab_icon_uses_the_explorer_file_type_color() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(app.explorer.root.join(".gitignore"));
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        let mut terminal = Terminal::new(TestBackend::new(40, 1)).expect("terminal");
        terminal
            .draw(|frame| draw_tab_bar(frame, &app, Rect::new(0, 0, 40, 1)))
            .expect("draw tab bar");

        let buffer = terminal.backend().buffer();
        let expected_icon_color = ratatui::style::Color::Rgb(241, 80, 47);
        assert_eq!(buffer.cell((5, 0)).unwrap().symbol(), "G");
        assert_eq!(buffer.cell((5, 0)).unwrap().fg, expected_icon_color);
        assert_eq!(buffer.cell((8, 0)).unwrap().fg, app.colors.ui.fg_tab_active);
    }

    #[test]
    fn settings_tab_ignores_document_flags_and_keeps_its_close_target() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::test_new();
        app.open_settings_tab();
        app.execute_editor_command("duplicate_line");
        assert!(app.editor().unwrap().read_only);
        assert!(!app.editor().unwrap().modified);
        assert_eq!(app.editor().unwrap().rope.len_chars(), 0);

        let area = Rect::new(0, 0, 80, 1);
        app.tab_bar_area = Some(area);
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal.draw(|frame| draw_tab_bar(frame, &app, area)).unwrap();
        let clean = terminal.backend().buffer().clone();
        let width = crate::mouse::tab_render_width(&app, 0, &app.tabs[0]);

        // Tool views may carry stale backing-buffer flags, but these are not
        // document state and must not decorate or widen their tabs.
        app.tabs[0].editor.modified = true;
        app.tabs[0].editor.disk_unavailable = true;
        terminal.draw(|frame| draw_tab_bar(frame, &app, area)).unwrap();
        assert_eq!(terminal.backend().buffer(), &clean);
        assert_eq!(crate::mouse::tab_render_width(&app, 0, &app.tabs[0]), width);
        let close_x = (0..80).find(|&x| clean.cell((x, 0)).unwrap().symbol() == "×").unwrap();
        crate::mouse::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close_x, row: 0, modifiers: KeyModifiers::NONE,
        });
        assert!(app.tabs.is_empty());
        assert!(!app.confirm_close_active);

        // Ordinary files still show their unsaved indicator.
        let mut editor = Editor::new();
        editor.modified = true;
        app.tabs.push(Tab::new_file(editor));
        terminal.draw(|frame| draw_tab_bar(frame, &app, area)).unwrap();
        assert!(terminal.backend().buffer().content.iter().any(|cell| cell.symbol() == "●"));
    }

    #[test]
    fn settings_in_open_editors_and_switcher_use_tool_icons_without_file_markers() {
        let mut app = App::test_new();
        app.open_settings_tab();
        app.show_open_editors = true;
        app.tab_mru_order = vec![0];
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        for switcher in [false, true] {
            let draw = |frame: &mut Frame, app: &mut App| {
                if switcher {
                    crate::ui_pickers::draw_tab_switcher(frame, app);
                } else {
                    crate::ui_chrome::draw_explorer(frame, app, Rect::new(0, 0, 40, 24));
                }
            };
            app.tabs[0].editor.modified = false;
            app.tabs[0].editor.disk_unavailable = false;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let clean = terminal.backend().buffer().clone();
            assert!(clean.content.iter().any(|cell| cell.symbol() == "≡"));
            app.tabs[0].editor.modified = true;
            app.tabs[0].editor.disk_unavailable = true;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(terminal.backend().buffer(), &clean);
        }
    }

    #[test]
    fn diff_layout_button_handles_unchanged_compares_and_narrow_breadcrumbs() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some("readme.md".into());
        editor.rope = ropey::Rope::from_str("unchanged\n");
        app.tabs.push(Tab::new_file(editor));
        app.compare_base_content = Some("unchanged\n".into());
        app.set_compare_diff_lines(crate::app::compute_file_diff(
            "unchanged\n",
            "unchanged\n",
        ));

        let mut terminal = Terminal::new(TestBackend::new(100, 2)).unwrap();
        for width in [0, 1, 13, 14, 20, 40, 90] {
            for mode in [DiffMode::SideBySide, DiffMode::Inline, DiffMode::Off] {
                app.diff_mode = mode;
                let area = Rect::new(5, 1, width, 1);
                terminal
                    .draw(|frame| draw_breadcrumb(frame, &mut app, area))
                    .unwrap();
                assert_eq!(
                    app.diff_layout_toggle_area.is_some(),
                    width >= 14 && mode != DiffMode::Off
                );
                assert!(app.diff_nav_prev_area.is_none());
                assert!(app.diff_nav_next_area.is_none());
                if let Some(button) = app.diff_layout_toggle_area {
                    assert_eq!(button.intersection(area), button);
                    assert!(button.x >= app.breadcrumb_area.unwrap().right());
                    if let Some(preview) = app.breadcrumb_markdown_preview_area {
                        assert!(button.right() <= preview.x);
                    }
                }
            }
        }
    }

    #[test]
    fn diff_navigation_buttons_use_cached_hunks_when_git_markers_are_empty() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(std::path::PathBuf::from("file.txt"));
        editor.rope = ropey::Rope::from_str("after\n");
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.diff_mode = DiffMode::SideBySide;
        app.compare_base_content = Some("before\n".to_string().into());
        app.set_compare_diff_lines(crate::app::compute_file_diff("before\n", "after\n"));
        app.editor_mut()
            .expect("active editor")
            .clear_git_line_status();

        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|frame| draw_breadcrumb(frame, &mut app, Rect::new(0, 0, 80, 1)))
            .expect("draw breadcrumb");

        assert!(app.diff_nav_prev_area.is_some());
        assert!(app.diff_nav_next_area.is_some());
        let rendered: String = (0..80)
            .filter_map(|x| terminal.backend().buffer().cell((x, 0)))
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains('▲'));
        assert!(rendered.contains('▼'));
    }

    #[test]
    #[ignore = "exports deterministic production tab frames for visual motion review"]
    fn export_tab_animation_preview() {
        use crate::animation_config::AnimationEffect;
        use ratatui::style::Color;
        use std::time::{Duration, Instant};

        fn color(value: Color) -> String {
            match value {
                Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                _ => "#b0b5c0".into(),
            }
        }

        let mut app = App::test_new();
        for name in ["main.rs", "scope_connector.rs", "theme.json"] {
            let mut editor = Editor::test_with_text("");
            editor.file_path = Some(name.into());
            app.tabs.push(Tab::new_file(editor));
        }
        app.active_tab = 0;
        app.terminal_has_focus = true;
        app.animation_settings.set(AnimationEffect::TabSlide, true);
        app.animation_settings.duration_ms = 420;
        app.animation_settings.intensity = 75;
        let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
        let area = Rect::new(0, 1, 80, 1);
        let start = Instant::now();
        terminal
            .draw(|frame| draw_tab_bar_at(frame, &app, area, start))
            .unwrap();

        let mut frames = Vec::with_capacity(120);
        let mut underlines = Vec::with_capacity(120);
        for index in 0..120 {
            match index {
                5 => app.active_tab = 2,
                13 => app.active_tab = 1,
                22 => app.active_tab = 0,
                38 => app.active_tab = 2,
                _ => {}
            }
            let now = start + Duration::from_millis(index * 16);
            app.animations.begin_frame();
            terminal
                .draw(|frame| {
                    frame.buffer_mut().set_style(
                        Rect::new(0, 0, 80, 4),
                        Style::default()
                            .fg(app.colors.fg_text)
                            .bg(app.colors.bg_editor),
                    );
                    draw_tab_bar_at(frame, &app, area, now);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let cells: Vec<_> = buffer
                .content
                .iter()
                .map(|cell| (cell.symbol().to_owned(), color(cell.fg), color(cell.bg)))
                .collect();
            let strokes: Vec<_> = buffer
                .content
                .iter()
                .map(|cell| {
                    cell.modifier.contains(Modifier::UNDERLINED).then(|| {
                        color(if cell.underline_color == Color::Reset {
                            cell.fg
                        } else {
                            cell.underline_color
                        })
                    })
                })
                .collect();
            frames.push(cells);
            underlines.push(strokes);
        }
        let scenes = serde_json::json!([{
            "name": "Tabs · elastic underline",
            "width": 80,
            "height": 4,
            "frames": frames,
            "underlines": underlines,
        }]);
        std::fs::write(
            "target/tab-animation-preview.json",
            serde_json::to_vec(&scenes).unwrap(),
        )
        .unwrap();
    }
}
