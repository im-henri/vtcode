#[macro_use]
mod logging;
mod animation_config;
mod animation_pacing;
mod animation_scope;
mod animation_structural_scope;
#[cfg(test)]
mod animation_ui_tests;
mod animations;
mod app;
mod app_acp;
mod app_chat_files;
mod app_chat_terminals;
mod app_commands;
mod app_commands_editor;
mod app_commands_git;
mod app_commands_settings;
mod app_commands_view;
mod app_context_menus;
mod app_deleted_files;
mod app_desks;
mod app_editor_path_links;
mod app_event_loop;
mod app_exit_backup;
mod app_explorer;
mod app_explorer_hover;
mod app_extension_catalog;
mod app_extension_install;
mod app_extensions;
mod app_external_open;
mod app_formatters;
mod app_git;
mod app_git_branch;
mod app_git_display;
mod app_global_search;
mod app_language_detection;
mod app_layout;
mod app_lsp_config;
mod app_lsp_edits;
mod app_lsp_format;
mod app_lsp_interactive;
mod app_lsp_links;
mod app_lsp_poll;
mod app_lsp_workspace_edit;
mod app_menu;
mod app_navigation;
mod app_open_file_path;
mod app_paste;
mod app_remote_connection;
mod app_remote_navigation;
mod app_remote_targets;
mod app_save_conflict;
mod app_search;
mod app_search_algo;
mod app_search_fields;
mod app_session;
mod app_settings_navigation;
mod app_settings_ui;
mod app_shortcuts;
mod app_sidebar;
mod app_source_control;
mod app_source_control_hover;
mod app_syntax_install;
mod app_tab_close;
mod app_tab_windows;
mod app_tabs;
mod app_terminal;
mod app_terminal_find;
mod app_terminal_links;
mod app_terminal_selection;
mod app_text_fields;
mod app_theme;
mod app_theme_install;
mod app_theme_loader;
mod app_theme_management;
mod app_ui_state;
mod app_vtcomplete;
mod app_vtcomplete_inline;
mod app_vtcomplete_settings;
mod app_workspace_events;
#[cfg(test)]
mod app_workspace_session_tests;
mod app_workspaces;
mod app_wrap;
mod background;
#[cfg(test)]
mod basic_file_safety_tests;
#[cfg(test)]
mod basic_operations_audit_tests;
mod builtin_themes;
mod cli;
mod clipboard;
mod code_path_links;
mod commit_details;
mod commit_input;
mod commit_review;
mod completion_activity;
mod completion_context;
mod completion_examples;
mod completion_history;
#[path = "vtcomplete_identifier.rs"]
mod completion_match;
mod completion_source;
mod copy_animation;
mod cursor_fire;
mod cursor_smear;
mod cursor_smear_raster;
mod diff_algorithm;
mod diff_context;
mod diff_scope_source;
mod diff_selection;
mod diff_tabs;
mod diff_wrap;
mod editor;
mod editor_ai_review;
mod editor_blame;
mod editor_columns;
mod editor_comments;
mod editor_conditional_folds;
mod editor_cursor;
mod editor_cursor_brackets;
mod editor_document;
mod editor_line_ops;
mod editor_multicursor;
mod editor_multicursor_ops;
mod editor_paste;
mod editor_selection;
mod editor_snippets;
mod editor_text_ops;
mod editor_transform;
mod editor_undo;
mod editor_view_state;
mod editor_wrap;
#[cfg(test)]
mod editorconfig;
mod explorer;
mod explorer_find;
mod extension_buffer;
mod extension_catalog;
mod extension_documents;
mod extension_editor_host;
#[cfg(test)]
mod extension_engineer_tests;
mod extension_handlers;
#[cfg(test)]
mod extension_hex_tests;
mod extension_host;
mod extension_install;
mod extension_lifecycle;
mod extension_manifest;
mod extension_module_cache;
mod extension_recovery;
mod extension_release_source;
mod extension_scheduler;
mod extension_state;
mod extension_surface;
#[cfg(test)]
mod extension_tabular_tests;
mod extension_versions;
mod extension_views;
mod extension_wasm;
#[cfg(test)]
mod extension_waveform_tests;
mod extensions;
#[cfg(test)]
mod extensions_ui_tests;
mod external_open;
mod file_associations;
mod file_deletion;
mod file_icons;
mod file_order;
mod file_search;
mod filesystem_roots;
mod fire_raster;
mod folder_browser;
mod formatters;
mod fs_ops;
mod fuzzy_score;
mod git_amend;
mod git_blame;
#[cfg(test)]
mod git_config_error_tests;
mod git_diff_sources;
mod git_history;
mod git_ignore;
mod git_operation;
mod git_output;
mod git_repositories;
mod git_status;
mod git_tasks;
mod git_trust;
mod helpers;
mod highlight;
mod hunk_preview;
mod inline_diff;
mod input;
mod input_dialogs;
mod input_keyboard;
mod input_keyboard_dialogs;
mod input_pickers;
mod input_terminal;
#[cfg(test)]
mod keyboard_protocol_tests;
mod language_detection;
mod last_workspaces;
mod line_changes;
mod lsp;
mod lsp_events;
mod lsp_parsing;
mod lsp_positions;
mod lsp_requests;
mod marketplace_http;
mod merge_conflict_ui;
mod merge_conflicts;
mod mouse;
mod mouse_dialogs;
mod mouse_drag;
mod mouse_overlay;
mod mouse_scroll;
mod mouse_selection;
mod native_paths;
mod path_links;
mod performance;
mod portable;
mod process_usage;
mod remote;
mod remote_targets;
mod rope_changes;
mod rope_line_blocks;
mod scope_bubble_config;
mod scope_selector;
mod screen_transition;
mod search_history;
mod search_paths;
mod search_regex;
mod search_replace;
mod search_results;
mod search_walk;
mod semantic_theme;
mod session;
mod settings;
mod settings_layout;
mod shortcut_context;
mod smooth_scroll;
mod source_control_rows;
mod source_symbols;
mod startup;
mod syntax_assets;
mod syntax_install;
mod tab_collection;
mod tab_indicator_motion;
mod tab_windows;
mod terminal;
mod terminal_completion;
mod terminal_input_activity;
#[cfg(any(windows, test))]
mod terminal_input_batch;
mod terminal_output;
mod terminal_scrollbar;
mod terminal_shell;
mod terminal_sidebar;
mod text_input;
mod text_wrap;
mod textmate;
mod textmate_backend;
mod textmate_grammar;
#[cfg(test)]
mod textmate_tests;
mod textmate_theme;
#[cfg(test)]
mod theme_audit_tests;
mod theme_catalog;
#[cfg(test)]
mod theme_coverage_tests;
mod theme_customizations;
mod theme_download;
mod theme_install;
#[cfg(test)]
mod theme_visibility_tests;
mod theme_workbench;
mod ui;
mod ui_agent_settings;
mod ui_animation_chrome;
mod ui_animation_frame;
mod ui_chat;
mod ui_chrome;
mod ui_chrome_tabs;
mod ui_colors;
mod ui_commit_details;
#[cfg(test)]
mod ui_control_visibility_tests;
mod ui_desks;
mod ui_dialogs;
#[cfg(test)]
mod ui_diff_animation_tests;
mod ui_diff_animations;
mod ui_diff_display;
mod ui_editor;
mod ui_editor_animations;
mod ui_editor_folds;
mod ui_editor_highlight;
mod ui_editor_minimap;
mod ui_empty_folder;
mod ui_exclusion_settings;
mod ui_explorer_find;
mod ui_extension_info;
mod ui_extension_versions;
mod ui_extensions;
mod ui_file_associations;
mod ui_file_deletion;
mod ui_folder_browser;
mod ui_formatter_settings;
mod ui_git_amend;
mod ui_git_branches;
mod ui_git_operation;
mod ui_helpers;
mod ui_hunk_peek;
mod ui_inline_blame;
mod ui_loading;
mod ui_logs;
mod ui_lsp_completion;
mod ui_lsp_settings;
mod ui_markdown;
mod ui_minimap_resize;
mod ui_modal;
mod ui_panels;
mod ui_performance;
mod ui_performance_details;
mod ui_pickers;
mod ui_quick_picker;
mod ui_remote_connection;
mod ui_remote_targets;
mod ui_row;
mod ui_scope_bubble_settings;
mod ui_search_controls;
mod ui_search_dialog;
mod ui_search_panel;
mod ui_search_preview;
mod ui_settings;
#[cfg(test)]
mod ui_settings_redesign_tests;
#[cfg(test)]
mod ui_settings_scroll_tests;
mod ui_settings_shortcuts;
mod ui_side_notice;
mod ui_source_control;
#[cfg(test)]
#[path = "ui_state_interaction_tests.rs"]
mod ui_state_interaction_tests;
mod ui_status;
mod ui_terminal_rows;
mod ui_text_selection;
mod ui_welcome;
mod ui_widgets;
mod ui_work;
mod ui_workspace_picker;
mod ui_workspace_sets;
mod ui_workspaces;
#[cfg(unix)]
mod unix_input;
mod vim;
mod vscode_color_registry;
#[cfg(test)]
mod vscode_color_render_tests;
#[cfg(test)]
mod vscode_search_tests;
mod vscode_theme;
mod vtcomplete_inline;
#[cfg(windows)]
mod windows_input;
#[cfg(windows)]
mod windows_mouse;
mod worker_priority;
#[cfg(test)]
mod workspace_exclusion_tests;
mod workspace_replace;
mod workspace_sets;
mod workspace_watch;

use std::io;
use std::path::{Path, PathBuf};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

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
    // libgit2's strict object creation re-reads the header of every object a
    // new tree or commit references. Committing beside 3,000 changed files
    // opened every loose blob first: write_tree took 14.5 s instead of 8 ms.
    // vtcode writes trees only from index entries and objects it has just
    // written, so the check repeats work (Git's own write-tree only checks
    // that objects exist). This covers the workbench and headless service.
    git2::opts::strict_object_creation(false);
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
        cli::Command::Headless(root) => return remote::service::run(root.as_deref()),
        cli::Command::DetachedWindow(endpoint) => return tab_windows::run_frontend(endpoint),
        #[cfg(windows)]
        cli::Command::ConsoleHost(args) => {
            std::process::exit(remote::launch::run_host(&args)?);
        }
        cli::Command::InstallTheme(source) => {
            let root = portable::theme_extensions_dir();
            let themes = theme_install::install(&source, &root).map_err(io::Error::other)?;
            for theme in &themes {
                println!("Installed: {} ({})", theme.name, theme.config_key);
            }
            println!(
                "Select a theme with Preferences: Color Theme. Use Preferences: Reload Color Themes in an already running vtcode."
            );
            return Ok(());
        }
        cli::Command::Editor(path) => (path, None),
        cli::Command::Remote { program, args } => {
            (None, Some(remote::launch::Launch::new(program, args)))
        }
        cli::Command::RemoteTarget(name) => {
            let store =
                remote_targets::Store::load(&remote_targets::path()).map_err(io::Error::other)?;
            (
                None,
                Some(
                    store
                        .find(&name)
                        .map_err(io::Error::other)?
                        .launch()
                        .map_err(io::Error::other)?,
                ),
            )
        }
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

    let mut stdout = io::stdout();
    let screen = TerminalScreen::enter(&mut stdout)?;
    let backend = terminal_output::AnsiBackend::new(terminal_output::buffered_output(stdout));
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
        let Some(loaded) = setup_loader.wait(&mut terminal, &mut events)? else {
            return Ok(());
        };
        let (setup, colors) = loaded?;
        let Some(workspace) = setup.wait(&mut terminal, &mut events, &colors)? else {
            return Ok(());
        };
        Some(workspace)
    } else {
        None
    };
    let remote_root = remote_workspace
        .as_ref()
        .map(|remote| remote.view_root.clone());
    // Without a path, reopen the folders this scope had open last time. A
    // remote service says whether it was started without a directory.
    let restore_scope = match &remote_workspace {
        Some(remote) => last_workspaces::remote_scope(remote),
        None => raw_arg
            .as_ref()
            .is_none_or(|arg| arg.is_empty())
            .then_some(workspace_sets::Scope::Local),
    };
    let loader = startup::Loader::start(wake.clone(), move || {
        let prepared = match remote_root {
            Some(root) => startup::Prepared::load_remote(root),
            None => startup::Prepared::load(raw_arg)?,
        };
        let last =
            restore_scope.and_then(|scope| last_workspaces::load(&last_workspaces::path(), &scope));
        io::Result::Ok((prepared, last))
    })?;
    let Some(prepared) = loader.wait(&mut terminal, &mut events)? else {
        startup_timings.mark("cancelled");
        startup_timings.finish();
        return Ok(());
    };
    startup_timings.mark("manifest_ready");
    let (prepared, last) = prepared?;
    let mut app = prepared.into_app_with_remote(wake, remote_workspace);
    if let Some(last) = last {
        app.restore_last_workspaces(last);
    }
    startup_timings.mark("workbench_ready");
    let res = app_event_loop::run_app(&mut terminal, &mut app, events, Some(startup_timings));

    app.save_settings();
    app.flush_last_workspaces();
    app.finish_workspaces_on_shutdown();

    drop(screen);

    if let Err(err) = res {
        eprintln!("Error: {err}");
    }

    Ok(())
}

struct TerminalScreen {
    #[cfg(unix)]
    _keyboard_protocol: unix_input::KeyboardProtocol,
    _cleanup: ScreenCleanup,
    #[cfg(windows)]
    _console_input: windows_input::ConsoleInputMode,
}

impl TerminalScreen {
    fn enter(stdout: &mut io::Stdout) -> io::Result<Self> {
        #[cfg(windows)]
        let console_input = windows_input::ConsoleInputMode::save()?;
        let cleanup = ScreenCleanup;
        enable_raw_mode()?;
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            crossterm::style::Print("\x1b[>1s"), // XTSHIFTESCAPE
            EnableBracketedPaste,
            EnableFocusChange
        )?;
        #[cfg(unix)]
        let keyboard_protocol = unix_input::KeyboardProtocol::enable(stdout)?;
        // Mouse capture replaces the Windows console input mode.
        #[cfg(windows)]
        {
            console_input.use_alternate_screen_coordinates();
            console_input.enable_virtual_terminal_input()?;
        }
        Ok(Self {
            #[cfg(unix)]
            _keyboard_protocol: keyboard_protocol,
            _cleanup: cleanup,
            #[cfg(windows)]
            _console_input: console_input,
        })
    }
}

// Startup can now be cancelled before an App exists. Restore the terminal on
// every early return/error as well as the ordinary editor shutdown path.
struct ScreenCleanup;
impl Drop for ScreenCleanup {
    fn drop(&mut self) {
        #[cfg(unix)]
        unix_input::restore_keyboard_protocol();
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::style::Print("\x1b[>0s"),
            DisableBracketedPaste,
            DisableFocusChange,
            crossterm::cursor::SetCursorStyle::DefaultUserShape,
            crossterm::cursor::Show,
            crossterm::terminal::SetTitle("")
        );
    }
}

#[cfg(test)]
mod basic_usage_performance_tests;
