//! Row ownership carries damage through scrolling and screen swaps. Mutable
//! access invalidates only this row; inspecting text or moving it does not.

use super::TermCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ROW: AtomicUsize = AtomicUsize::new(1);

#[inline]
fn next_identity() -> usize {
    loop {
        let identity = NEXT_ROW.fetch_add(1, Ordering::Relaxed);
        if identity != 0 {
            return identity;
        }
    }
}

#[derive(Debug, Default)]
pub struct TermRow {
    cells: Vec<TermCell>,
    identity: usize,
    generation: u64,
}

impl TermRow {
    #[inline]
    pub(crate) fn identity(&self) -> usize {
        self.identity
    }
    #[inline]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    #[inline]
    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.identity == 0 || self.generation == 0 {
            self.identity = next_identity();
        }
    }
}

impl From<Vec<TermCell>> for TermRow {
    fn from(cells: Vec<TermCell>) -> Self {
        Self {
            cells,
            identity: next_identity(),
            generation: 0,
        }
    }
}

impl Clone for TermRow {
    fn clone(&self) -> Self {
        // A copied row can later diverge independently (e.g. ConPTY's saved
        // repaint). It must not share a cache identity with its source.
        self.cells.clone().into()
    }
}

impl PartialEq for TermRow {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells
    }
}

#[cfg(test)]
impl PartialEq<Vec<TermCell>> for TermRow {
    fn eq(&self, other: &Vec<TermCell>) -> bool {
        &self.cells == other
    }
}

impl Deref for TermRow {
    type Target = Vec<TermCell>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.cells
    }
}

impl DerefMut for TermRow {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.changed();
        &mut self.cells
    }
}

impl IntoIterator for TermRow {
    type Item = TermCell;
    type IntoIter = std::vec::IntoIter<TermCell>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.cells.into_iter()
    }
}

impl<'a> IntoIterator for &'a TermRow {
    type Item = &'a TermCell;
    type IntoIter = std::slice::Iter<'a, TermCell>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.cells.iter()
    }
}

impl<'a> IntoIterator for &'a mut TermRow {
    type Item = &'a mut TermCell;
    type IntoIter = std::slice::IterMut<'a, TermCell>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.deref_mut().iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_access_is_clean_and_generation_wrap_changes_identity() {
        let mut row = TermRow::from(vec![TermCell::default(); 4]);
        let key = (row.identity(), row.generation());
        assert_eq!(row.iter().count(), 4);
        assert_eq!(row[0], TermCell::default());
        assert_eq!((row.identity(), row.generation()), key);
        row.generation = u64::MAX;
        row[0].ch = 'x';
        assert_ne!(row.identity(), key.0);
        assert_eq!(row.generation(), 0);
        assert_eq!(row[0].ch, 'x');
    }
}
