//! A virtual, continuous commit review. Only visible, expanded files request
//! revisions. Native editors and diff caches move between cards without copying
//! their source text, and offscreen caches are evicted under a bounded budget.
use std::{ops::Range, sync::Arc};

use ratatui::layout::Rect;

use crate::{
    app::{App, DiffMode, DiffRenderCache},
    commit_details::DiffContext,
    ui_editor_highlight::horizontal_scrollbar_thumb,
};

#[derive(Default)]
pub(crate) struct Review {
    pub collapsed: rustc_hash::FxHashSet<usize>,
    pub cached: rustc_hash::FxHashMap<usize, crate::diff_tabs::CommitFileView>,
    pub errors: rustc_hash::FxHashMap<usize, String>,
    pub pending: Option<usize>,
    /// Heights include the card header, revision labels, and breathing room.
    pub heights: rustc_hash::FxHashMap<usize, usize>,
    pub visible: Vec<Card>,
    pub context_buttons: Vec<ContextButton>,
    pub horizontal_drag: Option<HorizontalDrag>,
    /// Ranges in each full diff, kept even when its rendered cache is evicted.
    pub expanded_context: rustc_hash::FxHashMap<usize, Vec<Range<usize>>>,
    pub total: usize,
    pub scroll: usize,
    pub paint_row: Option<usize>,
    pub reveal: bool,
    pub mode: Option<DiffMode>,
    pub forced_mode: Option<DiffMode>,
    pub caret: Option<(usize, usize, usize)>,
    pub clock: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Card {
    pub file: usize,
    pub start: usize,
    pub header: Rect,
    pub body: Rect,
    pub open: Rect,
    pub first_row: usize,
    pub geometry: Geometry,
    pub scrollbars: [Option<HorizontalScrollbar>; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiffSide {
    Original,
    Target,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HorizontalScrollbar {
    pub area: Rect,
    pub side: DiffSide,
    pub total: usize,
    pub offset: usize,
}

impl HorizontalScrollbar {
    fn new(area: Option<Rect>, side: DiffSide, total: usize, offset: usize) -> Option<Self> {
        let area = area.filter(|area| !area.is_empty() && total > usize::from(area.width))?;
        Some(Self {
            area,
            side,
            total,
            offset,
        })
    }

    pub fn maximum(self) -> usize {
        self.total.saturating_sub(usize::from(self.area.width))
    }

    fn thumb(self) -> Option<Range<usize>> {
        let width = usize::from(self.area.width);
        horizontal_scrollbar_thumb(width, self.total, width, self.offset)
    }

    fn offset_at(self, column: u16, grab: usize, travel: usize) -> usize {
        if travel == 0 {
            return self.offset;
        }
        let position = usize::from(column.saturating_sub(self.area.x))
            .saturating_sub(grab)
            .min(travel);
        // The painted thumb rounds down. Invert that mapping to keep the grip
        // under the pointer and reach the exact ends of the text.
        (position as u128 * self.maximum() as u128).div_ceil(travel as u128) as usize
    }

    pub fn same_track(self, other: Self) -> bool {
        self.area == other.area && self.side == other.side && self.total == other.total
    }
}

#[derive(Clone, Copy)]
pub(crate) struct HorizontalDrag {
    pub file: usize,
    pub bar: HorizontalScrollbar,
    column: u16,
    offset: usize,
    grab: usize,
    travel: usize,
}

impl HorizontalDrag {
    pub fn start(file: usize, bar: HorizontalScrollbar, column: u16) -> Option<Self> {
        let thumb = bar.thumb()?;
        let travel = usize::from(bar.area.width) - thumb.len();
        let relative = usize::from(column.saturating_sub(bar.area.x));
        let (grab, offset) = if thumb.contains(&relative) {
            (relative - thumb.start, bar.offset)
        } else {
            let grab = thumb.len() / 2;
            (grab, bar.offset_at(column, grab, travel))
        };
        Some(Self {
            file,
            bar,
            column,
            offset,
            grab,
            travel,
        })
    }

    pub fn offset_at(self, column: u16) -> usize {
        if column == self.column {
            self.offset
        } else {
            self.bar.offset_at(column, self.grab, self.travel)
        }
    }
}

pub(crate) fn target_scroll_width(cache: &DiffRenderCache, aligned: bool) -> usize {
    if aligned {
        cache
            .max_right_text_width
            .saturating_add(cache.change_marker_width())
    } else {
        cache.max_inline_text_width
    }
}

pub(crate) fn capture_scrollbars(app: &App, aligned: bool) -> [Option<HorizontalScrollbar>; 2] {
    let cache = &app.diff_render_cache;
    let original = if aligned {
        HorizontalScrollbar::new(
            app.diff_left_h_scrollbar_area,
            DiffSide::Original,
            cache.max_left_text_width,
            app.diff_left_horizontal_scroll,
        )
    } else {
        None
    };
    let target = HorizontalScrollbar::new(
        app.h_scrollbar_area,
        DiffSide::Target,
        target_scroll_width(cache, aligned),
        app.editor().map_or(0, |editor| editor.horizontal_scroll),
    );
    [original, target]
}

pub(crate) const CONTEXT_STEP: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextExpansion {
    FromStart,
    All,
    FromEnd,
}

impl ContextExpansion {
    pub(crate) fn range(self, gap: &Range<usize>) -> Range<usize> {
        let count = gap.len().min(CONTEXT_STEP);
        match self {
            Self::FromStart => gap.start..gap.start + count,
            Self::All => gap.clone(),
            Self::FromEnd => gap.end - count..gap.end,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ContextButton {
    pub area: Rect,
    pub file: usize,
    pub aligned_row: usize,
    pub gap: Range<usize>,
    pub expansion: ContextExpansion,
}

/// Native input must use the geometry of the card clicked, not the last card
/// drawn. Keep only small rectangles here, never another document snapshot.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Geometry {
    code: Option<Rect>,
    gutter: Option<Rect>,
    gutter_width: u16,
    horizontal: Option<Rect>,
    left_horizontal: Option<Rect>,
    vertical: Option<Rect>,
    minimap: Option<Rect>,
    split: Option<Rect>,
    divider: Option<Rect>,
    left: Option<Rect>,
}

impl Geometry {
    pub fn capture(app: &App) -> Self {
        Self {
            code: app.code_area,
            gutter: app.gutter_area,
            gutter_width: app.gutter_width,
            horizontal: app.h_scrollbar_area,
            left_horizontal: if app.diff_mode == DiffMode::SideBySide {
                app.diff_left_h_scrollbar_area
            } else {
                None
            },
            vertical: app.scrollbar_area,
            minimap: app.minimap_area,
            split: app.diff_split_area,
            divider: app.diff_divider_area,
            left: app.diff_left_code_area,
        }
    }
    pub fn install(self, app: &mut App) {
        app.code_area = self.code;
        app.gutter_area = self.gutter;
        app.gutter_width = self.gutter_width;
        app.h_scrollbar_area = self.horizontal;
        app.diff_left_h_scrollbar_area = self.left_horizontal;
        app.diff_split_area = self.split;
        app.diff_divider_area = self.divider;
        app.diff_left_code_area = self.left;
        app.minimap_area = self.minimap;
        app.scrollbar_area = self.vertical;
    }
}

pub(crate) fn rows(cache: &DiffRenderCache, mode: DiffMode) -> usize {
    if mode == DiffMode::SideBySide {
        cache.rows.len()
    } else {
        cache.inline_rows.len()
    }
}

pub(crate) fn file_height(
    scroll: &mut crate::inline_diff::InlineDiffScroll,
    cache: &Arc<DiffRenderCache>,
    editor: &crate::editor::Editor,
    mode: DiffMode,
    width: u16,
    split_ratio: f32,
    animations: &crate::animation_config::AnimationSettings,
) -> usize {
    let widths = crate::ui_diff_display::measure_wrap(
        cache,
        editor,
        mode,
        width,
        split_ratio,
        None,
        animations.scope_enabled_for(editor),
    );
    scroll.prepare_wrap(cache, mode == DiffMode::SideBySide, widths);
    scroll
        .wrap
        .first_row(rows(cache, mode))
        .max(1)
        .saturating_add(4)
}

pub(crate) fn mode(app: &App, width: u16) -> DiffMode {
    app.commit_details.review.forced_mode.unwrap_or_else(|| {
        if width < 100 && !app.has_diff_layout_preference() {
            DiffMode::Inline
        } else {
            app.preferred_diff_mode
        }
    })
}

pub(crate) fn painting_row(app: &App) -> Option<usize> {
    app.commit_details.review.paint_row
}

impl Review {
    pub fn height(&self, file: usize) -> usize {
        if self.collapsed.contains(&file) {
            1
        } else {
            self.heights.get(&file).copied().unwrap_or(5).max(4)
        }
    }

    pub fn toggle(&mut self, file: usize) {
        if !self.collapsed.remove(&file) {
            self.collapse(file);
        }
    }

    pub fn collapse(&mut self, file: usize) {
        self.collapsed.insert(file);
        self.expanded_context.remove(&file);
        self.context_buttons.retain(|button| button.file != file);
    }

    pub fn clear_geometry(&mut self) {
        self.heights.clear();
        self.visible.clear();
        self.context_buttons.clear();
        self.horizontal_drag = None;
        self.reveal = true;
    }

    pub fn expanded_context_for(&self, file: usize) -> &[Range<usize>] {
        self.expanded_context.get(&file).map_or(&[], Vec::as_slice)
    }

    pub fn expand_context(&mut self, file: usize, mut range: Range<usize>) {
        let ranges = self.expanded_context.entry(file).or_default();
        let first = ranges.partition_point(|existing| existing.end < range.start);
        let end = first + ranges[first..].partition_point(|existing| existing.start <= range.end);
        if first < end {
            range.start = range.start.min(ranges[first].start);
            range.end = range.end.max(ranges[end - 1].end);
        }
        ranges.splice(first..end, [range]);
    }
}

impl App {
    pub(crate) fn clear_commit_review_files(&mut self) {
        for (_, view) in std::mem::take(&mut self.commit_details.review.cached) {
            view.discard(&mut self.animations);
        }
    }
    /// Keep the old native editor/view intact when another file is activated.
    /// Moving the rope, diff rows and selection avoids rebuilding or duplicating
    /// revisions on clicks and on every paint of a neighbouring card.
    pub(crate) fn stash_commit_review_file(&mut self) {
        let Some(file) = self.commit_details.diff_file else {
            return;
        };
        if let Some(mut view) = self.take_commit_file_view() {
            self.commit_details.review.clock = self.commit_details.review.clock.wrapping_add(1);
            view.last_used = self.commit_details.review.clock;
            self.commit_details.review.cached.insert(file, view);
        }
    }

    pub(crate) fn activate_commit_review_file(&mut self, file: usize) -> bool {
        if self.commit_details.diff_file == Some(file)
            && self.commit_details.diff_full_cache.is_some()
        {
            return true;
        }
        if !self.commit_details.review.cached.contains_key(&file) {
            return false;
        }
        self.stash_commit_review_file();
        let view = self.commit_details.review.cached.remove(&file).unwrap();
        self.put_commit_file_view(view);
        self.commit_details.diff_file = Some(file);
        self.commit_details.diff_loading = false;
        self.commit_details.diff_error = None;
        true
    }

    pub(crate) fn prepare_commit_review_context(&mut self) {
        let context = self.commit_details.diff_context;
        let Some(full) = self.commit_details.diff_full_cache.as_ref() else {
            return;
        };
        let expanded = self.commit_details.diff_file.map_or(&[][..], |file| {
            self.commit_details.review.expanded_context_for(file)
        });
        if !self
            .diff_render_cache
            .matches_context(context.changes_only.then_some(context.lines), expanded)
        {
            self.diff_render_cache = context.project(full, None, expanded);
            self.commit_details.context_revealed_line = None;
            self.diff_inline_scroll = Default::default();
        }
    }

    pub(crate) fn evict_commit_review_files(&mut self) {
        const BUDGET: usize = 32 * 1024 * 1024;
        let review = &mut self.commit_details.review;
        let mut bytes: usize = review.cached.values().map(|view| view.bytes).sum();
        while review.cached.len() > 8 || bytes > BUDGET {
            let victim = review
                .cached
                .iter()
                .filter(|(file, _)| !review.visible.iter().any(|card| card.file == **file))
                .min_by_key(|(_, view)| view.last_used)
                .map(|(&file, _)| file);
            let Some(victim) = victim else {
                break;
            };
            let view = review.cached.remove(&victim).unwrap();
            bytes = bytes.saturating_sub(view.bytes);
            view.discard(&mut self.animations);
        }
    }

    pub(crate) fn reset_commit_review_context(&mut self, old: DiffContext) {
        if self.commit_details.diff_context != old {
            self.commit_details.review.clear_geometry();
        }
    }
}
