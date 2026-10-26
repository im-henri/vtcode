use crate::app::App;
use std::path::PathBuf;

impl App {
    pub(crate) fn open_syntax_install_prompt(&mut self) {
        self.open_theme_install_prompt();
        self.palette.install_theme_mode = false;
        self.palette.install_syntax_mode = true;
        self.palette.syntax_install_generation =
            self.palette.syntax_install_generation.wrapping_add(1);
        self.modal = Default::default();
    }

    pub(crate) fn start_syntax_install(&mut self) {
        self.start_syntax_install_in(crate::portable::data_dir().join("syntaxes"));
    }

    fn start_syntax_install_in(&mut self, root: PathBuf) {
        if self.palette.syntax_install_running {
            return;
        }
        let source =
            match crate::syntax_install::source_path(&self.palette.query, &self.explorer.root) {
                Ok(source) => source,
                Err(error) => {
                    self.palette.theme_install_error = Some(error);
                    return;
                }
            };
        self.palette.syntax_install_running = true;
        self.palette.theme_install_error = None;
        let generation = self.palette.syntax_install_generation;
        let (syntaxes, _) = self.highlighter.shared_sets();
        let sender = self.background.sender.clone();
        let activity = self
            .background
            .activity
            .start("Installing syntax", Some(&source));
        if self
            .background
            .maintenance_sender()
            .send(Box::new(move || {
                let _activity = activity.running();
                let result = crate::syntax_install::install(&source, &root, &syntaxes);
                let _ = sender.send(crate::background::BackgroundResult::SyntaxInstalled {
                    generation,
                    result,
                });
            }))
            .is_err()
        {
            self.palette.syntax_install_running = false;
            self.palette.theme_install_error =
                Some("Syntax installer worker is unavailable.".into());
        }
    }

    pub(crate) fn finish_syntax_install(
        &mut self,
        generation: u64,
        result: Result<crate::syntax_install::InstalledSyntax, String>,
    ) {
        self.palette.syntax_install_running = false;
        let current = self.palette.active
            && self.palette.install_syntax_mode
            && self.palette.syntax_install_generation == generation;
        match result {
            Ok(installed) => {
                if current {
                    self.close_palette();
                }
                self.request_syntax_assets_async();
                self.notify(format!(
                    "Installed syntax: {}. Reloading highlighting…",
                    installed.name
                ));
            }
            Err(error) => {
                if current {
                    self.palette.theme_install_error = Some(error);
                } else {
                    self.notify_error(format!("Syntax installation failed: {error}"));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "syntax_install_tests.rs"]
mod tests;
