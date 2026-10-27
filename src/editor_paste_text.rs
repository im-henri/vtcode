//! Large paste normalization/reindentation writes directly into the final Rope.
//! The line iterator borrows the payload, including mixed CRLF/CR/LF input.
use ropey::{Rope, RopeBuilder};

#[derive(Clone)]
struct Lines<'a>(Option<&'a str>);

impl<'a> Iterator for Lines<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<Self::Item> {
        let text = self.0.take()?;
        if let Some(at) = memchr::memchr2(b'\r', b'\n', text.as_bytes()) {
            let skip = 1 + usize::from(
                text.as_bytes()[at] == b'\r' && text.as_bytes().get(at + 1) == Some(&b'\n'),
            );
            self.0 = Some(&text[at + skip..]);
            Some(&text[..at])
        } else {
            Some(text)
        }
    }
}

/// None means the original payload can be inserted directly without a rewrite.
pub(super) fn prepare(text: &str, destination: Option<&str>, line_copy: bool) -> Option<Rope> {
    // Column zero has no destination indentation to align the copied block to.
    let destination = destination.filter(|indent| !indent.is_empty());
    let lines = Lines(Some(text));
    let common = destination.and_then(|_| {
        let mut common: Option<&str> = None;
        for line in lines.clone().filter(|line| !line.trim().is_empty()) {
            let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
            common = Some(match common {
                None => indent,
                Some(previous) => {
                    let equal = previous
                        .bytes()
                        .zip(indent.bytes())
                        .take_while(|(a, b)| a == b)
                        .count();
                    &previous[..equal]
                }
            });
            if common == Some("") {
                break;
            }
        }
        common
    });
    let reindent = common.is_some();
    let trailing = line_copy && !text.ends_with(['\r', '\n']);
    if !reindent && !trailing && !text.contains('\r') {
        return None;
    }
    let mut builder = RopeBuilder::new();
    // Batch short line fragments before asking RopeBuilder to validate UTF-8
    // and count line endings. Scratch stays fixed instead of growing with the
    // clipboard; long contiguous fragments can be appended directly.
    let mut scratch = String::with_capacity(4096.min(text.len()));
    let append = |builder: &mut RopeBuilder, scratch: &mut String, part: &str| {
        if scratch.len() + part.len() > 4096 {
            builder.append(scratch);
            scratch.clear();
        }
        if part.len() >= 4096 {
            builder.append(part);
        } else {
            scratch.push_str(part);
        }
    };
    let strip = common.unwrap_or_default().len();
    for (index, line) in lines.enumerate() {
        if index > 0 {
            append(&mut builder, &mut scratch, "\n");
        }
        if reindent && !line.trim().is_empty() {
            if index > 0 {
                append(&mut builder, &mut scratch, destination.unwrap_or_default());
            }
            append(&mut builder, &mut scratch, &line[strip..]);
        } else {
            append(&mut builder, &mut scratch, line);
        }
    }
    if trailing {
        append(&mut builder, &mut scratch, "\n");
    }
    builder.append(&scratch);
    Some(builder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(text: &str, destination: Option<&str>, line_copy: bool) -> String {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if line_copy {
            return if text.ends_with('\n') {
                text
            } else {
                format!("{text}\n")
            };
        }
        let Some(destination) = destination.filter(|indent| !indent.is_empty()) else {
            return text;
        };
        let mut common: Option<&str> = None;
        for line in text.split('\n').filter(|line| !line.trim().is_empty()) {
            let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
            common = Some(match common {
                None => indent,
                Some(previous) => {
                    let equal = previous
                        .bytes()
                        .zip(indent.bytes())
                        .take_while(|(a, b)| a == b)
                        .count();
                    &previous[..equal]
                }
            });
            if common == Some("") {
                break;
            }
        }
        if common.is_none() {
            return text;
        }
        let strip = common.unwrap().len();
        let mut result = String::new();
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                result.push('\n');
            }
            if line.trim().is_empty() {
                result.push_str(line);
            } else {
                if index > 0 {
                    result.push_str(destination);
                }
                result.push_str(&line[strip..]);
            }
        }
        result
    }

    #[test]
    fn streamed_text_matches_normalize_then_reindent_including_blank_and_mixed_lines() {
        for body in [
            "",
            "café(界)",
            "    café\n        run()\n",
            "\t界\r\n\t\t🙂\r",
            "\r\n\r\n\n\r",
            "  \u{2003}\r\n    a\r\n  b\r\n",
            "\t a\n \tb\n",
        ] {
            for destination in [None, Some(""), Some("  "), Some("\t ")] {
                for line_copy in [false, true] {
                    let destination = if line_copy { None } else { destination };
                    let actual = prepare(body, destination, line_copy)
                        .map_or_else(|| body.to_owned(), |rope| rope.to_string());
                    assert_eq!(
                        actual,
                        reference(body, destination, line_copy),
                        "{body:?}, {destination:?}, {line_copy}"
                    );
                }
            }
        }
        assert!(prepare("a\nb\n", None, false).is_none());
        assert!(prepare("a\nb\n", Some(""), false).is_none());
    }

    #[test]
    fn batches_and_direct_long_fragments_preserve_unicode_and_line_boundaries() {
        for body in [
            "    café(界);\r\n\t \r\n    next(🙂);\n".repeat(1000),
            format!("    {}\r\n    tail\r", "界🙂é".repeat(5000)),
        ] {
            for destination in [None, Some(""), Some("\t "), Some("    ")] {
                let result = prepare(&body, destination, false)
                    .map_or_else(|| body.clone(), |rope| rope.to_string());
                assert_eq!(result, reference(&body, destination, false));
            }
        }
    }
}
