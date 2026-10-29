//! Compact, keyboard-accessible desk and tab-group controls in the tab strip.
use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Mode {
    Switch,
    New,
    Rename,
    GroupNew,
    Group(u64),
    AddGroup,
    Move { ids: Vec<u64>, copy: bool },
    Close,
}
#[derive(Clone)]
enum Action {
    Switch(u64),
    Rename,
    Close,
    Create(bool),
    SaveName,
    MakeGroup,
    AddGroup(u64),
    GroupRename(u64),
    Color(u64, u8),
    Collapse(u64),
    Ungroup(u64),
    CloseGroup(u64),
    Move(u64),
    MoveNew,
    MoveGroup(u64),
    ConfirmClose,
    Cancel,
}
#[derive(Default)]
pub(crate) struct Popup {
    pub mode: Option<Mode>,
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub history: crate::text_input::TextInputHistory,
    pub selected: usize,
    pub scroll: usize,
    pub area: Rect,
    pub input: Rect,
    pub input_map: Option<crate::text_input::TextInputHitMap>,
    pub input_drag: bool,
    pub origin: (u16, u16),
    hits: Vec<(Rect, Action)>,
}

pub(crate) fn open(app: &mut App, mode: Mode) {
    if crate::ui_modal::active(app).is_some() {
        return;
    }
    app.clear_transient_editor_state();
    app.clear_tab_tooltip();
    app.tab_context_active = false;
    app.palette.active = false;
    if mode == Mode::Close && app.current_desk_is_empty() {
        app.close_desk();
        return;
    }
    let text = match mode {
        Mode::Rename => app.current_desk_name().to_owned(),
        Mode::Group(id) => app
            .desks
            .groups
            .get(&id)
            .map_or(String::new(), |g| g.name.clone()),
        _ => String::new(),
    };
    let area = host_area(app).unwrap_or(Rect::new(2, 1, 0, 0));
    app.desks.popup = Popup {
        mode: Some(mode),
        cursor: text.len(),
        anchor: (!text.is_empty()).then_some(0),
        text,
        origin: (area.x, area.bottom()),
        ..Default::default()
    };
}

fn host_area(app: &App) -> Option<Rect> {
    app.groups
        .iter()
        .enumerate()
        .filter(|(i, _)| app.group_in_current_desk(*i))
        .filter_map(|(i, g)| {
            if i == app.active_group {
                app.tab_bar_area
            } else {
                g.tab_bar_area
            }
        })
        .chain(app.tab_bar_area)
        .min_by_key(|r| (r.y, r.x))
}

pub(crate) fn controls(app: &App, area: Rect, group: usize) -> (Rect, Rect, u16) {
    if group != app.desks.host || area.width < 4 {
        return (Rect::default(), Rect::default(), 0);
    }
    let plus = Rect::new(area.x, area.y, 3, 1);
    let width = if app.desk_count() > 1 {
        (app.current_desk_name().width() as u16 + 3)
            .min(22)
            .min(area.width.saturating_sub(16))
    } else {
        0
    };
    let width = if app.desk_count() > 1 && width < 3 && area.width >= 8 {
        2
    } else {
        width
    };
    let pick = Rect::new(area.x + 3, area.y, width, 1);
    (plus, pick, 4 + width)
}

pub(crate) fn group_color(app: &App, color: u8) -> Color {
    let color = match color % 6 {
        0 => app.colors.ui.focus_border.over(app.colors.bg_tabbar).unwrap_or(app.colors.fg_accent),
        1 => app.colors.ui.chart_color(1, app.colors.bg_tabbar),
        2 => app.colors.ui.chart_color(0, app.colors.bg_tabbar),
        3 => app.colors.ui.chart_color(2, app.colors.bg_tabbar),
        4 => app.colors.ui.chart_color(3, app.colors.bg_tabbar),
        _ => app.colors.ui.chart_color(5, app.colors.bg_tabbar),
    };
    app.colors
        .ui
        .readable_foreground(color, app.colors.bg_tabbar, 4.5)
}

pub(crate) fn draw_controls(f: &mut Frame, app: &App, area: Rect, group: usize) {
    let (plus, pick, width) = controls(app, area, group);
    let bg = app.colors.bg_tabbar;
    let hovered = |r: Rect| r.contains((app.mouse_col, app.mouse_row).into());
    let style = |r| {
        app.colors
            .ui
            .toolbar_style(app.colors.ui.fg_tab_active, bg, hovered(r), false, false)
            .remove_modifier(Modifier::UNDERLINED)
    };
    f.render_widget(Paragraph::new(" + ").style(style(plus)), plus);
    if !pick.is_empty() {
        let label = if pick.width < 5 {
            "▾".into()
        } else {
            format!(
                "{} ▾",
                clipped_name(
                    app.current_desk_name(),
                    usize::from(pick.width.saturating_sub(2))
                )
            )
        };
        f.render_widget(Paragraph::new(label).style(style(pick)), pick);
    }
    if width > 0 {
        f.render_widget(
            Paragraph::new("│").style(Style::default().fg(app.colors.fg_dim).bg(bg)),
            Rect::new(area.x + width - 1, area.y, 1, 1),
        );
    }
}

fn entries(app: &App, label_width: usize) -> Vec<(String, Action)> {
    let popup = &app.desks.popup;
    let query = popup.text.to_lowercase();
    let desks = || {
        app.desks
            .items
            .iter()
            .filter(|d| !d.closed && d.name.to_lowercase().contains(&query))
    };
    match popup.mode.as_ref() {
        Some(Mode::Switch) => desks()
            .map(|d| {
                let panes = app.groups.iter().filter(|g| g.desk == d.id).count().max(1);
                let dirty = app
                    .groups
                    .iter()
                    .filter(|g| g.desk == d.id)
                    .flat_map(|g| &g.tabs)
                    .chain(app.tabs.iter().filter(|_| app.desks.active == d.id))
                    .any(|t| t.has_unsaved_changes());
                (
                    desk_label(
                        &d.name,
                        panes,
                        dirty,
                        d.id == app.desks.active,
                        label_width,
                    ),
                    Action::Switch(d.id),
                )
            })
            .chain([
                ("Rename desk…".into(), Action::Rename),
                (
                    if app.current_desk_is_empty() { "Close desk" } else { "Close desk…" }.into(),
                    Action::Close,
                ),
            ])
            .collect(),
        Some(Mode::New) => vec![
            ("Empty layout".into(), Action::Create(false)),
            ("Copy layout".into(), Action::Create(true)),
        ],
        Some(Mode::Rename) => vec![("Save name".into(), Action::SaveName)],
        Some(Mode::GroupNew) => vec![("Create group".into(), Action::MakeGroup)],
        Some(Mode::Group(id)) => {
            let group = app.desks.groups.get(id);
            let mut rows = vec![
                ("Save name".into(), Action::GroupRename(*id)),
                (
                    if group.is_some_and(|g| g.collapsed) {
                        "Expand group"
                    } else {
                        "Collapse group"
                    }
                    .into(),
                    Action::Collapse(*id),
                ),
            ];
            for (color, name) in ["Accent", "Blue", "Green", "Amber", "Purple", "Cyan"]
                .iter()
                .enumerate()
            {
                rows.push((
                    format!(
                        "{} {name}",
                        if group.is_some_and(|g| g.color == color as u8) {
                            "✓"
                        } else {
                            " "
                        }
                    ),
                    Action::Color(*id, color as u8),
                ));
            }
            rows.extend([
                ("Move group to desk…".into(), Action::MoveGroup(*id)),
                ("Ungroup tabs".into(), Action::Ungroup(*id)),
                (
                    "Close group (keep pinned tabs)".into(),
                    Action::CloseGroup(*id),
                ),
            ]);
            rows
        }
        Some(Mode::AddGroup) => {
            let mut groups: Vec<_> = app
                .desks
                .groups
                .values()
                .filter(|g| {
                    g.name.to_lowercase().contains(&query)
                        && app.tabs.iter().any(|t| {
                            app.desks.membership.get(&t.editor.navigation_id) == Some(&g.id)
                        })
                })
                .collect();
            groups.sort_by_cached_key(|g| (g.name.to_lowercase(), g.id));
            groups
                .into_iter()
                .map(|g| (g.name.clone(), Action::AddGroup(g.id)))
                .chain([("+ New group…".into(), Action::MakeGroup)])
                .collect()
        }
        Some(Mode::Move { .. }) => desks()
            .filter(|d| d.id != app.desks.active)
            .map(|d| (d.name.clone(), Action::Move(d.id)))
            .chain([("+ New desk…".into(), Action::MoveNew)])
            .collect(),
        Some(Mode::Close) => vec![
            ("Cancel".into(), Action::Cancel),
            ("Close desk".into(), Action::ConfirmClose),
        ],
        None => Vec::new(),
    }
}

fn activate(app: &mut App, action: Action) {
    let text = app.desks.popup.text.clone();
    match action {
        Action::Switch(id) => app.switch_desk(id),
        Action::Rename => open(app, Mode::Rename),
        Action::Close => open(app, Mode::Close),
        Action::Create(copy) => {
            app.new_desk(&text, copy);
        }
        Action::SaveName => {
            let id = app.desks.active;
            if let Some(desk) = app.desks.items.iter_mut().find(|d| d.id == id) {
                desk.name = crate::app_desks::clean_name(&text, &desk.name);
            }
            app.desks.popup = Default::default();
        }
        Action::MakeGroup => {
            if app.desks.popup.mode == Some(Mode::AddGroup) {
                open(app, Mode::GroupNew);
            } else {
                app.group_selected_tabs(&text, None);
            }
        }
        Action::AddGroup(id) => app.group_selected_tabs("", Some(id)),
        Action::GroupRename(id) => {
            if let Some(group) = app.desks.groups.get_mut(&id) {
                group.name = crate::app_desks::clean_name(&text, &group.name);
            }
            app.desks.popup = Default::default();
        }
        Action::Color(id, color) => {
            if let Some(group) = app.desks.groups.get_mut(&id) {
                group.color = color;
            }
            app.desks.popup = Default::default();
        }
        Action::Collapse(id) => {
            if let Some(group) = app.desks.groups.get_mut(&id) {
                group.collapsed = !group.collapsed;
            }
            app.desks.popup = Default::default();
        }
        Action::Ungroup(id) => {
            app.desks.membership.retain(|_, g| *g != id);
            app.desks.groups.remove(&id);
            app.desks.popup = Default::default();
        }
        Action::CloseGroup(id) => {
            let members: Vec<_> = app
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| app.desks.membership.get(&t.editor.navigation_id) == Some(&id))
                .map(|(i, _)| i)
                .collect();
            app.desks.popup = Default::default();
            app.close_tabs_matching(|i| members.contains(&i));
        }
        Action::MoveGroup(id) => {
            let ids = app
                .tabs
                .iter()
                .filter(|t| app.desks.membership.get(&t.editor.navigation_id) == Some(&id))
                .map(|t| t.editor.navigation_id)
                .collect();
            open(app, Mode::Move { ids, copy: false });
        }
        Action::Move(id) => {
            if let Some(Mode::Move { ids, copy }) = app.desks.popup.mode.clone() {
                app.move_tabs_to_desk(id, copy, &ids);
            }
        }
        Action::MoveNew => {
            if let Some(Mode::Move { ids, copy }) = app.desks.popup.mode.clone() {
                let old = app.desks.active;
                let target = app.new_desk(&text, false);
                app.switch_desk(old);
                app.move_tabs_to_desk(target, copy, &ids);
                app.switch_desk(target);
            }
        }
        Action::ConfirmClose => app.close_desk(),
        Action::Cancel => app.desks.popup = Default::default(),
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if crate::ui_modal::active(app).is_some() {
        app.desks.popup = Default::default();
        app.desks.group_drag = None;
        return false;
    }
    if key.code == KeyCode::Esc && app.desks.group_drag.take().is_some() {
        return true;
    }
    if app.desks.popup.mode.is_none() {
        return false;
    }
    let rows = entries(app, usize::MAX);
    match key.code {
        KeyCode::Esc => app.desks.popup = Default::default(),
        KeyCode::Up | KeyCode::BackTab => {
            app.desks.popup.selected = app.desks.popup.selected.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Tab => {
            app.desks.popup.selected =
                (app.desks.popup.selected + 1).min(rows.len().saturating_sub(1))
        }
        KeyCode::Enter => {
            if let Some((_, action)) = rows.get(app.desks.popup.selected) {
                activate(app, action.clone());
            }
        }
        _ => {
            let p = &mut app.desks.popup;
            let result = p.history.edit(
                crate::text_input::TextInput::new(&mut p.text, &mut p.cursor, &mut p.anchor),
                key.code,
                key.modifiers,
                None,
            );
            if result.changed {
                p.selected = 0;
                p.scroll = 0;
            }
        }
    }
    true
}

pub(crate) fn paste(app: &mut App, text: &str) -> bool {
    if app.desks.popup.mode.is_none() {
        return false;
    }
    let p = &mut app.desks.popup;
    p.history.edit(
        crate::text_input::TextInput::new(&mut p.text, &mut p.cursor, &mut p.anchor),
        KeyCode::Null,
        KeyModifiers::empty(),
        Some(text),
    );
    p.selected = 0;
    p.scroll = 0;
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if crate::ui_modal::active(app).is_some() {
        return false;
    }
    let pos = (mouse.column, mouse.row).into();
    if app.desks.group_drag.is_some() {
        app.mouse_col = mouse.column;
        app.mouse_row = mouse.row;
    }
    if app.desks.popup.mode.is_some() {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !app.desks.popup.area.contains(pos) {
                    app.desks.popup = Default::default();
                } else if let Some(byte) = app
                    .desks
                    .popup
                    .input_map
                    .as_ref()
                    .filter(|m| m.contains(mouse.column, mouse.row))
                    .map(|m| m.byte_at(mouse.column, mouse.row))
                {
                    let p = &mut app.desks.popup;
                    if !mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        p.anchor = None;
                    } else if p.anchor.is_none() {
                        p.anchor = Some(p.cursor);
                    }
                    p.cursor = byte;
                    p.input_drag = true;
                } else if let Some((_, action)) = app
                    .desks
                    .popup
                    .hits
                    .iter()
                    .find(|(r, _)| r.contains(pos))
                    .cloned()
                {
                    activate(app, action);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if app.desks.popup.input_drag => {
                let p = &mut app.desks.popup;
                if let Some(map) = &p.input_map {
                    p.anchor.get_or_insert(p.cursor);
                    p.cursor = map.byte_at(mouse.column, mouse.row);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => app.desks.popup.input_drag = false,
            MouseEventKind::ScrollDown => {
                app.desks.popup.selected =
                    (app.desks.popup.selected + 1).min(entries(app, usize::MAX).len().saturating_sub(1))
            }
            MouseEventKind::ScrollUp => {
                app.desks.popup.selected = app.desks.popup.selected.saturating_sub(1)
            }
            MouseEventKind::Moved => {
                if let Some(index) = app
                    .desks
                    .popup
                    .hits
                    .iter()
                    .position(|(r, _)| r.contains(pos))
                {
                    app.desks.popup.selected = app.desks.popup.scroll + index;
                }
            }
            _ => {}
        }
        return true;
    }
    let Some(area) = host_area(app) else {
        return false;
    };
    let (plus, pick, _) = controls(app, area, app.desks.host);
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && (plus.contains(pos) || pick.contains(pos))
    {
        open(
            app,
            if plus.contains(pos) {
                Mode::New
            } else {
                Mode::Switch
            },
        );
        return true;
    }
    let pane = app
        .groups
        .iter()
        .enumerate()
        .filter(|(i, _)| app.group_in_current_desk(*i))
        .find_map(|(i, g)| {
            let area = if i == app.active_group {
                app.tab_bar_area
            } else {
                g.tab_bar_area
            };
            area.filter(|a| a.contains(pos)).map(|a| (i, a))
        })
        .or_else(|| {
            app.tab_bar_area
                .filter(|a| a.contains(pos))
                .map(|a| (app.active_group, a))
        });
    if let Some((id, source, start, dragged)) = app.desks.group_drag {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                let moved =
                    dragged || mouse.column.abs_diff(start.0) + mouse.row.abs_diff(start.1) >= 2;
                app.desks.group_drag = Some((id, source, start, moved));
                return true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                app.desks.group_drag = None;
                if dragged {
                    if let Some((target, area)) = pane {
                        let hits = crate::ui_chrome_tabs::TabBarHits::for_group(app, area, target);
                        let tabs = if target == app.active_group {
                            &app.tabs
                        } else {
                            &app.groups[target].tabs
                        };
                        let before = hits
                            .tabs
                            .iter()
                            .find(|(_, r, _)| mouse.column < r.x + r.width / 2)
                            .map(|(i, _, _)| tabs[*i].editor.navigation_id);
                        app.move_named_group(id, target, before);
                    }
                } else if let Some(group) = app.desks.groups.get_mut(&id) {
                    group.collapsed = !group.collapsed;
                }
                return true;
            }
            _ => {}
        }
    }
    if let Some((pane, area)) = pane {
        let hits = crate::ui_chrome_tabs::TabBarHits::for_group(app, area, pane);
        if let Some(&(id, _)) = hits.groups.iter().find(|(_, r)| r.contains(pos)) {
            // An individual tab keeps its drag capture while crossing a group
            // header, even if its next event is a release over empty space.
            if mouse.kind == MouseEventKind::Drag(MouseButton::Left)
                && app.dragging_tab.is_some()
            {
                return false;
            }
            if mouse.kind == MouseEventKind::Up(MouseButton::Left)
                && let Some(index) = app.dragging_tab
            {
                let source = app.dragging_tab_source_group.unwrap_or(app.active_group);
                let tab = if source == app.active_group {
                    app.tabs.get(index)
                } else {
                    app.groups.get(source).and_then(|g| g.tabs.get(index))
                };
                if let Some(view) = tab.map(|t| t.editor.navigation_id) {
                    if source != pane {
                        app.move_tab_between_groups(source, index, pane);
                        if let Some(target) = app.groups.iter().enumerate().find_map(|(i, g)| {
                            g.tabs
                                .iter()
                                .any(|t| t.editor.navigation_id == view)
                                .then_some(i)
                        }) {
                            app.switch_group(target);
                        }
                    }
                    app.desks.membership.insert(view, id);
                    if let Some(tab) = app.tabs.iter_mut().find(|t| t.editor.navigation_id == view)
                    {
                        tab.preview = false;
                    }
                    app.normalize_tab_groups();
                    app.clear_drag_states();
                    return true;
                }
            }
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    app.desks.group_drag = Some((id, pane, (mouse.column, mouse.row), false))
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    app.switch_group(pane);
                    open(app, Mode::Group(id));
                }
                _ => {}
            }
            return true;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && mouse
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        {
            if let Some((index, _, _)) = hits.tab_at(mouse.column) {
                app.switch_group(pane);
                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    for i in app.active_tab.min(index)..=app.active_tab.max(index) {
                        app.desks
                            .selected_tabs
                            .insert(app.tabs[i].editor.navigation_id);
                    }
                } else {
                    if app.desks.selected_tabs.is_empty() {
                        if let Some(tab) = app.tabs.get(app.active_tab) {
                            app.desks.selected_tabs.insert(tab.editor.navigation_id);
                        }
                    }
                    let id = app.tabs[index].editor.navigation_id;
                    if !app.desks.selected_tabs.remove(&id) {
                        app.desks.selected_tabs.insert(id);
                    }
                }
                return true;
            }
        }
    }
    if mouse.kind == MouseEventKind::Up(MouseButton::Left)
        && app.dragging_tab.is_some()
        && !app.desks.membership.is_empty()
    {
        let view = app
            .dragging_tab
            .and_then(|i| app.tabs.get(i))
            .map(|t| t.editor.navigation_id);
        app.normalize_tab_groups();
        app.dragging_tab =
            view.and_then(|id| app.tabs.iter().position(|t| t.editor.navigation_id == id));
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && !mouse
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
    {
        app.desks.selected_tabs.clear();
    }
    false
}

pub(crate) fn clipped_name(name: &str, width: usize) -> String {
    if name.width() <= width {
        return name.into();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in name.graphemes(true) {
        let cells = grapheme.width();
        if used + cells > width.saturating_sub(1) {
            break;
        }
        result.push_str(grapheme);
        used += cells;
    }
    if width > 0 {
        result.push('…');
    }
    result
}

fn desk_label(name: &str, panes: usize, dirty: bool, active: bool, width: usize) -> String {
    let mut detail = format!(
        "  · {panes} pane{}{}",
        if panes == 1 { "" } else { "s" },
        if dirty { " · ●" } else { "" },
    );
    // Keep the count and unsaved marker visible before allocating space to the
    // name. Very narrow windows use a compact count instead of clipping it off.
    if detail.width() + 3 > width {
        detail = format!(" {panes}{}", if dirty { " ●" } else { "" });
    }
    if detail.width() + 2 > width {
        return panes.to_string();
    }
    let name_width = width.saturating_sub(2 + detail.width());
    format!(
        "{} {}{detail}",
        if active { "✓" } else { " " },
        clipped_name(name, name_width),
    )
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let Some(mode) = app.desks.popup.mode.clone() else {
        if app
            .desks
            .group_drag
            .is_some_and(|(_, _, _, dragged)| dragged)
        {
            let screen = f.area();
            if screen.contains((app.mouse_col, app.mouse_row).into()) {
                f.render_widget(
                    Paragraph::new("│").style(
                        Style::default()
                            .fg(app.colors.fg_accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Rect::new(app.mouse_col, app.mouse_row, 1, 1),
                );
            }
        } else {
            draw_control_hint(f, app);
        }
        return;
    };
    let title = match mode {
        Mode::Switch => "Desks",
        Mode::New => "New Desk",
        Mode::Rename => "Rename Desk",
        Mode::GroupNew => "New Tab Group",
        Mode::Group(_) => "Tab Group",
        Mode::AddGroup => "Add to Group",
        Mode::Move { copy: true, .. } => "Also Open in Desk",
        Mode::Move { .. } => "Move to Desk",
        Mode::Close => "Close Desk?",
    };
    let placeholder = match mode {
        Mode::New | Mode::Rename => "Desk name…",
        Mode::GroupNew | Mode::Group(_) => "Group name…",
        Mode::Close => "Files and processes stay open.",
        _ => "Type to filter…",
    };
    let screen = f.area();
    // One cell of padding on each side, plus the popup's two borders.
    let rows = entries(app, usize::from(screen.width.min(58).saturating_sub(4)));
    let content_width = rows
        .iter()
        .map(|(label, _)| label.width())
        .chain([title.width(), placeholder.width()])
        .max()
        .unwrap_or(0);
    let width = screen.width.min((content_width + 4).clamp(28, 58) as u16);
    let height = screen.height.min((rows.len() + 3).clamp(4, 18) as u16);
    let p = &mut app.desks.popup;
    let area = Rect::new(
        p.origin.0.min(screen.right().saturating_sub(width)),
        p.origin.1.min(screen.bottom().saturating_sub(height)),
        width,
        height,
    );
    p.area = area;
    p.hits.clear();
    p.input = Rect::default();
    p.input_map = None;
    if area.width < 4 || area.height < 4 {
        return;
    }
    let surface = crate::ui_widgets::surface_background(f, area, app.colors.bg_editor);
    let bg = app.colors.ui.quick_background.over(surface).unwrap_or(surface);
    let fg = app.colors.ui.quick_text_color(bg, false);
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .style(Style::default().fg(fg).bg(bg))
            .border_style(Style::default().fg(app.colors.ui.widget_border.border(bg).unwrap_or(bg))),
        area,
    );
    let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
    p.input = Rect::new(inner.x, inner.y, inner.width, 1);
    if mode == Mode::Close {
        f.render_widget(
            Paragraph::new(placeholder).style(Style::default().fg(app.colors.ui.description_color(bg)).bg(bg)),
            p.input,
        );
        p.input_map = None;
    } else {
        p.input_map = Some(
            crate::ui_widgets::InputField::new(&p.text)
                .theme_colors(&app.colors)
                .prefix(" ", fg)
                .placeholder_text(placeholder)
                .cursor_pos(p.cursor)
                .selection_range(App::inline_selection_range(p.cursor, p.anchor))
                .render(f, p.input),
        );
    }
    let visible = usize::from(inner.height.saturating_sub(1));
    p.selected = p.selected.min(rows.len().saturating_sub(1));
    if p.selected < p.scroll {
        p.scroll = p.selected;
    } else if p.selected >= p.scroll + visible {
        p.scroll = (p.selected + 1).saturating_sub(visible);
    }
    for (i, (label, action)) in rows.into_iter().enumerate().skip(p.scroll).take(visible) {
        let rect = Rect::new(inner.x, inner.y + 1 + (i - p.scroll) as u16, inner.width, 1);
        let selected = i == p.selected;
        let row_bg = if selected { app.colors.ui.quick_focus_background.over(bg).unwrap_or(bg) } else { bg };
        let style = Style::default().fg(app.colors.ui.quick_text_color(row_bg, selected)).bg(row_bg);
        f.render_widget(Paragraph::new(format!(" {}", label)).style(style), rect);
        p.hits.push((rect, action));
    }
}

pub(crate) fn command(app: &mut App, id: &str) -> bool {
    match id {
        "desk_new" => open(app, Mode::New),
        "desk_switch" => open(app, Mode::Switch),
        "desk_rename" => open(app, Mode::Rename),
        "desk_previous" => {
            if let Some(id) = app.desks.previous {
                app.switch_desk(id);
            }
        }
        "desk_duplicate" => {
            let name = format!("{} copy", app.current_desk_name());
            app.new_desk(&name, true);
        }
        "desk_close" => open(app, Mode::Close),
        "desk_reopen" => app.reopen_desk(),
        "tab_group_remove" => {
            let ids = app
                .tabs
                .iter()
                .filter(|t| app.desks.selected_tabs.contains(&t.editor.navigation_id))
                .map(|t| t.editor.navigation_id)
                .collect::<Vec<_>>();
            if ids.is_empty() {
                if let Some(tab) = app.tabs.get(app.active_tab) {
                    app.desks.membership.remove(&tab.editor.navigation_id);
                }
            } else {
                for id in ids {
                    app.desks.membership.remove(&id);
                }
            }
            app.normalize_tab_groups();
            app.desks.selected_tabs.clear();
        }
        "tab_group_new" => open(app, Mode::GroupNew),
        "tab_group_add" => open(app, Mode::AddGroup),
        "tab_group_edit" => {
            if let Some(id) = app
                .tabs
                .get(app.active_tab)
                .and_then(|t| app.tab_group(t))
                .map(|g| g.id)
            {
                open(app, Mode::Group(id));
            }
        }
        "tab_move_desk" | "tab_copy_desk" => {
            let ids = app
                .tabs
                .iter()
                .filter(|t| app.desks.selected_tabs.contains(&t.editor.navigation_id))
                .map(|t| t.editor.navigation_id)
                .collect::<Vec<_>>();
            let ids = if ids.is_empty() {
                app.tabs
                    .get(app.active_tab)
                    .map(|t| vec![t.editor.navigation_id])
                    .unwrap_or_default()
            } else {
                ids
            };
            open(
                app,
                Mode::Move {
                    ids,
                    copy: id == "tab_copy_desk",
                },
            );
        }
        _ => return false,
    }
    true
}

fn draw_control_hint(f: &mut Frame, app: &App) {
    if crate::ui_modal::active(app).is_some()
        || app.palette.active
        || app.tab_context_active
        || app.desks.group_drag.is_some()
    {
        return;
    }
    let Some(area) = host_area(app) else {
        return;
    };
    let (plus, pick, _) = controls(app, area, app.desks.host);
    let pos = (app.mouse_col, app.mouse_row).into();
    let label = if plus.contains(pos) {
        "New Desk"
    } else if pick.contains(pos) {
        "Switch Desk"
    } else {
        return;
    };
    let screen = f.area();
    let width = (label.width() as u16 + 2).min(screen.width);
    let rect = Rect::new(
        area.x.min(screen.right().saturating_sub(width)),
        area.bottom().min(screen.bottom().saturating_sub(1)),
        width,
        1,
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(format!(" {label} ")).style(
            Style::default()
                .fg(app.colors.fg_text)
                .bg(app.colors.bg_dark),
        ),
        rect,
    );
}
