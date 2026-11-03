use super::*;
use crate::{
    app::{Tab, TabKind},
    editor::Editor,
    remote::{self, RemotePath, workspace::Workspace},
};
use ratatui::{Terminal, backend::TestBackend};
use std::{path::PathBuf, sync::Arc};

fn workspace() -> Workspace {
    let hello = remote::Hello {
        version: remote::VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        root: "/repo".into(),
        root_uri: "file:///repo/".into(),
        session: 42,
    };
    Workspace::new(
        remote::client::Client::attach(std::io::Cursor::new(Vec::new()), Vec::new(), hello)
            .unwrap(),
    )
}

fn remote_tab(workspace: &Workspace, bytes: &[u8]) -> Tab {
    let mut editor = Editor::new();
    editor.file_path = Some(workspace.view_key(&RemotePath::new(bytes.to_vec()).unwrap()));
    Tab::new_file(editor)
}

#[test]
fn remote_tab_names_decode_all_reserved_punctuation_once() {
    let workspace = workspace();
    let mut app = App::test_new();
    for (bytes, expected) in [
        (b"plain.rs".as_slice(), "plain.rs"),
        (b"part:one.rs".as_slice(), "part:one.rs"),
        (
            br#"back\colon:percent%less<greater>quote"pipe|question?star*tilde~.md"#.as_slice(),
            r#"back\colon:percent%less<greater>quote"pipe|question?star*tilde~.md"#,
        ),
        (
            b"literal%3a%3A%20%2f%ff%25.rs".as_slice(),
            "literal%3a%3A%20%2f%ff%25.rs",
        ),
        ("space #&+=[]' λ界é.rs".as_bytes(), "space #&+=[]' λ界é.rs"),
        (
            b"line\n\t\r\x1b\x7f.rs".as_slice(),
            r"line\n\t\r\u{1b}\u{7f}.rs",
        ),
        (b"raw-\xff.rs".as_slice(), "raw-�.rs"),
    ] {
        app.tabs = vec![remote_tab(&workspace, bytes)];
        let key = app.tabs[0].editor.file_path.clone().unwrap();
        assert_eq!(app.tabs[0].editor.display_name(), expected);
        for (kind, title) in [
            (TabKind::File, expected.to_string()),
            (TabKind::MarkdownPreview, format!("Preview: {expected}")),
            (TabKind::Diff(7), format!("{expected} (Diff)")),
        ] {
            app.tabs[0].kind = kind;
            assert_eq!(tab_display_name(&app, 0, &app.tabs[0]), title);
        }
        assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&key));
        assert_eq!(
            workspace.remote_path(&key).unwrap(),
            RemotePath::new(bytes.to_vec()).unwrap()
        );
    }
}

#[test]
fn native_percent_sequences_remain_literal_in_tab_names() {
    let mut app = App::test_new();
    for name in [
        "literal%3a.rs",
        "literal%20.rs",
        "literal%25.rs",
        "literal%2f%ff.rs",
    ] {
        let mut editor = Editor::new();
        editor.file_path = Some(PathBuf::from("native").join(name));
        app.tabs = vec![Tab::new_file(editor)];
        for (kind, title) in [
            (TabKind::File, name.to_string()),
            (TabKind::MarkdownPreview, format!("Preview: {name}")),
            (TabKind::Diff(7), format!("{name} (Diff)")),
        ] {
            app.tabs[0].kind = kind;
            assert_eq!(tab_display_name(&app, 0, &app.tabs[0]), title);
        }
    }
}

#[test]
fn duplicate_remote_names_decode_folder_hints_without_losing_component_boundaries() {
    let workspace = workspace();
    let mut app = App::test_new();
    let directories = [
        (b"dir:one".as_slice(), "dir:one"),
        (b"dir?two".as_slice(), "dir?two"),
        (b"100%3a".as_slice(), "100%3a"),
        (br#"quote""#.as_slice(), r#""quote\"""#),
        (br"line\n".as_slice(), r#""line\\n""#),
        (b"line\n".as_slice(), r#""line\n""#),
        (b"raw-\xff".as_slice(), r#""raw-\xFF""#),
    ];
    app.tabs = directories
        .iter()
        .map(|(bytes, _)| {
            let mut path = bytes.to_vec();
            path.extend_from_slice(b"/src/read:me.rs");
            remote_tab(&workspace, &path)
        })
        .collect();
    // Exercise the cached path used by large tab strips too.
    app.tabs.extend(
        (0..100).map(|index| remote_tab(&workspace, format!("other/file-{index}.rs").as_bytes())),
    );
    let cache = TabNameDuplicates::for_viewport(&app, 200).unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    let mut unique = std::collections::HashSet::new();
    for (index, (_, directory)) in directories.iter().enumerate() {
        let title = format!("read:me.rs (…{sep}{directory}{sep}…)");
        assert_eq!(tab_display_name(&app, index, &app.tabs[index]), title);
        assert_eq!(
            tab_display_name_with_duplicates(&app, index, &app.tabs[index], Some(&cache)),
            title,
        );
        assert!(unique.insert(title));
    }
}

#[test]
fn remote_revision_titles_decode_the_historical_filename() {
    let workspace = workspace();
    let old = workspace.view_key(&RemotePath::new(b"old:name%3a.rs".to_vec()).unwrap());
    let working = workspace.view_key(&RemotePath::new(b"new:name.rs".to_vec()).unwrap());
    let mut revision = crate::git_blame::Revision {
        workdir: workspace.view_root.clone(),
        path: old
            .strip_prefix(&workspace.view_root)
            .unwrap()
            .to_path_buf(),
        working_file: working,
        hash: "123456789abcdef".into(),
        date: String::new(),
        summary: String::new(),
        syntax_path: old,
    };
    assert_eq!(revision.tab_title(), "old:name%3a.rs @ 1234567");
    let mut app = App::test_new();
    let mut tab = remote_tab(&workspace, b"old:name%3a.rs");
    tab.kind = TabKind::GitRevision;
    tab.editor.blame.revision = Some(Arc::new(revision.clone()));
    app.tabs = vec![tab];
    assert_eq!(
        tab_display_name(&app, 0, &app.tabs[0]),
        "old:name%3a.rs @ 1234567"
    );

    revision.workdir = "native".into();
    revision.path = "literal%3a.rs".into();
    assert_eq!(revision.tab_title(), "literal%3a.rs @ 1234567");
}

#[test]
fn decoded_names_match_rendering_and_mouse_targets_in_plain_and_grouped_strips() {
    let workspace = workspace();
    for grouped in [false, true] {
        let mut app = App::test_new();
        app.terminal_has_focus = false;
        app.tabs = [b"read:me%.rs".as_slice(), b"guide:one.md", b"how?now.rs"]
            .iter()
            .map(|name| remote_tab(&workspace, name))
            .collect();
        app.tabs[1].kind = TabKind::MarkdownPreview;
        app.tabs[2].kind = TabKind::Diff(7);
        if grouped {
            app.desks
                .selected_tabs
                .extend(app.tabs.iter().map(|tab| tab.editor.navigation_id));
            app.group_selected_tabs("Feature", None);
        }
        let titles = ["read:me%.rs", "Preview: guide:one.md", "how?now.rs (Diff)"];
        let area = Rect::new(0, 0, 220, 1);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| draw_tab_bar(frame, &app, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        let hits = TabBarHits::new(&app, area);
        assert_eq!(hits.tabs.len(), titles.len());
        for &(index, rect, close) in &hits.tabs {
            assert!(text.contains(titles[index]), "{text}");
            assert_eq!(
                usize::from(rect.width),
                tab_label_width(&app, &app.tabs[index], titles[index])
            );
            assert_eq!(hits.tab_at(rect.x + 1).map(|hit| hit.0), Some(index));
            if index == app.active_tab {
                assert_eq!(buffer[(close.x + 1, close.y)].symbol(), "×");
            }
        }
    }
}

#[test]
fn comparing_remote_tabs_uses_decoded_names_in_the_compare_title() {
    let workspace = workspace();
    let mut app = App::test_new();
    app.tabs = vec![
        remote_tab(&workspace, b"base:one.rs"),
        remote_tab(&workspace, b"target?two.rs"),
    ];
    app.active_tab = 1;
    assert!(app.begin_tab_compare(0));
    assert_eq!(
        app.diff_tab_label.as_deref(),
        Some("base:one.rs ↔ target?two.rs")
    );
    assert_eq!(
        tab_display_name(&app, 1, &app.tabs[1]),
        "base:one.rs ↔ target?two.rs"
    );
}
