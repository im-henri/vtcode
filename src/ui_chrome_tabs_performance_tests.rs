use super::*;
use crate::{
    app::{Tab, TabKind},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};

fn fixture(count: usize, duplicate: bool) -> App {
    let mut app = App::test_new();
    app.terminal_has_focus = false;
    app.tabs = (0..count)
        .map(|index| {
            let mut editor = Editor::new();
            let name = if duplicate {
                format!("file-{:02}界.rs", index % 17)
            } else {
                format!("file-{index:05}界.rs")
            };
            editor.file_path = Some(std::path::PathBuf::from(if duplicate {
                format!("cores/core-{index}/common/src/{name}")
            } else {
                format!("folder-{index}/{name}")
            }));
            editor.modified = index % 7 == 0;
            editor.disk_unavailable = index % 11 == 0;
            let mut tab = Tab::new_file(editor);
            tab.pinned = index % 5 == 0;
            tab
        })
        .collect();
    app
}

fn scene(app: &App, width: u16, indexed: bool) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width + 4, 3)).unwrap();
    let full_area = Rect::new(2, 1, width, 1);
    let prefix=crate::ui_desks::controls(app,full_area,app.active_group).2;
    let area=Rect::new(full_area.x+prefix,full_area.y,full_area.width-prefix,1);
    let duplicates = indexed
        .then(|| TabNameDuplicates::for_viewport(app, width))
        .flatten();
    terminal
        .draw(|frame| {
            crate::ui_desks::draw_controls(frame,app,full_area,app.active_group);
            draw_tab_bar_labels(
                frame,
                app,
                area,
                std::time::Instant::now(),
                duplicates.as_ref(),
            )
        })
        .unwrap()
        .buffer
        .clone()
}

#[test]
fn viewport_duplicate_names_match_individual_labels_and_exact_frames_after_mutations() {
    for duplicate in [false, true] {
        let mut app = fixture(150, duplicate);
        app.tabs[2].kind = TabKind::MarkdownPreview;
        app.tabs[3].kind = TabKind::Diff(1);
        app.tabs[4].kind = TabKind::Settings;
        app.tabs[5].kind = TabKind::KeyboardShortcuts;
        app.tabs[6].kind = TabKind::ExtensionView;
        app.tabs[7].kind = TabKind::GitRevision;
        for width in [0, 1, 2, 4, 8, 24, 80, 160, 400] {
            for scroll in [0, 1, 7, 75, 145, 149, 150] {
                app.tab_scroll_offset = scroll;
                app.active_tab = scroll.min(149);
                let duplicates = TabNameDuplicates::for_viewport(&app, width);
                for (index, tab) in app.tabs.iter().enumerate().skip(scroll) {
                    assert_eq!(
                        tab_display_name_with_duplicates(&app, index, tab, duplicates.as_ref()),
                        tab_display_name(&app, index, tab)
                    );
                }
                assert_eq!(
                    scene(&app, width, true),
                    scene(&app, width, false),
                    "width={width}, scroll={scroll}"
                );
                assert!(
                    duplicates
                        .as_ref()
                        .is_none_or(|cache| cache.name_count() <= usize::from(width) / 4 + 2)
                );
            }
        }
        app.tabs.swap(1, 89);
        app.tabs.remove(3);
        app.tabs[1].editor.file_path = Some("renamed/path/renamed.rs".into());
        app.tabs[2].kind = TabKind::File;
        app.tabs[3].editor.file_path = app.tabs[2].editor.file_path.clone();
        app.tab_scroll_offset = 0;
        assert_eq!(scene(&app, 160, true), scene(&app, 160, false));
    }
    assert!(TabNameDuplicates::for_viewport(&fixture(64, false), 160).is_none());
}

#[test]
fn tab_hits_match_drawn_buttons_and_stay_inside_the_bar() {
    let mut app = fixture(150, true);
    for width in [0, 1, 2, 4, 8, 24, 80, 180, 400] {
        for scroll in [0, 1, 75, 149, 150] {
            app.tab_scroll_offset = scroll;
            app.active_tab = scroll.min(149);
            let area = Rect::new(2, 1, width, 1);
            let hits = TabBarHits::new(&app, area);
            let buffer = scene(&app, width, true);
            for button in [hits.left, hits.right, hits.new_tab] {
                if !button.is_empty() {
                    assert_eq!(button.intersection(area), button);
                    assert!(hits.tab_at(button.x).is_none());
                }
            }
            if hits.right.width == 2 {
                assert_eq!(buffer[(hits.right.x + 1, area.y)].symbol(), "▸");
            }
            if !hits.new_tab.is_empty() {
                assert_eq!(buffer[(hits.new_tab.x + 1, area.y)].symbol(), "+");
                assert!(hits.right.is_empty() || hits.new_tab.x >= hits.right.right());
            }
            for &(index, rect, close) in &hits.tabs {
                assert_eq!(rect.intersection(area), rect);
                assert_eq!(hits.tab_at(rect.x).map(|hit| hit.0), Some(index));
                if app.tabs[index].pinned {
                    assert!(close.is_empty());
                } else if index == app.active_tab && close.width == 3 {
                    assert_eq!(buffer[(close.x + 1, area.y)].symbol(), "×");
                }
            }
            assert!(hits.tab_at(area.right()).is_none());
        }
    }
}

#[test]
fn tab_hits_follow_renames_reorders_dirty_state_and_unicode_without_stale_geometry() {
    let mut app = fixture(150, false);
    let area = Rect::new(3, 2, 240, 1);
    for change in 0..6 {
        match change {
            0 => app.tabs.swap(0, 149),
            1 => app.tabs[1].editor.file_path = app.tabs[0].editor.file_path.clone(),
            2 => app.tabs[0].editor.modified = !app.tabs[0].editor.modified,
            3 => app.tabs[0].pinned = !app.tabs[0].pinned,
            4 => app.tabs[0].editor.file_path = Some("renamed/界 café.rs".into()),
            _ => { app.tabs.remove(0); }
        }
        let hits = TabBarHits::new(&app, area);
        let mut x = area.x+crate::ui_desks::controls(&app,area,app.active_group).2;
        for &(index, rect, _) in &hits.tabs {
            assert_eq!(rect.x, x);
            let width = tab_render_width(&app, index, &app.tabs[index]);
            assert_eq!(rect.width, width.min(area.right() - x));
            x = x.saturating_add(width);
        }
    }
    app.tabs[0].editor.file_path = Some(format!("{}.rs", "界".repeat(40_000)).into());
    let hits = TabBarHits::new(&app, Rect::new(65_500, 0, 20, 1));
    assert_eq!(hits.tabs[0].1.width, 16);
    assert!(hits.tabs[0].2.is_empty(), "off-screen close must not wrap into the pane");
    assert!(hits.tab_at(65_520).is_none());
}

#[test]
fn split_tab_bars_share_path_hints_and_matching_hit_geometry() {
    let mut app = fixture(100, false);
    app.tabs[0].editor.file_path = Some("cores/arm_cache/src/entry.cps".into());
    app.tabs[0].pinned = false;
    app.tabs[0].editor.modified = false;
    app.groups = vec![crate::app::EditorGroup::default(), crate::app::EditorGroup::default()];
    let mut other = Tab::new_file(Editor::new());
    other.editor.file_path = Some("cores/axi_rv32/src/entry.cps".into());
    app.groups[1].tabs.push(other);
    let mut terminal = Terminal::new(TestBackend::new(100, 4)).unwrap();
    terminal.draw(|frame| {
        draw_tab_bar_for_group(frame, &app, Rect::new(0, 0, 100, 1), 0, std::time::Instant::now());
        draw_tab_bar_for_group(frame, &app, Rect::new(0, 2, 100, 1), 1, std::time::Instant::now());
    }).unwrap();
    let buffer = terminal.backend().buffer();
    for (group, row, branch) in [(0, 0, "arm_cache"), (1, 2, "axi_rv32")] {
        let text: String = (0..100).map(|x| buffer[(x, row)].symbol()).collect();
        assert!(text.contains(branch), "{text}");
        let hits = TabBarHits::for_group(&app, Rect::new(0, row, 100, 1), group);
        let (_, rect, close) = hits.tabs[0];
        assert_eq!(hits.tab_at(rect.x).unwrap().0, 0);
        assert_eq!(buffer[(close.x + 1, row)].symbol(), "×");
    }
}

#[test]
fn collapsed_groups_cache_later_visible_tabs_instead_of_hidden_candidates() {
    let mut app = fixture(140, true);
    for tab in &mut app.tabs { tab.pinned = false; }
    app.tabs.last_mut().unwrap().pinned = true;
    app.desks.selected_tabs = app.tabs.iter().skip(1).take(138)
        .map(|tab| tab.editor.navigation_id).collect();
    app.group_selected_tabs("Hidden", None);
    app.desks.groups.values_mut().next().unwrap().collapsed = true;
    let index = TabNameDuplicates::for_viewport(&app, 180).unwrap();
    assert!(index.context(app.tabs.last().unwrap()).is_some(), "the later visible tab must use the indexed path");
    let hits = TabBarHits::new(&app, Rect::new(0, 0, 180, 1));
    assert!(hits.tabs.iter().any(|(index, _, _)| *index == app.tabs.len() - 1));
}

#[test]
fn preview_paths_are_distinct_and_compare_titles_stay_in_their_own_pane() {
    let mut app = fixture(2, false);
    app.tabs[0].editor.file_path = Some("cores/arm_cache/src/entry.md".into());
    app.tabs[1].editor.file_path = Some("cores/axi_rv32/src/entry.md".into());
    for tab in &mut app.tabs { tab.kind = TabKind::MarkdownPreview; }
    let first = tab_display_name(&app, 0, &app.tabs[0]);
    let second = tab_display_name(&app, 1, &app.tabs[1]);
    assert!(first.starts_with("Preview: entry.md") && first.contains("arm_cache"));
    assert!(second.starts_with("Preview: entry.md") && second.contains("axi_rv32"));

    for tab in &mut app.tabs { tab.kind = TabKind::File; }
    app.active_tab = 0;
    app.diff_mode = crate::app::DiffMode::Inline;
    app.diff_tab_label = Some("Active comparison".into());
    assert!(app.active_editor_matches_diff_target());
    let other = app.tabs.remove(1);
    app.groups = vec![crate::app::EditorGroup::default(), crate::app::EditorGroup::default()];
    app.groups[1].tabs.push(other);
    assert_eq!(tab_display_name(&app, 0, &app.tabs[0]), "Active comparison");
    assert!(tab_display_name(&app, 0, &app.groups[1].tabs[0]).contains("axi_rv32"));
}

#[test]
fn preview_disambiguated_tab_labels() {
    let Some(directory) = std::env::var_os("VTCODE_TAB_NAMES_PREVIEW_DIR") else { return; };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for (theme, colors) in [("dark", crate::app_theme::EditorTheme::Dark.colors()), ("light", crate::app_theme::EditorTheme::Light.colors())] {
        for width in [80, 140] {
            let mut app = App::test_new();
            app.colors = colors;
            app.show_explorer = false;
            app.show_panel = false;
            app.show_minimap = false;
            app.chat.visible = false;
            for core in ["arm_cache", "axi_rv32"] {
                let mut editor = Editor::test_with_text("// Core entry point\nfunction entry() {\n    initialize_core();\n}\n");
                editor.file_path = Some(format!("cores/{core}/src/entry.cps").into());
                app.tabs.push(Tab::new_file(editor));
            }
            app.active_tab = 0;
            let mut terminal = Terminal::new(TestBackend::new(width, 15)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
                "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
                "bold": cell.modifier.contains(Modifier::BOLD),
            })).collect();
            std::fs::write(directory.join(format!("tabs-{theme}-{width}.json")),
                serde_json::to_vec(&serde_json::json!({"width": width, "height": 15, "cells": cells})).unwrap()).unwrap();
        }
    }
}

#[test]
#[ignore = "comparative release tab mouse geometry benchmark"]
fn benchmark_tab_mouse_geometry() {
    // The previous hover path walked the labels independently for the tab,
    // close button, new button, overflow position and overflow detection.
    fn repeated_widths(app: &App, width: usize) -> usize {
        let mut checksum = 0;
        for _ in 0..5 {
            let mut used = 2;
            for (visible, (index, tab)) in app.tabs.iter().enumerate()
                .skip(app.tab_scroll_offset).enumerate()
            {
                let size = tab_render_width(app, index, tab) as usize;
                if used + size > width && visible > 0 { break; }
                used += size;
            }
            checksum += used;
        }
        checksum
    }
    for count in [100, 1_000, 10_000] {
        for duplicate in [false, true] {
            let mut app = fixture(count, duplicate);
            app.tab_scroll_offset = count / 2;
            app.active_tab = app.tab_scroll_offset;
            let area = Rect::new(0, 0, 180, 1);
            let mut samples = [Vec::new(), Vec::new()];
            for sample in 0..7 {
                for optimized in [sample % 2 == 0, sample % 2 != 0] {
                    let start = std::time::Instant::now();
                    for _ in 0..100 {
                        if optimized {
                            std::hint::black_box(TabBarHits::new(std::hint::black_box(&app), area));
                        } else {
                            std::hint::black_box(repeated_widths(std::hint::black_box(&app), 180));
                        }
                    }
                    samples[usize::from(optimized)].push(start.elapsed() / 100);
                }
            }
            samples.iter_mut().for_each(|samples| samples.sort_unstable());
            eprintln!("tab mouse {count}tabs duplicate={duplicate}: repeated {:?}, batched {:?}",
                samples[0][3], samples[1][3]);
        }
    }
}

#[test]
#[ignore = "comparative release normal tab-bar redraw benchmark"]
fn benchmark_tab_bar_duplicate_names() {
    for count in [100, 1_000, 10_000] {
        for duplicate in [false, true] {
            let mut app = fixture(count, duplicate);
            app.tab_scroll_offset = count / 2;
            app.active_tab = app.tab_scroll_offset;
            let area = Rect::new(0, 0, 180, 1);
            let expected = scene(&app, 180, false);
            assert_eq!(scene(&app, 180, true), expected);
            let mut before = Vec::new();
            let mut after = Vec::new();
            for sample in 0..7 {
                for indexed in [sample % 2 == 0, sample % 2 != 0] {
                    let mut terminal = Terminal::new(TestBackend::new(180, 1)).unwrap();
                    let started = std::time::Instant::now();
                    for _ in 0..50 {
                        let duplicates = indexed
                            .then(|| TabNameDuplicates::for_viewport(&app, area.width))
                            .flatten();
                        let completed = terminal
                            .draw(|frame| {
                                draw_tab_bar_labels(
                                    frame,
                                    &app,
                                    area,
                                    std::time::Instant::now(),
                                    duplicates.as_ref(),
                                )
                            })
                            .unwrap();
                        std::hint::black_box(completed.buffer);
                    }
                    let elapsed = started.elapsed() / 50;
                    if indexed {
                        after.push(elapsed);
                    } else {
                        before.push(elapsed);
                    }
                }
            }
            before.sort_unstable();
            after.sort_unstable();
            eprintln!(
                "tab-bar redraw {count}tabs duplicate={duplicate}: original {:?}, viewport index {:?}",
                before[3], after[3]
            );
        }
    }
}
