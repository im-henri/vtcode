use super::*;
use std::path::PathBuf;

fn file_tabs_app(count: usize) -> App {
    let mut app = App::test_new();
    app.tabs = (0..count).map(|i| {
        let mut editor = Editor::test_with_text("value = 1\n");
        editor.file_path = Some(PathBuf::from(format!("/benchmark/tab-{i}.py")));
        Tab::new_file(editor)
    }).collect();
    app.active_tab = count.saturating_sub(1);
    app.tab_mru_order = (0..count).rev().collect();
    app
}

#[test]
#[ignore = "manual optimized tab lifecycle benchmark; setup excluded"]
fn benchmark_tab_lifecycle() {
    for count in [1_000, 4_000] {
        for saved in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..5 {
                let mut app = file_tabs_app(count);
                let start = std::time::Instant::now();
                if saved { app.close_saved_tabs(); }
                else { app.close_tabs_matching(|_| true); }
                samples.push(start.elapsed());
                assert!(app.tabs.is_empty());
            }
            samples.sort_unstable();
            eprintln!("close {} {count} tabs: {:?}", if saved { "saved" } else { "group" }, samples[2]);
        }
    }
    let mut app = file_tabs_app(4_000);
    let mut other = file_tabs_app(4_000);
    let mut group = crate::app::EditorGroup::new();
    group.tabs = std::mem::take(&mut other.tabs);
    app.groups = vec![crate::app::EditorGroup::new(), group];
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            app.switch_group(1);
            app.switch_group(0);
        }
        samples.push(start.elapsed() / 20_000);
    }
    samples.sort_unstable();
    eprintln!("switch pane (4,000 tabs): {:?}", samples[3]);
}

fn tabs_app(count: usize) -> App {
    let mut app = App::test_new();
    app.tabs = (0..count)
        .map(|idx| {
            let mut editor = Editor::new();
            let names = [
                "a",
                "界.rs",
                "🧑‍💻.ts",
                "e\u{301}.rs",
                "a_quite_long_file_name.rs",
                "\u{301}",
                "",
            ];
            editor.file_path = Some(PathBuf::from(format!(
                "folder{idx}/{}",
                names[idx % names.len()]
            )));
            editor.modified = idx % 5 == 0;
            editor.disk_unavailable = idx % 7 == 0;
            let mut tab = Tab::new_file(editor);
            tab.pinned = idx % 3 == 0;
            tab.kind = match idx % 6 {
                0 => TabKind::MarkdownPreview,
                1 => TabKind::Diff(idx as u64),
                2 => TabKind::CommitDetails,
                3 => TabKind::Terminal,
                _ => TabKind::File,
            };
            tab
        })
        .collect();
    app
}

#[test]
fn cell_reveal_keeps_unicode_tabs_visible_in_narrow_bars() {
    let mut app = tabs_app(20);
    for width in [1, 2, 3, 4, 5, 8, 12, 24, 40, 80, 160, 600] {
        let area = ratatui::layout::Rect::new(0, 0, width, 1);
        app.tab_bar_area = Some(area);
        for offset in 0..app.tabs.len() {
            for active in 0..app.tabs.len() {
                app.active_tab = active;
                app.tab_scroll_offset = offset;
                app.tab_scroll_manual_anchor = None;
                app.tab_strip_scroll = None;
                app.ensure_active_tab_visible();
                let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, area);
                if width > crate::ui_desks::controls(&app, area, app.active_group).2 {
                    assert!(hits.tabs.iter().any(|(index, _, _)| *index == active),
                        "width={width} offset={offset} active={active}");
                }
                for (_, rect, close) in hits.tabs {
                    assert_eq!(rect.intersection(area), rect);
                    assert!(close.is_empty() || close.intersection(rect) == close);
                }
            }
        }
    }
}

#[test]
fn reveal_keeps_manual_index_scroll_and_unmeasured_layout_fallback() {
    let mut app = tabs_app(20);
    app.active_tab = 19;
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, 12);
    app.active_tab = 4;
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, 4);
    app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 40, 1));
    app.tab_scroll_offset = 19;
    app.tab_scroll_manual_anchor = Some(4);
    let expected = app.max_tab_scroll_offset();
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, 19.min(expected));
    assert_eq!(app.tab_scroll_manual_anchor, Some(4));
    app.active_tab = 17;
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_manual_anchor, None);
    assert!(app.tab_strip_scroll.is_some());
    app.tabs.clear();
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, 0);
    assert!(app.tab_strip_scroll.is_none());
}

#[test]
#[ignore = "manual release distant tab switching benchmark"]
fn benchmark_distant_tab_reveal() {
    for count in [2_000, 10_000] {
        let mut app = tabs_app(count);
        let area = ratatui::layout::Rect::new(0, 0, 160, 1);
        app.tab_bar_area = Some(area);
        let mut samples = Vec::new();
        for active in [count - 1, 0].into_iter().cycle().take(18) {
            app.active_tab = active;
            let start = std::time::Instant::now();
            app.ensure_active_tab_visible();
            samples.push(start.elapsed());
            assert!(crate::ui_chrome_tabs::TabBarHits::new(&app, area)
                .tabs.iter().any(|(index, _, _)| *index == active));
        }
        samples.sort_unstable();
        eprintln!("cell reveal {count} tabs: {:?}", samples[9]);
    }
}
