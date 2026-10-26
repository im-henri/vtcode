use crate::app::App;

impl App {
    /// Dispatch git-related commands.
    pub(crate) fn execute_git_command(&mut self, id: &str) -> bool {
        if let Some(action) = id.strip_prefix("merge_conflict_") {
            self.merge_action(action, None);
            return true;
        }
        if id == "source_control_toggle" {
            self.toggle_workspace_source_control();
            return true;
        }
        if !self.source_control_enabled && matches!(id,
            "git_refresh" | "git_filter_changes" | "git_show_untracked" | "git_toggle_blame" | "git_stage"
            | "git_unstage" | "git_stage_all" | "git_unstage_all" | "git_discard_all"
            | "git_commit" | "git_stash" | "git_stash_pop" | "git_fetch" | "git_pull"
            | "git_push" | "git_checkout") {
            self.notify("Source Control is disabled. Enable it in Source Control or Settings.");
            return true;
        }
        match id {
            "git_open_parent" => self.open_parent_repository(),
            "git_open_repository" => self.open_repository_picker(),
            "git_reopen_repository" => self.reopen_selected_repository(),
            "git_close_repository" => {
                let root = self.source_control_root().to_owned();
                self.close_source_control_repository(&root);
            }
            "git_reopen_repositories" => self.reopen_source_control_repositories(),
            "git_refresh" => {
                self.repositories.rescan = true;
                self.refresh_source_control_async();
                self.refresh_git_log_async();
                self.refresh_git_branch_async();
            }
            "git_filter_changes" => {
                if self.sidebar_view != crate::app::SidebarView::SourceControl || !self.show_explorer {
                    self.execute_palette_command("source_control");
                }
                self.active_pane = crate::app::ActivePane::Explorer;
                self.source_control_input_mode = false;
                let area = self.source_control_actions_area.or(self.explorer_area).unwrap_or_default();
                self.open_source_control_filter_menu(area.x, area.y.saturating_add(1));
            }
            "git_show_untracked" => {
                let actions = self.source_control_action_menu_items();
                let selected = self.context_menu_selected;
                // Follow a workspace override; otherwise remember the User
                // preference across folders, file launches and SSH workspaces.
                let scope = if self.settings_workspace.source_control_show_untracked.is_some() {
                    crate::settings::SettingsScope::Workspace
                } else {
                    crate::settings::SettingsScope::Global
                };
                self.save_source_control_show_untracked(scope, !self.sc_visibility.untracked);
                // Keep the menu open so the checkbox reflects the new state.
                let (x, y) = self.context_menu_pos;
                if actions {
                    self.open_source_control_action_menu(x, y);
                    self.context_menu_selected = selected.min(self.context_menu_items.len().saturating_sub(1));
                } else {
                    self.open_source_control_filter_menu(x, y);
                }
            }
            "git_toggle_blame" => crate::editor_blame::toggle(self),
            "git_stage" => {
                self.git_stage_changes(self.selected_source_control_changes());
            }
            "git_unstage" => {
                self.git_unstage_changes(self.selected_source_control_changes());
            }
            "git_stage_all" => {
                self.git_stage_all();
            }
            "git_unstage_all" => {
                self.git_unstage_all();
            }
            "git_discard_all" => self.request_source_control_discard_all(),
            "git_commit" => {
                self.git_commit();
            }
            "git_stash" => {
                self.git_stash_save();
            }
            "git_stash_pop" => {
                self.git_stash_pop();
            }
            "git_fetch" => {
                self.git_fetch_async();
            }
            "git_pull" => {
                self.git_pull_async();
            }
            "git_push" => {
                self.git_push_async();
            }
            "git_checkout" => {
                self.open_git_branch_picker();
            }
            _ => return false,
        }
        true
    }
}
