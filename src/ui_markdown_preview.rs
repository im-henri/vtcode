//! Owned, coalesced preview preparation on the existing interactive CPU pool.
//! The UI keeps the last complete display while a replacement is being built.
use super::*;
use crate::background::{BackgroundWorkers, CancellationToken, UiWake};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

const MAX_JOBS: usize = 2;
const RECENT_PREVIEWS: usize = 4;
const RETAINED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Key {
    pub rope: ropey::Rope,
    pub width: usize,
    pub palette: Palette,
    pub theme: String,
    pub registry: u64,
}
impl Key {
    fn matches(&self, other: &Self) -> bool {
        self.rope.is_instance(&other.rope)
            && self.width == other.width
            && self.palette == other.palette
            && self.theme == other.theme
            && self.registry == other.registry
    }
    fn matches_preview(&self, preview: &CachedPreview) -> bool {
        self.rope.is_instance(&preview.rope)
            && self.width == preview.width
            && self.palette == preview.palette
            && self.theme == preview.theme
            && self.registry == preview.registry
    }
    fn same_style(&self, other: &Self) -> bool {
        self.palette == other.palette
            && self.theme == other.theme
            && self.registry == other.registry
    }
}

struct Preparation {
    key: Key,
    document: Arc<PreparedDocument>,
    reset_style: bool,
}
impl Preparation {
    fn from_preview(preview: &CachedPreview) -> Self {
        Self {
            key: Key {
                rope: preview.rope.clone(),
                width: preview.width,
                palette: preview.palette,
                theme: preview.theme.clone(),
                registry: preview.registry,
            },
            document: preview.document.clone(),
            // A cancelled preparation may have updated shared style caches
            // since this display was published. Its display key cannot vouch
            // for those caches after the newer preparation was discarded.
            reset_style: true,
        }
    }
}
enum Outcome {
    Built {
        preparation: Option<Preparation>,
        preview: Option<CachedPreview>,
    },
    Retired,
}
struct Pending {
    key: Option<Key>,
    cancel: CancellationToken,
    result: mpsc::Receiver<Outcome>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
struct Slot {
    view: u64,
    visible: bool,
    wanted: Key,
    frame: usize,
    ready: Option<CachedPreview>,
    preparation: Option<Preparation>,
    pending: Option<Pending>,
    retired: Vec<CachedPreview>,
    failed: bool,
}
impl Slot {
    fn fresh(&self) -> bool {
        !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.key.is_some())
            && self
                .ready
                .as_ref()
                .is_some_and(|ready| self.wanted.matches_preview(ready))
            && self.preparation.as_ref().is_some_and(|prepared| {
                !prepared.reset_style
                    && self.wanted.rope.is_instance(&prepared.key.rope)
                    && self.wanted.same_style(&prepared.key)
            })
    }
    fn poll(&mut self) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                let relevant = pending
                    .key
                    .as_ref()
                    .is_none_or(|key| key.matches(&self.wanted));
                self.pending = None;
                self.failed = relevant;
                return true;
            }
        };
        self.pending = None;
        if let Outcome::Built {
            preparation,
            preview,
        } = result
        {
            self.preparation = preparation;
            if let Some(mut preview) = preview {
                if self.wanted.matches_preview(&preview) {
                    if let Some(previous) = &self.ready {
                        if previous.source_rows == preview.source_rows {
                            preview.source_rows = previous.source_rows.clone();
                        }
                        if previous.text_rows == preview.text_rows {
                            preview.text_rows = previous.text_rows.clone();
                        }
                    }
                    if let Some(previous) = self.ready.replace(preview) {
                        self.retired.push(previous);
                    }
                } else {
                    self.retired.push(preview);
                }
            }
            true
        } else {
            !self.fresh()
        }
    }
    fn cancel(&self) {
        if let Some(pending) = &self.pending {
            pending.cancel.cancel();
        }
    }
}

struct Permit {
    count: Arc<AtomicUsize>,
    wake: UiWake,
}
impl Permit {
    fn acquire(count: &Arc<AtomicUsize>, wake: UiWake) -> Option<Self> {
        count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_JOBS).then_some(count + 1)
            })
            .ok()?;
        Some(Self {
            count: count.clone(),
            wake,
        })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
        self.wake.notify();
    }
}

#[derive(Default)]
pub(crate) struct PreviewService {
    slots: Vec<Slot>,
    jobs: Arc<AtomicUsize>,
    frame: usize,
}
pub(super) struct Display<'a> {
    pub preview: Option<&'a mut CachedPreview>,
    pub loading: bool,
    pub failed: bool,
}
impl PreviewService {
    /// Called on an existing event-loop turn after the shared wake ticket was
    /// observed. No timer or idle polling is needed to publish worker results.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        for slot in &mut self.slots {
            changed |= slot.poll();
        }
        changed
            || (self.jobs.load(Ordering::Relaxed) < MAX_JOBS
                && self.slots.iter().any(|slot| {
                    slot.visible
                        && slot.frame == self.frame
                        && slot.pending.is_none()
                        && !slot.failed
                        && (!slot.fresh() || !slot.retired.is_empty())
                }))
    }

    pub(super) fn get(
        &mut self,
        view: u64,
        key: Key,
        frame: usize,
        scroll: &PreviewScroll,
        source: usize,
        height: usize,
        syntax: (Arc<SyntaxRegistry>, Arc<crate::textmate::ThemeSet>),
        background: &BackgroundWorkers,
    ) -> Display<'_> {
        self.frame = frame;
        let index = if let Some(index) = self.slots.iter().position(|slot| slot.view == view) {
            index
        } else {
            self.slots.push(Slot {
                view,
                visible: true,
                wanted: key.clone(),
                frame,
                ready: None,
                preparation: None,
                pending: None,
                retired: vec![],
                failed: false,
            });
            self.slots.len() - 1
        };
        let slot = &mut self.slots[index];
        if !slot.wanted.matches(&key) {
            slot.failed = false;
        }
        slot.wanted = key.clone();
        slot.frame = frame;
        slot.visible = true;
        // The event loop owns other panes' repaint signals. Consuming their
        // results here can leave an already drawn pane showing its old layout.
        slot.poll();
        self.trim(Some(view), frame, background);
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.view == view)
            .unwrap();
        if let Some(pending) = &slot.pending {
            if pending
                .key
                .as_ref()
                .is_some_and(|pending| !pending.matches(&key))
            {
                pending.cancel.cancel();
            }
        } else if !slot.failed && (!slot.fresh() || !slot.retired.is_empty()) {
            if let Some(permit) = Permit::acquire(&self.jobs, background.wake_handle()) {
                let (send, receive) = mpsc::sync_channel(1);
                let retired = std::mem::take(&mut slot.retired);
                let build = !slot.fresh();
                let preparation = if build {
                    slot.preparation
                        .take()
                        .or_else(|| slot.ready.as_ref().map(Preparation::from_preview))
                } else {
                    None
                };
                let hint = slot
                    .ready
                    .as_ref()
                    .filter(|ready| {
                        ready.rope.is_instance(&key.rope)
                            && ready.width == key.width
                            && scroll.source_scroll == source
                    })
                    .map(|_| scroll.row);
                let work_key = key.clone();
                let cancel = background.submit_interactive(move |cancel| {
                    let _permit = permit;
                    drop(retired);
                    let result = if build {
                        build_preview(work_key, preparation, source, hint, height, syntax, &cancel)
                    } else {
                        Outcome::Retired
                    };
                    let _ = send.send(result);
                });
                slot.pending = Some(Pending {
                    key: build.then_some(key),
                    cancel,
                    result: receive,
                });
            }
        }
        let loading = !slot.fresh() && !slot.failed;
        Display {
            preview: slot.ready.as_mut(),
            loading,
            failed: slot.failed,
        }
    }

    fn trim(&mut self, current: Option<u64>, frame: usize, background: &BackgroundWorkers) {
        loop {
            let bytes: usize = self
                .slots
                .iter()
                .flat_map(|slot| slot.ready.iter().chain(slot.retired.iter()))
                .map(|preview| preview.retained_bytes)
                .fold(0usize, usize::saturating_add);
            let inactive = self
                .slots
                .iter()
                .filter(|slot| {
                    Some(slot.view) != current
                        && (!slot.visible || slot.frame < frame.saturating_sub(1))
                })
                .count();
            if inactive <= RECENT_PREVIEWS && bytes <= RETAINED_BYTES {
                break;
            }
            let Some(index) = self
                .slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| {
                    Some(slot.view) != current
                        && (!slot.visible || slot.frame < frame.saturating_sub(1))
                })
                .min_by_key(|(_, slot)| slot.frame)
                .map(|(index, _)| index)
            else {
                break;
            };
            let slot = self.slots.remove(index);
            self.retire(slot, background);
        }
    }

    fn retire(&self, slot: Slot, background: &BackgroundWorkers) {
        slot.cancel();
        if let Some(permit) = Permit::acquire(&self.jobs, background.wake_handle()) {
            background.submit_interactive(move |_| {
                let _permit = permit;
                drop(slot);
            });
        }
        // At the admission bound, dropping a snapshot here is CPU-only; never
        // queue an unbounded chain of retirements behind occupied workers.
    }

    pub(crate) fn retain_open(&mut self, live: &[u64], background: &BackgroundWorkers) {
        let mut index = 0;
        while index < self.slots.len() {
            if live.contains(&self.slots[index].view) {
                index += 1;
            } else {
                let slot = self.slots.remove(index);
                self.retire(slot, background);
            }
        }
    }

    pub(crate) fn set_visible(&mut self, visible: &[u64], background: &BackgroundWorkers) {
        for slot in &mut self.slots {
            slot.visible = visible.contains(&slot.view);
            if !slot.visible
                && slot
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.key.is_some())
            {
                slot.cancel();
            }
        }
        self.trim(None, self.frame, background);
    }

    #[cfg(test)]
    pub(super) fn busy(&self) -> bool {
        self.jobs.load(Ordering::Relaxed) != 0
            || self.slots.iter().any(|slot| {
                slot.visible
                    && slot.frame == self.frame
                    && !slot.failed
                    && (!slot.fresh() || slot.pending.is_some() || !slot.retired.is_empty())
            })
    }
    #[cfg(test)]
    pub(super) fn ready(&self, view: u64) -> &CachedPreview {
        self.slots
            .iter()
            .find(|slot| slot.view == view)
            .unwrap()
            .ready
            .as_ref()
            .unwrap()
    }
}
impl Drop for PreviewService {
    fn drop(&mut self) {
        for slot in &self.slots {
            slot.cancel();
        }
    }
}

fn build_preview(
    key: Key,
    previous: Option<Preparation>,
    source: usize,
    hint: Option<usize>,
    height: usize,
    syntax: (Arc<SyntaxRegistry>, Arc<crate::textmate::ThemeSet>),
    cancel: &CancellationToken,
) -> Outcome {
    if cancel.is_cancelled() {
        return Outcome::Built {
            preparation: previous,
            preview: None,
        };
    }
    let mut preparation = match previous.filter(|previous| previous.key.rope.is_instance(&key.rope))
    {
        Some(previous) => previous,
        None => {
            let text = key.rope.to_string();
            let Some(document) = PreparedDocument::prepare(&text, Some(cancel)) else {
                return Outcome::Built {
                    preparation: None,
                    preview: None,
                };
            };
            Preparation {
                key: key.clone(),
                document: Arc::new(document),
                reset_style: false,
            }
        }
    };
    if preparation.reset_style || preparation.key.palette != key.palette {
        preparation.document.inline().clear();
    }
    if preparation.reset_style || !preparation.key.same_style(&key) {
        preparation.document.code().clear();
        preparation.document.highlighted().clear();
    }
    preparation.key = key.clone();
    preparation.reset_style = false;
    let rendered = (|| {
        let (rows, code_views) =
            preparation
                .document
                .layout_with_cancel(key.width, key.palette, None, Some(cancel))?;
        if cancel.is_cancelled() {
            return None;
        }
        let source_rows = source_row_map(&rows);
        let text_rows = preview_text_rows(&rows);
        if cancel.is_cancelled() {
            return None;
        }
        let mut preview = CachedPreview {
            rope: key.rope,
            width: key.width,
            palette: key.palette,
            theme: key.theme,
            registry: key.registry,
            links: rows.iter().map(|row| row.links.clone()).collect(),
            source_rows,
            text_rows,
            rows,
            code_views,
            document: preparation.document.clone(),
            retained_bytes: 0,
            #[cfg(test)]
            laid_out_on: std::thread::current().id(),
        };
        let row =
            hint.unwrap_or_else(|| mapped_viewport_start(&preview.source_rows, source, height));
        preview.highlight_visible(
            row..row.saturating_add(height),
            (&syntax.0, syntax.1.themes.get(&preview.theme)),
        );
        if cancel.is_cancelled() {
            return None;
        }
        preview.retained_bytes = estimate_bytes(&preview);
        Some(preview)
    })();
    Outcome::Built {
        preparation: Some(preparation),
        preview: rendered,
    }
}

fn estimate_bytes(preview: &CachedPreview) -> usize {
    // Estimated source/AST/prepared-word allowance plus actual rendered
    // buffers. The budget governs inactive retention; visible panes are pinned.
    preview
        .rope
        .len_bytes()
        .saturating_mul(16)
        .saturating_add(preview.rows.capacity() * std::mem::size_of::<Row>())
        .saturating_add(
            preview
                .rows
                .iter()
                .map(|row| {
                    row.line.spans.capacity() * std::mem::size_of::<Span<'static>>()
                        + row
                            .line
                            .spans
                            .iter()
                            .map(|span| span.content.len())
                            .sum::<usize>()
                        + row.links.capacity() * std::mem::size_of::<LinkSpan>()
                })
                .sum::<usize>(),
        )
        .saturating_add(
            preview
                .text_rows
                .iter()
                .map(|row| std::mem::size_of::<String>() + row.capacity())
                .sum::<usize>(),
        )
        .saturating_add(preview.source_rows.len() * std::mem::size_of::<usize>())
}

#[cfg(test)]
#[path = "ui_markdown_preview_tests.rs"]
mod tests;
