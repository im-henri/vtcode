//! VS Code's comparers.ts semantics, shared by Explorer and search results.
//! Reference: microsoft/vscode eaa5f41042f412308642339e8874c6bf13b3f01b
//! src/vs/base/common/comparers.ts (MIT, Microsoft Corporation).
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences,
    options::{CollatorOptions, Strength}, preferences::CollationNumericOrdering};
use std::{cmp::Ordering, path::Path, sync::{Arc, OnceLock}};
#[cfg(test)]
#[path = "file_order_tests.rs"]
mod tests;

#[derive(Debug)]
pub(crate) struct FileOrder {
    locale: String,
    names: CollatorBorrowed<'static>,
    base: CollatorBorrowed<'static>,
    accent: CollatorBorrowed<'static>,
    /// Whether precomputed sort keys reproduce `compare` for this collation.
    keys: OnceLock<bool>,
}

/// Names exercising case, accents, numerals, punctuation and scripts. The
/// sort-key path is used only if it orders all of them exactly like `compare`.
const KEY_PROBE: &[&str] = &["a", "A", "á", "Á", "ä", "b", "file1", "file01", "File1", "file10", "file9",
    "a 1", "a_1", "a-1", "a.1", "x.tar.gz", "x.gz", ".gitignore", "README", "readme.md", "ß", "ss", "SS",
    "cote", "côte", "coté", "côté", "e\u{301}", "é", "ǆ", "dž", "Å", "å", "日本", "ﾆﾎﾝ", "😀", "z", "Z"];

/// French (Canada) collation compares accents from the end of the string.
/// ICU4X 2.1's sort-key writer can panic in that mode, so never use it there.
fn backwards_secondary(collator: &CollatorBorrowed<'static>) -> bool {
    collator.compare("côte", "coté") == Ordering::Less
}

impl FileOrder {
    pub(crate) fn new(locale: &str) -> Option<Self> {
        let locale: icu_locale_core::Locale = locale.parse().ok()?;
        let mut preferences = CollatorPreferences::from(&locale);
        preferences.numeric_ordering = Some(CollationNumericOrdering::True);
        let make = |strength| {
            let mut options = CollatorOptions::default();
            options.strength = Some(strength);
            Collator::try_new(preferences, options).ok()
        };
        Some(Self { locale: locale.to_string(), names: make(Strength::Tertiary)?,
            base: make(Strength::Primary)?, accent: make(Strength::Secondary)?, keys: OnceLock::new() })
    }

    pub(crate) fn system() -> Arc<Self> {
        static SYSTEM: OnceLock<Arc<FileOrder>> = OnceLock::new();
        SYSTEM.get_or_init(|| Arc::new(sys_locale::get_locale().as_deref().and_then(Self::new)
            .unwrap_or_else(|| Self::new("en-US").expect("bundled default collation")))).clone()
    }

    pub(crate) fn locale(&self) -> &str { &self.locale }

    /// Explorer: numeric, variant-sensitive, then JavaScript string length.
    pub(crate) fn names(&self, a: &str, b: &str) -> Ordering {
        self.names.compare(a, b).then_with(|| utf16_len(a).cmp(&utf16_len(b)))
    }

    /// Search: numeric and base-sensitive, then JavaScript ordinal order.
    pub(crate) fn search_names(&self, a: &str, b: &str) -> Ordering {
        self.base.compare(a, b).then_with(|| ordinal(a, b))
    }

    /// Whether `name_key`/`extension_key` may be used for this collation.
    /// Checked once against `compare` on a probe set; otherwise callers keep
    /// comparing names directly.
    pub(crate) fn has_sort_keys(&self) -> bool {
        *self.keys.get_or_init(|| {
            if backwards_secondary(&self.names) || backwards_secondary(&self.accent) { return false; }
            let names: Vec<_> = KEY_PROBE.iter().map(|name| (*name, self.name_key(name), self.extension_key(name))).collect();
            names.iter().all(|(a, a_name, a_extension)| names.iter().all(|(b, b_name, b_extension)|
                a_name.cmp(b_name) == self.names(a, b) && a_extension.cmp(b_extension) == self.extensions(a, b)))
        })
    }

    /// A precomputed `names` ordering. Comparing keys orders exactly like
    /// `names`, but a large listing derives collation elements once per name
    /// instead of once per comparison (about log2(n) times per name). Use
    /// only when `has_sort_keys` is true.
    pub(crate) fn name_key(&self, name: &str) -> SortKey {
        SortKey { parts: [Self::collation_key(&self.names, name), Vec::new()],
            lengths: [utf16_len(name), 0] }
    }

    /// A precomputed `extensions` ordering (extension, then the full name).
    pub(crate) fn extension_key(&self, name: &str) -> SortKey {
        let extension = extension(name);
        SortKey { parts: [Self::collation_key(&self.accent, extension), Self::collation_key(&self.names, name)],
            lengths: [utf16_len(extension), utf16_len(name)] }
    }

    fn collation_key(collator: &CollatorBorrowed<'static>, value: &str) -> Vec<u8> {
        let mut key = Vec::with_capacity(value.len() * 4);
        let Ok(()) = collator.write_sort_key_to(value, &mut key);
        key
    }

    pub(crate) fn extensions(&self, a: &str, b: &str) -> Ordering {
        let (a_ext, b_ext) = (extension(a), extension(b));
        self.accent.compare(a_ext, b_ext)
            .then_with(|| utf16_len(a_ext).cmp(&utf16_len(b_ext)))
            .then_with(|| self.names(a, b))
    }

    pub(crate) fn paths(&self, a: &SearchPathKey, b: &SearchPathKey) -> Ordering {
        for index in 0.. {
            let a_end = index + 1 == a.parts.len();
            let b_end = index + 1 == b.parts.len();
            if a_end && b_end {
                return self.search_names(&a.parts[index], &b.parts[index])
                    .then_with(|| self.search_names(&a.name, &b.name));
            }
            // comparePaths prioritizes files at this shared parent before
            // paths continuing into subdirectories, regardless of the name.
            if a_end { return Ordering::Less; }
            if b_end { return Ordering::Greater; }
            let order = ordinal(&a.folded[index], &b.folded[index]);
            if order != Ordering::Equal { return order; }
        }
        unreachable!()
    }
}

/// See [`FileOrder::name_key`]. Keys from different `FileOrder`s or kinds
/// (name vs extension) must not be compared with each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortKey {
    parts: [Vec<u8>; 2],
    lengths: [usize; 2],
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // Same tie-breaks as the comparers: collation, then UTF-16 length.
        self.parts[0].cmp(&other.parts[0])
            .then_with(|| self.lengths[0].cmp(&other.lengths[0]))
            .then_with(|| self.parts[1].cmp(&other.parts[1]))
            .then_with(|| self.lengths[1].cmp(&other.lengths[1]))
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

fn utf16_len(value: &str) -> usize { value.encode_utf16().count() }
fn ordinal(a: &str, b: &str) -> Ordering { a.encode_utf16().cmp(b.encode_utf16()) }
fn extension(name: &str) -> &str {
    if name.starts_with('.') { "" } else { name.rsplit_once('.').map_or("", |(_, extension)| extension) }
}

#[derive(Debug)]
pub(crate) struct SearchPathKey {
    parts: Vec<String>,
    folded: Vec<String>,
    name: String,
}
impl SearchPathKey {
    pub(crate) fn new(path: &str, name: &str) -> Self {
        let normalized;
        let path = if cfg!(windows) { normalized = path.replace('\\', "/"); &normalized } else { path };
        let parts: Vec<String> = path.split('/').map(str::to_owned).collect();
        let folded = parts.iter().map(|part| part.to_lowercase()).collect();
        Self { parts, folded, name: name.to_owned() }
    }
    pub(crate) fn local(path: &Path) -> Self {
        Self::new(&path.to_string_lossy(), &path.file_name().unwrap_or_default().to_string_lossy())
    }
}
