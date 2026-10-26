use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;

pub(crate) use crate::ui_markdown::draw_markdown_preview;
pub(crate) use crate::ui_diff_display::{draw_inline_diff, draw_side_by_side_diff};

#[cfg(test)]
thread_local! {
    pub(crate) static BYPASS_PANE_CHROME_CACHE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PaneChromeKey {
    area: Rect,
    editor: u64,
    active: usize,
    scroll: usize,
    tabs: bool,
    breadcrumb: bool,
}

/// One small chrome surface per pane, reused only on certified animation frames.
pub(crate) struct PaneChromeCache {
    key: PaneChromeKey,
    cells: ratatui::buffer::Buffer,
    #[cfg(test)]
    pub(crate) captures: usize,
    #[cfg(test)]
    pub(crate) replays: std::cell::Cell<usize>,
}

impl PaneChromeCache {
    fn replay(&self, frame: &mut Frame, key: PaneChromeKey) -> bool {
        if self.key != key { return false; }
        let buffer = frame.buffer_mut();
        for y in key.area.y..key.area.bottom() {
            let source = self.cells.index_of(key.area.x, y);
            let target = buffer.index_of(key.area.x, y);
            buffer.content[target..target + usize::from(key.area.width)]
                .clone_from_slice(&self.cells.content[source..source + usize::from(key.area.width)]);
        }
        #[cfg(test)]
        self.replays.set(self.replays.get() + 1);
        true
    }

    fn capture(slot: &mut Option<Self>, frame: &mut Frame, key: PaneChromeKey) {
        let cache = slot.get_or_insert_with(|| Self {
            key, cells: ratatui::buffer::Buffer::empty(key.area),
            #[cfg(test)]
            captures: 0,
            #[cfg(test)]
            replays: std::cell::Cell::new(0),
        });
        cache.key = key;
        cache.cells.resize(key.area);
        let buffer = frame.buffer_mut();
        for y in key.area.y..key.area.bottom() {
            let source = buffer.index_of(key.area.x, y);
            let target = cache.cells.index_of(key.area.x, y);
            cache.cells.content[target..target + usize::from(key.area.width)]
                .clone_from_slice(&buffer.content[source..source + usize::from(key.area.width)]);
        }
        #[cfg(test)]
        { cache.captures += 1; }
    }
}

fn draw_inactive_pane_chrome(
    f: &mut Frame, app: &App, layout: &[Rect], group_idx: usize,
    split_idx: usize, show_tabs: bool, show_breadcrumb: bool,
) {
    if show_tabs {
        crate::ui_chrome_tabs::draw_tab_bar_for_group(f,app,layout[0],group_idx,std::time::Instant::now());
    }

    // Draw breadcrumb for file tabs only; Settings and other workbench tabs
    // begin directly beneath their tab, like VS Code.
    if show_breadcrumb {
        let breadcrumb_area = layout[1];
        let file_path = app.groups[group_idx].tabs[split_idx].editor.file_path.as_deref();
        let bg = app.colors.ui.bg_breadcrumb;
        let fg = app.colors.ui.readable_foreground(app.colors.ui.fg_breadcrumb, bg, 4.5);
        let sep = " › ";
        let path_str = file_path
            .as_ref()
            .map_or_else(|| "untitled".to_string(), |p| p.strip_prefix(&app.explorer.root).unwrap_or(p).to_string_lossy().into_owned());
        let parts: Vec<&str> = path_str.split(['/', '\\']).collect();
        let dir_parts = &parts[..parts.len().saturating_sub(1)];
        let mut crumb_spans: Vec<Span> = vec![Span::styled("  ", Style::default().bg(bg))];
        for (i, part) in dir_parts.iter().enumerate() {
            if i > 0 {
                crumb_spans.push(Span::styled(sep, Style::default().fg(fg).bg(bg)));
            }
            let style = Style::default().fg(fg).bg(bg);
            crumb_spans.push(Span::styled(*part, style));
        }
        let used: usize = crumb_spans.iter().map(Span::width).sum();
        let remaining_w = (breadcrumb_area.width as usize).saturating_sub(used);
        crumb_spans.push(Span::styled(crate::helpers::spaces(remaining_w), Style::default().bg(bg)));
        f.render_widget(Paragraph::new(Line::from(crumb_spans)), breadcrumb_area);
    }

}

pub(crate) fn draw_split_editor_for_group(f: &mut Frame, app: &mut App, area: Rect, group_idx: usize) {
    if group_idx >= app.groups.len() || app.groups[group_idx].tabs.is_empty() {
        // Safety fallback: render empty area with background instead of leaving black
        let bg = app.colors.bg_dark;
        f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);
        return;
    }
    let split_idx = app.groups[group_idx].active_tab.min(app.groups[group_idx].tabs.len().saturating_sub(1));
    let split_tab_kind = app.groups[group_idx].tabs[split_idx].kind;
    let show_tabs = app.show_tab_bar && !app.zen_mode;
    let show_breadcrumb = app.show_breadcrumbs && !app.zen_mode
        && matches!(
            split_tab_kind,
            crate::app::TabKind::File | crate::app::TabKind::GitRevision | crate::app::TabKind::Diff(_)
        );

    // Tab bar + breadcrumb + content layout
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(u16::from(show_tabs)),
            Constraint::Length(u16::from(show_breadcrumb)),
            Constraint::Min(1),
        ])
        .split(area);

    let tab_bar_area = layout[0];
    let chrome_key = PaneChromeKey {
        area: Rect { height: layout[0].height + layout[1].height, ..area },
        editor: app.groups[group_idx].tabs[split_idx].editor.navigation_id,
        active: split_idx,
        scroll: app.groups[group_idx].tab_scroll_offset,
        tabs: show_tabs,
        breadcrumb: show_breadcrumb,
    };
    let cache_enabled = (app.animation_settings.any_enabled() || app.loading.active()) && !chrome_key.area.is_empty();
    #[cfg(test)]
    let cache_enabled = cache_enabled && !BYPASS_PANE_CHROME_CACHE.get();
    let reused = cache_enabled && app.rendering_animation_frame
        && app.groups[group_idx].inactive_chrome_cache.as_ref()
            .is_some_and(|cache| cache.replay(f, chrome_key));
    if !reused {
        draw_inactive_pane_chrome(f, app, &layout, group_idx, split_idx, show_tabs, show_breadcrumb);
        if cache_enabled {
            PaneChromeCache::capture(&mut app.groups[group_idx].inactive_chrome_cache, f, chrome_key);
        } else {
            app.groups[group_idx].inactive_chrome_cache = None;
        }
    }

    // Render editor content: temporarily swap in the group's tab
    let saved_tabs = std::mem::take(&mut app.tabs);
    let saved_active_tab = app.active_tab;
    let saved_code_area = app.code_area;
    let saved_gutter_area = app.gutter_area;
    let saved_gutter_width = app.gutter_width;
    let saved_minimap_area = app.minimap_area;
    let saved_scrollbar_area = app.scrollbar_area;
    let saved_h_scrollbar_area = app.h_scrollbar_area;
    let saved_diff_split_area = app.diff_split_area;
    let saved_diff_divider_area = app.diff_divider_area;
    let saved_diff_left_code_area = app.diff_left_code_area;
    let saved_diff_left_h_scrollbar_area = app.diff_left_h_scrollbar_area;
    let saved_rendering_inactive_editor = app.rendering_inactive_editor;

    app.tabs = std::mem::take(&mut app.groups[group_idx].tabs);
    app.active_tab = split_idx;
    app.rendering_inactive_editor = true;
    let saved_diff_view = app.begin_inactive_diff_view();
    let current_tab_kind = app.tabs.get(app.active_tab).map(|t| t.kind);
    if show_breadcrumb {
        crate::editor_blame::header(f, app, layout[1]);
    }
    crate::ui_loading::draw_editor_header(f, app, layout[0], layout[1]);
    if current_tab_kind == Some(crate::app::TabKind::CommitDetails) {
        app.swap_group_highlight_requests(group_idx);
        crate::ui_commit_details::draw_in_area(f, app, layout[2]);
        app.swap_group_highlight_requests(group_idx);
    } else if current_tab_kind == Some(crate::app::TabKind::Settings) {
        let saved_settings_sidebar_area = app.settings_sidebar_area;
        crate::ui_settings::draw_settings_editor(f, app, layout[2]);
        app.settings_sidebar_area = saved_settings_sidebar_area;
    } else if current_tab_kind == Some(crate::app::TabKind::KeyboardShortcuts) {
        crate::ui_keybindings::draw(f, app, layout[2]);
    } else if current_tab_kind == Some(crate::app::TabKind::ExtensionInfo) {
        crate::ui_extension_info::draw_extension_info(f, app, layout[2]);
    } else if let Some(crate::app::TabKind::Chat(key)) = current_tab_kind {
        crate::ui_chat::queue_editor(app, key, group_idx, layout[2]);
    } else if current_tab_kind == Some(crate::app::TabKind::MarkdownPreview) {
        app.code_area = None;
        app.gutter_area = None;
        app.minimap_area = None;
        app.scrollbar_area = None;
        app.h_scrollbar_area = None;
        draw_markdown_preview(f, app, layout[2]);
    } else if current_tab_kind == Some(crate::app::TabKind::Terminal) {
        app.code_area = None;
        app.gutter_area = None;
        app.minimap_area = None;
        app.scrollbar_area = None;
        app.h_scrollbar_area = None;
        draw_terminal_in_group(f, app, layout[2], group_idx);
    } else {
        if app.should_render_diff_for_active_editor() && !matches!(current_tab_kind, Some(crate::app::TabKind::Diff(_))) {
            app.refresh_compare_diff();
        }
        if app.diff_mode == crate::app::DiffMode::SideBySide
            && app.should_render_diff_for_active_editor()
        {
            draw_side_by_side_diff(f, app, layout[2]);
        } else if app.diff_mode == crate::app::DiffMode::Inline
            && app.should_render_diff_for_active_editor()
        {
            app.editor_area = Some(layout[2]);
            draw_inline_diff(f, app, layout[2]);
        } else {
            crate::ui_editor::draw_editor(f, app, layout[2]);
        }
    }

    // Save group's layout areas back
    app.groups[group_idx].code_area = app.code_area;
    app.groups[group_idx].gutter_area = app.gutter_area;
    app.groups[group_idx].gutter_width = app.gutter_width;
    app.groups[group_idx].minimap_area = app.minimap_area;
    app.groups[group_idx].scrollbar_area = app.scrollbar_area;
    app.groups[group_idx].h_scrollbar_area = app.h_scrollbar_area;
    app.groups[group_idx].tab_bar_area = show_tabs.then_some(tab_bar_area);
    app.groups[group_idx].active_tab = app.active_tab;

    // Restore active group state
    app.end_inactive_diff_view(saved_diff_view);
    app.groups[group_idx].tabs = std::mem::take(&mut app.tabs);
    app.tabs = saved_tabs;
    app.active_tab = saved_active_tab;
    app.code_area = saved_code_area;
    app.gutter_area = saved_gutter_area;
    app.gutter_width = saved_gutter_width;
    app.minimap_area = saved_minimap_area;
    app.scrollbar_area = saved_scrollbar_area;
    app.h_scrollbar_area = saved_h_scrollbar_area;
    app.diff_split_area = saved_diff_split_area;
    app.diff_divider_area = saved_diff_divider_area;
    app.diff_left_code_area = saved_diff_left_code_area;
    app.diff_left_h_scrollbar_area = saved_diff_left_h_scrollbar_area;
    app.rendering_inactive_editor = saved_rendering_inactive_editor;
}

pub(crate) fn draw_extensions_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    crate::ui_extensions::draw(f, app, area);
}

/// Render a terminal PTY inside an editor tab area
pub(crate) fn draw_terminal_in_tab(f: &mut Frame, app: &mut App, area: Rect) {
    let group = app.active_group;
    draw_terminal_in_group(f, app, area, group);
}

fn draw_terminal_in_group(f: &mut Frame, app: &mut App, area: Rect, group: usize) {
    let term_idx = app.tabs.get(app.active_tab).and_then(|t| t.terminal_idx);
    let Some(idx) = term_idx else {
        let msg = Paragraph::new("No terminal attached")
            .style(Style::default().fg(app.colors.fg_dim).bg(app.colors.bg_editor));
        f.render_widget(msg, area);
        return;
    };
    if idx >= app.terminals.len() {
        let msg = Paragraph::new("Terminal has been closed")
            .style(Style::default().fg(app.colors.fg_dim).bg(app.colors.bg_editor));
        f.render_widget(msg, area);
        return;
    }

    let width = area.width as usize;
    let height = area.height as usize;

    app.ensure_terminal_pty_at(idx, width, height);

    // Resize PTY to match tab area
    if let Some(ref mut pty) = app.terminals[idx].terminal_pty {
        pty.resize(width, height);
    }

    if let Some(ref pty) = app.terminals[idx].terminal_pty {
        app.terminal_surfaces.push(crate::app_terminal_selection::TerminalSurface {
            index: idx, area, group: Some(group),
        });
        draw_terminal_emulator(
            f,
            pty,
            area,
            &app.colors.ui,
            app.cursor_visible,
        );
    } else {
        draw_terminal_launch_error(
            f,
            app.terminals[idx].terminal_launch_error.as_deref(),
            area,
            app.colors.fg_text,
            app.colors.fg_dim,
            app.colors.fg_error,
            app.colors.bg_dark,
        );
    }
}

/// Render a parsed VT grid and expose the PTY cursor to Ratatui. Calling
/// `Frame::set_cursor_position` is important: without it the backend hides the
/// host cursor after every frame, even though the shell is tracking one.
pub(crate) fn draw_terminal_emulator(
    f: &mut Frame,
    pty: &crate::terminal::TerminalEmulator,
    area: Rect,
    palette: &crate::theme_workbench::WorkbenchColors,
    blink_visible: bool,
) {
    let width = area.width as usize;
    let height = area.height as usize;
    let default_fg = palette.fg_terminal;
    let default_bg = palette.bg_terminal;
    let grid = &pty.grid;
    let steady_cursor = matches!(grid.cursor_shape, 2 | 4 | 6);
    let show_cursor = grid.cursor_visible
        && !grid.has_selection()
        && grid.scroll_view == 0
        && (steady_cursor || blink_visible)
        && grid.cursor_row < height
        && grid.cursor_col < width;

    {
        let buffer = f.buffer_mut();
        pty.render_rows.borrow_mut().paint(buffer, grid, area, palette);

        // Keep a buffer-level cursor as well, so screenshots and terminals that
        // suppress the native cursor still show an insertion point.
        if show_cursor {
            let x = area.x + grid.cursor_col as u16;
            let y = area.y + grid.cursor_row as u16;
            if let Some(cell) = buffer.cell_mut((x, y)) {
                match grid.cursor_shape {
                    3 | 4 => {
                        if cell.symbol().trim().is_empty() {
                            cell.set_char('▁');
                        }
                        cell.set_style(
                            Style::default()
                                .fg(default_fg)
                                .bg(default_bg)
                                .add_modifier(Modifier::UNDERLINED),
                        );
                    }
                    5 | 6 => {
                        if cell.symbol().trim().is_empty() {
                            cell.set_char('▏');
                            cell.set_style(Style::default().fg(default_fg).bg(default_bg));
                        } else {
                            cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                        }
                    }
                    _ => {
                        cell.set_style(
                            crate::ui_colors::block_cursor_style(
                                palette.bg_terminal_cursor, palette.fg_terminal_cursor, cell.bg, palette.contrast_colors()).remove_modifier(Modifier::DIM),
                        );
                    }
                }
            }
        }
    }

    if show_cursor {
        f.set_cursor_position((
            area.x + grid.cursor_col as u16,
            area.y + grid.cursor_row as u16,
        ));
    }
}

pub(crate) fn draw_terminal_launch_error(
    f: &mut Frame,
    error: Option<&str>,
    area: Rect,
    foreground: Color,
    dim: Color,
    error_color: Color,
    background: Color,
) {
    let mut lines = vec![
        Line::from(Span::styled(
            " The terminal process failed to launch",
            Style::default().fg(error_color).bg(background),
        )),
        Line::from(Span::styled(
            format!(" {}", error.unwrap_or("A shell has not been started.")),
            Style::default().fg(foreground).bg(background),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " Press Enter to retry",
            Style::default().fg(dim).bg(background),
        )),
    ];
    lines.truncate(area.height as usize);
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(background)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn inline_input(
        text: &str,
        cursor: Option<usize>,
        selection: Option<(usize, usize)>,
    ) -> ratatui::buffer::Buffer {
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let mut terminal = Terminal::new(TestBackend::new(10, 1)).unwrap();
        terminal.draw(|frame| {
            let area = frame.area();
            let field = crate::ui_widgets::InputField::new(text)
                .theme_colors(&colors)
                .prefix("", Color::Reset)
                .selection_range(selection);
            let field = if let Some(cursor) = cursor {
                field.cursor_pos(cursor)
            } else {
                field.cursor_marker("")
            };
            field.render(frame, area);
        }).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn inline_input_no_cursor_no_selection() {
        let buffer = inline_input("hello", None, None);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert_eq!(text.trim_end(), "hello");
        assert!(buffer.content.iter().all(|cell| !cell.modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn inline_input_cursor_at_start() {
        let buffer = inline_input("abc", Some(0), None);
        assert_eq!(buffer[(0, 0)].symbol(), "a");
        assert_eq!(buffer[(1, 0)].symbol(), "b");
        assert_eq!(buffer[(2, 0)].symbol(), "c");
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(1, 0)].modifier.contains(Modifier::BOLD));
        assert_ne!(buffer[(0, 0)].bg, buffer[(1, 0)].bg);
    }

    #[test]
    fn inline_input_cursor_at_end_adds_space() {
        let buffer = inline_input("ab", Some(2), None);
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert!(buffer[(2, 0)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(3, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn inline_input_selection_range() {
        let buffer = inline_input("hello", Some(5), Some((1, 4)));
        assert_ne!(buffer[(0, 0)].bg, buffer[(1, 0)].bg);
        assert_eq!(buffer[(1, 0)].bg, buffer[(2, 0)].bg);
        assert_eq!(buffer[(2, 0)].bg, buffer[(3, 0)].bg);
        assert_eq!(buffer[(0, 0)].bg, buffer[(4, 0)].bg);
    }

    #[test]
    fn inline_input_empty_text_with_cursor() {
        let buffer = inline_input("", Some(0), None);
        assert_eq!(buffer[(0, 0)].symbol(), " ");
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn split_file_tab_icon_uses_the_explorer_file_type_color() {
        let mut app = App::test_new();
        let mut editor = crate::editor::Editor::new();
        editor.file_path = Some(app.explorer.root.join(".gitignore"));
        let mut group = crate::app::EditorGroup::new();
        group.tabs.push(crate::app::Tab::new_file(editor));
        app.groups.push(group);

        let mut terminal = Terminal::new(TestBackend::new(40, 6)).expect("terminal");
        terminal
            .draw(|frame| {
                draw_split_editor_for_group(frame, &mut app, Rect::new(0, 0, 40, 6), 0)
            })
            .expect("draw split editor");

        let expected_icon_color = ratatui::style::Color::Rgb(241, 80, 47);
        let icon_cell = terminal.backend().buffer().cell((5, 0)).unwrap();
        assert_eq!(icon_cell.symbol(), "G");
        assert_eq!(icon_cell.fg, expected_icon_color);
    }

    #[test]
    fn settings_split_tab_never_shows_an_unsaved_marker() {
        let mut app = App::test_new();
        app.open_settings_tab();
        let mut group = crate::app::EditorGroup::new();
        group.tabs.push(app.tabs.pop().unwrap());
        app.groups.push(group);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let tab_text = |terminal: &Terminal<TestBackend>| {
            (0..80).map(|x| terminal.backend().buffer().cell((x, 0)).unwrap().symbol()).collect::<String>()
        };
        terminal.draw(|frame| draw_split_editor_for_group(frame, &mut app, Rect::new(0, 0, 80, 24), 0)).unwrap();
        let clean = tab_text(&terminal);
        assert!(clean.contains("Settings"));
        app.groups[0].tabs[0].editor.modified = true;
        terminal.draw(|frame| draw_split_editor_for_group(frame, &mut app, Rect::new(0, 0, 80, 24), 0)).unwrap();
        assert_eq!(tab_text(&terminal), clean);
        assert!(!clean.contains('●'));
    }
}

#[cfg(test)]
#[path = "ui_terminal_performance_tests.rs"]
mod terminal_performance_tests;
