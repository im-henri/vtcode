//! Source Control's current operation replaces ordinary commit controls.
use crate::{
    app::{App, TabKind},
    git_operation::{Action, Kind},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Start,
    Skip,
    Abort,
}

#[derive(Clone)]
pub(crate) struct Request {
    pub root: PathBuf,
    pub kind: Kind,
    pub choice: Choice,
    pub token: String,
}

#[derive(Default)]
pub(crate) struct Ui {
    pub pending: bool,
    pub request: Option<Request>,
    pub target: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub focus: Option<usize>,
    pub buttons: Vec<(Rect, usize, PathBuf, String)>,
}

impl App {
    pub(crate) fn current_git_operation(&self) -> Option<&crate::git_operation::State> {
        self.repositories
            .entries
            .iter()
            .find(|repo| self.current_repository_matches(&repo.root))
            .and_then(|repo| repo.operation.as_ref())
    }

    fn git_operation_unsaved_count(&self) -> usize {
        let root = self.source_control_root();
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == TabKind::File && tab.editor.modified)
            .filter_map(|tab| tab.editor.file_path.as_ref())
            .filter(|path| {
                crate::native_paths::strip_prefix(path, root).is_some()
                    && !self.repositories.entries.iter().any(|repo| {
                        repo.root != root
                            && crate::native_paths::strip_prefix(&repo.root, root).is_some()
                            && crate::native_paths::strip_prefix(path, &repo.root).is_some()
                    })
            })
            .count()
    }

    pub(crate) fn git_operation_blocker(&self) -> Option<String> {
        if !self.source_control_enabled
            || self.source_control_repository_state
                != crate::app::SourceControlRepositoryState::Available
        {
            return Some("Refresh Source Control to check this operation.".into());
        }
        if self.bg_git_action_running || self.bg_git_remote_running {
            return Some("Git operation running…".into());
        }
        if self.bg_git_status_running || self.bg_git_status_pending {
            return Some("Checking repository state…".into());
        }
        let count = self.git_operation_unsaved_count();
        if count > 0 {
            return Some(format!(
                "Save {count} edited file{} before continuing.",
                if count == 1 { "" } else { "s" }
            ));
        }
        self.current_git_operation()
            .and_then(|state| state.blocker.clone())
    }

    pub(crate) fn start_git_operation_dialog(&mut self, kind: Kind) {
        if !self.source_control_enabled {
            self.notify("Enable Source Control first.");
            return;
        }
        if self.bg_git_action_running || self.bg_git_remote_running {
            self.notify("Wait for the current Git operation to finish.");
            return;
        }
        if self.current_git_operation().is_some() {
            self.notify("Continue or abort the current Git operation first.");
            return;
        }
        if self.sidebar_view != crate::app::SidebarView::SourceControl || !self.sidebar_is_visible()
        {
            self.execute_palette_command("source_control");
        }
        self.active_pane = crate::app::ActivePane::Explorer;
        let root = self.source_control_root().to_owned();
        self.git_operation_ui.target = if kind == Kind::CherryPick && !self.sc_graph_collapsed {
            self.git_log
                .get(self.git_log_selected)
                .map(|commit| commit.full_hash.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.git_operation_ui.cursor = self.git_operation_ui.target.len();
        self.git_operation_ui.anchor = None;
        self.git_operation_ui.request = Some(Request {
            root,
            kind,
            choice: Choice::Start,
            token: String::new(),
        });
        self.source_control_input_mode = false;
        self.git_operation_ui.pending = false;
        self.modal = Default::default();
        self.clear_drag_states();
    }

    pub(crate) fn continue_git_operation(&mut self) {
        let Some(state) = self.current_git_operation().cloned() else {
            self.notify("No rebase or cherry-pick is in progress.");
            return;
        };
        if let Some(reason) = self.git_operation_blocker() {
            self.notify(reason);
            return;
        }
        self.queue_git_task(
            "git-operation",
            state.kind.command(),
            crate::git_tasks::Task::Operation(Action::Continue {
                kind: state.kind,
                token: state.token,
            }),
        );
    }

    pub(crate) fn confirm_git_operation_action(&mut self, choice: Choice) {
        let Some(state) = self.current_git_operation().cloned() else {
            return;
        };
        if self.bg_git_action_running || self.bg_git_remote_running {
            return;
        }
        self.git_operation_ui.request = Some(Request {
            root: self.source_control_root().to_owned(),
            kind: state.kind,
            choice,
            token: state.token,
        });
        self.source_control_input_mode = false;
        self.modal = Default::default();
        self.clear_drag_states();
    }

    pub(crate) fn submit_git_operation_dialog(&mut self) {
        let Some(request) = self.git_operation_ui.request.clone() else {
            return;
        };
        if !self.current_repository_matches(&request.root) {
            self.modal.error = Some(
                "The selected repository changed. Close this dialog and review Source Control."
                    .into(),
            );
            return;
        }
        if self.git_operation_unsaved_count() > 0 {
            self.modal.error = Some("Save or close edited files in this repository first.".into());
            return;
        }
        let action = match request.choice {
            Choice::Start => {
                let target = self.git_operation_ui.target.trim();
                if target.is_empty() {
                    self.modal.error = Some("Enter a branch, tag, or commit hash.".into());
                    return;
                }
                Action::Start {
                    kind: request.kind,
                    target: target.to_owned(),
                }
            }
            Choice::Skip => Action::Skip {
                kind: request.kind,
                token: request.token,
            },
            Choice::Abort => Action::Abort {
                kind: request.kind,
                token: request.token,
            },
        };
        self.queue_git_task(
            "git-operation",
            request.kind.command(),
            crate::git_tasks::Task::Operation(action),
        );
        if self.bg_git_action_running {
            self.git_operation_ui.pending = true;
        }
    }
}

pub(crate) fn primary(app: &App) -> &'static str {
    if app.git_operation_ui.pending {
        return "Running…";
    }
    match app
        .git_operation_ui
        .request
        .as_ref()
        .map(|request| (request.kind, request.choice))
    {
        Some((Kind::Rebase, Choice::Start)) => "Start Rebase",
        Some((Kind::CherryPick, Choice::Start)) => "Cherry-pick Commit",
        Some((_, Choice::Skip)) => "Skip Commit",
        Some((kind, _)) => kind.abort_label(),
        None => "Continue",
    }
}

pub(crate) fn dialog_copy(app: &App) -> (&'static str, String, Vec<String>, String, &'static str) {
    let Some(request) = &app.git_operation_ui.request else {
        return (
            "Git operation",
            String::new(),
            Vec::new(),
            String::new(),
            "",
        );
    };
    let mut context = vec![
        format!("Repository  {}", request.root.display()),
        format!("Current branch  {}", app.cached_git_branch),
    ];
    if request.choice == Choice::Start
        && request.kind == Kind::CherryPick
        && let Some(commit) = app
            .git_log
            .iter()
            .find(|commit| commit.full_hash == app.git_operation_ui.target.trim())
    {
        context.push(format!("Selected commit  {}", commit.message));
    }
    match (request.kind, request.choice) {
        (Kind::Rebase, Choice::Start) => ("Rebase Current Branch", "Replay this branch's commits on top of another branch or commit.".into(), context,
            "Commit or stash saved changes first. Git stops for conflicts; resolve, save, and stage each affected file, then use Continue Rebase in Source Control. Existing commit messages are preserved.".into(), "Base branch, tag, or commit"),
        (Kind::CherryPick, Choice::Start) => ("Cherry-pick Commit", "Apply a commit to the current branch.".into(), context,
            "Enter a commit hash or reference. Git stops for conflicts; resolve, save, and stage the affected files, then use Continue Cherry-pick. The original commit message is preserved.".into(), "Commit hash or reference"),
        (_, Choice::Skip) => ("Skip Current Commit?", format!("Skip the current commit and continue the {}?", request.kind.command()), context,
            "This discards this commit's resolution changes. Review the affected files before skipping.".into(), ""),
        _ => ("Abort Git Operation?", format!("Abort this {} and return to its starting point?", request.kind.command()), context,
            "This discards changes made while resolving this operation. Your other branches are kept.".into(), ""),
    }
}

pub(crate) fn draw_card(f: &mut Frame, app: &mut App, area: Rect) -> bool {
    let Some(state) = app.current_git_operation().cloned() else {
        return false;
    };
    app.source_control_input_mode = false;
    let bg = app.colors.bg_sidebar;
    let fg = app.colors.ui.sidebar_text_color(bg);
    let warning = app.colors.ui.warning_text_color(bg);
    let blocker = app.git_operation_blocker();
    let title = format!(
        "{}{} · {}",
        state.kind.name(),
        state
            .step
            .map_or(String::new(), |(step, total)| format!(" · {step}/{total}")),
        state.branch
    );
    if area.height > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                title,
                Style::default().fg(warning).add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().bg(bg)),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }
    if area.height > 2 {
        let help = blocker
            .clone()
            .unwrap_or_else(|| "All conflicts resolved. Ready to continue.".into());
        let lines = if area.width < 38
            || unicode_width::UnicodeWidthStr::width(help.as_str()) > area.width as usize
        {
            vec![Line::raw(help)]
        } else {
            vec![
                Line::raw(crate::ui_pickers::truncate_to_width(
                    &state.current,
                    area.width as usize,
                )),
                Line::raw(help),
            ]
        };
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(fg).bg(bg)),
            Rect::new(
                area.x,
                area.y + 1,
                area.width,
                area.height.saturating_sub(3),
            ),
        );
    }
    let busy = app.bg_git_action_running || app.bg_git_remote_running;
    let root = app.source_control_root().to_owned();
    let button =
        |f: &mut Frame, app: &mut App, rect: Rect, label: &str, index, primary, enabled| {
            if rect.is_empty() {
                return;
            }
            let focused = app.git_operation_ui.focus == Some(index);
            crate::ui_widgets::draw_action_button(
                f,
                rect,
                label,
                &app.colors,
                enabled && (primary || focused),
                rect.contains((app.mouse_col, app.mouse_row).into()),
                enabled,
            );
            app.git_operation_ui
                .buttons
                .push((rect, index, root.clone(), state.token.clone()));
        };
    if area.height >= 2 {
        let y = area.bottom() - 2;
        let gap = u16::from(area.width > 2);
        let left = area.width.saturating_sub(gap) / 2;
        button(
            f,
            app,
            Rect::new(area.x, y, left, 1),
            if area.width < 34 {
                "Skip…"
            } else {
                "Skip Commit…"
            },
            1,
            false,
            !busy,
        );
        button(
            f,
            app,
            Rect::new(area.x + left + gap, y, area.width - left - gap, 1),
            "Abort…",
            2,
            false,
            !busy,
        );
    }
    if area.height > 0 {
        button(
            f,
            app,
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
            state.kind.continue_label(),
            0,
            true,
            blocker.is_none(),
        );
    }
    true
}

fn activate(app: &mut App, index: usize) {
    match index {
        0 => app.continue_git_operation(),
        1 => app.confirm_git_operation_action(Choice::Skip),
        _ => app.confirm_git_operation_action(Choice::Abort),
    }
}

pub(crate) fn click(app: &mut App, col: u16, row: u16) -> bool {
    let hit = app
        .git_operation_ui
        .buttons
        .iter()
        .find(|(area, _, _, _)| area.contains((col, row).into()))
        .cloned();
    let Some((_, index, root, token)) = hit else {
        app.git_operation_ui.focus = None;
        return false;
    };
    if !app.current_repository_matches(&root)
        || !app
            .current_git_operation()
            .is_some_and(|state| state.token == token)
    {
        return true;
    }
    app.git_operation_ui.focus = Some(index);
    activate(app, index);
    true
}

pub(crate) fn handle_key(
    app: &mut App,
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let Some(focus) = app.git_operation_ui.focus else {
        return false;
    };
    if app.current_git_operation().is_none() {
        app.git_operation_ui.focus = None;
        return false;
    }
    match code {
        KeyCode::Enter | KeyCode::Char(' ') => activate(app, focus),
        KeyCode::Esc => app.git_operation_ui.focus = None,
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
            app.git_operation_ui.focus = Some(
                (focus
                    + if matches!(code, KeyCode::Up | KeyCode::Left) {
                        2
                    } else {
                        1
                    })
                    % 3,
            );
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let backwards = code == KeyCode::BackTab || modifiers.contains(KeyModifiers::SHIFT);
            if backwards && focus == 0 || !backwards && focus == 2 {
                app.git_operation_ui.focus = None;
                app.sc_graph_focused = backwards;
                if backwards {
                    app.sc_graph_collapsed = false;
                }
            } else {
                app.git_operation_ui.focus = Some(if backwards { focus - 1 } else { focus + 1 });
            }
        }
        _ => {}
    }
    true
}
