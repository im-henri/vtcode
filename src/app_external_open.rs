use crate::app::App;
use crate::external_open::Request;

impl App {
    pub(crate) fn open_external(&mut self, request: Request) {
        if self.remote_workspace.is_some() {
            match &request {
                Request::Terminal(path) => { let cwd = self.explorer_target_directory(Some(path)); self.new_terminal_tab_at(&cwd); return; }
                Request::SystemApp(_) | Request::Reveal(_) => { self.notify("This file is on the remote host. Use the VTCode Explorer or a remote terminal to access it."); return; }
                Request::Url(_) => {}
            }
        }
        let sender = self.background.sender.clone();
        self.background.spawn("external-open", move || {
            let result = request
                .run()
                .map_err(|error| format!("Open failed: {error}"));
            let _ = sender.send(crate::background::BackgroundResult::ExternalOpenDone(
                result,
            ));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_external_launch_reports_back_through_the_event_loop() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::test_new();
        app.open_external(Request::Terminal(dir.path().join("missing").join("file")));
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            app.notification_history
                .iter()
                .any(|(message, _, _)| message.starts_with("Open failed:"))
        });
        assert!(app.notification_history.iter().any(|(message, level, _)| {
            message.contains("terminal directory does not exist")
                && *level == crate::app::NotifyLevel::Error
        }));
    }
}
