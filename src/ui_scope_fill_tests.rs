use super::*;
use std::time::Duration;

fn settings() -> AnimationSettings {
    let mut settings = AnimationSettings::default();
    settings.set(Effect::ScopeConnector, true);
    settings.set_duration(Effect::ScopeConnector, 400);
    settings.set_intensity(Effect::ScopeConnector, 75);
    settings.scope_fill = true;
    settings
}

fn frame(
    text: &[&str],
    pair: Pair,
    settings: &AnimationSettings,
    events: &[AnimationEvent],
    active: bool,
    now: Instant,
    canonical: bool,
) -> (Buffer, Geometry) {
    let (mut buffer, geometry, gutter) = fixture(text, Rect::new(3, 1, 70, text.len() as u16));
    let mut painter = painter(&mut buffer, &geometry, text, pair);
    if canonical {
        let editor = crate::editor::Editor::test_with_text(&text.join("\n"));
        let shape = super::super::super::scope_paint_cache::View::default()
            .shape(&editor, &geometry, pair, settings.scope_tight)
            .unwrap();
        painter.scope_shapes.insert(pair, shape);
    }
    draw(
        &mut painter,
        &geometry,
        settings,
        events,
        active.then_some(pair),
        gutter,
        now,
    );
    painter.finish();
    (buffer, geometry)
}

#[test]
fn scope_fill_waits_for_the_outline_spreads_inward_and_matches_the_idle_frame() {
    let text = [
        "    fn example() {",
        "        work_with_many_arguments();",
        "        more_work();",
        "    }",
    ];
    let pair = ((0, 17), (3, 4));
    let now = Instant::now();
    let event = AnimationEvent {
        trigger: Trigger::Scope {
            pair,
            origin: (1, 8),
            exiting: None,
        },
        started: now,
    };
    let settings = settings();
    let mut outline_only = settings.clone();
    outline_only.scope_fill = false;
    for canonical in [false, true] {
        for ms in [0, 80, 200, 399, 400] {
            let at = now + Duration::from_millis(ms);
            let actual = frame(
                &text,
                pair,
                &settings,
                &[event.clone()],
                true,
                at,
                canonical,
            )
            .0;
            let expected = frame(
                &text,
                pair,
                &outline_only,
                &[event.clone()],
                true,
                at,
                canonical,
            )
            .0;
            assert_eq!(
                actual, expected,
                "fill started during outline: {ms}, canonical={canonical}"
            );
        }
        let baseline = frame(&text, pair, &outline_only, &[], true, now, canonical).0;
        let partial = frame(
            &text,
            pair,
            &settings,
            &[event.clone()],
            true,
            now + Duration::from_millis(600),
            canonical,
        )
        .0;
        let (full, geometry) = frame(
            &text,
            pair,
            &settings,
            &[event.clone()],
            true,
            now + Duration::from_millis(800),
            canonical,
        );
        let idle = frame(
            &text,
            pair,
            &settings,
            &[],
            true,
            now + Duration::from_secs(10),
            canonical,
        )
        .0;
        if canonical {
            crate::ui_control_visibility_tests::snapshot(&baseline, "scope-fill-outline");
            crate::ui_control_visibility_tests::snapshot(&partial, "scope-fill-halfway");
            crate::ui_control_visibility_tests::snapshot(&full, "scope-fill-complete");
        }
        assert_eq!(full, idle, "settling must not change brightness");
        let body: Vec<_> = geometry
            .anchors
            .iter()
            .filter(|a| a.line == 1)
            .map(|a| (a.x, a.y))
            .collect();
        let changed = |buffer: &Buffer| {
            body.iter()
                .filter(|&&p| buffer[p].bg != baseline[p].bg)
                .count()
        };
        assert!(changed(&partial) > 0);
        assert!(
            changed(&partial) < changed(&full),
            "the center should fill after the edges"
        );
        assert!(changed(&full) >= text[1].trim().len());
        for anchor in &geometry.anchors {
            if !anchor.ch.is_whitespace() && anchor.ch != '\0' {
                assert_eq!(
                    full[(anchor.x, anchor.y)].symbol(),
                    baseline[(anchor.x, anchor.y)].symbol()
                );
            }
        }
    }
}

#[test]
fn scope_fill_preserves_neighbors_masks_carets_and_annotations() {
    let text = ["before(); { work(\"界\"); more(); } after();"];
    let pair = (
        (0, 10),
        (0, text[0].chars().position(|ch| ch == '}').unwrap()),
    );
    for canonical in [false, true] {
        let (base, geometry, gutter) = fixture(&text, Rect::new(3, 1, 70, 1));
        let mut buffer = base.clone();
        let mut painter = painter(&mut buffer, &geometry, &text, pair);
        let protected = (18, 1);
        let caret = (19, 1);
        let overlay = (20, 1);
        painter.protected.to_mut().insert(protected);
        painter.carets.to_mut().insert(caret);
        painter.overlays.to_mut().insert(overlay);
        if canonical {
            let editor = crate::editor::Editor::test_with_text(text[0]);
            let shape = super::super::super::scope_paint_cache::View::default()
                .shape(&editor, &geometry, pair, true)
                .unwrap();
            painter.scope_shapes.insert(pair, shape);
        }
        draw(
            &mut painter,
            &geometry,
            &settings(),
            &[],
            Some(pair),
            gutter,
            Instant::now(),
        );
        painter.finish();
        for position in [protected, caret, overlay] {
            assert_eq!(buffer[position], base[position]);
        }
        let mut tinted = 0;
        for anchor in &geometry.anchors {
            let cell = (anchor.x, anchor.y);
            if (anchor.line, anchor.col) < pair.0 || (anchor.line, anchor.col) > pair.1 {
                // The adjacent whitespace can carry the contour, but never fill.
                if !matches!(anchor.ch, ' ' | '\0') {
                    assert_eq!(buffer[cell], base[cell], "neighbor changed at {cell:?}");
                }
            } else {
                tinted += usize::from(buffer[cell].bg != base[cell].bg);
                if anchor.ch != ' ' {
                    assert_eq!(buffer[cell].symbol(), base[cell].symbol());
                }
            }
        }
        assert!(tinted > 10);
    }
}

#[test]
fn scope_fill_freezes_its_reveal_on_exit_and_fades_to_the_original_background() {
    let text = [
        "    if ready {",
        "        work_with_many_arguments();",
        "    }",
    ];
    let pair = ((0, 13), (2, 4));
    let now = Instant::now();
    for elapsed in [200, 600, 800] {
        let exit = now + Duration::from_millis(elapsed);
        let event = AnimationEvent {
            trigger: Trigger::Scope {
                pair,
                origin: (1, 8),
                exiting: Some(exit),
            },
            started: now,
        };
        let settings = settings();
        let (first, geometry) = frame(&text, pair, &settings, &[event.clone()], false, exit, true);
        let fading = frame(
            &text,
            pair,
            &settings,
            &[event.clone()],
            false,
            exit + Duration::from_millis(80),
            true,
        )
        .0;
        let gone = frame(
            &text,
            pair,
            &settings,
            &[event],
            false,
            exit + AnimationEvent::scope_exit_duration(&settings),
            true,
        )
        .0;
        let baseline = frame(&text, pair, &settings, &[], false, now, true).0;
        assert_eq!(gone, baseline);
        for anchor in geometry.anchors.iter().filter(|a| a.line == 1) {
            let p = (anchor.x, anchor.y);
            if first[p].bg == baseline[p].bg {
                assert_eq!(
                    fading[p].bg, baseline[p].bg,
                    "exit continued filling new cells"
                );
            }
        }
        if elapsed > 400 {
            assert_ne!(first, fading);
        }
    }
}

#[test]
fn scope_fill_keeps_text_readable_in_light_and_dark_themes() {
    let text = ["    if ready {", "        work();", "    }"];
    let pair = ((0, 13), (2, 4));
    for theme in [
        crate::app_theme::EditorTheme::Dark,
        crate::app_theme::EditorTheme::Light,
    ] {
        let colors = theme.colors();
        let (mut buffer, geometry, gutter) = fixture(&text, Rect::new(3, 1, 70, 3));
        buffer.set_style(
            buffer.area,
            Style::default().fg(colors.fg_text).bg(colors.bg_editor),
        );
        let base = buffer.clone();
        let mut painter = painter(&mut buffer, &geometry, &text, pair);
        painter.colors = colors;
        draw(
            &mut painter,
            &geometry,
            &settings(),
            &[],
            Some(pair),
            gutter,
            Instant::now(),
        );
        painter.finish();
        for anchor in geometry
            .anchors
            .iter()
            .filter(|a| a.line == 1 && a.ch.is_alphabetic())
        {
            let cell = &buffer[(anchor.x, anchor.y)];
            let original = &base[(anchor.x, anchor.y)];
            assert_eq!(cell.symbol(), original.symbol());
            assert_ne!(cell.bg, original.bg);
            let floor = crate::ui_colors::contrast_ratio(original.fg, original.bg)
                .unwrap()
                .min(4.5);
            assert!(crate::ui_colors::contrast_ratio(cell.fg, cell.bg).unwrap() >= floor);
        }
    }
}
