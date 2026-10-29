use super::*;
use crate::app::GitFileChange;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileAction {
    Open,
    Discard,
    Stage,
    Unstage,
}

impl FileAction {
    fn for_file(staged: bool) -> &'static [Self] {
        if staged {
            &[Self::Open, Self::Unstage]
        } else {
            &[Self::Open, Self::Discard, Self::Stage]
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Open => "Open File",
            Self::Discard => "Discard Changes",
            Self::Stage => "Stage Changes",
            Self::Unstage => "Unstage Changes",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Open => "↗",
            Self::Discard => "↶",
            Self::Stage => "+",
            Self::Unstage => "−",
        }
    }

    fn disabled(self, app: &App) -> bool {
        self != Self::Open && (app.bg_git_action_running || app.bg_git_remote_running)
    }
}

pub(crate) struct FileActionRow {
    pub index: usize,
    pub change: GitFileChange,
    pub repository: Arc<PathBuf>,
    pub area: Rect,
    pub background: ratatui::style::Color,
}

impl FileActionRow {
    pub(crate) fn width(staged: bool, available: u16) -> u16 {
        let count = FileAction::for_file(staged).len() as u16;
        if available >= count * 3 {
            count * 3
        } else if available >= count {
            count
        } else {
            0
        }
    }

    pub(crate) fn buttons(&self) -> impl Iterator<Item = (FileAction, Rect)> + '_ {
        let actions = FileAction::for_file(self.change.staged);
        let width = self.area.width / actions.len() as u16;
        actions.iter().enumerate().map(move |(index, &action)| {
            (
                action,
                Rect::new(self.area.x + index as u16 * width, self.area.y, width, 1),
            )
        })
    }
}

fn hit(app: &App, col: u16, row: u16) -> Option<(usize, FileAction, Rect)> {
    if !app.source_control_enabled {
        return None;
    }
    app.source_control_file_actions.iter().find_map(|actions| {
        // Status refreshes and repository switches must not retarget an old button.
        let current = app.source_control_files.get(actions.index)?;
        if current.path != actions.change.path
            || current.directory != actions.change.directory
            || current.staged != actions.change.staged
            || app.source_control_root() != actions.repository.as_path()
            || !app.source_control_file_visible(actions.index)
        {
            return None;
        }
        actions
            .buttons()
            .find(|(_, area)| area.contains((col, row).into()))
            .map(|(action, area)| (actions.index, action, area))
    })
}

pub(crate) fn draw(f: &mut Frame, app: &App) {
    for row in &app.source_control_file_actions {
        for (action, area) in row.buttons() {
            let disabled = action.disabled(app);
            let hovered = !disabled && area.contains((app.mouse_col, app.mouse_row).into());
            let mut style = app.colors.ui.toolbar_layer_style(
                app.colors.ui.sidebar_foreground,
                row.background,
                hovered,
                false,
                false,
            );
            if disabled {
                style = style.fg(app
                    .colors
                    .ui
                    .disabled_foreground
                    .over(row.background)
                    .unwrap_or(app.colors.fg_dim));
            }
            f.render_widget(
                Paragraph::new(action.icon())
                    .alignment(Alignment::Center)
                    .style(style),
                area,
            );
        }
    }
}

pub(crate) fn draw_tooltip(f: &mut Frame, app: &App) {
    if let Some((index, action, area)) = hit(app, app.mouse_col, app.mouse_row) {
        let label = if action == FileAction::Open && app.source_control_files[index].directory {
            "Reveal in Explorer"
        } else { action.label() };
        crate::ui_widgets::Tooltip::hover(vec![Line::raw(label)], &app.colors)
            .prefer_below()
            .render(f, area.x, area.y);
    }
}

pub(crate) fn handle_click(
    app: &mut App,
    col: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    use crossterm::event::KeyModifiers;
    let Some((index, action, _)) = hit(app, col, row) else {
        return false;
    };
    app.sc_graph_focused = false;
    app.source_control_input_mode = false;
    let ctrl = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    if ctrl || shift {
        app.select_source_control_file(index, ctrl, shift);
        return true;
    }
    if action.disabled(app) {
        return true;
    }
    app.preserve_source_control_selection(index);
    match action {
        FileAction::Open => {
            let change = app.source_control_files[index].clone();
            app.open_source_control_entry(&change);
        }
        FileAction::Discard => {
            app.request_source_control_discard(app.selected_source_control_changes(), false)
        }
        FileAction::Stage => app.git_stage_changes(app.selected_source_control_changes()),
        FileAction::Unstage => app.git_unstage_changes(app.selected_source_control_changes()),
    }
    true
}
