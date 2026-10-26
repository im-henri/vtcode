use super::*;
use crate::{
    animation_config::AnimationEffect,
    app::{ActivePane, SidebarView, Tab},
    editor::Editor,
    explorer::{FileEntry, GitStatus},
};
use ratatui::{Terminal, backend::TestBackend};
use std::time::Instant;

fn fixture(count: usize) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("let value = 42;\n".repeat(100).as_str());
    editor.file_path = Some("workspace/file-00000界.rs".into());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    app.show_explorer = true;
    app.show_open_editors = true;
    app.cursor_blinking = false;
    app.animation_settings.set(AnimationEffect::TabSlide, true);
    app.explorer.root = "workspace".into();
    app.explorer.entries = (0..count)
        .map(|index| {
            let name = format!("file-{index:05}界.rs");
            FileEntry {
                path: app.explorer.root.join(&name),
                name,
                is_dir: false,
                depth: 0,
                expanded: false,
                git_status: GitStatus::Modified,
                git_status_summary: GitStatus::Modified.into(),
            }
        })
        .collect();
    app.explorer_multi_selected = app
        .explorer
        .entries
        .iter()
        .step_by(2)
        .map(|entry| entry.path.clone())
        .collect();
    app
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>, cosmetic: bool) -> Buffer {
    app.rendering_animation_frame = cosmetic;
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    app.rendering_animation_frame = false;
    terminal.backend().buffer().clone()
}

#[test]
fn cached_chrome_preserves_cells_and_targets_across_semantic_changes() {
    let mut app = fixture(300);
    let mut terminal = Terminal::new(TestBackend::new(150, 35)).unwrap();
    for step in 0..8 {
        match step {
            1 => {
                app.active_pane = ActivePane::Explorer;
                app.hovered_explorer = Some(5);
            }
            2 => {
                app.explorer.scroll_offset = 140;
                app.explorer.selected = 145;
            }
            3 => {
                app.colors = crate::app_theme::EditorTheme::Light.colors();
                app.colors.gradients.bg_sidebar = crate::app_theme::ThemeGradientSpec {
                    start: ratatui::style::Color::Rgb(250, 240, 230),
                    end: ratatui::style::Color::Rgb(220, 235, 250),
                    angle_deg: 32.0,
                };
            }
            4 => {
                app.sidebar_right = true;
                app.mouse_row = 0;
                app.mouse_col = 3;
            }
            5 => {
                app.explorer_width = 50;
                app.tabs[0].editor.modified = true;
            }
            6 => {
                app.sidebar_view = SidebarView::Outline;
            }
            7 => {
                app.show_explorer = false;
                app.show_menu_bar = false;
            }
            _ => {}
        }
        let expected = render(&mut app, &mut terminal, false);
        assert_eq!(app.animation_frame_cache.chrome.valid, 0);
        let code = app.code_area;
        let sidebar = app.explorer_area;
        let visible_height = app.explorer.visible_height;
        let replays = app.animation_frame_cache.chrome.replays;
        assert_eq!(
            render(&mut app, &mut terminal, true),
            expected,
            "step={step}"
        );
        assert_eq!(
            render(&mut app, &mut terminal, true),
            expected,
            "warm step={step}"
        );
        assert!(app.animation_frame_cache.chrome.replays > replays);
        assert_eq!(app.code_area, code);
        assert_eq!(app.explorer_area, sidebar);
        assert_eq!(app.explorer.visible_height, visible_height);
        REFERENCE.set(true);
        assert_eq!(
            render(&mut app, &mut terminal, true),
            expected,
            "uncached step={step}"
        );
        REFERENCE.set(false);
    }
}

#[test]
fn search_and_rename_inputs_keep_their_live_mouse_maps_on_cosmetic_frames() {
    use crate::text_input::TextInputId;
    let mut app = fixture(12);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    render(&mut app, &mut terminal, false);
    app.open_explorer_find();
    for id in [TextInputId::ExplorerFind, TextInputId::ExplorerRename] {
        if id == TextInputId::ExplorerRename {
            app.explorer.find.open = false;
            app.rename.active = true;
            app.rename.query = "new-name.rs".into();
            app.rename.target = Some(app.explorer.entries[0].path.clone());
        }
        render(&mut app, &mut terminal, false);
        let expected = app.text_input_regions.clone();
        assert!(!expected.is_empty(), "visible input {id:?}");
        for cursor in [false, true] {
            app.cursor_visible = cursor;
            let cached = render(&mut app, &mut terminal, true);
            assert_eq!(
                app.text_input_regions, expected,
                "input hit map survives repaint"
            );
            REFERENCE.set(true);
            assert_eq!(render(&mut app, &mut terminal, true), cached);
            REFERENCE.set(false);
        }
    }
}

#[test]
fn resized_chrome_is_repainted_and_disabled_animations_release_storage() {
    let mut app = fixture(80);
    let mut terminal = Terminal::new(TestBackend::new(150, 35)).unwrap();
    render(&mut app, &mut terminal, false);
    let mut resized = Terminal::new(TestBackend::new(70, 18)).unwrap();
    let actual = render(&mut app, &mut resized, true);
    let expected = render(&mut app, &mut resized, false);
    assert_eq!(actual, expected);
    app.animation_settings.set(AnimationEffect::TabSlide, false);
    render(&mut app, &mut resized, false);
    assert!(
        app.animation_frame_cache
            .chrome
            .scenes
            .iter()
            .all(Option::is_none)
    );
}

#[test]
fn popup_occlusion_does_not_get_baked_into_chrome() {
    let mut app = fixture(80);
    let mut terminal = Terminal::new(TestBackend::new(150, 35)).unwrap();
    app.explorer_context_active = true;
    app.explorer_context_pos = (8, 6);
    let expected = render(&mut app, &mut terminal, false);
    assert_eq!(render(&mut app, &mut terminal, true), expected);
    app.explorer_context_active = false;
    let dismissed = render(&mut app, &mut terminal, false);
    assert_ne!(dismissed, expected);
    assert_eq!(render(&mut app, &mut terminal, true), dismissed);
}

#[test]
fn chrome_cache_is_lazy_and_does_not_retain_oversized_regions() {
    let mut app = fixture(100);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    for _ in 0..3 {
        render(&mut app, &mut terminal, false);
    }
    assert!(
        app.animation_frame_cache
            .chrome
            .scenes
            .iter()
            .all(Option::is_none)
    );
    render(&mut app, &mut terminal, true);
    assert!(
        app.animation_frame_cache
            .chrome
            .scenes
            .iter()
            .any(Option::is_some)
    );
    let mut large = Terminal::new(TestBackend::new(400, 100)).unwrap();
    app.rendering_animation_frame = true;
    large
        .draw(|frame| draw(frame, &mut app, frame.area(), Region::Explorer))
        .unwrap();
    assert!(app.animation_frame_cache.chrome.scenes[Region::Explorer.slot()].is_none());
}

#[test]
#[ignore = "whole-workbench cosmetic redraw comparison"]
fn benchmark_animation_chrome_reuse() {
    for count in [80, 20_000] {
        let mut app = fixture(count);
        app.explorer.scroll_offset = count.saturating_sub(70);
        let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
        render(&mut app, &mut terminal, false);
        let expected = render(&mut app, &mut terminal, true);
        let mut times = [Vec::new(), Vec::new()];
        for batch in 0..9 {
            for reference in [batch % 2 == 0, batch % 2 != 0] {
                REFERENCE.set(reference);
                render(&mut app, &mut terminal, false);
                assert_eq!(render(&mut app, &mut terminal, true), expected);
                let start = Instant::now();
                app.rendering_animation_frame = true;
                for _ in 0..100 {
                    terminal
                        .draw(|frame| crate::ui::draw(frame, &mut app))
                        .unwrap();
                    std::hint::black_box(terminal.backend().buffer());
                }
                app.rendering_animation_frame = false;
                times[usize::from(!reference)].push(start.elapsed() / 100);
            }
        }
        REFERENCE.set(false);
        for samples in &mut times {
            samples.sort_unstable();
        }
        eprintln!(
            "Cosmetic workbench ({count} Explorer entries): redraw={:?}, reuse={:?}",
            times[0][4], times[1][4]
        );
    }
}
