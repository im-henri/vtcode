use super::{fixture, render};
use crate::{animation_config::AnimationEffect as Effect, animations::Trigger};
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::{Duration, Instant};

fn edit(app: &mut crate::app::App) {
    crate::input::handle_editor_key(app, KeyCode::Char(' '), KeyModifiers::NONE);
}

fn has_effect(events: &[crate::animations::AnimationEvent], effect: Effect) -> bool {
    events.iter().any(|event| match effect {
        Effect::CursorSmear | Effect::CursorFire => matches!(event.trigger, Trigger::Cursor { .. }),
        Effect::WhitespaceReveal => matches!(event.trigger, Trigger::Insert { .. }),
        Effect::ScopeConnector => matches!(event.trigger, Trigger::Scope { .. }),
        Effect::TabSlide => unreachable!(),
    })
}

#[test]
fn multicursor_suspends_each_editor_effect_and_resumes_without_replaying_edits() {
    for effect in [
        Effect::CursorSmear,
        Effect::CursorFire,
        Effect::WhitespaceReveal,
        Effect::ScopeConnector,
    ] {
        for (width, wrapped, large) in [(110, false, false), (45, true, false), (100, false, true)]
        {
            let mut app = fixture(
                "fn example() {\n    call(first_argument, second_argument, third_argument);\n    call_again();\n}\n",
            );
            app.show_minimap = false;
            app.sticky_scroll = false;
            app.tabs[0].editor.file_path = Some("example.rs".into());
            app.tabs[0].editor.word_wrap = wrapped;
            app.tabs[0].editor.large_file_mode = large;
            app.tabs[0].editor.set_cursor(1, 4);
            app.animation_settings.set(effect, true);
            app.animation_settings.scope_fill = true;
            app.animation_settings.duration_ms = 1500;
            let settings = app.animation_settings.clone();
            render(&mut app, width, 24);
            edit(&mut app);
            render(&mut app, width, 24);
            let id = app.tabs[0].editor.navigation_id;
            let events = app.animations.take_events(id);
            assert!(
                has_effect(&events, effect),
                "the initial single-cursor edit animates: {effect:?}, wrap={wrapped}, large={large}"
            );
            app.animations.restore_events(id, events);

            // Adding another caret does not move the primary caret or change
            // the document. Even a cached cosmetic frame must notice it.
            app.tabs[0].editor.extra_cursors.push((2, 4));
            let replays = app.animation_frame_cache.replays;
            app.rendering_animation_frame = true;
            let paused = render(&mut app, width, 24);
            app.rendering_animation_frame = false;
            assert_eq!(app.animation_frame_cache.replays, replays);
            assert!(app.animations.take_events(id).is_empty());
            assert!(
                app.animations
                    .fire_particles(id, &settings, Instant::now() + Duration::from_millis(100))
                    .is_empty()
            );
            assert_eq!(
                app.animations.next_frame_wait(&settings, Instant::now()),
                None
            );
            assert!(
                !app.animations
                    .cursor_smear_active(id, &settings, Instant::now())
            );
            app.animation_settings.intensity = 0;
            assert_eq!(
                paused,
                render(&mut app, width, 24),
                "all carets and text render normally: {effect:?}, wrap={wrapped}, large={large}"
            );
            app.animation_settings = settings.clone();

            let before = app.tabs[0].editor.rope.len_chars();
            edit(&mut app);
            assert_eq!(
                app.tabs[0].editor.rope.len_chars(),
                before + 2,
                "both carets still edit"
            );
            let source = app.tabs[0].editor.rope.to_string();
            render(&mut app, width, 24);
            assert!(app.animations.take_events(id).is_empty());
            assert_eq!(
                app.animations.next_frame_wait(&settings, Instant::now()),
                None
            );

            app.tabs[0].editor.extra_cursors.clear();
            app.tabs[0].editor.extra_cursor_anchors.clear();
            render(&mut app, width, 24);
            let events = app.animations.take_events(id);
            assert!(
                events
                    .iter()
                    .all(|event| matches!(event.trigger, Trigger::Scope { .. })),
                "returning to one cursor must not replay old motion or whitespace edits"
            );
            app.animations.restore_events(id, events);
            assert_eq!(app.tabs[0].editor.rope.to_string(), source);
            edit(&mut app);
            render(&mut app, width, 24);
            assert!(
                has_effect(&app.animations.take_events(id), effect),
                "single-cursor animation resumes: {effect:?}, wrap={wrapped}, large={large}"
            );
            assert_eq!(
                app.animation_settings, settings,
                "suspension never changes preferences"
            );
        }
    }
}
