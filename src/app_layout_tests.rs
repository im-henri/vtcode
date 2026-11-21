#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::app::{App, EditorGroup, SplitDirection, SplitNode, Tab};
    use crate::editor::Editor;

    fn make_tab() -> Tab {
        Tab::new_file(Editor::new())
    }

    fn make_group_with_tabs(n: usize) -> EditorGroup {
        let mut g = EditorGroup::new();
        for _ in 0..n {
            g.tabs.push(make_tab());
        }
        g
    }

    // --- split_active tests ---

    #[test]
    fn split_active_false_when_no_groups() {
        let app = App::test_new();
        assert!(!app.split_active());
    }

    #[test]
    fn split_active_true_when_groups_exist() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        assert!(app.split_active());
    }

    // --- build_group_index_map tests ---

    #[test]
    fn build_group_index_map_all_nonempty() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(make_group_with_tabs(2));
        app.groups.push(make_group_with_tabs(1));
        let map = app.build_group_index_map();
        assert_eq!(map.len(), 3);
        assert_eq!(map[&0], 0);
        assert_eq!(map[&1], 1);
        assert_eq!(map[&2], 2);
    }

    #[test]
    fn build_group_index_map_skips_empty() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(EditorGroup::new()); // empty
        app.groups.push(make_group_with_tabs(1));
        let map = app.build_group_index_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&0], 0);
        assert!(!map.contains_key(&1));
        assert_eq!(map[&2], 1);
    }

    #[test]
    fn build_group_index_map_all_empty() {
        let mut app = App::test_new();
        app.groups.push(EditorGroup::new());
        app.groups.push(EditorGroup::new());
        let map = app.build_group_index_map();
        assert!(map.is_empty());
    }

    // --- switch_group tests ---

    #[test]
    fn switch_group_changes_active() {
        let mut app = App::test_new();
        app.tabs.push(make_tab());
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(make_group_with_tabs(2));
        app.active_group = 0;

        app.switch_group(1);
        assert_eq!(app.active_group, 1);
    }

    #[test]
    fn switch_group_noop_same_group() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(make_group_with_tabs(1));
        app.active_group = 0;
        app.tabs = vec![make_tab()];
        app.switch_group(0); // same group
        assert_eq!(app.active_group, 0);
    }

    #[test]
    fn switch_group_noop_out_of_bounds() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.active_group = 0;
        app.tabs = vec![make_tab()];
        app.switch_group(5); // out of bounds
        assert_eq!(app.active_group, 0);
    }

    // --- create_split_group_dir tests ---

    #[test]
    fn create_split_group_first_split() {
        let mut app = App::test_new();
        app.tabs.push(make_tab());
        app.tabs.push(make_tab());
        app.active_tab = 0;

        app.create_split_group_dir(1, SplitDirection::Horizontal);

        // Should have created split mode
        assert!(app.split_active() || app.split_tree.is_some());
    }

    #[test]
    fn create_split_group_out_of_bounds_noop() {
        let mut app = App::test_new();
        app.tabs.push(make_tab());
        let groups_before = app.groups.len();
        app.create_split_group_dir(99, SplitDirection::Horizontal);
        assert_eq!(app.groups.len(), groups_before);
    }

    #[test]
    fn split_editor_commands_open_a_second_view_of_the_only_tab() {
        for (command, direction, leaves) in [
            ("split_editor", SplitDirection::Horizontal, [0, 1]),
            ("split_editor_down", SplitDirection::Vertical, [0, 1]),
            ("split_editor_left", SplitDirection::Horizontal, [1, 0]),
            ("split_editor_up", SplitDirection::Vertical, [1, 0]),
        ] {
            let mut app = App::test_new();
            app.tabs.push(make_tab());
            let original = app.tabs[0].editor.navigation_id;
            app.tabs[0].preview = true;
            app.terminal_panel_focused = true;
            assert!(!app.is_menu_cmd_disabled(command));

            app.execute_palette_command(command);

            let tree = app.split_tree.as_ref().unwrap();
            assert!(
                matches!(tree, SplitNode::Split { direction: actual, .. } if *actual == direction)
            );
            assert_eq!(tree.leaves(), leaves);
            assert_eq!(app.active_group, 1);
            assert!(!app.terminal_panel_focused);
            let source = &app.groups[0].tabs[0];
            let split = &app.tabs[0];
            assert_eq!(source.editor.navigation_id, original);
            assert_ne!(split.editor.navigation_id, original);
            assert!(source.editor.shares_document(&split.editor));
            assert!(!source.preview && !split.preview);
        }
    }

    #[test]
    fn split_editor_views_share_edits_undo_and_save_with_independent_viewports() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("shared.txt");
        let lines = "first\nsecond\n";
        let original = lines.repeat(2048);
        std::fs::write(&path, &original).unwrap();
        let mut editor = Editor::new();
        editor.open_file(&path).unwrap();
        editor.cursor_line = 2;
        editor.cursor_col = 2;
        editor.scroll_offset = 1;
        let mut app = App::new(root.path().to_owned());
        app.tabs.push(Tab::new_file(editor));

        for key in ['k', '\\'] {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL),
            );
        }
        let source = &app.groups[0].tabs[0].editor;
        let split = app.editor().unwrap();
        assert!(source.rope.is_instance(&split.rope));
        assert_eq!(
            (split.cursor_line, split.cursor_col, split.scroll_offset),
            (2, 2, 1)
        );

        let split = app.editor_mut().unwrap();
        split.set_cursor(0, 0);
        split.paste_plain_with_text("one ");
        app.sync_duplicate_file_tabs();
        let source = &app.groups[0].tabs[0].editor;
        assert!(source.rope.is_instance(&app.editor().unwrap().rope));
        assert_eq!(
            (source.cursor_line, source.cursor_col, source.scroll_offset),
            (2, 2, 1)
        );
        assert!(source.modified);

        app.switch_group(0);
        let source = app.editor_mut().unwrap();
        source.set_cursor(1, 0);
        source.paste_plain_with_text("two ");
        app.switch_group(1);
        assert_eq!(app.editor().unwrap().rope.line(1), "two second\n");
        app.editor_mut().unwrap().undo();
        app.switch_group(0);
        assert_eq!(app.editor().unwrap().rope.line(1), "second\n");
        app.editor_mut().unwrap().redo();
        app.save_current_file();

        app.wait_for_background(std::time::Duration::from_secs(10), |app| {
            app.pending_file_saves.is_empty() && !app.editor().unwrap().modified
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("one first\ntwo second\n{}", &original[lines.len()..])
        );
        assert!(!app.groups[1].tabs[0].editor.modified);

        let source = app.editor_mut().unwrap();
        source.set_cursor(1, 4);
        source.paste_plain_with_text("unsaved ");
        app.close_tab(0);
        assert!(!app.confirm_close_active);
        assert!(!app.split_active());
        assert!(app.editor().unwrap().modified);
        assert_eq!(app.editor().unwrap().rope.line(1), "two unsaved second\n");
        app.close_tab(0);
        assert!(app.confirm_close_active);
    }

    #[test]
    fn split_editor_session_recovers_one_document_with_two_view_positions() {
        for on_disk in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut app = App::new(root.path().to_owned());
            let mut editor = Editor::test_with_text("first\nsecond\n");
            if on_disk {
                let path = root.path().join("shared.txt");
                std::fs::write(&path, "first\nsecond\n").unwrap();
                editor.open_file(&path).unwrap();
            }
            app.tabs.push(Tab::new_file(editor));
            app.execute_palette_command("split_editor_down");
            let split = app.editor_mut().unwrap();
            split.set_cursor(1, 0);
            split.paste_plain_with_text("edited ");
            app.sync_duplicate_file_tabs();
            crate::session::save_session(root.path(), &app.build_session()).unwrap();
            let saved = crate::session::load_session(root.path()).unwrap();
            let source = &saved.groups[0].tabs[0];
            assert!(source.unsaved_file.is_some());
            assert_eq!(source.unsaved_file, saved.tabs[0].unsaved_file);
            assert!(source.undo_history_file.is_some());
            assert_eq!(source.undo_history_file, saved.tabs[0].undo_history_file);

            let mut restored = App::new(root.path().to_owned());
            restored.restore_session(&saved);
            restored.wait_for_background(std::time::Duration::from_secs(10), |app| {
                !app.editor().unwrap().bg_loading && !app.groups[0].tabs[0].editor.bg_loading
            });
            let source = &restored.groups[0].tabs[0].editor;
            let split = restored.editor().unwrap();
            assert!(source.shares_document(split));
            assert!(source.rope.is_instance(&split.rope));
            assert_eq!(split.rope.line(1), "edited second\n");
            assert_eq!((source.cursor_line, source.cursor_col), (0, 0));
            assert_eq!((split.cursor_line, split.cursor_col), (1, 7));
            restored.switch_group(0);
            restored.editor_mut().unwrap().undo();
            restored.switch_group(1);
            assert_eq!(restored.editor().unwrap().rope.line(1), "second\n");
        }
    }

    // --- close_empty_groups tests ---

    #[test]
    fn close_empty_groups_removes_empty() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(EditorGroup::new());
        app.groups.push(make_group_with_tabs(1));
        app.active_group = 0;
        app.tabs = vec![make_tab()];
        app.split_tree = Some(SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1), SplitNode::Leaf(2)],
            ratios: vec![0.33, 0.34, 0.33],
        });

        app.close_empty_groups();

        assert_eq!(app.groups.len(), 2);
    }

    #[test]
    fn close_empty_groups_all_empty_exits_split() {
        let mut app = App::test_new();
        app.groups.push(EditorGroup::new());
        app.active_group = 0;
        app.tabs = Vec::new();

        app.close_empty_groups();

        assert!(app.groups.is_empty());
        assert!(app.split_tree.is_none());
    }

    #[test]
    fn close_empty_groups_one_remaining_exits_split() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(2));
        app.groups.push(EditorGroup::new());
        app.active_group = 0;
        app.tabs = vec![make_tab(), make_tab()];

        app.close_empty_groups();

        // Should have exited split mode
        assert!(app.groups.is_empty());
        assert!(app.split_tree.is_none());
        assert_eq!(app.tabs.len(), 2);
    }

    // --- close_all_splits tests ---

    #[test]
    fn close_all_splits_merges_tabs() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(2));
        app.groups.push(make_group_with_tabs(3));
        app.active_group = 0;
        app.tabs = vec![make_tab(), make_tab()];
        app.split_tree = Some(SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)],
            ratios: vec![0.5, 0.5],
        });

        app.close_all_splits();

        assert!(app.groups.is_empty());
        assert!(app.split_tree.is_none());
        assert_eq!(app.tabs.len(), 5); // 2 + 3
    }

    #[test]
    fn close_all_splits_noop_when_no_groups() {
        let mut app = App::test_new();
        app.tabs.push(make_tab());
        app.close_all_splits();
        assert!(app.groups.is_empty());
        assert!(app.split_tree.is_none());
    }

    #[test]
    fn close_all_splits_clamps_active_tab() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(make_group_with_tabs(1));
        app.active_group = 0;
        app.active_tab = 99; // out of range
        app.tabs = vec![make_tab()];

        app.close_all_splits();

        assert!(app.active_tab < app.tabs.len());
    }

    // --- SplitNode tests ---

    #[test]
    fn split_node_leaves_single() {
        let node = SplitNode::Leaf(0);
        assert_eq!(node.leaves(), vec![0]);
    }

    #[test]
    fn split_node_leaves_nested() {
        let node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![
                SplitNode::Leaf(0),
                SplitNode::Split {
                    direction: SplitDirection::Vertical,
                    children: vec![SplitNode::Leaf(1), SplitNode::Leaf(2)],
                    ratios: vec![0.5, 0.5],
                },
            ],
            ratios: vec![0.5, 0.5],
        };
        assert_eq!(node.leaves(), vec![0, 1, 2]);
    }

    #[test]
    fn split_node_split_leaf_replaces() {
        let mut node = SplitNode::Leaf(0);
        let result = node.split_leaf(0, 1, SplitDirection::Horizontal, false);
        assert!(result);
        assert_eq!(node.leaves(), vec![0, 1]);
    }

    #[test]
    fn split_node_split_leaf_not_found() {
        let mut node = SplitNode::Leaf(0);
        let result = node.split_leaf(5, 1, SplitDirection::Horizontal, false);
        assert!(!result);
    }

    #[test]
    fn split_node_remove_leaf() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1), SplitNode::Leaf(2)],
            ratios: vec![0.33, 0.34, 0.33],
        };
        let result = node.remove_leaf(1);
        assert!(result);
        assert_eq!(node.leaves(), vec![0, 2]);
    }

    #[test]
    fn split_node_remove_leaf_not_found() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)],
            ratios: vec![0.5, 0.5],
        };
        let result = node.remove_leaf(99);
        assert!(!result);
    }

    #[test]
    fn split_node_remap_indices() {
        let mut map = rustc_hash::FxHashMap::default();
        map.insert(0_usize, 10_usize);
        map.insert(1, 11);

        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)],
            ratios: vec![0.5, 0.5],
        };
        node.remap_indices(&map);
        assert_eq!(node.leaves(), vec![10, 11]);
    }

    #[test]
    fn split_node_simplify_single_child() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0)],
            ratios: vec![1.0],
        };
        node.simplify();
        assert!(matches!(node, SplitNode::Leaf(0)));
    }

    #[test]
    fn split_node_simplify_flattens_same_direction() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![
                SplitNode::Leaf(0),
                SplitNode::Split {
                    direction: SplitDirection::Horizontal,
                    children: vec![SplitNode::Leaf(1), SplitNode::Leaf(2)],
                    ratios: vec![0.5, 0.5],
                },
            ],
            ratios: vec![0.5, 0.5],
        };
        node.simplify();
        // Should flatten to 3 leaves at top level
        assert_eq!(node.leaves(), vec![0, 1, 2]);
        if let SplitNode::Split { children, .. } = &node {
            assert_eq!(children.len(), 3);
        } else {
            panic!("expected Split");
        }
    }

    #[test]
    fn split_node_simplify_preserves_different_direction() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![
                SplitNode::Leaf(0),
                SplitNode::Split {
                    direction: SplitDirection::Vertical, // different!
                    children: vec![SplitNode::Leaf(1), SplitNode::Leaf(2)],
                    ratios: vec![0.5, 0.5],
                },
            ],
            ratios: vec![0.5, 0.5],
        };
        node.simplify();
        // Should NOT flatten since inner is different direction
        if let SplitNode::Split { children, .. } = &node {
            assert_eq!(children.len(), 2);
        } else {
            panic!("expected Split with 2 children");
        }
    }

    // --- compact_groups tests ---

    #[test]
    fn compact_groups_removes_empty_groups() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(EditorGroup::new()); // empty
        app.groups.push(make_group_with_tabs(1));
        app.active_group = 2;
        app.split_tree = Some(SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1), SplitNode::Leaf(2)],
            ratios: vec![0.33, 0.34, 0.33],
        });

        app.compact_groups();

        assert_eq!(app.groups.len(), 2);
        // active_group should have been remapped from 2 to 1
        assert_eq!(app.active_group, 1);
    }

    #[test]
    fn compact_groups_noop_when_all_nonempty() {
        let mut app = App::test_new();
        app.groups.push(make_group_with_tabs(1));
        app.groups.push(make_group_with_tabs(1));
        app.active_group = 0;

        app.compact_groups();

        assert_eq!(app.groups.len(), 2);
    }
}
