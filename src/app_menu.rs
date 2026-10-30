use std::borrow::Cow;
use crate::app::{App, PaletteCommand, ContextMenuItem};

const LANGUAGE_COMMAND_PREFIX: &str = "language_mode:";

pub(crate) fn language_command_id(language: &str) -> String {
    format!("{LANGUAGE_COMMAND_PREFIX}{language}")
}

pub(crate) fn language_from_command_id(id: &str) -> Option<&str> {
    id.strip_prefix(LANGUAGE_COMMAND_PREFIX).filter(|language| !language.is_empty())
}

impl App {
    pub(crate) fn build_context_menu() -> Vec<ContextMenuItem> {
        vec![
            ContextMenuItem { label: "Cut".into(), shortcut: "Ctrl+X".into(), id: "cut", separator_after: false },
            ContextMenuItem { label: "Copy".into(), shortcut: "Ctrl+C".into(), id: "copy", separator_after: false },
            ContextMenuItem { label: "Paste".into(), shortcut: "Ctrl+V".into(), id: "paste", separator_after: true },
            ContextMenuItem { label: "Select All".into(), shortcut: "Ctrl+A".into(), id: "select_all", separator_after: true },
            ContextMenuItem { label: "Select Line".into(), shortcut: "Ctrl+L".into(), id: "select_line", separator_after: false },
            ContextMenuItem { label: "Duplicate Line".into(), shortcut: "Alt+Shift+Down".into(), id: "duplicate_line", separator_after: false },
            ContextMenuItem { label: "Delete Line".into(), shortcut: "Alt+D".into(), id: "delete_line", separator_after: true },
            ContextMenuItem { label: "Toggle Comment".into(), shortcut: "Ctrl+/".into(), id: "toggle_comment", separator_after: false },
            ContextMenuItem { label: "Rename Symbol".into(), shortcut: "F2".into(), id: "rename_symbol", separator_after: true },
            ContextMenuItem { label: "Go to Definition".into(), shortcut: "F12".into(), id: "go_to_definition", separator_after: false },
            ContextMenuItem { label: "Peek Definition".into(), shortcut: "Alt+F12".into(), id: "peek_definition", separator_after: false },
            ContextMenuItem { label: "Find References".into(), shortcut: "Shift+F12".into(), id: "find_references", separator_after: true },
            ContextMenuItem { label: "Format Document".into(), shortcut: "Shift+Alt+F".into(), id: "format_document", separator_after: true },
            ContextMenuItem { label: "Command Palette".into(), shortcut: "F1".into(), id: "command_palette", separator_after: false },
        ]
    }

    pub(crate) fn build_command_list_with_themes(
        themes: &[crate::app_theme::ThemeFile],
        syntax_names: &[String],
    ) -> Vec<PaletteCommand> {
        let mut cmds = vec![
            PaletteCommand { name: "Desks: New Desk…".into(), shortcut: String::new(), id: Cow::Borrowed("desk_new") },
            PaletteCommand { name: "Desks: Switch Desk…".into(), shortcut: String::new(), id: Cow::Borrowed("desk_switch") },
            PaletteCommand { name: "Desks: Previous Desk".into(), shortcut: String::new(), id: Cow::Borrowed("desk_previous") },
            PaletteCommand { name: "Desks: Rename Desk…".into(), shortcut: String::new(), id: Cow::Borrowed("desk_rename") },
            PaletteCommand { name: "Desks: Duplicate Desk".into(), shortcut: String::new(), id: Cow::Borrowed("desk_duplicate") },
            PaletteCommand { name: "Desks: Close Desk…".into(), shortcut: String::new(), id: Cow::Borrowed("desk_close") },
            PaletteCommand { name: "Desks: Reopen Closed Desk".into(), shortcut: String::new(), id: Cow::Borrowed("desk_reopen") },
            PaletteCommand { name: "Tabs: Add to New Group…".into(), shortcut: String::new(), id: Cow::Borrowed("tab_group_new") },
            PaletteCommand { name: "Tabs: Add to Group…".into(), shortcut: String::new(), id: Cow::Borrowed("tab_group_add") },
            PaletteCommand { name: "Tabs: Remove from Group".into(), shortcut: String::new(), id: Cow::Borrowed("tab_group_remove") },
            PaletteCommand { name: "Tabs: Edit Group…".into(), shortcut: String::new(), id: Cow::Borrowed("tab_group_edit") },
            PaletteCommand { name: "Tabs: Move to Desk…".into(), shortcut: String::new(), id: Cow::Borrowed("tab_move_desk") },
            PaletteCommand { name: "Tabs: Also Open in Desk…".into(), shortcut: String::new(), id: Cow::Borrowed("tab_copy_desk") },
            PaletteCommand { name: "File: Save".into(), shortcut: "Ctrl+S".into(), id: Cow::Borrowed("save") },
            PaletteCommand { name: "File: Save All".into(), shortcut: "Ctrl+K S".into(), id: Cow::Borrowed("save_all") },
            PaletteCommand { name: "File: Save As...".into(), shortcut: "Ctrl+Shift+S".into(), id: Cow::Borrowed("save_as") },
            PaletteCommand { name: "File: New".into(), shortcut: "Ctrl+N".into(), id: Cow::Borrowed("new_file") },
            PaletteCommand { name: "File: Close Tab".into(), shortcut: "Ctrl+W".into(), id: Cow::Borrowed("close_tab") },
            PaletteCommand { name: "File: Close Other Tabs".into(), shortcut: String::new(), id: Cow::Borrowed("close_others") },
            PaletteCommand { name: "File: Close Tabs to the Right".into(), shortcut: String::new(), id: Cow::Borrowed("close_right") },
            PaletteCommand { name: "File: Close Tabs to the Left".into(), shortcut: String::new(), id: Cow::Borrowed("close_left") },
            PaletteCommand { name: "File: Close All Saved Tabs".into(), shortcut: String::new(), id: Cow::Borrowed("close_saved") },
            PaletteCommand { name: "File: Close All Tabs".into(), shortcut: String::new(), id: Cow::Borrowed("tab_close_all") },
            PaletteCommand { name: "File: Duplicate Tab".into(), shortcut: String::new(), id: Cow::Borrowed("duplicate_tab") },
            PaletteCommand { name: "File: Switch Header/Source".into(), shortcut: "Alt+O".into(), id: Cow::Borrowed("switch_header_source") },
            PaletteCommand { name: "Edit: Undo".into(), shortcut: "Ctrl+Z".into(), id: Cow::Borrowed("undo") },
            PaletteCommand { name: "Edit: Redo".into(), shortcut: "Ctrl+Y / Ctrl+Shift+Z".into(), id: Cow::Borrowed("redo") },
            PaletteCommand { name: "Edit: Cursor Undo".into(), shortcut: "Ctrl+U".into(), id: Cow::Borrowed("cursor_undo") },
            PaletteCommand { name: "Edit: Copy".into(), shortcut: "Ctrl+C".into(), id: Cow::Borrowed("copy") },
            PaletteCommand { name: "Edit: Cut".into(), shortcut: "Ctrl+X".into(), id: Cow::Borrowed("cut") },
            PaletteCommand { name: "Edit: Paste".into(), shortcut: "Ctrl+V".into(), id: Cow::Borrowed("paste") },
            PaletteCommand { name: "Edit: Paste Without Formatting".into(), shortcut: "Ctrl+Alt+V".into(), id: Cow::Borrowed("paste_plain") },
            PaletteCommand { name: "Edit: Select All".into(), shortcut: "Ctrl+A".into(), id: Cow::Borrowed("select_all") },
            PaletteCommand { name: "Edit: Select Line".into(), shortcut: "Ctrl+L".into(), id: Cow::Borrowed("select_line") },
            PaletteCommand { name: "Edit: Select Next Occurrence".into(), shortcut: "Ctrl+D".into(), id: Cow::Borrowed("select_next_occurrence") },
            PaletteCommand { name: "Edit: Select All Occurrences".into(), shortcut: "Ctrl+Shift+L".into(), id: Cow::Borrowed("select_all_occurrences") },
            PaletteCommand { name: "Edit: Add Cursors to Line Ends".into(), shortcut: "Alt+Shift+I".into(), id: Cow::Borrowed("add_cursors_line_ends") },
            PaletteCommand { name: "Selection: Add Cursor Above".into(), shortcut: "Ctrl+Alt+Up".into(), id: Cow::Borrowed("add_cursor_above") },
            PaletteCommand { name: "Selection: Add Cursor Below".into(), shortcut: "Ctrl+Alt+Down".into(), id: Cow::Borrowed("add_cursor_below") },
            PaletteCommand { name: "Edit: Duplicate Line".into(), shortcut: "Alt+Shift+Down".into(), id: Cow::Borrowed("duplicate_line") },
            PaletteCommand { name: "Edit: Duplicate Line Up".into(), shortcut: "Alt+Shift+Up".into(), id: Cow::Borrowed("duplicate_line_up") },
            PaletteCommand { name: "Edit: Delete Line".into(), shortcut: "Ctrl+Shift+K".into(), id: Cow::Borrowed("delete_line") },
            PaletteCommand { name: "Edit: Delete to Line Start".into(), shortcut: "Ctrl+Shift+Backspace".into(), id: Cow::Borrowed("delete_to_line_start") },
            PaletteCommand { name: "Edit: Delete to Line End".into(), shortcut: "Ctrl+Shift+Delete".into(), id: Cow::Borrowed("delete_to_line_end") },
            PaletteCommand { name: "View: Toggle Maximized Editor".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_maximized") },
            PaletteCommand { name: "View: Show Performance and Activity (CPU / Memory)".into(), shortcut: String::new(), id: Cow::Borrowed("performance") },
            PaletteCommand { name: "Show File Statistics".into(), shortcut: String::new(), id: Cow::Borrowed("file_stats") },
            PaletteCommand { name: "File: Show Details".into(), shortcut: String::new(), id: Cow::Borrowed("file_details") },
            PaletteCommand { name: "View: Customize Status Bar".into(), shortcut: String::new(), id: Cow::Borrowed("customize_status_bar") },
            PaletteCommand { name: "Fold: Collapse All".into(), shortcut: "Ctrl+K, 0".into(), id: Cow::Borrowed("fold_all") },
            PaletteCommand { name: "Fold: Expand All".into(), shortcut: "Ctrl+K, Ctrl+J".into(), id: Cow::Borrowed("unfold_all") },
            PaletteCommand { name: "Fold: Collapse Level 1".into(), shortcut: "Ctrl+K 1".into(), id: Cow::Borrowed("fold_level_1") },
            PaletteCommand { name: "Fold: Collapse Level 2".into(), shortcut: "Ctrl+K 2".into(), id: Cow::Borrowed("fold_level_2") },
            PaletteCommand { name: "Fold: Collapse Level 3".into(), shortcut: "Ctrl+K 3".into(), id: Cow::Borrowed("fold_level_3") },
            PaletteCommand { name: "Fold: Collapse Level 4".into(), shortcut: "Ctrl+K 4".into(), id: Cow::Borrowed("fold_level_4") },
            PaletteCommand { name: "Fold: Collapse Level 5".into(), shortcut: "Ctrl+K 5".into(), id: Cow::Borrowed("fold_level_5") },
            PaletteCommand { name: "Edit: Remove Empty Lines".into(), shortcut: String::new(), id: Cow::Borrowed("remove_empty_lines") },
            PaletteCommand { name: "Edit: Merge Blank Lines".into(), shortcut: String::new(), id: Cow::Borrowed("merge_blank_lines") },
            PaletteCommand { name: "View: Toggle Word Count in Status Bar".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_word_count") },
            PaletteCommand { name: "View: Toggle Indent Rainbow".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_indent_rainbow") },
            PaletteCommand { name: "View: Toggle Color Preview".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_color_preview") },
            PaletteCommand { name: "View: Toggle Bracket Pair Colorization".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_bracket_pair_colorization") },
            PaletteCommand { name: "View: Toggle Error Lens".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_error_lens") },
            PaletteCommand { name: "View: Toggle Inlay Hints".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_inlay_hints") },
            PaletteCommand { name: "View: Toggle Code Lens".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_code_lens") },
            PaletteCommand { name: "View: Toggle Semantic Highlighting".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_semantic_tokens") },
            PaletteCommand { name: "View: Toggle Sticky Scroll".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_sticky_scroll") },
            PaletteCommand { name: "View: Toggle Screencast Mode".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_screencast_mode") },
            PaletteCommand { name: "Macro: Start Recording".into(), shortcut: "Alt+R".into(), id: Cow::Borrowed("macro_start") },
            PaletteCommand { name: "Macro: Stop Recording".into(), shortcut: "Alt+R".into(), id: Cow::Borrowed("macro_stop") },
            PaletteCommand { name: "Macro: Replay".into(), shortcut: "Alt+T".into(), id: Cow::Borrowed("macro_replay") },
            PaletteCommand { name: "File: Cancel Deletion".into(), shortcut: String::new(), id: Cow::Borrowed("cancel_file_deletion") },
            PaletteCommand { name: "File: Open Folder...".into(), shortcut: String::new(), id: Cow::Borrowed("change_dir") },
            PaletteCommand { name: "File: Open Parent Folder".into(), shortcut: String::new(), id: Cow::Borrowed("open_parent") },
            PaletteCommand { name: "Git: Open Source Control".into(), shortcut: "Ctrl+Shift+G".into(), id: Cow::Borrowed("source_control") },
            PaletteCommand { name: "Git: Filter Uncommitted Changes...".into(), shortcut: String::new(), id: Cow::Borrowed("git_filter_changes") },
            PaletteCommand { name: "Git: Refresh".into(), shortcut: String::new(), id: Cow::Borrowed("git_refresh") },
            PaletteCommand { name: "Merge Conflict: Accept Current Change".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_current") },
            PaletteCommand { name: "Merge Conflict: Accept Incoming Change".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_incoming") },
            PaletteCommand { name: "Merge Conflict: Accept Both Changes".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_both") },
            PaletteCommand { name: "Merge Conflict: Compare Changes".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_compare") },
            PaletteCommand { name: "Merge Conflict: Next Conflict".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_next_conflict") },
            PaletteCommand { name: "Merge Conflict: Previous Conflict".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_previous_conflict") },
            PaletteCommand { name: "Merge Conflict: Resolve in Merge Editor".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_merge_editor") },
            PaletteCommand { name: "Merge Conflict: Back to Inline".into(), shortcut: String::new(), id: Cow::Borrowed("merge_conflict_close_merge_editor") },
            PaletteCommand { name: "Source Control: Enable / Disable for Workspace".into(), shortcut: String::new(), id: Cow::Borrowed("source_control_toggle") },
            PaletteCommand { name: "Git: Toggle File Blame".into(), shortcut: String::new(), id: Cow::Borrowed("git_toggle_blame") },
            PaletteCommand { name: "Git: Stage File".into(), shortcut: String::new(), id: Cow::Borrowed("git_stage") },
            PaletteCommand { name: "Git: Unstage File".into(), shortcut: String::new(), id: Cow::Borrowed("git_unstage") },
            PaletteCommand { name: "Git: Stage All".into(), shortcut: String::new(), id: Cow::Borrowed("git_stage_all") },
            PaletteCommand { name: "Git: Unstage All".into(), shortcut: String::new(), id: Cow::Borrowed("git_unstage_all") },
            PaletteCommand { name: "Git: Discard All Changes".into(), shortcut: String::new(), id: Cow::Borrowed("git_discard_all") },
            PaletteCommand { name: "Git: Commit".into(), shortcut: String::new(), id: Cow::Borrowed("git_commit") },
            PaletteCommand { name: "Git: Fetch".into(), shortcut: String::new(), id: Cow::Borrowed("git_fetch") },
            PaletteCommand { name: "Git: Pull".into(), shortcut: String::new(), id: Cow::Borrowed("git_pull") },
            PaletteCommand { name: "Git: Push".into(), shortcut: String::new(), id: Cow::Borrowed("git_push") },
            PaletteCommand { name: "Git: Switch / Create Branch".into(), shortcut: String::new(), id: Cow::Borrowed("git_checkout") },
            PaletteCommand { name: "Git: Stash".into(), shortcut: String::new(), id: Cow::Borrowed("git_stash") },
            PaletteCommand { name: "Git: Stash Pop".into(), shortcut: String::new(), id: Cow::Borrowed("git_stash_pop") },
            PaletteCommand { name: "Terminal: Toggle Right Terminal".into(), shortcut: "Ctrl+`".into(), id: Cow::Borrowed("toggle_terminal") },
            PaletteCommand { name: "Terminal: New Terminal".into(), shortcut: "Ctrl+Shift+`".into(), id: Cow::Borrowed("new_terminal") },
            PaletteCommand { name: "Terminal: New Terminal in Editor Tab".into(), shortcut: String::new(), id: Cow::Borrowed("new_terminal_in_editor_tab") },
            PaletteCommand { name: "Terminal: Close Terminal".into(), shortcut: String::new(), id: Cow::Borrowed("close_terminal") },
            PaletteCommand { name: "Terminal: Rename Terminal…".into(), shortcut: String::new(), id: Cow::Borrowed("rename_terminal") },
            PaletteCommand { name: "Terminal: Detach to Editor Tab".into(), shortcut: String::new(), id: Cow::Borrowed("detach_terminal_to_tab") },
            PaletteCommand { name: "Terminal: Move to Right Side".into(), shortcut: String::new(), id: Cow::Borrowed("move_terminal_to_right") },
            PaletteCommand { name: "Tasks: Run Task".into(), shortcut: String::new(), id: Cow::Borrowed("run_task") },
            PaletteCommand { name: "Tasks: Rerun Last Task".into(), shortcut: "Ctrl+Shift+B".into(), id: Cow::Borrowed("rerun_last_task") },
            PaletteCommand { name: "Help: Welcome".into(), shortcut: String::new(), id: Cow::Borrowed("show_welcome") },
            PaletteCommand { name: "Help: About".into(), shortcut: String::new(), id: Cow::Borrowed("about") },
            PaletteCommand { name: "Edit: Toggle Comment".into(), shortcut: "Ctrl+/".into(), id: Cow::Borrowed("toggle_comment") },
            PaletteCommand { name: "Edit: Toggle Block Comment".into(), shortcut: "Ctrl+Shift+/".into(), id: Cow::Borrowed("toggle_block_comment") },
            PaletteCommand { name: "Edit: Select to Matching Bracket".into(), shortcut: "Ctrl+Shift+\\".into(), id: Cow::Borrowed("select_to_bracket") },
            PaletteCommand { name: "Edit: Select Inside Brackets".into(), shortcut: String::new(), id: Cow::Borrowed("select_inside_brackets") },
            PaletteCommand { name: "Edit: Select Inside Quotes".into(), shortcut: String::new(), id: Cow::Borrowed("select_inside_quotes") },
            PaletteCommand { name: "Edit: Move Line Up".into(), shortcut: "Alt+Up".into(), id: Cow::Borrowed("move_line_up") },
            PaletteCommand { name: "Edit: Move Line Down".into(), shortcut: "Alt+Down".into(), id: Cow::Borrowed("move_line_down") },
            PaletteCommand { name: "Search: Find".into(), shortcut: "Ctrl+F".into(), id: Cow::Borrowed("find") },
            PaletteCommand { name: "Search: Find & Replace".into(), shortcut: "Ctrl+H".into(), id: Cow::Borrowed("find_replace") },
            PaletteCommand { name: "Search: Find Next".into(), shortcut: "F3".into(), id: Cow::Borrowed("find_next") },
            PaletteCommand { name: "Search: Find Previous".into(), shortcut: "Shift+F3".into(), id: Cow::Borrowed("find_prev") },
            PaletteCommand { name: "Search: Search in Files".into(), shortcut: "Alt+F".into(), id: Cow::Borrowed("global_search") },
            PaletteCommand { name: "Search: Replace in Files".into(), shortcut: String::new(), id: Cow::Borrowed("replace_all_in_files") },
            PaletteCommand { name: "Go to Line".into(), shortcut: "Ctrl+G".into(), id: Cow::Borrowed("goto_line") },
            PaletteCommand { name: "Go to Symbol".into(), shortcut: "Ctrl+Shift+O".into(), id: Cow::Borrowed("goto_symbol") },
            PaletteCommand { name: "Go to Symbol in Workspace".into(), shortcut: "Ctrl+T".into(), id: Cow::Borrowed("goto_workspace_symbol") },
            PaletteCommand { name: "Go Back".into(), shortcut: "Alt+Left".into(), id: Cow::Borrowed("navigate_back") },
            PaletteCommand { name: "Go Forward".into(), shortcut: "Alt+Right".into(), id: Cow::Borrowed("navigate_forward") },
            PaletteCommand { name: "Go to Definition".into(), shortcut: "F12".into(), id: Cow::Borrowed("go_to_definition") },
            PaletteCommand { name: "Go to Type Definition".into(), shortcut: String::new(), id: Cow::Borrowed("go_to_type_definition") },
            PaletteCommand { name: "Go to Implementation".into(), shortcut: "Ctrl+F12".into(), id: Cow::Borrowed("go_to_implementation") },
            PaletteCommand { name: "Go to Declaration".into(), shortcut: String::new(), id: Cow::Borrowed("go_to_declaration") },
            PaletteCommand { name: "Find All References".into(), shortcut: "Shift+F12".into(), id: Cow::Borrowed("find_references") },
            PaletteCommand { name: "Go to Next Diagnostic".into(), shortcut: "F8".into(), id: Cow::Borrowed("next_diagnostic") },
            PaletteCommand { name: "Go to Previous Diagnostic".into(), shortcut: "Shift+F8".into(), id: Cow::Borrowed("prev_diagnostic") },
            PaletteCommand { name: "Code Actions / Quick Fix".into(), shortcut: "Ctrl+.".into(), id: Cow::Borrowed("code_action") },
            PaletteCommand { name: "Show Call Hierarchy (Incoming)".into(), shortcut: String::new(), id: Cow::Borrowed("call_hierarchy_incoming") },
            PaletteCommand { name: "Show Call Hierarchy (Outgoing)".into(), shortcut: String::new(), id: Cow::Borrowed("call_hierarchy_outgoing") },
            PaletteCommand { name: "Show Type Hierarchy (Supertypes)".into(), shortcut: String::new(), id: Cow::Borrowed("type_hierarchy_supertypes") },
            PaletteCommand { name: "Show Type Hierarchy (Subtypes)".into(), shortcut: String::new(), id: Cow::Borrowed("type_hierarchy_subtypes") },
            PaletteCommand { name: "Format Document".into(), shortcut: "Shift+Alt+F".into(), id: Cow::Borrowed("format_document") },
            PaletteCommand { name: "Configure Formatter…".into(), shortcut: "".into(), id: Cow::Borrowed("configure_formatter") },
            PaletteCommand { name: "Format Selection".into(), shortcut: "Ctrl+K Ctrl+F".into(), id: Cow::Borrowed("format_selection") },
            PaletteCommand { name: "Toggle On-Type Formatting".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_on_type_formatting") },
            PaletteCommand { name: "Quick Open File".into(), shortcut: "Ctrl+P".into(), id: Cow::Borrowed("quick_open") },
            PaletteCommand { name: "Help: Keyboard Shortcuts".into(), shortcut: "Ctrl+?".into(), id: Cow::Borrowed("show_shortcuts") },
            PaletteCommand { name: "View: Toggle Explorer".into(), shortcut: "Ctrl+B".into(), id: Cow::Borrowed("toggle_explorer") },
            PaletteCommand { name: "View: Collapse All in Explorer".into(), shortcut: String::new(), id: Cow::Borrowed("collapse_explorer") },
            PaletteCommand { name: "View: Toggle Sidebar Visibility".into(), shortcut: "Ctrl+B".into(), id: Cow::Borrowed("toggle_sidebar") },
            PaletteCommand { name: "View: Toggle Sidebar Auto-hide".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_sidebar_auto_hide") },
            PaletteCommand { name: "Agent: Toggle Chat".into(), shortcut: "Ctrl+Shift+I".into(), id: Cow::Borrowed("acp_chat") },
            PaletteCommand { name: "Agent: Connect".into(), shortcut: String::new(), id: Cow::Borrowed("acp_connect") },
            PaletteCommand { name: "Agent: New Conversation".into(), shortcut: String::new(), id: Cow::Borrowed("acp_new") },
            PaletteCommand { name: "Agent: Conversations".into(), shortcut: String::new(), id: Cow::Borrowed("acp_sessions") },
            PaletteCommand { name: "Agent: Review Changes".into(), shortcut: String::new(), id: Cow::Borrowed("acp_changes") },
            PaletteCommand { name: "Agent: Message Checkpoints".into(), shortcut: String::new(), id: Cow::Borrowed("acp_checkpoints") },
            PaletteCommand { name: "Agent: Change Mode".into(), shortcut: String::new(), id: Cow::Borrowed("acp_modes") },
            PaletteCommand { name: "Agent: Session Options".into(), shortcut: String::new(), id: Cow::Borrowed("acp_options") },
            PaletteCommand { name: "Agent: Chat Display".into(), shortcut: String::new(), id: Cow::Borrowed("acp_display") },
            PaletteCommand { name: "Agent: Cycle Tool Output".into(), shortcut: String::new(), id: Cow::Borrowed("acp_output_detail") },
            PaletteCommand { name: "Agent: Toggle Thinking Text".into(), shortcut: String::new(), id: Cow::Borrowed("acp_thinking_detail") },
            PaletteCommand { name: "Agent: Authenticate".into(), shortcut: String::new(), id: Cow::Borrowed("acp_auth") },
            PaletteCommand { name: "Agent: Stop Response".into(), shortcut: String::new(), id: Cow::Borrowed("acp_stop") },
            PaletteCommand { name: "Agent: Disconnect".into(), shortcut: String::new(), id: Cow::Borrowed("acp_disconnect") },
            PaletteCommand { name: "Agent: Configure Agents".into(), shortcut: String::new(), id: Cow::Borrowed("acp_configure") },
            PaletteCommand { name: "View: Toggle Minimap".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_minimap") },
            PaletteCommand { name: "View: Toggle Ruler at 80".into(), shortcut: String::new(), id: Cow::Borrowed("ruler_80") },
            PaletteCommand { name: "View: Toggle Ruler at 120".into(), shortcut: String::new(), id: Cow::Borrowed("ruler_120") },
            PaletteCommand { name: "View: Clear All Rulers".into(), shortcut: String::new(), id: Cow::Borrowed("ruler_clear") },
            PaletteCommand { name: "View: Center Screen on Cursor".into(), shortcut: String::new(), id: Cow::Borrowed("center_screen") },
            PaletteCommand { name: "Preferences: Open Settings".into(), shortcut: "Ctrl+,".into(), id: Cow::Borrowed("settings") },
            PaletteCommand { name: "LSP: Configure Language Servers".into(), shortcut: String::new(), id: Cow::Borrowed("language_servers"),
            },
            PaletteCommand {
                name: "VTComplete: Manage File Learning".into(),
                shortcut: String::new(),
                id: Cow::Borrowed("completion_index") },
            PaletteCommand { name: "Explorer: Find Files and Folders by Name".into(), shortcut: String::new(), id: Cow::Borrowed("explorer_find") },
            PaletteCommand { name: "LSP: Restart Language Servers".into(), shortcut: String::new(), id: Cow::Borrowed("restart_language_servers") },
            PaletteCommand { name: "Preferences: Open Selected Settings (JSON)".into(), shortcut: String::new(), id: Cow::Borrowed("open_settings_json") },
            PaletteCommand { name: "Preferences: Open Global Settings (JSON)".into(), shortcut: String::new(), id: Cow::Borrowed("open_global_settings_json") },
            PaletteCommand { name: "Preferences: Open Workspace Settings (JSON)".into(), shortcut: String::new(), id: Cow::Borrowed("open_workspace_settings_json") },
        ];
        cmds.push(PaletteCommand { name: "Preferences: Reload Color Themes".into(), shortcut: String::new(), id: Cow::Borrowed("reload_color_themes") });
        cmds.push(PaletteCommand { name: "Preferences: Install Color Theme from File or Link".into(), shortcut: String::new(), id: Cow::Borrowed("install_color_theme") });
        cmds.push(PaletteCommand { name: "Preferences: Install Syntax from File".into(), shortcut: String::new(), id: Cow::Borrowed("install_syntax") });
        // Dynamically add theme commands from loaded themes
        for tf in themes {
            cmds.push(PaletteCommand {
                name: format!("Preferences: Color Theme {}", tf.name),
                shortcut: String::new(),
                id: Cow::Owned(crate::app_theme::theme_command_id(&tf.config_key)),
            });
        }
        cmds.push(PaletteCommand { name: "Change Language Mode".into(), shortcut: String::new(), id: Cow::Borrowed("change_language") });
        cmds.push(PaletteCommand { name: "Language: Auto-detect".into(), shortcut: String::new(), id: Cow::Borrowed("lang_auto") });
        cmds.push(PaletteCommand { name: "Language: Configure File Associations…".into(), shortcut: String::new(), id: Cow::Borrowed("file_associations") });
        for language in syntax_names {
            cmds.push(PaletteCommand {
                name: format!("Language: {language}"),
                shortcut: String::new(),
                id: Cow::Owned(language_command_id(language)),
            });
        }
        cmds.extend(vec![
            PaletteCommand { name: "View: Toggle Bottom Panel".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_panel") },
            PaletteCommand { name: "View: Show Notifications".into(), shortcut: String::new(), id: Cow::Borrowed("show_notifications") },
            PaletteCommand { name: "View: Output".into(), shortcut: String::new(), id: Cow::Borrowed("show_output") },
            PaletteCommand { name: "View: Toggle Line Numbers".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_line_numbers") },
            PaletteCommand { name: "View: Toggle Relative Line Numbers".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_relative_lines") },
            PaletteCommand { name: "View: Toggle Indent Guides".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_indent_guides") },
            PaletteCommand { name: "View: Toggle Word Wrap".into(), shortcut: "Alt+Z".into(), id: Cow::Borrowed("toggle_word_wrap") },
            PaletteCommand { name: "View: Toggle Render Whitespace".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_whitespace") },
            PaletteCommand { name: "View: Toggle Breadcrumbs".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_breadcrumbs") },
            PaletteCommand { name: "View: Toggle Status Bar".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_status_bar") },
            PaletteCommand { name: "View: Toggle Tab Bar".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_tab_bar") },
            PaletteCommand { name: "View: Toggle Menu Bar".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_menu_bar") },
            PaletteCommand { name: "View: Toggle Cursor Style".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_cursor_style") },
            PaletteCommand { name: "View: Toggle Cursor Blinking".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_cursor_blinking") },
            PaletteCommand { name: "View: Fold".into(), shortcut: "Ctrl+Shift+[".into(), id: Cow::Borrowed("fold") },
            PaletteCommand { name: "View: Unfold".into(), shortcut: "Ctrl+Shift+]".into(), id: Cow::Borrowed("unfold") },
            PaletteCommand { name: "Edit: Sort Imports".into(), shortcut: String::new(), id: Cow::Borrowed("sort_imports") },
            PaletteCommand { name: "Line Ending: Change to LF (Unix)".into(), shortcut: String::new(), id: Cow::Borrowed("eol_lf") },
            PaletteCommand { name: "Line Ending: Change to CRLF (Windows)".into(), shortcut: String::new(), id: Cow::Borrowed("eol_crlf") },
            PaletteCommand { name: "Encoding: UTF-8".into(), shortcut: String::new(), id: Cow::Borrowed("encoding_utf8") },
            PaletteCommand { name: "Encoding: UTF-8 with BOM".into(), shortcut: String::new(), id: Cow::Borrowed("encoding_utf8bom") },
            PaletteCommand { name: "Encoding: UTF-16 LE".into(), shortcut: String::new(), id: Cow::Borrowed("encoding_utf16le") },
            PaletteCommand { name: "Encoding: UTF-16 BE".into(), shortcut: String::new(), id: Cow::Borrowed("encoding_utf16be") },
            PaletteCommand { name: "Encoding: ASCII".into(), shortcut: String::new(), id: Cow::Borrowed("encoding_ascii") },
            PaletteCommand { name: "Encoding: ISO-8859-1 (Latin 1)".into(), shortcut: String::new(), id: Cow::Borrowed("encoding_latin1") },
            PaletteCommand { name: "Encoding: Reopen with Encoding…".into(), shortcut: String::new(), id: Cow::Borrowed("reopen_with_encoding") },
            PaletteCommand { name: "File: Open File Under Cursor".into(), shortcut: String::new(), id: Cow::Borrowed("open_file_at_cursor") },
            PaletteCommand { name: "View: Toggle Focus Mode".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_focus_mode") },
            PaletteCommand { name: "View: Toggle Sidebar Position".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_sidebar_position") },
            PaletteCommand { name: "Go to Last Edit Location".into(), shortcut: "Ctrl+K, Ctrl+Q".into(), id: Cow::Borrowed("goto_last_edit") },
            PaletteCommand { name: "Compare Active File With Next Tab".into(), shortcut: String::new(), id: Cow::Borrowed("compare_tabs") },
            PaletteCommand { name: "Edit: Transform to Uppercase".into(), shortcut: String::new(), id: Cow::Borrowed("to_uppercase") },
            PaletteCommand { name: "Edit: Transform to Lowercase".into(), shortcut: String::new(), id: Cow::Borrowed("to_lowercase") },
            PaletteCommand { name: "Edit: Toggle Case".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_case") },
            PaletteCommand { name: "Edit: Transpose Characters".into(), shortcut: "Ctrl+T".into(), id: Cow::Borrowed("transpose_chars") },
            PaletteCommand { name: "Edit: Trim Trailing Whitespace".into(), shortcut: String::new(), id: Cow::Borrowed("trim_trailing") },
            PaletteCommand { name: "Edit: Insert Line Above".into(), shortcut: "Alt+Enter".into(), id: Cow::Borrowed("insert_line_above") },
            PaletteCommand { name: "Edit: Insert Line Below".into(), shortcut: "Ctrl+Enter".into(), id: Cow::Borrowed("insert_line_below") },
            PaletteCommand { name: "Edit: Join Lines".into(), shortcut: "Ctrl+J".into(), id: Cow::Borrowed("join_lines") },
            PaletteCommand { name: "Edit: Go to Matching Bracket".into(), shortcut: "Ctrl+\\".into(), id: Cow::Borrowed("goto_bracket") },
            PaletteCommand { name: "Edit: Move to Previous Paragraph".into(), shortcut: "Ctrl+Up".into(), id: Cow::Borrowed("paragraph_up") },
            PaletteCommand { name: "Edit: Move to Next Paragraph".into(), shortcut: "Ctrl+Down".into(), id: Cow::Borrowed("paragraph_down") },
            PaletteCommand { name: "Edit: Select Paragraph".into(), shortcut: String::new(), id: Cow::Borrowed("select_paragraph") },
            PaletteCommand { name: "View: Center Cursor".into(), shortcut: String::new(), id: Cow::Borrowed("center_cursor") },
            PaletteCommand { name: "Editor: Indent Using 2 Spaces".into(), shortcut: String::new(), id: Cow::Borrowed("indent_2") },
            PaletteCommand { name: "Editor: Indent Using 4 Spaces".into(), shortcut: String::new(), id: Cow::Borrowed("indent_4") },
            PaletteCommand { name: "Editor: Indent Using 8 Spaces".into(), shortcut: String::new(), id: Cow::Borrowed("indent_8") },
            PaletteCommand { name: "File: Reopen Closed Tab".into(), shortcut: "Ctrl+Shift+T".into(), id: Cow::Borrowed("reopen_closed") },
            PaletteCommand { name: "View: Toggle Zen Mode".into(), shortcut: "F11".into(), id: Cow::Borrowed("toggle_zen") },
            PaletteCommand { name: "Developer: Reload Syntaxes".into(), shortcut: String::new(), id: Cow::Borrowed("reload_syntaxes") },
            PaletteCommand { name: "Bookmarks: Toggle Bookmark".into(), shortcut: "Ctrl+F2".into(), id: Cow::Borrowed("toggle_bookmark") },
            PaletteCommand { name: "Bookmarks: Next Bookmark".into(), shortcut: String::new(), id: Cow::Borrowed("next_bookmark") },
            PaletteCommand { name: "Bookmarks: Previous Bookmark".into(), shortcut: "Shift+F2".into(), id: Cow::Borrowed("prev_bookmark") },
            PaletteCommand { name: "Bookmarks: Clear All Bookmarks".into(), shortcut: String::new(), id: Cow::Borrowed("clear_bookmarks") },
            PaletteCommand { name: "Edit: Rename Symbol".into(), shortcut: "F2".into(), id: Cow::Borrowed("rename_symbol") },
            PaletteCommand { name: "Selection: Expand Selection".into(), shortcut: "Alt+Shift+Right".into(), id: Cow::Borrowed("expand_selection") },
            PaletteCommand { name: "Selection: Shrink Selection".into(), shortcut: "Alt+Shift+Left".into(), id: Cow::Borrowed("shrink_selection") },
            PaletteCommand { name: "Remove Duplicate Lines".into(), shortcut: String::new(), id: Cow::Borrowed("remove_duplicates") },
            PaletteCommand { name: "Remove Blank Lines".into(), shortcut: String::new(), id: Cow::Borrowed("remove_blank_lines") },
            PaletteCommand { name: "Reverse Lines".into(), shortcut: String::new(), id: Cow::Borrowed("reverse_lines") },
            PaletteCommand { name: "Shuffle Lines".into(), shortcut: String::new(), id: Cow::Borrowed("shuffle_lines") },
            PaletteCommand { name: "Transform: camelCase".into(), shortcut: String::new(), id: Cow::Borrowed("to_camel") },
            PaletteCommand { name: "Transform: PascalCase".into(), shortcut: String::new(), id: Cow::Borrowed("to_pascal") },
            PaletteCommand { name: "Transform: snake_case".into(), shortcut: String::new(), id: Cow::Borrowed("to_snake") },
            PaletteCommand { name: "Transform: CONSTANT_CASE".into(), shortcut: String::new(), id: Cow::Borrowed("to_constant") },
            PaletteCommand { name: "Transform: kebab-case".into(), shortcut: String::new(), id: Cow::Borrowed("to_kebab") },
            PaletteCommand { name: "Transform: Title Case".into(), shortcut: String::new(), id: Cow::Borrowed("to_title_case") },
            PaletteCommand { name: "Edit: Indent Line".into(), shortcut: "Tab".into(), id: Cow::Borrowed("indent_line") },
            PaletteCommand { name: "Edit: Outdent Line".into(), shortcut: "Shift+Tab".into(), id: Cow::Borrowed("outdent_line") },
            PaletteCommand { name: "Transform: Base64 Encode".into(), shortcut: String::new(), id: Cow::Borrowed("base64_encode") },
            PaletteCommand { name: "Transform: Base64 Decode".into(), shortcut: String::new(), id: Cow::Borrowed("base64_decode") },
            PaletteCommand { name: "Transform: URL Encode".into(), shortcut: String::new(), id: Cow::Borrowed("url_encode") },
            PaletteCommand { name: "Transform: URL Decode".into(), shortcut: String::new(), id: Cow::Borrowed("url_decode") },
            PaletteCommand { name: "File: Word Count".into(), shortcut: String::new(), id: Cow::Borrowed("word_count") },
            PaletteCommand { name: "File: Character Count".into(), shortcut: String::new(), id: Cow::Borrowed("char_count") },
            PaletteCommand { name: "Edit: Transpose Words".into(), shortcut: String::new(), id: Cow::Borrowed("transpose_words") },
            PaletteCommand { name: "Edit: Surround with ()".into(), shortcut: String::new(), id: Cow::Borrowed("surround_parens") },
            PaletteCommand { name: "Edit: Surround with []".into(), shortcut: String::new(), id: Cow::Borrowed("surround_brackets") },
            PaletteCommand { name: "Edit: Surround with {}".into(), shortcut: String::new(), id: Cow::Borrowed("surround_braces") },
            PaletteCommand { name: "Edit: Surround with \"\"".into(), shortcut: String::new(), id: Cow::Borrowed("surround_double_quotes") },
            PaletteCommand { name: "Edit: Surround with ''".into(), shortcut: String::new(), id: Cow::Borrowed("surround_single_quotes") },
            PaletteCommand { name: "Edit: Surround with ``".into(), shortcut: String::new(), id: Cow::Borrowed("surround_backticks") },
            PaletteCommand { name: "Edit: Surround with <tag>".into(), shortcut: String::new(), id: Cow::Borrowed("surround_tag") },
            PaletteCommand { name: "View: Pin/Unpin Tab".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_pin_tab") },
            PaletteCommand { name: "View: Move Tab Left".into(), shortcut: "Ctrl+Shift+PgUp".into(), id: Cow::Borrowed("move_tab_left") },
            PaletteCommand { name: "View: Move Tab Right".into(), shortcut: "Ctrl+Shift+PgDn".into(), id: Cow::Borrowed("move_tab_right") },
            PaletteCommand { name: "File: Open Containing Folder".into(), shortcut: String::new(), id: Cow::Borrowed("open_folder") },
            PaletteCommand { name: "File: Open in System App".into(), shortcut: String::new(), id: Cow::Borrowed("open_in_system_app") },
            PaletteCommand { name: "File: Copy File Name".into(), shortcut: String::new(), id: Cow::Borrowed("copy_filename") },
            PaletteCommand { name: "File: Copy File Path".into(), shortcut: String::new(), id: Cow::Borrowed("copy_filepath") },
            PaletteCommand { name: "File: Copy Relative Path".into(), shortcut: String::new(), id: Cow::Borrowed("copy_relative_path") },
            PaletteCommand { name: "Indentation: Convert to Spaces".into(), shortcut: String::new(), id: Cow::Borrowed("indent_to_spaces") },
            PaletteCommand { name: "Indentation: Convert to Tabs".into(), shortcut: String::new(), id: Cow::Borrowed("indent_to_tabs") },
            PaletteCommand { name: "Indentation: Toggle Tabs/Spaces".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_indent_mode") },
            PaletteCommand { name: "Indentation: Reindent Lines".into(), shortcut: String::new(), id: Cow::Borrowed("reindent_lines") },
            PaletteCommand { name: "View: Toggle Diff View".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_diff_view") },
            PaletteCommand { name: "View: Toggle Diff Layout (Side by Side / Inline)".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_diff_layout") },
            PaletteCommand { name: "View: Compare Active File with Tab...".into(), shortcut: String::new(), id: Cow::Borrowed("compare_with_tab") },
            PaletteCommand { name: "View: Compare with Git (Saved Version)".into(), shortcut: String::new(), id: Cow::Borrowed("compare_with_git") },
            PaletteCommand { name: "View: Toggle Typewriter Mode".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_typewriter") },
            PaletteCommand { name: "File: Toggle Read-Only Mode".into(), shortcut: String::new(), id: Cow::Borrowed("toggle_read_only") },
            PaletteCommand { name: "Edit: Cycle Paste from Clipboard History".into(), shortcut: "Ctrl+Shift+V".into(), id: Cow::Borrowed("clipboard_history") },
            PaletteCommand { name: "File: Revert File".into(), shortcut: String::new(), id: Cow::Borrowed("revert_file") },
            PaletteCommand { name: "Format: Format JSON".into(), shortcut: String::new(), id: Cow::Borrowed("format_json") },
            PaletteCommand { name: "Format: Minify JSON".into(), shortcut: String::new(), id: Cow::Borrowed("minify_json") },
            PaletteCommand { name: "Format: Sort JSON Keys".into(), shortcut: String::new(), id: Cow::Borrowed("sort_json_keys") },
            PaletteCommand { name: "Indentation: Set Tab Size to 2".into(), shortcut: String::new(), id: Cow::Borrowed("tab_size_2") },
            PaletteCommand { name: "Indentation: Set Tab Size to 4".into(), shortcut: String::new(), id: Cow::Borrowed("tab_size_4") },
            PaletteCommand { name: "Indentation: Set Tab Size to 8".into(), shortcut: String::new(), id: Cow::Borrowed("tab_size_8") },
            PaletteCommand { name: "Indentation: Detect Indentation".into(), shortcut: String::new(), id: Cow::Borrowed("detect_indentation") },
            PaletteCommand { name: "View: Switch Split Tab Next".into(), shortcut: String::new(), id: Cow::Borrowed("split_next_tab") },
            PaletteCommand { name: "View: Switch Split Tab Previous".into(), shortcut: String::new(), id: Cow::Borrowed("split_prev_tab") },
            PaletteCommand { name: "Go to Next Change".into(), shortcut: "Alt+F5".into(), id: Cow::Borrowed("next_change") },
            PaletteCommand { name: "Go to Previous Change".into(), shortcut: "Alt+Shift+F5".into(), id: Cow::Borrowed("prev_change") },
            PaletteCommand { name: "Go to Next Diff Hunk".into(), shortcut: String::new(), id: Cow::Borrowed("next_diff_hunk") },
            PaletteCommand { name: "Go to Previous Diff Hunk".into(), shortcut: String::new(), id: Cow::Borrowed("prev_diff_hunk") },
            PaletteCommand { name: "File: Copy Path".into(), shortcut: String::new(), id: Cow::Borrowed("copy_path") },
            PaletteCommand { name: "File: Reveal in Explorer".into(), shortcut: String::new(), id: Cow::Borrowed("reveal_in_explorer") },
            PaletteCommand { name: "Explorer: Sort by Name".into(), shortcut: String::new(), id: Cow::Borrowed("explorer_sort_name") },
            PaletteCommand { name: "Explorer: Sort by Size".into(), shortcut: String::new(), id: Cow::Borrowed("explorer_sort_size") },
            PaletteCommand { name: "Explorer: Sort by Date Modified".into(), shortcut: String::new(), id: Cow::Borrowed("explorer_sort_modified") },
            PaletteCommand { name: "Explorer: Sort by Type".into(), shortcut: String::new(), id: Cow::Borrowed("explorer_sort_type") },
            PaletteCommand { name: "Markdown: Toggle Preview".into(), shortcut: "Alt+V".into(), id: Cow::Borrowed("markdown_preview") },
            PaletteCommand { name: "View: Split Editor Right".into(), shortcut: "Ctrl+\\".into(), id: Cow::Borrowed("split_editor") },
            PaletteCommand { name: "View: Split Editor Down".into(), shortcut: String::new(), id: Cow::Borrowed("split_editor_down") },
            PaletteCommand { name: "View: Close Split Editor".into(), shortcut: String::new(), id: Cow::Borrowed("close_split") },
            PaletteCommand { name: "View: Close All Editors in Group".into(), shortcut: String::new(), id: Cow::Borrowed("close_group_tabs") },
            PaletteCommand { name: "Application: Quit".into(), shortcut: String::new(), id: Cow::Borrowed("quit") },
        ]);
        cmds
    }

    /// Backward-compatible wrapper; loads built-in themes only
    #[cfg(test)]
    pub(crate) fn build_command_list() -> Vec<PaletteCommand> {
        let themes = crate::app_theme::load_all_themes();
        let syntax_names = crate::highlight::Highlighter::new().syntax_names();
        Self::build_command_list_with_themes(&themes, &syntax_names)
    }

    pub(crate) fn menu_labels() -> &'static [&'static str] {
        &["File", "Edit", "Selection", "View", "Go", "Run", "Terminal", "Help"]
    }

    pub(crate) fn menu_items(&self, menu_idx: usize) -> Vec<(String, &'static str)> {
        // (label, default_shortcut, command_id)
        let raw: Vec<(&str, &str, &str)> = match menu_idx {
            0 => vec![ // File
                ("New File", "", "new_file"),
                ("New Folder...", "", "new_folder"),
                ("─", "", ""),
                ("Open File...", "", "quick_open"),
                ("Open Folder...", "", "change_dir"),
                ("Open Recent", "", "open_recent_menu"),
                ("─", "", ""),
                ("Save", "", "save"),
                ("Save As...", "", "save_as"),
                ("Save All", "", "save_all"),
                ("Revert File", "", "revert_file"),
                ("─", "", ""),
                ("Close Tab", "", "close_tab_menu"),
                ("Close All Tabs", "", "tab_close_all"),
                ("─", "", ""),
                ("Exit", "", "quit_menu"),
            ],
            1 => vec![ // Edit
                ("Undo", "", "undo"),
                ("Redo", "", "redo"),
                ("─", "", ""),
                ("Cut", "", "cut"),
                ("Copy", "", "copy"),
                ("Paste", "", "paste"),
                ("─", "", ""),
                ("Find", "", "find"),
                ("Replace", "", "find_replace"),
                ("Find in Files", "", "global_search"),
                ("─", "", ""),
                ("Select All", "", "select_all"),
                ("Toggle Comment", "", "toggle_comment"),
                ("Toggle Block Comment", "", "toggle_block_comment"),
                ("─", "", ""),
                ("Move Line Up", "Alt+Up", "move_line_up"),
                ("Move Line Down", "Alt+Down", "move_line_down"),
                ("Duplicate Line", "", "duplicate_line"),
                ("Delete Line", "", "delete_line"),
            ],
            2 => vec![ // Selection
                ("Select All", "", "select_all"),
                ("Select Line", "", "select_line"),
                ("─", "", ""),
                ("Expand Selection", "Ctrl+Shift+Right", "expand_selection"),
                ("Shrink Selection", "Ctrl+Shift+Left", "shrink_selection"),
                ("Select Inside Brackets", "", "select_inside_brackets"),
                ("Select Inside Quotes", "", "select_inside_quotes"),
                ("─", "", ""),
                ("Add Cursor Above", "Ctrl+Alt+Up", "add_cursor_above"),
                ("Add Cursor Below", "Ctrl+Alt+Down", "add_cursor_below"),
                ("Select Next Occurrence", "", "select_next_occurrence"),
                ("Select All Occurrences", "Ctrl+Shift+L", "select_all_occurrences"),
            ],
            3 => vec![ // View
                ("Command Palette", "", "palette_menu"),
                ("Reload Syntaxes", "", "reload_syntaxes"),
                ("Install Syntax…", "", "install_syntax"),
                ("─", "", ""),
                ("Explorer", "", "toggle_explorer"),
                ("Search", "", "global_search"),
                ("Source Control", "", "source_control"),
                ("Extensions", "", "extensions"),
                ("─", "", ""),
                ("Toggle Panel", "", "toggle_panel"),
                ("Output", "", "show_output"),
                ("Agent Chat", "Ctrl+Shift+I", "acp_chat"),
                ("Toggle Sidebar", "", "toggle_sidebar"),
                (if self.sidebar_auto_hide.enabled { "✓ Auto-hide Sidebar" } else { "Auto-hide Sidebar" }, "", "toggle_sidebar_auto_hide"),
                ("Toggle Minimap", "", "toggle_minimap"),
                ("Toggle Line Numbers", "", "toggle_line_numbers"),
                ("Toggle Breadcrumbs", "", "toggle_breadcrumbs"),
                ("Toggle Status Bar", "", "toggle_status_bar"),
                ("Toggle Tab Bar", "", "toggle_tab_bar"),
                ("Toggle Menu Bar", "", "toggle_menu_bar"),
                ("Zen Mode", "", "toggle_zen"),
                ("─", "", ""),
                ("Word Wrap", "", "toggle_word_wrap"),
                ("Settings", "Ctrl+,", "settings"),
            ],
            4 => vec![ // Go
                ("Go to File...", "", "quick_open"),
                ("Go to Line...", "", "goto_line"),
                ("Go to Symbol...", "Ctrl+Shift+O", "goto_symbol"),
                ("─", "", ""),
                ("Go Back", "Alt+Left", "navigate_back"),
                ("Go Forward", "Alt+Right", "navigate_forward"),
                ("Go to Last Edit", "Ctrl+K, Ctrl+Q", "goto_last_edit"),
                ("─", "", ""),
                ("Go to Bracket", "", "goto_bracket"),
                ("─", "", ""),
                ("Next Diagnostic", "", "next_diagnostic"),
                ("Previous Diagnostic", "", "prev_diagnostic"),
                ("─", "", ""),
                ("Next Change", "Alt+F5", "next_change"),
                ("Previous Change", "Alt+Shift+F5", "prev_change"),
            ],
            5 => vec![ // Run
                ("Run Task...", "", "run_task"),
                ("Rerun Last Task", "Ctrl+Shift+B", "rerun_last_task"),
            ],
            6 => vec![ // Terminal
                ("Toggle Right Terminal", "Ctrl+`", "toggle_terminal"),
                ("New Terminal", "Ctrl+Shift+`", "new_terminal"),
                ("New Terminal in Editor Tab", "", "new_terminal_in_editor_tab"),
                ("Rename Terminal…", "", "rename_terminal"),
                ("Move to Right Side", "", "move_terminal_to_right"),
                ("Detach to Editor Tab", "", "detach_terminal_to_tab"),
            ],
            7 => vec![ // Help
                ("Welcome", "", "show_welcome"),
                ("Keyboard Shortcuts", "", "show_shortcuts"),
                ("─", "", ""),
                ("About", "", "about"),
            ],
            _ => vec![],
        };

        raw.into_iter().map(|(label, default_sc, cmd_id)| {
            if label == "─" || cmd_id.is_empty() {
                return ("─".to_string(), "");
            }
            let shortcut = self.shortcut_for_command(cmd_id);
            let shortcut = if shortcut.is_empty() && !default_sc.is_empty() {
                default_sc.to_string()
            } else {
                shortcut
            };
            if shortcut.is_empty() {
                (label.to_string(), cmd_id)
            } else {
                // Pad label to align shortcuts
                let pad = 24usize.saturating_sub(label.len());
                let spaces = " ".repeat(pad);
                (format!("{label}{spaces}{shortcut}"), cmd_id)
            }
        }).collect()
    }

    /// Check if a menu command should be disabled given current app state
    pub(crate) fn is_menu_cmd_disabled(&self, cmd_id: &str) -> bool {
        let output = crate::ui_logs::focused(self);
        let has_editor = !output && self.editor().is_some();
        let has_selection = !output && self.editor().is_some_and(|e| e.has_selection());
        let is_modified = has_editor && self.editor().is_some_and(|e| e.modified);
        let is_read_only = output || has_editor && self.editor().is_some_and(|e| e.read_only)
            || self.diff_selection_owns_cursor();
        let has_file = has_editor && self.editor().is_some_and(|e| e.file_path.is_some());
        let has_tabs = !self.tabs.is_empty();
        let can_undo = has_editor && self.editor().is_some_and(|e| e.can_undo());
        let can_redo = has_editor && self.editor().is_some_and(|e| e.can_redo());

        if output {
            match cmd_id {
                "copy" => return !self.logs.has_selection(),
                "cut" if self.logs.search_focused() => return !self.logs.has_selection(),
                "paste" if self.logs.search_focused() => return false,
                "select_all" => return false,
                _ => {}
            }
        }
        match cmd_id {
            // File
            "save" => !has_editor || !is_modified || !has_file,
            "revert_file" => !has_editor || !is_modified || !has_file,
            "save_as" | "save_all" => !has_editor,
            "close_tab_menu" | "tab_close_all" => !has_tabs,
            // Edit
            "undo" => !can_undo || is_read_only,
            "redo" => !can_redo || is_read_only,
            "cut" => self.focused_terminal_index().is_some() || !has_selection || is_read_only,
            "copy" => if self.focused_terminal_index().is_some() {
                !self.focused_terminal_has_selection()
            } else { !has_selection && !self.has_diff_selection() },
            "paste" => self.focused_terminal_index().is_none() && (!has_editor || is_read_only),
            "delete_line" | "duplicate_line" | "move_line_up" | "move_line_down"
            | "indent" | "outdent" | "toggle_comment" | "join_lines"
            | "transform_uppercase" | "transform_lowercase" | "transform_titlecase" => {
                !has_editor || is_read_only
            }
            // Selection
            "select_all" | "select_line" | "expand_selection" | "shrink_selection" => !has_editor,
            "add_cursor_above" | "add_cursor_below" | "add_cursors_to_line_ends" => !has_editor,
            // View
            "toggle_diff_layout" => !self.should_render_diff_for_active_editor(),
            // Terminal layout actions need an open session, not the empty
            // placeholder retained at startup and after the last close.
            "move_terminal_to_right" | "rename_terminal" => {
                !self.terminal_is_open(self.active_terminal)
            }
            "detach_terminal_to_tab" => {
                !self.terminal_is_open(self.active_terminal)
                    || self.terminal_is_in_editor(self.active_terminal)
            }
            // Go
            "go_to_line" | "go_to_definition" | "go_to_references" | "go_to_symbol"
            | "go_to_bracket" | "next_change" | "prev_change"
            | "next_diff_hunk" | "prev_diff_hunk" => !has_editor,
            _ => false,
        }
    }

    pub(crate) fn tab_context_items() -> Vec<(&'static str, &'static str, &'static str)> {
        vec![
            ("Close", "tab_close", "Ctrl+W"),
            ("Rename Terminal…", "tab_rename_terminal", ""),
            ("Close Others", "tab_close_others", ""),
            ("Close All", "tab_close_all", ""),
            ("Close Saved", "tab_close_saved", ""),
            ("Close to the Right", "tab_close_right", ""),
            ("Close to the Left", "tab_close_left", ""),
            ("─", "", ""),
            ("Split Right", "tab_split_right", ""),
            ("Split Down", "tab_split_down", ""),
            ("Close Split", "tab_close_split", ""),
            ("─", "", ""),
            ("Select for Compare", "tab_select_compare", ""),
            ("Compare with Selected", "tab_compare_selected", ""),
            ("─", "", ""),
            ("Copy Path", "tab_copy_path", ""),
            ("Copy Relative Path", "tab_copy_rel_path", ""),
            ("─", "", ""),
            ("Pin Tab", "tab_pin", ""),
            ("Duplicate Tab", "duplicate_tab", ""),
            ("Add to New Group…", "tab_group_new", ""),
            ("Add to Group…", "tab_group_add", ""),
            ("Edit Group…", "tab_group_edit", ""),
            ("Remove from Group", "tab_group_remove", ""),
            ("Move to Desk…", "tab_move_desk", ""),
            ("Also Open in Desk…", "tab_copy_desk", ""),
            ("Reveal in Explorer", "tab_reveal", ""),
            ("Open Containing Folder", "tab_open_folder", ""),
            ("Open in System App", "tab_open_system", ""),
        ]
    }

    pub(crate) fn explorer_context_menu_items(&self) -> Vec<&str> {
        if self.explorer_context_is_workspace_root() {
            return vec![
                "Find by Name…",
                "Find in Folder…",
                "─",
                "New File",
                "New Folder",
                "─",
                "Paste File",
                "─",
                "Open in Terminal",
                "Reveal in File Explorer",
                "Copy Path",
                "─",
                "Sort by Name",
                "Sort by Size",
                "Sort by Date Modified",
                "Sort by Type",
            ];
        }
        let mut items = vec!["Open", "Find by Name…"];
        if self.explorer_context_target.as_ref().is_some_and(|target| {
            self.explorer.entries.iter()
                .find(|entry| Self::same_open_path(&entry.path, target))
                .map_or_else(|| target.is_dir(), |entry| entry.is_dir)
        }) {
            items.push("Find in Folder…");
        }
        items.extend_from_slice(&["─", "Copy File", "Cut File", "Paste File"]);
        if let Some(target) = self.explorer_context_target.as_ref().filter(|target| {
            self.explorer.entries.iter()
                .find(|entry| Self::same_open_path(&entry.path, target))
                .map_or_else(|| target.is_file(), |entry| !entry.is_dir)
        }) {
            items.extend_from_slice(&["─", "Select for Compare"]);
            if self.can_compare_with_selected(target) {
                items.push("Compare with Selected");
            }
        }
        items.extend_from_slice(&["─", "New File", "New Folder", "─", "Rename", "Delete", "─", "Open in System App", "Open in Terminal", "Reveal in File Explorer", "Copy Path", "Copy Relative Path"]);
        items.extend_from_slice(&["─", "Sort by Name", "Sort by Size", "Sort by Date Modified", "Sort by Type"]);
        if !self.explorer_context_extension_commands.is_empty() {
            items.push("─");
            for command in &self.explorer_context_extension_commands {
                items.push(command.title.as_str());
            }
        }
        items
    }

    /// Return the display string for the current keybinding of the given command ID.
    /// Returns an empty string if the command has no keybinding.
    pub(crate) fn shortcut_for_command(&self, cmd_id: &str) -> String {
        // Map command IDs that differ between palette/menu and keybinding system
        let kb_id = match cmd_id {
            "find" => "find",
            "find_replace" => "find_replace",
            "goto_line" => "go_to_line",
            "palette_menu" | "command_palette" => "palette",
            "close_tab_menu" => "close_tab",
            "quit_menu" => return String::new(), // no keybinding for quit
            other => other,
        };
        for kb in &self.keybindings {
            if kb.id == kb_id {
                let d = kb.display();
                if d == "Unassigned" { return String::new(); }
                return d;
            }
        }
        String::new()
    }

    /// Update shortcut hints in palette_commands and context_menu_items
    /// to reflect current keybindings.
    pub(crate) fn refresh_shortcut_hints(&mut self) {
        // Build lookup from keybindings first (avoids borrow conflict)
        let mut shortcut_map = std::collections::HashMap::new();
        for kb in &self.keybindings {
            let d = kb.display();
            if d != "Unassigned" {
                shortcut_map.entry(kb.id).or_insert(d);
            }
        }
        // Helper to resolve command ID aliases
        let resolve = |cmd_id: &str| -> String {
            let kb_id = match cmd_id {
                "find" => "find",
                "find_replace" => "find_replace",
                "goto_line" => "go_to_line",
                "palette_menu" | "command_palette" => "palette",
                "close_tab_menu" => "close_tab",
                "quit_menu" => return String::new(),
                other => other,
            };
            shortcut_map.get(kb_id).cloned().unwrap_or_default()
        };
        // Palette commands
        for cmd in &mut self.palette.commands {
            let hint = resolve(&cmd.id);
            if !hint.is_empty() || !cmd.shortcut.is_empty() {
                cmd.shortcut = hint;
            }
        }
        // Context menu items
        for item in &mut self.context_menu_items {
            let hint = resolve(item.id);
            if !hint.is_empty() || !item.shortcut.is_empty() {
                item.shortcut = hint;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use std::collections::HashSet;

    #[test]
    fn terminal_layout_commands_require_an_open_terminal() {
        const COMMANDS: [&str; 2] = [
            "move_terminal_to_right", "detach_terminal_to_tab",
        ];
        fn assert_closed(app: &mut App) {
            let tabs = app.tabs.len();
            let dock = app.terminals[0].dock;
            for command in COMMANDS {
                assert!(app.is_menu_cmd_disabled(command), "{command}");
                app.execute_palette_command(command);
                assert!(!app.show_terminal_sidebar);
                assert!(!app.show_panel);
                assert!(!app.terminal_panel_focused);
                assert_eq!(app.terminals.len(), 1);
                assert_eq!(app.terminals[0].dock, dock);
                assert!(app.terminals[0].terminal_pty.is_none());
                assert_eq!(app.tabs.len(), tabs);
            }
            assert!(!app.is_menu_cmd_disabled("new_terminal"));
            assert!(!app.is_menu_cmd_disabled("toggle_terminal"));
        }

        let mut app = App::test_new();
        assert_closed(&mut app);
        app.toggle_terminal_sidebar();
        // Opening is enough; actions must not wait for the first PTY paint.
        assert!(app.terminals[0].terminal_pty.is_none());
        for command in COMMANDS {
            assert!(!app.is_menu_cmd_disabled(command), "{command}");
        }
        app.execute_palette_command("move_terminal_to_right");
        assert!(app.show_terminal_sidebar);
        assert!(!app.show_panel);
        app.execute_palette_command("detach_terminal_to_tab");
        assert!(app.terminal_is_in_editor(0));
        assert!(app.is_menu_cmd_disabled("detach_terminal_to_tab"));
        assert!(!app.is_menu_cmd_disabled("move_terminal_to_right"));
        app.execute_palette_command("move_terminal_to_right");
        assert!(!app.terminal_is_in_editor(0));
        app.close_terminal_tab();
        assert_closed(&mut app);
    }

    #[test]
    fn hidden_terminal_sessions_can_still_be_moved() {
        for state in ["running", "launch error", "pending task"] {
            let mut app = App::test_new();
            match state {
                "running" => app.terminals[0].terminal_pty = Some(
                    crate::terminal::TerminalEmulator::test_with_output(40, 10, ""),
                ),
                "launch error" => app.terminals[0].terminal_launch_error = Some("spawn failed".into()),
                _ => app.terminals[0].pending_command = Some("task".into()),
            }
            assert!(!app.show_terminal_sidebar && !app.show_panel);
            for command in ["move_terminal_to_right", "detach_terminal_to_tab"] {
                assert!(!app.is_menu_cmd_disabled(command), "{state}: {command}");
            }
            app.execute_palette_command("move_terminal_to_right");
            assert!(app.show_terminal_sidebar && !app.show_panel);
            assert_eq!(app.terminals.len(), 1);
            app.close_terminal_tab();
            assert!(app.is_menu_cmd_disabled("move_terminal_to_right"));
        }
    }

    #[test]
    fn command_list_no_duplicate_ids() {
        let commands = App::build_command_list();
        let mut seen = HashSet::new();
        for cmd in &commands {
            assert!(seen.insert(&*cmd.id), "Duplicate command id: {}", cmd.id);
        }
    }

    #[test]
    fn command_list_non_empty_fields() {
        let commands = App::build_command_list();
        assert!(!commands.is_empty());
        for cmd in &commands {
            assert!(!cmd.id.is_empty(), "Command has empty id");
            assert!(!cmd.name.is_empty(), "Command {} has empty name", &*cmd.id);
        }
    }

    #[test]
    fn command_list_uses_every_visible_textmate_language() {
        let syntax_names = crate::highlight::Highlighter::new().syntax_names();
        let commands = App::build_command_list();
        let command_languages: Vec<&str> = commands
            .iter()
            .filter_map(|command| super::language_from_command_id(command.id.as_ref()))
            .collect();
        let expected: Vec<&str> = syntax_names.iter().map(String::as_str).collect();

        assert_eq!(command_languages, expected);
        assert!(commands.iter().any(|command| command.id == "lang_auto"));
    }

    #[test]
    fn save_shortcuts_match_vscode_conventions() {
        let commands = App::build_command_list();
        let save_all = commands.iter().find(|command| command.id == "save_all").unwrap();
        let save_as = commands.iter().find(|command| command.id == "save_as").unwrap();
        assert_eq!(save_all.shortcut, "Ctrl+K S");
        assert_eq!(save_as.shortcut, "Ctrl+Shift+S");
    }

    #[test]
    fn context_menu_no_duplicate_ids() {
        let items = App::build_context_menu();
        let mut seen = HashSet::new();
        for item in &items {
            assert!(seen.insert(item.id), "Duplicate context menu id: {}", item.id);
        }
    }

    #[test]
    fn context_menu_non_empty_labels() {
        let items = App::build_context_menu();
        assert!(!items.is_empty());
        for item in &items {
            assert!(!item.label.is_empty(), "Menu item {} has empty label", item.id);
        }
    }

    #[test]
    fn menu_items_all_valid() {
        let app = App::new(std::path::PathBuf::from("."));
        for idx in 0..App::menu_labels().len() {
            let items = app.menu_items(idx);
            assert!(!items.is_empty(), "Menu index {} is empty", idx);
            for (label, id) in &items {
                if label != "─" {
                    assert!(!id.is_empty(), "Menu item '{}' in menu {} has empty id", label, idx);
                }
            }
        }
    }

    #[test]
    fn menu_items_out_of_range_empty() {
        let app = App::new(std::path::PathBuf::from("."));
        let items = app.menu_items(99);
        assert!(items.is_empty());
    }

    #[test]
    fn vanilla_view_menu_offers_extensions() {
        let app = App::test_new();
        assert!(app.extension_manager.is_empty());
        assert!(app
            .menu_items(3)
            .iter()
            .any(|(_, command_id)| *command_id == "extensions"));
    }

    #[test]
    fn tab_context_items_no_duplicate_ids() {
        let items = App::tab_context_items();
        let mut seen = HashSet::new();
        for (label, id, _shortcut) in &items {
            if *label != "─" {
                assert!(seen.insert(*id), "Duplicate tab context id: {}", id);
            }
        }
    }
}
