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
