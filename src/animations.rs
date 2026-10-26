//! Finite, viewport-only animation state. Text changes are observed after editing;
//! this module never changes document contents or delays an editing operation.
use std::{
    cell::RefCell,
    collections::HashMap,
    time::{Duration, Instant},
};

use ratatui::layout::Rect;
use ropey::Rope;

use crate::animation_config::{AnimationEffect as Effect, AnimationSettings};
use crate::cursor_fire::{FireParticle, FireTrail};
use crate::cursor_smear::{SmearMotion, SmearSample};

const MAX_VIEWS: usize = 16;
const MAX_EVENTS: usize = 24;
const MAX_INSERT_EVENTS: usize = 8;
const MAX_COMPARE_CHARS: usize = 32 * 1024;

type ScopePair = ((usize, usize), (usize, usize));

#[path = "animation_scope_edits.rs"]
mod scope_edits;
use scope_edits::ScopeEdit;

struct ScopeSourceCache {
    rules: std::sync::Arc<crate::scope_bubble_config::Rules>,
    source: Rope,
    version: u64,
    language: Option<String>,
    path: Option<std::path::PathBuf>,
    previous: Option<Rope>,
    edit: Option<ScopeEdit>,
    depths: HashMap<ScopePair, ScopeDepth>,
}

#[derive(Clone, Copy)]
struct ScopeDepth {
    value: Option<usize>,
    verified: bool,
}

impl ScopeSourceCache {
    fn rebase(&self, old: &Rope, pair: ScopePair) -> Option<ScopePair> {
        self.previous
            .as_ref()
            .filter(|source| source.is_instance(old))?;
        self.edit?.pair(old, &self.source, pair)
    }
}

struct ScopeCache {
    rules: std::sync::Arc<crate::scope_bubble_config::Rules>,
    version: u64,
    source: Rope,
    indexed: bool,
    indexed_only: bool,
    line: usize,
    col: usize,
    language: Option<String>,
    path: Option<std::path::PathBuf>,
    result: Option<ScopePair>,
    keyword: Option<Option<ScopePair>>,
    keyword_index: Option<crate::animation_scope::KeywordScopeIndex>,
    immediate: Option<crate::animation_structural_scope::ImmediateScopeIndex>,
}

impl ScopeCache {
    fn matches_source(&self, editor: &crate::editor::Editor) -> bool {
        self.version == editor.content_version
            && self.source.is_instance(&editor.rope)
            && self.language.as_deref() == editor.syntax_language_token()
            && self.path.as_deref() == editor.syntax_file_path()
            && std::sync::Arc::ptr_eq(&self.rules, &editor.scope_bubble_rules)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub line: usize,
    pub col: usize,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub ch: char,
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub id: u64,
    pub rope: Rope,
    pub cursor: (usize, usize),
    pub offset: usize,
    pub content_version: u64,
    pub undo_revision: u64,
    pub scroll: (usize, usize, usize),
    pub selection: Option<(usize, usize, usize, usize)>,
    pub area: Rect,
}

impl Snapshot {
    /// Immutable comparison motion uses mapped source positions without
    /// constructing a document or inferring edit ranges. Reuse the empty Rope
    /// required by the editing snapshot rather than allocating it per frame.
    pub(crate) fn read_only_motion(
        id: u64,
        cursor: (usize, usize),
        scroll: (usize, usize, usize),
        area: Rect,
    ) -> Self {
        static EMPTY: std::sync::OnceLock<Rope> = std::sync::OnceLock::new();
        Self {
            id,
            rope: EMPTY.get_or_init(Rope::new).clone(),
            cursor,
            offset: 0,
            content_version: 0,
            undo_revision: 0,
            scroll,
            selection: None,
            area,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MotionPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TabIndicator {
    pub left: f32,
    pub right: f32,
}

#[derive(Default)]
struct TabIndicatorState {
    id: Option<u64>,
    area: Rect,
    motion: Option<crate::tab_indicator_motion::TabIndicatorMotion>,
    viewport: Rect,
    scroll_offset: usize,
    started: Option<Instant>,
    visible: bool,
    active: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Trigger {
    Cursor {
        motion: SmearMotion,
    },
    Insert {
        start: usize,
        end: usize,
        paste: bool,
    },
    Scope {
        pair: ScopePair,
        origin: (usize, usize),
        exiting: Option<Instant>,
    },
}

impl Trigger {
    fn enabled(&self, settings: &AnimationSettings) -> bool {
        match self {
            Self::Cursor { .. } => {
                settings.enabled(Effect::CursorSmear) || settings.enabled(Effect::CursorFire)
            }
            Self::Insert { .. } => settings.enabled(Effect::WhitespaceReveal),
            Self::Scope { .. } => settings.enabled(Effect::ScopeConnector),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AnimationEvent {
    pub trigger: Trigger,
    pub started: Instant,
}

// Rendering takes ownership of the retained event buffer and returns it after
// painting. Tests keep the convenient owned snapshot API for interaction
// assertions; the production observer never clones events or allocates here.
#[cfg(test)]
type ObservedEvents = Vec<AnimationEvent>;
#[cfg(not(test))]
type ObservedEvents = ();

fn observed_events(events: &[AnimationEvent]) -> ObservedEvents {
    #[cfg(test)]
    {
        events.to_vec()
    }
    #[cfg(not(test))]
    {
        let _ = events;
    }
}

impl AnimationEvent {
    pub fn progress(&self, settings: &AnimationSettings, now: Instant) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32()
            / self.lifetime(settings).as_secs_f32())
        .clamp(0.0, 1.0)
    }

    pub(crate) fn effect_progress(
        &self,
        settings: &AnimationSettings,
        effect: Effect,
        now: Instant,
    ) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32()
            / effect_lifetime(settings, effect).as_secs_f32())
        .clamp(0.0, 1.0)
    }

    fn lifetime(&self, settings: &AnimationSettings) -> Duration {
        match self.trigger {
            Trigger::Cursor { .. } => [Effect::CursorSmear, Effect::CursorFire]
                .into_iter()
                .filter(|effect| settings.enabled(*effect))
                .map(|effect| effect_lifetime(settings, effect))
                .max()
                .unwrap_or_else(|| effect_lifetime(settings, Effect::CursorSmear)),
            Trigger::Insert { .. } => effect_lifetime(settings, Effect::WhitespaceReveal),
            Trigger::Scope { .. } => Self::scope_entry_duration(settings),
        }
    }

    pub(crate) fn scope_entry_duration(settings: &AnimationSettings) -> Duration {
        effect_lifetime(settings, Effect::ScopeConnector) * if settings.scope_fill { 2 } else { 1 }
    }

    pub(crate) fn scope_fill_progress(&self, settings: &AnimationSettings, now: Instant) -> f32 {
        if !settings.scope_fill {
            return 0.0;
        }
        let outline = effect_lifetime(settings, Effect::ScopeConnector);
        (now.saturating_duration_since(self.started)
            .saturating_sub(outline)
            .as_secs_f32()
            / outline.as_secs_f32())
        .clamp(0.0, 1.0)
    }

    pub(crate) fn scope_exit_duration(settings: &AnimationSettings) -> Duration {
        Duration::from_millis((settings.duration(Effect::ScopeConnector) * 42 / 100).clamp(1, 240))
    }

    fn active(&self, settings: &AnimationSettings, now: Instant) -> bool {
        self.trigger.enabled(settings)
            && match self.trigger {
                Trigger::Scope {
                    exiting: Some(at), ..
                } => now.saturating_duration_since(at) < Self::scope_exit_duration(settings),
                _ => now.saturating_duration_since(self.started) < self.lifetime(settings),
            }
    }

    #[cfg(test)]
    pub(crate) fn smear_sample(
        &self,
        settings: &AnimationSettings,
        now: Instant,
    ) -> Option<SmearSample> {
        [Effect::CursorSmear, Effect::CursorFire]
            .into_iter()
            .find_map(|effect| self.cursor_sample(settings, effect, now))
    }

    pub(crate) fn cursor_sample(
        &self,
        settings: &AnimationSettings,
        effect: Effect,
        now: Instant,
    ) -> Option<SmearSample> {
        if !settings.enabled(effect) || self.effect_progress(settings, effect, now) >= 1.0 {
            return None;
        }
        match self.trigger {
            Trigger::Cursor { motion } => Some(motion.sample_for_effect(
                settings,
                effect,
                now.saturating_duration_since(self.started),
            )),
            _ => None,
        }
    }
}

struct ViewState {
    snapshot: Snapshot,
    cursor_point: Option<MotionPoint>,
    anchor_range: Option<ScopePair>,
    events: Vec<AnimationEvent>,
    seen: Instant,
    visible: bool,
    scope: Option<ScopePair>,
    fire: FireTrail,
    fire_active: bool,
}

#[derive(Default)]
pub(crate) struct AnimationState {
    views: HashMap<u64, ViewState>,
    last_view: Option<u64>,
    pending_tab: Option<u64>,
    last_frame: Option<Instant>,
    frame_pacing: crate::animation_pacing::FramePacing,
    scope_cache: HashMap<u64, ScopeCache>,
    scope_sources: HashMap<u64, ScopeSourceCache>,
    pub(crate) scope_paint: crate::ui_editor_animations::ScopePaintCache,
    pub(crate) diff_geometry: crate::ui_diff_animations::DiffGeometryCache,
    tab_motion: RefCell<TabIndicatorState>,
    fire_samples: Vec<FireParticle>,
}

fn effect_lifetime(settings: &AnimationSettings, effect: Effect) -> Duration {
    Duration::from_millis(settings.duration(effect))
}

#[cfg(test)]
fn lifetime(settings: &AnimationSettings) -> Duration {
    effect_lifetime(settings, Effect::ScopeConnector)
}

fn source_offset(rope: &Rope, line: usize, col: usize) -> usize {
    if line >= rope.len_lines() {
        return rope.len_chars();
    }
    rope.line_to_char(line)
        .saturating_add(col.min(rope.line(line).len_chars()))
        .min(rope.len_chars())
}

fn source_char(rope: &Rope, position: (usize, usize)) -> Option<char> {
    rope.get_line(position.0)?.get_char(position.1)
}

fn anchor_range(anchors: &[Anchor], ordered: bool) -> Option<ScopePair> {
    let first = anchors.first()?;
    let position = |anchor: &Anchor| (anchor.line, anchor.col);
    if ordered {
        Some((position(first), position(anchors.last()?)))
    } else {
        Some(anchors.iter().fold(
            (position(first), position(first)),
            |(first, last), anchor| {
                let position = position(anchor);
                (first.min(position), last.max(position))
            },
        ))
    }
}

fn source_bounds(rope: &Rope, range: Option<ScopePair>) -> Option<(usize, usize)> {
    let (first, last) = range?;
    if first.0 >= rope.len_lines() {
        return None;
    }
    Some((
        source_offset(rope, first.0, first.1),
        source_offset(rope, last.0, last.1)
            .saturating_add(1)
            .min(rope.len_chars()),
    ))
}

/// Rendering supplies source-ordered anchors in the usual editor path. Search
/// just the inserted interval, sharing Rope metadata for each matching line.
fn inserted_whitespace(
    rope: &Rope,
    anchors: &[Anchor],
    ordered: bool,
    start: usize,
    end: usize,
) -> bool {
    if start >= end || start >= rope.len_chars() {
        return false;
    }
    let anchors = if ordered {
        let start_line = rope.char_to_line(start);
        let end = end.min(rope.len_chars());
        let end_line = rope.char_to_line(end);
        let start_column = start - rope.line_to_char(start_line);
        // A virtual end-of-line anchor is clamped by source_offset. At a
        // line boundary it can therefore belong to this insertion as well.
        let first = if start_column == 0 && start_line > 0 {
            (start_line - 1, rope.line(start_line - 1).len_chars())
        } else {
            (start_line, start_column)
        };
        let last = (end_line, end - rope.line_to_char(end_line));
        let from = anchors.partition_point(|anchor| (anchor.line, anchor.col) < first);
        let to = anchors.partition_point(|anchor| (anchor.line, anchor.col) < last);
        &anchors[from..to]
    } else {
        anchors
    };
    let mut line = usize::MAX;
    let mut line_start = 0;
    let mut line_chars = 0;
    anchors.iter().any(|anchor| {
        if !matches!(anchor.ch, ' ' | '\t') || anchor.line >= rope.len_lines() {
            return false;
        }
        if anchor.line != line {
            line = anchor.line;
            let Some(slice) = rope.get_line(line) else {
                return false;
            };
            line_start = rope.line_to_char(line);
            line_chars = slice.len_chars();
        }
        let offset = line_start
            .saturating_add(anchor.col.min(line_chars))
            .min(rope.len_chars());
        offset >= start && offset < end
    })
}

/// Infer one changed neighborhood without walking the document. The old
/// selection gives exact replacement bounds; otherwise visible anchors and the
/// editing cursor bound a prefix/suffix comparison. Ambiguous repeated text
/// stays near the editing cursor rather than drifting to the document end.
#[cfg(test)]
fn changed_range(
    old: &Snapshot,
    new: &Snapshot,
    old_anchors: &[Anchor],
    new_anchors: &[Anchor],
) -> (usize, usize, usize) {
    changed_range_with_bounds(
        old,
        new,
        source_bounds(&old.rope, anchor_range(old_anchors, false)),
        source_bounds(&new.rope, anchor_range(new_anchors, false)),
    )
}

fn changed_range_with_bounds(
    old: &Snapshot,
    new: &Snapshot,
    old_bounds: Option<(usize, usize)>,
    new_bounds: Option<(usize, usize)>,
) -> (usize, usize, usize) {
    let old_len = old.rope.len_chars();
    let new_len = new.rope.len_chars();
    let delta = new_len as i128 - old_len as i128;
    let shifted = |offset: usize| (offset as i128 + delta).clamp(0, new_len as i128) as usize;
    if let Some((sl, sc, el, ec)) = old.selection
        && new.selection.is_none()
    {
        let start = source_offset(&old.rope, sl, sc);
        let end = source_offset(&old.rope, el, ec);
        // A disappearing selection with a changed document is a replacement,
        // including a paste whose total document length shrinks or stays equal.
        if end > start && end as i128 + delta >= start as i128 {
            return (start, end, shifted(end));
        }
    }

    let caret_origin = if delta > 0 {
        new.offset.saturating_sub(delta as usize)
    } else {
        new.offset
    };
    let candidate = old.offset.min(caret_origin).min(old_len.min(new_len));
    let first_anchor = old_bounds
        .into_iter()
        .chain(new_bounds)
        .map(|(start, _)| start)
        .min()
        .unwrap_or(candidate);
    let line_start =
        source_offset(&old.rope, old.cursor.0, 0).min(source_offset(&new.rope, new.cursor.0, 0));
    let scan_start = first_anchor
        .min(line_start)
        .min(candidate)
        .max(candidate.saturating_sub(MAX_COMPARE_CHARS));
    let mut start = candidate;
    for (index, (before, after)) in old
        .rope
        .slice(scan_start..candidate)
        .chars()
        .zip(new.rope.slice(scan_start..candidate).chars())
        .enumerate()
    {
        if before != after {
            start = scan_start + index;
            break;
        }
    }

    let visible_old_end = old_bounds.map_or(old.offset.min(old_len), |(_, end)| end);
    let visible_new_end = new_bounds.map_or(new.offset.min(new_len), |(_, end)| end);
    let unshifted = |offset: usize| (offset as i128 - delta).clamp(0, old_len as i128) as usize;
    let mut old_end = old
        .offset
        .max(unshifted(new.offset))
        .max(visible_old_end)
        .max(unshifted(visible_new_end))
        .max(start)
        .min(old_len);
    // Undo can restore the editing cursor from an entirely different viewport.
    // Its former position is not an edit boundary. Keep enough unchanged suffix
    // to identify the local operation without spanning that navigation gap;
    // large deletions still include the known length delta in full.
    let deleted = old_len.saturating_sub(new_len);
    old_end = old_end.min(
        start
            .saturating_add(MAX_COMPARE_CHARS)
            .saturating_add(deleted),
    );
    let mut new_end = shifted(old_end).max(start).min(new_len);
    // Include at least one cell for equal-length transformations at a stationary
    // cursor; visible suffix trimming then finds the actual last changed cell.
    if delta == 0 && old_end == start && start < old_len {
        old_end += 1;
        new_end += 1;
    }
    // Navigation and an edit can arrive before the next rendered snapshot.
    // In that case the previous caret is only a lower bound: refine through
    // the unchanged prefix toward the post-edit caret. Stop at the inferred
    // insertion origin so repeated text still belongs to the editing cursor.
    let prefix_end = caret_origin
        .min(old_end)
        .min(new_end)
        .min(start.saturating_add(MAX_COMPARE_CHARS));
    if prefix_end > start {
        for (before, after) in old
            .rope
            .slice(start..prefix_end)
            .chars()
            .zip(new.rope.slice(start..prefix_end).chars())
        {
            if before != after {
                break;
            }
            start += 1;
        }
    }
    let old_slice = old.rope.slice(start..old_end);
    let new_slice = new.rope.slice(start..new_end);
    for (before, after) in old_slice
        .chars_at(old_slice.len_chars())
        .reversed()
        .zip(new_slice.chars_at(new_slice.len_chars()).reversed())
        .take(MAX_COMPARE_CHARS)
    {
        if before != after {
            break;
        }
        old_end -= 1;
        new_end -= 1;
    }
    (start, old_end, new_end)
}

fn screen_point(
    anchors: &[Anchor],
    position: (usize, usize),
    ordered: bool,
) -> Option<MotionPoint> {
    let anchor = if ordered {
        anchors
            .get(anchors.partition_point(|anchor| (anchor.line, anchor.col) < position))
            .filter(|anchor| (anchor.line, anchor.col) == position)
    } else {
        anchors
            .iter()
            .find(|anchor| (anchor.line, anchor.col) == position)
    };
    anchor.map(|anchor| MotionPoint {
        x: anchor.x as f32,
        y: anchor.y as f32,
    })
}

/// Source offsets belong to one document revision. Preserve disjoint highlights
/// through an edit, translate following ranges, and discard overlapping ranges
/// rather than letting stale offsets illuminate unrelated replacement text.
fn remap_edits(events: &mut Vec<AnimationEvent>, start: usize, old_end: usize, new_end: usize) {
    let delta = new_end as i128 - old_end as i128;
    events.retain_mut(|event| match &mut event.trigger {
        Trigger::Insert {
            start: first,
            end: last,
            ..
        } => {
            if *last <= start {
                true
            } else if *first >= old_end {
                *first = (*first as i128 + delta).max(0) as usize;
                *last = (*last as i128 + delta).max(0) as usize;
                true
            } else {
                false
            }
        }
        _ => true,
    });
}

impl AnimationState {
    /// Temporarily lend the existing allocation to the overlay renderer so it
    /// can update paint caches while reading events, without cloning them.
    pub(crate) fn take_events(&mut self, id: u64) -> Vec<AnimationEvent> {
        self.views
            .get_mut(&id)
            .map_or_else(Vec::new, |view| std::mem::take(&mut view.events))
    }

    pub(crate) fn restore_events(&mut self, id: u64, events: Vec<AnimationEvent>) {
        if let Some(view) = self.views.get_mut(&id) {
            debug_assert!(view.events.is_empty());
            view.events = events;
        }
    }

    pub(crate) fn cursor_smear_active(
        &self,
        id: u64,
        settings: &AnimationSettings,
        now: Instant,
    ) -> bool {
        (settings.enabled(Effect::CursorSmear) || settings.enabled(Effect::CursorFire))
            && self.views.get(&id).is_some_and(|view| {
                view.events.iter().any(|event| {
                    matches!(event.trigger, Trigger::Cursor { motion: _, .. })
                        && event.active(settings, now)
                })
            })
    }

    #[cfg(test)]
    pub(crate) fn fire_particles(
        &self,
        id: u64,
        settings: &AnimationSettings,
        now: Instant,
    ) -> Vec<FireParticle> {
        let mut particles = Vec::new();
        self.sample_fire_particles(id, settings, now, &mut particles);
        particles
    }

    pub(crate) fn take_fire_particles(
        &mut self,
        id: u64,
        settings: &AnimationSettings,
        now: Instant,
    ) -> Vec<FireParticle> {
        let mut particles = std::mem::take(&mut self.fire_samples);
        self.sample_fire_particles(id, settings, now, &mut particles);
        particles
    }

    pub(crate) fn restore_fire_particles(&mut self, mut particles: Vec<FireParticle>) {
        particles.clear();
        if particles.capacity() > self.fire_samples.capacity() {
            self.fire_samples = particles;
        }
    }

    fn sample_fire_particles(
        &self,
        id: u64,
        settings: &AnimationSettings,
        now: Instant,
        particles: &mut Vec<FireParticle>,
    ) {
        particles.clear();
        if !settings.enabled(Effect::CursorFire) || settings.intensity(Effect::CursorFire) == 0 {
            return;
        }
        if let Some(view) = self.views.get(&id).filter(|view| view.visible) {
            let intensity = settings.intensity(Effect::CursorFire).min(100) as f32 / 100.0;
            particles.extend(view.fire.sample_iter(now).map(|mut particle| {
                particle.opacity *= intensity;
                particle
            }));
        }
    }

    pub(crate) fn fire_core(
        &self,
        id: u64,
        settings: &AnimationSettings,
        now: Instant,
    ) -> Option<SmearSample> {
        if !settings.enabled(Effect::CursorFire) {
            return None;
        }
        self.views
            .get(&id)
            .filter(|view| view.visible)?
            .fire
            .core(settings, now)
    }

    pub fn clear(&mut self) {
        self.views.clear();
        self.last_view = None;
        self.pending_tab = None;
        self.last_frame = None;
        // Clearing an effect must not erase measured rendering pressure.
        // Pacing also coalesces background updates when animations are off.
        self.scope_cache.clear();
        self.scope_sources.clear();
        self.scope_paint.clear();
        self.diff_geometry = Default::default();
        self.fire_samples.clear();
        *self.tab_motion.get_mut() = TabIndicatorState::default();
    }

    pub(crate) fn forget_view(&mut self, id: u64) {
        self.views.remove(&id);
        self.scope_cache.remove(&id);
        self.scope_sources.remove(&id);
        self.scope_paint.remove(id);
        self.diff_geometry.forget(id);
    }

    pub(crate) fn begin_frame(&mut self) {
        for view in self.views.values_mut() {
            view.visible = false;
        }
        self.tab_motion.get_mut().visible = false;
    }

    pub(crate) fn set_active_view(&mut self, id: Option<u64>) {
        if self.last_view != id {
            self.diff_geometry = Default::default();
            for view in self.views.values_mut() {
                view.fire.clear();
                view.fire_active = false;
                // A moving caret belongs to the interaction that started it.
                // Returning to a tab must not replay its old ribbon or fire core.
                view.events.clear();
            }
            self.last_view = id;
            self.pending_tab = id;
        }
    }

    /// Sample the active tab's underline without mutating the editor. The tab
    /// bar is drawn through an immutable App reference, hence interior state.
    pub(crate) fn tab_indicator(
        &self,
        id: u64,
        area: Rect,
        viewport: Rect,
        scroll_offset: usize,
        settings: &AnimationSettings,
        now: Instant,
    ) -> Option<TabIndicator> {
        use crate::tab_indicator_motion::TabIndicatorMotion;
        let mut state = self.tab_motion.borrow_mut();
        if !settings.enabled(Effect::TabSlide) || area.is_empty() || viewport.is_empty() {
            *state = TabIndicatorState::default();
            return None;
        }
        let left = area.x as f32;
        let right = area.right() as f32;
        let changed = state.id != Some(id) || state.area != area;
        let layout_changed = state.viewport != viewport || state.scroll_offset != scroll_offset;
        let elapsed = now.saturating_duration_since(state.started.unwrap_or(now));
        if state.id.is_none() || layout_changed || (state.id == Some(id) && state.area != area) {
            state.motion = Some(TabIndicatorMotion::stationary(left, right));
            state.started = Some(now);
            state.active = false;
        } else if changed {
            let current = state.motion.as_ref().unwrap().sample(elapsed);
            state.motion = Some(TabIndicatorMotion::retarget(
                current,
                left,
                right,
                effect_lifetime(settings, Effect::TabSlide),
                settings.intensity(Effect::TabSlide),
            ));
            state.started = Some(now);
            state.active = true;
        }
        state.id = Some(id);
        state.area = area;
        state.viewport = viewport;
        state.scroll_offset = scroll_offset;
        state.visible = true;
        let elapsed = now.saturating_duration_since(state.started.unwrap_or(now));
        let motion = state.motion.as_ref().unwrap();
        let current = motion.sample(elapsed);
        state.active = !motion.is_settled(elapsed);
        Some(TabIndicator {
            left: current.left,
            right: current.right,
        })
    }

    #[cfg(test)]
    pub(crate) fn keyword_scope(
        &mut self,
        editor: &crate::editor::Editor,
    ) -> Option<((usize, usize), (usize, usize))> {
        self.scope(editor, None)
    }

    /// Keep semantic palette depths rather than rendered colors: a theme
    /// change uses its new palette immediately, while an ordinary edit does
    /// not lose the established hue while a replacement index is pending.
    pub(crate) fn scope_bracket_depth(
        &mut self,
        editor: &crate::editor::Editor,
        pair: ScopePair,
        index: Option<&crate::background::BracketDepthIndex>,
    ) -> Option<usize> {
        self.synchronize_scope_source(editor);
        let pair = (pair.0.min(pair.1), pair.0.max(pair.1));
        let cached = self.scope_sources.get_mut(&editor.navigation_id)?;
        if let Some(depth) = cached.depths.get_mut(&pair) {
            if !depth.verified
                && let Some(index) = index.filter(|index| index.matches(editor))
            {
                depth.value = index.depth_at(editor, pair.0.0, pair.0.1);
                depth.verified = true;
            }
            return depth.value;
        }
        if let Some(index) = index.filter(|index| index.matches(editor)) {
            let value = index.depth_at(editor, pair.0.0, pair.0.1);
            if cached.depths.len() >= MAX_EVENTS && !cached.depths.contains_key(&pair) {
                cached.depths.clear();
            }
            cached.depths.insert(
                pair,
                ScopeDepth {
                    value,
                    verified: true,
                },
            );
            value
        } else {
            None
        }
    }

    fn synchronize_scope_source(&mut self, editor: &crate::editor::Editor) {
        let language = editor.syntax_language_token();
        let path = editor.syntax_file_path();
        let preserve_scope = self
            .scope_cache
            .get(&editor.navigation_id)
            .is_some_and(|cached| cached.result.is_some())
            || self.views.get(&editor.navigation_id).is_some_and(|view| {
                view.scope.is_some()
                    || view
                        .events
                        .iter()
                        .any(|event| matches!(event.trigger, Trigger::Scope { .. }))
            });
        if let Some(cached) = self.scope_sources.get_mut(&editor.navigation_id) {
            let same_context =
                cached.language.as_deref() == language && cached.path.as_deref() == path
                    && std::sync::Arc::ptr_eq(&cached.rules, &editor.scope_bubble_rules);
            if cached.version == editor.content_version
                && cached.source.is_instance(&editor.rope)
                && same_context
            {
                return;
            }
            let edit = (same_context
                && cached.version != editor.content_version
                && (preserve_scope || !cached.depths.is_empty()))
            .then(|| ScopeEdit::between(&cached.source, &editor.rope))
            .flatten();
            cached.depths = edit.map_or_else(HashMap::new, |edit| {
                cached
                    .depths
                    .iter()
                    .filter(|(pair, _)| edit.preserves_depth_at(&cached.source, pair.0))
                    .filter_map(|(&pair, depth)| {
                        Some((
                            edit.pair(&cached.source, &editor.rope, pair)?,
                            ScopeDepth {
                                value: depth.value,
                                verified: false,
                            },
                        ))
                    })
                    .collect()
            });
            cached.previous = Some(std::mem::replace(&mut cached.source, editor.rope.clone()));
            cached.edit = edit;
            cached.version = editor.content_version;
            if !same_context {
                cached.rules = editor.scope_bubble_rules.clone();
                cached.language = language.map(str::to_owned);
                cached.path = path.map(std::path::PathBuf::from);
            }
            return;
        }
        if self.scope_sources.len() >= MAX_VIEWS {
            self.scope_sources.clear();
        }
        self.scope_sources.insert(
            editor.navigation_id,
            ScopeSourceCache {
                rules: editor.scope_bubble_rules.clone(),
                source: editor.rope.clone(),
                version: editor.content_version,
                language: language.map(str::to_owned),
                path: path.map(std::path::PathBuf::from),
                previous: None,
                edit: None,
                depths: HashMap::new(),
            },
        );
    }

    pub(crate) fn scope(
        &mut self,
        editor: &crate::editor::Editor,
        index: Option<&crate::animation_structural_scope::StructuralScopeIndex>,
    ) -> Option<ScopePair> {
        self.scope_impl(editor, index, false)
    }

    pub(crate) fn diff_scope(
        &mut self,
        editor: &crate::editor::Editor,
        index: Option<&crate::animation_structural_scope::StructuralScopeIndex>,
    ) -> Option<ScopePair> {
        self.scope_impl(editor, index, true)
    }

    fn scope_impl(
        &mut self,
        editor: &crate::editor::Editor,
        index: Option<&crate::animation_structural_scope::StructuralScopeIndex>,
        indexed_only: bool,
    ) -> Option<ScopePair> {
        self.synchronize_scope_source(editor);
        let index = index.filter(|index| index.matches(editor));
        if let Some(cached) = self.scope_cache.get(&editor.navigation_id)
            && cached.matches_source(editor)
            && cached.indexed == index.is_some()
            && cached.indexed_only == indexed_only
            && cached.line == editor.cursor_line
            && cached.col == editor.cursor_col
        {
            return cached.result;
        }
        // Reuse the exact established block through plain local typing. The
        // caret must follow the verified edit; navigation into another nested
        // block still performs normal scope discovery.
        if index.is_none()
            && let Some(cached) = self.scope_cache.get_mut(&editor.navigation_id)
            && cached.indexed_only == indexed_only
            && let Some(source) = self.scope_sources.get(&editor.navigation_id)
            && let Some(old_pair) = cached.result
            && source
                .previous
                .as_ref()
                .is_some_and(|old| old.is_instance(&cached.source))
            && source
                .edit
                .filter(|edit| edit.preserves_scope(editor))
                .and_then(|edit| {
                    edit.cursor(&cached.source, &editor.rope, (cached.line, cached.col))
                })
                == Some((editor.cursor_line, editor.cursor_col))
            && let Some(pair) = source.rebase(&cached.source, old_pair)
            && source_char(&cached.source, old_pair.0) == Some('{')
            && source_char(&cached.source, old_pair.1) == Some('}')
            && source_char(&editor.rope, pair.0) == Some('{')
            && source_char(&editor.rope, pair.1) == Some('}')
        {
            cached.version = editor.content_version;
            cached.source = editor.rope.clone();
            cached.indexed = false;
            cached.line = editor.cursor_line;
            cached.col = editor.cursor_col;
            cached.result = Some(pair);
            cached.keyword = None;
            cached.keyword_index = None;
            cached.immediate = None;
            return Some(pair);
        }
        if !self
            .scope_cache
            .get(&editor.navigation_id)
            .is_some_and(|cached| cached.matches_source(editor))
        {
            if self.scope_cache.len() >= MAX_VIEWS
                && !self.scope_cache.contains_key(&editor.navigation_id)
            {
                self.scope_cache.clear();
            }
            self.scope_cache.insert(
                editor.navigation_id,
                ScopeCache {
                    rules: editor.scope_bubble_rules.clone(),
                    version: editor.content_version,
                    source: editor.rope.clone(),
                    indexed: index.is_some(),
                    indexed_only,
                    line: editor.cursor_line,
                    col: editor.cursor_col,
                    language: editor.syntax_language_token().map(str::to_owned),
                    path: editor.syntax_file_path().map(std::path::PathBuf::from),
                    result: None,
                    keyword: None,
                    keyword_index: None,
                    immediate: None,
                },
            );
        }
        let cached = self.scope_cache.get_mut(&editor.navigation_id).unwrap();
        let keyword = if index.is_none_or(|index| index.keyword_scopes_first()) {
            // Keyword blocks contain a cursor row, independently of its
            // column. Reuse both successful and negative discoveries across
            // horizontal movement and worker-index arrival on that row.
            let result = if cached.line == editor.cursor_line
                && let Some(keyword) = cached.keyword
            {
                keyword
            } else {
                if !cached
                    .keyword_index
                    .as_ref()
                    .is_some_and(|index| index.covers_cursor(editor.cursor_line))
                {
                    cached.keyword_index = crate::animation_scope::KeywordScopeIndex::build(editor);
                }
                cached
                    .keyword_index
                    .as_ref()
                    .and_then(|index| index.enclosing_scope(editor.cursor_line))
            };
            cached.keyword = Some(result);
            result
        } else {
            cached.keyword = None;
            None
        };
        let structural = (|| {
            if let Some(index) = index {
                index.enclosing_scope(editor)
            } else if !indexed_only {
                if cached.immediate.is_none() {
                    cached.immediate =
                        crate::animation_structural_scope::ImmediateScopeIndex::build(editor);
                }
                cached.immediate.as_ref()?.enclosing_scope(editor)
            } else {
                None
            }
        })();
        let result = match (keyword, structural) {
            (Some(keyword), Some(structural)) => {
                Some(if structural.0 >= keyword.0 && structural.1 <= keyword.1 {
                    structural
                } else {
                    keyword
                })
            }
            (keyword, structural) => keyword.or(structural),
        };
        cached.line = editor.cursor_line;
        cached.col = editor.cursor_col;
        cached.indexed = index.is_some();
        cached.indexed_only = indexed_only;
        cached.result = result;
        result
    }

    /// A connector belongs to a scope, not to every cursor step within it.
    /// Remember membership after its finite entrance animation has expired so
    /// ordinary navigation inside one block remains visually quiet.
    pub(crate) fn update_scope(
        &mut self,
        id: u64,
        pair: Option<ScopePair>,
        settings: &AnimationSettings,
        now: Instant,
    ) -> ObservedEvents {
        let Some(view) = self.views.get_mut(&id) else {
            return observed_events(&[]);
        };
        view.events.retain(|event| event.active(settings, now));
        let pair = settings
            .enabled(Effect::ScopeConnector)
            .then_some(pair)
            .flatten()
            .map(|(first, last)| {
                if first <= last {
                    (first, last)
                } else {
                    (last, first)
                }
            });
        if view.scope != pair {
            if let Some(old) = view.scope {
                let mut found = false;
                for event in &mut view.events {
                    if let Trigger::Scope { pair, exiting, .. } = &mut event.trigger
                        && *pair == old
                        && exiting.is_none()
                    {
                        *exiting = Some(now);
                        found = true;
                    }
                }
                if !found && settings.enabled(Effect::ScopeConnector) {
                    view.events.push(AnimationEvent {
                        trigger: Trigger::Scope {
                            pair: old,
                            origin: view.snapshot.cursor,
                            exiting: Some(now),
                        },
                        started: now
                            .checked_sub(AnimationEvent::scope_entry_duration(settings))
                            .unwrap_or(now),
                    });
                }
            }
            view.scope = pair;
            if let Some(pair) = pair {
                view.events.push(AnimationEvent {
                    trigger: Trigger::Scope {
                        pair,
                        origin: view.snapshot.cursor,
                        exiting: None,
                    },
                    started: now,
                });
            }
            let mut outgoing = 0;
            view.events.reverse();
            view.events.retain(|event| {
                if matches!(
                    event.trigger,
                    Trigger::Scope {
                        exiting: Some(_),
                        ..
                    }
                ) {
                    outgoing += 1;
                    outgoing <= 4
                } else {
                    true
                }
            });
            view.events.reverse();
        }
        observed_events(&view.events)
    }

    /// Returns the next finite animation deadline; an idle editor has none.
    pub fn next_frame_wait(&self, settings: &AnimationSettings, now: Instant) -> Option<Duration> {
        if !settings.any_enabled() {
            return None;
        }
        let active_tab = {
            let tab = self.tab_motion.borrow();
            settings.enabled(Effect::TabSlide) && tab.visible && tab.active
        };
        if !active_tab
            && !self
                .views
                .values()
                .any(|view| view.visible && (view.fire_active || !view.events.is_empty()))
        {
            return None;
        }
        let interval = Duration::from_secs_f64(1.0 / settings.fps.clamp(15, 120) as f64);
        let rate_wait = self.last_frame.map_or(Duration::ZERO, |last| {
            interval.saturating_sub(now.saturating_duration_since(last))
        });
        Some(rate_wait.max(self.frame_pacing.wait(now)))
    }

    pub(crate) fn record_render(&mut self, finished: Instant, elapsed: Duration) {
        self.frame_pacing.record(finished, elapsed);
    }

    pub(crate) fn background_frame_wait(&self, now: Instant) -> Duration {
        self.frame_pacing.background_wait(now)
    }

    /// Prunes expired events and requests the final clean frame once.
    pub fn advance_frame(&mut self, settings: &AnimationSettings, now: Instant) -> bool {
        if !settings.any_enabled() {
            let redraw = self
                .views
                .values()
                .any(|view| !view.events.is_empty() || view.fire_active);
            self.clear();
            return redraw;
        }
        if !settings.enabled(Effect::ScopeConnector) {
            for view in self.views.values_mut() {
                view.scope = None;
            }
        }
        if !self
            .next_frame_wait(settings, now)
            .is_some_and(|wait| wait.is_zero())
        {
            return false;
        }
        for view in self.views.values_mut() {
            if settings.enabled(Effect::CursorFire) && view.visible {
                view.fire.advance(now, settings);
                view.fire_active = view.fire.is_active(now, settings);
            } else {
                view.fire.clear();
                view.fire_active = false;
            }
            view.events.retain(|event| event.active(settings, now));
        }
        let tab = self.tab_motion.get_mut();
        if !settings.enabled(Effect::TabSlide)
            || tab.started.is_some_and(|started| {
                tab.motion
                    .as_ref()
                    .is_none_or(|motion| motion.is_settled(now.saturating_duration_since(started)))
            })
        {
            tab.active = false;
        }
        self.last_frame = Some(now);
        true
    }

    #[cfg(test)]
    pub(crate) fn observe(
        &mut self,
        snapshot: Snapshot,
        anchors: &[Anchor],
        settings: &AnimationSettings,
        now: Instant,
    ) -> ObservedEvents {
        self.observe_geometry(snapshot, anchors, false, settings, now)
    }

    /// Real render geometry records whether its source positions are ordered.
    /// Retain only the caret projection and source extent between draws; a
    /// viewport-sized anchor clone is never needed to observe an edit.
    pub(crate) fn observe_geometry(
        &mut self,
        snapshot: Snapshot,
        anchors: &[Anchor],
        ordered: bool,
        settings: &AnimationSettings,
        now: Instant,
    ) -> ObservedEvents {
        if !settings.any_enabled() {
            self.clear();
            return observed_events(&[]);
        }
        let cursor_motion =
            settings.enabled(Effect::CursorSmear) || settings.enabled(Effect::CursorFire);
        let cursor_point = cursor_motion
            .then(|| screen_point(anchors, snapshot.cursor, ordered))
            .flatten();
        let anchor_range = settings
            .enabled(Effect::WhitespaceReveal)
            .then(|| anchor_range(anchors, ordered))
            .flatten();
        let changed_view = self.pending_tab == Some(snapshot.id);
        if changed_view {
            self.pending_tab = None;
        }
        if self.views.len() >= MAX_VIEWS
            && !self.views.contains_key(&snapshot.id)
            && let Some(oldest) = self
                .views
                .iter()
                .min_by_key(|(_, view)| view.seen)
                .map(|(id, _)| *id)
        {
            self.views.remove(&oldest);
            self.scope_cache.remove(&oldest);
            self.scope_sources.remove(&oldest);
            self.scope_paint.remove(oldest);
        }
        self.last_frame = Some(now);
        let Some(view) = self.views.get_mut(&snapshot.id) else {
            self.views.insert(
                snapshot.id,
                ViewState {
                    snapshot,
                    cursor_point,
                    anchor_range,
                    events: Vec::new(),
                    seen: now,
                    visible: true,
                    scope: None,
                    fire: FireTrail::default(),
                    fire_active: false,
                },
            );
            return observed_events(&[]);
        };
        let old = &view.snapshot;
        let editing = old.content_version != snapshot.content_version;
        let projected_origin = if cursor_motion {
            if old.cursor == snapshot.cursor {
                cursor_point
            } else {
                screen_point(anchors, old.cursor, ordered)
            }
        } else {
            None
        };
        let layout_changed = cursor_motion && !editing && view.cursor_point != projected_origin;
        let remapped = old.scroll != snapshot.scroll || old.area != snapshot.area || layout_changed;
        if !settings.enabled(Effect::CursorFire) || remapped || changed_view {
            view.fire.clear();
            view.fire_active = false;
        } else {
            view.fire.advance(now, settings);
        }
        view.events.retain(|event| {
            event.active(settings, now)
                && !(remapped && matches!(event.trigger, Trigger::Cursor { .. }))
        });
        if old.cursor != snapshot.cursor
            && (settings.enabled(Effect::CursorSmear) || settings.enabled(Effect::CursorFire))
        {
            let previous = (!remapped && !changed_view)
                .then(|| {
                    view.events.iter().rev().find_map(|event| {
                        event.cursor_sample(
                            settings,
                            if settings.enabled(Effect::CursorSmear) {
                                Effect::CursorSmear
                            } else {
                                Effect::CursorFire
                            },
                            now,
                        )
                    })
                })
                .flatten();
            let origin = if remapped {
                projected_origin
            } else {
                view.cursor_point
            };
            if let Some((origin, target)) = origin.zip(cursor_point) {
                let motion = SmearMotion::retarget(origin, target, previous, editing);
                if settings.enabled(Effect::CursorFire) && !changed_view {
                    view.fire
                        .retarget_cursor(origin, target, now, editing, settings);
                }
                view.events
                    .retain(|event| !matches!(event.trigger, Trigger::Cursor { .. }));
                view.events.push(AnimationEvent {
                    trigger: Trigger::Cursor { motion },
                    started: now,
                });
            }
        }
        if !settings.enabled(Effect::ScopeConnector) {
            view.scope = None;
        }
        if editing
            && settings.enabled(Effect::ScopeConnector)
            && (view.scope.is_some()
                || view
                    .events
                    .iter()
                    .any(|event| matches!(event.trigger, Trigger::Scope { .. })))
        {
            let edit = self
                .scope_sources
                .get(&snapshot.id)
                .filter(|source| {
                    source.source.is_instance(&snapshot.rope)
                        && source
                            .previous
                            .as_ref()
                            .is_some_and(|previous| previous.is_instance(&old.rope))
                })
                .map(|source| source.edit)
                .unwrap_or_else(|| ScopeEdit::between(&old.rope, &snapshot.rope));
            if let Some(edit) = edit {
                view.scope = view
                    .scope
                    .and_then(|pair| edit.pair(&old.rope, &snapshot.rope, pair));
                view.events.retain_mut(|event| {
                    let Trigger::Scope { pair, origin, .. } = &mut event.trigger else {
                        return true;
                    };
                    let Some(mapped) = edit.pair(&old.rope, &snapshot.rope, *pair) else {
                        return false;
                    };
                    *pair = mapped;
                    *origin = edit
                        .cursor(&old.rope, &snapshot.rope, *origin)
                        .unwrap_or(snapshot.cursor);
                    true
                });
            } else {
                view.scope = None;
                view.events
                    .retain(|event| !matches!(event.trigger, Trigger::Scope { .. }));
            }
        }
        let mut inserted = false;
        if editing && settings.enabled(Effect::WhitespaceReveal) {
            let (start, old_end, new_end) = changed_range_with_bounds(
                old,
                &snapshot,
                source_bounds(&old.rope, view.anchor_range),
                source_bounds(&snapshot.rope, anchor_range),
            );
            remap_edits(&mut view.events, start, old_end, new_end);
            if new_end > start
                && old.undo_revision == snapshot.undo_revision
                && settings.enabled(Effect::WhitespaceReveal)
                && inserted_whitespace(&snapshot.rope, anchors, ordered, start, new_end)
            {
                view.events.push(AnimationEvent {
                    trigger: Trigger::Insert {
                        start,
                        end: new_end,
                        paste: new_end - start > 1,
                    },
                    started: now,
                });
                inserted = true;
            }
        }
        if inserted {
            let mut inserts = view
                .events
                .iter()
                .filter(|event| matches!(event.trigger, Trigger::Insert { .. }))
                .count();
            if inserts > MAX_INSERT_EVENTS {
                view.events.retain(|event| {
                    if inserts > MAX_INSERT_EVENTS
                        && matches!(event.trigger, Trigger::Insert { .. })
                    {
                        inserts -= 1;
                        false
                    } else {
                        true
                    }
                });
            }
        }
        if view.events.len() > MAX_EVENTS {
            view.events.drain(..view.events.len() - MAX_EVENTS);
        }
        view.snapshot = snapshot;
        view.cursor_point = cursor_point;
        view.anchor_range = anchor_range;
        view.seen = now;
        view.visible = true;
        view.fire_active =
            settings.enabled(Effect::CursorFire) && view.fire.is_active(now, settings);
        observed_events(&view.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(text: &str, offset: usize) -> Snapshot {
        let rope = Rope::from_str(text);
        let offset = offset.min(rope.len_chars());
        let line = rope.char_to_line(offset);
        let col = offset - rope.line_to_char(line);
        Snapshot {
            id: 1,
            rope,
            cursor: (line, col),
            offset,
            content_version: 1,
            undo_revision: 0,
            scroll: (0, 0, 0),
            selection: None,
            area: Rect::new(0, 0, 40, 10),
        }
    }

    fn anchors(snapshot: &Snapshot) -> Vec<Anchor> {
        snapshot
            .rope
            .chars()
            .take(128)
            .enumerate()
            .filter_map(|(offset, ch)| {
                if ch == '\n' {
                    return None;
                }
                let line = snapshot.rope.char_to_line(offset);
                let col = offset - snapshot.rope.line_to_char(line);
                Some(Anchor {
                    line,
                    col,
                    x: col as u16,
                    y: line as u16,
                    width: 1,
                    ch,
                })
            })
            .collect()
    }

    #[test]
    fn edits_near_a_distant_cursor_use_a_local_character_window() {
        let text = "a".repeat(MAX_COMPARE_CHARS * 8);
        let offset = MAX_COMPARE_CHARS * 4;
        let old = snapshot(&text, offset);
        let mut changed = text;
        changed.insert(offset, 'b');
        let new = snapshot(&changed, offset + 1);
        assert_eq!(
            changed_range(&old, &new, &[], &[]),
            (offset, offset, offset + 1)
        );
    }

    #[test]
    fn undo_restoring_a_distant_cursor_does_not_highlight_the_navigation_gap() {
        let text = "a".repeat(MAX_COMPARE_CHARS * 8);
        let old = snapshot(&text, MAX_COMPARE_CHARS * 4);
        let mut changed = text;
        changed.insert_str(20, "xyz");
        let new = snapshot(&changed, 23);
        assert_eq!(changed_range(&old, &new, &[], &[]), (20, 20, 23));
    }

    #[test]
    fn scope_entry_animates_once_and_stable_membership_stays_idle_after_expiry() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::ScopeConnector, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let initial = snapshot("if x\n  x\nendif", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let pair = ((0, 0), (2, 0));
        let entered = state.update_scope(initial.id, Some(pair), &settings, now);
        assert_eq!(entered.len(), 1);
        assert!(matches!(entered[0].trigger, Trigger::Scope {pair: actual, ..} if actual == pair));
        assert!(state.next_frame_wait(&settings, now).is_some());
        let at_move = now + Duration::from_millis(50);
        let moved = snapshot("if x\n  x\nendif", 7);
        let events = state.observe(moved.clone(), &anchors(&moved), &settings, at_move);
        assert_eq!(events.len(), 1); // No cursor event for a scope-only setting.
        let unchanged = state.update_scope(moved.id, Some(pair), &settings, at_move);
        assert_eq!(unchanged.len(), 1);
        assert_eq!(unchanged[0].started, now);
        let reversed = state.update_scope(moved.id, Some((pair.1, pair.0)), &settings, at_move);
        assert_eq!(reversed.len(), 1);
        assert_eq!(reversed[0].started, now);
        assert!(matches!(
            reversed[0].trigger,
            Trigger::Scope { origin: (0, 0), .. }
        ));
        let expired_at = now + lifetime(&settings) + Duration::from_millis(1);
        assert!(state.advance_frame(&settings, expired_at));
        assert!(
            state
                .update_scope(moved.id, Some(pair), &settings, expired_at)
                .is_empty()
        );
        assert_eq!(state.next_frame_wait(&settings, expired_at), None);
        let exiting = state.update_scope(moved.id, None, &settings, expired_at);
        assert_eq!(exiting.len(), 1);
        assert!(matches!(
            exiting[0].trigger,
            Trigger::Scope {
                exiting: Some(_),
                ..
            }
        ));
        let reentered = state.update_scope(moved.id, Some(pair), &settings, expired_at);
        assert_eq!(reentered.len(), 2);
        assert_eq!(reentered[1].started, expired_at);
    }

    #[test]
    fn scope_fill_finishes_after_the_outline_then_stays_idle_and_exits_fully_filled() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::ScopeConnector, true);
        settings.scope_fill = true;
        settings.set_duration(Effect::ScopeConnector, 400);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let initial = snapshot("if x\n  x\nendif", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let pair = ((0, 0), (2, 0));
        let entered = state.update_scope(initial.id, Some(pair), &settings, now);
        let outline_end = now + Duration::from_millis(400);
        assert_eq!(
            entered[0].effect_progress(&settings, Effect::ScopeConnector, outline_end),
            1.0
        );
        assert_eq!(entered[0].scope_fill_progress(&settings, outline_end), 0.0);
        assert_eq!(
            entered[0].scope_fill_progress(&settings, now + Duration::from_millis(600)),
            0.5
        );
        assert!(state.advance_frame(&settings, outline_end));
        assert!(state.next_frame_wait(&settings, outline_end).is_some());
        assert_eq!(
            state
                .update_scope(initial.id, Some(pair), &settings, outline_end)
                .len(),
            1
        );
        let fill_end = now + Duration::from_millis(800);
        assert!(state.advance_frame(&settings, fill_end));
        assert_eq!(state.next_frame_wait(&settings, fill_end), None);
        assert!(
            state
                .update_scope(initial.id, Some(pair), &settings, fill_end)
                .is_empty()
        );
        let exiting = state.update_scope(initial.id, None, &settings, fill_end);
        assert_eq!(exiting.len(), 1);
        assert_eq!(exiting[0].scope_fill_progress(&settings, fill_end), 1.0);
    }

    #[test]
    fn entering_another_scope_and_reenabling_scope_feedback_restarts_once() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::ScopeConnector, true);
        settings.set(Effect::WhitespaceReveal, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let initial = snapshot("if x\nendif\nif y\nendif", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        state.update_scope(initial.id, Some(((0, 0), (1, 0))), &settings, now);
        let next_at = now + Duration::from_millis(30);
        let next_pair = ((2, 0), (3, 0));
        let next = state.update_scope(initial.id, Some(next_pair), &settings, next_at);
        assert_eq!(next.len(), 2);
        assert_eq!(next[1].started, next_at);
        assert!(
            matches!(next[1].trigger, Trigger::Scope {pair, exiting: None, ..} if pair == next_pair)
        );
        settings.set(Effect::ScopeConnector, false);
        assert!(
            state
                .update_scope(initial.id, Some(next_pair), &settings, next_at)
                .is_empty()
        );
        settings.set(Effect::ScopeConnector, true);
        let reenabled = state.update_scope(initial.id, Some(next_pair), &settings, next_at);
        assert_eq!(reenabled.len(), 1);
        assert!(state.next_frame_wait(&settings, next_at).is_some());
    }

    #[test]
    fn tab_bar_owns_tab_motion_and_hidden_or_expired_indicators_have_no_deadline() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::TabSlide, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let view = snapshot("abcdef", 0);
        state.set_active_view(Some(view.id));
        let events = state.observe(view.clone(), &[], &settings, now);
        assert!(events.is_empty());
        assert_eq!(state.next_frame_wait(&settings, now), None);
        state.tab_indicator(
            1,
            Rect::new(0, 0, 10, 1),
            Rect::new(0, 0, 80, 1),
            0,
            &settings,
            now,
        );
        state.tab_indicator(
            2,
            Rect::new(20, 0, 10, 1),
            Rect::new(0, 0, 80, 1),
            0,
            &settings,
            now,
        );
        assert!(state.next_frame_wait(&settings, now).is_some());
        state.begin_frame();
        assert_eq!(state.next_frame_wait(&settings, now), None);
        state.set_active_view(Some(view.id));
        let events = state.observe(view, &[], &settings, now + Duration::from_millis(50));
        assert!(events.is_empty());
        state.tab_indicator(
            2,
            Rect::new(20, 0, 10, 1),
            Rect::new(0, 0, 80, 1),
            0,
            &settings,
            now + Duration::from_millis(50),
        );
        assert!(state.advance_frame(&settings, now + Duration::from_secs(2)));
        assert_eq!(
            state.next_frame_wait(&settings, now + Duration::from_secs(2)),
            None
        );
    }

    #[test]
    fn disabling_an_effect_removes_its_events_while_other_settings_stay_on() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::CursorSmear, true);
        settings.set(Effect::WhitespaceReveal, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let initial = snapshot("abcdef", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let moved = snapshot("abcdef", 4);
        let events = state.observe(moved.clone(), &anchors(&moved), &settings, now);
        assert!(
            events
                .iter()
                .any(|event| matches!(event.trigger, Trigger::Cursor { .. }))
        );
        settings.set(Effect::CursorSmear, false);
        assert!(state.advance_frame(&settings, now + Duration::from_millis(30)));
        assert_eq!(
            state.next_frame_wait(&settings, now + Duration::from_millis(30)),
            None
        );
        assert!(settings.enabled(Effect::WhitespaceReveal));
    }

    #[test]
    fn scope_cache_follows_language_overrides_and_save_as_extension_changes() {
        let mut state = AnimationState::default();
        let mut editor = crate::editor::Editor::test_with_text("if ready\n  echo 'yes'\nendif\n");
        editor.file_path = Some("fixture.vim".into());
        editor.cursor_line = 1;
        assert_eq!(state.keyword_scope(&editor), Some(((0, 0), (2, 0))));
        editor.language_override = Some("Plain Text".into());
        assert_eq!(state.keyword_scope(&editor), None);
        editor.language_override = None;
        editor.file_path = Some("fixture.rs".into());
        assert_eq!(state.keyword_scope(&editor), None);
        editor.file_path = Some("fixture.vim".into());
        assert_eq!(state.keyword_scope(&editor), Some(((0, 0), (2, 0))));
    }

    #[test]
    fn scope_cache_refreshes_at_same_caret_when_worker_index_arrives_or_source_is_replaced() {
        let source = format!("{}fn run() {{\n work();\n}}\n", "// padding\n".repeat(800));
        let mut editor = crate::editor::Editor::test_with_text(&source);
        editor.language_override = Some("Rust".into());
        editor.cursor_line = 801;
        let mut state = AnimationState::default();
        assert_eq!(state.scope(&editor, None), None);
        let index = crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
            .unwrap()
            .build(None, || false)
            .unwrap();
        assert_eq!(
            state.scope(&editor, Some(&index)),
            Some(((800, 9), (802, 0)))
        );
        editor.rope = ropey::Rope::from_str(&source.replace("fn run() {", "// run() {"));
        assert_eq!(state.scope(&editor, Some(&index)), None);
        editor.rope = ropey::Rope::from_str(&source);
        assert_eq!(state.scope(&editor, None), None);
        let fresh = crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
            .unwrap()
            .build(None, || false)
            .unwrap();
        assert_eq!(
            state.scope(&editor, Some(&fresh)),
            Some(((800, 9), (802, 0)))
        );
    }

    #[test]
    fn default_settings_never_schedule_frames() {
        let state = AnimationState::default();
        assert_eq!(
            state.next_frame_wait(&AnimationSettings::default(), Instant::now()),
            None
        );
    }

    #[test]
    fn indexed_inserted_whitespace_matches_full_scan_for_unicode_and_reordered_geometry() {
        let text = "\t  alpha 界\n beta\r\n\n \tomega\n";
        let snapshot = snapshot(text, 0);
        let mut anchors: Vec<_> = snapshot
            .rope
            .lines()
            .enumerate()
            .flat_map(|(line, text)| {
                text.chars().enumerate().map(move |(col, ch)| Anchor {
                    line,
                    col,
                    ch,
                    x: col as u16,
                    y: line as u16,
                    width: 1,
                })
            })
            .collect();
        for line in 0..=snapshot.rope.len_lines() {
            anchors.push(Anchor {
                line,
                col: 10_000,
                ch: ' ',
                x: 0,
                y: 0,
                width: 1,
            });
        }
        anchors.sort_by_key(|anchor| (anchor.line, anchor.col));
        for ordered in [true, false] {
            if !ordered {
                anchors.reverse();
            }
            for start in 0..=snapshot.rope.len_chars() {
                for end in start..=snapshot.rope.len_chars() {
                    let expected = anchors.iter().any(|anchor| {
                        matches!(anchor.ch, ' ' | '\t') && {
                            let offset = source_offset(&snapshot.rope, anchor.line, anchor.col);
                            offset >= start && offset < end
                        }
                    });
                    assert_eq!(
                        inserted_whitespace(&snapshot.rope, &anchors, ordered, start, end),
                        expected,
                        "ordered={ordered}, insertion={start}..{end}"
                    );
                }
            }
        }
    }

    #[test]
    fn fire_sampling_reuses_storage_and_matches_allocating_samples() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::CursorFire, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let initial = snapshot("abcdefghijklmnopqrstuvwxyz", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let moved = snapshot("abcdefghijklmnopqrstuvwxyz", 20);
        state.observe(moved.clone(), &anchors(&moved), &settings, now);
        let at = now + Duration::from_millis(60);
        state.advance_frame(&settings, at);
        let expected = state.fire_particles(moved.id, &settings, at);
        assert!(!expected.is_empty());
        let particles = state.take_fire_particles(moved.id, &settings, at);
        assert_eq!(particles, expected);
        let allocation = particles.as_ptr();
        state.restore_fire_particles(particles);
        let particles = state.take_fire_particles(moved.id, &settings, at);
        assert_eq!(particles.as_ptr(), allocation);
        assert_eq!(particles, expected);
        state.restore_fire_particles(particles);
        settings.set(Effect::CursorFire, false);
        let particles = state.take_fire_particles(moved.id, &settings, at);
        assert!(particles.is_empty());
        assert_eq!(particles.as_ptr(), allocation);
        state.restore_fire_particles(particles);
    }

    #[test]
    #[ignore = "comparative release microbenchmark"]
    fn benchmark_inserted_whitespace_lookup() {
        use std::hint::black_box;
        let text = "            let value = 123; // text\n".repeat(80_000);
        let rope = Rope::from_str(&text);
        let mut anchors = Vec::new();
        for line in 79_940..80_000 {
            for (col, ch) in rope.line(line).chars().enumerate() {
                anchors.push(Anchor {
                    line,
                    col,
                    ch,
                    x: col as u16,
                    y: (line - 79_940) as u16,
                    width: 1,
                });
            }
        }
        let start = rope.line_to_char(79_999) + 11;
        let end = start + 1;
        let iterations = 5_000;
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..7 {
            for indexed in [round % 2 == 0, round % 2 != 0] {
                let lookup = || {
                    if indexed {
                        inserted_whitespace(
                            black_box(&rope),
                            black_box(&anchors),
                            true,
                            black_box(start),
                            end,
                        )
                    } else {
                        anchors.iter().any(|anchor| {
                            matches!(anchor.ch, ' ' | '\t') && {
                                let offset =
                                    source_offset(black_box(&rope), anchor.line, anchor.col);
                                offset >= black_box(start) && offset < end
                            }
                        })
                    }
                };
                for _ in 0..100 {
                    black_box(lookup());
                }
                let now = Instant::now();
                for _ in 0..iterations {
                    black_box(lookup());
                }
                samples[usize::from(indexed)].push(now.elapsed() / iterations);
            }
        }
        for samples in &mut samples {
            samples.sort_unstable();
        }
        eprintln!(
            "inserted whitespace per lookup median: full scan {:?}, indexed {:?}",
            samples[0][3], samples[1][3]
        );
    }

    #[test]
    fn render_buffer_roundtrips_reuse_events_and_keep_the_original_motion_clock() {
        let mut state = AnimationState::default();
        let mut settings = AnimationSettings::default();
        settings.set(Effect::CursorSmear, true);
        let now = Instant::now();
        let initial = snapshot("abcdefghijklmnopqrstuvwxyz", 0);
        let moved = snapshot("abcdefghijklmnopqrstuvwxyz", 20);
        state.observe_geometry(initial.clone(), &anchors(&initial), true, &settings, now);
        state.observe_geometry(moved.clone(), &anchors(&moved), true, &settings, now);
        let events = state.take_events(moved.id);
        let allocation = events.as_ptr();
        let capacity = events.capacity();
        assert_eq!(events.len(), 1);
        state.restore_events(moved.id, events);
        for frame in 0..10 {
            let at = now + Duration::from_millis(frame * 10);
            state.observe_geometry(moved.clone(), &anchors(&moved), true, &settings, at);
            state.update_scope(moved.id, None, &settings, at);
            let events = state.take_events(moved.id);
            assert_eq!(events.as_ptr(), allocation);
            assert_eq!(events.capacity(), capacity);
            assert_eq!(events[0].started, now);
            assert!(events[0].smear_sample(&settings, at).is_some());
            state.restore_events(moved.id, events);
        }
    }

    #[test]
    fn ordered_and_reordered_geometry_keep_identical_caret_projections_and_edit_extents() {
        let before = snapshot("abcdefgh ijklmnop", 8);
        let mut after = snapshot("abcdefgh  ijklmnop", 9);
        after.content_version += 1;
        let sorted = anchors(&before);
        let mut reordered = sorted.clone();
        reordered.rotate_left(5);
        for position in [(0, 0), (0, 8), (0, 15), (0, 40)] {
            assert_eq!(
                screen_point(&sorted, position, true),
                screen_point(&reordered, position, false)
            );
        }
        assert_eq!(anchor_range(&sorted, true), anchor_range(&reordered, false));
        assert_eq!(
            changed_range_with_bounds(
                &before,
                &after,
                source_bounds(&before.rope, anchor_range(&sorted, true)),
                source_bounds(&after.rope, anchor_range(&anchors(&after), true))
            ),
            changed_range(&before, &after, &reordered, &anchors(&after)),
        );
    }

    #[test]
    fn cached_immediate_scopes_follow_worker_queries_across_navigation_and_source_replacement() {
        let mut editor = crate::editor::Editor::test_with_text(
            "fn outer() {\n  value();\n  if ready {\n    nested();\n  }\n  value();\n}\n",
        );
        editor.language_override = Some("Rust".into());
        let worker =
            crate::animation_structural_scope::StructuralScopeRequest::from_editor(&editor)
                .unwrap()
                .build(None, || false)
                .unwrap();
        let mut state = AnimationState::default();
        for turn in 0..50 {
            for line in 0..editor.rope.len_lines() {
                editor.cursor_line = line;
                editor.cursor_col = turn % editor.rope.line(line).len_chars().max(1);
                assert_eq!(state.scope(&editor, None), worker.enclosing_scope(&editor));
                if line < 7 {
                    assert!(state.scope_cache[&editor.navigation_id].immediate.is_some());
                }
            }
        }
        // Reusing a version number cannot make the old fallback decorate a
        // separately allocated replacement source.
        editor.rope = Rope::from_str("plain\nsource\n");
        editor.cursor_line = 0;
        editor.cursor_col = 0;
        assert_eq!(state.scope(&editor, None), None);
    }

    #[test]
    #[ignore = "comparative benchmark of repeated source discovery versus cached cursor queries"]
    fn benchmark_cached_scope_navigation() {
        use std::hint::black_box;
        let mut editor = crate::editor::Editor::test_with_text(&format!(
            "fn outer() {{\n{}\n}}\n",
            "  if ready {\n    value();\n  }\n".repeat(120)
        ));
        editor.language_override = Some("Rust".into());
        let mut state = AnimationState::default();
        editor.cursor_line = 1;
        black_box(state.scope(&editor, None));
        let mut fresh = Duration::ZERO;
        let mut cached = Duration::ZERO;
        for round in 0..6 {
            for reuse in [round % 2 == 0, round % 2 != 0] {
                let start = Instant::now();
                for query in 0..200 {
                    editor.cursor_line = 1 + query % 350;
                    editor.cursor_col = query % 8;
                    let scope = if reuse {
                        state.scope(&editor, None)
                    } else {
                        crate::animation_structural_scope::enclosing_scope(&editor)
                    };
                    black_box(scope);
                }
                if reuse {
                    cached += start.elapsed();
                } else {
                    fresh += start.elapsed();
                }
            }
        }
        eprintln!(
            "scope navigation 1200 queries: reparsed={fresh:?}, retained_intervals={cached:?}, {:.2}x",
            fresh.as_secs_f64() / cached.as_secs_f64()
        );
    }

    #[test]
    fn layout_remapping_clears_old_screen_space_motion_without_a_scroll_flash() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::CursorSmear, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let initial = snapshot("abcdefghijklmnopqrstuvwxyz", 1);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let moved = snapshot("abcdefghijklmnopqrstuvwxyz", 20);
        let events = state.observe(moved.clone(), &anchors(&moved), &settings, now);
        assert_eq!(events.len(), 1);
        let shifted: Vec<_> = anchors(&moved)
            .into_iter()
            .map(|mut anchor| {
                anchor.y += 1;
                anchor
            })
            .collect();
        let after = state.observe(moved, &shifted, &settings, now + Duration::from_millis(20));
        assert!(after.is_empty());
        assert_eq!(
            state.next_frame_wait(&settings, now + Duration::from_millis(20)),
            None
        );
    }

    #[test]
    fn inserted_highlights_follow_source_edits_and_discard_replaced_cells() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::WhitespaceReveal, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let old = snapshot("abcdEF", 3);
        state.observe(old.clone(), &anchors(&old), &settings, now);
        let mut first = snapshot("abc dEF", 4);
        first.content_version = 2;
        state.observe(first.clone(), &anchors(&first), &settings, now);
        let mut second = snapshot("a bc dEF", 2);
        second.content_version = 3;
        let events = state.observe(second.clone(), &anchors(&second), &settings, now);
        assert!(events.iter().any(|event| matches!(
            event.trigger,
            Trigger::Insert {
                start: 4,
                end: 5,
                ..
            }
        )));
        let mut third = snapshot("a bcdEF", 4);
        third.content_version = 4;
        let events = state.observe(third.clone(), &anchors(&third), &settings, now);
        assert!(!events.iter().any(|event| matches!(
            event.trigger,
            Trigger::Insert {
                start: 4,
                end: 5,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event.trigger,
            Trigger::Insert {
                start: 1,
                end: 2,
                ..
            }
        )));
    }

    #[test]
    fn tab_indicator_retargets_continuously_and_quiets_after_settling() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::TabSlide, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        let first = state
            .tab_indicator(
                1,
                Rect::new(0, 0, 10, 1),
                Rect::new(0, 0, 80, 1),
                0,
                &settings,
                now,
            )
            .unwrap();
        assert_eq!((first.left, first.right), (0.0, 10.0));
        assert_eq!(state.next_frame_wait(&settings, now), None);
        state.tab_indicator(
            2,
            Rect::new(20, 0, 15, 1),
            Rect::new(0, 0, 80, 1),
            0,
            &settings,
            now,
        );
        let sampled_at = now + Duration::from_millis(30);
        let sampled = state
            .tab_indicator(
                2,
                Rect::new(20, 0, 15, 1),
                Rect::new(0, 0, 80, 1),
                0,
                &settings,
                sampled_at,
            )
            .unwrap();
        let retarget = state
            .tab_indicator(
                3,
                Rect::new(40, 0, 8, 1),
                Rect::new(0, 0, 80, 1),
                0,
                &settings,
                sampled_at,
            )
            .unwrap();
        assert!((sampled.left - retarget.left).abs() < 0.0001);
        assert!((sampled.right - retarget.right).abs() < 0.0001);
        assert!(state.next_frame_wait(&settings, sampled_at).is_some());
        let settled_at = now + Duration::from_secs(1);
        assert!(state.advance_frame(&settings, settled_at));
        let settled = state
            .tab_indicator(
                3,
                Rect::new(40, 0, 8, 1),
                Rect::new(0, 0, 80, 1),
                0,
                &settings,
                settled_at,
            )
            .unwrap();
        assert_eq!((settled.left, settled.right), (40.0, 48.0));
        assert_eq!(state.next_frame_wait(&settings, settled_at), None);
        state.begin_frame();
        assert_eq!(state.next_frame_wait(&settings, settled_at), None);
    }

    #[test]
    fn fire_embers_outlive_cursor_motion_and_finish_with_one_clean_frame() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::CursorFire, true);
        let mut state = AnimationState::default();
        let now = Instant::now();
        state.set_active_view(Some(1));
        let initial = snapshot("abcdefghijklmnopqrstuvwxyz", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let moved = snapshot("abcdefghijklmnopqrstuvwxyz", 20);
        state.observe(moved.clone(), &anchors(&moved), &settings, now);
        let cooling = now + lifetime(&settings) + Duration::from_millis(150);
        assert!(state.advance_frame(&settings, cooling));
        let events = state.observe(moved.clone(), &anchors(&moved), &settings, cooling);
        assert!(events.is_empty(), "ordinary cursor motion has finished");
        assert!(
            !state.fire_particles(1, &settings, cooling).is_empty(),
            "cooling embers remain visible"
        );
        assert!(state.next_frame_wait(&settings, cooling).is_some());
        let finished = now + Duration::from_secs(3);
        assert!(state.advance_frame(&settings, finished));
        state.observe(moved.clone(), &anchors(&moved), &settings, finished);
        assert!(state.fire_particles(1, &settings, finished).is_empty());
        assert_eq!(state.next_frame_wait(&settings, finished), None);
        assert!(!state.advance_frame(&settings, finished));
    }

    #[test]
    fn fire_retarget_keeps_existing_embers_and_switch_remap_disable_clear_them() {
        for reset in 0..3 {
            let mut settings = AnimationSettings::default();
            settings.set(Effect::CursorFire, true);
            let mut state = AnimationState::default();
            let now = Instant::now();
            state.set_active_view(Some(1));
            let initial = snapshot("abcdefghijklmnopqrstuvwxyz", 0);
            state.observe(initial.clone(), &anchors(&initial), &settings, now);
            let moved = snapshot("abcdefghijklmnopqrstuvwxyz", 20);
            state.observe(moved.clone(), &anchors(&moved), &settings, now);
            let turn = now + Duration::from_millis(80);
            state.observe(moved.clone(), &anchors(&moved), &settings, turn);
            let before = state.fire_particles(1, &settings, turn);
            assert!(!before.is_empty());
            let mut reversed = snapshot("abcdefghijklmnopqrstuvwxyz", 4);
            state.observe(reversed.clone(), &anchors(&reversed), &settings, turn);
            let after = state.fire_particles(1, &settings, turn);
            for old in before {
                assert!(after.iter().any(|particle| particle.seed == old.seed
                    && (particle.position.x - old.position.x).abs() < 0.0001
                    && (particle.position.y - old.position.y).abs() < 0.0001));
            }
            match reset {
                0 => {
                    settings.set(Effect::CursorFire, false);
                    settings.set(Effect::WhitespaceReveal, true);
                }
                1 => {
                    reversed.scroll.0 = 1;
                }
                _ => {
                    state.set_active_view(Some(2));
                    state.set_active_view(Some(1));
                }
            }
            state.observe(reversed.clone(), &anchors(&reversed), &settings, turn);
            assert!(
                state.fire_particles(1, &settings, turn).is_empty(),
                "reset {reset} drops screen-space fire"
            );
        }
    }

    #[test]
    fn returning_to_a_tab_does_not_replay_its_live_fire_core_or_embers() {
        let mut settings = AnimationSettings::default();
        settings.set(Effect::CursorFire, true);
        assert!(!settings.enabled(Effect::CursorSmear));
        let mut state = AnimationState::default();
        let now = Instant::now();
        state.set_active_view(Some(1));
        let initial = snapshot("abcdefghijklmnopqrstuvwxyz", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let moved = snapshot("abcdefghijklmnopqrstuvwxyz", 20);
        let events = state.observe(moved.clone(), &anchors(&moved), &settings, now);
        assert!(
            events
                .iter()
                .any(|event| event.smear_sample(&settings, now).is_some())
        );
        let switched = now + Duration::from_millis(80);
        state.observe(moved.clone(), &anchors(&moved), &settings, switched);
        assert!(!state.fire_particles(1, &settings, switched).is_empty());
        assert!(state.cursor_smear_active(1, &settings, switched));

        state.begin_frame();
        state.set_active_view(Some(2));
        let mut second = snapshot("other tab", 0);
        second.id = 2;
        state.observe(second.clone(), &anchors(&second), &settings, switched);
        let returned = switched + Duration::from_millis(10);
        assert!(returned.duration_since(now) < lifetime(&settings));
        state.begin_frame();
        state.set_active_view(Some(1));
        let events = state.observe(moved.clone(), &anchors(&moved), &settings, returned);
        assert!(
            events
                .iter()
                .all(|event| !matches!(event.trigger, Trigger::Cursor { .. }))
        );
        assert!(!state.cursor_smear_active(1, &settings, returned));
        assert!(state.fire_particles(1, &settings, returned).is_empty());
        assert_eq!(state.next_frame_wait(&settings, returned), None);

        // A new movement starts from the actual caret, not the discarded path.
        let next = snapshot("abcdefghijklmnopqrstuvwxyz", 24);
        let events = state.observe(next.clone(), &anchors(&next), &settings, returned);
        let core = events
            .iter()
            .find_map(|event| event.smear_sample(&settings, returned))
            .unwrap();
        assert!((core.corners[0].x - 20.0).abs() < 0.0001);
        assert!(core.corners[0].y.abs() < 0.0001);
        assert!(state.cursor_smear_active(1, &settings, returned));
    }

    #[test]
    fn progress_is_bounded_and_monotonic() {
        let started = Instant::now();
        let event = AnimationEvent {
            trigger: Trigger::Insert {
                start: 0,
                end: 1,
                paste: false,
            },
            started,
        };
        let settings = AnimationSettings::default();
        assert_eq!(event.progress(&settings, started), 0.0);
        assert_eq!(
            event.progress(&settings, started + Duration::from_secs(10)),
            1.0
        );
    }

    #[test]
    fn animation_events_use_their_own_durations_and_share_the_frame_cap() {
        let started = Instant::now();
        let mut settings = AnimationSettings::default();
        for effect in Effect::ALL {
            settings.set(effect, true);
        }
        settings.set_duration(Effect::CursorSmear, 80);
        settings.set_duration(Effect::CursorFire, 900);
        settings.set_duration(Effect::WhitespaceReveal, 400);
        settings.set_duration(Effect::ScopeConnector, 1200);
        settings.set_duration(Effect::TabSlide, 150);
        settings.fps = 30;
        let cursor = AnimationEvent {
            trigger: Trigger::Cursor {
                motion: SmearMotion::retarget(
                    MotionPoint::default(),
                    MotionPoint { x: 20.0, y: 0.0 },
                    None,
                    false,
                ),
            },
            started,
        };
        let insert = AnimationEvent {
            trigger: Trigger::Insert {
                start: 0,
                end: 1,
                paste: false,
            },
            started,
        };
        let scope = AnimationEvent {
            trigger: Trigger::Scope {
                pair: ((0, 0), (2, 0)),
                origin: (0, 0),
                exiting: None,
            },
            started,
        };
        let at = started + Duration::from_millis(100);
        assert!(
            cursor
                .cursor_sample(&settings, Effect::CursorSmear, at)
                .is_none()
        );
        assert!(
            cursor
                .cursor_sample(&settings, Effect::CursorFire, at)
                .is_some()
        );
        assert!((insert.progress(&settings, at) - 0.25).abs() < 0.0001);
        assert!((scope.progress(&settings, at) - 1.0 / 12.0).abs() < 0.0001);
        assert!(!insert.active(&settings, started + Duration::from_millis(401)));
        assert!(scope.active(&settings, started + Duration::from_millis(901)));
        assert!(!cursor.active(&settings, started + Duration::from_millis(901)));

        let mut state = AnimationState::default();
        let view = snapshot("abc", 0);
        state.observe(view.clone(), &anchors(&view), &settings, started);
        state.views.get_mut(&view.id).unwrap().events = vec![cursor, insert, scope];
        for (id, x) in [(1, 0), (2, 20)] {
            state.tab_indicator(
                id,
                Rect::new(x, 0, 10, 1),
                Rect::new(0, 0, 80, 1),
                0,
                &settings,
                started,
            );
        }
        let tab = state
            .tab_indicator(
                2,
                Rect::new(20, 0, 10, 1),
                Rect::new(0, 0, 80, 1),
                0,
                &settings,
                started + Duration::from_millis(151),
            )
            .unwrap();
        assert_eq!((tab.left, tab.right), (20.0, 30.0));
        assert_eq!(
            state.next_frame_wait(&settings, started),
            Some(Duration::from_secs_f64(1.0 / 30.0))
        );
        assert_eq!(
            state.next_frame_wait(&settings, started + Duration::from_millis(34)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn all_effect_clocks_reach_the_configured_one_and_five_thousand_ms_bounds() {
        let started = Instant::now();
        for duration_ms in [1, 5000] {
            let mut settings = AnimationSettings::default();
            for effect in Effect::ALL {
                settings.set(effect, true);
                settings.set_duration(effect, duration_ms);
                assert_eq!(
                    effect_lifetime(&settings, effect),
                    Duration::from_millis(duration_ms)
                );
            }
            for trigger in [
                Trigger::Cursor {
                    motion: SmearMotion::retarget(
                        MotionPoint::default(),
                        MotionPoint { x: 20.0, y: 0.0 },
                        None,
                        false,
                    ),
                },
                Trigger::Insert {
                    start: 0,
                    end: 1,
                    paste: false,
                },
                Trigger::Scope {
                    pair: ((0, 0), (2, 0)),
                    origin: (0, 0),
                    exiting: None,
                },
            ] {
                let event = AnimationEvent { trigger, started };
                let halfway = started + Duration::from_micros(duration_ms * 500);
                assert!((event.progress(&settings, halfway) - 0.5).abs() < 0.0001);
                assert!(event.active(&settings, halfway));
                let finished = started + Duration::from_millis(duration_ms);
                assert_eq!(event.progress(&settings, finished), 1.0);
                assert!(!event.active(&settings, finished));
            }
            let state = AnimationState::default();
            for (id, x) in [(1, 0), (2, 20)] {
                state.tab_indicator(
                    id,
                    Rect::new(x, 0, 10, 1),
                    Rect::new(0, 0, 80, 1),
                    0,
                    &settings,
                    started,
                );
            }
            let tab = state
                .tab_indicator(
                    2,
                    Rect::new(20, 0, 10, 1),
                    Rect::new(0, 0, 80, 1),
                    0,
                    &settings,
                    started + Duration::from_millis(duration_ms),
                )
                .unwrap();
            assert_eq!((tab.left, tab.right), (20.0, 30.0));
        }
    }

    #[test]
    fn scope_exit_scales_down_with_short_duration_instead_of_lingering() {
        let started = Instant::now();
        for duration_ms in [1, 5, 79, 240, 5000] {
            let mut settings = AnimationSettings::default();
            settings.set(Effect::ScopeConnector, true);
            settings.set_duration(Effect::ScopeConnector, duration_ms);
            let exit_duration = AnimationEvent::scope_exit_duration(&settings);
            assert!(exit_duration >= Duration::from_millis(1));
            assert!(exit_duration <= Duration::from_millis(duration_ms));
            let event = AnimationEvent {
                trigger: Trigger::Scope {
                    pair: ((0, 0), (2, 0)),
                    origin: (0, 0),
                    exiting: Some(started),
                },
                started,
            };
            assert!(event.active(&settings, started));
            assert!(!event.active(&settings, started + exit_duration));
        }
    }

    #[test]
    fn a_settled_scope_remains_static_without_scheduling_or_replaying_motion() {
        let now = Instant::now();
        let mut settings = AnimationSettings::default();
        settings.set(Effect::ScopeConnector, true);
        let mut state = AnimationState::default();
        let view = snapshot("{\n work();\n}", 0);
        let pair = ((0, 0), (2, 0));
        state.observe(view.clone(), &anchors(&view), &settings, now);
        state.update_scope(view.id, Some(pair), &settings, now);
        let settled = now + Duration::from_secs(1);
        assert!(state.advance_frame(&settings, settled));
        for frame in 0..100 {
            let at = settled + Duration::from_millis(frame * 16);
            state.begin_frame();
            state.observe(view.clone(), &anchors(&view), &settings, at);
            assert!(
                state
                    .update_scope(view.id, Some(pair), &settings, at)
                    .is_empty()
            );
            assert_eq!(state.views[&view.id].scope, Some(pair));
            assert_eq!(state.next_frame_wait(&settings, at), None);
            assert!(!state.advance_frame(&settings, at));
        }
        let exited = settled + Duration::from_secs(2);
        let events = state.update_scope(view.id, None, &settings, exited);
        assert!(matches!(
            events[0].trigger,
            Trigger::Scope {
                exiting: Some(_),
                ..
            }
        ));
        let finished =
            exited + AnimationEvent::scope_exit_duration(&settings) + Duration::from_millis(1);
        assert!(state.advance_frame(&settings, finished));
        assert_eq!(state.next_frame_wait(&settings, finished), None);
    }
    #[test]
    fn cursor_effects_have_independent_live_geometry_and_fire_brightness() {
        let now = Instant::now();
        let mut settings = AnimationSettings::default();
        settings.set(Effect::CursorSmear, true);
        settings.set(Effect::CursorFire, true);
        settings.set_duration(Effect::CursorSmear, 80);
        settings.set_duration(Effect::CursorFire, 800);
        settings.set_intensity(Effect::CursorSmear, 90);
        settings.set_intensity(Effect::CursorFire, 25);
        let mut state = AnimationState::default();
        let initial = snapshot("abcdefghijklmnopqrstuvwxyz", 0);
        state.observe(initial.clone(), &anchors(&initial), &settings, now);
        let moved = snapshot("abcdefghijklmnopqrstuvwxyz", 20);
        state.observe(moved.clone(), &anchors(&moved), &settings, now);
        let at = now + Duration::from_millis(200);
        let events = state.observe(moved.clone(), &anchors(&moved), &settings, at);
        assert!(events.iter().all(|event| {
            event
                .cursor_sample(&settings, Effect::CursorSmear, at)
                .is_none()
        }));
        assert!(state.fire_core(moved.id, &settings, at).is_some());
        let particles = state.fire_particles(moved.id, &settings, at);
        assert!(!particles.is_empty());
        settings.set_intensity(Effect::CursorSmear, 5);
        assert_eq!(particles, state.fire_particles(moved.id, &settings, at));
        settings.set_intensity(Effect::CursorFire, 100);
        let brighter = state.fire_particles(moved.id, &settings, at);
        assert_eq!(particles.len(), brighter.len());
        assert!(
            brighter
                .iter()
                .zip(particles)
                .all(|(bright, dim)| bright.opacity > dim.opacity)
        );
    }
}

#[cfg(test)]
#[path = "animation_state_refinement_tests.rs"]
mod refinement_tests;

#[cfg(test)]
#[path = "animation_scope_edit_tests.rs"]
mod scope_edit_tests;
