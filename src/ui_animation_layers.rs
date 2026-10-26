use super::Layer;
use ratatui::layout::Rect;
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) const EXCLUDED: u8 = 1;
pub(super) const PROTECTED: u8 = 2;
pub(super) const CARET: u8 = 4;
pub(super) const BLOCKED_ROW: u8 = 8;

#[derive(Clone)]
pub(super) struct CellMask {
    area: Rect,
    flags: Vec<u8>,
}

impl CellMask {
    pub(super) fn new(area: Rect) -> Option<Self> {
        let cells = usize::from(area.width) * usize::from(area.height);
        (cells <= super::MAX_ANCHORS * 4).then(|| Self {
            area,
            flags: vec![0; cells],
        })
    }

    fn index(&self, position: (u16, u16)) -> Option<usize> {
        self.area.contains(position.into()).then(|| {
            usize::from(position.1 - self.area.y) * usize::from(self.area.width)
                + usize::from(position.0 - self.area.x)
        })
    }

    pub(super) fn insert(&mut self, position: (u16, u16), flags: u8) -> bool {
        if let Some(index) = self.index(position) {
            self.flags[index] |= flags;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub(super) fn cells(&self, flags: u8) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.flags.iter().enumerate().filter_map(move |(index, value)| {
            (value & flags != 0).then(|| (
                self.area.x + (index % usize::from(self.area.width)) as u16,
                self.area.y + (index / usize::from(self.area.width)) as u16,
            ))
        })
    }

    pub(super) fn get(&self, position: (u16, u16)) -> Option<u8> {
        self.index(position).map(|index| self.flags[index])
    }

    #[cfg(test)]
    pub(super) fn flags(&self, position: (u16, u16)) -> u8 {
        self.get(position).unwrap_or(0)
    }
}

/// Source occupancy is shared by every scope route in a frame. Terminal
/// coordinates have a small bounded domain; hashing each glyph repeatedly
/// costs more than a direct lookup. Keep a sparse fallback for large areas
/// and anchors outside the code rectangle (including test/diff projections).
#[derive(Clone)]
pub(super) struct SourceCells {
    dense: Option<CellMask>,
    outside: FxHashSet<(u16, u16)>,
}

impl SourceCells {
    pub(super) fn new(area: Rect, anchors: &[super::Anchor]) -> Self {
        let mut cells = Self {
            dense: CellMask::new(area),
            outside: FxHashSet::default(),
        };
        for anchor in anchors.iter().filter(|a| !matches!(a.ch, ' ' | '\t' | '\0')) {
            for x in anchor.x..anchor.x.saturating_add(anchor.width) {
                let position = (x, anchor.y);
                if let Some(dense) = cells.dense.as_mut()
                    .filter(|mask| mask.area.contains(position.into()))
                {
                    dense.insert(position, 1);
                } else {
                    cells.outside.insert(position);
                }
            }
        }
        cells
    }

    pub(super) fn contains(&self, position: (u16, u16)) -> bool {
        self.dense.as_ref().and_then(|mask| mask.get(position))
            .map_or_else(|| self.outside.contains(&position), |flags| flags != 0)
    }
}

// A viewport index avoids hashing each painted cell. Only touched slots are
// cleared, so a small cursor effect does not sweep the entire editor area.
// Unusually large terminal rectangles use the original bounded sparse map.
#[derive(Clone, Default)]
pub(super) struct Layers {
    area: Rect,
    slots: Vec<u32>,
    painted: Vec<((u16, u16), Layer)>,
    sparse: FxHashMap<(u16, u16), Layer>,
}

impl Layers {
    pub(super) fn configure(&mut self, area: Rect) {
        debug_assert!(self.is_empty());
        let cells = usize::from(area.width) * usize::from(area.height);
        if cells <= super::MAX_ANCHORS * 4 {
            self.area = area;
            self.slots.resize(cells, 0);
        } else {
            self.area = Rect::default();
            self.slots.clear();
        }
    }

    fn index(&self, position: (u16, u16)) -> Option<usize> {
        self.area.contains(position.into()).then(|| {
            usize::from(position.1 - self.area.y) * usize::from(self.area.width)
                + usize::from(position.0 - self.area.x)
        })
    }

    pub(super) fn len(&self) -> usize {
        self.painted.len() + self.sparse.len()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.painted.capacity() + self.sparse.capacity()
    }

    pub(super) fn contains_key(&self, position: &(u16, u16)) -> bool {
        self.index(*position).map_or_else(
            || self.sparse.contains_key(position),
            |index| self.slots[index] != 0,
        )
    }

    pub(super) fn get_or_insert(&mut self, position: (u16, u16)) -> &mut Layer {
        if let Some(index) = self.index(position) {
            let slot = &mut self.slots[index];
            if *slot == 0 {
                self.painted.push((position, Layer::default()));
                *slot = self.painted.len() as u32;
            }
            &mut self.painted[*slot as usize - 1].1
        } else {
            self.sparse.entry(position).or_default()
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&(u16, u16), &Layer)> {
        self.painted
            .iter()
            .map(|(position, layer)| (position, layer))
            .chain(self.sparse.iter())
    }

    pub(super) fn clear(&mut self) {
        for &(position, _) in &self.painted {
            let index = self.index(position).unwrap();
            self.slots[index] = 0;
        }
        self.painted.clear();
        self.sparse.clear();
    }
}

#[cfg(test)]
impl std::ops::Index<&(u16, u16)> for Layers {
    type Output = Layer;
    fn index(&self, position: &(u16, u16)) -> &Layer {
        if let Some(index) = self.index(*position) {
            &self.painted[self.slots[index] as usize - 1].1
        } else {
            &self.sparse[position]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_and_dense_layers_agree_after_resize_clear_and_origin_changes() {
        let mut layers = Layers::default();
        for area in [
            Rect::new(4, 7, 80, 40),
            Rect::new(0, 0, 400, 400),
            Rect::new(2, 3, 8, 9),
            Rect::new(1, 5, 80, 40),
        ] {
            layers.configure(area);
            let mut reference = FxHashMap::default();
            for step in 0..6000u32 {
                let position = ((step * 17 % 100) as u16, (step * 31 % 50) as u16);
                layers.get_or_insert(position).scope_connections = (step % 15) as u8;
                reference.insert(position, (step % 15) as u8);
                assert!(layers.contains_key(&position));
            }
            assert_eq!(layers.len(), reference.len());
            for (position, layer) in layers.iter() {
                assert_eq!(layer.scope_connections, reference[position]);
            }
            layers.clear();
            assert!(layers.is_empty());
            assert!(
                reference
                    .keys()
                    .all(|position| !layers.contains_key(position))
            );
            assert!(layers.slots.len() <= super::super::MAX_ANCHORS * 4);
        }
    }
}
