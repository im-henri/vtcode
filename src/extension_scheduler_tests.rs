use super::*;
use crate::{
    app::{EditorGroup, ExtensionViewTabRef, Tab},
    editor::Editor,
    extension_manifest::{
        DiscoveredExtension, ExtensionOverlay, ExtensionState, ExtensionTickHandler, ExtensionView,
    },
};

fn fixture(count: usize) -> App {
    let mut app = App::test_new();
    let discovered = (0..count)
        .map(|index| {
            let id = format!("fixture-{index}");
            DiscoveredExtension {
                manifest: serde_json::from_value(serde_json::json!({
                    "id": id, "name": id, "version": "1", "enabled": false,
                }))
                .unwrap(),
                manifest_path: format!("{id}/extension.json").into(),
                wasm_path: format!("{id}/extension.wasm").into(),
                manifest_mtime: None,
                wasm_mtime: None,
            }
        })
        .collect();
    app.extension_manager
        .sync_discovered(discovered, Vec::new());
    for record in &mut app.extension_manager.records {
        record.state = ExtensionState::Loaded;
        record.tick_handlers = vec![
            ExtensionTickHandler {
                export_name: "paint".into(),
                interval_ms: 100,
                background: false,
            },
            ExtensionTickHandler {
                export_name: "load".into(),
                interval_ms: 200,
                background: true,
            },
        ];
        record.views.insert(
            "view".into(),
            ExtensionView {
                id: "view".into(),
                title: "View".into(),
                content: String::new(),
                key_handler: None,
                mouse_handler: None,
                scroll_handler: None,
                resize_handler: None,
                close_handler: None,
                hover_handler: None,
                context_menu_handler: None,
                pointer_capture: false,
                input_pending: false,
                frame: None,
            },
        );
    }
    app
}

fn view(id: &str) -> Tab {
    let mut tab = Tab::new_file(Editor::new());
    tab.extension_view = Some(ExtensionViewTabRef {
        extension_id: id.into(),
        view_id: "view".into(),
        title: "View".into(),
    });
    tab
}

#[test]
fn borrowed_ticks_preserve_loaded_open_background_and_global_handler_rules() {
    let mut app = fixture(6);
    app.tabs = vec![view("fixture-0"), view("fixture-0")];
    let mut group = EditorGroup::new();
    group.tabs = vec![view("fixture-1")];
    app.groups = vec![group];
    app.extension_manager.records[2].views.clear();
    app.extension_manager.records[3].state = ExtensionState::Error;
    app.extension_manager.records[4].state = ExtensionState::Unloaded;
    for step in 0..3 {
        if step == 1 {
            app.tabs.clear();
        }
        if step == 2 {
            app.groups.clear();
        }
        let expected = app
            .extension_manager
            .visible_tick_handlers(&app.open_extension_view_ids());
        let actual: Vec<_> = handlers(&app)
            .map(|(id, h)| (id.to_owned(), h.export_name.clone(), h.background))
            .collect();
        assert_eq!(
            actual,
            expected
                .into_iter()
                .map(|(id, h)| (id, h.export_name, h.background))
                .collect::<Vec<_>>()
        );
    }
    let mut lookups = 0;
    let _: Vec<_> = app
        .extension_manager
        .tick_handlers(|_| {
            lookups += 1;
            false
        })
        .collect();
    assert_eq!(
        lookups, 3,
        "only loaded extensions with view-specific cosmetic ticks query open tabs"
    );
    for record in &mut app.extension_manager.records {
        record.tick_handlers.retain(|h| h.background);
    }
    assert_eq!(
        app.extension_manager
            .tick_handlers(|_| panic!("background ticks must not inspect tabs"))
            .count(),
        4
    );
}

#[test]
fn timer_deadlines_honor_focus_busy_state_exact_boundaries_and_failed_dispatch() {
    let mut app = fixture(1);
    app.tabs = vec![view("fixture-0")];
    let now = Instant::now();
    assert_eq!(next_wait_at(&app, now), Some(Duration::ZERO));
    app.extension_overlay_last_tick.insert(
        "fixture-0".into(),
        [("paint".into(), now), ("load".into(), now)]
            .into_iter()
            .collect(),
    );
    assert_eq!(next_wait_at(&app, now), Some(Duration::from_millis(100)));
    app.terminal_has_focus = false;
    assert_eq!(next_wait_at(&app, now), Some(Duration::from_millis(200)));
    app.terminal_has_focus = true;
    app.idle_quiescent = true;
    assert_eq!(next_wait_at(&app, now), Some(Duration::from_millis(200)));
    app.idle_quiescent = false;
    assert_eq!(
        next_wait_at(&app, now + Duration::from_millis(100)),
        Some(Duration::ZERO)
    );
    app.extension_manager
        .busy_extension_ids
        .insert("fixture-0".into());
    assert_eq!(next_wait_at(&app, now), None);
    app.extension_manager.busy_extension_ids.clear();
    app.extension_overlay_last_tick.clear();
    dispatch(&mut app); // No Wasm runtime: failed starts must stay due.
    assert!(app.extension_overlay_last_tick.is_empty());
}

#[test]
fn overlay_render_inventory_borrows_content_and_preserves_paint_order() {
    let mut app = fixture(3);
    for (index, record) in app.extension_manager.records.iter_mut().enumerate() {
        record.overlays.insert(
            "overlay".into(),
            ExtensionOverlay {
                id: "overlay".into(),
                title: format!("Overlay {index}"),
                content: "line\n".repeat(100_000),
                x: 2,
                y: 3,
                width: 40,
                height: 12,
                mouse_handler: None,
                key_handler: None,
            },
        );
    }
    app.extension_manager.records[1].state = ExtensionState::Unloaded;
    let borrowed: Vec<_> = app.extension_manager.overlays().collect();
    let owned = app.extension_manager.all_overlays();
    assert_eq!(borrowed.len(), 2);
    for ((id, overlay), (owned_id, owned_overlay)) in borrowed.iter().zip(&owned) {
        assert_eq!(id, owned_id);
        assert_eq!(*overlay, owned_overlay);
        let record = &app.extension_manager.records[app.extension_manager.index_by_id[*id]];
        assert_eq!(
            overlay.content.as_ptr(),
            record.overlays["overlay"].content.as_ptr()
        );
    }
    let targets = app.extension_manager.overlay_targets();
    for ((id, target), (owned_id, overlay)) in targets.iter().zip(&owned) {
        assert_eq!(id, owned_id);
        assert_eq!(target.id, overlay.id);
        assert_eq!(
            (target.x, target.y, target.width, target.height),
            (overlay.x, overlay.y, overlay.width, overlay.height)
        );
        assert_eq!(target.key_handler, overlay.key_handler.is_some());
        assert_eq!(target.mouse_handler, overlay.mouse_handler.is_some());
    }
}

#[test]
#[ignore = "manual extension scheduler and overlay allocation comparison"]
fn benchmark_extension_polling() {
    use std::hint::black_box;
    for extensions in [0, 8] {
        let mut app = fixture(extensions);
        app.tabs = (0..1000)
            .map(|index| {
                if extensions == 0 {
                    Tab::new_file(Editor::new())
                } else {
                    view(&format!("fixture-{}", index % 8))
                }
            })
            .collect::<Vec<_>>()
            .into();
        let now = Instant::now();
        for record in &app.extension_manager.records {
            app.extension_overlay_last_tick.insert(
                record.manifest.id.clone(),
                record
                    .tick_handlers
                    .iter()
                    .map(|h| (h.export_name.clone(), now))
                    .collect(),
            );
        }
        let mut times = [Vec::new(), Vec::new()];
        for batch in 0..9 {
            for reference in [batch % 2 == 0, batch % 2 != 0] {
                let start = Instant::now();
                for _ in 0..2000 {
                    if reference {
                        let visible = app.open_extension_view_ids();
                        let handlers = app.extension_manager.visible_tick_handlers(&visible);
                        for (id, handler) in handlers {
                            black_box(format!("{id}:{}", handler.export_name));
                            black_box(
                                app.extension_overlay_last_tick
                                    .get(&id)
                                    .and_then(|t| t.get(&handler.export_name)),
                            );
                        }
                    } else {
                        black_box(next_wait_at(&app, now));
                    }
                }
                times[usize::from(!reference)].push(start.elapsed() / 2000);
            }
        }
        for times in &mut times {
            times.sort_unstable();
        }
        eprintln!(
            "extension polling {extensions} extensions/1000 tabs: owned={:?}, borrowed={:?}",
            times[0][4], times[1][4]
        );
    }
    let mut app = fixture(1);
    app.extension_manager.records[0].overlays.insert(
        "overlay".into(),
        ExtensionOverlay {
            id: "overlay".into(),
            title: "Large overlay".into(),
            content: "line\n".repeat(200_000),
            x: 2,
            y: 3,
            width: 40,
            height: 12,
            mouse_handler: Some("mouse".into()),
            key_handler: Some("key".into()),
        },
    );
    for reference in [true, false] {
        let mut times = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..1000 {
                if reference {
                    black_box(app.extension_manager.all_overlays());
                } else {
                    black_box(app.extension_manager.overlay_targets());
                }
            }
            times.push(start.elapsed() / 1000);
        }
        times.sort_unstable();
        eprintln!(
            "1 MB overlay input inventory reference={reference}: {:?}",
            times[4]
        );
    }
}
