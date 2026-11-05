//! Field-local viewport movement, separate from caret, selection, and history.
use super::{TextInputHitMap, TextInputId};
use crate::app::App;
use crossterm::event::{KeyCode, MouseEventKind};
use std::hash::{Hash, Hasher};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputViewport {
    fingerprint: u64,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    pub(super) start: usize,
    end: usize,
    left: usize,
    right: usize,
    shift: isize,
}

impl InputViewport {
    pub(crate) fn scrolled(mut self, kind: MouseEventKind, amount: usize) -> Option<Self> {
        let right = match kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollRight => true,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollLeft => false,
            _ => return None,
        };
        let amount = amount.max(1).min(isize::MAX as usize) as isize;
        self.shift = self
            .shift
            .saturating_add(if right { amount } else { -amount })
            .clamp(
                -(self.left.min(isize::MAX as usize) as isize),
                self.right.min(isize::MAX as usize) as isize,
            );
        Some(self)
    }

    pub(crate) fn new(
        text: &str,
        cursor: Option<usize>,
        selection: Option<(usize, usize)>,
        start: usize,
        end: usize,
    ) -> Self {
        let mut hash = rustc_hash::FxHasher::default();
        text.hash(&mut hash);
        Self {
            fingerprint: hash.finish(),
            cursor,
            selection,
            start,
            end,
            left: 0,
            right: 0,
            shift: 0,
        }
    }

    pub(crate) fn matches(self, other: Self) -> bool {
        self.fingerprint == other.fingerprint
            && self.cursor == other.cursor
            && self.selection == other.selection
    }

    pub(crate) fn window(mut self, text: &str, start: usize, end: usize) -> Self {
        self.start = start;
        self.end = end;
        self.left = text[..start].width();
        self.right = text[end..].width();
        self.shift = 0;
        self
    }

    pub(crate) fn resolve(self, text: &str, width: usize) -> usize {
        let base = App::clamp_to_grapheme_boundary(text, self.start);
        let mut start = base;
        let mut used = 0usize;
        if self.shift < 0 {
            for (byte, glyph) in text[..base].grapheme_indices(true).rev() {
                if used >= self.shift.unsigned_abs() {
                    break;
                }
                start = byte;
                used = used.saturating_add(glyph.width().max(1));
            }
        } else {
            for (byte, glyph) in text[base..].grapheme_indices(true) {
                if used >= self.shift as usize {
                    break;
                }
                start = base + byte + glyph.len();
                used = used.saturating_add(glyph.width().max(1));
            }
        }
        let mut last_start = text.len();
        let mut used = 0;
        for (byte, glyph) in text.grapheme_indices(true).rev() {
            if used + glyph.width() > width {
                break;
            }
            used += glyph.width();
            last_start = byte;
        }
        start.min(last_start)
    }
}

impl TextInputHitMap {
    pub(crate) fn viewport(&self) -> Option<InputViewport> {
        self.horizontal
    }

    pub(crate) fn with_viewport(mut self, viewport: InputViewport) -> Self {
        self.horizontal = Some(viewport);
        self
    }
}

impl App {
    fn input_viewport_key(&self, id: TextInputId) -> (TextInputId, Option<u64>) {
        (
            id,
            matches!(id, TextInputId::SearchFind | TextInputId::SearchReplace)
                .then(|| self.file_search_owner())
                .flatten(),
        )
    }

    pub(crate) fn text_input_viewport(&self, id: TextInputId) -> Option<InputViewport> {
        (self.text_input_viewport_generation == self.workspace_generation)
            .then(|| {
                self.text_input_viewports
                    .get(&self.input_viewport_key(id))
                    .copied()
            })
            .flatten()
    }

    pub(super) fn clear_text_input_viewport(&mut self, id: TextInputId) {
        self.text_input_viewports
            .remove(&self.input_viewport_key(id));
    }

    pub(crate) fn reveal_text_input_navigation(&mut self, id: TextInputId, code: KeyCode) {
        if matches!(
            code,
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
        ) {
            self.clear_text_input_viewport(id);
        }
    }

    pub(super) fn keep_text_input_viewport_cursor(
        &mut self,
        id: TextInputId,
        cursor: usize,
        anchor: Option<usize>,
    ) {
        let key = self.input_viewport_key(id);
        if let Some(viewport) = self.text_input_viewports.get_mut(&key) {
            if cursor >= viewport.start && cursor <= viewport.end {
                viewport.cursor = Some(cursor);
                viewport.selection = Self::inline_selection_range(cursor, anchor);
            } else {
                self.text_input_viewports.remove(&key);
            }
        }
    }

    pub(super) fn register_input_viewport(
        &mut self,
        id: TextInputId,
        owner: Option<u64>,
        hit: &TextInputHitMap,
    ) {
        let key = (id, owner);
        if let Some(previous) = self.text_input_viewports.get(&key).copied() {
            if let Some(current) = hit.horizontal.filter(|current| current.matches(previous)) {
                self.text_input_viewports.insert(key, current);
            } else {
                self.text_input_viewports.remove(&key);
            }
        }
    }

    pub(crate) fn begin_text_input_frame(&mut self) {
        if self.text_input_viewport_generation != self.workspace_generation {
            self.text_input_viewports.clear();
            self.text_input_viewport_generation = self.workspace_generation;
        } else if !self.text_input_viewports.is_empty() {
            let visible: std::collections::HashSet<_> = self
                .text_input_regions
                .iter()
                .map(|region| (region.id, region.file_search_owner))
                .collect();
            self.text_input_viewports
                .retain(|key, _| visible.contains(key));
        }
        self.text_input_regions.clear();
    }

    pub(super) fn scroll_text_input(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        overlay: Option<bool>,
    ) -> bool {
        match kind {
            MouseEventKind::ScrollDown
            | MouseEventKind::ScrollRight
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollLeft => {}
            _ => return false,
        }
        let Some((id, owner, rendered)) = self
            .text_input_regions
            .iter()
            .rev()
            .find(|region| {
                overlay.is_none_or(|scope| self.text_input_is_overlay(region.id) == scope)
                    && (self.text_input_region_is_current(region)
                        || matches!(
                            region.id,
                            TextInputId::SearchFind | TextInputId::SearchReplace
                        ) && self.file_search_bars.iter().any(|bar| {
                            bar.owner == region.file_search_owner
                                && bar.area.contains((column, row).into())
                        }))
                    && region.hit_map.contains(column, row)
            })
            .and_then(|region| {
                region
                    .hit_map
                    .horizontal
                    .map(|view| (region.id, region.file_search_owner, view))
            })
        else {
            return false;
        };
        if self.text_input_viewport_generation != self.workspace_generation {
            self.text_input_viewports.clear();
            self.text_input_viewport_generation = self.workspace_generation;
        }
        let key = (id, owner);
        let viewport = self
            .text_input_viewports
            .get(&key)
            .copied()
            .filter(|view| view.matches(rendered))
            .unwrap_or(rendered);
        let viewport = viewport.scrolled(kind, self.scroll_speed).unwrap();
        self.text_input_viewports.insert(key, viewport);
        true
    }
}
