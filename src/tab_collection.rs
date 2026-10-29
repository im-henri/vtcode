use crate::app::Tab;

/// Compact a tab strip once and remap its selection/history once. The common
/// case where nothing is removed does not allocate an index map.
pub(crate) fn retain(
    tabs: &mut Vec<Tab>,
    active: &mut usize,
    mru: &mut Vec<usize>,
    mut keep: impl FnMut(usize, &mut Tab) -> bool,
) {
    let len = tabs.len();
    let mut map = Vec::new();
    let mut old = 0;
    let mut next = 0;
    tabs.retain_mut(|tab| {
        let retained = keep(old, tab);
        if !retained && map.is_empty() {
            map.extend(0..len);
        }
        if !map.is_empty() {
            map[old] = if retained { next } else { usize::MAX };
        }
        next += usize::from(retained);
        old += 1;
        retained
    });
    if map.is_empty() {
        return;
    }
    mru.retain_mut(|index| {
        *index = map.get(*index).copied().unwrap_or(usize::MAX);
        *index != usize::MAX
    });
    *active = map
        .get(*active)
        .copied()
        .filter(|&i| i != usize::MAX)
        .or_else(|| mru.first().copied())
        .unwrap_or((*active).min(tabs.len().saturating_sub(1)));
}

pub(crate) fn removed(mru: &mut Vec<usize>, index: usize) {
    mru.retain_mut(|i| {
        if *i == index {
            return false;
        }
        *i -= usize::from(*i > index);
        true
    });
}

pub(crate) fn swapped(mru: &mut [usize], left: usize, right: usize) {
    for index in mru {
        if *index == left {
            *index = right;
        } else if *index == right {
            *index = left;
        }
    }
}

/// Fill missing entries in linear time, retaining recency and dropping stale
/// or duplicate indices. The active tab leads the activation history.
pub(crate) fn complete_mru(mru: &mut Vec<usize>, active: usize, count: usize) {
    let mut seen = vec![false; count];
    mru.retain(|&index| index < count && !std::mem::replace(&mut seen[index], true));
    if active < count {
        if let Some(index) = mru.iter().position(|&index| index == active) {
            mru[..=index].rotate_right(1);
        } else {
            mru.insert(0, active);
            seen[active] = true;
        }
    }
    mru.extend(
        seen.iter()
            .enumerate()
            .filter_map(|(index, &seen)| (!seen).then_some(index)),
    );
}

#[cfg(test)]
#[path = "tab_collection_tests.rs"]
mod tests;
