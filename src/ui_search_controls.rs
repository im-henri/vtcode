use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    text::Line,
    widgets::{Clear, Paragraph},
};

use crate::app::{ActivePane, App};
use crate::ui_widgets::Tooltip;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchScope {
    File,
    Workspace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchControl {
    ToggleReplace,
    MatchCase,
    WholeWord,
    Regex,
    InSelection,
    PreserveCase,
    Previous,
    Next,
    Replace,
    ReplaceAll,
    Close,
    Refresh,
    Stop,
    Clear,
    CollapseResults,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchControlRegion {
    pub scope: SearchScope,
    pub control: SearchControl,
    pub area: Rect,
    pub disabled: bool,
}

pub(crate) fn control_at(
    app: &App,
    scope: SearchScope,
    col: u16,
    row: u16,
) -> Option<SearchControlRegion> {
    app.search_control_regions
        .iter()
        .rev()
        .copied()
        .find(|button| button.scope == scope && button.area.contains((col, row).into()))
}

pub(crate) fn draw_button(
    f: &mut Frame,
    app: &mut App,
    scope: SearchScope,
    control: SearchControl,
    area: Rect,
    label: &str,
    active: bool,
    disabled: bool,
) {
    let area = area.intersection(f.area());
    if area.is_empty() {
        return;
    }
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let fallback = if scope == SearchScope::File {
        app.colors.ui.widget_background_color(app.colors.bg_editor)
    } else { app.colors.bg_sidebar };
    let bg = f.buffer_mut().cell((area.x, area.y)).map(|cell| cell.bg)
        .filter(|color| *color != ratatui::style::Color::Reset).unwrap_or(fallback);
    let fg = if scope == SearchScope::File {
        app.colors.ui.widget_text_color(bg)
    } else { app.colors.ui.sidebar_text_color(bg) };
    let mut style = app.colors.ui.input_option_style(fg, bg, active, hovered);
    if disabled {
        let background = style.bg.unwrap_or(bg);
        style = style.fg(crate::ui_colors::disabled_text_color(
            app.colors.ui.disabled_foreground.over(background).unwrap_or(app.colors.fg_dim),
            style.fg.unwrap_or(fg), background, app.colors.ui.contrast_colors()));
    }
    // Centering the label leaves padding cells untouched, including any
    // border or text already drawn behind this button.
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(label)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
    app.search_control_regions.push(SearchControlRegion {
        scope,
        control,
        area,
        disabled,
    });
}

pub(crate) fn draw_tooltip(f: &mut Frame, app: &App, scope: SearchScope) {
    let Some(button) = control_at(app, scope, app.mouse_col, app.mouse_row) else {
        return;
    };
    let text = match button.control {
        SearchControl::ToggleReplace => {
            let expanded = match scope {
                SearchScope::File => app.replace_mode,
                SearchScope::Workspace => app.global_search_replace_visible,
            };
            if expanded {
                "Hide Replace"
            } else {
                "Show Replace"
            }
        }
        SearchControl::MatchCase => "Match Case (Alt+C)",
        SearchControl::WholeWord => "Match Whole Word (Alt+W)",
        SearchControl::Regex => "Use Regular Expression (Alt+R)",
        SearchControl::InSelection => "Find in Selection (Alt+L)",
        SearchControl::PreserveCase => "Preserve Case (Alt+P)",
        SearchControl::Previous => "Previous Match (Shift+Enter)",
        SearchControl::Next => "Next Match (Enter)",
        SearchControl::Replace => "Replace (Enter)",
        SearchControl::ReplaceAll => "Replace All (Ctrl+Alt+Enter)",
        SearchControl::Close => "Close (Escape)",
        SearchControl::Refresh => "Refresh Search",
        SearchControl::Stop => "Stop Search (Escape)",
        SearchControl::Clear => "Clear Search Results",
        SearchControl::CollapseResults => {
            if app.global_search_all_files_toggle_label() == "⊞" {
                "Expand All Results"
            } else {
                "Collapse All Results"
            }
        }
    };
    // Find tooltips stay below the entire widget instead of obscuring its
    // inputs. Sidebar tooltips are rendered after the workbench's panes.
    let anchor_y = if scope == SearchScope::File {
        app.search_bar_area
            .map_or(button.area.y, |area| area.bottom().saturating_sub(1))
    } else {
        button.area.y
    };
    Tooltip::hover(vec![Line::raw(text)], &app.colors)
    .prefer_below()
    .render(f, button.area.x, anchor_y);
}

pub(crate) fn handle_click(app: &mut App, scope: SearchScope, col: u16, row: u16) -> bool {
    let Some(button) = control_at(app, scope, col, row) else {
        return false;
    };
    if button.disabled {
        return true;
    }
    if scope == SearchScope::Workspace {
        app.search_input_focused = false;
        app.global_search_active = true;
        app.active_pane = ActivePane::Explorer;
    } else {
        app.search_input_focused = true;
        app.active_pane = ActivePane::Editor;
    }
    activate(app, scope, button.control);
    true
}

pub(crate) fn activate(app: &mut App, scope: SearchScope, control: SearchControl) {
    match control {
        SearchControl::ToggleReplace => match scope {
            SearchScope::File => {
                app.replace_mode = !app.replace_mode;
                app.search_focus_replace = app.replace_mode;
            }
            SearchScope::Workspace => {
                app.global_search_replace_visible = !app.global_search_replace_visible;
                app.search_focus_field = if app.global_search_replace_visible {
                    1
                } else {
                    0
                };
            }
        },
        SearchControl::MatchCase
        | SearchControl::WholeWord
        | SearchControl::Regex
        | SearchControl::InSelection => {
            match control {
                SearchControl::MatchCase => app.search_case_sensitive = !app.search_case_sensitive,
                SearchControl::WholeWord => app.search_whole_word = !app.search_whole_word,
                SearchControl::Regex => app.search_regex = !app.search_regex,
                SearchControl::InSelection => {
                    app.search_in_selection = !app.search_in_selection;
                    app.search_selection_range = if app.search_in_selection {
                        app.editor().and_then(|editor| editor.selection_range())
                    } else {
                        None
                    };
                }
                _ => {}
            }
            if scope == SearchScope::File || app.search_active {
                app.update_search();
            }
            if scope == SearchScope::Workspace
                || (control != SearchControl::InSelection
                    && app.show_explorer
                    && app.sidebar_view == crate::app::SidebarView::Search
                    && !app.global_search_query.is_empty())
            {
                app.update_global_search();
            }
        }
        SearchControl::PreserveCase => app.search_preserve_case = !app.search_preserve_case,
        SearchControl::Previous | SearchControl::Next => {
            app.search_focus_replace = false;
            crate::input::handle_search_key(
                app,
                KeyCode::Enter,
                if control == SearchControl::Previous {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::NONE
                },
            );
        }
        SearchControl::Replace => {
            app.search_focus_replace = true;
            crate::input::handle_search_key(app, KeyCode::Enter, KeyModifiers::NONE);
        }
        SearchControl::ReplaceAll => match scope {
            SearchScope::File => crate::input::handle_search_key(
                app,
                KeyCode::Enter,
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
            SearchScope::Workspace => app.replace_all_in_files(),
        },
        SearchControl::Close => {
            crate::input::handle_search_key(app, KeyCode::Esc, KeyModifiers::NONE)
        }
        SearchControl::Refresh => app.update_global_search(),
        SearchControl::Stop => app.stop_global_search(),
        SearchControl::Clear => app.clear_global_search(),
        SearchControl::CollapseResults => app.toggle_global_search_all_files_collapsed(),
    }
    if matches!(control, SearchControl::MatchCase | SearchControl::WholeWord
        | SearchControl::Regex | SearchControl::PreserveCase)
    {
        app.flush_live_settings();
    }
}

#[cfg(test)]
#[path = "search_controls_tests.rs"]
mod tests;
