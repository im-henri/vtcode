use crate::{app::App, app_theme::ThemeFile, ui_colors::contrast_ratio};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Color};

pub(crate) fn audit_themes() -> Vec<ThemeFile> {
    static THEMES: std::sync::OnceLock<Vec<ThemeFile>> = std::sync::OnceLock::new();
    THEMES.get_or_init(load_audit_themes).clone()
}

fn load_audit_themes() -> Vec<ThemeFile> {
    let mut themes = crate::app_theme::load_builtin_themes();
    for (kind, surface, text) in [
        ("vs-dark", "#eeeeee", "#111111"),
        ("vs", "#111111", "#eeeeee"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("controls.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "name":format!("Different control surfaces ({kind})"), "colors": {
                    "titleBar.activeBackground":surface, "titleBar.activeForeground":text,
                    "titleBar.inactiveBackground":text, "titleBar.inactiveForeground":surface,
                    "sideBar.background":surface, "sideBar.foreground":text,
                    "sideBarTitle.foreground":text, "descriptionForeground":surface,
                    "input.background":surface, "input.foreground":text,
                    "focusBorder":surface, "textLink.foreground":surface,
                    "statusBar.background":surface, "statusBar.foreground":surface,
                    "statusBarItem.hoverBackground":text, "statusBarItem.hoverForeground":text
                }
            })
            .to_string(),
        )
        .unwrap();
        themes.push(crate::vscode_theme::load_theme_file(&path, None, Some(kind)).unwrap());
    }
    // The same rendering checks can audit any locally installed VS Code themes
    // without requiring those extensions or an absolute path in CI.
    if let Some(root) = std::env::var_os("VTCODE_THEME_AUDIT_DIR") {
        let installed = crate::vscode_theme::load_extension_themes(std::path::Path::new(&root));
        assert!(
            !installed.is_empty(),
            "the requested theme audit directory has no loadable themes"
        );
        eprintln!("Auditing {} installed theme variants", installed.len());
        themes.extend(installed);
    }
    themes
}

pub(crate) fn check_region(
    buffer: &Buffer,
    area: Rect,
    label: &str,
    failures: &mut Vec<String>,
    palette: &crate::theme_workbench::WorkbenchColors,
) {
    check_region_with_file_icons(buffer, area, label, failures, palette, &[]);
}

fn check_region_with_file_icons(
    buffer: &Buffer,
    area: Rect,
    label: &str,
    failures: &mut Vec<String>,
    palette: &crate::theme_workbench::WorkbenchColors,
    files: &[&str],
) {
    // These ASCII badges represent file-icon assets, not workbench body text.
    // Check their exact palette color, then apply text contrast checks to every
    // other cell. Recoloring an icon to satisfy contrast would regress the
    // independent file-icon-theme behavior being tested separately below.
    let mut icon_areas = Vec::new();
    for file in files {
        let (glyph, color) = crate::file_icons::for_name(file);
        for y in area.y..area.bottom() {
            for x in area.x.saturating_sub(2)..area.right() {
                if glyph.chars().enumerate().all(|(i, ch)| {
                    buffer
                        .cell((x + i as u16, y))
                        .is_some_and(|cell| cell.symbol() == ch.to_string())
                }) {
                    for (i, ch) in glyph
                        .chars()
                        .enumerate()
                        .filter(|(_, ch)| !ch.is_whitespace())
                    {
                        assert_eq!(
                            buffer[(x + i as u16, y)].fg,
                            color,
                            "{label}: file icon {ch}"
                        );
                    }
                    icon_areas.push(Rect::new(x, y, glyph.chars().count() as u16, 1));
                }
            }
        }
    }
    let mut contrast_floors = std::collections::HashMap::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if icon_areas
                .iter()
                .any(|icon| y == icon.y && x >= icon.x && x < icon.right())
            {
                continue;
            }
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            if cell.symbol().trim().is_empty() {
                continue;
            }
            if palette.preserve_theme_colors {
                // Imported themes choose their own contrast. The upstream
                // registry/renderer conformance tests assert exact role values;
                // this broad surface audit catches unresolved/reset colors.
                if !matches!(cell.fg, Color::Rgb(..)) || !matches!(cell.bg, Color::Rgb(..)) {
                    failures.push(format!("{label}: unresolved colors {:?} on {:?}", cell.fg, cell.bg));
                    return;
                }
                continue;
            }
            // An authored palette can lack a color reaching 4.5 on a particular
            // fill (including custom gradient midpoints). Verify the best
            // achievable contrast there instead of demanding invented colors.
            let best = *contrast_floors.entry(cell.bg).or_insert_with(|| {
                palette.contrast_palette.iter()
                    .filter_map(|&color| contrast_ratio(color, cell.bg))
                    .fold(0.0f32, f32::max)
            });
            // Non-text icons use the same contrast floor as the icon renderer;
            // labels and descriptions still require readable text contrast.
            let minimum = if matches!(
                cell.symbol(),
                "◈" | "●" | "○" | "✖" | "ℹ" | "⚠" | "✗" | "📦" | "▸" | "ƒ"
            ) {
                3.0f32.min(best)
            } else {
                4.5f32.min(best)
            };
            let ratio = contrast_ratio(cell.fg, cell.bg);
            if ratio.is_none_or(|ratio| ratio + 0.001 < minimum) {
                failures.push(format!(
                    "{label}: {:?} at ({x},{y}), {:?} on {:?}, contrast {ratio:?}",
                    cell.symbol(),
                    cell.fg,
                    cell.bg
                ));
                return;
            }
        }
    }
}

pub(crate) fn assert_readable(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} unreadable regions; first failures:\n{}",
        failures.len(),
        failures.into_iter().take(40).collect::<Vec<_>>().join("\n")
    );
}

pub(crate) fn snapshot(buffer: &Buffer, name: &str) {
    let Some(root) = std::env::var_os("VTCODE_THEME_SNAPSHOTS") else {
        return;
    };
    let root = std::path::Path::new(&root);
    std::fs::create_dir_all(root).unwrap();
    let cells: Vec<_> = buffer
        .content
        .iter()
        .map(|cell| {
            serde_json::json!({
                "text":cell.symbol(), "fg":format!("{:?}",cell.fg), "bg":format!("{:?}",cell.bg),
                "bold":cell.modifier.contains(ratatui::style::Modifier::BOLD),
                "underline":cell.modifier.contains(ratatui::style::Modifier::UNDERLINED),
                "underlineColor":format!("{:?}",cell.underline_color)
            })
        })
        .collect();
    std::fs::write(root.join(format!("{}.json",crate::vscode_theme::slug(name))),
        serde_json::to_vec(&serde_json::json!({"name":name,"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
}

#[test]
fn title_bar_controls_remain_readable_when_idle_hovered_open_and_unfocused() {
    let mut app = App::test_new();
    // Keep both history buttons enabled for this text-contrast audit.
    let mut editor = crate::editor::Editor::new();
    editor.rope = ropey::Rope::from_str(&"line\n".repeat(100));
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    app.record_navigation();
    for line in [30, 60] {
        app.editor_mut().unwrap().set_cursor(line, 0);
        app.finish_navigation();
    }
    app.navigate_back();
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for width in [32, 80, 160] {
            let area = Rect::new(0, 0, width, 1);
            app.menu_bar_area = Some(area);
            for focused in [false, true] {
                app.terminal_has_focus = focused;
                for open in [false, true] {
                    app.chat.visible = open;
                    app.chat.layout.dock_visible = open;
                    for hover in 0..6 {
                        app.mouse_col = if hover == 1 {
                            crate::ui_chrome::agent_chat_button_area(area)
                                .map_or(u16::MAX, |area| area.x + 1)
                        } else if hover >= 4 {
                            (0..width).find(|&x| {
                                crate::ui_chrome::navigation_button_at(&app, x, 0)
                                    .is_some_and(|(_, forward)| forward == (hover == 5))
                            }).unwrap_or(u16::MAX)
                        } else {
                            u16::MAX
                        };
                        app.mouse_row = if hover == 1 || hover >= 4 { 0 } else { u16::MAX };
                        app.hovered_close_button = hover == 2;
                        app.hovered_menu = (hover == 3).then_some(0);
                        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
                        terminal
                            .draw(|frame| crate::ui_chrome::draw_menu_bar(frame, &app, area))
                            .unwrap();
                        let label = format!(
                            "{}: title {width}, focused={focused}, chat={open}, hover={hover}",
                            theme.name
                        );
                        check_region(
                            terminal.backend().buffer(),
                            area,
                            &label,
                            &mut failures,
                            &app.colors.ui,
                        );
                        if width == 160 && focused && hover == 0 {
                            snapshot(
                                terminal.backend().buffer(),
                                &format!(
                                    "{} - chat {}",
                                    theme.name,
                                    if open { "open" } else { "closed" }
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn sidebar_and_settings_actions_remain_readable_on_their_own_surfaces() {
    let mut app = App::test_new();
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for hover in [false, true] {
            let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
            app.mouse_col = if hover { 99 } else { u16::MAX };
            app.mouse_row = if hover { 0 } else { u16::MAX };
            terminal
                .draw(|frame| {
                    crate::ui_helpers::draw_extensions_sidebar(frame, &mut app, frame.area())
                })
                .unwrap();
            for (_, area) in &app.ext_sidebar.actions {
                check_region(
                    terminal.backend().buffer(),
                    *area,
                    &format!("{}: extension action, hover={hover}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }

            app.settings_editor_search = "font".into();
            app.mouse_col = if hover { 95 } else { u16::MAX };
            app.mouse_row = if hover { 1 } else { u16::MAX };
            terminal
                .draw(|frame| {
                    crate::ui_settings::draw_settings_editor(frame, &mut app, frame.area())
                })
                .unwrap();
            let areas = app
                .settings_scope_areas
                .into_iter()
                .flatten()
                .chain(app.settings_json_area)
                .chain(app.settings_clear_search_area);
            for area in areas {
                check_region(
                    terminal.backend().buffer(),
                    area,
                    &format!("{}: settings action, hover={hover}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn status_actions_remain_readable_at_rest_and_after_hover_and_gradients() {
    let mut app = App::test_new();
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for hovered in [None, Some(0), Some(1), Some(2)] {
            app.hovered_status_item = hovered;
            app.saving_flash = Some(std::time::Instant::now());
            let mut terminal = Terminal::new(TestBackend::new(160, 1)).unwrap();
            terminal
                .draw(|frame| crate::ui_panels::draw_status_bar(frame, &mut app, frame.area()))
                .unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(0, 0, 160, 1),
                &format!("{}: status, hover={hovered:?}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
    }
    assert_readable(failures);
}

#[test]
fn source_control_header_and_file_actions_remain_readable() {
    let mut app = App::test_new();
    let mut failures = Vec::new();
    app.source_control_files = [true, false]
        .into_iter()
        .map(|staged| crate::app::GitFileChange {
            path: "example.rs".into(),
            original_path: None,
            status: 'M',
            staged,
        })
        .collect();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for missing in [false, true] {
            app.source_control_repository_state = if missing {
                crate::app::SourceControlRepositoryState::Missing
            } else {
                crate::app::SourceControlRepositoryState::Available
            };
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
            let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_source_control::draw_source_control_sidebar(
                        frame,
                        &mut app,
                        frame.area(),
                    )
                })
                .unwrap();
            let mut areas: Vec<_> = [
                app.source_control_initialize_area,
                app.source_control_commit_area,
                app.source_control_amend_area,
                app.source_control_stage_all_area,
                app.source_control_unstage_all_area,
                app.source_control_discard_all_area,
            ]
            .into_iter()
            .flatten()
            .collect();
            if !missing {
                areas.push(Rect::new(0, 0, 99, 1));
            }
            assert!(!areas.is_empty());
            for area in areas {
                check_region(
                    terminal.backend().buffer(),
                    area,
                    &format!("{}: source control at rest", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
                app.mouse_col = area.x + 1;
                app.mouse_row = area.y;
                terminal
                    .draw(|frame| {
                        crate::ui_source_control::draw_source_control_sidebar(
                            frame,
                            &mut app,
                            frame.area(),
                        )
                    })
                    .unwrap();
                check_region(
                    terminal.backend().buffer(),
                    area,
                    &format!("{}: source control hovered", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert_readable(failures);
}

fn source_control_review_app(theme: &ThemeFile) -> App {
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app.sc_graph_height = 8;
    app.source_control_files = [('M', "changed.rs"), ('D', "removed.rs"), ('?', "new.rs")]
        .into_iter()
        .map(|(status, name)| crate::app::GitFileChange {
            path: format!("src/{name}"),
            original_path: None,
            status,
            staged: false,
        })
        .collect();
    app.git_log = ["Current change", "Previous change"]
        .into_iter()
        .enumerate()
        .map(|(index, message)| crate::app::GitCommitInfo {
            graph: String::new(),
            hash: format!("{index:07x}"),
            full_hash: format!("{index:040x}"),
            message: message.into(),
            author: "Pat".into(),
            time_ago: "now".into(),
            branches: if index == 0 {
                [false, true]
                    .into_iter()
                    .map(|remote| crate::app::GitCommitReference {
                        name: "origin/main".into(),
                        remote,
                    })
                    .collect()
            } else {
                Vec::new()
            },
        })
        .collect();
    app.commit_details.data.hash = "abc1234".into();
    app.commit_details.data.message = "Commit summary".into();
    app.commit_details.data.author = "Pat".into();
    app.commit_details.data.time_ago = "now".into();
    app.commit_details.data.files = [
        ('M', "src/changed.rs"),
        ('?', "src/new.rs"),
        ('D', "src/removed.rs"),
    ]
    .into_iter()
    .map(|(status, path)| crate::commit_details::CommitFile::new(status, path))
    .collect();
    app
}

#[test]
fn source_control_uses_sidebar_list_input_and_git_roles_on_their_own_surfaces() {
    use crate::{app::ActivePane, theme_workbench::ColorLayer};
    let mut theme = theme_with_popup_and_control_roles();
    for (id, value) in [
        ("sideBarTitle.foreground", "#ffeebbd0"),
        ("sideBarSectionHeader.background", "#26383f"),
        ("sideBarSectionHeader.foreground", "#bbffffd0"),
        ("gitDecoration.modifiedResourceForeground", "#ffddaad0"),
        ("gitDecoration.untrackedResourceForeground", "#bbffaae0"),
        ("gitDecoration.deletedResourceForeground", "#ffbbaac0"),
        ("selection.background", "#805060b0"),
    ] {
        theme.workbench_colors.insert(id.into(), value.into());
    }
    let sidebar = Color::Rgb(24, 48, 37);
    let mut app = source_control_review_app(&theme);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    for focused in [true, false] {
        app.active_pane = if focused {
            ActivePane::Explorer
        } else {
            ActivePane::Editor
        };
        terminal
            .draw(|frame| {
                crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let selected_bg = if focused {
            ColorLayer::rgba(48, 72, 56, 192)
        } else {
            ColorLayer::rgba(48, 72, 104, 192)
        }
        .over(sidebar)
        .unwrap();
        let selected_fg = if focused {
            ColorLayer::rgba(255, 255, 187, 224)
        } else {
            ColorLayer::rgba(187, 221, 255, 224)
        }
        .over(selected_bg)
        .unwrap();
        let (x, y) = find_cell(buffer, "changed.rs");
        assert_eq!(buffer[(x, y)].bg, selected_bg);
        assert_eq!(buffer[(x, y)].fg, selected_fg);
        assert_eq!(
            buffer[(98, y)].bg,
            selected_bg,
            "selection fills the complete file row"
        );
        let untracked = &buffer[find_cell(buffer, "new.rs")];
        assert_eq!(untracked.bg, sidebar);
        assert_eq!(
            untracked.fg,
            ColorLayer::rgba(221, 238, 255, 192).over(sidebar).unwrap()
        );
        let (_, y) = find_cell(buffer, "new.rs");
        assert_eq!(
            buffer[(98, y)].fg,
            ColorLayer::rgba(187, 255, 170, 224).over(sidebar).unwrap()
        );
        let actions = &buffer[find_cell(buffer, "Actions")];
        assert_eq!(
            actions.fg,
            ColorLayer::rgba(221, 238, 255, 192).over(sidebar).unwrap()
        );
        let section = &buffer[find_cell(buffer, "Unstaged Changes (2)")];
        assert_eq!(section.bg, Color::Rgb(38, 56, 63));
        assert_eq!(
            section.fg,
            ColorLayer::rgba(187, 255, 255, 208)
                .over(section.bg)
                .unwrap()
        );
        let input = &buffer[find_cell(buffer, "Message")];
        let input_bg = ColorLayer::rgba(48, 56, 80, 192).over(sidebar).unwrap();
        assert_eq!(input.bg, input_bg);
        assert_eq!(
            input.fg,
            ColorLayer::rgba(221, 238, 255, 224).over(input_bg).unwrap()
        );
    }
    let (x, y) = find_cell(terminal.backend().buffer(), "new.rs");
    app.mouse_col = x;
    app.mouse_row = y;
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
        })
        .unwrap();
    let hovered = &terminal.backend().buffer()[(x, y)];
    let hover_bg = ColorLayer::rgba(80, 56, 72, 192).over(sidebar).unwrap();
    assert_eq!(hovered.bg, hover_bg);
    assert_eq!(
        hovered.fg,
        ColorLayer::rgba(221, 255, 204, 224).over(hover_bg).unwrap()
    );
    app.source_control_input_mode = true;
    app.cursor_visible = false;
    app.source_control_commit_msg = "Commit draft".into();
    app.source_control_commit_cursor = 6;
    app.source_control_commit_selection_anchor = Some(0);
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let input_bg = ColorLayer::rgba(48, 56, 80, 192).over(sidebar).unwrap();
    let selection_bg = ColorLayer::rgba(128, 80, 96, 176).over(input_bg).unwrap();
    let selected = &buffer[find_cell(buffer, "Commit")];
    assert_eq!(selected.bg, selection_bg);
    assert_eq!(
        selected.fg,
        ColorLayer::rgba(255, 223, 187, 238)
            .over(selection_bg)
            .unwrap()
    );
    let draft = &buffer[find_cell(buffer, "draft")];
    assert_eq!(draft.bg, input_bg);
    assert_eq!(
        draft.fg,
        ColorLayer::rgba(255, 223, 187, 238).over(input_bg).unwrap()
    );
    snapshot(buffer, "independent source control roles");
}

#[test]
fn source_control_input_keeps_transparent_fills_borders_and_selection_and_uses_its_own_caret() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_popup_and_control_roles();
    for id in [
        "input.background",
        "input.border",
        "focusBorder",
        "selection.background",
    ] {
        theme.workbench_colors.insert(id.into(), "#00000000".into());
    }
    let mut app = source_control_review_app(&theme);
    let sidebar = Color::Rgb(24, 48, 37);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
        })
        .unwrap();
    let placeholder =
        &terminal.backend().buffer()[find_cell(terminal.backend().buffer(), "Message")];
    assert_eq!(placeholder.bg, sidebar);
    assert_eq!(
        placeholder.fg,
        ColorLayer::rgba(221, 238, 255, 224).over(sidebar).unwrap()
    );
    assert_eq!(terminal.backend().buffer()[(0, 1)].fg, sidebar);
    app.source_control_input_mode = true;
    app.cursor_visible = false;
    app.source_control_commit_msg = "draft".into();
    app.source_control_commit_selection_anchor = Some(0);
    app.source_control_commit_cursor = 5;
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
        })
        .unwrap();
    let (x, y) = find_cell(terminal.backend().buffer(), "draft");
    let selected = &terminal.backend().buffer()[(x, y)];
    assert_eq!(selected.bg, sidebar);
    assert_eq!(
        selected.fg,
        ColorLayer::rgba(255, 223, 187, 238).over(sidebar).unwrap()
    );
    assert_eq!(terminal.backend().buffer()[(0, 1)].fg, sidebar);
    app.source_control_commit_selection_anchor = None;
    app.source_control_commit_cursor = 0;
    app.cursor_visible = true;
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
        })
        .unwrap();
    let caret = &terminal.backend().buffer()[(x, y)];
    assert_eq!(
        caret.bg,
        ColorLayer::rgba(255, 223, 187, 238).over(sidebar).unwrap()
    );
    assert_eq!(caret.fg, sidebar);
}

#[test]
fn source_control_graph_reference_colors_and_commit_tab_are_independent_of_menus() {
    use crate::{app::ActivePane, theme_workbench::ColorLayer};
    let mut theme = theme_with_popup_and_control_roles();
    for (id, value) in [
        ("scmGraph.foreground1", "#ffbbddc0"),
        ("scmGraph.historyItemRefColor", "#ddeeaacc"),
        ("scmGraph.historyItemRemoteRefColor", "#bbcceeee"),
        ("scmGraph.historyItemHoverLabelForeground", "#091521"),
        ("gitDecoration.deletedResourceForeground", "#ffbb99c0"),
    ] {
        theme.workbench_colors.insert(id.into(), value.into());
    }
    let mut app = source_control_review_app(&theme);
    app.active_pane = ActivePane::Explorer;
    app.sc_graph_focused = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let sidebar = Color::Rgb(24, 48, 37);
    let selected_bg = ColorLayer::rgba(48, 72, 56, 192).over(sidebar).unwrap();
    let (x, y) = find_cell(buffer, "origin/main");
    assert_eq!(
        buffer[(x, y)].bg,
        ColorLayer::rgba(221, 238, 170, 204)
            .over(selected_bg)
            .unwrap()
    );
    assert_eq!(buffer[(x, y)].fg, Color::Rgb(9, 21, 33));
    // Local branches can have exactly the same display name as remote refs.
    assert_eq!(
        buffer[(x + 14, y)].bg,
        ColorLayer::rgba(187, 204, 238, 238)
            .over(selected_bg)
            .unwrap()
    );
    assert_eq!(buffer[(x + 14, y)].fg, Color::Rgb(9, 21, 33));
    let (x, y) = find_cell(buffer, "Previous change");
    assert_eq!(
        buffer[(x, y)].fg,
        ColorLayer::rgba(221, 238, 255, 192).over(sidebar).unwrap()
    );
    assert_eq!(
        buffer[(1, y)].fg,
        ColorLayer::rgba(255, 187, 221, 192).over(sidebar).unwrap()
    );
    let (_, y) = find_cell(buffer, "Current change");
    assert_eq!(buffer[(98, y)].bg, selected_bg);
    snapshot(buffer, "independent source control graph roles");

    terminal
        .draw(|frame| crate::ui_dialogs::draw_source_control_commit_details(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let widget = app.colors.bg_editor;
    let title = &buffer[find_cell(buffer, "Commit abc1234")];
    assert_eq!(title.bg, widget);
    assert_eq!(
        title.fg,
        ColorLayer::rgba(255, 238, 221, 224).over(widget).unwrap()
    );
    let selected_bg = ColorLayer::rgba(48, 72, 56, 192).over(widget).unwrap();
    // The selected path also appears in the full-height patch heading.
    let (x, y) = find_cell(buffer, "M src/changed.rs");
    let selected = &buffer[(x + 2, y)];
    assert_eq!(selected.bg, selected_bg);
    assert_eq!(
        selected.fg,
        ColorLayer::rgba(255, 255, 187, 224)
            .over(selected_bg)
            .unwrap()
    );
    let (status_x, y) = find_cell(buffer, "D src/removed.rs");
    let x = status_x + 6;
    assert_eq!(
        buffer[(status_x, y)].fg,
        ColorLayer::rgba(255, 187, 153, 192).over(widget).unwrap()
    );
    assert_eq!(buffer[(x, y)].fg, title.fg);
    snapshot(buffer, "independent commit details roles");
}

#[test]
fn source_control_content_remains_readable_in_list_input_and_popup_states() {
    use crate::app::ActivePane;
    let mut failures = Vec::new();
    for theme in audit_themes() {
        let mut app = source_control_review_app(&theme);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        for focused in [true, false] {
            app.active_pane = if focused {
                ActivePane::Explorer
            } else {
                ActivePane::Editor
            };
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
            terminal
                .draw(|frame| {
                    crate::ui_source_control::draw_source_control_sidebar(
                        frame,
                        &mut app,
                        frame.area(),
                    )
                })
                .unwrap();
            let (x, y) = find_cell(terminal.backend().buffer(), "new.rs");
            app.mouse_col = x;
            app.mouse_row = y;
            terminal
                .draw(|frame| {
                    crate::ui_source_control::draw_source_control_sidebar(
                        frame,
                        &mut app,
                        frame.area(),
                    )
                })
                .unwrap();
            check_region_with_file_icons(
                terminal.backend().buffer(),
                Rect::new(0, 6, 99, 24),
                &format!("{} source control focused={focused}", theme.name),
                &mut failures,
                &app.colors.ui,
                &["main.rs"],
            );
        }
        app.source_control_input_mode = true;
        app.source_control_commit_msg = "Commit draft".into();
        app.source_control_commit_selection_anchor = Some(0);
        app.source_control_commit_cursor = 6;
        app.cursor_visible = true;
        terminal
            .draw(|frame| {
                crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, frame.area())
            })
            .unwrap();
        check_region(
            terminal.backend().buffer(),
            Rect::new(1, 2, 97, 2),
            &format!("{} commit input", theme.name),
            &mut failures,
            &app.colors.ui,
        );
        terminal
            .draw(|frame| crate::ui_dialogs::draw_source_control_commit_details(frame, &mut app))
            .unwrap();
        let popup = Rect::new(0, 0, 100, 30);
        check_region(
            terminal.backend().buffer(),
            Rect::new(popup.x + 1, popup.y + 1, popup.width - 2, popup.height - 2),
            &format!("{} commit popup", theme.name),
            &mut failures,
            &app.colors.ui,
        );
        for state in [
            crate::app::SourceControlRepositoryState::Missing,
            crate::app::SourceControlRepositoryState::Checking,
            crate::app::SourceControlRepositoryState::Loading,
        ] {
            app.source_control_repository_state = state;
            terminal
                .draw(|frame| {
                    crate::ui_source_control::draw_source_control_sidebar(
                        frame,
                        &mut app,
                        frame.area(),
                    )
                })
                .unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(0, 0, 99, 12),
                &format!("{} repository state", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
    }
    assert_readable(failures);
}

#[test]
fn tab_and_diff_controls_survive_different_surfaces_and_gradients() {
    let mut app = App::test_new();
    let mut failures = Vec::new();
    for index in 0..5 {
        let mut editor = crate::editor::Editor::test_with_text("after\n");
        editor.file_path = Some(app.explorer.root.join(format!("file{index}.rs")));
        app.tabs.push(crate::app::Tab::new_file(editor));
    }
    app.active_tab = 1;
    app.tab_scroll_offset = 1;
    app.diff_mode = crate::app::DiffMode::SideBySide;
    app.compare_base_content = Some("before\n".into());
    app.set_compare_diff_lines(crate::app::compute_file_diff("before\n", "after\n"));
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        app.colors.gradients.bg_tabbar.start = Color::Rgb(250, 250, 250);
        app.colors.gradients.bg_tabbar.end = Color::Rgb(10, 10, 10);
        for hover in [false, true] {
            app.hovered_tab_scroll_left = hover;
            app.hovered_tab_scroll_right = hover;
            app.hovered_new_tab_button = hover;
            let mut terminal = Terminal::new(TestBackend::new(34, 1)).unwrap();
            terminal
                .draw(|frame| crate::ui_chrome_tabs::draw_tab_bar(frame, &app, frame.area()))
                .unwrap();
            check_region_with_file_icons(
                terminal.backend().buffer(),
                Rect::new(0, 0, 34, 1),
                &format!("{}: tab controls with gradient, hover={hover}", theme.name),
                &mut failures,
                &app.colors.ui,
                &["file.rs"],
            );
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        terminal
            .draw(|frame| crate::ui_chrome_tabs::draw_breadcrumb(frame, &mut app, frame.area()))
            .unwrap();
        for area in [
            app.diff_nav_prev_area,
            app.diff_nav_next_area,
            app.diff_layout_toggle_area,
        ]
        .into_iter()
        .flatten()
        {
            check_region(
                terminal.backend().buffer(),
                area,
                &format!("{}: diff control at rest", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            app.mouse_col = area.x + 1;
            app.mouse_row = area.y;
            terminal
                .draw(|frame| crate::ui_chrome_tabs::draw_breadcrumb(frame, &mut app, frame.area()))
                .unwrap();
            check_region(
                terminal.backend().buffer(),
                area,
                &format!("{}: diff control hovered", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
        app.colors.gradients.bg_statusbar.start = Color::Rgb(250, 250, 250);
        app.colors.gradients.bg_statusbar.end = Color::Rgb(10, 10, 10);
        app.hovered_status_item = None;
        let mut terminal = Terminal::new(TestBackend::new(160, 1)).unwrap();
        terminal
            .draw(|frame| crate::ui_panels::draw_status_bar(frame, &mut app, frame.area()))
            .unwrap();
        check_region(
            terminal.backend().buffer(),
            Rect::new(0, 0, 160, 1),
            &format!("{}: status controls with gradient", theme.name),
            &mut failures,
            &app.colors.ui,
        );
    }
    assert_readable(failures);
}

#[test]
fn command_center_uses_its_own_colors_instead_of_input_colors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("command-center.json");
    std::fs::write(
        &path,
        serde_json::json!({"name":"Command center", "colors":{
            "titleBar.activeBackground":"#111111", "titleBar.activeForeground":"#ffffff",
            "commandCenter.background":"#11223380", "commandCenter.foreground":"#eeeeee",
            "commandCenter.activeBackground":"#eeeeee", "commandCenter.activeForeground":"#111111",
            "commandCenter.inactiveForeground":"#bbbbbb", "commandCenter.border":"#008800",
            "commandCenter.activeBorder":"#880000", "commandCenter.inactiveBorder":"#0000",
            "input.background":"#ff00ff", "input.foreground":"#00ffff"
        }})
        .to_string(),
    )
    .unwrap();
    let palette = crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark"))
        .unwrap()
        .to_theme_colors();
    let idle = palette
        .ui
        .command_center_style(palette.ui.bg_title, true, false);
    assert_eq!(idle.bg, Some(Color::Rgb(17, 26, 34)));
    assert_eq!(idle.fg, Some(Color::Rgb(238, 238, 238)));
    assert_eq!(idle.underline_color, Some(Color::Rgb(0, 136, 0)));
    let hover = palette
        .ui
        .command_center_style(palette.ui.bg_title, true, true);
    assert_eq!(hover.bg, Some(Color::Rgb(238, 238, 238)));
    assert_eq!(hover.fg, Some(Color::Rgb(17, 17, 17)));
    assert_eq!(hover.underline_color, Some(Color::Rgb(136, 0, 0)));
    let inactive = palette
        .ui
        .command_center_style(palette.ui.bg_title, false, false);
    assert_eq!(inactive.fg, Some(Color::Rgb(187, 187, 187)));
    assert_eq!(inactive.underline_color, None);
}

#[test]
fn imported_roles_are_preserved_and_native_contrast_fallbacks_use_the_theme_palette() {
    let mut failures = Vec::new();
    for theme in audit_themes() {
        let colors = theme.to_theme_colors();
        for background in colors.ui.contrast_palette {
            for preferred in colors.ui.contrast_palette {
                let resolved = colors.ui.readable_foreground(preferred, background, 4.5);
                assert!(colors.ui.contrast_palette.contains(&resolved));
                if colors.ui.preserve_theme_colors || contrast_ratio(preferred, background).is_some_and(|r| r >= 4.5) {
                    assert_eq!(
                        resolved, preferred,
                        "{}: authored foreground changed",
                        theme.name
                    );
                } else {
                    let actual = contrast_ratio(resolved, background).unwrap();
                    let best = colors
                        .ui
                        .contrast_palette
                        .into_iter()
                        .filter_map(|c| contrast_ratio(c, background))
                        .fold(0.0f32, f32::max);
                    if actual + 0.001 < 4.5f32.min(best) {
                        failures.push(format!(
                            "{}: palette fallback missed a readable color",
                            theme.name
                        ));
                    }
                }
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn theme_name_does_not_affect_control_colors() {
    for mut theme in audit_themes() {
        let original = theme.to_theme_colors();
        for name in ["Dark", "LIGHT", "High Contrast", "Unrelated arbitrary name"] {
            theme.name = name.into();
            theme.config_key = name.into();
            let renamed = theme.to_theme_colors();
            assert_eq!(renamed.ui.contrast_palette, original.ui.contrast_palette);
            for hovered in [false, true] {
                for primary in [false, true] {
                    assert_eq!(
                        crate::ui_widgets::action_button_style(
                            &renamed,
                            renamed.bg_sidebar,
                            primary,
                            hovered,
                            true
                        ),
                        crate::ui_widgets::action_button_style(
                            &original,
                            original.bg_sidebar,
                            primary,
                            hovered,
                            true
                        )
                    );
                }
                assert_eq!(
                    renamed
                        .ui
                        .command_center_style(renamed.ui.bg_title, true, hovered),
                    original
                        .ui
                        .command_center_style(original.ui.bg_title, true, hovered)
                );
                assert_eq!(
                    renamed.ui.toolbar_style(
                        renamed.ui.fg_sidebar,
                        renamed.bg_sidebar,
                        hovered,
                        false,
                        false
                    ),
                    original.ui.toolbar_style(
                        original.ui.fg_sidebar,
                        original.bg_sidebar,
                        hovered,
                        false,
                        false
                    )
                );
            }
        }
    }
}

#[test]
fn markdown_preview_toggle_is_readable_when_closed_open_and_hovered() {
    let mut app = App::test_new();
    let mut failures = Vec::new();
    let path = app.explorer.root.join("readme.md");
    let make_tab = |kind| {
        let mut editor = crate::editor::Editor::test_with_text("# Preview");
        editor.file_path = Some(path.clone());
        let mut tab = crate::app::Tab::new_file(editor);
        tab.kind = kind;
        tab
    };
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for open in [false, true] {
            app.tabs = vec![make_tab(crate::app::TabKind::File)];
            if open {
                app.tabs
                    .push(make_tab(crate::app::TabKind::MarkdownPreview));
            }
            app.active_tab = 0;
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
            let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
            for hover in [false, true] {
                terminal
                    .draw(|frame| {
                        crate::ui_chrome_tabs::draw_breadcrumb(frame, &mut app, frame.area())
                    })
                    .unwrap();
                let area = app
                    .breadcrumb_markdown_preview_area
                    .expect("preview toggle must be visible");
                check_region(
                    terminal.backend().buffer(),
                    area,
                    &format!("{}: preview open={open}, hover={hover}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
                app.mouse_col = area.x + 1;
                app.mouse_row = area.y;
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn imported_theme_name_never_selects_light_or_dark_colors() {
    let directory = tempfile::tempdir().unwrap();
    for (background, foreground) in [("#141c24", "#e4ecf4"), ("#e4ecf4", "#141c24")] {
        let mut original = None;
        for name in ["Dark", "Light", "Dark Light", "Anything"] {
            let path = directory.path().join(format!("{name}.json"));
            std::fs::write(
                &path,
                serde_json::json!({"name":name,"colors":{
                    "editor.background":background, "editor.foreground":foreground,
                    "sideBar.background":background,"sideBar.foreground":foreground,
                    "button.background":background,"button.foreground":foreground,
                    "button.secondaryBackground":background,"button.secondaryForeground":foreground
                }})
                .to_string(),
            )
            .unwrap();
            // No manifest/uiTheme metadata: only actual palette values can
            // determine defaults. The filename and display name are arbitrary.
            let theme = crate::vscode_theme::load_theme_file(&path, None, None).unwrap();
            let colors = theme.to_theme_colors();
            let result = (
                theme.is_light,
                colors.ui.contrast_palette,
                crate::ui_widgets::action_button_style(
                    &colors,
                    colors.bg_sidebar,
                    true,
                    false,
                    true,
                ),
                colors.ui.toolbar_style(
                    colors.ui.fg_sidebar,
                    colors.bg_sidebar,
                    true,
                    false,
                    false,
                ),
            );
            if let Some(original) = &original {
                assert_eq!(&result, original);
            } else {
                original = Some(result);
            }
        }
    }
}

fn theme_with_icon_and_scrollbar_roles() -> ThemeFile {
    static THEME: std::sync::OnceLock<ThemeFile> = std::sync::OnceLock::new();
    THEME.get_or_init(load_icon_and_scrollbar_theme).clone()
}

fn load_icon_and_scrollbar_theme() -> ThemeFile {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("independent-roles.json");
    std::fs::write(&path, serde_json::json!({"name":"Independent roles", "colors":{
        "editor.background":"#101824", "editor.foreground":"#eeeeee",
        "sideBar.background":"#203344", "sideBar.foreground":"#dddddd",
        "foreground":"#e0e0e0", "descriptionForeground":"#bbbbbb",
        "icon.foreground":"#99cceec0",
        "symbolIcon.functionForeground":"#bbff99", "symbolIcon.structForeground":"#ffbbdd",
        "symbolIcon.moduleForeground":"#99ddff", "symbolIcon.methodForeground":"#ffee99",
        "editorGroupHeader.tabsBackground":"#101824", "tab.activeBackground":"#142a30",
        "editorSuggestWidget.background":"#101824", "editorSuggestWidget.foreground":"#eeeeee",
        "editorSuggestWidget.selectedBackground":"#24303c", "editorSuggestWidget.selectedForeground":"#eeeeee",
        "editorSuggestWidget.highlightForeground":"#ffdd99", "editorSuggestWidget.focusHighlightForeground":"#dd99ff",
        "editorSuggestWidget.selectedIconForeground":"#99ffdd",
        "scrollbar.background":"#22448840", "scrollbarSlider.background":"#ffffff60",
        "scrollbarSlider.hoverBackground":"#ffbb9980", "scrollbarSlider.activeBackground":"#bbff9980",
        "diffEditor.removedLineBackground":"#7744cc40", "diffEditor.removedTextBackground":"#9966ee80"
    }}).to_string()).unwrap();
    crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark")).unwrap()
}

pub(crate) fn theme_with_popup_and_control_roles() -> ThemeFile {
    static THEME: std::sync::OnceLock<ThemeFile> = std::sync::OnceLock::new();
    THEME.get_or_init(load_popup_and_control_theme).clone()
}

fn load_popup_and_control_theme() -> ThemeFile {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("popup-roles.json");
    std::fs::write(&path, r##"{"name":"Independent popup roles", "colors":{
        "editor.background":"#091521", "editor.foreground":"#eef3ff",
        "sideBar.background":"#183025", "sideBar.foreground":"#ddeeffc0",
        "editorWidget.background":"#202830", "editorWidget.foreground":"#ffeedde0", "editorWidget.border":"#77bbdd80",
        "descriptionForeground":"#c0ddf0c0", "focusBorder":"#bbffddc0",
        "errorForeground":"#ffbaaadd", "editorWarning.foreground":"#ffddaacc",
        "input.background":"#303850c0", "input.foreground":"#ffdfbbee", "input.placeholderForeground":"#ddeeffe0",
        "settings.textInputBackground":"#281818c0", "settings.textInputForeground":"#ddeeffe0", "settings.textInputBorder":"#88ccee80",
        "button.background":"#203b70", "button.foreground":"#fff4bb", "button.hoverBackground":"#324b80", "button.border":"#99ddff80",
        "button.secondaryBackground":"#303520", "button.secondaryForeground":"#ddeeff", "button.secondaryHoverBackground":"#48492a",
        "list.activeSelectionBackground":"#304838c0", "list.activeSelectionForeground":"#ffffbbe0",
        "list.inactiveSelectionBackground":"#304868c0", "list.inactiveSelectionForeground":"#bbddffe0",
        "list.hoverBackground":"#503848c0", "list.hoverForeground":"#ddffcce0",
        "list.focusOutline":"#ffaadd80", "list.focusAndSelectionOutline":"#ffaa7780",
        "icon.foreground":"#eacbffc0", "list.activeSelectionIconForeground":"#bbff99", "list.inactiveSelectionIconForeground":"#ffccbb",
        "editorActionList.background":"#281f3c", "editorActionList.foreground":"#eeffffe0",
        "editorActionList.focusBackground":"#403850c0", "editorActionList.focusForeground":"#ffeebbe0",
        "editorHoverWidget.background":"#112633", "editorHoverWidget.foreground":"#ddffeee0",
        "editorHoverWidget.highlightForeground":"#ffdd99c0", "editorHoverWidget.border":"#bb88cc80",
        "peekView.border":"#ddaacc80", "peekViewEditor.background":"#201d3c", "peekViewEditorGutter.background":"#17222cc0",
        "peekViewTitle.background":"#2c4637c0", "peekViewTitleLabel.foreground":"#ddffffe0", "peekViewTitleDescription.foreground":"#ffeebbc0",
        "editorLineNumber.foreground":"#ccccddc0"
    }}"##).unwrap();
    crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark")).unwrap()
}

pub(crate) fn find_cell(buffer: &Buffer, text: &str) -> (u16, u16) {
    for y in buffer.area.y..buffer.area.bottom() {
        let mut line = String::new();
        let mut positions = Vec::new();
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let symbol = buffer[(x, y)].symbol();
            positions.push((line.len(), x));
            line.push_str(symbol);
            // Skip the continuation cells Ratatui reserves for wide graphemes.
            // Their blank symbols are not extra spaces in the logical text.
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        if let Some(byte) = line.find(text) {
            return (
                positions
                    .iter()
                    .rev()
                    .find(|(start, _)| *start <= byte)
                    .unwrap()
                    .1,
                y,
            );
        }
    }
    panic!("missing rendered text {text:?}");
}

fn render_workbench_input(app: &mut App, terminal: &mut Terminal<TestBackend>, case: usize) {
    const VALUE: &str = "selected ordinary";
    app.cursor_visible = false;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    match case {
        0 | 6 => {
            app.global_search_active = true;
            app.global_search_query = VALUE.into();
            app.global_search_field_cursors[0] = 8;
            app.global_search_field_selection_anchors[0] = Some(0);
            app.search_focus_field = 0;
        }
        1 => {
            app.tabs
                .push(crate::app::Tab::new_file(crate::editor::Editor::new()));
            app.open_goto_line();
            app.goto_line_query = VALUE.into();
            app.goto_line_cursor = 8;
            app.goto_line_selection_anchor = Some(0);
        }
        2 => {
            app.palette.query = VALUE.into();
            app.palette.cursor = 8;
            app.palette.selection_anchor = Some(0);
        }
        3 => {
            app.active_pane = crate::app::ActivePane::Explorer;
            app.sidebar_view = crate::app::SidebarView::Extensions;
            app.extensions_filter = VALUE.into();
            app.extensions_filter_cursor = 8;
            app.extensions_filter_selection_anchor = Some(0);
        }
        4 => {
            app.settings_editor_search = VALUE.into();
            app.settings_editor_search_cursor = 8;
            app.settings_editor_search_selection_anchor = Some(0);
            app.settings_focus_categories = false;
            app.settings_focus_control = false;
            app.settings_value_editing = false;
        }
        5 => {
            app.search_active = true;
            app.search_input_focused = true;
            app.search_query = VALUE.into();
            app.search_cursor = 8;
            app.search_selection_anchor = Some(0);
        }
        _ => {
            app.active_pane = crate::app::ActivePane::Chat;
            app.chat.input = VALUE.into();
            app.chat.cursor = 8;
            app.chat.anchor = Some(0);
        }
    }
    terminal
        .draw(|frame| match case {
            0 => crate::ui_search_panel::draw_search_panel(frame, app, frame.area()),
            1 => crate::ui_modal::draw(frame, app, crate::ui_modal::Kind::Goto),
            2 => crate::ui_quick_picker::draw(frame, app, crate::ui_quick_picker::Kind::Commands),
            3 => crate::ui_helpers::draw_extensions_sidebar(frame, app, frame.area()),
            4 => crate::ui_settings::draw_settings_editor(frame, app, frame.area()),
            5 => crate::ui_search_dialog::draw_search_bar(frame, app),
            6 => crate::ui_search_dialog::draw_global_search(frame, app),
            _ => crate::ui_chat::draw(frame, app, frame.area()),
        })
        .unwrap();
}

#[test]
fn workbench_input_views_use_selection_roles_on_their_own_fields() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_popup_and_control_roles();
    theme
        .workbench_colors
        .insert("selection.background".into(), "#805060b0".into());
    for case in 0..8 {
        let mut app = App::test_new();
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(110, 36)).unwrap();
        render_workbench_input(&mut app, &mut terminal, case);
        let buffer = terminal.backend().buffer();
        let surface = match case {
            0 | 3 | 7 => Color::Rgb(24, 48, 37),
            4 => Color::Rgb(9, 21, 33),
            _ => Color::Rgb(32, 40, 48),
        };
        let field_bg = if case == 4 {
            ColorLayer::rgba(40, 24, 24, 192)
        } else {
            ColorLayer::rgba(48, 56, 80, 192)
        }
        .over(surface)
        .unwrap();
        let foreground = if case == 4 {
            ColorLayer::rgba(221, 238, 255, 224)
        } else {
            ColorLayer::rgba(255, 223, 187, 238)
        };
        let selection_bg = ColorLayer::rgba(128, 80, 96, 176).over(field_bg).unwrap();
        let selected = &buffer[find_cell(buffer, "selected")];
        assert_eq!(
            selected.bg, selection_bg,
            "case {case}: workbench selection fill"
        );
        assert_eq!(
            selected.fg,
            foreground.over(selection_bg).unwrap(),
            "case {case}: selected foreground opacity"
        );
        let ordinary = &buffer[find_cell(buffer, "ordinary")];
        assert_eq!(ordinary.bg, field_bg, "case {case}: field surface");
        assert_eq!(
            ordinary.fg,
            foreground.over(field_bg).unwrap(),
            "case {case}: field foreground"
        );
        snapshot(buffer, &format!("independent workbench input {case}"));
    }
}

#[test]
fn workbench_input_views_keep_explicitly_transparent_selections() {
    let mut theme = theme_with_popup_and_control_roles();
    theme
        .workbench_colors
        .insert("selection.background".into(), "#00000000".into());
    for case in 0..8 {
        let mut app = App::test_new();
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(110, 36)).unwrap();
        render_workbench_input(&mut app, &mut terminal, case);
        let buffer = terminal.backend().buffer();
        let selected = &buffer[find_cell(buffer, "selected")];
        let ordinary = &buffer[find_cell(buffer, "ordinary")];
        assert_eq!(
            selected.bg, ordinary.bg,
            "case {case}: transparent selection"
        );
        assert_eq!(
            selected.fg, ordinary.fg,
            "case {case}: original input foreground"
        );
    }
}

#[test]
fn workbench_input_selections_and_carets_remain_readable_across_themes() {
    let mut failures = Vec::new();
    for theme in audit_themes() {
        for case in 0..8 {
            let mut app = App::test_new();
            app.colors = theme.to_theme_colors();
            let mut terminal = Terminal::new(TestBackend::new(110, 36)).unwrap();
            render_workbench_input(&mut app, &mut terminal, case);
            for label in ["selected", "ordinary"] {
                let (x, y) = find_cell(terminal.backend().buffer(), label);
                check_region(
                    terminal.backend().buffer(),
                    Rect::new(x, y, label.len() as u16, 1),
                    &format!("{} field={case}, {label}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
            app.cursor_visible = true;
            terminal
                .draw(|frame| match case {
                    0 => crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area()),
                    1 => crate::ui_modal::draw(frame, &mut app, crate::ui_modal::Kind::Goto),
                    2 => crate::ui_quick_picker::draw(
                        frame,
                        &mut app,
                        crate::ui_quick_picker::Kind::Commands,
                    ),
                    3 => crate::ui_helpers::draw_extensions_sidebar(frame, &mut app, frame.area()),
                    4 => crate::ui_settings::draw_settings_editor(frame, &mut app, frame.area()),
                    5 => crate::ui_search_dialog::draw_search_bar(frame, &mut app),
                    6 => crate::ui_search_dialog::draw_global_search(frame, &mut app),
                    _ => crate::ui_chat::draw(frame, &mut app, frame.area()),
                })
                .unwrap();
            let (x, y) = find_cell(terminal.backend().buffer(), "selected");
            let caret = &terminal.backend().buffer()[(x + 8, y)];
            let field =
                &terminal.backend().buffer()[find_cell(terminal.backend().buffer(), "ordinary")];
            if app.colors.ui.preserve_theme_colors {
                assert_eq!(caret.bg, field.fg, "{} field={case}: themed caret block", theme.name);
                assert_eq!(caret.fg, field.bg, "{} field={case}: inverted caret text", theme.name);
                continue;
            }
            for (label, foreground, background, minimum) in [
                ("caret block", caret.bg, field.bg, 3.0f32),
                ("caret text", caret.fg, caret.bg, 4.5f32),
            ] {
                let best = app
                    .colors
                    .ui
                    .contrast_palette
                    .iter()
                    .filter_map(|&color| contrast_ratio(color, background))
                    .fold(0.0f32, f32::max);
                if contrast_ratio(foreground, background)
                    .is_none_or(|ratio| ratio + 0.001 < minimum.min(best))
                {
                    failures.push(format!(
                        "{} {label}, field={case}: {foreground:?} on {background:?}",
                        theme.name
                    ));
                }
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn native_input_border_settings_reach_the_composer_and_workbench_overrides_win() {
    use crate::theme_workbench::ColorLayer;
    // Round-trip the persisted native schema rather than changing runtime caches.
    let mut theme: ThemeFile =
        serde_json::from_value(serde_json::to_value(theme_with_popup_and_control_roles()).unwrap())
            .unwrap();
    theme.colors.border_input = Some("#ddeeff80".into());
    for (override_color, expected) in [
        (None, ColorLayer::rgba(221, 238, 255, 128)),
        (Some("#ffccaa80"), ColorLayer::rgba(255, 204, 170, 128)),
        (Some("#00000000"), ColorLayer::rgba(0, 0, 0, 0)),
    ] {
        theme.workbench_colors.clear();
        if let Some(value) = override_color {
            theme
                .workbench_colors
                .insert("input.border".into(), value.into());
        }
        let mut app = App::test_new();
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(110, 36)).unwrap();
        render_workbench_input(&mut app, &mut terminal, 7);
        app.active_pane = crate::app::ActivePane::Editor;
        terminal
            .draw(|frame| crate::ui_chat::draw(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_cell(buffer, "selected");
        let field = &buffer[find_cell(buffer, "ordinary")];
        let border = &buffer[(x - 2, y - 1)];
        assert_eq!(border.bg, field.bg);
        assert_eq!(
            border.fg,
            expected.over(field.bg).unwrap(),
            "override={override_color:?}"
        );
    }
}

#[test]
fn quick_picker_transparency_and_inherited_text_follow_the_visible_surface() {
    use crate::theme_workbench::ColorLayer;
    use ratatui::{
        style::{Modifier, Style},
        widgets::Block,
    };
    let mut app = App::test_new();
    app.quick_open_results = ["selected_result.rs", "normal_result.rs"]
        .map(|name| app.explorer.root.join("src").join(name))
        .into();
    // This query remains visible without highlighting either fixture label.
    app.quick_open_query = "visible query".into();
    app.quick_open_cursor = app.quick_open_query.len();
    app.cursor_visible = false;
    let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
    for owner in [Color::Rgb(20, 40, 60), Color::Rgb(50, 10, 65)] {
        for alpha in [128, 0] {
            let mut theme = theme_with_icon_and_scrollbar_roles();
            for (key, value) in [
                ("editorWidget.background", format!("#285064{alpha:02x}")),
                ("editorWidget.foreground", "#ddeeffE0".into()),
                ("quickInputTitle.background", "#30204080".into()),
                ("quickInputList.focusBackground", "#38402080".into()),
                ("list.activeSelectionForeground", "#ffeeddE0".into()),
                ("input.background", "#11002280".into()),
                ("input.foreground", "#ddffeeE0".into()),
            ] {
                theme.workbench_colors.insert(key.into(), value.into());
            }
            for key in [
                "quickInput.background",
                "quickInput.foreground",
                "quickInputList.focusForeground",
            ] {
                theme.workbench_colors.remove(key);
            }
            app.colors = theme.to_theme_colors();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        Block::default()
                            .style(Style::default().bg(owner).add_modifier(Modifier::ITALIC)),
                        frame.area(),
                    );
                    crate::ui_quick_picker::draw(
                        frame,
                        &mut app,
                        crate::ui_quick_picker::Kind::Files,
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let popup = ColorLayer::rgba(40, 80, 100, alpha).over(owner).unwrap();
            let title = ColorLayer::rgba(48, 32, 64, 128).over(popup).unwrap();
            let selected = ColorLayer::rgba(56, 64, 32, 128).over(popup).unwrap();
            let input = ColorLayer::rgba(17, 0, 34, 128).over(popup).unwrap();
            for (label, bg, fg) in [
                ("Quick Open", title, ColorLayer::rgba(221, 238, 255, 224)),
                (
                    "selected_result.rs",
                    selected,
                    ColorLayer::rgba(255, 238, 221, 224),
                ),
                (
                    "normal_result.rs",
                    popup,
                    ColorLayer::rgba(221, 238, 255, 224),
                ),
                ("visible query", input, ColorLayer::rgba(221, 255, 238, 224)),
            ] {
                let cell = &buffer[find_cell(buffer, label)];
                assert_eq!(cell.bg, bg, "{label}, alpha={alpha}, owner={owner:?}");
                assert_eq!(
                    cell.fg,
                    fg.over(bg).unwrap(),
                    "{label}, alpha={alpha}, owner={owner:?}"
                );
                assert!(!cell.modifier.contains(Modifier::ITALIC));
            }
            snapshot(
                buffer,
                &format!("quick-picker-opacity-{alpha}-on-{owner:?}"),
            );
        }
    }
}

#[test]
fn quick_picker_colors_compose_on_the_title_input_and_result_surfaces() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_icon_and_scrollbar_roles();
    for (key, value) in [
        ("quickInput.background", "#182838"),
        ("quickInput.foreground", "#ddeeffc0"),
        ("quickInputTitle.background", "#40506080"),
        ("quickInputList.focusBackground", "#384020"),
        ("quickInputList.focusForeground", "#ffeeddc0"),
        ("quickInputList.focusHighlightForeground", "#ddffbbc0"),
        ("quickInputList.focusIconForeground", "#ffdd99c0"),
        ("list.highlightForeground", "#ffee99c0"),
        ("descriptionForeground", "#ccddaac0"),
        ("input.background", "#11002280"),
        ("input.foreground", "#ddffeec0"),
        ("focusBorder", "#77aaccc0"),
        ("editorWidget.border", "#aabbccc0"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    app.quick_open_results = ["sample_selected.rs", "sample_normal.rs"]
        .map(|name| app.explorer.root.join("src").join(name))
        .into();
    app.quick_open_query = "sa".into();
    app.quick_open_cursor = 2;
    let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_quick_picker::draw(frame, &mut app, crate::ui_quick_picker::Kind::Files)
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let title = &buffer[find_cell(buffer, "Quick Open")];
    assert_eq!(
        title.bg,
        ColorLayer::rgba(64, 80, 96, 128)
            .over(app.colors.ui.bg_quick_input)
            .unwrap()
    );
    assert_eq!(
        title.fg,
        ColorLayer::rgba(221, 238, 255, 192).over(title.bg).unwrap()
    );
    for (name, match_color, text_color) in [
        ("sample_selected.rs", (221, 255, 187), (255, 238, 221)),
        ("sample_normal.rs", (255, 238, 153), (221, 238, 255)),
    ] {
        let (x, y) = find_cell(buffer, name);
        assert_eq!(
            buffer[(x, y)].fg,
            ColorLayer::rgba(match_color.0, match_color.1, match_color.2, 192)
                .over(buffer[(x, y)].bg)
                .unwrap()
        );
        assert_eq!(
            buffer[(x + 2, y)].fg,
            ColorLayer::rgba(text_color.0, text_color.1, text_color.2, 192)
                .over(buffer[(x + 2, y)].bg)
                .unwrap()
        );
        let badge = &buffer[(app.quick_picker.list.right() - 5, y)];
        assert_eq!(
            badge.bg,
            buffer[(x, y)].bg,
            "path badges belong to the result row"
        );
        let icon = &buffer[(x - 2, y)];
        assert_eq!(icon.fg, Color::Rgb(222, 165, 132));
    }
    let summary = &buffer[find_cell(buffer, "WORKSPACE FILES")];
    assert_eq!(
        summary.fg,
        ColorLayer::rgba(204, 221, 170, 192)
            .over(summary.bg)
            .unwrap()
    );
    let (input_x, input_y) = find_cell(buffer, "sa");
    assert_eq!(
        input_y,
        app.quick_picker.area.y + 2,
        "the query remains visible in the compact input row"
    );
    let input_cell = &buffer[(input_x, input_y)];
    assert_eq!(
        input_cell.bg,
        ColorLayer::rgba(17, 0, 34, 128)
            .over(app.colors.ui.bg_quick_input)
            .unwrap()
    );
    assert_eq!(
        input_cell.fg,
        ColorLayer::rgba(221, 255, 238, 192)
            .over(input_cell.bg)
            .unwrap()
    );
    snapshot(buffer, "independent quick picker roles");
}

#[test]
fn settings_search_and_edited_numbers_use_their_actual_control_backgrounds() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_icon_and_scrollbar_roles();
    for (key, value) in [
        ("settings.focusedRowBackground", "#344458"),
        ("settings.focusedRowBorder", "#ccddee80"),
        ("settings.numberInputBackground", "#302016c0"),
        ("settings.numberInputForeground", "#fff0bbe0"),
        ("settings.textInputBackground", "#201830c0"),
        ("settings.textInputForeground", "#eeffffe0"),
        ("input.placeholderForeground", "#ffffffc0"),
        ("settings.headerForeground", "#ddeeffc0"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    app.settings_editor_active = true;
    app.active_pane = crate::app::ActivePane::Editor;
    app.settings_category_selected = 1;
    app.settings_selected = app
        .settings_items()
        .iter()
        .position(|(_, _, id)| id == "tab_size")
        .unwrap();
    app.settings_focus_control = true;
    app.settings_value_editing = true;
    app.settings_value_setting_id = Some("tab_size".into());
    app.settings_value_buffer = "12".into();
    app.settings_value_cursor = 2;
    let mut terminal = Terminal::new(TestBackend::new(110, 38)).unwrap();
    terminal
        .draw(|frame| crate::ui_settings::draw_settings_editor(frame, &mut app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let number = &buffer[find_cell(buffer, "12")];
    assert_eq!(
        number.bg,
        ColorLayer::rgba(48, 32, 22, 192)
            .over(Color::Rgb(52, 68, 88))
            .unwrap()
    );
    assert_eq!(
        number.fg,
        ColorLayer::rgba(255, 240, 187, 224)
            .over(number.bg)
            .unwrap()
    );
    let placeholder = &buffer[find_cell(buffer, "Search settings")];
    assert_eq!(
        placeholder.bg,
        ColorLayer::rgba(32, 24, 48, 192)
            .over(app.colors.bg_editor)
            .unwrap()
    );
    assert_eq!(
        placeholder.fg,
        ColorLayer::rgba(255, 255, 255, 192)
            .over(placeholder.bg)
            .unwrap()
    );
    let content = app.settings_sidebar_area.unwrap();
    let (_, number_y) = find_cell(buffer, "12");
    let row_border = &buffer[(content.x, number_y)];
    assert_eq!(row_border.symbol(), "▏");
    assert_eq!(
        row_border.fg,
        ColorLayer::rgba(204, 221, 238, 128)
            .over(row_border.bg)
            .unwrap()
    );
    snapshot(buffer, "independent settings control roles");
}

#[test]
fn settings_option_lists_use_dropdown_and_list_roles_instead_of_menu_colors() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_icon_and_scrollbar_roles();
    for (key, value) in [
        ("menu.background", "#ffeecc"),
        ("menu.foreground", "#442200"),
        ("settings.dropdownBackground", "#203040"),
        ("settings.dropdownForeground", "#eeffdde0"),
        ("dropdown.listBackground", "#102018c0"),
        ("settings.dropdownListBorder", "#ffbbdd80"),
        ("list.activeSelectionBackground", "#304060c0"),
        ("list.activeSelectionForeground", "#ddeeffe0"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    app.settings_editor_active = true;
    app.settings_category_selected = App::settings_category_for_id("theme");
    app.settings_selected = app
        .settings_items()
        .iter()
        .position(|(_, _, id)| id == "theme")
        .unwrap();
    app.settings_dropdown_open = true;
    app.settings_dropdown_setting_id = Some("theme".into());
    app.settings_dropdown_options = vec!["Selected option".into(), "Normal option".into()];
    let mut terminal = Terminal::new(TestBackend::new(110, 38)).unwrap();
    terminal
        .draw(|frame| crate::ui_settings::draw_settings_editor(frame, &mut app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let normal = &buffer[find_cell(buffer, "Normal option")];
    assert_eq!(
        normal.bg,
        ColorLayer::rgba(16, 32, 24, 192)
            .over(app.colors.bg_editor)
            .unwrap()
    );
    assert_eq!(
        normal.fg,
        ColorLayer::rgba(238, 255, 221, 224)
            .over(normal.bg)
            .unwrap()
    );
    let selected = &buffer[find_cell(buffer, "Selected option")];
    assert_eq!(
        selected.bg,
        ColorLayer::rgba(48, 64, 96, 192).over(normal.bg).unwrap()
    );
    assert_eq!(
        selected.fg,
        ColorLayer::rgba(221, 238, 255, 224)
            .over(selected.bg)
            .unwrap()
    );
    let area = app.settings_dropdown_area.unwrap();
    assert_eq!(
        buffer[(area.x - 1, area.y - 1)].fg,
        ColorLayer::rgba(255, 187, 221, 128)
            .over(normal.bg)
            .unwrap()
    );
    snapshot(buffer, "independent settings dropdown roles");
}

#[test]
fn settings_labels_and_controls_remain_readable_across_loaded_palettes() {
    let mut failures = Vec::new();
    for theme in audit_themes() {
        let mut app = App::test_new();
        app.colors = theme.to_theme_colors();
        app.settings_editor_active = true;
        app.active_pane = crate::app::ActivePane::Editor;
        app.settings_category_selected = 1;
        app.settings_selected = app
            .settings_items()
            .iter()
            .position(|(_, _, id)| id == "tab_size")
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(110, 38)).unwrap();
        for (focused, hovered) in [(false, false), (false, true), (true, false)] {
            app.settings_focus_control = focused;
            app.mouse_col = 0;
            app.mouse_row = 0;
            terminal
                .draw(|frame| {
                    crate::ui_settings::draw_settings_editor(frame, &mut app, frame.area())
                })
                .unwrap();
            if hovered {
                let (x, y) = find_cell(terminal.backend().buffer(), "Tab Size");
                app.mouse_col = x;
                app.mouse_row = y;
                terminal
                    .draw(|frame| {
                        crate::ui_settings::draw_settings_editor(frame, &mut app, frame.area())
                    })
                    .unwrap();
            }
            let content = app.settings_sidebar_area.unwrap();
            // The leading focus/modified marker and the trailing scrollbar
            // are decorations; the controls, labels and help are body text.
            check_region(
                terminal.backend().buffer(),
                Rect::new(
                    content.x + 1,
                    content.y,
                    content.width.saturating_sub(2),
                    content.height,
                ),
                &format!(
                    "{}: settings, focused={focused}, hovered={hovered}",
                    theme.name
                ),
                &mut failures,
                &app.colors.ui,
            );
        }
    }
    assert_readable(failures);
}

#[test]
fn file_type_colors_stay_independent_of_color_themes_and_selection() {
    use crate::app::{ActivePane, GlobalSearchResult, SidebarView, Tab};
    use crate::explorer::{FileEntry, GitStatus, GitStatusSummary};

    // These are the palette colors from before the regression, independent of
    // both the color-theme roles and the file-icon classifier under test.
    const FILES: &[(&str, &str, Color)] = &[
        (".gitignore", "Gt ", Color::Rgb(241, 80, 47)),
        ("Cargo.lock", "Lk ", Color::Rgb(130, 130, 130)),
        ("Cargo.toml", "Cf ", Color::Rgb(160, 160, 160)),
        ("module.rs", "Rs ", Color::Rgb(222, 165, 132)),
        ("check.py", "Py ", Color::Rgb(55, 118, 171)),
        ("app.js", "JS ", Color::Rgb(241, 224, 90)),
        ("native.cpp", "C+ ", Color::Rgb(0, 122, 204)),
        ("readme.md", "Md ", Color::Rgb(55, 160, 230)),
    ];
    fn assert_icons(buffer: &Buffer, label: &str, expected_count: usize) {
        for &(_, glyph, expected) in FILES {
            let mut found = 0;
            for y in buffer.area.y..buffer.area.bottom() {
                for x in buffer.area.x..buffer.area.right().saturating_sub(2) {
                    if !glyph
                        .chars()
                        .enumerate()
                        .all(|(i, ch)| buffer[(x + i as u16, y)].symbol() == ch.to_string())
                    {
                        continue;
                    }
                    found += 1;
                    for (i, ch) in glyph
                        .chars()
                        .enumerate()
                        .filter(|(_, ch)| !ch.is_whitespace())
                    {
                        assert_eq!(
                            buffer[(x + i as u16, y)].fg,
                            expected,
                            "{label}: {glyph} character {ch} at ({x},{y})"
                        );
                    }
                }
            }
            assert!(found >= expected_count, "{label}: missing {glyph}");
        }
    }

    let mut app = App::test_new();
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app.tabs = FILES
        .iter()
        .map(|(name, _, _)| {
            let mut editor = crate::editor::Editor::new();
            editor.file_path = Some(app.explorer.root.join(name));
            Tab::new_file(editor)
        })
        .collect();
    app.explorer.entries = FILES
        .iter()
        .map(|(name, _, _)| FileEntry {
            name: (*name).into(),
            path: app.explorer.root.join(name),
            is_dir: false,
            depth: 0,
            expanded: false,
            git_status: GitStatus::Unmodified,
            git_status_summary: GitStatusSummary::default(),
        })
        .collect();
    app.explorer_multi_selected = app
        .explorer
        .entries
        .iter()
        .map(|e| e.path.clone())
        .collect();
    app.show_open_editors = true;
    app.quick_open_results = app.explorer_multi_selected.to_vec();
    app.global_search_query = "needle".into();
    app.global_search_results = FILES
        .iter()
        .map(|(name, _, _)| GlobalSearchResult {
            file_path: app.explorer.root.join(name),
            line_num: 1,
            line_text: "needle".into(),
            col_start: 0,
            col_end: 6,
            context_before: None,
            context_after: None,
        })
        .collect();
    app.source_control_files = FILES
        .iter()
        .map(|(name, _, _)| crate::app::GitFileChange {
            path: (*name).into(),
            original_path: None,
            status: 'M',
            staged: false,
        })
        .collect();
    app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
    app.tab_mru_order = (0..FILES.len()).collect();
    let mut terminal = Terminal::new(TestBackend::new(180, 45)).unwrap();

    for mut theme in audit_themes() {
        for (key, value) in [
            ("icon.foreground", "#12dd88"),
            ("list.activeSelectionIconForeground", "#ffccbb"),
            ("list.inactiveSelectionIconForeground", "#ff99cc"),
            ("quickInputList.focusIconForeground", "#dddd00"),
        ] {
            theme.workbench_colors.insert(key.into(), value.into());
        }
        app.colors = theme.to_theme_colors();
        // Breadcrumbs have no active/inactive sidebar state. Check each file
        // once per theme; the other views below exercise both focus states.
        for (index, &(_, glyph, expected)) in FILES.iter().enumerate() {
            app.active_tab = index;
            terminal
                .draw(|f| {
                    crate::ui_chrome_tabs::draw_breadcrumb(f, &mut app, Rect::new(0, 0, 180, 1))
                })
                .unwrap();
            let icon = find_cell(terminal.backend().buffer(), glyph);
            assert_eq!(
                terminal.backend().buffer()[icon].fg,
                expected,
                "{} breadcrumb {glyph}",
                theme.name
            );
        }
        for focused in [false, true] {
            app.active_pane = if focused {
                ActivePane::Explorer
            } else {
                ActivePane::Editor
            };
            app.active_tab = if focused { 4 } else { 0 };
            app.quick_open_selected = app.active_tab;
            app.hovered_tab = Some(5);
            app.hovered_explorer = Some(3);
            app.sidebar_view = SidebarView::Explorer;
            terminal
                .draw(|f| crate::ui_chrome::draw_explorer(f, &mut app, f.area()))
                .unwrap();
            assert_icons(
                terminal.backend().buffer(),
                &format!("{} explorer {focused}", theme.name),
                1,
            );
            if focused {
                snapshot(
                    terminal.backend().buffer(),
                    &format!("file icons - {}", theme.name),
                );
            }

            terminal
                .draw(|f| crate::ui_chrome_tabs::draw_tab_bar(f, &app, Rect::new(0, 0, 180, 1)))
                .unwrap();
            assert_icons(
                terminal.backend().buffer(),
                &format!("{} tabs {focused}", theme.name),
                1,
            );
            app.active_tab = FILES.len() - 1;
            terminal
                .draw(|f| {
                    crate::ui_quick_picker::draw(f, &mut app, crate::ui_quick_picker::Kind::Files)
                })
                .unwrap();
            assert_icons(
                terminal.backend().buffer(),
                &format!("{} quick open {focused}", theme.name),
                1,
            );

            app.sidebar_view = SidebarView::Search;
            terminal
                .draw(|f| crate::ui_search_panel::draw_search_panel(f, &mut app, f.area()))
                .unwrap();
            assert_icons(
                terminal.backend().buffer(),
                &format!("{} search {focused}", theme.name),
                1,
            );

            terminal
                .draw(|f| {
                    crate::ui_source_control::draw_source_control_sidebar(f, &mut app, f.area())
                })
                .unwrap();
            assert_icons(
                terminal.backend().buffer(),
                &format!("{} source control {focused}", theme.name),
                1,
            );

            terminal
                .draw(|f| crate::ui_pickers::draw_tab_switcher(f, &app))
                .unwrap();
            assert_icons(
                terminal.backend().buffer(),
                &format!("{} tab switcher {focused}", theme.name),
                1,
            );
        }
    }
}

#[test]
fn gradient_tabs_preserve_file_icons_with_scrolling_pins_modified_markers_and_clipping() {
    use crate::app::{EditorGroup, Tab};
    let mut app = App::test_new();
    let mut group = EditorGroup::new();
    for name in [".gitignore", "module.rs", "check.py", "app.js"] {
        for tabs in [&mut app.tabs, &mut group.tabs] {
            let mut editor = crate::editor::Editor::new();
            editor.file_path = Some(app.explorer.root.join(name));
            editor.modified = true;
            let mut tab = Tab::new_file(editor);
            tab.pinned = name == "module.rs";
            tabs.push(tab);
        }
    }
    app.active_tab = 3;
    app.tab_scroll_offset = 1;
    group.active_tab = 3;
    group.tab_scroll_offset = 1;
    app.groups.push(group);
    app.colors.gradients.bg_tabbar.start = Color::Rgb(250, 250, 250);
    app.colors.gradients.bg_tabbar.end = Color::Rgb(10, 10, 10);
    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    let mut failures = Vec::new();
    for split in [false, true] {
        for width in [24, 72] {
            let area = Rect::new(3, 1, width, 8);
            terminal
                .draw(|f| {
                    if split {
                        crate::ui_helpers::draw_split_editor_for_group(f, &mut app, area, 0);
                    } else {
                        crate::ui_chrome_tabs::draw_tab_bar(
                            f,
                            &app,
                            Rect::new(area.x, area.y, area.width, 1),
                        );
                    }
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let icon = find_cell(buffer, "Rs ");
            assert_eq!(
                buffer[icon].fg,
                Color::Rgb(222, 165, 132),
                "split={split}, width={width}"
            );
            assert_eq!(icon.1, area.y);
            if width == 72 {
                for (glyph, color) in [
                    ("Py ", Color::Rgb(55, 118, 171)),
                    ("JS ", Color::Rgb(241, 224, 90)),
                ] {
                    let icon = find_cell(buffer, glyph);
                    assert_eq!(buffer[icon].fg, color, "split={split}, {glyph}");
                }
                snapshot(buffer, &format!("file icons - gradient tabs split {split}"));
            }
            check_region_with_file_icons(
                buffer,
                Rect::new(area.x, area.y, width, 1),
                &format!("gradient tabs split={split}, width={width}"),
                &mut failures,
                &app.colors.ui,
                &["module.rs", "check.py", "app.js"],
            );
        }
    }
    assert_readable(failures);
}

#[test]
fn file_icon_palette_and_symbol_theme_roles_reach_tabs_breadcrumbs_outline_and_file_picker() {
    use crate::app::{ActivePane, SymbolItem, Tab};
    let mut app = App::test_new();
    app.colors = theme_with_icon_and_scrollbar_roles().to_theme_colors();
    let mut editor = crate::editor::Editor::new();
    editor.file_path = Some(app.explorer.root.join("example.py"));
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 0;
    app.outline_items = ["function", "struct", "module"]
        .into_iter()
        .enumerate()
        .map(|(line, kind)| SymbolItem {
            name: format!("sample_{kind}"),
            kind,
            line,
            tab_index: None,
            file_name: None,
            file_path: None,
        })
        .collect();
    app.active_pane = ActivePane::Explorer;
    app.outline_selected = 0;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_chrome_tabs::draw_tab_bar(frame, &app, Rect::new(0, 0, 100, 1));
            crate::ui_chrome_tabs::draw_breadcrumb(frame, &mut app, Rect::new(0, 1, 100, 1));
            crate::ui_chrome::draw_outline(frame, &app, Rect::new(0, 3, 100, 8));
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let icon = find_cell(buffer, "Py");
    assert_eq!(buffer[icon].fg, Color::Rgb(55, 118, 171));
    let symbol_text = find_cell(buffer, "sample_function");
    assert_eq!(
        buffer[symbol_text].fg,
        app.colors.ui.readable_foreground(
            app.colors.ui.fg_breadcrumb_focus,
            app.colors.ui.bg_breadcrumb,
            4.5
        )
    );
    for (glyph, expected) in [
        ("ƒ", Color::Rgb(187, 255, 153)),
        (" S ", Color::Rgb(255, 187, 221)),
        (" M ", Color::Rgb(153, 221, 255)),
    ] {
        let (x, y) = find_cell(buffer, glyph);
        let x = x + u16::from(glyph.starts_with(' '));
        assert_eq!(buffer[(x, y)].fg, expected);
    }
    snapshot(buffer, "independent icon roles");
    app.quick_open_results = vec![app.explorer.root.join("example.py")];
    terminal
        .draw(|frame| {
            crate::ui_quick_picker::draw(frame, &mut app, crate::ui_quick_picker::Kind::Files)
        })
        .unwrap();
    let icon = find_cell(terminal.backend().buffer(), "Py");
    let cell = &terminal.backend().buffer()[icon];
    assert_eq!(cell.fg, Color::Rgb(55, 118, 171));

    app.breadcrumb_dropdown_active = true;
    app.breadcrumb_dropdown_items = vec![
        (app.explorer.root.join("example.py"), false),
        (app.explorer.root.join("subdirectory"), true),
    ];
    terminal
        .draw(|frame| {
            crate::ui_chrome_tabs::draw_breadcrumb(frame, &mut app, Rect::new(0, 1, 100, 1))
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let folder = &buffer[find_cell(buffer, "📁")];
    assert_eq!(folder.fg, app.colors.ui.folder_icon_color(folder.bg));
}

fn sidebar_list_app() -> App {
    use crate::app::{SymbolItem, Tab};
    use crate::explorer::{FileEntry, GitStatus};
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::new();
    editor.file_path = Some(app.explorer.root.join("open.py"));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.show_open_editors = true;
    app.explorer.entries = [
        ("selected.rs", GitStatus::Unmodified),
        ("untracked.rs", GitStatus::Untracked),
        ("renamed.rs", GitStatus::Renamed),
        ("ignored.rs", GitStatus::Ignored),
    ]
    .into_iter()
    .map(|(name, status)| FileEntry {
        path: app.explorer.root.join(name),
        name: name.into(),
        is_dir: false,
        depth: 0,
        expanded: false,
        git_status: status,
        git_status_summary: status.into(),
    })
    .collect();
    app.explorer_multi_selected = vec![app.explorer.entries[0].path.clone()].into();
    app.explorer_selection_anchor = None;
    app.hovered_explorer = None;
    app.outline_items = ["selected_symbol", "hovered_symbol", "ordinary_symbol"]
        .into_iter()
        .enumerate()
        .map(|(line, name)| SymbolItem {
            name: name.into(),
            kind: "function",
            line,
            tab_index: None,
            file_name: None,
            file_path: None,
        })
        .collect();
    app.mouse_row = u16::MAX;
    app.mouse_col = u16::MAX;
    app
}

#[test]
fn ignored_entries_use_readable_hover_and_selection_colors_for_names_and_icons() {
    use crate::app::{ActivePane, EditorTheme};
    for is_dir in [false, true] {
        let mut app = sidebar_list_app();
        app.explorer.entries[3].is_dir = is_dir;
        for theme in [EditorTheme::Dark, EditorTheme::Light] {
            app.colors = theme.colors();
            for focused in [false, true] {
                app.active_pane = if focused { ActivePane::Explorer } else { ActivePane::Editor };
                for state in 0..3 {
                    let selected = state == 2;
                    let hovered = state == 1;
                    app.explorer_multi_selected.clear();
                    if selected {
                        app.explorer_multi_selected.push(app.explorer.entries[3].path.clone());
                    }
                    app.hovered_explorer = hovered.then_some(3);
                    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
                    terminal.draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, frame.area())).unwrap();
                    let buffer = terminal.backend().buffer();
                    let (x, y) = find_cell(buffer, "ignored.rs");
                    let cell = &buffer[(x, y)];
                    let expected = if selected || hovered {
                        app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground,
                            app.colors.bg_sidebar, selected, focused, hovered).fg.unwrap()
                    } else { app.colors.ui.git_status_color('I', app.colors.bg_sidebar) };
                    assert_eq!(cell.fg, expected, "folder={is_dir}, focused={focused}, state={state}");
                    let icon = (0..x).find(|&x| buffer[(x, y)].symbol() != " ").unwrap();
                    assert_eq!(buffer[(icon, y)].fg, expected);
                    let mut failures = Vec::new();
                    check_region(buffer, Rect::new(x, y, 10, 1), "ignored Explorer entry",
                        &mut failures, &app.colors.ui);
                    assert_readable(failures);
                }
            }
        }
    }
}

#[test]
fn sidebar_lists_compose_selection_hover_and_decorations_on_the_sidebar() {
    use crate::{
        app::{ActivePane, SidebarView},
        theme_workbench::ColorLayer,
    };
    let mut theme = theme_with_popup_and_control_roles();
    for (key, value) in [
        ("gitDecoration.untrackedResourceForeground", "#ddffbbe0"),
        ("gitDecoration.renamedResourceForeground", "#ffbbdde0"),
        ("gitDecoration.ignoredResourceForeground", "#bbeeffe0"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    let sidebar = Color::Rgb(24, 48, 37);
    for active in [false, true] {
        let mut app = sidebar_list_app();
        app.colors = theme.to_theme_colors();
        app.active_pane = if active {
            ActivePane::Explorer
        } else {
            ActivePane::Editor
        };
        app.sidebar_view = SidebarView::Explorer;
        let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
        terminal
            .draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (fill, text) = if active {
            (
                ColorLayer::rgba(48, 72, 56, 192),
                ColorLayer::rgba(255, 255, 187, 224),
            )
        } else {
            (
                ColorLayer::rgba(48, 72, 104, 192),
                ColorLayer::rgba(187, 221, 255, 224),
            )
        };
        for name in ["open.py", "selected.rs"] {
            let cell = &buffer[find_cell(buffer, name)];
            assert_eq!(
                cell.bg,
                fill.over(sidebar).unwrap(),
                "{name}, active={active}"
            );
            assert_eq!(
                cell.fg,
                text.over(cell.bg).unwrap(),
                "{name}, active={active}"
            );
        }
        for (name, badge, foreground) in [
            ("untracked.rs", Some("U"), ColorLayer::rgba(221, 255, 187, 224)),
            ("renamed.rs", Some("R"), ColorLayer::rgba(255, 187, 221, 224)),
            ("ignored.rs", None, ColorLayer::rgba(187, 238, 255, 224)),
        ] {
            let (x, y) = find_cell(buffer, name);
            assert_eq!(buffer[(x, y)].bg, sidebar);
            assert_eq!(buffer[(x, y)].fg, foreground.over(sidebar).unwrap());
            if let Some(badge) = badge {
                let badge = (0..80).rev()
                    .find(|&x| buffer[(x, y)].symbol() == badge).unwrap();
                assert_eq!(buffer[(badge, y)].fg, foreground.over(sidebar).unwrap());
            } else {
                assert!(!(x + name.len() as u16..80).any(|x| buffer[(x, y)].symbol() == "I"));
                let icon = (0..x).find(|&x| buffer[(x, y)].symbol() != " ").unwrap();
                assert_eq!(buffer[(icon, y)].fg, foreground.over(sidebar).unwrap());
            }
        }
        snapshot(buffer, &format!("sidebar list selection {active}"));
        app.hovered_explorer = Some(1);
        terminal
            .draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, frame.area()))
            .unwrap();
        let cell =
            &terminal.backend().buffer()[find_cell(terminal.backend().buffer(), "untracked.rs")];
        assert_eq!(
            cell.bg,
            ColorLayer::rgba(80, 56, 72, 192).over(sidebar).unwrap()
        );
        assert_eq!(
            cell.fg,
            ColorLayer::rgba(221, 255, 204, 224).over(cell.bg).unwrap()
        );

        app.sidebar_view = SidebarView::Outline;
        app.outline_hovered = Some(1);
        terminal
            .draw(|frame| crate::ui_chrome::draw_outline(frame, &app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let selected = &buffer[find_cell(buffer, "selected_symbol")];
        assert_eq!(selected.bg, fill.over(sidebar).unwrap());
        assert_eq!(selected.fg, text.over(selected.bg).unwrap());
        let hovered = &buffer[find_cell(buffer, "hovered_symbol")];
        assert_eq!(
            hovered.bg,
            ColorLayer::rgba(80, 56, 72, 192).over(sidebar).unwrap()
        );
        assert_eq!(
            hovered.fg,
            ColorLayer::rgba(221, 255, 204, 224)
                .over(hovered.bg)
                .unwrap()
        );
        let ordinary = &buffer[find_cell(buffer, "ordinary_symbol")];
        assert_eq!(ordinary.bg, sidebar);
        assert_eq!(
            ordinary.fg,
            ColorLayer::rgba(221, 238, 255, 192).over(sidebar).unwrap()
        );
        snapshot(buffer, &format!("outline list selection {active}"));
    }
}

#[test]
fn explorer_drag_target_uses_the_drop_role_and_keeps_transparent_fills() {
    use crate::{
        app::{ActivePane, SidebarView},
        theme_workbench::ColorLayer,
    };
    let mut theme = theme_with_popup_and_control_roles();
    theme
        .workbench_colors
        .insert("list.focusBackground".into(), "#806010".into());
    for (drop, expected) in [
        ("#40502080", ColorLayer::rgba(64, 80, 32, 128)),
        ("#00000000", ColorLayer::rgba(0, 0, 0, 0)),
    ] {
        theme
            .workbench_colors
            .insert("list.dropBackground".into(), drop.into());
        let mut app = sidebar_list_app();
        app.colors = theme.to_theme_colors();
        app.active_pane = ActivePane::Explorer;
        app.sidebar_view = SidebarView::Explorer;
        app.dragging_explorer_path = Some(app.explorer.entries[1].path.clone());
        app.explorer_drop_target_idx = Some(0);
        let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
        terminal
            .draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, frame.area()))
            .unwrap();
        let cell =
            &terminal.backend().buffer()[find_cell(terminal.backend().buffer(), "selected.rs")];
        assert_eq!(cell.bg, expected.over(Color::Rgb(24, 48, 37)).unwrap());
        assert_eq!(
            cell.fg,
            ColorLayer::rgba(221, 238, 255, 192).over(cell.bg).unwrap()
        );
    }
}

#[test]
fn explorer_sticky_folders_use_sidebar_sticky_colors_instead_of_editor_sticky_colors() {
    use crate::{
        explorer::{FileEntry, GitStatus, GitStatusSummary},
        theme_workbench::ColorLayer,
    };
    let mut theme = theme_with_popup_and_control_roles();
    theme
        .workbench_colors
        .insert("sideBarStickyScroll.background".into(), "#40302080".into());
    theme
        .workbench_colors
        .insert("editorStickyScroll.background".into(), "#802040".into());
    let mut app = sidebar_list_app();
    app.colors = theme.to_theme_colors();
    let folder = app.explorer.root.join("sticky_source");
    app.explorer.entries = (0..20)
        .map(|index| FileEntry {
            path: if index == 0 {
                folder.clone()
            } else {
                folder.join(format!("child_{index}.rs"))
            },
            name: if index == 0 {
                "sticky_source".into()
            } else {
                format!("child_{index}.rs")
            },
            is_dir: index == 0,
            depth: usize::from(index > 0),
            expanded: index == 0,
            git_status: GitStatus::Unmodified,
            git_status_summary: GitStatusSummary::default(),
        })
        .collect();
    app.explorer_multi_selected.clear();
    app.explorer.scroll_offset = 5;
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, frame.area()))
        .unwrap();
    let cell =
        &terminal.backend().buffer()[find_cell(terminal.backend().buffer(), "sticky_source")];
    assert_eq!(
        cell.bg,
        ColorLayer::rgba(64, 48, 32, 128)
            .over(Color::Rgb(24, 48, 37))
            .unwrap()
    );
    assert_eq!(
        cell.fg,
        ColorLayer::rgba(221, 238, 255, 192).over(cell.bg).unwrap()
    );
    snapshot(terminal.backend().buffer(), "sidebar sticky folder roles");
}

#[test]
fn sidebar_list_rows_remain_readable_across_themes_and_focus_states() {
    use crate::app::{ActivePane, SidebarView};
    let mut failures = Vec::new();
    for theme in audit_themes() {
        for active in [false, true] {
            let mut app = sidebar_list_app();
            app.colors = theme.to_theme_colors();
            app.active_pane = if active {
                ActivePane::Explorer
            } else {
                ActivePane::Editor
            };
            app.sidebar_view = SidebarView::Explorer;
            app.hovered_explorer = Some(1);
            let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
            terminal
                .draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, frame.area()))
                .unwrap();
            check_region_with_file_icons(
                terminal.backend().buffer(),
                Rect::new(0, 0, 79, 7),
                &format!("{} Explorer, active={active}", theme.name),
                &mut failures,
                &app.colors.ui,
                &["open.py", "selected.rs"],
            );
            app.sidebar_view = SidebarView::Outline;
            app.outline_hovered = Some(1);
            terminal
                .draw(|frame| crate::ui_chrome::draw_outline(frame, &app, frame.area()))
                .unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(0, 0, 80, 4),
                &format!("{} Outline, active={active}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
    }
    assert_readable(failures);
}

#[test]
fn explorer_file_icons_keep_type_colors_while_folder_icons_use_selection_overrides() {
    use crate::app::{ActivePane, SidebarView, Tab};
    use crate::explorer::{FileEntry, GitStatus, GitStatusSummary};
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::new();
    editor.file_path = Some(app.explorer.root.join("example.py"));
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 0;
    app.show_open_editors = true;
    app.sidebar_view = SidebarView::Explorer;
    let folder = app.explorer.root.join("source");
    app.explorer.entries = vec![FileEntry {
        path: folder.clone(),
        name: "source".into(),
        is_dir: true,
        depth: 0,
        expanded: false,
        git_status: GitStatus::Unmodified,
        git_status_summary: GitStatusSummary::default(),
    }];
    app.explorer_multi_selected = vec![folder].into();
    for overrides in [false, true] {
        let mut theme = theme_with_icon_and_scrollbar_roles();
        if overrides {
            theme.workbench_colors.insert(
                "list.activeSelectionIconForeground".into(),
                "#ffcc99c0".into(),
            );
            theme.workbench_colors.insert(
                "list.inactiveSelectionIconForeground".into(),
                "#ccff99c0".into(),
            );
        }
        app.colors = theme.to_theme_colors();
        for active in [false, true] {
            app.active_pane = if active {
                ActivePane::Explorer
            } else {
                ActivePane::Editor
            };
            let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
            terminal
                .draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for glyph in ["Py", "▸"] {
                let position = find_cell(buffer, glyph);
                let cell = &buffer[position];
                let layer = if !overrides {
                    app.colors.ui.icon_foreground
                } else if active {
                    app.colors.ui.list_active_icon
                } else {
                    app.colors.ui.list_inactive_icon
                };
                assert_eq!(
                    cell.fg,
                    if glyph == "Py" {
                        Color::Rgb(55, 118, 171)
                    } else {
                        layer.over(cell.bg).unwrap()
                    },
                    "{glyph}, active={active}, overrides={overrides}"
                );
                assert_ne!(cell.bg, app.colors.bg_sidebar);
            }
        }
    }
}

#[test]
fn keyboard_shortcut_text_remains_readable_on_its_own_rows() {
    let mut app = App::test_new();
    let mut failures = Vec::new();
    assert!(!app.keybindings.is_empty());
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for editing in [false, true] {
            app.keybindings_editing = editing;
            let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_keybindings::draw(frame, &mut app, frame.area())
                })
                .unwrap();
            for area in [Rect::new(0, 0, 100, 2), Rect::new(0, 3, 100, 15)] {
                check_region(
                    terminal.backend().buffer(),
                    area,
                    &format!("{}: keyboard shortcuts, editing={editing}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn shortcut_labels_and_table_use_dedicated_theme_roles() {
    let mut theme = theme_with_icon_and_scrollbar_roles();
    for (key, value) in [
        ("keybindingLabel.background", "#8866aa80"),
        ("keybindingLabel.foreground", "#ffffff"),
        ("keybindingLabel.border", "#ffbbdd"),
        ("keybindingLabel.bottomBorder", "#99ddff"),
        ("keybindingTable.headerBackground", "#305040"),
        ("keybindingTable.rowsBackground", "#30305080"),
        ("settings.headerForeground", "#ddccff"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
    terminal
        .draw(|frame| crate::ui_keybindings::draw(frame, &mut app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 0)].fg, Color::Rgb(221, 204, 255));
    assert_eq!(buffer[(1, 1)].bg, Color::Rgb(48, 80, 64));
    assert_eq!(buffer[(99, 1)].bg, Color::Rgb(48, 80, 64));
    assert_eq!(
        buffer[(1, 4)].bg,
        crate::theme_workbench::ColorLayer::rgba(48, 48, 80, 128)
            .over(app.colors.bg_editor)
            .unwrap()
    );
    assert_eq!(buffer[(31, 3)].fg, Color::Rgb(255, 255, 255));
    assert_eq!(
        buffer[(31, 3)].bg,
        crate::theme_workbench::ColorLayer::rgba(136, 102, 170, 128)
            .over(buffer[(1, 3)].bg)
            .unwrap()
    );
    assert_eq!(buffer[(31, 3)].underline_color, Color::Rgb(153, 221, 255));
    assert!(
        buffer[(31, 3)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    snapshot(buffer, "independent shortcut roles");
}

#[test]
fn panel_body_text_remains_readable_on_distinct_panel_and_hover_surfaces() {
    use crate::app::Tab;
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::test_with_text("some example text\n");
    editor.file_path = Some(app.explorer.root.join("example.py"));
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 0;
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for panel in [1, 3] {
            app.panel_tab = panel;
            for hover in [false, true] {
                app.bg_problems_analysis_running = true;
                app.cached_problems = (0..24)
                    .map(|i| {
                        (
                            "example.py".into(),
                            i + 1,
                            ["TODO", "FIXME", "NOTE", "HACK"][i % 4].into(),
                            "diagnostic message".into(),
                            0,
                        )
                    })
                    .collect();
                app.hovered_problem_row = hover.then_some(0);
                app.mouse_col = if hover { 4 } else { u16::MAX };
                app.mouse_row = if hover { 2 } else { u16::MAX };
                let mut terminal = Terminal::new(TestBackend::new(120, 16)).unwrap();
                terminal
                    .draw(|frame| {
                        crate::ui_panels::draw_bottom_panel(frame, &mut app, frame.area())
                    })
                    .unwrap();
                check_region(
                    terminal.backend().buffer(),
                    Rect::new(0, 1, 119, 15),
                    &format!("{}: panel={panel}, hover={hover}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
                if panel == 1 && hover {
                    assert_eq!(
                        terminal.backend().buffer()[(100, 2)].bg,
                        app.colors.bg_hover
                    );
                }
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn notification_icons_use_their_own_roles_and_descriptions_keep_surface_alpha() {
    use crate::app::NotifyLevel;
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_icon_and_scrollbar_roles();
    for (key, value) in [
        ("notifications.background", "#142a30"),
        ("notifications.foreground", "#ffffff"),
        ("descriptionForeground", "#ccccddc0"),
        ("notificationsInfoIcon.foreground", "#99ddffc0"),
        ("notificationsWarningIcon.foreground", "#bbff99c0"),
        ("notificationsErrorIcon.foreground", "#ffbbddc0"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    app.notification_history = [NotifyLevel::Info, NotifyLevel::Warning, NotifyLevel::Error]
        .into_iter()
        .enumerate()
        .map(|(i, level)| ("Notification example".into(), level, i as u64))
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
    terminal
        .draw(|frame| crate::ui_panels::draw_panel_notifications(frame, &app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for (glyph, color) in [
        ("ℹ", (153, 221, 255)),
        ("⚠", (187, 255, 153)),
        ("✗", (255, 187, 221)),
    ] {
        let icon = &buffer[find_cell(buffer, glyph)];
        assert_eq!(
            icon.fg,
            ColorLayer::rgba(color.0, color.1, color.2, 192)
                .over(icon.bg)
                .unwrap()
        );
    }
    let sequence = &buffer[find_cell(buffer, "#1")];
    assert_eq!(
        sequence.fg,
        ColorLayer::rgba(204, 204, 221, 192)
            .over(sequence.bg)
            .unwrap()
    );
}

#[test]
fn imported_annotations_inherit_diagnostics_and_native_annotations_survive_customizations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("annotations.json");
    std::fs::write(
        &path,
        serde_json::json!({"colors":{
            "editorWarning.foreground":"#aabbcc", "editorError.foreground":"#cc99bb"
        }})
        .to_string(),
    )
    .unwrap();
    let imported = crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark")).unwrap();
    let colors = imported.to_theme_colors();
    assert_eq!(colors.fg_todo, Color::Rgb(170, 187, 204));
    assert_eq!(colors.fg_hack, Color::Rgb(170, 187, 204));
    assert_eq!(colors.fg_fixme, Color::Rgb(204, 153, 187));

    // A legacy native theme has an explicitly authored palette and no imported
    // grammar/workbench metadata. Overrides must not replace its annotations.
    let mut native = crate::app_theme::load_builtin_themes().remove(0);
    native.textmate_theme = None;
    native.workbench_colors.clear();
    native.colors.fg_todo = Some("#123456".into());
    native.colors.fg_fixme = Some("#654321".into());
    native.colors.fg_hack = Some("#345678".into());
    let overrides = std::collections::BTreeMap::from([
        ("foreground".into(), serde_json::json!("#eeeeee")),
        (
            "editorWarning.foreground".into(),
            serde_json::json!("#ddccbb"),
        ),
    ]);
    let customized = crate::vscode_theme::customized_theme(&native, &overrides).to_theme_colors();
    assert_eq!(customized.fg_todo, Color::Rgb(18, 52, 86));
    assert_eq!(customized.fg_fixme, Color::Rgb(101, 67, 33));
    assert_eq!(customized.fg_hack, Color::Rgb(52, 86, 120));
}

#[test]
fn search_sidebar_text_and_matches_use_sidebar_and_find_roles() {
    use crate::app::{ActivePane, GlobalSearchResult, SidebarView};
    let mut app = App::test_new();
    app.sidebar_view = SidebarView::Search;
    app.global_search_query = "needle".into();
    app.global_search_results = (0..3)
        .map(|line| GlobalSearchResult {
            file_path: app.explorer.root.join("src/example.rs"),
            line_num: line + 1,
            line_text: "before needle after".into(),
            col_start: 7,
            col_end: 13,
            context_before: None,
            context_after: None,
        })
        .collect();
    app.global_search_selected = 0;
    let mut failures = Vec::new();
    let mut independent = theme_with_icon_and_scrollbar_roles();
    for (key, value) in [
        ("list.highlightForeground", "#99ccff"),
        ("list.focusHighlightForeground", "#ffbbdd"),
        ("editor.findMatchHighlightBackground", "#cc804080"),
        ("editor.findMatchHighlightBorder", "#ffff9980"),
        ("badge.background", "#66449980"),
        ("badge.foreground", "#ffffff"),
    ] {
        independent
            .workbench_colors
            .insert(key.into(), value.into());
    }
    for theme in audit_themes().into_iter().chain([independent]) {
        app.colors = theme.to_theme_colors();
        for focused in [false, true] {
            app.active_pane = if focused {
                ActivePane::Explorer
            } else {
                ActivePane::Editor
            };
            let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area())
                })
                .unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(6, 0, 94, 20),
                &format!("{}: search sidebar, focused={focused}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            if theme.name == "Independent roles" {
                let buffer = terminal.backend().buffer();
                let (x, y) = find_cell(buffer, "needle after");
                let cell = &buffer[(x, y)];
                let row = &buffer[(x - 1, y)];
                assert_eq!(cell.fg, row.fg, "match inherits the result row text");
                assert_eq!(cell.bg, crate::theme_workbench::ColorLayer::rgba(204, 128, 64, 128)
                    .over(row.bg).unwrap());
                assert_eq!(cell.underline_color, crate::theme_workbench::ColorLayer::rgba(255, 255, 153, 128)
                    .over(cell.bg).unwrap());
                assert!(!cell.modifier.contains(ratatui::style::Modifier::BOLD));
                let (file_x, file_y) = find_cell(buffer, "example.rs");
                let count = &buffer[(file_x + 13, file_y)];
                assert_eq!(count.symbol(), "3");
                assert_eq!(count.fg, Color::Rgb(255, 255, 255));
                assert_eq!(
                    count.bg,
                    crate::theme_workbench::ColorLayer::rgba(102, 68, 153, 128)
                        .over(app.colors.bg_sidebar)
                        .unwrap()
                );
                snapshot(buffer, "independent sidebar search roles");
            }
        }
    }
    assert_readable(failures);
}

fn search_list_app() -> App {
    let mut app = App::test_new();
    app.sidebar_view = crate::app::SidebarView::Search;
    app.global_search_query = "needle".into();
    app.global_search_results = (0..3)
        .map(|line| crate::app::GlobalSearchResult {
            file_path: app.explorer.root.join("src/example.rs"),
            line_num: line + 1,
            line_text: format!("row{line} needle after").into(),
            col_start: 5,
            col_end: 11,
            context_before: None,
            context_after: None,
        })
        .collect();
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app
}

#[test]
fn search_result_find_colors_follow_themes_selection_hover_and_preview() {
    use crate::{app::ActivePane, theme_workbench::ColorLayer};
    let mut app = search_list_app();
    for theme in audit_themes() {
        // Reuse the same results while changing the palette, as live theme
        // preview does. Imported roles retain alpha until the row is painted.
        app.colors = theme.to_theme_colors();
        let roles = crate::vscode_color_registry::resolve(
            &theme.workbench_colors, theme.is_light, theme.high_contrast);
        let fill = ColorLayer::resolve(&roles, "editor.findMatchHighlightBackground", ColorLayer::default());
        let border = ColorLayer::resolve(&roles, "editor.findMatchHighlightBorder", ColorLayer::default());
        for (active, selected, hovered) in [
            (false, false, false), (true, false, false),
            (false, true, false), (true, true, false), (true, false, true),
        ] {
            app.active_pane = if active { ActivePane::Explorer } else { ActivePane::Editor };
            app.global_search_selected = if selected { 1 } else { 0 };
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
            let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
            terminal.draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area())).unwrap();
            let (x, y) = find_cell(terminal.backend().buffer(), "row1 needle after");
            if hovered {
                (app.mouse_col, app.mouse_row) = (x, y);
                terminal.draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area())).unwrap();
            }
            let buffer = terminal.backend().buffer();
            let row = &buffer[(x, y)];
            let expected_bg = if theme.high_contrast { row.bg } else { fill.over(row.bg).unwrap_or(row.bg) };
            for col in x + 5..x + 11 {
                let matched = &buffer[(col, y)];
                assert_eq!(matched.bg, expected_bg,
                    "{}: active={active}, selected={selected}, hover={hovered}", theme.name);
                assert_eq!(matched.fg, row.fg, "{}: preserve result text", theme.name);
                assert!(!matched.modifier.contains(ratatui::style::Modifier::BOLD));
                if let Some(outline) = border.border(expected_bg) {
                    assert!(matched.modifier.contains(ratatui::style::Modifier::UNDERLINED));
                    assert_eq!(matched.underline_color, outline);
                }
            }
            assert_eq!(buffer[(x + 12, y)].bg, row.bg, "highlight stops at the match boundary");
            if !active && !selected && !hovered {
                snapshot(buffer, &format!("{} search results", theme.name));
            }
        }
    }
}

#[test]
fn monokai_search_results_use_the_registered_orange_fill_instead_of_white_bold_text() {
    let mut app = search_list_app();
    app.colors = crate::app_theme_loader::builtin_theme("monokai").unwrap().to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal.draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area())).unwrap();
    let buffer = terminal.backend().buffer();
    let (x, y) = find_cell(buffer, "row1 needle after");
    // Upstream #ea5c0055 over Monokai's #1e1f1c sidebar.
    assert_eq!(buffer[(x + 5, y)].bg, Color::Rgb(98, 51, 19));
    assert_eq!(buffer[(x + 5, y)].fg, buffer[(x, y)].fg);
    assert_eq!(buffer[(x + 12, y)].bg, Color::Rgb(30, 31, 28));
    assert!(!buffer[(x + 5, y)].modifier.contains(ratatui::style::Modifier::BOLD));
}

#[test]
fn search_results_honor_default_transparent_and_high_contrast_find_roles() {
    use crate::theme_workbench::ColorLayer;
    for (high_contrast, fill, border) in [
        (false, serde_json::Value::Null, serde_json::Value::Null),
        (false, serde_json::json!("#ff000000"), serde_json::json!("#00ff0000")),
        (false, serde_json::Value::Null, serde_json::json!("#aabbcc80")),
        (true, serde_json::json!("#ff0000"), serde_json::json!("#aabbcc80")),
    ] {
        let mut theme = theme_with_popup_and_control_roles();
        theme.high_contrast = high_contrast;
        // A null authored color inherits the registered default. Alpha zero
        // explicitly disables the fill; high contrast uses outlines only.
        let default_fill = !high_contrast && fill.is_null();
        theme.workbench_colors.insert("editor.findMatchHighlightBackground".into(), fill);
        theme.workbench_colors.insert("editor.findMatchHighlightBorder".into(), border.clone());
        let mut app = search_list_app();
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_cell(buffer, "row1 needle after");
        let matched = &buffer[(x + 5, y)];
        let row = &buffer[(x, y)];
        let background = if default_fill {
            ColorLayer::rgba(234, 92, 0, 85).over(row.bg).unwrap()
        } else { row.bg };
        assert_eq!(matched.bg, background);
        assert_eq!(matched.fg, buffer[(x, y)].fg);
        assert!(!matched.modifier.contains(ratatui::style::Modifier::BOLD));
        if border == serde_json::json!("#aabbcc80") {
            assert!(matched.modifier.contains(ratatui::style::Modifier::UNDERLINED));
            assert_eq!(matched.underline_color, ColorLayer::rgba(170, 187, 204, 128).over(matched.bg).unwrap());
        } else {
            assert!(!matched.modifier.contains(ratatui::style::Modifier::UNDERLINED));
        }
    }
}

#[test]
fn search_rows_resolve_selection_hover_text_and_outlines_on_the_sidebar() {
    use crate::{app::ActivePane, theme_workbench::ColorLayer};
    let sidebar = Color::Rgb(24, 48, 37);
    for active in [false, true] {
        let mut app = search_list_app();
        app.colors = theme_with_popup_and_control_roles().to_theme_colors();
        app.active_pane = if active {
            ActivePane::Explorer
        } else {
            ActivePane::Editor
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let selected = &buffer[find_cell(buffer, "row0")];
        let (fill, text) = if active {
            (
                ColorLayer::rgba(48, 72, 56, 192),
                ColorLayer::rgba(255, 255, 187, 224),
            )
        } else {
            (
                ColorLayer::rgba(48, 72, 104, 192),
                ColorLayer::rgba(187, 221, 255, 224),
            )
        };
        assert_eq!(selected.bg, fill.over(sidebar).unwrap());
        assert_eq!(selected.fg, text.over(selected.bg).unwrap());
        if active {
            let (_, y) = find_cell(buffer, "row0");
            assert!(
                buffer[(0, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::UNDERLINED)
            );
            assert_eq!(
                selected.underline_color,
                ColorLayer::rgba(255, 170, 119, 128)
                    .over(selected.bg)
                    .unwrap()
            );
        }
        let (x, y) = find_cell(buffer, "row1");
        assert_eq!(buffer[(x, y)].bg, sidebar);
        assert_eq!(
            buffer[(x, y)].fg,
            ColorLayer::rgba(221, 238, 255, 192).over(sidebar).unwrap()
        );
        snapshot(buffer, &format!("search list selection {active}"));
        app.mouse_col = x;
        app.mouse_row = y;
        terminal
            .draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let hovered = buffer[(x, y)].clone();
        assert_eq!(
            hovered.bg,
            ColorLayer::rgba(80, 56, 72, 192).over(sidebar).unwrap()
        );
        assert_eq!(
            hovered.fg,
            ColorLayer::rgba(221, 255, 204, 224)
                .over(hovered.bg)
                .unwrap()
        );
        let (file_x, file_y) = find_cell(buffer, "example.rs");
        app.mouse_col = file_x;
        app.mouse_row = file_y;
        terminal
            .draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(file_x, file_y)].bg, hovered.bg);
        assert_eq!(buffer[(file_x, file_y)].fg, hovered.fg);
    }
}

#[test]
fn search_rows_keep_transparent_selection_and_hover_overrides() {
    let mut theme = theme_with_popup_and_control_roles();
    for role in [
        "list.activeSelectionBackground",
        "list.inactiveSelectionBackground",
        "list.hoverBackground",
        "list.focusAndSelectionOutline",
        "list.inactiveFocusOutline",
    ] {
        theme
            .workbench_colors
            .insert(role.into(), "#00000000".into());
    }
    for active in [false, true] {
        let mut app = search_list_app();
        app.colors = theme.to_theme_colors();
        app.active_pane = if active {
            crate::app::ActivePane::Explorer
        } else {
            crate::app::ActivePane::Editor
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let cell = &buffer[find_cell(buffer, "row0")];
        assert_eq!(cell.bg, app.colors.bg_sidebar);
        assert!(!cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
        let (x, y) = find_cell(buffer, "row1");
        app.mouse_col = x;
        app.mouse_row = y;
        terminal
            .draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area()))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(x, y)].bg,
            app.colors.bg_sidebar
        );
    }
}

fn extension_list_app() -> App {
    use crate::extension_wasm::ExtensionRecord;
    let mut app = App::test_new();
    app.extension_manager.records = (0..2)
        .map(|index| ExtensionRecord {
            manifest: serde_json::from_value(serde_json::json!({
                "id":format!("test.example{index}"), "name":format!("Example Extension {index}"),
                "version":"1.0.0", "description":"Extension description", "enabled":index == 0
            }))
            .unwrap(),
            manifest_path: Default::default(),
            wasm_path: Default::default(),
            user_unloaded: false,
            state: if index == 0 {
                crate::extensions::ExtensionState::Loaded
            } else {
                crate::extensions::ExtensionState::Error
            },
            status_text: "Test extension state".into(),
            commands: vec![],
            views: Default::default(),
            explorer_context_commands: vec![],
            binary_open_commands: vec![],
            file_open_commands: vec![],
            requested_open_view: None,
            logs: vec!["A sample log entry".into()],
            last_error: Some("A sample error".into()),
            overlays: Default::default(),
            tick_handlers: vec![],
            manifest_mtime: None,
            wasm_mtime: None,
            runtime: None,
            text_edit_requests: vec![],
            published_diagnostics: Default::default(),
            diagnostics_version: 0,
        })
        .collect();
    app
}

fn extension_overlay_app() -> App {
    let mut app = extension_list_app();
    app.extension_manager.records[0].overlays.insert(
        "sample".into(),
        crate::extension_manifest::ExtensionOverlay {
            id: "sample".into(),
            title: "Extension overlay".into(),
            content: "Overlay content\nAnother line".into(),
            x: 8,
            y: 4,
            width: 52,
            height: 6,
            mouse_handler: None,
            key_handler: None,
        },
    );
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app
}

fn screencast_app(age_ms: u64) -> App {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = App::test_new();
    app.screencast_mode = true;
    let timestamp = std::time::Instant::now() - std::time::Duration::from_millis(age_ms);
    let key = KeyEvent::new(
        KeyCode::Char('K'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    app.screencast_keys.push_back((key, timestamp));
    app.screencast_keys.push_back((key, timestamp));
    app
}

#[test]
fn extension_and_key_overlays_use_widget_keycap_and_toolbar_roles() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_popup_and_control_roles();
    for (role, value) in [
        ("toolbar.hoverBackground", "#40506880"),
        ("toolbar.hoverOutline", "#99ddaa80"),
        ("keybindingLabel.background", "#304c2ccc"),
        ("keybindingLabel.foreground", "#fff5eec0"),
        ("keybindingLabel.bottomBorder", "#bbddffa0"),
    ] {
        theme.workbench_colors.insert(role.into(), value.into());
    }
    let surface = Color::Rgb(32, 40, 48);
    let mut app = extension_overlay_app();
    app.colors = theme.to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_extension_overlays(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for label in ["Extension overlay", "Overlay content", "Another line", "✕"] {
        let cell = &buffer[find_cell(buffer, label)];
        assert_eq!(cell.bg, surface, "{label}: owning widget surface");
        assert_eq!(
            cell.fg,
            ColorLayer::rgba(255, 238, 221, 224).over(surface).unwrap()
        );
    }
    assert_eq!(
        buffer[(8, 5)].fg,
        ColorLayer::rgba(119, 187, 221, 128).over(surface).unwrap()
    );
    snapshot(buffer, "extension overlay roles");
    let (x, y) = find_cell(buffer, "✕");
    assert_eq!(
        x,
        8 + 52 - 2,
        "close glyph is inside its existing hit target"
    );
    app.mouse_col = x;
    app.mouse_row = y;
    terminal
        .draw(|frame| crate::ui_dialogs::draw_extension_overlays(frame, &app))
        .unwrap();
    let close = &terminal.backend().buffer()[(x, y)];
    let hover = ColorLayer::rgba(64, 80, 104, 128).over(surface).unwrap();
    assert_eq!(close.bg, hover);
    assert_eq!(
        close.fg,
        ColorLayer::rgba(255, 238, 221, 224).over(hover).unwrap()
    );
    assert_eq!(
        close.underline_color,
        ColorLayer::rgba(153, 221, 170, 128).over(hover).unwrap()
    );
    snapshot(terminal.backend().buffer(), "extension overlay hover roles");

    let mut keys = screencast_app(500);
    keys.colors = theme.to_theme_colors();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_screencast_overlay(frame, &keys))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let (x, y) = find_cell(buffer, "Ctrl+Alt+K ×2");
    let key = &buffer[(x, y)];
    let key_background = ColorLayer::rgba(48, 76, 44, 204).over(surface).unwrap();
    assert_eq!(key.bg, key_background);
    assert_eq!(
        key.underline_color,
        ColorLayer::rgba(187, 221, 255, 160)
            .over(key_background)
            .unwrap()
    );
    assert_eq!(
        buffer[(x - 1, y)].bg,
        surface,
        "keycaps do not fill the padding"
    );
    assert_eq!(buffer[find_cell(buffer, "Keys")].bg, surface);
    snapshot(buffer, "screencast overlay roles");

    for role in [
        "editorWidget.border",
        "toolbar.hoverOutline",
        "keybindingLabel.background",
        "keybindingLabel.bottomBorder",
    ] {
        theme
            .workbench_colors
            .insert(role.into(), "#00000000".into());
    }
    app.colors = theme.to_theme_colors();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_extension_overlays(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(8, 5)].fg, surface);
    assert!(
        !buffer[find_cell(buffer, "✕")]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    let mut keys = screencast_app(500);
    keys.colors = theme.to_theme_colors();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_screencast_overlay(frame, &keys))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let key = &buffer[find_cell(buffer, "Ctrl+Alt+K ×2")];
    assert_eq!(key.bg, surface);
    assert!(!key.modifier.contains(ratatui::style::Modifier::UNDERLINED));
}

#[test]
fn search_and_overlay_text_remain_readable_on_all_theme_surfaces() {
    let mut failures = Vec::new();
    for theme in audit_themes() {
        let mut app = search_list_app();
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        for active in [false, true] {
            app.active_pane = if active {
                crate::app::ActivePane::Explorer
            } else {
                crate::app::ActivePane::Editor
            };
            for hover in ["row1", "example.rs"] {
                terminal
                    .draw(|frame| {
                        crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area())
                    })
                    .unwrap();
                (app.mouse_col, app.mouse_row) = find_cell(terminal.backend().buffer(), hover);
                terminal
                    .draw(|frame| {
                        crate::ui_search_panel::draw_search_panel(frame, &mut app, frame.area())
                    })
                    .unwrap();
                let top =
                    crate::ui_search_panel::search_results_start_y(&app, Rect::new(0, 0, 100, 24));
                check_region_with_file_icons(
                    terminal.backend().buffer(),
                    Rect::new(0, top, 99, 4),
                    &format!("{}: search active={active}, hover={hover}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                    &["example.rs"],
                );
            }
        }
        let mut app = extension_overlay_app();
        app.colors = theme.to_theme_colors();
        for hovered in [false, true] {
            app.mouse_col = if hovered { 58 } else { u16::MAX };
            app.mouse_row = if hovered { 4 } else { u16::MAX };
            terminal
                .draw(|frame| crate::ui_dialogs::draw_extension_overlays(frame, &app))
                .unwrap();
            let buffer = terminal.backend().buffer();
            check_region(
                buffer,
                Rect::new(9, 5, 50, 4),
                &format!("{}: extension overlay content", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            for label in ["Extension overlay", "✕"] {
                let (x, y) = find_cell(buffer, label);
                check_region(
                    buffer,
                    Rect::new(x, y, label.chars().count() as u16, 1),
                    &format!("{}: extension overlay {label}, hover={hovered}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
        for age in [0, 1500, 2500] {
            let mut app = screencast_app(age);
            app.colors = theme.to_theme_colors();
            terminal
                .draw(|frame| crate::ui_dialogs::draw_screencast_overlay(frame, &app))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for label in ["Keys", "Ctrl+Alt+K ×2"] {
                let (x, y) = find_cell(buffer, label);
                check_region(
                    buffer,
                    Rect::new(x, y, label.chars().count() as u16, 1),
                    &format!("{}: key overlay age={age}, {label}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert_readable(failures);
}

fn editor_annotation_app() -> App {
    use crate::app::{InlayHintDisplay, Tab};
    let mut app = App::test_new();
    app.tabs
        .push(Tab::new_file(crate::editor::Editor::test_with_text(
            "source();\nsecond();\n",
        )));
    app.active_pane = crate::app::ActivePane::Editor;
    app.show_inlay_hints = true;
    app.show_code_lenses = true;
    app.show_minimap = false;
    app.show_line_numbers = false;
    app.cursor_visible = false;
    app.rulers.clear();
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app.lsp_inlay_hints = vec![
        InlayHintDisplay::new(0, 1, "generic hint", None),
        InlayHintDisplay::new(0, 2, "typehint", Some(lsp_types::InlayHintKind::TYPE)),
        InlayHintDisplay::new(0, 3, "paramhint", Some(lsp_types::InlayHintKind::PARAMETER)),
    ];
    app.lsp_code_lenses = vec![(0, "3 references".into())];
    app
}

fn theme_with_editor_annotation_roles() -> ThemeFile {
    let mut theme = theme_with_popup_and_control_roles();
    theme.colors.bg_line_highlight = "#28303c".into();
    for (role, value) in [
        ("editorInlayHint.background", "#405860a0"),
        ("editorInlayHint.foreground", "#f5fff5d0"),
        ("editorInlayHint.typeBackground", "#304860c0"),
        ("editorInlayHint.typeForeground", "#aaffffee"),
        ("editorInlayHint.parameterBackground", "#604830b0"),
        ("editorInlayHint.parameterForeground", "#fff4ccee"),
        ("editorCodeLens.foreground", "#ffeeaacc"),
        ("editorStickyScroll.background", "#203c5080"),
        ("editorStickyScrollHover.background", "#405068a0"),
        ("editorStickyScroll.border", "#aaddee80"),
    ] {
        theme.workbench_colors.insert(role.into(), value.into());
    }
    theme
}

#[test]
fn editor_annotation_colors_follow_kind_and_actual_line_background() {
    use crate::theme_workbench::ColorLayer;
    for highlighted in [false, true] {
        let mut app = editor_annotation_app();
        app.colors = theme_with_editor_annotation_roles().to_theme_colors();
        app.tabs[0].editor.cursor_line = usize::from(!highlighted);
        let surface = if highlighted {
            app.colors.bg_line_highlight
        } else {
            app.colors.bg_editor
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
        terminal
            .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for (label, fill, foreground) in [
            (
                "generic hint",
                ColorLayer::rgba(64, 88, 96, 160),
                ColorLayer::rgba(245, 255, 245, 208),
            ),
            (
                "typehint",
                ColorLayer::rgba(48, 72, 96, 192),
                ColorLayer::rgba(170, 255, 255, 238),
            ),
            (
                "paramhint",
                ColorLayer::rgba(96, 72, 48, 176),
                ColorLayer::rgba(255, 244, 204, 238),
            ),
        ] {
            let cell = &buffer[find_cell(buffer, label)];
            let background = fill.over(surface).unwrap();
            assert_eq!(cell.bg, background, "{label}, highlighted={highlighted}");
            assert_eq!(
                cell.fg,
                foreground.over(background).unwrap(),
                "{label}: foreground opacity"
            );
            assert!(cell.modifier.contains(ratatui::style::Modifier::ITALIC));
        }
        let codelens = &buffer[find_cell(buffer, "3 references")];
        assert_eq!(codelens.bg, surface);
        assert_eq!(
            codelens.fg,
            ColorLayer::rgba(255, 238, 170, 204).over(surface).unwrap()
        );
        snapshot(
            buffer,
            &format!("editor annotation roles highlighted {highlighted}"),
        );
    }
}

#[test]
fn inlay_role_inheritance_preserves_alpha_and_transparent_backgrounds() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_editor_annotation_roles();
    for role in [
        "editorInlayHint.typeForeground",
        "editorInlayHint.typeBackground",
        "editorInlayHint.parameterForeground",
        "editorInlayHint.parameterBackground",
    ] {
        theme.workbench_colors.remove(role);
    }
    theme
        .workbench_colors
        .insert("editorInlayHint.background".into(), "#00000000".into());
    let mut app = editor_annotation_app();
    app.colors = theme.to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
    terminal
        .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, frame.area()))
        .unwrap();
    for label in ["generic hint", "typehint", "paramhint"] {
        let cell = &terminal.backend().buffer()[find_cell(terminal.backend().buffer(), label)];
        assert_eq!(cell.bg, app.colors.bg_line_highlight);
        assert_eq!(
            cell.fg,
            ColorLayer::rgba(245, 255, 245, 208).over(cell.bg).unwrap()
        );
    }
    theme.workbench_colors.remove("editorInlayHint.background");
    theme
        .workbench_colors
        .insert("badge.background".into(), "#44668880".into());
    let colors = theme.to_theme_colors();
    for kind in 0..3 {
        assert_eq!(
            colors.ui.inlay_hint_style(kind, colors.bg_editor).bg,
            ColorLayer::rgba(68, 102, 136, 13).over(colors.bg_editor),
            "kind={kind}: inherited badge opacity is multiplied"
        );
    }
    theme
        .workbench_colors
        .insert("badge.background".into(), "#00000000".into());
    let colors = theme.to_theme_colors();
    assert_eq!(
        colors.ui.inlay_hint_style(0, colors.bg_editor).bg,
        Some(colors.bg_editor)
    );
}

fn sticky_editor_app() -> App {
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::test_with_text(&format!(
        "fn outer() {{\n    if ready {{\n{}",
        "        work();\n".repeat(15)
    ));
    editor.scroll_offset = 5;
    editor.cursor_line = 8;
    app.tabs.push(crate::app::Tab::new_file(editor));
    app.active_pane = crate::app::ActivePane::Editor;
    app.sticky_scroll = true;
    app.show_line_numbers = true;
    app.show_minimap = false;
    app.cursor_visible = false;
    app.rulers.clear();
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app.cached_sticky_scopes = vec![
        crate::background::StickyScopeLine {
            line: 0,
            parent: None,
        },
        crate::background::StickyScopeLine {
            line: 1,
            parent: Some(0),
        },
    ].into();
    app
}

#[test]
fn sticky_editor_headers_use_editor_and_line_number_colors_on_header_and_hover_fills() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_editor_annotation_roles();
    theme
        .workbench_colors
        .insert("editor.foreground".into(), "#ffeeccc0".into());
    let mut app = sticky_editor_app();
    app.colors = theme.to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    terminal
        .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let surface = ColorLayer::rgba(32, 60, 80, 128)
        .over(app.colors.bg_editor)
        .unwrap();
    let foreground = ColorLayer::rgba(255, 238, 204, 192).over(surface).unwrap();
    let (x, y) = find_cell(buffer, "fn outer");
    assert_eq!(buffer[(x, y)].bg, surface);
    assert_eq!(buffer[(x, y)].fg, foreground);
    let (_, last_y) = find_cell(buffer, "if ready");
    assert!(
        !buffer[(x, y)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    let border = &buffer[(x, last_y)];
    assert!(
        border
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    assert_eq!(
        border.underline_color,
        ColorLayer::rgba(170, 221, 238, 128).over(surface).unwrap()
    );
    let gutter = app.gutter_area.unwrap();
    let number_x = (gutter.x..gutter.right())
        .find(|&col| buffer[(col, y)].symbol() == "1")
        .unwrap();
    assert_eq!(buffer[(number_x, y)].bg, surface);
    assert_eq!(
        buffer[(number_x, y)].fg,
        ColorLayer::rgba(204, 204, 221, 192).over(surface).unwrap()
    );
    snapshot(buffer, "sticky editor header roles");
    app.mouse_col = x;
    app.mouse_row = y;
    terminal
        .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let hover = ColorLayer::rgba(64, 80, 104, 160).over(surface).unwrap();
    assert_eq!(buffer[(x, y)].bg, hover);
    assert_eq!(
        buffer[(x, y)].fg,
        ColorLayer::rgba(255, 238, 204, 192).over(hover).unwrap()
    );
    assert_eq!(
        buffer[(number_x, y)].fg,
        ColorLayer::rgba(204, 204, 221, 192).over(hover).unwrap()
    );
    snapshot(buffer, "sticky editor hover roles");
    for role in [
        "editorStickyScroll.background",
        "editorStickyScrollHover.background",
        "editorStickyScroll.border",
    ] {
        theme
            .workbench_colors
            .insert(role.into(), "#00000000".into());
    }
    app.colors = theme.to_theme_colors();
    terminal
        .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(x, y)].bg, app.colors.bg_editor);
    assert!(
        !buffer[(x, last_y)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
}

#[test]
fn editor_annotation_and_sticky_header_text_remain_readable_across_themes() {
    let mut failures = Vec::new();
    for theme in audit_themes() {
        for highlighted in [false, true] {
            let mut app = editor_annotation_app();
            app.colors = theme.to_theme_colors();
            app.tabs[0].editor.cursor_line = usize::from(!highlighted);
            let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
            terminal
                .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for label in ["generic hint", "typehint", "paramhint", "3 references"] {
                let (x, y) = find_cell(buffer, label);
                check_region(
                    buffer,
                    Rect::new(x, y, label.len() as u16, 1),
                    &format!("{}: {label}, highlighted={highlighted}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
        let mut app = sticky_editor_app();
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        for hovered in [false, true] {
            terminal
                .draw(|frame| crate::ui_editor::draw_editor_content(frame, &mut app, frame.area()))
                .unwrap();
            if hovered {
                (app.mouse_col, app.mouse_row) = find_cell(terminal.backend().buffer(), "fn outer");
                terminal
                    .draw(|frame| {
                        crate::ui_editor::draw_editor_content(frame, &mut app, frame.area())
                    })
                    .unwrap();
            }
            let buffer = terminal.backend().buffer();
            for label in ["fn outer", "if ready"] {
                let (x, y) = find_cell(buffer, label);
                check_region(
                    buffer,
                    Rect::new(x, y, label.len() as u16, 1),
                    &format!("{}: sticky {label}, hovered={hovered}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
            let gutter = app.gutter_area.unwrap();
            check_region(
                buffer,
                Rect::new(gutter.x, 0, gutter.width, 2),
                &format!("{}: sticky gutter", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
    }
    assert_readable(failures);
}

fn merge_marker_app(mode: usize) -> App {
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::new();
    editor.rope = ropey::Rope::from_str(
        "<<<<<<< HEAD 界 extra long branch name label\ncurrent value\n||||||| base\ncommon value\n=======\nincoming value\n>>>>>>> branch\nordinary value\n",
    );
    editor.saved_rope = Some(editor.rope.clone());
    editor.merge_conflicts.index = Some(std::sync::Arc::new(crate::merge_conflicts::Index::update(None, editor.rope.clone())));
    editor.word_wrap = mode == 1;
    editor.large_file_mode = mode == 2;
    app.tabs.push(crate::app::Tab::new_file(editor));
    app.active_pane = crate::app::ActivePane::Editor;
    app.show_minimap = false;
    app.show_line_numbers = false;
    app.cursor_visible = false;
    app.error_lens = false;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app
}

#[test]
fn merge_marker_headers_and_buttons_use_their_own_theme_roles_in_every_renderer() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_popup_and_control_roles();
    theme.colors.bg_line_highlight = "#151f29".into();
    for (role, value) in [
        ("merge.currentHeaderBackground", "#22406680"),
        ("merge.incomingHeaderBackground", "#663344a0"),
        ("merge.commonHeaderBackground", "#34552280"),
        ("merge.border", "#bbdd99a0"),
        // Content fills are deliberately different, so these cannot silently
        // be substituted for a header or used for the accept action buttons.
        ("merge.currentContentBackground", "#00224440"),
        ("merge.incomingContentBackground", "#44002240"),
    ] {
        theme.workbench_colors.insert(role.into(), value.into());
    }
    for mode in 0..3 {
        let mut app = merge_marker_app(mode);
        app.colors = theme.to_theme_colors();
        let mut terminal =
            Terminal::new(TestBackend::new(if mode == 1 { 34 } else { 128 }, 20)).unwrap();
        terminal
            .draw(|frame| crate::editor_ai_review::draw(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for (label, fill, surface) in [
            (
                "<<<<<<<",
                ColorLayer::rgba(34, 64, 102, 128),
                app.colors.bg_line_highlight,
            ),
            (
                "|||||||",
                ColorLayer::rgba(52, 85, 34, 128),
                app.colors.bg_editor,
            ),
            (
                ">>>>>>>",
                ColorLayer::rgba(102, 51, 68, 160),
                app.colors.bg_editor,
            ),
            (
                "=======",
                ColorLayer::rgba(0, 0, 0, 0),
                app.colors.bg_editor,
            ),
        ] {
            let cell = &buffer[find_cell(buffer, label)];
            assert_eq!(cell.bg, fill.over(surface).unwrap(), "mode={mode}, {label}");
            assert_eq!(
                cell.underline_color,
                ColorLayer::rgba(187, 221, 153, 160).over(cell.bg).unwrap(),
                "mode={mode}, {label}: border opacity"
            );
            assert!(cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
        }
        snapshot(buffer, &format!("merge marker renderer {mode}"));
        if mode == 0 {
            for (label, action) in [
                ("Accept Current", "current"),
                ("Accept Incoming", "incoming"),
                ("Accept Both", "both"),
            ] {
                let (x, y) = find_cell(buffer, label);
                let cell = &buffer[(x, y)];
                let style = app.colors.ui.codelens_style(app.colors.bg_editor);
                assert_eq!(cell.bg, app.colors.bg_editor);
                assert_eq!(Some(cell.fg), style.fg);
                let button = app.editor().unwrap().ai_review.merge_hits
                    .iter()
                    .find(|button| button.action == action)
                    .unwrap();
                assert_eq!(
                    (button.area.y, button.area.x),
                    (y, x),
                    "Unicode marker keeps button hit boxes aligned"
                );
            }
            let (x, y) = find_cell(buffer, "Accept Current");
            app.mouse_col = x;
            app.mouse_row = y;
            terminal
                .draw(|frame| crate::editor_ai_review::draw(frame, &mut app, frame.area()))
                .unwrap();
            assert_eq!(
                terminal.backend().buffer()[(x, y)].bg,
                app.colors.bg_editor
            );
            assert!(terminal.backend().buffer()[(x, y)].modifier.contains(ratatui::style::Modifier::UNDERLINED));
        }
    }
    for role in [
        "merge.currentHeaderBackground",
        "merge.incomingHeaderBackground",
        "merge.commonHeaderBackground",
        "merge.border",
    ] {
        theme
            .workbench_colors
            .insert(role.into(), "#00000000".into());
    }
    for mode in 0..3 {
        let mut app = merge_marker_app(mode);
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| crate::editor_ai_review::draw(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for (label, surface) in [
            ("<<<<<<<", app.colors.bg_line_highlight),
            ("|||||||", app.colors.bg_editor),
            (">>>>>>>", app.colors.bg_editor),
            ("=======", app.colors.bg_editor),
        ] {
            let cell = &buffer[find_cell(buffer, label)];
            assert_eq!(cell.bg, surface, "mode={mode}, {label}: transparent header");
            assert!(!cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
        }
    }
}

#[test]
fn merge_header_defaults_and_native_overrides_do_not_borrow_content_or_list_colors() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_popup_and_control_roles();
    theme
        .workbench_colors
        .insert("merge.currentContentBackground".into(), "#ff009920".into());
    theme
        .workbench_colors
        .insert("merge.incomingContentBackground".into(), "#99ff0020".into());
    let palette = theme.to_theme_colors();
    for (marker, fill) in [
        ("<<<<<<<", ColorLayer::rgba(64, 200, 174, 128)),
        (">>>>>>>", ColorLayer::rgba(64, 166, 255, 128)),
        ("|||||||", ColorLayer::rgba(96, 96, 96, 102)),
    ] {
        assert_eq!(
            palette
                .ui
                .merge_marker_style(marker.chars(), palette.bg_editor)
                .unwrap()
                .bg,
            fill.over(palette.bg_editor)
        );
    }
    for light in [false, true] {
        theme.is_light = light;
        theme.high_contrast = true;
        let palette = theme.to_theme_colors();
        let style = palette
            .ui
            .merge_marker_style("<<<<<<<".chars(), palette.bg_editor)
            .unwrap();
        assert_eq!(style.bg, Some(palette.bg_editor));
        assert_eq!(
            style.underline_color,
            Some(if light {
                Color::Rgb(0, 122, 204)
            } else {
                Color::Rgb(195, 223, 111)
            })
        );
    }
    theme.high_contrast = false;
    theme.textmate_theme = None;
    theme.colors.bg_merge_current = Some("#1c406680".into());
    let palette = theme.to_theme_colors();
    assert_eq!(
        palette
            .ui
            .merge_marker_style("<<<<<<<".chars(), palette.bg_editor)
            .unwrap()
            .bg,
        ColorLayer::rgba(28, 64, 102, 128).over(palette.bg_editor)
    );
    theme
        .workbench_colors
        .insert("merge.currentHeaderBackground".into(), "#00000000".into());
    let palette = theme.to_theme_colors();
    assert_eq!(
        palette
            .ui
            .merge_marker_style("<<<<<<<".chars(), palette.bg_editor)
            .unwrap()
            .bg,
        Some(palette.bg_editor)
    );
    assert!(
        palette
            .ui
            .merge_marker_style("<<<<<<".chars(), palette.bg_editor)
            .is_none()
    );
    assert!(
        palette
            .ui
            .merge_marker_style("ordinary".chars(), palette.bg_editor)
            .is_none()
    );
}

#[test]
fn merge_marker_labels_and_actions_remain_readable_across_themes() {
    let mut failures = Vec::new();
    for theme in audit_themes() {
        for mode in 0..3 {
            let mut app = merge_marker_app(mode);
            app.colors = theme.to_theme_colors();
            let mut terminal = Terminal::new(TestBackend::new(128, 20)).unwrap();
            terminal
                .draw(|frame| crate::editor_ai_review::draw(frame, &mut app, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for label in ["<<<<<<<", "|||||||", "=======", ">>>>>>>"] {
                let (x, y) = find_cell(buffer, label);
                check_region(
                    buffer,
                    Rect::new(x, y, 7, 1),
                    &format!("{}: merge mode={mode}, {label}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
            if mode == 0 {
                for label in ["Accept Current", "Accept Incoming", "Accept Both"] {
                    let (x, y) = find_cell(buffer, label);
                    check_region(
                        buffer,
                        Rect::new(x, y, label.len() as u16, 1),
                        &format!("{}: merge {label}", theme.name),
                        &mut failures,
                        &app.colors.ui,
                    );
                }
                let (x, y) = find_cell(buffer, "Accept Current");
                app.mouse_col = x;
                app.mouse_row = y;
                terminal
                    .draw(|frame| {
                        crate::editor_ai_review::draw(frame, &mut app, frame.area())
                    })
                    .unwrap();
                check_region(
                    terminal.backend().buffer(),
                    Rect::new(x, y, 14, 1),
                    &format!("{}: merge button hover", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn extension_rows_use_active_inactive_hover_and_error_roles_on_their_own_backgrounds() {
    use crate::{
        app::{ActivePane, SidebarView},
        theme_workbench::ColorLayer,
    };
    let mut theme = theme_with_popup_and_control_roles();
    theme
        .workbench_colors
        .insert("list.errorForeground".into(), "#ffee99c0".into());
    let sidebar = Color::Rgb(24, 48, 37);
    for active in [false, true] {
        let mut app = extension_list_app();
        app.colors = theme.to_theme_colors();
        app.active_pane = if active {
            ActivePane::Explorer
        } else {
            ActivePane::Editor
        };
        app.sidebar_view = SidebarView::Extensions;
        app.mouse_row = u16::MAX;
        app.mouse_col = u16::MAX;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui_helpers::draw_extensions_sidebar(frame, &mut app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let selected = &buffer[find_cell(buffer, "Example Extension 0")];
        let (fill, text) = if active {
            (
                ColorLayer::rgba(48, 72, 56, 192),
                ColorLayer::rgba(255, 255, 187, 224),
            )
        } else {
            (
                ColorLayer::rgba(48, 72, 104, 192),
                ColorLayer::rgba(187, 221, 255, 224),
            )
        };
        assert_eq!(selected.bg, fill.over(sidebar).unwrap());
        assert_eq!(selected.fg, text.over(selected.bg).unwrap());
        let (x, y) = find_cell(buffer, "Example Extension 1");
        assert_eq!(buffer[(x, y)].bg, sidebar);
        assert_eq!(
            buffer[(x, y)].fg,
            ColorLayer::rgba(221, 238, 255, 192).over(sidebar).unwrap()
        );
        let state_x = app.ext_sidebar.rows[1].1.right() - 2 - 8;
        assert_eq!(
            buffer[(state_x, y + 2)].fg,
            ColorLayer::rgba(255, 238, 153, 192).over(sidebar).unwrap()
        );
        assert_eq!(
            buffer[(x, y + 1)].fg,
            ColorLayer::rgba(255, 238, 153, 192).over(sidebar).unwrap()
        );
        snapshot(buffer, &format!("extension list selection {active}"));
        app.mouse_col = x;
        app.mouse_row = y;
        terminal
            .draw(|frame| crate::ui_helpers::draw_extensions_sidebar(frame, &mut app, frame.area()))
            .unwrap();
        let cell = &terminal.backend().buffer()
            [find_cell(terminal.backend().buffer(), "Example Extension 1")];
        assert_eq!(
            cell.bg,
            ColorLayer::rgba(80, 56, 72, 192).over(sidebar).unwrap()
        );
        assert_eq!(
            cell.fg,
            ColorLayer::rgba(221, 255, 204, 224).over(cell.bg).unwrap()
        );
    }
}

#[test]
fn extension_list_details_and_page_text_remain_readable_on_own_surfaces() {
    let mut app = extension_list_app();
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for selected in 0..2 {
            app.extensions_selected = selected;
            for (hover, active) in [(false, false), (true, false), (false, true), (true, true)] {
                app.active_pane = if active {
                    crate::app::ActivePane::Explorer
                } else {
                    crate::app::ActivePane::Editor
                };
                app.sidebar_view = crate::app::SidebarView::Extensions;
                app.mouse_col = if hover { 6 } else { u16::MAX };
                app.mouse_row = if hover { 4 } else { u16::MAX };
                let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
                terminal
                    .draw(|frame| {
                        crate::ui_helpers::draw_extensions_sidebar(frame, &mut app, frame.area())
                    })
                    .unwrap();
                for area in [
                    Rect::new(0, 0, 100, 2),
                    Rect::new(4, 2, 96, 16),
                    Rect::new(0, 20, 100, 4),
                ] {
                    check_region(
                        terminal.backend().buffer(),
                        area,
                        &format!(
                            "{}: extension list, selected={selected}, hover={hover}, active={active}",
                            theme.name
                        ),
                        &mut failures,
                        &app.colors.ui,
                    );
                }
                let buttons = app.ext_sidebar.actions.clone();
                for (_, area) in buttons {
                    app.mouse_col = area.x + 1;
                    app.mouse_row = area.y;
                    terminal
                        .draw(|frame| {
                            crate::ui_helpers::draw_extensions_sidebar(
                                frame,
                                &mut app,
                                frame.area(),
                            )
                        })
                        .unwrap();
                    check_region(
                        terminal.backend().buffer(),
                        area,
                        &format!("{}: extension action hovered", theme.name),
                        &mut failures,
                        &app.colors.ui,
                    );
                }
            }
            let mut editor = crate::editor::Editor::new();
            editor.file_path = Some(format!("[ext-info:test.example{selected}]").into());
            app.tabs = vec![crate::app::Tab::new_file(editor)];
            app.active_tab = 0;
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_extension_info::draw_extension_info(frame, &mut app, frame.area())
                })
                .unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(5, 0, 95, 24),
                &format!("{}: extension information page", theme.name),
                &mut failures,
                &app.colors.ui,
            );
        }
    }
    assert_readable(failures);
}

#[test]
fn search_popup_matches_layer_find_colors_over_selected_and_unselected_rows() {
    use crate::theme_workbench::ColorLayer;
    let mut app = App::test_new();
    let mut theme = theme_with_popup_and_control_roles();
    for (key, value) in [
        ("list.highlightForeground", "#ffee99"),
        ("list.focusHighlightForeground", "#ffbbdd"),
        ("editor.findMatchHighlightBackground", "#cc804080"),
        ("editor.findMatchHighlightBorder", "#ffff9980"),
        ("editorSuggestWidget.highlightForeground", "#99ddff"),
        ("editorSuggestWidget.focusHighlightForeground", "#bbff99"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    app.colors = theme.to_theme_colors();
    app.global_search_query = "needle".into();
    app.global_search_results = (0..2)
        .map(|line| crate::app::GlobalSearchResult {
            file_path: app.explorer.root.join("example.py"),
            line_num: line + 1,
            line_text: "before needle after".into(),
            col_start: 7,
            col_end: 13,
            context_before: None,
            context_after: None,
        })
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui_search_dialog::draw_global_search(frame, &mut app))
        .unwrap();
    for label in ["example.py:1 before ", "example.py:2 before "] {
        let (x, y) = find_cell(terminal.backend().buffer(), label);
        let matched = &terminal.backend().buffer()[(x + label.len() as u16, y)];
        let cell = &terminal.backend().buffer()[(x, y)];
        assert_eq!(matched.fg, cell.fg);
        assert_eq!(matched.bg, ColorLayer::rgba(204, 128, 64, 128).over(cell.bg).unwrap());
        assert_eq!(matched.underline_color, ColorLayer::rgba(255, 255, 153, 128).over(matched.bg).unwrap());
        assert!(!matched.modifier.contains(ratatui::style::Modifier::BOLD));
        let background = if y == find_cell(terminal.backend().buffer(), "example.py:1").1 {
            ColorLayer::rgba(48, 72, 56, 192)
                .over(Color::Rgb(32, 40, 48))
                .unwrap()
        } else {
            Color::Rgb(32, 40, 48)
        };
        assert_eq!(cell.bg, background);
        let foreground = if label.contains(":1") {
            ColorLayer::rgba(255, 255, 187, 224)
        } else {
            ColorLayer::rgba(255, 238, 221, 224)
        };
        assert_eq!(cell.fg, foreground.over(background).unwrap());
    }
    snapshot(
        terminal.backend().buffer(),
        "independent search match roles",
    );
}

#[test]
fn completion_widgets_keep_registered_inheritance_opacity_hover_and_focus_on_their_own_surfaces() {
    use crate::theme_workbench::ColorLayer;
    use ratatui::{
        style::{Modifier, Style},
        widgets::Block,
    };
    let mut app = App::test_new();
    app.tabs
        .push(crate::app::Tab::new_file(crate::editor::Editor::new()));
    app.code_area = Some(Rect::new(7, 8, 80, 17));
    app.lsp_completion_active = true;
    app.lsp_manager.completion_items = ["selected_entry", "hovered_entry", "normal_entry"]
        .into_iter()
        .enumerate()
        .map(|(index, label)| crate::lsp::LspCompletionItem {
            label: label.into(),
            insert_text: String::new(),
            kind: Some(lsp_types::CompletionItemKind::METHOD),
            detail: (index == 0).then(|| "Completion detail".into()),
            documentation: (index == 0).then(|| "Completion documentation".into()),
            text_edit_range: None,
            additional_text_edits: Vec::new(),
            is_snippet: false,
            original_item: None,
        })
        .collect();
    let viewport = Rect::new(0, 0, 100, 35);
    let geometry =
        crate::ui_search_dialog::lsp_completion_popup_geometry(&mut app, viewport).unwrap();
    app.mouse_col = geometry.inner_area.x;
    app.mouse_row = geometry.inner_area.y + 1;
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    for owner in [Color::Rgb(20, 40, 60), Color::Rgb(50, 10, 65)] {
        for (alpha, border_alpha) in [(128, 128), (0, 128), (128, 0)] {
            let mut theme = theme_with_popup_and_control_roles();
            for (key, value) in [
                ("editor.foreground", "#ffeeccE0".to_owned()),
                ("editorWidget.foreground", "#bbffeeE0".to_owned()),
                (
                    "editorSuggestWidget.background",
                    format!("#285064{alpha:02x}"),
                ),
                (
                    "editorSuggestWidget.focusOutline",
                    format!("#dd99ff{border_alpha:02x}"),
                ),
                ("quickInputList.focusBackground", "#50382080".to_owned()),
                ("quickInputList.focusForeground", "#bbffddE0".to_owned()),
                ("quickInputList.focusIconForeground", "#ffddbbE0".to_owned()),
                ("list.hoverBackground", "#10286080".to_owned()),
                ("list.hoverForeground", "#99ff99E0".to_owned()),
                ("toolbar.hoverBackground", "#aa201080".to_owned()),
                ("toolbar.hoverOutline", "#99ffee80".to_owned()),
                ("symbolIcon.methodForeground", "#99cceeC0".to_owned()),
            ] {
                theme.workbench_colors.insert(key.into(), value.into());
            }
            for key in [
                "editorSuggestWidget.foreground",
                "editorSuggestWidget.selectedBackground",
                "editorSuggestWidget.selectedForeground",
                "editorSuggestWidget.selectedIconForeground",
            ] {
                theme.workbench_colors.remove(key);
            }
            app.colors = theme.to_theme_colors();
            let doc_owner = Color::Rgb(60, 20, 40);
            terminal
                .draw(|f| {
                    f.render_widget(
                        Block::default().style(
                            Style::default()
                                .bg(owner)
                                .fg(Color::Rgb(220, 230, 240))
                                .add_modifier(Modifier::ITALIC),
                        ),
                        viewport,
                    );
                    f.render_widget(
                        Block::default().style(Style::default().bg(doc_owner)),
                        Rect::new(
                            geometry.popup_area.right(),
                            0,
                            100 - geometry.popup_area.right(),
                            35,
                        ),
                    );
                    crate::ui_search_dialog::draw_lsp_completion(f, &mut app);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let popup = ColorLayer::rgba(40, 80, 100, alpha).over(owner).unwrap();
            let selected = ColorLayer::rgba(80, 56, 32, 128).over(popup).unwrap();
            let hovered = ColorLayer::rgba(16, 40, 96, 128).over(popup).unwrap();
            for (label, bg, fg) in [
                (
                    "selected_entry",
                    selected,
                    ColorLayer::rgba(187, 255, 221, 224).over(selected).unwrap(),
                ),
                (
                    "hovered_entry",
                    hovered,
                    ColorLayer::rgba(255, 238, 204, 224).over(hovered).unwrap(),
                ),
                (
                    "normal_entry",
                    popup,
                    ColorLayer::rgba(255, 238, 204, 224).over(popup).unwrap(),
                ),
            ] {
                let position = find_cell(buffer, label);
                let cell = &buffer[position];
                assert_eq!(cell.bg, bg, "{label}, alpha={alpha}, owner={owner:?}");
                assert_eq!(cell.fg, fg, "{label}, alpha={alpha}, owner={owner:?}");
                assert!(
                    !cell.modifier.contains(Modifier::ITALIC),
                    "underlying syntax must be cleared"
                );
                let selected = label == "selected_entry";
                assert_eq!(
                    cell.modifier.contains(Modifier::UNDERLINED),
                    selected && border_alpha != 0
                );
                if selected && border_alpha != 0 {
                    assert_eq!(
                        cell.underline_color,
                        ColorLayer::rgba(221, 153, 255, border_alpha)
                            .over(bg)
                            .unwrap()
                    );
                }
                let icon = &buffer[(position.0 - 3, position.1)];
                assert_eq!(
                    icon.fg,
                    if selected {
                        ColorLayer::rgba(255, 221, 187, 224).over(bg).unwrap()
                    } else {
                        ColorLayer::rgba(153, 204, 238, 192).over(bg).unwrap()
                    }
                );
            }
            let documentation = ColorLayer::rgba(40, 80, 100, alpha)
                .over(doc_owner)
                .unwrap();
            for label in ["Completion detail", "Completion documentation"] {
                let cell = &buffer[find_cell(buffer, label)];
                assert_eq!(cell.bg, documentation);
                assert_eq!(
                    cell.fg,
                    ColorLayer::rgba(255, 238, 204, 224)
                        .over(documentation)
                        .unwrap()
                );
            }
            snapshot(
                buffer,
                &format!("completion-opacity-{alpha}-{border_alpha}-on-{owner:?}"),
            );
        }
    }
}

#[test]
fn hover_signature_and_code_action_layers_compose_on_the_visible_background() {
    use crate::theme_workbench::ColorLayer;
    use ratatui::{
        style::{Modifier, Style},
        widgets::Block,
    };
    let mut app = language_popup_app();
    app.code_area = Some(Rect::new(7, 12, 80, 17));
    app.lsp_hover_active = true;
    app.lsp_manager.hover_content = Some("Hover documentation".into());
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    for owner in [Color::Rgb(20, 40, 60), Color::Rgb(50, 10, 65)] {
        for alpha in [128, 0] {
            let mut theme = theme_with_popup_and_control_roles();
            for (key, value) in [
                (
                    "editorHoverWidget.background",
                    format!("#285064{alpha:02x}"),
                ),
                ("editorHoverWidget.foreground", "#ffeeccE0".to_owned()),
                (
                    "editorHoverWidget.highlightForeground",
                    "#bbff99C0".to_owned(),
                ),
                ("editorHoverWidget.border", format!("#dd99ff{alpha:02x}")),
                ("editorActionList.background", format!("#322850{alpha:02x}")),
                ("editorActionList.foreground", "#eefcccE0".to_owned()),
                ("editorActionList.focusBackground", "#60482080".to_owned()),
                ("editorActionList.focusForeground", "#fffabbE0".to_owned()),
            ] {
                theme.workbench_colors.insert(key.into(), value.into());
            }
            app.colors = theme.to_theme_colors();
            for (name, draw) in [
                (
                    "hover",
                    crate::ui_dialogs::draw_lsp_hover as fn(&mut ratatui::Frame, &App),
                ),
                ("signature", crate::ui_dialogs::draw_lsp_signature_help),
                ("actions", crate::ui_dialogs::draw_lsp_code_actions),
            ] {
                terminal
                    .draw(|f| {
                        f.render_widget(
                            Block::default()
                                .style(Style::default().bg(owner).add_modifier(Modifier::ITALIC)),
                            f.area(),
                        );
                        draw(f, &app);
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let hover = ColorLayer::rgba(40, 80, 100, alpha).over(owner).unwrap();
                let action = ColorLayer::rgba(50, 40, 80, alpha).over(owner).unwrap();
                let focused = ColorLayer::rgba(96, 72, 32, 128).over(action).unwrap();
                let labels = match name {
                    "hover" => vec![(
                        "Hover documentation",
                        hover,
                        ColorLayer::rgba(255, 238, 204, 224),
                    )],
                    "signature" => vec![
                        ("example(", hover, ColorLayer::rgba(255, 238, 204, 224)),
                        ("value: String", hover, ColorLayer::rgba(187, 255, 153, 192)),
                        (
                            "Parameter documentation",
                            hover,
                            ColorLayer::rgba(255, 238, 204, 224),
                        ),
                    ],
                    _ => vec![
                        ("Code Actions", action, ColorLayer::rgba(238, 252, 204, 224)),
                        (
                            "Fix first issue",
                            focused,
                            ColorLayer::rgba(255, 250, 187, 224),
                        ),
                        (
                            "Refactor second issue",
                            action,
                            ColorLayer::rgba(238, 252, 204, 224),
                        ),
                    ],
                };
                for (label, bg, foreground) in labels {
                    let cell = &buffer[find_cell(buffer, label)];
                    assert_eq!(
                        cell.bg, bg,
                        "{name}, {label}, alpha={alpha}, owner={owner:?}"
                    );
                    assert_eq!(cell.fg, foreground.over(bg).unwrap(), "{name}, {label}");
                    assert!(!cell.modifier.contains(Modifier::ITALIC));
                }
                snapshot(buffer, &format!("{name}-opacity-{alpha}-on-{owner:?}"));
            }
        }
    }
}

#[test]
fn completion_kind_colors_reach_selected_and_unselected_popup_rows() {
    let mut app = App::test_new();
    app.colors = theme_with_icon_and_scrollbar_roles().to_theme_colors();
    app.tabs
        .push(crate::app::Tab::new_file(crate::editor::Editor::new()));
    app.code_area = Some(Rect::new(0, 0, 100, 24));
    app.lsp_completion_active = true;
    app.lsp_manager.completion_items = (0..2)
        .map(|index| crate::lsp::LspCompletionItem {
            label: format!("method_{index}"),
            insert_text: String::new(),
            detail: None,
            kind: Some(lsp_types::CompletionItemKind::METHOD),
            text_edit_range: None,
            additional_text_edits: Vec::new(),
            is_snippet: false,
            original_item: None,
            documentation: None,
        })
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui_search_dialog::draw_lsp_completion(frame, &mut app))
        .unwrap();
    for (label, expected) in [
        ("method_0", Color::Rgb(153, 255, 221)),
        ("method_1", Color::Rgb(255, 238, 153)),
    ] {
        let (x, y) = find_cell(terminal.backend().buffer(), label);
        assert_eq!(terminal.backend().buffer()[(x - 4, y)].fg, expected);
        assert_eq!(
            terminal.backend().buffer()[(x, y)].fg,
            Color::Rgb(238, 238, 238)
        );
    }
    snapshot(
        terminal.backend().buffer(),
        "independent completion icon roles",
    );
}

#[test]
fn completion_kind_badges_have_a_separator_before_aligned_labels() {
    use lsp_types::CompletionItemKind as Kind;
    let mut app = App::test_new();
    app.tabs.push(crate::app::Tab::new_file(crate::editor::Editor::new()));
    app.code_area = Some(Rect::new(0, 0, 100, 30));
    app.lsp_completion_active = true;
    app.lsp_manager.completion_items = [
        (Kind::VARIABLE, "measurements"), (Kind::CLASS, "ResultType"), (Kind::CONSTANT, "MAX_EMAX"),
        (Kind::FUNCTION, "mean"), (Kind::INTERFACE, "Protocol"), (Kind::MODULE, "package"),
        (Kind::PROPERTY, "field"), (Kind::KEYWORD, "while"), (Kind::SNIPPET, "template"), (Kind::ENUM, "Variant"),
    ].into_iter().map(|(kind, label)| crate::lsp::LspCompletionItem {
        label: label.into(), insert_text: label.into(), detail: None, kind: Some(kind),
        text_edit_range: None, additional_text_edits: vec![], is_snippet: false,
        original_item: None, documentation: None,
    }).collect();
    let geometry = crate::ui_search_dialog::lsp_completion_popup_geometry(&mut app, Rect::new(0, 0, 100, 30)).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| crate::ui_search_dialog::draw_lsp_completion(frame, &mut app)).unwrap();
    for item in &app.lsp_manager.completion_items {
        let (x, y) = find_cell(terminal.backend().buffer(), &item.label);
        assert_eq!(x, geometry.inner_area.x + 4, "badge column and one separator: {}", item.label);
        assert_eq!(terminal.backend().buffer()[(x - 1, y)].symbol(), " ");
        assert_eq!(terminal.backend().buffer()[(x - 1, y)].bg, terminal.backend().buffer()[(x, y)].bg);
    }
}

#[test]
fn completion_matches_use_suggest_roles_with_unicode_and_truncated_labels() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_popup_and_control_roles();
    for (key, value) in [
        ("editorSuggestWidget.background", "#182028"),
        ("editorSuggestWidget.foreground", "#ddeeffe0"),
        ("editorSuggestWidget.selectedBackground", "#28384080"),
        ("editorSuggestWidget.selectedForeground", "#ffeebbe0"),
        ("editorSuggestWidget.highlightForeground", "#bbffdde0"),
        ("editorSuggestWidget.focusHighlightForeground", "#ffbbdde0"),
        ("list.highlightForeground", "#ffff99"),
        ("list.focusHighlightForeground", "#99ddff"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    let mut editor = crate::editor::Editor::test_with_text("界h");
    editor.cursor_col = 2;
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    app.code_area = Some(Rect::new(0, 0, 100, 24));
    app.lsp_completion_active = true;
    app.lsp_manager.completion_items = [
        "界highlight_selected".into(),
        "界highlight_normal".into(),
        "界h".repeat(50),
    ]
    .into_iter()
    .map(|label| crate::lsp::LspCompletionItem {
        label,
        insert_text: String::new(),
        detail: None,
        kind: Some(lsp_types::CompletionItemKind::METHOD),
        text_edit_range: None,
        additional_text_edits: Vec::new(),
        is_snippet: false,
        original_item: None,
        documentation: None,
    })
    .collect();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui_search_dialog::draw_lsp_completion(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for (label, matched, ordinary) in [
        (
            "highlight_selected",
            ColorLayer::rgba(255, 187, 221, 224),
            ColorLayer::rgba(255, 238, 187, 224),
        ),
        (
            "highlight_normal",
            ColorLayer::rgba(187, 255, 221, 224),
            ColorLayer::rgba(221, 238, 255, 224),
        ),
    ] {
        let (x, y) = find_cell(buffer, label);
        for column in [x - 2, x] {
            assert_eq!(
                buffer[(column, y)].fg,
                matched.over(buffer[(column, y)].bg).unwrap()
            );
            assert!(
                buffer[(column, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::BOLD)
            );
        }
        assert_eq!(
            buffer[(x + 1, y)].fg,
            ordinary.over(buffer[(x + 1, y)].bg).unwrap()
        );
    }
    let (_, y) = find_cell(buffer, "highlight_normal");
    let last_row = y + 1;
    assert!(
        (0..100).any(|x| buffer[(x, last_row)].symbol() == "界"),
        "long Unicode label remains visible"
    );
    snapshot(buffer, "unicode completion highlight roles");
}

fn language_popup_app() -> App {
    let mut app = App::test_new();
    app.tabs
        .push(crate::app::Tab::new_file(crate::editor::Editor::new()));
    app.code_area = Some(Rect::new(0, 0, 100, 30));
    app.call_hierarchy_active = true;
    app.call_hierarchy_root_name = "example".into();
    app.call_hierarchy_results = ["call_first", "call_second"]
        .into_iter()
        .map(|name| crate::app::CallHierarchyEntry {
            name: name.into(),
            detail: "example.rs".into(),
            file_path: "example.rs".into(),
            line: 3,
            col: 0,
        })
        .collect();
    app.type_hierarchy_active = true;
    app.type_hierarchy_root_name = "Example".into();
    app.type_hierarchy_results = ["TypeFirst", "TypeSecond"]
        .into_iter()
        .map(|name| crate::app::CallHierarchyEntry {
            name: name.into(),
            detail: "example.rs".into(),
            file_path: "example.rs".into(),
            line: 3,
            col: 0,
        })
        .collect();
    app.lsp_references_active = true;
    app.lsp_manager.reference_locations = ["first.rs", "second.rs"]
        .into_iter()
        .map(|name| lsp_types::Location {
            uri: lsp_types::Url::parse(&format!("file:///C:/workspace/{name}")).unwrap(),
            range: lsp_types::Range::new(
                lsp_types::Position::new(3, 0),
                lsp_types::Position::new(3, 5),
            ),
        })
        .collect();
    app.lsp_code_action_active = true;
    app.lsp_manager.code_actions = ["Fix first issue", "Refactor second issue"]
        .into_iter()
        .map(|title| crate::lsp::LspCodeAction {
            title: title.into(),
            edit: None,
            command: None,
            kind: Some("quickfix".into()),
        })
        .collect();
    app.lsp_signature_active = true;
    app.lsp_manager.signature_help = Some(crate::lsp::LspSignatureHelp {
        signatures: vec![crate::lsp::LspSignatureInfo {
            label: "example(value: String)".into(),
            parameters: vec![(8, 21)],
            documentation: Some("Parameter documentation".into()),
        }],
        active_signature: 0,
        active_parameter: 0,
    });
    app.peek_definition = Some(crate::app::PeekDefinition {
        lines: vec!["fn example() {}".into(), "Definition body".into()],
        file_path: "example.rs".into(),
        start_line: 3,
    });
    app
}

#[test]
fn language_popups_use_independent_widget_action_hover_and_peek_roles() {
    use crate::theme_workbench::ColorLayer;
    let mut app = language_popup_app();
    app.colors = theme_with_popup_and_control_roles().to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let widget = Color::Rgb(32, 40, 48);
    let selected_bg = ColorLayer::rgba(48, 72, 56, 192).over(widget).unwrap();
    for (draw, first, second) in [
        (
            crate::ui_dialogs::draw_call_hierarchy as fn(&mut ratatui::Frame, &App),
            "call_first",
            "call_second",
        ),
        (
            crate::ui_dialogs::draw_type_hierarchy,
            "TypeFirst",
            "TypeSecond",
        ),
        (
            crate::ui_dialogs::draw_lsp_references,
            "first.rs:4:1",
            "second.rs:4:1",
        ),
    ] {
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = find_cell(buffer, first);
        assert_eq!(buffer[(x, y)].bg, selected_bg);
        assert_eq!(
            buffer[(x, y)].fg,
            ColorLayer::rgba(255, 255, 187, 224)
                .over(selected_bg)
                .unwrap()
        );
        let right = (x..100).find(|&x| buffer[(x, y)].symbol() == "│").unwrap();
        assert_eq!(
            buffer[(right - 1, y)].bg,
            selected_bg,
            "selection must fill the entire row"
        );
        let ordinary = &buffer[find_cell(buffer, second)];
        assert_eq!(ordinary.bg, widget);
        assert_eq!(
            ordinary.fg,
            ColorLayer::rgba(255, 238, 221, 224).over(widget).unwrap()
        );
    }
    terminal
        .draw(|frame| crate::ui_dialogs::draw_lsp_code_actions(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let action_bg = Color::Rgb(40, 31, 60);
    let focus_bg = ColorLayer::rgba(64, 56, 80, 192).over(action_bg).unwrap();
    let (x, y) = find_cell(buffer, "Fix first issue");
    assert_eq!(buffer[(x, y)].bg, focus_bg);
    assert_eq!(
        buffer[(x, y)].fg,
        ColorLayer::rgba(255, 238, 187, 224).over(focus_bg).unwrap()
    );
    let right = (x..100).find(|&x| buffer[(x, y)].symbol() == "│").unwrap();
    assert_eq!(buffer[(right - 1, y)].bg, focus_bg);
    let ordinary = &buffer[find_cell(buffer, "Refactor second issue")];
    assert_eq!(ordinary.bg, action_bg);
    assert_eq!(
        ordinary.fg,
        ColorLayer::rgba(238, 255, 255, 224)
            .over(action_bg)
            .unwrap()
    );
    snapshot(buffer, "independent code action roles");
    terminal
        .draw(|frame| crate::ui_dialogs::draw_lsp_signature_help(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let hover = Color::Rgb(17, 38, 51);
    let parameter = &buffer[find_cell(buffer, "value: String")];
    assert_eq!(parameter.bg, hover);
    assert_eq!(
        parameter.fg,
        ColorLayer::rgba(255, 221, 153, 192).over(hover).unwrap()
    );
    let documentation = &buffer[find_cell(buffer, "Parameter documentation")];
    assert_eq!(
        documentation.fg,
        ColorLayer::rgba(221, 255, 238, 224).over(hover).unwrap()
    );
    snapshot(buffer, "independent signature help roles");
    terminal
        .draw(|frame| crate::ui_dialogs::draw_peek_definition(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let (x, y) = find_cell(buffer, "fn example() {}");
    let body = Color::Rgb(32, 29, 60);
    assert_eq!(buffer[(x, y)].bg, body);
    assert_eq!(buffer[(x, y)].fg, Color::Rgb(238, 243, 255));
    let gutter = ColorLayer::rgba(23, 34, 44, 192).over(body).unwrap();
    assert_eq!(buffer[(x - 2, y)].bg, gutter);
    assert_eq!(
        buffer[(x - 2, y)].fg,
        ColorLayer::rgba(204, 204, 221, 192).over(gutter).unwrap()
    );
    let title = &buffer[find_cell(buffer, "example.rs (line 4)")];
    let title_bg = ColorLayer::rgba(44, 70, 55, 192).over(body).unwrap();
    assert_eq!(title.bg, title_bg);
    assert_eq!(
        title.fg,
        ColorLayer::rgba(221, 255, 255, 224).over(title_bg).unwrap()
    );
    snapshot(buffer, "independent definition peek roles");
}

#[test]
fn language_popup_labels_remain_readable_across_loaded_palettes() {
    let mut app = language_popup_app();
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        for (draw, labels) in [
            (
                crate::ui_dialogs::draw_call_hierarchy as fn(&mut ratatui::Frame, &App),
                ["call_first", "call_second"],
            ),
            (
                crate::ui_dialogs::draw_type_hierarchy,
                ["TypeFirst", "TypeSecond"],
            ),
            (
                crate::ui_dialogs::draw_lsp_references,
                ["first.rs:4:1", "second.rs:4:1"],
            ),
            (
                crate::ui_dialogs::draw_lsp_code_actions,
                ["Fix first issue", "Refactor second issue"],
            ),
            (
                crate::ui_dialogs::draw_lsp_signature_help,
                ["value: String", "Parameter documentation"],
            ),
            (
                crate::ui_dialogs::draw_peek_definition,
                ["fn example() {}", "example.rs (line 4)"],
            ),
            (
                crate::ui_dialogs::draw_shortcuts,
                ["Keyboard Shortcuts", "New File"],
            ),
        ] {
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            for label in labels {
                let (x, y) = find_cell(buffer, label);
                check_region(
                    buffer,
                    Rect::new(x, y, label.len() as u16, 1),
                    &format!("{} popup: {label}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn modal_inputs_and_actions_keep_their_own_colors_when_focus_changes() {
    use crate::theme_workbench::ColorLayer;
    let mut app = App::test_new();
    app.tabs
        .push(crate::app::Tab::new_file(crate::editor::Editor::new()));
    app.colors = theme_with_popup_and_control_roles().to_theme_colors();
    app.open_goto_line();
    app.goto_line_query = "42:8".into();
    app.goto_line_cursor = 4;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui_modal::draw(frame, &mut app, crate::ui_modal::Kind::Goto))
        .unwrap();
    let widget = Color::Rgb(32, 40, 48);
    for focus in 0..3 {
        app.modal.focus = focus;
        terminal
            .draw(|frame| crate::ui_modal::draw(frame, &mut app, crate::ui_modal::Kind::Goto))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let title = &buffer[find_cell(buffer, "Go to Line")];
        assert_eq!(title.bg, widget);
        assert_eq!(
            title.fg,
            ColorLayer::rgba(255, 238, 221, 224).over(widget).unwrap()
        );
        let value = &buffer[find_cell(buffer, "42:8")];
        let input = ColorLayer::rgba(48, 56, 80, 192).over(widget).unwrap();
        assert_eq!(value.bg, input);
        assert_eq!(
            value.fg,
            ColorLayer::rgba(255, 223, 187, 238).over(input).unwrap()
        );
        for (index, (area, _)) in app.modal.buttons.iter().enumerate() {
            let cell = &buffer[(area.x + 2, area.y)];
            let expected = match (index, focus == index + 1) {
                (0, false) => Color::Rgb(32, 59, 112),
                (0, true) => Color::Rgb(50, 75, 128),
                (_, false) => Color::Rgb(48, 53, 32),
                (_, true) => Color::Rgb(72, 73, 42),
            };
            assert_eq!(cell.bg, expected);
            assert_eq!(
                cell.fg,
                if index == 0 {
                    Color::Rgb(255, 244, 187)
                } else {
                    Color::Rgb(221, 238, 255)
                }
            );
        }
    }
    app.modal.error = Some("Sample input error".into());
    terminal
        .draw(|frame| crate::ui_modal::draw(frame, &mut app, crate::ui_modal::Kind::Goto))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer[find_cell(buffer, "Sample input error")].fg,
        ColorLayer::rgba(255, 186, 170, 221).over(widget).unwrap()
    );
    snapshot(buffer, "independent modal input and button roles");
}

#[test]
fn translucent_action_buttons_compose_on_the_surface_where_they_are_drawn() {
    use crate::theme_workbench::ColorLayer;
    use ratatui::style::Style;
    let mut theme = theme_with_popup_and_control_roles();
    for (id, value) in [
        ("button.background", "#66335580"),
        ("button.foreground", "#ffeedde0"),
        ("button.hoverBackground", "#22556680"),
        ("button.border", "#99ccdd80"),
        ("button.secondaryBackground", "#33665580"),
        ("button.secondaryForeground", "#ddeeffe0"),
        ("button.secondaryHoverBackground", "#66553380"),
    ] {
        theme
            .workbench_colors
            .insert(id.into(), serde_json::Value::String(value.into()));
    }
    let colors = theme.to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
    for surface in [
        Color::Rgb(32, 40, 48),
        Color::Rgb(24, 48, 37),
        Color::Rgb(49, 69, 102),
    ] {
        for primary in [false, true] {
            for hover in [false, true] {
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            ratatui::widgets::Block::default().style(Style::default().bg(surface)),
                            frame.area(),
                        );
                        crate::ui_widgets::draw_action_button(
                            frame,
                            Rect::new(1, 1, 18, 1),
                            "Run",
                            &colors,
                            primary,
                            hover,
                            true,
                        );
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let cell = &buffer[find_cell(buffer, "Run")];
                let fill = match (primary, hover) {
                    (true, false) => ColorLayer::rgba(102, 51, 85, 128),
                    (true, true) => ColorLayer::rgba(34, 85, 102, 128),
                    (false, false) => ColorLayer::rgba(51, 102, 85, 128),
                    (false, true) => ColorLayer::rgba(102, 85, 51, 128),
                };
                let bg = fill.over(surface).unwrap();
                assert_eq!(cell.bg, bg);
                let text = if primary {
                    ColorLayer::rgba(255, 238, 221, 224)
                } else {
                    ColorLayer::rgba(221, 238, 255, 224)
                };
                assert_eq!(cell.fg, text.over(bg).unwrap());
                if primary {
                    assert_eq!(
                        cell.underline_color,
                        ColorLayer::rgba(153, 204, 221, 128).over(bg).unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn shortcut_help_uses_widget_keycap_and_table_header_colors() {
    use crate::theme_workbench::ColorLayer;
    let mut theme = theme_with_popup_and_control_roles();
    for (id, value) in [
        ("keybindingLabel.background", "#304c2ccc"),
        ("keybindingLabel.foreground", "#fff5eec0"),
        ("keybindingTable.headerBackground", "#47284e80"),
    ] {
        theme
            .workbench_colors
            .insert(id.into(), serde_json::Value::String(value.into()));
    }
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_shortcuts(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let widget = Color::Rgb(32, 40, 48);
    let title = &buffer[find_cell(buffer, "Keyboard Shortcuts")];
    assert_eq!(title.bg, widget);
    assert_eq!(
        title.fg,
        ColorLayer::rgba(255, 238, 221, 224).over(widget).unwrap()
    );
    let (x, y) = find_cell(buffer, "── File ──");
    let header = ColorLayer::rgba(71, 40, 78, 128).over(widget).unwrap();
    assert_eq!(buffer[(x, y)].bg, header);
    assert_eq!(
        buffer[(x, y)].fg,
        ColorLayer::rgba(255, 238, 221, 224).over(header).unwrap()
    );
    assert_eq!(buffer[(x + 67, y)].bg, header);
    let key = &buffer[find_cell(buffer, "Ctrl+N")];
    let key_bg = ColorLayer::rgba(48, 76, 44, 204).over(widget).unwrap();
    assert_eq!(key.bg, key_bg);
    assert_eq!(
        key.fg,
        ColorLayer::rgba(255, 245, 238, 192).over(key_bg).unwrap()
    );
    let description = &buffer[find_cell(buffer, "New File")];
    assert_eq!(description.bg, widget);
    assert_eq!(description.fg, title.fg);
    snapshot(buffer, "independent shortcut help roles");
}

#[test]
fn sidebar_border_and_resize_hover_use_theme_layers_on_the_final_surface() {
    let mut theme = theme_with_popup_and_control_roles();
    theme.workbench_colors.insert(
        "sideBar.border".into(),
        serde_json::Value::String("#99ddbb80".into()),
    );
    theme.workbench_colors.insert(
        "sash.hoverBorder".into(),
        serde_json::Value::String("#ffbaaa80".into()),
    );
    let mut app = App::test_new();
    app.colors = theme.to_theme_colors();
    app.tabs
        .push(crate::app::Tab::new_file(crate::editor::Editor::new()));
    app.show_panel = false;
    app.explorer_width = 30;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let sidebar = app.explorer_area.unwrap();
    let position = (sidebar.right() - 1, sidebar.y + 10);
    let resting = &terminal.backend().buffer()[position];
    assert_eq!(resting.symbol(), "│");
    assert_eq!(resting.bg, Color::Rgb(24, 48, 37));
    assert_eq!(resting.fg, Color::Rgb(89, 135, 112));
    for resizing in [false, true] {
        app.resizing_explorer = resizing;
        app.mouse_col = if resizing { u16::MAX } else { position.0 };
        app.mouse_row = if resizing { u16::MAX } else { position.1 };
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[position].fg,
            Color::Rgb(140, 117, 104)
        );
    }
    app.resizing_explorer = false;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    theme.workbench_colors.insert(
        "sideBar.border".into(),
        serde_json::Value::String("#0000".into()),
    );
    app.colors = theme.to_theme_colors();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[position].symbol(), " ");
    assert_eq!(
        terminal.backend().buffer()[position].fg,
        Color::Rgb(24, 48, 37)
    );
}

#[test]
fn modal_and_agent_configuration_text_remains_readable_across_loaded_palettes() {
    let mut app = App::test_new();
    app.tabs
        .push(crate::app::Tab::new_file(crate::editor::Editor::new()));
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        app.open_goto_line();
        app.goto_line_query = "42:8".into();
        app.goto_line_cursor = 4;
        terminal
            .draw(|frame| crate::ui_modal::draw(frame, &mut app, crate::ui_modal::Kind::Goto))
            .unwrap();
        for focus in 0..3 {
            app.modal.focus = focus;
            app.modal.error = Some("Sample input error".into());
            terminal
                .draw(|frame| crate::ui_modal::draw(frame, &mut app, crate::ui_modal::Kind::Goto))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for label in [
                "Go to Line",
                "42:8",
                "Sample input error",
                "Go to line",
                "Cancel",
            ] {
                let (x, y) = find_cell(buffer, label);
                check_region(
                    buffer,
                    Rect::new(x, y, label.len() as u16, 1),
                    &format!("{} modal focus={focus}, {label}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
        app.open_agent_settings();
        for editing in [false, true] {
            if editing {
                crate::ui_agent_settings::activate(&mut app, crate::ui_agent_settings::Action::Add);
            }
            app.agent_settings.error = Some("Sample configuration error".into());
            terminal
                .draw(|frame| crate::ui_agent_settings::draw(frame, &mut app))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let (left, top) = find_cell(buffer, "┌");
            let (right, _) = find_cell(buffer, "┐");
            let (_, bottom) = find_cell(buffer, "└");
            check_region(
                buffer,
                Rect::new(
                    left + 1,
                    top + 1,
                    right.saturating_sub(left + 2),
                    bottom.saturating_sub(top + 1),
                ),
                &format!("{} agent configuration editing={editing}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            if editing && theme.name == "Dark+" {
                snapshot(terminal.backend().buffer(), "agent configuration dark plus");
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn translucent_scrollbar_roles_compose_on_the_owning_surface_in_each_state() {
    use crate::theme_workbench::ColorLayer;
    let colors = theme_with_icon_and_scrollbar_roles().to_theme_colors();
    for surface in [
        colors.bg_editor,
        colors.bg_sidebar,
        colors.ui.bg_panel,
        colors.bg_tab_active,
    ] {
        let track = ColorLayer::rgba(34, 68, 136, 64).over(surface).unwrap();
        for (hovered, active, tint) in [
            (false, false, (255, 255, 255, 96)),
            (true, false, (255, 187, 153, 128)),
            (true, true, (187, 255, 153, 128)),
        ] {
            let thumb = ColorLayer::rgba(tint.0, tint.1, tint.2, tint.3)
                .over(track)
                .unwrap();
            assert_eq!(
                colors.ui.scrollbar_colors(surface, hovered, active),
                (track, thumb)
            );
            let mut terminal = Terminal::new(TestBackend::new(30, 1)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_editor_highlight::draw_horizontal_scrollbar(
                        frame,
                        crate::ui_editor_highlight::HScrollbarParams {
                            area: frame.area(),
                            total_width: 100,
                            visible_width: 20,
                            scroll: 0,
                            bg_editor: surface,
                            hovered,
                            active,
                            colors: &colors.ui,
                        },
                    )
                })
                .unwrap();
            assert_eq!(terminal.backend().buffer()[(0, 0)].bg, thumb);
            assert_eq!(terminal.backend().buffer()[(29, 0)].bg, track);
        }
    }
}

#[test]
fn outline_and_search_text_remain_readable_across_installed_theme_surfaces() {
    let mut app = App::test_new();
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app.outline_items = ["function", "struct", "module"]
        .into_iter()
        .enumerate()
        .map(|(line, kind)| crate::app::SymbolItem {
            name: format!("sample_{kind}"),
            kind,
            line,
            tab_index: None,
            file_name: None,
            file_path: None,
        })
        .collect();
    app.global_search_query = "needle".into();
    app.global_search_results = (0..2)
        .map(|line| crate::app::GlobalSearchResult {
            file_path: app.explorer.root.join("example.py"),
            line_num: line + 1,
            line_text: "before needle after".into(),
            col_start: 7,
            col_end: 13,
            context_before: None,
            context_after: None,
        })
        .collect();
    let mut failures = Vec::new();
    for theme in audit_themes() {
        app.colors = theme.to_theme_colors();
        for focused in [false, true] {
            app.active_pane = if focused {
                crate::app::ActivePane::Explorer
            } else {
                crate::app::ActivePane::Editor
            };
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal
                .draw(|frame| crate::ui_chrome::draw_outline(frame, &app, frame.area()))
                .unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(3, 1, 97, 3),
                &format!("{} outline, focus={focused}", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            terminal
                .draw(|frame| crate::ui_search_dialog::draw_global_search(frame, &mut app))
                .unwrap();
            for label in ["example.py:1 before ", "example.py:2 before "] {
                let (x, y) = find_cell(terminal.backend().buffer(), label);
                check_region(
                    terminal.backend().buffer(),
                    Rect::new(x, y, 35, 1),
                    &format!("{} search result, focus={focused}", theme.name),
                    &mut failures,
                    &app.colors.ui,
                );
            }
        }
    }
    assert_readable(failures);
}

#[test]
fn floating_widgets_keep_translucent_and_transparent_fills_on_the_view_they_cover() {
    use crate::theme_workbench::ColorLayer;
    use ratatui::{style::Style, widgets::Paragraph};
    for case in [0, 1, 2, 3, 5, 6, 7, 8, 9, 10] {
        let mut app = match case {
            9 => extension_overlay_app(),
            10 => screencast_app(0),
            _ => language_popup_app(),
        };
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        app.rename.symbol_old = "old_name".into();
        app.rename.symbol_new = "new_name".into();
        app.folder_browser_path = app.explorer.root.clone();
        app.change_dir_query = app.explorer.root.display().to_string();
        let mut terminal = Terminal::new(TestBackend::new(100, 36)).unwrap();
        for (fill, owner, expected_bg) in [
            ("#28506480", Color::Rgb(20, 40, 60), Color::Rgb(30, 60, 80)),
            ("#28506480", Color::Rgb(50, 10, 65), Color::Rgb(45, 45, 83)),
            ("#28506400", Color::Rgb(20, 40, 60), Color::Rgb(20, 40, 60)),
        ] {
            let mut theme = theme_with_popup_and_control_roles();
            theme
                .workbench_colors
                .insert("editorWidget.background".into(), fill.into());
            app.colors = theme.to_theme_colors();
            if case == 5 {
                app.open_agent_settings();
            }
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        Paragraph::new("Underlying editor").style(Style::default().bg(owner)),
                        frame.area(),
                    );
                    match case {
                        0 => crate::ui_dialogs::draw_call_hierarchy(frame, &app),
                        1 => crate::ui_dialogs::draw_type_hierarchy(frame, &app),
                        2 => crate::ui_dialogs::draw_lsp_references(frame, &app),
                        3 => crate::ui_dialogs::draw_shortcuts(frame, &app),
                        5 => crate::ui_agent_settings::draw(frame, &mut app),
                        6 => crate::ui_modal::draw(frame, &mut app, crate::ui_modal::Kind::Rename),
                        7 => crate::ui_folder_browser::draw(frame, &mut app),
                        8 => crate::ui_search_dialog::draw_global_search(frame, &mut app),
                        9 => crate::ui_dialogs::draw_extension_overlays(frame, &app),
                        _ => crate::ui_dialogs::draw_screencast_overlay(frame, &app),
                    }
                })
                .unwrap();
            let label = [
                "Callers of",
                "Supertypes of",
                "References",
                "Keyboard Shortcuts",
                "Keyboard Shortcuts",
                "Agent Chat",
                "Rename",
                "Open Folder",
                "Search in Files",
                "Extension overlay",
                "Keys",
            ][case];
            let buffer = terminal.backend().buffer();
            let title = &buffer[find_cell(buffer, label)];
            assert_eq!(
                title.bg, expected_bg,
                "widget {case}, {fill}, owner={owner:?}"
            );
            let expected_fg = if case == 4 {
                app.colors.ui.settings_header_color(expected_bg)
            } else {
                ColorLayer::rgba(255, 238, 221, 224)
                    .over(expected_bg)
                    .unwrap()
            };
            assert_eq!(
                title.fg, expected_fg,
                "widget {case}: title foreground on the final fill"
            );
            if case == 5 {
                let draft = &buffer[find_cell(buffer, "Connect any local agent")];
                assert_eq!(draft.bg, expected_bg);
            }
            snapshot(
                buffer,
                &format!("floating widget {case} {fill} on {owner:?}"),
            );
        }
    }
}
