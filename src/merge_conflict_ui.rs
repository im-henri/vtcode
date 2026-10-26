//! VS Code-style inline conflict actions and an optional merge-editor layout.
//! Every decision edits the existing shared result document.
use crate::{
    app::{ActivePane, App, TabKind},
    merge_conflicts::{Index, Review},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
};
use ropey::Rope;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub area: Rect,
    pub line: usize,
    pub action: &'static str,
    pub version: u64,
}

#[derive(Default)]
pub(crate) struct Chrome {
    pub area: Rect,
    pub preview: Rect,
    pub buttons: Vec<Hit>,
    pub version: u64,
}

/// Geometry belongs to each view, including inactive splits. The two source
/// panes own their pointer and cannot start a selection in the result below.
pub(crate) fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) -> bool {
    use crossterm::event::{MouseButton, MouseEventKind};
    let position = (mouse.column, mouse.row).into();
    let active = app.active_group;
    let desk = app.desks.active;
    let active_tab = app.active_tab;
    if mouse.kind == MouseEventKind::Moved {
        let mut changed = false;
        for tab in app.tabs.get_mut(active_tab).into_iter().chain(
            app.groups
                .iter_mut()
                .enumerate()
                .filter(|(group, entry)| *group != active && entry.desk == desk)
                .filter_map(|(_, group)| group.tabs.get_mut(group.active_tab)),
        ) {
            let editor = &mut tab.editor;
            let chrome = editor
                .merge_conflicts
                .review
                .as_ref()
                .map(|review| &review.chrome);
            let hover = chrome
                .into_iter()
                .flat_map(|chrome| &chrome.buttons)
                .chain(&editor.ai_review.merge_hits)
                .find(|hit| hit.version == editor.content_version && hit.area.contains(position))
                .map(|hit| hit.area);
            changed |= editor.merge_conflicts.hover != hover;
            editor.merge_conflicts.hover = hover;
        }
        if changed {
            app.mouse_hover_changed = true;
            app.hovered_editor_pos = None;
            app.hovered_gutter_line = None;
            return true;
        }
    }
    let target = app
        .tabs
        .get(active_tab)
        .map(|tab| (active, active_tab, tab))
        .into_iter()
        .chain(
            app.groups
                .iter()
                .enumerate()
                .filter(|(group, entry)| *group != active && entry.desk == desk)
                .filter_map(|(id, group)| {
                    group
                        .tabs
                        .get(group.active_tab)
                        .map(|tab| (id, group.active_tab, tab))
                }),
        )
        .find_map(|(group, _, tab)| {
            let review = tab.editor.merge_conflicts.review.as_ref()?;
            review.chrome.area.contains(position).then(|| {
                (
                    group,
                    review.chrome.version,
                    review.chrome.preview.contains(position),
                    review
                        .chrome
                        .buttons
                        .iter()
                        .find(|hit| hit.area.contains(position))
                        .cloned(),
                )
            })
        });
    let Some((group, version, preview, hit)) = target else {
        return false;
    };
    if matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
        return false;
    }
    if let MouseEventKind::Down(button) = mouse.kind {
        crate::ui_panels::capture_pointer(app, button);
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
        || preview
            && matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
    {
        if group != app.active_group {
            app.switch_group(group);
        }
        app.active_pane = ActivePane::Editor;
        app.terminal_panel_focused = false;
        if app
            .editor()
            .is_some_and(|editor| editor.content_version == version)
        {
            if let Some(hit) = hit.filter(|_| mouse.kind == MouseEventKind::Down(MouseButton::Left))
            {
                if matches!(hit.action, "source_up" | "source_down") {
                    source_scroll(app, hit.action == "source_down");
                } else {
                    app.merge_action(
                        hit.action,
                        if matches!(
                            hit.action,
                            "next_conflict" | "previous_conflict" | "close_merge_editor"
                        ) {
                            None
                        } else {
                            Some(hit.line)
                        },
                    );
                }
            } else if mouse.kind == MouseEventKind::ScrollUp
                || mouse.kind == MouseEventKind::ScrollDown
            {
                source_scroll(app, mouse.kind == MouseEventKind::ScrollDown);
            }
        }
    }
    app.hovered_editor_pos = None;
    app.hovered_gutter_line = None;
    true
}

pub(crate) struct Comparison {
    pub view: u64,
    pub source: Rope,
    pub incoming: Rope,
    pub base: Arc<str>,
    pub lines: Vec<(char, String)>,
    pub cache: crate::app::DiffRenderCache,
}

impl App {
    pub(crate) fn prepare_merge_conflicts(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            Self::schedule_merge_conflicts(tab, &mut self.background);
        }
    }

    /// Queue analysis during frame preparation, never from paint. Visible
    /// inactive panes are included; hidden tabs and desks do no conflict work.
    pub(crate) fn prepare_visible_merge_conflicts(&mut self) {
        self.prepare_merge_conflicts();
        for (id, group) in self.groups.iter_mut().enumerate() {
            if id != self.active_group && group.desk == self.desks.active {
                if let Some(tab) = group.tabs.get_mut(group.active_tab) {
                    Self::schedule_merge_conflicts(tab, &mut self.background);
                }
            }
        }
    }

    fn schedule_merge_conflicts(
        tab: &mut crate::app::Tab,
        background: &mut crate::background::BackgroundWorkers,
    ) {
        if tab.kind != TabKind::File {
            return;
        }
        let editor = &mut tab.editor;
        if editor.bg_loading
            || editor.load_failed
            || editor.encoding == "Binary"
            || editor.merge_conflicts.pending
            || editor.merge_conflicts.current(&editor.rope).is_some()
        {
            return;
        }
        let source = editor.rope.clone();
        let previous = editor.merge_conflicts.index.clone();
        let view = editor.navigation_id;
        let path = editor
            .file_path
            .clone()
            .unwrap_or_else(|| "[Untitled]".into());
        editor.merge_conflicts.pending = true;
        let sender = background.sender.clone();
        background.read_for_file("merge-conflicts", &path, false, move || {
            let index = Arc::new(Index::update(previous.as_deref(), source));
            let _ =
                sender.send(crate::background::BackgroundResult::MergeConflicts { view, index });
        });
    }

    pub(crate) fn merge_action(&mut self, action: &str, requested: Option<usize>) {
        let Some(editor) = self.editor_mut() else {
            return;
        };
        if action == "close_merge_editor" {
            editor.merge_conflicts.review = None;
            return;
        }
        let Some(index) = editor.merge_conflicts.current(&editor.rope) else {
            self.prepare_merge_conflicts();
            self.notify("Checking merge conflicts…");
            return;
        };
        let regions = &index.regions;
        if regions.is_empty() {
            self.notify("No complete merge conflicts in this file");
            return;
        }
        if requested.is_some_and(|start| !regions.iter().any(|region| region.start == start)) {
            self.notify("The conflict changed. Review its updated controls before accepting.");
            return;
        }
        let region = requested
            .and_then(|start| regions.iter().find(|region| region.start == start).copied())
            .or_else(|| index.region_for_line(editor.cursor_line))
            .unwrap_or_else(|| {
                regions[regions
                    .partition_point(|r| r.end < editor.cursor_line)
                    .min(regions.len() - 1)]
            });
        let region = match action {
            "next_conflict" => regions
                .iter()
                .find(|r| r.start > editor.cursor_line)
                .copied()
                .unwrap_or(regions[0]),
            "previous_conflict" => regions
                .iter()
                .rev()
                .find(|r| r.start < editor.cursor_line)
                .copied()
                .unwrap_or(*regions.last().unwrap()),
            _ => region,
        };
        match action {
            "current" | "incoming" | "both" => {
                editor.resolve_conflict(region.start, action);
                self.sync_duplicate_file_tabs();
            }
            "next_conflict" | "previous_conflict" | "merge_editor" => {
                editor.cursor_line = region.start;
                editor.cursor_col = 0;
                editor.selection_anchor = None;
                editor.unfold_all();
                if action == "merge_editor" || editor.merge_conflicts.review.is_some() {
                    editor.merge_conflicts.review = Some(Review {
                        source: editor.rope.clone(),
                        region,
                        scroll: 0,
                        chrome: Default::default(),
                    });
                }
                editor.ensure_visible();
                self.active_pane = ActivePane::Editor;
            }
            "compare" => {
                if editor.merge_conflicts.comparing {
                    return;
                }
                editor.merge_conflicts.comparing = true;
                let view = editor.navigation_id;
                let source = editor.rope.clone();
                let path = editor
                    .file_path
                    .clone()
                    .unwrap_or_else(|| "[Untitled]".into());
                let sender = self.background.sender.clone();
                self.background
                    .read_for_file("merge-compare", &path, true, move || {
                        let current = source
                            .slice(crate::merge_conflicts::line_chars(
                                &source,
                                region.current(),
                            ))
                            .to_string();
                        let incoming: Rope = source
                            .slice(crate::merge_conflicts::line_chars(
                                &source,
                                region.incoming(),
                            ))
                            .into();
                        let lines = crate::app::compute_file_diff(&current, &incoming.to_string());
                        let cache = crate::app::DiffRenderCache::new(&lines);
                        let _ = sender.send(crate::background::BackgroundResult::MergeComparison(
                            Comparison {
                                view,
                                source,
                                incoming,
                                base: current.into(),
                                lines,
                                cache,
                            },
                        ));
                    });
            }
            _ => {}
        }
    }

    pub(crate) fn apply_merge_comparison(&mut self, comparison: Comparison) {
        let active = self.editor().map(|editor| editor.navigation_id);
        let editor = self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .find(|tab| tab.editor.navigation_id == comparison.view)
            .map(|tab| &mut tab.editor);
        let Some(editor) = editor else {
            return;
        };
        editor.merge_conflicts.comparing = false;
        if active != Some(comparison.view) {
            return;
        }
        if !editor.rope.is_instance(&comparison.source) {
            self.notify("The conflict changed while comparing. Choose Compare Changes again.");
            return;
        }
        let path = editor.file_path.clone();
        let language = editor.language_name().to_owned();
        let markers = crate::app_git_display::target_line_markers_from_compare_diff(
            &comparison.lines,
            comparison.incoming.len_lines(),
        );
        self.create_snapshot_diff_tab();
        let limit = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
        let editor = self.editor_mut().unwrap();
        editor.rope = comparison.incoming;
        editor.saved_rope = Some(editor.rope.clone());
        editor.file_size = editor.rope.len_bytes() as u64;
        editor.update_large_file_mode(limit);
        editor.mark_syntax_dirty_range(0, editor.rope.len_lines());
        editor.language_override = Some(language);
        editor.file_path = path.clone();
        self.compare_base_content = Some(comparison.base);
        self.compare_base_path = None;
        self.compare_diff_base_revision = Some(crate::app::DiffBaseRevision::Snapshot);
        self.diff_labels = Some(crate::app::DiffLabels::new(
            "Current Change",
            "Incoming Change",
        ));
        self.diff_target_path = path;
        self.diff_tab_label = Some("Merge Conflict (Compare Changes)".into());
        self.diff_mode = crate::app::DiffMode::SideBySide;
        self.apply_prepared_compare_diff(comparison.lines, comparison.cache, markers);
    }
}

pub(crate) fn content_background(
    editor: &crate::editor::Editor,
    line: usize,
    colors: &crate::app_theme::ThemeColors,
    surface: ratatui::style::Color,
) -> ratatui::style::Color {
    editor
        .merge_conflicts
        .current(&editor.rope)
        .and_then(|index| index.region_for_line(line))
        .and_then(|region| region.content_kind(line))
        .and_then(|kind| colors.ui.merge_contents[kind].over(surface))
        .unwrap_or(surface)
}

fn action_layout(width: u16, footer: bool) -> Vec<(u16, u16, &'static str, &'static str)> {
    if footer {
        return vec![(0, 0, "Resolve in Merge Editor", "merge_editor")];
    }
    let full = width >= 88;
    let labels = if full {
        [
            "Accept Current Change",
            "Accept Incoming Change",
            "Accept Both Changes",
            "Compare Changes",
        ]
    } else {
        [
            "Accept Current",
            "Accept Incoming",
            "Accept Both",
            "Compare",
        ]
    };
    let mut result = Vec::with_capacity(4);
    let (mut x, mut y) = (0, 0);
    for (label, action) in labels
        .into_iter()
        .zip(["current", "incoming", "both", "compare"])
    {
        let len = label.len() as u16;
        if x > 0 && x + len > width {
            x = 0;
            y += 1;
        }
        result.push((x, y, label, action));
        x = x.saturating_add(len).saturating_add(3);
    }
    result
}

pub(crate) fn action_rows(width: u16) -> u16 {
    action_layout(width.max(1), false)
        .last()
        .map_or(0, |action| action.1 + 1)
}

pub(crate) fn draw_marker_label(
    f: &mut Frame,
    app: &App,
    code: Rect,
    row: u16,
    source: crate::editor::WrappedRowInfo,
) {
    let Some(editor) = app.editor() else {
        return;
    };
    let Some(index) = editor.merge_conflicts.current(&editor.rope) else {
        return;
    };
    let Some(region) = index.region_for_line(source.line_idx) else {
        return;
    };
    let label = if source.line_idx == region.start {
        " (Current Change)"
    } else if source.line_idx == region.end {
        " (Incoming Change)"
    } else if Some(source.line_idx) == region.base {
        " (Common Ancestor)"
    } else {
        return;
    };
    if source.end_col < source.line_len {
        return;
    }
    let start = if editor.word_wrap {
        source.start_col
    } else {
        editor.horizontal_scroll
    };
    let line = editor.rope.line(source.line_idx);
    if source.line_len.saturating_sub(start) > code.width as usize {
        return;
    }
    let text: String = line
        .slice(start.min(source.line_len)..source.line_len)
        .chars()
        .collect();
    let width = unicode_width::UnicodeWidthStr::width(text.as_str()) as u16;
    if width >= code.width {
        return;
    }
    let x = code.x + width;
    let background = f.buffer_mut()[(x, code.y + row)].bg;
    f.render_widget(
        Paragraph::new(label).style(
            Style::default()
                .fg(app.colors.ui.editor_text_color(background))
                .bg(background),
        ),
        Rect::new(x, code.y + row, code.width - width, 1),
    );
}

pub(crate) fn draw_actions(
    f: &mut Frame,
    app: &App,
    area: Rect,
    line: usize,
    offset: u16,
    footer: bool,
    hits: &mut Vec<Hit>,
) {
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(app.colors.bg_editor)),
        area,
    );
    let Some(editor) = app.editor() else {
        return;
    };
    let layout = action_layout(area.width, footer);
    for (index, &(x, y, label, action)) in layout.iter().enumerate() {
        if y != offset || x >= area.width {
            continue;
        }
        let button = Rect::new(
            area.x + x,
            area.y,
            (label.len() as u16).min(area.width - x),
            1,
        );
        let enabled = action == "compare" || action == "merge_editor" || !editor.read_only;
        let hovered = button.contains((app.mouse_col, app.mouse_row).into());
        let mut style = app
            .colors
            .ui
            .codelens_style(app.colors.bg_editor)
            .remove_modifier(Modifier::ITALIC);
        if hovered && enabled {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if !enabled {
            style = style.add_modifier(Modifier::DIM);
        }
        f.render_widget(Paragraph::new(label).style(style), button);
        if enabled {
            hits.push(Hit {
                area: button,
                line,
                action,
                version: editor.content_version,
            });
        }
        if layout.get(index + 1).is_some_and(|next| next.1 == y)
            && button.right() + 3 <= area.right()
        {
            f.render_widget(
                Paragraph::new(" | ").style(style),
                Rect::new(button.right(), area.y, 3, 1),
            );
        }
    }
    if footer && area.width > 30 {
        if let Some(index) = editor.merge_conflicts.current(&editor.rope) {
            let position = index.regions.partition_point(|region| region.start < line) + 1;
            let label = format!("   Conflict {position}/{}", index.regions.len());
            f.render_widget(
                Paragraph::new(label).style(app.colors.ui.codelens_style(app.colors.bg_editor)),
                Rect::new(area.x + 23, area.y, area.width - 23, 1),
            );
        }
    }
}

fn label(source: &Rope, line: usize, side: &str) -> String {
    let name: String = source
        .line(line)
        .chars()
        .skip_while(|ch| matches!(ch, '<' | '>'))
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| !matches!(ch, '\r' | '\n'))
        .take(80)
        .collect();
    format!("{side} · {name}")
}

/// Explicit merge-editor mode keeps one editable result and two read-only
/// source panes. The source snapshot shares its text with the undo snapshot.
pub(crate) fn draw_merge_editor(f: &mut Frame, app: &mut App, area: Rect) -> Rect {
    let Some(editor) = app.editor() else {
        return area;
    };
    let Some(review) = &editor.merge_conflicts.review else {
        return area;
    };
    let source = review.source.clone();
    let region = review.region;
    let scroll = review.scroll;
    let index = editor.merge_conflicts.current(&editor.rope);
    let count = index.map(|index| index.regions.len());
    let unresolved_markers = index.is_some_and(Index::has_markers);
    let read_only = editor.read_only;
    let accept_enabled = !editor.read_only
        && source.is_instance(&editor.rope)
        && index.is_some_and(|index| index.regions.contains(&region));
    let mut chrome = Chrome {
        version: editor.content_version,
        ..Default::default()
    };
    let total = region.current().len().max(region.incoming().len());
    let preview_h = area.height.saturating_sub(6).min(14) / 2;
    let source_max_scroll = total.saturating_sub(preview_h.saturating_sub(1).max(1) as usize);
    let top = Rect::new(area.x, area.y, area.width, u16::from(area.height > 1));
    f.render_widget(
        Paragraph::new(format!(
            " Merge Editor · {} · {}",
            count.map_or_else(
                || "Updating…".into(),
                |n| if n == 0 && unresolved_markers {
                    "Conflict markers remain".into()
                } else if n == 1 {
                    "1 conflict remaining".into()
                } else {
                    format!("{n} conflicts remaining")
                }
            ),
            if read_only {
                "Result is read-only"
            } else {
                "Result is editable"
            }
        ))
        .style(
            Style::default()
                .fg(app.colors.fg_text)
                .bg(app.colors.bg_sidebar),
        ),
        top,
    );
    let header = Rect::new(area.x, top.bottom(), area.width, u16::from(area.height > 2));
    let close_label = if header.width >= 25 {
        "Back to Inline"
    } else {
        "×"
    };
    let close_width =
        (unicode_width::UnicodeWidthStr::width(close_label) as u16 + 4).min(header.width);
    let close = Rect::new(
        header.right() - close_width,
        header.y,
        close_width,
        header.height,
    );
    if !close.is_empty() {
        crate::ui_widgets::draw_action_button(
            f,
            close,
            close_label,
            &app.colors,
            false,
            close.contains((app.mouse_col, app.mouse_row).into()),
            true,
        );
        chrome.buttons.push(Hit {
            area: close,
            action: "close_merge_editor",
            line: region.start,
            version: chrome.version,
        });
    }
    let mut x = header.x;
    for (text, action) in [
        (
            if header.width >= 70 {
                "‹ Previous"
            } else {
                "‹"
            },
            "previous_conflict",
        ),
        (
            if header.width >= 70 {
                "Next ›"
            } else {
                "›"
            },
            "next_conflict",
        ),
        ("↑", "source_up"),
        ("↓", "source_down"),
        ("Accept Both", "both"),
    ] {
        let width = unicode_width::UnicodeWidthStr::width(text) as u16 + 4;
        if width > close.x.saturating_sub(x) {
            break;
        }
        let button = Rect::new(x, header.y, width, header.height);
        if width == 0 {
            break;
        }
        let enabled = if action == "both" {
            accept_enabled
        } else if action == "source_up" {
            scroll.min(source_max_scroll) > 0
        } else if action == "source_down" {
            scroll < source_max_scroll
        } else {
            count.is_some_and(|count| count > 0)
        };
        crate::ui_widgets::draw_action_button(
            f,
            button,
            text,
            &app.colors,
            false,
            button.contains((app.mouse_col, app.mouse_row).into()),
            enabled,
        );
        if enabled {
            chrome.buttons.push(Hit {
                area: button,
                action,
                line: region.start,
                version: chrome.version,
            });
        }
        x = button.right();
    }
    let preview = Rect::new(area.x, header.bottom(), area.width, preview_h);
    chrome.preview = preview;
    let split = area.width / 2;
    for (pane, lines, title, kind) in [
        (
            Rect::new(preview.x, preview.y, split, preview.height),
            region.current(),
            label(&source, region.start, "Current"),
            0,
        ),
        (
            Rect::new(
                preview.x + split,
                preview.y,
                preview.width - split,
                preview.height,
            ),
            region.incoming(),
            label(&source, region.end, "Incoming"),
            1,
        ),
    ] {
        if pane.is_empty() {
            continue;
        }
        let surface = app.colors.ui.merge_contents[kind]
            .over(app.colors.bg_editor)
            .unwrap_or(app.colors.bg_editor);
        let style = Style::default()
            .fg(app.colors.ui.editor_text_color(surface))
            .bg(surface);
        let header_bg = app.colors.ui.merge_headers[kind]
            .over(surface)
            .unwrap_or(surface);
        let mut contrast = crate::ui_colors::TextContrastCache::new(&app.colors.ui);
        let header_style = style
            .bg(header_bg)
            .fg(contrast.foreground(app.colors.fg_text, header_bg));
        f.render_widget(Paragraph::new("").style(style), pane);
        let (action_label, action) = if kind == 0 {
            ("Accept Current", "current")
        } else {
            ("Accept Incoming", "incoming")
        };
        let action_width = action_label.len() as u16 + 4;
        let button_width = if pane.width >= action_width + 16 {
            action_width
        } else {
            0
        };
        f.render_widget(
            Paragraph::new(title).style(header_style),
            Rect::new(pane.x, pane.y, pane.width - button_width, 1),
        );
        if button_width > 0 {
            let button = Rect::new(pane.right() - button_width, pane.y, button_width, 1);
            crate::ui_widgets::draw_action_button(
                f,
                button,
                action_label,
                &app.colors,
                false,
                button.contains((app.mouse_col, app.mouse_row).into()),
                accept_enabled,
            );
            if accept_enabled {
                chrome.buttons.push(Hit {
                    area: button,
                    action,
                    line: region.start,
                    version: chrome.version,
                });
            }
        }
        let offset = scroll.min(total.saturating_sub(pane.height.saturating_sub(1) as usize));
        for (row, line) in lines
            .skip(offset)
            .take(pane.height.saturating_sub(1) as usize)
            .enumerate()
        {
            let text: String = source
                .line(line)
                .chars()
                .take(pane.width as usize)
                .collect();
            f.render_widget(
                Paragraph::new(text.replace('\t', "    ")).style(style),
                Rect::new(pane.x, pane.y + 1 + row as u16, pane.width, 1),
            );
        }
    }
    let result_header = Rect::new(
        area.x,
        preview.bottom(),
        area.width,
        u16::from(preview.bottom() < area.bottom()),
    );
    f.render_widget(
        Paragraph::new(if read_only {
            " Result · Read-only document"
        } else if count == Some(0) && !unresolved_markers {
            " Result · All conflicts resolved — save and stage"
        } else {
            " Result · Edit normally, then save and stage"
        })
        .style(
            Style::default()
                .fg(app.colors.fg_text)
                .bg(app.colors.bg_sidebar),
        ),
        result_header,
    );
    chrome.area = Rect::new(
        area.x,
        area.y,
        area.width,
        result_header.bottom().saturating_sub(area.y),
    );
    app.editor_mut()
        .unwrap()
        .merge_conflicts
        .review
        .as_mut()
        .unwrap()
        .chrome = chrome;
    Rect::new(
        area.x,
        result_header.bottom(),
        area.width,
        area.bottom().saturating_sub(result_header.bottom()),
    )
}

pub(crate) fn source_scroll(app: &mut App, down: bool) {
    if let Some(review) = app
        .editor_mut()
        .and_then(|editor| editor.merge_conflicts.review.as_mut())
    {
        let max = review
            .region
            .current()
            .len()
            .max(review.region.incoming().len())
            .saturating_sub(review.chrome.preview.height.saturating_sub(1).max(1) as usize);
        let scroll = review.scroll.min(max);
        review.scroll = if down {
            scroll.saturating_add(3).min(max)
        } else {
            scroll.saturating_sub(3)
        };
    }
}

#[cfg(test)]
#[path = "merge_conflict_ui_tests.rs"]
mod tests;
