//! On-demand diagnostics. Everything here runs on existing UI events; browsing,
//! reports, and frame history do not start a timer. Process counters have one
//! ten-second deadline, only while the overview is visible and focused.
use std::{
    cmp::Reverse,
    fmt::Write,
    path::Path,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    performance::{ActivityMonitor, RenderTiming, format_elapsed},
    theme_workbench::ColorLayer,
    ui_performance::clip_middle,
};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    #[default]
    Overview,
    Running,
    Recent,
    Workspace,
    Timings,
}

impl Section {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::Running,
        Self::Recent,
        Self::Workspace,
        Self::Timings,
    ];
    fn step(self, backwards: bool) -> Self {
        let sections = Self::ALL;
        let index = sections
            .iter()
            .position(|&section| section == self)
            .unwrap();
        sections[(index + if backwards { sections.len() - 1 } else { 1 }) % sections.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Close,
    Pin,
    Tab(Section),
    Copy,
    Reset,
    ResetTimings,
    Clear,
    OpenFile,
    CopyPath,
    RenderInfo,
    TailInfo,
    ResourceInfo,
    Inspect(Section, u64),
    Row(u64),
}

impl Hit {
    fn hint(self) -> &'static str {
        match self {
            Self::Close => "Close details · Esc",
            Self::Pin => "Pin beside the editor to keep monitoring while you work · Ctrl+L",
            Self::Tab(Section::Overview) => {
                "CPU, memory, activity and UI draw times · Tab to switch"
            }
            Self::Tab(Section::Running) => "Active tasks, including queued work · ↑↓ to browse",
            Self::Tab(Section::Recent) => "Last 32 tasks taking at least 400ms · ↑↓ to browse",
            Self::Tab(Section::Workspace) => {
                "Open documents, editor mode and language servers · ↑↓ to scroll"
            }
            Self::Tab(Section::Timings) => {
                "UI responsiveness, slow calls and all tracked task totals · ↑↓ to scroll"
            }
            Self::Copy => "Copy all timings and file paths · Ctrl+C",
            Self::Reset => "Reset UI draw times · Keeps task history · Ctrl+R",
            Self::ResetTimings => {
                "Reset UI and task totals · Keeps running work and recent history · Ctrl+R"
            }
            Self::Clear => "Clear timing history · Keeps work running · Ctrl+Del",
            Self::OpenFile => "Open or focus this file · Enter",
            Self::CopyPath => "Copy this file's full path · Ctrl+Shift+C",
            Self::RenderInfo => "Time to draw the UI · Sampled only when it redraws",
            Self::TailInfo => "P95: 95% of recent frames finished within this time",
            Self::ResourceInfo => "Resident memory · excludes LSPs and terminals",
            Self::Row(_) => "Select to inspect · Double-click opens attached files",
            Self::Inspect(_, _) => "Inspect task timing and file details",
        }
    }
}

#[derive(Default)]
struct CardLayout {
    controls: Vec<(Rect, Hit)>,
    list: Rect,
    content_rows: usize,
}

impl CardLayout {
    fn clear(&mut self) {
        self.controls.clear();
        self.list = Rect::default();
        self.content_rows = 0;
    }
}

#[derive(Clone, Copy, Default)]
struct BrowsePosition {
    selected: Option<u64>,
    cursor: usize,
    scroll: usize,
}

#[derive(Default)]
pub(crate) struct ViewState {
    pub section: Section,
    pub(crate) pinned: bool,
    focused: bool,
    dock_area: Option<Rect>,
    pub(crate) resources: crate::process_usage::ProcessUsage,
    selected: Option<u64>,
    cursor: usize,
    scroll: usize,
    hover: Option<Hit>,
    feedback: Option<&'static str>,
    layout: CardLayout,
    positions: [BrowsePosition; 5],
    last_row_click: Option<(Section, u64, Instant)>,
    displayed_totals: u64,
}

impl ViewState {
    fn switch(&mut self, section: Section) {
        if self.section != section {
            self.resources.suspend();
            self.positions[self.section as usize] = BrowsePosition {
                selected: self.selected,
                cursor: self.cursor,
                scroll: self.scroll,
            };
            let position = self.positions[section as usize];
            self.selected = position.selected;
            self.cursor = position.cursor;
            self.scroll = position.scroll;
        }
        self.section = section;
        self.hover = None;
        self.feedback = None;
        self.last_row_click = None;
    }

    fn hit(&self, point: Position) -> Option<Hit> {
        self.layout
            .controls
            .iter()
            .find(|(area, _)| area.contains(point))
            .map(|(_, hit)| *hit)
    }

    fn reconcile(&mut self, entries: &[Entry<'_>], rows: usize) {
        self.cursor = self
            .selected
            .and_then(|id| entries.iter().position(|entry| entry.id == id))
            .unwrap_or(self.cursor)
            .min(entries.len().saturating_sub(1));
        self.selected = entries.get(self.cursor).map(|entry| entry.id);
        let rows = rows.max(1);
        self.scroll = self.scroll.min(entries.len().saturating_sub(rows));
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll + rows {
            self.scroll = self.cursor + 1 - rows;
        }
    }
}

struct Entry<'a> {
    id: u64,
    label: &'static str,
    path: Option<&'a Path>,
    elapsed: Duration,
    stopped: bool,
    count: usize,
    queued: bool,
    queue_time: Option<Duration>,
}

fn entries(monitor: &ActivityMonitor, section: Section, now: Instant) -> Vec<Entry<'_>> {
    if section == Section::Recent {
        return monitor
            .recent
            .iter()
            .map(|task| Entry {
                id: task.id,
                label: task.label,
                path: task.path.as_deref(),
                elapsed: task.elapsed,
                stopped: task.interrupted,
                count: 1,
                queued: false,
                queue_time: task.queue_time,
            })
            .collect();
    }
    let mut entries: Vec<_> = monitor
        .active
        .iter()
        .map(|task| {
            let queue_time = task.queue_time();
            Entry {
                id: task.id,
                label: task.label,
                path: task.path.as_deref(),
                elapsed: now.saturating_duration_since(task.started),
                stopped: false,
                count: 1,
                queued: queue_time.is_none(),
                queue_time,
            }
        })
        .collect();
    entries.sort_unstable_by_key(|entry| (entry.path.is_none(), Reverse(entry.elapsed), entry.id));
    let unnamed = monitor.job_count().saturating_sub(entries.len());
    if unnamed > 0 {
        entries.push(Entry {
            id: 0,
            label: "Other background work",
            path: None,
            elapsed: Duration::ZERO,
            stopped: false,
            count: unnamed,
            queued: false,
            queue_time: None,
        });
    }
    entries
}

fn clean_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| {
            if ch.is_control() {
                ' '
            } else if ch == '\\' {
                '/'
            } else {
                ch
            }
        })
        .collect()
}

fn obscured(app: &App) -> bool {
    app.change_dir_active
        || app.context_menu_active
        || app.menu_open
        || app.tab_context_active
        || app.explorer_context_active
        || app.sc_context_active
        || app.tab_switcher_active
        || app.shortcuts_active
        || app.settings_active
        || app.hunk_popup_active
        || app.status_details.open
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
}

pub(crate) fn dismiss(app: &mut App) {
    app.performance_view.resources.suspend();
    app.background.activity.details_open = false;
    app.performance_area = None;
    app.performance_view.layout.clear();
    app.performance_view.hover = None;
    app.performance_view.feedback = None;
    app.performance_view.last_row_click = None;
    app.performance_view.focused = false;
    app.performance_view.dock_area = None;
}

/// Ordinary workbench interactions release focus without closing a pinned view.
pub(crate) fn release_focus(app: &mut App) {
    if app.performance_view.pinned {
        app.performance_view.focused = false;
        app.performance_view.hover = None;
        app.performance_view.last_row_click = None;
    } else {
        dismiss(app);
    }
}

pub(crate) fn captures_keyboard(app: &App) -> bool {
    app.background.activity.details_open
        && (!app.performance_view.pinned || app.performance_view.focused)
}

pub(crate) fn timings_need_redraw(app: &App) -> bool {
    app.background.activity.details_open
        && app.performance_view.section == Section::Timings
        && app.performance_view.layout.list.height > 0
        && !obscured(app)
        && app.performance_view.displayed_totals != app.background.activity.totals.completed
}

fn toggle_pin(app: &mut App) {
    let view = &mut app.performance_view;
    view.pinned = !view.pinned;
    view.focused = !view.pinned;
    view.hover = None;
    view.feedback = None;
    view.dock_area = None;
    view.layout.clear();
    // The old surface remains visible until the next paint. Keep its resource
    // visibility through the input-to-draw poll so docking does not discard
    // the CPU baseline and make an already live reading warm up again.
    app.clear_drag_states();
}

/// Reserve real workbench space, so the pinned monitor never covers editor
/// text, tabs, scrollbars or the terminal. Narrow windows use a bottom dock.
pub(crate) fn reserve_dock(app: &mut App, area: Rect) -> Rect {
    app.performance_view.dock_area = None;
    if !app.background.activity.details_open || !app.performance_view.pinned {
        return area;
    }
    let (content, dock) = if area.width >= 100 {
        let width = (area.width / 5 * 2).clamp(44, 62);
        (
            Rect::new(area.x, area.y, area.width - width, area.height),
            Rect::new(area.right() - width, area.y, width, area.height),
        )
    } else if area.height >= 12 && area.width >= 20 {
        let height = (area.height / 2).clamp(6, 19);
        (
            Rect::new(area.x, area.y, area.width, area.height - height),
            Rect::new(area.x, area.bottom() - height, area.width, height),
        )
    } else {
        // A one-line dock still offers Unpin and Close on tiny terminals.
        let height = u16::from(area.height > 1);
        (
            Rect::new(area.x, area.y, area.width, area.height - height),
            Rect::new(area.x, area.bottom() - height, area.width, height),
        )
    };
    app.performance_view.dock_area = Some(dock);
    content
}

fn resources_visible(app: &App) -> bool {
    app.background.activity.details_open
        && app.performance_view.section == Section::Overview
        && app.terminal_has_focus
        && app
            .performance_area
            .is_some_and(|area| area.width >= 8 && area.height >= 6)
        && !obscured(app)
}

pub(crate) fn resource_next_wait(app: &App, now: Instant) -> Option<Duration> {
    app.performance_view
        .resources
        .next_wait(resources_visible(app), now)
}

pub(crate) fn poll_resources(app: &mut App, now: Instant) -> bool {
    let visible = resources_visible(app);
    app.performance_view.resources.refresh(visible, now)
}

pub(crate) fn toggle(app: &mut App) {
    if app.background.activity.details_open {
        dismiss(app);
    } else {
        app.background.activity.details_open = true;
        app.performance_view.focused = !app.performance_view.pinned;
        app.performance_area = None;
        app.performance_view.layout.clear();
        app.performance_view.feedback = None;
        app.clear_drag_states();
        app.reset_hover_states();
        app.clear_transient_editor_state();
    }
}

/// The palette's Show command also provides keyboard access to a dock whose
/// focus was returned to the editor. The status indicator remains a toggle.
pub(crate) fn show_or_focus(app: &mut App) {
    if !app.background.activity.details_open {
        toggle(app);
    }
    app.performance_view.focused = true;
    app.performance_view.feedback = None;
    app.clear_drag_states();
}

pub(crate) fn close_area(area: Rect) -> Rect {
    Rect::new(
        area.right().saturating_sub(5).max(area.x + 1),
        area.y + 1,
        area.width.saturating_sub(2).min(3),
        1,
    )
}

fn navigate(app: &mut App, delta: isize, end: Option<bool>) {
    if matches!(
        app.performance_view.section,
        Section::Workspace | Section::Timings
    ) {
        let count = app.performance_view.layout.content_rows;
        let rows = usize::from(app.performance_view.layout.list.height.max(1));
        let max = count.saturating_sub(rows);
        app.performance_view.scroll = match end {
            Some(true) => max,
            Some(false) => 0,
            None => app
                .performance_view
                .scroll
                .saturating_add_signed(delta)
                .min(max),
        };
        return;
    }
    if app.performance_view.section == Section::Overview {
        app.performance_view
            .switch(if app.background.activity.job_count() > 0 {
                Section::Running
            } else {
                Section::Recent
            });
    }
    let entries = entries(
        &app.background.activity,
        app.performance_view.section,
        Instant::now(),
    );
    let view = &mut app.performance_view;
    let rows = usize::from(view.layout.list.height.max(1));
    view.reconcile(&entries, rows);
    view.cursor = match end {
        Some(true) => entries.len().saturating_sub(1),
        Some(false) => 0,
        None => view
            .cursor
            .saturating_add_signed(delta)
            .min(entries.len().saturating_sub(1)),
    };
    view.selected = entries.get(view.cursor).map(|entry| entry.id);
    view.reconcile(&entries, rows);
    view.feedback = None;
    view.last_row_click = None;
}

fn reset_render(app: &mut App) {
    app.background.activity.render.reset();
    app.performance_view.feedback = Some("Measuring new frames");
}

fn reset_timings(app: &mut App) {
    reset_render(app);
    #[cfg(windows)]
    crate::terminal_input_batch::reset_delivery_timings();
    app.background.activity.ui.reset(Instant::now());
    app.background.activity.totals.reset();
    app.performance_view.feedback = Some("Timings reset · running work and recent history kept");
}

fn clear_history(app: &mut App, all: bool) {
    let selected = app.performance_view.selected;
    if all || selected.is_some() {
        if app
            .background
            .activity
            .remove_recent(if all { None } else { selected })
        {
            app.performance_view.feedback = Some(if all {
                "History cleared"
            } else {
                "Timing removed"
            });
        }
        let entries = entries(&app.background.activity, Section::Recent, Instant::now());
        let view = &mut app.performance_view;
        view.reconcile(&entries, usize::from(view.layout.list.height.max(1)));
    }
}

/// A snapshot of in-memory measurements. Full paths remain available even when
/// their visual labels are clipped. No document contents or disk reads enter it.
pub(crate) fn report(app: &App, now: Instant) -> String {
    let monitor = &app.background.activity;
    let timing = &monitor.render;
    let mut text = format!(
        "VTCode performance & activity\nVersion: {} · {} / {} · PID {}\nBackground tasks: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::process::id(),
        monitor.job_count()
    );
    let resources = &app.performance_view.resources;
    if let Some(sampled) = resources.sampled_at() {
        let _ = writeln!(
            text,
            "VTCode process: CPU {}, resident memory {} (sampled {}s ago)",
            resources.cpu_label(),
            resources.memory_label(),
            now.saturating_duration_since(sampled).as_secs()
        );
        if let Some(period) = resources.averaged_over {
            let _ = writeln!(
                text,
                "CPU averaged over {:.1}s; 100% = one logical CPU",
                period.as_secs_f64()
            );
        }
        if resources.cpu_is_cached() {
            text.push_str("CPU value is cached; sampling continuity was interrupted.\n");
        }
        text.push_str("Process counters: at most once per 10s while overview is visible and focused; excludes child processes\n");
        if let Some(bytes) = resources.peak_resident_bytes {
            let _ = writeln!(
                text,
                "Peak sampled resident memory: {}",
                crate::process_usage::format_bytes(bytes)
            );
        }
    } else {
        text.push_str("VTCode process: not sampled; open Overview to see CPU and memory\n");
    }
    text.push_str("Resource history (unavailable values are '-'): \n");
    for sample in resources.history() {
        let _ = writeln!(
            text,
            "age_s={:.1} cpu_percent={} interval_s={} resident_bytes={}",
            now.saturating_duration_since(sample.at).as_secs_f64(),
            sample
                .cpu_percent
                .map_or_else(|| "-".into(), |value| format!("{value:.2}")),
            sample.averaged_over.map_or_else(
                || "-".into(),
                |period| format!("{:.2}", period.as_secs_f64())
            ),
            sample
                .resident_bytes
                .map_or_else(|| "-".into(), |value| value.to_string())
        );
    }
    if timing.has_samples() {
        let _ = writeln!(
            text,
            "UI render: {:.1}ms last, {:.1}ms smoothed average, {:.1}ms peak since reset ({} frames)",
            timing.last_ms,
            timing.average_ms,
            timing.peak_ms,
            timing.sample_count()
        );
        text.push_str("Recent frame times (ms):");
        for ms in timing.recent_ms() {
            let _ = write!(text, " {ms:.1}");
        }
        text.push('\n');
        let (p95, slow, count) = timing.recent_summary();
        let _ = writeln!(
            text,
            "Recent {count} frames: P95 {p95:.1}ms; {slow} over 16.7ms (60 Hz budget). Draw + terminal output, not input latency or FPS."
        );
    } else {
        text.push_str("UI render: awaiting first frame\n");
    }
    for section in [Section::Running, Section::Recent] {
        let rows = entries(monitor, section, now);
        let _ = writeln!(
            text,
            "\n{} ({})",
            if section == Section::Running {
                "Running"
            } else {
                "Recent timings"
            },
            rows.len()
        );
        if rows.is_empty() {
            text.push_str("None\n");
        }
        for entry in rows {
            let _ = write!(text, "- {}", entry.label);
            if entry.count > 1 {
                let _ = write!(text, " ({} tasks)", entry.count);
            }
            if let Some(path) = entry.path {
                let _ = write!(text, " | {}", clean_path(path));
            }
            let elapsed = if entry.id == 0 {
                "timing unavailable".into()
            } else {
                format_elapsed(entry.elapsed)
            };
            let _ = writeln!(
                text,
                " | {}{}",
                elapsed,
                if entry.stopped { " | stopped" } else { "" }
            );
            if entry.queued {
                text.push_str("  Queued; execution has not started\n");
            }
            if let Some(wait) = entry.queue_time {
                let _ = writeln!(
                    text,
                    "  Queue {:.3}ms; execution {:.3}ms",
                    wait.as_secs_f64() * 1000.0,
                    entry.elapsed.saturating_sub(wait).as_secs_f64() * 1000.0
                );
            }
        }
    }
    timings::append_report(&mut text, app, now);
    text.push_str("\nWorkspace snapshot (in-memory metadata; file tabs can share buffers)\n");
    for (label, value) in dashboard::workspace_lines(app) {
        if !label.is_empty() {
            let _ = writeln!(
                text,
                "{label}{}",
                if value.is_empty() {
                    String::new()
                } else {
                    format!(": {value}")
                }
            );
        }
    }
    text
}

fn copy_report(app: &mut App) {
    copy_report_with(app, |text| crate::clipboard::set_text(text).is_ok());
}

pub(crate) fn copy_report_with(app: &mut App, copy: impl FnOnce(&str) -> bool) {
    let text = report(app, Instant::now());
    copy_text_with(app, text, "Report copied", copy);
}

fn copy_text_with(
    app: &mut App,
    text: String,
    success: &'static str,
    copy: impl FnOnce(&str) -> bool,
) {
    let copied = copy(&text);
    app.push_clipboard_history(&text);
    app.performance_view.feedback = Some(if copied {
        success
    } else {
        "Saved to clipboard history"
    });
    if !copied {
        app.notify("System clipboard unavailable — saved to clipboard history");
    }
}

fn selected_path(app: &App) -> Option<&Path> {
    entries(
        &app.background.activity,
        app.performance_view.section,
        Instant::now(),
    )
    .into_iter()
    .find(|entry| Some(entry.id) == app.performance_view.selected)
    .and_then(|entry| entry.path)
}

fn copy_path_with(app: &mut App, copy: impl FnOnce(&str) -> bool) {
    if let Some(path) = selected_path(app) {
        let text = path.to_string_lossy().into_owned();
        copy_text_with(app, text, "Path copied", copy);
    } else {
        app.performance_view.feedback = Some("Select a task with a file first");
    }
}

fn copy_path(app: &mut App) {
    copy_path_with(app, |text| crate::clipboard::set_text(text).is_ok());
}

fn open_selected(app: &mut App) {
    if let Some(path) = selected_path(app).map(Path::to_path_buf) {
        // This is a user action, not monitoring. Historical paths can disappear;
        // do not turn a stale timing into a new empty document.
        let already_open = app
            .tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
            .any(|tab| {
                tab.kind == crate::app::TabKind::File
                    && tab
                        .editor
                        .file_path
                        .as_ref()
                        .is_some_and(|open| App::same_open_path(open, &path))
            });
        if already_open || path.is_file() {
            release_focus(app);
            app.open_file(&path);
        } else {
            app.performance_view.feedback = Some("File is no longer available");
        }
    } else {
        app.performance_view.feedback = Some("This task has no file");
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.background.activity.details_open {
        return false;
    }
    if obscured(app) {
        release_focus(app);
        return false;
    }
    if !captures_keyboard(app) {
        return false;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL)
        && !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER);
    if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            copy_path(app);
        } else {
            copy_report(app);
        }
        return true;
    }
    if ctrl && matches!(key.code, KeyCode::Char('r' | 'R')) {
        if app.performance_view.section == Section::Timings {
            reset_timings(app);
        } else {
            reset_render(app);
        }
        return true;
    }
    if ctrl && matches!(key.code, KeyCode::Char('l' | 'L')) {
        toggle_pin(app);
        return true;
    }
    if key.code == KeyCode::Delete
        && app.performance_view.section == Section::Recent
        && !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        clear_history(app, ctrl);
        return true;
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        release_focus(app);
        return false;
    }
    match key.code {
        KeyCode::Esc => release_focus(app),
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
            let backwards = matches!(key.code, KeyCode::BackTab | KeyCode::Left)
                || key.modifiers.contains(KeyModifiers::SHIFT);
            let section = app.performance_view.section.step(backwards);
            app.performance_view.switch(section);
        }
        KeyCode::Up => navigate(app, -1, None),
        KeyCode::Down => navigate(app, 1, None),
        KeyCode::PageUp | KeyCode::PageDown => {
            let page = isize::try_from(app.performance_view.layout.list.height.max(1)).unwrap_or(1);
            navigate(
                app,
                if key.code == KeyCode::PageUp {
                    -page
                } else {
                    page
                },
                None,
            );
        }
        KeyCode::Home | KeyCode::End => navigate(app, 0, Some(key.code == KeyCode::End)),
        KeyCode::Enter => {
            if app.performance_view.section == Section::Overview {
                navigate(app, 0, None);
            } else if !matches!(
                app.performance_view.section,
                Section::Workspace | Section::Timings
            ) {
                open_selected(app);
            }
        }
        _ => {
            release_focus(app);
            return false;
        }
    }
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent, previous: (u16, u16)) -> bool {
    if !app.background.activity.details_open {
        return false;
    }
    if obscured(app) {
        release_focus(app);
        return false;
    }
    // A gesture started outside the dock keeps its drag/release even when it
    // crosses this surface. Focus is set only on a press/scroll inside, so this
    // also covers split, settings, diff and extension gestures without a list
    // of unrelated drag flags that can go stale as new controls are added.
    if app.performance_view.pinned
        && !app.performance_view.focused
        && matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
    {
        return false;
    }
    let point = Position::new(mouse.column, mouse.row);
    if let Some(area) = app.performance_area
        && area.contains(point)
    {
        let entered = !area.contains(previous.into());
        if entered {
            app.reset_hover_states();
            app.clear_transient_editor_state();
            app.tab_tooltip_tab = None;
            app.tab_tooltip_hover_start = None;
            app.tab_tooltip_shown = false;
        }
        let hit = app.performance_view.hit(point);
        app.mouse_hover_changed = entered || hit != app.performance_view.hover;
        if hit != app.performance_view.hover {
            app.performance_view.feedback = None;
        }
        app.performance_view.hover = hit;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                app.performance_view.focused = true;
                if !matches!(hit, Some(Hit::Row(_))) {
                    app.performance_view.last_row_click = None;
                }
                match hit {
                    Some(Hit::Close) => dismiss(app),
                    Some(Hit::Pin) => toggle_pin(app),
                    Some(Hit::Tab(section)) => app.performance_view.switch(section),
                    Some(Hit::Copy) => copy_report(app),
                    Some(Hit::Reset) => reset_render(app),
                    Some(Hit::ResetTimings) => reset_timings(app),
                    Some(Hit::Clear) => clear_history(app, true),
                    Some(Hit::OpenFile) => open_selected(app),
                    Some(Hit::CopyPath) => copy_path(app),
                    Some(Hit::RenderInfo | Hit::TailInfo | Hit::ResourceInfo) => {}
                    Some(Hit::Inspect(section, id)) => {
                        app.performance_view.switch(section);
                        app.performance_view.selected = Some(id);
                    }
                    Some(Hit::Row(id)) => {
                        let now = Instant::now();
                        let double_click = app.performance_view.last_row_click.is_some_and(
                            |(section, previous_id, last)| {
                                section == app.performance_view.section
                                    && previous_id == id
                                    && now.saturating_duration_since(last)
                                        < Duration::from_millis(400)
                            },
                        );
                        app.performance_view.selected = Some(id);
                        app.performance_view.feedback = None;
                        app.performance_view.last_row_click =
                            Some((app.performance_view.section, id, now));
                        if double_click {
                            open_selected(app);
                        }
                    }
                    None => {}
                }
                app.mouse_hover_changed = true;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                app.performance_view.focused = true;
                navigate(
                    app,
                    if mouse.kind == MouseEventKind::ScrollUp {
                        -3
                    } else {
                        3
                    },
                    None,
                );
                app.mouse_hover_changed = true;
            }
            MouseEventKind::Down(_) => app.performance_view.last_row_click = None,
            _ => {}
        }
        return true;
    }
    if matches!(
        mouse.kind,
        MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) && !app.status_bar_items.iter().any(|&(x, width, action)| {
        action == "performance"
            && app.status_bar_area.is_some_and(|area| mouse.row == area.y)
            && mouse.column >= x
            && mouse.column < x.saturating_add(width)
    }) {
        release_focus(app);
    }
    // The hover belongs to the card, even when the pointer leaves for chrome.
    // Its old highlight must disappear on that existing mouse event.
    if app.performance_view.hover.take().is_some() {
        app.mouse_hover_changed = true;
    }
    false
}

#[derive(Clone, Copy)]
struct Palette {
    normal: Style,
    dim: Style,
    heading: Style,
    accent: Color,
    bg: Color,
}

fn palette(app: &App, bg: Color) -> Palette {
    let ui = &app.colors.ui;
    let normal = Style::default().fg(ui.widget_text_color(bg)).bg(bg);
    let dim = Style::default()
        .fg(ui.readable_foreground(
            ui.description_foreground
                .over(bg)
                .unwrap_or(app.colors.fg_dim),
            bg,
            4.5,
        ))
        .bg(bg);
    Palette {
        normal,
        dim,
        heading: normal.add_modifier(Modifier::BOLD),
        accent: ui.readable_foreground(app.colors.fg_accent, bg, 4.5),
        bg,
    }
}

fn button(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    label: &str,
    hit: Hit,
    enabled: bool,
    p: Palette,
) {
    if area.is_empty() {
        return;
    }
    if enabled {
        app.performance_view.layout.controls.push((area, hit));
    }
    let hovered = enabled && area.contains((app.mouse_col, app.mouse_row).into());
    let style = if enabled {
        app.colors
            .ui
            .toolbar_style(p.normal.fg.unwrap(), p.bg, hovered, false, false)
    } else {
        p.dim
    };
    f.render_widget(
        Paragraph::new(label)
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}

fn task_line(
    entry: &Entry<'_>,
    root: &Path,
    width: usize,
    normal: Style,
    dim: Style,
) -> Line<'static> {
    let timing = if entry.id == 0 {
        "—".into()
    } else if entry.queued {
        "Queued".into()
    } else {
        format_elapsed(entry.elapsed)
    };
    let room = width.saturating_sub(UnicodeWidthStr::width(timing.as_str()) + 2);
    let action = if entry.count > 1 {
        format!("{} ×{}", entry.label, entry.count)
    } else {
        entry.label.to_string()
    };
    let action_width = UnicodeWidthStr::width(action.as_str());
    let (action, name) = if let Some(path) = entry.path.filter(|_| room >= action_width + 5) {
        let path = path.strip_prefix(root).unwrap_or(path);
        (
            action,
            format!(
                "  {}",
                clip_middle(&clean_path(path), room - action_width - 2)
            ),
        )
    } else {
        (clip_middle(&action, room), String::new())
    };
    let gap = width.saturating_sub(
        UnicodeWidthStr::width(action.as_str())
            + UnicodeWidthStr::width(name.as_str())
            + UnicodeWidthStr::width(timing.as_str()),
    );
    Line::from(vec![
        Span::styled(action, normal),
        Span::styled(name, dim),
        Span::raw(" ".repeat(gap)),
        Span::styled(timing, normal),
    ])
}

fn summary(monitor: &ActivityMonitor, now: Instant, width: usize) -> String {
    if monitor.job_count() == 0 {
        return "All caught up".into();
    }
    let mut state = format!(
        "{} {} active",
        monitor.job_count(),
        if monitor.job_count() == 1 {
            "task"
        } else {
            "tasks"
        }
    );
    let files = monitor
        .active
        .iter()
        .filter(|task| task.path.is_some())
        .count();
    if monitor.job_count() > 1 {
        let context = format!(
            " · {files} file {}",
            if files == 1 { "task" } else { "tasks" }
        );
        if files > 0 && state.len() + UnicodeWidthStr::width(context.as_str()) <= width {
            state.push_str(&context);
        }
        if let Some(start) = monitor
            .active
            .iter()
            .map(|task| task.started)
            .min()
            .or_else(|| monitor.busy_since(now))
        {
            let context = format!(
                " · longest {}",
                format_elapsed(now.saturating_duration_since(start))
            );
            if UnicodeWidthStr::width(state.as_str()) + UnicodeWidthStr::width(context.as_str())
                <= width
            {
                state.push_str(&context);
            }
        }
    }
    clip_middle(&state, width)
}

fn metric_line(values: &[String; 3], width: usize, style: Style) -> Line<'static> {
    let mut spans = Vec::with_capacity(6);
    for (index, value) in values.iter().enumerate() {
        let room = (index + 1) * width / 3 - index * width / 3;
        let value = clip_middle(value, room);
        let gap = room - UnicodeWidthStr::width(value.as_str());
        spans.push(Span::styled(value, style));
        spans.push(Span::raw(" ".repeat(gap)));
    }
    Line::from(spans)
}

#[cfg(test)]
fn frame_trend(timing: &RenderTiming) -> (String, f32) {
    let ceiling = timing.recent_ms().fold(16.7f32, f32::max);
    let trend = timing
        .recent_ms()
        .map(|ms| {
            ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
                [(ms / ceiling * 7.0).round().clamp(0.0, 7.0) as usize]
        })
        .collect();
    (trend, ceiling)
}

fn path_lines(path: &str, width: usize, rows: usize) -> Vec<Line<'static>> {
    if width == 0 || rows == 0 {
        return Vec::new();
    }
    // Reserve a cell at each wrap boundary for wide graphemes, keeping both
    // ends of an unusually long path in the available space.
    let path = clip_middle(path, width * rows - rows.saturating_sub(1));
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for grapheme in path.graphemes(true) {
        let cells = UnicodeWidthStr::width(grapheme);
        if used + cells > width {
            lines.push(Line::raw(std::mem::take(&mut line)));
            used = 0;
        }
        line.push_str(grapheme);
        used += cells;
    }
    if !line.is_empty() {
        lines.push(Line::raw(line));
    }
    lines.truncate(rows);
    lines
}

#[path = "ui_performance_dashboard.rs"]
mod dashboard;

#[path = "ui_performance_timings.rs"]
mod timings;

fn table_line(
    task: &str,
    target: &str,
    duration: &str,
    width: usize,
    normal: Style,
    dim: Style,
) -> Line<'static> {
    let timing_width = width.min(8);
    let task_width = if width >= 52 {
        (width / 3).min(26)
    } else {
        width.saturating_sub(timing_width + 1)
    };
    let target_width = width.saturating_sub(task_width + timing_width + 2);
    let task = clip_middle(task, task_width);
    let target = clip_middle(target, target_width);
    let duration = clip_middle(duration, timing_width);
    let mut spans = vec![
        Span::styled(task.clone(), normal),
        Span::raw(" ".repeat(task_width.saturating_sub(UnicodeWidthStr::width(task.as_str())))),
    ];
    if target_width > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(target.clone(), dim));
        spans.push(Span::raw(" ".repeat(
            target_width.saturating_sub(UnicodeWidthStr::width(target.as_str())),
        )));
    }
    let used: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    spans.push(Span::raw(" ".repeat(
        width.saturating_sub(used + UnicodeWidthStr::width(duration.as_str())),
    )));
    spans.push(Span::styled(duration, normal));
    Line::from(spans)
}

fn draw_list(f: &mut Frame, app: &mut App, body: Rect, p: Palette, now: Instant) {
    if body.height == 0 {
        return;
    }
    let section = app.performance_view.section;
    let entries = entries(&app.background.activity, section, now);
    let detail_height = if body.height >= 10 {
        4
    } else if body.height >= 8 {
        3
    } else if body.height >= 5 {
        2
    } else {
        0
    };
    let rows = body
        .height
        .saturating_sub(2 + detail_height + u16::from(detail_height > 0))
        .max(1);
    let list = Rect::new(
        body.x,
        body.y + 2,
        body.width.saturating_sub(1),
        rows.min(body.height.saturating_sub(2)),
    );
    let view = &mut app.performance_view;
    view.layout.list = list;
    view.reconcile(&entries, usize::from(list.height));
    let first = if entries.is_empty() {
        0
    } else {
        view.scroll + 1
    };
    let last = (view.scroll + list.height as usize).min(entries.len());
    let heading = if body.width < 45 {
        if section == Section::Running {
            "Active".to_string()
        } else {
            "Recent".to_string()
        }
    } else if section == Section::Running {
        let count = app.background.activity.job_count();
        format!("Active · {count} task{}", if count == 1 { "" } else { "s" })
    } else {
        "Recent tasks · ≥400ms".to_string()
    };
    let range = format!("{first}–{last} / {}", entries.len());
    let range_width = UnicodeWidthStr::width(range.as_str()) as u16;
    let header_width = body
        .width
        .saturating_sub(if section == Section::Recent { 8 } else { 0 });
    let show_range = header_width > range_width + 4;
    f.render_widget(
        Paragraph::new(clip_middle(
            &heading,
            header_width.saturating_sub(if show_range { range_width + 2 } else { 0 }) as usize,
        ))
        .style(p.dim),
        Rect::new(body.x, body.y, header_width, 1),
    );
    if show_range {
        f.render_widget(
            Paragraph::new(range).style(p.dim),
            Rect::new(body.x + header_width - range_width, body.y, range_width, 1),
        );
    }
    if body.height >= 2 {
        let heading = table_line(
            "Task",
            "File / target",
            if section == Section::Running {
                "Elapsed"
            } else {
                "Duration"
            },
            list.width as usize,
            p.dim,
            p.dim,
        );
        f.render_widget(
            Paragraph::new(heading),
            Rect::new(list.x, body.y + 1, list.width, 1),
        );
    }
    if entries.is_empty() {
        let message = if section == Section::Running {
            "No background work right now"
        } else {
            "No long-running tasks recorded"
        };
        f.render_widget(
            Paragraph::new(clip_middle(message, list.width as usize)).style(p.dim),
            list,
        );
    }
    for (offset, entry) in entries
        .iter()
        .enumerate()
        .skip(view.scroll)
        .take(list.height as usize)
    {
        let row = Rect::new(
            list.x,
            list.y + (offset - view.scroll) as u16,
            list.width,
            1,
        );
        let selected = view.selected == Some(entry.id);
        let hovered = row.contains((app.mouse_col, app.mouse_row).into());
        let style = app.colors.ui.list_row_style(
            ColorLayer::opaque(p.normal.fg.unwrap()),
            p.bg,
            selected,
            true,
            hovered,
        );
        let dim = if selected || hovered { style } else { p.dim };
        let indicator = if entry.queued {
            "… "
        } else if entry.stopped {
            "○ "
        } else if section == Section::Recent {
            "✓ "
        } else {
            "  "
        };
        let path = entry
            .path
            .map(|path| clean_path(path.strip_prefix(&app.explorer.root).unwrap_or(path)))
            .unwrap_or_else(|| {
                if entry.count > 1 {
                    format!("{} tasks", entry.count)
                } else {
                    "Workspace".into()
                }
            });
        let duration = if entry.id == 0 {
            "—".into()
        } else if entry.queued {
            "Queued".into()
        } else {
            format_elapsed(entry.elapsed)
        };
        let label = format!("{}{}", if selected { "› " } else { indicator }, entry.label);
        let line = table_line(&label, &path, &duration, row.width as usize, style, dim);
        f.render_widget(Paragraph::new(line).style(style), row);
        view.layout.controls.push((row, Hit::Row(entry.id)));
    }
    if entries.len() > list.height as usize && list.height > 0 {
        let track_x = body.right() - 1;
        let thumb =
            (usize::from(list.height) * usize::from(list.height) / entries.len()).max(1) as u16;
        let top = view.scroll * usize::from(list.height - thumb)
            / entries.len().saturating_sub(list.height as usize).max(1);
        for y in 0..list.height {
            f.render_widget(
                Paragraph::new(if (top as u16..top as u16 + thumb).contains(&y) {
                    "┃"
                } else {
                    "│"
                })
                .style(p.dim),
                Rect::new(track_x, list.y + y, 1, 1),
            );
        }
    }
    let mut file_controls = None;
    if detail_height > 0
        && let Some(entry) = entries.get(view.cursor)
    {
        let detail = Rect::new(
            body.x,
            body.bottom() - detail_height,
            body.width,
            detail_height,
        );
        f.render_widget(
            Paragraph::new("─".repeat(detail.width as usize)).style(p.dim),
            Rect::new(detail.x, detail.y - 1, detail.width, 1),
        );
        let status = if entry.queued {
            "Queued"
        } else if entry.stopped {
            "Stopped"
        } else if section == Section::Recent {
            "Completed"
        } else if entry.id != 0 && entry.elapsed >= Duration::from_secs(10) {
            "Still working"
        } else {
            "In progress"
        };
        let title = format!("{} · {status}", entry.label);
        let action_width = if entry.path.is_some() && detail.width >= 48 {
            20
        } else if entry.path.is_some() && detail.width >= 28 {
            8
        } else {
            0
        };
        let title_width = detail.width.saturating_sub(action_width);
        f.render_widget(
            Paragraph::new(clip_middle(&title, title_width as usize)).style(p.heading),
            Rect::new(detail.x, detail.y, title_width, 1),
        );
        if action_width > 0 {
            file_controls = Some((
                Rect::new(detail.right() - action_width + 1, detail.y, 6, 1),
                (action_width == 20).then(|| Rect::new(detail.right() - 11, detail.y, 11, 1)),
            ));
        }
        let path = entry
            .path
            .map(clean_path)
            .unwrap_or_else(|| "Workspace activity · no file attached".into());
        let timing_row = u16::from(detail.height >= 4);
        if timing_row > 0 {
            let timing = if entry.id == 0 {
                "Timing unavailable for untracked work".into()
            } else if entry.queued {
                format!("Waiting for worker · {}", format_elapsed(entry.elapsed))
            } else if let Some(wait) = entry.queue_time {
                format!(
                    "Queue {} · execution {} · total {}",
                    format_elapsed(wait),
                    format_elapsed(entry.elapsed.saturating_sub(wait)),
                    format_elapsed(entry.elapsed)
                )
            } else {
                format!(
                    "Elapsed {} · queue timing unavailable",
                    format_elapsed(entry.elapsed)
                )
            };
            f.render_widget(
                Paragraph::new(clip_middle(&timing, detail.width as usize)).style(p.dim),
                Rect::new(detail.x, detail.y + 1, detail.width, 1),
            );
        }
        let path_area = Rect::new(
            detail.x,
            detail.y + 1 + timing_row,
            detail.width,
            detail.height - 1 - timing_row,
        );
        f.render_widget(
            Paragraph::new(path_lines(
                &path,
                path_area.width as usize,
                path_area.height as usize,
            ))
            .style(p.dim),
            path_area,
        );
    }
    let has_entries = !entries.is_empty();
    if let Some((open, copy)) = file_controls {
        button(f, app, open, "Open", Hit::OpenFile, true, p);
        if let Some(copy) = copy {
            button(f, app, copy, "Copy path", Hit::CopyPath, true, p);
        }
    }
    if section == Section::Recent {
        let clear = Rect::new(
            body.right() - 7.min(body.width),
            body.y,
            7.min(body.width),
            1,
        );
        button(f, app, clear, "Clear", Hit::Clear, has_entries, p);
    }
}

fn activity_hint(app: &App, now: Instant) -> String {
    let monitor = &app.background.activity;
    let task_label = |label: &str, path: Option<&Path>| {
        path.and_then(Path::file_name).map_or_else(
            || label.to_string(),
            |name| format!("{label} {}", name.to_string_lossy()),
        )
    };
    let label = if let Some(task) = monitor.primary(now) {
        let extra = if monitor.job_count() > 1 {
            let count = monitor.job_count() - 1;
            format!(" · +{count} task{}", if count == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        format!(
            "{} · {}{extra}",
            task_label(task.label, task.path.as_deref()),
            format_elapsed(now.saturating_duration_since(task.started)),
        )
    } else if monitor.job_count() > 0 {
        format!(
            "{} background {}",
            monitor.job_count(),
            if monitor.job_count() == 1 {
                "task"
            } else {
                "tasks"
            }
        )
    } else if let Some(task) = monitor.recent.front() {
        format!(
            "Last: {} · {} · {}",
            task_label(task.label, task.path.as_deref()),
            format_elapsed(task.elapsed),
            if task.interrupted {
                "Stopped"
            } else {
                "Finished"
            },
        )
    } else if monitor.render.has_samples() {
        format!("Average UI draw time · {:.1}ms", monitor.render.average_ms)
    } else {
        "Average UI draw time · —".into()
    };
    format!(" {label} · Click for CPU & memory ")
}

fn draw_hint(f: &mut Frame, app: &App) {
    if !app.terminal_has_focus {
        return;
    }
    let Some(bar) = app.status_bar_area else {
        return;
    };
    if app.mouse_row != bar.y || bar.y <= f.area().y {
        return;
    }
    let Some(&(x, width, _)) = app.status_bar_items.iter().find(|&&(x, width, action)| {
        action == "performance" && app.mouse_col >= x && app.mouse_col < x.saturating_add(width)
    }) else {
        return;
    };
    let text = activity_hint(app, Instant::now());
    let hint_width = UnicodeWidthStr::width(text.as_str())
        .min(64)
        .min(f.area().width as usize) as u16;
    let area = Rect::new(
        x.saturating_add(width)
            .saturating_sub(hint_width)
            .max(f.area().x),
        bar.y - 1,
        hint_width,
        1,
    );
    let bg = crate::ui_widgets::widget_background(f, &app.colors, area);
    f.render_widget(
        Paragraph::new(clip_middle(&text, hint_width as usize)).style(palette(app, bg).normal),
        area,
    );
}

pub(crate) fn draw_docked(f: &mut Frame, app: &mut App) {
    if !app.background.activity.details_open || !app.performance_view.pinned {
        return;
    }
    app.performance_area = None;
    app.performance_view.layout.clear();
    if obscured(app) {
        release_focus(app);
    }
    if let Some(area) = app
        .performance_view
        .dock_area
        .filter(|area| !area.is_empty())
    {
        draw_card(f, app, area);
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    if app.background.activity.details_open && app.performance_view.pinned {
        return;
    }
    app.performance_area = None;
    app.performance_view.layout.clear();
    if obscured(app) {
        release_focus(app);
        return;
    }
    if !app.background.activity.details_open {
        draw_hint(f, app);
        return;
    }
    let screen = f.area();
    if screen.width < 8 || screen.height < 4 {
        return;
    }
    let width = screen.width.saturating_sub(2).min(92);
    let height = screen.height.saturating_sub(2).min(30);
    let area = Rect::new(
        screen.right() - width - 1,
        screen.bottom() - height - 1,
        width,
        height,
    );
    draw_card(f, app, area);
}

fn draw_card(f: &mut Frame, app: &mut App, area: Rect) {
    app.performance_area = Some(area);
    let bg = crate::ui_widgets::widget_background(f, &app.colors, area);
    let p = palette(app, bg);
    let border = if app.performance_view.pinned && app.performance_view.focused {
        p.accent
    } else {
        app.colors.ui.widget_border.border(bg).unwrap_or(bg)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(p.normal);
    let inner = if area.height < 3 {
        area
    } else {
        block.inner(area)
    };
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let close = if area.height < 3 {
        Rect::new(
            inner.right().saturating_sub(3).max(inner.x),
            inner.y,
            inner.width.min(3),
            1,
        )
    } else {
        close_area(area)
    };
    button(f, app, close, "×", Hit::Close, true, p);
    let pin = (inner.width >= 14)
        .then(|| Rect::new(close.x.saturating_sub(8).max(inner.x), inner.y, 7, 1));
    let copy = (inner.width >= 44).then(|| Rect::new(pin.unwrap().x - 13, inner.y, 12, 1));
    let actions_x = copy.or(pin).unwrap_or(close).x;
    let title_x = inner.x + u16::from(inner.width > 4);
    let title_width = actions_x.saturating_sub(title_x + 1);
    let title = if title_width >= 22 {
        "Performance & activity"
    } else if title_width >= 11 {
        "Performance"
    } else {
        "Monitor"
    };
    f.render_widget(
        Paragraph::new(clip_middle(title, title_width as usize)).style(p.heading),
        Rect::new(title_x, inner.y, title_width, 1),
    );
    if let Some(copy) = copy {
        button(f, app, copy, "Copy report", Hit::Copy, true, p);
    }
    if let Some(pin) = pin {
        button(
            f,
            app,
            pin,
            if app.performance_view.pinned {
                "Unpin"
            } else {
                "Pin"
            },
            Hit::Pin,
            true,
            p,
        );
    }
    let tabs = inner.height >= 7 && inner.width >= 23;
    let airy = inner.height >= 20;
    let tabs_y = inner.y + 1 + u16::from(airy);
    if tabs {
        let labels = if inner.width >= 70 {
            [
                "Overview".into(),
                format!("Running {}", app.background.activity.job_count()),
                format!("Recent {}", app.background.activity.recent.len()),
                "Workspace".into(),
                "Timings".into(),
            ]
        } else if inner.width >= 54 {
            [
                "Overview".into(),
                "Running".into(),
                "Recent".into(),
                "Workspace".into(),
                "Timings".into(),
            ]
        } else {
            [
                "All".into(),
                "Now".into(),
                "Past".into(),
                "Info".into(),
                "Time".into(),
            ]
        };
        let available = inner.width.saturating_sub(2);
        for (index, (section, label)) in Section::ALL.into_iter().zip(labels).enumerate() {
            let left = index as u16 * available / Section::ALL.len() as u16;
            let right = (index as u16 + 1) * available / Section::ALL.len() as u16;
            let tab = Rect::new(inner.x + 1 + left, tabs_y, right - left, 1);
            button(f, app, tab, &label, Hit::Tab(section), true, p);
            if app.performance_view.section == section {
                let style = app
                    .colors
                    .ui
                    .toolbar_style(p.accent, p.bg, false, true, false)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                f.render_widget(
                    Paragraph::new(label)
                        .alignment(Alignment::Center)
                        .style(style),
                    tab,
                );
            }
        }
    }
    let body_y = inner.y + 1 + u16::from(tabs) + 2 * u16::from(tabs && airy);
    let body = Rect::new(
        inner.x + 1,
        body_y,
        inner.width.saturating_sub(2),
        inner.bottom().saturating_sub(body_y + 1),
    );
    let now = Instant::now();
    match app.performance_view.section {
        Section::Overview => dashboard::draw_overview(f, app, body, p, now),
        Section::Workspace => dashboard::draw_workspace(f, app, body, p),
        Section::Timings => timings::draw(f, app, body, p, now),
        _ => draw_list(f, app, body, p, now),
    }
    if inner.height >= 3 {
        let footer = Rect::new(body.x, inner.bottom() - 1, body.width, 1);
        let hint = if let Some(feedback) = app.performance_view.feedback {
            feedback.to_string()
        } else if let Some(hit) = app.performance_view.hover {
            if hit == Hit::Pin && app.performance_view.pinned {
                "Unpin to return to a popover".into()
            } else if hit == Hit::Close && app.performance_view.pinned {
                "Close monitor".into()
            } else {
                hit.hint().into()
            }
        } else if app.performance_view.pinned {
            if app.performance_view.focused {
                "Pinned · Esc returns to work".into()
            } else {
                "Pinned · keep working to see live updates".into()
            }
        } else if app.performance_view.section == Section::Overview {
            "Pin to keep open while you work · Esc Close".into()
        } else if matches!(
            app.performance_view.section,
            Section::Workspace | Section::Timings
        ) {
            "↑↓ Scroll · Tab Views · Esc Close".into()
        } else {
            "↑↓ Browse · Enter Open · Tab Views · Esc Close".into()
        };
        f.render_widget(
            Paragraph::new(clip_middle(&hint, footer.width as usize)).style(p.dim),
            footer,
        );
    }
}

#[cfg(test)]
#[path = "ui_process_usage_tests.rs"]
mod process_usage_tests;

#[cfg(test)]
#[path = "ui_performance_monitor_tests.rs"]
mod monitor_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Tab, editor::Editor};
    use ratatui::{Terminal, backend::TestBackend};

    fn app() -> App {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(
            "private document contents\n",
        ))];
        app.active_tab = 0;
        app.active_pane = crate::app::ActivePane::Editor;
        app.show_panel = false;
        app.show_minimap = false;
        app.show_explorer = false;
        app.background
            .activity
            .render
            .record(Duration::from_micros(2_400));
        app
    }

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
    }

    fn mouse(app: &mut App, kind: MouseEventKind, point: Position) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: point.x,
                row: point.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    fn displayed(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn completed(app: &mut App, number: usize) -> u64 {
        let path = app
            .explorer
            .root
            .join("src")
            .join(format!("completed_{number}.rs"));
        let _ticket = app.background.activity.start("Analyzing", Some(&path));
        let task = app.background.activity.active.last_mut().unwrap();
        task.started -= Duration::from_secs(number as u64 + 2);
        let id = task.id;
        task.finish_for_test(Duration::from_secs(number as u64 + 1));
        app.background.refresh_activity(Instant::now());
        id
    }

    fn control(app: &App, hit: Hit) -> Position {
        let &(area, _) = app
            .performance_view
            .layout
            .controls
            .iter()
            .find(|(_, candidate)| *candidate == hit)
            .unwrap();
        Position::new(area.x + area.width / 2, area.y)
    }

    #[test]
    fn performance_all_jobs_and_retained_timings_are_reachable_by_keyboard_and_wheel() {
        let mut app = app();
        for number in 0..24 {
            completed(&mut app, number);
        }
        let mut tickets = Vec::new();
        for number in 0..24 {
            let path = app
                .explorer
                .root
                .join("src")
                .join(format!("running_{number}.rs"));
            tickets.push(app.background.activity.start("Highlighting", Some(&path)));
            app.background.activity.active.last_mut().unwrap().started -=
                Duration::from_secs(number + 1);
        }
        app.background.refresh_activity(Instant::now());
        toggle(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let area = app.performance_area;
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_view.section, Section::Running);
        let running: Vec<_> = entries(&app.background.activity, Section::Running, Instant::now())
            .iter()
            .map(|entry| entry.id)
            .collect();
        let mut visited = Vec::new();
        for _ in 0..running.len() {
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            visited.push(app.performance_view.selected.unwrap());
            key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        }
        assert_eq!(visited, running);
        assert!(app.performance_view.scroll > 0);
        assert_eq!(app.performance_area, area);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_view.section, Section::Recent);
        assert_eq!(
            app.performance_view.selected,
            app.background.activity.recent.back().map(|task| task.id)
        );
        assert_eq!(app.background.activity.recent.len(), 24);
        assert!(displayed(&terminal).contains("/ 24"));
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let list = app.performance_view.layout.list;
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            Position::new(list.x + 2, list.y + 1),
        );
        assert_eq!(app.performance_view.cursor, 3);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let selected = app.performance_view.selected;
        completed(&mut app, 30);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(
            app.performance_view.selected, selected,
            "a newly completed task must not move the selection to another file"
        );
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert!(app.performance_view.cursor > 3);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(
            app.performance_view.selected,
            app.background.activity.recent.back().map(|task| task.id)
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "private document contents\n"
        );
    }

    #[test]
    fn performance_report_reset_and_history_controls_are_independent() {
        let mut app = app();
        let completed_id = completed(&mut app, 7);
        let path = app.explorer.root.join("src/report_context.rs");
        let _ticket = app.background.activity.start("Highlighting", Some(&path));
        app.background.activity.active[0].started -= Duration::from_secs(12);
        app.background.refresh_activity(Instant::now());
        toggle(&mut app);
        let mut copied = String::new();
        copy_report_with(&mut app, |text| {
            copied = text.to_string();
            true
        });
        for expected in [
            "VTCode performance & activity",
            "Background tasks: 1",
            "2.4ms smoothed average",
            "report_context.rs",
            "completed_7.rs",
            "12s",
            "8s",
            "Recent frame times",
        ] {
            assert!(copied.contains(expected), "missing {expected} in {copied}");
        }
        assert!(!copied.contains("private document contents"));
        assert!(app.clipboard_history.contains(&copied));
        assert_eq!(app.performance_view.feedback, Some("Report copied"));
        copy_report_with(&mut app, |_| false);
        assert_eq!(
            app.performance_view.feedback,
            Some("Saved to clipboard history")
        );
        key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(app.background.activity.render.sample_count(), 0);
        assert_eq!(app.background.activity.recent[0].id, completed_id);
        assert_eq!(app.background.activity.active.len(), 1);
        app.background
            .activity
            .render
            .record(Duration::from_micros(1_200));
        app.performance_view.switch(Section::Recent);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert!(app.background.activity.recent.is_empty());
        assert_eq!(app.background.activity.active.len(), 1);
        assert_eq!(app.background.activity.render.peak_ms, 1.2);
        completed(&mut app, 8);
        completed(&mut app, 9);
        key(&mut app, KeyCode::Delete, KeyModifiers::CONTROL);
        assert!(app.background.activity.recent.is_empty());
        assert_eq!(app.background.activity.active.len(), 1);
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "private document contents\n"
        );
    }

    #[test]
    fn performance_mouse_selection_and_controls_do_not_edit_or_repaint_plain_content() {
        let mut app = app();
        completed(&mut app, 1);
        completed(&mut app, 2);
        toggle(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let recent = control(&app, Hit::Tab(Section::Recent));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), recent);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let second = app.background.activity.recent[1].id;
        let row = control(&app, Hit::Row(second));
        mouse(&mut app, MouseEventKind::Moved, row);
        assert!(app.mouse_hover_changed);
        mouse(
            &mut app,
            MouseEventKind::Moved,
            Position::new(row.x + 1, row.y),
        );
        assert!(!app.mouse_hover_changed);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), row);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_view.selected, Some(second));
        let clear = control(&app, Hit::Clear);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), clear);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(app.background.activity.recent.is_empty());
        assert!(displayed(&terminal).contains("No long-running tasks recorded"));
        assert!(
            !app.performance_view
                .layout
                .controls
                .iter()
                .any(|(_, hit)| *hit == Hit::Clear)
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "private document contents\n"
        );
    }

    #[test]
    fn performance_enter_focuses_existing_file_and_does_not_create_stale_files() {
        let mut app = app();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let mut editor = Editor::test_with_text("unsaved in-memory contents\n");
        editor.file_path = Some(path.clone());
        editor.modified = true;
        app.tabs.push(Tab::new_file(editor));
        // A loaded, modified buffer remains reachable after external deletion.
        std::fs::remove_file(&path).unwrap();
        let _ticket = app.background.activity.start("Highlighting", Some(&path));
        let file_task = app.background.activity.active.last().unwrap().id;
        app.background.refresh_activity(Instant::now());
        toggle(&mut app);
        app.performance_view.switch(Section::Running);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.background.activity.details_open);
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "unsaved in-memory contents\n"
        );
        app.active_tab = 0;
        toggle(&mut app);
        app.performance_view.selected = Some(file_task);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let open = control(&app, Hit::OpenFile);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), open);
        assert!(!app.background.activity.details_open);
        assert_eq!(app.active_tab, 1);
        app.active_tab = 0;
        toggle(&mut app);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let file_row = control(&app, Hit::Row(file_task));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), file_row);
        assert!(app.background.activity.details_open);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), file_row);
        assert!(!app.background.activity.details_open);
        assert_eq!(app.active_tab, 1);
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "unsaved in-memory contents\n"
        );
        let missing = directory.path().join("removed.rs");
        let _missing = app.background.activity.start("Opening", Some(&missing));
        let missing_id = app.background.activity.active.last().unwrap().id;
        app.background.refresh_activity(Instant::now());
        toggle(&mut app);
        app.performance_view.selected = Some(file_task);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let file_row = control(&app, Hit::Row(file_task));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), file_row);
        let missing_row = control(&app, Hit::Row(missing_id));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            missing_row,
        );
        assert_eq!(app.performance_view.selected, Some(missing_id));
        assert!(
            app.performance_view.feedback.is_none(),
            "clicks on different tasks must not open a file"
        );
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.background.activity.details_open);
        assert_eq!(
            app.performance_view.feedback,
            Some("File is no longer available")
        );
        assert!(!missing.exists());
        assert_eq!(app.tabs.len(), 2);
    }

    #[test]
    fn performance_other_overlays_and_typing_take_focus_without_losing_input() {
        let mut app = app();
        toggle(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.background.activity.details_open);
        assert_eq!(app.performance_view.section, Section::Recent);
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "private document contents\n"
        );
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.performance_view.section, Section::Workspace);
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.performance_view.section, Section::Recent);
        key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.performance_view.section, Section::Workspace);
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(!app.background.activity.details_open);
        assert!(app.editor().unwrap().rope.to_string().starts_with('x'));
        for overlay in 0..3 {
            toggle(&mut app);
            match overlay {
                0 => app.palette.active = true,
                1 => app.context_menu_active = true,
                _ => app.menu_open = true,
            }
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(!app.background.activity.details_open);
            assert!(app.performance_area.is_none());
            assert!(app.performance_view.layout.controls.is_empty());
            app.palette.active = false;
            app.context_menu_active = false;
            app.menu_open = false;
        }
    }

    #[test]
    fn performance_unnamed_jobs_are_accounted_for_without_inventing_timings() {
        let mut app = app();
        let now = Instant::now();
        app.background.activity.refresh(now, 4);
        let rows = entries(
            &app.background.activity,
            Section::Running,
            now + Duration::from_secs(10),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 4);
        assert_eq!(rows[0].elapsed, Duration::ZERO);
        assert_eq!(rows[0].queue_time, None);
        let text = report(&app, now + Duration::from_secs(10));
        assert!(text.contains("Other background work (4 tasks) | timing unavailable"));
        let tasks = text
            .split("\nRunning (")
            .nth(1)
            .unwrap()
            .split("\nUI responsiveness")
            .next()
            .unwrap();
        assert!(!tasks.contains("10s"));
        toggle(&mut app);
        app.performance_view.switch(Section::Running);
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = displayed(&terminal);
        assert!(text.contains("Timing unavailable for untracked work"));
        assert!(!text.contains("Elapsed 0ms"));
    }

    #[test]
    fn performance_tabs_remember_selection_and_scroll_across_updates_and_resize() {
        let mut app = app();
        for number in 0..8 {
            completed(&mut app, number);
        }
        let mut tickets = Vec::new();
        for number in 0..12 {
            let path = app.explorer.root.join(format!("running_{number}.rs"));
            tickets.push(app.background.activity.start("Highlighting", Some(&path)));
        }
        app.background.refresh_activity(Instant::now());
        toggle(&mut app);
        app.performance_view.switch(Section::Running);
        // Keep both lists taller than the viewport, even when the card grows.
        let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let running = app.performance_view.selected;
        let running_scroll = app.performance_view.scroll;
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let recent = app.performance_view.selected;
        let recent_scroll = app.performance_view.scroll;
        assert!(running_scroll > 0 && recent_scroll > 0);
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_view.selected, running);
        assert_eq!(app.performance_view.scroll, running_scroll);
        // Re-selecting the current tab keeps the same place, too.
        let tab = control(&app, Hit::Tab(Section::Running));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), tab);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_view.selected, running);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_view.selected, recent);
        assert_eq!(app.performance_view.scroll, recent_scroll);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        completed(&mut app, 30); // Evicts the previously selected oldest timing.
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(
            app.performance_view.selected,
            app.background.activity.recent.back().map(|task| task.id)
        );
        let selected = app.performance_view.selected;
        dismiss(&mut app);
        toggle(&mut app);
        let mut narrow = Terminal::new(TestBackend::new(40, 12)).unwrap();
        narrow.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_view.selected, selected);
        assert!(app.performance_view.cursor >= app.performance_view.scroll);
        assert!(
            app.performance_view.cursor
                < app.performance_view.scroll
                    + usize::from(app.performance_view.layout.list.height)
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "private document contents\n"
        );
    }

    #[test]
    fn performance_copy_path_keeps_full_native_spelling_and_handles_unattached_tasks() {
        let mut app = app();
        let path = app.explorer.root.join("long folder/文件名-e\u{301}-👩‍💻.rs");
        let _ticket = app.background.activity.start("Opening", Some(&path));
        app.background.refresh_activity(Instant::now());
        toggle(&mut app);
        app.performance_view.switch(Section::Running);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let mut copied = String::new();
        copy_path_with(&mut app, |text| {
            copied = text.to_string();
            true
        });
        assert_eq!(copied, path.to_string_lossy());
        assert_eq!(app.performance_view.feedback, Some("Path copied"));
        assert_eq!(app.clipboard_history.back(), Some(&copied));
        assert!(!path.exists(), "copying a path must not create its file");
        copy_path_with(&mut app, |_| false);
        assert_eq!(
            app.performance_view.feedback,
            Some("Saved to clipboard history")
        );
        assert!(app.background.activity.details_open);
        app.performance_view.switch(Section::Recent);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        key(
            &mut app,
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(
            app.performance_view.feedback,
            Some("Select a task with a file first")
        );
        assert!(app.background.activity.details_open);
        assert!(
            !app.performance_view
                .layout
                .controls
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::OpenFile | Hit::CopyPath))
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "private document contents\n"
        );
    }

    #[test]
    fn performance_context_help_and_chart_scale_do_not_start_idle_redraws() {
        let mut app = app();
        app.background.activity.render.reset();
        app.background
            .activity
            .render
            .record(Duration::from_millis(120));
        for _ in 0..crate::performance::FRAME_HISTORY {
            app.background
                .activity
                .render
                .record(Duration::from_millis(2));
        }
        let (trend, ceiling) = frame_trend(&app.background.activity.render);
        assert_eq!(trend.chars().count(), crate::performance::FRAME_HISTORY);
        assert_eq!(ceiling, 16.7);
        assert_eq!(app.background.activity.render.peak_ms, 120.0);
        toggle(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(displayed(&terminal).contains("0–16.7ms"));
        app.background
            .activity
            .render
            .record(Duration::from_micros(42_500));
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(displayed(&terminal).contains("0–42.5ms"));
        assert!(
            frame_trend(&app.background.activity.render)
                .0
                .ends_with('█')
        );
        let info = control(&app, Hit::RenderInfo);
        mouse(&mut app, MouseEventKind::Moved, info);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(displayed(&terminal).contains("Sampled only when it redraws"));
        mouse(
            &mut app,
            MouseEventKind::Moved,
            Position::new(info.x + 1, info.y),
        );
        assert!(!app.mouse_hover_changed);
        let reset = control(&app, Hit::Reset);
        mouse(&mut app, MouseEventKind::Moved, reset);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(displayed(&terminal).contains("Keeps task history"));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), reset);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(displayed(&terminal).contains("Measuring new frames"));
        app.background.activity.mark_drawn(Instant::now());
        assert_eq!(app.background.activity.next_wait(Instant::now()), None);
        let text = report(&app, Instant::now());
        assert!(!text.contains("120.0ms"));
        assert!(text.contains("awaiting first frame"));
        let hint = activity_hint(&app, Instant::now());
        assert!(hint.contains("Average UI draw time"));
        app.background.activity.refresh(Instant::now(), 4);
        assert!(activity_hint(&app, Instant::now()).contains("4 background tasks"));
        completed(&mut app, 7);
        let hint = activity_hint(&app, Instant::now());
        assert!(hint.contains("Last: Analyzing completed_7.rs · 8s · Finished"));
        let path = app.explorer.root.join("processing.rs");
        let _first = app.background.activity.start("Highlighting", Some(&path));
        app.background.activity.active[0].started -= Duration::from_secs(12);
        let _second = app.background.activity.start("Refreshing Git", None);
        app.background.refresh_activity(Instant::now());
        let hint = activity_hint(&app, Instant::now());
        assert!(hint.contains("Highlighting processing.rs · 12s · +1 task"));
    }

    #[test]
    fn performance_selected_unicode_paths_keep_their_ends_and_fit() {
        let path = "C:/workspace/very long folder/文件名-e\u{301}-👩‍💻/another very long folder/final_name.rs";
        for width in 8..64 {
            let lines = path_lines(path, width, 2);
            assert!(lines.len() <= 2);
            assert!(lines.iter().all(|line| line.width() <= width));
            let text: String = lines
                .iter()
                .flat_map(|line| &line.spans)
                .map(|span| span.content.as_ref())
                .collect();
            assert!(text.ends_with(".rs"), "{width}: {text}");
        }
    }

    #[test]
    fn performance_views_are_responsive_readable_and_click_targets_stay_inside() {
        use crate::ui_control_visibility_tests::{
            assert_readable, audit_themes, check_region, snapshot,
        };
        let mut app = app();
        for number in 0..8 {
            completed(&mut app, number);
        }
        let path = app.explorer.root.join("src/parser.rs");
        let editor = app.editor_mut().unwrap();
        editor.file_path = Some(path.clone());
        editor.rope = ropey::Rope::from_str(
            "// Activity stays quiet while you work.\nfn main() {\n    println!(\"Ready when you are\");\n}\n",
        );
        app.background.activity.render.reset();
        for ms in [
            2.4, 2.1, 3.2, 2.6, 4.5, 2.8, 2.3, 2.1, 8.6, 3.8, 2.5, 2.2, 3.1, 2.4, 2.6, 2.4,
        ] {
            app.background
                .activity
                .render
                .record(Duration::from_secs_f64(ms / 1_000.0));
        }
        let _ticket = app.background.activity.start("Highlighting", Some(&path));
        app.background.activity.active[0].started -= Duration::from_secs(12);
        app.background.refresh_activity(Instant::now());
        let mut failures = Vec::new();
        for theme in audit_themes() {
            app.colors = theme.to_theme_colors();
            for (width, height) in [(8, 4), (20, 8), (28, 12), (40, 18), (80, 24), (120, 32)] {
                for section in Section::ALL {
                    app.background.activity.details_open = true;
                    app.performance_view.switch(section);
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                    let area = app.performance_area.unwrap();
                    assert_eq!(area.intersection(Rect::new(0, 0, width, height)), area);
                    for &(hit, _) in &app.performance_view.layout.controls {
                        assert_eq!(
                            hit.intersection(area),
                            hit,
                            "control outside card: {hit:?} in {area:?}"
                        );
                    }
                    if width >= 28 && height >= 12 {
                        for section in Section::ALL {
                            assert!(
                                app.performance_view
                                    .layout
                                    .controls
                                    .iter()
                                    .any(|(_, hit)| *hit == Hit::Tab(section)),
                                "missing {section:?} at {width}x{height}"
                            );
                        }
                    }
                    check_region(
                        terminal.backend().buffer(),
                        Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2),
                        &format!("{}: {section:?} {width}x{height}", theme.name),
                        &mut failures,
                        &app.colors.ui,
                    );
                    if width == 120 {
                        snapshot(
                            terminal.backend().buffer(),
                            &format!("{} - activity {section:?}", theme.name),
                        );
                        let controls: Vec<_> = app
                            .performance_view
                            .layout
                            .controls
                            .iter()
                            .filter(|(_, hit)| !matches!(hit, Hit::Row(_) | Hit::Tab(_)))
                            .copied()
                            .collect();
                        for (control, hit) in controls {
                            mouse(
                                &mut app,
                                MouseEventKind::Moved,
                                Position::new(control.x + control.width / 2, control.y),
                            );
                            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                            check_region(
                                terminal.backend().buffer(),
                                Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2),
                                &format!("{}: {section:?} hover {hit:?}", theme.name),
                                &mut failures,
                                &app.colors.ui,
                            );
                            if hit == Hit::CopyPath || hit == Hit::RenderInfo {
                                snapshot(
                                    terminal.backend().buffer(),
                                    &format!("{} - activity {section:?} help", theme.name),
                                );
                            }
                        }
                        app.mouse_col = 0;
                        app.mouse_row = 0;
                        app.performance_view.hover = None;
                    }
                }
            }
        }
        assert_readable(failures);
    }
}
