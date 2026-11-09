//! Inline workspace replacement previews use the same expansion rules as edits.
use crate::{
    app::{App, GlobalSearchResult},
    app_theme::ThemeColors,
    search_regex::{SearchRegex, SearchText},
    search_replace::{WorkspaceReplacement, preserve_case},
};
use ratatui::{style::{Modifier, Style}, text::Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) struct Preview<'a> {
    replacement: Option<&'a str>,
    pattern: Option<WorkspaceReplacement>,
    regex: Option<std::sync::Arc<SearchRegex>>,
    preserve: bool,
}

impl<'a> Preview<'a> {
    pub(crate) fn new(app: &'a App) -> Self {
        let replacement = (app.global_search_replace_visible
            && !app.global_search_replace_query.is_empty()
            && app.global_search_error.is_none()).then_some(app.global_search_replace_query.as_str());
        let pattern = replacement.filter(|_| app.global_search_regex).map(WorkspaceReplacement::new);
        let regex = pattern.as_ref().filter(|pattern| pattern.needs_captures()).and_then(|_|
            SearchRegex::cached_workspace(&app.global_search_query, app.global_search_case_sensitive,
                app.global_search_whole_word, true).ok());
        Self { replacement, pattern, regex, preserve: app.global_search_preserve_case }
    }

    fn replacement(&self, matched: &str, context: &str, start: usize) -> Option<String> {
        let replacement = self.replacement?;
        let matched = SearchText::new(matched, true);
        if let Some(pattern) = &self.pattern {
            if pattern.needs_captures() {
                let regex = self.regex.as_ref()?;
                let input = SearchText::new(context, true);
                Some(pattern.expand_match(regex, &matched.text, &input.text,
                    input.normalized_offset(start), self.preserve))
            } else if self.preserve {
                Some(preserve_case(&matched.text, pattern.fallback()))
            } else {
                Some(pattern.fallback().to_owned())
            }
        } else if self.preserve {
            Some(preserve_case(&matched.text, replacement))
        } else {
            Some(replacement.to_owned())
        }
    }

    pub(crate) fn spans(&self, result: &GlobalSearchResult, width: usize,
        row: Style, colors: &ThemeColors) -> Vec<Span<'static>>
    {
        if width == 0 { return Vec::new(); }
        let source = &*result.line_text;
        let byte = |col| source.char_indices().nth(col).map_or(source.len(), |(byte, _)| byte);
        let start = byte(result.col_start);
        let end = byte(result.col_end).max(start);
        // Whitespace can itself be the match; only trim indentation before it.
        let indent = source.chars().take_while(|ch| ch.is_whitespace()).count().min(result.col_start);
        let before = display_text(&source[byte(indent)..start]);
        let after = display_text(&source[end..]);
        let replacement = self.replacement(&source[start..end], source, start).map(|text| display_text(&text));
        let removed = if start == end && replacement.is_none() { "│".into() }
            else { display_text(&source[start..end]) };
        let added = replacement.as_deref().unwrap_or("");
        let old_width = UnicodeWidthStr::width(removed.as_str());
        let new_width = UnicodeWidthStr::width(added);
        let (old_budget, new_budget) = if old_width.saturating_add(new_width) <= width {
            (old_width, new_width)
        } else if new_width == 0 {
            (width, 0)
        } else {
            let old_budget = old_width.min(width / 2);
            let new_budget = new_width.min(width - old_budget);
            (old_width.min(width - new_budget), new_budget)
        };
        let context_budget = width.saturating_sub(old_budget + new_budget);
        let before_budget = UnicodeWidthStr::width(before.as_str()).min(context_budget.min(width / 3));
        let (before, before_width) = clip(&before, before_budget, true);
        let (after, _) = clip(&after, context_budget - before_width, false);
        let (removed, _) = clip(&removed, old_budget, false);
        let (added, _) = clip(added, new_budget, false);
        let mut spans = vec![Span::styled(before, row)];
        if replacement.is_some() {
            spans.push(Span::styled(removed, row
                .fg(colors.fg_diff_removed).bg(colors.bg_diff_removed_highlight)
                .add_modifier(Modifier::CROSSED_OUT)));
            spans.push(Span::styled(added, row
                .fg(colors.fg_diff_added).bg(colors.bg_diff_added_highlight)
                .remove_modifier(Modifier::CROSSED_OUT)));
        } else {
            spans.push(Span::styled(removed, colors.ui.search_match_style(row)));
        }
        spans.push(Span::styled(after, row));
        spans
    }
}

fn display_text(text: &str) -> String {
    text.chars().map(|ch| match ch {
        '\n' => '↵', '\r' => ' ', '\t' => '→',
        ch if ch.is_control() => '�', ch => ch,
    }).collect()
}

/// Clip by terminal cells while retaining complete Unicode graphemes.
fn clip(text: &str, width: usize, tail: bool) -> (String, usize) {
    let text_width = UnicodeWidthStr::width(text);
    if text_width <= width { return (text.to_owned(), text_width); }
    if width == 0 { return (String::new(), 0); }
    let mut used = 1;
    let mut pieces = Vec::new();
    let mut take = |part: &str| {
        let next = UnicodeWidthStr::width(part);
        if used + next > width { return false; }
        used += next;
        pieces.push(part.to_owned());
        true
    };
    if tail {
        for part in text.graphemes(true).rev() { if !take(part) { break; } }
        pieces.reverse();
        (format!("…{}", pieces.concat()), used)
    } else {
        for part in text.graphemes(true) { if !take(part) { break; } }
        (format!("{}…", pieces.concat()), used)
    }
}

#[cfg(test)]
#[path = "ui_search_preview_tests.rs"]
mod tests;
