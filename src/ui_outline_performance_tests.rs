use super::*;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn reference_outline(f: &mut Frame, app: &App, area: Rect) {
    let sidebar_bg = app.colors.bg_sidebar;
    let focused = app.active_pane == crate::app::ActivePane::Explorer
        && app.sidebar_view == crate::app::SidebarView::Outline;
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(sidebar_bg)),
        area,
    );

    let header_area = Rect::new(area.x, area.y, area.width, 1);
    let outline_header_hovered = app.mouse_row == header_area.y
        && app.mouse_col >= header_area.x
        && app.mouse_col < header_area.x + header_area.width;
    let header_style = app
        .colors
        .ui
        .toolbar_layer_style(
            app.colors.ui.section_foreground,
            app.colors.ui.bg_section,
            outline_header_hovered,
            false,
            false,
        )
        .add_modifier(Modifier::BOLD);
    f.render_widget(
        Paragraph::new(Span::styled(" Outline", header_style)).style(header_style),
        header_area,
    );

    if area.height < 2 {
        return;
    }
    let list_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let items = &app.outline_items;

    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                " No symbols found",
                Style::default()
                    .fg(app.colors.ui.description_color(sidebar_bg))
                    .bg(sidebar_bg),
            )),
            Rect::new(list_area.x, list_area.y, list_area.width, 1),
        );
        return;
    }

    // Reserve 1 column for scrollbar if content overflows
    let total_items = items.len();
    let needs_scrollbar = total_items > list_area.height as usize && list_area.width > 5;
    let (entry_area, sb_area) = if needs_scrollbar {
        (
            Rect::new(
                list_area.x,
                list_area.y,
                list_area.width.saturating_sub(1),
                list_area.height,
            ),
            Some(Rect::new(
                list_area.x + list_area.width.saturating_sub(1),
                list_area.y,
                1,
                list_area.height,
            )),
        )
    } else {
        (list_area, None)
    };
    let visible = entry_area.height as usize;
    let render_width = entry_area.width;

    let scroll = app.outline_scroll;
    let mut kind_buf = String::with_capacity(8);
    let mut line_info_buf = String::with_capacity(8);
    for (i, item) in items.iter().enumerate().skip(scroll).take(visible) {
        let y = entry_area.y.saturating_add((i - scroll) as u16);
        let is_selected = i == app.outline_selected;

        let kind_icon = match item.kind {
            "function" => "ƒ",
            "struct" | "class" => "S",
            "enum" => "E",
            "trait" | "interface" => "I",
            "impl" => "⊕",
            "module" => "M",
            "constant" | "static" => "C",
            "type" => "T",
            _ => "·",
        };
        let row_style = app.colors.ui.list_row_style(
            app.colors.ui.sidebar_foreground,
            sidebar_bg,
            is_selected,
            focused,
            app.outline_hovered == Some(i),
        );
        let bg = row_style.bg.unwrap_or(sidebar_bg);
        let kind_color = app.colors.ui.list_icon_color(
            app.colors.ui.symbol_color(item.kind, bg),
            bg,
            is_selected,
            focused,
        );
        let kind_color = app.colors.ui.readable_foreground(
            kind_color,
            bg,
            if kind_icon.is_ascii() { 4.5 } else { 3.0 },
        );
        let detail_foreground = app.colors.ui.description_color(bg);

        let max_name = (render_width as usize).saturating_sub(4);
        let name_display = if UnicodeWidthStr::width(item.name.as_str()) > max_name {
            let mut truncated = String::with_capacity(max_name);
            let mut w = 0;
            for ch in item.name.chars() {
                let cw = ch.width().unwrap_or(0);
                if w + cw + 1 > max_name {
                    break;
                }
                truncated.push(ch);
                w += cw;
            }
            format!("{truncated}…")
        } else {
            item.name.clone()
        };
        line_info_buf.clear();
        let _ = write!(line_info_buf, ":{}", item.line + 1); // Error OK: write to String buffer is infallible in practice

        kind_buf.clear();
        let _ = write!(kind_buf, " {kind_icon} "); // Error OK: write to String buffer is infallible in practice
        // Pre-compute total width before building spans to avoid double iteration
        let text_width: u16 = (UnicodeWidthStr::width(kind_buf.as_str())
            + UnicodeWidthStr::width(name_display.as_str())
            + UnicodeWidthStr::width(line_info_buf.as_str())) as u16;
        let mut spans = vec![
            Span::styled(
                std::mem::take(&mut kind_buf),
                Style::default().fg(kind_color).bg(bg),
            ),
            Span::styled(name_display, row_style),
            Span::styled(
                std::mem::take(&mut line_info_buf),
                Style::default().fg(detail_foreground).bg(bg),
            ),
        ];
        let fill = render_width.saturating_sub(text_width);
        if fill > 0 {
            spans.push(Span::styled(
                crate::helpers::spaces(fill as usize),
                row_style,
            ));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(row_style),
            Rect::new(entry_area.x, y, render_width, 1),
        );
    }

    // Draw scrollbar if content overflows
    if let Some(sb) = sb_area {
        let height = sb.height as usize;
        let thumb_size = ((visible as f64 / total_items as f64) * height as f64).max(1.0) as usize;
        let thumb_size = thumb_size.min(height);
        let max_scroll = total_items.saturating_sub(visible);
        let thumb_pos = if max_scroll > 0 {
            ((scroll as f64 / max_scroll as f64) * (height - thumb_size) as f64) as usize
        } else {
            0
        };
        let hovered = sb.contains((app.mouse_col, app.mouse_row).into());
        let (track_color, thumb_color) = app.colors.ui.scrollbar_colors(sidebar_bg, hovered, false);
        let mut sb_lines: Vec<Line> = Vec::with_capacity(height);
        for i in 0..height {
            let in_thumb = i >= thumb_pos && i < thumb_pos + thumb_size;
            let color = if in_thumb { thumb_color } else { track_color };
            sb_lines.push(Line::from(Span::styled(
                "▐",
                Style::default().fg(color).bg(track_color),
            )));
        }
        f.render_widget(Paragraph::new(sb_lines), sb);
    }
}

fn fixture() -> App {
    let mut app = App::test_new();
    app.sidebar_view = crate::app::SidebarView::Outline;
    app.outline_items = (0..1000)
        .map(|i| crate::app::SymbolItem {
            name: [
                "function",
                "界café",
                "extremely_long_function_name_for_truncation",
                "👩‍💻test",
                "\twith_tab",
            ][i % 5]
                .into(),
            kind: ["function", "struct", "impl", "type", "enum", "constant"][i % 6],
            line: i * 100,
            tab_index: None,
            file_name: None,
            file_path: None,
        })
        .collect();
    app
}

fn frame(app: &App, width: u16, optimized: bool) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width + 5, 55)).unwrap();
    terminal
        .draw(|f| {
            let area = Rect::new(2, 1, width, 52);
            if optimized {
                draw_outline(f, app, area);
            } else {
                reference_outline(f, app, area);
            }
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn outline_borrowed_rows_match_all_original_cells() {
    let mut app = fixture();
    for theme in [
        crate::app_theme::EditorTheme::Dark,
        crate::app_theme::EditorTheme::Light,
    ] {
        app.colors = theme.colors();
        for focused in [true, false] {
            app.active_pane = if focused {
                crate::app::ActivePane::Explorer
            } else {
                crate::app::ActivePane::Editor
            };
            for scroll in [0, 50, 993] {
                app.outline_scroll = scroll;
                app.outline_hovered = Some(scroll + 2);
                app.outline_selected = scroll + 4;
                for width in [0, 1, 5, 16, 38, 90] {
                    assert_eq!(
                        frame(&app, width, true),
                        frame(&app, width, false),
                        "width={width}, scroll={scroll}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "manual optimized outline redraw comparison"]
fn benchmark_outline_redraw() {
    use std::{hint::black_box, time::Instant};
    let app = fixture();
    let mut times = [Vec::new(), Vec::new()];
    let mut terminal = Terminal::new(TestBackend::new(55, 60)).unwrap();
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let start = Instant::now();
            for _ in 0..100 {
                terminal
                    .draw(|f| {
                        let area = f.area();
                        if optimized {
                            draw_outline(f, &app, area);
                        } else {
                            reference_outline(f, &app, area);
                        }
                    })
                    .unwrap();
                black_box(terminal.backend().buffer());
            }
            times[usize::from(optimized)].push(start.elapsed() / 100);
        }
    }
    for samples in &mut times {
        samples.sort_unstable();
    }
    eprintln!(
        "outline 55x60: before={:?}, after={:?}",
        times[0][3], times[1][3]
    );
}
