//! Event-driven file-learning controls. Rendering reads published status and a
//! bounded inventory page; it never scans the workspace or polls the worker.
use std::path::PathBuf;

use crate::{
    app::App,
    app_vtcomplete_settings::{LearningMode, number_error},
    completion_index::{FilePage, IndexPhase, IndexSettings, IndexSpeed, IndexStatus},
    ui_performance::clip_middle,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

const CONTROL_COUNT: usize = 6;
const FILE_PAGE_SIZE: usize = 32;
const CHOICE_IDS: [&str; 3] = [
    "vtcomplete_learning_mode",
    "vtcomplete_sources",
    "vtcomplete_index_speed",
];

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum Page {
    #[default]
    Learning,
    Limits,
    Files,
}
impl Page {
    fn ordinal(self) -> usize {
        match self {
            Self::Learning => 0,
            Self::Limits => 1,
            Self::Files => 2,
        }
    }
    fn first(self) -> usize {
        if self == Self::Limits { 3 } else { 0 }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    ToggleLearning,
    Refresh,
    Discard,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) open: bool,
    page: Page,
    selected: usize,
    scroll: usize,
    area: Rect,
    close: Rect,
    tabs: Vec<(Rect, Page)>,
    controls: Vec<(Rect, usize)>,
    buttons: Vec<(Rect, usize, bool, bool)>,
    choices: Vec<(Rect, usize, usize)>,
    actions: Vec<(Rect, Action, bool)>,
    values: Vec<(Rect, usize)>,
    settings_button: Rect,
    editing: Option<NumberEdit>,
    edit_error: Option<String>,
    file_selected: usize,
    file_scroll: usize,
    file_area: Rect,
    file_rows: Vec<FileRow>,
    file_cache: Option<FileCache>,
    file_message: Option<String>,
}
struct FileCache {
    requested_offset: usize,
    page: FilePage,
}
#[derive(Clone)]
struct FileRow {
    area: Rect,
    action: Rect,
    ordinal: usize,
    path: PathBuf,
    revision: u64,
}
struct NumberEdit {
    control: usize,
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    history: crate::text_input::TextInputHistory,
    hit_map: Option<crate::text_input::TextInputHitMap>,
}
impl State {
    fn clear_targets(&mut self) {
        self.close = Rect::default();
        self.settings_button = Rect::default();
        self.tabs.clear();
        self.controls.clear();
        self.buttons.clear();
        self.choices.clear();
        self.actions.clear();
        self.values.clear();
        self.file_area = Rect::default();
        self.file_rows.clear();
        if let Some(edit) = &mut self.editing {
            edit.hit_map = None;
        }
    }
    fn hit(&self, point: ratatui::layout::Position) -> Option<usize> {
        if self.close.contains(point) {
            return Some(0);
        }
        if self.settings_button.contains(point) {
            return Some(1);
        }
        if let Some(i) = self.tabs.iter().position(|(r, _)| r.contains(point)) {
            return Some(10 + i);
        }
        if let Some(i) = self.actions.iter().position(|(r, _, _)| r.contains(point)) {
            return Some(20 + i);
        }
        if let Some(i) = self.choices.iter().position(|(r, _, _)| r.contains(point)) {
            return Some(30 + i);
        }
        if let Some(i) = self
            .buttons
            .iter()
            .position(|(r, _, _, _)| r.contains(point))
        {
            return Some(40 + i);
        }
        if let Some(i) = self.values.iter().position(|(r, _)| r.contains(point)) {
            return Some(50 + i);
        }
        if let Some(row) = self.file_rows.iter().find(|r| r.area.contains(point)) {
            return Some(128 + row.ordinal * 2 + usize::from(row.action.contains(point)));
        }
        self.controls
            .iter()
            .find(|(r, _)| r.contains(point))
            .map(|(_, i)| 64 + i)
    }
    fn file_total(&self) -> usize {
        self.file_cache.as_ref().map_or(0, |c| c.page.total)
    }
    fn select_page(&mut self, page: Page) {
        self.page = page;
        self.file_message = None;
        if !(page.first()..page.first() + 3).contains(&self.selected) {
            self.selected = page.first();
        }
        self.scroll = 0;
        self.clear_targets();
    }
    fn show_files(&mut self) {
        self.select_page(Page::Files);
    }
}

fn file_action(app: &mut App, row: FileRow) {
    if row.area.is_empty()
        || row.action.is_empty()
        || app
            .completion_index_ui
            .file_cache
            .as_ref()
            .is_none_or(|cache| cache.page.revision != row.revision)
    {
        return;
    }
    // Retain the identity painted under the pointer/selection. A publication
    // between drawing and input must never turn an ordinal into another file.
    let accepted = app.completion_index.remove_file(&row.path);
    let state = &mut app.completion_index_ui;
    state.file_message = Some(if accepted {
        "Removal queued. Source file kept.".into()
    } else {
        "Removal unavailable. Try again shortly.".into()
    });
    state.file_cache = None;
    state.file_rows.clear();
}

fn move_file_selection(state: &mut State, delta: isize) {
    let last = state.file_total().saturating_sub(1);
    state.file_selected = state.file_selected.saturating_add_signed(delta).min(last);
}

fn refresh_file_page(app: &mut App, height: usize) {
    let state = &mut app.completion_index_ui;
    let height = height.max(1).min(FILE_PAGE_SIZE);
    state.file_scroll = state
        .file_scroll
        .min(state.file_selected)
        .max(state.file_selected.saturating_sub(height - 1));
    let revision = app.completion_index.revision();
    if state.file_cache.as_ref().is_none_or(|cache| {
        cache.page.revision != revision || cache.requested_offset != state.file_scroll
    }) {
        state.file_cache = Some(FileCache {
            requested_offset: state.file_scroll,
            page: app
                .completion_index
                .file_page(state.file_scroll, FILE_PAGE_SIZE),
        });
    }
    let total = state.file_total();
    state.file_selected = state.file_selected.min(total.saturating_sub(1));
    let scroll = state.file_scroll.min(total.saturating_sub(height));
    if scroll != state.file_scroll {
        state.file_scroll = scroll;
        state.file_cache = Some(FileCache {
            requested_offset: scroll,
            page: app.completion_index.file_page(scroll, FILE_PAGE_SIZE),
        });
    }
}

pub(crate) fn open(app: &mut App) {
    app.close_palette();
    app.dismiss_vtcomplete();
    app.lsp_completion_active = false;
    app.lsp_signature_active = false;
    app.dispatch_lsp_suggestions();
    app.clear_transient_editor_state();
    app.clear_drag_states();
    app.reset_hover_states();
    app.context_menu_active = false;
    app.menu_open = false;
    app.tab_context_active = false;
    app.explorer_context_active = false;
    app.sc_context_active = false;
    app.sc_context_targets.clear();
    app.status_details.open = false;
    crate::ui_performance_details::release_focus(app);
    app.completion_index_ui = State {
        open: true,
        ..State::default()
    };
}

pub(crate) fn phase_label(phase: IndexPhase) -> &'static str {
    match phase {
        IndexPhase::Loading => "Loading learned files",
        IndexPhase::Idle => "Up to date",
        IndexPhase::Indexing => "Updating learned files",
        IndexPhase::Scanning => "Discovering workspace files",
        IndexPhase::Paused | IndexPhase::Cancelled => "Learning paused",
        IndexPhase::Disabled => "Learned-file suggestions off",
        IndexPhase::Limited => "Learning limit reached",
    }
}
pub(crate) fn status_label(status: &IndexStatus) -> String {
    let label = match LearningMode::from_settings(&status.settings) {
        LearningMode::Off => "VTComplete · learning off",
        LearningMode::Paused => "VTComplete · learning paused",
        LearningMode::On => match status.phase {
            IndexPhase::Loading | IndexPhase::Indexing | IndexPhase::Scanning => "VTComplete · learning",
            IndexPhase::Limited => "VTComplete · learning limited",
            _ => "VTComplete",
        },
    };
    format!("{label} · {}", status.indexed_files)
}
fn control_rows(settings: &IndexSettings) -> [(String, String); CONTROL_COUNT] {
    [
        (
            "Learning mode".into(),
            LearningMode::from_settings(settings).label().into(),
        ),
        (
            "Learn from".into(),
            if settings.project_wide {
                "Entire workspace"
            } else {
                "Files you open"
            }
            .into(),
        ),
        (
            "File learning CPU".into(),
            match settings.speed {
                IndexSpeed::Low => "Low CPU",
                IndexSpeed::Balanced => "Balanced",
                IndexSpeed::Fast => "Faster",
            }
            .into(),
        ),
        (
            "Maximum indexed files".into(),
            settings.max_files.to_string(),
        ),
        (
            "Source file size limit".into(),
            settings.max_file_bytes.div_ceil(1024 * 1024).to_string(),
        ),
        (
            "Index RAM budget".into(),
            settings.max_memory_mb.to_string(),
        ),
    ]
}
fn control_help(control: usize, settings: &IndexSettings) -> &'static str {
    match control {
        0 => match LearningMode::from_settings(settings) {
            LearningMode::Off => "Learned suggestions off. Current-file suggestions still work.",
            LearningMode::Paused | LearningMode::On => {
                "Paused keeps suggestions. Off hides learned suggestions."
            }
        },
        1 => {
            if settings.project_wide {
                "Discovers workspace files using ignore rules and limits."
            } else {
                "Only opened files. Previously learned files stay available."
            }
        }
        2 => "Learning and cache loading; not prediction.",
        3 => "All indexed files share the RAM budget.",
        4 => "Larger source files are skipped.",
        _ => "Shared index RAM. Excludes editor buffers and disk cache.",
    }
}
fn step_limit(current: usize, choices: &[usize], backwards: bool) -> usize {
    if backwards {
        choices
            .iter()
            .rev()
            .copied()
            .find(|&v| v < current)
            .unwrap_or(choices[0])
    } else {
        choices
            .iter()
            .copied()
            .find(|&v| v > current)
            .unwrap_or(*choices.last().unwrap())
    }
}
fn choose(app: &mut App, control: usize, option: usize) {
    if let Some(id) = CHOICE_IDS.get(control)
        && let Some(value) = app
            .completion_dropdown_options(id)
            .and_then(|values| values.get(option))
    {
        app.set_completion_dropdown(id, value);
        app.completion_index_ui.file_message = None;
    }
}
fn activate(app: &mut App, control: usize, backwards: bool) {
    let mut settings = app.completion_index.settings();
    if control < 3 {
        let rows = control_rows(&settings);
        let options = app
            .completion_dropdown_options(CHOICE_IDS[control])
            .unwrap();
        let current = options.iter().position(|s| *s == rows[control].1).unwrap();
        choose(
            app,
            control,
            (current + if backwards { options.len() - 1 } else { 1 }) % options.len(),
        );
        return;
    }
    match control {
        3 => {
            settings.max_files = step_limit(
                settings.max_files,
                &[1, 100, 1_000, 5_000, 10_000, 20_000, 50_000, 100_000],
                backwards,
            )
        }
        4 => {
            settings.max_file_bytes = step_limit(
                settings.max_file_bytes.div_ceil(1024 * 1024),
                &[1, 10, 25, 50, 100, 250, 500, 1024],
                backwards,
            ) * 1024
                * 1024
        }
        5 => {
            settings.max_memory_mb = step_limit(
                settings.max_memory_mb,
                &[4, 8, 16, 32, 48, 64, 128, 256],
                backwards,
            )
        }
        _ => return,
    }
    app.completion_index.configure(settings);
}
fn perform(app: &mut App, action: Action) {
    match action {
        Action::ToggleLearning => {
            let on =
                LearningMode::from_settings(&app.completion_index.settings()) == LearningMode::On;
            app.set_completion_dropdown(
                "vtcomplete_learning_mode",
                if on { "Paused" } else { "On" },
            );
            app.completion_index_ui.file_message = None;
        }
        Action::Refresh => {
            app.completion_index_ui.file_message = Some(if app.refresh_completion_files() {
                "Refreshing files in the background.".into()
            } else {
                "Start learning before refreshing files.".into()
            });
        }
        Action::Discard => {
            app.discard_completion_updates();
            app.completion_index_ui.file_message =
                Some("Updates discarded. Learning paused; suggestions kept.".into());
        }
    }
}
fn numeric_id(control: usize) -> Option<&'static str> {
    match control {
        3 => Some("vtcomplete_max_files"),
        4 => Some("vtcomplete_max_file_mb"),
        5 => Some("vtcomplete_memory_mb"),
        _ => None,
    }
}
fn begin_number(app: &mut App, control: usize) {
    if numeric_id(control).is_none() {
        return;
    }
    app.completion_index_ui.select_page(Page::Limits);
    app.completion_index_ui.selected = control;
    let value = control_rows(&app.completion_index.settings())[control]
        .1
        .clone();
    app.completion_index_ui.editing = Some(NumberEdit {
        control,
        cursor: value.len(),
        text: value,
        anchor: Some(0),
        history: Default::default(),
        hit_map: None,
    });
    app.completion_index_ui.edit_error = None;
}
fn commit_number(app: &mut App) -> bool {
    let Some(edit) = &app.completion_index_ui.editing else {
        return true;
    };
    let id = numeric_id(edit.control).unwrap();
    let error = match edit.text.trim().parse::<u64>() {
        Ok(value) => number_error(id, value),
        Err(_) => Some("Enter a whole number. Esc cancels the edit.".into()),
    };
    if let Some(error) = error {
        app.completion_index_ui.edit_error = Some(error);
        return false;
    }
    app.set_completion_number(id, edit.text.trim().parse().unwrap());
    app.completion_index_ui.editing = None;
    app.completion_index_ui.edit_error = None;
    true
}
pub(crate) fn handle_paste(app: &mut App, text: &str) {
    if let Some(edit) = &mut app.completion_index_ui.editing {
        edit.history.edit(
            crate::text_input::TextInput::new(&mut edit.text, &mut edit.cursor, &mut edit.anchor)
                .filter(|ch| ch.is_ascii_digit()),
            KeyCode::Null,
            KeyModifiers::NONE,
            Some(text),
        );
        app.completion_index_ui.edit_error = None;
    }
}
pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.completion_index_ui.open {
        return false;
    }
    if app.completion_index_ui.editing.is_some() {
        match key.code {
            KeyCode::Esc => {
                app.completion_index_ui.editing = None;
                app.completion_index_ui.edit_error = None;
            }
            KeyCode::Enter => {
                commit_number(app);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if commit_number(app) {
                    let back =
                        key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                    let selected =
                        3 + (app.completion_index_ui.selected - 3 + if back { 2 } else { 1 }) % 3;
                    begin_number(app, selected);
                }
            }
            _ => {
                let edit = app.completion_index_ui.editing.as_mut().unwrap();
                edit.history.edit(
                    crate::text_input::TextInput::new(
                        &mut edit.text,
                        &mut edit.cursor,
                        &mut edit.anchor,
                    )
                    .filter(|ch| ch.is_ascii_digit()),
                    key.code,
                    key.modifiers,
                    None,
                );
                app.completion_index_ui.edit_error = None;
            }
        }
        return true;
    }
    if matches!(
        key.code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    ) {
        app.completion_index_ui.file_message = None;
    }
    match key.code {
        KeyCode::Esc => app.completion_index_ui.open = false,
        KeyCode::Char('s' | 'S') => app.open_completion_settings(),
        KeyCode::Char('p' | 'P') => perform(app, Action::ToggleLearning),
        KeyCode::Char('r' | 'R') => perform(app, Action::Refresh),
        KeyCode::Char('c' | 'C') => perform(app, Action::Discard),
        KeyCode::Char('1') => app.completion_index_ui.select_page(Page::Learning),
        KeyCode::Char('2') => app.completion_index_ui.select_page(Page::Limits),
        KeyCode::Char('3' | 'f' | 'F') => app.completion_index_ui.show_files(),
        KeyCode::Tab | KeyCode::BackTab => {
            let state = &mut app.completion_index_ui;
            let back = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            state.select_page(
                [Page::Learning, Page::Limits, Page::Files]
                    [(state.page.ordinal() + if back { 2 } else { 1 }) % 3],
            );
        }
        _ if app.completion_index_ui.page == Page::Files => {
            let state = &mut app.completion_index_ui;
            match key.code {
                KeyCode::Up => move_file_selection(state, -1),
                KeyCode::Down => move_file_selection(state, 1),
                KeyCode::PageUp | KeyCode::PageDown => {
                    let step = state.file_area.height.max(1) as isize;
                    move_file_selection(
                        state,
                        if key.code == KeyCode::PageDown {
                            step
                        } else {
                            -step
                        },
                    );
                }
                KeyCode::Home => state.file_selected = 0,
                KeyCode::End => state.file_selected = state.file_total().saturating_sub(1),
                KeyCode::Enter | KeyCode::Delete => {
                    if let Some(row) = state
                        .file_rows
                        .iter()
                        .find(|r| r.ordinal == state.file_selected)
                        .cloned()
                    {
                        file_action(app, row);
                    }
                }
                _ => {}
            }
        }
        _ => {
            let state = &mut app.completion_index_ui;
            let first = state.page.first();
            match key.code {
                KeyCode::Up => state.selected = first + (state.selected - first + 2) % 3,
                KeyCode::Down => state.selected = first + (state.selected - first + 1) % 3,
                KeyCode::Home => state.selected = first,
                KeyCode::End => state.selected = first + 2,
                KeyCode::Enter | KeyCode::Char(' ') if numeric_id(state.selected).is_some() => {
                    let selected = state.selected;
                    begin_number(app, selected);
                }
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Left => {
                    let selected = state.selected;
                    activate(app, selected, key.code == KeyCode::Left);
                }
                _ => {}
            }
        }
    }
    true
}
pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.completion_index_ui.open {
        return false;
    }
    let point = (mouse.column, mouse.row).into();
    let previous = (app.mouse_col, app.mouse_row).into();
    app.mouse_hover_changed =
        app.completion_index_ui.hit(point) != app.completion_index_ui.hit(previous);
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.completion_index_ui.close.contains(point)
                || !app.completion_index_ui.area.contains(point)
            {
                app.completion_index_ui.editing = None;
                app.completion_index_ui.edit_error = None;
                app.completion_index_ui.open = false;
                return true;
            }
            if let Some(edit) = &mut app.completion_index_ui.editing
                && let Some(map) = &edit.hit_map
                && map.contains(mouse.column, mouse.row)
            {
                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    edit.anchor.get_or_insert(edit.cursor);
                } else {
                    edit.anchor = None;
                }
                edit.cursor = map.byte_at(mouse.column, mouse.row);
                return true;
            }
            if !commit_number(app) {
                return true;
            }
            if let Some((_, page)) = app
                .completion_index_ui
                .tabs
                .iter()
                .find(|(r, _)| r.contains(point))
                .copied()
            {
                app.completion_index_ui.select_page(page);
            } else if app.completion_index_ui.settings_button.contains(point) {
                app.open_completion_settings();
            } else if let Some((_, action, enabled)) = app
                .completion_index_ui
                .actions
                .iter()
                .find(|(r, _, _)| r.contains(point))
                .copied()
            {
                if enabled {
                    perform(app, action);
                } else if action == Action::Refresh {
                    app.completion_index_ui.file_message =
                        Some("Start learning before refreshing files.".into());
                }
            } else if let Some((_, control, option)) = app
                .completion_index_ui
                .choices
                .iter()
                .find(|(r, _, _)| r.contains(point))
                .copied()
            {
                app.completion_index_ui.selected = control;
                choose(app, control, option);
            } else if let Some((_, control, back, enabled)) = app
                .completion_index_ui
                .buttons
                .iter()
                .find(|(r, _, _, _)| r.contains(point))
                .copied()
            {
                app.completion_index_ui.selected = control;
                if enabled {
                    activate(app, control, back);
                }
            } else if let Some((_, control)) = app
                .completion_index_ui
                .values
                .iter()
                .find(|(r, _)| r.contains(point))
                .copied()
            {
                begin_number(app, control);
            } else if let Some(row) = app
                .completion_index_ui
                .file_rows
                .iter()
                .find(|r| r.area.contains(point))
                .cloned()
            {
                app.completion_index_ui.file_selected = row.ordinal;
                if row.action.contains(point) {
                    file_action(app, row);
                }
            } else if let Some((_, control)) = app
                .completion_index_ui
                .controls
                .iter()
                .find(|(r, _)| r.contains(point))
                .copied()
            {
                app.completion_index_ui.selected = control;
            }
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let state = &mut app.completion_index_ui;
            let down = mouse.kind == MouseEventKind::ScrollDown;
            if state.page == Page::Files {
                move_file_selection(state, if down { 1 } else { -1 });
            } else {
                state.selected = state
                    .selected
                    .saturating_add_signed(if down { 1 } else { -1 })
                    .clamp(state.page.first(), state.page.first() + 2);
            }
        }
        _ => {}
    }
    true
}

fn draw_files(f: &mut Frame, app: &mut App, area: Rect, normal: Style, dim: Color, bg: Color) {
    if area.width < 6 || area.height < 2 {
        return;
    }
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    refresh_file_page(app, body.height as usize);
    let state = &mut app.completion_index_ui;
    state.file_area = body;
    let cache = state.file_cache.as_ref().unwrap();
    let label = if area.width >= 14 {
        "Source file"
    } else {
        "File"
    };
    let heading = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(
        Paragraph::new(clip_middle(label, area.width as usize)).style(normal.fg(dim)),
        heading,
    );
    if cache.page.total > body.height as usize {
        let range = format!(
            "{}–{} / {}",
            state.file_scroll + 1,
            (state.file_scroll + body.height as usize).min(cache.page.total),
            cache.page.total,
        );
        let width = unicode_width::UnicodeWidthStr::width(range.as_str()) as u16;
        if width + label.len() as u16 + 2 <= heading.width {
            f.render_widget(
                Paragraph::new(range).style(normal.fg(dim)),
                Rect::new(heading.right() - width, heading.y, width, 1),
            );
        }
    }
    if cache.page.paths.is_empty() {
        let text =
            if LearningMode::from_settings(&app.completion_index.settings()) == LearningMode::On {
                "No learned files yet. Open a file to add it."
            } else {
                "No learned files yet. Start learning to add files."
            };
        f.render_widget(
            Paragraph::new(text)
                .style(normal.fg(dim))
                .wrap(Wrap { trim: true }),
            body,
        );
        return;
    }
    let skip = state.file_scroll.saturating_sub(cache.page.offset);
    for (screen_row, (offset, path)) in cache
        .page
        .paths
        .iter()
        .enumerate()
        .skip(skip)
        .take(body.height as usize)
        .enumerate()
    {
        let ordinal = cache.page.offset + offset;
        let row = Rect::new(body.x, body.y + screen_row as u16, body.width, 1);
        let selected = ordinal == state.file_selected;
        let style = app.colors.ui.list_row_style(
            app.colors.ui.widget_foreground,
            bg,
            selected,
            true,
            row.contains((app.mouse_col, app.mouse_row).into()),
        );
        let action_text = if row.width >= 22 { " Remove " } else { " - " };
        let action_width = action_text.len() as u16;
        let action = Rect::new(row.right() - action_width, row.y, action_width, 1);
        let label = format!(
            "{} {}",
            if selected { "›" } else { " " },
            app.remote_workspace
                .as_ref()
                .and_then(|remote| remote.display_path(path).ok())
                .unwrap_or_else(|| path
                    .strip_prefix(&app.explorer.root)
                    .unwrap_or(path)
                    .display()
                    .to_string())
        );
        f.render_widget(
            Paragraph::new(clip_middle(
                &label,
                row.width.saturating_sub(action_width) as usize,
            ))
            .style(style),
            row,
        );
        let action_style = app.colors.ui.toolbar_layer_style(
            app.colors.ui.widget_foreground,
            bg,
            action.contains((app.mouse_col, app.mouse_row).into()),
            false,
            false,
        );
        f.render_widget(Paragraph::new(action_text).style(action_style), action);
        state.file_rows.push(FileRow {
            area: row,
            action,
            ordinal,
            path: path.clone(),
            revision: cache.page.revision,
        });
    }
}

#[path = "ui_vtcomplete_index_view.rs"]
mod view;
pub(crate) use view::draw;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn file_app(count: usize) -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let mut app = App::test_new();
        app.explorer.root = root.clone();
        app.completion_index = crate::completion_index::CompletionIndex::new_for_test(
            root.clone(),
            directory.path().join("cache"),
        );
        app.completion_index.configure(IndexSettings {
            paused: false,
            speed: IndexSpeed::Fast,
            ..Default::default()
        });
        for n in 0..count {
            let path = root.join(format!("file{n:02}.custom"));
            let text = format!("identifier{n} identifier{n}");
            std::fs::write(&path, &text).unwrap();
            assert!(
                app.completion_index
                    .observe_buffer(path, ropey::Rope::from_str(&text), true)
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while app.completion_index.status().indexed_files != count {
            assert!(std::time::Instant::now() < deadline, "index did not settle");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        open(&mut app);
        app.completion_index_ui.show_files();
        (directory, app)
    }

    fn click(app: &mut App, area: Rect) {
        assert!(!area.is_empty());
        handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    fn wait_file_count(app: &App, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let status = app.completion_index.status();
            assert!(status.error.is_none(), "{status:?}");
            if app.completion_index.file_page(0, 0).total == count
                && status.pending_files == 0
                && status.settings == app.completion_index.settings()
            {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "{status:?}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn file_inventory_pages_all_files_and_keyboard_removal_erases_the_selected_record() {
        let (_directory, mut app) = file_app(36);
        wait_file_count(&app, 36);
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.completion_index_ui.file_total(), 36);
        assert!(
            app.completion_index_ui
                .file_cache
                .as_ref()
                .unwrap()
                .page
                .paths
                .len()
                <= FILE_PAGE_SIZE
        );
        let revision = app
            .completion_index_ui
            .file_cache
            .as_ref()
            .unwrap()
            .page
            .revision;
        let storage = app
            .completion_index_ui
            .file_cache
            .as_ref()
            .unwrap()
            .page
            .paths
            .as_ptr();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        if app
            .completion_index_ui
            .file_cache
            .as_ref()
            .unwrap()
            .page
            .revision
            == revision
        {
            assert_eq!(
                app.completion_index_ui
                    .file_cache
                    .as_ref()
                    .unwrap()
                    .page
                    .paths
                    .as_ptr(),
                storage
            );
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.completion_index_ui.file_selected, 35);
        handle_key(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(
            app.completion_index.file_page(0, 0).total,
            36,
            "an unpainted row is not an action target"
        );
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let last = app
            .completion_index_ui
            .file_rows
            .iter()
            .find(|row| row.ordinal == 35)
            .unwrap()
            .path
            .clone();
        handle_key(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        wait_file_count(&app, 35);
        assert_eq!(
            std::fs::read_to_string(&last).unwrap(),
            "identifier35 identifier35"
        );
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.completion_index_ui.file_total(), 35);
        assert_eq!(app.completion_index_ui.file_selected, 34);
        assert!(
            app.completion_index_ui
                .file_rows
                .iter()
                .any(|row| row.ordinal == 34)
        );
        handle_key(&mut app, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.completion_index_ui.file_total(), 35);
        assert!(
            !app.completion_index_ui
                .file_rows
                .iter()
                .any(|row| row.path == last)
        );
    }

    #[test]
    fn pointer_file_actions_keep_the_painted_path_across_index_publication() {
        let (_directory, mut app) = file_app(4);
        wait_file_count(&app, 4);
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let first = app.completion_index_ui.file_rows[0].path.clone();
        let second = app.completion_index_ui.file_rows[1].clone();
        let third = app.completion_index_ui.file_rows[2].path.clone();
        click(&mut app, second.area);
        assert_eq!(app.completion_index_ui.file_selected, second.ordinal);
        assert_eq!(app.completion_index.file_page(0, 0).total, 4);
        assert!(app.completion_index.remove_file(&first));
        wait_file_count(&app, 3);
        // The backend now has another row at ordinal one. Do not redraw before
        // acting on the path that was painted beneath the pointer.
        click(&mut app, second.action);
        wait_file_count(&app, 2);
        let paths = app.completion_index.file_page(0, 32).paths;
        assert!(!paths.contains(&first));
        assert!(!paths.contains(&second.path));
        assert!(paths.contains(&third));
    }

    #[test]
    fn tiny_file_pane_has_visible_hit_targets_and_clears_stale_delete_targets() {
        let (_directory, mut app) = file_app(3);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        for (width, height) in [(45, 18), (18, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(!app.completion_index_ui.file_rows.is_empty());
            handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(
                app.completion_index_ui
                    .file_rows
                    .iter()
                    .any(|row| row.ordinal == 2),
                "a short bounded backend page must still scroll inside a tiny file pane"
            );
            for row in &app.completion_index_ui.file_rows {
                assert!(row.action.width >= 3);
                assert!(
                    app.completion_index_ui
                        .area
                        .contains((row.action.x, row.action.y).into())
                );
                assert!(row.action.right() < app.completion_index_ui.area.right());
                assert!(row.action.bottom() < app.completion_index_ui.area.bottom());
                assert!(row.area.y >= app.completion_index_ui.file_area.y);
                assert!(row.area.bottom() <= app.completion_index_ui.file_area.bottom());
                let action: String = (row.action.x..row.action.right())
                    .map(|x| terminal.backend().buffer()[(x, row.action.y)].symbol())
                    .collect();
                assert!(
                    matches!(action.trim(), "Remove" | "-"),
                    "visible action must match its hit target"
                );
            }
            if width == 18 {
                let first = &app.completion_index_ui.file_rows[0];
                assert_eq!(first.ordinal, app.completion_index_ui.file_scroll);
                assert_eq!(
                    first.area.y, app.completion_index_ui.file_area.y,
                    "skipping cached rows must not skip screen rows"
                );
                let footer_y = app.completion_index_ui.area.bottom() - 2;
                let footer: String = (app.completion_index_ui.area.x + 1
                    ..app.completion_index_ui.area.right() - 1)
                    .map(|x| terminal.backend().buffer()[(x, footer_y)].symbol())
                    .collect();
                assert!(
                    footer.contains("Esc close"),
                    "the narrow pane must keep its close shortcut visible"
                );
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(8, 5)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(app.completion_index_ui.file_rows.is_empty());
        handle_key(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.completion_index.file_page(0, 0).total, 3);
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_ne!(app.completion_index_ui.page, Page::Files);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        );
        assert_eq!(app.completion_index_ui.page, Page::Files);
    }

    #[test]
    fn file_erasure_remains_available_while_processing_is_paused_or_disabled() {
        for (enabled, paused) in [(true, true), (false, false)] {
            let (_directory, mut app) = file_app(1);
            wait_file_count(&app, 1);
            let mut settings = app.completion_index.settings();
            settings.enabled = enabled;
            settings.paused = paused;
            app.completion_index.configure(settings);
            wait_file_count(&app, 1);
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            );
            let mut terminal = Terminal::new(TestBackend::new(45, 18)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let path = app.completion_index_ui.file_rows[0].path.clone();
            handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            wait_file_count(&app, 0);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(app.completion_index_ui.file_rows.is_empty());
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                "identifier0 identifier0"
            );
        }
    }

    #[test]
    fn resource_controls_step_limits_without_wrapping() {
        assert_eq!(step_limit(5000, &[1000, 5000, 10000], false), 10000);
        assert_eq!(step_limit(5000, &[1000, 5000, 10000], true), 1000);
        assert_eq!(step_limit(10000, &[1000, 5000, 10000], false), 10000);
        assert_eq!(step_limit(1000, &[1000, 5000, 10000], true), 1000);
    }

    #[test]
    fn learning_state_and_available_action_stay_clear_across_themes_and_sizes() {
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let mut app = App::test_new();
            app.colors = theme.colors();
            open(&mut app);
            for (width, height) in [(110, 32), (70, 24), (46, 18), (40, 16)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                for (mode, phase, state, label) in [
                    ("On", IndexPhase::Idle, "Learning on", "Pause learning"),
                    ("On", IndexPhase::Indexing, "Learning on", "Pause learning"),
                    ("On", IndexPhase::Limited, "Learning on", "Pause learning"),
                    (
                        "Paused",
                        IndexPhase::Paused,
                        "Learning paused",
                        "Start learning",
                    ),
                    (
                        "Paused",
                        IndexPhase::Indexing,
                        "Learning paused",
                        "Start learning",
                    ),
                    (
                        "Off",
                        IndexPhase::Disabled,
                        "Learning off",
                        "Start learning",
                    ),
                ] {
                    app.set_completion_dropdown("vtcomplete_learning_mode", mode);
                    let settings = app.completion_index.settings();
                    let snapshot = IndexStatus {
                        phase,
                        indexed_files: 3,
                        ..IndexStatus::default()
                    };
                    terminal
                        .draw(|f| {
                            view::draw_snapshot(f, &mut app, snapshot.clone(), settings.clone())
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                    assert!(
                        text.contains("VTComplete"),
                        "{width}×{height} must identify the provider"
                    );
                    assert!(text.contains(state), "{width}×{height}, {mode}, {phase:?}");
                    for clutter in [
                        "↑↓ select",
                        "←→ change",
                        "Tab pages",
                        "[P]",
                        "0 pending",
                        "0 skipped",
                    ] {
                        assert!(!text.contains(clutter), "unexpected clutter: {clutter}");
                    }
                    let (rect, _, enabled) = app
                        .completion_index_ui
                        .actions
                        .iter()
                        .find(|(_, action, _)| *action == Action::ToggleLearning)
                        .copied()
                        .unwrap();
                    assert!(enabled);
                    let painted: String = (rect.x..rect.right())
                        .map(|x| buffer[(x, rect.y)].symbol())
                        .collect();
                    assert!(
                        painted.contains(label),
                        "action must match its painted hit target"
                    );
                    let cell = &buffer[(rect.x + 3, rect.y)];
                    let ratio = crate::ui_colors::contrast_ratio(cell.fg, cell.bg).unwrap();
                    assert!(ratio >= 4.5, "enabled action must remain readable: {ratio}");
                    assert_ne!(
                        cell.bg,
                        buffer[(rect.x, rect.y + 1)].bg,
                        "an available action needs a visible button fill"
                    );
                    if width == 46 {
                        assert_eq!(app.completion_index_ui.controls.len(), 3);
                    }
                    for (target, _, _) in &app.completion_index_ui.actions {
                        assert_eq!(target.intersection(app.completion_index_ui.area), *target);
                        assert!(target.bottom() < app.completion_index_ui.area.bottom());
                    }
                }
            }
        }
    }

    #[test]
    fn primary_learning_button_updates_both_the_mode_and_the_visible_action() {
        let mut app = App::test_new();
        open(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(46, 18)).unwrap();
        for (initial, next) in [
            ("On", LearningMode::Paused),
            ("Paused", LearningMode::On),
            ("Off", LearningMode::On),
        ] {
            app.set_completion_dropdown("vtcomplete_learning_mode", initial);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let rect = app
                .completion_index_ui
                .actions
                .iter()
                .find(|(_, action, _)| *action == Action::ToggleLearning)
                .unwrap()
                .0;
            click(&mut app, rect);
            assert_eq!(
                LearningMode::from_settings(&app.completion_index.settings()),
                next
            );
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains(if next == LearningMode::On {
                "Pause learning"
            } else {
                "Start learning"
            }));
            assert!(text.contains(if next == LearningMode::On {
                "Learning on"
            } else {
                "Learning paused"
            }));
        }
    }

    #[test]
    fn remote_learning_controls_and_file_removal_work_without_offering_a_workspace_scan() {
        let temp = tempfile::tempdir().unwrap();
        let remote = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
            temp.path(),
            Default::default(),
            Default::default(),
        ));
        let path = remote.target_key("remote-file.rs").unwrap();
        let mut app = App::new_deferred(remote.view_root.clone());
        app.remote_workspace = Some(remote.clone());
        app.settings_persistence_enabled = true;
        app.poll_completion_index();
        app.completion_index.observe_buffer(
            path,
            ropey::Rope::from_str("remoteSharedIdentifier"),
            false,
        );
        wait_file_count(&app, 1);
        assert_eq!(
            app.setting_dropdown_options("vtcomplete_sources")
                .unwrap(),
            ["Files you open"]
        );
        assert!(!app.set_completion_dropdown("vtcomplete_sources", "Entire workspace"));
        open(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(46, 18)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(
            app.completion_index_ui
                .choices
                .iter()
                .all(|(_, control, _)| *control != 1)
        );
        if let Some(output) = std::env::var_os("VTCODE_COMPLETION_PREVIEW_DIR") {
            let output = std::path::PathBuf::from(output);
            std::fs::create_dir_all(&output).unwrap();
            capture(&terminal, &output.join("index-remote-dark-46.json"));
        }
        let primary = app
            .completion_index_ui
            .actions
            .iter()
            .find(|(_, action, enabled)| *action == Action::ToggleLearning && *enabled)
            .unwrap()
            .0;
        click(&mut app, primary);
        assert_eq!(
            LearningMode::from_settings(&app.completion_index.settings()),
            LearningMode::Paused
        );
        begin_number(&mut app, 5);
        handle_paste(&mut app, "16");
        assert!(commit_number(&mut app));
        assert_eq!(app.completion_index.settings().max_memory_mb, 16);
        app.completion_index_ui.select_page(Page::Learning);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Start learning"));
        assert!(!text.contains("unavailable"));
        assert!(!text.contains("Entire workspace"));
        app.completion_index_ui.show_files();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let row = app.completion_index_ui.file_rows.first().unwrap().clone();
        click(&mut app, row.action);
        wait_file_count(&app, 0);
        assert!(app.completion_index.status().error.is_none());
        remote
            .client
            .call(crate::remote::Command::Shutdown)
            .unwrap();
    }

    #[test]
    fn limit_editor_keeps_units_and_rejects_invalid_values_at_narrow_widths() {
        let mut app = App::test_new();
        open(&mut app);
        for (control, unit, bad) in [
            (3, "files", "0"),
            (4, "MiB per file", "1025"),
            (5, "MiB RAM", "3"),
        ] {
            begin_number(&mut app, control);
            let before = control_rows(&app.completion_index.settings())[control]
                .1
                .clone();
            handle_paste(&mut app, bad);
            assert!(!commit_number(&mut app));
            assert_eq!(
                control_rows(&app.completion_index.settings())[control].1,
                before
            );
            for (width, height) in [(100, 35), (46, 18), (40, 16)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|f| draw(f, &mut app)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(
                    text.contains(unit),
                    "units must stay outside the editor: {width} {control}"
                );
                assert!(text.contains("Enter "), "range validation must be visible");
                assert!(
                    app.completion_index_ui
                        .editing
                        .as_ref()
                        .unwrap()
                        .hit_map
                        .is_some()
                );
            }
            handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(app.completion_index_ui.open);
        }
    }

    #[test]
    fn tabs_do_not_leave_invisible_actions_and_narrow_choices_remain_reachable() {
        let mut app = App::test_new();
        open(&mut app);
        app.completion_index_ui.selected = 1;
        let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let scope = app
            .completion_index_ui
            .choices
            .iter()
            .find(|(_, c, option)| *c == 1 && *option == 1)
            .unwrap()
            .0;
        click(&mut app, scope);
        assert!(app.completion_index.settings().project_wide);
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.completion_index_ui.page, Page::Limits);
        assert!(app.completion_index_ui.choices.is_empty());
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(!app.completion_index_ui.values.is_empty());
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.completion_index_ui.page, Page::Files);
        assert!(app.completion_index_ui.values.is_empty());
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.completion_index_ui.page, Page::Learning);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        );
        assert_eq!(app.completion_index_ui.page, Page::Files);
    }

    #[test]
    fn opening_index_controls_dismisses_editor_suggestions() {
        let mut app = App::test_new();
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = vec!["value".into()];
        app.lsp_completion_active = true;
        app.lsp_signature_active = true;
        open(&mut app);
        assert!(!app.vtcomplete_active);
        assert!(app.vtcomplete_suggestions.is_empty());
        assert!(!app.lsp_completion_active);
        assert!(!app.lsp_signature_active);
        assert!(app.completion_index_ui.open);
    }

    #[test]
    fn indexing_dialog_renders_and_keeps_keyboard_focus_on_small_screens() {
        let mut app = App::test_new();
        open(&mut app);
        app.completion_index_ui.select_page(Page::Limits);
        for (width, height) in [(100, 35), (45, 18), (18, 8), (2, 2)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(handle_key(
                &mut app,
                KeyEvent::new(KeyCode::End, KeyModifiers::NONE)
            ));
            assert_eq!(app.completion_index_ui.selected, CONTROL_COUNT - 1);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
        }
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        assert!(!app.completion_index_ui.open);
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
        ));
    }

    #[test]
    fn keyboard_and_pointer_controls_change_project_settings_and_keep_edits_out() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let mut app = App::test_new();
        app.completion_index = crate::completion_index::CompletionIndex::new_for_test(
            project,
            directory.path().join("cache"),
        );
        let editor = crate::editor::Editor::test_with_text("unchanged");
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.active_tab = 0;
        open(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let scope = app
            .completion_index_ui
            .controls
            .iter()
            .find(|(_, index)| *index == 1)
            .unwrap()
            .0;
        let pointer = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert!(handle_mouse(
            &mut app,
            pointer(MouseEventKind::Moved, scope.x + 3, scope.y)
        ));
        assert!(app.mouse_hover_changed);
        assert_eq!((app.mouse_col, app.mouse_row), (scope.x + 3, scope.y));
        assert!(handle_mouse(
            &mut app,
            pointer(MouseEventKind::Moved, scope.x + 4, scope.y)
        ));
        assert!(
            !app.mouse_hover_changed,
            "moving within a control needs no redraw"
        );
        assert!(handle_mouse(
            &mut app,
            pointer(
                MouseEventKind::Down(MouseButton::Left),
                scope.x + 4,
                scope.y
            )
        ));
        assert!(!app.completion_index.settings().project_wide);
        let scope_next = app
            .completion_index_ui
            .choices
            .iter()
            .find(|(_, control, option)| *control == 1 && *option == 1)
            .unwrap()
            .0;
        handle_mouse(
            &mut app,
            pointer(
                MouseEventKind::Down(MouseButton::Left),
                scope_next.x + 1,
                scope_next.y,
            ),
        );
        assert!(app.completion_index.settings().project_wide);
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.completion_index.settings().speed, IndexSpeed::Low);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );
        assert!(app.completion_index.settings().paused);
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert_eq!(app.editor().unwrap().rope.to_string(), "unchanged");
        handle_mouse(
            &mut app,
            pointer(MouseEventKind::Down(MouseButton::Left), 0, 0),
        );
        assert!(!app.completion_index_ui.open);
    }

    #[test]
    fn narrow_dialog_scrolls_selected_controls_into_the_viewport() {
        let mut app = App::test_new();
        open(&mut app);
        app.completion_index_ui.select_page(Page::Limits);
        handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(
            app.completion_index_ui
                .controls
                .iter()
                .any(|(_, index)| *index == CONTROL_COUNT - 1)
        );
        let area = app.completion_index_ui.area;
        assert!(
            app.completion_index_ui
                .controls
                .iter()
                .all(|(rect, _)| rect.y > area.y && rect.bottom() < area.bottom())
        );
        assert!(app.completion_index_ui.controls.len() < CONTROL_COUNT);
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("VTComplete learning"));
        assert!(text.contains("Esc close"));
    }

    #[test]
    fn pointer_arrows_and_direct_number_editing_work_in_both_directions() {
        let mut app = App::test_new();
        open(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        let click = |app: &mut App, rect: Rect| {
            handle_mouse(
                app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x + 1,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
        };
        app.completion_index_ui.select_page(Page::Limits);
        for control in [3, 4, 5] {
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let before = control_rows(&app.completion_index.settings())[control]
                .1
                .clone();
            let back = app
                .completion_index_ui
                .buttons
                .iter()
                .find(|(_, c, back, _)| *c == control && *back)
                .unwrap()
                .0;
            click(&mut app, back);
            assert_ne!(
                control_rows(&app.completion_index.settings())[control].1,
                before
            );
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let next = app
                .completion_index_ui
                .buttons
                .iter()
                .find(|(_, c, back, _)| *c == control && !*back)
                .unwrap()
                .0;
            click(&mut app, next);
            assert_eq!(
                control_rows(&app.completion_index.settings())[control].1,
                before
            );
        }
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let number = app
            .completion_index_ui
            .values
            .iter()
            .find(|(_, c)| *c == 3)
            .unwrap()
            .0;
        click(&mut app, number);
        assert!(app.completion_index_ui.editing.is_some());
        handle_paste(&mut app, "12345");
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.completion_index.settings().max_files, 12_345);
        begin_number(&mut app, 4);
        handle_paste(&mut app, "1024");
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.completion_index.settings().max_file_bytes,
            crate::completion_index::MAX_FILE_BYTES
        );
        begin_number(&mut app, 4);
        handle_paste(&mut app, "10");
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.completion_index_ui.open);
        assert_eq!(
            app.completion_index.settings().max_file_bytes,
            crate::completion_index::MAX_FILE_BYTES
        );
        begin_number(&mut app, 3);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert!(!commit_number(&mut app));
        click(&mut app, Rect::new(0, 0, 2, 1));
        assert!(
            !app.completion_index_ui.open,
            "invalid edits must never trap focus"
        );
        open(&mut app);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let settings = app.completion_index_ui.settings_button;
        click(&mut app, settings);
        assert!(!app.completion_index_ui.open);
        assert_eq!(app.settings_selected_section(1), Some("VTComplete"));
        assert!(
            app.settings_items()
                .iter()
                .any(|(_, value, id)| id == "vtcomplete_max_files" && value == "12345")
        );
    }

    #[test]
    fn preview_completion_surfaces_dark_light_and_narrow() {
        let Some(output) = std::env::var_os("VTCODE_COMPLETION_PREVIEW_DIR") else {
            return;
        };
        let output = std::path::PathBuf::from(output);
        std::fs::create_dir_all(&output).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("atlas");
        std::fs::create_dir(&project).unwrap();
        let index = crate::completion_index::CompletionIndex::new_for_test(
            project.clone(),
            directory.path().join("cache"),
        );
        for (name, text) in [
            ("api.rs", "client.getUser(); client.getUsers();"),
            ("state.rs", "user_settings = getUserSettings();"),
            ("main.rs", "client.getUser(); client.getURL();"),
        ] {
            let path = project.join(name);
            std::fs::write(&path, text).unwrap();
            index.observe(path, ropey::Rope::from_str(text));
        }
        let start = std::time::Instant::now();
        while index.status().indexed_files < 3
            && start.elapsed() < std::time::Duration::from_secs(3)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(index.status().indexed_files, 3);
        let mut settings = index.settings();
        settings.paused = true;
        index.configure(settings);
        for (name, theme) in [
            ("dark", crate::app_theme::EditorTheme::Dark),
            ("light", crate::app_theme::EditorTheme::Light),
        ] {
            for (width, height) in [(110, 32), (46, 18)] {
                let mut settings = index.settings();
                settings.enabled = true;
                settings.paused = true;
                index.configure(settings);
                let mut app = App::test_new();
                app.completion_index = index.clone();
                app.colors = theme.colors();
                app.explorer.root = project.clone();
                app.show_panel = false;
                app.show_explorer = false;
                app.show_minimap = false;
                app.chat.visible = false;
                let text = "fn load_profile(client: &Client) {\n    let settings = getUserSettings();\n    client.getUsers();\n    client.gU\n}\n";
                let mut editor = crate::editor::Editor::test_with_text(text);
                editor.language_override = Some("Rust".into());
                editor.file_path = Some(project.join("profile.rs"));
                editor.cursor_line = 3;
                editor.cursor_col = 13;
                app.tabs.push(crate::app::Tab::new_file(editor));
                app.active_tab = 0;
                app.vtcomplete_active = true;
                app.vtcomplete_suggestions = [
                    "getUser()",
                    "getUsers()",
                    "getUserSettings()",
                    "getURL()",
                    "getUserName",
                    "getUserProfile()",
                ]
                .into_iter()
                .map(str::to_string)
                .collect();
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                capture(
                    &terminal,
                    &output.join(format!("completion-{name}-{width}.json")),
                );
                open(&mut app);
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                capture(
                    &terminal,
                    &output.join(format!("index-{name}-{width}.json")),
                );
                for (page, label) in [(Page::Limits, "limits"), (Page::Files, "files")] {
                    app.completion_index_ui.select_page(page);
                    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                    capture(
                        &terminal,
                        &output.join(format!("index-{label}-{name}-{width}.json")),
                    );
                }
                app.completion_index_ui.select_page(Page::Learning);
                for (label, phase, mode, pending, error) in [
                    ("empty", IndexPhase::Paused, LearningMode::Paused, 0, None),
                    ("off", IndexPhase::Disabled, LearningMode::Off, 0, None),
                    ("on", IndexPhase::Idle, LearningMode::On, 0, None),
                    ("working", IndexPhase::Indexing, LearningMode::On, 12, None),
                    ("limited", IndexPhase::Limited, LearningMode::On, 0, None),
                    (
                        "error",
                        IndexPhase::Idle,
                        LearningMode::On,
                        0,
                        Some(
                            "Could not save the disk cache: access denied. Learned data remains in RAM.",
                        ),
                    ),
                ] {
                    let mut snapshot = index.status();
                    snapshot.phase = phase;
                    snapshot.pending_files = pending;
                    snapshot.error = error.map(str::to_owned);
                    snapshot.settings.enabled = mode != LearningMode::Off;
                    snapshot.settings.paused = mode != LearningMode::On;
                    if label == "empty" {
                        snapshot.indexed_files = 0;
                        snapshot.memory_bytes = 0;
                    }
                    if label == "working" {
                        snapshot.current_file = Some(project.join("state.rs"));
                        snapshot.current_file_bytes = 1048576;
                        snapshot.processed_bytes = 524288;
                    }
                    let settings = snapshot.settings.clone();
                    index.configure(settings.clone());
                    terminal
                        .draw(|f| {
                            crate::ui::draw(f, &mut app);
                            view::draw_snapshot(f, &mut app, snapshot.clone(), settings.clone());
                        })
                        .unwrap();
                    capture(
                        &terminal,
                        &output.join(format!("index-{label}-{name}-{width}.json")),
                    );
                }
                app.completion_index_ui.selected = 3;
                begin_number(&mut app, 3);
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                capture(
                    &terminal,
                    &output.join(format!("index-edit-{name}-{width}.json")),
                );
                begin_number(&mut app, 5);
                handle_paste(&mut app, "1024");
                assert!(!commit_number(&mut app));
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                capture(
                    &terminal,
                    &output.join(format!("index-invalid-{name}-{width}.json")),
                );
                app.open_completion_settings();
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                capture(
                    &terminal,
                    &output.join(format!("settings-{name}-{width}.json")),
                );
                app.settings_editor_search = "Source File Size".into();
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                capture(
                    &terminal,
                    &output.join(format!("settings-size-{name}-{width}.json")),
                );
            }
        }
    }

    fn capture(terminal: &Terminal<TestBackend>, path: &std::path::Path) {
        let buffer = terminal.backend().buffer();
        let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
            "text": cell.symbol(), "fg": format!("{:?}",cell.fg), "bg": format!("{:?}",cell.bg),
            "bold": cell.modifier.contains(Modifier::BOLD),
            "underline": cell.modifier.contains(Modifier::UNDERLINED),
        })).collect();
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "width":buffer.area.width,"height":buffer.area.height,"cells":cells,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
