//! Shared line geometry for Settings rendering, navigation and pointer input.
use crate::app::App;
use ratatui::layout::Rect;
use std::{cell::RefCell, rc::Rc};

pub(crate) fn control_offset(kind: &str, lines: usize, width: usize) -> usize {
    if kind == "checkbox" && lines > 1 {
        1
    } else {
        lines + usize::from(width >= 60 && lines > 1)
    }
}

pub(crate) fn visual_height(kind: &str, lines: usize, width: usize) -> usize {
    if kind == "heading" {
        lines.max(1)
    } else if kind == "checkbox" && lines > 1 {
        lines + App::setting_row_gap(width)
    } else {
        control_offset(kind, lines, width) + 1 + App::setting_row_gap(width)
    }
}

pub(crate) struct Row {
    pub index: usize,
    pub start: usize,
    pub height: usize,
    pub control: usize,
    pub lines: Vec<String>,
    id: String,
}

pub(crate) struct Layout {
    pub rows: Vec<Row>,
    pub total: usize,
    width: usize,
}

impl Layout {
    pub fn max_scroll(&self, height: usize) -> usize {
        self.total.saturating_sub(height)
    }

    pub fn visible_start(&self, line: usize) -> usize {
        self.rows
            .partition_point(|row| row.start + row.height <= line)
    }

    pub fn row_at(&self, line: usize) -> Option<&Row> {
        self.rows.get(self.visible_start(line))
    }

    pub fn row_for_item(&self, index: usize) -> Option<&Row> {
        self.rows.iter().find(|row| row.index == index)
    }

    pub fn scrollbar(&self, area: Rect) -> Option<(u16, usize, usize, usize)> {
        let height = area.height as usize;
        let max = self.max_scroll(height);
        if area.is_empty() || max == 0 {
            return None;
        }
        let thumb = ((height * height) / self.total).max(1).min(height);
        Some((area.right() - 1, height, thumb, max))
    }
}

#[derive(Default)]
pub(crate) struct Cache(RefCell<Vec<Rc<Layout>>>);

impl Cache {
    pub fn get(
        &self,
        items: &[(String, String, String)],
        filtered: &[usize],
        width: usize,
    ) -> Rc<Layout> {
        let mut cached = self.0.borrow_mut();
        // Values do not affect geometry. Compare labels and ids exactly, so
        // keybinding changes/search/category changes cannot reuse stale rows.
        if let Some(layout) = cached.iter().find(|layout| {
            layout.width == width
                && layout.rows.len() == filtered.len()
                && layout.rows.iter().zip(filtered).all(|(row, &index)| {
                    row.index == index
                        && items
                            .get(index)
                            .is_some_and(|(label, _, id)| row.lines[0] == *label && row.id == *id)
                })
        }) {
            return Rc::clone(layout);
        }

        let mut total = 0;
        let rows = filtered
            .iter()
            .filter_map(|&index| {
                let (label, _, id) = items.get(index)?;
                let kind = App::setting_field_kind(id);
                let lines = App::setting_description_lines(label, id, width);
                let height = visual_height(kind, lines.len(), width);
                let row = Row {
                    index,
                    start: total,
                    height,
                    control: control_offset(kind, lines.len(), width),
                    lines,
                    id: id.clone(),
                };
                total += height;
                Some(row)
            })
            .collect();
        let layout = Rc::new(Layout { rows, total, width });
        // Retain a few widths for split panes without unbounded search history.
        if cached.len() == 4 {
            cached.remove(0);
        }
        cached.push(Rc::clone(&layout));
        layout
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Drag {
    pub row: u16,
    pub offset: usize,
}

impl Drag {
    pub fn offset_at(self, row: u16, travel: usize, max: usize) -> usize {
        if travel == 0 {
            return self.offset.min(max);
        }
        let delta = ((i64::from(row) - i64::from(self.row)) as f64 * max as f64 / travel as f64)
            .round() as isize;
        self.offset.saturating_add_signed(delta).min(max)
    }
}

impl App {
    pub(crate) fn settings_line_layout(&self, width: usize) -> Rc<Layout> {
        let items = self.settings_items();
        let filtered =
            self.settings_filtered_indices_with_items(self.settings_category_selected, &items);
        self.settings_layout_cache.get(&items, &filtered, width)
    }
}
