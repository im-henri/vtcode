//! Terminal cards reserve a fixed viewport in chat; only visible pages are read.
use super::*;
use vtcode::acp::terminal_history::TERMINAL_ROWS;
use vtcode::acp::terminal_style::{AnsiColor, TerminalLine};

const COLLAPSED_COMMAND_ROWS: usize = 4;

#[derive(Clone)]
pub(super) enum Part {
    SummaryHeader,
    Command(String),
    CommandMore(usize),
    Directory,
    Output(usize, usize),
    Top,
    Bottom,
}
#[derive(Clone)]
pub(super) struct TerminalRow {
    pub id: String,
    reported: Option<Arc<tool_terminals::ReportedTerminal>>,
    part: Part,
    index: usize,
    expanded: bool,
    minimal: bool,
}

pub(super) fn rows(
    rows: &mut Vec<ChatRow>,
    id: &str,
    command: &str,
    width: usize,
    index: usize,
    expanded: bool,
    preview: bool,
) {
    terminal_rows(
        rows,
        id,
        &format!("$ {command}"),
        width,
        index,
        expanded,
        preview,
        None,
    );
}

pub(super) fn reported_rows(
    rows: &mut Vec<ChatRow>,
    terminal: Arc<tool_terminals::ReportedTerminal>,
    width: usize,
    index: usize,
    expanded: bool,
    preview: bool,
) {
    let id = terminal.id.clone();
    let command = format!(
        "{}{}",
        if terminal.command_reported { "$ " } else { "" },
        terminal.command
    );
    terminal_rows(
        rows,
        &id,
        &command,
        width,
        index,
        expanded,
        preview,
        Some(terminal),
    );
}

fn terminal_rows(
    rows: &mut Vec<ChatRow>,
    id: &str,
    command: &str,
    width: usize,
    index: usize,
    expanded: bool,
    preview: bool,
    reported: Option<Arc<tool_terminals::ReportedTerminal>>,
) {
    let minimal = !expanded && !preview;
    let height = if expanded {
        TERMINAL_ROWS
    } else if preview {
        2
    } else {
        0
    };
    let mut chrome = vec![Part::SummaryHeader];
    if expanded && reported.as_ref().is_none_or(|t| t.cwd.is_some()) {
        chrome.push(Part::Directory);
    }
    if !minimal {
        chrome.push(Part::Top);
    }
    let mut command_lines = wrap_command(command, width.saturating_sub(4));
    let hidden_lines = if !expanded && command_lines.len() > COLLAPSED_COMMAND_ROWS {
        let visible = COLLAPSED_COMMAND_ROWS - 1;
        let hidden = command_lines.len() - visible;
        command_lines.truncate(visible);
        Some(hidden)
    } else {
        None
    };
    let parts = chrome
        .into_iter()
        .chain(command_lines.into_iter().map(Part::Command))
        .chain(hidden_lines.map(Part::CommandMore))
        .chain((0..height).map(|i| Part::Output(i, height)))
        .chain((!minimal).then_some(Part::Bottom));
    rows.extend(parts.map(|part| ChatRow {
        line: Line::default(),
        action: None,
        background: None,
        terminal: Some(TerminalRow {
            id: id.into(),
            reported: reported.clone(),
            part,
            index,
            expanded,
            minimal,
        }),
        separator: false,
    }));
}

pub(super) fn draw_row(f: &mut Frame, app: &mut App, rect: Rect, row: &TerminalRow) {
    let id = &row.id;
    let terminal = app.chat.session().and_then(|s| s.terminals.get(id));
    if terminal.is_none() && row.reported.is_none() {
        return;
    }
    let alive = terminal.is_some_and(|t| !t.released && t.exit_status.is_none() && !t.killed);
    let active_input = app.chat.terminal_input.as_ref() == Some(id);
    let chrome = !matches!(
        row.part,
        Part::Output(..) | Part::Command(_) | Part::CommandMore(_) | Part::Top | Part::Bottom
    );
    let background = if chrome {
        app.colors.bg_sidebar
    } else {
        app.colors.ui.bg_terminal
    };
    let foreground = app.colors.ui.readable_foreground(app.colors.ui.fg_terminal, background, 4.5);
    let dim = app.colors.ui.readable_foreground(app.colors.fg_dim, background, 4.5);
    let style = Style::default().fg(foreground).bg(background);
    f.render_widget(
        Paragraph::new(" ".repeat(rect.width as usize)).style(style),
        rect,
    );
    if matches!(row.part, Part::SummaryHeader) {
        let status = terminal_status(row, terminal, active_input);
        summary_header(f, app, rect, row, &status, alive);
        return;
    }
    if matches!(row.part, Part::Top | Part::Bottom) {
        let (left, right) = if matches!(row.part, Part::Top) {
            ('╭', '╮')
        } else {
            ('╰', '╯')
        };
        f.render_widget(
            Paragraph::new(format!(
                "{left}{}{right}",
                "─".repeat(rect.width.saturating_sub(2) as usize)
            ))
            .style(style.fg(app.colors.ui.fg_group_border)),
            rect,
        );
        return;
    }
    if !row.minimal
        && matches!(
            row.part,
            Part::Command(_) | Part::CommandMore(_) | Part::Output(..)
        )
        && rect.width > 1
    {
        for x in [rect.x, rect.right() - 1] {
            f.render_widget(
                Paragraph::new("│").style(style.fg(app.colors.ui.fg_group_border)),
                Rect::new(x, rect.y, 1, 1),
            );
        }
    }
    if let Part::CommandMore(hidden) = &row.part {
        let area = Rect::new(rect.x + 2, rect.y, rect.width.saturating_sub(4), 1);
        let label = format!("… {hidden} more lines · Expand");
        f.render_widget(
            Paragraph::new(elide(&label, area.width as usize)).style(style.fg(dim)),
            area,
        );
        if area.width > 0 {
            app.chat.hits.push(ChatHit {
                area,
                action: ChatAction::ToggleEntry(row.index),
            });
        }
        return;
    }
    let label = match &row.part {
        Part::Command(text) => Some(text.clone()),
        Part::Directory => Some(format!(
            "  {}",
            clean(
                &row.reported
                    .as_ref()
                    .and_then(|t| t.cwd.as_deref())
                    .unwrap_or_else(|| &terminal.unwrap().cwd)
                    .display()
                    .to_string()
            )
        )),
        _ => None,
    };
    if let Some(label) = label {
        if matches!(row.part, Part::Command(_)) {
            f.render_widget(
                Paragraph::new(command_line(app, &label)).style(style),
                Rect::new(rect.x + 2, rect.y, rect.width.saturating_sub(4), 1),
            );
            return;
        }
        f.render_widget(
            Paragraph::new(elide(&label, rect.width as usize)).style(style.fg(
                if matches!(row.part, Part::Directory) {
                    dim
                } else {
                    app.colors.ui.fg_sidebar
                },
            )),
            rect,
        );
        return;
    }
    match row.part {
        Part::Output(offset, height) => {
            let width = rect.width.saturating_sub(2) as usize;
            if let Some(terminal) = &row.reported {
                app.request_chat_tool_terminal_page(
                    id,
                    terminal.revision,
                    &terminal.lines,
                    &terminal.styled,
                    terminal.columns,
                    width,
                    height,
                );
            } else {
                app.request_chat_terminal_page(id, width, height);
            }
            let view = app.chat.terminal_views.get(id).unwrap();
            let line = (offset == 0)
                .then_some(view.error.as_deref())
                .flatten()
                .or_else(|| {
                    view.page
                        .as_ref()
                        .and_then(|p| p.lines.get(offset))
                        .map(String::as_str)
                })
                .unwrap_or(if offset == 0 && view.page.is_none() {
                    "Loading output…"
                } else {
                    ""
                });
            let styled = view
                .page
                .as_ref()
                .and_then(|p| p.styled.get(offset))
                .filter(|_| offset != 0 || view.error.is_none());
            let line = styled.map_or_else(
                || Line::raw(line.to_owned()),
                |line| output_line(line, &app.colors),
            );
            f.render_widget(
                Paragraph::new(line).style(style),
                Rect::new(rect.x + 1, rect.y, rect.width.saturating_sub(2), 1),
            );
            if row.expanded {
                if let Some((_, area)) = app
                    .chat
                    .terminal_areas
                    .iter_mut()
                    .find(|(key, area)| key == id && area.bottom() == rect.y)
                {
                    area.height += 1;
                } else {
                    app.chat.terminal_areas.push((id.clone(), rect));
                }
                if let Some(hit) = app.chat.hits.iter_mut().find(|h| {
                    h.action == ChatAction::FocusTerminal(id.clone()) && h.area.bottom() == rect.y
                }) {
                    hit.area.height += 1;
                } else {
                    app.chat.hits.push(ChatHit {
                        area: rect,
                        action: ChatAction::FocusTerminal(id.clone()),
                    });
                }
            }
        }
        _ => {}
    }
}

fn terminal_status(
    row: &TerminalRow,
    terminal: Option<&vtcode::acp::TerminalSnapshot>,
    active_input: bool,
) -> String {
    if let Some(reported) = &row.reported {
        return reported.status.clone();
    }
    let terminal = terminal.unwrap();
    if let Some(error) = &terminal.error {
        format!("! {error}")
    } else if terminal.killed {
        if terminal.exit_status.is_some() || terminal.released {
            "■ Killed".into()
        } else {
            "■ Stopping…".into()
        }
    } else if let Some(status) = &terminal.exit_status {
        status.exit_code.map_or_else(
            || {
                format!(
                    "Exited · {}",
                    status.signal.as_deref().unwrap_or("unknown status")
                )
            },
            |code| format!("Exit {code}"),
        )
    } else if terminal.released {
        "Released".into()
    } else if active_input {
        "● Running · Input unlocked".into()
    } else {
        "● Running · Read-only".into()
    }
}

fn summary_header(
    f: &mut Frame,
    app: &mut App,
    rect: Rect,
    row: &TerminalRow,
    status: &str,
    alive: bool,
) {
    // Keep the toggle anchored to the same columns in both states, even when
    // expanding reveals input controls or more terminal metadata.
    let toggle = match (rect.width >= 46, row.expanded) {
        (true, false) => " Expand   ",
        (true, true) => " Collapse ",
        (false, false) => " ▸ ",
        (false, true) => " ▾ ",
    };
    if rect.width < toggle.width() as u16 {
        button(
            f,
            app,
            rect,
            if row.expanded { "▾" } else { "▸" },
            ChatAction::ToggleEntry(row.index),
            true,
            false,
        );
        return;
    }
    let stop = if alive {
        Some((" Kill ", ChatAction::StopTerminal(row.id.clone())))
    } else if row.reported.as_ref().is_some_and(|t| t.running) && app.chat.busy() {
        Some((
            if rect.width >= 60 {
                " Stop response "
            } else {
                " Stop "
            },
            ChatAction::Stop,
        ))
    } else {
        None
    };
    let stop = stop.filter(|(label, _)| toggle.width() + label.width() + 1 <= rect.width as usize);
    let active_input = app.chat.terminal_input.as_ref() == Some(&row.id);
    let input = (alive && row.expanded).then(|| {
        let label = match (rect.width >= 76, active_input) {
            (true, true) => " Lock input ",
            (true, false) => " Unlock input ",
            (false, true) => " Lock ",
            (false, false) => " Input ",
        };
        (
            label,
            if active_input {
                ChatAction::BackToChat
            } else {
                ChatAction::Terminal(row.id.clone())
            },
        )
    });
    let controls_width = toggle.width() + stop.as_ref().map_or(0, |(label, _)| label.width() + 1);
    let input = input.filter(|(label, _)| controls_width + label.width() + 2 < rect.width as usize);
    let controls_width = controls_width + input.as_ref().map_or(0, |(label, _)| label.width() + 1);
    let title_width = rect.width.saturating_sub(controls_width as u16 + 1);
    let status = if row.minimal {
        status
            .trim_end_matches(" · Read-only")
            .trim_end_matches(" · Agent-managed")
    } else {
        status
    };
    let (icon, detail, color) = if let Some(detail) = status.strip_prefix("✓ ") {
        ("✓", detail, app.colors.fg_success)
    } else if status == "Exit 0" || status.starts_with("Exit 0 ·") {
        ("✓", status, app.colors.fg_success)
    } else if let Some(detail) = status.strip_prefix("● ") {
        ("●", detail, app.colors.fg_accent_blue)
    } else if let Some(detail) = status.strip_prefix("■ ") {
        ("■", detail, app.colors.fg_warning)
    } else if status.starts_with('!') || status.starts_with("Exit ") {
        ("!", status.trim_start_matches("! "), app.colors.fg_error)
    } else {
        ("▣", status, app.colors.fg_dim)
    };
    // Preserve the agent-terminal identity and state icon in narrow panes;
    // add the full status and agent name when there is room on this same row.
    let mut title = format!("{icon} Agent terminal");
    if title.width() + detail.width() + 3 <= title_width as usize {
        title.push_str(&format!(" · {detail}"));
    }
    let name_width = (title_width as usize).saturating_sub(title.width() + 3);
    if name_width >= 4 {
        let name = if row.reported.is_some() {
            app.chat.name().to_owned()
        } else {
            format!("{} · {}", app.chat.name(), row.id)
        };
        title.push_str(&format!(" · {}", elide(&name, name_width)));
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(elide(
                title.strip_prefix(icon).unwrap_or(&title),
                title_width.saturating_sub(icon.width() as u16) as usize,
            )),
        ]))
        .style(Style::default().fg(app.colors.fg_dim)),
        Rect::new(rect.x, rect.y, title_width, 1),
    );
    let mut x = rect.right().saturating_sub(controls_width as u16);
    if let Some((label, action)) = stop {
        small_button(f, app, &mut x, rect, label, action, true);
    }
    if let Some((label, action)) = input {
        small_button(f, app, &mut x, rect, label, action, true);
    }
    small_button(
        f,
        app,
        &mut x,
        rect,
        toggle,
        ChatAction::ToggleEntry(row.index),
        true,
    );
}

fn wrap_command(command: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut rows = Vec::new();
    for line in clean_terminal(command).split('\n') {
        let mut row = String::new();
        let mut columns = 0;
        for grapheme in line.graphemes(true) {
            if columns + grapheme.width() > width.max(1) && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                columns = 0;
            }
            row.push_str(grapheme);
            columns += grapheme.width();
        }
        rows.push(row);
    }
    rows
}

#[test]
fn terminal_command_wrap_preserves_full_text_and_unicode_columns() {
    for command in [
        "$ cargo test --features very_long_feature_name -- --nocapture",
        "$ echo '世界 🌍 e\u{301} and more text'",
    ] {
        for width in [8, 17, 36, 80] {
            let lines = wrap_command(command, width);
            assert_eq!(lines.concat(), command);
            assert!(lines.iter().all(|line| line.width() <= width));
        }
    }
    assert_eq!(wrap_command("first\nsecond", 20), vec!["first", "second"]);
}

fn output_line(line: &TerminalLine, colors: &ThemeColors) -> Line<'static> {
    let color = |c: Option<AnsiColor>, fg| {
        colors.ui.terminal_color(
            match c {
                None => Color::Reset,
                Some(AnsiColor::Indexed(n)) => Color::Indexed(n),
                Some(AnsiColor::Rgb(r, g, b)) => Color::Rgb(r, g, b),
            },
            fg,
        )
    };
    let fallback = if line.text.starts_with('+') {
        Some(colors.ui.ansi[2])
    } else if line.text.starts_with('-') {
        Some(colors.ui.ansi[1])
    } else if line.text.starts_with("@@") || line.text.starts_with("diff --git ") {
        Some(colors.ui.ansi[6])
    } else {
        None
    };
    Line::from(
        line.runs
            .iter()
            .map(|run| {
                let mut style = Style::default()
                    .fg(if run.style.fg.is_none() {
                        fallback.unwrap_or_else(|| color(None, true))
                    } else {
                        color(run.style.fg, true)
                    })
                    .bg(color(run.style.bg, false));
                for (bit, modifier) in [
                    (1, Modifier::BOLD),
                    (2, Modifier::DIM),
                    (4, Modifier::ITALIC),
                    (8, Modifier::UNDERLINED),
                    (16, Modifier::REVERSED),
                    (32, Modifier::CROSSED_OUT),
                ] {
                    if run.style.attributes & bit != 0 {
                        style = style.add_modifier(modifier);
                    }
                }
                Span::styled(line.text[run.start..run.end].to_owned(), style)
            })
            .collect::<Vec<_>>(),
    )
}

fn command_line(app: &App, text: &str) -> Line<'static> {
    let (syntaxes, themes) = app.highlighter.shared_sets();
    let Some(theme) = themes.themes.get(&app.syntax_theme) else {
        return Line::styled(
            text.to_owned(),
            Style::default().fg(app.colors.fg_accent_blue),
        );
    };
    let syntax = syntaxes
        .find_syntax_by_extension("sh")
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
    let spans = highlighter
        .highlight_line(text, &syntaxes)
        .unwrap_or_default()
        .into_iter()
        .map(|(style, token)| {
            let fg = style.foreground;
            Span::styled(
                token.to_owned(),
                Style::default().fg(app.colors.ui.readable_foreground(
                    Color::Rgb(fg.r, fg.g, fg.b),
                    app.colors.ui.bg_terminal,
                    4.5,
                )),
            )
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}
