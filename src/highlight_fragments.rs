//! Diff fragments share document/theme/language keys. A warm row lookup uses
//! borrowed keys and never constructs PathBufs or Strings on the paint thread.
use super::{HighlightSpans, MAX_CACHED_LINES_PER_DOCUMENT, select_syntax, select_theme, span_bytes};
use rustc_hash::FxHashMap;
use std::{
    path::{Path, PathBuf},
    rc::Rc,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Style, Theme, ThemeSet},
    parsing::{SyntaxReference, SyntaxSet},
};

/// Resolve the document's grammar once per batch, using its real first line.
/// Each diff row still starts a fresh parser because adjacent rows may belong
/// to different revisions or omit context.
pub(crate) struct FragmentHighlighter<'a> {
    syntax_set: &'a SyntaxSet,
    syntax: &'a SyntaxReference,
    theme: Option<&'a Theme>,
}

impl<'a> FragmentHighlighter<'a> {
    pub(crate) fn new(
        first_line: &str,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
        syntax_set: &'a SyntaxSet,
        theme_set: &'a ThemeSet,
    ) -> Self {
        Self {
            syntax_set,
            syntax: select_syntax(syntax_set, file_path, language, first_line),
            theme: select_theme(theme_set, theme_key),
        }
    }

    pub(crate) fn highlight_line(&self, line: &str) -> HighlightSpans {
        let Some(theme) = self.theme else {
            return vec![(Style::default(), line.to_string())];
        };
        HighlightLines::new(self.syntax, theme)
            .highlight_line(line, self.syntax_set)
            .map_or_else(
                |_| vec![(Style::default(), line.to_string())],
                |ranges| {
                    ranges
                        .into_iter()
                        .map(|(style, text)| (style, text.to_string()))
                        .collect()
                },
            )
    }
}

type Lines = FxHashMap<(usize, u64), Rc<HighlightSpans>>;
const MAX_CACHED_FRAGMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct Languages {
    inferred: Lines,
    explicit: FxHashMap<String, Lines>,
}

impl Languages {
    fn len(&self) -> usize {
        self.inferred.len() + self.explicit.values().map(Lines::len).sum::<usize>()
    }

    fn bytes(&self) -> usize {
        std::iter::once(&self.inferred).chain(self.explicit.values())
            .flat_map(|lines| lines.values()).map(|spans| span_bytes(spans)).sum()
    }
}

#[derive(Default)]
pub(super) struct FragmentCache {
    documents: FxHashMap<PathBuf, FxHashMap<String, Languages>>,
    len: usize,
    bytes: usize,
}

impl FragmentCache {
    pub(super) fn get(
        &self,
        path: &Path,
        theme: &str,
        language: Option<&str>,
        line: usize,
        hash: u64,
    ) -> Option<Rc<HighlightSpans>> {
        let languages = self.documents.get(path)?.get(theme)?;
        let lines = match language {
            Some(language) => languages.explicit.get(language)?,
            None => &languages.inferred,
        };
        lines.get(&(line, hash)).map(Rc::clone)
    }

    pub(super) fn insert(
        &mut self,
        path: &Path,
        theme: &str,
        language: Option<&str>,
        line: usize,
        hash: u64,
        spans: HighlightSpans,
    ) {
        let bytes = span_bytes(&spans);
        let previous = self.get(path, theme, language, line, hash);
        let previous_bytes = previous.as_ref().map_or(0, |spans| span_bytes(spans));
        let replacing = previous.is_some();
        drop(previous);
        if (self.len + usize::from(!replacing) > MAX_CACHED_LINES_PER_DOCUMENT
            || self.bytes - previous_bytes + bytes > MAX_CACHED_FRAGMENT_BYTES)
            && self.len > usize::from(replacing)
        {
            self.clear();
        }
        // Owned context keys are allocated only when a new context is added.
        if !self.documents.contains_key(path) {
            self.documents
                .insert(path.to_path_buf(), FxHashMap::default());
        }
        let themes = self.documents.get_mut(path).unwrap();
        if !themes.contains_key(theme) {
            themes.insert(theme.to_owned(), Languages::default());
        }
        let languages = themes.get_mut(theme).unwrap();
        let lines = if let Some(language) = language {
            if !languages.explicit.contains_key(language) {
                languages
                    .explicit
                    .insert(language.to_owned(), Lines::default());
            }
            languages.explicit.get_mut(language).unwrap()
        } else {
            &mut languages.inferred
        };
        if let Some(previous) = lines.insert((line, hash), Rc::new(spans)) {
            self.bytes -= span_bytes(&previous);
        } else { self.len += 1; }
        self.bytes += bytes;
        // A single unusually large result remains usable, even if it exceeds
        // the budget. Later rows evict it instead of accumulating beside it.
    }

    pub(super) fn clear(&mut self) {
        self.documents.clear();
        self.len = 0;
        self.bytes = 0;
    }

    pub(super) fn remove_theme(&mut self, theme: &str) {
        self.documents.retain(|_, themes| {
            if let Some(languages) = themes.remove(theme) {
                self.len -= languages.len();
                self.bytes -= languages.bytes();
            }
            !themes.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual optimized memory benchmark; run alone on one idle CPU"]
    fn benchmark_memory_fragment_cache() {
        use std::time::Instant;
        let mut cache = FragmentCache::default();
        let started = Instant::now();
        for line in 0..5000 {
            let spans = (0..64).map(|_| (Style::default(), "a".repeat(64))).collect();
            cache.insert(Path::new("diff.cpp"), "dark", Some("C++"), line, line as u64, spans);
        }
        let elapsed = started.elapsed();
        let mut retained = 0;
        for themes in cache.documents.values() {
            for languages in themes.values() {
                for lines in std::iter::once(&languages.inferred).chain(languages.explicit.values()) {
                    for spans in lines.values() {
                        retained += spans.capacity() * std::mem::size_of::<(Style, String)>()
                            + spans.iter().map(|(_, text)| text.capacity()).sum::<usize>();
                    }
                }
            }
        }
        println!("5000 tokenized diff rows: {elapsed:?}; {} retained rows; {retained} retained span bytes", cache.len);
    }

    #[test]
    fn fragment_contexts_keep_paths_themes_languages_hashes_and_replacements_separate() {
        let mut cache = FragmentCache::default();
        let spans = |label: &str| vec![(syntect::highlighting::Style::default(), label.to_owned())];
        for path in ["first.rs", "second.rs"] {
            for theme in ["dark", "light"] {
                for language in [None, Some(""), Some("Rust"), Some("C")] {
                    for (line, hash) in [(1, 7), (1, 8), (2, 7)] {
                        cache.insert(Path::new(path), theme, language, line, hash, spans(path));
                    }
                }
            }
        }
        assert_eq!(cache.len, 48);
        let path = Path::new("first.rs");
        let old = cache.get(path, "dark", None, 1, 7).unwrap();
        assert!(Rc::ptr_eq(
            &old,
            &cache.get(path, "dark", None, 1, 7).unwrap()
        ));
        cache.insert(path, "dark", None, 1, 7, spans("changed"));
        assert_eq!(cache.len, 48);
        assert_eq!(cache.get(path, "dark", None, 1, 7).unwrap()[0].1, "changed");
        assert_eq!(
            cache.get(path, "dark", Some(""), 1, 7).unwrap()[0].1,
            "first.rs"
        );
        cache.remove_theme("dark");
        assert_eq!(cache.len, 24);
        assert!(cache.get(path, "dark", None, 1, 7).is_none());
        assert!(cache.get(path, "light", None, 1, 7).is_some());
        cache.clear();
        assert_eq!(cache.len, 0);
        assert!(cache.documents.is_empty());
    }

    #[test]
    fn fragment_cache_remains_bounded_when_many_contexts_are_opened() {
        let mut cache = FragmentCache::default();
        for i in 0..MAX_CACHED_LINES_PER_DOCUMENT * 2 + 7 {
            cache.insert(
                Path::new("diff.rs"),
                &format!("theme{i}"),
                Some("Rust"),
                i,
                i as u64,
                Vec::new(),
            );
            assert!(cache.len <= MAX_CACHED_LINES_PER_DOCUMENT);
            assert!(cache.documents.len() <= cache.len);
        }
        assert_eq!(cache.len, 7);
        assert_eq!(cache.documents[Path::new("diff.rs")].len(), 7);
    }

    #[test]
    fn fragment_byte_accounting_handles_replacement_and_theme_removal() {
        let mut cache = FragmentCache::default();
        let path = Path::new("memory.cpp");
        let spans = |capacity| {
            let mut text = String::with_capacity(capacity);
            text.push('x');
            vec![(Style::default(), text)]
        };
        cache.insert(path, "dark", None, 0, 1, spans(4096));
        cache.insert(path, "light", Some("C++"), 0, 1, spans(8192));
        let old = cache.get(path, "dark", None, 0, 1).unwrap();
        assert_eq!(cache.bytes, span_bytes(&old)
            + span_bytes(&cache.get(path, "light", Some("C++"), 0, 1).unwrap()));
        cache.insert(path, "dark", None, 0, 1, spans(32));
        assert_eq!(cache.len, 2);
        cache.remove_theme("light");
        assert_eq!(cache.len, 1);
        assert_eq!(cache.bytes, span_bytes(&cache.get(path, "dark", None, 0, 1).unwrap()));
        // Published rows stay valid even when the cache evicts/replaces them.
        assert_eq!(old[0].1, "x");
        assert!(old[0].1.capacity() >= 4096);
        cache.clear();
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn wide_fragment_rows_obey_a_byte_budget_instead_of_only_a_row_count() {
        let path = Path::new("wide.cpp");
        let mut cache = FragmentCache::default();
        for line in 0..1000 {
            cache.insert(path, "dark", None, line, line as u64,
                vec![(Style::default(), "a".repeat(16 * 1024))]);
            assert!(cache.bytes <= MAX_CACHED_FRAGMENT_BYTES);
            assert!(cache.get(path, "dark", None, line, line as u64).is_some());
        }
        assert!(cache.len < 1000);
        assert_eq!(cache.bytes, cache.documents.values()
            .flat_map(|themes| themes.values()).map(Languages::bytes).sum::<usize>());
    }

    #[test]
    fn replacing_a_full_fragment_cache_does_not_clear_unrelated_rows() {
        let path = Path::new("replace.cpp");
        let mut cache = FragmentCache::default();
        for line in 0..MAX_CACHED_LINES_PER_DOCUMENT {
            cache.insert(path, "dark", None, line, line as u64, Vec::new());
        }
        cache.insert(path, "dark", None, 0, 0, vec![(Style::default(), "changed".into())]);
        assert_eq!(cache.len, MAX_CACHED_LINES_PER_DOCUMENT);
        assert!(cache.get(path, "dark", None, 1, 1).is_some());
    }

    #[test]
    #[ignore = "manual optimized-profile borrowed fragment lookup benchmark"]
    fn benchmark_borrowed_fragment_lookup() {
        use std::{hint::black_box, time::Instant};
        let path = Path::new("C:/workspace/project/src/nested/a-very-long-comparison-name.cpp");
        let mut cache = FragmentCache::default();
        let mut reference = FxHashMap::default();
        for line in 0..5000 {
            cache.insert(
                path,
                "Default Dark Modern",
                Some("C++"),
                line,
                line as u64,
                Vec::new(),
            );
            reference.insert(
                (
                    path.to_path_buf(),
                    "Default Dark Modern".to_owned(),
                    Some("C++".to_owned()),
                    line,
                    line as u64,
                ),
                Rc::new(Vec::<()>::new()),
            );
        }
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..7 {
            for borrowed in [round % 2 == 0, round % 2 != 0] {
                let start = Instant::now();
                for step in 0..100_000 {
                    let line = step % 5000;
                    if borrowed {
                        black_box(cache.get(
                            black_box(path),
                            "Default Dark Modern",
                            Some("C++"),
                            line,
                            line as u64,
                        ));
                    } else {
                        black_box(
                            reference
                                .get(&(
                                    black_box(path).to_path_buf(),
                                    "Default Dark Modern".to_owned(),
                                    Some("C++".to_owned()),
                                    line,
                                    line as u64,
                                ))
                                .cloned(),
                        );
                    }
                }
                samples[usize::from(borrowed)].push(start.elapsed() / 100_000);
            }
        }
        for sample in &mut samples {
            sample.sort();
        }
        eprintln!(
            "fragment lookup allocated={:?} borrowed={:?}",
            samples[0][3], samples[1][3]
        );
    }
}
