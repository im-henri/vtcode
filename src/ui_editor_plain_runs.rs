//! Ordinary, unwrapped text can share its syntax/selection style across a run.
//! Stop before anything requiring the full cell renderer; never retain a style
//! across frames, so settings, worker results and selections take effect at once.
use crate::ui_editor_highlight::LineWhitespace;

#[cfg(test)]
thread_local! {
    pub(super) static REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static CELLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static ASCII_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn enabled() -> bool {
    #[cfg(test)]
    if REFERENCE.get() { return false; }
    true
}

/// One-column text shares style work across a run. Tabs, wide/combining glyphs,
/// regional indicators (ambiguous animation ownership), brackets and visible
/// whitespace still use the existing renderer and its exact rules.
#[allow(clippy::too_many_arguments)]
pub(super) fn end(
    chars: &[char], start: usize, end: usize, colorize_brackets: bool,
    decorated_indent: usize, bracket_guides: bool, selected: bool,
    whitespace: &mut LineWhitespace<'_>, source_spaces: &[bool],
) -> usize {
    #[cfg(test)]
    let unicode = !ASCII_ONLY.get();
    #[cfg(not(test))]
    let unicode = true;
    let mut at = start;
    while at < end {
        let ch = chars[at];
        let ordinary = if ch.is_ascii() { (' '..='~').contains(&ch) } else {
            unicode && unicode_width::UnicodeWidthChar::width(ch) == Some(1)
                && !('\u{1f1e6}'..='\u{1f1ff}').contains(&ch)
        };
        if !ordinary
            || (colorize_brackets && matches!(ch, '(' | ')' | '[' | ']' | '{' | '}'))
        {
            break;
        }
        if ch == ' ' && (at < decorated_indent || bracket_guides || whitespace.shows(
            ch, at, at.checked_sub(1).is_some_and(|prev| chars[prev] == ' ')
                || source_spaces.get(at + 1) == Some(&true), selected,
        )) {
            break;
        }
        at += 1;
    }
    #[cfg(test)]
    CELLS.set(CELLS.get() + at - start);
    at
}
