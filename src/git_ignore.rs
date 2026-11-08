//! Cached Git ignore rules shared by discovery and Explorer workers.
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rustc_hash::{FxHashMap, FxHashSet};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

struct DirectoryRules {
    // Only nonempty rule sets, shared with descendants. Match a flat chain,
    // rather than walking/hashing every ancestor for every file.
    matchers: Vec<Arc<Gitignore>>,
    ignored: bool,
}

pub(crate) struct Rules {
    workdir: PathBuf,
    directories: FxHashMap<PathBuf, Arc<DirectoryRules>>,
    /// The parent of the previous lookup. Explorer and discovery match whole
    /// listings, so consecutive siblings reuse it without rehashing the path.
    last: Option<(PathBuf, Arc<DirectoryRules>)>,
    global: Vec<Arc<Gitignore>>,
    case_insensitive: bool,
    reliable: bool,
}

impl Rules {
    pub(crate) fn new(repo: &git2::Repository, workdir: &Path, on_read: &mut impl FnMut()) -> Self {
        let config = repo.config().ok();
        let mut reliable = config.is_some();
        let case_insensitive = config
            .as_ref()
            .and_then(|config| value(config.get_bool("core.ignorecase"), &mut reliable))
            .unwrap_or(false);
        let mut global = Vec::new();
        if let Some(path) = config
            .as_ref()
            .and_then(|config| value(config.get_path("core.excludesfile"), &mut reliable))
            .or_else(ignore::gitignore::gitconfig_excludes_path)
        {
            append_rules(
                &mut global,
                read_rules(
                    workdir,
                    &workdir.join(path),
                    case_insensitive,
                    on_read,
                    &mut reliable,
                ),
            );
        }
        append_rules(
            &mut global,
            read_rules(
                workdir,
                &repo.commondir().join("info/exclude"),
                case_insensitive,
                on_read,
                &mut reliable,
            ),
        );
        Self {
            workdir: workdir.into(),
            directories: FxHashMap::default(),
            last: None,
            global,
            case_insensitive,
            reliable,
        }
    }

    /// Discovery must fall back to libgit2 when a rule/configuration could not
    /// be read, rather than silently descending into excluded build output.
    pub(crate) fn reliable(&self) -> bool {
        self.reliable
    }

    pub(crate) fn trim(&mut self, limit: usize) {
        if self.directories.len() >= limit {
            self.directories.clear();
            self.last = None;
        }
    }

    pub(crate) fn invalidate_directories(&mut self, roots: &FxHashSet<PathBuf>) {
        self.directories
            .retain(|path, _| !path.ancestors().any(|p| roots.contains(p)));
        self.last = None;
    }

    fn directory(&mut self, path: &Path, on_read: &mut impl FnMut()) {
        if self.directories.contains_key(path) {
            return;
        }
        let ignored = path != self.workdir && self.is_ignored(path, true, on_read);
        // Git does not read ignore files beneath an excluded directory: a
        // child negation cannot resurrect a path with an ignored parent.
        let mut matchers = if ignored {
            Vec::new()
        } else if path == self.workdir {
            self.global.clone()
        } else {
            self.directories[path.parent().unwrap()].matchers.clone()
        };
        if !ignored {
            let file = path.join(".gitignore");
            if !file.is_symlink() {
                append_rules(
                    &mut matchers,
                    read_rules(
                        path,
                        &file,
                        self.case_insensitive,
                        on_read,
                        &mut self.reliable,
                    ),
                );
            }
        }
        self.directories
            .insert(path.to_owned(), Arc::new(DirectoryRules { matchers, ignored }));
    }

    pub(crate) fn is_ignored(
        &mut self,
        path: &Path,
        is_dir: bool,
        on_read: &mut impl FnMut(),
    ) -> bool {
        if path.file_name().is_some_and(|name| name == ".git") {
            return true;
        }
        let Some(parent) = path.parent() else { return false; };
        let rules = match &self.last {
            // Byte-identical parents compare without component parsing.
            Some((last, rules)) if last.as_path() == parent => Arc::clone(rules),
            _ => {
                if !parent.starts_with(&self.workdir) { return false; }
                self.directory(parent, on_read);
                let rules = Arc::clone(&self.directories[parent]);
                self.last = Some((parent.to_owned(), Arc::clone(&rules)));
                rules
            }
        };
        if rules.ignored {
            return true;
        }
        // The deepest .gitignore wins, including standalone negations of
        // patterns in parent files (libgit2's ignore lookup drops those).
        for matcher in rules.matchers.iter().rev() {
            match matcher.matched(path, is_dir) {
                ignore::Match::Ignore(_) => return true,
                ignore::Match::Whitelist(_) => return false,
                ignore::Match::None => {}
            }
        }
        false
    }
}

fn append_rules(matchers: &mut Vec<Arc<Gitignore>>, rules: Gitignore) {
    if !rules.is_empty() {
        matchers.push(Arc::new(rules));
    }
}

fn read_rules(
    root: &Path,
    file: &Path,
    case_insensitive: bool,
    on_read: &mut impl FnMut(),
    reliable: &mut bool,
) -> Gitignore {
    on_read();
    let mut builder = GitignoreBuilder::new(root);
    let _ = builder.case_insensitive(case_insensitive);
    if let Some(error) = builder.add(file)
        && !error
            .io_error()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        *reliable = false;
    }
    // Explorer keeps valid rules from a partial parse. Discovery asks libgit2
    // for its authoritative result when the fast matcher was incomplete.
    builder.build().unwrap_or_else(|_| {
        *reliable = false;
        Gitignore::empty()
    })
}

fn value<T>(result: Result<T, git2::Error>, reliable: &mut bool) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            if error.code() != git2::ErrorCode::NotFound {
                *reliable = false;
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_lookups_read_each_ignore_chain_once_and_invalidation_reloads_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(root.join("src/.gitignore"), "generated*/\n").unwrap();
        let mut reads = 0;
        let mut rules = Rules::new(&repo, &root, &mut || reads += 1);
        let initial = reads;
        for index in 0..1_000 {
            assert!(rules.is_ignored(
                &root.join(format!("src/generated-{index}")),
                true,
                &mut || reads += 1
            ));
            assert!(!rules.is_ignored(
                &root.join(format!("src/source-{index}")),
                true,
                &mut || reads += 1
            ));
        }
        assert!(rules.reliable());
        assert_eq!(
            reads - initial,
            2,
            "only root and src ignore files need reading"
        );
        std::fs::write(root.join("src/.gitignore"), "output*/\n").unwrap();
        rules.invalidate_directories(&[root.join("src")].into_iter().collect());
        assert!(!rules.is_ignored(&root.join("src/generated-0"), true, &mut || reads += 1));
        assert!(rules.is_ignored(&root.join("src/output-0"), true, &mut || reads += 1));
        assert_eq!(reads - initial, 3);
    }

    #[test]
    fn unreadable_ignore_files_cannot_silently_prune_or_traverse_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        // A directory in place of an ignore file fails consistently on both OSes.
        std::fs::create_dir(root.join(".gitignore")).unwrap();
        let mut rules = Rules::new(&repo, &root, &mut || {});
        rules.is_ignored(&root.join("source"), true, &mut || {});
        assert!(
            !rules.reliable(),
            "discovery must ask libgit2 for the authoritative result"
        );
    }
}
