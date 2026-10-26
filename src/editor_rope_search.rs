//! Literal occurrence search over borrowed Rope chunks. Search complete chunks
//! with memmem; a prefix automaton carries matches across chunk boundaries.
use memchr::memmem::Finder;
use ropey::Rope;

pub(crate) struct RopeSearch<'a> {
    finder: Finder<'a>,
    fallback: Vec<usize>,
    chars: usize,
}

impl<'a> RopeSearch<'a> {
    pub(crate) fn new(needle: &'a str) -> Self {
        let bytes = needle.as_bytes();
        let mut fallback = vec![0; bytes.len()];
        let mut matched = 0;
        for index in 1..bytes.len() {
            while matched > 0 && bytes[index] != bytes[matched] {
                matched = fallback[matched - 1];
            }
            if bytes[index] == bytes[matched] {
                matched += 1;
            }
            fallback[index] = matched;
        }
        Self {
            finder: Finder::new(bytes),
            fallback,
            chars: needle.chars().count(),
        }
    }

    pub(crate) fn char_len(&self) -> usize {
        self.chars
    }

    /// Non-overlapping, character-indexed matches in the exact requested range.
    /// Neither the document nor a boundary overlap buffer is copied. Chunk
    /// interiors and boundary suffixes keep total work linear, including long,
    /// nearly matching patterns.
    pub(crate) fn matches<'s, 'r>(
        &'s self,
        rope: &'r Rope,
        start: usize,
        end: usize,
    ) -> impl Iterator<Item = usize> + use<'a, 's, 'r> {
        let valid =
            self.chars > 0 && start < end && end <= rope.len_chars() && end - start >= self.chars;
        let slice = if valid {
            rope.slice(start..end)
        } else {
            rope.slice(..0)
        };
        let mut chunks = slice.chunks();
        let needle = self.finder.needle();
        let mut chunk = "";
        let mut offset = 0;
        let mut matched = 0;
        let mut chars = start;
        let mut counted = 0;
        let mut done = !valid;
        std::iter::from_fn(move || {
            loop {
                if done {
                    return None;
                }
                if offset == chunk.len() {
                    // All Rope chunks and complete matches end on UTF-8 boundaries.
                    // Count each scalar only once, without per-match Rope tree seeks.
                    chars += chunk[counted..].chars().count();
                    let Some(next) = chunks.next() else {
                        done = true;
                        return None;
                    };
                    chunk = next;
                    offset = 0;
                    counted = 0;
                }
                let bytes = chunk.as_bytes();
                if matched == 0 && bytes.len() - offset >= needle.len() {
                    if let Some(found) = self.finder.find(&bytes[offset..]) {
                        offset += found + needle.len();
                        chars += chunk[counted..offset].chars().count();
                        counted = offset;
                        return Some(chars - self.chars);
                    }
                    // No complete match remains in this chunk. Only its last
                    // needle.len()-1 bytes could begin a cross-chunk match.
                    offset = bytes.len() - (needle.len() - 1);
                }
                // Finish a partial match, or discover the chunk's matching suffix.
                // KMP fallback preserves linear work for repeated-prefix patterns.
                while offset < bytes.len() {
                    let byte = bytes[offset];
                    while matched > 0 && byte != needle[matched] {
                        matched = self.fallback[matched - 1];
                    }
                    if byte == needle[matched] {
                        matched += 1;
                    }
                    offset += 1;
                    if matched == needle.len() {
                        matched = 0;
                        chars += chunk[counted..offset].chars().count();
                        counted = offset;
                        return Some(chars - self.chars);
                    }
                    if matched == 0 {
                        break;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(rope: &Rope, needle: &str, start: usize, end: usize) {
        let search = RopeSearch::new(needle);
        let expected = if needle.is_empty() || start >= end || end > rope.len_chars() {
            Vec::new()
        } else {
            let text = rope.slice(start..end).to_string();
            text.match_indices(needle)
                .map(|(byte, _)| start + text[..byte].chars().count())
                .collect()
        };
        assert_eq!(
            search.matches(rope, start, end).collect::<Vec<_>>(),
            expected,
            "needle={needle:?}, range={start}..{end}"
        );
        let mut exhausted = search.matches(rope, start, end);
        while exhausted.next().is_some() {}
        assert_eq!(exhausted.next(), None);
        assert_eq!(exhausted.next(), None);
    }

    #[test]
    fn occurrence_chunks_match_contiguous_unicode_oracle() {
        let text = "ababa 世界🙂 é\r\n\tααaaab\r".repeat(180);
        let mut rope = Rope::from_str(&text);
        // Edit the Rope first, so chunks need not follow the builder's layout.
        rope.insert(403, "🙂longer_chunk_boundary");
        rope.remove(100..105);
        let chars: Vec<_> = rope.chars().collect();
        let mut boundary = 0;
        for chunk in rope.chunks() {
            boundary += chunk.chars().count();
            if boundary == rope.len_chars() {
                break;
            }
            for back in [1, 2, 4, 9, 19, 400] {
                for after in [1, 3, 17, 700, 1400] {
                    let first = boundary.saturating_sub(back);
                    let end = (boundary + after).min(chars.len());
                    let needle: String = chars[first..end].iter().collect();
                    check(&rope, &needle, 0, chars.len());
                    check(&rope, &needle, first, end);
                    check(&rope, &needle, first + 1, end);
                }
            }
        }
        for needle in ["", "a", "🙂", "世界", "aba", "\r\n", "é", "missing"] {
            for start in [0, 1, 3, 700, chars.len(), chars.len() + 1, usize::MAX] {
                for end in [0, 7, 1000, chars.len(), chars.len() + 1] {
                    check(&rope, needle, start, end);
                }
            }
        }
    }

    #[test]
    fn occurrence_search_preserves_fallback_and_non_overlapping_matches() {
        for (unit, other) in [("a", "b"), ("🙂", "界"), ("aba", "c")] {
            let rope = Rope::from_str(&format!(
                "{}{other}{}{other}",
                unit.repeat(5000),
                unit.repeat(5000)
            ));
            for count in [1, 2, 31, 255, 1024, 3000] {
                for needle in [
                    unit.repeat(count),
                    format!("{}{other}", unit.repeat(count)),
                    format!("{}!", unit.repeat(count)),
                ] {
                    check(&rope, &needle, 0, rope.len_chars());
                    check(&rope, &needle, 13, rope.len_chars() - 5);
                }
            }
        }
    }

    #[test]
    fn occurrence_search_matches_oracle_after_random_unicode_edits() {
        let alphabet = ['a', 'b', 'c', '\r', '\n', '\t', '界', '🙂', '\u{301}'];
        let mut seed = 0xd73ea9b4_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as usize
        };
        let mut rope = Rope::from_str(&"ab🙂界\r\n".repeat(500));
        for _ in 0..300 {
            let at = next() % (rope.len_chars() + 1);
            if next() % 3 == 0 && at < rope.len_chars() {
                rope.remove(at..(at + next() % 20).min(rope.len_chars()));
            } else {
                rope.insert_char(at, alphabet[next() % alphabet.len()]);
            }
            let start = next() % (rope.len_chars() + 1);
            let end = (start + next() % 1500).min(rope.len_chars());
            let needle = if next() % 2 == 0 {
                rope.slice(start..(start + next() % 600).min(rope.len_chars()))
                    .to_string()
            } else {
                (0..next() % 10)
                    .map(|_| alphabet[next() % alphabet.len()])
                    .collect()
            };
            check(&rope, &needle, 0, rope.len_chars());
            check(&rope, &needle, start, end);
        }
    }
}
