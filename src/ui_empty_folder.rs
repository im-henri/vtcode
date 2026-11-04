use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Paragraph},
};

use crate::app::{ActivePane, App, SidebarView};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Action {
    #[default]
    OpenFolder,
    SavedWorkspaces,
}

#[derive(Default)]
pub(crate) struct State {
    focused: Action,
}

struct Layout {
    title: Rect,
    description: Rect,
    lines: Vec<String>,
    open: Rect,
    saved: Option<Rect>,
    note: Rect,
    note_lines: Vec<String>,
}

fn layout(app: &App, area: Rect) -> Option<Layout> {
    let body = crate::ui_chrome::empty_folder_state_area(app, area)?;
    if body.width < 4 || body.height == 0 {
        return None;
    }
    let inset = if body.width >= 24 { 2 } else { 1 };
    let top = u16::from(body.height >= 12);
    let inner = Rect::new(
        body.x + inset,
        body.y + top,
        body.width - inset * 2,
        body.height - top,
    );
    let tall_buttons = inner.height >= 18 && inner.width >= 14;
    let button_height = if tall_buttons { 3 } else { 1 };
    let title_height = u16::from(inner.height >= 3 && inner.width >= 14);
    let secondary = inner.height >= 11 && inner.width >= 7;
    let gap = u16::from(inner.height >= 6);
    let reserve =
        title_height + gap + button_height + if secondary { 1 + button_height } else { 0 };
    let max_text = inner.height.saturating_sub(reserve + gap);
    let description = match app.sidebar_view {
        SidebarView::SourceControl => {
            "Open a folder containing a Git repository to view changes and history."
        }
        SidebarView::Search => "Open a folder to search across your files.",
        _ => "Open a folder to browse its files and start working.",
    };
    let mut lines = if inner.height >= 7 {
        crate::ui_commit_details::wrapped(description, usize::from(inner.width))
    } else {
        Vec::new()
    };
    if lines.len() > usize::from(max_text) {
        lines = crate::ui_commit_details::wrapped(
            "Open a folder to get started.",
            usize::from(inner.width),
        );
        if lines.len() > usize::from(max_text) {
            lines.clear();
        }
    }
    let title = Rect::new(inner.x, inner.y, inner.width, title_height);
    let description = Rect::new(
        inner.x,
        title.bottom() + gap,
        inner.width,
        lines.len() as u16,
    );
    let open_y = description.bottom() + u16::from(!lines.is_empty());
    let open = Rect::new(
        inner.x,
        open_y,
        inner.width,
        button_height.min(inner.bottom().saturating_sub(open_y)),
    );
    if open.is_empty() {
        return None;
    }
    let saved = secondary
        .then(|| Rect::new(inner.x, open.bottom() + 1, inner.width, button_height))
        .filter(|button| button.bottom() <= inner.bottom());
    let note_y = saved
        .unwrap_or(open)
        .bottom()
        .saturating_add(2)
        .min(inner.bottom());
    let keeps_editors = app
        .tabs
        .iter()
        .chain(app.groups.iter().flat_map(|group| &group.tabs))
        .any(|tab| tab.kind == crate::app::TabKind::File)
        || app
            .terminals
            .iter()
            .enumerate()
            .any(|(index, _)| app.terminal_is_open(index));
    let note_lines = if keeps_editors {
        crate::ui_commit_details::wrapped(
            "Your open files and terminals stay open.",
            usize::from(inner.width),
        )
    } else {
        Vec::new()
    };
    let note_lines = if note_lines.len() <= usize::from(inner.bottom() - note_y) {
        note_lines
    } else {
        Vec::new()
    };
    let note = Rect::new(inner.x, note_y, inner.width, note_lines.len() as u16);
    Some(Layout {
        title,
        description,
        lines,
        open,
        saved,
        note,
        note_lines,
    })
}

#[cfg(test)]
pub(crate) fn open_area(app: &App, area: Rect) -> Option<Rect> {
    layout(app, area).map(|layout| layout.open)
}

pub(crate) fn saved_area(app: &App, area: Rect) -> Option<Rect> {
    layout(app, area)?.saved
}

pub(crate) fn button_at(app: &App, col: u16, row: u16) -> Option<Action> {
    if !app.sidebar_is_visible() {
        return None;
    }
    let layout = layout(app, app.explorer_area?)?;
    let point = (col, row).into();
    if layout.open.contains(point) {
        Some(Action::OpenFolder)
    } else if layout.saved.is_some_and(|area| area.contains(point)) {
        Some(Action::SavedWorkspaces)
    } else {
        None
    }
}

fn activate(app: &mut App, action: Action) {
    app.empty_folder.focused = action;
    match action {
        Action::OpenFolder => crate::ui_workspaces::open_folder(app),
        Action::SavedWorkspaces => crate::ui_workspace_sets::open(app),
    }
}

pub(crate) fn click(app: &mut App, col: u16, row: u16) -> bool {
    let Some(action) = button_at(app, col, row) else {
        return false;
    };
    app.active_pane = ActivePane::Explorer;
    activate(app, action);
    true
}

pub(crate) fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let saved_visible = app
        .explorer_area
        .and_then(|area| saved_area(app, area))
        .is_some();
    if !saved_visible {
        app.empty_folder.focused = Action::OpenFolder;
    }
    match code {
        KeyCode::Enter | KeyCode::Char(' ') if modifiers.is_empty() => {
            let action = app.empty_folder.focused;
            activate(app, action);
        }
        KeyCode::Tab | KeyCode::BackTab
            if !modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            app.empty_folder.focused =
                if saved_visible && app.empty_folder.focused == Action::OpenFolder {
                    Action::SavedWorkspaces
                } else {
                    Action::OpenFolder
                };
        }
        KeyCode::Esc => {
            app.global_search_active = false;
            app.active_pane = ActivePane::Editor;
        }
        _ => {}
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(layout) = layout(app, area) else {
        return;
    };
    if layout.saved.is_none() {
        app.empty_folder.focused = Action::OpenFolder;
    }
    let bg = app.colors.bg_sidebar;
    let text = Style::default()
        .bg(bg)
        .fg(app.colors.ui.sidebar_text_color(bg));
    let description = text.fg(app.colors.ui.description_color(bg));
    f.render_widget(
        Paragraph::new("No folder open").style(text.add_modifier(Modifier::BOLD)),
        layout.title,
    );
    f.render_widget(
        Paragraph::new(layout.lines.into_iter().map(Line::raw).collect::<Vec<_>>())
            .style(description),
        layout.description,
    );
    for (area, action) in std::iter::once((layout.open, Action::OpenFolder))
        .chain(layout.saved.map(|area| (area, Action::SavedWorkspaces)))
    {
        let focused = app.active_pane == ActivePane::Explorer && app.empty_folder.focused == action;
        let hovered = area.contains((app.mouse_col, app.mouse_row).into());
        // Single-row controls encode theme borders as underlines. Keep the
        // padded button's entire fill free of those per-row decorations.
        let style = app
            .colors
            .ui
            .button_style(bg, action == Action::OpenFolder, hovered || focused, true)
            .remove_modifier(Modifier::UNDERLINED);
        f.render_widget(Block::default().style(style), area);
        let label = match action {
            Action::OpenFolder if area.width >= 14 => "Open folder…",
            Action::OpenFolder if area.width >= 7 => "Open…",
            Action::OpenFolder => "+",
            Action::SavedWorkspaces if area.width >= 19 => "Saved workspaces…",
            Action::SavedWorkspaces if area.width >= 13 => "Workspaces…",
            Action::SavedWorkspaces => "Saved…",
        };
        let style = if focused {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        };
        let label_area = Rect::new(area.x, area.y + area.height / 2, area.width, 1);
        f.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style),
            label_area,
        );
    }
    f.render_widget(
        Paragraph::new(
            layout
                .note_lines
                .into_iter()
                .map(Line::raw)
                .collect::<Vec<_>>(),
        )
        .style(description),
        layout.note,
    );
}
