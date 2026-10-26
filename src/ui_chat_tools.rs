//! Human-readable tool results. Protocol envelopes never become chat prose.
use super::*;
use serde_json::Value;
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

pub(super) fn decoded(value: &Value) -> Cow<'_, Value> {
    value
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .filter(|v: &Value| v.is_object() || v.is_array())
        .map_or(Cow::Borrowed(value), Cow::Owned)
}

pub(super) fn string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
}

/// Text results can be strings, nested envelopes, or ACP/MCP content arrays.
pub(super) fn output_text(value: &Value) -> Option<String> {
    fn extract(value: &Value, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match value {
            Value::String(text) => Some(text.clone()),
            Value::Array(values) => {
                let parts: Vec<_> = values
                    .iter()
                    .filter_map(|v| extract(v, depth + 1))
                    .collect();
                (!parts.is_empty()).then(|| parts.join("\n"))
            }
            Value::Object(_) => {
                let mut parts = Vec::new();
                for key in ["content", "text", "output", "result", "message", "error"] {
                    if let Some(text) = value.get(key).and_then(|v| extract(v, depth + 1))
                        && !text.is_empty()
                        && !parts.contains(&text)
                    {
                        parts.push(text);
                    }
                }
                (!parts.is_empty()).then(|| parts.join("\n"))
            }
            _ => None,
        }
    }
    extract(&decoded(value), 0)
}

fn label(key: &str) -> String {
    let mut text = String::new();
    for ch in key.chars() {
        if ch == '_' || ch == '-' {
            text.push(' ');
        } else {
            if ch.is_uppercase() && !text.is_empty() {
                text.push(' ');
            }
            if text.is_empty() {
                text.extend(ch.to_uppercase());
            } else {
                text.extend(ch.to_lowercase());
            }
        }
    }
    text
}

/// Retain unfamiliar fields as labeled values, without JSON punctuation or
/// escaped newlines. Keep nesting bounded even for unusual provider payloads.
pub(super) fn readable(value: &Value) -> String {
    fn visit(value: &Value, depth: usize, lines: &mut Vec<String>) {
        if depth > 8 {
            lines.push("…".into());
            return;
        }
        match value {
            Value::Null => {}
            Value::String(text) => lines.push(text.clone()),
            Value::Bool(value) => lines.push(if *value { "Yes" } else { "No" }.into()),
            Value::Number(value) => lines.push(value.to_string()),
            Value::Array(values) => {
                for value in values {
                    visit(value, depth + 1, lines);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    let mut detail = Vec::new();
                    visit(value, depth + 1, &mut detail);
                    if !detail.is_empty() {
                        lines.push(format!("{}: {}", label(key), detail.join("\n")));
                    }
                }
            }
        }
    }
    let mut lines = Vec::new();
    visit(&decoded(value), 0, &mut lines);
    lines.join("\n")
}

fn literal(
    rows: &mut Vec<ChatRow>,
    text: &str,
    width: usize,
    c: &ThemeColors,
    limit: usize,
    search: bool,
) {
    let first = rows.len();
    let end = first.saturating_add(limit);
    let mut more = false;
    for line in clean_terminal(text).lines() {
        if rows.len() >= end {
            more = true;
            break;
        }
        let start = rows.len();
        wrapped(rows, line, width, c.ui.fg_sidebar, None);
        let action = if search {
            search_location(line).map(|(path, line)| ChatAction::OpenFile(path, Some(line)))
        } else {
            None
        };
        for row in &mut rows[start..] {
            row.action = action.clone();
        }
        if rows.len() > end {
            more = true;
            break;
        }
    }
    rows.truncate(end.min(rows.len()));
    if more {
        plain(rows, "…".into(), c.fg_dim, None);
    }
}

fn code(
    rows: &mut Vec<ChatRow>,
    text: &str,
    path: Option<&Path>,
    width: usize,
    c: &ThemeColors,
    limit: usize,
    syntax: ChatSyntax<'_>,
) {
    // Fence the source literally, including files containing Markdown fences.
    let text = clean_terminal(text);
    let clipped = text.lines().take(limit).collect::<Vec<_>>().join("\n");
    let longest = clipped
        .split(|ch| ch != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.saturating_add(1).max(3));
    let language = path
        .and_then(Path::extension)
        .and_then(|s| s.to_str())
        .filter(|s| s.chars().all(char::is_alphanumeric))
        .unwrap_or("");
    markdown(
        rows,
        &format!("{fence}{language}\n{clipped}\n{fence}"),
        width,
        c,
        limit.saturating_add(4),
        syntax,
    );
    if text.lines().count() > limit {
        plain(rows, "…".into(), c.fg_dim, None);
    }
}

fn search_location(line: &str) -> Option<(PathBuf, u32)> {
    // Skip the drive-letter colon on Windows and recognize rg's path:line: form.
    for (position, _) in line.match_indices(':') {
        let (number, _) = line[position + 1..].split_once(':')?;
        if let Ok(number) = number.parse::<u32>()
            && number > 0
        {
            return Some((PathBuf::from(&line[..position]), number - 1));
        }
    }
    None
}

pub(super) fn location(tool: &p::ToolCall) -> Option<(PathBuf, Option<u32>)> {
    // Schema locations carry the validated cross-platform path. Raw input is
    // intentionally opaque and may still contain the node's original spelling.
    if let Some(location) = tool.locations.first() { return Some((location.path.clone(), location.line)); }
    let input = tool.raw_input.as_ref().map(decoded);
    let path = input.as_deref().and_then(|v| string(v, &["path", "file_path", "filePath"]))
        .map(PathBuf::from);
    if let Some(path) = path {
        let line = tool.locations.iter().find(|l| l.path == path).and_then(|l| l.line)
            .or_else(|| input.as_deref()?.get("view_range")?.as_array()?.first()?.as_u64()?.checked_sub(1)?.try_into().ok());
        Some((path, line))
    } else { tool.locations.first().map(|l| (l.path.clone(), l.line)) }
}

pub(super) fn body(
    rows: &mut Vec<ChatRow>,
    tool: &p::ToolCall,
    width: usize,
    c: &ThemeColors,
    open: bool,
    syntax: ChatSyntax<'_>,
) {
    let input = tool.raw_input.as_ref().map(decoded);
    let input = input.as_deref();
    let path = tool.locations.first().map(|location| location.path.clone()).or_else(|| input
        .and_then(|v| string(v, &["path", "file_path", "filePath"]))
        .map(PathBuf::from));
    let mut locations: Vec<_> = tool
        .locations
        .iter()
        .map(|l| (l.path.clone(), l.line))
        .collect();
    if let Some(path) = &path
        && !locations.iter().any(|l| &l.0 == path)
    {
        locations.push((path.clone(), None));
    }
    for (path, line) in locations.into_iter().filter(|_| open) {
        let label = format!(
            "  ↗ {}{}",
            crate::remote::workspace::display_key(&path).unwrap_or_else(|| path.display().to_string()),
            line.map_or(String::new(), |n| format!(":{}", u64::from(n) + 1))
        );
        let start = rows.len();
        wrapped(rows, &label, width, c.fg_accent_blue, None);
        for row in &mut rows[start..] {
            row.action = Some(ChatAction::OpenFile(path.clone(), line));
        }
    }
    if open && let Some(input) = input {
        if !matches!(tool.kind, p::ToolKind::Read | p::ToolKind::Search | p::ToolKind::Edit)
            || matches!(tool.status, p::ToolCallStatus::Pending | p::ToolCallStatus::InProgress) {
            literal(rows, &readable(input), width, c, usize::MAX, false);
        }
        if let Some(range) = input.get("view_range").and_then(Value::as_array)
            && let [start, end] = range.as_slice()
            && let (Some(start), Some(end)) = (start.as_u64(), end.as_i64())
        {
            plain(
                rows,
                if end < 0 {
                    format!("  Lines {start}–end")
                } else {
                    format!("  Lines {start}–{end}")
                },
                c.fg_dim,
                None,
            );
        }
        if let Some(query) = string(input, &["pattern", "query", "regex"]) {
            wrapped(rows, &format!("  Search: {query}"), width, c.fg_dim, None);
        }
    }
    let limit = if open { usize::MAX } else { 5 };
    let mut blocks = Vec::new();
    for content in &tool.content {
        match content {
            p::ToolCallContent::Content(content) => {
                let text = content_text(&content.content);
                let value = Value::String(text.clone());
                let decoded = decoded(&value);
                // A read tool may legitimately return a JSON source file.
                // Its typed text is source content, not another envelope.
                let text = if tool.kind != p::ToolKind::Read
                    && (decoded.is_object() || decoded.is_array())
                {
                    output_text(&decoded).unwrap_or_else(|| readable(&decoded))
                } else {
                    text
                };
                if !text.trim().is_empty()
                    && !blocks.iter().any(|s: &String| s.trim() == text.trim())
                {
                    blocks.push(text);
                }
            }
            p::ToolCallContent::Diff(diff) => {
                diff_rows(rows, diff, width, c, if open { 200 } else { 6 })
            }
            p::ToolCallContent::Terminal(terminal) => plain(
                rows,
                format!("  › Open agent terminal {}", terminal.terminal_id),
                c.fg_dim,
                Some(ChatAction::RevealTerminal(terminal.terminal_id.to_string())),
            ),
            _ => {}
        }
    }
    // The display content is authoritative. Use raw output only as a fallback,
    // avoiding duplicate results and repeated full-file edit payloads.
    if blocks.is_empty()
        && let Some(value) = &tool.raw_output
    {
        if let Some(text) = output_text(value) {
            blocks.push(text);
        } else if !tool.content.iter().any(|c| {
            matches!(
                c,
                p::ToolCallContent::Diff(_) | p::ToolCallContent::Terminal(_)
            )
        }) {
            blocks.push(readable(value));
        }
    }
    let text = blocks.join("\n");
    if text.trim().is_empty() {
        return;
    }
    match tool.kind {
        p::ToolKind::Read => {
            let start = rows.len();
            code(rows, &text, path.as_deref(), width, c, limit, syntax);
            if let Some((path, line)) = location(tool) {
                for row in &mut rows[start..] {
                    row.action = Some(ChatAction::OpenFile(path.clone(), line));
                }
            }
        }
        p::ToolKind::Search => literal(rows, &text, width, c, limit, true),
        _ => literal(rows, &text, width, c, limit, false),
    }
}

/// Command cards show their command and directory themselves. Keep any other
/// inputs (for example a delegated task prompt or environment) available when
/// expanded, including while a permission request is waiting.
pub(super) fn terminal_input_details(rows: &mut Vec<ChatRow>, tool: &p::ToolCall, width: usize, c: &ThemeColors) {
    let Some(input) = &tool.raw_input else { return; };
    let mut input = decoded(input).into_owned();
    if let Value::Object(fields) = &mut input {
        for key in ["command", "cmd", "commands", "args", "cwd", "workingDirectory", "working_directory"] { fields.remove(key); }
    }
    literal(rows, &readable(&input), width, c, usize::MAX, false);
}
