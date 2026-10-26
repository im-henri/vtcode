//! Shared workbench search surface. Rendering owns the pointer hit regions.
use crate::{app::App, text_input::TextInputId, ui_widgets::InputField};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    OpenWith,
    Commands,
    Files,
    Symbols,
    Tasks,
    Branches,
}

impl Kind {
    pub(crate) fn input(self) -> Option<TextInputId> {
        match self {
            Self::OpenWith => None,
            Self::Commands => Some(TextInputId::CommandPalette),
            Self::Files => Some(TextInputId::QuickOpen),
            Self::Symbols => Some(TextInputId::SymbolList),
            Self::Tasks => Some(TextInputId::TaskPicker),
            Self::Branches => Some(TextInputId::GitBranch),
        }
    }
}

#[derive(Default)]
pub(crate) struct PickerState {
    kind: Option<Kind>,
    pub area: Rect,
    pub close: Rect,
    pub list: Rect,
    pub rows: Vec<(Rect, usize)>,
    scroll: usize,
    last_selection: Option<usize>,
    last_query: String,
}

pub(crate) fn visible_rows(app: &App, kind: Kind) -> usize {
    if app.quick_picker.kind == Some(kind) && app.quick_picker.list.height > 0 {
        app.quick_picker.list.height as usize
    } else {
        crate::ui_pickers::PALETTE_MAX_ITEMS as usize
    }
}

pub(crate) fn begin_frame(app: &mut App) {
    if active(app).is_none() {
        app.quick_picker = PickerState::default();
    }
}

pub(crate) fn draw_active(f: &mut Frame, app: &mut App) {
    match active(app) {
        Some(Kind::Commands) => crate::ui_pickers::draw_command_palette(f, app),
        Some(Kind::Files) => crate::ui_search_dialog::draw_quick_open(f, app),
        Some(Kind::Symbols) => crate::ui_pickers::draw_symbol_list(f, app),
        Some(Kind::Tasks) => crate::ui_pickers::draw_task_picker(f, app),
        Some(Kind::Branches) => crate::ui_pickers::draw_git_branch_picker(f, app),
        Some(Kind::OpenWith) => crate::ui_dialogs::draw_binary_open_prompt(f, app),
        None => {}
    }
}

struct Item {
    name: String,
    detail: String,
    badge: String,
    icon: String,
    color: Color,
}

pub(crate) fn active(app: &App) -> Option<Kind> {
    if app.change_dir_active || crate::ui_modal::active(app).is_some() {
        return None;
    }
    if app.binary_open_prompt_active {
        Some(Kind::OpenWith)
    } else if app.palette.active && !app.theme_install_prompt_visible() {
        Some(Kind::Commands)
    } else if app.quick_open_active {
        Some(Kind::Files)
    } else if app.git_branch_picker_active {
        Some(Kind::Branches)
    } else if app.symbol_list_active {
        Some(Kind::Symbols)
    } else if app.task_picker_active {
        Some(Kind::Tasks)
    } else {
        None
    }
}

fn selected(app: &App, kind: Kind) -> usize {
    match kind {
        Kind::OpenWith => app.binary_open_prompt_selected,
        Kind::Commands => app.palette.selected,
        Kind::Files => app.quick_open_selected,
        Kind::Symbols => app.symbol_list_selected,
        Kind::Tasks => app.task_picker_selected,
        Kind::Branches => app.git_branch_picker_selected,
    }
}

fn select(app: &mut App, kind: Kind, index: usize) {
    match kind {
        Kind::OpenWith => app.binary_open_prompt_selected = index,
        Kind::Commands => {
            app.palette.selected = index;
            app.preview_palette_theme();
        }
        Kind::Files => app.quick_open_selected = index,
        Kind::Symbols => app.symbol_list_selected = index,
        Kind::Tasks => app.task_picker_selected = index,
        Kind::Branches => app.git_branch_picker_selected = index,
    }
}

fn dispatch(app: &mut App, kind: Kind, key: KeyCode) {
    match kind {
        Kind::OpenWith => {
            crate::input_keyboard_dialogs::handle_binary_open_prompt_key(
                app,
                KeyEvent::new(key, KeyModifiers::NONE),
            );
        }
        Kind::Commands => {
            crate::input_pickers::handle_palette_key(app, key, KeyModifiers::NONE);
        }
        Kind::Files => crate::input_pickers::handle_quick_open_key(app, key, KeyModifiers::NONE),
        Kind::Symbols => crate::input_pickers::handle_symbol_list_key(app, key, KeyModifiers::NONE),
        Kind::Branches => {
            crate::input_pickers::handle_git_branch_picker_key(app, key, KeyModifiers::NONE)
        }
        Kind::Tasks => {
            crate::input_dialogs::handle_task_picker_key(
                app,
                KeyEvent::new(key, KeyModifiers::NONE),
            );
        }
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let Some(kind) = active(app) else {
        return false;
    };
    if app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers) {
        return true;
    }
    if app.quick_picker.kind != Some(kind) {
        return true;
    }
    let point = (mouse.column, mouse.row).into();
    let hit = app
        .quick_picker
        .rows
        .iter()
        .find(|(area, _)| area.contains(point))
        .map(|(_, index)| *index);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.quick_picker.close.contains(point) || !app.quick_picker.area.contains(point) {
                dispatch(app, kind, KeyCode::Esc);
            } else if let Some(index) = hit {
                select(app, kind, index);
                dispatch(app, kind, KeyCode::Enter);
            }
        }
        MouseEventKind::Moved => {
            if let Some(index) = hit {
                select(app, kind, index);
            }
            app.mouse_hover_changed = true;
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = mouse.kind == MouseEventKind::ScrollUp;
            if kind == Kind::Commands {
                let max = app
                    .palette
                    .filtered
                    .len()
                    .saturating_sub(app.quick_picker.list.height as usize);
                app.quick_picker.scroll = if up {
                    app.quick_picker.scroll.saturating_sub(1)
                } else {
                    (app.quick_picker.scroll + 1).min(max)
                };
            } else {
                dispatch(app, kind, if up { KeyCode::Up } else { KeyCode::Down });
            }
        }
        _ => {}
    }
    if active(app) != Some(kind) {
        app.quick_picker.kind = None;
    }
    true
}

fn item_count(app: &App, kind: Kind) -> usize {
    match kind {
        Kind::OpenWith => app.binary_open_prompt_commands.len() + 2,
        Kind::Commands => app.palette.filtered.len(),
        Kind::Files => app.quick_open_results.len(),
        Kind::Symbols => app.symbol_list_filtered.len(),
        Kind::Tasks => app.task_list.iter().filter(|task| {
            app.task_picker_query.is_empty()
                || crate::app::contains_ignore_case(&task.label, &app.task_picker_query)
        }).count(),
        Kind::Branches => app.git_branch_picker_total_rows(),
    }
}

/// Format only the rows that will actually be drawn. A workspace can have tens
/// of thousands of symbols/files, but a picker shows at most one screenful.
fn items(app: &App, kind: Kind, range: std::ops::Range<usize>) -> Vec<Item> {
    let accent = app.colors.fg_accent;
    match kind {
        Kind::OpenWith => {
            let mut choices = vec![Item {
                name: if app.binary_open_prompt_open_normally {
                    "Open in the editor"
                } else {
                    "View file information"
                }
                .into(),
                detail: if app.binary_open_prompt_open_normally {
                    "Use the built-in text editor"
                } else {
                    "This format cannot be displayed as text"
                }
                .into(),
                badge: if app.binary_open_prompt_open_normally {
                    "Default"
                } else {
                    "Read-only"
                }
                .into(),
                icon: "1".into(),
                color: accent,
            }];
            for command in &app.binary_open_prompt_commands {
                choices.push(Item {
                    name: command.title.clone(),
                    detail: "Open using an installed extension".into(),
                    badge: "Extension".into(),
                    icon: (choices.len() + 1).to_string(),
                    color: accent,
                });
            }
            choices.push(Item {
                name: "Cancel".into(),
                detail: "Return to the workbench".into(),
                badge: "Esc".into(),
                icon: "×".into(),
                color: app.colors.fg_dim,
            });
            choices.into_iter().skip(range.start).take(range.len()).collect()
        }
        Kind::Commands => app
            .palette
            .filtered
            .iter()
            .skip(range.start)
            .take(range.len())
            .map(|&index| {
                let command = &app.palette.commands[index];
                Item {
                    name: command.name.clone(),
                    detail: command.name.clone(),
                    badge: command.shortcut.clone(),
                    icon: ">".into(),
                    color: accent,
                }
            })
            .collect(),
        Kind::Files => app
            .quick_open_results
            .iter()
            .skip(range.start)
            .take(range.len())
            .map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let (icon, color) = crate::ui_dialogs::file_icon_colored(&name);
                let relative = path.strip_prefix(&app.explorer.root).unwrap_or(path);
                Item {
                    name,
                    detail: relative.display().to_string(),
                    badge: relative
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map_or(String::new(), |p| p.display().to_string()),
                    icon: icon.trim().into(),
                    color,
                }
            })
            .collect(),
        Kind::Symbols => app
            .symbol_list_filtered
            .iter()
            .skip(range.start)
            .take(range.len())
            .filter_map(|&index| app.symbol_list_items.get(index))
            .map(|symbol| {
                let icon = match symbol.kind {
                    "function" => "ƒ",
                    "struct" | "class" => "S",
                    "enum" => "E",
                    "trait" | "interface" => "I",
                    "module" => "M",
                    _ => "·",
                };
                let location = symbol.file_name.as_deref().unwrap_or("Current file");
                Item {
                    name: symbol.name.clone(),
                    detail: format!("{} · {location}:{}", symbol.kind, symbol.line + 1),
                    badge: format!("Line {}", symbol.line + 1),
                    icon: icon.into(),
                    color: app.colors.ui.symbol_color(symbol.kind, app.colors.ui.bg_quick_input),
                }
            })
            .collect(),
        Kind::Tasks => app
            .task_list
            .iter()
            .filter(|task| {
                app.task_picker_query.is_empty()
                    || crate::app::contains_ignore_case(&task.label, &app.task_picker_query)
            })
            .skip(range.start)
            .take(range.len())
            .map(|task| Item {
                name: task.label.clone(),
                detail: task.command.clone(),
                badge: task.source.as_str().into(),
                icon: "▶".into(),
                color: app.colors.fg_success,
            })
            .collect(),
        Kind::Branches => {
            let branch_count = app.git_branch_picker_filtered_iter().count();
            let mut items: Vec<_> = app
                .git_branch_picker_filtered_iter()
                .skip(range.start)
                .take(range.len())
                .map(|branch| {
                    let current = branch.target_branch_name() == app.cached_git_branch.trim()
                        && branch.remote_ref().is_none();
                    Item {
                        name: branch
                            .remote_ref()
                            .unwrap_or_else(|| branch.target_branch_name())
                            .into(),
                        detail: branch
                            .remote_ref()
                            .unwrap_or_else(|| branch.target_branch_name())
                            .into(),
                        badge: if current {
                            "Current".into()
                        } else if branch.remote_ref().is_some() {
                            "Remote".into()
                        } else {
                            "Local".into()
                        },
                        icon: "↳".into(),
                        color: if current {
                            app.colors.fg_success
                        } else {
                            accent
                        },
                    }
                })
                .collect();
            if let Some(name) = app.git_branch_picker_create_candidate()
                .filter(|_| range.contains(&branch_count)) {
                items.push(Item {
                    name: format!("Create branch: {name}"),
                    detail: "Create a new branch from the current revision".into(),
                    badge: "New".into(),
                    icon: "+".into(),
                    color: app.colors.fg_success,
                });
            }
            items
        }
    }
}

fn ellipsis(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.into();
    }
    if width == 0 {
        return String::new();
    }
    format!(
        "{}…",
        crate::ui_pickers::truncate_to_width(value, width - 1)
    )
}

fn text<'a>(f: &mut Frame, area: Rect, value: impl Into<Line<'a>>, style: Style) {
    if !area.is_empty() {
        f.render_widget(Paragraph::new(value.into()).style(style), area);
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, kind: Kind) {
    if app.quick_picker.kind != Some(kind) {
        app.quick_picker = PickerState {
            kind: Some(kind),
            ..PickerState::default()
        };
    }
    app.quick_picker.rows.clear();
    app.text_input_regions.clear();
    let count = item_count(app, kind);
    let selection = selected(app, kind).min(count.saturating_sub(1));
    let theme_preview = kind == Kind::Commands && app.palette.active
        && !app.theme_install_prompt_visible() && app.palette_selected_theme_key().is_some();
    let empty = String::new();
    let (title, placeholder, caption, id, query, cursor, anchor) = match kind {
        Kind::OpenWith => (
            "Open With",
            "",
            "CHOOSE HOW TO OPEN THIS FILE".into(),
            TextInputId::QuickOpen,
            &empty,
            0,
            None,
        ),
        Kind::Commands => (
            "Command Palette",
            "Search commands by name…",
            if theme_preview { "THEME PREVIEW" } else { "COMMANDS" }.into(),
            TextInputId::CommandPalette,
            &app.palette.query,
            app.palette.cursor,
            app.palette.selection_anchor,
        ),
        Kind::Files => (
            "Quick Open",
            "Search files by name or path…",
            if app.quick_open_query.is_empty() {
                "RECENT FILES".into()
            } else {
                "WORKSPACE FILES".into()
            },
            TextInputId::QuickOpen,
            &app.quick_open_query,
            app.quick_open_cursor,
            app.quick_open_selection_anchor,
        ),
        Kind::Symbols => (
            if app.symbol_list_workspace {
                "Workspace Symbols"
            } else {
                "Go to Symbol"
            },
            "Search symbols…",
            if app.source_symbol_work.picker_pending() {
                "READING SOURCE SYMBOLS…".into()
            } else if app.symbol_list_workspace {
                "WORKSPACE".into()
            } else {
                "CURRENT FILE".into()
            },
            TextInputId::SymbolList,
            &app.symbol_list_query,
            app.symbol_list_cursor,
            app.symbol_list_selection_anchor,
        ),
        Kind::Tasks => (
            "Run Task",
            "Search available tasks…",
            "AVAILABLE TASKS".into(),
            TextInputId::TaskPicker,
            &app.task_picker_query,
            app.task_picker_cursor,
            app.task_picker_selection_anchor,
        ),
        Kind::Branches => (
            "Git Branches",
            "Find a branch, or enter a new name…",
            format!(
                "ON {}",
                if app.cached_git_branch.is_empty() {
                    "—"
                } else {
                    app.cached_git_branch.trim()
                }
            ),
            TextInputId::GitBranch,
            &app.git_branch_picker_query,
            app.git_branch_picker_cursor,
            app.git_branch_picker_selection_anchor,
        ),
    };
    let query = query.clone();
    let viewport = f.area();
    let width = 86.min(
        viewport
            .width
            .saturating_sub(if viewport.width >= 50 { 8 } else { 2 }),
    );
    let max_height = viewport
        .height
        .saturating_sub(if viewport.height >= 16 { 4 } else { 0 });
    let height = (11
        + count
            .clamp(3, crate::ui_pickers::PALETTE_MAX_ITEMS as usize) as u16)
        .min(max_height);
    let area = Rect::new(
        viewport.x + (viewport.width - width) / 2,
        viewport.y + if viewport.height >= 16 { 2 } else { 0 },
        width,
        height,
    );
    app.quick_picker.area = area;
    let colors = &app.colors;
    let surface = crate::ui_widgets::surface_background(f, area, colors.bg_editor);
    let bg = colors.ui.quick_background.over(surface).unwrap_or(surface);
    let fg = colors.ui.quick_text_color(bg, false);
    let dim = colors.ui.description_color(bg);
    let border = colors.ui.widget_border.border(bg).unwrap_or(bg);
    // Theme previews must show the workbench's original colors and styles.
    if !theme_preview {
        for cell in &mut f.buffer_mut().content {
            cell.modifier.insert(Modifier::DIM);
        }
    }
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().fg(fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let pad = if width >= 35 {
        2
    } else {
        1.min(inner.width / 2)
    };
    let padded = Rect::new(
        inner.x + pad,
        inner.y,
        inner.width.saturating_sub(2 * pad),
        inner.height,
    );
    let compact = height < 15;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 1 } else { 2 }),
            Constraint::Length(if compact { 1 } else { 3 }),
            Constraint::Length(if compact { 1 } else { 2 }),
            Constraint::Min(1),
            Constraint::Length(if compact { 0 } else { 2 }),
        ])
        .split(padded);
    let heading = Rect::new(inner.x, inner.y, inner.width, rows[0].height.min(1));
    let title_bg = colors.ui.quick_title_background.over(bg).unwrap_or(bg);
    text(
        f,
        heading,
        format!("  {title}"),
        Style::default()
            .fg(colors.ui.quick_text_color(title_bg, false))
            .bg(title_bg)
            .add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(
        heading.right().saturating_sub(3).max(heading.x),
        heading.y,
        3.min(heading.width),
        heading.height,
    );
    app.quick_picker.close = close;
    text(
        f,
        close,
        " × ",
        colors.ui.toolbar_style(
            colors.ui.quick_text_color(title_bg, false), title_bg,
            close.contains((app.mouse_col, app.mouse_row).into()), false, false,
        ),
    );
    let hit = if kind == Kind::OpenWith {
        let path = app
            .binary_open_prompt_path
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("File"));
        let filename = path.file_name().unwrap_or_default().to_string_lossy();
        f.render_widget(
            Block::default().style(Style::default().bg(bg)),
            rows[1],
        );
        text(
            f,
            Rect::new(rows[1].x, rows[1].y, rows[1].width, rows[1].height.min(1)),
            ellipsis(&filename, rows[1].width as usize),
            Style::default()
                .fg(fg)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        );
        if rows[1].height > 1 {
            text(
                f,
                Rect::new(rows[1].x, rows[1].y + 1, rows[1].width, 1),
                ellipsis(&path.display().to_string(), rows[1].width as usize),
                Style::default().fg(dim).bg(bg),
            );
        }
        None
    } else {
        let input_style = colors.ui.input_style(bg);
        let input_bg = input_style.bg.unwrap_or(bg);
        let input = Block::default()
            .borders(if rows[1].height >= 3 {
                Borders::ALL
            } else {
                Borders::NONE
            })
            .border_style(Style::default().fg(colors.ui.focus_border.border(input_bg).unwrap_or(input_bg)))
            .style(input_style);
        let input_inner = input.inner(rows[1]);
        f.render_widget(input, rows[1]);
        Some(
            InputField::new(&query)
        .theme_colors(&colors)
                .prefix("  ", colors.ui.description_color(input_bg))
                .placeholder_text(placeholder)
                .cursor_marker("│")
                .cursor_pos(cursor)
                .selection_range(
                    App::inline_selection_range(cursor, anchor),
                )
                .bg(input_bg)
                .render(f, input_inner),
        )
    };
    let summary = Rect::new(
        rows[2].x,
        rows[2].bottom().saturating_sub(1),
        rows[2].width,
        rows[2].height.min(1),
    );
    let count_label = if kind == Kind::Symbols && app.source_symbol_work.picker_pending() {
        "Loading…".into()
    } else if count == 0 {
        "0 results".into()
    } else {
        format!("{} / {}", selection + 1, count)
    };
    let count_width = (count_label.width() as u16).min(summary.width);
    text(
        f,
        summary,
        ellipsis(
            &caption,
            summary.width.saturating_sub(count_width + 2) as usize,
        ),
        Style::default().fg(dim).bg(bg),
    );
    text(
        f,
        Rect::new(
            summary.right() - count_width,
            summary.y,
            count_width,
            summary.height,
        ),
        count_label,
        Style::default().fg(dim).bg(bg),
    );
    let list = rows[3];
    let resized = app.quick_picker.list.height != list.height;
    app.quick_picker.list = list;
    if app.quick_picker.last_query != query {
        app.quick_picker.last_query = query.clone();
        app.quick_picker.scroll = 0;
        app.quick_picker.last_selection = None;
    }
    let visible = list.height.max(1) as usize;
    let mut scroll = app
        .quick_picker
        .scroll
        .min(count.saturating_sub(visible));
    if app.quick_picker.last_selection != Some(selection) || resized {
        if selection < scroll {
            scroll = selection;
        }
        if selection >= scroll + visible {
            scroll = selection + 1 - visible;
        }
    }
    app.quick_picker.scroll = scroll;
    app.quick_picker.last_selection = Some(selection);
    if kind == Kind::Commands {
        app.palette.scroll_offset = scroll;
    }
    if kind == Kind::Branches {
        app.git_branch_picker_scroll_offset = scroll;
    }
    let items = items(app, kind, scroll..scroll.saturating_add(list.height as usize));
    // Highlight with the same case handling used when filtering commands.
    let match_query = if kind == Kind::Commands {
        std::borrow::Cow::Owned(query.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(query.as_str())
    };
    for (row, item) in items.iter().enumerate() {
        let index = scroll + row;
        let rect = Rect::new(list.x, list.y + (index - scroll) as u16, list.width, 1);
        let chosen = index == selection;
        let row_bg = if chosen {
            colors.ui.quick_focus_background.over(bg).unwrap_or(bg)
        } else {
            bg
        };
        let fg = colors.ui.quick_text_color(row_bg, chosen);
        let style = Style::default().fg(fg).bg(row_bg);
        f.render_widget(Block::default().style(style), rect);
        let badge = ellipsis(
            &item.badge,
            if kind == Kind::Files {
                rect.width as usize / 3
            } else {
                rect.width as usize / 2
            },
        );
        let badge_w = if badge.is_empty() {
            0
        } else {
            badge.width() + 2
        };
        let prefix = format!(" {} ", item.icon);
        let name_width = (rect.width as usize).saturating_sub(prefix.width() + badge_w + 2);
        let name = ellipsis(&item.name, name_width);
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            crate::app::fuzzy_match_indices(&item.name, &match_query)
                .map_or(Vec::new(), |(_, indices)| indices)
        };
        let mut matches = matches.into_iter().peekable();
        let icon_color = match kind {
            Kind::Files => item.color,
            Kind::Symbols => app.symbol_list_filtered.get(index).and_then(|&i| app.symbol_list_items.get(i))
                .map_or(item.color, |symbol| colors.ui.symbol_color(symbol.kind, row_bg)),
            _ => item.color,
        };
        let icon_color = if kind == Kind::Files { icon_color }
            else { colors.ui.quick_icon_color(icon_color, row_bg, chosen) };
        let mut spans = vec![Span::styled(prefix, style.fg(icon_color))];
        let match_style = style.fg(colors.ui.quick_match_color(row_bg, chosen))
            .add_modifier(Modifier::BOLD);
        let mut run_start = 0;
        let mut run_highlighted = false;
        for (index, (byte, _)) in name.char_indices().enumerate() {
            let highlighted = matches.peek() == Some(&index);
            if highlighted { matches.next(); }
            if byte > run_start && highlighted != run_highlighted {
                spans.push(Span::styled(&name[run_start..byte], if run_highlighted { match_style } else { style }));
                run_start = byte;
            }
            run_highlighted = highlighted;
        }
        if run_start < name.len() {
            spans.push(Span::styled(&name[run_start..], if run_highlighted { match_style } else { style }));
        }
        text(f, rect, Line::from(spans), style);
        if badge_w > 0 {
            let badge_w = (badge_w as u16).min(rect.width.saturating_sub(1));
            text(
                f,
                Rect::new(
                    rect.right().saturating_sub(badge_w + 1).max(rect.x),
                    rect.y,
                    badge_w,
                    1,
                ),
                format!(" {badge} "),
                if kind == Kind::Commands {
                    colors.ui.keybinding_label_style(row_bg)
                } else {
                    Style::default()
                        .fg(if chosen { fg } else { colors.ui.description_color(row_bg) })
                        .bg(row_bg)
                },
            );
        }
        app.quick_picker.rows.push((rect, index));
    }
    if count == 0 {
        let (message, help) = if kind == Kind::Branches && app.bg_git_branch_picker_running {
            ("Loading branches…", "Reading the repository")
        } else if kind == Kind::Symbols && app.source_symbol_work.picker_pending() {
            ("Reading source symbols…", "You can keep typing while the list loads")
        } else if query.is_empty() {
            match kind {
                Kind::Files => (
                    "No recent files",
                    "Type a file name to search this workspace",
                ),
                Kind::Tasks => (
                    "No tasks available",
                    "Add a task configuration to this workspace",
                ),
                Kind::Symbols => (
                    "No symbols found",
                    "Open a source file with symbols to navigate",
                ),
                Kind::Branches => ("No branches found", "Enter a name to create a branch"),
                _ => ("No commands available", ""),
            }
        } else {
            ("No matching results", "Try a shorter or different search")
        };
        let y = list.y + u16::from(list.height >= 3);
        text(
            f,
            Rect::new(list.x, y, list.width, list.height.min(1)),
            ellipsis(message, list.width as usize),
            Style::default()
                .fg(fg)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        );
        if list.height >= 3 {
            text(
                f,
                Rect::new(list.x, y + 1, list.width, 1),
                ellipsis(help, list.width as usize),
                Style::default().fg(dim).bg(bg),
            );
        }
    } else if count > visible && !list.is_empty() {
        let x = list.right().saturating_sub(1);
        let thumb = ((visible * visible) / count).max(1);
        let offset = scroll * (visible - thumb) / count.saturating_sub(visible).max(1);
        let (track, slider) = colors.ui.scrollbar_colors(bg, app.mouse_col == x
            && app.mouse_row >= list.y && app.mouse_row < list.bottom(), false);
        for row in 0..visible {
            text(
                f,
                Rect::new(x, list.y + row as u16, 1, 1),
                if row >= offset && row < offset + thumb {
                    "┃"
                } else {
                    "│"
                },
                Style::default().fg(if row >= offset && row < offset + thumb { slider } else { track }).bg(track),
            );
        }
    }
    if rows[4].height >= 2 {
        text(
            f,
            Rect::new(rows[4].x, rows[4].y, rows[4].width, 1),
            "─".repeat(rows[4].width as usize),
            Style::default().fg(border).bg(bg),
        );
        let selected_item = selection.checked_sub(scroll).and_then(|row| items.get(row));
        let offscreen_item = selected_item.is_none().then(|| self::items(app, kind, selection..selection + 1));
        let detail = if theme_preview {
            "↑↓ Preview   Enter Apply   Esc Cancel"
        } else {
            selected_item.or_else(|| offscreen_item.as_ref().and_then(|items| items.first()))
                .map_or("Search above to get started", |item| item.detail.as_str())
        };
        text(
            f,
            Rect::new(rows[4].x, rows[4].y + 1, rows[4].width, 1),
            ellipsis(detail, rows[4].width as usize),
            Style::default().fg(dim).bg(bg),
        );
    }
    if let Some(hit) = hit {
        app.register_text_input(id, hit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{GitBranchPickerItem, PaletteCommand, SymbolItem, Tab, TaskDefinition};
    use ratatui::{backend::TestBackend, Terminal};

    fn example(kind: Kind) -> App {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(crate::editor::Editor::test_with_text(
            "fn main() {\n    load_workspace();\n}\n",
        ))];
        app.active_tab = 0;
        for i in 0..36 {
            app.palette.commands.push(PaletteCommand {
                name: format!("Workspace: Open café project {i:02}"),
                shortcut: "Ctrl+Alt+O".into(),
                id: "toggle_line_numbers".into(),
            });
            app.quick_open_results.push(
                app.explorer
                    .root
                    .join(format!("src/workspace/components/project_{i:02}.rs")),
            );
            app.symbol_list_items.push(SymbolItem {
                name: format!("load_workspace_{i:02}"),
                kind: "function",
                line: i,
                tab_index: Some(0),
                file_name: Some("workspace.rs".into()),
                file_path: None,
            });
            app.task_list.push(TaskDefinition {
                label: format!("检查 café workspace {i:02}"),
                command: "cargo test --workspace".into(),
                source: "cargo".into(),
            });
            app.git_branch_picker_items
                .push(GitBranchPickerItem::Local {
                    name: format!("feature/workspace-{i:02}"),
                });
        }
        app.palette.filtered = (0..app.palette.commands.len()).collect();
        app.symbol_list_filtered = (0..app.symbol_list_items.len()).collect();
        app.cached_git_branch = "feature/workspace-00".into();
        match kind {
            Kind::OpenWith => {
                app.binary_open_prompt_active = true;
                app.binary_open_prompt_path =
                    Some(app.explorer.root.join("assets/workspace-design.png"));
            }
            Kind::Commands => app.palette.active = true,
            Kind::Files => app.quick_open_active = true,
            Kind::Symbols => app.symbol_list_active = true,
            Kind::Tasks => app.task_picker_active = true,
            Kind::Branches => app.git_branch_picker_active = true,
        }
        app
    }

    fn render(app: &mut App, kind: Kind, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                text(
                    f,
                    f.area(),
                    "  VT Code · workspace.rs\n\n  fn main() {\n      load_workspace();\n  }",
                    Style::default()
                        .fg(app.colors.fg_text)
                        .bg(app.colors.bg_editor),
                );
                draw(f, app, kind);
            })
            .unwrap();
        terminal
    }

    #[test]
    fn command_palette_case_variants_keep_the_same_highlights() {
        let mut app = example(Kind::Commands);
        let mut expected = None;
        for query in ["project", "PROJECT", "PrOjEcT"] {
            app.palette.query = query.into();
            app.filter_palette_commands();
            let terminal = render(&mut app, Kind::Commands, 100, 30);
            let buffer = terminal.backend().buffer();
            let rows: Vec<Vec<_>> = app.quick_picker.rows.iter().map(|(rect, _)| {
                (rect.x..rect.right()).map(|x| buffer[(x, rect.y)].clone()).collect()
            }).collect();
            assert!(!rows.is_empty(), "query {query:?}");
            assert!(rows[0].iter().any(|cell| cell.modifier.contains(Modifier::BOLD)));
            if let Some(expected) = &expected {
                assert_eq!(&rows, expected, "query {query:?}");
            } else {
                expected = Some(rows);
            }
            assert_eq!(app.palette.query, query, "keep the user's original input");
        }
    }

    #[test]
    fn picker_viewport_rows_keep_full_result_indices_and_selected_details() {
        for kind in [Kind::Commands, Kind::Files, Kind::Symbols, Kind::Tasks, Kind::Branches] {
            let app = example(kind);
            let all = items(&app, kind, 0..item_count(&app, kind));
            let visible = items(&app, kind, 23..30);
            assert_eq!(visible.len(), 7, "{kind:?}");
            for (row, expected) in visible.iter().zip(&all[23..30]) {
                assert_eq!((&row.name, &row.detail, &row.badge), (&expected.name, &expected.detail, &expected.badge));
            }
        }
        let mut app = example(Kind::Branches);
        app.git_branch_picker_query = "workspace".into();
        let count = item_count(&app, Kind::Branches);
        let last = items(&app, Kind::Branches, count - 1..count);
        assert_eq!(last[0].name, "Create branch: workspace");
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_picker_visible_rows() {
        let mut app = example(Kind::Commands);
        app.palette.commands = (0..20_000).map(|index| PaletteCommand {
            name: format!("Workspace: command {index}"),
            shortcut: "Ctrl+Alt+O".into(),
            id: "unknown-command".into(),
        }).collect();
        app.palette.filtered = (0..20_000).collect();
        let mut eager = Vec::new();
        let mut viewport = Vec::new();
        for _ in 0..7 {
            let started = std::time::Instant::now();
            for _ in 0..50 {
                // Reference: before virtualization every redraw formatted the
                // complete result list and only then selected visible rows.
                let all = items(std::hint::black_box(&app), Kind::Commands, 0..20_000);
                std::hint::black_box(&all[15_000..15_012]);
            }
            eager.push(started.elapsed());
            let started = std::time::Instant::now();
            for _ in 0..50 {
                std::hint::black_box(items(std::hint::black_box(&app), Kind::Commands, 15_000..15_012));
            }
            viewport.push(started.elapsed());
        }
        eager.sort();
        viewport.sort();
        eprintln!("picker format 20,000 results, 50 redraws: eager={:?}, visible={:?}", eager[3], viewport[3]);
    }

    #[test]
    fn picker_labels_and_descriptions_are_readable_across_loaded_palettes() {
        use crate::ui_control_visibility_tests::{audit_themes, assert_readable, check_region};
        let mut failures = Vec::new();
        for theme in audit_themes() {
            for kind in [Kind::OpenWith, Kind::Commands, Kind::Files, Kind::Symbols, Kind::Tasks, Kind::Branches] {
                let mut app = example(kind);
                app.colors = theme.to_theme_colors();
                for size in [(100, 30), (42, 14)] {
                    let terminal = render(&mut app, kind, size.0, size.1);
                    let buffer = terminal.backend().buffer();
                    let area = app.quick_picker.area;
                    check_region(buffer, Rect::new(area.x + 1, area.y + 1, area.width - 2, 1),
                        &format!("{}: {kind:?} heading", theme.name), &mut failures, &app.colors.ui);
                    for &(row, _) in &app.quick_picker.rows {
                        // The file/symbol glyph has the icon contrast floor;
                        // labels and result badges have the text contrast floor.
                        check_region(buffer, Rect::new(row.x + 4, row.y, row.width.saturating_sub(5), row.height),
                            &format!("{}: {kind:?} result", theme.name), &mut failures, &app.colors.ui);
                    }
                    if size.0 == 100 {
                        check_region(buffer, Rect::new(area.x + 3, area.bottom() - 2, area.width - 6, 1),
                            &format!("{}: {kind:?} description", theme.name), &mut failures, &app.colors.ui);
                    }
                }
            }
        }
        assert_readable(failures);
    }

    #[test]
    fn polished_pickers_fit_and_keep_keyboard_selection_visible_after_resize() {
        for kind in [
            Kind::OpenWith,
            Kind::Commands,
            Kind::Files,
            Kind::Symbols,
            Kind::Tasks,
            Kind::Branches,
        ] {
            let mut app = example(kind);
            let selected = if kind == Kind::OpenWith { 1 } else { 35 };
            select(&mut app, kind, selected);
            for (width, height) in [(120, 36), (80, 24), (42, 20), (28, 12), (12, 7), (3, 3)] {
                let terminal = render(&mut app, kind, width, height);
                let viewport = Rect::new(0, 0, width, height);
                assert_eq!(
                    app.quick_picker.area.intersection(viewport),
                    app.quick_picker.area
                );
                for (row, _) in &app.quick_picker.rows {
                    assert_eq!(row.intersection(app.quick_picker.area), *row);
                }
                if width >= 28 && height >= 12 {
                    assert!(
                        app.quick_picker.rows.iter().any(|(_, i)| *i == selected),
                        "{kind:?} {width}x{height}"
                    );
                }
                if width >= 42 {
                    if let Ok(root) = std::env::var("VTCODE_PICKER_SNAPSHOTS") {
                        std::fs::create_dir_all(&root).unwrap();
                        let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
                            "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
                            "bold": cell.modifier.contains(Modifier::BOLD), "dim": cell.modifier.contains(Modifier::DIM),
                        })).collect();
                        std::fs::write(std::path::Path::new(&root).join(format!("{kind:?}-{width}x{height}.json")), serde_json::to_vec(&serde_json::json!({"width": width, "height": height, "cells": cells})).unwrap()).unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn polished_picker_clicks_use_the_scrolled_row_and_close_button() {
        let mut app = example(Kind::Commands);
        // Only one command, deliberately scrolled into view, changes this setting.
        app.palette.commands = (0..36)
            .map(|index| PaletteCommand {
                name: format!("Command {index}"),
                shortcut: String::new(),
                id: if index == 35 {
                    "toggle_line_numbers"
                } else {
                    "unknown-command"
                }
                .into(),
            })
            .collect();
        app.palette.filtered = (0..36).collect();
        app.palette.selected = 35;
        render(&mut app, Kind::Commands, 80, 24);
        let (row, _) = *app.quick_picker.rows.last().unwrap();
        let before = app.show_line_numbers;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: row.x + 1,
                row: row.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!app.palette.active);
        assert_ne!(app.show_line_numbers, before);
        for kind in [
            Kind::OpenWith,
            Kind::Commands,
            Kind::Files,
            Kind::Symbols,
            Kind::Tasks,
            Kind::Branches,
        ] {
            let mut app = example(kind);
            render(&mut app, kind, 80, 24);
            let close = app.quick_picker.close;
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: close.x,
                    row: close.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert!(active(&app).is_none(), "{kind:?}");
        }
    }

    #[test]
    fn polished_open_with_owns_keyboard_and_paste_above_the_editor() {
        let mut app = example(Kind::OpenWith);
        app.active_pane = crate::app::ActivePane::Editor;
        app.search_active = true;
        app.search_input_focused = true;
        app.search_query = "needle".into();
        let before = app.editor().unwrap().rope.to_string();
        render(&mut app, Kind::OpenWith, 80, 24);

        assert!(app.paste_into_focused_text_input("do not insert"));
        for code in [KeyCode::Delete, KeyCode::Char('x')] {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(code, KeyModifiers::NONE),
            );
        }
        assert_eq!(app.search_query, "needle");
        assert_eq!(app.editor().unwrap().rope.to_string(), before);
        assert!(app.binary_open_prompt_active);
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(!app.binary_open_prompt_active);
    }

    #[test]
    fn polished_picker_empty_states_are_visible_and_non_actionable() {
        for kind in [
            Kind::Commands,
            Kind::Files,
            Kind::Symbols,
            Kind::Tasks,
            Kind::Branches,
        ] {
            let mut app = example(kind);
            app.palette.filtered.clear();
            app.quick_open_results.clear();
            app.symbol_list_filtered.clear();
            app.task_list.clear();
            app.git_branch_picker_items.clear();
            let terminal = render(&mut app, kind, 80, 24);
            assert!(app.quick_picker.rows.is_empty());
            let contents: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(contents.contains("No "), "{kind:?}");
            let list = app.quick_picker.list;
            handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: list.x,
                    row: list.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert_eq!(active(&app), Some(kind));
        }
    }
}
