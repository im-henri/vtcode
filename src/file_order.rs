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
            base: make(Strength::Primary)?, accent: make(Strength::Secondary)? })
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
