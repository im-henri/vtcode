#[macro_use]
mod logging;
mod clipboard;
mod cli;
mod portable;
mod native_paths;
mod filesystem_roots;
mod file_icons;
mod app;
mod animation_config;
mod animation_scope;
mod animation_structural_scope;
mod animations;
mod copy_animation;
mod editor_paste;
mod app_paste;
mod app_exit_backup;
mod animation_pacing;
mod ui_animation_frame;
mod ui_animation_chrome;
mod cursor_smear;
mod cursor_smear_raster;
mod cursor_fire;
mod fire_raster;
mod tab_indicator_motion;
mod app_acp;
mod app_chat_files;
mod app_chat_terminals;
mod app_vtcomplete;
mod app_vtcomplete_index;
mod app_vtcomplete_settings;
mod completion_index;
mod search_history;
mod completion_activity;
mod completion_context;
mod completion_source;
mod completion_history;
#[path = "vtcomplete_identifier.rs"]
mod completion_match;
mod ui_vtcomplete_index;
mod app_commands;
mod app_commands_editor;
mod app_commands_git;
mod app_commands_settings;
mod app_commands_view;
mod app_context_menus;
mod app_deleted_files;
mod app_event_loop;
mod app_explorer;
mod app_explorer_hover;
mod app_workspace_events;
mod app_external_open;
mod app_extensions;
mod app_git;
mod hunk_preview;
mod app_git_branch;
mod app_git_display;
mod app_source_control;
mod app_global_search;
mod app_layout;
mod app_desks;
mod editor_document;
mod merge_conflicts;
mod merge_conflict_ui;
mod ui_desks;
mod app_lsp_poll;
mod app_lsp_edits;
mod app_lsp_workspace_edit;
mod app_lsp_interactive;
mod app_lsp_format;
mod app_formatters;
mod file_associations;
mod scope_bubble_config;
mod ui_file_associations;
mod ui_exclusion_settings;
#[cfg(test)]
mod workspace_exclusion_tests;
mod ui_scope_bubble_settings;
mod formatters;
mod ui_formatter_settings;
mod app_lsp_links;
mod app_menu;
mod app_navigation;
mod app_remote_navigation;
mod app_remote_connection;
mod app_save_conflict;
mod ui_remote_connection;
mod ui_side_notice;
mod ui_logs;
mod app_search;
mod source_symbols;
mod workspace_replace;
mod app_search_algo;
mod search_regex;
mod search_replace;
mod search_paths;
mod search_walk;
mod search_results;
mod file_order;
#[cfg(test)]
mod vscode_search_tests;
mod app_search_fields;
mod file_search;
mod editor_view_state;
mod app_session;
#[cfg(test)]
mod app_workspace_session_tests;
mod app_settings_ui;
mod app_settings_navigation;
mod app_tabs;
mod app_tab_close;
mod tab_collection;
mod app_wrap;
mod app_terminal;
mod app_terminal_selection;
mod app_text_fields;
mod app_theme;
mod app_theme_loader;
mod builtin_themes;
mod app_ui_state;
mod background;
mod workspace_watch;
mod worker_priority;
#[cfg(any(windows, test))]
mod terminal_input_batch;
mod ui_work;
mod ui_loading;
mod performance;
mod process_usage;
mod fs_ops;
mod file_deletion;
mod ui_file_deletion;
#[cfg(test)]
mod basic_file_safety_tests;
#[cfg(test)]
mod basic_operations_audit_tests;
mod editor;
mod editor_ai_review;
mod diff_context;
mod diff_selection;
mod diff_scope_source;
mod editor_comments;
mod editor_columns;
mod editor_cursor;
mod editor_cursor_brackets;
mod editor_line_ops;
mod rope_changes;
mod rope_line_blocks;
mod editor_multicursor;
mod editor_multicursor_ops;
mod editor_selection;
mod editor_snippets;
mod editor_text_ops;
mod editor_transform;
mod editor_undo;
mod editor_wrap;
mod editorconfig;
mod explorer;
mod explorer_find;
mod ui_explorer_find;
mod folder_browser;
mod external_open;
mod extension_handlers;
mod extension_scheduler;
mod extension_documents;
mod extension_buffer;
mod extension_editor_host;
mod extension_surface;
mod extension_host;
mod extension_install;
mod extension_release_source;
mod extension_versions;
mod extension_recovery;
#[cfg(test)] mod extension_hex_tests;
#[cfg(test)] mod extension_waveform_tests;
#[cfg(test)] mod extension_tabular_tests;
mod app_extension_install;
mod extension_lifecycle;
mod extension_manifest;
mod extension_views;
mod extension_wasm;
mod extensions;
mod helpers;
mod highlight;
mod input;
mod input_dialogs;
mod input_keyboard;
mod input_keyboard_dialogs;
mod input_pickers;
mod input_terminal;
mod lsp;
mod lsp_events;
mod lsp_parsing;
mod lsp_positions;
mod lsp_requests;
mod mouse;
mod fuzzy_score;
mod mouse_dialogs;
mod mouse_drag;
mod mouse_overlay;
mod mouse_scroll;
mod mouse_selection;
mod session;
mod startup;
mod settings;
mod settings_layout;
mod syntax_assets;
mod terminal;
mod terminal_output;
mod ui_terminal_rows;
mod remote;
mod git_tasks;
mod git_output;
#[cfg(windows)]
mod windows_input;
#[cfg(unix)]
mod unix_input;
#[cfg(test)]
mod keyboard_protocol_tests;
#[cfg(windows)]
mod windows_mouse;
mod text_input;
mod commit_input;
mod commit_details;
mod commit_review;
mod ui_commit_details;
mod diff_tabs;
mod git_diff_sources;
mod inline_diff;
mod git_history;
mod git_repositories;
mod git_trust;
mod git_blame;
mod editor_blame;
mod ui_inline_blame;
mod ui;
mod ui_keybindings;
#[cfg(test)]
#[path = "ui_state_interaction_tests.rs"]
mod ui_state_interaction_tests;
mod ui_chat;
mod ui_agent_settings;
mod ui_lsp_settings;
mod app_lsp_config;
mod ui_chrome;
mod ui_chrome_tabs;
mod ui_colors;
mod ui_dialogs;
mod ui_modal;
mod ui_quick_picker;
mod ui_diff_display;
mod ui_diff_animations;
mod ui_hunk_peek;
mod ui_editor;
mod ui_editor_folds;
mod ui_editor_animations;
#[cfg(test)]
mod animation_ui_tests;
#[cfg(test)]
mod ui_diff_animation_tests;
mod ui_editor_highlight;
mod ui_editor_minimap;
mod ui_extension_info;
mod ui_extensions;
#[cfg(test)]
mod extensions_ui_tests;
mod ui_extension_versions;
mod ui_helpers;
mod ui_markdown;
mod ui_panels;
mod ui_status;
mod ui_performance;
mod ui_performance_details;
mod ui_pickers;
mod ui_folder_browser;
mod ui_search_dialog;
mod ui_lsp_completion;
mod ui_search_controls;
mod ui_search_panel;
mod ui_settings;
#[cfg(test)]
mod ui_settings_redesign_tests;
#[cfg(test)]
mod ui_settings_scroll_tests;
mod ui_source_control;
mod source_control_rows;
mod ui_welcome;
mod ui_widgets;
mod vscode_theme;
mod semantic_theme;
mod scope_selector;
mod textmate;
#[cfg(test)]
mod textmate_tests;
mod textmate_theme;
mod textmate_backend;
mod vscode_color_registry;
#[cfg(test)]
mod vscode_color_render_tests;
mod textmate_grammar;
mod theme_install;
mod theme_catalog;
mod app_theme_management;
mod theme_download;
mod app_theme_install;
mod app_syntax_install;
mod syntax_install;
mod theme_workbench;
mod theme_customizations;
#[cfg(test)]
mod theme_visibility_tests;
#[cfg(test)]
mod theme_audit_tests;
#[cfg(test)]
mod theme_coverage_tests;
#[cfg(test)]
mod ui_control_visibility_tests;

use std::io;
use std::path::{Path, PathBuf};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;

fn startup_file_directory(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

#[cfg(test)]
#[path = "startup_chat_tests.rs"]
mod startup_chat_tests;

#[cfg(test)]
mod startup_highlight_tests;

#[cfg(test)]
mod startup_performance_tests;

fn main() {
    if let Err(error) = run_main() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run_main() -> io::Result<()> {
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}\n\nRun 'vtcode --help' for usage and available arguments.");
            std::process::exit(2);
        }
    };
    let (raw_arg, remote_launch) = match command {
        cli::Command::Help => return cli::write_help(&mut io::stdout().lock()),
        cli::Command::Version => {
            println!("vtcode {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        cli::Command::Headless(root) => return remote::service::run(&root),
        #[cfg(windows)]
        cli::Command::ConsoleHost(args) => {
            std::process::exit(remote::launch::run_host(&args)?);
        }
        cli::Command::InstallTheme(source) => {
            let root = portable::theme_extensions_dir();
            let themes = theme_install::install(&source, &root).map_err(io::Error::other)?;
            for theme in &themes { println!("Installed: {} ({})", theme.name, theme.config_key); }
            println!("Select a theme with Preferences: Color Theme. Use Preferences: Reload Color Themes in an already running vtcode.");
            return Ok(());
        }
        cli::Command::Editor(path) => (path, None),
        cli::Command::Remote { program, args } => (None, Some(remote::launch::Launch::new(program, args))),
    };
    let mut startup_timings = startup::Timings::new();
    let _clipboard_guard = clipboard::ClipboardGuard;

    // No workspace/settings/asset access may precede the first terminal frame.
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        #[cfg(unix)]
        unix_input::restore_keyboard_protocol();
        let _ = disable_raw_mode(); // Error OK: terminal state cleanup is best-effort
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::style::Print("\x1b[>0s"),
            DisableBracketedPaste,
            DisableFocusChange,
            crossterm::cursor::SetCursorStyle::DefaultUserShape,
        );
        default_panic(info);
    }));

    #[cfg(windows)]
    let console_input = windows_input::ConsoleInputMode::save()?;
    let screen_cleanup = ScreenCleanup;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        // XTSHIFTESCAPE: ask supporting terminals to forward Shift gestures.
        // Windows Terminal needs the focused native bridge in windows_mouse.
        crossterm::style::Print("\x1b[>1s"),
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    #[cfg(unix)]
    let keyboard_protocol = unix_input::KeyboardProtocol::enable(&mut stdout)?;
    // Mouse capture replaces the Windows input mode, so enable VT input last.
    #[cfg(windows)]
    {
        console_input.use_alternate_screen_coordinates();
        console_input.enable_virtual_terminal_input()?;
    }
    let backend = CrosstermBackend::new(terminal_output::buffered_output(stdout));
    let mut terminal = terminal_output::event_driven_terminal(backend)?;
    startup_timings.mark("terminal_ready");
    {
        let _priority = worker_priority::InputPriority::new(true);
        terminal.draw(ui_welcome::draw_bootstrap_screen)?;
    }
    startup_timings.mark("first_frame");
    let wake = background::UiWake::default();
    let mut events = app_event_loop::TerminalEventPump::start(wake.clone())?;
    let remote_workspace = if let Some(launch) = remote_launch {
        let size = terminal.get_frame().area().as_size();
        let setup_loader = startup::Loader::start(wake.clone(), {
            let wake = wake.clone();
            move || -> std::io::Result<_> {
                let setup = launch.start(size, wake)?;
                let mut settings = settings::load_from_file(&settings::global_settings_path());
                settings.validate();
                let colors = startup::connection_colors(&settings, &portable::base_dir());
                Ok((setup, colors))
            }
        })?;
        let Some(loaded) = setup_loader.wait(&mut terminal, &mut events)? else { return Ok(()); };
        let (setup, colors) = loaded?;
        let Some(workspace) = setup.wait(&mut terminal, &mut events, &colors)? else { return Ok(()); };
        Some(workspace)
    } else { None };
    let remote_root = remote_workspace.as_ref().map(|remote| remote.view_root.clone());
    let loader = startup::Loader::start(wake.clone(), move || match remote_root {
        Some(root) => startup::Prepared::load_remote(root),
        None => startup::Prepared::load(raw_arg),
    })?;
    let Some(prepared) = loader.wait(&mut terminal, &mut events)? else {
        startup_timings.mark("cancelled");
        startup_timings.finish();
        return Ok(());
    };
    startup_timings.mark("manifest_ready");
    let mut app = prepared.into_app_with_remote(wake, remote_workspace);
    startup_timings.mark("workbench_ready");
    let res = app_event_loop::run_app(&mut terminal, &mut app, events, Some(startup_timings));

    // Save session before exit (skip if user chose to discard)
    if app.exit_discard_quit {
        // Discard unsaved file changes but preserve settings
        for tab in &mut app.tabs {
            if tab.kind == crate::app::TabKind::File && tab.has_unsaved_changes() {
                if let Some(path) = tab.editor.file_path.clone() {
                    if path.exists() {
                        let _ = tab.editor.open_file(&path); // Error OK: file may not be loadable
                    }
                }
            }
        }
        for group in &mut app.groups {
            for tab in &mut group.tabs {
                if tab.kind == crate::app::TabKind::File && tab.has_unsaved_changes() {
                    if let Some(path) = tab.editor.file_path.clone() {
                        if path.exists() {
                            let _ = tab.editor.open_file(&path); // Error OK: file may not be loadable
                        }
                    }
                }
            }
        }
    } else {
        // Normal exit - save everything as-is
    }
    app.save_settings();
    // Keep & quit already committed its recovery on a worker, before allowing
    // the UI to close. Do not write the same large buffers a second time here.
    if !app.exit_keep_unsaved_quit || !app.exit_recovery.saved { app.save_session(); }
    app.lsp_manager.shutdown_all();

    #[cfg(unix)]
    drop(keyboard_protocol);
    drop(screen_cleanup);

    if let Err(err) = res {
        eprintln!("Error: {err}");
    }

    Ok(())
}

// Startup can now be cancelled before an App exists. Restore the terminal on
// every early return/error as well as the ordinary editor shutdown path.
struct ScreenCleanup;
impl Drop for ScreenCleanup {
    fn drop(&mut self) {
        #[cfg(unix)]
        unix_input::restore_keyboard_protocol();
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture,
            crossterm::style::Print("\x1b[>0s"), DisableBracketedPaste, DisableFocusChange,
            crossterm::cursor::SetCursorStyle::DefaultUserShape, crossterm::cursor::Show,
            crossterm::terminal::SetTitle(""));
    }
}

#[cfg(test)]
mod basic_usage_performance_tests;
