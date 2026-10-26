//! Reuse the static editor scene on frames requested only by cosmetic effects.
//! The event loop certifies these frames: input and worker changes always take
//! the ordinary rendering path. Static chrome has a separate bounded cache;
//! blame cards, interactive fields and dialogs still render in their original
//! order, so neither hit maps nor occlusion are cached.
use crate::{app::App, ui_editor_animations::Geometry};
use ratatui::{Frame, buffer::Buffer, layout::Rect};

const MAX_SCENES: usize = 16;
const MAX_SCENE_CELLS: usize = 262_144;

#[cfg(test)]
thread_local! {
    static REFERENCE_FRESH_GEOMETRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// Keep the immutable source alive and compare its identity, never its contents.
// A file replacement may reuse a revision number; a dropped allocation may
// also have its address reused by a later comparison cache.
struct SourceIdentity(ropey::Rope);

impl PartialEq for SourceIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_instance(&other.0)
    }
}
impl Eq for SourceIdentity {}

struct ComparisonIdentity(std::sync::Arc<crate::app::DiffRenderCache>);

impl PartialEq for ComparisonIdentity {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ComparisonIdentity {}

#[derive(PartialEq, Eq)]
struct Key {
    id: u64,
    source: SourceIdentity,
    area: Rect,
    version: u64,
    wrap_revision: u64,
    cursor: (usize, usize),
    selection: Option<(usize, usize, usize, usize)>,
    scroll: (usize, usize, usize),
    inactive: bool,
    cursor_visible: bool,
    kind: u8,
    comparison: ComparisonIdentity,
    diff_cursor: Option<(bool, (usize, usize))>,
    diff_scroll: (usize, usize),
}

impl Key {
    fn current(app: &App, area: Rect) -> Option<Self> {
        let editor = app.editor()?;
        if !editor.extra_cursors.is_empty() {
            return None;
        }
        Some(Self {
            id: editor.navigation_id,
            source: SourceIdentity(editor.rope.clone()),
            area,
            version: editor.content_version,
            wrap_revision: editor.wrap_revision,
            cursor: (editor.cursor_line, editor.cursor_col),
            selection: editor.selection_range(),
            scroll: (
                editor.scroll_offset,
                editor.horizontal_scroll,
                editor.wrap_scroll_offset,
            ),
            inactive: app.rendering_inactive_editor,
            cursor_visible: app.cursor_visible,
            kind: 0,
            comparison: ComparisonIdentity(std::sync::Arc::clone(&app.diff_render_cache)),
            diff_cursor: app.diff_scope_cursor(),
            diff_scroll: (app.diff_inline_scroll.row, app.diff_inline_scroll.column),
        })
    }
}

struct Layout {
    code: Option<Rect>,
    gutter: Option<Rect>,
    gutter_width: u16,
    minimap: Option<Rect>,
    scrollbar: Option<Rect>,
    horizontal_scrollbar: Option<Rect>,
    inline_blame: Option<(Rect, usize)>,
    inline_blame_candidate: Option<(Rect, usize)>,
    fold_buttons: crate::ui_editor_folds::Buttons,
    editor: Option<Rect>,
    diff_split: Option<Rect>,
    diff_divider: Option<Rect>,
    diff_left_code: Option<Rect>,
    diff_left_scrollbar: Option<Rect>,
}

impl Layout {
    fn capture(app: &App) -> Self {
        Self {
            code: app.code_area,
            gutter: app.gutter_area,
            gutter_width: app.gutter_width,
            minimap: app.minimap_area,
            scrollbar: app.scrollbar_area,
            horizontal_scrollbar: app.h_scrollbar_area,
            inline_blame: app.editor().and_then(|editor| editor.blame.inline_annotation),
            inline_blame_candidate: app.editor().and_then(|editor| editor.blame.inline_candidate),
            fold_buttons: app.editor().map_or_else(Default::default, |editor| editor.fold_buttons.clone()),
            editor: app.editor_area,
            diff_split: app.diff_split_area,
            diff_divider: app.diff_divider_area,
            diff_left_code: app.diff_left_code_area,
            diff_left_scrollbar: app.diff_left_h_scrollbar_area,
        }
    }

    fn restore(&self, app: &mut App) {
        app.code_area = self.code;
        app.gutter_area = self.gutter;
        app.gutter_width = self.gutter_width;
        app.minimap_area = self.minimap;
        app.scrollbar_area = self.scrollbar;
        app.h_scrollbar_area = self.horizontal_scrollbar;
        if let Some(editor) = app.editor_mut() {
            editor.blame.inline_annotation = self.inline_blame;
            editor.blame.inline_candidate = self.inline_blame_candidate;
            editor.fold_buttons.clone_from(&self.fold_buttons);
        }
        app.editor_area = self.editor;
        app.diff_split_area = self.diff_split;
        app.diff_divider_area = self.diff_divider;
        app.diff_left_code_area = self.diff_left_code;
        app.diff_left_h_scrollbar_area = self.diff_left_scrollbar;
    }
}

struct Scene {
    key: Key,
    underlay: Buffer,
    paint: Paint,
    layout: Layout,
}

enum Paint {
    Editor {
        geometry: Geometry,
        gutter: Rect,
        tooltips: bool,
    },
    Diff {
        aligned: bool,
    },
}

#[derive(Default)]
pub(crate) struct AnimationFrameCache {
    pub(crate) chrome: crate::ui_animation_chrome::Cache,
    scenes: Vec<Scene>,
    spare: Vec<Buffer>,
    spare_geometry: Vec<Geometry>,
    previous_geometry: Vec<(u64, Geometry)>,
    capture_area: Option<Rect>,
    #[cfg(test)]
    pub(crate) captures: usize,
    #[cfg(test)]
    pub(crate) replays: usize,
    #[cfg(test)]
    pub(crate) projection_reuses: usize,
}

impl AnimationFrameCache {
    pub(crate) fn begin_frame(&mut self, animation_only: bool, enabled: bool) {
        self.chrome.begin_frame(animation_only, enabled);
        if !enabled {
            self.scenes.clear();
            self.spare.clear();
            self.spare_geometry.clear();
            self.previous_geometry.clear();
        } else if !animation_only {
            // Keep one previous projection until its replacement is painted.
            // Cursor/selection changes often leave every source cell unchanged.
            for (_, mut geometry) in self.previous_geometry.drain(..) {
                geometry.reset(Rect::default(), false);
                self.spare_geometry.push(geometry);
            }
            for scene in self.scenes.drain(..) {
                self.spare.push(scene.underlay);
                if let Paint::Editor { geometry, .. } = scene.paint {
                    #[cfg(test)]
                    if REFERENCE_FRESH_GEOMETRY.get() { continue; }
                    self.previous_geometry.push((scene.key.id, geometry));
                }
            }
            self.spare.truncate(MAX_SCENES);
            self.spare_geometry.truncate(MAX_SCENES);
        }
        self.capture_area = None;
    }

    pub(crate) fn geometry(&mut self, area: Rect, enabled: bool) -> Geometry {
        #[cfg(test)]
        if REFERENCE_FRESH_GEOMETRY.get() {
            return Geometry::new(area, enabled);
        }
        if enabled && let Some(mut geometry) = self.spare_geometry.pop() {
            geometry.configure_recycled(area, enabled);
            geometry
        } else {
            Geometry::new(area, enabled)
        }
    }

    fn underlay(&mut self, frame: &mut Frame, area: Rect) -> Buffer {
        let mut underlay = self.spare.pop().unwrap_or_else(|| Buffer::empty(area));
        underlay.resize(area);
        let buffer = frame.buffer_mut();
        for y in area.y..area.bottom() {
            let source = buffer.index_of(area.x, y);
            let target = underlay.index_of(area.x, y);
            underlay.content[target..target + usize::from(area.width)]
                .clone_from_slice(&buffer.content[source..source + usize::from(area.width)]);
        }
        underlay
    }

    fn record_replay(&mut self) {
        #[cfg(test)]
        {
            self.replays += 1;
        }
    }
}

/// Start before text layout, and return true when the static scene was reused.
pub(crate) fn begin_editor(frame: &mut Frame, app: &mut App, area: Rect) -> bool {
    app.animation_frame_cache.capture_area = Some(area);
    replay(frame, app, area, 0)
}

pub(crate) fn begin_diff(frame: &mut Frame, app: &mut App, area: Rect, aligned: bool) -> bool {
    replay(frame, app, area, if aligned { 1 } else { 2 })
}

fn replay(frame: &mut Frame, app: &mut App, area: Rect, kind: u8) -> bool {
    if crate::ui_editor_animations::suspend_for_multiple_cursors(app)
        || app.copy_animation.active()
        || !app.rendering_animation_frame
        || !(app.animation_settings.any_enabled() || app.loading.active())
    {
        return false;
    }
    let Some(mut key) = Key::current(app, area) else {
        return false;
    };
    key.kind = kind;
    let mut cache = std::mem::take(&mut app.animation_frame_cache);
    let reused = if let Some(scene) = cache.scenes.iter().find(|scene| scene.key == key) {
        scene.layout.restore(app);
        let buffer = frame.buffer_mut();
        for y in area.y..area.bottom() {
            let source = scene.underlay.index_of(area.x, y);
            let target = buffer.index_of(area.x, y);
            buffer.content[target..target + usize::from(area.width)].clone_from_slice(
                &scene.underlay.content[source..source + usize::from(area.width)],
            );
        }
        match &scene.paint {
            Paint::Editor {
                geometry,
                gutter,
                tooltips,
            } => {
                crate::ui_editor::refresh_cached_editor_diagnostics(app);
                crate::ui_editor_animations::draw(frame, app, geometry, *gutter);
                if *tooltips {
                    crate::ui_editor::draw_animation_frame_tooltips(frame, app);
                }
            }
            Paint::Diff { aligned } => {
                crate::ui_diff_display::draw_diff_layers(frame, app, *aligned)
            }
        }
        cache.record_replay();
        true
    } else {
        false
    };
    app.animation_frame_cache = cache;
    reused
}

/// Save text, decorations and overview cells before cosmetic overlays modify
/// them. Only semantic frames pay for this copy or build source geometry.
pub(crate) fn finish_editor(
    frame: &mut Frame,
    app: &mut App,
    mut geometry: Geometry,
    gutter: Rect,
    tooltips: bool,
) {
    if !app.copy_animation.active() && (app.animation_settings.any_enabled() || app.loading.active())
        && !app.should_render_diff_for_active_editor()
        && let Some(area) = app.animation_frame_cache.capture_area
        && !area.is_empty()
        && area.intersection(frame.area()) == area
        && usize::from(area.width) * usize::from(area.height) <= MAX_SCENE_CELLS
        && let Some(key) = Key::current(app, area)
    {
        let underlay = app.animation_frame_cache.underlay(frame, area);
        geometry.retain_validated(frame.buffer_mut());
        let cache = &mut app.animation_frame_cache;
        if let Some(index) = cache.previous_geometry.iter().position(|(id, old)| {
            *id == key.id && old.area == geometry.area
        }) {
            let (_, mut old) = cache.previous_geometry.swap_remove(index);
            let _reused = geometry.reuse_projection(&mut old);
            #[cfg(test)]
            { cache.projection_reuses += usize::from(_reused); }
            old.reset(Rect::default(), false);
            if cache.spare_geometry.len() < MAX_SCENES { cache.spare_geometry.push(old); }
        }
        crate::ui_editor_animations::draw(frame, app, &geometry, gutter);
        let scene = Scene {
            key,
            underlay,
            paint: Paint::Editor {
                geometry,
                gutter,
                tooltips,
            },
            layout: Layout::capture(app),
        };
        let cache = &mut app.animation_frame_cache;
        if let Some(existing) = cache
            .scenes
            .iter_mut()
            .find(|entry| entry.key.id == scene.key.id && entry.key.area == scene.key.area)
        {
            *existing = scene;
        } else if cache.scenes.len() < MAX_SCENES {
            cache.scenes.push(scene);
        }
        #[cfg(test)]
        {
            cache.captures += 1;
        }
    } else {
        crate::ui_editor_animations::draw(frame, app, &geometry, gutter);
    }
}

pub(crate) fn finish_diff(frame: &mut Frame, app: &mut App, area: Rect, aligned: bool) {
    if !app.copy_animation.active() && (app.animation_settings.any_enabled() || app.loading.active())
        && !area.is_empty()
        && area.intersection(frame.area()) == area
        && usize::from(area.width) * usize::from(area.height) <= MAX_SCENE_CELLS
        && let Some(mut key) = Key::current(app, area)
    {
        key.kind = if aligned { 1 } else { 2 };
        let underlay = app.animation_frame_cache.underlay(frame, area);
        let scene = Scene {
            key,
            underlay,
            paint: Paint::Diff { aligned },
            layout: Layout::capture(app),
        };
        let cache = &mut app.animation_frame_cache;
        if let Some(existing) = cache.scenes.iter_mut().find(|entry| {
            entry.key.id == scene.key.id
                && entry.key.area == scene.key.area
                && entry.key.kind == scene.key.kind
        }) {
            *existing = scene;
        } else if cache.scenes.len() < MAX_SCENES {
            cache.scenes.push(scene);
        }
        #[cfg(test)]
        {
            cache.captures += 1;
        }
    }
    crate::ui_diff_display::draw_diff_layers(frame, app, aligned);
}

#[cfg(test)]
#[path = "ui_animation_frame_tests.rs"]
mod tests;
