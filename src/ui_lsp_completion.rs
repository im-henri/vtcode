//! Completion presentation and interaction. Server results remain authoritative;
//! cached presentation never supplies edits to the editor.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui_search_dialog::{CompletionPopupGeometry, completion_index_at};
use crate::{app::App, lsp::LspCompletionItem, ui_markdown::EmbeddedMarkdown};

const MAX_ROWS: usize = 10;
const MAX_DOC_BYTES: usize = 64 * 1024;

pub(crate) struct State {
    pub(crate) details_visible: bool,
    pub(crate) explicit: bool,
    pub(crate) status: Option<&'static str>,
    pub(crate) layout: Option<Layout>,
    doc_scroll: usize,
    doc_rows: usize,
    doc_key: Option<(u64, usize)>,
    doc: Option<EmbeddedMarkdown>,
    width_cache: Option<(u64, u16)>,
    scroll_drag: Option<bool>,
    pub(crate) signature: std::cell::RefCell<Option<crate::ui_dialogs::SignaturePresentation>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            details_visible: true,
            explicit: false,
            status: None,
            layout: None,
            doc_scroll: 0,
            doc_rows: 0,
            doc_key: None,
            doc: None,
            width_cache: None,
            scroll_drag: None,
            signature: Default::default(),
        }
    }
}

impl State {
    pub(crate) fn begin_request(&mut self, continuing: bool) {
        if !continuing {
            self.explicit = false;
        }
        self.status = Some("Loading suggestions…");
        self.layout = None;
        self.doc_key = None;
        self.doc = None;
        self.doc_scroll = 0;
        self.width_cache = None;
        self.scroll_drag = None;
    }

    fn scroll_docs(&mut self, delta: isize) {
        let height = self
            .layout
            .and_then(|layout| layout.details)
            .map_or(0, |area| area.height.saturating_sub(2) as usize);
        self.doc_scroll = self
            .doc_scroll
            .saturating_add_signed(delta)
            .min(self.doc_rows.saturating_sub(height));
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub(crate) list: CompletionPopupGeometry,
    pub(crate) details: Option<Rect>,
    pub(crate) footer: Rect,
    toggle: Rect,
    close: Rect,
}

pub(crate) fn visible(app: &App) -> bool {
    app.lsp_completion_active
        && (!app.lsp_manager.completion_items.is_empty()
            || app.completion_ui.explicit && app.completion_ui.status.is_some())
}

pub(crate) fn occupied_area(app: &App) -> Option<Rect> {
    visible(app)
        .then_some(app.completion_ui.layout)
        .flatten()
        .map(|layout| {
            layout.details.map_or(layout.list.popup_area, |details| {
                layout.list.popup_area.union(details)
            })
        })
}

fn cell_width(text: &str) -> usize {
    // Width sampling is bounded even for generated, megabyte-long labels.
    text.chars()
        .take(80)
        .map(|ch| ch.width().unwrap_or(0))
        .sum()
}

fn list_width(app: &mut App) -> u16 {
    let revision = app.lsp_manager.completion_revision;
    if let Some((_, width)) = app
        .completion_ui
        .width_cache
        .filter(|(r, _)| *r == revision)
    {
        return width;
    }
    let width = app
        .lsp_manager
        .completion_items
        .iter()
        .take(32)
        .map(|item| cell_width(&item.label) + 6)
        .max()
        .unwrap_or(40)
        .clamp(40, 64) as u16;
    app.completion_ui.width_cache = Some((revision, width));
    width
}

pub(crate) fn layout(app: &mut App, viewport: Rect) -> Option<Layout> {
    let bounds = app.code_area?.intersection(viewport);
    if bounds.width < 8 || bounds.height < 4 {
        return None;
    }
    let (cx, cy) = crate::ui_search_dialog::completion_cursor_position(app)?;
    let cx = cx.clamp(bounds.x, bounds.right().saturating_sub(1));
    let cy = cy.clamp(bounds.y, bounds.bottom().saturating_sub(1));
    let below = bounds.bottom().saturating_sub(cy + 1);
    let above = cy.saturating_sub(bounds.y);
    let count = app.lsp_manager.completion_items.len();
    let wanted = count.max(1).min(MAX_ROWS) as u16 + 3;
    let down = below >= wanted || below >= above;
    let available = if down { below } else { above };
    if available < 3 {
        return None;
    }
    let height = wanted.min(available);
    let footer_height = u16::from(height >= 4);
    let max_width = if app.completion_ui.details_visible && bounds.width >= 64 {
        bounds.width - 24
    } else {
        bounds.width
    };
    let width = list_width(app).min(max_width);
    let x = cx.min(bounds.right() - width);
    let y = if down { cy + 1 } else { cy - height };
    let area = Rect::new(x, y, width, height);
    let inner = Rect::new(x + 1, y + 1, width - 2, height - 2 - footer_height);
    let selected = app.lsp_completion_selected.min(count.saturating_sub(1));
    let list = CompletionPopupGeometry {
        popup_area: area,
        inner_area: inner,
        scroll: selected.saturating_sub(inner.height.saturating_sub(1) as usize),
    };
    let footer = Rect::new(x + 1, inner.bottom(), width - 2, footer_height);
    let toggle = Rect::new(
        footer.right().saturating_sub(15).max(footer.x),
        footer.y,
        footer.width.min(15),
        footer.height,
    );
    let close = Rect::new(area.right() - 4, area.y, 3, 1);
    let details = if app.completion_ui.details_visible && count > 0 {
        let right = bounds.right() - area.right();
        let left = area.x - bounds.x;
        let doc_height = available.min(height.max(12));
        let doc_y = if down { area.y } else { cy - doc_height };
        if right >= 24 {
            Some(Rect::new(area.right(), doc_y, right.min(52), doc_height))
        } else if left >= 24 {
            let width = left.min(52);
            Some(Rect::new(area.x - width, doc_y, width, doc_height))
        } else if available.saturating_sub(height) >= 5 {
            let doc_height = (available - height).min(10);
            Some(Rect::new(
                area.x,
                if down {
                    area.bottom()
                } else {
                    area.y - doc_height
                },
                area.width,
                doc_height,
            ))
        } else {
            None
        }
    } else {
        None
    };
    Some(Layout {
        list,
        details,
        footer,
        toggle,
        close,
    })
}

fn clip(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        let w = ch.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        result.push(ch);
        used += w;
    }
    result
}

fn kind(item: &LspCompletionItem) -> (&'static str, &'static str) {
    use lsp_types::CompletionItemKind as K;
    match item.kind {
        Some(K::FUNCTION) => ("fn ", "Function"),
        Some(K::METHOD) => ("fn ", "Method"),
        Some(K::VARIABLE) => ("var", "Variable"),
        Some(K::CLASS) => ("typ", "Class"),
        Some(K::STRUCT) => ("typ", "Struct"),
        Some(K::INTERFACE) => ("ifc", "Interface"),
        Some(K::MODULE) => ("mod", "Module"),
        Some(K::PROPERTY) | Some(K::FIELD) => ("fld", "Field"),
        Some(K::KEYWORD) => ("kw ", "Keyword"),
        Some(K::SNIPPET) => ("snp", "Snippet"),
        Some(K::CONSTANT) => ("cst", "Constant"),
        Some(K::ENUM) | Some(K::ENUM_MEMBER) => ("enm", "Enum"),
        Some(K::FILE) => ("fil", "File"),
        Some(K::FOLDER) => ("dir", "Folder"),
        Some(K::TYPE_PARAMETER) => ("typ", "Type parameter"),
        _ => ("   ", "Symbol"),
    }
}

#[allow(deprecated)]
fn deprecated(item: &LspCompletionItem) -> bool {
    item.original_item.as_ref().is_some_and(|item| {
        item.deprecated == Some(true)
            || item
                .tags
                .as_ref()
                .is_some_and(|tags| tags.contains(&lsp_types::CompletionItemTag::DEPRECATED))
    })
}

fn annotation(item: &LspCompletionItem) -> &str {
    item.original_item
        .as_ref()
        .and_then(|item| item.label_details.as_ref())
        .and_then(|details| details.description.as_deref().or(details.detail.as_deref()))
        .or_else(|| {
            item.detail
                .as_deref()
                .map(|detail| detail.strip_prefix(&item.label).unwrap_or(detail))
        })
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
}

fn bounded(text: &str) -> &str {
    let mut end = text.len().min(MAX_DOC_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn documentation(item: &LspCompletionItem) -> String {
    let mut source = String::new();
    // Signatures are literal code, not Markdown (e.g. C pointers and generics).
    for line in bounded(item.detail.as_deref().unwrap_or(&item.label)).lines() {
        source.push_str("    ");
        source.push_str(line);
        source.push('\n');
    }
    source.push('\n');
    source.push_str(kind(item).1);
    if item.is_snippet {
        source.push_str(" · Snippet");
    }
    if !item.additional_text_edits.is_empty() {
        source.push_str(" · Additional edits / auto import");
    }
    if deprecated(item) {
        source.push_str(" · Deprecated");
    }
    if let Some(description) = item
        .original_item
        .as_ref()
        .and_then(|item| item.label_details.as_ref())
        .and_then(|details| details.description.as_deref())
    {
        source.push_str("\n\n    ");
        source.push_str(&clip(description, 256));
    }
    if let Some(doc) = item
        .documentation
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        source.push_str("\n\n");
        // Preserve plaintext documentation literally, but render MarkupContent.
        let plain = item.original_item.as_ref().and_then(|item| item.documentation.as_ref())
            .is_some_and(|doc| matches!(doc, lsp_types::Documentation::String(_))
                || matches!(doc, lsp_types::Documentation::MarkupContent(markup) if markup.kind == lsp_types::MarkupKind::PlainText));
        if plain {
            for line in bounded(doc).lines() {
                source.push_str("    ");
                source.push_str(line);
                source.push('\n');
            }
        } else {
            source.push_str(bounded(doc));
        }
        if doc.len() > MAX_DOC_BYTES {
            source.push_str("\n\nDocumentation truncated.");
        }
    }
    source
}

fn panel(f: &mut Frame, app: &App, area: Rect, title: &str) -> (Color, Style) {
    let surface = crate::ui_widgets::surface_background(f, area, app.colors.bg_editor);
    let bg = app
        .colors
        .ui
        .suggest_colors
        .background
        .over(surface)
        .unwrap_or(surface);
    let style = app.colors.ui.suggest_row_style(bg, false, false);
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(style)
            .style(style)
            .border_style(
                Style::default().fg(app.colors.ui.suggest_colors.border.border(bg).unwrap_or(bg)),
            ),
        area,
    );
    (bg, style)
}

fn scrollbar(f: &mut Frame, area: Rect, offset: usize, page: usize, total: usize, style: Style) {
    if total <= page || area.height == 0 {
        return;
    }
    let track = area.height as usize;
    let thumb = (page * track / total).max(1).min(track);
    let top = offset.min(total - page) * (track - thumb) / (total - page);
    for row in 0..track {
        f.render_widget(
            Paragraph::new(if row >= top && row < top + thumb {
                "┃"
            } else {
                "│"
            })
            .style(style),
            Rect::new(area.x, area.y + row as u16, 1, 1),
        );
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    app.completion_ui.layout = None;
    if !visible(app) {
        return;
    }
    let Some(layout) = layout(app, f.area()) else {
        return;
    };
    app.completion_ui.layout = Some(layout);
    let geometry = layout.list;
    let inner = geometry.inner_area;
    let count = app.lsp_manager.completion_items.len();
    app.lsp_completion_selected = app.lsp_completion_selected.min(count.saturating_sub(1));
    let (bg, normal) = panel(f, app, geometry.popup_area, " LSP suggestions ");
    f.render_widget(Paragraph::new(" × ").style(normal), layout.close);
    let query = app
        .editor()
        .and_then(|editor| editor.word_prefix_at_cursor())
        .unwrap_or_default();
    let hovered = completion_index_at(geometry, app.mouse_col, app.mouse_row, count);
    if count == 0 {
        f.render_widget(
            Paragraph::new(app.completion_ui.status.unwrap_or("No suggestions")).style(normal),
            inner,
        );
    }
    for (row, (index, item)) in app
        .lsp_manager
        .completion_items
        .iter()
        .enumerate()
        .skip(geometry.scroll)
        .take(inner.height as usize)
        .enumerate()
    {
        let selected = index == app.lsp_completion_selected;
        let style =
            app.colors
                .ui
                .suggest_row_style(bg, selected, !selected && hovered == Some(index));
        let surface = style.bg.unwrap_or(bg);
        let label_style = if deprecated(item) {
            style.add_modifier(Modifier::CROSSED_OUT)
        } else {
            style
        };
        let available = inner.width.saturating_sub(4) as usize;
        let hint = annotation(item);
        let hint_width = if available >= 24 && !hint.is_empty() {
            (available / 3).min(cell_width(hint))
        } else {
            0
        };
        let label_width = available.saturating_sub(if hint_width > 0 { hint_width + 2 } else { 0 });
        let label = clip(&item.label, label_width);
        let matches = if query.is_empty() {
            vec![]
        } else {
            crate::app::fuzzy_match_indices(&label, &query)
                .map(|(_, indices)| indices)
                .unwrap_or_default()
        };
        let highlight = label_style
            .fg(app.colors.ui.suggest_match_color(surface, selected))
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![
            Span::styled(
                kind(item).0,
                style.fg(app
                    .colors
                    .ui
                    .completion_icon_color(item.kind, surface, selected)),
            ),
            Span::styled(" ", style),
        ];
        let mut start = 0;
        let mut matched = matches.first() == Some(&0);
        for (index, (byte, _)) in label.char_indices().enumerate() {
            let next = matches.binary_search(&index).is_ok();
            if next != matched {
                spans.push(Span::styled(
                    &label[start..byte],
                    if matched { highlight } else { label_style },
                ));
                start = byte;
                matched = next;
            }
        }
        spans.push(Span::styled(
            &label[start..],
            if matched { highlight } else { label_style },
        ));
        let padding = label_width.saturating_sub(label.width());
        spans.push(Span::styled(crate::helpers::spaces(padding), style));
        if hint_width > 0 {
            spans.push(Span::styled(
                format!("  {}", clip(hint, hint_width)),
                style.fg(app.colors.ui.description_color(surface)),
            ));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(style),
            Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
        );
    }
    let counter = if count == 0 {
        "Esc close".into()
    } else {
        format!(" {}/{}  Tab/Enter", app.lsp_completion_selected + 1, count)
    };
    f.render_widget(
        Paragraph::new(clip(
            &counter,
            layout.toggle.x.saturating_sub(layout.footer.x) as usize,
        ))
        .style(normal.fg(app.colors.ui.description_color(bg))),
        layout.footer,
    );
    let toggle_hovered = layout
        .toggle
        .contains((app.mouse_col, app.mouse_row).into());
    f.render_widget(
        Paragraph::new(if app.completion_ui.details_visible {
            " Details ▾ "
        } else {
            " Details ▸ "
        })
        .style(app.colors.ui.suggest_row_style(bg, false, toggle_hovered)),
        layout.toggle,
    );
    scrollbar(
        f,
        Rect::new(geometry.popup_area.right() - 1, inner.y, 1, inner.height),
        geometry.scroll,
        inner.height as usize,
        count,
        normal,
    );

    let Some(doc_area) = layout.details else {
        return;
    };
    let key = (
        app.lsp_manager.completion_revision,
        app.lsp_completion_selected,
    );
    if app.completion_ui.doc_key != Some(key) {
        if app
            .completion_ui
            .doc_key
            .is_none_or(|(_, selected)| selected != key.1)
        {
            app.completion_ui.doc_scroll = 0;
        }
        let item = &app.lsp_manager.completion_items[key.1];
        app.completion_ui.doc = Some(EmbeddedMarkdown::new(&documentation(item)));
        app.completion_ui.doc_key = Some(key);
    }
    let (bg, style) = panel(
        f,
        app,
        doc_area,
        if doc_area.width >= 30 {
            " Details · Alt+↑↓ scroll "
        } else {
            " Details · Alt+↑↓ "
        },
    );
    let body = Rect::new(
        doc_area.x + 1,
        doc_area.y + 1,
        doc_area.width - 2,
        doc_area.height - 2,
    );
    let lines = app.completion_ui.doc.as_mut().unwrap().widget_lines(
        body.width as usize,
        &app.colors,
        bg,
        style.fg.unwrap_or(app.colors.fg_text),
    );
    app.completion_ui.doc_rows = lines.len();
    app.completion_ui.doc_scroll = app
        .completion_ui
        .doc_scroll
        .min(lines.len().saturating_sub(body.height as usize));
    for (row, line) in lines
        .iter()
        .skip(app.completion_ui.doc_scroll)
        .take(body.height as usize)
        .enumerate()
    {
        f.render_widget(
            Paragraph::new(line.clone()).style(style),
            Rect::new(body.x, body.y + row as u16, body.width, 1),
        );
    }
    scrollbar(
        f,
        Rect::new(doc_area.right() - 1, body.y, 1, body.height),
        app.completion_ui.doc_scroll,
        body.height as usize,
        app.completion_ui.doc_rows,
        style,
    );
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.lsp_completion_active {
        return false;
    }
    let count = app.lsp_manager.completion_items.len();
    if count == 0 {
        return false;
    }
    if key.code == KeyCode::Char(' ') && key.modifiers == KeyModifiers::CONTROL {
        app.completion_ui.details_visible = !app.completion_ui.details_visible;
        return true;
    }
    if key.modifiers == KeyModifiers::ALT
        && matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        )
    {
        let page = app
            .completion_ui
            .layout
            .and_then(|layout| layout.details)
            .map_or(8, |area| area.height.saturating_sub(2)) as isize;
        app.completion_ui.scroll_docs(match key.code {
            KeyCode::Up => -1,
            KeyCode::Down => 1,
            KeyCode::PageUp => -page,
            _ => page,
        });
        return true;
    }
    if !key.modifiers.is_empty() {
        return false;
    }
    let page = app
        .completion_ui
        .layout
        .map_or(MAX_ROWS, |layout| layout.list.inner_area.height as usize)
        .max(1);
    let index = app.lsp_completion_selected;
    app.lsp_completion_selected = match key.code {
        KeyCode::Up => index.saturating_sub(1),
        KeyCode::Down => index.saturating_add(1).min(count - 1),
        KeyCode::PageUp => index.saturating_sub(page),
        KeyCode::PageDown => index.saturating_add(page).min(count - 1),
        KeyCode::Enter | KeyCode::Tab => {
            app.apply_lsp_completion();
            return true;
        }
        _ => return false,
    };
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !visible(app) {
        app.completion_ui.scroll_drag = None;
        return false;
    }
    let Some(layout) = app.completion_ui.layout else {
        return false;
    };
    let point = (mouse.column, mouse.row).into();
    let in_list = layout.list.popup_area.contains(point);
    let in_doc = layout.details.is_some_and(|area| area.contains(point));
    if let Some(doc) = app.completion_ui.scroll_drag {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                scroll_to_mouse(app, layout, doc, mouse.row);
                return true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                app.completion_ui.scroll_drag = None;
                return true;
            }
            MouseEventKind::Down(_) => app.completion_ui.scroll_drag = None,
            _ => {}
        }
    }
    if !in_list && !in_doc {
        return false;
    }
    match mouse.kind {
        MouseEventKind::Moved => {
            app.mouse_hover_changed = true;
            app.hovered_editor_pos = None;
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if mouse.kind == MouseEventKind::ScrollUp {
                -3
            } else {
                3
            };
            if in_doc {
                app.completion_ui.scroll_docs(delta);
            } else {
                app.lsp_completion_selected = app
                    .lsp_completion_selected
                    .saturating_add_signed(delta)
                    .min(app.lsp_manager.completion_items.len().saturating_sub(1));
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if layout.close.contains(point) {
                app.lsp_completion_active = false;
                app.dismiss_vtcomplete();
                app.dispatch_lsp_suggestions();
            } else if layout.toggle.contains(point) {
                app.completion_ui.details_visible = !app.completion_ui.details_visible;
            } else if in_doc || mouse.column == layout.list.popup_area.right() - 1 {
                let area = if in_doc {
                    layout.details.unwrap()
                } else {
                    layout.list.popup_area
                };
                if mouse.column == area.right() - 1
                    && mouse.row > area.y
                    && mouse.row < area.bottom() - 1
                {
                    app.completion_ui.scroll_drag = Some(in_doc);
                    scroll_to_mouse(app, layout, in_doc, mouse.row);
                }
            } else if let Some(index) = completion_index_at(
                layout.list,
                mouse.column,
                mouse.row,
                app.lsp_manager.completion_items.len(),
            ) {
                app.lsp_completion_selected = index;
                app.apply_lsp_completion();
            }
        }
        _ => {}
    }
    true
}

fn scroll_to_mouse(app: &mut App, layout: Layout, doc: bool, row: u16) {
    let area = if doc {
        let Some(area) = layout.details else {
            app.completion_ui.scroll_drag = None;
            return;
        };
        Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        )
    } else {
        layout.list.inner_area
    };
    let height = area.height as usize;
    let rel = row
        .saturating_sub(area.y)
        .min(height.saturating_sub(1) as u16) as usize;
    let total = if doc {
        app.completion_ui.doc_rows.saturating_sub(height)
    } else {
        app.lsp_manager.completion_items.len().saturating_sub(1)
    };
    let offset = rel * total / height.saturating_sub(1).max(1);
    if doc {
        app.completion_ui.doc_scroll = offset;
    } else {
        app.lsp_completion_selected = offset;
    }
}

#[cfg(test)]
#[path = "ui_lsp_completion_tests.rs"]
mod tests;
