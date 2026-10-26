use std::{
    borrow::Cow,
    cell::OnceCell,
    path::{Component, Path, PathBuf},
};

/// One candidate can be tested against workspace, explicit-root and absolute
/// globs. Prepare each distinct spelling only if a glob actually needs it.
/// The usual single-root case keeps its cache entry inline; additional roots
/// allocate only when needed. All representations die with this candidate.
pub(crate) struct Candidate<'a> {
    path: &'a Path,
    prepared: std::cell::RefCell<PreparedPaths<'a>>,
    #[cfg(test)]
    preparations: std::cell::Cell<usize>,
}

#[derive(Default)]
struct PreparedPaths<'a> {
    first: Option<(&'a Path, PreparedText<'a>)>,
    others: Vec<(&'a Path, PreparedText<'a>)>,
}

struct PreparedText<'a> {
    text: Cow<'a, str>,
    ascii: bool,
    utf16: OnceCell<Vec<u16>>,
}

impl PreparedText<'_> {
    fn utf16(&self) -> &[u16] {
        self.utf16.get_or_init(|| {
            self.text
                .encode_utf16()
                .map(|unit| {
                    if unit == u16::from(b'\\') {
                        u16::from(b'/')
                    } else {
                        unit
                    }
                })
                .collect()
        })
    }
}

impl<'a> Candidate<'a> {
    pub(crate) fn new(path: &'a Path) -> Self {
        Self {
            path,
            prepared: Default::default(),
            #[cfg(test)]
            preparations: Default::default(),
        }
    }

    fn with_prepared<T>(&self, path: &'a Path, run: impl FnOnce(&PreparedText<'_>) -> T) -> T {
        let mut prepared = self.prepared.borrow_mut();
        // Compare exact spellings: Path equality folds lexical components such
        // as repeated separators, but those still matter to the glob engine.
        if let Some((source, text)) = &prepared.first
            && source.as_os_str() == path.as_os_str()
        {
            return run(text);
        }
        if let Some((_, text)) = prepared
            .others
            .iter()
            .find(|(source, _)| source.as_os_str() == path.as_os_str())
        {
            return run(text);
        }
        let source = path.to_string_lossy();
        let ascii = source.is_ascii();
        // Unicode candidates go straight to UTF-16. Normalize there rather
        // than allocating an unused intermediate UTF-8 spelling on Windows.
        let text = if ascii && source.as_bytes().contains(&b'\\') {
            Cow::Owned(source.replace('\\', "/"))
        } else {
            source
        };
        let text = PreparedText {
            ascii,
            text,
            utf16: OnceCell::new(),
        };
        #[cfg(test)]
        self.preparations.set(self.preparations.get() + 1);
        let slot = if prepared.first.is_none() {
            prepared.first.insert((path, text))
        } else {
            prepared.others.push((path, text));
            prepared.others.last_mut().unwrap()
        };
        run(&slot.1)
    }

    fn matches(&self, globs: &SearchGlobs, path: &'a Path) -> bool {
        !globs.0.is_empty() && self.with_prepared(path, |text| globs.is_match(text))
    }

    #[cfg(test)]
    fn with_text<T>(&self, path: &'a Path, run: impl FnOnce(&[u16]) -> T) -> T {
        self.with_prepared(path, |text| run(text.utf16()))
    }
}

// Behavioral ports of base/common/glob.ts and queryBuilder.ts at the pinned
// VS Code revision. In particular, glob question marks count UTF-16 units.
#[derive(Clone)]
pub(crate) struct SearchGlobs(Vec<regress::Regex>, Option<FastGlobs>);

#[derive(Clone)]
struct FastGlobs {
    matcher: regex::bytes::RegexSet,
    fallback: Vec<usize>,
}

#[derive(Default)]
struct GlobBuilder {
    original: Vec<regress::Regex>,
    ascii_sources: Vec<Option<String>>,
}

impl GlobBuilder {
    fn add(&mut self, source: String, ascii: bool) -> bool {
        let Ok(regex) = regress::Regex::new(&source) else {
            return false;
        };
        self.original.push(regex);
        self.ascii_sources
            .push(ascii.then(|| ascii_regex(&source)).flatten());
        true
    }

    fn finish(self) -> SearchGlobs {
        let fast = if self.ascii_sources.iter().any(Option::is_some) {
            regex::bytes::RegexSetBuilder::new(
                self.ascii_sources.iter().filter_map(Option::as_deref),
            )
            .unicode(false)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .build()
            .ok()
            .map(|matcher| FastGlobs {
                matcher,
                fallback: self
                    .ascii_sources
                    .iter()
                    .enumerate()
                    .filter_map(|(index, source)| source.is_none().then_some(index))
                    .collect(),
            })
        } else {
            None
        };
        SearchGlobs(self.original, fast)
    }
}

// UTF-8 bytes and UTF-16 units agree on ASCII paths. Unicode candidates always
// use the original engine, preserving VS Code's question-mark semantics.
fn ascii_glob(pattern: &str) -> bool {
    pattern.is_ascii()
}

fn ascii_regex(source: &str) -> Option<String> {
    let mut output = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_class = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                output.push(ch);
                output.push(chars.next()?);
            }
            '[' => {
                in_class = true;
                output.push(ch);
            }
            ']' => {
                in_class = false;
                output.push(ch);
            }
            // These are literal class members in JavaScript, but form set
            // operations in Rust. Ambiguous double dashes stay on the oracle.
            '&' if in_class => output.push_str("\\x26"),
            '~' if in_class => output.push_str("\\x7e"),
            '-' if in_class && chars.peek() == Some(&'-') => return None,
            // JavaScript dot also excludes CR, unlike Rust's default byte dot.
            '.' if !in_class => output.push_str("[^\\r\\n]"),
            _ => output.push(ch),
        }
    }
    Some(output)
}

impl SearchGlobs {
    fn is_match(&self, text: &PreparedText<'_>) -> bool {
        if text.ascii
            && let Some(fast) = &self.1
        {
            return fast.matcher.is_match(text.text.as_bytes())
                || !fast.fallback.is_empty()
                    && fast.fallback.iter().any(|&index| {
                        self.0[index]
                            .find_from_ucs2(text.utf16(), 0)
                            .next()
                            .is_some()
                    });
        }
        self.0
            .iter()
            .any(|regex| regex.find_from_ucs2(text.utf16(), 0).next().is_some())
    }
}

fn split_aware(source: &str, delimiter: char) -> Vec<&str> {
    let (mut braces, mut brackets, mut start) = (false, false, 0);
    let mut bracket_empty = false;
    let mut result = Vec::new();
    for (i, ch) in source.char_indices() {
        if brackets {
            // A leading ']' is a literal member, as in []]. Braces and
            // delimiters inside a character class are literal as well.
            if ch == ']' && !bracket_empty {
                brackets = false;
            }
            bracket_empty = false;
            continue;
        }
        match ch {
            '{' => braces = true,
            '}' => braces = false,
            '[' => {
                brackets = true;
                bracket_empty = true;
            }
            ch if ch == delimiter && !braces && !brackets => {
                result.push(&source[start..i]);
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    if !source.is_empty() {
        result.push(&source[start..]);
    }
    result
}

pub(crate) fn split_patterns(source: &str) -> Vec<&str> {
    split_aware(source, ',')
        .into_iter()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .collect()
}

/// Include exactly this folder and its descendants, even when its name
/// contains characters that otherwise act as globs or pattern separators.
pub(crate) fn folder_include_pattern(workspace: &Path, folder: &Path) -> String {
    let path = match crate::native_paths::strip_prefix(folder, workspace) {
        Some(relative) => format!("./{}", relative.to_string_lossy().replace('\\', "/")),
        None => folder.to_string_lossy().replace('\\', "/"),
    };
    let mut pattern = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | ',') {
            pattern.push('[');
            pattern.push(ch);
            pattern.push(']');
        } else {
            pattern.push(ch);
        }
    }
    pattern
}

fn glob_regex(pattern: &str) -> String {
    if pattern.is_empty() {
        return String::new();
    }
    let segments = split_aware(pattern, '/');
    if segments.iter().all(|&segment| segment == "**") {
        return ".*".to_owned();
    }
    let mut regex = String::new();
    let mut previous_globstar = false;
    for (i, segment) in segments.iter().enumerate() {
        if *segment == "**" {
            if previous_globstar {
                continue;
            }
            regex.push_str(if i + 1 == segments.len() {
                "(?:/|[^/]+/|/[^/]+)*?"
            } else {
                "(?:/|[^/]+/)*?"
            });
        } else {
            let (mut braces, mut brackets) = (false, false);
            let (mut brace_value, mut bracket_value) = (String::new(), String::new());
            for ch in segment.chars() {
                if ch != '}' && braces {
                    brace_value.push(ch);
                    continue;
                }
                if brackets && (ch != ']' || bracket_value.is_empty()) {
                    if ch == '-' {
                        bracket_value.push(ch);
                    } else if matches!(ch, '^' | '!') && bracket_value.is_empty() {
                        bracket_value.push('^');
                    } else if ch != '/' {
                        bracket_value
                            .push_str(&crate::search_regex::escape_literal(&ch.to_string()));
                    }
                    continue;
                }
                match ch {
                    '{' => braces = true,
                    '[' => brackets = true,
                    '}' => {
                        regex.push_str("(?:");
                        regex.push_str(
                            &split_aware(&brace_value, ',')
                                .into_iter()
                                .map(glob_regex)
                                .collect::<Vec<_>>()
                                .join("|"),
                        );
                        regex.push(')');
                        braces = false;
                        brace_value.clear();
                    }
                    ']' => {
                        regex.push('[');
                        regex.push_str(&bracket_value);
                        regex.push(']');
                        brackets = false;
                        bracket_value.clear();
                    }
                    '?' => regex.push_str("[^/]"),
                    '*' => regex.push_str("[^/]*?"),
                    _ => regex.push_str(&crate::search_regex::escape_literal(&ch.to_string())),
                }
            }
            if i + 1 < segments.len() && (segments[i + 1] != "**" || i + 2 < segments.len()) {
                regex.push('/');
            }
        }
        previous_globstar = *segment == "**";
    }
    regex
}

fn add_glob(builder: &mut GlobBuilder, pattern: &str) {
    builder.add(format!("^{}$", glob_regex(pattern)), ascii_glob(pattern));
}

pub(crate) fn compile_patterns(source: &str) -> SearchGlobs {
    let mut builder = GlobBuilder::default();
    for pattern in split_patterns(source) {
        let pattern = pattern.replace('\\', "/");
        let pattern = pattern.trim_end_matches('/');
        if pattern == "." || pattern.is_empty() {
            add_glob(&mut builder, "**");
            continue;
        }
        if let Some(rooted) = pattern.strip_prefix("./") {
            add_glob(&mut builder, rooted);
            if !rooted.ends_with("**") {
                add_glob(&mut builder, &format!("{rooted}/**"));
            }
        } else {
            let pattern = if pattern.starts_with('.') {
                format!("*{pattern}")
            } else {
                pattern.to_owned()
            };
            for suffix in ["/**", ""] {
                let mut expanded = format!("**/{pattern}{suffix}");
                while expanded.contains("**/**") {
                    expanded = expanded.replace("**/**", "**");
                }
                add_glob(&mut builder, &expanded);
            }
        }
    }
    builder.finish()
}

#[cfg(test)]
pub(crate) fn matches_glob(path: &str, pattern: &str) -> bool {
    matches_glob_prepared(&Candidate::new(Path::new(path)), pattern)
}

pub(crate) fn matches_glob_prepared(candidate: &Candidate<'_>, pattern: &str) -> bool {
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<(String, SearchGlobs)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let index = if let Some(index) = cache.iter().position(|(source, _)| source == pattern) {
            index
        } else {
            if cache.len() == 8 {
                cache.remove(0);
            }
            cache.push((pattern.to_owned(), compile_patterns(pattern)));
            cache.len() - 1
        };
        candidate.matches(&cache[index].1, candidate.path)
    })
}

#[cfg(test)]
pub(crate) fn glob_match(path: &str, pattern: &str) -> bool {
    let mut patterns = GlobBuilder::default();
    add_glob(&mut patterns, &pattern.replace('\\', "/"));
    Candidate::new(Path::new(path)).matches(&patterns.finish(), Path::new(path))
}

pub(crate) struct SearchPaths {
    folders: Vec<(PathBuf, Option<SearchGlobs>)>,
    // Conservative directory-prefix matchers, parallel to `folders`. Exact
    // file/open-buffer semantics remain in the original compiled patterns.
    // Compile only for directory walking, never for exclusions/open-buffer
    // scope checks or global includes that cannot prune any directory.
    descendants: Vec<(Vec<String>, std::sync::OnceLock<Option<SearchGlobs>>)>,
    global: SearchGlobs,
    has_global: bool,
    using_paths: bool,
}

/// Match ancestors of an include, and the subtree it can include. Nest optional
/// components instead of generating every prefix: compilation stays linear in
/// pattern length. Recursive/complex segments conservatively allow their whole
/// subtree. A false positive only does extra walking; a false negative could
/// hide a result, so unsupported shapes must never narrow the search.
fn descendant_globs(patterns: &[String]) -> Option<SearchGlobs> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobBuilder::default();
    let mut last_source = String::new();
    for pattern in patterns {
        let mut regex = String::from("^");
        let mut depth = 0;
        for segment in split_aware(pattern, '/') {
            if segment.contains("**") || segment.contains('/') {
                break;
            }
            if segment.is_empty() || matches!(segment, "." | "..") || depth == 64 {
                return None;
            }
            regex.push_str("(?:");
            if depth != 0 {
                regex.push('/');
            }
            regex.push_str(&glob_regex(segment));
            depth += 1;
        }
        if depth == 0 {
            return None;
        }
        // Dot in the glob engine does not include line breaks. Filenames can.
        regex.push_str("(?:/[\\s\\S]*)?");
        for _ in 0..depth {
            regex.push_str(")?");
        }
        regex.push('$');
        // Repeated local/** patterns have the same reachability as local.
        if last_source == regex {
            continue;
        }
        last_source.clone_from(&regex);
        if !builder.add(regex, ascii_glob(pattern)) {
            return None;
        }
    }
    Some(builder.finish())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

impl SearchPaths {
    /// Reuse syntax plans as the search text changes. Never cache file lists,
    /// ignore decisions, metadata or conditional-sibling existence checks.
    pub(crate) fn cached(workspace: &Path, source: &str) -> std::sync::Arc<Self> {
        type Entry = (
            PathBuf,
            String,
            Option<std::ffi::OsString>,
            std::sync::Arc<SearchPaths>,
        );
        thread_local! {
            static CACHE: std::cell::RefCell<Vec<Entry>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        // Large inputs still work, without retaining their plans indefinitely.
        if source.len() > 4096 {
            return std::sync::Arc::new(Self::new(workspace, source));
        }
        let home = (source.contains("~/") || source.contains("~\\"))
            .then(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")))
            .flatten();
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(index) = cache
                .iter()
                .position(|entry| entry.0 == workspace && entry.1 == source && entry.2 == home)
            {
                let entry = cache.remove(index);
                let plan = entry.3.clone();
                cache.push(entry);
                return plan;
            }
            let plan = std::sync::Arc::new(Self::new(workspace, source));
            if cache.len() == 4 {
                cache.remove(0);
            }
            cache.push((workspace.to_owned(), source.to_owned(), home, plan.clone()));
            plan
        })
    }

    pub(crate) fn new(workspace: &Path, source: &str) -> Self {
        let mut folders = Vec::<(PathBuf, Vec<String>)>::new();
        let mut globals = Vec::new();
        for pattern in split_patterns(source) {
            let mut pattern = pattern.replace('\\', "/");
            if let Some(tail) = pattern.strip_prefix("~/") {
                if let Some(home) =
                    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
                {
                    pattern = Path::new(&home)
                        .join(tail)
                        .to_string_lossy()
                        .replace('\\', "/");
                }
            }
            let is_path = Path::new(&pattern).is_absolute()
                || matches!(pattern.as_str(), "." | "..")
                || pattern.starts_with("./")
                || pattern.starts_with("../");
            if !is_path {
                globals.push(pattern);
                continue;
            }
            let glob_start = pattern.find(['*', '{', '}', '(', ')', '[', ']', '?']);
            let split = glob_start.and_then(|index| pattern[..index].rfind('/'));
            let (path, glob) = split.map_or((pattern.as_str(), ""), |index| {
                (&pattern[..index + 1], &pattern[index + 1..])
            });
            let (folder, local) = if path.starts_with("./") || path == "." {
                (
                    workspace.to_path_buf(),
                    format!(
                        "{}/{}",
                        path.strip_prefix("./").unwrap_or("").trim_end_matches('/'),
                        glob
                    )
                    .trim_matches('/')
                    .to_owned(),
                )
            } else {
                (
                    normalize_path(&if Path::new(path).is_absolute() {
                        PathBuf::from(path)
                    } else {
                        workspace.join(path)
                    }),
                    glob.to_owned(),
                )
            };
            let index =
                if let Some(index) = folders.iter().position(|(existing, _)| *existing == folder) {
                    index
                } else {
                    folders.push((folder, Vec::new()));
                    folders.len() - 1
                };
            if !local.is_empty() {
                folders[index].1.push(local.clone());
                if !local.ends_with("**") {
                    folders[index].1.push(format!("{local}/**"));
                }
            }
        }
        let using_paths = !folders.is_empty();
        if folders.is_empty() {
            folders.push((workspace.to_path_buf(), Vec::new()));
        }
        let (folders, descendants) = folders
            .into_iter()
            .map(|(path, patterns)| {
                let mut builder = GlobBuilder::default();
                for pattern in &patterns {
                    add_glob(&mut builder, pattern);
                }
                (
                    (path, (!patterns.is_empty()).then(|| builder.finish())),
                    (patterns, std::sync::OnceLock::new()),
                )
            })
            .unzip();
        Self {
            folders,
            descendants,
            global: compile_patterns(&globals.join(",")),
            has_global: !globals.is_empty(),
            using_paths,
        }
    }

    pub(crate) fn roots(&self) -> impl Iterator<Item = &Path> {
        self.folders.iter().map(|(path, _)| path.as_path())
    }

    pub(crate) fn may_descend(&self, candidate: &Candidate<'_>) -> bool {
        self.folders
            .iter()
            .zip(&self.descendants)
            .any(|((root, _), (patterns, descendants))| {
                match crate::native_paths::strip_prefix(candidate.path, root) {
                    Some(relative) => {
                        self.has_global
                            || descendants
                                .get_or_init(|| descendant_globs(patterns))
                                .as_ref()
                                .is_none_or(|glob| candidate.matches(glob, relative))
                    }
                    None => crate::native_paths::strip_prefix(root, candidate.path).is_some(),
                }
            })
    }

    pub(crate) fn includes(&self, candidate: &Candidate<'_>) -> bool {
        self.folders.iter().any(|(root, pattern)| {
            crate::native_paths::strip_prefix(candidate.path, root).is_some_and(|relative| {
                (!self.has_global && pattern.is_none())
                    || candidate.matches(&self.global, relative)
                    || pattern
                        .as_ref()
                        .is_some_and(|pattern| candidate.matches(pattern, relative))
            })
        })
    }

    pub(crate) fn includes_open(&self, candidate: &Candidate<'_>) -> bool {
        // searchService.ts also searches open editors outside the workspace.
        (!self.has_global && !self.using_paths)
            || candidate.matches(&self.global, candidate.path)
            || self.using_paths && self.includes(candidate)
    }

    pub(crate) fn excludes(&self, candidate: &Candidate<'_>, workspace: &Path) -> bool {
        self.has_global
            && candidate.matches(
                &self.global,
                crate::native_paths::strip_prefix(candidate.path, workspace)
                    .unwrap_or(candidate.path),
            )
            || self.using_paths
                && self.folders.iter().any(|(root, pattern)| {
                    crate::native_paths::strip_prefix(candidate.path, root).is_some_and(
                        |relative| {
                            pattern
                                .as_ref()
                                .is_none_or(|pattern| candidate.matches(pattern, relative))
                        },
                    )
                })
    }
}

/// Setting expressions are globs, unlike the expanded include/exclude fields.
/// Search exclusions override file exclusions, including explicit false values.
pub(crate) struct ExcludeRules(Vec<(SearchGlobs, Option<String>)>);

impl ExcludeRules {
    pub(crate) fn new(
        files: &std::collections::BTreeMap<String, serde_json::Value>,
        search: &std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Self {
        let mut merged = std::collections::BTreeMap::new();
        for pattern in [
            "**/.git",
            "**/.svn",
            "**/.hg",
            "**/CVS",
            "**/.DS_Store",
            "**/Thumbs.db",
            "**/node_modules",
            "**/bower_components",
            "**/*.code-search",
        ] {
            merged.insert(pattern.to_owned(), serde_json::Value::Bool(true));
        }
        merged.extend(
            files
                .iter()
                .chain(search)
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        let mut unconditional = GlobBuilder::default();
        let mut rules = merged
            .into_iter()
            .filter_map(|(pattern, value)| {
                let when = match &value {
                    serde_json::Value::Bool(true) => None,
                    serde_json::Value::Object(object) => {
                        Some(object.get("when")?.as_str()?.to_owned())
                    }
                    _ => return None,
                };
                if when.is_none() {
                    add_glob(&mut unconditional, &pattern.replace('\\', "/"));
                    None
                } else {
                    let mut patterns = GlobBuilder::default();
                    add_glob(&mut patterns, &pattern.replace('\\', "/"));
                    Some((patterns.finish(), when))
                }
            })
            .collect::<Vec<_>>();
        if !unconditional.original.is_empty() {
            rules.insert(0, (unconditional.finish(), None));
        }
        Self(rules)
    }

    pub(crate) fn excludes(&self, candidate: &Candidate<'_>, root: &Path) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let path = candidate.path;
        let relative = crate::native_paths::strip_prefix(path, root).unwrap_or(path);
        candidate.with_prepared(relative, |text| {
            self.0.iter().any(|(glob, when)| {
                glob.is_match(text)
                    && when.as_ref().is_none_or(|when| {
                        path.file_stem()
                            .and_then(|stem| stem.to_str())
                            .is_some_and(|stem| {
                                path.with_file_name(when.replace("$(basename)", stem))
                                    .exists()
                            })
                    })
            })
        })
    }
}

#[cfg(test)]
#[path = "search_paths_tests.rs"]
mod prepared_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_includes_are_literal_and_rooted() {
        let workspace = std::env::current_dir().unwrap();
        assert_eq!(folder_include_pattern(&workspace, &workspace), "./");
        for name in [
            "src",
            "folder name",
            "nested/[id]",
            "a,b",
            "a{b",
            "a}b",
            "a]b",
            "a*b?c",
        ] {
            let folder = workspace.join(name);
            let include = folder_include_pattern(&workspace, &folder);
            let paths = SearchPaths::new(&workspace, &include);
            assert!(
                paths.includes(&Candidate::new(&folder.join("file.rs"))),
                "{include}"
            );
            assert!(
                paths.includes(&Candidate::new(&folder.join("nested/file.rs"))),
                "{include}"
            );
            assert!(
                !paths.includes(&Candidate::new(
                    &workspace.join(format!("{name}-sibling/file.rs"))
                )),
                "{include}"
            );
            assert!(
                !paths.includes(&Candidate::new(
                    &workspace.join("elsewhere").join(name).join("file.rs")
                )),
                "{include}"
            );
            assert!(
                !paths.includes_open(&Candidate::new(&workspace.join("elsewhere/file.rs"))),
                "{include}"
            );
            // Escaped punctuation must not consume a following include pattern.
            let combined = format!("{include}, ./extra");
            assert_eq!(split_patterns(&combined), [include.as_str(), "./extra"]);
            assert!(
                SearchPaths::new(&workspace, &combined)
                    .includes(&Candidate::new(&workspace.join("extra/file.rs")))
            );
        }
    }
}
