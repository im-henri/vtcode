//! An extension's documentation and management page, beside the installed list.
use crate::{
    app::{ActivePane, App, Tab, TabKind},
    extensions::{ExtensionRecordView, ExtensionState},
    ui_extensions::{clean, clip, publisher, status},
    ui_markdown::EmbeddedMarkdown,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};
use rustc_hash::FxHashMap;
use std::{io::Read, path::PathBuf, time::SystemTime};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Section {
    #[default]
    Details,
    Features,
    Runtime,
}
const SECTIONS: [(Section, &str); 3] = [
    (Section::Details, "DETAILS"),
    (Section::Features, "FEATURES"),
    (Section::Runtime, "RUNTIME"),
];
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Toggle,
    Reload,
    Versions,
    Repository,
    CopyId,
}
#[derive(Default)]
struct Page {
    section: Section,
    scroll: usize,
    source: Option<(PathBuf, String, Option<SystemTime>)>,
    readme: Option<EmbeddedMarkdown>,
}
#[derive(Default)]
pub(crate) struct State {
    pages: FxHashMap<String, Page>,
    id: String,
    views: Vec<View>,
    focus: Option<usize>,
    pressed: Option<MouseButton>,
}
#[derive(Default)]
struct View {
    editor: u64,
    id: String,
    area: Rect,
    body: Rect,
    total_rows: usize,
    actions: Vec<(Action, Rect)>,
    sections: Vec<(Section, Rect)>,
}

pub(crate) fn begin_frame(app: &mut App) {
    app.ext_info.views.clear();
}

pub(crate) fn cancel_drag(app: &mut App) {
    app.ext_info.pressed = None;
}

fn active_view(app: &App) -> Option<&View> {
    let editor = app.tabs.get(app.active_tab)?.editor.navigation_id;
    app.ext_info
        .views
        .iter()
        .find(|view| view.editor == editor && view.id == app.ext_info.id)
}

fn view_group(app: &App, view: &View) -> Option<usize> {
    let group = app.visible_tab_group(view.editor, TabKind::ExtensionInfo)?;
    let tab = if group == app.active_group {
        app.tabs.get(app.active_tab)
    } else {
        app.groups
            .get(group)
            .and_then(|group| group.tabs.get(group.active_tab))
    }?;
    // Preview replacement can change the represented extension without a new
    // frame. A previous extension's actions must never target the new one.
    tab.editor
        .file_path
        .as_deref()
        .and_then(|path| path.to_str())
        .and_then(|path| path.rsplit_once("[ext-info:"))
        .is_some_and(|(_, tail)| tail.strip_suffix(']') == Some(view.id.as_str()))
        .then_some(group)
}

pub(crate) fn tab_extension_id(tab: &Tab) -> Option<String> {
    let path = tab.editor.file_path.as_ref()?.to_string_lossy();
    let (_, id) = path.rsplit_once("[ext-info:")?;
    Some(id.strip_suffix(']')?.to_string())
}
fn active_id(app: &App) -> Option<String> {
    let tab = app.tabs.get(app.active_tab)?;
    (tab.kind == TabKind::ExtensionInfo)
        .then(|| tab_extension_id(tab))
        .flatten()
}
fn repository_url(ext: &ExtensionRecordView) -> Option<String> {
    let source = crate::extension_install::ExtensionSource::parse(&ext.repository).ok()?;
    let url = url::Url::parse(&source.repository).ok()?;
    Some(format!(
        "https://{}{}",
        url.host_str()?,
        url.path().trim_end_matches(".git")
    ))
}
fn run_action(app: &mut App, action: Action) {
    let Some(id) = active_id(app) else {
        return;
    };
    let views = app.extension_views();
    let Some(index) = views.iter().position(|ext| ext.id == id) else {
        return;
    };
    let ext = &views[index];
    match action {
        Action::Toggle | Action::Reload => {
            // The page can stay open while another extension is selected in the sidebar.
            let result = match action {
                Action::Reload => app.extension_manager.reload_extension(index),
                _ if ext.state == ExtensionState::Loaded => {
                    app.extension_manager.unload_extension(index)
                }
                _ => app.extension_manager.load_extension(index),
            };
            app.notify(result.unwrap_or_else(|error| error));
            app.refresh_palette_commands();
            app.refresh_open_extension_view_tabs();
            app.clamp_extensions_selection();
        }
        Action::Versions => {
            app.open_extension_install_prompt();
            app.palette.query = ext.repository.clone();
            app.palette.cursor = app.palette.query.chars().count();
            app.palette.selection_anchor = None;
            app.refresh_extension_versions();
        }
        Action::Repository => {
            if let Some(url) = repository_url(ext) {
                app.open_external(crate::external_open::Request::Url(url));
            }
        }
        Action::CopyId => match crate::clipboard::set_text(&id) {
            Ok(()) => app.notify("Extension identifier copied"),
            Err(error) => app.notify(format!("Could not copy identifier: {error}")),
        },
    }
}
fn prepare_readme(app: &mut App, ext: &ExtensionRecordView) {
    let Some(record) = app
        .extension_manager
        .records
        .iter()
        .find(|record| record.manifest.id == ext.id)
    else {
        return;
    };
    let source = (
        record.manifest_path.clone(),
        ext.version.clone(),
        record.manifest_mtime,
    );
    let page = app.ext_info.pages.entry(ext.id.clone()).or_default();
    if page.source.as_ref() == Some(&source) {
        return;
    }
    page.source = Some(source);
    page.readme = None;
    if let Some(directory) = record.manifest_path.parent() {
        for name in [
            "README.md",
            "readme.md",
            "Readme.md",
            "README.markdown",
            "README",
        ] {
            if let Ok(file) = std::fs::File::open(directory.join(name)) {
                let mut text = String::new();
                if file.take(512 * 1024).read_to_string(&mut text).is_ok()
                    && !text.trim().is_empty()
                {
                    page.readme = Some(EmbeddedMarkdown::new(&text));
                    break;
                }
            }
        }
    }
}
fn metadata(ext: &ExtensionRecordView) -> String {
    format!(
        "### Installation\n\n**Identifier**\n\n{}\n\n**Version**\n\n{}\n\n**Publisher**\n\n{}\n\n**Status**\n\n{}\n\n### Resources\n\n{}\n\nUse **Versions** to install another release from this repository.",
        clean(&ext.id),
        clean(&ext.version),
        clean(publisher(ext)),
        status(ext),
        if ext.repository.is_empty() {
            "Local extension".into()
        } else {
            clean(&ext.repository)
        }
    )
}
fn page_lines(
    app: &mut App,
    ext: &ExtensionRecordView,
    width: u16,
    include_metadata: bool,
) -> Vec<Line<'static>> {
    let page = app.ext_info.pages.entry(ext.id.clone()).or_default();
    let mut lines = match page.section {
        Section::Details if page.readme.is_some() => page
            .readme
            .as_mut()
            .unwrap()
            .lines(usize::from(width), &app.colors, app.colors.bg_editor)
            .to_vec(),
        Section::Details => {
            let description = if ext.description.is_empty() {
                "This extension has not provided a description."
            } else {
                &ext.description
            };
            crate::ui_markdown::chat_lines_on_surface(
                &format!(
                    "# {}\n\n{}\n\n## About this extension\n\nNo README was included with this extension. Explore **Features** for contributed commands and views, or **Runtime** for status and logs.",
                    clean(&ext.name),
                    clean(description)
                ),
                usize::from(width),
                &app.colors,
                None,
                app.colors.bg_editor,
            )
        }
        Section::Features => {
            let mut text = String::from("# Features\n\n## Commands\n\n");
            if ext.commands.is_empty() {
                text.push_str("No commands are currently registered. Disabled extensions register their commands when enabled.\n\n");
            }
            for command in &ext.commands {
                text.push_str(&format!(
                    "- **{}** · `{}`\n",
                    clean(&command.title),
                    clean(&command.id)
                ));
            }
            text.push_str(
                "\nRun extension commands from the command palette (F1).\n\n## Views\n\n",
            );
            if ext.views.is_empty() {
                text.push_str("No views are currently registered.\n");
            }
            let mut views: Vec<_> = ext.views.iter().collect();
            views.sort_by_key(|(id, _)| *id);
            for (id, view) in views {
                text.push_str(&format!("- **{}** · `{}`\n", clean(&view.title), clean(id)));
            }
            crate::ui_markdown::chat_lines_on_surface(
                &text,
                usize::from(width),
                &app.colors,
                None,
                app.colors.bg_editor,
            )
        }
        Section::Runtime => {
            let text = format!(
                "# Runtime\n\n**State: {}** · {}\n\n{}",
                ext.state.label(),
                clean(&ext.status_text),
                if ext.state == ExtensionState::Unloaded {
                    "Enable the extension to register its commands and views."
                } else {
                    "Reload the extension to restart its runtime."
                }
            );
            let mut lines = crate::ui_markdown::chat_lines_on_surface(
                &text,
                usize::from(width),
                &app.colors,
                None,
                app.colors.bg_editor,
            );
            if let Some(error) = &ext.last_error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Last error",
                    Style::default()
                        .fg(app.colors.ui.error_text_color(app.colors.bg_editor))
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(crate::ui_markdown::chat_lines_on_surface(
                    &clean(error),
                    usize::from(width),
                    &app.colors,
                    None,
                    app.colors.bg_editor,
                ));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Console output",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            if ext.logs.is_empty() {
                lines.push(Line::from("No output yet."));
            }
            for log in &ext.logs {
                // Logs are literal text, never Markdown or byte-sliced UTF-8.
                let text = clean(log);
                let mut row = String::new();
                for grapheme in text.graphemes(true) {
                    if row.width() + grapheme.width() > usize::from(width) && !row.is_empty() {
                        lines.push(Line::from(std::mem::take(&mut row)));
                    }
                    row.push_str(grapheme);
                }
                lines.push(Line::from(row));
            }
            lines
        }
    };
    if include_metadata {
        lines.push(Line::from(""));
        lines.extend(crate::ui_markdown::chat_lines_on_surface(
            &metadata(ext),
            usize::from(width),
            &app.colors,
            None,
            app.colors.bg_editor,
        ));
    }
    lines
}

pub(crate) fn draw_extension_info(f: &mut Frame, app: &mut App, area: Rect) {
    let bg = app.colors.bg_editor;
    let fg = app.colors.ui.workbench_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    let style = Style::default().fg(fg).bg(bg);
    app.code_area = None;
    app.gutter_area = None;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    let interactive = !app.rendering_inactive_editor;
    f.render_widget(Block::default().style(style), area);
    if area.is_empty() {
        return;
    }
    let Some(id) = active_id(app) else {
        return;
    };
    let editor = app.tabs[app.active_tab].editor.navigation_id;
    app.ext_info.views.retain(|view| view.editor != editor);
    let slot = app.ext_info.views.len();
    app.ext_info.views.push(View {
        editor,
        id: id.clone(),
        area,
        ..Default::default()
    });
    if interactive && app.ext_info.id != id {
        app.ext_info.id = id.clone();
        app.ext_info.focus = None;
    }
    let Some(ext) = app.extension_views().into_iter().find(|ext| ext.id == id) else {
        f.render_widget(
            Paragraph::new(
                "This extension is no longer installed.\nReturn to Extensions to install it again.",
            )
            .style(style.fg(dim))
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    };
    prepare_readme(app, &ext);
    let padding = if area.width >= 50 {
        3
    } else {
        1.min(area.width / 2)
    };
    let inner = Rect::new(
        area.x + padding,
        area.y,
        area.width.saturating_sub(padding * 2),
        area.height,
    );
    let badge_width = if inner.width >= 62 && inner.height >= 16 {
        9
    } else {
        0
    };
    let title_x = inner.x + badge_width;
    let title_w = inner.width.saturating_sub(badge_width);
    let top = inner.y + u16::from(inner.height >= 12);
    if badge_width > 0 {
        let letter = ext
            .name
            .graphemes(true)
            .find(|grapheme| grapheme.width() == 1 && !grapheme.trim().is_empty())
            .unwrap_or("+");
        let badge = format!("┌─────┐\n│     │\n│  {letter}  │\n│     │\n└─────┘");
        let badge_style = app.colors.ui.button_style(bg, false, false, true);
        f.render_widget(
            Paragraph::new(badge).style(badge_style),
            Rect::new(inner.x, top, 7, 5),
        );
    }
    f.render_widget(
        Paragraph::new(clip(&ext.name, title_w)).style(style.add_modifier(Modifier::BOLD)),
        Rect::new(title_x, top, title_w, 1),
    );
    if top + 1 < inner.bottom() {
        f.render_widget(
            Paragraph::new(clip(
                &format!(
                    "{}   ·   v{}   ·   VTCode extension",
                    publisher(&ext),
                    ext.version
                ),
                title_w,
            ))
            .style(style.fg(dim)),
            Rect::new(title_x, top + 1, title_w, 1),
        );
    }
    if top + 2 < inner.bottom() {
        f.render_widget(
            Paragraph::new(clip(&ext.description, title_w)).style(style),
            Rect::new(title_x, top + 2, title_w, 1),
        );
    }
    let mut x = title_x;
    let mut y = (top + 4).min(inner.bottom());
    let mut action_hits = Vec::new();
    let toggle = match ext.state {
        ExtensionState::Loaded => "Disable",
        ExtensionState::Error => "Retry",
        ExtensionState::Unloaded => "Enable",
    };
    let labels = [
        (Action::Toggle, toggle),
        (Action::Reload, "Reload"),
        (Action::Versions, "Versions"),
        (Action::Repository, "Repository ↗"),
        (Action::CopyId, "Copy ID"),
    ];
    for (action, label) in labels {
        if matches!(action, Action::Versions | Action::Repository) && repository_url(&ext).is_none()
        {
            continue;
        }
        let width = (label.width() as u16 + 2).min(title_w);
        if x > title_x && x + width > inner.right() {
            x = title_x;
            y += 2;
        }
        if y >= inner.bottom().saturating_sub(3) || width == 0 {
            break;
        }
        let rect = Rect::new(x, y, width, 1);
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        let focused = interactive
            && app.active_pane == ActivePane::Editor
            && app.ext_info.focus == Some(action_hits.len());
        let button_style = crate::ui_widgets::action_button_style(
            &app.colors,
            bg,
            action == Action::Toggle && ext.state != ExtensionState::Loaded,
            hovered || focused,
            true,
        );
        f.render_widget(
            Paragraph::new(clip(&format!(" {label} "), width)).style(button_style.add_modifier(
                if focused {
                    Modifier::UNDERLINED
                } else {
                    Modifier::empty()
                },
            )),
            rect,
        );
        action_hits.push((action, rect));
        x += width + 1;
    }
    y = (y + 2).min(inner.bottom());
    let (note, color) = match ext.state {
        ExtensionState::Loaded => ("● This extension is enabled.", app.colors.fg_success),
        ExtensionState::Error => (
            "✖ Activation failed. See Runtime for details, or Retry.",
            app.colors.ui.error_text_color(bg),
        ),
        ExtensionState::Unloaded if !ext.enabled => {
            ("○ This extension is disabled by its configuration.", dim)
        }
        ExtensionState::Unloaded => ("○ This extension is disabled for this session.", dim),
    };
    if y < inner.bottom() {
        f.render_widget(
            Paragraph::new(clip(note, inner.width))
                .style(style.fg(app.colors.ui.readable_foreground(color, bg, 4.5))),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 2;
    }
    let section = app.ext_info.pages.entry(id.clone()).or_default().section;
    let mut section_hits = Vec::new();
    x = inner.x;
    if y < inner.bottom() {
        for (index, (tab, label)) in SECTIONS.iter().enumerate() {
            let width = (label.width() as u16 + 2).min(inner.right().saturating_sub(x));
            if width == 0 {
                break;
            }
            let rect = Rect::new(x, y, width, 1);
            let selected = section == *tab;
            let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
            let focused = interactive && app.ext_info.focus == Some(action_hits.len() + index);
            let color = if selected {
                app.colors
                    .ui
                    .readable_foreground(app.colors.fg_accent, bg, 4.5)
            } else {
                dim
            };
            f.render_widget(
                Paragraph::new(clip(&format!(" {label} "), width)).style(
                    style
                        .fg(color)
                        .add_modifier(if selected || hovered || focused {
                            Modifier::UNDERLINED
                        } else {
                            Modifier::empty()
                        }),
                ),
                rect,
            );
            section_hits.push((*tab, rect));
            x += width + 1;
        }
    }
    y = (y + 2).min(inner.bottom());
    let body = Rect::new(
        inner.x,
        y,
        inner.width,
        inner.bottom().saturating_sub(y + 1),
    );
    let side_width = if body.width >= 100 && body.height >= 10 {
        28
    } else {
        0
    };
    let content = Rect::new(
        body.x,
        body.y,
        body.width.saturating_sub(side_width),
        body.height,
    );
    let text_width = content.width.saturating_sub(2).max(1);
    let lines = page_lines(app, &ext, text_width, side_width == 0);
    let aside = Rect::new(
        content.right().saturating_add(2),
        body.y,
        side_width.saturating_sub(2),
        body.height,
    );
    let aside_lines = if side_width > 0 {
        crate::ui_markdown::chat_lines_on_surface(
            &metadata(&ext),
            usize::from(aside.width),
            &app.colors,
            None,
            bg,
        )
    } else {
        vec![]
    };
    let total_rows = lines.len().max(aside_lines.len());
    let page = app.ext_info.pages.get_mut(&id).unwrap();
    page.scroll = page
        .scroll
        .min(total_rows.saturating_sub(usize::from(content.height)));
    if !content.is_empty() {
        f.render_widget(
            Paragraph::new(
                lines
                    .iter()
                    .skip(page.scroll)
                    .take(usize::from(content.height))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .style(style),
            Rect::new(
                content.x,
                content.y,
                text_width.min(content.width),
                content.height,
            ),
        );
        if total_rows > usize::from(content.height) {
            let thumb_h = (usize::from(content.height).pow(2) / total_rows).max(1) as u16;
            let thumb_y = content.y
                + ((usize::from(content.height - thumb_h) * page.scroll)
                    / total_rows
                        .saturating_sub(usize::from(content.height))
                        .max(1)) as u16;
            f.render_widget(
                Paragraph::new("▏\n".repeat(usize::from(thumb_h))).style(style.fg(dim)),
                Rect::new(content.right() - 1, thumb_y, 1, thumb_h),
            );
        }
    }
    if side_width > 0 {
        f.render_widget(
            Paragraph::new(
                aside_lines
                    .into_iter()
                    .skip(page.scroll)
                    .take(usize::from(aside.height))
                    .collect::<Vec<_>>(),
            )
            .style(style),
            aside,
        );
    }
    if inner.height >= 8 {
        f.render_widget(
            Paragraph::new(clip(
                "↑↓ Scroll   ←→ Sections   Tab Actions   Esc Extensions",
                inner.width,
            ))
            .style(style.fg(dim)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
    let view = &mut app.ext_info.views[slot];
    view.body = content;
    view.total_rows = total_rows;
    view.actions = action_hits;
    view.sections = section_hits;
}
fn scroll(app: &mut App, delta: isize) {
    let max = active_view(app).map_or(0, |view| {
        view.total_rows
            .saturating_sub(usize::from(view.body.height))
    });
    let state = &mut app.ext_info;
    if let Some(page) = state.pages.get_mut(&state.id) {
        page.scroll = page.scroll.saturating_add_signed(delta).min(max);
    }
}
fn select_section(app: &mut App, section: Section) {
    if let Some(page) = app.ext_info.pages.get_mut(&app.ext_info.id) {
        page.section = section;
        page.scroll = 0;
    }
}
pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.active_pane != ActivePane::Editor {
        return false;
    }
    let Some(id) = active_id(app) else {
        return false;
    };
    if app.ext_info.id != id {
        app.ext_info.pages.entry(id.clone()).or_default();
        app.ext_info.id = id;
        app.ext_info.focus = None;
    }
    let (height, action_count, section_count) = active_view(app).map_or((0, 0, 0), |view| {
        (view.body.height, view.actions.len(), view.sections.len())
    });
    match key.code {
        KeyCode::Esc => {
            app.open_extensions_sidebar();
            app.ext_sidebar.search_focused = false;
        }
        KeyCode::Up => scroll(app, -1),
        KeyCode::Down => scroll(app, 1),
        KeyCode::PageUp => scroll(app, -(isize::try_from(height).unwrap_or(1)).max(1)),
        KeyCode::PageDown => scroll(app, isize::try_from(height).unwrap_or(1).max(1)),
        KeyCode::Home => scroll(app, isize::MIN),
        KeyCode::End => scroll(app, isize::MAX),
        KeyCode::Left | KeyCode::Right => {
            let current = app.ext_info.pages.get(&app.ext_info.id).map_or(0, |page| {
                SECTIONS
                    .iter()
                    .position(|(section, _)| *section == page.section)
                    .unwrap_or(0)
            });
            let index = (current + if key.code == KeyCode::Left { 2 } else { 1 }) % 3;
            select_section(app, SECTIONS[index].0);
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let count = action_count + section_count;
            if count > 0 {
                let backwards =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                app.ext_info.focus = Some(
                    app.ext_info
                        .focus
                        .map_or(if backwards { count - 1 } else { 0 }, |index| {
                            (index + if backwards { count - 1 } else { 1 }) % count
                        }),
                );
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(index) = app.ext_info.focus {
                if let Some(action) = active_view(app)
                    .and_then(|view| view.actions.get(index))
                    .map(|(action, _)| *action)
                {
                    run_action(app, action);
                } else if let Some(section) = active_view(app)
                    .and_then(|view| view.sections.get(index.saturating_sub(action_count)))
                    .map(|(section, _)| *section)
                {
                    select_section(app, section);
                }
            }
        }
        _ => {}
    }
    true
}
pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    // A drag originating in another workbench pane keeps its gesture.
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.ext_info.pressed = None;
    }
    if let MouseEventKind::Drag(button) | MouseEventKind::Up(button) = mouse.kind {
        if app.ext_info.pressed != Some(button) {
            return false;
        }
        if matches!(mouse.kind, MouseEventKind::Up(_)) {
            app.ext_info.pressed = None;
        }
        return true;
    }
    let point = (mouse.column, mouse.row).into();
    let Some(index) = app
        .ext_info
        .views
        .iter()
        .position(|view| view.area.contains(point) && view_group(app, view).is_some())
    else {
        return false;
    };
    match mouse.kind {
        MouseEventKind::Moved => {
            app.reset_hover_states();
            app.clear_transient_editor_state();
            app.mouse_hover_changed = true;
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let view = &app.ext_info.views[index];
            let max = view
                .total_rows
                .saturating_sub(usize::from(view.body.height));
            if let Some(page) = app.ext_info.pages.get_mut(&view.id) {
                page.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    page.scroll.saturating_sub(app.scroll_speed)
                } else {
                    page.scroll.saturating_add(app.scroll_speed)
                }
                .min(max);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let view = &app.ext_info.views[index];
            let group = view_group(app, view).unwrap();
            let id = view.id.clone();
            let action = view
                .actions
                .iter()
                .enumerate()
                .find(|(_, (_, area))| area.contains(point))
                .map(|(index, (action, _))| (index, *action));
            let section = view
                .sections
                .iter()
                .enumerate()
                .find(|(_, (_, area))| area.contains(point))
                .map(|(index, (section, _))| (view.actions.len() + index, *section));
            app.auto_save_on_focus_change();
            app.switch_group(group);
            app.active_pane = ActivePane::Editor;
            app.terminal_panel_focused = false;
            app.search_input_focused = false;
            app.ext_info.id = id;
            if let Some((index, action)) = action {
                app.ext_info.focus = Some(index);
                run_action(app, action);
            } else if let Some((index, section)) = section {
                app.ext_info.focus = Some(index);
                select_section(app, section);
            } else {
                app.ext_info.focus = None;
            }
        }
        _ => {}
    }
    if let MouseEventKind::Down(button) = mouse.kind {
        app.clear_drag_states();
        app.drag_start_pos = None;
        app.drag_has_moved = false;
        app.ext_info.pressed = Some(button);
    }
    true
}
