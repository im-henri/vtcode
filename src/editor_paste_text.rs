//! Large paste line-ending normalization writes directly into the final Rope.
//! The line iterator borrows the payload, including mixed CRLF/CR/LF input.
use ropey::{Rope, RopeBuilder};

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
pub(super) fn prepare(text: &str, line_copy: bool) -> Option<Rope> {
    let lines = Lines(Some(text));
    let trailing = line_copy && !text.ends_with(['\r', '\n']);
    if !trailing && !text.contains('\r') {
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
    for (index, line) in lines.enumerate() {
        if index > 0 {
            append(&mut builder, &mut scratch, "\n");
        }
        append(&mut builder, &mut scratch, line);
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

    fn reference(text: &str, line_copy: bool) -> String {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if line_copy {
            return if text.ends_with('\n') {
                text
            } else {
                format!("{text}\n")
            };
        }
        text
    }

    #[test]
    fn streamed_text_preserves_whitespace_while_normalizing_line_endings() {
        for body in [
            "",
            "café(界)",
            "    café\n        run()\n",
            "\t界\r\n\t\t🙂\r",
            "\r\n\r\n\n\r",
            "  \u{2003}\r\n    a\r\n  b\r\n",
            "\t a\n \tb\n",
        ] {
            for line_copy in [false, true] {
                let actual = prepare(body, line_copy)
                    .map_or_else(|| body.to_owned(), |rope| rope.to_string());
                assert_eq!(actual, reference(body, line_copy), "{body:?}, {line_copy}");
            }
        }
        assert!(prepare("a\nb\n", false).is_none());
        assert!(prepare("    first\n\t second\n", false).is_none());
    }

    #[test]
    fn batches_and_direct_long_fragments_preserve_unicode_and_line_boundaries() {
        for body in [
            "    café(界);\r\n\t \r\n    next(🙂);\n".repeat(1000),
            format!("    {}\r\n    tail\r", "界🙂é".repeat(5000)),
        ] {
            for line_copy in [false, true] {
                let result = prepare(&body, line_copy)
                    .map_or_else(|| body.clone(), |rope| rope.to_string());
                assert_eq!(result, reference(&body, line_copy));
            }
        }
    }
}
