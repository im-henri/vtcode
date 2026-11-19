use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app_with(text: &str) -> App {
    let mut app = App::test_new();
    let mut tab = crate::app::Tab::new_file(crate::editor::Editor::test_with_text(text));
    tab.editor.language_override = Some("Rust".into());
    tab.editor.cursor_line = tab.editor.rope.len_lines() - 1;
    tab.editor.cursor_col = tab.editor.rope.line(tab.editor.cursor_line).len_chars();
    app.tabs.push(tab);
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app
}

fn show(app: &mut App, text: &str) {
    let target = app.inline_target().unwrap();
    let rope = app.editor().unwrap().rope.clone();
    app.vtcomplete_inline.set_ghost_for_test(target, rope, text);
}

fn with_ghost() -> App {
    let mut app = app_with("let value = ");
    show(&mut app, "project_value.len();");
    app
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let key = KeyEvent::new(code, modifiers);
    app.begin_inline_key();
    assert!(!crate::input_keyboard::handle_keyboard_event(app, key));
    app.finish_inline_key(trigger_key(key));
}

fn text(app: &App) -> String {
    app.editor().unwrap().rope.to_string()
}

const SETTERS: &str = "\
impl Margins {
    fn set_left(&mut self, left: u32) {
        self.left = left;
    }

    fn set_top(&mut self, top: u32) {
        self.top = top;
    }

    fn set_bottom(&mut self, bottom: u32) {
        self.bottom = bottom;
    }

    fn set_right(&mut self, right: u32) {
        self";

#[test]
fn typing_requests_a_prediction_and_tab_inserts_it() {
    let mut app = app_with(SETTERS);
    press(&mut app, KeyCode::Char('.'), KeyModifiers::NONE);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.inline_ghost_visible()
    });
    assert_eq!(app.inline_ghost_text(), Some("right = right;"));
    assert!(text(&app).ends_with("self."), "prediction must not edit the buffer");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(text(&app).ends_with("        self.right = right;"));
    app.editor_mut().unwrap().undo();
    assert!(text(&app).ends_with("        self."));
}

#[test]
fn tab_accepts_as_one_undo_step_and_escape_only_hides() {
    let mut app = with_ghost();
    assert!(app.inline_ghost_visible());
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "let value = project_value.len();");
    app.editor_mut().unwrap().undo();
    assert_eq!(text(&app), "let value = ");

    let mut app = with_ghost();
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(text(&app), "let value = ");
    assert!(!app.inline_ghost_visible());
    // A late answer for the dismissed caret position stays hidden.
    let target = app.inline_target().unwrap();
    let rope = app.editor().unwrap().rope.clone();
    let generation = app.vtcomplete_inline.generation;
    assert!(!app.receive_inline(generation, target, rope, Some("again();".into())));
    assert!(!app.inline_ghost_visible());
}

#[test]
fn control_right_accepts_one_word_and_keeps_the_rest() {
    let mut app = with_ghost();
    press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(text(&app), "let value = project_value");
    assert_eq!(app.inline_ghost_text(), Some(".len();"));
    press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(text(&app), "let value = project_value.len");
    assert_eq!(app.inline_ghost_text(), Some("();"));
    // Without a prediction the shortcut moves by a word as before.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    app.editor_mut().unwrap().cursor_col = 0;
    press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(text(&app), "let value = project_value.len");
    assert!(app.editor().unwrap().cursor_col > 0);
}

#[test]
fn word_acceptance_always_advances() {
    assert_eq!(next_word_end("project_value.len()"), 13);
    assert_eq!(next_word_end(".len()"), 4);
    assert_eq!(next_word_end("();"), 3);
    assert_eq!(next_word_end(" = größe;"), 10);
    assert_eq!(next_word_end("a\n    b"), 1);
    assert_eq!(next_word_end("\n    b"), 6);
}

#[test]
fn typing_along_keeps_the_remainder_without_a_new_answer() {
    let mut app = with_ghost();
    press(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(text(&app), "let value = pr");
    assert_eq!(app.inline_ghost_text(), Some("oject_value.len();"));
    // Typing something else hides it at once.
    press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(!app.inline_ghost_visible());
}

#[test]
fn caret_movement_selection_and_other_edits_hide_the_prediction() {
    let mut app = with_ghost();
    app.editor_mut().unwrap().cursor_col -= 1;
    assert!(!app.inline_ghost_visible());
    assert!(!app.accept_inline(false));
    app.editor_mut().unwrap().cursor_col += 1;
    assert!(app.inline_ghost_visible());
    app.editor_mut().unwrap().selection_anchor = Some((0, 0));
    assert!(!app.inline_ghost_visible());
    app.editor_mut().unwrap().selection_anchor = None;
    app.editor_mut().unwrap().rope.insert(0, " ");
    assert!(!app.accept_inline(false));
    assert_eq!(text(&app), " let value = ");
}

#[test]
fn navigation_keys_do_not_request_predictions() {
    let mut app = app_with(SETTERS);
    press(&mut app, KeyCode::Left, KeyModifiers::NONE);
    press(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(app.vtcomplete_inline.submissions, 0);
    press(&mut app, KeyCode::Char('.'), KeyModifiers::NONE);
    assert_eq!(app.vtcomplete_inline.submissions, 1);
}

#[test]
fn the_setting_turns_predictions_off() {
    let mut app = app_with(SETTERS);
    let mut settings = app.completion_preferences;
    assert!(settings.inline_predictions);
    settings.inline_predictions = false;
    app.completion_preferences = settings;
    press(&mut app, KeyCode::Char('.'), KeyModifiers::NONE);
    assert_eq!(app.vtcomplete_inline.submissions, 0);
    assert!(!app.inline_ghost_visible());
}

#[test]
fn overlays_reject_ghost_text_and_acceptance() {
    type Overlay = (&'static str, fn(&mut App));
    let overlays: &[Overlay] = &[
        ("explorer context", |app| app.explorer_context_active = true),
        ("shortcuts reference", |app| app.shortcuts_active = true),
        ("settings panel", |app| app.settings_active = true),
        ("menu dropdown", |app| app.menu_open = true),
        ("hunk popup", |app| app.hunk_popup_active = true),
        ("code actions", |app| app.lsp_code_action_active = true),
        ("command chord", |app| app.chord_pending = true),
        ("read-only", |app| app.editor_mut().unwrap().read_only = true),
        ("explicit popup", |app| app.vtcomplete_explicit = true),
        ("navigated popup", |app| app.vtcomplete_navigated = true),
    ];
    for &(name, activate) in overlays {
        let mut app = with_ghost();
        activate(&mut app);
        assert!(!app.inline_ghost_visible(), "{name}");
        assert!(!app.accept_inline(false), "{name}");
        assert_eq!(text(&app), "let value = ", "{name}");
    }
}

#[test]
fn the_word_popup_is_hidden_behind_a_prediction_and_returns_after_escape() {
    let mut app = with_ghost();
    app.vtcomplete_active = true;
    app.vtcomplete_suggestions = vec!["indexed_value".into()];
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "let value = project_value.len();");

    let mut app = with_ghost();
    app.vtcomplete_active = true;
    app.vtcomplete_suggestions = vec!["indexed_value".into()];
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.vtcomplete_active, "the first Esc hides only the prediction");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "let value = indexed_value");
}

#[test]
fn stale_answers_are_ignored() {
    let mut app = app_with("let value = ");
    let target = app.inline_target().unwrap();
    let rope = app.editor().unwrap().rope.clone();
    let generation = app.vtcomplete_inline.generation;
    assert!(!app.receive_inline(generation + 1, target.clone(), rope.clone(), Some("a();".into())));
    let other = ropey::Rope::from_str("let value = ");
    assert!(!app.receive_inline(generation, target.clone(), other, Some("a();".into())));
    assert!(!app.inline_ghost_visible());
    assert!(app.receive_inline(generation, target.clone(), rope.clone(), Some("a();".into())));
    assert_eq!(app.inline_ghost_text(), Some("a();"));
    // An abstaining answer leaves what is shown.
    assert!(!app.receive_inline(generation, target, rope, None));
    assert_eq!(app.inline_ghost_text(), Some("a();"));
}

#[test]
fn large_documents_are_windowed_at_line_boundaries() {
    let line = "let value = compute(input);\n";
    let rope = ropey::Rope::from_str(&line.repeat(2 * MAX_DOCUMENT_BYTES / line.len()));
    let last = rope.len_lines() - 2;
    let (before, rest, below, window) = split(&rope, (last, 4)).unwrap();
    assert!(before.len() <= MAX_DOCUMENT_BYTES);
    assert!(before.starts_with("let value") && before.ends_with("\nlet "));
    assert_eq!(rest, "value = compute(input);");
    assert_eq!(below, "");
    assert!(window > 0);
    let (before, rest, below, window) = split(&rope, (1, 4)).unwrap();
    assert_eq!((before.as_str(), rest.as_str(), window), (&*format!("{line}let "), "value = compute(input);", 0));
    assert!(below.len() <= MAX_BELOW_BYTES && below.starts_with("let value"));
    assert!(split(&rope, (rope.len_lines(), 0)).is_none());
}

fn render(app: &mut App, width: u16, height: u16) -> ratatui::Terminal<ratatui::backend::TestBackend> {
    use ratatui::{Terminal, backend::TestBackend, style::Color};
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            for y in 0..height {
                for x in 0..width {
                    frame.buffer_mut()[(x, y)].set_symbol(".").set_bg(Color::Black);
                }
            }
            draw_ghost(frame, app);
        })
        .unwrap();
    terminal
}

fn row(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>, y: u16, from: u16, to: u16) -> String {
    (from..to)
        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
        .collect()
}

#[test]
fn ghost_is_painted_before_the_rest_of_the_line_without_editing() {
    use ratatui::{layout::Rect, style::Modifier};
    let mut app = app_with("let value = existing();");
    app.editor_mut().unwrap().cursor_col = 12;
    show(&mut app, "project_");
    app.code_area = Some(Rect::new(2, 1, 48, 2));
    let before = app.editor().unwrap().rope.clone();
    let terminal = render(&mut app, 54, 4);
    assert!(row(&terminal, 1, 14, 36).starts_with("project_existing();"));
    let buffer = terminal.backend().buffer();
    assert!(buffer[(14, 1)].modifier.contains(Modifier::ITALIC));
    assert!(!buffer[(22, 1)].modifier.contains(Modifier::ITALIC));
    assert!(app.editor().unwrap().rope.is_instance(&before));
    assert_eq!(buffer[(0, 0)].symbol(), ".");
}

#[test]
fn further_predicted_lines_overlay_the_rows_below() {
    use ratatui::layout::Rect;
    let mut app = app_with("    if ready {\nnext();\n");
    let editor = app.editor_mut().unwrap();
    editor.cursor_line = 0;
    editor.cursor_col = 14;
    show(&mut app, "\n        start();\n    }");
    app.code_area = Some(Rect::new(0, 0, 40, 4));
    let terminal = render(&mut app, 40, 4);
    assert!(row(&terminal, 1, 0, 40).starts_with("        start();"));
    assert!(row(&terminal, 2, 0, 40).starts_with("    }"));
    assert!(row(&terminal, 3, 0, 40).starts_with("...."));
    assert_eq!(text(&app), "    if ready {\nnext();\n");
}

#[test]
fn an_offscreen_caret_draws_nothing() {
    use ratatui::layout::Rect;
    let mut app = app_with(&"line\n".repeat(50));
    show(&mut app, "ghost_text");
    app.editor_mut().unwrap().scroll_offset = 0;
    app.code_area = Some(Rect::new(0, 0, 20, 3));
    let terminal = render(&mut app, 20, 3);
    for y in 0..3 {
        assert_eq!(row(&terminal, y, 0, 20), ".".repeat(20));
    }
}

fn screen(app: &mut App) -> (Vec<String>, ratatui::buffer::Buffer) {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let rows = (0..30)
        .map(|y| (0..100).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();
    (rows, buffer)
}

/// The whole application frame, drawn by the real renderer after real key
/// handling and a real worker answer.
#[test]
fn the_full_editor_frame_shows_the_prediction_dimmed_and_then_the_accepted_text() {
    use ratatui::style::Modifier;
    let mut app = app_with(SETTERS);
    screen(&mut app);
    press(&mut app, KeyCode::Char('.'), KeyModifiers::NONE);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.inline_ghost_visible()
    });
    let (rows, buffer) = screen(&mut app);
    if std::env::var_os("VTCODE_PRINT_INLINE_FRAME").is_some() {
        eprintln!("{}", rows.join("\n"));
    }
    let (y, x) = rows
        .iter()
        .enumerate()
        .find_map(|(y, row)| row.find("self.right = right;").map(|x| (y, row[..x].chars().count())))
        .expect("the prediction is on screen");
    // `self.` is the document's own text; the rest is the italic prediction.
    assert!(!buffer[(x as u16, y as u16)].modifier.contains(Modifier::ITALIC));
    for offset in 5..19 {
        assert!(
            buffer[((x + offset) as u16, y as u16)].modifier.contains(Modifier::ITALIC),
            "{offset}"
        );
    }
    assert!(text(&app).ends_with("        self."));

    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(text(&app).ends_with("        self.right = right;"));
    let (rows, buffer) = screen(&mut app);
    let (y, x) = rows
        .iter()
        .enumerate()
        .find_map(|(y, row)| row.find("self.right = right;").map(|x| (y, row[..x].chars().count())))
        .expect("the accepted text is on screen");
    assert!(!buffer[((x + 8) as u16, y as u16)].modifier.contains(Modifier::ITALIC));
}

#[test]
fn the_predict_here_shortcut_asks_at_the_caret_without_typing() {
    let mut app = app_with(SETTERS);
    // Nothing was typed, so nothing was asked yet.
    assert_eq!(app.vtcomplete_inline.submissions, 0);
    press(&mut app, KeyCode::Char('i'), KeyModifiers::ALT);
    assert_eq!(app.vtcomplete_inline.submissions, 1);
    assert!(text(&app).ends_with("        self"), "the shortcut must not type");
    // It works with automatic predictions turned off, and through the palette.
    let mut settings = app.completion_preferences;
    settings.inline_predictions = false;
    app.completion_preferences = settings;
    app.execute_palette_command("vtcomplete_inline_request");
    assert_eq!(app.vtcomplete_inline.submissions, 2);
    // Where predictions cannot be shown it does nothing.
    app.editor_mut().unwrap().read_only = true;
    app.execute_palette_command("vtcomplete_inline_request");
    assert_eq!(app.vtcomplete_inline.submissions, 2);
}

#[test]
fn inline_patterns_use_the_active_file() {
    let mut app = app_with("let peer = lookup();\nlet delta = Channel::open(config);\ndelta.");
    let other_text = "let alpha = Channel::open(config);\nalpha.connect(peer);\n\
                      let beta = Channel::open(config);\nbeta.connect(peer);\n\
                      let gamma = Channel::open(config);\ngamma.connect(peer);\n";
    let mut other = crate::editor::Editor::test_with_text(other_text);
    other.language_override = Some("Rust".into());
    app.tabs.push(crate::app::Tab::new_file(other));
    app.request_inline_prediction();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.vtcomplete_inline.cancel.is_none()
    });
    assert!(app.inline_ghost_text().is_none());
}

#[test]
fn eagerness_is_a_saved_setting_that_lowers_the_confidence_needed() {
    use crate::app_vtcomplete_settings::InlineEagerness;
    let mut app = app_with(SETTERS);
    assert_eq!(app.completion_preferences.inline_eagerness, InlineEagerness::Balanced);
    for (label, level) in [("Cautious", 0), ("Balanced", 1), ("Eager", 2), ("Always", 3)] {
        app.apply_settings_dropdown_value("vtcomplete_inline_eagerness", label);
        let chosen = app.completion_preferences.inline_eagerness;
        assert_eq!(chosen.label(), label);
        assert_eq!(InlineEagerness::ALL[level], chosen);
    }
    app.apply_settings_dropdown_value("vtcomplete_inline_eagerness", "Reckless");
    assert_eq!(app.completion_preferences.inline_eagerness, InlineEagerness::Always, "an unknown choice changes nothing");
    // Each step is more willing to show a guess than the one before.
    let levels: Vec<Options> = (0..4).map(Options::preset).collect();
    for pair in levels.windows(2) {
        assert!(pair[1].first < pair[0].first && pair[1].floor < pair[0].floor);
        assert!(pair[1].next < pair[0].next && pair[1].block < pair[0].block);
    }
    // A requested prediction shows its first token whatever the chance,
    // and is never more cautious than the setting about the rest.
    for level in &levels {
        let asked = Options::requested(*level);
        assert_eq!((asked.first, asked.floor), (0., 0.));
        assert!(asked.next <= level.next.min(0.35) && asked.block <= level.block);
    }
    // The choice survives saving and loading the settings.
    let saved = serde_json::to_string(&app.completion_preferences).unwrap();
    assert!(saved.contains("\"inlineEagerness\":\"always\""), "{saved}");
}

/// The real path with the language model on: a key typed inside a comment,
/// the worker's answer, and the frame the renderer draws. The pattern
/// predictor has nothing to say in a comment, so the text shown is the
/// model's own.
#[test]
fn a_comment_is_continued_in_the_editor_by_the_language_model() {
    use ratatui::style::Modifier;
    if crate::vtcomplete_inline::model::embedded().is_none() {
        return; // No model asset in this build.
    }
    let mut app = app_with(
        "\
/// A rectangle with a position and a size.
pub struct Rectangle {
    pub width: f32,
    pub height: f32,
}

impl Rectangle {
    /// Creates a rectangle from its size.
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// Returns the area of the rec",
    );
    app.vtcomplete_inline.with_language_model = true;
    press(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
    app.wait_for_background(std::time::Duration::from_secs(240), |app| {
        app.inline_ghost_visible()
    });
    let ghost = app.inline_ghost_text().expect("a prediction inside the comment").to_owned();
    assert!(ghost.starts_with("angle"), "{ghost:?}");
    assert!(text(&app).ends_with("of the rect"), "prediction must not edit the buffer");
    let (rows, buffer) = screen(&mut app);
    let (y, x) = rows
        .iter()
        .enumerate()
        .find_map(|(y, row)| row.find("of the rectangle").map(|x| (y, row[..x].chars().count())))
        .expect("the prediction is on screen");
    // `rect` was typed; `angle` is the dimmed prediction.
    assert!(!buffer[((x + 9) as u16, y as u16)].modifier.contains(Modifier::ITALIC));
    assert!(buffer[((x + 11) as u16, y as u16)].modifier.contains(Modifier::ITALIC));
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(text(&app).contains("/// Returns the area of the rectangle"));
}

/// Typing a name in lower case still finds it, and accepting the prediction
/// restores its capitals in the same undo step.
#[test]
fn accepting_restores_the_capitals_of_a_name_typed_in_lower_case() {
    let mut app = app_with(
        "\
struct LayoutRenderers;
fn first() -> LayoutRenderers {
    LayoutRenderers
}
fn second() -> LayoutRenderers {
    LayoutRenderers
}
fn third() -> layout",
    );
    press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.inline_ghost_visible()
    });
    let ghost = app.inline_ghost_text().expect("a prediction").to_owned();
    assert!(ghost.starts_with("enderers"), "{ghost:?}");
    assert!(text(&app).ends_with("-> layoutr"), "nothing is respelled before accepting");
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(text(&app).contains("fn third() -> LayoutRenderers"), "{}", text(&app));
    app.editor_mut().unwrap().undo();
    assert!(text(&app).ends_with("fn third() -> layoutr"));
}

fn with_popup(text: &str) -> App {
    let mut app = app_with(text);
    app.vtcomplete_active = true;
    app.vtcomplete_suggestions = vec!["indexed_value".into()];
    app
}

#[test]
fn the_word_popup_waits_for_a_pending_prediction_unless_already_shown() {
    let mut app = with_popup("let value = ind");
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(app.inline_holds_popup(), "a popup not yet on screen waits");
    assert!(app.inline_hides_popup());
    assert!(app.inline_hold_wait().is_some());
    // The answer ends the wait; with nothing to show, the popup appears.
    let target = app.inline_target().unwrap();
    let rope = app.editor().unwrap().rope.clone();
    let generation = app.vtcomplete_inline.generation;
    assert!(app.receive_inline(generation, target, rope, None));
    assert!(!app.inline_holds_popup());
    assert!(!app.inline_hides_popup());

    // A popup already in view stays in view.
    let mut app = with_popup("let value = ind");
    app.vtcomplete_inline.popup_drawn = true;
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(!app.inline_holds_popup());
}

#[test]
fn a_held_popup_ends_on_time_and_its_keys_act_as_if_it_were_not_there() {
    let mut app = with_popup("let value = ind");
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(app.inline_holds_popup());
    // Tab cannot accept a suggestion nobody has seen.
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(!text(&app).contains("indexed_value"), "{}", text(&app));

    let mut app = with_popup("let value = ind");
    press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    app.vtcomplete_inline.holding = Some(std::time::Instant::now() - INLINE_HOLD);
    assert!(!app.inline_holds_popup());
    assert!(app.expire_inline_hold(), "running out redraws the popup");
    assert!(!app.expire_inline_hold());
    assert_eq!(app.inline_hold_wait(), None);
}

#[test]
fn accepting_shows_the_prediction_made_ahead_for_the_accepted_text_at_once() {
    let mut app = with_ghost();
    let document = app.inline_target().unwrap().document;
    let accepted_text = "let value = project_value.len();";
    app.vtcomplete_inline.queue.lock().unwrap().speculation = Some(Speculation {
        document,
        cursor: (0, accepted_text.chars().count()),
        rope: ropey::Rope::from_str(accepted_text),
        text: " // counted".into(),
    });
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), accepted_text);
    assert_eq!(app.inline_ghost_text(), Some(" // counted"));

    // Made ahead for other text, it is not used.
    let mut app = with_ghost();
    let document = app.inline_target().unwrap().document;
    app.vtcomplete_inline.queue.lock().unwrap().speculation = Some(Speculation {
        document,
        cursor: (0, accepted_text.chars().count()),
        rope: ropey::Rope::from_str("let other = project_value.len();"),
        text: " // counted".into(),
    });
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_ne!(app.inline_ghost_text(), Some(" // counted"));
}

#[test]
fn the_accepted_text_and_caret_are_worked_out_across_lines() {
    let rope = ropey::Rope::from_str("fn a() {\n    b\n}\n");
    let (one, caret) = accepted(&rope, (1, 5), "(1);").unwrap();
    assert_eq!(one.to_string(), "fn a() {\n    b(1);\n}\n");
    assert_eq!(caret, (1, 9));
    let (two, caret) = accepted(&rope, (1, 5), "(1);\n    c();").unwrap();
    assert_eq!(two.to_string(), "fn a() {\n    b(1);\n    c();\n}\n");
    assert_eq!(caret, (2, 8));
    assert!(accepted(&rope, (9, 0), "x").is_none());
}
