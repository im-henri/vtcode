//! Diagnostics for finding a bottleneck, rather than just observing CPU usage.
use super::*;
use crate::performance::diagnostics::UiPhase;

fn ms(value: f64) -> String {
    format!("{value:.1}ms")
}

fn timing_row(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    timing: &RenderTiming,
    width: usize,
    p: Palette,
) {
    let (p95, _, _) = timing.recent_summary();
    let last = if timing.has_samples() {
        ms(timing.last_ms)
    } else {
        "—".into()
    };
    let p95 = if timing.has_samples() {
        ms(p95.into())
    } else {
        "—".into()
    };
    let peak = if timing.has_samples() {
        ms(timing.peak_ms)
    } else {
        "—".into()
    };
    if width >= 70 {
        lines.push(Line::styled(
            format!(
                "{label:<20} {last:>10} {p95:>10} {peak:>10} {:>9}",
                timing.sample_count()
            ),
            p.normal,
        ));
    } else {
        lines.push(Line::styled(label.to_owned(), p.heading));
        lines.push(Line::styled(
            clip_middle(&format!("{last} last · {p95} P95 · {peak} peak"), width),
            p.dim,
        ));
    }
}

fn lines(app: &App, width: usize, p: Palette, now: Instant) -> Vec<Line<'static>> {
    let monitor = &app.background.activity;
    let ui = &monitor.ui;
    let (queued, running, slots, results) = app.background.queue_snapshot();
    let mut rows = vec![
        Line::styled(
            format!(
                "Since reset: {}",
                format_elapsed(now.saturating_duration_since(ui.since))
            ),
            p.dim,
        ),
        Line::styled(
            clip_middle(
                &format!(
                    "Pool {running}/{slots} running · {queued} queued · {results} results waiting"
                ),
                width,
            ),
            p.normal,
        ),
        Line::raw(""),
        Line::styled("UI RESPONSIVENESS", p.heading),
        Line::styled("Wall time includes OS scheduling delays.", p.dim),
        Line::styled("Input to draw starts at dispatch, not keypress.", p.dim),
        Line::styled("Terminal diff/output includes console waits, not terminal paint.", p.dim),
        Line::styled("P95: last 120 calls; peak/count: since reset.", p.dim),
    ];
    if width >= 70 {
        rows.push(Line::styled(
            format!(
                "{:<20} {:>10} {:>10} {:>10} {:>9}",
                "Phase", "Last", "P95", "Peak", "Calls"
            ),
            p.dim,
        ));
    }
    timing_row(
        &mut rows,
        "Input to draw",
        ui.timing(UiPhase::InputToFrame),
        width,
        p,
    );
    for phase in UiPhase::ALL
        .into_iter()
        .filter(|phase| *phase != UiPhase::InputToFrame)
    {
        timing_row(&mut rows, phase.label(), ui.timing(phase), width, p);
    }
    timing_row(&mut rows, "Draw + output", &monitor.render, width, p);
    #[cfg(windows)]
    crate::terminal_input_batch::with_delivery_timings(|delivery| {
        rows.push(Line::raw(""));
        rows.push(Line::styled("WINDOWS INPUT DELIVERY", p.heading));
        rows.push(Line::styled("Queue age: decoding through first UI dequeue, including batching.", p.dim));
        rows.push(Line::styled("Drag queue includes samples coalesced by the UI.", p.dim));
        rows.push(Line::styled("Press delivery: matched physical hook to terminal decoding.", p.dim));
        rows.push(Line::styled("Press samples do not measure physical drag delivery or screen paint.", p.dim));
        for (label, timing) in delivery_rows(delivery) {
            timing_row(&mut rows, label, timing, width, p);
        }
    });
    rows.push(Line::raw(""));
    rows.push(Line::styled(
        format!("SLOW UI WORK · ≥50ms · {} recorded", ui.slow_count),
        p.heading,
    ));
    rows.push(Line::styled(
        "Last 16 calls; input-to-draw overlaps phases.",
        p.dim,
    ));
    if ui.slow_count == 0 {
        rows.push(Line::styled("No slow calls recorded.", p.normal));
    }
    for event in ui.slow_events() {
        rows.push(Line::styled(
            clip_middle(
                &format!(
                    "+{}  {}  {}",
                    format_elapsed(event.at.saturating_duration_since(ui.since)),
                    event.label,
                    ms(event.elapsed.as_secs_f64() * 1000.0)
                ),
                width,
            ),
            p.normal,
        ));
    }
    rows.push(Line::raw(""));
    rows.push(Line::styled(
        format!("TRACKED TASKS · {} completed", monitor.totals.completed),
        p.heading,
    ));
    rows.push(Line::styled(
        "Includes fast tasks; sorted by total elapsed.",
        p.dim,
    ));
    rows.push(Line::styled(
        "Concurrent durations overlap; these are not CPU times.",
        p.dim,
    ));
    let mut totals: Vec<_> = monitor.totals.rows.iter().collect();
    totals.sort_unstable_by_key(|row| (Reverse(row.elapsed), row.label));
    if totals.is_empty() {
        rows.push(Line::styled(
            "No tracked tasks completed since reset.",
            p.normal,
        ));
    }
    for row in totals {
        rows.push(Line::styled(
            clip_middle(
                &format!(
                    "{} · {} calls · {} total",
                    row.label,
                    row.count,
                    format_elapsed(row.elapsed)
                ),
                width,
            ),
            p.heading,
        ));
        rows.push(Line::styled(
            clip_middle(
                &format!(
                    "Mean {} · peak {} · {} stopped",
                    ms(row.elapsed.as_secs_f64() * 1000.0 / row.count as f64),
                    ms(row.peak.as_secs_f64() * 1000.0),
                    row.stopped
                ),
                width,
            ),
            p.normal,
        ));
        if row.queue_samples > 0 {
            rows.push(Line::styled(
                clip_middle(
                    &format!(
                        "Queue wait {} total / {} measured tasks",
                        format_elapsed(row.queued),
                        row.queue_samples
                    ),
                    width,
                ),
                p.dim,
            ));
        }
    }
    rows
}

pub(super) fn draw(f: &mut Frame, app: &mut App, body: Rect, p: Palette, now: Instant) {
    app.performance_view.displayed_totals = app.background.activity.totals.completed;
    if body.is_empty() {
        return;
    }
    f.render_widget(
        Paragraph::new("Timings & queues").style(p.heading),
        Rect::new(body.x, body.y, body.width.saturating_sub(9), 1),
    );
    if body.width >= 20 {
        button(
            f,
            app,
            Rect::new(body.right() - 8, body.y, 7, 1),
            "Reset",
            Hit::ResetTimings,
            true,
            p,
        );
    }
    let content = Rect::new(
        body.x,
        body.y + 1,
        body.width,
        body.height.saturating_sub(1),
    );
    let rows = lines(app, content.width.saturating_sub(2) as usize, p, now);
    let view = &mut app.performance_view;
    view.layout.list = content;
    view.layout.content_rows = rows.len();
    view.scroll = view
        .scroll
        .min(rows.len().saturating_sub(content.height as usize));
    f.render_widget(
        Paragraph::new(rows).scroll((view.scroll as u16, 0)),
        content,
    );
    if view.layout.content_rows > content.height as usize && !content.is_empty() {
        use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
        let mut state = ScrollbarState::new(view.layout.content_rows - content.height as usize + 1)
            .position(view.scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(p.dim),
            content,
            &mut state,
        );
    }
}

pub(super) fn append_report(text: &mut String, app: &App, now: Instant) {
    let monitor = &app.background.activity;
    let ui = &monitor.ui;
    let _ = writeln!(
        text,
        "\nUI responsiveness ({} since reset; elapsed wall time, not CPU time)",
        format_elapsed(now.saturating_duration_since(ui.since))
    );
    text.push_str("Input-to-draw: dispatch through completed terminal write; excludes input queue and terminal paint. P95 uses the last 120 calls per phase. Phases overlap input-to-draw.\n");
    for phase in UiPhase::ALL {
        let series = ui.timing(phase);
        let (p95, _, count) = series.recent_summary();
        if count == 0 {
            let _ = writeln!(text, "{}: not measured", phase.label());
        } else {
            let _ = writeln!(
                text,
                "{}: {:.3}ms last; {p95:.3}ms P95 ({count} recent); {:.3}ms peak; {} calls",
                phase.label(),
                series.last_ms,
                series.peak_ms,
                series.sample_count()
            );
        }
    }
    #[cfg(windows)]
    crate::terminal_input_batch::with_delivery_timings(|delivery| {
        text.push_str("\nWindows input delivery: queue age is decode through first UI dequeue (includes batching and coalesced samples). Press delivery is matched native mouse hook through terminal decode; it does not measure physical drag delivery. Neither includes screen presentation.\n");
        for (label, series) in delivery_rows(delivery) {
            let (p95, _, count) = series.recent_summary();
            if count == 0 {
                let _ = writeln!(text, "{label}: not measured");
            } else {
                let _ = writeln!(text,
                    "{label}: {:.3}ms last; {p95:.3}ms P95 ({count} recent); {:.3}ms peak; {} calls",
                    series.last_ms, series.peak_ms, series.sample_count());
            }
        }
    });
    let _ = writeln!(
        text,
        "Slow UI work (>=50ms): {} calls; retaining last 16",
        ui.slow_count
    );
    for event in ui.slow_events() {
        let _ = writeln!(
            text,
            "+{:.3}s {}: {:.3}ms",
            event.at.saturating_duration_since(ui.since).as_secs_f64(),
            event.label,
            event.elapsed.as_secs_f64() * 1000.0
        );
    }
    let (queued, running, slots, results) = app.background.queue_snapshot();
    let _ = writeln!(
        text,
        "\nPool snapshot: {running}/{slots} executing, {queued} queued; {results} results awaiting UI (independent atomic counters)"
    );
    let _ = writeln!(
        text,
        "Tracked task totals: {} completions since reset, including short tasks. Elapsed includes queue wait; concurrent tasks overlap; these are not CPU times.",
        monitor.totals.completed
    );
    let mut totals: Vec<_> = monitor.totals.rows.iter().collect();
    totals.sort_unstable_by_key(|row| (Reverse(row.elapsed), row.label));
    for row in totals {
        let _ = writeln!(
            text,
            "{}: {} calls, {} stopped; {:.3}ms total; {:.3}ms mean; {:.3}ms peak; {:.3}ms queue wait across {} measured tasks",
            row.label,
            row.count,
            row.stopped,
            row.elapsed.as_secs_f64() * 1000.0,
            row.elapsed.as_secs_f64() * 1000.0 / row.count as f64,
            row.peak.as_secs_f64() * 1000.0,
            row.queued.as_secs_f64() * 1000.0,
            row.queue_samples
        );
    }
}

#[cfg(windows)]
fn delivery_rows(delivery: &crate::terminal_input_batch::DeliveryTimings) -> [(&'static str, &RenderTiming); 3] {
    [
        ("Decoded input queue", &delivery.queue),
        ("Decoded drag queue", &delivery.drag_queue),
        ("Terminal press delivery", &delivery.press_ingress),
    ]
}
