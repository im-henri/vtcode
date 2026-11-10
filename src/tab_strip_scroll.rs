//! Horizontal cell scrolling shared by active-tab reveal and manual panning.
use super::{
    App, CloseOverlay, Rect, TabBarHits, TabNameDuplicates, tab_label_name, tab_label_width,
};
use crate::app::Tab;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StripScroll {
    pub(crate) anchor: u64,
    pub(crate) columns: usize,
    pub(crate) manual: bool,
}

fn group_state(app: &App, group: usize) -> Option<(&[Tab], usize, usize, Option<StripScroll>)> {
    if group == app.active_group && app.groups.get(group).is_none_or(|g| g.tabs.is_empty()) {
        Some((
            &app.tabs,
            app.active_tab,
            app.tab_scroll_offset,
            app.tab_strip_scroll,
        ))
    } else {
        app.groups.get(group).map(|g| {
            (
                g.tabs.as_slice(),
                g.active_tab,
                g.tab_scroll_offset,
                g.tab_strip_scroll,
            )
        })
    }
}

pub(crate) fn position(app: &App, group: usize) -> Option<StripScroll> {
    let (tabs, active, _, scroll) = group_state(app, group)?;
    scroll.filter(|scroll| {
        tabs.get(active)
            .is_some_and(|tab| tab.editor.navigation_id == scroll.anchor)
    })
}

#[derive(Clone, Copy)]
enum Kind {
    Tab(usize),
    Group(u64),
    New,
}

struct Item {
    kind: Kind,
    start: usize,
    width: usize,
    close: Option<CloseOverlay>,
}

struct Strip {
    items: Vec<Item>,
    width: usize,
    prefix: u16,
}

impl Strip {
    fn new(app: &App, area: Rect, group: usize, names: Option<&TabNameDuplicates>) -> Self {
        let mut strip = Self {
            items: Vec::new(),
            width: 0,
            prefix: crate::ui_desks::controls(app, area, group).2,
        };
        let Some((tabs, active, _, _)) = group_state(app, group) else {
            return strip;
        };
        let owned_names;
        let names = if names.is_some() {
            names
        } else {
            owned_names = TabNameDuplicates::for_scroll(app, tabs, active);
            owned_names.as_ref()
        };
        let mut counts = rustc_hash::FxHashMap::<u64, usize>::default();
        if !app.desks.membership.is_empty() {
            for tab in tabs {
                if let Some(named) = app.tab_group(tab) {
                    *counts.entry(named.id).or_default() += 1;
                }
            }
        }
        let mut last_group = None;
        for (index, tab) in tabs.iter().enumerate() {
            let named = app.tab_group(tab);
            if let Some(named) = named
                && last_group != Some(named.id)
            {
                let count = counts.get(&named.id).copied().unwrap_or(0);
                strip.push(
                    Kind::Group(named.id),
                    (named.name.width() + count.to_string().len() + 6).min(25),
                    None,
                );
            }
            last_group = named.map(|g| g.id);
            if named.is_some_and(|g| g.collapsed) && index != active && !tab.pinned {
                continue;
            }
            let name = tab_label_name(app, area, group, index, tab, names);
            let width = tab_label_width(app, tab, &name);
            strip.push(
                Kind::Tab(index),
                width,
                CloseOverlay::for_tab(tab, &name, width),
            );
        }
        strip.push(Kind::New, 3, None);
        strip
    }

    fn push(&mut self, kind: Kind, width: usize, close: Option<CloseOverlay>) {
        self.items.push(Item {
            kind,
            start: self.width,
            width,
            close,
        });
        self.width = self.width.saturating_add(width);
    }

    fn available(&self, area: Rect) -> usize {
        usize::from(area.width.saturating_sub(self.prefix))
    }

    fn max_scroll(&self, area: Rect) -> usize {
        let width = self.available(area);
        if width == 0 || self.width <= width {
            return 0;
        }
        self.width
            .saturating_sub(width.saturating_sub(2.min(width.saturating_sub(1))))
    }

    fn initial_scroll(&self, tab_offset: usize) -> usize {
        if tab_offset == 0 {
            return 0;
        }
        self.items
            .iter()
            .find(|item| matches!(item.kind, Kind::Tab(index) if index >= tab_offset))
            .map_or(0, |item| item.start)
    }

    fn viewport(&self, area: Rect, columns: usize) -> (Rect, usize, usize) {
        let available = self.available(area);
        let left = if columns > 0 {
            2.min(available.saturating_sub(1))
        } else {
            0
        };
        let right = if columns.saturating_add(available.saturating_sub(left)) < self.width {
            2.min(available.saturating_sub(left + 1))
        } else {
            0
        };
        (
            Rect::new(
                area.x
                    .saturating_add(self.prefix)
                    .saturating_add(left as u16),
                area.y,
                available.saturating_sub(left + right) as u16,
                area.height,
            ),
            left,
            right,
        )
    }
}

/// Move only the cells necessary to reveal the active label. In particular,
/// opening on the right keeps the tail of the previous tab at the left edge.
pub(crate) fn reveal(app: &mut App, area: Rect, group: usize) {
    let Some((tabs, active, tab_offset, previous)) = group_state(app, group) else {
        return;
    };
    let Some(tab) = tabs.get(active) else {
        return;
    };
    let anchor = tab.editor.navigation_id;
    let manual = if group == app.active_group {
        app.tab_scroll_manual_anchor == Some(active)
    } else {
        previous.is_some_and(|scroll| scroll.manual && scroll.anchor == anchor)
    };
    if manual {
        return;
    }
    let strip = Strip::new(app, area, group, None);
    if strip.available(area) == 0 {
        return;
    }
    let Some(target) = strip
        .items
        .iter()
        .find(|item| matches!(item.kind, Kind::Tab(index) if index == active))
    else {
        return;
    };
    let max = strip.max_scroll(area);
    let mut columns = previous
        .map_or_else(|| strip.initial_scroll(tab_offset), |scroll| scroll.columns)
        .min(max);
    if target.start < columns
        || target.width > usize::from(strip.viewport(area, target.start).0.width)
    {
        columns = target.start.min(max);
    } else if target.start + target.width
        > columns + usize::from(strip.viewport(area, columns).0.width)
    {
        // From column one onward the right edge is monotonic, even when the
        // right arrow disappears. Find the first viewport containing the end.
        let mut first = columns.max(1);
        let mut last = max;
        while first < last {
            let middle = first + (last - first) / 2;
            if middle + usize::from(strip.viewport(area, middle).0.width)
                >= target.start + target.width
            {
                last = middle;
            } else {
                first = middle + 1;
            }
        }
        columns = first.min(max);
    }
    let offset = strip
        .items
        .iter()
        .find_map(|item| match item.kind {
            Kind::Tab(index) if item.start + item.width > columns => Some(index),
            _ => None,
        })
        .unwrap_or(0);
    let scroll = Some(StripScroll {
        anchor,
        columns,
        manual: false,
    });
    if group == app.active_group {
        app.tab_strip_scroll = scroll;
        app.tab_scroll_offset = offset;
    } else if let Some(group) = app.groups.get_mut(group) {
        if group.tab_strip_scroll != scroll {
            group.inactive_chrome_cache = None;
        }
        group.tab_strip_scroll = scroll;
        group.tab_scroll_offset = offset;
    }
}

pub(crate) fn scroll_by(app: &mut App, area: Rect, group: usize, columns: isize) {
    let Some((tabs, active, tab_offset, _)) = group_state(app, group) else {
        return;
    };
    let Some(tab) = tabs.get(active) else {
        return;
    };
    let anchor = tab.editor.navigation_id;
    let strip = Strip::new(app, area, group, None);
    let max = strip.max_scroll(area);
    let current = position(app, group)
        .map_or_else(|| strip.initial_scroll(tab_offset), |scroll| scroll.columns)
        .min(max);
    let scroll = StripScroll {
        anchor,
        columns: current.saturating_add_signed(columns).min(max),
        manual: true,
    };
    if group == app.active_group {
        app.tab_strip_scroll = Some(scroll);
        app.tab_scroll_manual_anchor = Some(app.active_tab);
    } else if let Some(group) = app.groups.get_mut(group) {
        group.tab_strip_scroll = Some(scroll);
        group.inactive_chrome_cache = None;
    }
}

pub(super) fn hits(
    app: &App,
    area: Rect,
    group: usize,
    scroll: StripScroll,
    names: Option<&TabNameDuplicates>,
) -> TabBarHits {
    let strip = Strip::new(app, area, group, names);
    let available = strip.available(area);
    let columns = scroll.columns.min(strip.max_scroll(area));
    let (viewport, left, right) = strip.viewport(area, columns);
    let empty = Rect::default();
    let mut hits = TabBarHits {
        tabs: Vec::new(),
        groups: Vec::new(),
        left: empty,
        right: empty,
        new_tab: empty,
        after_tabs: empty,
        tab_clips: Vec::new(),
        tab_widths: Vec::new(),
        group_clips: Vec::new(),
        new_tab_clip: 0,
    };
    if available == 0 || area.height == 0 {
        return hits;
    }
    hits.left = Rect::new(area.x + strip.prefix, area.y, left as u16, 1);
    hits.right = Rect::new(
        area.right().saturating_sub(right as u16),
        area.y,
        right as u16,
        1,
    );
    let clip = |start: usize, width: usize| {
        let first = start.max(columns);
        let end = start
            .saturating_add(width)
            .min(columns.saturating_add(usize::from(viewport.width)));
        if end <= first {
            return (empty, 0);
        }
        (
            Rect::new(
                viewport.x + (first - columns) as u16,
                area.y,
                (end - first) as u16,
                1,
            ),
            first - start,
        )
    };
    for item in &strip.items {
        let (rect, inset) = clip(item.start, item.width);
        if rect.is_empty() {
            continue;
        }
        match item.kind {
            Kind::Tab(index) => {
                let close = item.close.map_or(empty, |close| close.rect(rect, inset));
                hits.tabs.push((index, rect, close));
                hits.tab_clips.push(inset);
                hits.tab_widths.push(item.width);
            }
            Kind::Group(id) => {
                hits.groups.push((id, rect));
                hits.group_clips.push((inset, item.width));
            }
            Kind::New => {
                hits.new_tab = rect;
                hits.new_tab_clip = inset;
            }
        }
    }
    if right == 0 {
        let end = strip.width.saturating_sub(3);
        hits.after_tabs = clip(
            end,
            columns
                .saturating_add(usize::from(viewport.width))
                .saturating_sub(end),
        )
        .0;
    }
    hits
}

#[cfg(test)]
#[path = "tab_strip_scroll_tests.rs"]
mod tests;
