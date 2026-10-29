//! Paint before touching workspace/configuration files. Only the small launch
//! manifest is needed to construct the workbench; optional assets use its queues.
use std::{ffi::OsString, io, path::PathBuf, sync::mpsc, time::Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::Backend};

use crate::{app::App, app_event_loop::TerminalEventPump, background::UiWake, settings::Settings};

pub(crate) struct Target {
    pub root: PathBuf,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

impl Target {
    /// Metadata can block on a network drive. Resolve the CLI on the loader,
    /// including literal colon-containing filenames before file:line:column.
    pub(crate) fn resolve(raw: Option<OsString>) -> Self {
        let mut line = None;
        let mut column = None;
        let mut path = raw.as_ref().map(PathBuf::from);
        let mut metadata = path.as_ref().and_then(|path| std::fs::metadata(path).ok());
        if metadata.is_none()
            && let Some(raw) = raw.as_ref().and_then(|arg| arg.to_str())
        {
            if let Some((prefix, number)) = raw.rsplit_once(':')
                && let Ok(number) = number.parse::<usize>()
            {
                if let Some((file, row)) = prefix.rsplit_once(':')
                    && let Ok(row) = row.parse::<usize>()
                    && let Ok(found) = std::fs::metadata(file)
                {
                    path = Some(file.into());
                    line = Some(row);
                    column = Some(number);
                    metadata = Some(found);
                } else if let Ok(found) = std::fs::metadata(prefix) {
                    path = Some(prefix.into());
                    line = Some(number);
                    metadata = Some(found);
                }
            }
        }
        let (root, file) = match path {
            Some(path) if metadata.is_some_and(|meta| meta.is_dir()) => (path, None),
            Some(path) => (crate::startup_file_directory(&path), Some(path)),
            None => (
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                None,
            ),
        };
        Self {
            root,
            file,
            line,
            column,
        }
    }
}

pub(crate) struct Prepared {
    pub target: Target,
    pub global: Settings,
    pub workspace: Settings,
    pub session: Option<crate::session::Session>,
}

/// Used on the launch worker before a remote workspace is available. Resolve
/// just the saved global palette, including installed themes and overrides.
pub(crate) fn connection_colors(settings: &Settings, portable_base: &std::path::Path) -> crate::app_theme::ThemeColors {
    let key = settings.theme.as_deref().unwrap_or("dark");
    let theme = crate::app_theme_loader::builtin_theme(key).cloned()
        .or_else(|| crate::app_theme_loader::load_installed_theme(portable_base, key))
        .unwrap_or_else(|| crate::app_theme_loader::builtin_theme("dark").unwrap().clone());
    crate::vscode_theme::customized_theme(&theme, &settings.workbench_color_customizations).to_theme_colors()
}

impl Prepared {
    pub(crate) fn load_remote(root: PathBuf) -> Self {
        let mut global = crate::settings::load_from_file(&crate::settings::global_settings_path());
        global.validate();
        let _ = crate::highlight::Highlighter::startup_theme_set();
        let workspace = crate::settings::load_from_file(&crate::settings::project_settings_path(&root));
        let session = crate::session::load_session(&root);
        Self { target: Target { root, file: None, line: None, column: None }, global, workspace, session }
    }
    pub(crate) fn load(raw: Option<OsString>) -> Self {
        let target = Target::resolve(raw);
        let mut global = crate::settings::load_from_file(&crate::settings::global_settings_path());
        let mut workspace =
            crate::settings::load_from_file(&crate::settings::project_settings_path(&target.root));
        global.validate();
        workspace.validate();
        let session = crate::session::load_session(&target.root);
        // Initialize only the shared startup palette, on this worker. Other
        // built-ins and installed themes are discovered after the workbench draws.
        let _ = crate::highlight::Highlighter::startup_theme_set();
        let selected = workspace
            .theme
            .as_ref()
            .or(global.theme.as_ref())
            .or_else(|| session.as_ref().filter(|_| target.file.is_none()).and_then(|session| session.theme.as_ref()));
        if let Some(key) = selected {
            let _ = crate::app_theme_loader::builtin_theme(key);
        }
        Self {
            target,
            global,
            workspace,
            session,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_app(self, wake: UiWake) -> App {
        self.into_app_with_remote(wake, None)
    }

    pub(crate) fn into_app_with_remote(self, wake: UiWake, remote: Option<crate::remote::workspace::Workspace>) -> App {
        let mut app = App::new_startup(self.target.root, wake);
        app.remote_workspace = remote;
        app.git_output();
        if app.remote_workspace.is_some() { app.refresh_palette_commands(); }
        app.lsp_manager.remote_workspace = app.remote_workspace.clone();
        if let Some(remote) = &app.remote_workspace { remote.client.set_wake(app.background.wake_handle()); }
        let selected = self
            .workspace
            .theme
            .as_ref()
            .or(self.global.theme.as_ref())
            .or_else(|| {
                self.session
                    .as_ref()
                    .filter(|_| self.target.file.is_none())
                    .and_then(|session| session.theme.as_ref())
            })
            .cloned()
            .unwrap_or_else(|| "dark".into());
        if !app
            .loaded_themes
            .iter()
            .any(|theme| theme.config_key == selected)
        {
            let theme = crate::app_theme_loader::builtin_theme(&selected)
                .cloned()
                .unwrap_or_else(|| {
                    // Preserve the requested identity in settings/session snapshots
                    // while its optional disk assets load. Never save the fallback
                    // palette as if the user had chosen a different theme.
                    let mut theme = crate::app_theme_loader::builtin_theme("dark")
                        .unwrap()
                        .clone();
                    theme.config_key = selected.clone();
                    theme.builtin = false;
                    theme.name = format!("Loading theme: {selected}");
                    app.startup_theme_placeholder = Some(selected.clone());
                    theme
                });
            app.loaded_themes.push(theme);
            app.refresh_palette_commands();
        }
        app.settings_global = self.global;
        app.settings_workspace = self.workspace;
        app.apply_effective_settings();
        if let Some(path) = self.target.file {
            if let Some(session) = &self.session {
                app.restore_search_histories(session);
            }
            app.open_file(&path);
            if let Some(line) = self.target.line
                && let Some(editor) = app.editor_mut()
            {
                editor.set_cursor(
                    line.saturating_sub(1),
                    self.target.column.unwrap_or(1).saturating_sub(1),
                );
            }
        } else if let Some(session) = self.session {
            app.restore_session(&session);
        }
        app.apply_session_settings();
        if app.remote_workspace.is_some() {
            app.notify("Remote workspace · editing and rendering run locally");
        }
        app.capture_session_baseline();
        app
    }
}

pub(crate) struct Loader<T> {
    receiver: mpsc::Receiver<T>,
    wake: UiWake,
}

impl<T: Send + 'static> Loader<T> {
    pub(crate) fn start(
        wake: UiWake,
        load: impl FnOnce() -> T + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let thread_wake = wake.clone();
        std::thread::Builder::new()
            .name("startup-manifest".into())
            .spawn(move || {
                // Also wake on a failed worker, so a disconnected channel cannot
                // leave the UI asleep indefinitely.
                struct Notify(UiWake);
                impl Drop for Notify {
                    fn drop(&mut self) {
                        self.0.notify();
                    }
                }
                // Tuple fields drop in order: close the channel before waking,
                // including when loading panics before sending a value.
                let completion = (sender, Notify(thread_wake));
                let _ = completion.0.send(load());
            })?;
        Ok(Self { receiver, wake })
    }

    pub(crate) fn wait<B: Backend>(
        &self,
        terminal: &mut Terminal<B>,
        events: &mut TerminalEventPump,
    ) -> io::Result<Option<T>> {
        let mut deferred = std::collections::VecDeque::new();
        loop {
            let observed = self.wake.generation();
            // Keep resize/quit live even while a configuration read is stalled.
            // Keystrokes/pastes retain order; clicks on the startup logo must
            // not accidentally activate controls in the restored layout.
            for _ in 0..64 {
                let Some(event) = events.try_recv()? else {
                    break;
                };
                if crate::terminal_output::apply_resize(terminal, &event)
                    .map_err(|error| io::Error::other(error.to_string()))?.is_some()
                {
                    terminal.draw(crate::ui_welcome::draw_bootstrap_screen)
                        .map_err(|error| io::Error::other(error.to_string()))?;
                }
                match event {
                    Event::Key(key)
                        if key.kind != KeyEventKind::Release
                            && key.modifiers == KeyModifiers::CONTROL
                            && matches!(key.code, KeyCode::Char('q' | 'c')) =>
                    {
                        return Ok(None);
                    }
                    Event::Resize(..) => {}
                    Event::Mouse(_) => {}
                    event => deferred.push_back(event),
                }
            }
            match self.receiver.try_recv() {
                Ok(ready) => {
                    events.replay_startup_input(deferred);
                    return Ok(Some(ready));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(io::Error::other("startup loader stopped"));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if !events.has_pending_event() {
                self.wake.wait_for_change(observed, None);
            }
        }
    }
}

/// Opt-in evidence for slow machines, including the point at which VTCode's
/// own entry point runs. Writing the report never holds up a frame.
pub(crate) struct Timings {
    start: Instant,
    path: Option<std::ffi::OsString>,
    marks: Vec<(&'static str, std::time::Duration)>,
}

impl Timings {
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            path: std::env::var_os("VTCODE_STARTUP_TIMINGS"),
            marks: Vec::new(),
        }
    }
    pub(crate) fn mark(&mut self, name: &'static str) {
        if self.path.is_some() {
            self.marks.push((name, self.start.elapsed()));
        }
    }
    pub(crate) fn finish(mut self) {
        let Some(path) = self.path.take() else { return };
        let _ = std::thread::Builder::new().name("startup-timings".into()).spawn(move || {
            use std::fmt::Write;
            let mut output = String::from("Milliseconds since VTCode entered main (excludes OS executable/terminal-host loading)\n");
            for (name, elapsed) in self.marks { let _ = writeln!(output, "{name}: {:.3}", elapsed.as_secs_f64() * 1000.0); }
            let _ = std::fs::write(path, output);
        });
    }
}
