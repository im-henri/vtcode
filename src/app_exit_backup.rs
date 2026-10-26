//! Exit recovery and a metadata-only preview of large backups. Never scan text
//! or touch disk while opening/drawing the warning, including remote documents.
use crate::app::{App, TabKind};

pub(crate) const LARGE_FILE_BYTES: usize = 10 * 1024 * 1024;

#[derive(PartialEq, Eq)]
pub(crate) struct File {
    pub name: String,
    pub bytes: u64,
    pub history_bytes: u64,
}

#[derive(PartialEq, Eq)]
pub(crate) struct Warning {
    pub files: Vec<File>,
    pub total_bytes: u64,
}

#[derive(Default)]
pub(crate) struct RecoveryState {
    pending: Option<Pending>,
    pub saved: bool,
    pub approved: Option<Warning>,
}

struct Pending {
    generation: u64,
    session: crate::session::Session,
    documents: Vec<(u64, u64, bool)>,
}

impl RecoveryState {
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }
}

impl App {
    fn exit_recovery_documents(&self) -> Vec<(u64, u64, bool)> {
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == TabKind::File)
            .map(|tab| {
                (
                    tab.editor.save_identity,
                    tab.editor.content_version,
                    tab.editor.modified,
                )
            })
            .collect()
    }

    pub(crate) fn start_exit_recovery(&mut self) {
        if self.exit_recovery.pending() || self.exit_recovery.saved {
            return;
        }
        self.sync_duplicate_file_tabs();
        let warning = self.large_backup_warning();
        if warning.is_some() && warning != self.exit_recovery.approved {
            // A queued agent edit can grow/add a file after the quit action.
            // Never start an unapproved large write in that intervening frame.
            self.exit_backup_warning = warning;
            self.exit_keep_unsaved_quit = false;
            self.custom_exit_started = false;
            self.exit_confirm_active = true;
            self.modal = Default::default();
            return;
        }
        let session = self.build_session();
        let generation = self.workspace_generation;
        self.exit_recovery.pending = Some(Pending {
            generation,
            session: session.clone(),
            documents: self.exit_recovery_documents(),
        });
        let root = self.explorer.root.clone();
        let job_root = root.clone();
        let sender = self.background.sender.clone();
        self.background.save_for_file(&root, move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::session::save_session(&job_root, &session).map_err(|error| error.to_string())
            }))
            .unwrap_or_else(|_| Err("The recovery worker could not complete the backup".into()));
            let _ = sender.send(crate::background::BackgroundResult::ExitRecoverySaved {
                generation,
                result,
            });
        });
        self.modal = Default::default();
    }

    pub(crate) fn finish_exit_recovery(&mut self, generation: u64, result: Result<(), String>) {
        if self
            .exit_recovery
            .pending
            .as_ref()
            .is_none_or(|pending| pending.generation != generation)
        {
            return;
        }
        let pending = self.exit_recovery.pending.take().unwrap();
        let result = result.and_then(|()| {
            if generation != self.workspace_generation || pending.documents != self.exit_recovery_documents() {
                Err("Documents changed while the backup was being written; quitting was cancelled to protect your newest edits".into())
            } else { Ok(()) }
        });
        match result {
            Ok(()) => {
                self.exit_recovery.saved = true;
                self.session_baseline = Some(pending.session);
            }
            Err(error) => {
                self.exit_keep_unsaved_quit = false;
                self.custom_exit_started = false;
                self.exit_confirm_active = true;
                self.modal = Default::default();
                self.show_save_error(format!("Could not back up your unsaved files: {error}. VTCode will stay open so you can save your edits or try again."));
            }
        }
    }

    pub(crate) fn large_backup_warning(&self) -> Option<Warning> {
        let mut seen = rustc_hash::FxHashSet::default();
        let mut files = Vec::new();
        let mut total_bytes = 0u64;
        for tab in self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
        {
            let editor = &tab.editor;
            if tab.kind != TabKind::File || !editor.modified
                // A not-yet-hydrated recovery is retained, not written again.
                || (editor.bg_loading && editor.pending_session_restore.is_some())
                || !seen.insert(editor.save_identity)
            {
                continue;
            }
            let bytes = editor.rope.len_bytes() as u64;
            // A mostly deleted file can still retain a large undo version.
            // Inspect at most the 50 saved entries per stack, never their text.
            let history_bytes = [&editor.undo_stack, &editor.redo_stack]
                .into_iter()
                .map(|stack| {
                    stack
                        .read()
                        .iter()
                        .rev()
                        .take(crate::session::MAX_UNDO_ENTRIES)
                        .map(|state| state.rope.len_bytes() as u64)
                        .max()
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            let minimum_bytes = bytes.max(history_bytes);
            if minimum_bytes < LARGE_FILE_BYTES as u64 {
                continue;
            }
            let name = editor
                .file_path
                .as_ref()
                .map_or_else(
                    || editor.display_name().into_owned(),
                    |path| self.display_path(path),
                )
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            total_bytes = total_bytes.saturating_add(minimum_bytes);
            files.push(File {
                name,
                bytes,
                history_bytes,
            });
        }
        (!files.is_empty()).then_some(Warning { files, total_bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{EditorGroup, Tab},
        editor::Editor,
        ui_modal::{self, Kind},
    };
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::io::Read;

    struct Project(tempfile::TempDir);
    impl Project {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }
        fn path(&self) -> &std::path::Path {
            self.0.path()
        }
    }
    impl Drop for Project {
        fn drop(&mut self) {
            let root = crate::portable::workspace_dir(self.path());
            assert!(root.starts_with(crate::portable::data_dir().join("workspaces")));
            let _ = std::fs::remove_dir_all(root);
        }
    }

    fn large_rope() -> ropey::Rope {
        ropey::Rope::from_reader(std::io::repeat(b'x').take(LARGE_FILE_BYTES as u64)).unwrap()
    }

    fn file(rope: &ropey::Rope, path: Option<&str>, modified: bool) -> Tab {
        let mut editor = Editor::new();
        editor.rope = rope.clone();
        editor.file_path = path.map(Into::into);
        editor.modified = modified;
        Tab::new_file(editor)
    }

    fn key(app: &mut App, code: KeyCode) {
        assert!(!crate::input_keyboard::handle_keyboard_event(
            app,
            KeyEvent::new(code, KeyModifiers::NONE)
        ));
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| ui_modal::draw_active(frame, app))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn click(app: &mut App, index: usize) {
        let area = app
            .modal
            .buttons
            .iter()
            .find(|(_, button)| *button == index)
            .unwrap()
            .0;
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    #[test]
    fn preview_counts_unique_modified_documents_in_all_panes_using_buffer_bytes() {
        let mut app = App::test_new();
        let rope = large_rope();
        let mut smaller = rope.clone();
        smaller.remove(0..1);
        app.tabs = vec![
            file(&rope, Some("large.log"), true),
            file(&smaller, Some("small.log"), true),
            file(&rope, Some("saved.log"), false),
        ];
        let duplicate = app.clone_desk_tab(&app.tabs[0]);
        let mut group = EditorGroup::new();
        group.tabs = vec![duplicate, file(&rope, None, true)];
        let mut settings = file(&rope, Some("settings.json"), true);
        settings.kind = TabKind::Settings;
        group.tabs.push(settings);
        app.groups.push(group);
        let warning = app.large_backup_warning().unwrap();
        assert_eq!(
            warning.files.len(),
            2,
            "split views must not count as another backup"
        );
        assert_eq!(warning.total_bytes, 2 * LARGE_FILE_BYTES as u64);
        assert!(
            warning
                .files
                .iter()
                .any(|file| file.name.contains("large.log"))
        );
        assert!(
            warning
                .files
                .iter()
                .all(|file| file.bytes == LARGE_FILE_BYTES as u64)
        );
        app.tabs[0].editor.modified = false;
        for group in &mut app.groups {
            for tab in &mut group.tabs {
                tab.editor.modified = false;
            }
        }
        assert!(
            app.large_backup_warning().is_none(),
            "small files require no extra confirmation"
        );
    }

    #[test]
    fn a_large_retained_undo_version_warns_even_after_most_text_is_deleted() {
        let mut app = App::test_new();
        app.tabs = vec![file(&large_rope(), Some("truncated.log"), true)];
        let editor = app.editor_mut().unwrap();
        editor.save_undo_public();
        editor.rope = "small now".into();
        let warning = app.large_backup_warning().unwrap();
        assert_eq!(warning.files.len(), 1);
        assert_eq!(warning.files[0].bytes, 9);
        assert_eq!(warning.files[0].history_bytes, LARGE_FILE_BYTES as u64);
        assert_eq!(warning.total_bytes, LARGE_FILE_BYTES as u64);
    }

    #[test]
    fn keyboard_and_mouse_keep_require_a_separate_large_backup_confirmation() {
        let rope = large_rope();
        for mouse in [false, true] {
            let mut app = App::test_new();
            app.tabs = vec![file(&rope, Some("unsaved-large.log"), true)];
            app.exit_confirm_active = true;
            if mouse {
                render(&mut app, 90, 30);
                click(&mut app, 2);
            } else {
                key(&mut app, KeyCode::Char('k'));
            }
            assert_eq!(ui_modal::active(&app), Some(Kind::ExitBackup));
            assert!(!app.exit_keep_unsaved_quit && !app.custom_exit_started);
            let text = render(&mut app, 90, 30);
            for label in [
                "Back up large unsaved files?",
                "unsaved-large.log",
                "10.0 MiB",
                "Back up & quit",
                "Back",
            ] {
                assert!(text.contains(label), "missing {label}");
            }
            assert_eq!(
                app.modal.focus, 1,
                "a repeated Enter must not accidentally authorize storage"
            );
            key(&mut app, KeyCode::Char('k'));
            assert!(!app.exit_keep_unsaved_quit);
            if mouse {
                click(&mut app, 1);
            } else {
                key(&mut app, KeyCode::Enter);
            }
            assert_eq!(ui_modal::active(&app), Some(Kind::Exit));
            assert!(!app.exit_keep_unsaved_quit);
            key(&mut app, KeyCode::Char('k'));
            render(&mut app, 90, 30);
            if mouse {
                click(&mut app, 0);
            } else {
                key(&mut app, KeyCode::Tab);
                key(&mut app, KeyCode::Enter);
            }
            assert!(app.exit_keep_unsaved_quit && !app.exit_confirm_active);
            assert!(app.exit_backup_warning.is_none());
            assert!(app.editor().unwrap().rope.is_instance(&rope));
        }
    }

    #[test]
    fn large_backup_list_scrolls_without_losing_buttons_in_small_viewports() {
        for (width, height) in [(100, 35), (60, 24), (40, 18), (32, 14)] {
            let mut app = App::test_new();
            app.exit_confirm_active = true;
            app.exit_backup_warning = Some(Warning {
                files: (0..40)
                    .map(|i| File {
                        name: format!("file-{i:02}.log"),
                        bytes: LARGE_FILE_BYTES as u64,
                        history_bytes: 0,
                    })
                    .collect(),
                total_bytes: 40 * LARGE_FILE_BYTES as u64,
            });
            let text = render(&mut app, width, height);
            assert_eq!(app.modal.buttons.len(), 2, "{width}x{height}");
            assert!(
                text.contains("file-00.log"),
                "{width}x{height}: {text:?}; {:?}",
                app.modal.context_area
            );
            assert!(!text.contains("file-39.log"));
            key(&mut app, KeyCode::End);
            let text = render(&mut app, width, height);
            assert!(
                text.contains("file-39.log"),
                "last file unreachable at {width}x{height}"
            );
            key(&mut app, KeyCode::Home);
            render(&mut app, width, height);
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert!(app.modal.context_scroll > 0);
            key(&mut app, KeyCode::Esc);
            assert_eq!(ui_modal::active(&app), Some(Kind::Exit));
            assert!(!app.exit_keep_unsaved_quit);
        }
    }

    #[test]
    fn growing_files_require_updated_approval_before_any_large_backup_starts() {
        let mut app = App::test_new();
        app.tabs = vec![file(&large_rope(), Some("growing.log"), true)];
        app.exit_confirm_active = true;
        key(&mut app, KeyCode::Char('k'));
        render(&mut app, 90, 30);
        app.editor_mut().unwrap().paste_plain_with_text("more");
        click(&mut app, 0);
        assert!(!app.exit_keep_unsaved_quit && !app.exit_recovery.pending());
        assert_eq!(
            app.exit_backup_warning.as_ref().unwrap().files[0].bytes,
            LARGE_FILE_BYTES as u64 + 4
        );
        assert!(app.modal.error.as_ref().unwrap().contains("Files changed"));
        render(&mut app, 90, 30);
        click(&mut app, 0);
        assert!(app.exit_keep_unsaved_quit);
        // A second queued edit can arrive between approval and the next loop.
        app.editor_mut().unwrap().paste_plain_with_text("again");
        app.start_exit_recovery();
        assert!(!app.exit_keep_unsaved_quit && !app.exit_recovery.pending());
        assert_eq!(ui_modal::active(&app), Some(Kind::ExitBackup));
        assert_eq!(
            app.exit_backup_warning.as_ref().unwrap().files[0].bytes,
            LARGE_FILE_BYTES as u64 + 9
        );
    }

    #[test]
    fn exit_recovery_is_background_durable_and_not_written_again_on_completion() {
        let root = Project::new();
        let mut app = App::new(root.path().to_path_buf());
        app.tabs = vec![Tab::new_file(Editor::test_with_text("unsaved local edits"))];
        app.tabs[0].editor.modified = true;
        app.exit_keep_unsaved_quit = true;
        app.start_exit_recovery();
        assert_eq!(ui_modal::active(&app), Some(Kind::ExitRecovery));
        assert!(!app.exit_recovery.saved);
        key(&mut app, KeyCode::Esc);
        assert!(app.exit_recovery.pending());
        app.wait_for_background(std::time::Duration::from_secs(10), |app| {
            !app.exit_recovery.pending()
        });
        assert!(app.exit_recovery.saved && app.exit_keep_unsaved_quit);
        let original = std::fs::read(crate::session::session_path(root.path())).unwrap();
        let mut saved = crate::session::load_session(root.path())
            .unwrap()
            .tabs
            .remove(0);
        crate::session::hydrate_saved_tab(root.path(), &mut saved);
        assert_eq!(
            saved.pending_unsaved_content.as_ref().unwrap(),
            "unsaved local edits"
        );
        app.start_exit_recovery();
        assert!(!app.exit_recovery.pending());
        assert_eq!(
            std::fs::read(crate::session::session_path(root.path())).unwrap(),
            original
        );
    }

    #[test]
    fn failed_exit_recovery_keeps_edits_open_and_reports_the_error() {
        let root = Project::new();
        let mut app = App::new(root.path().to_path_buf());
        app.tabs = vec![Tab::new_file(Editor::test_with_text("irreplaceable edits"))];
        app.tabs[0].editor.modified = true;
        let original = app.tabs[0].editor.rope.clone();
        // A directory at the manifest path fails the final commit after the
        // sidecars have been written, just as a full/unwritable disk can fail.
        std::fs::create_dir_all(crate::session::session_path(root.path())).unwrap();
        app.exit_keep_unsaved_quit = true;
        app.start_exit_recovery();
        app.wait_for_background(std::time::Duration::from_secs(10), |app| {
            !app.exit_recovery.pending()
        });
        assert!(!app.exit_recovery.saved && !app.exit_keep_unsaved_quit);
        assert!(app.exit_confirm_active && app.save_error.is_some());
        assert!(!app.shutdown_anim_active);
        assert!(
            app.editor().unwrap().modified && app.editor().unwrap().rope.is_instance(&original)
        );
        assert!(
            app.save_error
                .as_ref()
                .unwrap()
                .contains("VTCode will stay open")
        );
    }

    #[test]
    fn edits_arriving_during_exit_recovery_cancel_quit_and_retain_the_newest_text() {
        let root = Project::new();
        let mut app = App::new(root.path().to_path_buf());
        app.tabs = vec![Tab::new_file(Editor::test_with_text("original edits"))];
        app.tabs[0].editor.modified = true;
        app.exit_keep_unsaved_quit = true;
        app.start_exit_recovery();
        // A worker/agent can change text even while normal input is blocked.
        app.editor_mut().unwrap().paste_plain_with_text("newer ");
        let newest = app.editor().unwrap().rope.clone();
        app.wait_for_background(std::time::Duration::from_secs(10), |app| {
            !app.exit_recovery.pending()
        });
        assert!(!app.exit_recovery.saved && !app.exit_keep_unsaved_quit);
        assert!(app.editor().unwrap().rope.is_instance(&newest));
        assert!(
            app.save_error
                .as_ref()
                .unwrap()
                .contains("Documents changed")
        );
    }
}
