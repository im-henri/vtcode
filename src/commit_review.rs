//! A virtual, continuous commit review. Only visible, expanded files request
//! revisions. Native editors and diff caches move between cards without copying
//! their source text, and offscreen caches are evicted under a bounded budget.
use std::sync::Arc;

use ratatui::layout::Rect;

use crate::{
    app::{App, DiffMode, DiffRenderCache},
    commit_details::DiffContext,
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
    pub total: usize,
    pub scroll: usize,
    pub paint_row: Option<usize>,
    pub reveal: bool,
    pub mode: Option<DiffMode>,
    pub forced_mode: Option<DiffMode>,
    pub caret: Option<(usize, usize)>,
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
            left_horizontal: app.diff_left_h_scrollbar_area,
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

pub(crate) fn mode(app: &App, width: u16) -> DiffMode {
    app.commit_details.review.forced_mode.unwrap_or_else(|| {
        if width < 100 {
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
            self.collapsed.insert(file);
        }
    }

    pub fn clear_geometry(&mut self) {
        self.heights.clear();
        self.visible.clear();
        self.reveal = true;
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
        if self.diff_render_cache.context_lines != context.changes_only.then_some(context.lines) {
            self.diff_render_cache = if context.changes_only {
                Arc::new(full.with_context(context.lines))
            } else {
                full.clone()
            };
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
