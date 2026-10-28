//! CommonMark/GFM preview, laid out in terminal cells before viewport scrolling.
use crate::textmate_backend::HighlightLines;
use std::{cell::RefCell, ops::Range, rc::Rc};

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use crate::textmate::{Theme,SyntaxRegistry};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{app::App, app_theme::ThemeColors};

#[path = "ui_markdown_diagrams.rs"]
mod diagrams;
#[path = "ui_markdown_markup.rs"]
mod markup;
#[path = "ui_markdown_extensions.rs"]
mod extensions;
#[cfg(test)]
#[path = "ui_markdown_feature_tests.rs"]
mod feature_tests;

#[derive(Clone, Copy, PartialEq)]
struct Palette {
    compact_code: bool,
    contrast: Option<[Color; 14]>,
    text: Color,
    dim: Color,
    heading: Color,
    link: Color,
    code: Color,
    code_bg: Color,
    inline_code: Color,
    inline_bg: Color,
    quote: Color,
    quote_background: Option<Color>,
    quote_text: Color,
    success: Color,
    warning: Color,
    error: Color,
}
impl From<&ThemeColors> for Palette {
    fn from(c: &ThemeColors) -> Self {
        Self {
            compact_code: false,
            contrast: c.ui.contrast_fallbacks(),
            text: c.fg_text,
            dim: c.fg_dim,
            heading: c.fg_md_heading,
            link: c.fg_md_link,
            code: c.fg_md_code,
            code_bg: c.bg_md_code,
            inline_code: c.ui.readable_foreground(c.ui.fg_md_inline, c.ui.bg_md_inline, 4.5),
            inline_bg: c.ui.bg_md_inline,
            quote: c.ui.md_quote_border.over(c.ui.md_quote_background.over(c.bg_editor).unwrap_or(c.bg_editor)).unwrap_or(c.fg_md_blockquote),
            quote_background: c.ui.md_quote_background.over(c.bg_editor),
            quote_text: c.ui.editor_foreground.over(c.ui.md_quote_background.over(c.bg_editor).unwrap_or(c.bg_editor)).unwrap_or(c.fg_text),
            success: c.fg_success,
            warning: c.fg_warning,
            error: c.fg_error,
        }
    }
}

struct CachedPreview {
    rope: ropey::Rope,
    width: usize,
    palette: Palette,
    theme: String,
    registry: u64,
    rows: Vec<Row>,
    text_rows: Rc<[String]>,
    source_rows: Rc<[usize]>,
    links: Rc<[Vec<LinkSpan>]>,
    document: PreparedDocument,
    code_views: Vec<CodeView>,
}

#[derive(Clone)]
struct CodeView {
    block: usize,
    rows: Range<usize>,
    prefix: String,
}

impl CachedPreview {
    fn highlight_visible(&mut self, rows: Range<usize>, syntax: (&SyntaxRegistry, Option<&Theme>)) {
        // Layout ranges are sorted. A scroll inspects only the visible fences,
        // and previously highlighted fences retain their measured text/spans.
        let first = self.code_views.partition_point(|view| view.rows.end <= rows.start);
        for view in self.code_views[first..].iter().take_while(|view| view.rows.start < rows.end) {
            if self.document.highlighted.borrow().contains(&view.block) { continue; }
            let Some(path) = self.document.code_paths.get(&view.block) else { continue; };
            let mut nodes = &self.document.nodes;
            let mut node = None;
            for &index in path { node = nodes.get(index); nodes = &node.unwrap().children; }
            let Some(node) = node else { continue; };
            let Event::Start(Tag::CodeBlock(kind)) = &node.event else { continue; };
            let previous = self.document.code.borrow().get(&view.block).cloned();
            let mut renderer = Renderer { rows: vec![], code_views: vec![], width: self.width.max(1), palette: self.palette, document: &self.document, syntax: Some(syntax) };
            renderer.code(node, kind, &view.prefix);
            let range = renderer.code_views[0].rows.clone();
            let stable = range.len() == view.rows.len() && self.rows[view.rows.clone()].iter().zip(&renderer.rows[range.clone()]).all(|(old, new)| {
                old.source == new.source && old.line.spans.iter().flat_map(|span| span.content.chars()).eq(new.line.spans.iter().flat_map(|span| span.content.chars()))
            });
            if !stable {
                // A grammar can split a Unicode grapheme across token styles.
                // Keep literal text if that would move rows or invalidate an
                // active selection. Do not retry the same grammar every frame.
                if let Some(previous) = previous { self.document.code.borrow_mut().insert(view.block, previous); }
                continue;
            }
            for (old, new) in self.rows[view.rows.clone()].iter_mut().zip(renderer.rows.drain(range)) { *old = new; }
        }
    }
}

/// Per-tab visual scroll state. Source lines and rendered rows are different
/// coordinate systems, especially inside long paragraphs and tables.
#[derive(Clone, Default)]
pub(crate) struct PreviewScroll {
    pub(crate) source_scroll: usize,
    pub(crate) pending_source: Option<usize>,
    row: usize,
    height: usize,
    source_rows: Rc<[usize]>,
    drag_offset: usize,
    text_rows: Rc<[String]>,
    links: Rc<[Vec<LinkSpan>]>,
    anchors: Rc<rustc_hash::FxHashMap<String, usize>>,
    body: Rect,
    anchor: Option<(usize, usize)>,
    cursor: (usize, usize),
    pub(crate) selecting: bool,
}

impl PreviewScroll {
    fn link_at(&self, col: u16, row: u16) -> Option<Rc<str>> {
        if !self.body.contains((col, row).into()) { return None; }
        let row = self.row + (row - self.body.y) as usize;
        let cell = (col - self.body.x) as usize;
        self.links.get(row)?.iter().find(|link| link.cells.contains(&cell)).map(|link| Rc::clone(&link.target))
    }
    fn update_text(&mut self, rows: Rc<[String]>, body: Rect) {
        if !Rc::ptr_eq(&self.text_rows, &rows) && self.text_rows != rows {
            self.anchor = None;
            self.cursor = (0, 0);
            self.selecting = false;
        }
        self.text_rows = rows;
        self.body = body;
    }

    fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then_some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    pub(crate) fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        let mut selected = String::new();
        for row in start.0..=end.0 {
            let text = self.text_rows.get(row)?;
            if row != start.0 { selected.push('\n'); }
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { text.len() };
            selected.push_str(text.get(from..to)?);
        }
        Some(selected)
    }

    pub(crate) fn select_all(&mut self) {
        if let Some(last) = self.text_rows.last() {
            self.anchor = Some((0, 0));
            self.cursor = (self.text_rows.len() - 1, last.len());
        }
    }

    fn byte_at_cell(text: &str, cell: usize) -> usize {
        let mut width = 0;
        for (byte, grapheme) in text.grapheme_indices(true) {
            width += grapheme.width();
            if width > cell { return byte; }
        }
        text.len()
    }

    fn point(&self, col: u16, row: u16) -> (usize, usize) {
        let row = self.row.saturating_add(row.saturating_sub(self.body.y) as usize)
            .min(self.text_rows.len().saturating_sub(1));
        let byte = self.text_rows.get(row).map_or(0, |text| {
            Self::byte_at_cell(text, col.saturating_sub(self.body.x) as usize)
        });
        (row, byte)
    }

    fn click(&mut self, col: u16, row: u16, extend: bool, count: u8) {
        let point = self.point(col, row);
        if extend {
            self.anchor.get_or_insert(self.cursor);
            self.cursor = point;
        } else {
            self.cursor = point;
            self.anchor = Some(point);
            if let Some(text) = self.text_rows.get(point.0) {
                if count >= 3 {
                    self.anchor = Some((point.0, 0));
                    self.cursor.1 = text.len();
                } else if count == 2 {
                    if let Some((byte, word)) = text.split_word_bound_indices()
                        .find(|(byte, word)| *byte <= point.1 && point.1 < byte + word.len()) {
                        self.anchor = Some((point.0, byte));
                        self.cursor.1 = byte + word.len();
                    }
                }
            }
        }
        self.selecting = true;
    }

    fn drag_selection(&mut self, col: u16, row: u16) {
        if row < self.body.y { self.scroll(-1); }
        else if row >= self.body.bottom() { self.scroll(1); }
        self.cursor = self.point(col, row.clamp(self.body.y, self.body.bottom().saturating_sub(1)));
    }

    fn paint_selection(&self, f: &mut Frame, color: Color) {
        if self.body.height == 0 { return; }
        let Some((start, end)) = self.selection() else { return };
        for row in start.0.max(self.row)..=end.0.min(self.row + self.body.height as usize - 1) {
            let Some(text) = self.text_rows.get(row) else { continue };
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { text.len() };
            let left = text[..from].width().min(self.body.width as usize);
            let right = if row < end.0 { self.body.width as usize } else { text[..to].width().min(self.body.width as usize) };
            for col in left..right {
                f.buffer_mut()[(self.body.x + col as u16, self.body.y + (row - self.row) as u16)].set_bg(color);
            }
        }
    }

    fn update(&mut self, source_rows: Rc<[usize]>, source: usize, height: usize) {
        if !Rc::ptr_eq(&self.source_rows, &source_rows) || self.source_scroll != source {
            self.row = mapped_viewport_start(&source_rows, source, height);
        }
        self.source_rows = source_rows;
        self.source_scroll = source;
        self.height = height;
        self.row = self.row.min(self.max_scroll());
    }

    fn max_scroll(&self) -> usize {
        self.source_rows.len().saturating_sub(self.height)
    }

    fn set_row(&mut self, row: usize) {
        self.row = row.min(self.max_scroll());
        let source = self.source_rows.get(self.row).copied().unwrap_or(0);
        self.source_scroll = source;
        self.pending_source = Some(source);
    }

    fn scroll(&mut self, delta: isize) {
        self.set_row(self.row.saturating_add_signed(delta));
    }

    fn thumb(&self) -> (usize, usize) {
        let size = (self.height.saturating_mul(self.height) / self.source_rows.len().max(1))
            .max(1)
            .min(self.height);
        let top =
            self.row.saturating_mul(self.height.saturating_sub(size)) / self.max_scroll().max(1);
        (top, size)
    }

    fn drag(&mut self, relative: usize) {
        let (_, size) = self.thumb();
        let travel = self.height.saturating_sub(size);
        let position = relative.saturating_sub(self.drag_offset).min(travel);
        self.set_row(position.saturating_mul(self.max_scroll()) / travel.max(1));
    }
}
thread_local! {
    // Bound retention by document, not by width: resizing must not evict parsed
    // Markdown and highlighted code. Rope identity also
    // catches file reloads whose content_version has been reset to zero.
    static PREVIEWS: RefCell<Vec<CachedPreview>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn draw_markdown_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(editor) = app.editor() else { return };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let colors = app.colors;
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(colors.bg_editor)),
        area,
    );
    let title_area = Rect::new(area.x, area.y, area.width, 1);
    let badge = "  MARKDOWN  ";
    let hint = "  Ctrl+Click · Link   Alt+V · Close  ";
    let hint_width = if area.width >= 60 {
        hint.width() as u16
    } else {
        0
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                badge,
                Style::default()
                    .fg(colors.fg_md_heading)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                editor.file_name().to_owned(),
                Style::default().fg(colors.fg_text),
            ),
        ]))
        .style(Style::default().bg(colors.bg_tabbar)),
        title_area,
    );
    if hint_width > 0 {
        f.render_widget(
            Paragraph::new(hint).style(Style::default().fg(colors.fg_dim).bg(colors.bg_tabbar)),
            Rect::new(area.right() - hint_width, area.y, hint_width, 1),
        );
    }
    if area.height <= 1 {
        return;
    }
    // A readable measure on large terminals, with graceful degradation in splits.
    let gutter = if area.width >= 40 {
        3
    } else if area.width >= 8 {
        1
    } else {
        0
    };
    let content_width = area.width.saturating_sub(1);
    let width = content_width.saturating_sub(gutter * 2).min(100);
    let body = Rect::new(
        area.x + (content_width - width) / 2,
        area.y + 1,
        width,
        area.height - 1,
    );
    let palette = Palette::from(&colors);
    let viewport = PREVIEWS.with_borrow_mut(|cache| {
        let hit = cache.iter().position(|entry| {
            entry.rope.is_instance(&editor.rope)
                && entry.palette == palette
                && entry.theme == app.syntax_theme
                && entry.registry == app.highlighter.registry_generation()
        });
        let index = if let Some(index) = hit {
            index
        } else {
            let source = editor.rope.to_string();
            let document = PreparedDocument::new(&source);
            let (rows, code_views) = document.layout(width as usize, palette, None);
            if cache.len() == 4 {
                cache.remove(0);
            }
            cache.push(CachedPreview {
                rope: editor.rope.clone(),
                width: width as usize,
                palette,
                theme: app.syntax_theme.clone(),
                registry: app.highlighter.registry_generation(),
                source_rows: source_row_map(&rows),
                text_rows: preview_text_rows(&rows),
                links: rows.iter().map(|row| row.links.clone()).collect(),
                rows,
                document,
                code_views,
            });
            cache.len() - 1
        };
        let entry = &mut cache[index];
        if entry.width != width as usize {
            (entry.rows, entry.code_views) = entry.document.layout(width as usize, palette, None);
            entry.source_rows = source_row_map(&entry.rows);
            entry.text_rows = preview_text_rows(&entry.rows);
            entry.links = entry.rows.iter().map(|row| row.links.clone()).collect();
            entry.width = width as usize;
        }
        let mut viewport = editor.markdown_preview_scroll.clone().unwrap_or_default();
        viewport.update_text(Rc::clone(&entry.text_rows), body);
        viewport.links = Rc::clone(&entry.links);
        viewport.anchors = Rc::clone(&entry.document.anchors);
        viewport.update(
            Rc::clone(&entry.source_rows),
            editor.scroll_offset,
            body.height as usize,
        );
        let start = viewport.row;
        let (syntaxes, themes) = app.highlighter.shared_sets();
        entry.highlight_visible(start..start + body.height as usize, (&syntaxes, themes.themes.get(&app.syntax_theme)));
        let visible: Vec<Line<'static>> = entry.rows
            .iter()
            .skip(start)
            .take(body.height as usize)
            .map(|row| row.line.clone())
            .collect();
        f.render_widget(
            Paragraph::new(visible).style(Style::default().fg(colors.fg_text)),
            body,
        );
        viewport.paint_selection(f, colors.bg_selection);
        viewport
    });
    let scrollbar = Rect::new(area.right() - 1, body.y, 1, body.height);
    let (top, size) = viewport.thumb();
    let (track_color, thumb_color) = colors.ui.scrollbar_colors(
        colors.bg_editor, scrollbar.contains((app.mouse_col, app.mouse_row).into()), app.dragging_v_scrollbar);
    let track = (0..body.height as usize)
        .map(|row| {
            let thumb = row >= top && row < top + size;
            Line::from(Span::styled(
                if thumb { "┃" } else { "│" },
                Style::default().fg(if thumb {
                    thumb_color
                } else {
                    track_color
                }),
            ))
        })
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(track), scrollbar);
    app.code_area = Some(Rect::new(area.x, body.y, content_width, body.height));
    app.scrollbar_area = Some(scrollbar);
    if let Some(editor) = app.editor_mut() {
        editor.markdown_preview_scroll = Some(viewport);
    }
}

#[derive(Debug)]
struct Node<'a> {
    event: Event<'a>,
    range: Range<usize>,
    children: Vec<Node<'a>>,
}

fn parse_nodes<'a>(events: &mut impl Iterator<Item = (Event<'a>, Range<usize>)>) -> Vec<Node<'a>> {
    let mut nodes = Vec::new();
    let mut stack: Vec<Node<'a>> = Vec::new();
    let mut flattened = 0;
    for (event, range) in events.by_ref() {
        let node = match event {
            Event::End(_) => {
                if flattened > 0 { flattened -= 1; continue; }
                let Some(node) = stack.pop() else { continue; };
                node
            }
            Event::Start(_) => {
                // Bound layout/drop recursion too. Deeply nested documents keep
                // their text even once additional indentation is unhelpful.
                if stack.len() >= 128 { flattened += 1; }
                else { stack.push(Node { event, range, children: vec![] }); }
                continue;
            }
            _ => Node { event, range, children: vec![] },
        };
        if let Some(parent) = stack.last_mut() { parent.children.push(node); }
        else { nodes.push(node); }
    }
    nodes
}

#[derive(Clone, Debug)]
struct Row {
    line: Line<'static>,
    source: usize,
    links: Vec<LinkSpan>,
}

#[derive(Clone, Debug)]
struct LinkSpan { cells: Range<usize>, target: Rc<str> }

fn append_link(links: &mut Vec<LinkSpan>, target: &Option<Rc<str>>, start: usize, width: usize) {
    let Some(target) = target else { return; };
    if width == 0 { return; }
    if let Some(last) = links.last_mut().filter(|link| link.target == *target && link.cells.end == start) {
        last.cells.end += width;
    } else { links.push(LinkSpan { cells: start..start + width, target: Rc::clone(target) }); }
}

/// Reuse the editor's CommonMark layout for streamed chat messages.
pub(crate) fn chat_lines(
    source: &str,
    width: usize,
    colors: &ThemeColors,
    syntax: Option<(&SyntaxRegistry, Option<&Theme>)>,
) -> Vec<Line<'static>> {
    chat_lines_on_surface(source, width, colors, syntax, colors.bg_sidebar)
}

/// Request bubbles and agent messages have different owning surfaces. Resolve
/// text and code layers before caching the laid-out message.
pub(crate) fn chat_lines_on_surface(
    source: &str,
    width: usize,
    colors: &ThemeColors,
    syntax: Option<(&SyntaxRegistry, Option<&Theme>)>,
    surface: Color,
) -> Vec<Line<'static>> {
    let palette = surface_palette(colors, surface);
    PreparedDocument::new(source).render(width, palette, syntax)
        .into_iter().map(|row| row.line).collect()
}

fn surface_palette(colors: &ThemeColors, surface: Color) -> Palette {
    let inline_bg = colors.ui.md_inline_background.over(surface).unwrap_or(surface);
    let quote_background = colors.ui.md_quote_background.over(surface);
    let quote_surface = quote_background.unwrap_or(surface);
    Palette {
        text: colors.ui.sidebar_text_color(surface),
        heading: if colors.ui.preserve_theme_colors { colors.ui.sidebar_text_color(surface) } else { colors.fg_md_heading },
        quote_background,
        quote_text: colors.ui.sidebar_text_color(quote_surface),
        quote: colors.ui.md_quote_border.over(quote_surface).unwrap_or(colors.fg_md_blockquote),
        code_bg: colors.ui.md_code_background.over(surface).unwrap_or(surface),
        inline_code: colors.ui.readable_foreground(colors.ui.md_inline_foreground.over(inline_bg).unwrap_or(colors.ui.fg_md_inline), inline_bg, 4.5),
        inline_bg,
        ..Palette::from(colors)
    }
}

/// Cached CommonMark for embedded documentation, reflowed only on width/theme changes.
pub(crate) struct EmbeddedMarkdown {
    document: PreparedDocument,
    cache: Option<(usize, Palette, Vec<Line<'static>>)>,
}

impl EmbeddedMarkdown {
    pub(crate) fn new(source: &str) -> Self {
        Self { document: PreparedDocument::new(source), cache: None }
    }

    pub(crate) fn lines(&mut self, width: usize, colors: &ThemeColors, surface: Color) -> &[Line<'static>] {
        let palette = surface_palette(colors, surface);
        self.render_lines(width, palette)
    }

    /// Embedded language help inherits its widget's foreground and surface.
    pub(crate) fn widget_lines(&mut self, width: usize, colors: &ThemeColors, surface: Color, foreground: Color) -> &[Line<'static>] {
        let mut palette = surface_palette(colors, surface);
        palette.text = foreground;
        palette.heading = foreground;
        palette.quote_text = foreground;
        palette.code = foreground;
        palette.code_bg = surface;
        palette.compact_code = true;
        self.render_lines(width, palette)
    }

    fn render_lines(&mut self, width: usize, palette: Palette) -> &[Line<'static>] {
        if self.cache.as_ref().is_none_or(|(w, p, _)| *w != width || *p != palette) {
            if self.cache.as_ref().is_some_and(|(_, previous, _)| *previous != palette) {
                // Prepared spans include colors. Width-only reflow can reuse
                // them; a theme or translucent surface change cannot.
                self.document.code.borrow_mut().clear();
                self.document.inline.borrow_mut().clear();
                self.document.highlighted.borrow_mut().clear();
            }
            self.cache = Some((width, palette, self.document.render(width, palette, None).into_iter().map(|row| row.line).collect()));
        }
        &self.cache.as_ref().unwrap().2
    }
}

fn preview_text_rows(rows: &[Row]) -> Rc<[String]> {
    rows.iter().map(|row| row.line.spans.iter().map(|span| span.content.as_ref()).collect()).collect()
}
#[derive(Clone)]
struct Piece {
    span: Span<'static>,
    source: usize,
    literal: bool,
    link: Option<Rc<str>>,
}

/// Width-independent content. Inline styles, code highlighting, and Unicode
/// word measurements are prepared once and shared by every subsequent reflow.
struct PreparedDocument {
    nodes: Vec<Node<'static>>,
    line_starts: Vec<usize>,
    source_lines: Vec<usize>,
    inline: RefCell<rustc_hash::FxHashMap<(usize, usize, Style), PreparedText>>,
    code: RefCell<rustc_hash::FxHashMap<usize, Rc<[CodeLine]>>>,
    diagrams: rustc_hash::FxHashMap<usize, diagrams::Diagram>,
    anchors: Rc<rustc_hash::FxHashMap<String, usize>>,
    code_paths: rustc_hash::FxHashMap<usize, Vec<usize>>,
    highlighted: RefCell<rustc_hash::FxHashSet<usize>>,
}

struct CodeLine {
    text: PreparedText,
    source: usize,
}

impl PreparedDocument {
    fn new(source: &str) -> Self {
        let (source, source_lines) = extensions::prepare(source);
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_GFM
            | Options::ENABLE_DEFINITION_LIST
            | Options::ENABLE_MATH
            | Options::ENABLE_SUPERSCRIPT
            | Options::ENABLE_SUBSCRIPT
            | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
            | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
        let nodes = parse_nodes(
            &mut Parser::new_ext(&source, options)
                .into_offset_iter()
                .map(|(event, range)| (event.into_static(), range)),
        );
        let mut line_starts = vec![0];
        line_starts.extend(source.match_indices('\n').map(|(offset, _)| offset + 1));
        fn collect_code(nodes: &[Node<'_>], path: &mut Vec<usize>, diagrams: &mut rustc_hash::FxHashMap<usize, diagrams::Diagram>, paths: &mut rustc_hash::FxHashMap<usize, Vec<usize>>) {
            for (index, node) in nodes.iter().enumerate() {
                path.push(index);
                if let Event::Start(Tag::CodeBlock(kind)) = &node.event {
                    paths.insert(node.range.start, path.clone());
                    if let CodeBlockKind::Fenced(info) = kind && info.split_whitespace().next() == Some("mermaid") {
                        let source: String = node.children.iter().filter_map(|node| match &node.event { Event::Text(text) => Some(text.as_ref()), _ => None }).collect();
                        if let Some(diagram) = diagrams::Diagram::parse(&source) { diagrams.insert(node.range.start, diagram); }
                    }
                } else { collect_code(&node.children, path, diagrams, paths); }
                path.pop();
            }
        }
        let mut diagrams = rustc_hash::FxHashMap::default();
        let mut code_paths = rustc_hash::FxHashMap::default();
        collect_code(&nodes, &mut vec![], &mut diagrams, &mut code_paths);
        fn anchors(nodes: &[Node<'_>], starts: &[usize], result: &mut rustc_hash::FxHashMap<String, usize>, suffixes: &mut rustc_hash::FxHashMap<String, usize>) {
            fn label(nodes: &[Node<'_>], out: &mut String) {
                for node in nodes {
                    match &node.event {
                        Event::Text(t) | Event::Code(t) => out.push_str(t),
                        Event::SoftBreak | Event::HardBreak => out.push(' '),
                        _ => label(&node.children, out),
                    }
                }
            }
            for node in nodes {
                let key = match &node.event {
                    Event::Start(Tag::Heading { id, .. }) => {
                        let mut text = String::new();
                        label(&node.children, &mut text);
                        let slug = id.as_ref().map_or_else(|| text.to_lowercase().chars().filter_map(|c| {
                            if c.is_whitespace() { Some('-') } else if c.is_alphanumeric() || matches!(c, '-' | '_') { Some(c) } else { None }
                        }).collect::<String>(), ToString::to_string);
                        let mut key = slug.clone();
                        let suffix = suffixes.entry(slug.clone()).or_default();
                        while result.contains_key(&key) { *suffix += 1; key = format!("{slug}-{suffix}"); }
                        Some(key)
                    }
                    Event::Start(Tag::FootnoteDefinition(label)) => Some(format!("fn-{label}")),
                    _ => None,
                };
                if let Some(key) = key { result.insert(key, starts.partition_point(|&offset| offset <= node.range.start).saturating_sub(1)); }
                anchors(&node.children, starts, result, suffixes);
            }
        }
        let mut anchor_map = rustc_hash::FxHashMap::default();
        anchors(&nodes, &line_starts, &mut anchor_map, &mut rustc_hash::FxHashMap::default());
        for line in anchor_map.values_mut() { *line = source_lines.get(*line).copied().unwrap_or(*line); }
        Self {
            nodes,
            line_starts,
            source_lines,
            inline: RefCell::default(),
            code: RefCell::default(),
            diagrams,
            anchors: Rc::new(anchor_map),
            code_paths,
            highlighted: RefCell::default(),
        }
    }

    fn render(
        &self,
        width: usize,
        palette: Palette,
        syntax: Option<(&SyntaxRegistry, Option<&Theme>)>,
    ) -> Vec<Row> {
        self.layout(width, palette, syntax).0
    }

    fn layout(&self, width: usize, palette: Palette, syntax: Option<(&SyntaxRegistry, Option<&Theme>)>) -> (Vec<Row>, Vec<CodeView>) {
        let mut renderer = Renderer {
            rows: Vec::new(),
            code_views: Vec::new(),
            width: width.max(1),
            palette,
            document: self,
            syntax,
        };
        renderer.gap(0, "");
        renderer.blocks(&self.nodes, "", Style::default().fg(palette.text));
        (renderer.rows, renderer.code_views)
    }
}

struct Renderer<'a> {
    rows: Vec<Row>,
    code_views: Vec<CodeView>,
    width: usize,
    palette: Palette,
    document: &'a PreparedDocument,
    syntax: Option<(&'a SyntaxRegistry, Option<&'a Theme>)>,
}

#[cfg(test)]
fn render_document(
    source: &str,
    width: usize,
    palette: Palette,
    syntax: Option<(&SyntaxRegistry, Option<&Theme>)>,
) -> Vec<Row> {
    PreparedDocument::new(source).render(width, palette, syntax)
}

fn source_row_map(rows: &[Row]) -> Rc<[usize]> {
    rows.iter()
        .scan(0, |last, row| {
            *last = (*last).max(row.source);
            Some(*last)
        })
        .collect()
}

#[cfg(test)]
fn viewport_start(rows: &[Row], source_line: usize, height: usize) -> usize {
    mapped_viewport_start(&source_row_map(rows), source_line, height)
}

fn mapped_viewport_start(rows: &[usize], source_line: usize, height: usize) -> usize {
    if source_line == 0 {
        return 0;
    }
    // Start at the first visual row for the closest preceding source line.
    // Wrapping, hidden delimiters, and decorative rows must not cause drift.
    let preceding = rows
        .partition_point(|&line| line <= source_line)
        .saturating_sub(1);
    let mapped = rows.get(preceding).copied().unwrap_or(0);
    rows.partition_point(|&line| line < mapped)
        .min(rows.len().saturating_sub(height))
}

impl Renderer<'_> {
    fn source_line(&self, offset: usize) -> usize {
        let line = self.document
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        self.document.source_lines.get(line).copied().unwrap_or(line)
    }
    fn piece(&self, text: impl Into<String>, style: Style, offset: usize) -> Piece {
        let text = text.into();
        let text = if text.chars().any(|ch| ch.is_control() && ch != '\n') { clean_text(&text) } else { text };
        Piece {
            span: Span::styled(text, style),
            source: self.source_line(offset),
            literal: false,
            link: None,
        }
    }
    fn gap(&mut self, source: usize, prefix: &str) {
        if self.rows.last().is_none_or(|row| {
            !row.line
                .spans
                .iter()
                .all(|span| span.content.trim().is_empty())
        }) {
            self.rows.push(Row {
                line: Line::from(Span::styled(
                    truncate_cells(prefix, self.width),
                    Style::default().fg(self.palette.quote),
                )),
                source,
                links: Vec::new(),
            });
        }
    }
    fn emit(
        &mut self,
        pieces: impl Into<PreparedText>,
        prefix: &str,
        continuation: &str,
        source: usize,
    ) {
        self.rows.extend(wrap_pieces(
            &pieces.into(),
            self.width,
            prefix,
            continuation,
            source,
            self.palette.quote,
            false,
        ));
    }
    fn rule(&mut self, prefix: &str, label: &str, fill: &str, style: Style, source: usize) {
        let prefix = truncate_cells(prefix, self.width.saturating_sub(1));
        let available = self.width.saturating_sub(prefix.width());
        let label = truncate_cells(label, available);
        let content = format!(
            "{label}{}",
            fill.repeat(available.saturating_sub(label.width()))
        );
        self.rows.push(Row {
            line: Line::from(vec![
                Span::styled(prefix, Style::default().fg(self.palette.quote)),
                Span::styled(content, style),
            ]),
            source,
            links: Vec::new(),
        });
    }
    fn inline(&self, nodes: &[Node<'_>], style: Style) -> PreparedText {
        let key = (
            nodes.first().map_or(0, |node| node.range.start),
            nodes.last().map_or(0, |node| node.range.end),
            style,
        );
        if let Some(text) = self.document.inline.borrow().get(&key) {
            return text.clone();
        }
        let text = PreparedText::from(self.inline_pieces(nodes, style));
        self.document.inline.borrow_mut().insert(key, text.clone());
        text
    }
    fn inline_pieces(&self, nodes: &[Node<'_>], style: Style) -> Vec<Piece> {
        self.inline_with_html(nodes, style, &mut markup::Html::default())
    }
    fn prose(&self, text: &str, style: Style, at: usize) -> Vec<Piece> {
        static LINKS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(
            r"(?i)\b(?:https?://|www\.)[^\s<>]+|\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b"
        ).unwrap());
        let mut pieces = Vec::new();
        let mut previous = 0;
        for link in LINKS.find_iter(text) {
            let mut end = link.end();
            let candidate = &text[link.start()..end];
            let mut excess = [
                candidate.matches(')').count().saturating_sub(candidate.matches('(').count()),
                candidate.matches(']').count().saturating_sub(candidate.matches('[').count()),
                candidate.matches('}').count().saturating_sub(candidate.matches('{').count()),
            ];
            while let Some(last) = text[link.start()..end].chars().last() {
                let bracket = match last { ')' => Some(0), ']' => Some(1), '}' => Some(2), _ => None };
                let unbalanced = bracket.is_some_and(|i| excess[i] > 0);
                if unbalanced { excess[bracket.unwrap()] -= 1; }
                if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"') || unbalanced { end -= last.len_utf8(); }
                else { break; }
            }
            if end == link.start() { continue; }
            if link.start() > previous { pieces.push(self.piece(&text[previous..link.start()], style, at)); }
            let label = &text[link.start()..end];
            let mut piece = self.piece(label, style.fg(self.palette.link).add_modifier(Modifier::UNDERLINED), at);
            piece.link = Some(Rc::from(if label.as_bytes().get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"www.")) { format!("https://{label}") }
                else if label.contains('@') && !label.contains("://") { format!("mailto:{label}") } else { label.into() }));
            pieces.push(piece);
            previous = end;
        }
        if previous < text.len() { pieces.push(self.piece(&text[previous..], style, at)); }
        pieces
    }
    fn inline_with_html(&self, nodes: &[Node<'_>], base: Style, html: &mut markup::Html) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut nodes = nodes.iter().peekable();
        while let Some(node) = nodes.next() {
            let at = node.range.start;
            let style = html.style(base);
            match &node.event {
                Event::Start(tag) => {
                    let styled = match tag {
                        Tag::Strong => style.add_modifier(Modifier::BOLD),
                        Tag::Emphasis => style.add_modifier(Modifier::ITALIC),
                        Tag::Strikethrough => style.add_modifier(Modifier::CROSSED_OUT),
                        Tag::Link { .. } => style
                            .fg(self.palette.link)
                            .add_modifier(Modifier::UNDERLINED),
                        Tag::Image { .. } => style.fg(self.palette.dim),
                        _ => style,
                    };
                    if let Tag::Image { .. } = tag {
                        out.push(self.piece("[Image: ", styled, at));
                    }
                    let mut children = self.inline_with_html(&node.children, styled, html);
                    if let Tag::Link { dest_url, .. } = tag {
                        let target: Rc<str> = Rc::from(dest_url.as_ref());
                        if children.is_empty() { children.push(self.piece(dest_url.to_string(), styled, at)); }
                        for piece in &mut children { piece.link = Some(Rc::clone(&target)); }
                    }
                    if matches!(tag, Tag::Superscript | Tag::Subscript) {
                        for piece in &mut children {
                            piece.span.content = markup::script(&piece.span.content, matches!(tag, Tag::Superscript)).into();
                        }
                    }
                    out.extend(children);
                    if let Tag::Image { dest_url, .. } = tag {
                        if node.children.is_empty() {
                            out.push(self.piece("image", styled, at));
                        }
                        out.push(self.piece(format!("] ({})", markup::image_destination(dest_url)), styled, at));
                    }
                }
                Event::Text(text) if html.visible() => {
                    // CommonMark may split one literal URL at an unmatched
                    // emphasis delimiter. Join adjacent text once before
                    // detecting links; otherwise only its prefix is clickable.
                    let mut text = std::borrow::Cow::Borrowed(text.as_ref());
                    while nodes.peek().is_some_and(|node| matches!(node.event, Event::Text(_))) {
                        if let Event::Text(next) = &nodes.next().unwrap().event { text.to_mut().push_str(next); }
                    }
                    if html.literal() { let mut piece = self.piece(html.text(&text), style, at); piece.literal = true; out.push(piece); }
                    else { out.extend(self.prose(&html.text(&text), style, at)); }
                }
                Event::Code(text) => {
                    let mut piece = self.piece(text.to_string(), style.fg(self.palette.inline_code).bg(self.palette.inline_bg), at);
                    piece.literal = true;
                    out.push(piece);
                }
                Event::SoftBreak => out.push(self.piece(" ", style, at)),
                Event::HardBreak => out.push(self.piece("\n", style, at)),
                Event::FootnoteReference(label) => {
                    let mut piece = self.piece(format!("[{label}]"), style.fg(self.palette.link).add_modifier(Modifier::UNDERLINED), at);
                    piece.link = Some(Rc::from(format!("#fn-{label}")));
                    out.push(piece);
                }
                Event::TaskListMarker(checked) => out.push(self.piece(
                    if *checked { "☑ " } else { "☐ " },
                    style.fg(if *checked {
                        self.palette.success
                    } else {
                        self.palette.dim
                    }),
                    at,
                )),
                Event::Html(text) | Event::InlineHtml(text) => {
                    out.extend(html.feed(text, base, self.palette).into_iter().map(|(text, style)| self.piece(text, style, at)));
                }
                Event::InlineMath(text) => {
                    let mut piece = self.piece(markup::math(text), style.fg(self.palette.code), at);
                    piece.literal = true;
                    out.push(piece);
                }
                Event::DisplayMath(text) => {
                    let mut piece = self.piece(format!("\n{}\n", markup::math(text).trim()), style.fg(self.palette.code), at);
                    piece.literal = true;
                    out.push(piece);
                }
                _ => {}
            }
        }
        out
    }
    fn blocks(&mut self, nodes: &[Node<'_>], prefix: &str, style: Style) {
        let mut index = 0;
        while index < nodes.len() {
            let node = &nodes[index];
            let source = self.source_line(node.range.start);
            match &node.event {
                Event::Start(Tag::Paragraph) => {
                    self.emit(self.inline(&node.children, style), prefix, prefix, source);
                    self.gap(source, prefix);
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    self.gap(source, prefix);
                    let mut heading_style = style.fg(self.palette.heading).add_modifier(Modifier::BOLD);
                    if matches!(level, HeadingLevel::H4 | HeadingLevel::H6) { heading_style = heading_style.add_modifier(Modifier::ITALIC); }
                    if matches!(level, HeadingLevel::H5 | HeadingLevel::H6) { heading_style = heading_style.fg(self.palette.dim); }
                    self.emit(
                        self.inline(&node.children, heading_style),
                        prefix,
                        prefix,
                        source,
                    );
                    if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                        let rule = if *level == HeadingLevel::H1 {
                            "━"
                        } else {
                            "─"
                        };
                        self.rule(
                            prefix,
                            "",
                            rule,
                            Style::default().fg(self.palette.heading),
                            source,
                        );
                    }
                    self.gap(source, prefix);
                }
                Event::Start(Tag::BlockQuote(kind)) => {
                    self.gap(source, prefix);
                    let quote_prefix = format!("{prefix}▎ ");
                    let color = match kind {
                        Some(pulldown_cmark::BlockQuoteKind::Tip) => self.palette.success,
                        Some(pulldown_cmark::BlockQuoteKind::Warning) => self.palette.warning,
                        Some(pulldown_cmark::BlockQuoteKind::Caution) => self.palette.error,
                        _ => self.palette.quote,
                    };
                    if let Some(kind) = kind {
                        self.emit(
                            vec![self.piece(
                                format!("{kind:?}").to_uppercase(),
                                style.fg(color).add_modifier(Modifier::BOLD),
                                node.range.start,
                            )],
                            &quote_prefix,
                            &quote_prefix,
                            source,
                        );
                    }
                    let body_style = if kind.is_some() {
                        style
                    } else if self.palette.contrast.is_none() {
                        style.fg(self.palette.quote_text)
                    } else {
                        style.fg(self.palette.quote).add_modifier(Modifier::ITALIC)
                    };
                    let start = self.rows.len();
                    self.blocks(&node.children, &quote_prefix, body_style);
                    if self.palette.contrast.is_none() && let Some(bg) = self.palette.quote_background {
                        for row in &mut self.rows[start..] {
                            row.line.style = row.line.style.bg(bg);
                        }
                    }
                    self.gap(source, prefix);
                }
                Event::Start(Tag::List(start)) => {
                    for (number, item) in node.children.iter().enumerate() {
                        let marker = start.map_or_else(
                            || "• ".to_owned(),
                            |n| format!("{}. ", n.saturating_add(number as u64)),
                        );
                        let item_prefix = format!("{prefix}{}", " ".repeat(marker.width()));
                        let first = self.rows.len();
                        if matches!(item.event, Event::Start(Tag::Item)) {
                            self.blocks(&item.children, &item_prefix, style);
                        } else {
                            // Containers beyond the nesting limit are flattened;
                            // their literal children must still remain visible.
                            self.blocks(std::slice::from_ref(item), &item_prefix, style);
                        }
                        if let Some(row) = self.rows.get_mut(first) {
                            // Replace only the hanging indent; nested blocks keep their own prefix.
                            if let Some(span) = row.line.spans.first_mut() {
                                span.content =
                                    truncate_cells(&format!("{prefix}{marker}"), span.width())
                                        .into();
                                span.style = Style::default().fg(self.palette.heading);
                            }
                        }
                    }
                    self.gap(source, prefix);
                }
                Event::Start(Tag::CodeBlock(kind)) => self.code(node, kind, prefix),
                Event::Start(Tag::MetadataBlock(_)) => {},
                Event::Start(Tag::HtmlBlock) => {
                    self.gap(source, prefix);
                    self.rows.extend(wrap_pieces(&self.inline(&node.children, style), self.width, prefix, prefix, source, self.palette.quote, true));
                    self.gap(source, prefix);
                }
                Event::Start(Tag::Table(alignments)) => self.table(node, alignments, prefix, style),
                Event::Start(Tag::FootnoteDefinition(label)) => {
                    self.emit(
                        vec![self.piece(
                            format!("[{label}]"),
                            style.fg(self.palette.link).add_modifier(Modifier::BOLD),
                            node.range.start,
                        )],
                        prefix,
                        prefix,
                        source,
                    );
                    self.blocks(&node.children, &format!("{prefix}  "), style);
                }
                Event::Start(Tag::DefinitionListTitle) => {
                    self.emit(
                        self.inline(&node.children, style.add_modifier(Modifier::BOLD)),
                        prefix,
                        prefix,
                        source,
                    );
                }
                Event::Start(Tag::DefinitionListDefinition) => {
                    self.blocks(&node.children, &format!("{prefix}  "), style)
                }
                Event::Start(Tag::DefinitionList | Tag::Item) => {
                    self.blocks(&node.children, prefix, style)
                }
                Event::Rule => {
                    self.gap(source, prefix);
                    self.rule(prefix, "", "─", style.fg(self.palette.dim), source);
                    self.gap(source, prefix);
                }
                _ => {
                    // Tight list items contain inline events directly (no Paragraph).
                    let first = index;
                    while index + 1 < nodes.len() && !is_block(&nodes[index + 1].event) {
                        index += 1;
                    }
                    self.emit(
                        self.inline(&nodes[first..=index], style),
                        prefix,
                        prefix,
                        source,
                    );
                }
            }
            index += 1;
        }
    }
    fn code(&mut self, node: &Node<'_>, kind: &CodeBlockKind<'_>, prefix: &str) {
        let source = self.source_line(node.range.start);
        if let Some(diagram) = self.document.diagrams.get(&node.range.start) {
            self.gap(source, prefix);
            self.rule(prefix, &format!("╭─ {} ", diagram.title()), "─", Style::default().fg(self.palette.heading), source);
            let width = self.width.saturating_sub(prefix.width()).max(1);
            for line in diagram.lines(width) {
                self.rows.push(Row { line: Line::from(vec![Span::raw(truncate_cells(prefix, self.width.saturating_sub(1))), Span::styled(line, Style::default().fg(self.palette.code))]), source, links: vec![] });
            }
            self.rule(prefix, "╰", "─", Style::default().fg(self.palette.dim), self.source_line(node.range.end.saturating_sub(1)));
            self.gap(self.source_line(node.range.end.saturating_sub(1)), prefix);
            return;
        }
        let language = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("text"),
            _ => "text",
        };
        let style = Style::default()
            .fg(self.palette.code)
            .bg(self.palette.code_bg);
        self.gap(source, prefix);
        let view_start = self.rows.len();
        if !self.palette.compact_code {
            self.rule(prefix, &format!("╭─ {language} "), "─", style.fg(self.palette.dim), source);
        }
        let cached = self.document.code.borrow().get(&node.range.start).cloned()
            .filter(|_| self.syntax.is_none() || self.document.highlighted.borrow().contains(&node.range.start));
        let lines = if let Some(lines) = cached {
            lines
        } else {
            let mut lines = Vec::new();
            let mut highlighter = self.syntax.and_then(|(syntaxes, theme)| {
                let syntax = syntaxes.find_syntax_by_token(language).or_else(|| {
                    syntaxes
                        .syntaxes()
                        .iter()
                        .find(|s| s.name.eq_ignore_ascii_case(language))
                })?;
                Some(HighlightLines::new(syntax, theme?))
            });
            let mut highlighted_bytes = 0usize;
            for child in &node.children {
                if let Event::Text(text) = &child.event {
                    let mut offset = child.range.start;
                    for (line_number, line) in text.split_inclusive('\n').enumerate() {
                        let line_source = self.source_line(child.range.start) + line_number;
                        let expanded = expand_code_tabs(line);
                        let content = expanded.trim_end_matches(['\n', '\r']);
                        highlighted_bytes += line.len();
                        // Regex highlighting of generated/minified fences can
                        // otherwise dominate opening a preview. After the limit
                        // keep complete literal text, including subsequent lines.
                        if line.len() > 4096 || highlighted_bytes > 262_144 { highlighter = None; }
                        let mut spans: Vec<Piece> = highlighter
                            .as_mut()
                            .map(|h| h.highlight_line(&expanded))
                            .map(|ranges| {
                                ranges
                                    .into_iter()
                                    .map(|(s, text)| {
                                        let foreground = crate::ui_colors::syntax_color(s.foreground, self.palette.code_bg);
                                        let mut token_style = style.fg(crate::ui_colors::ensure_contrast(foreground, self.palette.code_bg, 4.5, crate::ui_colors::contrast_colors(&self.palette.contrast)));
                                        token_style = token_style.add_modifier(crate::ui_colors::syntax_font_modifiers(s.font_style));
                                        self.piece(
                                            text.trim_end_matches(['\n', '\r']),
                                            token_style,
                                            offset,
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_else(|| {
                                vec![self.piece(content, style, offset)]
                            });
                        for piece in &mut spans {
                            piece.source = line_source;
                        }
                        lines.push(CodeLine {
                            text: spans.into(),
                            source: line_source,
                        });
                        offset += line.len();
                    }
                }
            }
            let lines: Rc<[CodeLine]> = lines.into();
            self.document
                .code
                .borrow_mut()
                .insert(node.range.start, Rc::clone(&lines));
            lines
        };
        if self.syntax.is_some() { self.document.highlighted.borrow_mut().insert(node.range.start); }
        let code_prefix = if self.palette.compact_code { prefix.to_owned() } else { format!("{prefix}│ ") };
        for line in lines.iter() {
            let first = self.rows.len();
            self.rows.extend(wrap_pieces(
                &line.text,
                self.width,
                &code_prefix,
                &code_prefix,
                line.source,
                self.palette.dim,
                true,
            ));
            for row in &mut self.rows[first..] {
                let pad = self.width.saturating_sub(row.line.width());
                row.line.spans.push(Span::styled(" ".repeat(pad), style));
                row.line.style = style;
            }
        }
        if !self.palette.compact_code {
            self.rule(prefix, "╰", "─", style.fg(self.palette.dim), self.source_line(node.range.end.saturating_sub(1)));
        }
        self.code_views.push(CodeView { block: node.range.start, rows: view_start..self.rows.len(), prefix: prefix.into() });
        self.gap(self.source_line(node.range.end.saturating_sub(1)), prefix);
    }
    fn table(&mut self, node: &Node<'_>, alignments: &[Alignment], prefix: &str, style: Style) {
        let source = self.source_line(node.range.start);
        let columns = alignments.len();
        if columns == 0 {
            return;
        }
        let available = self.width.saturating_sub(prefix.width());
        self.gap(source, prefix);
        let cells: Vec<Vec<PreparedText>> = node
            .children
            .iter()
            .map(|row| {
                row.children
                    .iter()
                    .map(|cell| {
                        self.inline(
                            &cell.children,
                            if matches!(row.event, Event::Start(Tag::TableHead)) {
                                style.fg(self.palette.heading).add_modifier(Modifier::BOLD)
                            } else {
                                style
                            },
                        )
                    })
                    .collect()
            })
            .collect();
        // When borders alone would consume the pane, use stacked labeled fields.
        if available < columns * 4 + 1 {
            for (row_index, row) in cells.iter().enumerate().skip(1) {
                for (column, cell) in row.iter().enumerate() {
                    let mut pieces = cells[0]
                        .get(column)
                        .map(|text| text.0.pieces.clone())
                        .unwrap_or_default();
                    pieces.push(self.piece(": ", style.fg(self.palette.dim), node.range.start));
                    pieces.extend(cell.0.pieces.clone());
                    self.emit(
                        pieces,
                        prefix,
                        prefix,
                        self.source_line(node.children[row_index].range.start),
                    );
                }
                self.gap(source, prefix);
            }
            if cells.len() == 1 {
                for cell in &cells[0] {
                    self.emit(cell.clone(), prefix, prefix, source);
                }
            }
            return;
        }
        let mut widths = vec![1; columns];
        for row in &cells {
            for (column, cell) in row.iter().enumerate().take(columns) {
                widths[column] = widths[column].max(cell.0.width);
            }
        }
        let budget = available - columns * 3 - 1;
        if widths.iter().sum::<usize>() > budget {
            let natural = widths;
            widths = vec![1; columns];
            let mut remaining = budget - columns;
            while remaining > 0 {
                for column in 0..columns {
                    if remaining > 0 && widths[column] < natural[column] {
                        widths[column] += 1;
                        remaining -= 1;
                    }
                }
            }
        }
        self.table_border(&widths, prefix, ['╭', '┬', '╮'], source);
        for (row_index, row) in cells.iter().enumerate() {
            let row_source = self.source_line(node.children[row_index].range.start);
            let wrapped: Vec<Vec<Row>> = (0..columns)
                .map(|column| {
                    wrap_pieces(
                        &row.get(column).cloned().unwrap_or_default(),
                        widths[column],
                        "",
                        "",
                        row_source,
                        self.palette.dim,
                        false,
                    )
                })
                .collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
            for y in 0..height {
                let mut links = Vec::new();
                let mut cell_start = prefix.width() + 1;
                let mut spans = vec![
                    Span::raw(prefix.to_owned()),
                    Span::styled("│", style.fg(self.palette.dim)),
                ];
                for column in 0..columns {
                    let line = wrapped[column]
                        .get(y)
                        .map(|r| r.line.clone())
                        .unwrap_or_default();
                    let padding = widths[column].saturating_sub(line.width());
                    let left = match alignments[column] {
                        Alignment::Right => padding,
                        Alignment::Center => padding / 2,
                        _ => 0,
                    };
                    if let Some(row) = wrapped[column].get(y) {
                        links.extend(row.links.iter().map(|link| LinkSpan { cells: link.cells.start + cell_start + left + 1..link.cells.end + cell_start + left + 1, target: Rc::clone(&link.target) }));
                    }
                    cell_start += widths[column] + 3;
                    spans.push(Span::raw(" ".repeat(left + 1)));
                    spans.extend(line.spans);
                    spans.push(Span::raw(" ".repeat(padding - left + 1)));
                    spans.push(Span::styled("│", style.fg(self.palette.dim)));
                }
                let mut line = Line::from(spans);
                if row_index == 0 {
                    line.style = Style::default().bg(self.palette.code_bg);
                }
                self.rows.push(Row {
                    line,
                    source: row_source,
                    links,
                });
            }
            if row_index == 0 {
                self.table_border(&widths, prefix, ['├', '┼', '┤'], row_source);
            }
        }
        self.table_border(
            &widths,
            prefix,
            ['╰', '┴', '╯'],
            self.source_line(node.range.end.saturating_sub(1)),
        );
        self.gap(self.source_line(node.range.end.saturating_sub(1)), prefix);
    }
    fn table_border(&mut self, widths: &[usize], prefix: &str, chars: [char; 3], source: usize) {
        let body = widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join(&chars[1].to_string());
        self.rows.push(Row {
            line: Line::from(Span::styled(
                format!("{prefix}{}{body}{}", chars[0], chars[2]),
                Style::default().fg(self.palette.dim),
            )),
            source,
            links: vec![],
        });
    }
}

fn is_block(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::Rule
            | Event::Start(
                Tag::Paragraph
                    | Tag::Heading { .. }
                    | Tag::BlockQuote(_)
                    | Tag::CodeBlock(_)
                    | Tag::List(_)
                    | Tag::Table(_)
                    | Tag::HtmlBlock
                    | Tag::FootnoteDefinition(_)
                    | Tag::DefinitionList
                    | Tag::DefinitionListTitle
                    | Tag::DefinitionListDefinition
                    | Tag::MetadataBlock(_)
            )
    )
}

fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .map(|ch| if ch == '\t' { ' ' } else { ch })
        .collect()
}

fn expand_code_tabs(text: &str) -> String {
    if !text.contains('\t') { return text.to_owned(); }
    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    for glyph in text.graphemes(true) {
        if glyph == "\t" {
            let spaces = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else { out.push_str(glyph); if glyph.contains('\n') { column = 0; } else { column += glyph.width(); } }
    }
    out
}
fn truncate_cells(text: &str, width: usize) -> String {
    let mut used = 0;
    text.graphemes(true)
        .take_while(|g| {
            used += g.width();
            used <= width
        })
        .collect()
}

/// The text and its measured word fragments are shared across pane widths.
/// Fragments borrow by byte range from owned pieces, avoiding a String/Style
/// allocation per character and repeated Unicode segmentation during resizing.
#[derive(Clone, Default)]
struct PreparedText(Rc<TextData>);
#[derive(Default)]
struct TextData {
    pieces: Vec<Piece>,
    words: Vec<Word>,
    width: usize,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WordKind {
    Text,
    Space,
    Newline,
}
struct Word {
    fragments: Vec<Fragment>,
    width: usize,
    kind: WordKind,
}
struct Fragment {
    piece: usize,
    bytes: Range<usize>,
    width: usize,
}

impl From<Vec<Piece>> for PreparedText {
    fn from(pieces: Vec<Piece>) -> Self {
        let mut words: Vec<Word> = Vec::new();
        let mut width = 0;
        for (piece_index, piece) in pieces.iter().enumerate() {
            for (start, glyph) in piece.span.content.grapheme_indices(true) {
                let kind = if glyph == "\n" {
                    WordKind::Newline
                } else if glyph.chars().all(char::is_whitespace) {
                    WordKind::Space
                } else {
                    WordKind::Text
                };
                if kind == WordKind::Newline || words.last().is_none_or(|word| word.kind != kind) {
                    words.push(Word {
                        fragments: Vec::new(),
                        width: 0,
                        kind,
                    });
                }
                let word = words.last_mut().unwrap();
                let size = glyph.width();
                let end = start + glyph.len();
                if let Some(fragment) = word
                    .fragments
                    .last_mut()
                    .filter(|f| f.piece == piece_index && f.bytes.end == start)
                {
                    fragment.bytes.end = end;
                    fragment.width += size;
                } else {
                    word.fragments.push(Fragment {
                        piece: piece_index,
                        bytes: start..end,
                        width: size,
                    });
                }
                word.width += size;
                width += size;
            }
        }
        Self(Rc::new(TextData {
            pieces,
            words,
            width,
        }))
    }
}

fn append_fragment(line: &mut Line<'static>, text: &str, style: Style, merge: bool) {
    if let Some(last) = line
        .spans
        .last_mut()
        .filter(|last| merge && last.style == style)
    {
        last.content.to_mut().push_str(text);
    } else {
        line.spans.push(Span::styled(text.to_owned(), style));
    }
}

/// Reflow small language-help paragraphs without losing active parameter styles.
pub(crate) fn wrap_help_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    lines.into_iter().flat_map(|line| {
        let pieces = line.spans.into_iter().map(|span| Piece { span, source: 0, literal: true, link: None }).collect::<Vec<_>>();
        wrap_pieces(&PreparedText::from(pieces), width.max(1), "", "", 0, Color::Reset, true)
            .into_iter().map(|row| row.line)
    }).collect()
}

/// Reflow already measured text. The common path copies whole styled words;
/// only words wider than the pane need to be split into grapheme clusters.
#[allow(clippy::too_many_arguments)]
fn wrap_pieces(
    text: &PreparedText,
    width: usize,
    first: &str,
    continuation: &str,
    source: usize,
    prefix_color: Color,
    preserve_spaces: bool,
) -> Vec<Row> {
    let width = width.max(1);
    let prefix = truncate_cells(first, width / 2);
    let continuation = truncate_cells(continuation, width / 2);
    let mut line = Line::from(Span::styled(
        prefix.clone(),
        Style::default().fg(prefix_color),
    ));
    let mut used = prefix.width();
    let mut indent = used;
    let mut row_source = source;
    let mut rows = Vec::new();
    let mut links = Vec::new();
    for word in &text.0.words {
        let whitespace = word.kind == WordKind::Space;
        let newline = word.kind == WordKind::Newline;
        if newline || (!whitespace && used > indent && used + word.width > width) {
            rows.push(Row {
                line,
                source: row_source,
                links: std::mem::take(&mut links),
            });
            line = Line::from(Span::styled(
                continuation.clone(),
                Style::default().fg(prefix_color),
            ));
            used = continuation.width();
            indent = used;
            row_source = text.0.pieces[word.fragments[0].piece].source;
            if newline {
                continue;
            }
        }
        if !preserve_spaces && whitespace && used == indent && word.fragments.iter().all(|fragment| !text.0.pieces[fragment.piece].literal) {
            continue;
        }
        if !preserve_spaces && whitespace && word.fragments.iter().all(|fragment| !text.0.pieces[fragment.piece].literal) {
            if used < width {
                append_fragment(&mut line, " ", text.0.pieces[word.fragments[0].piece].span.style, used > indent);
                append_link(&mut links, &text.0.pieces[word.fragments[0].piece].link, used, 1);
                used += 1;
            }
            continue;
        }
        for fragment in &word.fragments {
            let piece = &text.0.pieces[fragment.piece];
            let content = &piece.span.content[fragment.bytes.clone()];
            if used + fragment.width <= width {
                if used == indent {
                    row_source = piece.source;
                }
                append_fragment(&mut line, content, piece.span.style, used > indent);
                append_link(&mut links, &piece.link, used, fragment.width);
                used += fragment.width;
                continue;
            }
            // Long code/URLs and whitespace crossing a row boundary.
            for grapheme in content.graphemes(true) {
                let size = grapheme.width();
                if !preserve_spaces && whitespace && used + size > width {
                    continue;
                }
                if used + size > width {
                    rows.push(Row {
                        line,
                        source: row_source,
                        links: std::mem::take(&mut links),
                    });
                    line = Line::from(Span::styled(
                        continuation.clone(),
                        Style::default().fg(prefix_color),
                    ));
                    used = continuation.width();
                    indent = used;
                    row_source = piece.source;
                }
                if used == indent {
                    row_source = piece.source;
                }
                if size > width.saturating_sub(used) {
                    continue;
                }
                append_fragment(&mut line, grapheme, piece.span.style, used > indent);
                append_link(&mut links, &piece.link, used, size);
                used += size;
            }
        }
    }
    rows.push(Row {
        line,
        source: row_source,
        links,
    });
    rows
}

fn preview_link_url(target: &str, path: Option<&std::path::Path>, remote: Option<&crate::remote::workspace::Workspace>) -> Option<url::Url> {
    if let Ok(uri) = url::Url::parse(target) { return Some(uri); }
    let base = if let Some(remote) = remote {
        let local = url::Url::from_file_path(path?).ok()?;
        url::Url::parse(&remote.lsp_uri(local.as_str(), true)?).ok()?
    } else {
        url::Url::from_file_path(std::path::absolute(path?).ok()?).ok()?
    };
    // Resolve ../ against the node directory, never a synthetic client view key.
    base.join(target).ok()
}

impl App {
    pub(crate) fn markdown_preview_link_click(&mut self, col: u16, row: u16) -> bool {
        let Some(target) = self.editor().and_then(|ed| ed.markdown_preview_scroll.as_ref()).and_then(|view| view.link_at(col, row)) else { return false; };
        if let Some(anchor) = target.strip_prefix('#') {
            let anchor = decode_fragment(anchor);
            let editor = self.editor_mut().unwrap();
            let view = editor.markdown_preview_scroll.as_mut().unwrap();
            if let Some(source) = if anchor.is_empty() { Some(0) } else { view.anchors.get(&anchor).copied() } {
                let row = mapped_viewport_start(&view.source_rows, source, view.height);
                view.set_row(row);
                view.clear_selection();
                view.selecting = false;
                editor.scroll_offset = view.source_scroll;
                self.sync_markdown_preview_state();
            } else { self.notify_warn(format!("Markdown heading not found: {anchor}")); }
            return true;
        }
        if target.chars().any(char::is_control) { self.notify_warn("Invalid Markdown link"); return true; }
        let url = preview_link_url(&target, self.editor().and_then(|ed| ed.file_path.as_deref()), self.remote_workspace.as_ref());
        match url.as_ref().map(url::Url::scheme) {
            Some("http" | "https" | "mailto" | "tel") => self.open_external(crate::external_open::Request::Url(url.unwrap().to_string())),
            Some("file") => {
                let url = url.unwrap();
                let path = self.remote_workspace.as_ref().map_or_else(|| url.to_file_path().ok(), |remote| remote.file_uri_key(url.as_str()));
                if let Some(path) = path {
                    if self.remote_workspace.is_some() {
                        self.open_remote_location(path, None, crate::app_remote_navigation::Source::Markdown);
                    } else { self.push_nav_history(); self.open_file(&path); }
                }
                else { self.notify_warn("Invalid file link"); }
            }
            _ => self.notify_warn("This Markdown link cannot be opened"),
        }
        true
    }
    pub(crate) fn markdown_preview_selection_click(&mut self, col: u16, row: u16, extend: bool) {
        if !self.code_area.is_some_and(|area| area.contains((col, row).into())) { return; }
        let now = std::time::Instant::now();
        self.click_count = if now.duration_since(self.last_click_time).as_millis() < 400
            && self.last_click_pos == (col, row) { (self.click_count + 1).min(3) } else { 1 };
        self.last_click_time = now;
        self.last_click_pos = (col, row);
        let count = self.click_count;
        if let Some(view) = self.editor_mut().and_then(|ed| ed.markdown_preview_scroll.as_mut()) {
            view.click(col, row, extend, count);
        }
    }

    pub(crate) fn markdown_preview_selection_drag(&mut self, col: u16, row: u16) -> bool {
        let Some(editor) = self.editor_mut() else { return false };
        let Some(view) = &mut editor.markdown_preview_scroll else { return false };
        if view.selecting {
            view.drag_selection(col, row);
            editor.scroll_offset = view.source_scroll;
            self.sync_markdown_preview_state();
        }
        true
    }

    pub(crate) fn markdown_preview_selection_key(&mut self, key: crossterm::event::KeyCode, modifiers: crossterm::event::KeyModifiers) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        if self.active_pane != crate::app::ActivePane::Editor || modifiers.contains(KeyModifiers::ALT) { return false; }
        let Some(editor) = self.editor_mut() else { return false };
        let Some(view) = &mut editor.markdown_preview_scroll else { return false };
        if key == KeyCode::Esc {
            view.anchor = None;
            return true;
        }
        if !modifiers.contains(KeyModifiers::SHIFT) { return false; }
        let Some(text) = view.text_rows.get(view.cursor.0) else { return false };
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let mut next = view.cursor;
        match key {
            KeyCode::Left => {
                if next.1 > 0 {
                    next.1 = text[..next.1].grapheme_indices(true).next_back().map_or(0, |(byte, _)| byte);
                } else if next.0 > 0 {
                    next.0 -= 1;
                    next.1 = view.text_rows[next.0].len();
                }
            }
            KeyCode::Right => {
                if next.1 < text.len() {
                    next.1 += text[next.1..].graphemes(true).next().map_or(0, str::len);
                } else if next.0 + 1 < view.text_rows.len() { next = (next.0 + 1, 0); }
            }
            KeyCode::Home => { next = if ctrl { (0, 0) } else { (next.0, 0) }; }
            KeyCode::End => {
                if ctrl { next.0 = view.text_rows.len() - 1; }
                next.1 = view.text_rows[next.0].len();
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let amount = if matches!(key, KeyCode::PageUp | KeyCode::PageDown) { view.height.max(1) } else { 1 };
                next.0 = if matches!(key, KeyCode::Up | KeyCode::PageUp) { next.0.saturating_sub(amount) }
                    else { (next.0 + amount).min(view.text_rows.len() - 1) };
                next.1 = PreviewScroll::byte_at_cell(&view.text_rows[next.0], text[..view.cursor.1].width());
            }
            _ => return false,
        }
        view.anchor.get_or_insert(view.cursor);
        view.cursor = next;
        if next.0 < view.row { view.set_row(next.0); }
        else if next.0 >= view.row + view.height { view.set_row(next.0 + 1 - view.height); }
        editor.scroll_offset = view.source_scroll;
        self.sync_markdown_preview_state();
        true
    }

    /// Wheel input is routed by pane geometry, without stealing keyboard focus.
    pub(crate) fn scroll_markdown_preview_at(&mut self, col: u16, row: u16, delta: isize) -> bool {
        let point = ratatui::layout::Position::new(col, row);
        let mut editor = if self.code_area.is_some_and(|area| area.contains(point))
            || self.scrollbar_area.is_some_and(|area| area.contains(point))
        {
            self.tabs
                .get_mut(self.active_tab)
                .filter(|tab| tab.kind == crate::app::TabKind::MarkdownPreview)
                .map(|tab| &mut tab.editor)
        } else {
            None
        };
        if editor.is_none() {
            editor = self
                .groups
                .iter_mut()
                .enumerate()
                .filter(|(index, _)| *index != self.active_group)
                .find_map(|(_, group)| {
                    if group.code_area.is_some_and(|area| area.contains(point))
                        || group
                            .scrollbar_area
                            .is_some_and(|area| area.contains(point))
                    {
                        group
                            .tabs
                            .get_mut(group.active_tab)
                            .filter(|tab| tab.kind == crate::app::TabKind::MarkdownPreview)
                            .map(|tab| &mut tab.editor)
                    } else {
                        None
                    }
                });
        }
        let Some(editor) = editor else { return false };
        if let Some(view) = &mut editor.markdown_preview_scroll {
            view.scroll(delta);
            editor.scroll_offset = view.source_scroll;
        }
        self.sync_markdown_preview_state();
        true
    }

    pub(crate) fn markdown_preview_scrollbar_input(&mut self, row: u16, begin: bool) -> bool {
        let Some(area) = self.scrollbar_area else {
            return false;
        };
        let Some(tab) = self
            .tabs
            .get_mut(self.active_tab)
            .filter(|tab| tab.kind == crate::app::TabKind::MarkdownPreview)
        else {
            return false;
        };
        if let Some(view) = &mut tab.editor.markdown_preview_scroll {
            let relative = row
                .saturating_sub(area.y)
                .min(area.height.saturating_sub(1)) as usize;
            if begin {
                let (top, size) = view.thumb();
                if (top..top + size).contains(&relative) {
                    view.drag_offset = relative - top;
                    return true;
                }
                view.drag_offset = size / 2;
            }
            view.drag(relative);
            tab.editor.scroll_offset = view.source_scroll;
        }
        self.sync_markdown_preview_state();
        true
    }

    pub(crate) fn markdown_preview_navigation(&mut self, key: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode;
        let Some(tab) = self
            .tabs
            .get_mut(self.active_tab)
            .filter(|tab| tab.kind == crate::app::TabKind::MarkdownPreview)
        else {
            return false;
        };
        if !matches!(
            key,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
        ) {
            return false;
        }
        if let Some(view) = &mut tab.editor.markdown_preview_scroll {
            match key {
                KeyCode::Up => view.scroll(-1),
                KeyCode::Down => view.scroll(1),
                KeyCode::PageUp => view.scroll(-(view.height.max(1) as isize)),
                KeyCode::PageDown => view.scroll(view.height.max(1) as isize),
                KeyCode::Home => view.set_row(0),
                KeyCode::End => view.set_row(view.max_scroll()),
                _ => {}
            }
            tab.editor.scroll_offset = view.source_scroll;
        }
        self.sync_markdown_preview_state();
        true
    }
}

fn decode_fragment(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let (Some(a), Some(b)) = ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16)) {
            out.push((a * 16 + b) as u8); i += 3;
        } else { out.push(bytes[i]); i += 1; }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_theme::EditorTheme;

    fn render(source: &str, width: usize) -> Vec<Row> {
        render_document(
            source,
            width,
            Palette::from(&EditorTheme::Dark.colors()),
            None,
        )
    }
    fn text(rows: &[Row]) -> String {
        rows.iter()
            .map(|row| {
                row.line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn styled<'a>(rows: &'a [Row], content: &str) -> &'a Span<'static> {
        rows.iter()
            .flat_map(|row| &row.line.spans)
            .find(|span| span.content.contains(content))
            .unwrap()
    }

    #[test]
    fn nested_inline_formatting_links_entities_and_escapes() {
        let rows = render(
            "**bold and *italic***, ~~gone~~, ``a ` b``, [**docs**][ref], <https://example.com>, \\*literal\\* &amp; snake_case_name\n\n[ref]: https://example.com",
            120,
        );
        let output = text(&rows);
        assert!(
            styled(&rows, "italic")
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
        );
        assert!(
            styled(&rows, "gone")
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
        assert!(
            styled(&rows, "docs")
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );
        assert!(output.contains("a ` b"));
        assert!(output.contains("*literal* & snake_case_name"));
        assert!(!output.contains("[ref]:"));
    }

    #[test]
    fn task_lists_nest_and_keep_hanging_indentation() {
        let rows = render(
            "- [x] **Complete**\n- [ ] Pending with a long description that wraps\n  - *Nested*\n\n7. Seven\n8. Eight",
            28,
        );
        let output = text(&rows);
        assert!(output.contains("• ☑ Complete"), "{output}");
        assert!(output.contains("• ☐ Pending"), "{output}");
        assert!(output.contains("  • Nested"), "{output}");
        assert!(output.contains("7. Seven"));
        assert!(output.contains("8. Eight"));
        assert!(rows.iter().all(|row| row.line.width() <= 28));
    }

    #[test]
    fn headings_setext_rules_and_callouts() {
        let rows = render(
            "Title\n=====\n\n## Section *two*\n\n> [!WARNING]\n> **Careful** with `code`.\n>\n> > Nested quote\n\n- - -\n\n###### Small",
            40,
        );
        let output = text(&rows);
        assert!(output.contains("━"));
        assert!(output.contains("WARNING"));
        assert!(output.contains("▎ ▎ Nested quote"), "{output}");
        assert!(
            styled(&rows, "Careful")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            styled(&rows, "two")
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
        );
        assert!(!output.contains("######"));
    }

    #[test]
    fn code_fences_preserve_indentation_and_literal_markdown() {
        let rows = render(
            "~~~rust\nfn main() {\n    let text = \"**literal**\";\n\t// comment\n}\n~~~\n\n    indented code",
            50,
        );
        let output = text(&rows);
        assert!(output.contains("╭─ rust"));
        assert!(
            output.contains("│     let text = \"**literal**\";"),
            "{output}"
        );
        assert!(output.contains("│     // comment"), "{output}");
        assert!(output.contains("│ indented code"));
        assert!(rows.iter().all(|row| row.line.width() <= 50));
    }

    #[test]
    fn table_alignment_unicode_and_narrow_fallback() {
        let source = "| Feature | State | Count |\n| :--- | :---: | ---: |\n| **Tables** | ready | 42 |\n| 世界 | yes | 7 |";
        let rows = render(source, 50);
        let output = text(&rows);
        assert!(output.contains("╭"));
        assert!(output.contains("│ 世界    │  yes  │     7 │"), "{output}");
        assert!(
            styled(&rows, "Tables")
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        for width in 1..45 {
            let rows = render(source, width);
            assert!(
                rows.iter().all(|row| row.line.width() <= width),
                "width {width}: {}",
                text(&rows)
            );
        }
        let narrow = text(&render(source, 12));
        assert!(narrow.contains("Count: 42"), "{narrow}");
    }

    #[test]
    fn footnotes_images_and_hard_breaks() {
        let rows = render(
            "First  \nSecond[^note]\n\n![A **diagram**](diagram.png)\n\n[^note]: Footnote with *emphasis*.",
            60,
        );
        let output = text(&rows);
        assert!(output.contains("First\nSecond[note]"), "{output}");
        assert!(output.contains("[Image: A diagram] (diagram.png)"));
        assert!(output.contains("Footnote with emphasis."));
    }

    #[test]
    fn source_mapping_survives_wrapping_and_added_rows() {
        let rows = render(
            "# Title\n\nA paragraph long enough to wrap into several visual rows.\n\n## Next\n\nTarget paragraph\n",
            20,
        );
        let start = viewport_start(&rows, 6, 2);
        assert!(
            text(&rows[start..]).starts_with("Target paragraph"),
            "{}",
            text(&rows[start..])
        );
        assert_eq!(viewport_start(&rows, 0, 2), 0);
    }

    #[test]
    fn wrapping_keeps_punctuation_with_styled_words() {
        let rows = render("before **word**:", 11);
        assert!(text(&rows).contains("before \nword:"));
    }

    #[test]
    fn unicode_and_small_panes_do_not_overflow() {
        let source = "# 界面 👩‍💻\n\n- **café e\u{301}** and 👩‍💻 plus https://example.com/averylongpath\n  > 深い引用\n\n```\n    世界\n```";
        for width in 1..45 {
            let rows = render(source, width);
            assert!(
                rows.iter().all(|row| row.line.width() <= width),
                "width {width}: {}",
                text(&rows)
            );
        }
        let output = text(&render(source, 18));
        assert!(output.contains("👩‍💻"));
        assert!(output.contains("e\u{301}"));
    }

    #[test]
    fn unclosed_fences_and_terminal_controls_are_safe() {
        let rows = render("# unfinished\n\n```unknown\nhello\u{1b}[31m\n**raw**", 24);
        let output = text(&rows);
        assert!(!output.contains('\u{1b}'));
        assert!(output.contains("**raw**"));
        assert!(output.contains('╰'));
    }

    #[test]
    fn showcase_in_terminal_buffer() {
        let source = include_str!("../examples/markdown-preview.md");
        let highlighter = crate::highlight::Highlighter::new();
        let (syntaxes, themes) = highlighter.shared_sets();
        for theme in [EditorTheme::Dark, EditorTheme::Light] {
            let palette = Palette::from(&theme.colors());
            let rows = render_document(
                source,
                76,
                palette,
                Some((&syntaxes, themes.themes.get(theme.syntax_theme_key()))),
            );
            assert_ne!(
                styled(&rows, "fn").style.fg,
                styled(&rows, "Hello").style.fg
            );
            let backend = ratatui::backend::TestBackend::new(80, 55);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        Paragraph::new(
                            rows.iter()
                                .take(55)
                                .map(|row| row.line.clone())
                                .collect::<Vec<_>>(),
                        )
                        .style(
                            Style::default()
                                .fg(palette.text)
                                .bg(theme.colors().bg_editor),
                        ),
                        Rect::new(2, 0, 76, 55),
                    );
                })
                .unwrap();
            assert!(rows.iter().all(|row| row.line.width() <= 76));
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "☑")
            );
            if let Some(directory) = std::env::var_os("VTCODE_MARKDOWN_SNAPSHOT") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| {
                    let color = |c: Color| match c { Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"), _ => "#202020".to_owned() };
                    serde_json::json!({"text":cell.symbol(), "fg":color(cell.fg), "bg":color(cell.bg),
                        "bold":cell.modifier.contains(Modifier::BOLD), "italic":cell.modifier.contains(Modifier::ITALIC),
                        "underline":cell.modifier.contains(Modifier::UNDERLINED)})
                }).collect();
                let name = if theme == EditorTheme::Dark {
                    "dark"
                } else {
                    "light"
                };
                std::fs::write(
                    directory.join(format!("{name}.json")),
                    serde_json::to_string(&cells).unwrap(),
                )
                .unwrap();
            }
        }
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;
    use crate::app::{SplitDirection, SplitNode, Tab, TabKind};
    use crate::editor::Editor;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn split_preview() -> (App, Terminal<TestBackend>) {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        let source = (0..80)
            .map(|i| {
                format!(
                    "## Section {i}\n\n{}\n\n",
                    "A long paragraph with **styled words**. ".repeat(12)
                )
            })
            .collect::<String>();
        editor.rope = ropey::Rope::from_str(&source);
        editor.file_path = Some("scroll-preview.md".into());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.show_panel = false;
        app.show_terminal_sidebar = false;
        app.toggle_markdown_preview_tab();
        // Preview normally opens in the current pane; exercise an explicit
        // user-created split here to retain coverage of cross-pane scrolling.
        app.create_split_group_dir(app.active_tab, SplitDirection::Horizontal);
        let mut terminal = Terminal::new(TestBackend::new(150, 36)).unwrap();
        draw(&mut app, &mut terminal);
        (app, terminal)
    }

    fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
        app.sync_markdown_preview_state();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    }

    #[test]
    fn preview_mouse_selection_paints_and_copies_rendered_text_without_editing_source() {
        let (mut app, mut terminal) = split_preview();
        let source = "# Heading\n\nHello **bold** [world](https://example.com).\n\nSecond paragraph.\n";
        let editor = &mut app.groups[0].tabs[0].editor;
        editor.rope = ropey::Rope::from_str(source);
        editor.content_version += 1;
        draw(&mut app, &mut terminal);
        let view = app.editor().unwrap().markdown_preview_scroll.as_ref().unwrap();
        let index = view.text_rows.iter().position(|text| text == "Hello bold world.").unwrap();
        let x = view.body.x;
        let y = view.body.y + (index - view.row) as u16;
        // Focus the source first: clicking the preview must route into that pane.
        app.switch_group(0);
        draw(&mut app, &mut terminal);
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for (kind, column) in [(MouseEventKind::Down(MouseButton::Left), x + 6),
            (MouseEventKind::Drag(MouseButton::Left), x + 16),
            (MouseEventKind::Up(MouseButton::Left), x + 16)] {
            crate::mouse::handle_mouse(&mut app, MouseEvent { kind, column, row: y, modifiers: KeyModifiers::NONE });
        }
        assert_eq!(app.tabs[app.active_tab].kind, TabKind::MarkdownPreview);
        draw(&mut app, &mut terminal);
        let view = app.editor().unwrap().markdown_preview_scroll.as_ref().unwrap();
        assert_eq!(view.selected_text().as_deref(), Some("bold world"));
        assert!(!view.selecting);
        assert!(!app.is_menu_cmd_disabled("copy"));
        assert!(app.is_menu_cmd_disabled("cut"));
        assert!(app.is_menu_cmd_disabled("paste"));
        assert_eq!(terminal.backend().buffer()[(x + 6, y)].bg, app.colors.bg_selection);
        assert_ne!(terminal.backend().buffer()[(x + 5, y)].bg, app.colors.bg_selection);
        crate::input::handle_editor_key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.editor().unwrap().clipboard, "bold world");
        for key in [KeyCode::Char('z'), KeyCode::Backspace, KeyCode::Delete, KeyCode::Enter] {
            crate::input::handle_editor_key(&mut app, key, KeyModifiers::NONE);
        }
        assert_eq!(app.editor().unwrap().rope.to_string(), source);
        assert_eq!(app.groups[0].tabs[0].editor.rope.to_string(), source);
        assert!(!app.editor().unwrap().modified);
        crate::input::handle_editor_key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        let selected = app.editor().unwrap().markdown_preview_scroll.as_ref().unwrap().selected_text().unwrap();
        assert!(selected.contains("Hello bold world."));
        assert!(selected.contains("Second paragraph."));
        assert!(!selected.contains("**"));
        assert!(!selected.contains("https://"));
        crate::input::handle_editor_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.editor().unwrap().markdown_preview_scroll.as_ref().unwrap().selected_text().is_none());
    }

    #[test]
    fn preview_selection_handles_unicode_reverse_drag_and_layout_changes() {
        let mut view = PreviewScroll::default();
        let rows: Rc<[String]> = vec!["a界e\u{301}z".into(), "next".into()].into();
        view.update_text(Rc::clone(&rows), Rect::new(10, 5, 20, 4));
        view.click(14, 5, false, 1);
        view.drag_selection(11, 5);
        assert_eq!(view.selected_text().as_deref(), Some("界e\u{301}"));
        // A theme redraw with identical text preserves the selection.
        view.update_text(rows.iter().cloned().collect(), view.body);
        assert_eq!(view.selected_text().as_deref(), Some("界e\u{301}"));
        view.click(10, 5, false, 1);
        view.drag_selection(12, 6);
        assert_eq!(view.selected_text().as_deref(), Some("a界e\u{301}z\nne"));
        // Edits or reflow must not retain stale row/byte offsets.
        view.update_text(vec!["short".into()].into(), view.body);
        assert!(view.selected_text().is_none());
        view.click(12, 5, false, 2);
        assert_eq!(view.selected_text().as_deref(), Some("short"));
    }

    #[test]
    fn preview_keyboard_selection_and_drag_scrolling_use_rendered_rows() {
        let (mut app, mut terminal) = split_preview();
        let view = app.editor_mut().unwrap().markdown_preview_scroll.as_mut().unwrap();
        let index = view.text_rows.iter().position(|text| !text.is_empty()).unwrap();
        view.click(view.body.x, view.body.y + index as u16, false, 1);
        crate::input::handle_editor_key(&mut app, KeyCode::End, KeyModifiers::SHIFT);
        let view = app.editor().unwrap().markdown_preview_scroll.as_ref().unwrap();
        assert_eq!(view.selected_text().as_deref(), Some(view.text_rows[index].as_str()));
        let (x, y) = (view.body.x + 2, view.body.bottom());
        app.markdown_preview_selection_drag(x, y);
        let view = app.editor().unwrap().markdown_preview_scroll.as_ref().unwrap();
        assert_eq!(view.row, 1);
        assert!(view.cursor.0 >= view.height);
        let selected = view.selected_text();
        draw(&mut app, &mut terminal);
        assert_eq!(app.editor().unwrap().markdown_preview_scroll.as_ref().unwrap().selected_text(), selected);
    }

    #[test]
    fn manually_split_preview_has_its_own_scrollbar_and_closes_cleanly() {
        let (mut app, terminal) = split_preview();
        assert_eq!(app.active_group, 1);
        assert_eq!(app.tabs[app.active_tab].kind, TabKind::MarkdownPreview);
        assert_eq!(app.groups[0].tabs[0].kind, TabKind::File);
        assert!(matches!(
            app.split_tree,
            Some(SplitNode::Split {
                direction: SplitDirection::Horizontal,
                ..
            })
        ));
        let source_bar = app.groups[0].scrollbar_area.unwrap();
        let preview_bar = app.scrollbar_area.unwrap();
        assert!(source_bar.x < app.code_area.unwrap().x);
        assert!(preview_bar.x > source_bar.x);
        if let Some(directory) = std::env::var_os("VTCODE_MARKDOWN_SNAPSHOT") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| {
                let color = |c: Color| match c { Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"), _ => "#202020".to_owned() };
                serde_json::json!({"text":cell.symbol(), "fg":color(cell.fg), "bg":color(cell.bg),
                    "bold":cell.modifier.contains(Modifier::BOLD), "italic":cell.modifier.contains(Modifier::ITALIC),
                    "underline":cell.modifier.contains(Modifier::UNDERLINED)})
            }).collect();
            std::fs::write(
                directory.join("split.json"),
                serde_json::to_string(&cells).unwrap(),
            )
            .unwrap();
        }
        app.toggle_markdown_preview_tab();
        assert!(!app.split_active());
        assert!(!app.markdown_preview);
        assert_eq!(app.tabs[app.active_tab].kind, TabKind::File);
    }

    #[test]
    fn wheel_on_inactive_preview_syncs_source_without_losing_visual_position() {
        let (mut app, mut terminal) = split_preview();
        app.switch_group(0);
        draw(&mut app, &mut terminal);
        let area = app.groups[1].code_area.unwrap();
        for _ in 0..4 {
            crate::mouse_scroll::handle_scroll_down(
                &mut app,
                area.x + 1,
                area.y + 1,
                KeyModifiers::NONE,
            );
        }
        let view = app.groups[1].tabs[0]
            .editor
            .markdown_preview_scroll
            .as_ref()
            .unwrap();
        let top = view.row;
        assert_eq!(top, app.scroll_speed * 4);
        assert_eq!(app.editor().unwrap().scroll_offset, view.source_rows[top]);
        draw(&mut app, &mut terminal);
        let view = app.groups[1].tabs[0]
            .editor
            .markdown_preview_scroll
            .as_ref()
            .unwrap();
        assert_eq!(
            view.row, top,
            "rendering must not snap back to the start of the paragraph"
        );
        assert_eq!(
            app.active_group, 0,
            "hover scrolling must preserve source focus"
        );
    }

    #[test]
    fn wheel_on_inactive_source_repositions_preview() {
        let (mut app, mut terminal) = split_preview();
        let source_area = app.groups[0].code_area.unwrap();
        crate::mouse_scroll::handle_scroll_down(
            &mut app,
            source_area.x + 1,
            source_area.y + 1,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        let source_line = app.groups[0].tabs[0].editor.scroll_offset;
        let view = app
            .editor()
            .unwrap()
            .markdown_preview_scroll
            .as_ref()
            .unwrap();
        assert!(source_line > 0);
        assert_eq!(view.source_scroll, source_line);
        assert_eq!(
            view.row,
            mapped_viewport_start(&view.source_rows, source_line, view.height)
        );
    }

    #[test]
    fn preview_scrollbar_drag_reaches_both_ends_and_syncs_wrapped_source() {
        let (mut app, mut terminal) = split_preview();
        app.groups[0].tabs[0].editor.word_wrap = true;
        draw(&mut app, &mut terminal);
        let bar = app.scrollbar_area.unwrap();
        crate::mouse_selection::handle_editor_area_left_click(
            &mut app,
            bar.x,
            bar.y,
            KeyModifiers::NONE,
        );
        assert!(app.dragging_v_scrollbar);
        crate::mouse_drag::handle_left_drag(
            &mut app,
            bar.x,
            bar.bottom() + 5,
            bar.x,
            bar.y,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        let view = app
            .editor()
            .unwrap()
            .markdown_preview_scroll
            .as_ref()
            .unwrap();
        assert_eq!(view.row, view.max_scroll());
        assert_eq!(
            app.groups[0].tabs[0].editor.scroll_offset,
            view.source_scroll
        );
        assert!(app.groups[0].tabs[0].editor.wrap_scroll_offset > 0);
        crate::mouse_drag::handle_left_drag(
            &mut app,
            bar.x,
            0,
            bar.x,
            bar.bottom(),
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(
            app.editor()
                .unwrap()
                .markdown_preview_scroll
                .as_ref()
                .unwrap()
                .row,
            0
        );
        assert_eq!(app.groups[0].tabs[0].editor.scroll_offset, 0);
    }

    #[test]
    fn keyboard_pages_preview_and_source_edits_refresh_it() {
        let (mut app, mut terminal) = split_preview();
        let height = app
            .editor()
            .unwrap()
            .markdown_preview_scroll
            .as_ref()
            .unwrap()
            .height;
        crate::input::handle_editor_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        draw(&mut app, &mut terminal);
        assert_eq!(
            app.editor()
                .unwrap()
                .markdown_preview_scroll
                .as_ref()
                .unwrap()
                .row,
            height
        );
        let source = &mut app.groups[0].tabs[0].editor;
        source.rope = ropey::Rope::from_str("# Updated\n\nShort content.");
        source.content_version += 1;
        source.scroll_offset = 0;
        draw(&mut app, &mut terminal);
        assert_eq!(
            app.editor()
                .unwrap()
                .markdown_preview_scroll
                .as_ref()
                .unwrap()
                .row,
            0
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "# Updated\n\nShort content."
        );
    }

    #[test]
    fn scrolling_a_wrapped_paragraph_preserves_all_intermediate_rows() {
        let mut view = PreviewScroll::default();
        let mapping: Rc<[usize]> = vec![0, 2, 2, 2, 2, 2, 2, 2, 4, 5].into();
        view.update(Rc::clone(&mapping), 0, 3);
        for row in 1..=7 {
            view.scroll(1);
            view.update(Rc::clone(&mapping), view.source_scroll, 3);
            assert_eq!(view.row, row);
        }
        view.update(mapping, 0, 3);
        assert_eq!(view.row, 0);
    }

    #[test]
    fn preview_opens_in_source_pane_and_reuses_its_tab_at_the_current_scroll_position() {
        let (mut app, mut terminal) = split_preview();
        app.switch_group(0);
        let other_preview = app.groups[1].tabs[0].editor.navigation_id;
        let source = app.tabs[0].editor.navigation_id;
        app.toggle_markdown_preview_tab();
        assert_eq!(app.active_group, 0);
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[0].editor.navigation_id, source);
        assert_eq!(app.groups[1].tabs[0].editor.navigation_id, other_preview);
        let preview = app.editor().unwrap().navigation_id;
        app.active_tab = 0;
        app.editor_mut().unwrap().scroll_offset = 40;
        app.editor_mut().unwrap().cursor_line = 40;
        app.toggle_markdown_preview_tab();
        assert_eq!(app.active_group, 0);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.editor().unwrap().navigation_id, preview);
        assert_eq!(app.editor().unwrap().scroll_offset, 40);
        draw(&mut app, &mut terminal);
        assert_eq!(
            app.editor()
                .unwrap()
                .markdown_preview_scroll
                .as_ref()
                .unwrap()
                .source_scroll,
            40
        );
        assert_eq!(app.groups.len(), 2);
    }

    #[test]
    fn clicking_an_inactive_preview_scrollbar_focuses_and_scrolls_that_pane() {
        let (mut app, mut terminal) = split_preview();
        app.switch_group(0);
        draw(&mut app, &mut terminal);
        let bar = app.groups[1].scrollbar_area.unwrap();
        let y = bar.y + bar.height / 2;
        crate::mouse_selection::switch_focus_if_split(&mut app, bar.x, y, true);
        crate::mouse_selection::handle_editor_area_left_click(
            &mut app,
            bar.x,
            y,
            KeyModifiers::NONE,
        );
        draw(&mut app, &mut terminal);
        assert_eq!(app.active_group, 1);
        assert!(
            app.editor()
                .unwrap()
                .markdown_preview_scroll
                .as_ref()
                .unwrap()
                .row
                > 0
        );
        assert!(app.groups[0].tabs[0].editor.scroll_offset > 0);
    }
}

#[cfg(test)]
mod resize_bench {
    use super::*;
    use std::{hint::black_box, time::Instant};

    #[test]
    #[ignore = "manual resize performance measurement"]
    fn markdown_resize_benchmark() {
        let source = include_str!("../examples/markdown-preview.md").repeat(40);
        let highlighter = crate::highlight::Highlighter::new();
        let (syntaxes, themes) = highlighter.shared_sets();
        let palette = Palette::from(&crate::app_theme::EditorTheme::Dark.colors());
        let syntax = Some((&*syntaxes, themes.themes.get("dark-plus")));
        let widths = [40, 48, 56, 64, 72, 80, 88, 96];
        let document = PreparedDocument::new(&source);
        black_box(document.render(76, palette, syntax));
        let start = Instant::now();
        for width in widths {
            black_box(document.render(width, palette, syntax));
        }
        println!(
            "markdown resize: {} bytes, {} widths, {:.2} ms/width",
            source.len(),
            widths.len(),
            start.elapsed().as_secs_f64() * 1000.0 / widths.len() as f64
        );
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;
    use crate::{app::Tab, app_theme::EditorTheme, editor::Editor};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn cached_reflow_matches_fresh_rendering_at_every_width() {
        let source = format!(
            "{}\n\n> ## Nested heading\n>\n> ```rust\n> let 世界 = 42;\n> ```\n",
            include_str!("../examples/markdown-preview.md")
        );
        let highlighter = crate::highlight::Highlighter::new();
        let (syntaxes, themes) = highlighter.shared_sets();
        for theme in [EditorTheme::Dark, EditorTheme::Light] {
            let palette = Palette::from(&theme.colors());
            let syntax = Some((&*syntaxes, themes.themes.get(theme.syntax_theme_key())));
            let document = PreparedDocument::new(&source);
            for width in [76, 40, 18, 8, 1, 100, 53] {
                assert_eq!(
                    format!("{:?}", document.render(width, palette, syntax)),
                    format!("{:?}", render_document(&source, width, palette, syntax)),
                    "width {width}"
                );
            }
        }
    }

    fn app_and_terminal() -> (App, Terminal<TestBackend>) {
        PREVIEWS.with_borrow_mut(Vec::clear);
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.rope = ropey::Rope::from_str(include_str!("../examples/markdown-preview.md"));
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        (app, Terminal::new(TestBackend::new(120, 30)).unwrap())
    }

    fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>, width: u16, height: u16) {
        terminal
            .draw(|frame| draw_markdown_preview(frame, app, Rect::new(0, 0, width, height)))
            .unwrap();
    }

    #[test]
    fn resize_reuses_parsed_document_highlights_and_word_measurements() {
        let (mut app, mut terminal) = app_and_terminal();
        draw(&mut app, &mut terminal, 80, 25);
        let (nodes, code, inline, source_rows) = PREVIEWS.with_borrow(|cache| {
            let entry = &cache[0];
            (
                entry.document.nodes.as_ptr(),
                entry.document.code.borrow().clone(),
                entry.document.inline.borrow().clone(),
                Rc::clone(&entry.source_rows),
            )
        });
        assert!(!code.is_empty());
        assert!(!inline.is_empty());
        // Height-only changes need neither parsing nor reflow.
        draw(&mut app, &mut terminal, 80, 19);
        PREVIEWS.with_borrow(|cache| assert!(Rc::ptr_eq(&source_rows, &cache[0].source_rows)));
        for width in [40, 45, 50, 55, 60, 65, 70, 75, 90] {
            draw(&mut app, &mut terminal, width, 25);
            PREVIEWS.with_borrow(|cache| {
                assert_eq!(
                    cache.len(),
                    1,
                    "different widths must share one document entry"
                );
                assert_eq!(
                    cache[0].document.nodes.as_ptr(),
                    nodes,
                    "resize reparsed Markdown"
                );
                for (key, lines) in &code {
                    assert!(
                        Rc::ptr_eq(lines, &cache[0].document.code.borrow()[key]),
                        "resize reran highlighting"
                    );
                }
                for (key, text) in &inline {
                    assert!(
                        Rc::ptr_eq(&text.0, &cache[0].document.inline.borrow()[key].0),
                        "resize remeasured text"
                    );
                }
            });
        }
    }

    #[test]
    fn document_edits_palette_and_syntax_registry_invalidate_preparation() {
        let (mut app, mut terminal) = app_and_terminal();
        draw(&mut app, &mut terminal, 80, 25);
        // A reload can replace the rope while retaining content_version zero.
        app.editor_mut().unwrap().rope =
            ropey::Rope::from_str("# Reloaded\n\n```rust\nlet x = 1;\n```\n");
        draw(&mut app, &mut terminal, 80, 25);
        PREVIEWS.with_borrow(|cache| {
            assert_eq!(cache.len(), 2);
            assert!(!cache[0].rope.is_instance(&cache[1].rope));
        });
        app.colors.fg_md_code = Color::Magenta;
        draw(&mut app, &mut terminal, 80, 25);
        PREVIEWS.with_borrow(|cache| assert_eq!(cache.len(), 3));
        app.highlighter
            .register_theme(&app.syntax_theme, Theme::new("test","#000000","#ffffff",&[]).unwrap());
        draw(&mut app, &mut terminal, 80, 25);
        PREVIEWS.with_borrow(|cache| {
            assert_eq!(cache.len(), 4);
            assert_ne!(cache[2].registry, cache[3].registry);
        });
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "R")
        );
    }
}
