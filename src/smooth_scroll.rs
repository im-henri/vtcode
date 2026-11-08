//! Animate the real document viewport in whole display rows. Wheel deltas
//! accumulate at the destination, even when several arrive before a frame.
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::{App, Tab, TabKind};
use crate::inline_diff::InlineDiffScroll;

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SETTLE_LIMIT: Duration = Duration::from_millis(96);

#[derive(Default)]
pub(crate) struct WheelScroll {
    motion: Option<Motion>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Text(Option<usize>),
    Diff(usize, bool, Option<crate::diff_wrap::Widths>),
    Markdown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Position {
    editor_id: u64,
    tab: TabKind,
    version: u64,
    layout: Layout,
    row: usize,
}

struct Motion {
    current: Position,
    target: usize,
    next_frame: Instant,
    last_wheel: Instant,
}

struct Viewport<'a> {
    tab: &'a mut Tab,
    diff: Option<&'a mut InlineDiffScroll>,
}

impl Viewport<'_> {
    fn can_animate(&self) -> bool {
        !self.tab.editor.bg_loading
            && self.tab.extension_view.is_none()
            && match self.tab.kind {
                TabKind::File | TabKind::GitRevision => true,
                TabKind::MarkdownPreview => self.tab.editor.markdown_preview_scroll.is_some(),
                _ => self.diff.is_some(),
            }
    }

    fn position(&self) -> Position {
        let editor = &self.tab.editor;
        let (layout, row) = if let Some(diff) = &self.diff {
            (
                Layout::Diff(
                    Arc::as_ptr(&diff.cache) as usize,
                    diff.aligned,
                    diff.wrap.widths(),
                ),
                diff.row,
            )
        } else if let Some(preview) = &editor.markdown_preview_scroll {
            (Layout::Markdown, preview.row)
        } else if editor.word_wrap {
            (
                Layout::Text(Some(editor.effective_wrap_width())),
                editor.wrap_scroll_offset,
            )
        } else {
            (Layout::Text(None), editor.visible_scroll_offset())
        };
        Position {
            editor_id: editor.navigation_id,
            tab: self.tab.kind,
            version: editor.content_version,
            layout,
            row,
        }
    }

    fn limit(&mut self) -> usize {
        let editor = &mut self.tab.editor;
        if let Some(diff) = &self.diff {
            diff.max_row()
        } else if let Some(preview) = &editor.markdown_preview_scroll {
            preview.max_scroll()
        } else if editor.word_wrap {
            editor.max_wrap_scroll_offset()
        } else {
            editor.max_visible_scroll_offset()
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let editor = &mut self.tab.editor;
        if let Some(diff) = &mut self.diff {
            diff.scroll(editor, false, delta);
        } else if self.tab.kind == TabKind::MarkdownPreview {
            if let Some(preview) = &mut editor.markdown_preview_scroll {
                preview.scroll(delta);
                editor.scroll_offset = preview.source_scroll;
            }
        } else if delta < 0 {
            editor.scroll_up(delta.unsigned_abs());
        } else {
            editor.scroll_down(delta as usize);
        }
    }

    fn step(&mut self, current: usize, target: usize, rows: usize) {
        let rows = rows as isize;
        self.scroll_by(if target < current { -rows } else { rows });
    }
}

impl WheelScroll {
    pub(crate) fn next_wait(&self, now: Instant) -> Option<Duration> {
        self.motion
            .as_ref()
            .map(|motion| motion.next_frame.saturating_duration_since(now))
    }

    fn input(&mut self, mut view: Viewport<'_>, delta: isize, now: Instant) {
        let current = view.position();
        let previous = self
            .motion
            .take()
            .filter(|motion| motion.current == current);
        let destination = previous
            .as_ref()
            .map_or(current.row, |motion| motion.target);
        let target = destination.saturating_add_signed(delta).min(view.limit());
        if target == current.row {
            return;
        }
        let next_frame = match previous {
            Some(motion) if target.cmp(&current.row) == destination.cmp(&current.row) => {
                motion.next_frame
            }
            _ => {
                // A fresh gesture or reversal moves a row on the input frame.
                view.step(current.row, target, 1);
                now + FRAME_INTERVAL
            }
        };
        let current = view.position();
        if current.row != target {
            self.motion = Some(Motion {
                current,
                target,
                next_frame,
                last_wheel: now,
            });
        }
    }
}

pub(crate) fn scroll_rows(
    driver: Option<&mut WheelScroll>,
    tab: &mut Tab,
    diff: Option<&mut InlineDiffScroll>,
    delta: isize,
) {
    tab.editor.refresh_document();
    let mut view = Viewport { tab, diff };
    if let Some(driver) = driver {
        if delta.unsigned_abs() > 1 && view.can_animate() {
            driver.input(view, delta, Instant::now());
            return;
        }
        driver.motion = None;
    }
    view.scroll_by(delta);
}

impl App {
    pub(crate) fn cancel_wheel_scroll(&mut self) {
        if let Some(driver) = &mut self.wheel_scroll {
            driver.motion = None;
        }
    }

    pub(crate) fn advance_wheel_scroll(&mut self, now: Instant) -> bool {
        let Some(mut motion) = self
            .wheel_scroll
            .as_mut()
            .and_then(|driver| driver.motion.take_if(|motion| motion.next_frame <= now))
        else {
            return false;
        };
        let Some(group) = self.visible_tab_group(motion.current.editor_id, motion.current.tab)
        else {
            return false;
        };
        let (tab, diff) = if group == self.active_group {
            let row_diff = self.row_diff_active();
            (
                &mut self.tabs[self.active_tab],
                row_diff.then_some(&mut self.diff_inline_scroll),
            )
        } else {
            let group = &mut self.groups[group];
            let tab = &mut group.tabs[group.active_tab];
            let diff = self.diff_tabs.row_scroll_mut(tab.kind);
            (tab, diff)
        };
        tab.editor.refresh_document();
        let mut view = Viewport { tab, diff };
        if self.scroll_speed <= 1 || !view.can_animate() || view.position() != motion.current {
            return false;
        }
        let current = motion.current.row;
        motion.target = motion.target.min(view.limit());
        let gap = current.abs_diff(motion.target);
        if gap == 0 {
            return false;
        }

        // Half the remaining gap per frame. A late frame accounts for the
        // elapsed time, and integer rounding always makes at least one step.
        let elapsed = FRAME_INTERVAL + now.saturating_duration_since(motion.next_frame);
        let fraction = 1.0 - 0.5_f64.powf(elapsed.as_secs_f64() / FRAME_INTERVAL.as_secs_f64());
        let rows = if now.saturating_duration_since(motion.last_wheel) >= SETTLE_LIMIT {
            gap
        } else {
            ((gap as f64 * fraction).round() as usize).clamp(1, gap)
        };
        view.step(current, motion.target, rows);
        motion.current = view.position();
        let changed = motion.current.row != current;
        if changed && motion.current.row != motion.target {
            motion.next_frame = now + FRAME_INTERVAL;
            self.wheel_scroll.as_mut().unwrap().motion = Some(motion);
        }
        if changed && self.markdown_preview {
            self.sync_markdown_preview_state();
        }
        changed
    }
}
