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

// Frozen pre-optimization geometry, including its temporary names and width
// list. This independently checks the binary offset search and benchmarks the
// same behavior with distant tabs and narrow tab bars.
fn original_width(app: &App, tab: &Tab) -> usize {
    if let TabKind::Chat(key) = tab.kind {
        return app.chat.editor_tab_title(key).width() + 6 + if tab.pinned { 4 } else { 0 };
    }
    let name = if let Some(revision) = tab
        .editor
        .blame
        .revision
        .as_ref()
        .filter(|_| tab.kind == TabKind::GitRevision)
    {
        revision.tab_title()
    } else if tab.kind == TabKind::CommitDetails {
        "Commit details".to_string()
    } else if tab.kind == TabKind::MarkdownPreview {
        format!("Preview: {}", tab.editor.file_name())
    } else if matches!(tab.kind, TabKind::Diff(_)) {
        format!("{} (Diff)", tab.editor.file_name())
    } else {
        tab.editor.file_name().to_string()
    };
    1 + if tab.pinned { 4 } else { 0 }
        + if tab.has_unsaved_changes() { 2 } else { 0 }
        + if matches!(
            tab.kind,
            TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails
        ) {
            2
        } else {
            0
        }
        + if tab.kind == TabKind::File && tab.editor.disk_unavailable {
            1
        } else {
            0
        }
        + name.width()
        + 3
}

fn original_visible(app: &App, offset: usize, width: usize) -> Option<usize> {
    if app.tabs.is_empty() || offset >= app.tabs.len() || width == 0 {
        return None;
    }
    let mut used = if offset > 0 { 2 } else { 0 };
    let mut widths = Vec::new();
    for (idx, tab) in app.tabs.iter().enumerate().skip(offset) {
        let w = original_width(app, tab);
        if !widths.is_empty() && used + w > width {
            break;
        }
        used += w;
        widths.push((idx, w));
    }
    if widths
        .last()
        .is_some_and(|(idx, _)| idx + 1 < app.tabs.len())
    {
        while widths.len() > 1 && used + 2 > width {
            used = used.saturating_sub(widths.pop().unwrap().1);
        }
    }
    widths.last().map(|(idx, _)| *idx)
}

fn original_offset(app: &App, mut offset: usize, width: usize) -> usize {
    if app.tabs.is_empty() {
        return 0;
    }
    if app.tab_scroll_manual_anchor == Some(app.active_tab) {
        return offset.min(app.max_tab_scroll_offset());
    }
    if app.active_tab < offset {
        offset = app.active_tab;
    }
    if width == 0 {
        if app.active_tab >= offset + 8 {
            offset = app.active_tab.saturating_sub(7);
        }
        return offset;
    }
    while original_visible(app, offset, width).is_some_and(|last| app.active_tab > last) {
        offset = (offset + 1).min(app.active_tab);
    }
    if original_visible(app, offset, width).is_some_and(|last| last + 1 >= app.tabs.len()) {
        while offset > 0 {
            let candidate = offset - 1;
            if !original_visible(app, candidate, width)
                .is_some_and(|last| candidate <= app.active_tab && app.active_tab <= last)
            {
                break;
            }
            offset = candidate;
        }
    }
    offset
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
fn tab_geometry_and_binary_reveal_match_original_for_unicode_and_tiny_bars() {
    let mut app = tabs_app(20);
    for width in [0, 1, 2, 3, 4, 5, 8, 12, 24, 40, 80, 160, 600] {
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, width as u16, 1));
        for offset in 0..app.tabs.len() {
            assert_eq!(
                app.visible_last_tab_index(&app.tabs, offset, width),
                original_visible(&app, offset, width)
            );
            for active in 0..app.tabs.len() {
                app.active_tab = active;
                app.tab_scroll_offset = offset;
                app.tab_scroll_manual_anchor = None;
                let prefix=crate::ui_desks::controls(&app,app.tab_bar_area.unwrap(),app.active_group).2 as usize;
                let expected = original_offset(&app, offset, width.saturating_sub(prefix));
                app.ensure_active_tab_visible();
                assert_eq!(
                    app.tab_scroll_offset, expected,
                    "width={width} offset={offset} active={active}"
                );
            }
        }
    }
    app.tab_scroll_offset = 19;
    app.active_tab = 4;
    app.tab_scroll_manual_anchor = Some(4);
    let expected = original_offset(&app, 19, 596);
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, expected);
    assert_eq!(app.tab_scroll_manual_anchor, Some(4));
    app.active_tab = 17;
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_manual_anchor, None);
    app.tabs.clear();
    app.ensure_active_tab_visible();
    assert_eq!(app.tab_scroll_offset, 0);
}

#[test]
#[ignore = "manual release distant tab switching benchmark"]
fn benchmark_distant_tab_reveal() {
    for count in [2_000, 10_000] {
        let mut app = tabs_app(count);
        app.active_tab = count - 1;
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 160, 1));
        let mut before = Vec::new();
        let mut after = Vec::new();
        let expected = original_offset(&app, 0, 160);
        for sample in 0..9 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                app.tab_scroll_offset = 0;
                let start = std::time::Instant::now();
                let actual = if optimized {
                    app.ensure_active_tab_visible();
                    app.tab_scroll_offset
                } else {
                    original_offset(&app, 0, 160)
                };
                let elapsed = start.elapsed();
                assert_eq!(actual, expected);
                if optimized {
                    after.push(elapsed);
                } else {
                    before.push(elapsed);
                }
            }
        }
        before.sort_unstable();
        after.sort_unstable();
        eprintln!(
            "distant tab reveal {count}tabs: original {:?}, optimized {:?}, {:.1}x",
            before[4],
            after[4],
            before[4].as_secs_f64() / after[4].as_secs_f64()
        );
    }
}
