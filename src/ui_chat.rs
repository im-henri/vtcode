//! Theme-native ACP conversation dock. Message layout shares the editor's
//! CommonMark renderer; tool cards retain protocol identities across updates.
use crate::{
    app::{ActivePane, App},
    app_acp::{ChatAction, ChatHit, Picker, OutputDetail},
    app_theme::ThemeColors,
    text_input::{TextInputHitMap, TextInputId},
    ui_colors::disabled_text_color,
};
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vtcode::acp::{ChatEntry, Session, content_text, protocol as p};

#[path = "ui_chat_context.rs"]
mod context;
#[path = "ui_chat_surfaces.rs"]
mod surfaces;
pub(crate) use surfaces::{draw_surfaces, queue_editor};
#[path = "ui_chat_tabs.rs"]
mod tabs;
#[path = "ui_chat_agents.rs"]
mod agents;
#[path = "ui_chat_terminals.rs"]
mod terminals;
#[path = "ui_chat_tool_terminals.rs"]
mod tool_terminals;
#[path = "ui_chat_tools.rs"]
mod tools;
#[path = "ui_chat_tasks.rs"]
mod tasks;
#[path = "ui_chat_activity.rs"]
mod activity;
pub(crate) use activity::{ActivityAnimation, advance_activity_frame, next_activity_frame};
#[path = "ui_chat_permissions.rs"]
mod permissions;
#[path = "ui_chat_queue.rs"]
mod queue;
#[path = "ui_chat_widgets.rs"]
mod widgets;
#[path = "ui_chat_header.rs"]
mod header;
use header::draw as draw_header;
#[cfg(test)]
use header::layout as header_layout;
#[cfg(test)]
#[path = "ui_chat_terminal_tests.rs"]
mod terminal_tests;
#[cfg(test)]
#[path = "ui_chat_presentation_tests.rs"]
mod presentation_tests;
#[cfg(test)]
#[path = "ui_chat_controls_tests.rs"]
mod controls_tests;
#[cfg(test)]
#[path = "ui_chat_followups_tests.rs"]
mod followups_tests;
#[cfg(test)]
#[path = "ui_chat_thinking_tests.rs"]
mod thinking_tests;
#[cfg(test)]
#[path = "ui_chat_model_picker_tests.rs"]
mod model_picker_tests;
#[cfg(test)]
#[path = "ui_chat_picker_hover_tests.rs"]
mod picker_hover_tests;
#[cfg(test)]
#[path = "ui_chat_tasks_tests.rs"]
mod tasks_tests;
#[cfg(test)]
#[path = "ui_chat_activity_tests.rs"]
mod activity_tests;

#[derive(Clone)]
struct ChatRow {
    line: Line<'static>,
    action: Option<ChatAction>,
    background: Option<Color>,
    terminal: Option<terminals::TerminalRow>,
    separator: bool,
}

#[derive(Default)]
pub(crate) struct ChatCache {
    context: Option<ChatContext>,
    revision: Option<u64>,
    entries: Vec<(u64, bool, Vec<Arc<ChatRow>>)>,
    rows: Vec<Arc<ChatRow>>,
}

impl ChatCache {
    pub(crate) fn entry_height(&self, index: usize) -> usize {
        self.entries.get(index).map_or(0, |entry| entry.2.len())
    }
    pub(crate) fn entry_has_terminal(&self, index: usize) -> bool {
        self.entries.get(index).is_some_and(|entry| entry.2.iter().any(|row| row.terminal.is_some()))
    }
}

#[derive(PartialEq)]
struct ChatContext {
    preserve_theme_colors: bool,
    output_detail: OutputDetail,
    session: p::SessionId,
    history_epoch: u64,
    width: u16,
    colors: Vec<Color>,
    agent_name: String,
    syntax_name: String,
    syntax_generation: u64,
}

struct ChatTheme<'a> {
    colors: &'a ThemeColors,
    highlighter: &'a crate::highlight::Highlighter,
    syntax_name: &'a str,
}

type ChatSyntax<'a> = Option<(
    &'a crate::textmate::SyntaxRegistry,
    Option<&'a crate::textmate::Theme>,
)>;

const ACTIVITY_INTERVAL: Duration = Duration::from_millis(120);
const ACTIVITY_FRAMES: [&str; 10] = ["⠋ ", "⠙ ", "⠹ ", "⠸ ", "⠼ ", "⠴ ", "⠦ ", "⠧ ", "⠇ ", "⠏ "];
const FILES_BUTTON: &str = " + Files ";
const COMMANDS_BUTTON: &str = " Commands ";

fn activity_row(app: &App) -> Option<ChatRow> {
    let session = app.chat.session().filter(|s| s.busy)?;
    let spinner = app.chat.activity.symbol();
    let (icon, label, color) = if session.cancelling {
        (spinner, "Stopping…", app.colors.fg_warning)
    } else if app.chat.permission().is_some() {
        ("○", "Waiting for permission", app.colors.fg_warning)
    } else {
        (spinner, "Working…", app.colors.fg_accent_blue)
    };
    Some(ChatRow {
        line: Line::from(vec![
            Span::styled(format!("{icon}  "), Style::default().fg(color)),
            Span::styled(label, Style::default().fg(app.colors.fg_dim)),
        ]),
        action: None,
        background: None,
        terminal: None,
        separator: false,
    })
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.chat.area = Some(area);
    app.chat.transcript_area = None;
    app.chat.context_usage_area = None;
    app.chat.model_selector_area = None;
    app.chat.tab_area = None;
    app.chat.agent_tab_area = None;
    app.chat.hits.clear();
    app.chat.terminal_areas.clear();
    app.chat.scrollbars.clear();
    app.chat.files.popup = None;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let c = &app.colors;
    let border = if app.chat.resizing || app.mouse_col == area.x {
        c.ui.sash_hover_border
    } else {
        c.ui.sidebar_border.or(c.ui.border_sidebar
            .map(crate::theme_workbench::ColorLayer::opaque).unwrap_or_default())
            .or(crate::theme_workbench::ColorLayer::opaque(c.ui.fg_group_border))
    }.border(c.bg_sidebar).unwrap_or(c.bg_sidebar);
    f.render_widget(
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(border))
            .style(Style::default().fg(c.ui.fg_sidebar).bg(c.bg_sidebar)),
        area,
    );
    let body = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(1),
        area.height,
    );
    if body.width < 6 || body.height < 3 {
        return;
    }
    if app.terminal_has_focus && body.height >= 6 && !app.chat.loading_conversation() {
        app.chat.acknowledge_selected_activity();
    }
    let name = app.chat.name().to_owned();
    let header_height = draw_header(f, app, body);
    if body.height < 6 {
        return;
    }
    let auth_available = app
        .chat
        .initialized
        .as_ref()
        .is_some_and(|i| !i.auth_methods.is_empty());

    let width = body.width.saturating_sub(6).max(1) as usize;
    app.chat.connection.input_layout.layout(&app.chat.connection.input, width);
    let input_height = (app.chat.input_layout.rows.len() as u16).clamp(2, 6);
    let attachments_height = if app.chat.terminal_input.is_none()
        && !app.chat.loading_id
        && !app.chat.files.attached.is_empty()
    {
        1 + app.chat.files.attached.len().min(3) as u16
    } else {
        u16::from(app.chat.terminal_input.is_none() && !app.chat.loading_id && app.chat.files.attaching.contains_key(&app.chat.selected))
    };
    let selectors = composer_selectors(
        app,
        body.width.saturating_sub(4),
        body.height
            .saturating_sub(input_height + attachments_height + 10)
            .clamp(1, 4),
    );
    let composer_height =
        (input_height + 5 + attachments_height + selectors.height.saturating_sub(1))
            .min(body.height.saturating_sub(4));
    let composer = Rect::new(
        body.x + 1,
        body.bottom().saturating_sub(composer_height),
        body.width.saturating_sub(2),
        composer_height,
    );
    let permission_height = permissions::height(app, body.width.saturating_sub(2))
        .min(body.height / 2);
    let queue_height = queue::height(app).min(body.height.saturating_sub(composer_height + permission_height + 8));
    let error_height = u16::from(app.chat.error.is_some()) * 3;
    let tasks_height = tasks::height(app).min(body.height.saturating_sub(
        header_height + composer_height + permission_height + queue_height + error_height + 3));
    let content_bottom = composer
        .y
        .saturating_sub(tasks_height)
        .saturating_sub(permission_height)
        .saturating_sub(queue_height)
        .saturating_sub(error_height)
        .max(body.y + 3);
    let transcript = Rect::new(
        body.x + 2,
        body.y + header_height,
        body.width.saturating_sub(4),
        content_bottom.saturating_sub(body.y + header_height),
    );
    app.chat.transcript_area = Some(transcript);

    if app.chat.loading_conversation() {
        let label = format!("◌ {}", app.chat.status);
        let width = (label.width() as u16).min(transcript.width);
        f.render_widget(Paragraph::new(elide(&label, width as usize))
            .style(Style::default().fg(app.colors.fg_accent_blue)),
            Rect::new(transcript.x + transcript.width.saturating_sub(width) / 2,
                transcript.y + transcript.height / 2, width, u16::from(transcript.height > 0)));
    } else if let Some(session) = app.chat.session() {
        let _ = session;
        let expanded = (0..session.entries.len()).filter(|&i| app.chat.entry_expanded(i)).collect();
        let session = app
            .chat.connection
            .sessions
            .get(app.chat.selected.as_ref().unwrap())
            .unwrap();
        app.chat.connection.cache.update(
            session,
            transcript.width,
            ChatTheme {
                colors: &app.colors,
                highlighter: &app.highlighter,
                syntax_name: &app.syntax_theme,
            },
            &expanded,
            &name,
            app.chat.output_detail,
        );
        if let Some(id) = app.chat.reveal_terminal.take()
            && let Some(index) = app.chat.cache.rows.iter()
                .position(|r| r.terminal.as_ref().is_some_and(|t| t.id == id))
        {
            app.chat.scroll = index;
        }
        if let Some((index, before)) = app.chat.expansion_scroll.take() {
            app.chat.scroll = app.chat.scroll.saturating_add(app.chat.cache.entry_height(index).saturating_sub(before));
        }
        if let Some(index) = app.chat.reveal_entry.take() {
            app.chat.scroll = app.chat.cache.entries.iter().take(index).map(|entry| entry.2.len()).sum();
        }
        // Activity is a transient row at the live end, never transcript content.
        // Keep it outside the layout cache so animation cannot reflow messages.
        let activity = activity_row(app).map(Arc::new);
        let row_count = app.chat.cache.rows.len() + usize::from(activity.is_some());
        if row_count == 0 {
            welcome(f, app, transcript);
        }
        app.chat.max_scroll = row_count.saturating_sub(transcript.height as usize);
        if app.chat.follow {
            app.chat.scroll = app.chat.max_scroll;
        }
        app.chat.scroll = app.chat.scroll.min(app.chat.max_scroll);
        let hovered_message = transcript
            .contains((app.mouse_col, app.mouse_row).into())
            .then(|| app.chat.cache.rows.get(app.chat.scroll + (app.mouse_row - transcript.y) as usize))
            .flatten()
            .and_then(|r| r.action.clone())
            .filter(|a| matches!(a, ChatAction::Picker(Picker::Message(..))));
        let mut contrast = crate::ui_colors::TextContrastCache::new(&app.colors.ui);
        for offset in 0..transcript.height as usize {
            let index = app.chat.scroll + offset;
            let row = if index == app.chat.cache.rows.len() {
                if activity.is_some() && (app.chat.permission().is_none()
                        || app.chat.session().is_some_and(|s| s.cancelling)) {
                    app.chat.activity.show();
                }
                activity.clone()
            } else {
                app.chat.cache.rows.get(index).cloned()
            };
            let Some(row) = row else {
                break;
            };
            let rect = Rect::new(
                transcript.x,
                transcript.y + offset as u16,
                transcript.width,
                1,
            );
            if let Some(terminal) = &row.terminal {
                terminals::draw_row(f, app, rect, terminal);
                continue;
            }
            let hover = row.action.is_some()
                && (rect.contains((app.mouse_col, app.mouse_row).into())
                    || row.action == app.chat.focus
                    || (hovered_message.is_some() && row.action == hovered_message));
            let mut line = row.line.clone();
            let background = row.background.unwrap_or(app.colors.bg_sidebar);
            let mut style = Style::default().fg(app.colors.ui.sidebar_text_color(background)).bg(background);
            if hover {
                let c = &app.colors;
                style = if row.action == app.chat.focus {
                    c.ui.list_focus_style(c.ui.sidebar_foreground, background)
                } else {
                    let user = matches!(row.action, Some(ChatAction::Picker(Picker::Message(..))));
                    if user {
                        let hover_bg = c.ui.chat_request_hover_background.over(background).unwrap_or(background);
                        Style::default().bg(hover_bg).fg(c.ui.readable_foreground(
                            c.ui.list_hover_foreground.over(hover_bg).unwrap_or_else(|| c.ui.sidebar_text_color(hover_bg)),
                            hover_bg, 4.5))
                    } else {
                        c.ui.list_row_style(c.ui.sidebar_foreground, background, false, true, true)
                    }
                }.add_modifier(Modifier::BOLD);
                // Span colors take precedence over Paragraph styles. Replace both
                // sides of the pair, including avatars and inline code, on hover.
                line.style = line.style.patch(style);
                for span in &mut line.spans {
                    span.style = span.style.patch(style);
                }
            }
            // Request bubbles, avatars, and code spans can each own a different
            // surface. Guard the foreground that will actually reach each cell.
            if !row.separator {
                for span in &mut line.spans {
                    let foreground = span.style.fg.or(line.style.fg).or(style.fg)
                        .unwrap_or(app.colors.ui.fg_sidebar);
                    let background = span.style.bg.or(line.style.bg).or(style.bg)
                        .unwrap_or(app.colors.bg_sidebar);
                    span.style = span.style.fg(contrast.foreground(foreground, background));
                }
            }
            f.render_widget(Paragraph::new(line).style(style), rect);
            if hover && matches!(row.action, Some(ChatAction::Copy(_))) && rect.width > 16 {
                f.render_widget(
                    Paragraph::new(" Copy ").style(style),
                    Rect::new(rect.right() - 6, rect.y, 6, 1),
                );
            }
            if hover
                && rect.contains((app.mouse_col, app.mouse_row).into())
                && matches!(row.action, Some(ChatAction::Picker(Picker::Message(..))))
                && rect.width > 6
            {
                f.render_widget(
                    Paragraph::new(" ⋯ ").style(style),
                    Rect::new(rect.right() - 3, rect.y, 3, 1),
                );
            }
            if let Some(action) = row.action.clone() {
                if matches!(action, ChatAction::Picker(Picker::Message(..)))
                    && let Some(previous) = app.chat.hits.last_mut()
                        .filter(|h| h.action == action && h.area.bottom() == rect.y)
                {
                    previous.area.height += 1;
                } else {
                    app.chat.hits.push(ChatHit { area: rect, action: action.clone() });
                    if let ChatAction::ToggleEntry(index) = action
                        && let Some(ChatEntry::Tool(tool)) = app.chat.session().and_then(|s| s.entries.get(index))
                        && tools::location(tool).is_some()
                        && rect.width > 5
                    {
                        // The chevron shares the header's full-row toggle target.
                        f.render_widget(Paragraph::new(if app.chat.entry_expanded(index) { " ▾ " } else { " ▸ " })
                            .style(style), Rect::new(rect.right() - 3, rect.y, 3, 1));
                    }
                }
            }
        }
        if app.chat.max_scroll > 0 && transcript.height > 0 {
            draw_scrollbar(f, app, crate::app_acp::ChatScrollbar {
                area: Rect::new(body.right() - 2, transcript.y, 2, transcript.height),
                first: app.chat.scroll, total: row_count, visible: transcript.height as usize, terminal: None, picker: None,
            });
        }
        for (id, area) in app.chat.terminal_areas.clone() {
            if let Some(view) = app.chat.terminal_views.get(&id)
                && let Some(page) = &view.page
                && page.total > view.height
            {
                draw_scrollbar(f, app, crate::app_acp::ChatScrollbar {
                    area: Rect::new(area.right() - 1, area.y, 1, area.height),
                    first: page.first, total: page.total, visible: view.height, terminal: Some(id), picker: None,
                });
            }
        }
        if !app.chat.follow && app.chat.scroll < app.chat.max_scroll && transcript.height > 1 {
            let label = " ↓ Latest ";
            button(
                f,
                app,
                Rect::new(
                    transcript.right().saturating_sub(11).max(transcript.x),
                    transcript.bottom() - 1,
                    11.min(transcript.width),
                    1,
                ),
                label,
                ChatAction::Follow,
                true,
                true,
            );
        }
    } else {
        welcome(f, app, transcript);
    }

    if let Some(error) = app.chat.error.clone() {
        let error_area = Rect::new(
            body.x + 2,
            content_bottom,
            body.width.saturating_sub(4),
            error_height.min(composer.y.saturating_sub(tasks_height + content_bottom)),
        );
        f.render_widget(
            Paragraph::new(format!("! {}", clean(&error)))
                .style(Style::default().fg(app.colors.fg_error))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            error_area,
        );
        if error_area.height >= 3 {
            let checkpoint_error = app.chat.rollback.initialization_error() == Some(error.as_str());
            let action = if checkpoint_error {
                ChatAction::RetryCheckpointStorage
            } else if auth_available {
                ChatAction::Picker(Picker::Auth)
            } else {
                ChatAction::Reconnect
            };
            let label = if checkpoint_error {
                " Retry checkpoints "
            } else if auth_available {
                " Authenticate… "
            } else {
                " Reconnect "
            };
            button(
                f,
                app,
                Rect::new(error_area.x, error_area.bottom() - 1, error_area.width, 1),
                label,
                action,
                app.chat.agent.is_some() && (!checkpoint_error || !app.chat.rollback.busy()),
                false,
            );
        }
    }
    if permission_height > 0 {
        permissions::draw(
            f,
            app,
            Rect::new(
                body.x + 1,
                composer.y.saturating_sub(tasks_height + permission_height + queue_height),
                body.width.saturating_sub(2),
                permission_height,
            ),
        );
    }
    queue::draw(f, app, Rect::new(composer.x, composer.y.saturating_sub(tasks_height + queue_height), composer.width, queue_height));
    tasks::draw(f, app, Rect::new(composer.x, composer.y.saturating_sub(tasks_height), composer.width, tasks_height));
    draw_composer(f, app, composer, selectors);
    app.chat.files.popup = None;
    if app.chat.picker.is_some() {
        draw_picker(f, app, body, composer);
    } else if app.active_pane == ActivePane::Chat {
        draw_file_completion(f, app, body, composer);
    }
    draw_context_usage_tooltip(f, app);
    widgets::draw_yolo_tooltip(f, app);
    header::tooltip(f, app);
    tabs::draw_tooltip(f, app, body);
    agents::draw_tooltip(f, app, body);
}

fn draw_scrollbar(f: &mut Frame, app: &mut App, bar: crate::app_acp::ChatScrollbar) {
    let (start, size) = bar.thumb();
    let hovered = bar.area.contains((app.mouse_col, app.mouse_row).into()) || app.chat.scroll_drag.is_some();
    let surface = crate::ui_widgets::surface_background(f, bar.area, app.colors.bg_sidebar);
    let (track, thumb) = app.colors.ui.scrollbar_colors(surface, hovered, app.chat.scroll_drag.is_some());
    for y in 0..bar.area.height {
        let in_thumb = (start..start + size).contains(&(y as usize));
        let color = if in_thumb { thumb } else { track };
        f.render_widget(Paragraph::new(if in_thumb { "█" } else { " " }.repeat(bar.area.width as usize))
            .style(Style::default().fg(color).bg(color)),
            Rect::new(bar.area.x, bar.area.y + y, bar.area.width, 1));
    }
    app.chat.scrollbars.push(bar);
}


fn elide(text: &str, width: usize) -> String {
    let text = clean(text);
    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        if used + grapheme.width() > width - 1 {
            break;
        }
        used += grapheme.width();
        out.push_str(grapheme);
    }
    out.push('…');
    out
}

fn welcome(f: &mut Frame, app: &mut App, area: Rect) {
    let height = 6.min(area.height);
    let welcome = Rect::new(
        area.x + 1,
        area.y + area.height.saturating_sub(height) / 3,
        area.width.saturating_sub(2),
        height,
    );
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(
                "What are we working on?",
                Style::default()
                    .fg(app.colors.ui.fg_sidebar)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::default(),
            Line::styled(
                "Ask, plan, or build in your workspace.",
                Style::default().fg(app.colors.fg_dim),
            ),
        ])
        .wrap(ratatui::widgets::Wrap { trim: false }),
        welcome,
    );
    if welcome.height >= 5 && app.chat.session().is_none() {
        button(
            f,
            app,
            Rect::new(welcome.x, welcome.y + 4, welcome.width.min(19), 1),
            if app.chat.connected() {
                " + New conversation "
            } else {
                " Connect an agent "
            },
            if app.chat.connected() {
                ChatAction::New
            } else {
                ChatAction::Picker(Picker::Agents)
            },
            !app.chat.control_busy,
            true,
        );
    }
}

fn small_button(
    f: &mut Frame,
    app: &mut App,
    x: &mut u16,
    bounds: Rect,
    label: &str,
    action: ChatAction,
    enabled: bool,
) {
    let width = label.width().min(u16::MAX as usize) as u16;
    if x.saturating_add(width) > bounds.right() {
        return;
    }
    widgets::draw_button(
        f,
        app,
        Rect::new(*x, bounds.y, width, 1),
        label,
        action.clone(),
        enabled,
        widgets::toolbar_kind(&action),
    );
    *x += width + 1;
}

fn secondary_button(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    label: &str,
    action: ChatAction,
    enabled: bool,
) {
    widgets::draw_button(
        f, app, area, label,
        action,
        enabled,
        widgets::ButtonKind::Toolbar,
    );
}

fn button(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    label: &str,
    action: ChatAction,
    enabled: bool,
    primary: bool,
) {
    widgets::draw_button(f, app, area, label, action, enabled,
        if primary { widgets::ButtonKind::Primary } else { widgets::ButtonKind::Quiet });
}

fn draw_composer(f: &mut Frame, app: &mut App, mut area: Rect, mut selectors: ComposerSelectors) {
    if area.height < 3 {
        return;
    }
    if app.chat.terminal_input.is_none() && !app.chat.loading_id {
        let height = draw_attached_files(f, app, area, selectors.height);
        area.y += height;
        area.height = area.height.saturating_sub(height);
    }
    let focused = app.active_pane == ActivePane::Chat
        && app.chat.picker.is_none()
        && app.chat.focus.is_none();
    let terminal = app.chat.terminal_input.is_some();
    let terminal_title = format!(
        " Agent terminal input · {} ",
        app.chat.terminal_input.as_deref().unwrap_or("")
    );
    let dropping = app.chat_file_drop_hover();
    let title = if dropping {
        " Drop files to add context "
    } else if terminal {
        terminal_title.as_str()
    } else if app.chat.loading_id {
        " Session ID "
    } else {
        ""
    };
    let input_style = app.colors.ui.input_style(app.colors.bg_sidebar);
    let input_bg = input_style.bg.unwrap_or(app.colors.bg_sidebar);
    let border = (if focused || dropping { app.colors.ui.focus_border } else { app.colors.ui.input_colors.border })
        .border(input_bg).unwrap_or(input_bg);
    let caret_style = app.colors.ui.input_caret_style(input_bg);
    let selection_style = app.colors.ui.input_selection_style(input_bg);
    if selectors.height > area.height.saturating_sub(5) {
        selectors = composer_selectors(
            app,
            area.width.saturating_sub(2),
            area.height.saturating_sub(5),
        );
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(input_style)
        .title(title);
    f.render_widget(block, area);
    let input_bottom = area.bottom().saturating_sub(if selectors.height > 0 {
        selectors.height + 3
    } else {
        2
    });
    let inside = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        input_bottom.saturating_sub(area.y + 1),
    );
    app.chat.connection
        .input_layout
        .layout(&app.chat.connection.input, inside.width.max(1) as usize);
    app.chat.input_layout.height = inside.height as usize;
    app.chat.connection.input_layout.reveal(app.chat.connection.cursor);
    let cursor_row = app.chat.input_layout.cursor_row(app.chat.cursor);
    let selected = App::inline_selection_range(app.chat.cursor, app.chat.anchor);
    let mut hit_rows = Vec::new();
    for y in 0..inside.height as usize {
        let row = app.chat.input_layout.scroll + y;
        let mut spans = Vec::new();
        let mut hits = Vec::new();
        if let Some(range) = app.chat.input_layout.rows.get(row) {
            for (offset, grapheme) in app.chat.input[range.clone()].grapheme_indices(true) {
                let byte = range.start + offset;
                let caret =
                    focused && app.cursor_visible && row == cursor_row && byte == app.chat.cursor;
                let selection = selected.is_some_and(|(a, b)| byte >= a && byte < b);
                let style = if caret {
                    caret_style
                } else if selection {
                    selection_style
                } else {
                    input_style
                };
                let display = if grapheme == "\t" { " " } else { grapheme };
                hits.extend(std::iter::repeat_n(byte, display.width()));
                spans.push(Span::styled(clean(display), style));
            }
            if hits.len() < inside.width as usize {
                let caret = focused
                    && app.cursor_visible
                    && row == cursor_row
                    && app.chat.cursor == range.end;
                spans.push(Span::styled(
                    " ",
                    if caret {
                        caret_style
                    } else {
                        input_style
                    },
                ));
            }
            hits.resize(inside.width as usize, range.end);
        } else {
            hits.resize(inside.width as usize, app.chat.input.len());
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(inside.x, inside.y + y as u16, inside.width, 1),
        );
        hit_rows.push(hits);
    }
    if app.chat.input.is_empty() && inside.width > 2 {
        let offset = u16::from(focused);
        f.render_widget(
            Paragraph::new(if app.chat.loading_id {
                "Session ID…"
            } else if terminal {
                "Command input…"
            } else {
                "Ask anything…"
            })
            .style(input_style.fg(app.colors.ui.input_placeholder_color(input_bg))),
            Rect::new(
                inside.x + offset,
                inside.y,
                inside.width.saturating_sub(offset),
                1,
            ),
        );
    }
    app.register_text_input(
        TextInputId::Chat,
        TextInputHitMap::multiline(inside, hit_rows, app.chat.input.len()),
    );
    let controls = Rect::new(
        area.x + 1,
        area.bottom() - 2 - selectors.height,
        area.width.saturating_sub(2),
        1,
    );
    let has_draft = !app.chat.input.trim().is_empty() || !app.chat.files.attached.is_empty() || app.chat.files.attaching.contains_key(&app.chat.selected);
    let queuing = !terminal && !app.chat.loading_id && has_draft
        && (app.chat.busy() || app.chat.has_queued_prompt() || app.chat.checkpoint_pending());
    let send_width = (if queuing { 12 } else { 8 }).min(controls.width);
    let before_send = controls.width.saturating_sub(send_width + 1);
    let usage_width = if terminal || app.chat.loading_id {
        0
    } else {
        draw_context_usage(f, app, Rect::new(controls.x, controls.y, before_send, 1))
    };
    let actions = Rect::new(
        controls.x,
        controls.y,
        before_send.saturating_sub(usage_width + u16::from(usage_width > 0)),
        1,
    );
    let mut x = controls.x;
    if terminal || app.chat.loading_id {
        small_button(
            f,
            app,
            &mut x,
            actions,
            " ← Chat ",
            ChatAction::BackToChat,
            true,
        );
    } else {
        small_button(
            f,
            app,
            &mut x,
            actions,
            if actions.width as usize >= FILES_BUTTON.width()
                && (!app.chat.session().is_some_and(|s| !s.commands.is_empty() || app.chat.rollback.enabled)
                    || actions.width as usize > FILES_BUTTON.width() + COMMANDS_BUTTON.width())
            {
                FILES_BUTTON
            } else {
                " + "
            },
            ChatAction::Attach,
            true,
        );
        if app.chat.session().is_some_and(|s| !s.commands.is_empty() || app.chat.rollback.enabled) {
            small_button(
                f,
                app,
                &mut x,
                actions,
                COMMANDS_BUTTON,
                ChatAction::Picker(Picker::Commands),
                !app.chat.control_busy,
            );
        }
    }
    let busy = app.chat.busy() && !terminal;
    let label = if queuing {
        " Queue up ↑ "
    } else if busy {
        " ■ Stop "
    } else if app.chat.loading_id {
        " Open ↑ "
    } else {
        " Send ↑ "
    };
    widgets::draw_button(
        f,
        app,
        Rect::new(controls.right() - send_width, controls.y, send_width, 1),
        label,
        if busy && !queuing {
            ChatAction::Stop
        } else {
            ChatAction::Send
        },
        app.chat.connected()
            && (app.chat.selected.is_some() || app.chat.loading_id)
            && !app
                .chat
                .selected
                .as_ref()
                .is_some_and(|id| app.chat.deleting_sessions.contains(id))
            && (busy || !app.chat.control_busy)
            && (busy || terminal || has_draft),
        if busy && !queuing { widgets::ButtonKind::Secondary } else { widgets::ButtonKind::Primary },
    );
    if selectors.height > 0 {
        draw_selectors(
            f,
            app,
            Rect::new(
                area.x + 1,
                controls.y + 1,
                area.width.saturating_sub(2),
                selectors.height,
            ),
            &selectors,
        );
    }
}

/// Reserve space for file actions on the left and keep the meter beside Send.
fn draw_context_usage(f: &mut Frame, app: &mut App, bounds: Rect) -> u16 {
    let Some(session) = app.chat.session() else {
        return 0;
    };
    let c = &app.colors;
    let (icon, percent, color) = match session.usage.as_ref().filter(|u| u.size > 0) {
        Some(usage) => {
            // Widen before multiplying: token counts are agent-provided u64s.
            let percent = usage.used as u128 * 100 / usage.size as u128;
            let label = if usage.used > usage.size {
                ">100%".into()
            } else if percent == 0 && usage.used > 0 {
                "<1%".into()
            } else {
                format!("{percent}%")
            };
            let icon = if usage.used == 0 {
                "○"
            } else {
                match percent {
                    0..=37 => "◔",
                    38..=62 => "◑",
                    63..=87 => "◕",
                    _ => "●",
                }
            };
            let color = match percent {
                0..=79 => c.fg_accent_blue,
                80..=94 => c.fg_warning,
                _ => c.fg_error,
            };
            (icon, label, color)
        }
        None => ("○", "—".into(), c.fg_dim),
    };
    let full = format!(" {icon} {percent} context ");
    let compact = format!(" {icon} {percent} ");
    let actions_width = FILES_BUTTON.width()
        + if session.commands.is_empty() {
            0
        } else {
            COMMANDS_BUTTON.width() + 1
        };
    let minimum_actions = 3 + if session.commands.is_empty() { 0 } else { COMMANDS_BUTTON.width() + 1 };
    let percentage = format!(" {percent} ");
    let label = if full.width() + actions_width < bounds.width as usize {
        full
    } else if compact.width() + minimum_actions < bounds.width as usize {
        compact
    } else if percentage.width() + minimum_actions < bounds.width as usize {
        percentage
    } else {
        return 0;
    };
    let width = label.width() as u16;
    let area = Rect::new(bounds.right() - width, bounds.y, width, 1);
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let style = c.ui.toolbar_style(color, c.bg_input, hovered, false, false);
    let style = if hovered { style.add_modifier(Modifier::UNDERLINED) } else { style };
    f.render_widget(
        Paragraph::new(label).style(style),
        area,
    );
    app.chat.context_usage_area = Some(area);
    width
}

fn token_count(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

fn draw_context_usage_tooltip(f: &mut Frame, app: &App) {
    if app.menu_active
        || app.context_menu_active
        || app.chat.picker.is_some()
        || app.chat.files.popup.is_some()
    {
        return;
    }
    let Some(anchor) = app
        .chat
        .context_usage_area
        .filter(|area| area.contains((app.mouse_col, app.mouse_row).into()))
    else {
        return;
    };
    let c = &app.colors;
    let lines = match app.chat.session().and_then(|s| s.usage.as_ref()) {
        Some(usage) if usage.size > 0 => vec![
            (
                format!(
                    "{} / {} tokens",
                    token_count(usage.used),
                    token_count(usage.size)
                ),
                c.ui.hover_colors.foreground,
            ),
            if usage.used > usage.size {
                (
                    format!("{} tokens over limit", token_count(usage.used - usage.size)),
                    c.ui.error_foreground,
                )
            } else {
                (
                    format!("{} tokens remaining", token_count(usage.size - usage.used)),
                    c.ui.description_foreground,
                )
            },
            ("Last reported by agent".into(), c.ui.description_foreground),
        ],
        Some(usage) => vec![
            (
                format!("{} tokens used", token_count(usage.used)),
                c.ui.hover_colors.foreground,
            ),
            ("Context limit not reported".into(), c.ui.description_foreground),
        ],
        None => vec![
            ("The agent has not reported".into(), c.ui.description_foreground),
            ("context usage for this session.".into(), c.ui.description_foreground),
        ],
    };
    // Wrap counts as well as prose so even large values fit narrow terminals.
    let width = f.area().width.min(64).saturating_sub(4).max(1) as usize;
    let mut rows = Vec::new();
    let mut layers = Vec::new();
    for (line, color) in lines {
        let first = rows.len();
        wrapped(
            &mut rows,
            &line,
            width,
            Color::Reset,
            None,
        );
        layers.extend((first..rows.len()).map(|index| (index, color)));
    }
    let mut tooltip = crate::ui_widgets::Tooltip::hover(
        rows.into_iter().map(|row| row.line).collect(),
        c,
    )
    .title(
        "Context usage",
        Style::default()
            .add_modifier(Modifier::BOLD),
    );
    for (index, foreground) in layers {
        tooltip = tooltip.line_colors(index, crate::theme_workbench::ControlColors { foreground, ..Default::default() });
    }
    tooltip.render(f, anchor.x, anchor.y);
}

#[derive(Clone)]
struct ComposerSelector {
    name: Option<String>,
    value: String,
    picker: Picker,
}

impl ComposerSelector {
    fn width(&self) -> usize {
        self.name.as_ref().map_or(0, |s| s.width() + if self.picker == Picker::Models { 3 } else { 2 })
            + self.value.width() + 4
    }

    fn label(&self, width: usize) -> String {
        let Some(name) = &self.name else {
            return elide(&self.value, width);
        };
        // Preserve the current value when a long agent-provided label is clipped.
        let value_width = self
            .value
            .width()
            .min(width.saturating_sub(4).max(width / 2));
        let separator = if self.picker == Picker::Models { " · " } else { ": " };
        let name_width = width.saturating_sub(value_width + separator.width());
        if name_width < 2 {
            return elide(&self.value, width);
        }
        format!(
            "{}{}{}",
            elide(name, name_width),
            separator,
            elide(&self.value, value_width)
        )
    }
}

#[derive(Default)]
struct ComposerSelectors {
    items: Vec<(Rect, ComposerSelector)>,
    height: u16,
    overflow: bool,
}

fn composer_selectors(app: &App, width: u16, max_rows: u16) -> ComposerSelectors {
    let mut layout = ComposerSelectors::default();
    if width < 4 || max_rows == 0 || app.chat.terminal_input.is_some() || app.chat.loading_id {
        return layout;
    }
    let mode = app
        .chat
        .primary_config(p::SessionConfigOptionCategory::Mode);
    let model = app
        .chat
        .primary_config(p::SessionConfigOptionCategory::Model);
    let mut selectors = Vec::new();
    if mode.is_none()
        && let Some(name) = app.chat.mode_name()
    {
        selectors.push(ComposerSelector {
            name: None,
            value: clean(name),
            picker: Picker::Modes,
        });
    }
    if let Some(session) = app.chat.session() {
        // Honor the agent's priority order, including unknown or absent categories.
        for option in session
            .config_options
            .iter()
            .filter(|o| crate::app_acp::config_supported(o))
        {
            let thinking = app.chat.thinking_option().is_some_and(|o| o.id == option.id);
            if thinking && model.is_some() { continue; }
            if model.is_some_and(|o| o.id == option.id) {
                let (effort, reason) = app.chat.thinking_status();
                selectors.push(ComposerSelector {
                    name: None,
                    value: format!(
                        "{} * {}{}",
                        clean(crate::app_acp::config_value_name(option)),
                        clean(&effort),
                        if reason.is_some() { " ⊘" } else { "" }
                    ),
                    picker: Picker::Models,
                });
                continue;
            }
            let readonly_thinking = thinking && !crate::app_acp::config_changeable(option);
            let picker = if readonly_thinking {
                Picker::ThinkingInfo
            } else if mode.is_some_and(|o| o.id == option.id) {
                Picker::Modes
            } else if model.is_some_and(|o| o.id == option.id) {
                Picker::Models
            } else {
                Picker::Values(option.id.to_string())
            };
            let name = if matches!(picker, Picker::Modes | Picker::Models) {
                None
            } else {
                Some(if thinking {
                    "Thinking".into()
                } else {
                    clean(&option.name)
                })
            };
            selectors.push(ComposerSelector {
                name,
                value: clean(crate::app_acp::config_value_name(option)),
                picker,
            });
        }
        if model.is_none() && app.chat.thinking_option().is_none() {
            selectors.push(ComposerSelector {
                name: Some("Thinking".into()),
                value: "?".into(),
                picker: Picker::ThinkingInfo,
            });
        }
    }
    let thinking_picker = if model.is_some() { Picker::Models } else {
        app.chat.thinking_option()
            .filter(|o| crate::app_acp::config_changeable(o))
            .map_or(Picker::ThinkingInfo, |o| Picker::Values(o.id.to_string()))
    };
    let thinking = selectors.iter().find(|s| s.picker == thinking_picker).cloned();
    let (mut x, mut y) = (0u16, 0u16);
    for selector in selectors {
        let item_width = selector.width().min(width as usize) as u16;
        if x > 0 && item_width > width.saturating_sub(x) {
            x = 0;
            y += 1;
        }
        if y >= max_rows {
            layout.overflow = true;
            break;
        }
        layout
            .items
            .push((Rect::new(x, y, item_width, 1), selector));
        layout.height = y + 1;
        x = x.saturating_add(item_width).saturating_add(1);
    }
    if layout.overflow {
        // Only constrained views need overflow; every setting remains reachable.
        while let Some((rect, _)) = layout.items.last_mut() {
            if rect.y + 1 < layout.height || rect.right() <= width.saturating_sub(4) {
                break;
            }
            if rect.x + 7 <= width {
                rect.width = width - rect.x - 4;
                break;
            }
            layout.items.pop();
        }
        // Keep the effort visible even when other settings need the overflow menu.
        if width >= 8
            && !layout.items.iter().any(|(_, s)| s.picker == thinking_picker)
            && let Some(thinking) = thinking
        {
            let item_width = thinking.width().min(width.saturating_sub(4) as usize) as u16;
            let x = width - 4 - item_width;
            let y = layout.height.saturating_sub(1);
            layout.items.retain(|(rect, _)| {
                rect.y < y || rect.right().saturating_add(1) <= x
            });
            layout.items.push((Rect::new(x, y, item_width, 1), thinking));
        }
    }
    layout
}

fn draw_selectors(f: &mut Frame, app: &mut App, area: Rect, layout: &ComposerSelectors) {
    let idle = app.chat.initialized.is_some() && !app.chat.control_busy && !app.chat.busy();
    for (rect, control) in &layout.items {
        let rect = Rect::new(area.x + rect.x, area.y + rect.y, rect.width, 1);
        if control.picker == Picker::Models { app.chat.model_selector_area = Some(rect); }
        selector(
            f,
            app,
            rect,
            &control.label(rect.width.saturating_sub(4) as usize),
            control.picker.clone(),
            idle || control.picker == Picker::ThinkingInfo,
        );
    }
    if layout.overflow {
        button(
            f,
            app,
            Rect::new(area.right() - 3, area.bottom() - 1, 3, 1),
            " ⋯ ",
            ChatAction::Picker(Picker::Options),
            idle,
            false,
        );
    }
}

fn selector(f: &mut Frame, app: &mut App, area: Rect, label: &str, picker: Picker, enabled: bool) {
    if area.width < 3 {
        return;
    }
    let label = elide(label, area.width.saturating_sub(4) as usize);
    let padding = area.width.saturating_sub(label.width() as u16 + 3) as usize;
    let label = format!(" {label}{}{} ", " ".repeat(padding),
        if picker == Picker::ThinkingInfo { "⊘" } else { "▾" });
    widgets::draw_button(
        f,
        app,
        area,
        &label,
        ChatAction::Picker(picker),
        enabled,
        widgets::ButtonKind::Dropdown,
    );
}

fn draw_attached_files(f: &mut Frame, app: &mut App, area: Rect, selector_rows: u16) -> u16 {
    let count = app.chat.files.attached.len();
    let checking = app.chat.files.attaching.contains_key(&app.chat.selected);
    if count == 0 && checking && area.height > 0 {
        f.render_widget(Paragraph::new(" Checking attachments…").style(Style::default().fg(app.colors.fg_dim)), Rect::new(area.x, area.y, area.width, 1));
        return 1;
    }
    let rows = count
        .min(3)
        .min(area.height.saturating_sub(6 + selector_rows.max(1)) as usize);
    if rows == 0 || area.width < 8 {
        return 0;
    }
    let start = app.chat.files.scroll.min(count.saturating_sub(rows));
    let caption = if checking { format!(" Files · {count} · Checking… ") } else { format!(" Files · {count} ▾ ") };
    button(
        f,
        app,
        Rect::new(area.x, area.y, area.width.saturating_sub(8), 1),
        &caption,
        ChatAction::Picker(Picker::Attachments),
        true,
        false,
    );
    if count > rows {
        button(
            f,
            app,
            Rect::new(area.right() - 8, area.y, 4, 1),
            " ↑ ",
            ChatAction::AttachmentPage(true),
            start > 0,
            false,
        );
        button(
            f,
            app,
            Rect::new(area.right() - 4, area.y, 4, 1),
            " ↓ ",
            ChatAction::AttachmentPage(false),
            start + rows < count,
            false,
        );
    }
    for row in 0..rows {
        let index = start + row;
        let path = app.chat.files.attached[index].clone();
        let label = crate::app_chat_files::file_label(&path, app.chat_file_root());
        let y = area.y + 1 + row as u16;
        button(
            f,
            app,
            Rect::new(area.x + 1, y, area.width.saturating_sub(5), 1),
            &format!(" ▤ {}", clean(&label)),
            ChatAction::OpenFile(path, None),
            true,
            false,
        );
        button(
            f,
            app,
            Rect::new(area.right() - 4, y, 3, 1),
            " × ",
            ChatAction::RemoveAttachment(index),
            true,
            false,
        );
    }
    rows as u16 + 1
}

fn draw_file_completion(f: &mut Frame, app: &mut App, body: Rect, composer: Rect) {
    if app.chat.terminal_input.is_some() || app.chat.loading_id || app.chat.focus.is_some() {
        return;
    }
    let Some(completion) = &app.chat.files.completion else {
        return;
    };
    let height =
        (completion.paths.len().clamp(1, 6) as u16 + 2).min(composer.y.saturating_sub(body.y + 1));
    if height < 3 || composer.width < 8 {
        return;
    }
    let area = Rect::new(composer.x, composer.y - height, composer.width, height);
    let rows = height.saturating_sub(2) as usize;
    let start = completion.selected.saturating_sub(rows.saturating_sub(1));
    let selected = completion.selected;
    let searching = completion.searching;
    let labels: Vec<_> = completion
        .paths
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(index, path)| {
            (
                index,
                crate::app_chat_files::file_label(path, &completion.root),
                app.chat
                    .files
                    .attached
                    .iter()
                    .any(|p| App::same_open_path(p, path)),
            )
        })
        .collect();
    let surface = crate::ui_widgets::surface_background(f, area, app.colors.bg_sidebar);
    let popup_bg = app.colors.ui.suggest_colors.background.over(surface).unwrap_or(surface);
    let popup_style = app.colors.ui.suggest_row_style(popup_bg, false, false);
    f.render_widget(ratatui::widgets::Clear, area);
    // Remove hit targets hidden beneath the popup, including keyboard targets.
    app.chat.hits.retain(|hit| !hit.area.intersects(area));
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(app.colors.ui.suggest_colors.border.border(popup_bg).unwrap_or(popup_bg)))
            .title_style(Style::default().fg(popup_style.fg.unwrap()).bg(popup_bg))
            .style(
                Style::default()
                    .fg(popup_style.fg.unwrap())
                    .bg(popup_bg),
            )
            .title(if searching {
                " Add files · searching… "
            } else {
                " Add files "
            }),
        area,
    );
    app.chat.files.popup = Some(area);
    if labels.is_empty() {
        f.render_widget(
            Paragraph::new(if searching {
                "Searching workspace…"
            } else {
                "No matching files"
            })
            .style(Style::default().fg(app.colors.ui.description_color(popup_bg)).bg(popup_bg)),
            Rect::new(area.x + 2, area.y + 1, area.width - 4, 1),
        );
    }
    for (row, (index, label, attached)) in labels.into_iter().enumerate() {
        let rect = Rect::new(area.x + 1, area.y + 1 + row as u16, area.width - 2, 1);
        let active = selected == index || rect.contains((app.mouse_col, app.mouse_row).into());
        let style = app.colors.ui.suggest_row_style(popup_bg, active, false);
        let prefix = if attached { "✓" } else { "+" };
        f.render_widget(
            Paragraph::new(format!(" {prefix} {}", clean(&label))).style(style),
            rect,
        );
        app.chat.hits.push(ChatHit {
            area: rect,
            action: ChatAction::ChooseFile(index),
        });
    }
}

fn draw_picker(f: &mut Frame, app: &mut App, area: Rect, composer: Rect) {
    let picker = app.chat.picker.as_ref().unwrap().clone();
    let title = match &picker {
        Picker::Agents => "Agents",
        Picker::Sessions => "History",
        Picker::Modes => "Mode",
        Picker::Models => "Model & thinking",
        Picker::Thinking => "Thinking level",
        Picker::Options => "Chat options",
        Picker::Values(id) => app
            .chat
            .session()
            .and_then(|s| s.config_options.iter().find(|o| o.id.to_string() == *id))
            .map_or("Options", |o| o.name.as_str()),
        Picker::Auth => "Authentication",
        Picker::Commands => "Commands",
        Picker::Permissions => "Permission",
        Picker::Attachments => "Files",
        Picker::Changes => "Pending agent changes",
        Picker::ChangedFile(_) => "File changes",
        Picker::Change(_) => "Review change",
        Picker::ChangeChunk(_, _) => "Review chunk",
        Picker::Checkpoints => "Message checkpoints",
        Picker::Restore(_) => "Restore message checkpoint",
        Picker::Message(..) => "Message",
        Picker::ThinkingInfo => "Thinking effort",
        Picker::Display => "Chat display",
        Picker::Tasks => "TODOs",
    }
    .to_owned();
    let bottom = matches!(
        picker,
        Picker::Modes
            | Picker::Models
            | Picker::Thinking
            | Picker::Options
            | Picker::Values(_)
            | Picker::Commands
            | Picker::Permissions
            | Picker::Attachments
            | Picker::ThinkingInfo
    );
    let anchor_picker = if let Picker::Restore(id) = picker {
        app.chat.session()
            .and_then(|s| s.checkpoints.iter().find(|(_, cp)| **cp == id)
                .map(|(index, _)| Picker::Message(*index, s.history_epoch)))
            .unwrap_or_else(|| picker.clone())
    } else {
        picker.clone()
    };
    let anchor = app
        .chat
        .hits
        .iter()
        .filter(|hit| hit.action == ChatAction::Picker(anchor_picker.clone()))
        .min_by_key(|hit| !hit.area.contains((app.mouse_col, app.mouse_row).into()))
        .map(|hit| hit.area)
        .or_else(|| matches!(picker, Picker::Models | Picker::Thinking).then_some(app.chat.model_selector_area).flatten());
    // Reserve space for every description so hovering never moves the rows
    // underneath the pointer, including when the current choice has no detail.
    let details: Vec<_> = app
        .chat
        .choices
        .iter()
        .map(|c| clean(&c.detail))
        .collect();
    let detail = details.get(app.chat.picker_selected).map_or("", String::as_str);
    let detail_width = details.iter().flat_map(|s| s.lines())
        .map(UnicodeWidthStr::width).max().unwrap_or(0);
    let label_width = app
        .chat
        .choices
        .iter()
        .map(|choice| choice.label.width() + if picker == Picker::Sessions { 14 } else { 6 })
        .max()
        .unwrap_or(24);
    let permission_info = picker == Picker::Permissions;
    let width = (if picker == Picker::Tasks { composer.width } else if permission_info { 76 }
        else { label_width.max(detail_width.min(44) + 4).clamp(28, 52) as u16 })
        .min(area.width.saturating_sub(2));
    let anchor_y = anchor.map_or(composer.bottom().saturating_sub(3), |r| r.y);
    let max_height = if picker == Picker::Tasks && anchor.is_some() {
        anchor_y.saturating_sub(app.chat.transcript_area.map_or(area.y + 1, |r| r.y))
    } else if picker == Picker::Display && anchor.is_some() {
        area.bottom().saturating_sub(anchor.unwrap().bottom() + 1)
    } else if bottom && !permission_info {
        anchor_y.saturating_sub(area.y + 1)
    } else {
        area.height.saturating_sub(2)
    };
    let detail_rows = if detail_width == 0 || max_height < 6 {
        0
    } else if matches!(picker, Picker::Modes | Picker::Values(_) | Picker::ThinkingInfo | Picker::Thinking | Picker::Tasks | Picker::Display) {
        let columns = width.saturating_sub(4).max(1) as usize;
        let rows = details.iter().map(|text| {
            text.lines().map(|line| line.width().div_ceil(columns).max(1)).sum::<usize>()
        }).max().unwrap_or(0);
        rows.saturating_add(1).min(6).min(max_height.saturating_sub(3) as usize) as u16
    } else {
        3
    };
    let mut intro = Vec::new();
    if permission_info {
        let (title, detail) = permissions::request_summary(app);
        for line in format!("{title}\n{detail}").lines() {
            wrapped(&mut intro, line, width.saturating_sub(4) as usize, app.colors.fg_menu, None);
        }
    }
    intro.truncate(max_height.saturating_sub(detail_rows + 5) as usize);
    let intro_height = if intro.is_empty() { 0 } else { intro.len() as u16 + 1 };
    let height = (app.chat.choices.len().clamp(1, 8) as u16 + 2 + detail_rows + intro_height)
        .min(max_height)
        .max(3)
        .min(area.height);
    let x = anchor
        .map_or(area.x + 1, |r| r.x)
        .min(area.right().saturating_sub(width));
    let y = if picker == Picker::Tasks && anchor.is_some() {
        // The checklist grows upward, leaving its toggle and message input
        // fixed in place even in a tall chat pane.
        anchor_y.saturating_sub(height).max(area.y)
    } else if matches!(anchor_picker, Picker::Message(..) | Picker::Display | Picker::Changes) && anchor.is_some() {
        let below = anchor.unwrap().bottom();
        if below + height <= area.bottom() {
            below
        } else {
            anchor_y.saturating_sub(height).max(area.y)
        }
    } else if bottom && !permission_info {
        anchor_y.saturating_sub(height).max(area.y)
    } else {
        area.y + 1
    };
    let popup = Rect::new(x, y, width, height.min(area.bottom().saturating_sub(y)));
    if popup.width < 4 || popup.height < 3 {
        return;
    }
    let owner = crate::ui_widgets::surface_background(f, popup, app.colors.bg_sidebar);
    let colors = &app.colors.ui;
    let background = colors.menu_colors.background.over(owner).unwrap_or(owner);
    let foreground = colors.menu_colors.foreground.over(background).unwrap_or(app.colors.fg_menu);
    let selected_background = colors.menu_selection_colors.background.over(background).unwrap_or(background);
    let selected_foreground = colors.menu_selection_colors.foreground.over(selected_background).unwrap_or(foreground);
    let border = colors.menu_colors.border.or(colors.widget_border).border(background).unwrap_or(background);
    let separator = colors.menu_separator.over(background).unwrap_or(background);
    let description = colors.description_color(background);
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title_style(Style::default().fg(foreground))
            .title(format!(
                " {} ",
                elide(&title, width.saturating_sub(8) as usize)
            ))
            .style(
                Style::default()
                    .fg(foreground)
                    .bg(background),
            ),
        popup,
    );
    let inside = popup.inner(Margin::new(1, 1));
    let visible = inside.height.saturating_sub(detail_rows + intro_height).max(1) as usize;
    let scrollbar_width = if app.chat.choices.len() > visible { 2.min(inside.width.saturating_sub(1)) } else { 0 };
    if app.chat.picker_selected < app.chat.picker_scroll {
        app.chat.picker_scroll = app.chat.picker_selected;
    }
    if app.chat.picker_selected >= app.chat.picker_scroll + visible {
        app.chat.picker_scroll = app.chat.picker_selected + 1 - visible;
    }
    // Keep the pinned dropdown reachable so clicking its label also collapses
    // the open checklist. Other controls beneath a picker remain blocked.
    let tasks_toggle = matches!(picker, Picker::Tasks | Picker::Display).then(|| app.chat.hits.iter()
        .find(|hit| hit.action == ChatAction::Picker(picker.clone())).cloned()).flatten();
    app.chat.hits.clear();
    if let Some(hit) = tasks_toggle { app.chat.hits.push(hit); }
    for (i, row) in intro.into_iter().enumerate() {
        f.render_widget(Paragraph::new(row.line).style(Style::default().fg(app.colors.fg_menu)),
            Rect::new(inside.x + 1, inside.y + i as u16, inside.width.saturating_sub(2), 1));
    }
    if intro_height > 0 {
        f.render_widget(Paragraph::new("─".repeat(inside.width.saturating_sub(2) as usize))
            .style(Style::default().fg(separator)),
            Rect::new(inside.x + 1, inside.y + intro_height - 1, inside.width.saturating_sub(2), 1));
    }
    for offset in 0..visible {
        let index = app.chat.picker_scroll + offset;
        let Some(choice) = app.chat.choices.get(index).cloned() else {
            break;
        };
        let rect = Rect::new(inside.x, inside.y + intro_height + offset as u16, inside.width.saturating_sub(scrollbar_width), 1);
        let selected = index == app.chat.picker_selected;
        let (fg, bg) = if selected {
            (selected_foreground, selected_background)
        } else {
            (foreground, background)
        };
        let fg = app.colors.ui.readable_foreground(fg, bg, 4.5);
        let task_marker = if picker == Picker::Tasks {
            app.chat.session().and_then(|s| s.tasks.as_ref())
                .and_then(|tasks| tasks.entries.get(index))
                .map(|entry| tasks::marker(entry.status, &app.colors))
        } else { None };
        let (checked, label) = if task_marker.is_some() { (false, choice.label.as_str()) }
            else { choice.label.strip_prefix("✓ ")
                .map_or((false, choice.label.as_str()), |label| (true, label)) };
        let delete_id = if picker == Picker::Sessions {
            if let ChatAction::Switch(id) = &choice.action {
                Some(id.clone())
            } else {
                None
            }
        } else {
            None
        };
        let delete_width = if delete_id.is_some() {
            8.min(inside.width / 2)
        } else {
            0
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    task_marker.map_or(if checked { " ✓ ".into() } else { "   ".into() }, |(icon, _)| format!(" {icon} ")),
                    Style::default().fg(if let Some((_, color)) = task_marker.filter(|_| !selected) { color }
                    else if checked && !selected {
                        app.colors.ui.readable_foreground(app.colors.fg_success, bg, 4.5)
                    } else {
                        fg
                    }),
                ),
                Span::raw(elide(
                    label,
                    rect.width.saturating_sub(4 + delete_width) as usize,
                )),
            ]))
            .style(Style::default().fg(fg).bg(bg)),
            rect,
        );
        app.chat.hits.push(ChatHit {
            area: Rect::new(
                rect.x,
                rect.y,
                rect.width.saturating_sub(delete_width),
                rect.height,
            ),
            action: ChatAction::Pick(index),
        });
        if let Some(id) = delete_id {
            let enabled = app.chat.history_delete_unavailable(&id).is_none();
            let pending = app.chat.deleting_sessions.contains(&id);
            button(
                f,
                app,
                Rect::new(rect.right() - delete_width, rect.y, delete_width, 1),
                if pending {
                    " … "
                } else if delete_width >= 8 {
                    if app.chat.initialized.as_ref().is_some_and(|info| {
                        info.agent_capabilities
                            .session_capabilities
                            .delete
                            .is_some()
                    }) {
                        " Delete "
                    } else {
                        " Remove "
                    }
                } else {
                    " × "
                },
                ChatAction::DeleteSession(id),
                enabled,
                false,
            );
        }
    }
    if app.chat.choices.is_empty() {
        f.render_widget(
            Paragraph::new(if picker == Picker::Thinking && app.chat.thinking_loading() {
                "  Changing model…"
            } else { "  Nothing available" }).style(Style::default().fg(description)),
            inside,
        );
    }
    if detail_rows > 0 {
        let detail_y = inside.bottom().saturating_sub(detail_rows);
        f.render_widget(
            Paragraph::new("─".repeat(inside.width.saturating_sub(2) as usize))
                .style(Style::default().fg(separator)),
            Rect::new(inside.x + 1, detail_y, inside.width.saturating_sub(2), 1),
        );
        f.render_widget(
            Paragraph::new(detail)
                .style(Style::default().fg(description))
                .wrap(ratatui::widgets::Wrap { trim: true }),
            Rect::new(
                inside.x + 1,
                detail_y + 1,
                inside.width.saturating_sub(2),
                detail_rows.saturating_sub(1),
            ),
        );
    }
    if scrollbar_width > 0 {
        let bar = Rect::new(inside.right() - scrollbar_width, inside.y + intro_height,
            scrollbar_width, visible as u16);
        let hovered = bar.contains((app.mouse_col, app.mouse_row).into());
        let (track, thumb) = app.colors.ui.scrollbar_colors(background, hovered, app.chat.scroll_drag.is_some());
        crate::ui_widgets::VerticalScrollbar::new(bar.x, bar.y, bar.height,
            app.chat.choices.len(), app.chat.picker_scroll)
            .width(bar.width).colors(thumb, track, track).symbols('█', ' ').render(f);
        app.chat.scrollbars.push(crate::app_acp::ChatScrollbar {
            area: bar, first: app.chat.picker_scroll, total: app.chat.choices.len(), visible,
            terminal: None, picker: Some(picker.clone()),
        });
    }
    button(
        f,
        app,
        Rect::new(popup.right() - 4, popup.y, 3, 1),
        " × ",
        ChatAction::DismissPicker,
        true,
        false,
    );
}

impl ChatCache {
    fn update(
        &mut self,
        session: &Session,
        width: u16,
        theme: ChatTheme<'_>,
        expanded: &std::collections::HashSet<usize>,
        agent_name: &str,
        output_detail: OutputDetail,
    ) {
        let c = theme.colors;
        let inline_bg = c.ui.md_inline_background.over(c.bg_sidebar).unwrap_or(c.bg_sidebar);
        let request_inline_bg = c.ui.md_inline_background.over(c.ui.bg_chat_request).unwrap_or(c.ui.bg_chat_request);
        let quote_bg = c.ui.md_quote_background.over(c.bg_sidebar).unwrap_or(c.bg_sidebar);
        let request_quote_bg = c.ui.md_quote_background.over(c.ui.bg_chat_request).unwrap_or(c.ui.bg_chat_request);
        let avatar = c.ui.chat_avatar_style(c.bg_sidebar);
        let request_avatar = c.ui.chat_avatar_style(c.ui.bg_chat_request);
        let mut colors = vec![
            c.fg_text,
            c.ui.fg_sidebar,
            c.ui.fg_group_border,
            c.fg_dim,
            c.fg_accent_blue,
            c.fg_success,
            c.fg_error,
            c.fg_warning,
            c.bg_md_code,
            c.fg_md_code,
            c.ui.bg_md_inline,
            c.ui.fg_md_inline,
            c.ui.md_code_background.over(c.bg_sidebar).unwrap_or(c.bg_md_code),
            inline_bg,
            c.ui.md_inline_foreground.over(inline_bg).unwrap_or(c.ui.fg_md_inline),
            c.fg_md_heading,
            c.fg_md_link,
            c.fg_md_blockquote,
            quote_bg,
            request_quote_bg,
            c.ui.md_quote_border.over(quote_bg).unwrap_or(c.fg_md_blockquote),
            c.ui.md_quote_border.over(request_quote_bg).unwrap_or(c.fg_md_blockquote),
            c.bg_diff_added,
            c.bg_diff_removed,
            c.ui.fg_gutter_added,
            c.ui.fg_gutter_deleted,
            c.ui.bg_terminal,
            c.ui.fg_terminal,
            c.ui.bg_chat_request,
            avatar.bg.unwrap(),
            avatar.fg.unwrap(),
            request_avatar.bg.unwrap(),
            request_avatar.fg.unwrap(),
            c.ui.sidebar_text_color(c.bg_sidebar),
            c.ui.sidebar_text_color(c.ui.bg_chat_request),
            request_inline_bg,
            c.ui.md_inline_foreground.over(request_inline_bg).unwrap_or(c.ui.fg_md_inline),
            c.ui.md_code_background.over(c.ui.bg_chat_request).unwrap_or(c.bg_md_code),
            c.ui.fg_chat_edited_file,
            c.ui.fg_chat_added,
            c.ui.fg_chat_removed,
            c.bg_sidebar,
        ];
        colors.extend(c.ui.contrast_palette);
        let context = ChatContext {
            preserve_theme_colors: c.ui.preserve_theme_colors,
            output_detail,
            session: session.id.clone(),
            history_epoch: session.history_epoch,
            width,
            colors,
            agent_name: agent_name.to_string(),
            syntax_name: theme.syntax_name.to_string(),
            syntax_generation: theme.highlighter.registry_generation(),
        };
        if self.context.as_ref() != Some(&context) {
            *self = Self {
                context: Some(context),
                ..Default::default()
            };
        }
        if self.revision == Some(session.revision)
            && self
                .entries
                .iter()
                .enumerate()
                .all(|(i, e)| e.1 == expanded.contains(&i))
        {
            return;
        }
        self.entries.truncate(session.entries.len());
        let (syntaxes, themes) = theme.highlighter.shared_sets();
        let syntax = Some((&*syntaxes, themes.themes.get(theme.syntax_name)));
        for index in 0..session.entries.len() {
            let revision = session.entry_revision(index);
            let open = expanded.contains(&index);
            if self
                .entries
                .get(index)
                .is_some_and(|e| e.0 == revision && e.1 == open)
            {
                continue;
            }
            let rows = entry_rows_detail(session, index, width as usize, c, open, agent_name, syntax, output_detail)
                .into_iter()
                .map(Arc::new)
                .collect();
            if index < self.entries.len() {
                self.entries[index] = (revision, open, rows);
            } else {
                self.entries.push((revision, open, rows));
            }
        }
        self.rows.clear();
        if session.history_epoch > 0 {
            self.rows.push(Arc::new(ChatRow {
                line: Line::styled(
                    "Earlier activity trimmed · history stays with the agent",
                    Style::default().fg(c.fg_dim),
                ),
                action: None,
                background: None,
                terminal: None,
                separator: false,
            }));
        }
        self.rows
            .extend(self.entries.iter().flat_map(|e| e.2.iter().cloned()));
        self.revision = Some(session.revision);
    }
}

#[cfg(test)]
fn entry_rows(session: &Session, index: usize, width: usize, c: &ThemeColors, open: bool, agent_name: &str, syntax: ChatSyntax<'_>) -> Vec<ChatRow> {
    entry_rows_detail(session, index, width, c, open, agent_name, syntax, OutputDetail::Compact)
}

fn entry_rows_detail(
    session: &Session,
    index: usize,
    width: usize,
    c: &ThemeColors,
    open: bool,
    agent_name: &str,
    syntax: ChatSyntax<'_>,
    output_detail: OutputDetail,
) -> Vec<ChatRow> {
    let mut rows = Vec::new();
    let entry = &session.entries[index];
    match entry {
        ChatEntry::User(text) | ChatEntry::Agent(text) => {
            let user = matches!(entry, ChatEntry::User(_));
            let name = if user { "You" } else { agent_name };
            let surface = if user { c.ui.bg_chat_request } else { c.bg_sidebar };
            plain(&mut rows, "─".repeat(width), c.ui.fg_group_border, None);
            rows.last_mut().unwrap().separator = true;
            row(
                &mut rows,
                Line::from(vec![
                    Span::styled(
                        if user { "●  " } else { "◇  " },
                        c.ui.chat_avatar_style(surface),
                    ),
                    Span::styled(
                        clean(name),
                        Style::default()
                            .fg(c.ui.sidebar_text_color(surface))
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Some(ChatAction::Copy(index)),
                None,
            );
            if user || !context::render(&mut rows, text, width, c) {
                markdown_on_surface(
                    &mut rows, text,
                    if user { width.saturating_sub(3) } else { width },
                    c, usize::MAX, syntax, surface,
                );
            }
            if user {
                for row in &mut rows {
                    row.background = Some(c.ui.bg_chat_request);
                    if !row.separator { row.action = Some(ChatAction::Picker(Picker::Message(index, session.history_epoch))); }
                }
            }
            plain(&mut rows, "─".repeat(width), c.ui.fg_group_border, None);
            rows.last_mut().unwrap().separator = true;
        }
        ChatEntry::Thought(text) => {
            plain(
                &mut rows,
                format!("{} Thinking", if open { "▾" } else { "▸" }),
                c.fg_dim,
                Some(ChatAction::ToggleEntry(index)),
            );
            if open {
                // Expanded thinking is a live transcript, not a preview. A
                // head-only limit hides every new chunk after it fills up.
                markdown(&mut rows, text, width, c, usize::MAX, syntax);
            }
        }
        ChatEntry::Plan(plan) => {
            let done = plan
                .entries
                .iter()
                .filter(|e| e.status == p::PlanEntryStatus::Completed)
                .count();
            plain(
                &mut rows,
                format!("≡  TODOs   {done}/{}", plan.entries.len()),
                c.fg_accent_blue,
                None,
            );
            for entry in &plan.entries {
                let (icon, color) = match entry.status {
                    p::PlanEntryStatus::Completed => ("✓", c.fg_success),
                    p::PlanEntryStatus::InProgress => ("●", c.fg_accent_blue),
                    _ => ("○", c.fg_dim),
                };
                wrapped(
                    &mut rows,
                    &format!(" {icon} {}", entry.content),
                    width,
                    color,
                    None,
                );
            }
        }
        ChatEntry::Tool(tool) => {
            if let Some(tasks) = vtcode::acp::tool_tasks(tool) {
                let (icon, color) = match tool.status {
                    p::ToolCallStatus::Completed => ("✓", c.fg_success),
                    p::ToolCallStatus::Failed => ("!", c.fg_error),
                    _ => ("◌", c.fg_accent_blue),
                };
                plain(&mut rows, format!("{icon}  TODOs {}/{} {}", tasks.completed(), tasks.entries.len(),
                    if open { "▾" } else { "▸" }), color, Some(ChatAction::ToggleEntry(index)));
                if open || output_detail != OutputDetail::Minimal {
                    tasks::rows(&mut rows, &tasks, width, c, if open { usize::MAX } else { 3 });
                }
            } else if let Some(terminal) = tool_terminals::parse(tool, session.entry_revision(index)) {
                terminals::reported_rows(&mut rows, terminal, width, index, open, output_detail != OutputDetail::Minimal);
                if open { tools::terminal_input_details(&mut rows, tool, width, c); }
            } else {
                let (icon, color) = match tool.status {
                    p::ToolCallStatus::Completed => ("✓", c.fg_success),
                    p::ToolCallStatus::Failed => ("!", c.fg_error),
                    p::ToolCallStatus::InProgress => ("◌", c.fg_accent_blue),
                    _ => ("○", c.fg_dim),
                };
                plain(
                    &mut rows,
                    format!("{icon}  {}  {}", elide(&tool.title, width.saturating_sub(7)), if tools::location(tool).is_some() { "" } else if open { "▾" } else { "▸" }),
                    color,
                    Some(ChatAction::ToggleEntry(index)),
                );
                if open || output_detail != OutputDetail::Minimal {
                    tools::body(&mut rows, tool, width, c, open, syntax);
                }
            }
        }
        ChatEntry::Edit(edit) => {
            let pending = session.rollback.pending_file(&edit.path);
            if let Some(change) = pending {
                plain(&mut rows, "●  File updated · Pending review".into(), c.fg_warning,
                    Some(ChatAction::Picker(Picker::ChangedFile(change.path.clone()))));
            } else {
                plain(&mut rows, "✓  File updated".into(), c.fg_success, None);
            }
            let diff =
                p::Diff::new(edit.path.clone(), edit.after.clone()).old_text(edit.before.clone());
            diff_rows(&mut rows, &diff, width, c, 8);
        }
        ChatEntry::Terminal(id) => {
            if let Some(terminal) = session.terminals.get(id) {
                terminals::rows(&mut rows, id, &terminal.command, width, index, open, output_detail != OutputDetail::Minimal);
            }
        }
        ChatEntry::Notice(message) => wrapped(&mut rows, message, width, c.fg_dim, None),
        ChatEntry::Error(message) => {
            wrapped(&mut rows, &format!("! {message}"), width, c.fg_error, None)
        }
    }
    plain(&mut rows, String::new(), c.fg_dim, None);
    for row in &mut rows {
        // Rules are decoration; keep their subtle theme border color rather
        // than raising it to the prose contrast threshold.
        if row.separator { continue; }
        let background = row.line.style.bg.or(row.background).unwrap_or(c.bg_sidebar);
        let foreground = row.line.style.fg.unwrap_or(c.ui.fg_sidebar);
        row.line.style = row.line.style.fg(c.ui.readable_foreground(
            foreground, background, 4.5,
        ));
        for span in &mut row.line.spans {
            span.style = span.style.fg(c.ui.readable_foreground(
                span.style.fg.unwrap_or(foreground),
                span.style.bg.unwrap_or(background),
                4.5,
            ));
        }
    }
    rows
}

fn diff_rows(rows: &mut Vec<ChatRow>, diff: &p::Diff, width: usize, c: &ThemeColors, limit: usize) {
    let before = diff.old_text.as_deref().unwrap_or_default();
    // Avoid doing expensive full-file diff work on a streamed transcript frame.
    // The complete snapshots remain available in the native Review action.
    if before.len() + diff.new_text.len() > 256 * 1024
        || before.lines().take(2001).count() > 2000
        || diff.new_text.lines().take(2001).count() > 2000
    {
        plain(
            rows,
            format!(
                "  ↗ {} · Review full change",
                diff.path.file_name().unwrap_or_default().to_string_lossy()
            ),
            c.ui.fg_chat_edited_file,
            Some(ChatAction::Review(Arc::new(diff.clone()))),
        );
        return;
    }
    let changes =
        crate::app::compute_file_diff(diff.old_text.as_deref().unwrap_or_default(), &diff.new_text);
    let added = changes.iter().filter(|(kind, _)| *kind == '+').count();
    let removed = changes.iter().filter(|(kind, _)| *kind == '-').count();
    let review = ChatAction::Review(Arc::new(diff.clone()));
    row(
        rows,
        Line::from(vec![
            Span::styled(
                format!(
                    "  ↗ {}  ",
                    clean(&diff.path.file_name().unwrap_or_default().to_string_lossy())
                ),
                Style::default().fg(c.ui.fg_chat_edited_file),
            ),
            Span::styled(format!("+{added}"), Style::default().fg(c.ui.fg_chat_added)),
            Span::raw(" "),
            Span::styled(
                format!("−{removed}"),
                Style::default().fg(c.ui.fg_chat_removed),
            ),
            Span::styled("  Review", Style::default().fg(c.fg_md_link)),
        ]),
        Some(review.clone()),
        None,
    );
    let mut count = 0;
    for (kind, text) in changes.iter().filter(|(kind, _)| *kind != ' ').take(limit) {
        let (fg, bg) = if *kind == '+' {
            (c.ui.fg_gutter_added, c.bg_diff_added)
        } else {
            (c.ui.fg_gutter_deleted, c.bg_diff_removed)
        };
        wrapped(rows, &format!("{kind} {text}"), width, fg, Some(bg));
        count += 1;
    }
    if added + removed > count {
        plain(rows, "  … open full diff".into(), c.fg_dim, Some(review));
    }
}

fn markdown(
    rows: &mut Vec<ChatRow>,
    text: &str,
    width: usize,
    colors: &ThemeColors,
    limit: usize,
    syntax: ChatSyntax<'_>,
) {
    append_markdown(rows, crate::ui_markdown::chat_lines(text, width.max(1), colors, syntax), limit, colors.fg_dim);
}
fn markdown_on_surface(
    rows: &mut Vec<ChatRow>,
    text: &str,
    width: usize,
    colors: &ThemeColors,
    limit: usize,
    syntax: ChatSyntax<'_>,
    surface: Color,
) {
    let lines = crate::ui_markdown::chat_lines_on_surface(text, width.max(1), colors, syntax, surface);
    append_markdown(rows, lines, limit, colors.fg_dim);
}
fn append_markdown(rows: &mut Vec<ChatRow>, lines: Vec<Line<'static>>, limit: usize, dim: Color) {
    let more = lines.len() > limit;
    for line in lines.into_iter().take(limit) {
        row(rows, line, None, None);
    }
    if more {
        plain(rows, "…".into(), dim, None);
    }
}
fn wrapped(
    rows: &mut Vec<ChatRow>,
    text: &str,
    width: usize,
    color: Color,
    background: Option<Color>,
) {
    let mut line = String::new();
    let mut used = 0;
    for g in clean(text).graphemes(true).take(32 * 1024) {
        if g == "\n" || used + g.width() > width.max(1) {
            row(
                rows,
                Line::styled(std::mem::take(&mut line), Style::default().fg(color)),
                None,
                background,
            );
            used = 0;
        }
        if g != "\n" {
            line.push_str(g);
            used += g.width();
        }
    }
    row(
        rows,
        Line::styled(line, Style::default().fg(color)),
        None,
        background,
    );
}
fn plain(rows: &mut Vec<ChatRow>, text: String, color: Color, action: Option<ChatAction>) {
    row(
        rows,
        Line::styled(clean(&text), Style::default().fg(color)),
        action,
        None,
    );
}
fn row(
    rows: &mut Vec<ChatRow>,
    line: Line<'static>,
    action: Option<ChatAction>,
    background: Option<Color>,
) {
    rows.push(ChatRow {
        line,
        action,
        background,
        terminal: None,
        separator: false,
    });
}
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}
pub(crate) fn clean_terminal(text: &str) -> String {
    struct Output {
        text: String,
        carriage_return: bool,
    }
    impl vte::Perform for Output {
        fn print(&mut self, c: char) {
            if std::mem::take(&mut self.carriage_return) {
                let start = self.text.rfind('\n').map_or(0, |i| i + 1);
                self.text.truncate(start);
            }
            self.text.push(c);
        }
        fn execute(&mut self, byte: u8) {
            if byte == b'\n' {
                self.carriage_return = false;
                self.text.push('\n');
            } else if byte == b'\r' {
                self.carriage_return = true;
            } else if byte == b'\t' {
                for _ in 0..4 {
                    self.print(' ');
                }
            } else if byte == 8 && !self.text.ends_with('\n') {
                self.text.pop();
            }
        }
    }
    let mut parser = vte::Parser::new();
    let mut output = Output {
        text: String::new(),
        carriage_return: false,
    };
    for byte in text.bytes() {
        parser.advance(&mut output, byte);
    }
    output.text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{Terminal, backend::TestBackend};

    pub(super) fn seeded() -> App {
        let mut app = App::test_new();
        app.chat.visible = true;
        app.active_pane = ActivePane::Chat;
        app.chat.initialized = Some(serde_json::from_value(serde_json::json!({"protocolVersion":1,"agentInfo":{"name":"local-agent","title":"Workspace Agent","version":"1.0"},"agentCapabilities":{"loadSession":true}})).unwrap());
        app.chat.agent = Some(vtcode::acp::AgentConfig::new("Local agent", "local-agent"));
        let id = p::SessionId::new("ui-session");
        let mut session = Session::new(id.clone());
        session.modes = Some(serde_json::from_value(serde_json::json!({"currentModeId":"discover","availableModes":[{"id":"discover","name":"Explore"},{"id":"craft","name":"Build","description":"Make changes"}]})).unwrap());
        session.config_options = serde_json::from_value(serde_json::json!([
            {"id":"engine-b47","name":"Engine","category":"model","type":"select","currentValue":"precision-47","options":[
                {"group":"local","name":"Workspace models","options":[
                    {"value":"precision-47","name":"Workspace Large","description":"Detailed coding and planning"},
                    {"value":"swift-29","name":"Workspace Fast","description":"Quick everyday changes"}]}]},
            {"id":"budget-83","name":"Context","category":"model_config","type":"select","currentValue":"ctx-large","options":[
                {"value":"ctx-small","name":"128K"},{"value":"ctx-large","name":"256K","description":"More room for files and conversation history"}]},
            {"id":"effort-29","name":"Thinking","category":"thought_level","type":"select","currentValue":"balanced","options":[
                {"value":"balanced","name":"Balanced"},{"value":"deep","name":"Deep"}]}
        ])).unwrap();
        session.begin_prompt("Add a friendly greeting and verify it.".into());
        for update in [
            serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I'll update the greeting, then run the checks.\n\nThe change keeps the public API intact."}}),
            serde_json::json!({"sessionUpdate":"plan","entries":[{"content":"Inspect the greeting","priority":"high","status":"completed"},{"content":"Update the message","priority":"high","status":"completed"},{"content":"Run the checks","priority":"medium","status":"in_progress"}]}),
            serde_json::json!({"sessionUpdate":"tool_call","toolCallId":"edit","title":"Update src/greeting.rs","kind":"edit","status":"completed","content":[{"type":"diff","path":"src/greeting.rs","oldText":"Hello\n","newText":"Hello, world!\n"}]}),
        ] {
            session.apply(serde_json::from_value(update).unwrap());
        }
        session.terminal(vtcode::acp::TerminalSnapshot { session_id: id.clone(), id: "checks".into(), command: "cargo test greeting".into(), output: "running 2 tests\ntest greeting::welcomes_the_reader ... ok\ntest greeting::handles_empty_names ... ok\n\ntest result: ok. 2 passed; 0 failed\n".into(), truncated: false, exit_status: Some(p::TerminalExitStatus::new().exit_code(0)), released: true, cwd: Default::default(), history: None, history_version: 0, killed: false, error: None });
        session.apply(serde_json::from_value(serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The greeting is ready. **Both tests passed.**\n\nOpen the diff above to review the change."}})).unwrap());
        session.finish(Some(p::StopReason::EndTurn));
        app.chat.sessions.insert(id.clone(), session);
        app.chat.selected = Some(id.clone());
        app.chat.open_tabs.push(id.clone());
        app.chat.session_order.push(id);
        app.chat.status = "Ready".into();
        app
    }

    fn text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn transcript_text(app: &App, terminal: &Terminal<TestBackend>) -> String {
        let area = app.chat.transcript_area.unwrap();
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn acp_chat_context_usage_updates_on_hover_and_tracks_the_selected_session() {
        for theme in ["dark", "light", "high-contrast", "monokai"] {
            let mut app = seeded();
            app.set_theme_by_key(theme);
            app.new_file();
            app.focus_chat();
            let mut terminal = Terminal::new(TestBackend::new(140, 44)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(text(&terminal).contains("— context"));
            let id = app.chat.selected.clone().unwrap();
            app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
                id.clone(),
                p::SessionUpdate::UsageUpdate(p::UsageUpdate::new(53_000, 200_000)),
            )));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(text(&terminal).contains("◔ 26% context"));
            assert!(!text(&terminal).contains("53,000 / 200,000 tokens"));
            let badge = app.chat.context_usage_area.unwrap();
            assert!(app.chat.hits.iter().all(|h| !h.area.intersects(badge)));
            let background = terminal
                .backend()
                .buffer()
                .cell((badge.x, badge.y))
                .unwrap()
                .bg;
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: badge.x,
                    row: badge.y,
                    modifiers: KeyModifiers::empty(),
                },
            );
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(text(&terminal).contains("Context usage"));
            assert!(text(&terminal).contains("53,000 / 200,000 tokens"));
            assert!(text(&terminal).contains("147,000 tokens remaining"));
            let hovered = terminal.backend().buffer().cell((badge.x, badge.y)).unwrap();
            assert_eq!(hovered.bg, app.colors.ui.toolbar_hover.over(background).unwrap_or(background));
            assert!(hovered.modifier.contains(Modifier::UNDERLINED));
            // An update refreshes the open tooltip without another mouse event.
            app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
                id.clone(),
                p::SessionUpdate::UsageUpdate(p::UsageUpdate::new(164_000, 200_000)),
            )));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(text(&terminal).contains("82% context"));
            assert!(text(&terminal).contains("164,000 / 200,000 tokens"));
            assert!(text(&terminal).contains("36,000 tokens remaining"));
            let cell = terminal
                .backend()
                .buffer()
                .cell((badge.x, badge.y))
                .unwrap();
            assert_eq!(
                cell.fg,
                app.colors.ui.readable_foreground(app.colors.fg_warning, cell.bg, 4.5)
            );
            app.chat_action(ChatAction::Picker(Picker::Models));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(!text(&terminal).contains("Context usage"));
            app.chat_action(ChatAction::DismissPicker);
            // Hover exit into the editor must request a redraw and remove the popup.
            let editor = app.editor_area.unwrap();
            app.mouse_hover_changed = false;
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: editor.x + 5,
                    row: editor.y + 3,
                    modifiers: KeyModifiers::empty(),
                },
            );
            assert!(app.mouse_hover_changed);
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(!text(&terminal).contains("Context usage"));
            let other = p::SessionId::new("other-session");
            app.chat
                .sessions
                .insert(other.clone(), Session::new(other.clone()));
            app.chat_action(ChatAction::Switch(other));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(text(&terminal).contains("— context"));
            assert!(!text(&terminal).contains("82% context"));
            app.chat_action(ChatAction::Switch(id));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(text(&terminal).contains("82% context"));
            app.chat.visible = false;
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            assert!(app.chat.context_usage_area.is_none());
        }
    }

    #[test]
    fn acp_chat_context_usage_handles_missing_limits_and_narrow_composers() {
        for width in [36, 40, 64] {
            let mut app = seeded();
            app.chat.session_mut().unwrap().commands = serde_json::from_value(serde_json::json!([
                {"name":"inspect", "description":"Inspect workspace"}
            ]))
            .unwrap();
            let mut terminal = Terminal::new(TestBackend::new(width, 28)).unwrap();
            for (usage, label, details) in [
                (None, "—", "context usage for this session."),
                (Some((0, 8192)), "0%", "0 / 8,192 tokens"),
                (Some((1, 8192)), "<1%", "1 / 8,192 tokens"),
                (Some((4096, 8192)), "50%", "4,096 / 8,192 tokens"),
                (Some((8192, 8192)), "100%", "0 tokens remaining"),
                (Some((10000, 8192)), ">100%", "1,808 tokens over limit"),
                (Some((73, 0)), "—", "Context limit not reported"),
            ] {
                app.mouse_col = 0;
                app.mouse_row = 0;
                app.chat.session_mut().unwrap().usage =
                    usage.map(|(used, size)| p::UsageUpdate::new(used, size));
                terminal
                    .draw(|f| draw(f, &mut app, Rect::new(0, 0, width, 28)))
                    .unwrap();
                let badge = app.chat.context_usage_area.unwrap();
                let content: String = (badge.x..badge.right())
                    .map(|x| {
                        terminal
                            .backend()
                            .buffer()
                            .cell((x, badge.y))
                            .unwrap()
                            .symbol()
                    })
                    .collect();
                assert!(content.contains(label), "missing {label}: {content}");
                for action in [ChatAction::Attach, ChatAction::Picker(Picker::Commands)] {
                    let hit = app
                        .chat
                        .hits
                        .iter()
                        .find(|h| h.action == action)
                        .unwrap_or_else(|| {
                            panic!("missing {action:?} at width {width} with usage {label}")
                        });
                    assert!(!hit.area.intersects(badge));
                }
                assert!(text(&terminal).contains("Send ↑"));
                app.mouse_col = badge.x;
                app.mouse_row = badge.y;
                terminal
                    .draw(|f| draw(f, &mut app, Rect::new(0, 0, width, 28)))
                    .unwrap();
                assert!(
                    text(&terminal).contains(details),
                    "missing {details} at width {width}"
                );
                if usage == Some((73, 0)) {
                    assert!(text(&terminal).contains("73 tokens used"));
                }
            }
        }
        // No overflow or truncated counts for large protocol values.
        let mut app = seeded();
        app.chat.session_mut().unwrap().usage = Some(p::UsageUpdate::new(u64::MAX, u64::MAX));
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 80, 28)))
            .unwrap();
        let badge = app.chat.context_usage_area.unwrap();
        app.mouse_col = badge.x;
        app.mouse_row = badge.y;
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 80, 28)))
            .unwrap();
        assert!(text(&terminal).contains("100% context"));
        assert_eq!(
            text(&terminal)
                .matches("18,446,744,073,709,551,615")
                .count(),
            2
        );
    }

    #[test]
    fn acp_chat_inline_activity_tracks_the_live_turn_without_changing_history() {
        for theme in ["dark", "light", "high-contrast"] {
            let mut app = seeded();
            app.set_theme_by_key(theme);
            let session = app.chat.session_mut().unwrap();
            *session = Session::new(session.id.clone());
            session.begin_prompt("Inspect this workspace".into());
            let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 40, 24)))
                .unwrap();
            let transcript = transcript_text(&app, &terminal);
            assert!(
                transcript.find("Inspect this workspace").unwrap()
                    < transcript.find("Working…").unwrap()
            );
            assert!(transcript.contains("⠋  Working…"));
            let area = app.chat.transcript_area.unwrap();
            let spinner = terminal
                .backend()
                .buffer()
                .cell((area.x, area.y + app.chat.cache.rows.len() as u16))
                .unwrap();
            assert_eq!(spinner.fg, app.colors.ui.readable_foreground(app.colors.fg_accent_blue, spinner.bg, 4.5));
            assert_eq!(app.chat.session().unwrap().entries.len(), 1);
            let cached = app.chat.cache.rows[0].clone();
            app.chat.activity.tick.set(Some(Instant::now() - ACTIVITY_INTERVAL));
            assert!(advance_activity_frame(&mut app));
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 40, 24)))
                .unwrap();
            assert!(transcript_text(&app, &terminal).contains("⠙  Working…"));
            assert!(Arc::ptr_eq(&cached, &app.chat.cache.rows[0]));

            app.chat
                .session_mut()
                .unwrap()
                .apply(p::SessionUpdate::AgentMessageChunk(p::ContentChunk::new(
                    p::ContentBlock::Text(p::TextContent::new("Partial response")),
                )));
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 40, 24)))
                .unwrap();
            let transcript = transcript_text(&app, &terminal);
            assert!(
                transcript.find("Partial response").unwrap() < transcript.find("Working…").unwrap()
            );

            app.apply_acp_event(vtcode::acp::Event::Permission {
                ticket: 0,
                id: serde_json::from_value(serde_json::json!(17)).unwrap(),
                request: serde_json::from_value(serde_json::json!({"sessionId":"ui-session","toolCall":{"toolCallId":"check","title":"Run checks"},"options":[{"optionId":"once","name":"Allow once","kind":"allow_once"}]})).unwrap(),
            });
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 40, 24)))
                .unwrap();
            assert!(transcript_text(&app, &terminal).contains("Waiting for permission"));
            assert_eq!(next_activity_frame(&app), None);
            app.chat.session_mut().unwrap().cancelling = true;
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 40, 24)))
                .unwrap();
            assert!(transcript_text(&app, &terminal).contains("Stopping…"));
            app.chat
                .session_mut()
                .unwrap()
                .finish(Some(p::StopReason::Cancelled));
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 40, 24)))
                .unwrap();
            assert!(activity_row(&app).is_none());
            assert!(!transcript_text(&app, &terminal).contains("Stopping…"));
            assert_eq!(next_activity_frame(&app), None);
            assert_eq!(app.chat.session().unwrap().entries.len(), 3);
        }
    }

    #[test]
    fn acp_chat_inline_activity_respects_scrolling_and_session_visibility() {
        let mut app = seeded();
        app.chat
            .session_mut()
            .unwrap()
            .begin_prompt("Next task".into());
        let mut terminal = Terminal::new(TestBackend::new(50, 20)).unwrap();
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 50, 20)))
            .unwrap();
        assert!(transcript_text(&app, &terminal).contains("Working…"));
        assert!(next_activity_frame(&app).is_some());
        app.chat.follow = false;
        app.chat.scroll = 0;
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 50, 20)))
            .unwrap();
        assert_eq!(app.chat.scroll, 0);
        assert!(!transcript_text(&app, &terminal).contains("Working…"));
        // Running tabs remain visible even when the live transcript is offscreen.
        assert!(next_activity_frame(&app).is_some());
        app.chat_action(ChatAction::Follow);
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 50, 20)))
            .unwrap();
        assert!(next_activity_frame(&app).is_some());
        app.chat.visible = false;
        app.chat.activity.begin_frame();
        assert_eq!(next_activity_frame(&app), None);
        app.chat.visible = true;
        app.terminal_has_focus = false;
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 50, 20)))
            .unwrap();
        assert!(next_activity_frame(&app).is_some());
        app.terminal_has_focus = true;
        let id = p::SessionId::new("other-session");
        app.chat
            .sessions
            .insert(id.clone(), Session::new(id.clone()));
        app.chat_action(ChatAction::Switch(id));
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 50, 20)))
            .unwrap();
        assert!(!transcript_text(&app, &terminal).contains("Working…"));
        assert!(next_activity_frame(&app).is_some());
    }

    #[test]
    fn acp_chat_composer_selectors_popovers_and_activity() {
        for theme in ["dark", "light", "high-contrast"] {
            let mut app = seeded();
            app.chat.agent_order = vec![app.chat.active_agent];
            app.set_theme_by_key(theme);
            let mut terminal = Terminal::new(TestBackend::new(66, 30)).unwrap();
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30)))
                .unwrap();
            assert!(!text(&terminal).contains("Ready"));
            let mode = app
                .chat
                .hits
                .iter()
                .find(|h| h.action == ChatAction::Picker(Picker::Modes))
                .unwrap()
                .area;
            let model = app
                .chat
                .hits
                .iter()
                .find(|h| h.action == ChatAction::Picker(Picker::Models))
                .unwrap()
                .area;
            assert!(mode.y > app.chat.transcript_area.unwrap().bottom());
            assert_eq!(model.y, mode.y);
            assert!(!model.intersects(mode));
            assert!(text(&terminal).contains("Context: 256K"));
            assert!(text(&terminal).contains("Workspace Large * Balanced"));
            for id in ["budget-83"] {
                let hit = app
                    .chat
                    .hits
                    .iter()
                    .find(|h| h.action == ChatAction::Picker(Picker::Values(id.into())))
                    .unwrap();
                assert!(hit.area.y >= mode.y);
            }
            assert!(
                !app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::Picker(Picker::Options))
            );
            let before = terminal
                .backend()
                .buffer()
                .cell((model.x, model.y))
                .unwrap()
                .style();
            app.mouse_col = model.x;
            app.mouse_row = model.y;
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30)))
                .unwrap();
            assert_ne!(
                before,
                terminal
                    .backend()
                    .buffer()
                    .cell((model.x, model.y))
                    .unwrap()
                    .style(),
                "hover may use a high contrast outline"
            );
            app.handle_chat_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: model.x,
                row: model.y,
                modifiers: KeyModifiers::empty(),
            });
            assert_eq!(app.chat.picker, Some(Picker::Models));
            assert_eq!(app.chat.choices.len(), 3);
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30)))
                .unwrap();
            let rendered = text(&terminal);
            assert!(rendered.contains("Workspace Fast"));
            for clutter in [
                "Enter send",
                "Enter select",
                "Esc close",
                "↑↓",
                "select to remove",
            ] {
                assert!(!rendered.contains(clutter), "unexpected {clutter}");
            }
            let rows: Vec<_> = app
                .chat
                .hits
                .iter()
                .filter(|h| matches!(h.action, ChatAction::Pick(_)))
                .collect();
            assert!(rows.iter().all(|h| h.area.bottom() < model.y));
            assert_eq!(rows[1].area.y - rows[0].area.y, 1);
            app.handle_chat_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
            assert!(app.chat.picker.is_none());
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30)))
                .unwrap();
            let thinking = app
                .chat
                .hits
                .iter()
                .find(|h| h.action == ChatAction::Picker(Picker::Models))
                .unwrap()
                .area;
            app.handle_chat_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: thinking.x,
                row: thinking.y,
                modifiers: KeyModifiers::empty(),
            });
            // Selecting the already-current model opens thinking without an RPC.
            app.chat_action(ChatAction::Pick(0));
            assert_eq!(app.chat.picker, Some(Picker::Thinking));
            assert_eq!(app.chat.choices.len(), 3);
            assert_eq!(app.chat.choices[0].label, "✓ Balanced");
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30)))
                .unwrap();
            assert!(
                app.chat
                    .hits
                    .iter()
                    .filter(|h| matches!(h.action, ChatAction::Pick(_)))
                    .all(|h| h.area.bottom() < thinking.y)
            );
            app.chat_action(ChatAction::DismissPicker);
            app.chat.session_mut().unwrap().busy = true;
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30)))
                .unwrap();
            assert!(text(&terminal).contains("Working…"));
            assert!(
                !app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::Picker(Picker::Models))
            );
            app.chat.session_mut().unwrap().busy = false;
            app.chat.initialized = None;
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30)))
                .unwrap();
            let agent = app.chat.hits.iter()
                .find(|hit| matches!(hit.action, ChatAction::SwitchAgent(_))).unwrap().area;
            app.mouse_col = agent.x;
            app.mouse_row = agent.y;
            terminal.draw(|f| draw(f, &mut app, Rect::new(0, 0, 66, 30))).unwrap();
            assert!(text(&terminal).contains("Disconnected"));
        }
    }

    #[test]
    fn acp_chat_config_categories_prefer_current_protocol_and_preserve_unknown_options() {
        let mut app = seeded();
        let mode: p::SessionConfigOption = serde_json::from_value(serde_json::json!({
            "id":"behavior-207","name":"Behavior","category":"mode","type":"select","currentValue":"review-1",
            "options":[{"value":"review-1","name":"Ask"},{"value":"draft-2","name":"Plan"}]
        })).unwrap();
        app.chat.session_mut().unwrap().config_options.push(mode);
        assert_eq!(app.chat.mode_name(), Some("Ask"));
        app.chat_action(ChatAction::Picker(Picker::Modes));
        assert!(
            matches!(&app.chat.choices[1].action, ChatAction::Config(id, _) if id.to_string() == "behavior-207")
        );
        assert_eq!(app.chat.choices[1].label, "Plan");
        // Missing categories on conventional IDs remain usable; explicit,
        // unknown categories are preserved as ordinary agent configuration.
        let session = app.chat.session_mut().unwrap();
        session.config_options[0].category = None;
        session.config_options[0].id = "model".into();
        assert_eq!(app.chat.model_name(), Some("Workspace Large"));
        app.chat.session_mut().unwrap().config_options[0].category =
            Some(p::SessionConfigOptionCategory::Other("_custom".into()));
        assert!(app.chat.model_name().is_none());
        app.chat_action(ChatAction::Picker(Picker::Options));
        assert!(app.chat.choices.iter().any(|c| c.label == "Engine"));
        app.chat.session_mut().unwrap().config_options.push(
            p::SessionConfigOption::boolean("custom-74", "References", true)
                .category(p::SessionConfigOptionCategory::Other("_references".into())),
        );
        for (width, height) in [(70, 28), (38, 18), (24, 12), (12, 8)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            app.chat.picker = None;
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, width, height)))
                .unwrap();
            let controls: Vec<_> = app
                .chat
                .hits
                .iter()
                .filter(|h| matches!(h.action, ChatAction::Picker(_)))
                .collect();
            for (index, hit) in controls.iter().enumerate() {
                assert!(
                    controls
                        .iter()
                        .skip(index + 1)
                        .all(|other| !hit.area.intersects(other.area))
                );
            }
            if width >= 24 {
                let overflow = controls
                    .iter()
                    .any(|h| h.action == ChatAction::Picker(Picker::Options));
                for picker in [
                    Picker::Modes,
                    Picker::Values("model".into()),
                    Picker::Values("budget-83".into()),
                    Picker::Values("effort-29".into()),
                    Picker::Values("custom-74".into()),
                ] {
                    assert!(
                        overflow
                            || controls
                                .iter()
                                .any(|h| h.action == ChatAction::Picker(picker.clone())),
                        "missing {picker:?} at {width}x{height}"
                    );
                }
            }
            for picker in [
                Picker::Modes,
                Picker::Models,
                Picker::Options,
                Picker::Values("effort-29".into()),
                Picker::Values("budget-83".into()),
                Picker::Values("custom-74".into()),
                Picker::Agents,
            ] {
                app.chat.picker = None;
                app.chat_action(ChatAction::Picker(picker));
                terminal
                    .draw(|f| draw(f, &mut app, Rect::new(0, 0, width, height)))
                    .unwrap();
                assert!(
                    app.chat
                        .hits
                        .iter()
                        .all(|h| h.area.right() <= width && h.area.bottom() <= height)
                );
            }
        }
        app.chat_action(ChatAction::Picker(Picker::Values("custom-74".into())));
        assert!(matches!(
            app.chat.choices[0].action,
            ChatAction::Config(_, p::SessionConfigOptionValue::Boolean { value: true })
        ));
        let session_id = app.chat.selected.clone().unwrap();
        let options: Vec<_> = app
            .chat
            .session()
            .unwrap()
            .config_options
            .iter()
            .filter(|o| o.id.to_string() != "custom-74")
            .cloned()
            .collect();
        app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
            session_id,
            p::SessionUpdate::ConfigOptionUpdate(p::ConfigOptionUpdate::new(options)),
        )));
        assert!(
            app.chat.picker.is_none(),
            "removed settings must dismiss their stale picker"
        );
    }

    #[test]
    fn acp_chat_renders_native_activity_and_diff_review() {
        let mut app = seeded();
        app.chat.width = 66;
        let mut terminal = Terminal::new(TestBackend::new(140, 44)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let content = text(&terminal);
        for needle in [
            "Workspace Agent",
            "TODOs",
            "Review",
            "Both tests passed",
            "Workspace Large",
        ] {
            assert!(content.contains(needle), "missing {needle}");
        }
        let review = app
            .chat
            .hits
            .iter()
            .find(|h| matches!(h.action, ChatAction::Review(_)))
            .unwrap()
            .clone();
        let before = terminal
            .backend()
            .buffer()
            .cell((review.area.x, review.area.y))
            .unwrap()
            .bg;
        app.mouse_col = review.area.x;
        app.mouse_row = review.area.y;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_ne!(
            terminal
                .backend()
                .buffer()
                .cell((review.area.x, review.area.y))
                .unwrap()
                .bg,
            before
        );
        app.handle_chat_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: review.area.x,
            row: review.area.y,
            modifiers: KeyModifiers::empty(),
        });
        assert!(matches!(
            app.tabs[app.active_tab].kind,
            crate::app::TabKind::Diff(_)
        ));
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(app.compare_base_content.as_deref(), Some("Hello\n"));
        assert!(app.diff_mode != crate::app::DiffMode::Off);
        assert_eq!(app.editor().unwrap().rope.to_string(), "Hello, world!\n");
    }

    #[test]
    fn acp_chat_narrow_views_and_permission_choices_stay_inside_the_dock() {
        for (width, height) in [(140, 44), (80, 24), (45, 14), (24, 10), (8, 4), (1, 1)] {
            let mut app = seeded();
            let request: p::RequestPermissionRequest = serde_json::from_value(serde_json::json!({"sessionId":"ui-session","toolCall":{"toolCallId":"review","title":"Run workspace checks"},"options":(0..8).map(|i| serde_json::json!({"optionId":format!("choice-{i}"),"name":format!("Choice {i}"),"kind":"allow_once"})).collect::<Vec<_>>()})).unwrap();
            app.apply_acp_event(vtcode::acp::Event::Permission {
                ticket: 0,
                id: serde_json::from_value(serde_json::json!("permission-id")).unwrap(),
                request,
            });
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            terminal
                .draw(|frame| draw(frame, &mut app, Rect::new(0, 0, width, height)))
                .unwrap();
            for hit in &app.chat.hits {
                assert!(
                    hit.area.right() <= width && hit.area.bottom() <= height,
                    "{width}x{height}: {:?}",
                    hit.area
                );
            }
            app.chat_action(ChatAction::Picker(Picker::Permissions));
            assert_eq!(app.chat.choices.len(), 10);
            terminal
                .draw(|frame| draw(frame, &mut app, Rect::new(0, 0, width, height)))
                .unwrap();
        }
    }

    #[test]
    fn acp_chat_input_preserves_editor_and_permission_enter_never_auto_approves() {
        let mut app = seeded();
        app.new_file();
        app.focus_chat();
        app.paste_into_focused_text_input("hello 世界\nsecond line");
        assert_eq!(app.chat.input, "hello 世界\nsecond line");
        assert!(app.editor().unwrap().rope.to_string().is_empty());
        app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(app.chat.input.ends_with('\n'));
        app.handle_chat_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert!(!app.chat.input.ends_with('\n'));
        let request = serde_json::from_value(serde_json::json!({"sessionId":"ui-session","toolCall":{"toolCallId":"p","title":"Approval"},"options":[{"optionId":"once","name":"Allow once","kind":"allow_once"}]})).unwrap();
        app.apply_acp_event(vtcode::acp::Event::Permission {
            ticket: 0,
            id: serde_json::from_value(serde_json::json!(77)).unwrap(),
            request,
        });
        app.chat.session_mut().unwrap().busy = true;
        app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(app.chat.permissions.len(), 1);
        assert!(app.chat.focus.is_none());
        assert!(app.chat.error.is_none());
    }

    #[test]
    fn acp_chat_scroll_anchor_survives_streaming_and_modes_are_dynamic() {
        let mut app = seeded();
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut app, Rect::new(0, 0, 60, 20)))
            .unwrap();
        app.chat.follow = false;
        app.chat.scroll = 0;
        let id = app.chat.selected.clone().unwrap();
        app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(id, serde_json::from_value(serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"More streamed output\n"}})).unwrap())));
        terminal
            .draw(|frame| draw(frame, &mut app, Rect::new(0, 0, 60, 20)))
            .unwrap();
        assert_eq!(app.chat.scroll, 0);
        app.chat_action(ChatAction::Picker(Picker::Modes));
        assert_eq!(app.chat.choices.len(), 2);
        assert!(
            matches!(&app.chat.choices[1].action, ChatAction::Mode(id) if id.to_string() == "craft")
        );
        app.handle_chat_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(app.chat.picker_selected, 1);
    }

    #[test]
    fn acp_chat_keeps_drafts_separate_from_terminal_input() {
        let mut app = seeded();
        app.chat.input = "Describe the architecture".into();
        app.chat.cursor = 5;
        let terminal = app
            .chat
            .session_mut()
            .unwrap()
            .terminals
            .get_mut("checks")
            .unwrap();
        terminal.released = false;
        terminal.exit_status = None;
        app.chat_action(ChatAction::Terminal("checks".into()));
        assert!(app.chat.input.is_empty());
        app.paste_into_focused_text_input("yes");
        app.chat_action(ChatAction::BackToChat);
        assert_eq!(app.chat.input, "Describe the architecture");
        assert_eq!(app.chat.cursor, 5);
        assert!(app.chat.terminal_input.is_none());
    }

    #[test]
    fn acp_chat_terminal_output_handles_control_sequences_and_carriage_returns() {
        assert_eq!(clean_terminal("first\r\nsecond\r\n"), "first\nsecond\n");
        assert_eq!(
            clean_terminal("old progress\r\x1b[32mfinished\x1b[0m\n"),
            "finished\n"
        );
        assert_eq!(clean_terminal("abc\x08d"), "abd");
    }

    #[test]
    fn acp_chat_workbench_shortcuts_and_menus_keep_their_priority() {
        let mut app = seeded();
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(app.quick_open_active);
        app.quick_open_active = false;
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::F(1), KeyModifiers::empty()),
        );
        assert!(app.palette.active);
        app.palette.active = false;
        app.chat.area = Some(Rect::new(20, 1, 50, 30));
        app.menu_active = true;
        assert!(!app.handle_chat_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 30,
            row: 5,
            modifiers: KeyModifiers::empty()
        }));
        app.menu_active = false;
        app.execute_palette_command("toggle_zen");
        assert!(app.zen_mode && app.active_pane == ActivePane::Editor);
        app.focus_chat();
        assert!(!app.zen_mode && app.active_pane == ActivePane::Chat);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.active_pane = ActivePane::Editor;
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(app.chat.area.is_none());
        assert!(app.workbench_body_area.unwrap().width > 0);

        // The top-level entry point remains usable with menus open and after resize.
        for width in [40, 80, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let column = (0..width)
                .find(|&x| terminal.backend().buffer().cell((x, 0)).unwrap().symbol() == "◇")
                .expect("Agent Chat button in the title bar");
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: 0,
                modifiers: KeyModifiers::empty(),
            };
            app.menu_active = true;
            app.menu_open = true;
            crate::mouse::handle_mouse(&mut app, click);
            assert!(!app.menu_active && !app.menu_open);
            assert!(app.chat.visible && app.active_pane == ActivePane::Chat);
            crate::mouse::handle_mouse(&mut app, click);
            assert!(!app.chat.visible && app.active_pane == ActivePane::Editor);
        }
    }

    #[test]
    fn acp_chat_streaming_reuses_unchanged_layout_in_long_conversations() {
        let mut app = seeded();
        for _ in 0..400 {
            app.chat.session_mut().unwrap().push(ChatEntry::Agent("## Earlier message\n\nA paragraph with **formatting**, `code`, and words to wrap across several lines.\n\n- First item\n- Second item".into()));
        }
        let mut terminal = Terminal::new(TestBackend::new(70, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut app, Rect::new(0, 0, 70, 30)))
            .unwrap();
        let original = app.chat.cache.entries[100].2[0].clone();
        for _ in 0..100 {
            app.chat
                .session_mut()
                .unwrap()
                .apply(p::SessionUpdate::AgentMessageChunk(p::ContentChunk::new(
                    p::ContentBlock::Text(p::TextContent::new("stream ")),
                )));
            terminal
                .draw(|frame| draw(frame, &mut app, Rect::new(0, 0, 70, 30)))
                .unwrap();
            assert!(Arc::ptr_eq(&original, &app.chat.cache.entries[100].2[0]));
        }
        // Focus remains attached to its action after new rows move the hits.
        app.chat.focus = Some(ChatAction::Picker(Picker::Agents));
        terminal
            .draw(|frame| draw(frame, &mut app, Rect::new(0, 0, 70, 30)))
            .unwrap();
        app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(app.chat.picker, Some(Picker::Agents));
    }

    #[test]
    #[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
    fn acp_chat_sdk_round_trip_inline_activity_before_agent_output() {
        fn until(app: &mut App, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(15);
            while !done(app) {
                assert!(
                    Instant::now() < deadline,
                    "agent timed out: {:?}",
                    app.chat.error
                );
                app.poll_acp();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let root = tempfile::tempdir().unwrap();
        let mut app = App::test_new();
        app.explorer.root = root.path().to_path_buf();
        let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
        let mut config = vtcode::acp::AgentConfig::new("Official SDK scenario agent", python);
        config.args = vec![
            "-u".into(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/scenario_agent.py")
                .to_string_lossy()
                .into_owned(),
        ];
        app.settings_effective.acp_agents = Some(vec![config]);
        app.focus_chat();
        app.chat_action(ChatAction::Connect(0));
        until(&mut app, |app| app.chat.selected.is_some());
        let mut terminal = Terminal::new(TestBackend::new(58, 28)).unwrap();
        app.paste_into_focused_text_input("silent");
        app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        // First frame is drawn before polling a single response from the agent.
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 58, 28)))
            .unwrap();
        assert!(transcript_text(&app, &terminal).contains("Working…"));
        until(&mut app, |app| {
            app.chat
                .diagnostics
                .iter()
                .any(|s| s.contains("Silent prompt received"))
        });
        assert_eq!(app.chat.session().unwrap().entries.len(), 1);
        app.chat.activity.tick.set(Some(Instant::now() - ACTIVITY_INTERVAL));
        assert!(advance_activity_frame(&mut app));
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 58, 28)))
            .unwrap();
        assert!(transcript_text(&app, &terminal).contains("⠙  Working…"));
        let stop = app
            .chat
            .hits
            .iter()
            .find(|h| h.action == ChatAction::Stop)
            .unwrap()
            .area;
        app.handle_chat_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: stop.x,
            row: stop.y,
            modifiers: KeyModifiers::empty(),
        });
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 58, 28)))
            .unwrap();
        assert!(transcript_text(&app, &terminal).contains("Stopping…"));
        until(&mut app, |app| !app.chat.busy());
        assert_eq!(
            app.chat.session().unwrap().stop_reason,
            Some(p::StopReason::Cancelled)
        );
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 58, 28)))
            .unwrap();
        assert!(activity_row(&app).is_none());

        // Completion, a recoverable prompt error, and agent exit all clear activity.
        for prompt in ["Hello from the workbench", "error", "crash"] {
            app.paste_into_focused_text_input(prompt);
            app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 58, 28)))
                .unwrap();
            assert!(transcript_text(&app, &terminal).contains("Working…"));
            until(&mut app, |app| !app.chat.busy());
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 58, 28)))
                .unwrap();
            assert!(!transcript_text(&app, &terminal).contains("Working…"));
            assert_eq!(next_activity_frame(&app), None);
            if prompt == "Hello from the workbench" {
                assert!(app.chat.error.is_none());
                assert!(
                    app.chat
                        .session()
                        .unwrap()
                        .entries
                        .iter()
                        .any(|e| matches!(e, ChatEntry::Agent(_)))
                );
            } else {
                assert!(app.chat.error.is_some());
            }
        }
        app.chat_action(ChatAction::Disconnect);
    }

    #[test]
    #[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
    fn acp_chat_sdk_round_trip_composer_model_and_mode() {
        use std::time::{Duration, Instant};
        fn settle(app: &mut App) {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                app.poll_acp();
                assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
                if app.chat.selected.is_some() && !app.chat.control_busy {
                    break;
                }
                assert!(Instant::now() < deadline, "agent control request timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn click(app: &mut App, terminal: &mut Terminal<TestBackend>, action: ChatAction) {
            terminal
                .draw(|f| draw(f, app, Rect::new(0, 0, 64, 30)))
                .unwrap();
            let area = app
                .chat
                .hits
                .iter()
                .find(|h| h.action == action)
                .expect("visible chat control")
                .area;
            app.handle_chat_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::empty(),
            });
        }
        let root = tempfile::tempdir().unwrap();
        let mut app = App::test_new();
        app.explorer.root = root.path().to_path_buf();
        let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
        let mut config = vtcode::acp::AgentConfig::new("SDK selectors", python);
        config.args = vec![
            "-u".into(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/scenario_agent.py")
                .to_string_lossy()
                .into_owned(),
        ];
        config
            .env
            .insert("ACP_TEST_CHAT_SELECTORS".into(), "1".into());
        config
            .env
            .insert("ACP_TEST_MODEL_A".into(), "arbitrary-first-762".into());
        config
            .env
            .insert("ACP_TEST_MODEL_B".into(), "arbitrary-second-908".into());
        app.settings_effective.acp_agents = Some(vec![config]);
        app.focus_chat();
        app.chat_action(ChatAction::Connect(0));
        settle(&mut app);
        assert_eq!(app.chat.model_name(), Some("Workspace Fast"));
        assert_eq!(app.chat.mode_name(), Some("Ask"));
        let mut terminal = Terminal::new(TestBackend::new(64, 30)).unwrap();
        click(&mut app, &mut terminal, ChatAction::Picker(Picker::Models));
        assert!(
            matches!(&app.chat.choices[1].action, ChatAction::Model(_, p::SessionConfigOptionValue::ValueId { value }) if value.to_string() == "arbitrary-second-908")
        );
        click(&mut app, &mut terminal, ChatAction::Pick(1));
        assert_eq!(app.chat.picker, Some(Picker::Thinking));
        assert!(app.chat.thinking_loading());
        assert!(app.chat.choices.is_empty());
        settle(&mut app);
        assert_eq!(app.chat.model_name(), Some("Workspace Large"));
        assert_eq!(app.chat.picker, Some(Picker::Thinking));
        assert_eq!(app.chat.choices[2].label, "Maximum");
        click(&mut app, &mut terminal, ChatAction::Pick(2));
        settle(&mut app);
        assert_eq!(app.chat.thinking_status().0, "Maximum");
        click(&mut app, &mut terminal, ChatAction::Picker(Picker::Modes));
        click(&mut app, &mut terminal, ChatAction::Pick(1));
        settle(&mut app);
        assert_eq!(app.chat.mode_name(), Some("Plan"));
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 64, 30)))
            .unwrap();
        assert!(text(&terminal).contains("Context: 256K"));
        for (id, index, visible) in [
            ("budget-control", 0, "Context: 128K"),
            ("detail-level", 1, "Response detail: Full"),
            ("include-references", 1, "References: Off"),
        ] {
            click(
                &mut app,
                &mut terminal,
                ChatAction::Picker(Picker::Values(id.into())),
            );
            click(&mut app, &mut terminal, ChatAction::Pick(index));
            settle(&mut app);
            terminal
                .draw(|f| draw(f, &mut app, Rect::new(0, 0, 64, 30)))
                .unwrap();
            assert!(text(&terminal).contains(visible), "missing {visible}");
        }
        // A new model can replace both the current values and available choices.
        click(&mut app, &mut terminal, ChatAction::Picker(Picker::Models));
        click(&mut app, &mut terminal, ChatAction::Pick(0));
        settle(&mut app);
        terminal
            .draw(|f| draw(f, &mut app, Rect::new(0, 0, 64, 30)))
            .unwrap();
        assert_eq!(app.chat.model_name(), Some("Workspace Fast"));
        assert!(text(&terminal).contains("Workspace Fast * Balanced"));
        assert_eq!(app.chat.picker, Some(Picker::Thinking));
        assert_eq!(app.chat.choices.len(), 3);
        assert_eq!(app.chat.choices[0].label, "✓ Balanced");
        app.chat_action(ChatAction::DismissPicker);
        click(
            &mut app,
            &mut terminal,
            ChatAction::Picker(Picker::Values("budget-control".into())),
        );
        assert_eq!(app.chat.choices.len(), 1);
        assert_eq!(app.chat.choices[0].label, "✓ 128K");
        app.chat_action(ChatAction::Disconnect);
    }

    #[test]
    #[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
    fn acp_chat_sdk_round_trip_through_workbench() {
        use std::time::{Duration, Instant};
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("context.txt"), "disk one\ndisk two\n").unwrap();
        std::fs::write(root.path().join("generated.txt"), "before\n").unwrap();
        let mut app = App::test_new();
        app.explorer.root = root.path().to_path_buf();
        let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
        let mut config = vtcode::acp::AgentConfig::new("Official SDK scenario agent", python);
        config.args = vec![
            "-u".into(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/scenario_agent.py")
                .to_string_lossy()
                .into_owned(),
        ];
        app.settings_effective.acp_agents = Some(vec![config]);
        app.focus_chat();
        app.chat_action(ChatAction::Connect(0));
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.chat.selected.is_none() {
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        app.open_file(&root.path().join("context.txt"));
        app.wait_for_background(Duration::from_secs(20), |app| {
            app.editor().is_some_and(|editor| !editor.bg_loading)
        });
        assert_eq!(app.editor().unwrap().rope, "disk one\ndisk two\n");
        // The host must serve the latest unsaved editor content to the agent.
        let editor = app.editor_mut().unwrap();
        editor.rope = ropey::Rope::from_str("unsaved one\r\nunsaved two\r\n");
        editor.modified = true;
        editor.publish_document();
        app.focus_chat();
        app.chat.input = "exercise".into();
        app.chat.cursor = 8;
        app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        let mut terminal = Terminal::new(TestBackend::new(140, 44)).unwrap();
        let mut approved = false;
        while app.chat.busy() {
            app.poll_background_results();
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            if !approved && app.chat.permission().is_some() {
                let hit = app
                    .chat
                    .hits
                    .iter()
                    .find(|h| matches!(&h.action, ChatAction::Permission(_, _, Some(_))))
                    .expect("visible permission control")
                    .clone();
                app.handle_chat_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: hit.area.x,
                    row: hit.area.y,
                    modifiers: KeyModifiers::empty(),
                });
                approved = true;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(approved);
        assert!(app.chat.permissions.is_empty());
        assert_eq!(
            std::fs::read_to_string(root.path().join("generated.txt")).unwrap(),
            "after\n"
        );
        assert!(app.editor().unwrap().modified);
        let session = app.chat.session().unwrap();
        assert!(
            session
                .entries
                .iter()
                .any(|e| matches!(e, ChatEntry::Agent(t) if t.contains("unsaved two\r\n")))
        );
        assert!(
            session
                .entries
                .iter()
                .any(|e| matches!(e, ChatEntry::Edit(_)))
        );
        assert!(session.terminals.values().all(|t| t.released));
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(text(&terminal).contains("streaming"));
        assert!(text(&terminal).contains("3% context"));
        let usage = app
            .chat
            .context_usage_area
            .expect("visible context usage meter");
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: usage.x,
                row: usage.y,
                modifiers: KeyModifiers::empty(),
            },
        );
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(text(&terminal).contains("256 / 8,192 tokens"));
        assert!(text(&terminal).contains("7,936 tokens remaining"));
        app.mouse_col = 0;
        app.mouse_row = 0;
        app.open_file(&root.path().join("generated.txt"));
        app.wait_for_background(Duration::from_secs(20), |app| {
            app.editor().is_some_and(|editor| !editor.bg_loading)
        });
        assert_eq!(app.editor().unwrap().rope, "after\n");
        let editor = app.editor_mut().unwrap();
        editor.select_all();
        editor.paste_plain_with_text("my unsaved change\n");
        app.focus_chat();
        app.chat.input = "exercise".into();
        app.chat.cursor = 8;
        app.chat_action(ChatAction::Send);
        let mut answered = false;
        while app.chat.busy() {
            app.poll_background_results();
            app.poll_acp();
            if !answered && let Some(permission) = app.chat.permission() {
                let action = ChatAction::Permission(
                    permission.id.clone(),
                    permission.ticket,
                    Some(permission.request.options[0].option_id.clone()),
                );
                app.chat_action(action);
                answered = true;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(answered && app.chat.error.is_some());
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "my unsaved change\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("generated.txt")).unwrap(),
            "after\n"
        );
        app.chat_action(ChatAction::Disconnect);
    }

    #[test]
    #[ignore = "writes render captures for visual QA; set VTCODE_CHAT_CAPTURE"]
    fn acp_chat_capture_workbench() {
        let folder = std::path::PathBuf::from(
            std::env::var_os("VTCODE_CHAT_CAPTURE").expect("capture directory"),
        );
        std::fs::create_dir_all(&folder).unwrap();
        for theme in ["dark", "light", "high-contrast", "monokai"] {
            let mut app = seeded();
            app.set_theme_by_key(theme);
            app.chat.width = 64;
            app.chat.input = "Explain the change and suggest a follow-up test.".into();
            app.chat.cursor = app.chat.input.len();
            app.chat.session_mut().unwrap().usage = Some(p::UsageUpdate::new(53_000, 200_000));
            app.new_file();
            app.editor_mut().unwrap().rope = ropey::Rope::from_str(
                "pub fn greeting(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n\n#[test]\nfn welcomes_the_reader() {\n    assert_eq!(greeting(\"world\"), \"Hello, world!\");\n}\n",
            );
            app.focus_chat();
            let mut terminal = Terminal::new(TestBackend::new(144, 44)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let cells: Vec<_> = buffer.content.iter().map(|c| serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg),"bold":c.modifier.contains(Modifier::BOLD)})).collect();
            std::fs::write(
                folder.join(format!("chat-{theme}.json")),
                serde_json::to_vec(&serde_json::json!({"width":144,"height":44,"cells":cells}))
                    .unwrap(),
            )
            .unwrap();
        }
        for (name, width, height) in [
            ("permission", 58, 28),
            ("modes", 100, 32),
            ("models", 100, 32),
            ("thinking", 100, 32),
            ("context", 100, 32),
            ("options", 100, 32),
            ("narrow", 40, 20),
            ("usage", 100, 30),
            ("usage-narrow", 36, 22),
            ("usage-unavailable", 100, 30),
            ("working", 100, 30),
            ("welcome", 100, 30),
            ("welcome-narrow", 36, 26),
            ("toolbar-narrow", 36, 26),
            ("toolbar-default", 58, 30),
            ("toolbar-wide", 110, 30),
        ] {
            let mut app = if name.starts_with("welcome") {
                let mut app = App::test_new();
                app.focus_chat();
                app
            } else {
                seeded()
            };
            if name.starts_with("toolbar") {
                app.chat.agent_order = vec![app.chat.active_agent];
                app.chat.rollback.enabled = true;
                app.chat.rollback.initialized = true;
                app.chat.thinking_expanded = true;
                app.chat.session_mut().unwrap().commands = serde_json::from_value(serde_json::json!([
                    {"name":"inspect", "description":"Inspect workspace"}
                ])).unwrap();
            } else if name == "working" {
                let session = app.chat.session_mut().unwrap();
                *session = Session::new(session.id.clone());
                session.begin_prompt("Inspect the workspace and suggest improvements.".into());
            } else if name == "permission" {
                app.chat.session_mut().unwrap().busy = true;
                let request = serde_json::from_value(serde_json::json!({"sessionId":"ui-session","toolCall":{"toolCallId":"review","title":"Run workspace checks","rawInput":{"command":"cargo test"}},"options":[{"optionId":"once","name":"Allow once","kind":"allow_once"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]})).unwrap();
                app.apply_acp_event(vtcode::acp::Event::Permission {
                    ticket: 0,
                    id: serde_json::from_value(serde_json::json!(12)).unwrap(),
                    request,
                });
            } else if name == "modes" {
                app.chat_action(ChatAction::Picker(Picker::Modes));
            } else if name == "models" {
                app.chat_action(ChatAction::Picker(Picker::Models));
            } else if name == "thinking" {
                app.chat_action(ChatAction::Picker(Picker::Values("effort-29".into())));
            } else if name == "context" {
                app.chat_action(ChatAction::Picker(Picker::Values("budget-83".into())));
            } else if name == "options" {
                app.chat_action(ChatAction::Picker(Picker::Options));
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    if name.starts_with("toolbar") {
                        draw(frame, &mut app, frame.area());
                    } else {
                        crate::ui::draw(frame, &mut app);
                    }
                })
                .unwrap();
            if name.starts_with("usage") {
                if name != "usage-unavailable" {
                    app.chat.session_mut().unwrap().usage =
                        Some(p::UsageUpdate::new(164_000, 200_000));
                }
                // Render once more because percentage width may change the anchor.
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                let usage = app.chat.context_usage_area.unwrap();
                app.mouse_col = usage.x;
                app.mouse_row = usage.y;
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
            }
            let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|c| serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg),"bold":c.modifier.contains(Modifier::BOLD)})).collect();
            std::fs::write(
                folder.join(format!("chat-{name}.json")),
                serde_json::to_vec(
                    &serde_json::json!({"width":width,"height":height,"cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }
}

#[cfg(test)]
#[path = "ui_chat_theme_tests.rs"]
mod theme_tests;

#[cfg(test)]
#[path = "ui_chat_tabs_tests.rs"]
mod tab_tests;

#[cfg(test)]
#[path = "ui_chat_history_tests.rs"]
mod history_tests;
