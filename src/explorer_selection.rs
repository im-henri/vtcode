//! Keep selection order for file actions, with a lazy membership index for
//! redraws. Mutating access invalidates the index even for same-length edits.
use std::{
    cell::RefCell,
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};

#[derive(Debug, Default)]
pub(crate) struct Selection {
    paths: Vec<PathBuf>,
    index: RefCell<Vec<(u64, usize)>>,
    #[cfg(test)]
    builds: std::cell::Cell<usize>,
}

impl Selection {
    pub(crate) fn contains(&self, path: &Path) -> bool {
        if self.paths.len() <= 32 {
            return self.paths.iter().any(|selected| selected == path);
        }
        let mut index = self.index.borrow_mut();
        if index.len() != self.paths.len() {
            index.clear();
            index.extend(
                self.paths
                    .iter()
                    .enumerate()
                    .map(|(i, path)| (path_hash(path), i)),
            );
            index.sort_unstable_by_key(|&(hash, _)| hash);
            #[cfg(test)]
            self.builds.set(self.builds.get() + 1);
        }
        self.index_contains(&index, path_hash(path), path)
    }

    fn index_contains(&self, index: &[(u64, usize)], hash: u64, path: &Path) -> bool {
        let first = index.partition_point(|&(key, _)| key < hash);
        index[first..]
            .iter()
            .take_while(|&&(key, _)| key == hash)
            .any(|&(_, i)| self.paths[i] == path)
    }
}

fn path_hash(path: &Path) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    path.hash(&mut hasher);
    hasher.finish()
}

impl Deref for Selection {
    type Target = Vec<PathBuf>;
    fn deref(&self) -> &Self::Target {
        &self.paths
    }
}

impl DerefMut for Selection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.index.get_mut().clear();
        &mut self.paths
    }
}

impl From<Vec<PathBuf>> for Selection {
    fn from(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }
}

impl FromIterator<PathBuf> for Selection {
    fn from_iter<T: IntoIterator<Item = PathBuf>>(paths: T) -> Self {
        paths.into_iter().collect::<Vec<_>>().into()
    }
}

impl Clone for Selection {
    fn clone(&self) -> Self {
        self.paths.clone().into()
    }
}

impl AsRef<[PathBuf]> for Selection {
    fn as_ref(&self) -> &[PathBuf] {
        &self.paths
    }
}

impl<T: AsRef<[PathBuf]>> PartialEq<T> for Selection {
    fn eq(&self, other: &T) -> bool {
        self.paths == other.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_reuses_storage_and_invalidates_all_mutating_access() {
        let mut selection: Selection = (0..1000)
            .map(|i| PathBuf::from(format!("root/file{i}.rs")))
            .collect();
        let first = selection.paths.clone();
        for _ in 0..3 {
            for path in &first {
                assert!(selection.contains(path));
            }
            assert!(!selection.contains(Path::new("missing")));
        }
        assert_eq!(selection.builds.get(), 1);
        let allocation = selection.index.borrow().as_ptr();
        selection[300] = "changed".into();
        assert!(!selection.contains(&first[300]));
        assert!(selection.contains(Path::new("changed")));
        assert_eq!(selection.builds.get(), 2);
        assert_eq!(selection.index.borrow().as_ptr(), allocation);
        selection.retain(|path| path != Path::new("changed"));
        assert!(!selection.contains(Path::new("changed")));
        selection.reverse();
        assert!(selection.contains(&first[999]));
        assert_eq!(selection[0], first[999]);
        let copied = selection.clone();
        assert_eq!(copied, selection);
        selection.clear();
        assert!(!selection.contains(&first[999]));
        assert!(copied.contains(&first[999]));
    }

    #[test]
    fn membership_preserves_path_equality_duplicates_and_hash_collision_checks() {
        let mut selection: Selection = vec![PathBuf::from("root/./界.rs"); 40].into();
        assert!(selection.contains(Path::new("root/界.rs")));
        assert!(!selection.contains(Path::new("root/other.rs")));
        selection[1] = "second".into();
        let collisions = vec![(7, 0), (7, 1), (7, 2)];
        assert!(selection.index_contains(&collisions, 7, Path::new("second")));
        assert!(!selection.index_contains(&collisions, 7, Path::new("missing")));
        selection.truncate(1);
        assert!(selection.contains(Path::new("root/界.rs")));
        assert!(!selection.contains(Path::new("second")));
    }

    #[test]
    #[ignore = "manual optimized-profile cold and warm Explorer membership benchmark"]
    fn benchmark_explorer_membership_index() {
        use std::{hint::black_box, time::Instant};
        let paths: Vec<_> = (0..20_000)
            .map(|i| PathBuf::from(format!("workspace/file-{i:05}界.rs")))
            .collect();
        let selection: Selection = paths
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 5 != 0)
            .map(|(_, path)| path.clone())
            .collect();
        let visible = &paths[19_950..];
        let mut cold = Vec::new();
        let mut warm = Vec::new();
        for _ in 0..7 {
            selection.index.borrow_mut().clear();
            let start = Instant::now();
            for path in visible {
                black_box(selection.contains(path));
            }
            cold.push(start.elapsed());
            let start = Instant::now();
            for _ in 0..1000 {
                for path in visible {
                    black_box(selection.contains(path));
                }
            }
            warm.push(start.elapsed() / 1000);
        }
        cold.sort();
        warm.sort();
        eprintln!(
            "Explorer 16k selected / 50 visible: rebuild+queries={:?} warm_queries={:?}",
            cold[3], warm[3]
        );
    }
}
