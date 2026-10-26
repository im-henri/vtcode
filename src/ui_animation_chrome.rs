//! Static workbench surfaces can reuse their cells on certified cosmetic frames.
//! Ordinary input/worker frames invalidate them, and overlays still paint last.
//! Only surfaces without frame-local hit maps are eligible; editable Explorer
//! fields always render normally so their caret and mouse geometry stay live.
use crate::app::App;
use ratatui::{Frame, buffer::Buffer, layout::Rect};

const MAX_CELLS: usize = 32_768;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Region {
    Menu,
    Activity,
    Explorer,
    Outline,
}

impl Region {
    fn slot(self) -> usize {
        match self {
            Self::Menu => 0,
            Self::Activity => 1,
            Self::Explorer | Self::Outline => 2,
        }
    }
}

struct Scene {
    region: Region,
    cells: Buffer,
}

#[derive(Default)]
pub(crate) struct Cache {
    scenes: [Option<Scene>; 3],
    valid: u8,
    enabled: bool,
    #[cfg(test)]
    pub(crate) replays: usize,
}

impl Cache {
    pub(crate) fn begin_frame(&mut self, animation_only: bool, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.scenes = Default::default();
        }
        if !animation_only || !enabled {
            self.valid = 0;
        }
    }
}

#[cfg(test)]
thread_local! {
    static REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn draw(frame: &mut Frame, app: &mut App, area: Rect, region: Region) {
    let slot = region.slot();
    let eligible = app.animation_frame_cache.chrome.enabled
        && !area.is_empty()
        && area.intersection(frame.area()) == area
        && usize::from(area.width) * usize::from(area.height) <= MAX_CELLS
        && (region != Region::Explorer || (!app.explorer.find.open && !app.rename.active));
    #[cfg(test)]
    let eligible = eligible && !REFERENCE.get();
    let cache = &mut app.animation_frame_cache.chrome;
    if eligible
        && app.rendering_animation_frame
        && cache.valid & (1 << slot) != 0
        && let Some(scene) = cache.scenes[slot].as_ref()
        && scene.region == region
        && scene.cells.area == area
    {
        copy_area(&scene.cells, frame.buffer_mut(), area);
        #[cfg(test)]
        {
            cache.replays += 1;
        }
        return;
    }

    match region {
        Region::Menu => crate::ui_chrome::draw_menu_bar(frame, app, area),
        Region::Activity => crate::ui_chrome::draw_activity_bar(frame, app, area),
        Region::Explorer => crate::ui_chrome::draw_explorer(frame, app, area),
        Region::Outline => crate::ui_chrome::draw_outline(frame, app, area),
    }
    let cache = &mut app.animation_frame_cache.chrome;
    if eligible && app.rendering_animation_frame {
        // Capture lazily on the first cosmetic tick. Typing, scrolling, and
        // quiet enabled effects must not pay for copying unused chrome.
        let scene = cache.scenes[slot].get_or_insert_with(|| Scene {
            region,
            cells: Buffer::empty(area),
        });
        scene.region = region;
        scene.cells.resize(area);
        copy_area(frame.buffer_mut(), &mut scene.cells, area);
        cache.valid |= 1 << slot;
    } else if !eligible {
        cache.scenes[slot] = None;
        cache.valid &= !(1 << slot);
    }
}

fn copy_area(source: &Buffer, target: &mut Buffer, area: Rect) {
    for y in area.y..area.bottom() {
        let from = source.index_of(area.x, y);
        let to = target.index_of(area.x, y);
        let width = usize::from(area.width);
        target.content[to..to + width].clone_from_slice(&source.content[from..from + width]);
    }
}

#[cfg(test)]
#[path = "ui_animation_chrome_tests.rs"]
mod tests;
