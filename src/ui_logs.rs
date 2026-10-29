//! Incremental, on-demand Output panel. Logging continues while hidden;
//! only visible output subscribes to UI wakeups. No polling or remote requests.
use crate::{
    app::{ActivePane, App},
    editor::Editor,
    remote::diagnostics::Log,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};
use std::collections::{BTreeMap, VecDeque};
#[path = "ui_output_text.rs"]
mod text;
#[path = "ui_output_layout.rs"]
mod layout;
#[path = "ui_output_styles.rs"]
mod styles;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    #[default]
    RemoteSsh,
    Git,
}
impl Source {
    const ALL: [Self; 2] = [Self::RemoteSsh, Self::Git];
    fn label(self) -> &'static str {
        match self {
            Self::RemoteSsh => "Remote SSH Connection",
            Self::Git => "Git",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Source,
    Channel(Source),
    CaptureCpu,
    Copy,
    Autoscroll,
    Wrap,
    Clear,
    More,
}
#[derive(Default)]
pub(crate) struct State {
    log: Option<Log>,
    source: Source,
    git: Log,
    remote_log: Option<Log>,
    saved_readers: BTreeMap<Source, Box<text::Reader>>,
    observer: Option<Log>,
    visible: bool,
    seen: u64,
    reader: Option<Box<text::Reader>>,
    controls: Vec<(Rect, Control)>,
    focus: Option<Control>,
    dropdown: Option<Control>,
    dropdown_area: Option<Rect>,
    selected: usize,
}
struct View {
    log: Option<Log>,
    version: u64,
    entries: VecDeque<(u64, usize, usize)>,
    widths: BTreeMap<usize, usize>,
    omitted: u64,
    autoscroll: bool,
    generation: u64,
    first_line: u64,
}
impl Default for View {
    fn default() -> Self {
        Self {
            log: None,
            version: u64::MAX,
            entries: VecDeque::new(),
            widths: BTreeMap::new(),
            omitted: 0,
            autoscroll: true,
            generation: 0,
            first_line: 0,
        }
    }
}
fn end(editor: &mut Editor) {
    editor.selection_anchor = None;
    editor.extra_cursors.clear();
    editor.extra_cursor_anchors.clear();
    let line = editor.rope.len_lines().saturating_sub(1);
    editor.set_cursor_without_scrolling(line, Editor::line_content_len(editor.rope.line(line)));
    editor.scroll_offset = line.saturating_sub(editor.viewport_height.saturating_sub(1));
    editor.wrap_scroll_offset = 0;
}
impl View {
    fn refresh(&mut self, editor: &mut Editor, log: Option<&Log>) {
        let Some(log) = log else {
            return;
        };
        let reset = self.log.as_ref().is_none_or(|previous| !previous.same(log));
        // Reading old output or selecting text must pause autoscroll.
        if !reset
            && (editor.selection_anchor.is_some()
                || !editor.extra_cursors.is_empty()
                || editor.cursor_line + 1 < editor.rope.len_lines())
        {
            self.autoscroll = false;
        }
        if !reset && self.version == log.version() {
            return;
        }
        // A retained MiB can contain thousands of timestamped records. Build it
        // over bounded frames so opening Logs never formats it all on input's
        // latency budget. finish_frame schedules the next batch while visible.
        let snapshot = log.batch(if reset { 0 } else { self.version }, 128, 64 * 1024);
        let mut removed = 0;
        if reset {
            self.generation = self.generation.wrapping_add(1);
            self.first_line = 0;
            self.entries.clear();
            self.widths.clear();
            removed = editor.rope.len_chars();
            self.log = Some(log.clone());
        } else {
            while self
                .entries
                .front()
                .is_some_and(|(id, _, _)| *id < snapshot.first)
            {
                let (_, chars, width) = self.entries.pop_front().unwrap();
                removed += chars;
                if let Some(count) = self.widths.get_mut(&width) {
                    *count -= 1;
                    if *count == 0 {
                        self.widths.remove(&width);
                    }
                }
            }
        }
        let mut appended = String::new();
        for (id, text) in snapshot.entries {
            let width = text.lines().map(text::line_width).max().unwrap_or(0);
            *self.widths.entry(width).or_default() += 1;
            self.entries
                .push_back((id, text.chars().count() + 1, width));
            appended.push_str(&text);
            appended.push('\n');
        }
        if removed > 0 || !appended.is_empty() {
            let removed_lines = editor.rope.char_to_line(removed);
            if !reset { self.first_line = self.first_line.saturating_add(removed_lines as u64); }
            let dirty_from = if removed == 0 {
                editor.rope.len_lines().saturating_sub(1)
            } else {
                0
            };
            if removed > 0 {
                editor.rope.remove(..removed);
                editor.folded_ranges.clear();
                editor.bookmarks = editor
                    .bookmarks
                    .iter()
                    .filter_map(|line| line.checked_sub(removed_lines))
                    .collect();
            }
            editor.rope.insert(editor.rope.len_chars(), &appended);
            editor.content_version = editor.content_version.wrapping_add(1);
            editor.file_size = editor.rope.len_bytes() as u64;
            editor.cached_visible_line_count = None;
            editor.cached_wrapped_total = None;
            editor.cached_row_offsets = None;
            editor.mark_syntax_dirty_range(dirty_from, editor.rope.len_lines().saturating_sub(1));
            let clamp = |(line, col): (usize, usize)| {
                if line < removed_lines {
                    return (0, 0);
                }
                let line = line
                    .saturating_sub(removed_lines)
                    .min(editor.rope.len_lines() - 1);
                (
                    line,
                    col.min(Editor::line_content_len(editor.rope.line(line))),
                )
            };
            (editor.cursor_line, editor.cursor_col) =
                clamp((editor.cursor_line, editor.cursor_col));
            editor.selection_anchor = editor.selection_anchor.map(clamp);
            for cursor in &mut editor.extra_cursors {
                *cursor = clamp(*cursor);
            }
            for anchor in editor.extra_cursor_anchors.iter_mut().flatten() {
                *anchor = clamp(*anchor);
            }
            editor.scroll_offset = editor.scroll_offset.saturating_sub(removed_lines);
            editor.modified = false;
        }
        if self.autoscroll {
            end(editor);
        }
        self.version = snapshot.version;
        self.omitted = snapshot.omitted;
    }
}
impl State {
    pub(crate) fn git_log(&self) -> Log {
        self.git.clone()
    }
    fn select_source(&mut self, source: Source) {
        if self.source != source {
            self.hide();
            if self.source == Source::RemoteSsh {
                self.remote_log = self.log.clone();
            }
            if let Some(reader) = self.reader.take() {
                self.saved_readers.insert(self.source, reader);
            }
            self.reader = self.saved_readers.remove(&source);
            self.source = source;
        }
        self.log = match source {
            Source::RemoteSsh => self.remote_log.clone().or_else(|| self.log.clone()
                .filter(|log| !log.same(&self.git))),
            Source::Git => Some(self.git.clone()),
        };
        self.dropdown = None;
        self.focus = None;
    }
    pub(crate) fn editor(&self) -> Option<&Editor> {
        self.reader.as_ref().map(|reader| &reader.editor)
    }
    pub(crate) fn selected_text(&self) -> Option<String> {
        self.reader
            .as_ref()
            .and_then(|reader| reader.selected_text())
    }
    pub(crate) fn has_selection(&self) -> bool {
        self.reader
            .as_ref()
            .is_some_and(|reader| reader.has_selection())
    }
    pub(crate) fn search_focused(&self) -> bool {
        self.reader
            .as_ref()
            .is_some_and(|reader| reader.search_focused())
    }
    fn reader(&mut self) -> &mut text::Reader {
        self.reader
            .get_or_insert_with(|| Box::new(text::Reader::default()))
    }
    pub(crate) fn hide(&mut self) {
        self.visible = false;
        self.controls.clear();
        self.dropdown = None;
        self.dropdown_area = None;
        self.focus = None;
        if let Some(reader) = &mut self.reader {
            reader.hide();
        }
        if let Some(log) = self.observer.take() {
            log.observe(None);
        }
    }
    pub(crate) fn reset_views(&mut self) {
        self.hide();
        self.reader = None;
        self.saved_readers.clear();
    }
    pub(crate) fn finish_frame(&mut self, wake: crate::background::UiWake) {
        let wanted = self.log.as_ref().filter(|_| self.visible);
        let same = match (&self.observer, wanted) {
            (Some(old), Some(new)) => old.same(new),
            (None, None) => true,
            _ => false,
        };
        if !same {
            if let Some(log) = self.observer.take() {
                log.observe(None);
            }
            if let Some(log) = wanted {
                log.observe(Some(wake.clone()));
                self.observer = Some(log.clone());
            }
        }
        if let Some(log) = &self.observer {
            self.seen = self.reader.as_ref().map_or(0, |reader| reader.view.version);
            if log.version() != self.seen {
                wake.notify();
            }
        }
    }
    pub(crate) fn take_changed(&mut self) -> bool {
        let Some(log) = self.observer.as_ref().filter(|_| self.visible) else {
            return false;
        };
        let version = log.version();
        let changed = version != self.seen;
        self.seen = version;
        changed
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(log) = &self.observer {
            log.observe(None);
        }
    }
}

pub(crate) fn begin_frame(app: &mut App) {
    app.logs.visible = false;
    app.logs.controls.clear();
    app.logs.dropdown_area = None;
    if let Some(log) = app
        .remote_workspace
        .as_ref()
        .and_then(|remote| remote.diagnostics())
        .or_else(|| app.remote_connection.diagnostic_log())
    {
        app.logs.remote_log = Some(log.clone());
        if app.logs.source == Source::RemoteSsh {
            app.logs.log = Some(log);
        }
    }
    if let Some(remote) = &app.remote_workspace {
        remote.client.set_git_output(&app.logs.git);
    }
    if !app.show_panel || app.panel_tab != 2 {
        app.logs.hide();
        if app.active_pane == ActivePane::Panel {
            app.active_pane = ActivePane::Editor;
        }
    }
}
fn blocked(app: &App) -> bool {
    crate::ui_modal::active(app).is_some()
        || app.remote_connection.details
        || app.palette.active
        || app.quick_open_active
        || app.menu_active
        || app.menu_open
        || app.context_menu_active
        || app.tab_context_active
        || app.explorer_context_active
        || app.sc_context_active
        || app.change_dir_active
        || app.desks.popup.mode.is_some()
        || crate::ui_quick_picker::active(app).is_some()
}
pub(crate) fn focused(app: &App) -> bool {
    app.show_panel && app.panel_tab == 2 && app.active_pane == ActivePane::Panel
}

pub(crate) fn dropdown_open(app: &App) -> bool {
    app.show_panel && app.panel_tab == 2 && app.logs.dropdown.is_some()
}

pub(crate) fn draw_content(frame: &mut Frame, app: &mut App, area: Rect) {
    let log = app.logs.log.clone();
    let source = app.logs.source;
    let focused = focused(app);
    let mouse = (app.mouse_col, app.mouse_row);
    let text_focused = focused && app.logs.focus.is_none();
    app.logs.visible = !area.is_empty();
    app.logs
        .reader()
        .draw(frame, &app.colors, area, text_focused, mouse, log.as_ref(), source);
}

pub(crate) fn draw_toolbar(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }
    let remote = app.remote_workspace.as_ref().filter(|_| app.logs.source == Source::RemoteSsh);
    let capturing = remote.is_some_and(|remote| remote.client.capturing_cpu());
    let can_capture = remote.is_some_and(|remote| remote.client.can_capture_cpu()) && !capturing;
    let autoscroll = app
        .logs
        .reader
        .as_ref()
        .is_none_or(|reader| reader.view.autoscroll);
    let bg = app.colors.ui.bg_panel;
    let mut right = area.right();
    let mut take = |width: u16| {
        let width = width.min(right.saturating_sub(area.x));
        let rect = Rect::new(right - width, area.y, width, 1);
        right = rect.x.saturating_sub(1).max(area.x);
        rect
    };
    let more = take(3);
    let autoscroll_rect = if area.width >= 34 {
        take(14)
    } else {
        Rect::default()
    };
    let copy = if area.width >= 48 {
        take(6)
    } else {
        Rect::default()
    };
    let capture = if remote.is_some() && area.width >= 68 {
        take(9)
    } else {
        Rect::default()
    };
    let source = Rect::new(
        right.saturating_sub(28).max(area.x),
        area.y,
        right.saturating_sub(area.x).min(28),
        1,
    );
    for (rect, control, label, disabled) in [
        (source, Control::Source, app.logs.source.label(), false),
        (
            capture,
            Control::CaptureCpu,
            if capturing { "Measuring" } else { "CPU" },
            !can_capture,
        ),
        (copy, Control::Copy, "Copy", app.logs.log.is_none()),
        (
            autoscroll_rect,
            Control::Autoscroll,
            if autoscroll { "✓ Autoscroll" } else { "Autoscroll" },
            app.logs.log.is_none(),
        ),
        (more, Control::More, "⋯", false),
    ] {
        if rect.is_empty() {
            continue;
        }
        let hover = rect.contains((app.mouse_col, app.mouse_row).into());
        let selected = app.logs.focus == Some(control) && focused(app);
        let mut style = app.colors.ui.toolbar_style(
            app.colors.ui.fg_panel_title,
            bg,
            hover,
            selected,
            disabled,
        );
        if control == Control::Source {
            let dropdown = app.colors.ui.dropdown_colors;
            let fill = dropdown.background.over(bg).unwrap_or(bg);
            style = style
                .bg(fill)
                .fg(dropdown.foreground.over(fill).unwrap_or(app.colors.fg_text));
            let text_width = rect.width.saturating_sub(3);
            let label = crate::ui_performance::clip_middle(
                if rect.width < 24 && app.logs.source == Source::RemoteSsh { "Remote SSH" } else { label },
                text_width as usize,
            );
            frame.render_widget(Paragraph::new(format!(" {label}")).style(style), rect);
            let arrow = Rect::new(
                rect.right().saturating_sub(2).max(rect.x),
                rect.y,
                rect.width.min(2),
                1,
            );
            frame.render_widget(
                Paragraph::new(if app.logs.dropdown == Some(Control::Source) {
                    "▴ "
                } else {
                    "▾ "
                })
                .style(style),
                arrow,
            );
        } else {
            frame.render_widget(
                Paragraph::new(label)
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(style),
                rect,
            );
        }
        app.logs.controls.push((rect, control));
    }
}

fn menu_actions(app: &App, menu: Control) -> Vec<(&'static str, Control)> {
    if menu == Control::Source {
        return Source::ALL.into_iter().map(|source| (source.label(), Control::Channel(source))).collect();
    }
    let mut items = vec![
        ("Select Output…", Control::Source),
        ("Copy all output", Control::Copy),
        ("Clear output", Control::Clear),
        (if app.logs.reader.as_ref().is_none_or(|reader| reader.wrap) { "✓ Wrap lines" } else { "Wrap lines" }, Control::Wrap),
        (
            if app
                .logs
                .reader
                .as_ref()
                .is_none_or(|reader| reader.view.autoscroll)
            {
                "✓ Autoscroll"
            } else {
                "Autoscroll"
            },
            Control::Autoscroll,
        ),
    ];
    if app.remote_workspace.is_some() && app.logs.source == Source::RemoteSsh {
        items.push(("Capture headless CPU…", Control::CaptureCpu));
    }
    items
}
fn disabled(app: &App, control: Control) -> bool {
    match control {
        Control::CaptureCpu => app.logs.source != Source::RemoteSsh || app
            .remote_workspace
            .as_ref()
            .is_none_or(|remote| !remote.client.can_capture_cpu() || remote.client.capturing_cpu()),
        Control::Copy | Control::Autoscroll => app.logs.log.is_none(),
        _ => false,
    }
}
pub(crate) fn draw_dropdown(frame: &mut Frame, app: &mut App) {
    if blocked(app) || !app.show_panel || app.panel_tab != 2 {
        return;
    }
    let Some(menu) = app.logs.dropdown else {
        return;
    };
    let Some((anchor, _)) = app
        .logs
        .controls
        .iter()
        .find(|(_, control)| *control == menu)
        .or_else(|| {
            app.logs
                .controls
                .iter()
                .find(|(_, control)| *control == Control::More)
        })
    else {
        return;
    };
    let actions = menu_actions(app, menu);
    let items: Vec<_> = actions
        .iter()
        .map(|(label, control)| {
            crate::ui_widgets::DropdownItem::new(label).disabled(disabled(app, *control))
        })
        .collect();
    app.logs.dropdown_area = Some(
        crate::ui_widgets::DropdownMenu::new(
            &items,
            crate::ui_widgets::themed_dropdown_colors(&app.colors),
        )
        .anchor(anchor.x, anchor.bottom())
        .min_width(anchor.width)
        .selected(app.logs.selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render(frame),
    );
}
fn action(app: &mut App, control: Control) {
    if disabled(app, control) {
        return;
    }
    match control {
        Control::Source | Control::More => {
            app.logs.dropdown = if app.logs.dropdown == Some(control) {
                None
            } else {
                Some(control)
            };
            app.logs.selected = if control == Control::Source {
                Source::ALL.iter().position(|source| *source == app.logs.source).unwrap_or(0)
            } else { 0 };
        }
        Control::Channel(source) => app.logs.select_source(source),
        Control::Copy => {
            if let Some(editor) = app.logs.editor() {
                if let Err(error) = crate::clipboard::set_text(editor.rope.to_string()) {
                    app.notify_error(format!("Could not copy output: {error}"));
                }
            }
        }
        Control::CaptureCpu => {
            if app.remote_workspace.as_ref().is_some_and(|remote| {
                remote.client.can_capture_cpu() && !remote.client.capturing_cpu()
            }) {
                app.capture_remote_cpu();
            }
        }
        Control::Autoscroll => {
            let reader = app.logs.reader();
            if reader.view.autoscroll {
                reader.view.autoscroll = false;
            } else {
                reader.resume_autoscroll();
            }
        }
        Control::Wrap => app.logs.reader().toggle_wrap(),
        Control::Clear => {
            let log = app.logs.log.clone();
            app.logs.reader().clear(log.as_ref());
        }
    }
}
pub(crate) fn mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.show_panel || app.panel_tab != 2 {
        return false;
    }
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        if let Some(reader) = &mut app.logs.reader {
            reader.cancel_capture();
        }
    } else if matches!(
        mouse.kind,
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
    ) && app
        .logs
        .reader
        .as_ref()
        .is_some_and(|reader| reader.capturing())
    {
        app.mouse_col = mouse.column;
        app.mouse_row = mouse.row;
        return app.logs.reader().mouse(mouse, app.scroll_speed);
    }
    if blocked(app) {
        return false;
    }
    if matches!(
        mouse.kind,
        MouseEventKind::Down(MouseButton::Back | MouseButton::Forward)
            | MouseEventKind::Up(MouseButton::Back | MouseButton::Forward)
            | MouseEventKind::Drag(MouseButton::Back | MouseButton::Forward)
    ) {
        return false;
    }
    let point = (mouse.column, mouse.row).into();
    if let Some(menu) = app.logs.dropdown {
        if let Some(area) = app.logs.dropdown_area.filter(|area| area.contains(point)) {
            let actions = menu_actions(app, menu);
            let selected = (mouse.row - area.y) as usize;
            if selected < actions.len() {
                app.logs.selected = selected;
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    crate::ui_panels::capture_pointer(app, MouseButton::Left);
                    if disabled(app, actions[selected].1) {
                        return true;
                    }
                    app.logs.dropdown = None;
                    action(app, actions[selected].1);
                }
            }
            app.mouse_col = mouse.column;
            app.mouse_row = mouse.row;
            app.mouse_hover_changed = true;
            return true;
        }
        if matches!(mouse.kind, MouseEventKind::Down(_))
            && !app
                .logs
                .controls
                .iter()
                .any(|(area, control)| *control == menu && area.contains(point))
        {
            app.logs.dropdown = None;
        }
    }
    if let Some((_, control)) = app
        .logs
        .controls
        .iter()
        .find(|(area, _)| area.contains(point))
        .copied()
    {
        app.mouse_col = mouse.column;
        app.mouse_row = mouse.row;
        app.mouse_hover_changed = true;
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            crate::ui_panels::capture_pointer(app, MouseButton::Left);
            app.active_pane = ActivePane::Panel;
            app.terminal_panel_focused = false;
            app.logs.reader().blur_search();
            app.logs.focus = Some(control);
            action(app, control);
        }
        return true;
    }
    if mouse.kind == MouseEventKind::Moved
        && app
            .logs
            .controls
            .iter()
            .any(|(area, _)| area.contains((app.mouse_col, app.mouse_row).into()))
    {
        app.mouse_hover_changed = true;
    }
    let content = app
        .logs
        .reader
        .as_ref()
        .is_some_and(|reader| reader.area.contains(point) || reader.capturing());
    if content {
        app.mouse_col = mouse.column;
        app.mouse_row = mouse.row;
        if mouse.kind == MouseEventKind::Down(MouseButton::Right)
            && app
                .logs
                .reader
                .as_ref()
                .is_some_and(|reader| reader.text.contains(point))
        {
            app.active_pane = ActivePane::Panel;
            app.terminal_panel_focused = false;
            app.logs.reader().blur_search();
            app.context_menu_items = vec![
                crate::app::ContextMenuItem {
                    label: "Copy".into(),
                    shortcut: "Ctrl+C".into(),
                    id: "copy",
                    separator_after: false,
                },
                crate::app::ContextMenuItem {
                    label: "Select All".into(),
                    shortcut: "Ctrl+A".into(),
                    id: "select_all",
                    separator_after: false,
                },
            ];
            app.context_menu_pos = (mouse.column, mouse.row);
            app.context_menu_inner = None;
            app.context_menu_selected = 0;
            app.context_menu_active = true;
            return true;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            app.active_pane = ActivePane::Panel;
            app.terminal_panel_focused = false;
            app.search_input_focused = false;
            app.logs.focus = None;
        }
        return app.logs.reader().mouse(mouse, app.scroll_speed);
    }
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.logs.focus = None;
    }
    false
}

pub(crate) fn paste(app: &mut App, text: &str) -> bool {
    if blocked(app) || !focused(app) {
        return false;
    }
    app.logs.reader().paste(text);
    true
}
pub(crate) fn key(app: &mut App, key: KeyEvent) -> bool {
    if blocked(app) || !focused(app) {
        return false;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && matches!(key.code, KeyCode::Char('w' | 'W')) {
        crate::ui_panels::close_panel(app);
        return true;
    }
    if ctrl
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('f' | 'F'))
    {
        app.logs.focus = None;
        app.logs.reader().open_find();
        return true;
    }
    if ctrl && matches!(key.code, KeyCode::Char('c' | 'C') | KeyCode::Insert) {
        if app.logs.reader().search_focused() {
            app.logs.reader().key(key);
            return true;
        }
        if let Some(text) = app.logs.selected_text() {
            let _ = crate::clipboard::set_text(text);
        }
        return true;
    }
    // Workbench commands keep their normal bindings. Editing keys are owned
    // by this read-only buffer, including when an editor terminal is behind it.
    let command = app.lookup_keybinding(key.modifiers, key.code);
    if command == Some("toggle_word_wrap") {
        action(app, Control::Wrap);
        return true;
    }
    if app.logs.search_focused()
        && key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
    {
        app.logs.reader().key(key);
        return true;
    }
    if matches!(
        command,
        Some(
            "quit"
                | "palette"
                | "quick_open"
                | "toggle_explorer"
                | "toggle_terminal"
                | "source_control"
                | "extensions"
                | "new_file"
                | "toggle_panel"
                | "toggle_problems"
                | "global_search"
                | "new_terminal"
                | "acp_chat"
                | "show_shortcuts"
                | "toggle_menu_bar"
                | "next_tab"
                | "prev_tab"
                | "next_tab_page"
                | "prev_tab_page"
        )
    ) || key.code == KeyCode::F(1)
        || (key.code == KeyCode::F(11) && key.modifiers.is_empty())
        || (ctrl
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Char('e' | 'E')))
        || (key.modifiers.contains(KeyModifiers::ALT)
            && !key.modifiers.contains(KeyModifiers::CONTROL))
    {
        return false;
    }
    if let Some(menu) = app.logs.dropdown {
        let actions = menu_actions(app, menu);
        match key.code {
            KeyCode::Esc => app.logs.dropdown = None,
            KeyCode::Up => app.logs.selected = app.logs.selected.saturating_sub(1),
            KeyCode::Down => app.logs.selected = (app.logs.selected + 1).min(actions.len() - 1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if disabled(app, actions[app.logs.selected.min(actions.len() - 1)].1) {
                    return true;
                }
                app.logs.dropdown = None;
                action(app, actions[app.logs.selected.min(actions.len() - 1)].1);
            }
            _ => {}
        }
        return true;
    }
    if app.logs.reader().search_focused() {
        app.logs.reader().key(key);
        return true;
    }
    if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) && !ctrl {
        let backwards = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
        let mut order = [None; 6];
        let length = app.logs.controls.len().min(5) + 1;
        for (item, (_, control)) in order[1..length].iter_mut().zip(&app.logs.controls) {
            *item = Some(*control);
        }
        let order = &order[..length];
        let current = order
            .iter()
            .position(|control| *control == app.logs.focus)
            .unwrap_or(0);
        app.logs.focus =
            order[(current + if backwards { order.len() - 1 } else { 1 }) % order.len()];
        return true;
    }
    if let Some(control) = app.logs.focus {
        match key.code {
            KeyCode::Esc => app.logs.focus = None,
            KeyCode::Enter | KeyCode::Char(' ') => action(app, control),
            KeyCode::Up | KeyCode::Down if control == Control::Source => action(app, control),
            _ => {}
        }
    } else if key.code == KeyCode::Esc {
        let finding = app.logs.reader().searching();
        app.logs.reader().key(key);
        if !finding {
            app.active_pane = ActivePane::Editor;
        }
    } else {
        app.logs.reader().key(key);
    }
    true
}

pub(crate) fn hint(app: &App) -> Option<String> {
    if blocked(app) || !app.show_panel || app.panel_tab != 2 || app.logs.dropdown.is_some() {
        return None;
    }
    let (_, control) = app
        .logs
        .controls
        .iter()
        .find(|(area, _)| area.contains((app.mouse_col, app.mouse_row).into()))?;
    Some(match control {
        Control::Source => {
            let omitted = app
                .logs
                .reader
                .as_ref()
                .map_or(0, |reader| reader.view.omitted);
            if omitted == 0 {
                "Output channel · Logging continues while Output is hidden".into()
            } else {
                format!("Output channel · Newest 1 MiB retained · {omitted} older events omitted")
            }
        }
        Control::Copy => "Copy all displayed output".into(),
        Control::Autoscroll => "Autoscroll · Scroll up to pause; scroll past the bottom to resume".into(),
        Control::CaptureCpu => {
            "Measure headless process and thread CPU for 10 seconds · Runs only when requested"
                .into()
        }
        _ => "More Output actions".into(),
    })
}

pub(crate) fn command(app: &mut App, id: &str) -> bool {
    if !focused(app) {
        return false;
    }
    match id {
        "copy" => {
            if let Some(text) = app.logs.selected_text() {
                let _ = crate::clipboard::set_text(text);
            }
        }
        "select_all" => app
            .logs
            .reader()
            .key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
        "find" => app.logs.reader().open_find(),
        "toggle_word_wrap" => app.logs.reader().toggle_wrap(),
        "find_next" | "find_prev" => app.logs.reader().key(KeyEvent::new(
            KeyCode::F(3),
            if id == "find_prev" {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            },
        )),
        "cut" | "paste" | "undo" | "redo" if app.logs.search_focused() => {
            let key = match id {
                "cut" => 'x',
                "paste" => 'v',
                "undo" => 'z',
                _ => 'y',
            };
            app.logs
                .reader()
                .key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL));
        }
        "format_document"
        | "format_selection"
        | "rename_symbol"
        | "clipboard_history"
        | "cut"
        | "paste"
        | "paste_plain"
        | "undo"
        | "redo"
        | "save"
        | "save_as"
        | "delete_line"
        | "duplicate_line"
        | "move_line_up"
        | "move_line_down"
        | "indent"
        | "outdent"
        | "toggle_comment"
        | "join_lines"
        | "transform_uppercase"
        | "transform_lowercase"
        | "transform_titlecase"
        | "toggle_read_only" => {}
        _ => return false,
    }
    true
}

impl App {
    pub(crate) fn capture_remote_cpu(&mut self) {
        let Some(remote) = &self.remote_workspace else {
            self.notify("CPU capture requires a remote SSH connection");
            return;
        };
        match remote.client.capture_cpu() {
            Ok(true) => {
                self.logs.select_source(Source::RemoteSsh);
                self.show_output();
                self.notify(
                    "Measuring headless CPU for 10 seconds · results will appear in Output",
                );
            }
            Ok(false) => self.notify("A headless CPU capture is already running"),
            Err(error) => self.notify_error(format!("Could not capture headless CPU: {error}")),
        }
    }
    pub(crate) fn show_output(&mut self) {
        if let Some(log) = self
            .remote_workspace
            .as_ref()
            .and_then(|remote| remote.diagnostics())
            .or_else(|| self.remote_connection.diagnostic_log())
        {
            self.logs.remote_log = Some(log.clone());
            if self.logs.source == Source::RemoteSsh {
                self.logs.log = Some(log);
            }
        }
        self.show_panel = true;
        self.panel_tab = 2;
        self.active_pane = ActivePane::Panel;
        self.terminal_panel_focused = false;
        self.search_input_focused = false;
        self.logs.reader();
        self.logs.focus = None;
        self.logs.dropdown = None;
    }
    pub(crate) fn show_remote_diagnostics(&mut self) {
        self.side_notice.dismiss(crate::app_remote_connection::NOTICE_ID);
        self.remote_connection.details = false;
        self.logs.select_source(Source::RemoteSsh);
        self.show_output();
    }
    pub(crate) fn copy_remote_diagnostics(&mut self) {
        let log = self
            .remote_workspace
            .as_ref()
            .and_then(|remote| remote.diagnostics())
            .or_else(|| self.remote_connection.diagnostic_log())
            .or_else(|| self.logs.remote_log.clone())
            .or_else(|| self.logs.log.clone().filter(|_| self.logs.source == Source::RemoteSsh));
        let Some(log) = log else {
            self.notify("No SSH connection events are available yet");
            return;
        };
        match crate::clipboard::set_text(log.report()) {
            Ok(()) => self.notify("Connection log copied"),
            Err(error) => self.notify_error(format!("Could not copy connection log: {error}")),
        }
    }
}

#[cfg(test)]
#[path = "ui_logs_tests.rs"]
mod tests;
