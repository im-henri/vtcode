// Frozen comparison renderer from 5da4a00; test-only.
use super::*;
pub(super) fn reference_terminal_draw(
    f: &mut Frame,
    pty: &crate::terminal::TerminalEmulator,
    area: Rect,
    palette: &crate::theme_workbench::WorkbenchColors,
    blink_visible: bool,
) {
    let width = area.width as usize;
    let height = area.height as usize;
    let default_fg = palette.fg_terminal;
    let default_bg = palette.bg_terminal;
    let grid = &pty.grid;
    let mut selection_style = crate::ui_colors::TerminalSelectionStyle::new(palette);
    let steady_cursor = matches!(grid.cursor_shape, 2 | 4 | 6);
    let show_cursor = grid.cursor_visible
        && !grid.has_selection()
        && grid.scroll_view == 0
        && (steady_cursor || blink_visible)
        && grid.cursor_row < height
        && grid.cursor_col < width;

    {
        let buffer = f.buffer_mut();
        for row in 0..height {
            let y = area.y.saturating_add(row as u16);
            let cells = grid.display_line(row);
            for col in 0..width {
                let x = area.x.saturating_add(col as u16);
                let Some(cell) = buffer.cell_mut((x, y)) else {
                    continue;
                };
                if let Some(term_cell) = cells.get(col) {
                    let foreground = palette.terminal_color(term_cell.fg, true);
                    let background = palette.terminal_color(term_cell.bg, false);
                    let mut style = Style::default().fg(foreground).bg(background);
                    if term_cell.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if term_cell.underline {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    if term_cell.dim {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    if term_cell.italic {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if term_cell.strikethrough {
                        style = style.add_modifier(Modifier::CROSSED_OUT);
                    }
                    if grid.selection_contains(row, col) {
                        style = selection_style.apply(style, default_fg);
                    }
                    cell.set_char(if term_cell.ch == '\0' {
                        ' '
                    } else {
                        term_cell.ch
                    });
                    cell.set_style(style);
                } else {
                    cell.set_char(' ');
                    cell.set_style(Style::default().fg(default_fg).bg(default_bg));
                }
            }
        }

        // Keep a buffer-level cursor as well, so screenshots and terminals that
        // suppress the native cursor still show an insertion point.
        if show_cursor {
            let x = area.x + grid.cursor_col as u16;
            let y = area.y + grid.cursor_row as u16;
            if let Some(cell) = buffer.cell_mut((x, y)) {
                match grid.cursor_shape {
                    3 | 4 => {
                        if cell.symbol().trim().is_empty() {
                            cell.set_char('▁');
                        }
                        cell.set_style(
                            Style::default()
                                .fg(default_fg)
                                .bg(default_bg)
                                .add_modifier(Modifier::UNDERLINED),
                        );
                    }
                    5 | 6 => {
                        if cell.symbol().trim().is_empty() {
                            cell.set_char('▏');
                            cell.set_style(Style::default().fg(default_fg).bg(default_bg));
                        } else {
                            cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                        }
                    }
                    _ => {
                        cell.set_style(
                            crate::ui_colors::block_cursor_style(
                                palette.bg_terminal_cursor,
                                palette.fg_terminal_cursor,
                                cell.bg,
                                palette.contrast_colors(),
                            )
                            .remove_modifier(Modifier::DIM),
                        );
                    }
                }
            }
        }
    }

    if show_cursor {
        f.set_cursor_position((
            area.x + grid.cursor_col as u16,
            area.y + grid.cursor_row as u16,
        ));
    }
}
