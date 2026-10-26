//! Reuse the parsed prefix above the caret. Ordinary typing replays only the
//! current line; structural observations and source snapshots stay bounded.
use super::VtCompleteSnapshot;
use crate::completion_context::{Analyzer, Context, Event};

const MAX_BYTES: usize = 48 * 1024;
const MAX_OBSERVATIONS: usize = 2048;
const MAX_ENTRIES: usize = 4;

pub(super) struct Observation {
    pub(super) word: String,
    pub(super) context: Context,
    pub(super) callable: bool,
    offset: usize,
}

fn observe(words: &mut Vec<Observation>, event: Event<'_>) {
    match event {
        Event::Word {
            word,
            context,
            offset,
        } => {
            if words.len() >= MAX_OBSERVATIONS {
                words.drain(..MAX_OBSERVATIONS / 4);
            }
            words.push(Observation {
                word: word.to_owned(),
                context,
                callable: false,
                offset,
            });
        }
        Event::Call { offset } => {
            if let Ok(index) = words.binary_search_by_key(&offset, |word| word.offset) {
                words[index].callable = true;
            }
        }
    }
}

struct Entry {
    document: u64,
    line: usize,
    rope: Option<ropey::Rope>,
    analyzer: Analyzer,
    words: Vec<Observation>,
    used: u64,
    query: Option<Query>,
}

struct Query {
    rope: Option<ropey::Rope>,
    start: usize,
    context: Context,
    words: Vec<Observation>,
}

#[derive(Default)]
pub(super) struct Cache {
    entries: Vec<Entry>,
    clock: u64,
    #[cfg(test)]
    pub(super) builds: usize,
    #[cfg(test)]
    pub(super) parsed_bytes: usize,
}

pub(super) struct Analysis<'a> {
    pub(super) context: Context,
    pub(super) prior: &'a [Observation],
    pub(super) current: &'a [Observation],
}

impl Cache {
    pub(super) fn analyze(
        &mut self,
        snapshot: &VtCompleteSnapshot,
        prefix: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Option<Analysis<'_>> {
        self.clock = self.clock.wrapping_add(1);
        let retain = snapshot.rope.len_bytes() <= super::MAX_DOCUMENT_BYTES
            && snapshot.rope.len_lines() <= super::MAX_DOCUMENT_LINES;
        if !retain {
            self.entries
                .retain(|entry| entry.document != snapshot.document_id);
        }
        let mut found = None;
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.document == snapshot.document_id && entry.line == snapshot.cursor_line {
                let Some(previous) = entry.rope.as_ref() else {
                    continue;
                };
                let unchanged = if previous.is_instance(&snapshot.rope) {
                    true
                } else {
                    let (old, new) =
                        crate::rope_changes::changed_lines(previous, &snapshot.rope, cancelled)?;
                    (old.is_empty() && new.is_empty())
                        || (old.start >= entry.line && new.start >= entry.line)
                };
                if unchanged {
                    found = Some(index);
                    break;
                }
            }
        }
        let index = if let Some(index) = found {
            index
        } else {
            if cancelled() {
                return None;
            }
            let mut first = snapshot.cursor_line;
            let mut bytes = 0;
            while first > snapshot.cursor_line.saturating_sub(128) {
                let line_bytes = snapshot.rope.line(first - 1).len_bytes();
                if line_bytes > 4096 || bytes + line_bytes > MAX_BYTES {
                    break;
                }
                first -= 1;
                bytes += line_bytes;
            }
            let mut analyzer = Analyzer::at(snapshot.rope.line_to_char(first));
            let mut words = Vec::new();
            for line in first..snapshot.cursor_line {
                if cancelled() {
                    return None;
                }
                let text = snapshot.rope.line(line).to_string();
                analyzer.feed(&text, |event| observe(&mut words, event));
                analyzer.flush(|event| observe(&mut words, event));
            }
            #[cfg(test)]
            {
                self.builds += 1;
                self.parsed_bytes += bytes;
            }
            if self.entries.len() >= MAX_ENTRIES {
                let oldest = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(i, _)| i)
                    .unwrap();
                self.entries.swap_remove(oldest);
            }
            self.entries.push(Entry {
                document: snapshot.document_id,
                line: snapshot.cursor_line,
                rope: retain.then(|| snapshot.rope.clone()),
                analyzer,
                words,
                used: self.clock,
                query: None,
            });
            self.entries.len() - 1
        };
        let entry = &mut self.entries[index];
        entry.used = self.clock;
        entry.rope = retain.then(|| snapshot.rope.clone());
        let source = snapshot.rope.line(snapshot.cursor_line);
        let cursor = snapshot.cursor_col.min(source.len_chars());
        let start = if source.len_bytes() > 4096 {
            cursor.saturating_sub(2048)
        } else {
            0
        };
        let prefix_start = snapshot.rope.line_to_char(snapshot.cursor_line)
            + cursor.saturating_sub(prefix.chars().count());
        let reuse = if let Some(query) = &entry.query {
            if query.start != prefix_start
                || !retain
                || prefix_start > 0 && snapshot.rope.char(prefix_start - 1) == '#'
            {
                false
            } else if let Some(previous) = &query.rope {
                let (old, new) = crate::rope_changes::changed_chars_cancellable(previous, &snapshot.rope, cancelled)?;
                (old.is_empty() && new.is_empty())
                    || (old.start >= prefix_start && new.start >= prefix_start)
            } else {
                false
            }
        } else {
            false
        };
        if !reuse {
            let mut analyzer = if start == 0 {
                entry.analyzer.clone()
            } else {
                Analyzer::at(snapshot.rope.line_to_char(snapshot.cursor_line) + start)
            };
            let mut current = Vec::new();
            let text = source.slice(start..cursor).to_string();
            #[cfg(test)]
            {
                self.parsed_bytes += text.len();
            }
            analyzer.feed(&text, |event| observe(&mut current, event));
            let context = analyzer.query(prefix_start, prefix.is_empty(), |event| {
                observe(&mut current, event)
            });
            current.retain(|word| word.offset < prefix_start);
            entry.query = Some(Query {
                rope: retain.then(|| snapshot.rope.clone()),
                start: prefix_start,
                context,
                words: current,
            });
        }
        if cancelled() {
            return None;
        }
        let query = entry.query.as_mut().unwrap();
        query.rope = retain.then(|| snapshot.rope.clone());
        Some(Analysis {
            context: query.context,
            prior: &entry.words,
            current: &query.words,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_buffers_do_not_leave_retained_source_snapshots() {
        let mut cache = Cache::default();
        let rope = ropey::Rope::from_str(
            &("x".repeat(super::super::MAX_DOCUMENT_BYTES + 1) + "\nclient.re"),
        );
        let snapshot = VtCompleteSnapshot {
            source: Default::default(),
            rope,
            cursor_line: 1,
            cursor_col: 9,
            document_id: 1,
            active: true,
        };
        cache.analyze(&snapshot, "re", &|| false).unwrap();
        assert!(
            cache
                .entries
                .iter()
                .all(|entry| entry.rope.is_none() && entry.query.as_ref().unwrap().rope.is_none())
        );
    }
}
