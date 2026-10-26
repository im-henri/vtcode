//! Readable, bounded dashboards over counters the workbench already owns.
//! This module never reads documents, enumerates processes or queries disks.
use super::*;
use crate::process_usage::{ProcessUsage, SAMPLE_INTERVAL, format_bytes};

fn text(f: &mut Frame, area: Rect, row: u16, value: &str, style: Style) {
    if row < area.height {
        f.render_widget(
            Paragraph::new(clip_middle(value, area.width as usize)).style(style),
            Rect::new(area.x, area.y + row, area.width, 1),
        );
    }
}

fn card(f: &mut Frame, app: &App, area: Rect, title: &str, p: Palette) -> Rect {
    let border = app
        .colors
        .ui
        .widget_border
        .border(p.bg)
        .unwrap_or(p.dim.fg.unwrap());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Line::styled(format!(" {title} "), p.dim))
        .style(p.normal);
    let inner = block.inner(area);
    f.render_widget(block, area);
    Rect::new(
        inner.x + u16::from(inner.width > 2),
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    )
}

fn glyph(value: f64, ceiling: f64) -> char {
    ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'][(value / ceiling.max(f64::EPSILON) * 7.0)
        .round()
        .clamp(0.0, 7.0) as usize]
}

/// Each column is a real ten-second sampling slot. Gaps stay blank when the
/// monitor was hidden; a missing reading is never presented as zero usage.
fn resource_trend(usage: &ProcessUsage, cpu: bool, now: Instant, width: usize) -> (String, f64) {
    let width = width.min(30);
    let mut values = [None; 30];
    let end = usage.sampled_at().map_or(now, |sampled| sampled.max(now));
    for sample in usage.history() {
        let slot = end.saturating_duration_since(sample.at).as_secs() / SAMPLE_INTERVAL.as_secs();
        if slot < width as u64 {
            values[width - 1 - slot as usize] = if cpu {
                sample.cpu_percent
            } else {
                sample.resident_bytes.map(|bytes| bytes as f64)
            };
        }
    }
    let max = values[..width]
        .iter()
        .flatten()
        .copied()
        .fold(0.0, f64::max);
    let ceiling = if cpu {
        (max.max(100.0) / 100.0).ceil() * 100.0
    } else {
        max.max(1024.0 * 1024.0)
    };
    (
        values[..width]
            .iter()
            .map(|value| value.map_or(' ', |value| glyph(value, ceiling)))
            .collect(),
        ceiling,
    )
}

fn resource_card(f: &mut Frame, app: &App, area: Rect, cpu: bool, p: Palette, now: Instant) {
    let usage = &app.performance_view.resources;
    let inner = card(f, app, area, if cpu { "CPU" } else { "Memory" }, p);
    text(
        f,
        inner,
        0,
        if cpu {
            usage.cpu_label()
        } else {
            usage.memory_label()
        },
        p.heading.fg(p.accent),
    );
    let subtitle = if cpu {
        if usage.cpu_percent.is_none() && usage.cpu_label() == "Measuring…" {
            "First CPU reading after 10s".into()
        } else {
            "100% = 1 core".into()
        }
    } else {
        usage
            .peak_resident_bytes
            .map_or("Resident memory".into(), |bytes| {
                format!("Peak sampled  {}", format_bytes(bytes))
            })
    };
    text(f, inner, 1, &subtitle, p.dim);
    if inner.height >= 4 {
        let (trend, ceiling) = resource_trend(usage, cpu, now, inner.width as usize);
        text(f, inner, 2, &trend, p.normal.fg(p.accent));
        let scale = if cpu {
            format!("{ceiling:.0}%")
        } else {
            format_bytes(ceiling as u64)
        };
        text(f, inner, 3, &format!("10s / bar · 0–{scale}"), p.dim);
    }
}

fn frame_chart(timing: &RenderTiming, width: usize) -> (String, f32) {
    let mut values = [0.0f32; crate::performance::FRAME_HISTORY];
    let mut count = 0;
    let mut ceiling = 16.7f32;
    for ms in timing.recent_ms() {
        values[count] = ms;
        ceiling = ceiling.max(ms);
        count += 1;
    }
    let columns = width.min(count);
    let chart = (0..columns)
        .map(|index| {
            // Preserve spikes when several frames share a terminal column.
            let start = index * count / columns;
            let end = (index + 1) * count / columns;
            glyph(
                values[start..end].iter().copied().fold(0.0f32, f32::max) as f64,
                ceiling as f64,
            )
        })
        .collect();
    (chart, ceiling)
}

fn frame_card(f: &mut Frame, app: &mut App, area: Rect, p: Palette) {
    let inner = card(f, app, area, "UI frame time", p);
    let timing = &app.background.activity.render;
    if timing.has_samples() {
        let values = [
            format!("{:.1}ms", timing.last_ms),
            format!("{:.1}ms", timing.average_ms),
            format!("{:.1}ms", timing.peak_ms),
        ];
        if inner.height > 0 {
            f.render_widget(
                Paragraph::new(metric_line(&values, inner.width as usize, p.heading)),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
        }
        if inner.height > 1 {
            f.render_widget(
                Paragraph::new(metric_line(
                    &["Last".into(), "Average".into(), "Peak".into()],
                    inner.width as usize,
                    p.dim,
                )),
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
        }
        let (p95, slow, count) = timing.recent_summary();
        text(
            f,
            inner,
            2,
            &format!("P95 {p95:.1}ms · {slow}/{count} over 16.7ms"),
            p.normal,
        );
        if inner.height >= 5 {
            let (trend, ceiling) = frame_chart(timing, inner.width as usize);
            text(f, inner, 3, &trend, p.normal.fg(p.accent));
            text(
                f,
                inner,
                4,
                &format!("Last {count} frames · 0–{ceiling:.1}ms"),
                p.dim,
            );
        }
    } else {
        text(f, inner, 0, "Waiting for the first frame", p.dim);
    }
    app.performance_view
        .layout
        .controls
        .push((inner, Hit::RenderInfo));
    if inner.height > 2 {
        app.performance_view.layout.controls.insert(
            0,
            (
                Rect::new(inner.x, inner.y + 2, inner.width, 1),
                Hit::TailInfo,
            ),
        );
    }
    if area.width >= 28 {
        button(
            f,
            app,
            Rect::new(area.right() - 9, area.y, 7, 1),
            "Reset",
            Hit::Reset,
            true,
            p,
        );
    }
    if area.width >= 48 {
        button(
            f,
            app,
            Rect::new(area.right() - 21, area.y, 9, 1),
            "Details",
            Hit::Tab(Section::Timings),
            true,
            p,
        );
    }
}

pub(super) fn draw_overview(f: &mut Frame, app: &mut App, body: Rect, p: Palette, now: Instant) {
    if body.is_empty() {
        return;
    }
    poll_resources(app, now);
    // Compact terminals retain readable values and actions; charts use extra
    // room instead of making the same dense layout overflow at every size.
    if body.height < 9 || body.width < 36 {
        let usage = &app.performance_view.resources;
        text(f, body, 0, &format!("CPU {}", usage.cpu_label()), p.heading);
        text(
            f,
            body,
            1,
            &format!("Memory {}", usage.memory_label()),
            p.heading,
        );
        let timing = &app.background.activity.render;
        text(
            f,
            body,
            2,
            &format!("UI frame  {:.1}ms", timing.last_ms),
            p.normal,
        );
        if body.height > 4 {
            text(
                f,
                body,
                3,
                &summary(&app.background.activity, now, body.width as usize),
                p.dim,
            );
            text(f, body, 4, &sampling_note(app, now), p.dim);
        } else if body.height <= 2 {
            text(
                f,
                body,
                body.height - 1,
                &format!("UI {:.1}ms", timing.last_ms),
                p.normal,
            );
        }
        app.performance_view.layout.controls.push((
            Rect::new(body.x, body.y, body.width, body.height.min(2)),
            Hit::ResourceInfo,
        ));
        return;
    }
    let stacked = body.width < 60 && body.height >= 26;
    let card_height = if body.height >= 18 { 6 } else { 4 };
    let resource_height = if stacked { 12 } else { card_height };
    let (cpu, memory) = if stacked {
        (
            Rect::new(body.x, body.y, body.width, 6),
            Rect::new(body.x, body.y + 6, body.width, 6),
        )
    } else {
        let width = (body.width - 1) / 2;
        (
            Rect::new(body.x, body.y, width, card_height),
            Rect::new(
                body.x + width + 1,
                body.y,
                body.width - width - 1,
                card_height,
            ),
        )
    };
    resource_card(f, app, cpu, true, p, now);
    resource_card(f, app, memory, false, p, now);
    app.performance_view.layout.controls.push((
        Rect::new(body.x, body.y, body.width, resource_height),
        Hit::ResourceInfo,
    ));
    text(f, body, resource_height, &sampling_note(app, now), p.dim);
    let frame_y = body.y + resource_height + 2;
    let remaining = body.bottom().saturating_sub(frame_y);
    let frame_height = if remaining >= 10 { 7 } else { remaining.min(5) };
    let frame_area = Rect::new(body.x, frame_y, body.width, frame_height);
    frame_card(f, app, frame_area, p);
    let y = frame_area.bottom() + 1;
    if y >= body.bottom() {
        return;
    }
    let busy = app.background.activity.job_count() > 0;
    let section = if busy {
        Section::Running
    } else {
        Section::Recent
    };
    let header = Rect::new(body.x, y, body.width, 1);
    text(
        f,
        header,
        0,
        if busy {
            "Background work"
        } else {
            "Recent work"
        },
        p.heading,
    );
    if body.width >= 32 {
        let label = if busy {
            format!("{} active →", app.background.activity.job_count())
        } else {
            "View history →".into()
        };
        let width = (label.len() as u16 + 2).min(body.width / 2);
        button(
            f,
            app,
            Rect::new(body.right() - width, y, width, 1),
            &label,
            Hit::Tab(section),
            true,
            p,
        );
    }
    if y + 1 >= body.bottom() {
        return;
    }
    text(
        f,
        body,
        y + 1 - body.y,
        &summary(&app.background.activity, now, body.width as usize),
        p.dim,
    );
    let first_row = y + 2;
    let rows = entries(&app.background.activity, section, now);
    if rows.is_empty() {
        text(
            f,
            body,
            first_row - body.y,
            "Tasks taking ≥400ms appear here",
            p.dim,
        );
    } else {
        let mut controls = Vec::new();
        for (index, entry) in rows
            .iter()
            .take(usize::from(body.bottom().saturating_sub(first_row)))
            .enumerate()
        {
            let row = Rect::new(body.x, first_row + index as u16, body.width, 1);
            let hovered = row.contains((app.mouse_col, app.mouse_row).into());
            let style = app.colors.ui.list_row_style(
                ColorLayer::opaque(p.normal.fg.unwrap()),
                p.bg,
                false,
                false,
                hovered,
            );
            f.render_widget(
                Paragraph::new(task_line(
                    entry,
                    &app.explorer.root,
                    row.width as usize,
                    style,
                    if hovered { style } else { p.dim },
                ))
                .style(style),
                row,
            );
            controls.push((row, Hit::Inspect(section, entry.id)));
        }
        app.performance_view.layout.controls.extend(controls);
    }
}

fn sampling_note(app: &App, now: Instant) -> String {
    let usage = &app.performance_view.resources;
    let age = usage.sampled_at().map_or_else(
        || "not sampled yet".to_owned(),
        |at| {
            format!(
                "sample {}s old",
                now.saturating_duration_since(at).as_secs()
            )
        },
    );
    if !app.terminal_has_focus {
        format!("Paused (inactive window) · {age}")
    } else if obscured(app) {
        format!("Paused (overlay open) · {age}")
    } else if usage.cpu_is_cached() {
        format!("Cached · {age} · CPU resumes next interval")
    } else {
        format!("VTCode only · updates every 10s · {age}")
    }
}

/// This is metadata, not a sum of allocated memory: ropes share storage across
/// splits and undo snapshots. Never label text size as process memory usage.
pub(super) fn workspace_lines(app: &App) -> Vec<(String, String)> {
    let mut tabs = 0usize;
    let mut files = 0usize;
    let mut modified = 0usize;
    let mut loading = 0usize;
    let mut large = 0usize;
    for tab in app.tabs.iter().chain(
        app.groups
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != app.active_group)
            .flat_map(|(_, group)| &group.tabs),
    ) {
        tabs += 1;
        if tab.kind == crate::app::TabKind::File {
            files += 1;
            modified += usize::from(tab.editor.modified);
            loading += usize::from(tab.editor.bg_loading);
            large += usize::from(tab.editor.large_file_mode);
        }
    }
    let (queued, running, slots, results) = app.background.queue_snapshot();
    let mut lines = vec![
        ("WORKSPACE".into(), String::new()),
        ("Open tabs".into(), tabs.to_string()),
        ("Editor panes".into(), app.groups.len().max(1).to_string()),
        (
            "File tabs / modified".into(),
            format!("{files} / {modified}"),
        ),
        (
            "Loading / large-file mode".into(),
            format!("{loading} / {large}"),
        ),
        ("Terminal tabs".into(), app.terminals.len().to_string()),
        (
            "Pool running / workers".into(),
            format!("{running} / {slots}"),
        ),
        ("Pool queued".into(), queued.to_string()),
        ("Results awaiting UI".into(), results.to_string()),
        (String::new(), String::new()),
        ("ACTIVE DOCUMENT".into(), String::new()),
    ];
    if let Some(tab) = app
        .tabs
        .get(app.active_tab)
        .filter(|tab| tab.kind == crate::app::TabKind::File)
    {
        let editor = &tab.editor;
        lines.extend([
            ("Name".into(), editor.file_name().to_string()),
            ("Lines".into(), editor.line_count().to_string()),
            (
                "Text size (UTF-8)".into(),
                format_bytes(editor.rope.len_bytes() as u64),
            ),
            (
                "Word wrap".into(),
                if editor.word_wrap { "On" } else { "Off" }.into(),
            ),
            (
                "Editing".into(),
                if editor.read_only {
                    "Read-only"
                } else if editor.modified {
                    "Unsaved changes"
                } else if editor.file_path.is_none() {
                    "Untitled"
                } else {
                    "Saved"
                }
                .into(),
            ),
            (
                "Syntax".into(),
                if editor.bg_loading {
                    "Loading"
                } else if editor.large_file_mode {
                    "Limited (large file)"
                } else if editor.syntax_dirty_from.is_some() {
                    "Updating"
                } else {
                    "Up to date"
                }
                .into(),
            ),
        ]);
    } else {
        lines.push(("No text document selected".into(), String::new()));
    }
    lines.extend([
        (String::new(), String::new()),
        ("LANGUAGE SERVERS".into(), String::new()),
    ]);
    if app.lsp_manager.clients.is_empty() {
        lines.push((
            if app.lsp_enabled {
                "No servers started"
            } else {
                "LSP is disabled"
            }
            .into(),
            String::new(),
        ));
    } else {
        for client in &app.lsp_manager.clients {
            lines.push((
                client.language.clone(),
                if client.server_dead {
                    "Stopped"
                } else if client.initialized {
                    "Ready"
                } else {
                    "Starting"
                }
                .into(),
            ));
        }
    }
    lines
}

pub(super) fn draw_workspace(f: &mut Frame, app: &mut App, body: Rect, p: Palette) {
    let rows = workspace_lines(app);
    let view = &mut app.performance_view;
    view.layout.list = body;
    view.layout.content_rows = rows.len();
    view.scroll = view
        .scroll
        .min(rows.len().saturating_sub(body.height as usize));
    let width = body.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = rows
        .iter()
        .skip(view.scroll)
        .take(body.height as usize)
        .map(|(label, value)| {
            if value.is_empty() {
                return Line::styled(clip_middle(label, width), p.heading);
            }
            let label_width = (width / 2).min(27);
            let label = clip_middle(label, label_width);
            let value = clip_middle(value, width.saturating_sub(label_width + 2));
            Line::from(vec![
                Span::styled(
                    format!(
                        "{label}{}",
                        " ".repeat(label_width + 2 - UnicodeWidthStr::width(label.as_str()))
                    ),
                    p.dim,
                ),
                Span::styled(value, p.normal),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), body);
    if rows.len() > body.height as usize && body.width > 0 && body.height > 0 {
        use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
        let mut state = ScrollbarState::new(rows.len().saturating_sub(body.height as usize) + 1)
            .position(view.scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .style(p.dim),
            body,
            &mut state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_usage::Counters;

    #[test]
    fn resource_charts_leave_real_gaps_and_scale_multicore_cpu() {
        let now = Instant::now();
        let mut usage = ProcessUsage::default();
        usage.refresh_with(true, now, || Counters {
            cpu_time: Some(Duration::ZERO),
            resident_bytes: Some(32 * 1024 * 1024),
        });
        assert!(resource_trend(&usage, true, now, 30).0.trim().is_empty());
        usage.refresh_with(true, now + SAMPLE_INTERVAL, || Counters {
            cpu_time: Some(Duration::from_secs(20)),
            resident_bytes: Some(64 * 1024 * 1024),
        });
        let (cpu, max) = resource_trend(&usage, true, now + SAMPLE_INTERVAL, 30);
        assert_eq!(max, 200.0);
        assert!(cpu.ends_with('█'));
        usage.suspend();
        usage.refresh_with(true, now + SAMPLE_INTERVAL * 4, || Counters {
            cpu_time: Some(Duration::from_secs(21)),
            resident_bytes: Some(48 * 1024 * 1024),
        });
        let (cpu, _) = resource_trend(&usage, true, now + SAMPLE_INTERVAL * 4, 5);
        assert!(
            cpu.ends_with("   "),
            "hidden intervals and new baseline are gaps: {cpu:?}"
        );
        let (memory, _) = resource_trend(&usage, false, now + SAMPLE_INTERVAL * 4, 5);
        assert_eq!(memory.chars().nth(2), Some(' '));
        assert_eq!(memory.chars().nth(3), Some(' '));
        assert_ne!(memory.chars().nth(4), Some(' '));
        let (aged, _) = resource_trend(&usage, false, now + SAMPLE_INTERVAL * 7, 5);
        assert!(
            aged.ends_with("   "),
            "old values must move away from the live edge"
        );
        assert!(
            resource_trend(&usage, false, now + SAMPLE_INTERVAL * 40, 30)
                .0
                .trim()
                .is_empty()
        );
        assert_eq!(resource_trend(&usage, false, now, 0).0, "");
    }

    #[test]
    fn frame_charts_preserve_spikes_in_short_viewports() {
        let mut timing = RenderTiming::default();
        for index in 0..120 {
            timing.record(Duration::from_millis(if index == 44 { 200 } else { 1 }));
        }
        for width in [1, 4, 20, 80, 160] {
            let (chart, ceiling) = frame_chart(&timing, width);
            assert_eq!(chart.chars().count(), width.min(120));
            assert!(chart.contains('█'));
            assert_eq!(ceiling, 200.0);
        }
        assert_eq!(frame_chart(&timing, 0).0, "");
        let (p95, slow, count) = timing.recent_summary();
        assert_eq!((p95, slow, count), (1.0, 1, 120));
    }
}
