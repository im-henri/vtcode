//! Feedback while typing: which pane has the keyboard, the keys waiting for
//! the rest of a command (`^W`, `g`, `d2`...), a hint popup listing what can
//! follow a prefix (like Neovim's which-key), and a flash for keys that are
//! not a command.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::keys::Key;
use crate::app::{ActivePane, App, SidebarView, TabKind};

/// How long a prefix waits before its hints appear: fast typists never see
/// them, a pause shows what comes next.
pub(crate) const HINT_DELAY: Duration = Duration::from_millis(350);
/// How long the badge flashes after a key that is not a command.
pub(crate) const BELL: Duration = Duration::from_millis(180);

/// The pane that has the keyboard when it is not the Vim editor.
pub(crate) fn context(app: &App) -> Option<&'static str> {
    if super::app_host::owns_editor(app) || app.vim.cmdline.is_some() {
        return None;
    }
    if app.palette.active {
        return Some("PALETTE");
    }
    if app.quick_open_active {
        return Some("QUICK OPEN");
    }
    if app.menu_active
        || app.menu_open
        || app.tab_context_active
        || app.context_menu_active
        || app.explorer_context_active
        || app.sc_context_active
        || app.text_input_context_menu.is_some()
    {
        return Some("MENU");
    }
    if crate::ui_modal::active(app).is_some() || app.confirm_close_active || app.exit_confirm_active
    {
        return Some("DIALOG");
    }
    if app.terminal_panel_focused || app.focused_terminal_index().is_some() {
        return Some("TERMINAL");
    }
    match app.active_pane {
        ActivePane::Chat => return Some("CHAT"),
        ActivePane::Panel => {
            return Some(match app.panel_tab {
                2 => "OUTPUT",
                3 => "NOTIFICATIONS",
                _ => "PROBLEMS",
            });
        }
        ActivePane::Explorer => {
            return Some(match app.sidebar_view {
                SidebarView::Explorer => "EXPLORER",
                SidebarView::Search => "SEARCH",
                SidebarView::SourceControl => "SOURCE CONTROL",
                SidebarView::Extensions => "EXTENSIONS",
                SidebarView::Remote => "REMOTE",
                SidebarView::Settings => "SETTINGS",
            });
        }
        ActivePane::Editor => {}
    }
    if app.settings_editor_has_focus() {
        return Some("SETTINGS");
    }
    if matches!(
        app.focused_text_input_id(),
        Some(
            crate::text_input::TextInputId::SearchFind
                | crate::text_input::TextInputId::SearchReplace
        )
    ) {
        return Some("FIND");
    }
    Some(match app.tabs.get(app.active_tab).map(|t| t.kind) {
        Some(TabKind::Settings) => "SETTINGS",
        Some(TabKind::SourceControl) => "SOURCE CONTROL",
        Some(TabKind::MarkdownPreview) => "PREVIEW",
        Some(TabKind::Diff(_)) => "DIFF",
        Some(TabKind::CommitDetails | TabKind::AmendCommit) => "COMMIT",
        Some(TabKind::ExtensionView | TabKind::ExtensionInfo) => "EXTENSION",
        Some(TabKind::Terminal) => "TERMINAL",
        Some(TabKind::Chat(_)) => "CHAT",
        _ => "VIEW",
    })
}

/// The status bar badge: the Vim mode in the editor, else the focused pane.
pub(crate) fn badge(app: &App) -> &'static str {
    context(app).unwrap_or_else(|| app.vim.mode_name())
}

/// Keys typed so far that wait for more, in Vim's `showcmd` notation:
/// the engine's (`2d`, `"a`, `g`) or the workbench prefixes (`^W`, `^\`).
pub(crate) fn pending(app: &App) -> String {
    if app.vim_window_prefix {
        return "^W".into();
    }
    if app.vim_terminal_prefix {
        return "^\\".into();
    }
    if let Some(c) = app.vim_explorer_pending {
        return c.to_string();
    }
    if context(app).is_some() {
        return String::new();
    }
    if let Some(waiting) = awaiting(app) {
        return waiting;
    }
    if app.vim.is_visual() && app.vim.pending.is_empty() {
        return selection_size(app);
    }
    app.vim.showcmd()
}

/// Vim's `showcmd` in Visual mode: the selection's size (characters on one
/// line, else lines; rows x columns for a block).
fn selection_size(app: &App) -> String {
    let Some(ed) = app.editor() else {
        return String::new();
    };
    let a = app.vim.visual_anchor;
    let (line, col) = (ed.cursor_line, ed.cursor_col);
    let lines = a.line.abs_diff(line) + 1;
    match app.vim.mode {
        super::Mode::Visual(super::VisualKind::Line) => lines.to_string(),
        super::Mode::Visual(super::VisualKind::Block) => {
            let t = super::text::Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let ts = super::exec::tabstop(ed);
            let (c1, c2) = (t.vcol(a.line, a.col, ts), t.vcol(line, col, ts));
            let width = if app.vim.curswant_eol {
                "$".to_string()
            } else {
                (c1.abs_diff(c2) + 1).to_string()
            };
            format!("{lines}x{width}")
        }
        _ if lines == 1 => (a.col.abs_diff(col) + 1).to_string(),
        _ => lines.to_string(),
    }
}

/// Insert mode and the command line wait for one key after <C-r>, <C-v>
/// and <C-k>.
fn awaiting(app: &App) -> Option<String> {
    use super::insert::Await;
    if app
        .vim
        .cmdline
        .as_ref()
        .is_some_and(|c| c.awaiting_register)
    {
        return Some("^R".into());
    }
    Some(match app.vim.insert.as_ref()?.awaiting? {
        Await::Register => "^R".into(),
        Await::Literal => "^V".into(),
        Await::Digraph(None) => "^K".into(),
        Await::Digraph(Some(c)) => format!("^K{c}"),
    })
}

pub(crate) struct Hint {
    pub(crate) title: &'static str,
    pub(crate) items: &'static [(&'static str, &'static str)],
}

const WINDOW: Hint = Hint {
    title: "window",
    items: &[
        ("h j k l", "move left / down / up / right"),
        ("w W", "next / previous window"),
        ("p", "last window"),
        ("t b", "top / bottom window"),
        ("s v", "split / split vertically"),
        ("n", "new empty window"),
        ("c q", "close window"),
        ("o", "close all other windows"),
        ("x r", "exchange / rotate"),
        ("H J K L", "move window to the edge"),
        ("= _ |", "equalize / maximize"),
        ("T", "move to its own tab"),
    ],
};

const TERMINAL: Hint = Hint {
    title: "leave",
    items: &[
        ("^N", "back to the editor"),
        ("h j k l", "move to that window"),
        ("w p", "next / last window"),
        ("other", "send ^\\ and the key"),
    ],
};

const G: Hint = Hint {
    title: "g",
    items: &[
        ("g", "first line"),
        ("d D", "definition / declaration"),
        ("y r", "type definition / references"),
        ("h .", "hover / code action"),
        ("t T", "next / previous tab"),
        ("e E", "back to word end"),
        ("j k", "down / up display line"),
        ("0 $ m", "display line start / end / middle"),
        ("_", "last non-blank"),
        ("v", "reselect last Visual"),
        ("i I", "insert at last insert / column 0"),
        ("J", "join without spaces"),
        ("p P", "put, cursor after"),
        ("n N", "select next / previous match"),
        ("* #", "search word, partial"),
        ("u U ~", "lowercase / uppercase / toggle"),
        ("c", "comment"),
        ("q w", "format lines"),
        ("?", "rot13"),
        ("; ,", "older / newer change"),
        ("f x", "open file / URL under cursor"),
        ("a", "character code"),
        ("&", "repeat :s on all lines"),
    ],
};

const Z: Hint = Hint {
    title: "z",
    items: &[
        ("z .", "line to centre (. at first non-blank)"),
        ("t <CR>", "line to top"),
        ("b -", "line to bottom"),
        ("h l", "scroll left / right"),
        ("H L", "half a screen left / right"),
        ("s e", "scroll cursor to left / right edge"),
        ("o O", "open fold / recursively"),
        ("c C", "close fold / recursively"),
        ("a A", "toggle fold / recursively"),
        ("v", "open folds at cursor"),
        ("R M", "open / close all folds"),
        ("f", "create fold (operator)"),
        ("d E", "delete fold / all folds"),
        ("j k", "next / previous fold"),
    ],
};

const BRACKET: Hint = Hint {
    title: "[ ]",
    items: &[
        ("d", "previous / next diagnostic"),
        ("c", "previous / next change"),
        ("p P", "put with indent"),
        ("z", "start / end of fold"),
        ("( {", "unmatched ( or {"),
        ("[ ]", "section"),
        ("<Space>", "add blank line"),
    ],
};

const OPERATOR: Hint = Hint {
    title: "motion or object",
    items: &[
        ("same key", "whole line (dd, yy, cc...)"),
        ("w b e", "word forward / back / end"),
        ("0 ^ $", "line start / first char / end"),
        ("gg G", "first / last line"),
        ("f t F T", "to a character"),
        ("} {", "paragraph"),
        ("/ ?", "search"),
        ("iw aw", "word"),
        ("i\" a\"", "quoted string"),
        ("i( a( i{ i[", "brackets"),
        ("it at", "tag"),
        ("ip ap", "paragraph"),
        ("v V ^V", "force charwise / linewise / block"),
    ],
};

const OBJECT: Hint = Hint {
    title: "text object",
    items: &[
        ("w W", "word / WORD"),
        ("s p", "sentence / paragraph"),
        ("\" ' `", "quoted string"),
        ("( ) b", "parentheses"),
        ("{ } B", "braces"),
        ("[ ]", "brackets"),
        ("< >", "angle brackets"),
        ("t", "tag"),
        ("e", "whole buffer"),
    ],
};

const REGISTER: Hint = Hint {
    title: "register",
    items: &[
        ("a-z", "named (A-Z appends)"),
        ("0-9", "yank / delete history"),
        ("+ *", "system clipboard"),
        ("\"", "unnamed"),
        ("-", "small delete"),
        ("_", "black hole"),
        (". : / %", "last insert / command / search / file name"),
    ],
};

const MARK: Hint = Hint {
    title: "set mark",
    items: &[("a-z", "mark in this file"), ("A-Z", "mark across files")],
};
const JUMP: Hint = Hint {
    title: "jump to mark",
    items: &[
        ("a-z A-Z", "your marks"),
        ("` '", "before the last jump"),
        (".", "last change"),
        ("[ ]", "start / end of last change"),
        ("< >", "start / end of last Visual"),
    ],
};
const RECORD: Hint = Hint {
    title: "record macro",
    items: &[
        ("a-z", "into register (A-Z appends)"),
        ("q", "again to stop"),
    ],
};
const PLAY: Hint = Hint {
    title: "run macro",
    items: &[
        ("a-z", "register"),
        ("@", "the last one"),
        (":", "the last : command"),
    ],
};
const CHAR: Hint = Hint {
    title: "character",
    items: &[("any key", "the character to find")],
};
const REPLACE: Hint = Hint {
    title: "replace",
    items: &[("any key", "replaces the character")],
};
const PASTE: Hint = Hint {
    title: "insert register",
    items: &[
        ("a-z 0-9", "a register's text"),
        ("+ *", "the clipboard"),
        ("\"", "the unnamed register"),
        (". : /", "last insert / command / search"),
        ("^W", "the word under the cursor"),
    ],
};
const LITERAL: Hint = Hint {
    title: "insert literally",
    items: &[("any key", "inserted as typed (Tab, Esc, Ctrl keys)")],
};
const DIGRAPH: Hint = Hint {
    title: "digraph",
    items: &[
        ("two keys", "a special character"),
        ("a: o: u:", "ä ö ü"),
        ("e' e! e>", "é è ê"),
        ("Eu Pd", "€ £"),
        ("-> <-", "→ ←"),
    ],
};
const EXPLORER_G: Hint = Hint {
    title: "g",
    items: &[("g", "first item")],
};

/// What may follow the keys typed so far, if they form a prefix.
pub(crate) fn hint(app: &App) -> Option<&'static Hint> {
    if app.vim_window_prefix {
        return Some(&WINDOW);
    }
    if app.vim_terminal_prefix {
        return Some(&TERMINAL);
    }
    if app.vim_explorer_pending == Some('g') {
        return Some(&EXPLORER_G);
    }
    if context(app).is_some() {
        return None;
    }
    match awaiting(app).as_deref() {
        Some("^R") => return Some(&PASTE),
        Some("^V") => return Some(&LITERAL),
        Some("^K") => return Some(&DIGRAPH),
        Some(_) => return None,
        None => {}
    }
    if !matches!(app.vim.mode, super::Mode::Normal | super::Mode::Visual(_)) {
        return None;
    }
    engine_hint(&app.vim.pending)
}

fn engine_hint(pending: &[Key]) -> Option<&'static Hint> {
    // Skip a count and a register prefix: `2"a3d` waits like `d`.
    let mut keys = pending;
    let skip_count = |keys: &mut &[Key]| {
        if keys
            .first()
            .and_then(|k| k.char())
            .is_some_and(|c| c.is_ascii_digit() && c != '0')
        {
            while keys
                .first()
                .and_then(|k| k.char())
                .is_some_and(|c| c.is_ascii_digit())
            {
                *keys = &keys[1..];
            }
        }
    };
    skip_count(&mut keys);
    if keys.first().and_then(|k| k.char()) == Some('"') {
        if keys.len() == 1 {
            return Some(&REGISTER);
        }
        keys = &keys[2..];
        skip_count(&mut keys);
    }
    let chars: Vec<Option<char>> = keys
        .iter()
        .map(|k| (!k.ctrl && !k.alt).then(|| k.char()).flatten())
        .collect();
    let operator = |c: &[Option<char>]| match c {
        [Some('d' | 'c' | 'y' | '<' | '>' | '=' | '!')] => true,
        [Some('g'), Some('u' | 'U' | '~' | '?' | 'q' | 'w' | 'c')] => true,
        [Some('z'), Some('f')] => true,
        _ => false,
    };
    if keys.len() == 1 && keys[0].is_ctrl('w') {
        return Some(&WINDOW);
    }
    // After an operator: its own count, then a motion or a text object.
    let mut i = 0;
    while i < chars.len() && !operator(&chars[..=i]) {
        i += 1;
    }
    if i < chars.len() {
        let mut rest = &keys[i + 1..];
        skip_count(&mut rest);
        let rest: Vec<Option<char>> = rest.iter().map(|k| k.char()).collect();
        return match rest.as_slice() {
            [] => Some(&OPERATOR),
            [Some('i' | 'a')] => Some(&OBJECT),
            [Some('f' | 't' | 'F' | 'T')] => Some(&CHAR),
            [Some('g')] => Some(&G),
            [Some('[' | ']')] => Some(&BRACKET),
            [Some('\'' | '`')] => Some(&JUMP),
            _ => None,
        };
    }
    match chars.as_slice() {
        [Some('g')] => Some(&G),
        [Some('z')] => Some(&Z),
        [Some('[' | ']')] => Some(&BRACKET),
        [Some('m')] => Some(&MARK),
        [Some('\'' | '`')] => Some(&JUMP),
        [Some('q')] => Some(&RECORD),
        [Some('@')] => Some(&PLAY),
        [Some('f' | 't' | 'F' | 'T')] => Some(&CHAR),
        [Some('r')] => Some(&REPLACE),
        [Some('i' | 'a')] if keys.len() == 1 => None,
        _ => None,
    }
}

/// Track how long the current prefix has waited (called before each frame).
pub(crate) fn update(app: &mut App) {
    let now = Instant::now();
    let key = pending(app);
    app.vim_hint_since = match app.vim_hint_since.take() {
        Some((k, since)) if k == key => Some((k, since)),
        _ if key.is_empty() => None,
        _ => {
            app.vim_hint_shown = false;
            Some((key, now))
        }
    };
}

/// Timed feedback changes on its own: the hints appear, the flash and the
/// outline end. Returns true when the screen must be redrawn.
pub(crate) fn tick(app: &mut App) -> bool {
    let mut changed = false;
    if app
        .vim_focus_flash
        .is_some_and(|(at, _)| at.elapsed() >= FOCUS_FLASH)
    {
        app.vim_focus_flash = None;
        changed = true;
    }
    if app.vim.bell.is_some_and(|at| at.elapsed() >= BELL) {
        app.vim.bell = None;
        changed = true;
    }
    if !app.vim_hint_shown && hint_ready(app) && hint(app).is_some() {
        app.vim_hint_shown = true;
        changed = true;
    }
    changed
}

/// How long the pane that took the keyboard stays outlined.
pub(crate) const FOCUS_FLASH: Duration = Duration::from_millis(300);

/// Outline the pane that just took the keyboard, so a window move shows
/// where it landed.
pub(crate) fn flash_focus(app: &mut App, area: Rect) {
    app.vim_focus_flash = Some((Instant::now(), area));
}

pub(crate) fn draw_focus_flash(f: &mut Frame, app: &App) {
    let Some((at, area)) = app.vim_focus_flash else {
        return;
    };
    if at.elapsed() >= FOCUS_FLASH || area.width < 2 || area.height < 2 {
        return;
    }
    let area = area.intersection(f.area());
    let color = app.colors.fg_accent_blue;
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(color)),
        area,
    );
}

/// The next time the screen must change on its own: hints appearing or the
/// flash ending.
pub(crate) fn next_deadline(app: &App) -> Option<Instant> {
    let now = Instant::now();
    let hint = app
        .vim_hint_since
        .as_ref()
        .filter(|_| app.vim.opts.which_key && hint(app).is_some())
        .map(|(_, since)| *since + HINT_DELAY)
        .filter(|at| *at > now);
    let bell = app.vim.bell.map(|at| at + BELL).filter(|at| *at > now);
    let flash = app
        .vim_focus_flash
        .map(|(at, _)| at + FOCUS_FLASH)
        .filter(|at| *at > now);
    [hint, bell, flash].into_iter().flatten().min()
}

fn hint_ready(app: &App) -> bool {
    app.vim.opts.which_key
        && app
            .vim_hint_since
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() >= HINT_DELAY)
}

pub(crate) fn bell_active(app: &App) -> bool {
    app.vim.bell.is_some_and(|at| at.elapsed() < BELL)
}

/// The hint popup, above the right end of the status bar.
pub(crate) fn draw_hint(f: &mut Frame, app: &App) {
    if !hint_ready(app) {
        return;
    }
    let (Some(hint), Some(status)) = (hint(app), app.status_bar_area) else {
        return;
    };
    let screen = f.area();
    let keys_w = hint
        .items
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(1);
    let desc_w = hint
        .items
        .iter()
        .map(|(_, d)| d.chars().count())
        .max()
        .unwrap_or(1);
    let item_w = keys_w + 2 + desc_w;
    let room = status.y.saturating_sub(screen.y + 1) as usize;
    if room < 3 || screen.width < 20 {
        return;
    }
    // Two columns when the list is long and there is width for it.
    let cols = if hint.items.len() > room.saturating_sub(2) || hint.items.len() > 8 {
        2
    } else {
        1
    };
    let cols = if (item_w * cols + 3 * cols + 2) as u16 > screen.width {
        1
    } else {
        cols
    };
    let rows = hint.items.len().div_ceil(cols).min(room.saturating_sub(2));
    let width = ((item_w + 3) * cols + 1).min(screen.width as usize) as u16;
    let height = rows as u16 + 2;
    let rect = Rect::new(
        screen.right().saturating_sub(width + 1),
        status.y.saturating_sub(height),
        width,
        height,
    );
    let bg = app.colors.ui.bg_widget;
    let fg = app
        .colors
        .ui
        .readable_foreground(app.colors.fg_statusbar, bg, 4.5);
    let key_fg = app
        .colors
        .ui
        .readable_foreground(app.colors.fg_accent_blue, bg, 4.5);
    let dim = app
        .colors
        .ui
        .readable_foreground(app.colors.fg_dim, bg, 3.0);
    let title = format!(" {} {} ", pending(app), hint.title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dim).bg(bg))
        .title(Span::styled(
            title,
            Style::default()
                .fg(key_fg)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(bg));
    let lines: Vec<Line> = (0..rows)
        .map(|row| {
            let mut spans = Vec::new();
            for col in 0..cols {
                let Some((k, d)) = hint.items.get(col * rows + row) else {
                    break;
                };
                spans.push(Span::styled(
                    format!(" {k:<keys_w$}"),
                    Style::default()
                        .fg(key_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!("  {d:<desc_w$} "),
                    Style::default().fg(fg).bg(bg),
                ));
                if col + 1 < cols {
                    spans.push(Span::styled(" ", Style::default().bg(bg)));
                }
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Badge colour for a pane that is not the Vim editor.
pub(crate) fn context_color(app: &App) -> Color {
    app.colors.fg_info_label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<Key> {
        super::super::keys::parse(s, &[])
    }

    #[test]
    fn prefixes_have_hints_and_complete_commands_do_not() {
        let title = |s: &str| engine_hint(&keys(s)).map(|h| h.title);
        assert_eq!(title("g"), Some("g"));
        assert_eq!(title("3z"), Some("z"));
        assert_eq!(title("<C-w>"), Some("window"));
        assert_eq!(title("d"), Some("motion or object"));
        assert_eq!(title("2d3"), Some("motion or object"));
        assert_eq!(
            title("\"a"),
            None,
            "a register then nothing is a plain count/command wait"
        );
        assert_eq!(title("\""), Some("register"));
        assert_eq!(title("\"ay"), Some("motion or object"));
        assert_eq!(title("di"), Some("text object"));
        assert_eq!(title("gU"), Some("motion or object"));
        assert_eq!(title("df"), Some("character"));
        assert_eq!(title("m"), Some("set mark"));
        assert_eq!(title("q"), Some("record macro"));
        assert_eq!(title("3"), None);
        assert_eq!(title("x"), None);
    }
}
