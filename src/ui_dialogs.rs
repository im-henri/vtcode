use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::helpers::len_as_u16;
use crate::ui_pickers::truncate_to_width;
use crate::ui_widgets::{DropdownMenu, PopupDialog, Tooltip};


/// Maximum visible items in call-hierarchy / references popups.
const HIERARCHY_MAX_VISIBLE: usize = 12;
/// Maximum visible items in the code-actions tooltip.
const CODE_ACTIONS_MAX_VISIBLE: usize = 8;

/// Width of the peek-definition popup.
const PEEK_DEFINITION_WIDTH: u16 = 80;
/// Duration (ms) before screencast-key overlay fades out.
const SCREENCAST_FADE_MS: u64 = 3000;
/// Maximum width for the screencast toast panel.
const SCREENCAST_MAX_WIDTH: usize = 50;

pub(crate) fn file_icon(name: &str) -> &'static str {
    crate::file_icons::for_name(name).0
}

pub(crate) fn tab_icon(kind: crate::app::TabKind, name: &str) -> &'static str {
    match kind {
        crate::app::TabKind::File | crate::app::TabKind::GitRevision => file_icon(name),
        crate::app::TabKind::Diff(_) => "↔ ",
        crate::app::TabKind::CommitDetails => "◈ ",
        crate::app::TabKind::MarkdownPreview => "Md ",
        crate::app::TabKind::Settings => "≡ ",
        crate::app::TabKind::KeyboardShortcuts => "Kb ",
        crate::app::TabKind::ExtensionView | crate::app::TabKind::ExtensionInfo => "Ex ",
        crate::app::TabKind::Terminal => ">_ ",
        crate::app::TabKind::Chat(_) => "◇ ",
    }
}

/// File icons have their own palette, separate from workbench color themes.
pub(crate) fn file_icon_colored(name: &str) -> (&'static str, Color) {
    crate::file_icons::for_name(name)
}

pub(crate) fn draw_confirm_close(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::Close);
}

pub(crate) fn draw_confirm_delete(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::Delete);
}

pub(crate) fn draw_confirm_move(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::Move);
}

pub(crate) fn draw_binary_open_prompt(f: &mut Frame, app: &mut App) {
    crate::ui_quick_picker::draw(f, app, crate::ui_quick_picker::Kind::OpenWith);
}

pub(crate) fn draw_exit_confirm(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::Exit);
}

pub(crate) fn draw_context_menu(f: &mut Frame, app: &mut App) {
    let (menu_x, menu_y) = app.context_menu_pos;
    let items: Vec<crate::ui_widgets::DropdownItem> = app.context_menu_items.iter()
        .map(|ci| {
            let mut item = crate::ui_widgets::DropdownItem::new(&ci.label).shortcut(&ci.shortcut)
                .disabled(app.source_control_menu_command_disabled(ci.id));
            if ci.separator_after { item = item.separator(); }
            item
        })
        .collect();
    let colors = crate::ui_widgets::themed_dropdown_colors(&app.colors);
    let inner = DropdownMenu::new(&items, colors)
        .anchor(menu_x, menu_y)
        .min_width(28)
        .selected(app.context_menu_selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render_scrolled(f, &mut app.context_menu_scroll);
    app.context_menu_inner = (!inner.is_empty()).then_some(inner);
}

pub(crate) fn draw_explorer_context_menu(f: &mut Frame, app: &mut App) {
    use crate::explorer::ExplorerSortMode;

    let str_items = app.explorer_context_menu_items();
    let mut items = crate::ui_widgets::items_from_str_slice(&str_items);
    let selected_sort = match app.explorer.sort_mode {
        ExplorerSortMode::Name => "Sort by Name",
        ExplorerSortMode::Size => "Sort by Size",
        ExplorerSortMode::Modified => "Sort by Date Modified",
        ExplorerSortMode::Type => "Sort by Type",
    };
    for item in &mut items {
        // Reserve the same marker column on every row so labels stay aligned.
        item.icon = if item.label == selected_sort { "✓ " } else { "  " };
    }
    let (menu_x, menu_y) = app.explorer_context_pos;
    let inner = DropdownMenu::new(&items, crate::ui_widgets::themed_dropdown_colors(&app.colors))
        .anchor(menu_x, menu_y)
        .min_width(22)
        .selected(app.explorer_context_selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render(f);
    app.explorer_context_inner = Some(inner);
}

pub(crate) fn draw_source_control_context_menu(f: &mut Frame, app: &mut App) {
    let str_items = app.source_control_context_menu_items();
    let items = crate::ui_widgets::items_from_str_slice(&str_items);
    let (menu_x, menu_y) = app.sc_context_pos;
    let inner = DropdownMenu::new(&items, crate::ui_widgets::themed_dropdown_colors(&app.colors))
        .anchor(menu_x, menu_y)
        .min_width(20)
        .selected(app.sc_context_selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render(f);
    app.sc_context_inner = Some(inner);
}

pub(crate) fn draw_hunk_popup(f: &mut Frame, app: &mut App) {
    if app.hunk_peek.is_some() || app.hunk_preview_feedback.is_some() {
        crate::ui_hunk_peek::draw(f, app);
        return;
    }
    let str_items = app.hunk_popup_items();
    let items = crate::ui_widgets::items_from_str_slice(&str_items);
    let (menu_x, menu_y) = app.hunk_popup_pos;
    DropdownMenu::new(&items, crate::ui_widgets::themed_dropdown_colors(&app.colors))
        .anchor(menu_x, menu_y)
        .min_width(16)
        .selected(app.hunk_popup_selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render(f);
}

#[cfg(test)]
pub(crate) use crate::ui_commit_details::draw as draw_source_control_commit_details;

pub(crate) fn draw_source_control_discard_confirm(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::Discard);
}


pub(crate) fn draw_save_as_dialog(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::SaveAs);
}

pub(crate) fn draw_rename_symbol_dialog(f: &mut Frame, app: &mut App) {
    crate::ui_modal::draw(f, app, crate::ui_modal::Kind::Rename);
}

pub(crate) fn draw_lsp_hover(f: &mut Frame, app: &App) {
    if !app.lsp_hover_active { return; }
    let Some(content) = &app.lsp_manager.hover_content else { return };
    let Some(code_area) = app.code_area else { return };
    let Some(editor) = app.editor() else { return };

    let hover_y = (app.lsp_hover_line.saturating_sub(editor.scroll_offset)) as u16 + code_area.y;
    let hover_x = (app.lsp_hover_col.saturating_sub(editor.horizontal_scroll)) as u16 + code_area.x;

    let lines: Vec<Line> = content.lines().take(10).map(Line::raw).collect();

    Tooltip::hover(lines, &app.colors)
        .max_width(64)
        .max_height(12)
        .avoid(crate::ui_lsp_completion::occupied_area(app))
        .render(f, hover_x, hover_y);
}

pub(crate) struct SignaturePresentation {
    key: (u64, usize, usize, u16),
    lines: Vec<Line<'static>>,
    highlights: Vec<(usize, usize)>,
}

pub(crate) fn draw_lsp_signature_help(f: &mut Frame, app: &App) {
    if !app.lsp_signature_active { return; }
    let Some(sig_help) = &app.lsp_manager.signature_help else { return };
    if sig_help.signatures.is_empty() { return; }
    let Some(code_area) = app.code_area else { return };
    let Some(editor) = app.editor() else { return };

    let sig_idx = sig_help.active_signature.min(sig_help.signatures.len() - 1);
    let sig = &sig_help.signatures[sig_idx];
    let active_param = sig_help.active_parameter;

    let cursor_y = (editor.cursor_line.saturating_sub(editor.scroll_offset)) as u16 + code_area.y;
    let cursor_x = (editor.cursor_col.saturating_sub(editor.horizontal_scroll)) as u16 + code_area.x;

    let width = code_area.width.min(f.area().width).min(72);
    let key = (app.lsp_manager.signature_revision, sig_idx, active_param, width);
    let mut cached = app.completion_ui.signature.borrow_mut();
    if cached.as_ref().is_none_or(|cached| cached.key != key) {
        let normal = Style::default();
        let highlight = normal.add_modifier(Modifier::BOLD);

        let mut spans: Vec<Span> = Vec::with_capacity(3);
        if active_param < sig.parameters.len() {
            let (start, end) = sig.parameters[active_param];
            let start = start.min(sig.label.len());
            let end = end.min(sig.label.len());
            // Snap to nearest char boundary to avoid panicking on multi-byte UTF-8
            let start = (0..=start).rev().find(|&i| sig.label.is_char_boundary(i)).unwrap_or(0);
            let end = (end..=sig.label.len()).find(|&i| sig.label.is_char_boundary(i)).unwrap_or(sig.label.len());
            if start < end {
                spans.push(Span::styled(sig.label[..start].to_owned(), normal));
                spans.push(Span::styled(sig.label[start..end].to_owned(), highlight));
                spans.push(Span::styled(sig.label[end..].to_owned(), normal));
            } else {
                spans.push(Span::styled(sig.label.clone(), normal));
            }
        } else {
            spans.push(Span::styled(sig.label.clone(), normal));
        }

        let mut lines_to_render: Vec<Line> = vec![Line::from(spans)];
        if let Some(doc) = &sig.documentation {
            for dl in doc.lines().take(3) {
                lines_to_render.push(Line::from(Span::styled(dl.to_owned(), normal)));
            }
        }

        let lines = crate::ui_markdown::wrap_help_lines(lines_to_render, width.saturating_sub(4) as usize);
        let highlights = lines.iter().enumerate().flat_map(|(row, line)| line.spans.iter().enumerate()
            .filter(|(_, span)| span.style.add_modifier.contains(Modifier::BOLD)).map(move |(span, _)| (row, span))).collect::<Vec<_>>();
        *cached = Some(SignaturePresentation { key, lines, highlights });
    }
    let cached = cached.as_ref().unwrap();
    let title = if sig_help.signatures.len() > 1 {
        format!("Signature {}/{} · Alt+[ ]", sig_idx + 1, sig_help.signatures.len())
    } else { "Signature".into() };
    let mut tooltip = Tooltip::hover(cached.lines.iter().take(10).cloned().collect(), &app.colors)
        .title(&title, Style::default())
        .max_width(width)
        .max_height(12)
        .avoid(crate::ui_lsp_completion::occupied_area(app));
    for &(row, span) in &cached.highlights { tooltip = tooltip.span_foreground(row, span, app.colors.ui.hover_highlight); }
    tooltip.render(f, cursor_x, cursor_y);
}

pub(crate) fn draw_lsp_code_actions(f: &mut Frame, app: &App) {
    if !app.lsp_code_action_active || app.lsp_manager.code_actions.is_empty() { return; }
    let Some(code_area) = app.code_area else { return; };
    let Some(editor) = app.editor() else { return; };

    let cursor_y = (editor.cursor_line.saturating_sub(editor.scroll_offset)) as u16 + code_area.y;
    let cursor_x = (editor.cursor_col.saturating_sub(editor.horizontal_scroll)) as u16 + code_area.x;

    let actions = &app.lsp_manager.code_actions;
    let max_visible = CODE_ACTIONS_MAX_VISIBLE.min(actions.len());
    let max_width = actions.iter().map(|a| a.title.len()).max().unwrap_or(20).min(50) + 6;

    let scroll = app.lsp_code_action_selected.saturating_sub(max_visible - 1);
    let mut lines: Vec<Line> = actions.iter().skip(scroll).take(max_visible).map(|action| {
        let kind_icon = match action.kind.as_deref() {
            Some("quickfix") => "🔧 ",
            Some("refactor") => "✏️ ",
            Some(k) if k.starts_with("refactor") => "✏️ ",
            _ => "💡 ",
        };
        let label: String = action.title.chars().take(max_width.saturating_sub(4)).collect();
        Line::raw(format!("{kind_icon}{label}"))
    }).collect();

    // Paragraph only paints the spans, so include the trailing cells in every
    // row's style. Retain the tooltip's existing size when padding its content.
    let popup_width = (lines.iter().map(Line::width).max().unwrap_or(0) + 4)
        .min(max_width).min(f.area().width as usize) as u16;
    for line in &mut lines {
        let padding = popup_width.saturating_sub(2).saturating_sub(line.width() as u16);
        line.spans.push(Span::raw(crate::helpers::spaces(padding as usize)));
    }

    let mut tooltip = Tooltip::themed(lines, crate::theme_workbench::ControlColors {
        background: app.colors.ui.action_list_background,
        foreground: app.colors.ui.action_list_foreground,
        border: app.colors.ui.widget_border,
    }, &app.colors)
        .title("Code Actions", Style::default().add_modifier(Modifier::BOLD))
        .max_width(popup_width)
        .max_height(max_visible as u16 + 2);
    for row in 0..max_visible {
        let selected = row + scroll == app.lsp_code_action_selected;
        let colors = if selected { crate::theme_workbench::ControlColors {
            background: app.colors.ui.action_list_focus_background,
            foreground: app.colors.ui.action_list_focus_foreground,
            border: app.colors.ui.list_focus_outline,
        } } else { crate::theme_workbench::ControlColors::default() };
        tooltip = tooltip.line_colors(row, colors);
    }
    tooltip.render(f, cursor_x, cursor_y + 1);
}

pub(crate) fn draw_call_hierarchy(f: &mut Frame, app: &App) {
    if !app.call_hierarchy_active { return; }

    let results = &app.call_hierarchy_results;
    let max_visible = HIERARCHY_MAX_VISIBLE.min(results.len().max(1));
    let popup_height = max_visible as u16 + 3;

    let mode_label = match app.call_hierarchy_mode {
        crate::app::CallHierarchyMode::Incoming => "Callers of",
        crate::app::CallHierarchyMode::Outgoing => "Calls from",
    };
    let title = format!("{mode_label} {}", app.call_hierarchy_root_name);
    let popup = PopupDialog::new(&title).size(60, popup_height);
    let bg = popup.widget_background(f, &app.colors);
    let fg = app.colors.ui.widget_text_color(bg);
    let inner = popup
        .title_color(fg)
        .border_color(app.colors.ui.widget_border.border(bg).unwrap_or(bg))
        .bg(bg)
        .render(f);

    if results.is_empty() {
        let msg = Line::from(Span::styled("  No results", Style::default().fg(app.colors.ui.description_color(bg)).bg(bg)));
        f.render_widget(Paragraph::new(vec![msg]), inner);
        return;
    }

    let scroll = app.call_hierarchy_selected.saturating_sub(max_visible - 1);
    let mut lines: Vec<Line> = Vec::with_capacity(max_visible);
    let mut fmt_buf = String::with_capacity(64);
    for (i, entry) in results.iter().enumerate().skip(scroll).take(max_visible) {
        let is_selected = i == app.call_hierarchy_selected;
        fmt_buf.clear();
        {
            use std::fmt::Write;
            let _ = write!(fmt_buf, "  {} ({}:{})", entry.name, entry.detail, entry.line + 1);
        }
        if UnicodeWidthStr::width(fmt_buf.as_str()) > inner.width as usize {
            fmt_buf = truncate_to_width(&fmt_buf, inner.width as usize);
        }
        fmt_buf.push_str(&crate::helpers::spaces((inner.width as usize).saturating_sub(UnicodeWidthStr::width(fmt_buf.as_str()))));
        let style = app.colors.ui.list_row_style(app.colors.ui.widget_foreground, bg, is_selected, true, false);
        lines.push(Line::from(Span::styled(fmt_buf.clone(), style)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn draw_type_hierarchy(f: &mut Frame, app: &App) {
    if !app.type_hierarchy_active { return; }

    let results = &app.type_hierarchy_results;
    let max_visible = HIERARCHY_MAX_VISIBLE.min(results.len().max(1));
    let popup_height = max_visible as u16 + 3;

    let mode_label = match app.type_hierarchy_mode {
        crate::app::TypeHierarchyMode::Supertypes => "Supertypes of",
        crate::app::TypeHierarchyMode::Subtypes => "Subtypes of",
    };
    let title = format!("{mode_label} {}", app.type_hierarchy_root_name);
    let popup = PopupDialog::new(&title).size(60, popup_height);
    let bg = popup.widget_background(f, &app.colors);
    let fg = app.colors.ui.widget_text_color(bg);
    let inner = popup
        .title_color(fg)
        .border_color(app.colors.ui.widget_border.border(bg).unwrap_or(bg))
        .bg(bg)
        .render(f);

    if results.is_empty() {
        let msg = Line::from(Span::styled("  No results", Style::default().fg(app.colors.ui.description_color(bg)).bg(bg)));
        f.render_widget(Paragraph::new(vec![msg]), inner);
        return;
    }

    let scroll = app.type_hierarchy_selected.saturating_sub(max_visible - 1);
    let mut lines: Vec<Line> = Vec::with_capacity(max_visible);
    let mut fmt_buf = String::with_capacity(64);
    for (i, entry) in results.iter().enumerate().skip(scroll).take(max_visible) {
        let is_selected = i == app.type_hierarchy_selected;
        fmt_buf.clear();
        {
            use std::fmt::Write;
            let _ = write!(fmt_buf, "  {} ({}:{})", entry.name, entry.detail, entry.line + 1);
        }
        if UnicodeWidthStr::width(fmt_buf.as_str()) > inner.width as usize {
            fmt_buf = truncate_to_width(&fmt_buf, inner.width as usize);
        }
        fmt_buf.push_str(&crate::helpers::spaces((inner.width as usize).saturating_sub(UnicodeWidthStr::width(fmt_buf.as_str()))));
        let style = app.colors.ui.list_row_style(app.colors.ui.widget_foreground, bg, is_selected, true, false);
        lines.push(Line::from(Span::styled(fmt_buf.clone(), style)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn draw_lsp_references(f: &mut Frame, app: &App) {
    if !app.lsp_references_active || app.lsp_manager.reference_locations.is_empty() { return; }

    let locations = &app.lsp_manager.reference_locations;
    let max_visible = 12.min(locations.len());
    let popup_height = max_visible as u16 + 3;

    let title = format!("References ({})", locations.len());
    let popup = PopupDialog::new(&title).size(70, popup_height);
    let bg = popup.widget_background(f, &app.colors);
    let fg = app.colors.ui.widget_text_color(bg);
    let inner = popup
        .title_color(fg)
        .border_color(app.colors.ui.widget_border.border(bg).unwrap_or(bg))
        .bg(bg)
        .render(f);

    let scroll = app.lsp_references_selected.saturating_sub(max_visible - 1);
    let mut lines: Vec<Line> = Vec::with_capacity(max_visible);
    let mut fmt_buf = String::with_capacity(64);
    for (i, loc) in locations.iter().enumerate().skip(scroll).take(max_visible) {
        let is_selected = i == app.lsp_references_selected;
        fmt_buf.clear();
        {
            use std::fmt::Write;
            let file_name = loc.uri.to_file_path()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            match file_name {
                Some(name) => { let _ = write!(fmt_buf, "  {}:{}:{}", name, loc.range.start.line + 1, loc.range.start.character + 1); }
                None => { let _ = write!(fmt_buf, "  {}:{}:{}", loc.uri.path(), loc.range.start.line + 1, loc.range.start.character + 1); }
            }
        }
        if UnicodeWidthStr::width(fmt_buf.as_str()) > inner.width as usize {
            fmt_buf = truncate_to_width(&fmt_buf, inner.width as usize);
        }
        fmt_buf.push_str(&crate::helpers::spaces((inner.width as usize).saturating_sub(UnicodeWidthStr::width(fmt_buf.as_str()))));
        let style = app.colors.ui.list_row_style(app.colors.ui.widget_foreground, bg, is_selected, true, false);
        lines.push(Line::from(Span::styled(fmt_buf.clone(), style)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn draw_shortcuts(f: &mut Frame, app: &App) {
    let popup = PopupDialog::new("Keyboard Shortcuts").size(70, 35);
    let bg = popup.widget_background(f, &app.colors);
    let fg = app.colors.ui.widget_text_color(bg);
    let inner = popup
        .title_color(fg)
        .border_color(app.colors.ui.widget_border.border(bg).unwrap_or(bg))
        .bg(bg)
        .render(f);

    let key = app.colors.ui.keybinding_label_style(bg).add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(fg).bg(bg);
    let heading_bg = app.colors.ui.keybinding_table_header.over(bg).unwrap_or(bg);
    let heading = Style::default().fg(app.colors.ui.widget_text_color(heading_bg)).bg(heading_bg).add_modifier(Modifier::BOLD);
    let header = |label: &'static str| Line::from(Span::styled(
        format!("{label:width$}", width = inner.width as usize), heading));

    let shortcuts = vec![
        header("── File ──"),
        Line::from(vec![Span::styled("Ctrl+N          ", key), Span::styled("New File", desc)]),
        Line::from(vec![Span::styled("Ctrl+S          ", key), Span::styled("Save", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+S    ", key), Span::styled("Save As", desc)]),
        Line::from(vec![Span::styled("Ctrl+W          ", key), Span::styled("Close Tab", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+T    ", key), Span::styled("Reopen Closed Tab", desc)]),
        Line::from(vec![Span::styled("Ctrl+Q          ", key), Span::styled("Quit", desc)]),
        Line::from(""),
        header("── Edit ──"),
        Line::from(vec![Span::styled("Ctrl+Z/Y        ", key), Span::styled("Undo / Redo", desc)]),
        Line::from(vec![Span::styled("Ctrl+C/X/V      ", key), Span::styled("Copy / Cut / Paste", desc)]),
        Line::from(vec![Span::styled("Ctrl+D          ", key), Span::styled("Select Next Occurrence", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+L    ", key), Span::styled("Select All Occurrences", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+D    ", key), Span::styled("Duplicate Line", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+K    ", key), Span::styled("Delete Line", desc)]),
        Line::from(vec![Span::styled("Ctrl+J          ", key), Span::styled("Join Lines", desc)]),
        Line::from(vec![Span::styled("Ctrl+/          ", key), Span::styled("Toggle Comment", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+U    ", key), Span::styled("Toggle Case", desc)]),
        Line::from(vec![Span::styled("Alt+Up/Down     ", key), Span::styled("Move Line Up/Down", desc)]),
        Line::from(vec![Span::styled("Tab / Shift+Tab ", key), Span::styled("Indent / Unindent", desc)]),
        Line::from(""),
        header("── Selection ──"),
        Line::from(vec![Span::styled("Ctrl+A          ", key), Span::styled("Select All", desc)]),
        Line::from(vec![Span::styled("Shift+Arrow     ", key), Span::styled("Extend Selection", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+K    ", key), Span::styled("Delete Line", desc)]),
        Line::from(vec![Span::styled("Ctrl+L          ", key), Span::styled("Select Line", desc)]),
        Line::from(vec![Span::styled("Alt+Click       ", key), Span::styled("Toggle Cursor", desc)]),
        Line::from(""),
        header("── Navigate ──"),
        Line::from(vec![Span::styled("Ctrl+P          ", key), Span::styled("Quick Open File", desc)]),
        Line::from(vec![Span::styled("Ctrl+G          ", key), Span::styled("Go to Line:Column", desc)]),
        Line::from(vec![Span::styled("Ctrl+F          ", key), Span::styled("Find", desc)]),
        Line::from(vec![Span::styled("Ctrl+H          ", key), Span::styled("Find & Replace", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+F    ", key), Span::styled("Search Across Files", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+O    ", key), Span::styled("Go to Symbol", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+P    ", key), Span::styled("Command Palette", desc)]),
        Line::from(vec![Span::styled("Ctrl+Shift+\\   ", key), Span::styled("Go to Matching Bracket", desc)]),
        Line::from(vec![Span::styled("Alt+Left/Right  ", key), Span::styled("Navigate Back/Forward", desc)]),
        Line::from(vec![Span::styled("Alt+1-9         ", key), Span::styled("Switch to Tab N", desc)]),
        Line::from(""),
        header("── View ──"),
        Line::from(vec![Span::styled("Ctrl+B          ", key), Span::styled("Toggle Explorer", desc)]),
        Line::from(vec![Span::styled("Ctrl+`          ", key), Span::styled("Toggle Right Terminal", desc)]),
        Line::from(vec![Span::styled("Ctrl+\\          ", key), Span::styled("Split Editor", desc)]),
        Line::from(vec![Span::styled("Ctrl+K Z        ", key), Span::styled("Zen Mode", desc)]),
        Line::from(vec![Span::styled("Ctrl++/-        ", key), Span::styled("Zoom In/Out (minimap)", desc)]),
        Line::from(vec![Span::styled("Ctrl+?          ", key), Span::styled("Show This Help", desc)]),
        Line::from(""),
        Line::from(Span::styled("Press Esc to close", Style::default().fg(app.colors.ui.description_color(bg)).bg(bg))),
    ];

    let p = Paragraph::new(shortcuts).scroll((app.shortcuts_scroll as u16, 0));
    f.render_widget(p, inner);
}

/// Find scope-defining lines above scroll offset for sticky scroll display.
/// Returns up to 3 lines with their line numbers and text.
#[cfg(test)]
pub(crate) fn find_sticky_scroll_lines(editor: &crate::editor::Editor, scroll: usize) -> Vec<(usize, String)> {
    if scroll == 0 {
        return Vec::new();
    }

    let scope_keywords = [
        "fn ", "pub(crate) fn ", "async fn ", "pub async fn ",
        "impl ", "pub struct ", "struct ", "pub enum ", "enum ", "pub trait ", "trait ",
        "mod ", "pub mod ",
        "class ", "function ", "def ", "if ", "for ", "while ", "match ", "loop ",
    ];

    let mut result = Vec::new();
    let mut target_indent = usize::MAX;

    // Scan backward from scroll offset, limit to 200 lines for performance
    let scan_start = scroll.saturating_sub(200);
    for line_idx in (scan_start..scroll).rev() {
        if line_idx >= editor.rope.len_lines() {
            continue;
        }
        let line_buf = editor.rope.line(line_idx).to_string();
        let trimmed = line_buf.trim_end_matches(['\n', '\r']);
        let indent = trimmed.len() - trimmed.trim_start().len();

        // Only consider lines with less indent than what we're looking for
        if indent >= target_indent {
            continue;
        }

        let stripped = trimmed.trim_start();
        let is_scope = stripped.ends_with('{')
            || scope_keywords.iter().any(|kw| stripped.starts_with(kw));

        if is_scope && !stripped.is_empty() {
            result.push((line_idx, trimmed.to_string()));
            target_indent = indent;
            if result.len() >= 3 || indent == 0 {
                break;
            }
        }
    }

    result.reverse();
    result
}

pub(crate) fn draw_peek_definition(f: &mut Frame, app: &App) {
    let Some(peek) = &app.peek_definition else { return; };

    // Position below the editor area or in center
    let area = app.code_area.unwrap_or_else(|| f.area());
    let width = area.width.min(PEEK_DEFINITION_WIDTH);
    let height = (len_as_u16(peek.lines.len()) + 2).clamp(5, 15);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + area.height.saturating_sub(height);
    let popup_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, popup_area);
    let bg = app.colors.ui.peek_editor_background.over(app.colors.bg_editor).unwrap_or(app.colors.bg_editor);
    let gutter_bg = app.colors.ui.peek_gutter_background.over(bg).unwrap_or(bg);
    let (title_bg, title_fg, _) = app.colors.ui.peek_title_colors(bg);
    let foreground = app.colors.ui.editor_text_color(bg);
    let line_number = app.colors.ui.readable_foreground(app.colors.ui.line_number_foreground.over(gutter_bg).unwrap_or(foreground), gutter_bg, 4.5);
    let title = format!(" {} (line {}) ", peek.file_path, peek.start_line + 1);
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(title_fg).bg(title_bg)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.colors.ui.peek_border.border(bg).unwrap_or(bg)))
        .style(Style::default().bg(bg));

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    for (i, line_text) in peek.lines.iter().enumerate() {
        if i as u16 >= inner.height { break; }
        let line_num = peek.start_line + i + 1;
        let num_span = Span::styled(
            format!("{line_num:>4} "),
            Style::default().fg(line_number).bg(gutter_bg),
        );
        let text_span = Span::styled(
            line_text.clone(),
            Style::default().fg(foreground).bg(bg),
        );
        let line = Paragraph::new(Line::from(vec![num_span, text_span]));
        f.render_widget(line, Rect::new(inner.x, inner.y + i as u16, inner.width, 1));
    }
}

pub(crate) fn get_cached_blame_line(app: &App, editor: &crate::editor::Editor, line: usize) -> Option<String> {
    if editor.bg_loading {
        return None;
    }
    let document = crate::git_blame::cached(app, editor)?;
    let label = document.lines.get(line)?.label();
    (!label.is_empty()).then_some(label)
}

/// VS Code-style search panel drawn in the sidebar
pub(crate) fn draw_search_panel(f: &mut Frame, app: &mut App, area: Rect) {
    crate::ui_search_panel::draw_search_panel(f, app, area);
}

/// Format a KeyEvent into a human-readable string like "Ctrl+Shift+A", "F5", "Enter".
fn format_key_event(key: &crossterm::event::KeyEvent) -> String {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut parts = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("Alt");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        // Only show Shift for non-character keys (Shift is implicit for uppercase chars)
        match key.code {
            KeyCode::Char(_) => {}
            _ => parts.push("Shift"),
        }
    }
    let key_name = match key.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                c.to_uppercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => "Shift+Tab".to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        _ => format!("{:?}", key.code),
    };
    parts.push(&key_name);
    parts.join("+")
}

/// Screencast mode overlay: shows recent key presses at bottom-center.
pub(crate) fn draw_screencast_overlay(f: &mut Frame, app: &App) {
    if !app.screencast_mode || app.screencast_keys.is_empty() {
        return;
    }

    let now = std::time::Instant::now();
    let fade_duration = std::time::Duration::from_millis(SCREENCAST_FADE_MS);

    // Collect visible entries (not yet faded out)
    let visible: Vec<_> = app.screencast_keys.iter()
        .filter(|(_, t)| now.duration_since(*t) < fade_duration)
        .collect();

    if visible.is_empty() {
        return;
    }

    // Merge consecutive identical keys into "Key ×N"
    let mut merged: Vec<(String, usize, std::time::Instant)> = Vec::new();
    for (key, ts) in &visible {
        let name = format_key_event(key);
        if let Some(last) = merged.last_mut() {
            if last.0 == name {
                last.1 += 1;
                last.2 = *ts;
                continue;
            }
        }
        merged.push((name, 1, *ts));
    }

    // Build display strings
    let display: Vec<String> = merged.iter().map(|(name, count, _)| {
        if *count > 1 {
            format!("{} ×{}", name, count)
        } else {
            name.clone()
        }
    }).collect();

    // Calculate dimensions
    let max_text_w = display.iter().map(|s| s.width()).max().unwrap_or(10);
    let panel_w = (max_text_w + 4).min(SCREENCAST_MAX_WIDTH) as u16; // padding + border
    let panel_h = (display.len() + 2) as u16; // items + border

    let area = f.area();
    if area.width < panel_w + 4 || area.height < panel_h + 2 {
        return;
    }

    // Position at bottom-center
    let x = (area.width.saturating_sub(panel_w)) / 2;
    let y = area.height.saturating_sub(panel_h + 2);
    let rect = Rect::new(x, y, panel_w, panel_h);
    let surface = crate::ui_widgets::widget_background(f, &app.colors, rect);
    let foreground = app.colors.ui.widget_text_color(surface);

    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.colors.ui.widget_border.border(surface).unwrap_or(surface)))
        .style(Style::default().bg(surface).fg(foreground))
        .title(Span::styled(" ⌨ Keys ", Style::default().fg(foreground).add_modifier(Modifier::BOLD)));

    let inner = block.inner(rect);
    f.render_widget(block, rect);

    // Render each key line with fade effect
    for (i, (text, _, ts)) in merged.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        let age = now.duration_since(*ts).as_millis() as f64;
        let fade = 1.0 - (age / fade_duration.as_millis() as f64).min(1.0);
        let key_style = app.colors.ui.keybinding_label_style(surface);
        let key_background = key_style.bg.unwrap_or(surface);
        let color = crate::app_theme::lerp_color(app.colors.ui.description_color(key_background),
            key_style.fg.unwrap_or(foreground), fade as f32);
        let color = app.colors.ui.readable_foreground(color, key_background, 4.5);

        let display_text = if merged[i].1 > 1 {
            format!("{} ×{}", text, merged[i].1)
        } else {
            text.clone()
        };

        // Center the text in the panel
        let text_w = display_text.width();
        let pad = (inner.width as usize).saturating_sub(text_w) / 2;
        f.render_widget(
            Span::styled(display_text, key_style.fg(color).add_modifier(Modifier::BOLD)),
            Rect::new(inner.x + pad as u16, inner.y + i as u16,
                inner.width.saturating_sub(pad as u16), 1),
        );
    }
}

pub(crate) fn draw_extension_overlays(f: &mut Frame, app: &App) {
    let screen = f.area();
    for (_ext_id, overlay) in app.extension_manager.overlays() {
        let x = (overlay.x).min(screen.width.saturating_sub(overlay.width));
        let y = (overlay.y).min(screen.height.saturating_sub(overlay.height));
        let w = overlay.width.min(screen.width.saturating_sub(x));
        let h = overlay.height.min(screen.height.saturating_sub(y));

        if w < 4 || h < 3 {
            continue;
        }

        let area = Rect::new(x, y, w, h);
        let surface = crate::ui_widgets::widget_background(f, &app.colors, area);
        let foreground = app.colors.ui.widget_text_color(surface);

        f.render_widget(Clear, area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.colors.ui.widget_border.border(surface).unwrap_or(surface)))
            .style(Style::default().bg(surface).fg(foreground));
        f.render_widget(block, area);

        // Title bar (first row inside border)
        let title_area = Rect::new(x + 1, y, w.saturating_sub(2), 1);
        let close_text = " \u{2715}";
        let close_width = len_as_u16(close_text.width());
        let title_width = title_area.width.saturating_sub(close_width);

        let title = truncate_to_width(&overlay.title, title_width as usize);
        let title_span = Span::styled(
            &title,
            Style::default()
                .fg(foreground).bg(surface)
                .add_modifier(Modifier::BOLD),
        );
        f.render_widget(title_span, Rect::new(x + 1, y, title_width, 1));

        let close_x = x + w.saturating_sub(close_width + 1);
        let hovered = Rect::new(close_x, y, close_width + 1, 1)
            .contains((app.mouse_col, app.mouse_row).into());
        let close_style = app.colors.ui.toolbar_layer_style(app.colors.ui.widget_foreground,
            surface, hovered, false, false);
        let close_span = Span::styled(close_text, close_style);
        f.render_widget(
            close_span,
            Rect::new(close_x, y, close_width, 1),
        );

        // Content area (inside border, below title)
        let content_area = Rect::new(x + 1, y + 1, w.saturating_sub(2), h.saturating_sub(2));
        let lines: Vec<Line> = overlay
            .content
            .lines()
            .take(content_area.height as usize)
            .map(|line| {
                let truncated = truncate_to_width(line, content_area.width as usize);
                Line::from(truncated)
            })
            .collect();
        let text = ratatui::text::Text::from(lines);
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(foreground).bg(surface));
        f.render_widget(paragraph, content_area);
    }
}

#[cfg(test)]
mod tests {
    use super::find_sticky_scroll_lines;
    use crate::editor::Editor;

    #[test]
    fn exit_dialog_counts_files_and_ignores_settings_buffers() {
        use crate::app::{App, EditorGroup, Tab};
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};

        let mut app = App::test_new();
        app.open_settings_tab();
        app.editor_mut().unwrap().modified = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let render = |terminal: &mut Terminal<TestBackend>, app: &mut App| {
            terminal.draw(|frame| super::draw_exit_confirm(frame, app)).unwrap();
            let buffer = terminal.backend().buffer();
            Rect::new(0, 0, 100, 20).rows().flat_map(|row| {
                (row.x..row.right()).map(move |x| buffer.cell((x, row.y)).unwrap().symbol())
            }).collect::<String>()
        };

        let text = render(&mut terminal, &mut app);
        assert_eq!(app.unsaved_file_count(), 0);
        assert!(text.contains("Close this workspace and quit?"));
        assert!(!text.contains("unsaved file"));
        assert_eq!(app.exit_confirm_buttons.len(), 2);

        // A dirty file in another pane must still be protected while Settings
        // has focus. Untitled file tabs count too, even without a file path.
        let mut group = EditorGroup::new();
        let mut editor = Editor::new();
        editor.insert_char('x');
        group.tabs.push(Tab::new_file(editor));
        app.groups.push(group);
        let text = render(&mut terminal, &mut app);
        assert_eq!(app.unsaved_file_count(), 1);
        assert!(text.contains("1 unsaved document"));
        assert_eq!(app.exit_confirm_buttons.len(), 4);
    }

    #[test]
    fn sticky_scroll_zero_returns_empty() {
        let editor = Editor::new();
        let result = find_sticky_scroll_lines(&editor, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn sticky_scroll_finds_fn_scope() {
        let editor = Editor::test_with_text("fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        let result = find_sticky_scroll_lines(&editor, 2);
        assert_eq!(result.len(), 1);
        assert!(result[0].1.contains("fn main"));
    }

    #[test]
    fn sticky_scroll_empty_file() {
        let editor = Editor::new();
        let result = find_sticky_scroll_lines(&editor, 5);
        assert!(result.is_empty());
    }
}
