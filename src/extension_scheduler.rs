//! Extension work uses the same deadlines and wakeups as terminal input.
use std::time::{Duration, Instant};

use crate::app::App;

#[cfg(test)]
#[path = "extension_scheduler_tests.rs"]
mod tests;

fn handlers(app: &App) -> impl Iterator<Item = (&str, &crate::extensions::ExtensionTickHandler)> {
    let open = std::cell::OnceCell::<rustc_hash::FxHashSet<&str>>::new();
    app.extension_manager.tick_handlers(move |id| {
        open.get_or_init(|| {
            app.tabs.iter().chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
                .filter_map(|tab| tab.extension_view.as_ref().map(|view| view.extension_id.as_str()))
                .collect()
        }).contains(id)
    })
}

pub(crate) fn next_wait(app: &App) -> Option<Duration> {
    next_wait_at(app, Instant::now())
}

fn next_wait_at(app: &App, now: Instant) -> Option<Duration> {
    handlers(app)
        .filter(|(id, handler)| {
            (handler.background || (app.terminal_has_focus && !app.idle_quiescent))
                && !app.extension_manager.busy_extension_ids.contains(*id)
        })
        .map(|(id, handler)| {
            let interval = Duration::from_millis(u64::from(handler.interval_ms));
            app.extension_overlay_last_tick
                .get(id).and_then(|ticks| ticks.get(handler.export_name.as_str()))
                .map_or(Duration::ZERO, |last| {
                    interval.saturating_sub(now.saturating_duration_since(*last))
                })
        })
        .min()
}

pub(crate) fn dispatch(app: &mut App) {
    let mut due: rustc_hash::FxHashMap<String, Vec<String>> = rustc_hash::FxHashMap::default();
    let now = Instant::now();
    for (id, handler) in handlers(app) {
        if (!handler.background && (!app.terminal_has_focus || app.idle_quiescent))
            || app.extension_manager.busy_extension_ids.contains(id)
        {
            continue;
        }
        // A busy custom editor shares one Wasmtime store between input and
        // tasks. Alternate when both need it, so neither a stream of mouse
        // events nor a long-running loader can monopolize that store.
        if app.extension_manager.tick_yield_to_input.contains(id)
            && app
                .extension_manager
                .pending_view_events
                .iter()
                .any(|event| event.extension_id == id)
        {
            continue;
        }
        let interval = Duration::from_millis(u64::from(handler.interval_ms));
        if app
            .extension_overlay_last_tick
            .get(id).and_then(|ticks| ticks.get(handler.export_name.as_str()))
            .is_none_or(|last| now.duration_since(*last) >= interval)
        {
            due.entry(id.to_owned())
                .or_default()
                .push(handler.export_name.clone());
        }
    }
    let context = (!due.is_empty()).then(|| app.extension_editor_context_snapshot());
    for (id, exports) in due {
        let keys = exports.clone();
        if app
            .extension_manager
            .invoke_tick_handlers_async(&id, exports, &mut app.background, context.clone())
            .is_ok()
        {
            // A callback skipped while its store is busy is still due. Only
            // advance its deadline when it actually starts on a worker.
            let ticks = app.extension_overlay_last_tick.entry(id).or_default();
            for key in keys {
                ticks.insert(key, now);
            }
        }
    }
    app.extension_manager
        .dispatch_view_events(&mut app.background);
}
