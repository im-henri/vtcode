//! One animation clock for every visible agent/conversation, independent of selection.
use super::*;
use std::cell::Cell;

#[derive(Default)]
pub(crate) struct ActivityAnimation {
    frame: usize,
    pub(super) tick: Cell<Option<Instant>>,
    visible: Cell<bool>,
}

impl ActivityAnimation {
    pub(crate) fn begin_frame(&self) {
        self.visible.set(false);
    }

    pub(crate) fn symbol(&self) -> &'static str {
        ACTIVITY_FRAMES[self.frame].trim_end()
    }

    pub(crate) fn tab_symbol(&self) -> &'static str {
        ACTIVITY_FRAMES[self.frame]
    }

    /// Register only spinners that actually reach a rendered viewport. Tab
    /// overflow, hidden panes, and transcript scrollback need no cosmetic timer.
    pub(crate) fn show(&self) {
        self.visible.set(true);
        if self.tick.get().is_none() {
            self.tick.set(Some(Instant::now()));
        }
    }
}

pub(crate) fn next_activity_frame(app: &App) -> Option<Duration> {
    if !app.chat.activity.visible.get() || (!app.chat.visible && app.chat.layout.editors.is_empty())
    {
        return None;
    }
    // A completion/permission event can arrive between painting and polling
    // this deadline. Do not keep waking on a stale rendered spinner.
    let running = app.chat.agents().any(|(_, state)| {
        state.closing
            || (state.control_busy && state.permissions.is_empty())
            || state.sessions.iter().any(|(id, session)| {
                session.busy
                    && (session.cancelling
                        || !state
                            .permissions
                            .iter()
                            .any(|p| &p.request.session_id == id))
            })
    });
    if !running {
        return None;
    }
    app.chat
        .activity
        .tick
        .get()
        .map(|last| ACTIVITY_INTERVAL.saturating_sub(last.elapsed()))
}

pub(crate) fn advance_activity_frame(app: &mut App) -> bool {
    if next_activity_frame(app) != Some(Duration::ZERO) {
        return false;
    }
    app.chat.activity.frame = (app.chat.activity.frame + 1) % ACTIVITY_FRAMES.len();
    app.chat.activity.tick.set(Some(Instant::now()));
    true
}
