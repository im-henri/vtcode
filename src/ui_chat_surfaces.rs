//! Paint each conversation in its placed pane, retaining per-session UI state.
use super::*;
use crate::app::TabKind;
use crate::app_acp::layout::Surface;

pub(crate) fn queue_editor(app: &mut App, key: u64, group: usize, area: Rect) {
    app.code_area = None;
    app.gutter_area = None;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    app.chat.layout.placements.push((key, group, area));
}

pub(crate) fn draw_surfaces(f: &mut Frame, app: &mut App, dock: Option<Rect>) {
    let mut placements: Vec<_> = std::mem::take(&mut app.chat.layout.placements)
        .into_iter()
        .filter_map(|(key, group, area)| {
            Some((
                Some(key),
                app.chat.layout.editors.get(&key)?.agent,
                group,
                area,
                Some(app.chat.layout.editors.get(&key)?.session.clone()),
            ))
        })
        .collect();
    if let Some(area) = dock {
        let agent = app.chat.layout.dock_agent;
        let id = app.chat.agent_state(agent).and_then(|state| {
            state
                .dock_selected
                .clone()
                .filter(|id| state.open_tabs.contains(id))
                .or_else(|| {
                    state
                        .selected
                        .clone()
                        .filter(|id| state.open_tabs.contains(id))
                })
                .or_else(|| state.open_tabs.last().cloned())
        });
        placements.push((None, agent, app.active_group, area, id));
    }
    if app.active_pane == ActivePane::Editor
        && !app.terminal_panel_focused
        && let Some(TabKind::Chat(key)) = app.tabs.get(app.active_tab).map(|tab| tab.kind)
        && let Some(view) = app.chat.layout.editors.get(&key).cloned()
    {
        app.chat.switch_connection(view.agent);
        if app.chat.layout.active != Some(key) || app.chat.selected.as_ref() != Some(&view.session)
        {
            app.chat.select_tab(Some(view.session.clone()));
        }
        app.chat.layout.active = Some(key);
        if app.terminal_has_focus {
            app.chat.acknowledge_selected_activity();
        }
        app.chat.visible = true;
        app.active_pane = ActivePane::Chat;
    }
    let selected = app.chat.selected.clone();
    let agent = app.chat.active_agent;
    let active = app.chat.layout.active;
    placements.sort_by_key(|(key, ..)| *key == active);
    let active_visible = placements.iter().any(|(key, ..)| *key == active);
    app.chat.layout.surfaces.clear();
    for (key, owner, group, area, id) in placements {
        if !app.chat.switch_connection(owner) {
            continue;
        }
        app.chat.select_view(id.clone());
        let pane = app.active_pane;
        let focus = app.chat.focus.take();
        let picker = app.chat.picker.take();
        if key == active {
            app.chat.focus = focus.clone();
            app.chat.picker = picker.clone();
        } else {
            app.active_pane = ActivePane::Editor;
        }
        app.chat.layout.rendering = key;
        app.poll_chat_terminals();
        draw(f, app, area);
        app.chat.layout.surfaces.push(Surface {
            agent: owner,
            key,
            group,
            session: id,
            area,
            hits: app.chat.hits.clone(),
            scrollbars: app.chat.scrollbars.clone(),
            transcript: app.chat.transcript_area,
            terminals: app.chat.terminal_areas.clone(),
            tabs: app.chat.tab_area,
            agents: app.chat.agent_tab_area,
        });
        app.active_pane = pane;
        app.chat.focus = focus;
        app.chat.picker = picker;
    }
    app.chat.switch_connection(agent);
    app.chat.layout.rendering = active;
    if !active_visible {
        app.chat.select_view(selected);
        app.chat.area = None;
        app.chat.hits.clear();
        app.chat.scrollbars.clear();
    }
}
