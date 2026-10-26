use super::*;
use crate::{
    animation_config::AnimationEffect,
    app::{EditorGroup, SplitDirection, SplitNode, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn tree() -> SplitNode {
    SplitNode::Split {
        direction: SplitDirection::Horizontal,
        children: vec![
            SplitNode::Leaf(0),
            SplitNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![SplitNode::Leaf(1), SplitNode::Leaf(2), SplitNode::Leaf(3)],
                ratios: vec![0.2, 0.5, 0.3],
            },
        ],
        ratios: vec![0.4, 0.6],
    }
}

// Preserve the original recursive layout calculations as a reference, including
// the path allocations that used to happen on every draw.
fn reference_layout(
    node: &SplitNode,
    area: Rect,
    path: &[usize],
    panes: &mut Vec<(usize, Rect)>,
    dividers: &mut Vec<SplitDivider>,
) {
    match node {
        SplitNode::Leaf(group) => panes.push((*group, area)),
        SplitNode::Split {
            direction,
            children,
            ratios,
        } => {
            if children.is_empty() {
                return;
            }
            let direction_ui = match direction {
                SplitDirection::Horizontal => Direction::Horizontal,
                SplitDirection::Vertical => Direction::Vertical,
            };
            let rects = Layout::default()
                .direction(direction_ui)
                .constraints(split_constraints(children.len(), ratios))
                .split(area);
            for (index, child) in children.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(index);
                reference_layout(child, rects[index * 2], &child_path, panes, dividers);
                if index > 0 {
                    dividers.push((
                        rects[index * 2 - 1],
                        path.to_vec(),
                        index - 1,
                        *direction,
                        area,
                    ));
                }
            }
        }
    }
}

#[test]
fn cached_split_layout_matches_reference_after_resize_reorder_and_ratio_changes() {
    let mut tree = tree();
    let mut cache = SplitPaneLayout::default();
    let mut dividers = Vec::new();
    for change in 0..4 {
        match change {
            1 => tree.adjust_ratio(&[1], 0, 0.13),
            2 => {
                tree.split_leaf(0, 4, SplitDirection::Vertical, true);
            }
            3 => {
                tree.remove_leaf(2);
                tree.simplify();
            }
            _ => {}
        }
        for width in [0, 1, 2, 3, 12, 80, 180] {
            for height in [0, 1, 2, 3, 8, 40] {
                let area = Rect::new(2, 3, width, height);
                let mut expected = Vec::new();
                let mut expected_dividers = Vec::new();
                reference_layout(&tree, area, &[], &mut expected, &mut expected_dividers);
                cache.update(&tree, area, &mut dividers);
                assert_eq!(cache.panes, expected);
                assert_eq!(dividers, expected_dividers);
                let builds = cache.builds;
                let paths: Vec<_> = dividers.iter().map(|(_, path, ..)| path.as_ptr()).collect();
                for _ in 0..3 {
                    cache.update(&tree, area, &mut dividers);
                    assert_eq!(cache.builds, builds);
                    assert_eq!(
                        dividers
                            .iter()
                            .map(|(_, path, ..)| path.as_ptr())
                            .collect::<Vec<_>>(),
                        paths
                    );
                }
            }
        }
    }
}

#[test]
fn split_layout_handles_invalid_ratios_empty_branches_and_leaf_visits() {
    let tree = SplitNode::Split {
        direction: SplitDirection::Horizontal,
        children: vec![
            tree(),
            SplitNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![],
                ratios: vec![],
            },
            SplitNode::Leaf(4),
        ],
        ratios: vec![f32::NAN, -1.0],
    };
    let mut leaves = Vec::new();
    tree.visit_leaves(&mut |leaf| leaves.push(leaf));
    assert_eq!(leaves, vec![0, 1, 2, 3, 4]);
    assert_eq!(tree.leaves(), leaves);
    let mut cache = SplitPaneLayout::default();
    let mut dividers = Vec::new();
    let area = Rect::new(0, 0, 9, 5);
    cache.update(&tree, area, &mut dividers);
    for (_, rect) in &cache.panes {
        assert_eq!(*rect, rect.intersection(area));
    }
    let mut expected = Vec::new();
    let mut expected_dividers = Vec::new();
    reference_layout(&tree, area, &[], &mut expected, &mut expected_dividers);
    assert_eq!(cache.panes, expected);
    assert_eq!(dividers, expected_dividers);
}

fn fixture() -> App {
    let mut app = App::test_new();
    app.terminal_has_focus = false;
    app.show_minimap = false;
    app.show_tab_bar = true;
    app.show_breadcrumbs = true;
    app.animation_settings.set(AnimationEffect::TabSlide, true);
    app.groups = (0..4)
        .map(|group| {
            let mut pane = EditorGroup::new();
            pane.tabs = (0..6)
                .map(|tab| {
                    let mut editor = Editor::test_with_text("fn main() {\n    hello();\n}\n");
                    editor.file_path = Some(format!("group-{group}/folder/file-{tab}.rs").into());
                    Tab::new_file(editor)
                })
                .collect();
            pane
        })
        .collect();
    app.active_group = 0;
    app.tabs = std::mem::take(&mut app.groups[0].tabs);
    app.split_tree = Some(tree());
    app
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>, animation: bool) -> Buffer {
    app.rendering_animation_frame = animation;
    app.animation_frame_cache
        .begin_frame(animation, app.animation_settings.any_enabled());
    let area = terminal.size().unwrap();
    terminal
        .draw(|frame| draw_editor_section(frame, app, Rect::new(0, 0, area.width, area.height)))
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn inactive_panes_reuse_exact_chrome_on_animation_frames_and_refresh_after_input() {
    let mut app = fixture();
    let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
    render(&mut app, &mut terminal, false);
    let expected = render(&mut app, &mut terminal, false);
    for _ in 0..10 {
        assert_eq!(render(&mut app, &mut terminal, true), expected);
    }
    assert_eq!(app.split_layout_cache.builds, 1);
    for group in &app.groups[1..] {
        let cache = group.inactive_chrome_cache.as_ref().unwrap();
        assert_eq!(cache.captures, 2);
        assert_eq!(cache.replays.get(), 10);
    }
    for change in 0..7 {
        match change {
            0 => app.groups[1].tabs[0].editor.modified = true,
            1 => app.groups[1].tabs[0].editor.file_path = Some("renamed/界.rs".into()),
            2 => {
                app.groups[1].active_tab = 3;
                app.groups[1].tab_scroll_offset = 3;
            }
            3 => app.switch_group(2),
            4 => app.split_tree.as_mut().unwrap().adjust_ratio(&[], 0, 0.15),
            5 => terminal = Terminal::new(TestBackend::new(150, 32)).unwrap(),
            _ => app.mouse_col = app.split_dividers[0].0.x,
        }
        let expected = render(&mut app, &mut terminal, false);
        assert_eq!(
            render(&mut app, &mut terminal, true),
            expected,
            "change {change}"
        );
    }
}

#[test]
fn hidden_chrome_and_disabled_animations_release_pane_chrome_and_hit_areas() {
    let mut app = fixture();
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    render(&mut app, &mut terminal, false);
    app.zen_mode = true;
    render(&mut app, &mut terminal, false);
    assert!(app.tab_bar_area.is_none());
    for group in &app.groups[1..] {
        assert!(group.tab_bar_area.is_none());
        assert!(group.inactive_chrome_cache.is_none());
    }
    app.zen_mode = false;
    app.show_tab_bar = false;
    let expected = render(&mut app, &mut terminal, false);
    assert_eq!(render(&mut app, &mut terminal, true), expected);
    for group in &app.groups[1..] {
        assert!(group.tab_bar_area.is_none());
    }
    app.animation_settings = Default::default();
    render(&mut app, &mut terminal, false);
    for group in &app.groups[1..] {
        assert!(group.inactive_chrome_cache.is_none());
    }
    app.close_all_splits();
    render(&mut app, &mut terminal, false);
    assert!(app.split_dividers.is_empty());
    assert!(app.split_layout_cache.tree.is_none());
}

#[test]
fn direct_dividers_match_widget_cells_without_painting_empty_rectangles() {
    let style = Style::default().fg(Color::Cyan).bg(Color::Blue);
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let area = if direction == Direction::Horizontal {
            Rect::new(4, 3, 1, 12)
        } else {
            Rect::new(4, 3, 30, 1)
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal
            .draw(|frame| draw_divider(frame, area, &direction, style))
            .unwrap();
        let actual = terminal.backend().buffer().clone();
        terminal
            .draw(|frame| {
                if direction == Direction::Horizontal {
                    for y in area.y..area.bottom() {
                        frame.render_widget(
                            Paragraph::new("│").style(style),
                            Rect::new(area.x, y, 1, 1),
                        );
                    }
                } else {
                    frame.render_widget(
                        Paragraph::new("─".repeat(area.width as usize)).style(style),
                        area,
                    );
                }
            })
            .unwrap();
        assert_eq!(*terminal.backend().buffer(), actual);
        terminal
            .draw(|frame| draw_divider(frame, Rect { width: 0, ..area }, &direction, style))
            .unwrap();
        assert_eq!(
            *terminal.backend().buffer(),
            Buffer::empty(Rect::new(0, 0, 40, 20))
        );
    }
}

#[test]
fn duplicate_panes_detect_equal_length_replacements_and_keep_text_caches_on_save() {
    let mut app = App::test_new();
    let mut first = Editor::test_with_text("aa\n  bb\n");
    first.file_path = Some("shared.rs".into());
    let mut second = Editor::test_with_text("xx\n  yy\n");
    second.file_path = first.file_path.clone();
    app.tabs = vec![Tab::new_file(first), Tab::new_file(second)];
    app.sync_duplicate_file_tabs();
    assert!(
        app.tabs[0]
            .editor
            .rope
            .is_instance(&app.tabs[1].editor.rope)
    );
    let revision = app.tabs[1].editor.wrap_revision;
    app.tabs[0].editor.modified = true;
    app.sync_duplicate_file_tabs();
    assert!(app.tabs[1].editor.modified);
    assert_eq!(app.tabs[1].editor.wrap_revision, revision);
    app.tabs[0].editor.modified = false;
    app.tabs[0].editor.saved_rope = Some(app.tabs[0].editor.rope.clone());
    app.sync_duplicate_file_tabs();
    assert!(!app.tabs[1].editor.modified);
    assert_eq!(app.tabs[1].editor.wrap_revision, revision);
    for _ in 0..10 {
        app.sync_duplicate_file_tabs();
    }
    assert_eq!(app.tabs[1].editor.wrap_revision, revision);
    app.tabs[0].editor.saved_rope = Some(ropey::Rope::from_str("new baseline"));
    app.sync_duplicate_file_tabs();
    assert!(
        app.tabs[0]
            .editor
            .saved_rope
            .as_ref()
            .unwrap()
            .is_instance(app.tabs[1].editor.saved_rope.as_ref().unwrap())
    );
    assert_eq!(app.tabs[1].editor.wrap_revision, revision);
}

#[test]
#[ignore = "comparative release split layout benchmark"]
fn benchmark_repeated_split_layout() {
    let tree = tree();
    let area = Rect::new(0, 0, 180, 50);
    let mut cache = SplitPaneLayout::default();
    let mut dividers = Vec::new();
    cache.update(&tree, area, &mut dividers);
    let mut samples = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let start = std::time::Instant::now();
            for _ in 0..10_000 {
                if optimized {
                    cache.update(std::hint::black_box(&tree), area, &mut dividers);
                    std::hint::black_box(&cache.panes);
                } else {
                    let mut panes = Vec::new();
                    dividers.clear();
                    reference_layout(
                        std::hint::black_box(&tree),
                        area,
                        &[],
                        &mut panes,
                        &mut dividers,
                    );
                    std::hint::black_box(panes);
                }
            }
            samples[usize::from(optimized)].push(start.elapsed() / 10_000);
        }
    }
    samples
        .iter_mut()
        .for_each(|samples| samples.sort_unstable());
    eprintln!(
        "four-pane stable layout: recursive {:?}, cached {:?}",
        samples[0][3], samples[1][3]
    );
}

#[test]
#[ignore = "comparative release inactive pane animation chrome benchmark"]
fn benchmark_inactive_pane_animation_chrome() {
    let mut app = fixture();
    let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
    render(&mut app, &mut terminal, false);
    let mut samples = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for cached in [sample % 2 == 0, sample % 2 != 0] {
            crate::ui_helpers::BYPASS_PANE_CHROME_CACHE.set(!cached);
            // Warm both paths; do not count setup or snapshot copies.
            render(&mut app, &mut terminal, true);
            let start = std::time::Instant::now();
            for _ in 0..100 {
                app.rendering_animation_frame = true;
                app.animation_frame_cache.begin_frame(true, true);
                terminal
                    .draw(|frame| draw_editor_section(frame, &mut app, Rect::new(0, 0, 180, 40)))
                    .unwrap();
                std::hint::black_box(terminal.backend().buffer());
            }
            crate::ui_helpers::BYPASS_PANE_CHROME_CACHE.set(false);
            samples[usize::from(cached)].push(start.elapsed() / 100);
        }
    }
    samples
        .iter_mut()
        .for_each(|samples| samples.sort_unstable());
    eprintln!(
        "four-pane animation frame: redraw chrome {:?}, reuse chrome {:?}",
        samples[0][3], samples[1][3]
    );
}
