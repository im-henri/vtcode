//! Rendering: the mode indicator, command line, messages, multi-line
//! output, wildmenu, and search highlighting.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use super::text::Text;
use super::{Mode, VisualKind};
use crate::app::App;

thread_local! {
    static CARET: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn begin_caret_frame() {
    CARET.with(|c| c.set(None));
}

/// The editor renderer reports where it drew the primary caret.
pub(crate) fn note_caret(x: u16, y: u16) {
    CARET.with(|c| c.set(Some((x, y))));
}

/// Insert and Replace mode use the terminal cursor (a bar or underline)
/// instead of the painted block, as terminal Vim does.
pub(crate) fn bar_caret(app: &App) -> bool {
    app.vim.enabled
        && matches!(app.vim.mode, Mode::Insert | Mode::Replace)
        && app.terminal_has_focus
        && !app.rendering_inactive_editor
        && super::app_host::owns_editor(app)
}

/// Show the terminal cursor at the recorded caret for a bar caret.
pub(crate) fn place_caret(f: &mut Frame, app: &mut App) {
    let shape = if bar_caret(app)
        && let Some(pos) = CARET.with(|c| c.get())
    {
        f.set_cursor_position(pos);
        Some(match (app.vim.mode, app.cursor_blinking) {
            (Mode::Replace, true) => crossterm::cursor::SetCursorStyle::BlinkingUnderScore,
            (Mode::Replace, false) => crossterm::cursor::SetCursorStyle::SteadyUnderScore,
            (_, true) => crossterm::cursor::SetCursorStyle::BlinkingBar,
            (_, false) => crossterm::cursor::SetCursorStyle::SteadyBar,
        })
    } else {
        None
    };
    app.vim_cursor_shape = shape;
}

fn mode_color(app: &App) -> Color {
    let c = &app.colors;
    match app.vim.mode {
        Mode::Normal if app.vim.op_pending() => c.fg_warning,
        Mode::Normal => c.fg_accent_blue,
        Mode::Insert => c.fg_success,
        Mode::Replace => c.fg_error,
        Mode::Visual(VisualKind::Char)
        | Mode::Visual(VisualKind::Line)
        | Mode::Visual(VisualKind::Block) => c.fg_warning,
        Mode::Cmdline => c.fg_accent_blue,
    }
}

/// The status bar item: (text, colour). Shown whenever Vim mode is on.
pub(crate) fn status_item(app: &App) -> Option<(String, Color)> {
    if !app.vim.enabled {
        return None;
    }
    let mut text = format!(" {} ", super::feedback::badge(app));
    if let Some(rec) = app.vim.recording_label() {
        text.push_str(&format!(" {rec}"));
    }
    let pending = super::feedback::pending(app);
    if app.vim.opts.showcmd && !pending.is_empty() {
        text.push_str(&format!(" {pending}"));
    }
    Some((text, badge_color(app)))
}

/// The badge colour: the mode's in the editor, a neutral one elsewhere, red
/// for a moment after a key that is not a command.
fn badge_color(app: &App) -> Color {
    if super::feedback::bell_active(app) {
        return app.colors.fg_error;
    }
    if super::feedback::context(app).is_some() {
        return super::feedback::context_color(app);
    }
    mode_color(app)
}

/// Paint the mode badge and, while active, the command line or message
/// over the status bar row.
pub(crate) fn draw_status(f: &mut Frame, app: &mut App, area: Rect) {
    if !app.vim.enabled || area.is_empty() {
        return;
    }
    let area = Rect::new(area.x, area.y, area.width, 1);
    let bg = app.colors.bg_statusbar;
    let fg = app
        .colors
        .ui
        .readable_foreground(app.colors.fg_statusbar, bg, 4.5);
    let badge_color = badge_color(app);
    let badge_fg = app.colors.ui.readable_foreground(bg, badge_color, 4.5);
    let badge_text = super::feedback::badge(app);
    let mode_width = badge_text.chars().count() as u16 + 2;
    let badge_x = app
        .status_bar_items
        .iter()
        .find(|(_, _, id)| *id == "vim_mode")
        .map(|(x, _, _)| *x);
    let paint_badge = |f: &mut Frame, x: u16| {
        for dx in 0..mode_width.min(area.right().saturating_sub(x)) {
            if let Some(cell) = f.buffer_mut().cell_mut((x + dx, area.y)) {
                cell.set_bg(badge_color);
                cell.set_fg(badge_fg);
                cell.modifier.insert(Modifier::BOLD);
            }
        }
    };
    let cmdline = app
        .vim
        .cmdline
        .as_ref()
        .map(|c| (c.kind, c.string(), c.cursor));
    let message = app
        .vim
        .message
        .as_ref()
        .filter(|m| m.lines.is_empty())
        .map(|m| (m.text.clone(), m.error));
    let confirm = app.vim.confirm.is_some();
    if cmdline.is_none() && message.is_none() && !confirm {
        if let Some(x) = badge_x {
            paint_badge(f, x);
        }
        return;
    }
    // The Vim command line replaces the status bar while in use.
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);
    let mut x = area.x;
    if cmdline.is_none() {
        let label = format!(" {badge_text} ");
        f.render_widget(
            Paragraph::new(label.clone()).style(
                Style::default()
                    .fg(badge_fg)
                    .bg(badge_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Rect::new(x, area.y, mode_width.min(area.width), 1),
        );
        x += mode_width + 1;
    }
    let width = area.right().saturating_sub(x);
    if let Some((kind, text, cursor)) = cmdline {
        let full: Vec<char> = std::iter::once(kind).chain(text.chars()).collect();
        let cursor = cursor + 1;
        // Scroll so the cursor stays visible.
        let w = width as usize;
        let start = cursor.saturating_sub(w.saturating_sub(1));
        let visible: String = full.iter().skip(start).take(w).collect();
        let mut spans = Vec::new();
        let rel = cursor - start;
        let before: String = visible.chars().take(rel).collect();
        let at: String = visible
            .chars()
            .nth(rel)
            .map_or(" ".to_string(), |c| c.to_string());
        let after: String = visible.chars().skip(rel + 1).collect();
        spans.push(Span::styled(before, Style::default().fg(fg).bg(bg)));
        spans.push(Span::styled(at, Style::default().fg(bg).bg(fg)));
        spans.push(Span::styled(after, Style::default().fg(fg).bg(bg)));
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(x, area.y, width, 1),
        );
        return;
    }
    if let Some((text, error)) = message {
        let color = if error { app.colors.fg_error } else { fg };
        let shown: String = text.chars().take(width as usize).collect();
        f.render_widget(
            Paragraph::new(shown).style(Style::default().fg(color).bg(bg)),
            Rect::new(x, area.y, width, 1),
        );
    }
}

/// Multi-line output (`:reg`, `:ls`...) and the wildmenu, above the
/// status bar.
pub(crate) fn draw_overlays(f: &mut Frame, app: &mut App) {
    if !app.vim.enabled {
        return;
    }
    super::feedback::draw_focus_flash(f, app);
    super::feedback::draw_hint(f, app);
    let Some(status) = app.status_bar_area else {
        return;
    };
    let screen = f.area();
    let bg = app.colors.bg_statusbar;
    let fg = app
        .colors
        .ui
        .readable_foreground(app.colors.fg_statusbar, bg, 4.5);
    if let Some(m) = app.vim.message.as_ref().filter(|m| !m.lines.is_empty()) {
        let max = status.y.saturating_sub(screen.y) as usize;
        let lines: Vec<&String> = m.lines.iter().collect();
        let shown = lines.len().min(max.saturating_sub(1));
        let skip = lines.len() - shown;
        let h = (shown + 1) as u16;
        let rect = Rect::new(screen.x, status.y.saturating_sub(h), screen.width, h);
        f.render_widget(Clear, rect);
        let mut text: Vec<Line> = lines[skip..]
            .iter()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(fg).bg(bg))))
            .collect();
        text.push(Line::from(Span::styled(
            "Press ENTER or type command to continue",
            Style::default().fg(app.colors.fg_success).bg(bg),
        )));
        f.render_widget(Paragraph::new(text).style(Style::default().bg(bg)), rect);
    }
    if let Some(comp) = app.vim.cmdline.as_ref().and_then(|c| c.completion.as_ref()) {
        if comp.items.is_empty() {
            return;
        }
        let rect = Rect::new(screen.x, status.y.saturating_sub(1), screen.width, 1);
        f.render_widget(Clear, rect);
        let mut spans = Vec::new();
        let mut used = 0usize;
        let sel = comp.index;
        // Keep the selected item in view.
        let start = sel.map_or(0, |s| {
            let mut w = 0;
            let mut st = s;
            while st > 0 && w + comp.items[st].chars().count() + 2 < screen.width as usize {
                w += comp.items[st].chars().count() + 2;
                st -= 1;
            }
            if w + 2 >= screen.width as usize {
                st + 1
            } else {
                st
            }
        });
        for (i, item) in comp.items.iter().enumerate().skip(start) {
            let w = item.chars().count() + 2;
            if used + w > screen.width as usize {
                spans.push(Span::styled(">", Style::default().fg(fg).bg(bg)));
                break;
            }
            let style = if Some(i) == sel {
                Style::default().fg(bg).bg(app.colors.fg_warning)
            } else {
                Style::default().fg(fg).bg(bg)
            };
            spans.push(Span::styled(format!(" {item} "), style));
            used += w;
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            rect,
        );
    }
}

/// Compute `hlsearch`/`incsearch` matches for visible editors before
/// rendering. Matches use the editor's own search highlighting.
pub(crate) fn prepare_frame(app: &mut App) {
    if !app.vim.enabled {
        return;
    }
    super::feedback::update(app);
    let incsearch = app
        .vim
        .incsearch
        .clone()
        .filter(|_| app.vim.mode == Mode::Cmdline);
    let pattern = incsearch.clone().or_else(|| {
        (app.vim.opts.hlsearch && !app.vim.hl_hidden)
            .then(|| app.vim.last_search.as_ref().map(|s| s.0.clone()))
            .flatten()
    });
    let preview = app.vim.cmdline.as_ref().and_then(|c| c.preview);
    let re = pattern.as_ref().and_then(|p| app.vim.compile(p).ok());
    let owned = app.vim_owns_search_matches;
    let find_widget = app.search_active;
    if find_widget {
        app.vim_owns_search_matches = false;
        return;
    }
    let Some(re) = re else {
        if owned {
            for tab in visible_tabs(app) {
                tab.file_search.search_matches.clear();
            }
            app.vim_owns_search_matches = false;
        }
        return;
    };
    let source = re.source.clone();
    let cursor_idx = app
        .editor()
        .map(|ed| Text::of(&ed.rope, ed.vim_buf.trailing_empty).idx(super::exec::cursor(ed)));
    let active_nav = app.editor().map(|ed| ed.navigation_id);
    for tab in visible_tabs(app) {
        let ed = &tab.editor;
        if ed.large_file_mode {
            continue;
        }
        let key = (ed.navigation_id, ed.content_version, source.clone());
        let fresh = tab.file_search.vim_hl_key.as_ref() == Some(&key)
            && tab.file_search.search_matches.len() == tab.file_search.vim_hl_len;
        // Large buffers: only the lines around the view (Vim also only
        // highlights what is visible).
        let window = if ed.rope.len_bytes() > 2_000_000 {
            let first = ed.scroll_offset.saturating_sub(500);
            let last = (ed.scroll_offset + ed.viewport_height + 500)
                .min(ed.rope.len_lines().saturating_sub(1));
            Some((first, last))
        } else {
            None
        };
        let fresh = fresh && window.is_none();
        if !fresh {
            let (hay, base_char) = match window {
                Some((first, last)) => {
                    let s = ed.rope.line_to_char(first);
                    let e = if last + 1 < ed.rope.len_lines() {
                        ed.rope.line_to_char(last + 1)
                    } else {
                        ed.rope.len_chars()
                    };
                    (std::sync::Arc::new(ed.rope.slice(s..e).to_string()), s)
                }
                None => (std::sync::Arc::new(ed.rope.to_string()), 0),
            };
            let base_byte = ed.rope.char_to_byte(base_char);
            let mut matches = Vec::new();
            for (s, e) in re.find_iter(&hay) {
                if e == s {
                    continue;
                }
                let sc = ed.rope.byte_to_char(base_byte + s);
                let ec = ed.rope.byte_to_char(base_byte + e);
                let line = ed.rope.char_to_line(sc);
                let base = ed.rope.line_to_char(line);
                matches.push((line, sc - base, ec - base));
                if matches.len() >= 100_000 {
                    break;
                }
            }
            tab.file_search.vim_hl_len = matches.len();
            tab.file_search.search_matches = matches;
            tab.file_search.vim_hl_key = Some(key);
        }
        // The current match: the incsearch preview or the one at the cursor.
        let current = if Some(ed.navigation_id) == active_nav {
            let at = preview.map(|(s, _)| s).or(cursor_idx);
            at.and_then(|at| {
                let line = ed.rope.char_to_line(at.min(ed.rope.len_chars()));
                let col = at - ed.rope.line_to_char(line);
                tab.file_search
                    .search_matches
                    .iter()
                    .position(|&(l, s, e)| l == line && col >= s && col < e.max(s + 1))
            })
        } else {
            None
        };
        tab.file_search.search_current = current.unwrap_or(usize::MAX);
    }
    app.vim_owns_search_matches = true;
}

fn visible_tabs(app: &mut App) -> Vec<&mut crate::app::Tab> {
    let active = app.active_tab;
    let active_group = app.active_group;
    let desk = app.desks.active;
    let mut out: Vec<&mut crate::app::Tab> = Vec::new();
    if let Some(t) = app.tabs.get_mut(active) {
        out.push(t);
    }
    for (i, g) in app.groups.iter_mut().enumerate() {
        if i == active_group || g.desk != desk {
            continue;
        }
        let at = g.active_tab;
        if let Some(t) = g.tabs.get_mut(at) {
            out.push(t);
        }
    }
    out
}
