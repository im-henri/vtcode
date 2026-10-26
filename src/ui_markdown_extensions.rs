//! Normalize a few widely used block extensions before CommonMark parsing.
//! Each emitted line retains its original line number for synchronized scrolling.
use std::borrow::Cow;

pub(super) fn prepare(source: &str) -> (Cow<'_, str>, Vec<usize>) {
    if !["!!!", "???", ":::", "\\(", "\\["]
        .iter()
        .any(|marker| source.contains(marker))
    {
        return (Cow::Borrowed(source), vec![]);
    }
    let lines: Vec<_> = source.lines().collect();
    // Only extension-bearing documents take this extra, lightweight block pass.
    // It protects fences/indented code nested inside lists and blockquotes too.
    let starts: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut code_lines = vec![false; lines.len()];
    for (event, range) in pulldown_cmark::Parser::new(source).into_offset_iter() {
        if matches!(
            event,
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(_))
        ) {
            let first = starts
                .partition_point(|&i| i <= range.start)
                .saturating_sub(1);
            let last = starts
                .partition_point(|&i| i < range.end)
                .min(code_lines.len());
            code_lines[first..last].fill(true);
        }
    }
    let mut out = String::with_capacity(source.len());
    let mut origins = Vec::with_capacity(lines.len());
    let mut fence = None;
    let mut i = 0;
    let mut display_end = None;
    let mut metadata = None;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if i == 0 && matches!(trimmed, "---" | "+++") {
            metadata = lines[1..]
                .iter()
                .position(|line| line.trim() == trimmed)
                .map(|end| end + 1);
        }
        if metadata.is_some_and(|end| i <= end) {
            emit(&mut out, &mut origins, line, i);
            i += 1;
            continue;
        }
        if code_lines[i]
            || in_fence(line, &mut fence)
            || line.starts_with("    ")
            || line.starts_with('\t')
        {
            emit(&mut out, &mut origins, line, i);
            i += 1;
            continue;
        }
        if let Some((kind, title, container)) = admonition(line) {
            let mut end = i + 1;
            let mut nested_fence = None;
            if container {
                while end < lines.len() {
                    if !in_fence(lines[end], &mut nested_fence)
                        && lines[end].trim().starts_with(":::")
                    {
                        break;
                    }
                    end += 1;
                }
                if end == lines.len() || lines[end].trim() != ":::" {
                    emit(&mut out, &mut origins, line, i);
                    i += 1;
                    continue;
                }
            } else {
                while end < lines.len()
                    && (lines[end].trim().is_empty()
                        || lines[end].starts_with("    ")
                        || lines[end].starts_with('\t'))
                {
                    end += 1;
                }
                if end == i + 1 {
                    emit(&mut out, &mut origins, line, i);
                    i += 1;
                    continue;
                }
            }
            emit(&mut out, &mut origins, &format!("> [!{kind}]"), i);
            if !title.is_empty() {
                emit(&mut out, &mut origins, &format!("> **{title}**"), i);
                emit(&mut out, &mut origins, ">", i);
            }
            for (line_number, body) in lines.iter().enumerate().take(end).skip(i + 1) {
                let body = if container {
                    body
                } else {
                    body.strip_prefix("    ")
                        .or_else(|| body.strip_prefix('\t'))
                        .unwrap_or(body)
                };
                let text = if in_fence(body, &mut nested_fence)
                    || body.starts_with("    ")
                    || body.starts_with('\t')
                {
                    body.to_string()
                } else {
                    inline_math(body)
                };
                emit(&mut out, &mut origins, &format!("> {text}"), line_number);
            }
            if container {
                emit(&mut out, &mut origins, "", end);
            } else {
                emit(&mut out, &mut origins, "", end.saturating_sub(1));
            }
            i = end + usize::from(container);
            continue;
        }
        if trimmed == "\\[" && display_end.is_none() {
            display_end = lines[i + 1..]
                .iter()
                .position(|line| line.trim() == "\\]")
                .map(|end| end + i + 1);
            if display_end.is_some() {
                emit(&mut out, &mut origins, "$$", i);
                i += 1;
                continue;
            }
        }
        if display_end == Some(i) {
            emit(&mut out, &mut origins, "$$", i);
            display_end = None;
        } else {
            emit(&mut out, &mut origins, &inline_math(line), i);
        }
        i += 1;
    }
    (Cow::Owned(out), origins)
}

fn emit(out: &mut String, origins: &mut Vec<usize>, text: &str, line: usize) {
    out.push_str(text);
    out.push('\n');
    origins.push(line);
}

fn in_fence(line: &str, fence: &mut Option<(u8, usize)>) -> bool {
    let trimmed = line.trim_start();
    let marker = trimmed.as_bytes().first().copied();
    if let Some((ch, length)) = *fence {
        if marker == Some(ch)
            && trimmed.bytes().take_while(|b| *b == ch).count() >= length
            && trimmed.trim_matches(ch as char).trim().is_empty()
        {
            *fence = None;
        }
        return true;
    }
    if matches!(marker, Some(b'`' | b'~')) && line.len() - trimmed.len() <= 3 {
        let ch = marker.unwrap();
        let length = trimmed.bytes().take_while(|b| *b == ch).count();
        if length >= 3 {
            *fence = Some((ch, length));
            return true;
        }
    }
    false
}

fn admonition(line: &str) -> Option<(&'static str, &str, bool)> {
    let (rest, container) = if let Some(rest) = line.strip_prefix(":::") {
        (rest.trim(), true)
    } else {
        (
            line.strip_prefix("!!! ")
                .or_else(|| line.strip_prefix("??? "))?
                .trim(),
            false,
        )
    };
    let end = rest
        .find(|ch: char| ch.is_whitespace() || ch == '[')
        .unwrap_or(rest.len());
    let kind = match &rest[..end] {
        "note" | "info" => "NOTE",
        "tip" | "hint" | "success" => "TIP",
        "important" => "IMPORTANT",
        "warning" | "warn" => "WARNING",
        "danger" | "caution" | "error" => "CAUTION",
        _ => return None,
    };
    let title = rest[end..].trim().trim_matches(['"', '[', ']']);
    Some((kind, title, container))
}

fn inline_math(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        if rest.starts_with("\\\\") {
            out.push_str("\\\\");
            rest = &rest[2..];
            continue;
        }
        if rest.starts_with('`') {
            let count = rest.bytes().take_while(|b| *b == b'`').count();
            let marker = &rest[..count];
            let end = rest[count..]
                .find(marker)
                .map_or(rest.len(), |i| count + i + count);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if let Some(body) = rest.strip_prefix("\\(")
            && let Some(end) = body.find("\\)")
        {
            out.push('$');
            out.push_str(body[..end].trim());
            out.push('$');
            rest = &body[end + 2..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_admonitions_and_math_without_touching_fenced_examples() {
        let source = "!!! warning \"Careful\"\n    **Body**\n\n:::tip[Advice]\nUse \\(x^2\\).\n:::\n\n```markdown\n!!! note\n\\(literal\\)\n```\n`\\(code\\)`";
        let (text, origins) = prepare(source);
        assert!(text.contains("> [!WARNING]\n> **Careful**\n>\n> **Body**"));
        assert!(text.contains("> [!TIP]\n> **Advice**\n>\n> Use $x^2$."));
        assert!(text.contains("```markdown\n!!! note\n\\(literal\\)\n```"));
        assert!(text.contains("`\\(code\\)`"));
        assert_eq!(origins[1], 0);
        assert_eq!(origins[3], 1);
    }
    #[test]
    fn leaves_incomplete_extensions_and_metadata_readable() {
        let source = "---\ntitle: '\\(not math\\)'\n---\n:::note\nUnclosed\n\\[\nUnclosed math";
        let (text, _) = prepare(source);
        assert_eq!(text.trim_end(), source);
    }
}
