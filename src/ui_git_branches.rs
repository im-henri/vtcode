//! Branch browsing, creation and checkout review. Pointer selection never
//! executes Git; the scrollbar has its own reserved, captured hit area.
use crate::{
    app::{App, GitBranchPickerItem},
    text_input::TextInputId,
    ui_widgets::{InputField, draw_dialog_button_with_hover},
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Default)]
enum Mode {
    #[default]
    Browse,
    Create,
    Confirm(GitBranchPickerItem),
}

#[derive(Clone, Copy)]
enum Action {
    New,
    Review,
    Submit,
    Back,
    Close,
    Retry,
}

#[derive(Default)]
pub(crate) struct State {
    mode: Mode,
    focus: usize,
    pub pending: bool,
    pub error: Option<String>,
    repository: Option<PathBuf>,
    saved_query: String,
    saved_selection: usize,
    saved_scroll: usize,
    buttons: Vec<(Rect, Action, usize, bool)>,
    scrollbar: Rect,
    grab: Option<usize>,
    last_selection: Option<usize>,
    last_height: u16,
    generation: u64,
    painted: Option<u64>,
    review_ready: bool,
}

impl State {
    pub(crate) fn for_repository(repository: PathBuf) -> Self {
        Self {
            repository: Some(repository),
            ..Self::default()
        }
    }
    pub(crate) fn accepts_text(&self) -> bool {
        !self.pending && !matches!(self.mode, Mode::Confirm(_))
    }
    pub(crate) fn field_changed(&mut self) {
        self.focus = 0;
        if matches!(self.mode, Mode::Create) {
            self.error = None;
        }
        self.last_selection = None;
    }
    pub(crate) fn failed(&mut self, error: String) {
        self.pending = false;
        self.error = Some(error);
        self.focus = if matches!(self.mode, Mode::Create) {
            2
        } else {
            0
        };
        self.invalidate();
    }
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.painted = None;
        self.buttons.clear();
        self.grab = None;
        self.review_ready = false;
    }
}

fn current(app: &App) -> &str {
    app.cached_git_branch
        .split_whitespace()
        .next()
        .unwrap_or("—")
}
fn is_current(app: &App, item: &GitBranchPickerItem) -> bool {
    item.target_branch_name() == current(app)
}
fn selected(app: &App) -> Option<GitBranchPickerItem> {
    app.git_branch_picker_filtered_iter()
        .nth(app.git_branch_picker_selected)
        .cloned()
}
fn busy(app: &App) -> bool {
    app.git_branch_dialog.pending || app.bg_git_action_running
}

fn hovered(app: &App, area: Rect) -> bool {
    !busy(app) && app.git_branch_dialog.grab.is_none()
        && area.contains((app.mouse_col, app.mouse_row).into())
}

fn replace_input(app: &mut App, value: String) {
    app.git_branch_picker_cursor = value.len();
    app.git_branch_picker_query = value;
    app.git_branch_picker_selection_anchor = None;
    app.text_input_histories.remove(&TextInputId::GitBranch);
    app.text_input_regions.clear();
    app.text_input_drag = None;
}

fn name_error(app: &App) -> Option<&'static str> {
    let name = app.git_branch_picker_query.as_str();
    if name.is_empty() {
        return Some("Enter a name for the new branch.");
    }
    if name.starts_with('-')
        || name == "HEAD"
        || !git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
    {
        return Some(
            "Use a valid branch name, such as feature/search. Spaces, .. and ~ ^ : ? * [ are not allowed.",
        );
    }
    for item in &app.git_branch_picker_items {
        if let GitBranchPickerItem::Local { name: existing } = item {
            if existing == name {
                return Some("A local branch with this name already exists.");
            }
            if existing.starts_with(&format!("{name}/"))
                || name.starts_with(&format!("{existing}/"))
            {
                return Some("This name conflicts with an existing branch path.");
            }
        }
    }
    None
}

fn close(app: &mut App) {
    if busy(app) {
        return;
    }
    app.git_branch_picker_active = false;
    app.git_branch_dialog = State::default();
    replace_input(app, String::new());
    app.git_branch_picker_scroll_offset = 0;
    app.quick_picker.rows.clear();
}

fn back(app: &mut App) {
    if busy(app) {
        return;
    }
    if matches!(app.git_branch_dialog.mode, Mode::Create) {
        let query = std::mem::take(&mut app.git_branch_dialog.saved_query);
        replace_input(app, query);
        app.git_branch_picker_selected = app.git_branch_dialog.saved_selection;
        app.git_branch_picker_scroll_offset = app.git_branch_dialog.saved_scroll;
    }
    app.git_branch_dialog.mode = Mode::Browse;
    app.git_branch_dialog.focus = 1;
    app.git_branch_dialog.error = None;
    app.git_branch_dialog.invalidate();
    app.text_input_regions.clear();
}

fn activate(app: &mut App, action: Action) {
    if busy(app) {
        return;
    }
    match action {
        Action::Close => close(app),
        Action::Back => back(app),
        Action::Retry => {
            let query = app.git_branch_picker_query.clone();
            app.open_git_branch_picker();
            replace_input(app, query);
        }
        Action::New => {
            if app.bg_git_branch_picker_running
                || app.git_branch_dialog.error.is_some() && app.git_branch_picker_items.is_empty()
            {
                return;
            }
            let name = app.git_branch_picker_create_candidate().unwrap_or_default();
            app.git_branch_dialog.saved_query = app.git_branch_picker_query.clone();
            app.git_branch_dialog.saved_selection = app.git_branch_picker_selected;
            app.git_branch_dialog.saved_scroll = app.git_branch_picker_scroll_offset;
            app.git_branch_dialog.mode = Mode::Create;
            app.git_branch_dialog.focus = 0;
            app.git_branch_dialog.error = None;
            app.git_branch_dialog.invalidate();
            replace_input(app, name);
        }
        Action::Review => {
            if app.bg_git_branch_picker_running {
                return;
            }
            if let Some(item) = selected(app).filter(|item| !is_current(app, item)) {
                app.git_branch_dialog.mode = Mode::Confirm(item);
                // A held Enter cannot accept the new confirmation as well.
                app.git_branch_dialog.focus = 0;
                app.git_branch_dialog.error = None;
                app.git_branch_dialog.invalidate();
                app.text_input_regions.clear();
            }
        }
        Action::Submit => {
            if !app.git_branch_dialog.review_ready {
                return;
            }
            if app
                .git_branch_dialog
                .repository
                .as_ref()
                .is_some_and(|root| !app.current_repository_matches(root))
            {
                app.git_branch_dialog.error =
                    Some("The repository changed. Reopen Git Branches to continue.".into());
                return;
            }
            match app.git_branch_dialog.mode.clone() {
                Mode::Create if name_error(app).is_none() => {
                    app.create_and_checkout_branch(&app.git_branch_picker_query.clone());
                }
                Mode::Confirm(item) if !is_current(app, &item) => {
                    app.checkout_git_branch_item(item)
                }
                _ => return,
            }
            app.git_branch_dialog.pending = app.bg_git_action_running;
            app.git_branch_dialog.error =
                (!app.bg_git_action_running).then(|| app.status_message.clone());
            app.git_branch_dialog.invalidate();
        }
    }
    app.quick_picker.rows.clear();
}

pub(crate) fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if busy(app) {
        return;
    }
    let mode = app.git_branch_dialog.mode.clone();
    if code == KeyCode::Esc {
        if matches!(mode, Mode::Browse) {
            close(app);
        } else {
            back(app);
        }
        return;
    }
    if matches!(mode, Mode::Browse) && modifiers.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('n' | 'N') => {
                activate(app, Action::New);
                return;
            }
            KeyCode::Char('r' | 'R') => {
                activate(app, Action::Retry);
                return;
            }
            KeyCode::Char('f' | 'F') => {
                app.git_branch_dialog.focus = 0;
                app.git_branch_picker_selection_anchor = Some(0);
                app.git_branch_picker_cursor = app.git_branch_picker_query.len();
                return;
            }
            _ => {}
        }
    }
    if matches!(code, KeyCode::Tab | KeyCode::BackTab) {
        let mut order = match mode {
            Mode::Browse => {
                let mut order = vec![0];
                if app.git_branch_picker_total_rows() > 0 {
                    order.push(1);
                }
                if !app.bg_git_branch_picker_running {
                    order.push(2);
                }
                if !app.bg_git_branch_picker_running
                    && selected(app).is_some_and(|item| !is_current(app, &item))
                {
                    order.push(3);
                }
                order.push(4);
                order
            }
            Mode::Create => {
                let mut order = vec![0];
                if app.git_branch_dialog.review_ready && name_error(app).is_none() {
                    order.push(1);
                }
                order.push(2);
                order
            }
            Mode::Confirm(_) => {
                if app.git_branch_dialog.review_ready {
                    vec![0, 1]
                } else {
                    vec![0]
                }
            }
        };
        let backwards = code == KeyCode::BackTab || modifiers.contains(KeyModifiers::SHIFT);
        if backwards {
            order.reverse();
        }
        let index = order
            .iter()
            .position(|focus| *focus == app.git_branch_dialog.focus)
            .map_or(0, |index| (index + 1) % order.len());
        app.git_branch_dialog.focus = order[index];
        return;
    }
    if matches!(mode, Mode::Confirm(_)) {
        match code {
            KeyCode::Left | KeyCode::Right => app.git_branch_dialog.focus ^= 1,
            KeyCode::Enter => activate(
                app,
                if app.git_branch_dialog.focus == 0 {
                    Action::Back
                } else {
                    Action::Submit
                },
            ),
            _ => {}
        }
        return;
    }
    if code == KeyCode::Enter {
        let action = if matches!(mode, Mode::Create) {
            if app.git_branch_dialog.focus == 2 {
                Action::Back
            } else {
                Action::Submit
            }
        } else {
            match app.git_branch_dialog.focus {
                2 => Action::New,
                4 => Action::Close,
                _ => Action::Review,
            }
        };
        activate(app, action);
        return;
    }
    if matches!(mode, Mode::Browse)
        && matches!(
            code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        )
    {
        let page = app.git_branch_picker_visible_rows().max(1);
        let selected = app.git_branch_picker_selected;
        app.git_branch_picker_selected = match code {
            KeyCode::Up => selected.saturating_sub(1),
            KeyCode::PageUp => selected.saturating_sub(page),
            KeyCode::PageDown => selected.saturating_add(page),
            _ => selected.saturating_add(1),
        }
        .min(app.git_branch_picker_total_rows().saturating_sub(1));
        app.git_branch_dialog.focus = 1;
        app.sync_git_branch_picker_scroll();
        return;
    }
    if matches!(mode, Mode::Browse)
        && app.git_branch_dialog.focus == 1
        && matches!(code, KeyCode::Home | KeyCode::End)
    {
        app.git_branch_picker_selected = if code == KeyCode::Home {
            0
        } else {
            app.git_branch_picker_total_rows().saturating_sub(1)
        };
        app.sync_git_branch_picker_scroll();
        return;
    }
    let result = app.edit_text_input(TextInputId::GitBranch, code, modifiers, None);
    if result.changed {
        app.git_branch_dialog.field_changed();
        app.git_branch_picker_selected = 0;
        app.git_branch_picker_scroll_offset = 0;
    }
}

fn thumb(app: &App) -> (usize, usize, usize) {
    let height = app.git_branch_dialog.scrollbar.height as usize;
    let total = app.git_branch_picker_total_rows();
    let visible = app.quick_picker.list.height as usize;
    let maximum = total.saturating_sub(visible);
    let size = if maximum == 0 {
        height
    } else {
        (height * visible / total.max(1)).max(2).min(height)
    };
    let top = app.git_branch_picker_scroll_offset * height.saturating_sub(size) / maximum.max(1);
    (top, size, maximum)
}

fn drag_scroll(app: &mut App, y: u16, grab: usize) {
    let (_, size, maximum) = thumb(app);
    let track = app.git_branch_dialog.scrollbar;
    let travel = (track.height as usize).saturating_sub(size);
    let relative = if y < track.y {
        0
    } else {
        (y - track.y) as usize
    };
    app.git_branch_picker_scroll_offset =
        relative.saturating_sub(grab).min(travel) * maximum / travel.max(1);
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if busy(app) {
        return true;
    }
    if let Some(grab) = app.git_branch_dialog.grab {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                drag_scroll(app, mouse.row, grab);
                return true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                app.git_branch_dialog.grab = None;
                return true;
            }
            _ => {}
        }
    }
    if app.git_branch_dialog.painted != Some(app.git_branch_dialog.generation) {
        return true;
    }
    let point = (mouse.column, mouse.row).into();
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && app.git_branch_dialog.scrollbar.contains(point)
    {
        app.text_input_drag = None;
        let (top, size, maximum) = thumb(app);
        if maximum > 0 {
            let relative = (mouse.row - app.git_branch_dialog.scrollbar.y) as usize;
            let grab = if (top..top + size).contains(&relative) {
                relative - top
            } else {
                size / 2
            };
            app.git_branch_dialog.grab = Some(grab);
            drag_scroll(app, mouse.row, grab);
        }
        return true;
    }
    if app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers) {
        app.git_branch_dialog.focus = 0;
        return true;
    }
    let button = app
        .git_branch_dialog
        .buttons
        .iter()
        .find(|(rect, _, _, _)| rect.contains(point))
        .copied();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.text_input_drag = None;
            if app.quick_picker.close.contains(point) || !app.quick_picker.area.contains(point) {
                close(app);
            } else if let Some((_, action, focus, enabled)) = button {
                app.git_branch_dialog.focus = focus;
                if enabled {
                    activate(app, action);
                }
            } else if let Some((_, index)) = app
                .quick_picker
                .rows
                .iter()
                .find(|(rect, _)| rect.contains(point))
                .copied()
            {
                app.git_branch_picker_selected = index;
                app.git_branch_dialog.focus = 1;
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if matches!(app.git_branch_dialog.mode, Mode::Browse) =>
        {
            let maximum = app
                .git_branch_picker_total_rows()
                .saturating_sub(app.quick_picker.list.height as usize);
            app.git_branch_picker_scroll_offset = if mouse.kind == MouseEventKind::ScrollUp {
                app.git_branch_picker_scroll_offset.saturating_sub(3)
            } else {
                app.git_branch_picker_scroll_offset
                    .saturating_add(3)
                    .min(maximum)
            };
        }
        MouseEventKind::Moved => {
            app.mouse_hover_changed = true;
        }
        _ => {}
    }
    true
}

fn text(f: &mut Frame, area: Rect, value: impl Into<ratatui::text::Text<'static>>, style: Style) {
    if !area.is_empty() {
        f.render_widget(
            Paragraph::new(value)
                .style(style)
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}
fn single(f: &mut Frame, area: Rect, value: &str, style: Style) {
    let value = if value.width() > area.width as usize {
        format!(
            "{}…",
            crate::ui_pickers::truncate_to_width(value, area.width.saturating_sub(1) as usize)
        )
    } else {
        value.to_string()
    };
    text(f, area, value, style);
}
fn button(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    label: &str,
    action: Action,
    focus: usize,
    primary: bool,
    enabled: bool,
) {
    if area.is_empty() {
        return;
    }
    draw_dialog_button_with_hover(
        f,
        area,
        label,
        &app.colors,
        primary,
        app.git_branch_dialog.focus == focus,
        hovered(app, area),
        false,
        enabled,
    );
    app.git_branch_dialog
        .buttons
        .push((area, action, focus, enabled));
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let viewport = f.area();
    let browsing = matches!(app.git_branch_dialog.mode, Mode::Browse);
    let width = viewport
        .width
        .saturating_sub(if viewport.width >= 50 { 8 } else { 2 })
        .min(100);
    let desired = if browsing {
        app.git_branch_picker_total_rows().clamp(5, 14) as u16 + 13
    } else {
        19
    };
    let height =
        desired.min(
            viewport
                .height
                .saturating_sub(if viewport.height >= 16 { 2 } else { 0 }),
        );
    let area = Rect::new(
        viewport.x + (viewport.width - width) / 2,
        viewport.y + (viewport.height - height) / 2,
        width,
        height,
    );
    app.quick_picker.area = area;
    app.quick_picker.close = Rect::default();
    app.quick_picker.list = Rect::default();
    app.git_branch_dialog.buttons.clear();
    app.git_branch_dialog.scrollbar = Rect::default();
    app.git_branch_dialog.review_ready = false;
    let colors = app.colors;
    let surface = crate::ui_widgets::surface_background(f, area, colors.bg_editor);
    let bg = colors.ui.quick_background.over(surface).unwrap_or(surface);
    let fg = colors.ui.quick_text_color(bg, false);
    let dim = colors.ui.description_color(bg);
    let border = colors.ui.widget_border.border(bg).unwrap_or(bg);
    for cell in &mut f.buffer_mut().content {
        cell.modifier.insert(Modifier::DIM);
    }
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().fg(fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.git_branch_dialog.painted = Some(app.git_branch_dialog.generation);
    if inner.is_empty() {
        return;
    }
    let title_bg = colors.ui.quick_title_background.over(bg).unwrap_or(bg);
    let title = match app.git_branch_dialog.mode {
        Mode::Browse => "Git Branches",
        Mode::Create => "Create a Branch",
        Mode::Confirm(_) => "Switch Branch?",
    };
    single(
        f,
        Rect::new(inner.x, inner.y, inner.width, 1),
        &format!(" {title}"),
        Style::default()
            .fg(colors.ui.quick_text_color(title_bg, false))
            .bg(title_bg)
            .add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(
        inner.right().saturating_sub(3).max(inner.x),
        inner.y,
        inner.width.min(3),
        1,
    );
    app.quick_picker.close = close;
    single(
        f,
        close,
        " × ",
        colors.ui.toolbar_style(
            fg,
            title_bg,
            hovered(app, close),
            false,
            busy(app),
        ),
    );
    let pad = u16::from(inner.width >= 4);
    let body = Rect::new(
        inner.x + pad,
        inner.y.saturating_add(1).min(inner.bottom()),
        inner.width.saturating_sub(pad * 2),
        inner.height.saturating_sub(1),
    );
    let repository = app
        .source_control_root()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    single(
        f,
        Rect::new(body.x, body.y, body.width, body.height.min(1)),
        &format!(
            "{repository}   ·   Current: {}",
            app.cached_git_branch.trim()
        ),
        Style::default().fg(dim).bg(bg),
    );
    if browsing {
        draw_browse(f, app, body, bg);
    } else {
        draw_form(f, app, body, bg);
    }
}

fn field(f: &mut Frame, app: &mut App, area: Rect, placeholder: &str, bg: ratatui::style::Color) {
    if area.is_empty() {
        return;
    }
    let colors = app.colors;
    let style = colors.ui.input_style(bg);
    let input_bg = style.bg.unwrap_or(bg);
    let block = Block::default()
        .borders(if area.height >= 3 {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_style(Style::default().fg(if app.git_branch_dialog.focus == 0 {
            colors.ui.focus_border.border(input_bg).unwrap_or(input_bg)
        } else {
            colors.ui.widget_border.border(input_bg).unwrap_or(input_bg)
        }))
        .style(style);
    let input = block.inner(area);
    f.render_widget(block, area);
    let hit = InputField::new(&app.git_branch_picker_query)
        .theme_colors(&colors)
        .placeholder_text(placeholder)
        .prefix(" ", colors.ui.description_color(input_bg))
        .cursor_marker(
            if app.git_branch_dialog.focus == 0 && !app.git_branch_dialog.pending {
                "│"
            } else {
                ""
            },
        )
        .cursor_pos(app.git_branch_picker_cursor)
        .selection_range(App::inline_selection_range(
            app.git_branch_picker_cursor,
            app.git_branch_picker_selection_anchor,
        ))
        .bg(input_bg)
        .render(f, input);
    if app.git_branch_dialog.accepts_text() {
        app.register_text_input(TextInputId::GitBranch, hit);
    }
}

fn draw_browse(f: &mut Frame, app: &mut App, body: Rect, bg: ratatui::style::Color) {
    let colors = app.colors;
    let dim = colors.ui.description_color(bg);
    let compact = body.height < 15;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(u16::from(!compact)),
            Constraint::Length(if compact { 1 } else { 3 }),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(if compact { 0 } else { 2 }),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(body);
    let new_width = (if body.width >= 48 { 18 } else { 9 }).min(rows[2].width);
    let new = Rect::new(
        rows[2].right().saturating_sub(new_width),
        rows[2].y + rows[2].height.saturating_sub(1) / 2,
        new_width,
        rows[2].height.min(1),
    );
    let search = Rect::new(
        rows[2].x,
        rows[2].y,
        rows[2].width.saturating_sub(new_width + 1),
        rows[2].height,
    );
    field(f, app, search, "Find a local or remote branch…", bg);
    let retry = app.git_branch_dialog.error.is_some() && app.git_branch_picker_items.is_empty();
    button(
        f,
        app,
        new,
        if retry {
            "Retry"
        } else if body.width >= 48 {
            "+ New branch…"
        } else {
            "+ New…"
        },
        if retry { Action::Retry } else { Action::New },
        2,
        false,
        !busy(app) && !app.bg_git_branch_picker_running,
    );
    let count = app.git_branch_picker_total_rows();
    single(
        f,
        rows[3],
        &if app.bg_git_branch_picker_running {
            "Loading branches…".into()
        } else {
            format!(
                "{} {}",
                count,
                if count == 1 { "branch" } else { "branches" }
            )
        },
        Style::default().fg(dim).bg(bg),
    );
    let track_width = rows[4].width.min(3);
    let list = Rect::new(
        rows[4].x,
        rows[4].y,
        rows[4].width.saturating_sub(track_width + 1),
        rows[4].height,
    );
    let track = Rect::new(
        rows[4].right().saturating_sub(track_width),
        rows[4].y,
        track_width,
        rows[4].height,
    );
    app.quick_picker.list = list;
    app.git_branch_dialog.scrollbar = track;
    if app.git_branch_dialog.last_selection != Some(app.git_branch_picker_selected)
        || app.git_branch_dialog.last_height != list.height
    {
        app.sync_git_branch_picker_scroll();
    }
    app.git_branch_dialog.last_selection = Some(app.git_branch_picker_selected);
    app.git_branch_dialog.last_height = list.height;
    app.git_branch_picker_scroll_offset = app
        .git_branch_picker_scroll_offset
        .min(count.saturating_sub(list.height as usize));
    let scroll = app.git_branch_picker_scroll_offset;
    let items: Vec<_> = app
        .git_branch_picker_filtered_iter()
        .skip(scroll)
        .take(list.height as usize)
        .cloned()
        .collect();
    for (row, item) in items.iter().enumerate() {
        let rect = Rect::new(list.x, list.y + row as u16, list.width, 1);
        if rect.is_empty() {
            continue;
        }
        let chosen = scroll + row == app.git_branch_picker_selected;
        let style = if chosen {
            let row_bg = colors.ui.quick_focus_background.over(bg).unwrap_or(bg);
            Style::default().fg(colors.ui.quick_text_color(row_bg, true)).bg(row_bg)
        } else {
            colors.ui.list_row_style(
                crate::theme_workbench::ColorLayer::opaque(colors.ui.quick_text_color(bg, false)),
                bg, false, false, hovered(app, rect),
            )
        };
        f.render_widget(Block::default().style(style), rect);
        let local_current = is_current(app, item) && item.remote_ref().is_none();
        let badge = if local_current {
            "Current"
        } else if item.remote_ref().is_some() {
            "Remote"
        } else {
            "Local"
        };
        let badge_width = (badge.width() as u16 + 2).min(rect.width / 3);
        let name = item
            .remote_ref()
            .unwrap_or_else(|| item.target_branch_name());
        single(
            f,
            Rect::new(rect.x, rect.y, rect.width.saturating_sub(badge_width), 1),
            &format!(
                " {} {name}",
                if local_current {
                    "✓"
                } else if item.remote_ref().is_some() {
                    "↓"
                } else {
                    " "
                }
            ),
            style,
        );
        single(
            f,
            Rect::new(
                rect.right().saturating_sub(badge_width),
                rect.y,
                badge_width,
                1,
            ),
            badge,
            style.add_modifier(if local_current {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        );
        app.quick_picker.rows.push((rect, scroll + row));
    }
    if count == 0 {
        text(
            f,
            list,
            if app.bg_git_branch_picker_running {
                "Loading branches…"
            } else if app.git_branch_picker_query.is_empty() {
                "No branches found.\nCreate a branch after your first commit."
            } else {
                "No matching branches.\nUse New branch to create one."
            }
            .to_string(),
            Style::default().fg(dim).bg(bg),
        );
    }
    if !track.is_empty() {
        let (top, size, maximum) = thumb(app);
        let (track_bg, slider) = colors.ui.scrollbar_colors(
            bg,
            !busy(app) && track.contains((app.mouse_col, app.mouse_row).into()),
            app.git_branch_dialog.grab.is_some(),
        );
        for row in 0..track.height as usize {
            let color = if maximum > 0 && (top..top + size).contains(&row) {
                slider
            } else {
                track_bg
            };
            text(
                f,
                Rect::new(track.x, track.y + row as u16, track.width, 1),
                "█".repeat(track.width as usize),
                Style::default().fg(color).bg(color),
            );
        }
    }
    let item = selected(app);
    let detail = app.git_branch_dialog.error.clone().unwrap_or_else(|| {
        item.as_ref().map_or_else(
            || "Select a branch to review a switch.".into(),
            |item| {
                if is_current(app, item) {
                    format!("{} · You are on this branch.", item.target_branch_name())
                } else if let Some(remote) = item.remote_ref() {
                    format!(
                        "{remote}\nSwitch using local branch {}",
                        item.target_branch_name()
                    )
                } else {
                    format!(
                        "{}\nSelect Switch… to review before changing branches.",
                        item.target_branch_name()
                    )
                }
            },
        )
    });
    text(
        f,
        rows[5],
        detail,
        Style::default()
            .fg(if app.git_branch_dialog.error.is_some() {
                colors.ui.error_text_color(bg)
            } else {
                dim
            })
            .bg(bg),
    );
    let cancel_width = rows[6].width.min(10);
    button(
        f,
        app,
        Rect::new(rows[6].x, rows[6].y, cancel_width, rows[6].height),
        "Close",
        Action::Close,
        4,
        false,
        !busy(app),
    );
    let switch_width = rows[6].width.saturating_sub(cancel_width + 1).min(16);
    button(
        f,
        app,
        Rect::new(
            rows[6].right().saturating_sub(switch_width),
            rows[6].y,
            switch_width,
            rows[6].height,
        ),
        "Switch…",
        Action::Review,
        3,
        true,
        !busy(app)
            && !app.bg_git_branch_picker_running
            && item.as_ref().is_some_and(|item| !is_current(app, item)),
    );
    single(
        f,
        rows[7],
        "↑↓ Select   Enter Review   Ctrl+N New   Ctrl+R Refresh   Esc Close",
        Style::default().fg(dim).bg(bg),
    );
}

fn draw_form(f: &mut Frame, app: &mut App, body: Rect, bg: ratatui::style::Color) {
    let colors = app.colors;
    let fg = colors.ui.quick_text_color(bg, false);
    let dim = colors.ui.description_color(bg);
    let content = Rect::new(
        body.x,
        (body.y + 2).min(body.bottom()),
        body.width,
        body.height.saturating_sub(5),
    );
    let creating = matches!(app.git_branch_dialog.mode, Mode::Create);
    if creating {
        single(
            f,
            Rect::new(content.x, content.y, content.width, content.height.min(1)),
            "New branch name",
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        );
        let input = Rect::new(
            content.x,
            (content.y + 1).min(content.bottom()),
            content.width,
            content.height.saturating_sub(1).min(3),
        );
        field(f, app, input, "feature/my-change", bg);
        let details = Rect::new(
            content.x,
            (input.bottom() + 1).min(content.bottom()),
            content.width,
            content.bottom().saturating_sub(input.bottom() + 1),
        );
        let validation = app
            .git_branch_dialog
            .error
            .as_deref()
            .or_else(|| name_error(app));
        text(
            f,
            details,
            format!(
                "Create from current branch: {}\n\n{}",
                current(app),
                if app.git_branch_dialog.pending {
                    "Creating branch and switching…"
                } else {
                    validation.unwrap_or("Creates the new branch and switches to it.")
                }
            ),
            Style::default()
                .fg(
                    if app.git_branch_dialog.error.is_some()
                        || !app.git_branch_picker_query.is_empty() && validation.is_some()
                    {
                        colors.ui.error_text_color(bg)
                    } else {
                        dim
                    },
                )
                .bg(bg),
        );
        app.git_branch_dialog.review_ready = content.width >= 24 && content.height >= 7;
    } else if let Mode::Confirm(item) = app.git_branch_dialog.mode.clone() {
        let remote_detail = match &item {
            GitBranchPickerItem::Remote {
                remote_ref,
                local_name,
            } => {
                if app.git_branch_picker_items.iter().any(|item| matches!(item, GitBranchPickerItem::Local { name } if name == local_name)) {
                    format!("Use existing local branch {local_name}.\nRemote reference: {remote_ref}")
                } else { format!("Create local branch {local_name} tracking {remote_ref}.") }
            }
            _ => "Local branch".into(),
        };
        let message = format!(
            "Switch to\n{}\n\nFrom: {}\n{}\n\n{}",
            item.target_branch_name(),
            current(app),
            remote_detail,
            app.git_branch_dialog
                .error
                .as_deref()
                .unwrap_or(if app.git_branch_dialog.pending {
                    "Switching branch…"
                } else {
                    "Git keeps compatible local changes and stops if files would be overwritten."
                })
        );
        text(f, content, message, Style::default().fg(fg).bg(bg));
        app.git_branch_dialog.review_ready = content.width >= 24
            && content.height
                >= 3 + item
                    .target_branch_name()
                    .width()
                    .div_ceil(content.width.max(1) as usize) as u16;
    }
    let actions = Rect::new(
        body.x,
        body.bottom().saturating_sub(2).max(body.y),
        body.width,
        body.height.min(1),
    );
    let back_width = actions.width.min(10);
    button(
        f,
        app,
        Rect::new(actions.x, actions.y, back_width, actions.height),
        "Back",
        Action::Back,
        if creating { 2 } else { 0 },
        false,
        !busy(app),
    );
    let primary_width = actions.width.saturating_sub(back_width + 1).min(24);
    let enabled = !busy(app)
        && app.git_branch_dialog.review_ready
        && (!creating || name_error(app).is_none());
    button(
        f,
        app,
        Rect::new(
            actions.right().saturating_sub(primary_width),
            actions.y,
            primary_width,
            actions.height,
        ),
        if busy(app) {
            "Working…"
        } else if creating && primary_width < 19 {
            "Create"
        } else if !creating && primary_width < 17 {
            "Switch"
        } else if creating {
            "Create & switch"
        } else {
            "Switch branch"
        },
        Action::Submit,
        1,
        true,
        enabled,
    );
    let hint = Rect::new(
        body.x,
        body.bottom().saturating_sub(1).max(body.y),
        body.width,
        body.height.min(1),
    );
    single(
        f,
        hint,
        if app.git_branch_dialog.review_ready {
            "Tab Select action   Enter Activate   Esc Back"
        } else {
            "Enlarge the terminal to continue"
        },
        Style::default().fg(dim).bg(bg),
    );
}

#[cfg(test)]
#[path = "ui_git_branches_tests.rs"]
mod tests;
