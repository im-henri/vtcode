use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::editor::Editor;
use crate::explorer::FileExplorer;
use crate::highlight::Highlighter;

mod types;

#[path = "app_lsp_sync.rs"]
mod lsp_sync;

pub(crate) type VisibleHighlightKey = (PathBuf, u64, usize, usize, String, Option<String>, u64);

pub(crate) type OverviewChangeRowsKey = (
    u64,             // stable editor identity (also distinguishes untitled panes)
    Option<PathBuf>, // document identity
    bool,            // whether unsaved markers are currently visible
    bool,            // whether document-wide markers are disabled
    u64,             // applied marker revision
    usize,           // compatibility status length
    usize,           // document line count
    usize,           // overview height
    usize,           // compare marker allocation identity
    usize,           // compare marker count
    u64,             // compare generation
    u64,             // compare refresh generation
);
pub(crate) type CursorWordKey = (u64, usize, usize, u64);

pub(crate) use types::{
    compute_file_diff, default_keybindings, ActivePane, AlignedDiffRow, AutoSaveMode,
    CallHierarchyEntry, CallHierarchyMode, ContextMenuItem, CursorStyle, DiffBaseRevision,
    DiffLabels, DiffMode, DiffRenderCache, DiffScopeLineSummary, EditorGroup, ExtensionPaletteAction, ExtensionViewTabRef,
    GitBranchPickerItem, GitCommitInfo, GitCommitReference, GitFileChange, GlobalSearchResult, InlayHintDisplay, KeyBinding, NotifyLevel,
    PaletteCommand, PaletteState, PeekDefinition, RenameState, SidebarView,
    SourceControlRepositoryState, SplitDirection, SplitNode, SymbolItem, Tab, TabDropZone, TabKind,
    TaskDefinition, TerminalDock, TerminalTab, TypeHierarchyMode, WhitespaceMode,
};
pub(crate) use types::{contains_ignore_case, fuzzy_match, fuzzy_match_indices, fuzzy_match_score};

#[derive(Default)]
struct RemoteDiskChecks {
    queued: rustc_hash::FxHashSet<PathBuf>,
    running: Option<crate::background::CancellationToken>,
}

impl Drop for RemoteDiskChecks {
    fn drop(&mut self) {
        if let Some(cancel) = &self.running { cancel.cancel(); }
    }
}

pub(crate) struct App {
    pub(crate) remote_workspace: Option<crate::remote::workspace::Workspace>,
    pub(crate) remote_connection: crate::app_remote_connection::State,
    pub(crate) side_notice: crate::ui_side_notice::State,
    pub(crate) logs: crate::ui_logs::State,
    pub(crate) loading: crate::ui_loading::State,
    pub(crate) remote_explorer_error: Option<String>,
    pub(crate) explorer_expanding: rustc_hash::FxHashSet<PathBuf>,
    pub(crate) remote_navigation: crate::app_remote_navigation::State,
    pub(crate) global_search_service: std::sync::OnceLock<crate::app_search::LatestQueryService>,
    pub(crate) desks: crate::app_desks::Desks,
    pub(crate) chat: crate::app_acp::ChatState,
    pub active_pane: ActivePane,
    pub explorer: FileExplorer,
    pub session_baseline: Option<crate::session::Session>,
    pub(crate) workspace_generation: u64,
    pub(crate) workspace_open_generation: u64,
    // Active group state (these are the "live" fields for the currently focused group)
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub tab_scroll_offset: usize,
    pub tab_scroll_manual_anchor: Option<usize>,
    // Editor groups — groups[i] stores saved state for non-active groups.
    // The active group's state lives in the flat App fields above.
    pub groups: Vec<EditorGroup>,
    pub active_group: usize,
    pub split_vertical: bool,
    pub split_ratio: f32,
    pub resizing_split: bool,
    /// Horizontal ratio and live geometry for the internal side-by-side diff split.
    pub diff_split_ratio: f32,
    pub resizing_diff_split: bool,
    pub diff_split_area: Option<ratatui::layout::Rect>,
    pub diff_divider_area: Option<ratatui::layout::Rect>,
    pub diff_left_code_area: Option<ratatui::layout::Rect>,
    pub diff_left_h_scrollbar_area: Option<ratatui::layout::Rect>,
    pub diff_left_horizontal_scroll: usize,
    pub dragging_diff_left_h_scrollbar: bool,
    pub split_tree: Option<SplitNode>, // None = single pane, Some = nested splits
    /// Divider rects computed during rendering, for mouse hit detection.
    /// Each entry: (Rect, path to split node, child index before the divider, direction)
    pub split_dividers: Vec<(
        ratatui::layout::Rect,
        Vec<usize>,
        usize,
        SplitDirection,
        ratatui::layout::Rect,
    )>, // (divider_rect, path, child_idx, direction, parent_area)
    pub(crate) split_layout_cache: crate::ui_chrome::SplitPaneLayout,
    /// Active divider being dragged: (path, child_index, direction, total_area_size)
    pub dragging_divider: Option<(Vec<usize>, usize, SplitDirection, u16)>,
    pub show_explorer: bool,
    pub show_activity_bar: bool,
    pub sidebar_view: SidebarView,
    pub show_open_editors: bool,
    pub(crate) open_editors_scroll_offset: usize,
    pub(crate) open_editors_scroll_anchor: Option<(usize, u64)>,
    pub show_minimap: bool,
    pub minimap_width: u16,
    pub show_panel: bool,
    pub show_line_numbers: bool,
    pub relative_line_numbers: bool,
    pub show_indent_guides: bool,
    pub rulers: Vec<u16>,    // Column ruler positions (e.g., 80, 120)
    pub chord_pending: bool, // Ctrl+K chord mode
    pub chord_start: std::time::Instant, // When chord was initiated (for timeout)
    pub cursor_visible: bool, // For blink effect
    pub cursor_blinking: bool,
    /// Crossterm focus state. Background/minimized windows must not animate a
    /// logical editor caret or manufacture idle frames.
    pub(crate) terminal_has_focus: bool,
    /// Real terminal input, rather than worker progress, keeps foreground-only
    /// animation and extension timers active. After a short quiet period the
    /// event loop converges to an indefinite wait even if the terminal still
    /// technically owns focus.
    pub(crate) last_user_activity: std::time::Instant,
    pub(crate) idle_quiescent: bool,
    pub(crate) animation_settings: crate::animation_config::AnimationSettings,
    pub(crate) animations: crate::animations::AnimationState,
    pub(crate) copy_animation: crate::copy_animation::CopyAnimation,
    pub(crate) animation_frame_cache: crate::ui_animation_frame::AnimationFrameCache,
    pub(crate) rendering_animation_frame: bool,
    pub(crate) tab_animation_cache:
        std::cell::RefCell<Option<crate::ui_chrome_tabs::TabAnimationCache>>,
    pub(crate) tab_name_cache: std::cell::RefCell<crate::ui_chrome_tabs::NameCache>,
    pub scroll_speed: usize,
    pub language_associations: rustc_hash::FxHashMap<String, String>,
    pub cursor_style: CursorStyle, // line, block, or underline
    pub last_blink: std::time::Instant,
    pub last_terminal_cursor_pos: Option<(u16, u16)>,
    pub terminal_cursor_shown: bool,
    pub status_message: String,
    pub notification_history: Vec<(String, NotifyLevel, u64)>, // message, level, seconds since app start
    // Cached highlighter (avoid recreating each frame)
    pub highlighter: Highlighter,
    // Multi-click detection (double/triple click)
    pub last_click_time: std::time::Instant,
    pub last_click_pos: (u16, u16),
    pub click_count: u8,
    /// Anchor range for word/line-granularity drag selection (start_line, start_col, end_line, end_col)
    pub multi_click_anchor: Option<(usize, usize, usize, usize)>,
    // Layout areas (updated each frame for mouse hit-testing)
    /// The last painted viewport is authoritative for pointer geometry. Never
    /// query the console host synchronously for each hover, press or drag.
    pub(crate) viewport_area: ratatui::layout::Rect,
    pub explorer_area: Option<ratatui::layout::Rect>,
    /// Explorer scrollbar drag origin: (section, pointer row, scroll offset).
    pub(crate) explorer_scrollbar_drag: Option<(crate::ui_chrome::ExplorerSection, u16, usize)>,
    pub editor_area: Option<ratatui::layout::Rect>,
    pub tab_bar_area: Option<ratatui::layout::Rect>,
    pub gutter_width: u16,
    pub gutter_area: Option<ratatui::layout::Rect>,
    pub code_area: Option<ratatui::layout::Rect>,
    /// Transient outer editor pane projection, available only while drawing it.
    pub(crate) editor_gradient_area: Option<ratatui::layout::Rect>,
    pub minimap_area: Option<ratatui::layout::Rect>,
    pub hovered_gutter_line: Option<usize>,
    pub hovered_editor_pos: Option<(usize, usize)>, // (line, col) for diagnostic hover
    pub scrollbar_area: Option<ratatui::layout::Rect>,
    pub h_scrollbar_area: Option<ratatui::layout::Rect>,
    pub dragging_v_scrollbar: bool,
    pub dragging_h_scrollbar: bool,
    pub dragging_minimap: bool,
    pub minimap_drag_start: Option<(usize, usize)>, // (pointer row, scroll offset)
    pub(crate) settings_scrollbar_drag: Option<crate::settings_layout::Drag>,
    /// Text-field hit maps rebuilt from the actual rendered viewport each frame.
    pub text_input_regions: Vec<crate::text_input::TextInputRegion>,
    /// Active pointer selection: (field identity, UTF-8 byte anchor).
    pub text_input_drag: Option<(crate::text_input::TextInputId, usize)>,
    pub(crate) text_input_histories: std::collections::HashMap<crate::text_input::TextInputId, crate::text_input::TextInputHistory>,
    pub(crate) text_input_click_id: Option<crate::text_input::TextInputId>,
    pub(crate) text_input_drag_unit: Option<(usize, usize, bool)>,
    pub status_bar_area: Option<ratatui::layout::Rect>,
    pub(crate) performance_area: Option<ratatui::layout::Rect>,
    pub(crate) performance_view: crate::ui_performance_details::ViewState,
    pub status_bar_items: Vec<(u16, u16, &'static str)>, // (x, width, action_id)
    pub(crate) status_details: crate::ui_status::DetailsState,
    pub(crate) status_bar_pins: std::collections::BTreeMap<String, bool>,
    pub(crate) chrome_hover: crate::ui_status::HoverState,
    pub(crate) panel_tab_areas: Vec<(ratatui::layout::Rect, u8)>,
    // Breadcrumb
    pub show_breadcrumbs: bool,
    pub show_status_bar: bool,
    pub show_tab_bar: bool,
    pub enable_preview_tabs: bool,
    pub mouse_back_forward_to_navigate: bool,
    pub show_menu_bar: bool,
    pub breadcrumb_area: Option<ratatui::layout::Rect>,
    /// (x, width, segment_index) for each breadcrumb segment
    pub breadcrumb_segments: Vec<(u16, u16, usize)>,
    pub breadcrumb_dropdown_active: bool,
    pub breadcrumb_dropdown_segment: usize,
    /// Cached path and directory bit; dropdown rendering never probes disk.
    pub breadcrumb_dropdown_items: Vec<(std::path::PathBuf, bool)>,
    pub breadcrumb_dropdown_selected: usize,
    pub breadcrumb_dropdown_loading: bool,
    pub breadcrumb_dropdown_generation: u64,
    pub breadcrumb_markdown_preview_area: Option<ratatui::layout::Rect>,
    pub diff_nav_prev_area: Option<ratatui::layout::Rect>,
    pub diff_nav_next_area: Option<ratatui::layout::Rect>,
    pub diff_layout_toggle_area: Option<ratatui::layout::Rect>,
    // Search
    pub search_active: bool,
    pub search_pre_cursor: Option<(usize, usize, usize)>, // (line, col, scroll_offset) before search
    pub search_query: String,
    pub search_cursor: usize,
    pub search_selection_anchor: Option<usize>,
    // Character offsets from the starting line; the end can extend into later lines.
    pub search_matches: Vec<(usize, usize, usize)>,
    pub search_error: Option<String>,
    pub search_truncated: bool,
    pub search_current: usize,
    /// The Find widget can remain visible while keyboard focus is in the editor.
    pub search_input_focused: bool,
    pub replace_mode: bool,
    pub replace_query: String,
    pub replace_cursor: usize,
    pub replace_selection_anchor: Option<usize>,
    pub search_focus_replace: bool, // true when replace input is focused
    pub search_case_sensitive: bool,
    pub search_regex: bool,
    pub search_whole_word: bool,
    pub search_in_selection: bool,
    pub search_preserve_case: bool,
    pub search_selection_range: Option<(usize, usize, usize, usize)>, // (start_line, start_col, end_line, end_col)
    pub(crate) search_history: crate::search_history::History,
    // Quick open (Ctrl+P)
    pub quick_open_active: bool,
    pub quick_open_query: String,
    pub quick_open_cursor: usize,
    pub quick_open_selection_anchor: Option<usize>,
    pub quick_open_results: Vec<PathBuf>,
    pub(crate) quick_open_index: crate::app_search::QuickOpenIndex,
    pub quick_open_selected: usize,
    pub git_branch_picker_active: bool,
    pub git_branch_picker_query: String,
    pub git_branch_picker_cursor: usize,
    pub git_branch_picker_selection_anchor: Option<usize>,
    pub git_branch_picker_items: Vec<GitBranchPickerItem>,
    pub git_branch_picker_selected: usize,
    pub git_branch_picker_scroll_offset: usize,
    pub recent_files: Vec<PathBuf>,
    // Go to line (Ctrl+G)
    pub goto_line_active: bool,
    pub goto_line_query: String,
    pub goto_line_cursor: usize,
    pub goto_line_selection_anchor: Option<usize>,
    pub goto_line_saved_selection: Option<crate::app_navigation::NavigationSelection>,
    pub goto_line_saved_scroll: Option<usize>,
    // Close confirm dialog
    pub confirm_close_active: bool,
    pub confirm_close_tab: usize,
    pub confirm_close_btn_areas: Vec<(u16, u16, u16)>,
    // Explorer delete confirmation dialog
    pub confirm_delete_active: bool,
    pub confirm_delete_target: Option<PathBuf>,
    pub confirm_delete_targets: Vec<PathBuf>,
    pub confirm_delete_btn_areas: Vec<(u16, u16, u16)>,
    pub confirm_delete_selected: usize,
    // Explorer drag-and-drop move confirmation dialog
    pub confirm_move_active: bool,
    pub confirm_move_source: Option<PathBuf>,
    pub confirm_move_destination: Option<PathBuf>,
    pub confirm_move_btn_areas: Vec<(u16, u16, u16)>,
    pub confirm_move_selected: usize,
    pub palette: PaletteState,
    // Context menu (right-click)
    pub context_menu_active: bool,
    pub context_menu_pos: (u16, u16),
    pub context_menu_inner: Option<ratatui::layout::Rect>,
    pub(crate) context_menu_button_drag: Option<ratatui::layout::Rect>,
    pub context_menu_selected: usize,
    pub(crate) context_menu_scroll: usize,
    pub context_menu_items: Vec<ContextMenuItem>,
    // VTComplete
    pub vtcomplete_active: bool,
    pub vtcomplete_suggestions: Vec<String>,
    pub(crate) vtcomplete_details: rustc_hash::FxHashMap<String, crate::app_vtcomplete::CompletionDetail>,
    pub vtcomplete_selected: usize,
    pub(crate) vtcomplete_explicit: bool,
    pub(crate) vtcomplete_navigated: bool,
    pub(crate) completion_index: crate::completion_index::CompletionIndex,
    pub(crate) completion_index_sync: crate::app_vtcomplete_index::State,
    pub(crate) completion_index_ui: crate::ui_vtcomplete_index::State,
    pub(crate) vtcomplete_generation: u64,
    pub(crate) vtcomplete_cancel_token: Option<crate::background::CancellationToken>,
    pub(crate) vtcomplete_worker: crate::app_vtcomplete::VtCompleteWorker,
    pub(crate) vtcomplete_input_batch: crate::app_vtcomplete::InputBatch,
    /// Active visible-range syntax job identity. Syntax work is demand-driven
    /// and worker-backed for all files rather than preloading whole documents.
    pub(crate) visible_highlight_key: Option<VisibleHighlightKey>,
    pub(crate) visible_highlight_cancel_token: Option<crate::background::CancellationToken>,
    /// Last viewport for which a fast provisional Syntect pass was submitted.
    /// Keeping this separate prevents each authoritative 40 ms slice from
    /// enqueuing duplicate provisional work while it advances toward the view.
    pub(crate) provisional_highlight_key: Option<VisibleHighlightKey>,
    pub(crate) provisional_highlight_cancel_token: Option<crate::background::CancellationToken>,
    /// Show completion suggestions automatically while typing. Explicit
    /// completion commands remain available when this is disabled.
    pub quick_suggestions: bool,
    // Explorer context menu
    pub explorer_context_active: bool,
    pub explorer_context_pos: (u16, u16),
    pub explorer_context_inner: Option<ratatui::layout::Rect>,
    pub explorer_context_selected: usize,
    pub explorer_context_target: Option<PathBuf>,
    pub explorer_context_extension_commands: Vec<crate::extensions::ExtensionContextCommand>,
    pub explorer_clipboard: Option<crate::app_explorer::FileClipboard>,
    pub explorer_multi_selected: crate::explorer::Selection,
    pub explorer_selection_anchor: Option<usize>,
    pub binary_open_prompt_active: bool,
    pub binary_open_prompt_path: Option<PathBuf>,
    pub binary_open_prompt_selected: usize,
    pub binary_open_prompt_commands: Vec<crate::extensions::ExtensionContextCommand>,
    pub binary_open_prompt_open_normally: bool,
    pub binary_open_prompt_skip_once: Option<PathBuf>,
    pub rename: RenameState,
    pub creating_new_file: bool,
    pub creating_new_folder: bool,
    // Save As dialog
    pub save_as_active: bool,
    pub(crate) modal: crate::ui_modal::ModalState,
    pub(crate) save_error: Option<String>,
    pub(crate) save_conflict: Option<crate::app_save_conflict::Conflict>,
    pub(crate) quick_picker: crate::ui_quick_picker::PickerState,
    pub save_as_query: String,
    pub save_as_cursor: usize,
    pub save_as_selection_anchor: Option<usize>,
    // Global search (Ctrl+Shift+F)
    pub global_search_active: bool,
    pub global_search_query: String,
    pub(crate) global_search_history: crate::search_history::History,
    pub global_search_error: Option<String>,
    pub global_search_replace_query: String,
    pub global_search_replace_visible: bool,
    pub global_search_results: crate::search_results::SearchResults,
    pub global_search_truncated: bool,
    pub global_search_selected: usize,
    pub global_search_scroll: usize,
    pub max_global_search_results: usize,
    /// Files larger than this many MiB skip rich analysis but remain editable.
    pub large_file_threshold_mb: u64,
    /// Maximum runtime for automatic rich analysis before it is paused.
    pub large_file_analysis_timeout_ms: u64,
    pub search_include_pattern: String,
    pub search_exclude_pattern: String,
    pub search_focus_field: u8, // 0=query, 1=replace, 2=include, 3=exclude
    pub global_search_field_cursors: [usize; 4], // byte cursors for query/replace/include/exclude
    pub global_search_field_selection_anchors: [Option<usize>; 4], // byte selection anchors
    pub hovered_global_search_close: bool,
    // Go to symbol (Ctrl+Shift+O)
    pub symbol_list_active: bool,
    pub symbol_list_query: String,
    pub symbol_list_cursor: usize,
    pub symbol_list_selection_anchor: Option<usize>,
    pub symbol_list_items: Vec<SymbolItem>,
    pub symbol_list_filtered: Vec<usize>,
    pub symbol_list_selected: usize,
    pub symbol_list_workspace: bool,
    pub(crate) source_symbol_work: crate::source_symbols::State,
    pub outline_items: Vec<SymbolItem>,
    pub outline_scroll: usize,
    pub outline_selected: usize,
    pub outline_hovered: Option<usize>,
    pub closed_tab_paths: std::collections::VecDeque<(PathBuf, usize, usize)>,
    pub dragging_tab: Option<usize>,
    pub dragging_tab_source_group: Option<usize>,
    pub dragging_terminal_tab: Option<usize>,
    pub dragging_explorer_path: Option<PathBuf>,
    pub drag_start_pos: Option<(u16, u16)>,
    pub drag_has_moved: bool,
    pub explorer_drop_target_idx: Option<usize>,
    pub dragging_text: bool,
    pub dragging_text_start: Option<(u16, u16)>,
    /// Document that received the press starting an ordinary editor selection.
    pub(crate) editor_selection_drag: Option<u64>,
    /// Suppresses drag/release handling after an Alt+mousedown cursor toggle.
    pub alt_cursor_mouse_down_active: bool,
    pub dragging_gutter: bool,
    pub dragging_gutter_start_line: Option<usize>,
    pub tab_drop_zone: Option<TabDropZone>,
    pub tab_drop_target_group: Option<usize>,
    pub about_info: Option<String>,
    pub shortcuts_active: bool,
    pub shortcuts_scroll: usize,
    pub zen_mode: bool,
    pub zen_pre_state: Option<(bool, bool, bool, bool, bool, bool)>, // (explorer, minimap, panel, tab_bar, status_bar, activity_bar)
    pub(crate) navigation: crate::app_navigation::NavigationHistory,
    // Ctrl+Tab switcher
    pub tab_switcher_active: bool,
    pub tab_switcher_selected: usize,
    pub tab_mru_order: Vec<usize>, // Tab indices in most-recently-used order
    // Settings panel (Ctrl+,)
    pub settings_active: bool,
    pub settings_selected: usize,
    pub settings_category_selected: usize,
    pub settings_section_selected: Option<(usize, &'static str)>,
    pub settings_category_scroll: usize,
    pub dragging_settings_categories: bool,
    pub settings_category_collapsed: [bool; crate::app_settings_ui::SETTINGS_CATEGORIES.len()],
    pub agent_settings: crate::ui_agent_settings::AgentSettingsState,
    pub lsp_settings: crate::ui_lsp_settings::LspSettingsState,
    pub lsp_enabled: bool,
    pub settings_category_area: Option<ratatui::layout::Rect>,
    pub settings_category_picker_area: Option<ratatui::layout::Rect>,
    pub settings_sidebar_area: Option<ratatui::layout::Rect>,
    pub settings_scroll_areas: Vec<ratatui::layout::Rect>, // Visible Settings panes, including unfocused splits.
    pub settings_focus_categories: bool,
    pub settings_focus_control: bool,
    pub settings_editor_active: bool,
    pub settings_editor_scroll: usize, // Rendered lines, including partial settings.
    pub(crate) settings_layout_cache: crate::settings_layout::Cache,
    pub(crate) settings_reveal_selected: bool,
    pub settings_editor_search: String,
    pub settings_editor_search_cursor: usize,
    pub settings_editor_search_selection_anchor: Option<usize>,
    pub settings_scope: crate::settings::SettingsScope,
    pub settings_scope_areas: [Option<ratatui::layout::Rect>; 2],
    pub settings_json_area: Option<ratatui::layout::Rect>,
    pub settings_clear_search_area: Option<ratatui::layout::Rect>,
    pub settings_value_editing: bool,
    pub settings_value_setting_id: Option<String>,
    pub settings_value_buffer: String,
    pub settings_value_cursor: usize,
    pub settings_value_selection_anchor: Option<usize>,
    pub settings_kb_editing: bool, // true when capturing a new keybinding in settings
    pub settings_dropdown_open: bool,
    pub settings_dropdown_setting_id: Option<String>, // None while open means the category picker.
    pub settings_dropdown_options: Vec<String>,
    pub settings_dropdown_selected: usize,
    pub settings_dropdown_scroll: usize,
    pub settings_dropdown_area: Option<ratatui::layout::Rect>,
    pub settings_dropdown_theme_original: Option<String>,
    pub settings_dropdown_committed: bool,
    pub(crate) theme_management: crate::app_theme_management::State,
    pub(crate) settings_defaults: crate::settings::Settings,
    pub(crate) settings_global: crate::settings::Settings,
    pub(crate) settings_workspace: crate::settings::Settings,
    pub(crate) settings_effective: crate::settings::Settings,
    pub(crate) settings_global_path: PathBuf,
    pub(crate) settings_workspace_path: PathBuf,
    pub(crate) settings_persistence_enabled: bool,
    pub trim_trailing_on_save: bool,
    pub format_on_save: bool,
    pub insert_final_newline: bool,
    pub auto_save: AutoSaveMode,
    pub auto_save_delay: u64,
    pub explorer_width: u16,
    pub resizing_explorer: bool,
    pub peek_definition: Option<PeekDefinition>,
    pub blame_cache: Option<(PathBuf, std::sync::Arc<crate::git_blame::Document>)>,
    pub(crate) blame_documents: rustc_hash::FxHashMap<u64, std::sync::Arc<crate::git_blame::Document>>,
    pub(crate) inline_blame_hover: Option<crate::ui_inline_blame::Hover>,
    pub blame_cache_key: Option<crate::background::BlameCacheKey>,
    pub blame_requested_path: Option<PathBuf>,
    pub bg_blame_running: bool,
    pub disk_check_last: std::time::Instant,
    pub disk_check_index: usize,
    pub panel_tab: u8, // 1=Problems, 2=Output, 3=Notifications
    pub(crate) panel_chrome: crate::ui_panels::Chrome,
    pub terminal_panel_focused: bool,
    pub panel_height: u16,
    pub problem_scroll: usize,
    pub resizing_panel: bool,
    pub panel_area: Option<ratatui::layout::Rect>,
    /// The terminal's default home: an independently collapsible right-side dock.
    pub show_terminal_sidebar: bool,
    pub terminal_sidebar_revealed: bool,
    pub terminal_sidebar_area: Option<ratatui::layout::Rect>,
    pub terminal_sidebar_toggle_area: Option<ratatui::layout::Rect>,
    pub terminal_sidebar_plus_area: Option<ratatui::layout::Rect>,
    pub terminal_sidebar_tab_areas: Vec<(ratatui::layout::Rect, usize)>,
    pub terminal_sidebar_close_areas: Vec<(ratatui::layout::Rect, usize)>,
    /// Full body area used to expose right/bottom terminal drag targets even
    /// while the corresponding dock is collapsed.
    pub workbench_body_area: Option<ratatui::layout::Rect>,
    pub terminal_dock_drop_target: Option<TerminalDock>,
    pub terminals: Vec<TerminalTab>,
    pub active_terminal: usize,
    pub(crate) next_terminal_to_process: usize,
    pub(crate) terminal_surfaces: Vec<crate::app_terminal_selection::TerminalSurface>,
    pub(crate) terminal_selection_drag: Option<crate::app_terminal_selection::TerminalSurface>,
    pub(crate) terminal_mouse_capture: Option<crate::app_terminal_selection::TerminalSurface>,
    pub task_list: Vec<TaskDefinition>,
    pub last_task: Option<String>,
    pub task_picker_active: bool,
    pub task_picker_selected: usize,
    pub task_picker_query: String,
    pub task_picker_cursor: usize,
    pub task_picker_selection_anchor: Option<usize>,
    pub markdown_preview: bool,
    pub hovered_tab: Option<usize>,
    pub hovered_tab_close_btn: bool,
    pub hovered_tab_scroll_left: bool,
    pub hovered_tab_scroll_right: bool,
    pub tab_tooltip_hover_start: Option<std::time::Instant>,
    pub tab_tooltip_tab: Option<usize>,
    pub(crate) tab_tooltip_shown: bool,
    pub hovered_activity: Option<usize>,
    pub hovered_menu: Option<usize>,
    pub hovered_status_item: Option<usize>,
    pub hovered_explorer: Option<usize>,
    pub explorer_line_count_cache: rustc_hash::FxHashMap<PathBuf, usize>,
    pub(crate) explorer_hover_info_cache:
        rustc_hash::FxHashMap<PathBuf, crate::background::ExplorerHoverInfo>,
    pub(crate) explorer_hover: crate::app_explorer_hover::State,
    pub hovered_close_button: bool,
    pub hovered_panel_close: bool,
    pub hovered_panel_tab: Option<u8>,
    pub hovered_problem_row: Option<usize>,
    pub hovered_terminal_plus: bool,
    pub hovered_terminal_tab: Option<usize>,
    pub hovered_terminal_close: Option<usize>,
    pub hovered_terminal_sidebar_toggle: bool,
    pub hovered_new_tab_button: bool,
    pub hovered_scrollbar: bool,
    pub hovered_minimap: bool,
    pub hovered_h_scrollbar: bool,
    pub hovered_search_toggle: Option<usize>, // 0=Aa, 1=W, 2=.*, 3=⊞, 4=AB(preserve case)
    pub search_control_regions: Vec<crate::ui_search_controls::SearchControlRegion>,
    pub search_bar_area: Option<ratatui::layout::Rect>,
    pub(crate) search_bar_layout: crate::ui_search_dialog::SearchBarLayout,
    pub rendering_inactive_editor: bool,
    pub saving_flash: Option<std::time::Instant>,
    pub mouse_row: u16,
    pub mouse_col: u16,
    pub mouse_hover_changed: bool,
    /// Cached diagnostics for the current file. Rendering reuses both the
    /// detailed list and its compact minimap projection until either provider
    /// publishes a new revision.
    pub cached_diag_lines: std::sync::Arc<Vec<(usize, u8)>>,
    pub cached_diag_key: (Option<PathBuf>, u64, u64),
    pub(crate) cached_editor_diagnostics: std::sync::Arc<Vec<crate::lsp::LspDiagnostic>>,
    pub(crate) cached_diagnostic_index:
        std::sync::Arc<crate::ui_editor_minimap::DiagnosticLineIndex>,
    /// Diagnostic indices keyed by every inexpensive line span they cover.
    /// This keeps the per-frame editor loop proportional to visible matches,
    /// even when a language server publishes thousands of diagnostics.
    pub(crate) cached_diag_indices_by_line:
        std::sync::Arc<rustc_hash::FxHashMap<usize, Vec<usize>>>,
    /// Exceptionally wide diagnostic ranges are queried separately so a
    /// malformed provider response cannot inflate the cache across a file.
    pub(crate) cached_wide_diag_indices: std::sync::Arc<Vec<usize>>,
    pub(crate) editor_diagnostic_cache:
        std::collections::VecDeque<crate::ui_editor::EditorDiagnosticCacheEntry>,
    /// Quarter-cell scrollbar coverage, cached independently of the text minimap.
    pub(crate) cached_overview_change_rows: std::sync::Arc<crate::ui_editor_minimap::ChangeRows>,
    pub(crate) cached_overview_change_rows_key: Option<OverviewChangeRowsKey>,
    pub(crate) editor_overview_cache: std::collections::VecDeque<(
        OverviewChangeRowsKey,
        std::sync::Arc<crate::ui_editor_minimap::ChangeRows>,
    )>,
    pub(crate) cached_editor_change_index:
        std::sync::Arc<crate::ui_editor_minimap::ChangeLineIndex>,
    pub(crate) editor_change_index_cache:
        std::collections::VecDeque<crate::ui_editor::EditorChangeIndexEntry>,
    pub exit_confirm_buttons: Vec<(u16, u16, u16)>, // (x, y, width) for each button
    pub diff_mode: DiffMode,
    /// Last chosen diff layout, retained when closing or switching comparisons.
    pub(crate) preferred_diff_mode: DiffMode,
    pub(crate) diff_inline_scroll: crate::inline_diff::InlineDiffScroll,
    pub(crate) diff_selection: crate::diff_selection::DiffSelection,
    pub(crate) diff_scope_source: crate::diff_scope_source::DiffScopeSource,
    pub(crate) diff_tabs: crate::diff_tabs::DiffTabs,
    pub(crate) selected_compare_path: Option<PathBuf>, // file chosen by Select for Compare
    pub compare_base_path: Option<PathBuf>, // live base file for compare views that should refresh from disk/tab state
    pub compare_base_content: Option<std::sync::Arc<str>>, // shared snapshot of the compare base
    /// Rope snapshot used only until an unsaved compare base is flattened by a worker.
    pub(crate) pending_compare_base_rope: Option<ropey::Rope>,
    pub compare_diff_lines: Vec<(char, String)>, // computed diff: ' ' same, '+' added, '-' removed
    pub(crate) diff_render_cache: std::sync::Arc<DiffRenderCache>,
    pub(crate) compare_diff_target_version: Option<u64>,
    pub(crate) compare_diff_base_revision: Option<DiffBaseRevision>,
    pub diff_tab_label: Option<String>, // e.g. "file1.txt ↔ file2.txt"
    pub(crate) diff_labels: Option<DiffLabels>, // sources shown above the base and target
    pub diff_target_path: Option<PathBuf>, // file currently shown as diff target
    /// File waiting to enter compare-with-HEAD mode. Large files remain here
    /// until their asynchronous file load has completed.
    pub(crate) pending_git_compare_path: Option<PathBuf>,
    /// Invalidates compare jobs when another file/view supersedes them.
    pub(crate) git_compare_generation: u64,
    /// True while the current generation is being prepared by a worker.
    pub(crate) bg_git_compare_running: bool,
    /// Invalidates asynchronous refreshes of an already-open diff.
    pub(crate) diff_refresh_generation: u64,
    /// True while a worker is rebuilding the active diff snapshot.
    pub(crate) bg_diff_refresh_running: bool,
    pub clipboard_history: std::collections::VecDeque<String>,
    pub clipboard_cycle_active: bool,
    pub clipboard_cycle_index: usize,
    pub(crate) bulk_paste: crate::app_paste::State,
    pub colors: crate::app_theme::ThemeColors,
    pub loaded_themes: Vec<crate::app_theme::ThemeFile>,
    pub theme_key: String,
    pub(crate) startup_theme_placeholder: Option<String>,
    pub theme_name: String,
    pub syntax_theme: String,
    /// Highlight registry generation last applied by the workbench theme.
    pub(crate) applied_theme_generation: Option<u64>,
    pub semantic_theme_colors: Option<rustc_hash::FxHashMap<String, (u8, u8, u8)>>,
    pub focus_mode: bool,
    pub sidebar_right: bool,
    pub last_edit_position: Option<(usize, usize, usize)>, // (tab_idx, line, col)
    pub last_edit_time: std::time::Instant,
    pub show_word_count: bool,
    pub show_color_preview: bool,
    pub indent_rainbow: bool,
    pub bracket_pair_colorization: bool,
    pub error_lens: bool,
    pub sticky_scroll: bool,
    pub sticky_scroll_max: usize,
    // Screencast mode: show recent key presses as floating overlay
    pub screencast_mode: bool,
    pub screencast_keys:
        std::collections::VecDeque<(crossterm::event::KeyEvent, std::time::Instant)>,
    pub scroll_past_end: bool,
    pub default_tab_size: usize,
    pub default_use_tabs: bool,
    pub default_word_wrap: bool,
    pub default_whitespace_mode: WhitespaceMode,
    /// None inherits each editor's general whitespace mode.
    pub(crate) comment_whitespace_mode: Option<WhitespaceMode>,
    pub default_read_only: bool,
    pub default_cursor_surrounding_lines: usize,
    /// Word wrap column (0 = viewport width, >0 = fixed column)
    pub word_wrap_column: usize,
    // Call hierarchy state
    pub call_hierarchy_active: bool,
    pub call_hierarchy_mode: CallHierarchyMode,
    pub call_hierarchy_results: Vec<CallHierarchyEntry>,
    pub call_hierarchy_selected: usize,
    pub call_hierarchy_root_name: String,
    // Type hierarchy state
    pub type_hierarchy_active: bool,
    pub type_hierarchy_mode: TypeHierarchyMode,
    pub type_hierarchy_results: Vec<CallHierarchyEntry>,
    pub type_hierarchy_selected: usize,
    pub type_hierarchy_root_name: String,
    pub macro_recording: bool,
    pub macro_buffer: Vec<crossterm::event::KeyEvent>,
    pub macro_saved: Vec<crossterm::event::KeyEvent>,
    pub macro_replay_queue: std::collections::VecDeque<crossterm::event::KeyEvent>,
    pub exit_confirm_active: bool,
    pub(crate) exit_backup_warning: Option<crate::app_exit_backup::Warning>,
    pub(crate) exit_recovery: crate::app_exit_backup::RecoveryState,
    pub exit_save_and_quit: bool,
    pub exit_discard_quit: bool,
    pub exit_keep_unsaved_quit: bool,
    pub(crate) custom_exit_started: bool,
    pub(crate) custom_exit_remaining: usize,
    pub(crate) custom_exit_failed: bool,
    pub(crate) pending_custom_recoveries: Vec<crate::extension_recovery::RecoveryRecord>,
    pub change_dir_active: bool,
    pub change_dir_query: String,
    pub(crate) folder_browser: crate::folder_browser::FolderBrowser,
    pub folder_browser_entries: Vec<std::path::PathBuf>,
    pub(crate) folder_browser_loading: bool,
    pub folder_browser_selected: usize,
    pub folder_browser_scroll: usize,
    pub folder_browser_path: std::path::PathBuf,
    pub folder_browser_area: Option<ratatui::layout::Rect>,
    pub folder_browser_buttons: Vec<(u16, u16, u16)>,
    pub middle_drag_start: Option<(usize, usize, usize)>, // (line, col, visual_col) for column selection
    pub(crate) block_drag_cache: Option<crate::mouse_drag::BlockDragCache>,
    pub source_control_repository_state: SourceControlRepositoryState,
    pub(crate) repositories: crate::git_repositories::Repositories,
    pub(crate) source_control_enabled: bool,
    pub(crate) source_control_cancel: crate::background::CancellationToken,
    pub(crate) source_control_toggle_area: Option<ratatui::layout::Rect>,
    pub(crate) source_control_last_refresh: Option<std::time::Instant>,
    pub source_control_initialize_area: Option<ratatui::layout::Rect>,
    pub source_control_commit_area: Option<ratatui::layout::Rect>,
    pub source_control_amend_area: Option<ratatui::layout::Rect>,
    pub source_control_stage_all_area: Option<ratatui::layout::Rect>,
    pub source_control_unstage_all_area: Option<ratatui::layout::Rect>,
    pub source_control_discard_all_area: Option<ratatui::layout::Rect>,
    pub(crate) source_control_untracked_stage_area: Option<ratatui::layout::Rect>,
    pub(crate) source_control_untracked_discard_area: Option<ratatui::layout::Rect>,
    pub(crate) source_control_actions_area: Option<ratatui::layout::Rect>,
    /// Git status ordered merge, staged, unstaged, untracked, then path within each group.
    pub source_control_files: Vec<GitFileChange>,
    pub(crate) source_control_selection: crate::app_source_control::SourceControlSelection,
    pub source_control_selected: usize,
    pub sc_context_active: bool,
    pub sc_context_pos: (u16, u16),
    pub(crate) sc_context_inner: Option<ratatui::layout::Rect>,
    pub sc_context_selected: usize,
    pub sc_context_target: Option<GitFileChange>,
    pub(crate) sc_context_targets: Vec<GitFileChange>,
    pub hunk_popup_active: bool,
    pub hunk_popup_pos: (u16, u16),
    pub hunk_popup_selected: usize,
    pub hunk_popup_file: Option<PathBuf>,
    pub hunk_popup_line: usize,
    pub hunk_popup_status: char,
    pub(crate) hunk_peek: Option<crate::ui_hunk_peek::HunkPeek>,
    pub(crate) hunk_popup_generation: u64,
    pub sc_discard_confirm_active: bool,
    pub(crate) sc_discard_request: Option<crate::app_source_control::DiscardRequest>,
    pub sc_discard_confirm_btn_areas: Vec<(u16, u16, u16)>,
    pub(crate) sc_stage_all_commit_request: Option<crate::app_git::GitCommitRequest>,
    pub source_control_commit_msg: String,
    pub source_control_commit_cursor: usize, // byte cursor position in commit msg
    pub(crate) commit_input: crate::commit_input::CommitInputState,
    pub source_control_commit_selection_anchor: Option<usize>,
    pub source_control_input_mode: bool, // true = typing commit msg
    /// Rendered-row offset in the filtered changes list (including headers).
    pub source_control_scroll: usize,
    pub(crate) sc_merge_collapsed: bool,
    pub sc_staged_collapsed: bool,
    pub sc_changes_collapsed: bool,
    pub(crate) sc_untracked_collapsed: bool,
    pub(crate) sc_visibility: crate::source_control_rows::Visibility,
    pub sc_graph_collapsed: bool,
    pub sc_graph_height: u16,
    pub resizing_sc_graph: bool,
    pub sc_graph_click_start: Option<(u16, u16)>,
    pub sc_amend_mode: bool,
    pub git_log: Vec<GitCommitInfo>,
    pub(crate) git_history: crate::git_history::GitHistory,
    pub git_log_scroll: usize,
    pub git_log_selected: usize,
    pub(crate) commit_details: crate::commit_details::CommitDetails,
    pub(crate) sc_graph_focused: bool,
    // Keybindings
    pub keybindings: Vec<KeyBinding>,
    pub(crate) keybindings_ui: crate::ui_keybindings::State,
    pub keybindings_selected: usize,
    pub keybindings_editing: bool, // waiting for new key combo
    /// Cached keybinding settings items (label, value, id).
    /// Rebuilt when keybindings are loaded or modified.
    pub keybinding_settings_cache: Vec<(String, String, String)>,
    // Menu bar
    pub menu_active: bool,
    pub menu_selected: usize,      // which top-level menu is selected
    pub menu_open: bool,           // dropdown is open
    pub menu_item_selected: usize, // which item in dropdown
    // Activity bar
    pub activity_bar_selected: usize, // 0=Explorer, 1=Search, 2=SourceControl, 4=Extensions, 5=Outline, usize::MAX=None
    pub activity_bar_area: Option<ratatui::layout::Rect>,
    pub menu_bar_area: Option<ratatui::layout::Rect>,
    pub zen_exit_area: Option<ratatui::layout::Rect>,
    // Tab context menu (right-click)
    pub tab_context_active: bool,
    pub tab_context_pos: (u16, u16),
    pub tab_context_inner: Option<ratatui::layout::Rect>,
    pub tab_context_tab: usize,
    pub tab_context_selected: usize,
    // Performance caches (avoid expensive per-frame recomputation)
    pub cached_git_branch: String,
    pub cached_error_count: usize,
    pub cached_warning_count: usize,
    pub cached_max_line_width: usize,
    pub cached_editor_word_count: usize,
    pub cached_editor_analysis_target: Option<crate::background::EditorAnalysisTarget>,
    pub(crate) content_analysis_cache: std::sync::Arc<std::sync::Mutex<crate::app_event_loop::content_analysis::Cache>>,
    // Cached word occurrence count for status bar (invalidated on cursor/content change)
    pub cached_word_occ: Option<String>,
    pub cached_word_occ_target: Option<crate::background::WordOccurrenceTarget>,
    // Cached word count (invalidated on content change)
    pub cached_word_count: Option<String>,
    // Cached selection stats (invalidated on selection/content change)
    pub cached_sel_stats: Option<String>,
    pub cached_sel_stats_key: Option<(usize, usize)>,
    // Cached problems panel scan results (invalidated on content change)
    pub cached_problems: Vec<crate::background::ProblemItem>,
    pub(crate) cached_problems_target: Option<crate::background::ProblemsAnalysisTarget>,
    pub cached_problems_error_count: usize,
    pub cached_problems_warning_count: usize,
    pub cached_problems_info_count: usize,
    pub cached_problems_file_count: usize,
    // Cached sticky scroll lines (invalidated on scroll/content change)
    pub cached_sticky_lines: Vec<(usize, String)>,
    pub cached_sticky_key: u64,
    pub(crate) cached_sticky_scopes: std::sync::Arc<[crate::background::StickyScopeLine]>,
    // Cached bracket guides (invalidated on scroll/content change)
    pub(crate) cached_bracket_guides: crate::ui_editor_highlight::BracketGuides,
    pub(crate) cached_bracket_analysis_target: Option<crate::background::BracketAnalysisTarget>,
    pub(crate) cached_bracket_depth_index:
        Option<std::sync::Arc<crate::background::BracketDepthIndex>>,
    pub(crate) cached_bracket_starting_depth: i32,
    pub(crate) cached_structural_scope_index:
        Option<std::sync::Arc<crate::animation_structural_scope::StructuralScopeIndex>>,
    pub(crate) pending_structural_scope:
        Option<crate::animation_structural_scope::StructuralScopeRequest>,
    pub(crate) structural_scope_cancel: Option<crate::background::CancellationToken>,
    /// Cursor decorations computed off-thread. Same-editor/same-caret values
    /// remain renderable across content versions until their replacement lands.
    pub(crate) cached_cursor_match_target: Option<crate::background::CursorMatchTarget>,
    pub(crate) cached_matching_bracket: Option<(usize, usize)>,
    pub(crate) cached_matching_tag: Option<(usize, usize, usize, usize, usize, usize)>,
    // Cached word-at-cursor for occurrence highlighting (invalidated on cursor/content change)
    pub cached_cursor_word: Option<std::sync::Arc<str>>,
    pub cached_cursor_word_key: CursorWordKey, // (editor identity, cursor_line, cursor_col, content_version)
    pub(crate) editor_cursor_word_cache:
        std::cell::RefCell<std::collections::VecDeque<crate::ui_editor::CursorWordEntry>>,
    pub(crate) editor_selection_word_cache:
        std::collections::VecDeque<crate::ui_editor::SelectionWordEntry>,
    // Shutdown animation
    pub shutdown_anim_active: bool,
    pub shutdown_anim_start: std::time::Instant,
    // Extensions
    pub extension_manager: crate::extensions::ExtensionManager,
    pub(crate) extension_pointer_capture: Option<crate::extension_views::PointerCapture>,
    pub extensions_selected: usize,
    pub extensions_scroll: usize,
    pub extensions_filter: String,
    pub extensions_filter_cursor: usize,
    pub extensions_filter_selection_anchor: Option<usize>,
    pub ext_sidebar: crate::ui_extensions::Sidebar,
    pub ext_info: crate::ui_extension_info::State,
    pub extension_palette_action_map: rustc_hash::FxHashMap<usize, ExtensionPaletteAction>,
    /// Active extension overlays with their extension IDs and drag state
    pub extension_overlay_drag: Option<(String, String, i16, i16)>, // (ext_id, overlay_id, offset_x, offset_y)
    pub extension_overlay_last_tick: rustc_hash::FxHashMap<String, rustc_hash::FxHashMap<String, std::time::Instant>>,
    // LSP (Language Server Protocol)
    pub lsp_manager: crate::lsp::LspManager,
    pub pending_lsp_workspace_edits: std::collections::VecDeque<crate::app_lsp_workspace_edit::PendingWorkspaceEdit>,
    pub lsp_suggestions: crate::app_lsp_interactive::LspSuggestions,
    /// Active LSP completion popup state
    pub lsp_completion_active: bool,
    pub lsp_completion_selected: usize,
    pub _lsp_completion_filter: String,
    pub(crate) completion_ui: crate::ui_lsp_completion::State,
    /// Active LSP hover tooltip state
    pub hover_enabled: bool,
    pub lsp_hover_active: bool,
    /// Keyboard/caret interaction suppresses hover until the pointer moves again.
    pub lsp_hover_suppressed: bool,
    pub lsp_hover_line: usize,
    pub lsp_hover_col: usize,
    pub lsp_hover_request_time: Option<std::time::Instant>,
    pub lsp_hover_pending_pos: Option<(usize, usize)>,
    pub(crate) lsp_link_hover: crate::app_lsp_links::LinkHover,
    /// Pending go-to-definition navigation
    pub lsp_goto_pending: bool,
    /// Active LSP signature help state
    pub lsp_signature_active: bool,
    /// Active LSP code action menu
    pub lsp_code_action_active: bool,
    pub lsp_code_action_selected: usize,
    /// Pending LSP format document request
    pub lsp_format_pending: bool,
    pub lsp_format_target_path: Option<PathBuf>,
    pub lsp_save_after_format_path: Option<PathBuf>,
    pub(crate) formatting: crate::app_lsp_format::FormattingState,
    pub(crate) formatters: std::collections::BTreeMap<String, crate::formatters::Config>,
    pub(crate) formatter_settings: crate::ui_formatter_settings::State,
    pub(crate) file_associations: crate::ui_file_associations::State,
    pub(crate) scope_bubble_settings: crate::ui_scope_bubble_settings::State,
    /// Pending LSP rename request
    pub lsp_rename_pending: bool,
    /// Pending LSP prepareRename request
    pub lsp_prepare_rename_pending: bool,
    /// Pending LSP references request
    pub lsp_references_pending: bool,
    /// Whether the references popup is shown
    pub lsp_references_active: bool,
    /// Selected reference in the references popup
    pub lsp_references_selected: usize,
    /// Cached inlay hints for the active editor, keyed by (line, character)
    pub lsp_inlay_hints: Vec<InlayHintDisplay>,
    /// Last scroll offset when inlay hints were requested (to re-request on scroll)
    pub lsp_inlay_hint_last_scroll: usize,
    /// Last content version when inlay hints were requested
    pub lsp_inlay_hint_last_version: u64,
    /// Whether inlay hints are enabled
    pub show_inlay_hints: bool,
    /// Decoded semantic tokens for the current file (line, start_char, length, type_idx, modifiers)
    pub lsp_semantic_tokens: Vec<crate::lsp::DecodedSemanticToken>,
    /// Last content version when semantic tokens were requested
    pub lsp_semantic_tokens_last_version: u64,
    /// Exact editor snapshot associated with the outstanding semantic-token
    /// request. A content version alone is not unique across tabs.
    pub(crate) lsp_semantic_tokens_request_target:
        Option<crate::background::EditorAnalysisTarget>,
    /// Whether semantic token highlighting is enabled
    pub show_semantic_tokens: bool,
    /// Cached document highlights for the current cursor position
    pub lsp_document_highlights: Vec<(u32, u32, u32, u32)>, // (start_line, start_col, end_line, end_col)
    /// Multiline subset kept separately so ordinary single-line symbol
    /// highlights can be found by binary search during rendering.
    pub(crate) lsp_document_multiline_highlights: Vec<(u32, u32, u32, u32)>,
    /// Scalar cursor corresponding to the manager's UTF-16 highlight request.
    /// Avoid converting unchanged long lines on every idle/animation frame.
    pub(crate) lsp_highlight_last_cursor: (usize, usize),
    /// Last content version when LSP document symbols were requested for outline
    pub lsp_outline_last_version: u64,
    /// Whether LSP document symbols are available (to skip regex fallback)
    pub lsp_outline_available: bool,
    /// Last content version when LSP folding ranges were requested
    pub lsp_folding_last_version: u64,
    /// LSP folding ranges stored as (start_line, end_line) pairs
    pub lsp_folding_ranges: Vec<(usize, usize)>,
    /// LSP selection ranges for smart expand/shrink - flattened from innermost to outermost
    /// Each entry is (start_line, start_col, end_line, end_col)
    pub lsp_selection_ranges: Vec<(usize, usize, usize, usize)>,
    /// Last cursor position produced by expansion/shrinking in this request.
    pub lsp_selection_range_cursor: (usize, usize),
    /// Exact document behind both pending and cached selection ranges.
    pub(crate) lsp_selection_range_source: Option<(u64, PathBuf, ropey::Rope)>,
    /// LSP code lenses: (line, label) per line
    pub lsp_code_lenses: Vec<(usize, String)>,
    /// Raw code lenses from LSP (for resolve)
    pub lsp_code_lenses_raw: Vec<lsp_types::CodeLens>,
    /// Last content version when code lenses were requested
    pub lsp_code_lens_last_version: u64,
    /// Whether to show code lenses
    pub show_code_lenses: bool,
    /// Document links from LSP (line, start_col, end_col, target_url)
    pub lsp_document_links: Vec<(usize, usize, usize, String)>,
    /// Last content version when document links were requested
    pub lsp_document_links_last_version: u64,
    /// Whether on-type formatting is enabled
    pub on_type_formatting_enabled: bool,
    /// Document content version associated with the active on-type format request.
    pub lsp_on_type_format_target: Option<crate::background::LspDocumentTarget>,
    /// On-type request waiting for the exact document snapshot to be flattened
    /// by a worker and queued ahead of the formatting request.
    pub(crate) pending_lsp_on_type_format: Option<(PathBuf, u32, u32, String, u32, bool, u64)>,
    // Background worker system for async operations
    pub background: crate::background::BackgroundWorkers,
    /// True while a background git-status refresh is in flight.
    pub bg_git_status_running: bool,
    /// A newer git-status refresh was requested while one was in flight.
    pub bg_git_status_pending: bool,
    pub(crate) pending_git_status_paths: Option<rustc_hash::FxHashSet<PathBuf>>,
    /// True while the visible Explorer tree is being rebuilt.
    pub(crate) bg_explorer_refresh_running: bool,
    /// A newer tree refresh was requested while one was in flight.
    pub(crate) bg_explorer_refresh_pending: bool,
    pub(crate) explorer_refresh_generation: u64,
    pub(crate) explorer_refresh_active_generation: Option<u64>,
    /// Only filesystem changes invalidate the immutable Quick Open index.
    /// Folder expansion, sorting and selection affect the visible tree alone.
    pub(crate) workspace_file_index_generation: u64,
    /// True while extension manifests are being discovered on a worker.
    pub(crate) bg_extension_scan_running: bool,
    pub(crate) bg_extension_scan_pending: bool,
    pub(crate) extension_scan_generation: u64,
    /// Themes explicitly installed into vtcode are discovered only after the first
    /// workbench frame so filesystem traversal never delays visual startup.
    pub(crate) bg_external_theme_scan_running: bool,
    /// Grammar validation/build runs after the startup screen or on demand.
    pub(crate) bg_syntax_asset_refresh_running: bool,
    pub(crate) bg_syntax_asset_refresh_pending: bool,
    pub(crate) syntax_reload_requested: bool,
    /// True while a background git-branch refresh is in flight.
    pub bg_git_branch_running: bool,
    pub(crate) bg_git_branch_pending: bool,
    /// True while a background git-log refresh is in flight.
    pub bg_git_log_running: bool,
    /// True while a background git fetch/pull/push is in flight.
    pub bg_git_remote_running: bool,
    /// Serializes repository mutations such as stage, commit, checkout, and stash.
    pub(crate) bg_git_action_running: bool,
    pub(crate) bg_git_branch_picker_running: bool,
    pub(crate) bg_filesystem_action_running: bool,
    pub(crate) file_deletion: Option<crate::file_deletion::Deletion>,
    /// True while a background global search is in flight.
    pub bg_global_search_running: bool,
    /// True while the active file's find matches are being scanned.
    pub(crate) bg_file_search_running: bool,
    pub(crate) file_search_generation: u64,
    pub(crate) file_search_cancel_token: Option<crate::background::CancellationToken>,
    /// Exact editor snapshot represented by `search_matches`.
    pub(crate) file_search_matches_target: Option<crate::background::EditorAnalysisTarget>,
    pub(crate) file_search_key: Option<crate::app_search_algo::FileSearchKey>,
    /// A large/pathological edit needs one full scan after typing settles.
    pub(crate) file_search_refresh_pending: bool,
    pub(crate) bg_file_replace_running: bool,
    pub(crate) file_replace_generation: u64,
    pub(crate) bg_global_replace_running: bool,
    pub(crate) global_replace_generation: u64,
    pub(crate) workspace_replace: crate::workspace_replace::State,
    /// Generation counter for search debouncing - incremented on each new search
    pub search_generation: u64,
    /// Cancellation token for the current global search (cancel old search when new one starts)
    pub search_cancel_token: Option<crate::background::CancellationToken>,
    /// True while a background quick-open file collection is in flight.
    pub bg_quick_open_running: bool,
    /// Generation counter for quick-open debouncing
    pub quick_open_generation: u64,
    /// Cancellation token for the current quick-open collection
    pub quick_open_cancel_token: Option<crate::background::CancellationToken>,
    /// True while a background git line diff is in flight.
    pub bg_git_diff_running: bool,
    /// A newer active-file line diff was requested while one was in flight.
    pub bg_git_diff_pending: bool,
    /// True while exact content-derived editor caches are being computed.
    pub bg_editor_analysis_running: bool,
    pub(crate) pending_editor_analysis: Option<(crate::background::EditorAnalysisTarget, crate::background::CancellationToken)>,
    /// A modified editor needs whole-file analysis once the typing quiet
    /// period expires. Keeping this explicit lets the event loop wake the
    /// worker without manufacturing redraws while the screen is stable.
    pub(crate) editor_analysis_deferred: bool,
    /// True while viewport bracket guides are being prepared.
    pub(crate) bg_bracket_analysis_running: bool,
    pub(crate) pending_bracket_analysis: Option<(crate::background::BracketAnalysisTarget, crate::background::CancellationToken)>,
    /// True while matching bracket/tag decorations are being prepared.
    pub(crate) bg_cursor_match_running: bool,
    pub(crate) pending_cursor_match: Option<(crate::background::CursorMatchTarget, crate::background::CancellationToken)>,
    /// True while the Problems panel marker list is being scanned.
    pub(crate) bg_problems_analysis_running: bool,
    /// True while an exact word-occurrence count is being computed.
    pub bg_word_occ_running: bool,
    /// Cursor movement coalesces the optional whole-document occurrence count
    /// until input settles, keeping worker capacity available for rendering.
    pub(crate) word_occurrence_deferred: bool,
    /// Latest shared LSP snapshot per path, coalesced until the next poll.
    /// Diffing and serialization run on each language server's writer thread.
    pending_lsp_syncs: rustc_hash::FxHashMap<PathBuf, (u64, ropey::Rope)>,
    pending_lsp_did_save: rustc_hash::FxHashSet<PathBuf>,
    pending_lsp_semantic_decode: Option<(
        crate::background::EditorAnalysisTarget,
        crate::background::CancellationToken,
    )>,
    /// Latest whole-document word-occurrence request. Cursor motion cancels
    /// obsolete scans so they cannot compete with syntax/layout workers.
    pub(crate) pending_word_occurrence: Option<(
        crate::background::WordOccurrenceTarget,
        crate::background::CancellationToken,
    )>,
    /// Latest wrap request for each visible pane. Stable identities let a
    /// request survive focus changes and tab/group moves during resizing.
    pub(crate) pending_wrap_layouts:
        rustc_hash::FxHashMap<u64, crate::app_wrap::PendingWrapLayout>,
    /// Serialize writes per path and retain only the newest snapshot requested
    /// while an earlier save is still in flight.
    pub(crate) pending_file_saves: rustc_hash::FxHashMap<PathBuf, (u64, crate::editor::SaveIntent)>,
    pub(crate) deferred_file_saves:
        rustc_hash::FxHashMap<PathBuf, (u64, crate::editor::BackgroundSaveRequest)>,
    /// Save As adopts its destination only after a successful write. Keyed by
    /// document identity so untitled and shared views retain their original
    /// path, disk checkpoint and undo history if the write fails.
    pub(crate) pending_save_as: rustc_hash::FxHashMap<u64, (u64, PathBuf)>,
    /// Last editor version submitted by delayed auto-save for each path. A
    /// failed write must not turn an expired timer into an immediate retry
    /// loop; the next content version becomes eligible again automatically.
    pub(crate) auto_save_attempted_versions: rustc_hash::FxHashMap<PathBuf, u64>,
    /// Paths currently being read for an automatic external-change reload.
    pub(crate) pending_file_reloads: rustc_hash::FxHashSet<PathBuf>,
    /// A newer change arrived while the corresponding path was being read.
    pub(crate) pending_file_reload_again: rustc_hash::FxHashSet<PathBuf>,
    /// Metadata probes currently running outside the event loop.
    pending_disk_checks: rustc_hash::FxHashSet<PathBuf>,
    remote_disk_checks: RemoteDiskChecks,
    remote_changes_deferred: rustc_hash::FxHashSet<PathBuf>,
    pub(crate) remote_change_warnings: rustc_hash::FxHashSet<PathBuf>,
    /// Native watcher coverage lets workspace files avoid periodic metadata
    /// probes entirely. Unwatched/external files retain a low-rate fallback.
    pub(crate) workspace_watcher_active: bool,
    /// Frame counter for batching background worker reaping
    reap_frame_counter: u32,
}

pub(crate) use crate::app_theme::EditorTheme;

impl App {
    pub(crate) const fn normalize_panel_tab(index: u8) -> u8 {
        if index == 2 || index == 3 { index } else { 1 }
    }

    #[cfg(test)]
    pub(crate) fn new(root: PathBuf) -> Self {
        Self::new_with_explorer(root, false)
    }

    /// Test constructor using the same deferred assets as production startup.
    #[cfg(test)]
    pub(crate) fn new_deferred(root: PathBuf) -> Self {
        Self::new_with_explorer(root, true)
    }

    pub(crate) fn new_startup(root: PathBuf, wake: crate::background::UiWake) -> Self {
        Self::new_with_background(root, true, crate::background::BackgroundWorkers::with_wake(wake))
    }

    #[cfg(test)]
    fn new_with_explorer(root: PathBuf, defer_explorer_scan: bool) -> Self {
        Self::new_with_background(root, defer_explorer_scan, crate::background::BackgroundWorkers::new())
    }

    fn new_with_background(root: PathBuf, defer_explorer_scan: bool, background: crate::background::BackgroundWorkers) -> Self {
        let browser_path = root.clone();
        let loaded_themes = if defer_explorer_scan {
            vec![crate::app_theme_loader::builtin_theme("dark").unwrap().clone()]
        } else { crate::app_theme::load_builtin_themes() };
        let highlighter = if defer_explorer_scan { Highlighter::new_deferred() }
            else { Highlighter::new() };
        let syntax_names = highlighter.syntax_names();
        let palette_commands = Self::build_command_list_with_themes(&loaded_themes, &syntax_names);
        let ui_wake = background.wake_handle();
        let mut app = Self {
            remote_workspace: None,
            remote_connection: Default::default(),
            side_notice: Default::default(),
            logs: Default::default(),
            remote_explorer_error: None,
            explorer_expanding: Default::default(),
            loading: Default::default(),
            remote_navigation: crate::app_remote_navigation::State::default(),
            global_search_service: Default::default(),
            desks: crate::app_desks::Desks::default(),
            chat: crate::app_acp::ChatState::default(),
            active_pane: ActivePane::Explorer,
            explorer: if defer_explorer_scan {
                FileExplorer::new_deferred(root)
            } else {
                FileExplorer::new(root)
            },
            session_baseline: None,
            workspace_generation: 0,
            workspace_open_generation: 0,
            tabs: Vec::new(),
            active_tab: 0,
            tab_scroll_offset: 0,
            tab_scroll_manual_anchor: None,
            groups: Vec::new(),
            active_group: 0,
            split_vertical: true,
            split_ratio: 0.5,
            resizing_split: false,
            diff_split_ratio: 0.5,
            resizing_diff_split: false,
            diff_split_area: None,
            diff_divider_area: None,
            diff_left_code_area: None,
            diff_left_h_scrollbar_area: None,
            diff_left_horizontal_scroll: 0,
            dragging_diff_left_h_scrollbar: false,
            split_tree: None,
            split_dividers: Vec::new(),
            split_layout_cache: Default::default(),
            dragging_divider: None,
            show_explorer: true,
            show_activity_bar: true,
            sidebar_view: SidebarView::Explorer,
            show_open_editors: true,
            open_editors_scroll_offset: 0,
            open_editors_scroll_anchor: None,
            show_minimap: true,
            minimap_width: 10,
            show_panel: false,
            show_line_numbers: true,
            relative_line_numbers: false,
            show_indent_guides: true,
            rulers: vec![80],
            chord_pending: false,
            chord_start: std::time::Instant::now(),
            cursor_visible: true,
            cursor_blinking: true,
            terminal_has_focus: true,
            last_user_activity: std::time::Instant::now(),
            idle_quiescent: false,
            animation_settings: crate::animation_config::AnimationSettings::default(),
            animations: crate::animations::AnimationState::default(),
            copy_animation: crate::copy_animation::CopyAnimation::default(),
            animation_frame_cache: Default::default(),
            rendering_animation_frame: false,
            tab_animation_cache: Default::default(),
            tab_name_cache: Default::default(),
            scroll_speed: 3,
            language_associations: rustc_hash::FxHashMap::default(),
            cursor_style: CursorStyle::Line,
            last_blink: std::time::Instant::now(),
            last_terminal_cursor_pos: None,
            terminal_cursor_shown: false,
            status_message: String::from("Ready"),
            notification_history: Vec::new(),
            highlighter,
            last_click_time: std::time::Instant::now() - std::time::Duration::from_secs(1),
            last_click_pos: (0, 0),
            click_count: 0,
            multi_click_anchor: None,
            viewport_area: ratatui::layout::Rect::new(0, 0, 80, 24),
            explorer_area: None,
            explorer_scrollbar_drag: None,
            editor_area: None,
            tab_bar_area: None,
            gutter_width: 0,
            gutter_area: None,
            code_area: None,
            editor_gradient_area: None,
            minimap_area: None,
            hovered_gutter_line: None,
            hovered_editor_pos: None,
            scrollbar_area: None,
            h_scrollbar_area: None,
            dragging_v_scrollbar: false,
            dragging_h_scrollbar: false,
            dragging_minimap: false,
            minimap_drag_start: None,
            settings_scrollbar_drag: None,
            text_input_regions: Vec::new(),
            text_input_histories: std::collections::HashMap::new(),
            text_input_click_id: None,
            text_input_drag_unit: None,
            text_input_drag: None,
            status_bar_area: None,
            performance_area: None,
            performance_view: crate::ui_performance_details::ViewState::default(),
            status_bar_items: Vec::new(),
            status_details: crate::ui_status::DetailsState::default(),
            status_bar_pins: std::collections::BTreeMap::new(),
            chrome_hover: crate::ui_status::HoverState::default(),
            panel_tab_areas: Vec::new(),
            show_breadcrumbs: true,
            show_status_bar: true,
            show_tab_bar: true,
            enable_preview_tabs: true,
            mouse_back_forward_to_navigate: true,
            show_menu_bar: true,
            breadcrumb_area: None,
            breadcrumb_segments: Vec::new(),
            breadcrumb_dropdown_active: false,
            breadcrumb_dropdown_segment: 0,
            breadcrumb_dropdown_items: Vec::new(),
            breadcrumb_dropdown_selected: 0,
            breadcrumb_dropdown_loading: false,
            breadcrumb_dropdown_generation: 0,
            breadcrumb_markdown_preview_area: None,
            diff_nav_prev_area: None,
            diff_nav_next_area: None,
            diff_layout_toggle_area: None,
            search_active: false,
            search_pre_cursor: None,
            search_query: String::new(),
            search_cursor: 0,
            search_selection_anchor: None,
            search_matches: Vec::new(),
            search_error: None,
            search_truncated: false,
            search_current: 0,
            search_input_focused: false,
            replace_mode: false,
            replace_query: String::new(),
            replace_cursor: 0,
            replace_selection_anchor: None,
            search_focus_replace: false,
            search_case_sensitive: false,
            search_regex: false,
            search_whole_word: false,
            search_in_selection: false,
            search_preserve_case: false,
            search_selection_range: None,
            search_history: Default::default(),
            quick_open_active: false,
            quick_open_query: String::new(),
            quick_open_cursor: 0,
            quick_open_selection_anchor: None,
            quick_open_results: Vec::new(),
            quick_open_index: crate::app_search::QuickOpenIndex::default(),
            quick_open_selected: 0,
            git_branch_picker_active: false,
            git_branch_picker_query: String::new(),
            git_branch_picker_cursor: 0,
            git_branch_picker_selection_anchor: None,
            git_branch_picker_items: Vec::new(),
            git_branch_picker_selected: 0,
            git_branch_picker_scroll_offset: 0,
            recent_files: Vec::new(),
            goto_line_active: false,
            goto_line_query: String::new(),
            goto_line_cursor: 0,
            goto_line_selection_anchor: None,
            goto_line_saved_selection: None,
            goto_line_saved_scroll: None,
            confirm_close_active: false,
            confirm_close_tab: 0,
            confirm_close_btn_areas: Vec::new(),
            confirm_delete_active: false,
            confirm_delete_target: None,
            confirm_delete_targets: Vec::new(),
            confirm_delete_btn_areas: Vec::new(),
            confirm_delete_selected: 0,
            confirm_move_active: false,
            confirm_move_source: None,
            confirm_move_destination: None,
            confirm_move_btn_areas: Vec::new(),
            confirm_move_selected: 0,
            palette: PaletteState {
                commands: palette_commands,
                ..Default::default()
            },
            context_menu_active: false,
            context_menu_pos: (0, 0),
            context_menu_inner: None,
            context_menu_button_drag: None,
            context_menu_selected: 0,
            context_menu_scroll: 0,
            context_menu_items: Self::build_context_menu(),
            vtcomplete_active: false,
            vtcomplete_suggestions: Vec::new(),
            vtcomplete_details: Default::default(),
            vtcomplete_selected: 0,
            vtcomplete_explicit: false,
            vtcomplete_navigated: false,
            completion_index: Default::default(),
            completion_index_sync: Default::default(),
            completion_index_ui: Default::default(),
            vtcomplete_generation: 0,
            vtcomplete_cancel_token: None,
            vtcomplete_worker: crate::app_vtcomplete::VtCompleteWorker::default(),
            vtcomplete_input_batch: crate::app_vtcomplete::InputBatch::default(),
            visible_highlight_key: None,
            visible_highlight_cancel_token: None,
            provisional_highlight_key: None,
            provisional_highlight_cancel_token: None,
            quick_suggestions: true,
            explorer_context_active: false,
            explorer_context_pos: (0, 0),
            explorer_context_inner: None,
            explorer_context_selected: 0,
            explorer_context_target: None,
            explorer_context_extension_commands: Vec::new(),
            explorer_clipboard: None,
            explorer_multi_selected: Default::default(),
            explorer_selection_anchor: Some(0),
            binary_open_prompt_active: false,
            binary_open_prompt_path: None,
            binary_open_prompt_selected: 0,
            binary_open_prompt_commands: Vec::new(),
            binary_open_prompt_open_normally: false,
            binary_open_prompt_skip_once: None,
            rename: RenameState::default(),
            creating_new_file: false,
            creating_new_folder: false,
            save_as_active: false,
            modal: crate::ui_modal::ModalState::default(),
            save_error: None,
            save_conflict: None,
            quick_picker: crate::ui_quick_picker::PickerState::default(),
            save_as_query: String::new(),
            save_as_cursor: 0,
            save_as_selection_anchor: None,
            global_search_active: false,
            global_search_query: String::new(),
            global_search_history: Default::default(),
            global_search_error: None,
            global_search_replace_query: String::new(),
            global_search_replace_visible: true,
            global_search_results: Default::default(),
            global_search_truncated: false,
            global_search_selected: 0,
            global_search_scroll: 0,
            max_global_search_results: 20_000,
            large_file_threshold_mb: crate::editor::DEFAULT_LARGE_FILE_THRESHOLD_MB,
            large_file_analysis_timeout_ms: crate::editor::DEFAULT_LARGE_FILE_ANALYSIS_TIMEOUT_MS,
            search_include_pattern: String::new(),
            search_exclude_pattern: String::new(),
            search_focus_field: 0,
            global_search_field_cursors: [0; 4],
            global_search_field_selection_anchors: [None; 4],
            hovered_global_search_close: false,
            symbol_list_active: false,
            symbol_list_query: String::new(),
            symbol_list_cursor: 0,
            symbol_list_selection_anchor: None,
            symbol_list_items: Vec::new(),
            symbol_list_filtered: Vec::new(),
            symbol_list_selected: 0,
            symbol_list_workspace: false,
            source_symbol_work: Default::default(),
            outline_items: Vec::new(),
            outline_scroll: 0,
            outline_selected: 0,
            outline_hovered: None,
            closed_tab_paths: std::collections::VecDeque::new(),
            dragging_tab: None,
            dragging_tab_source_group: None,
            dragging_terminal_tab: None,
            dragging_explorer_path: None,
            drag_start_pos: None,
            drag_has_moved: false,
            explorer_drop_target_idx: None,
            dragging_text: false,
            dragging_text_start: None,
            editor_selection_drag: None,
            alt_cursor_mouse_down_active: false,
            dragging_gutter: false,
            dragging_gutter_start_line: None,
            tab_drop_zone: None,
            tab_drop_target_group: None,
            about_info: None,
            shortcuts_active: false,
            shortcuts_scroll: 0,
            zen_mode: false,
            zen_pre_state: None,
            navigation: crate::app_navigation::NavigationHistory::default(),
            tab_switcher_active: false,
            tab_switcher_selected: 0,
            tab_mru_order: Vec::new(),
            settings_active: false,
            settings_selected: 0,
            settings_category_selected: 0, // default to "Commonly Used"
            settings_section_selected: None,
            settings_category_scroll: 0,
            dragging_settings_categories: false,
            settings_category_collapsed: [true; crate::app_settings_ui::SETTINGS_CATEGORIES.len()],
            agent_settings: crate::ui_agent_settings::AgentSettingsState::default(),
            lsp_settings: crate::ui_lsp_settings::LspSettingsState::default(),
            lsp_enabled: true,
            settings_category_area: None,
            settings_category_picker_area: None,
            settings_sidebar_area: None,
            settings_scroll_areas: Vec::new(),
            settings_focus_categories: false,
            settings_focus_control: false,
            settings_editor_active: false,
            settings_editor_scroll: 0,
            settings_layout_cache: Default::default(),
            settings_reveal_selected: false,
            settings_editor_search: String::new(),
            settings_editor_search_cursor: 0,
            settings_editor_search_selection_anchor: None,
            settings_scope: crate::settings::SettingsScope::Global,
            settings_scope_areas: [None, None],
            settings_json_area: None,
            settings_clear_search_area: None,
            settings_value_editing: false,
            settings_value_setting_id: None,
            settings_value_buffer: String::new(),
            settings_value_cursor: 0,
            settings_value_selection_anchor: None,
            settings_kb_editing: false,
            settings_dropdown_open: false,
            settings_dropdown_setting_id: None,
            settings_dropdown_options: Vec::new(),
            settings_dropdown_selected: 0,
            settings_dropdown_scroll: 0,
            settings_dropdown_area: None,
            settings_dropdown_theme_original: None,
            settings_dropdown_committed: false,
            theme_management: Default::default(),
            settings_defaults: crate::settings::Settings::default(),
            settings_global: crate::settings::Settings::default(),
            settings_workspace: crate::settings::Settings::default(),
            settings_effective: crate::settings::Settings::default(),
            settings_global_path: crate::settings::global_settings_path(),
            settings_workspace_path: crate::settings::project_settings_path(&browser_path),
            settings_persistence_enabled: !cfg!(test),
            trim_trailing_on_save: true,
            format_on_save: false,
            insert_final_newline: false,
            auto_save: AutoSaveMode::Off,
            auto_save_delay: 1000,
            explorer_width: 30,
            resizing_explorer: false,
            peek_definition: None,
            blame_cache: None,
            blame_documents: Default::default(),
            inline_blame_hover: None,
            blame_cache_key: None,
            blame_requested_path: None,
            bg_blame_running: false,
            disk_check_last: std::time::Instant::now(),
            disk_check_index: 0,
            panel_tab: 1,
            panel_chrome: Default::default(),
            terminal_panel_focused: false,
            panel_height: 10,
            problem_scroll: 0,
            resizing_panel: false,
            panel_area: None,
            show_terminal_sidebar: false,
            terminal_sidebar_revealed: false,
            terminal_sidebar_area: None,
            terminal_sidebar_toggle_area: None,
            terminal_sidebar_plus_area: None,
            terminal_sidebar_tab_areas: Vec::new(),
            terminal_sidebar_close_areas: Vec::new(),
            workbench_body_area: None,
            terminal_dock_drop_target: None,
            terminals: vec![TerminalTab::placeholder(
                browser_path.clone(), TerminalDock::Right,
            )],
            active_terminal: 0,
            next_terminal_to_process: 0,
            terminal_surfaces: Vec::new(),
            terminal_selection_drag: None,
            terminal_mouse_capture: None,
            task_list: Vec::new(),
            last_task: None,
            task_picker_active: false,
            task_picker_selected: 0,
            task_picker_query: String::new(),
            task_picker_cursor: 0,
            task_picker_selection_anchor: None,
            markdown_preview: false,
            hovered_tab: None,
            hovered_tab_close_btn: false,
            hovered_tab_scroll_left: false,
            hovered_tab_scroll_right: false,
            tab_tooltip_hover_start: None,
            tab_tooltip_tab: None,
            tab_tooltip_shown: false,
            hovered_activity: None,
            hovered_menu: None,
            hovered_status_item: None,
            hovered_explorer: None,
            explorer_line_count_cache: rustc_hash::FxHashMap::default(),
            explorer_hover_info_cache: rustc_hash::FxHashMap::default(),
            explorer_hover: Default::default(),
            hovered_close_button: false,
            hovered_panel_close: false,
            hovered_panel_tab: None,
            hovered_problem_row: None,
            hovered_terminal_plus: false,
            hovered_terminal_tab: None,
            hovered_terminal_close: None,
            hovered_terminal_sidebar_toggle: false,
            hovered_new_tab_button: false,
            hovered_scrollbar: false,
            hovered_minimap: false,
            hovered_h_scrollbar: false,
            hovered_search_toggle: None,
            search_control_regions: Vec::new(),
            search_bar_area: None,
            search_bar_layout: Default::default(),
            rendering_inactive_editor: false,
            saving_flash: None,
            mouse_row: 0,
            mouse_col: 0,
            mouse_hover_changed: false,
            cached_diag_lines: std::sync::Arc::new(Vec::new()),
            cached_diag_key: (None, 0, 0),
            cached_editor_diagnostics: std::sync::Arc::new(Vec::new()),
            cached_diagnostic_index: std::sync::Arc::new(
                crate::ui_editor_minimap::DiagnosticLineIndex::default(),
            ),
            cached_diag_indices_by_line: std::sync::Arc::new(rustc_hash::FxHashMap::default()),
            cached_wide_diag_indices: std::sync::Arc::new(Vec::new()),
            editor_diagnostic_cache: std::collections::VecDeque::new(),
            cached_overview_change_rows: std::sync::Arc::default(),
            cached_overview_change_rows_key: None,
            editor_overview_cache: std::collections::VecDeque::new(),
            cached_editor_change_index: std::sync::Arc::new(
                crate::ui_editor_minimap::ChangeLineIndex::default(),
            ),
            editor_change_index_cache: std::collections::VecDeque::new(),
            exit_confirm_buttons: Vec::new(),
            diff_mode: DiffMode::Off,
            preferred_diff_mode: DiffMode::SideBySide,
            diff_inline_scroll: crate::inline_diff::InlineDiffScroll::default(),
            diff_selection: crate::diff_selection::DiffSelection::default(),
            diff_scope_source: crate::diff_scope_source::DiffScopeSource::default(),
            diff_tabs: crate::diff_tabs::DiffTabs::default(),
            selected_compare_path: None,
            compare_base_path: None,
            compare_base_content: None,
            pending_compare_base_rope: None,
            compare_diff_lines: Vec::new(),
            diff_render_cache: std::sync::Arc::new(DiffRenderCache::default()),
            compare_diff_target_version: None,
            compare_diff_base_revision: None,
            diff_tab_label: None,
            diff_labels: None,
            diff_target_path: None,
            pending_git_compare_path: None,
            git_compare_generation: 0,
            bg_git_compare_running: false,
            diff_refresh_generation: 0,
            bg_diff_refresh_running: false,
            clipboard_history: std::collections::VecDeque::new(),
            clipboard_cycle_active: false,
            clipboard_cycle_index: 0,
            bulk_paste: Default::default(),
            colors: EditorTheme::Dark.colors(),
            loaded_themes,
            theme_key: "dark".to_string(),
            startup_theme_placeholder: None,
            theme_name: "Dark+".to_string(),
            syntax_theme: "dark-plus".to_string(),
            applied_theme_generation: None,
            semantic_theme_colors: None,
            focus_mode: false,
            sidebar_right: false,
            last_edit_position: None,
            last_edit_time: std::time::Instant::now(),
            show_word_count: false,
            show_color_preview: false,
            indent_rainbow: false,
            bracket_pair_colorization: true,
            error_lens: false,
            sticky_scroll: true,
            sticky_scroll_max: 3,
            screencast_mode: false,
            screencast_keys: std::collections::VecDeque::new(),
            scroll_past_end: true,
            default_tab_size: 4,
            default_use_tabs: false,
            default_word_wrap: false,
            default_whitespace_mode: WhitespaceMode::Trailing,
            comment_whitespace_mode: Some(WhitespaceMode::All),
            default_read_only: false,
            default_cursor_surrounding_lines: 5,
            word_wrap_column: 0,
            call_hierarchy_active: false,
            call_hierarchy_mode: CallHierarchyMode::Incoming,
            call_hierarchy_results: Vec::new(),
            call_hierarchy_selected: 0,
            call_hierarchy_root_name: String::new(),
            type_hierarchy_active: false,
            type_hierarchy_mode: TypeHierarchyMode::Supertypes,
            type_hierarchy_results: Vec::new(),
            type_hierarchy_selected: 0,
            type_hierarchy_root_name: String::new(),
            macro_recording: false,
            macro_buffer: Vec::new(),
            macro_saved: Vec::new(),
            macro_replay_queue: std::collections::VecDeque::new(),
            exit_confirm_active: false,
            exit_backup_warning: None,
            exit_recovery: Default::default(),
            exit_save_and_quit: false,
            exit_discard_quit: false,
            exit_keep_unsaved_quit: false,
            custom_exit_started: false,
            custom_exit_remaining: 0,
            custom_exit_failed: false,
            pending_custom_recoveries: Vec::new(),
            change_dir_active: false,
            change_dir_query: String::new(),
            folder_browser: crate::folder_browser::FolderBrowser::default(),
            folder_browser_entries: Vec::new(),
            folder_browser_loading: false,
            folder_browser_selected: 0,
            folder_browser_scroll: 0,
            folder_browser_path: browser_path.clone(),
            folder_browser_area: None,
            folder_browser_buttons: Vec::new(),
            middle_drag_start: None,
            block_drag_cache: None,
            source_control_repository_state: SourceControlRepositoryState::Checking,
            repositories: Default::default(),
            source_control_enabled: true,
            source_control_cancel: crate::background::CancellationToken::new(),
            source_control_toggle_area: None,
            source_control_last_refresh: None,
            source_control_initialize_area: None,
            source_control_commit_area: None,
            source_control_amend_area: None,
            source_control_stage_all_area: None,
            source_control_unstage_all_area: None,
            source_control_discard_all_area: None,
            source_control_untracked_stage_area: None,
            source_control_untracked_discard_area: None,
            source_control_actions_area: None,
            source_control_files: Vec::new(),
            source_control_selection: Default::default(),
            source_control_selected: 0,
            sc_context_active: false,
            sc_context_pos: (0, 0),
            sc_context_inner: None,
            sc_context_selected: 0,
            sc_context_target: None,
            sc_context_targets: Vec::new(),
            hunk_popup_active: false,
            hunk_popup_pos: (0, 0),
            hunk_popup_selected: 0,
            hunk_popup_file: None,
            hunk_popup_line: 0,
            hunk_popup_status: ' ',
            hunk_peek: None,
            hunk_popup_generation: 0,
            sc_discard_confirm_active: false,
            sc_discard_request: None,
            sc_discard_confirm_btn_areas: Vec::new(),
            sc_stage_all_commit_request: None,
            source_control_commit_msg: String::new(),
            source_control_commit_cursor: 0,
            commit_input: crate::commit_input::CommitInputState::default(),
            source_control_commit_selection_anchor: None,
            source_control_input_mode: false,
            source_control_scroll: 0,
            sc_merge_collapsed: false,
            sc_staged_collapsed: false,
            sc_changes_collapsed: false,
            sc_untracked_collapsed: false,
            sc_visibility: Default::default(),
            sc_graph_collapsed: false,
            sc_graph_height: 10,
            resizing_sc_graph: false,
            sc_graph_click_start: None,
            sc_amend_mode: false,
            git_log: Vec::new(),
            git_history: crate::git_history::GitHistory::default(),
            git_log_scroll: 0,
            git_log_selected: 0,
            commit_details: crate::commit_details::CommitDetails::default(),
            sc_graph_focused: false,
            keybindings: default_keybindings(),
            keybindings_ui: Default::default(),
            keybindings_selected: 0,
            keybindings_editing: false,
            keybinding_settings_cache: Vec::new(),
            menu_active: false,
            menu_selected: 0,
            menu_open: false,
            menu_item_selected: 0,
            activity_bar_selected: 0,
            activity_bar_area: None,
            menu_bar_area: None,
            zen_exit_area: None,
            tab_context_active: false,
            tab_context_pos: (0, 0),
            tab_context_inner: None,
            tab_context_tab: 0,
            tab_context_selected: 0,
            cached_git_branch: String::new(),
            cached_error_count: 0,
            cached_warning_count: 0,
            cached_max_line_width: 0,
            cached_editor_word_count: 0,
            cached_editor_analysis_target: None,
            content_analysis_cache: Default::default(),
            cached_word_occ: None,
            cached_word_occ_target: None,
            cached_word_count: None,
            cached_sel_stats: None,
            cached_sel_stats_key: None,
            cached_problems: Vec::new(),
            cached_problems_target: None,
            cached_problems_error_count: 0,
            cached_problems_warning_count: 0,
            cached_problems_info_count: 0,
            cached_problems_file_count: 0,
            cached_sticky_lines: Vec::new(),
            cached_sticky_key: 0,
            cached_sticky_scopes: Default::default(),
            cached_bracket_guides: Default::default(),
            cached_bracket_analysis_target: None,
            cached_bracket_depth_index: None,
            cached_bracket_starting_depth: 0,
            cached_structural_scope_index: None,
            pending_structural_scope: None,
            structural_scope_cancel: None,
            cached_cursor_match_target: None,
            cached_matching_bracket: None,
            cached_matching_tag: None,
            cached_cursor_word: None,
            cached_cursor_word_key: (u64::MAX, 0, 0, 0),
            editor_cursor_word_cache: Default::default(),
            editor_selection_word_cache: std::collections::VecDeque::new(),
            shutdown_anim_active: false,
            shutdown_anim_start: std::time::Instant::now(),
            extension_manager: crate::extensions::ExtensionManager::new(&browser_path),
            extension_pointer_capture: None,
            extensions_selected: 0,
            extensions_scroll: 0,
            extensions_filter: String::new(),
            extensions_filter_cursor: 0,
            extensions_filter_selection_anchor: None,
            ext_sidebar: crate::ui_extensions::Sidebar::default(),
            ext_info: crate::ui_extension_info::State::default(),
            extension_palette_action_map: rustc_hash::FxHashMap::default(),
            extension_overlay_drag: None,
            extension_overlay_last_tick: rustc_hash::FxHashMap::default(),
            lsp_manager: crate::lsp::LspManager::new_with_wake(
                browser_path.clone(),
                ui_wake,
            ),
            lsp_completion_active: false,
            lsp_completion_selected: 0,
            _lsp_completion_filter: String::new(),
            completion_ui: Default::default(),
            hover_enabled: true,
            lsp_hover_active: false,
            lsp_hover_suppressed: false,
            lsp_hover_line: 0,
            lsp_hover_col: 0,
            lsp_hover_request_time: None,
            lsp_hover_pending_pos: None,
            lsp_link_hover: Default::default(),
            lsp_goto_pending: false,
            lsp_signature_active: false,
            lsp_code_action_active: false,
            lsp_code_action_selected: 0,
            lsp_format_pending: false,
            lsp_format_target_path: None,
            pending_lsp_workspace_edits: std::collections::VecDeque::new(),
            lsp_suggestions: Default::default(),
            lsp_save_after_format_path: None,
            formatting: Default::default(),
            formatters: Default::default(),
            formatter_settings: Default::default(),
            file_associations: Default::default(),
            scope_bubble_settings: Default::default(),
            lsp_rename_pending: false,
            lsp_prepare_rename_pending: false,
            lsp_references_pending: false,
            lsp_references_active: false,
            lsp_references_selected: 0,
            lsp_inlay_hints: Vec::new(),
            lsp_inlay_hint_last_scroll: usize::MAX,
            lsp_inlay_hint_last_version: u64::MAX,
            show_inlay_hints: true,
            lsp_semantic_tokens: Vec::new(),
            lsp_semantic_tokens_last_version: u64::MAX,
            lsp_semantic_tokens_request_target: None,
            show_semantic_tokens: true,
            lsp_document_highlights: Vec::new(),
            lsp_document_multiline_highlights: Vec::new(),
            lsp_highlight_last_cursor: (usize::MAX, usize::MAX),
            lsp_outline_last_version: u64::MAX,
            lsp_outline_available: false,
            lsp_folding_last_version: u64::MAX,
            lsp_folding_ranges: Vec::new(),
            lsp_selection_ranges: Vec::new(),
            lsp_selection_range_cursor: (usize::MAX, usize::MAX),
            lsp_selection_range_source: None,
            lsp_code_lenses: Vec::new(),
            lsp_code_lenses_raw: Vec::new(),
            lsp_code_lens_last_version: u64::MAX,
            show_code_lenses: true,
            lsp_document_links: Vec::new(),
            lsp_document_links_last_version: u64::MAX,
            // Match VS Code's conservative default. Language-server formatting
            // while typing is opt-in and Enter itself never invokes it.
            on_type_formatting_enabled: false,
            lsp_on_type_format_target: None,
            pending_lsp_on_type_format: None,
            background,
            bg_git_status_running: false,
            bg_git_status_pending: false,
            pending_git_status_paths: None,
            bg_explorer_refresh_running: false,
            bg_explorer_refresh_pending: false,
            explorer_refresh_generation: 0,
            explorer_refresh_active_generation: None,
            workspace_file_index_generation: 0,
            bg_extension_scan_running: false,
            bg_extension_scan_pending: false,
            extension_scan_generation: 0,
            bg_external_theme_scan_running: false,
            bg_syntax_asset_refresh_running: false,
            bg_syntax_asset_refresh_pending: false,
            syntax_reload_requested: false,
            bg_git_branch_running: false,
            bg_git_branch_pending: false,
            bg_git_log_running: false,
            bg_git_remote_running: false,
            bg_git_action_running: false,
            bg_git_branch_picker_running: false,
            bg_filesystem_action_running: false,
            file_deletion: None,
            bg_global_search_running: false,
            bg_file_search_running: false,
            file_search_generation: 0,
            file_search_cancel_token: None,
            file_search_matches_target: None,
            file_search_key: None,
            file_search_refresh_pending: false,
            bg_file_replace_running: false,
            file_replace_generation: 0,
            bg_global_replace_running: false,
            global_replace_generation: 0,
            workspace_replace: Default::default(),
            search_generation: 0,
            search_cancel_token: None,
            bg_quick_open_running: false,
            quick_open_generation: 0,
            quick_open_cancel_token: None,
            bg_git_diff_running: false,
            bg_git_diff_pending: false,
            bg_editor_analysis_running: false,
            pending_editor_analysis: None,
            editor_analysis_deferred: false,
            bg_bracket_analysis_running: false,
            pending_bracket_analysis: None,
            bg_cursor_match_running: false,
            pending_cursor_match: None,
            bg_problems_analysis_running: false,
            bg_word_occ_running: false,
            word_occurrence_deferred: false,
            pending_lsp_syncs: rustc_hash::FxHashMap::default(),
            pending_lsp_did_save: rustc_hash::FxHashSet::default(),
            pending_lsp_semantic_decode: None,
            pending_word_occurrence: None,
            pending_wrap_layouts: rustc_hash::FxHashMap::default(),
            pending_file_saves: rustc_hash::FxHashMap::default(),
            deferred_file_saves: rustc_hash::FxHashMap::default(),
            pending_save_as: rustc_hash::FxHashMap::default(),
            auto_save_attempted_versions: rustc_hash::FxHashMap::default(),
            pending_file_reloads: rustc_hash::FxHashSet::default(),
            pending_file_reload_again: rustc_hash::FxHashSet::default(),
            pending_disk_checks: rustc_hash::FxHashSet::default(),
            remote_disk_checks: RemoteDiskChecks::default(),
            remote_changes_deferred: Default::default(),
            remote_change_warnings: Default::default(),
            workspace_watcher_active: false,
            reap_frame_counter: 0,
        };
        let defaults = app.current_settings();
        app.settings_defaults = defaults.clone();
        app.settings_effective = defaults;
        app
    }

    /// Poll background workers for completed results and apply them.
    #[cfg(test)]
    pub(crate) fn poll_background_results(&mut self) -> bool {
        self.poll_background_results_until(std::time::Instant::now() + crate::ui_work::TURN_BUDGET)
    }

    pub(crate) fn poll_background_results_until(&mut self, deadline: std::time::Instant) -> bool {
        if let Some(remote) = &self.remote_workspace { remote.apply_notifications(&self.background.sender, self.workspace_generation); }
        let disconnected = self.poll_remote_connection(deadline);
        self.sync_diff_tab();
        let mut budget = crate::background::ResultBudget::until(deadline);
        // A worker reply is not necessarily a visual change. Periodic disk,
        // branch, and blame probes commonly return the exact state already on
        // screen; treating those replies as redraw requests kept an otherwise
        // idle workbench rendering forever.
        let mut has_visual_changes = disconnected;
        let mut file_reload_names: Vec<String> = Vec::new();
        while let Some(result) = budget.next(&mut self.background) {
            self.completion_index_sync.buffers_changed = true;
            let result = match result {
                crate::background::BackgroundResult::WorkspaceResult { generation, result } => {
                    if generation != self.workspace_generation { continue; }
                    *result
                }
                result => result,
            };
            let result = match result {
                crate::background::BackgroundResult::SourceControlRead { cancel, result } => {
                    if !self.source_control_enabled || cancel.is_cancelled() { continue; }
                    *result
                }
                result => result,
            };
            let result = match result {
                crate::background::BackgroundResult::RepositoryRead { root, generation, result } => {
                    if root != self.source_control_root() || generation != self.repositories.view_generation { continue; }
                    *result
                }
                result => result,
            };
            let (result, remote_checkpoint) = match result {
                crate::background::BackgroundResult::RemoteFileResult { checkpoint, result } => (*result, Some(checkpoint)),
                result => (result, None),
            };
            let redraws_by_default = !matches!(
                &result,
                crate::background::BackgroundResult::ChatPastePrepared { .. }
                    | crate::background::BackgroundResult::GitBranch(_)
                    | crate::background::BackgroundResult::GitRepositoryFound { .. }
                    | crate::background::BackgroundResult::GitRepositories { .. }
                    | crate::background::BackgroundResult::ExplorerRefreshed { .. }
                    | crate::background::BackgroundResult::ExplorerRefreshFailed { .. }
                    | crate::background::BackgroundResult::FolderBrowserEntries { .. }
                    | crate::background::BackgroundResult::FolderBrowserCreated { .. }
                    | crate::background::BackgroundResult::BreadcrumbEntries { .. }
                    | crate::background::BackgroundResult::GlobalSearchResults { .. }
                    | crate::background::BackgroundResult::GlobalSearchFailed { .. }
                    | crate::background::BackgroundResult::SearchMatches { .. }
                    | crate::background::BackgroundResult::QuickOpenFiles { .. }
                    | crate::background::BackgroundResult::SourceSymbols(_)
                    | crate::background::BackgroundResult::FileDiskState(_)
                    | crate::background::BackgroundResult::RemoteDiskChecksFailed { .. }
                    | crate::background::BackgroundResult::RemoteDiskChecksDone(_)
                    | crate::background::BackgroundResult::RemoteFileValidated { .. }
                    | crate::background::BackgroundResult::RemoteLocationResolved { .. }
                    | crate::background::BackgroundResult::RemoteLocationProgress { .. }
                    | crate::background::BackgroundResult::FileWatcherState { .. }
                    | crate::background::BackgroundResult::SessionUntitledLoaded { .. }
                    | crate::background::BackgroundResult::FileLoaded { .. }
                    | crate::background::BackgroundResult::FileLoadFailed { .. }
                    | crate::background::BackgroundResult::FileReloaded { .. }
                    | crate::background::BackgroundResult::ExtensionsDiscovered { .. }
                    | crate::background::BackgroundResult::ExtensionsInitialized { .. }
                    | crate::background::BackgroundResult::ExtensionViewEventDone { .. }
                    | crate::background::BackgroundResult::ExtensionTickDone { .. }
                    | crate::background::BackgroundResult::ExternalThemesLoaded(_)
                    | crate::background::BackgroundResult::SyntaxAssetsLoaded { .. }
                    | crate::background::BackgroundResult::GitLineDiff { .. }
                    | crate::background::BackgroundResult::GitCompare { .. }
                    | crate::background::BackgroundResult::SourceControlDiffs { .. }
                    | crate::background::BackgroundResult::DiffRefresh { .. }
                    | crate::background::BackgroundResult::EditorAnalysis { .. }
                    | crate::background::BackgroundResult::EditorWorkCancelled { .. }
                    | crate::background::BackgroundResult::RichFeaturesTimedOut { .. }
                    | crate::background::BackgroundResult::BracketAnalysis { .. }
                    | crate::background::BackgroundResult::StructuralScopes { .. }
                    | crate::background::BackgroundResult::CursorMatches { .. }
                    | crate::background::BackgroundResult::ProblemsAnalysis { .. }
                    | crate::background::BackgroundResult::WordOccurrences { .. }
                    | crate::background::BackgroundResult::VtComplete { .. }
                    | crate::background::BackgroundResult::HighlightBatch(_)
                    | crate::background::BackgroundResult::HighlightFragments { .. }
                    | crate::background::BackgroundResult::LspSemanticTokens { .. }
                    | crate::background::BackgroundResult::WrapLayout { .. }
                    | crate::background::BackgroundResult::BlameCache { .. }
                    | crate::background::BackgroundResult::BlameHistory { .. }
                    | crate::background::BackgroundResult::BlameSnapshot { .. }
                    | crate::background::BackgroundResult::RemoteTransferProgress { .. }
            );
            has_visual_changes |= redraws_by_default;
            match result {
                crate::background::BackgroundResult::RemoteReconnectStarted { generation, serial, result } => {
                    self.remote_reconnect_started(generation, serial, result);
                }
                crate::background::BackgroundResult::RemoteReconnected { generation, serial, result } => {
                    self.finish_remote_reconnect(generation, serial, result);
                }
                crate::background::BackgroundResult::RemoteReconnectFile { generation, check } => {
                    self.finish_remote_reconnect_file(generation, check);
                }
                crate::background::BackgroundResult::RemoteLocationResolved { id, result } => {
                    has_visual_changes |= self.apply_remote_location(id, result);
                }
                crate::background::BackgroundResult::RemoteLocationProgress { id, completed, total } => {
                    has_visual_changes |= self.apply_remote_location_progress(id, completed, total);
                }
                crate::background::BackgroundResult::AgentFileResolved { agent, handle, id, ticket, request, aliases } => {
                    self.apply_agent_file_resolved(agent, handle, id, ticket, request, aliases);
                }
                crate::background::BackgroundResult::RemoteTransferProgress { generation, path, saving, completed, total } => {
                    if generation != self.workspace_generation { continue; }
                    let active = if saving { self.pending_file_saves.contains_key(&path) } else {
                        self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
                            .any(|tab| tab.editor.bg_loading && tab.editor.file_path.as_ref() == Some(&path))
                    };
                    if !active { continue; }
                    has_visual_changes = true;
                    if saving && completed == total {
                        self.status_message = format!("Saving {} · upload complete, waiting for disk", self.display_path(&path));
                        continue;
                    }
                    let percent = if total == 0 { 100 } else { ((completed as u128 * 100) / total as u128).min(100) };
                    self.status_message = format!("{} {} · {:.1}/{:.1} MiB ({percent}%)",
                        if saving { "Saving" } else { "Opening" }, self.display_path(&path),
                        completed as f64 / 1_048_576.0, total as f64 / 1_048_576.0);
                }
                crate::background::BackgroundResult::RemoteWorkspaceOpened { request, source, result } => {
                    if request != self.workspace_open_generation || source != self.workspace_generation { continue; }
                    match result {
                        Ok(remote) => {
                            let path = remote.view_root.clone();
                            self.change_workspace_ready(&path, Some(remote));
                        }
                        Err(error) => self.notify_error(format!("Cannot open remote folder: {error}")),
                    }
                }
                crate::background::BackgroundResult::RemoteDiskChecksFailed { paths } => {
                    for path in paths { self.pending_disk_checks.remove(&path); }
                }
                crate::background::BackgroundResult::RemoteDiskChecksDone(cancel) => {
                    if self.remote_disk_checks.running.as_ref().is_some_and(|running| running.same_task(&cancel)) {
                        self.remote_disk_checks.running = None;
                        self.start_remote_disk_checks();
                    }
                }
                crate::background::BackgroundResult::RemoteFileValidated { generation, path, document, baseline, result } => {
                    has_visual_changes |= self.finish_remote_file_validation(generation, path, document, baseline, result);
                }
                crate::background::BackgroundResult::RemoteWorkspaceError { operation, error } => {
                    self.bg_git_status_running = false;
                    self.notify_error(format!("Remote {operation}: {error}"));
                    has_visual_changes = true;
                }
                crate::background::BackgroundResult::SourceControlRead { .. } => unreachable!("nested Git result"),
                crate::background::BackgroundResult::RepositoryRead { .. } => unreachable!("nested repository result"),
                crate::background::BackgroundResult::RemoteFileResult { .. } => unreachable!("nested remote file result"),
                crate::background::BackgroundResult::WorkspaceResult { .. } => unreachable!("nested workspace result"),
                crate::background::BackgroundResult::GitRepositories { root, workspace_generation, repository_generation, discovery, scans } => {
                    if root != self.explorer.root || workspace_generation != self.workspace_generation { continue; }
                    let obsolete = repository_generation != self.repositories.membership_generation
                        || (self.sc_visibility.untracked && scans.iter().any(|scan| !scan.include_untracked));
                    if obsolete {
                        self.background.spawn("stale-git-status-drop", move || drop((discovery, scans)));
                    } else {
                        has_visual_changes |= self.apply_repository_scan(root, workspace_generation, discovery, scans);
                    }
                    self.bg_git_status_running = false;
                    if obsolete {
                        self.bg_git_status_pending = false;
                        self.pending_git_status_paths = None;
                        self.refresh_source_control_async();
                    } else if self.bg_git_status_pending {
                        self.bg_git_status_pending = false;
                        let paths = self.pending_git_status_paths.take();
                        self.start_source_control_status_refresh(paths);
                    }
                }
                crate::background::BackgroundResult::GitBranch(branch) => {
                    if let Some(repo) = self.repositories.entries.iter_mut().find(|repo| Some(&repo.root) == self.repositories.active.as_ref()) {
                        repo.branch = branch.clone();
                    }
                    has_visual_changes |= self.cached_git_branch != branch;
                    self.cached_git_branch = branch;
                    self.bg_git_branch_running = false;
                    if self.bg_git_branch_pending {
                        self.bg_git_branch_pending = false;
                        self.refresh_git_branch_async();
                    }
                }
                crate::background::BackgroundResult::GitRepositoryFound { root, workspace_generation, repository_generation } => {
                    if self.explorer.root == root
                        && self.workspace_generation == workspace_generation
                        && self.repositories.membership_generation == repository_generation
                        && matches!(self.source_control_repository_state,
                            SourceControlRepositoryState::Checking | SourceControlRepositoryState::Missing)
                    {
                        self.source_control_repository_state = SourceControlRepositoryState::Loading;
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::ExplorerRefreshed {
                    generation,
                    canonical_root,
                    entries,
                    matches,
                } => {
                    if self.finish_explorer_refresh(generation) {
                        self.remote_explorer_error = None;
                        self.explorer_expanding.clear();
                        let select_find_match = self.explorer.find.select_first;
                        self.explorer
                            .apply_refresh_entries(canonical_root, entries, matches);
                        if self.remote_workspace.is_some() && self.rename.active && self.rename.placeholder_active
                            && let Some(path) = self.rename.target.clone()
                        {
                            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                            self.explorer.insert_remote_placeholder(path, name, self.creating_new_folder);
                        }
                        if select_find_match {
                            self.clear_explorer_selection();
                            self.set_explorer_single_selection(self.explorer.selected);
                        }
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::ExplorerRefreshFailed { generation, error } => {
                    if self.finish_explorer_refresh(generation) {
                        // A failed request is not an empty directory. Keep the
                        // last complete tree and its selection/scroll position.
                        self.remote_explorer_error = Some(error.clone());
                        self.explorer.cancel_unloaded_expansions(std::mem::take(&mut self.explorer_expanding));
                        self.explorer.find.loading = false;
                        self.explorer.find.select_first = false;
                        if self.explorer.find.open { self.explorer.find.results.error = Some(error.clone()); }
                        if let Some(log) = self.remote_workspace.as_ref().and_then(|remote| remote.diagnostics()) {
                            log.event("Explorer", &format!("Refresh failed; retaining cached files: {error}"));
                        }
                        self.status_message = "Remote Explorer unavailable; showing cached files. View > Output has connection details.".into();
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::ExplorerHoverInfo { generation, info } => {
                    if self.explorer_hover.finish(generation) && let Some(info) = info {
                        has_visual_changes |= self.hovered_explorer
                            .and_then(|index| self.explorer.entries.get(index))
                            .is_some_and(|entry| entry.path == info.path);
                        if let Some(line_count) = info.line_count {
                            self.explorer_line_count_cache.insert(info.path.clone(), line_count);
                        }
                        self.explorer_hover_info_cache.insert(info.path.clone(), info);
                    }
                }
                crate::background::BackgroundResult::FolderBrowserEntries { path, generation, entries, modified, locations, error } => {
                    if self.folder_browser_path == path && self.folder_browser.generation == generation {
                        self.folder_browser_entries = entries;
                        self.folder_browser.modified = modified;
                        self.folder_browser.locations = locations;
                        self.folder_browser.error = error;
                        self.folder_browser_selected = 0;
                        self.folder_browser_scroll = 0;
                        self.folder_browser_loading = false;
                        self.filter_folder_browser();
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::FolderBrowserCreated { path, generation, error } => {
                    if self.folder_browser.generation == generation {
                        self.folder_browser.creating = false;
                        if let Some(error) = error {
                            self.folder_browser.error = Some(error);
                        } else {
                            self.folder_browser.new_folder = false;
                            self.navigate_folder_browser(path);
                        }
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::BreadcrumbEntries {
                    generation,
                    segment,
                    entries,
                } => {
                    if generation == self.breadcrumb_dropdown_generation {
                        self.breadcrumb_dropdown_segment = segment;
                        self.breadcrumb_dropdown_items = entries;
                        self.breadcrumb_dropdown_selected = 0;
                        self.breadcrumb_dropdown_loading = false;
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::GitLogPage(page) => {
                    has_visual_changes |= self.apply_git_history_page(page);
                }
                crate::background::BackgroundResult::CommitDetails(result) => {
                    has_visual_changes |= self.apply_commit_details(result);
                }
                crate::background::BackgroundResult::RemoteHunkPeek(result) => {
                    has_visual_changes |= self.apply_remote_hunk_peek(result);
                }
                crate::background::BackgroundResult::RemoteTasks(result) => {
                    match result {
                        Ok(tasks) => { self.task_list = tasks; self.status_message = if self.task_list.is_empty() { "No remote project tasks found".into() } else { String::new() }; }
                        Err(error) => self.notify_error(format!("Cannot load remote tasks: {error}")),
                    }
                    has_visual_changes = true;
                }
                crate::background::BackgroundResult::CommitDiff(result) => {
                    has_visual_changes |= self.apply_commit_diff(result);
                }
                crate::background::BackgroundResult::GitRemoteOp {
                    op,
                    stdout,
                    stderr,
                    success,
                } => {
                    self.bg_git_remote_running = false;
                    if success {
                        self.refresh_source_control_async();
                        if !self.git_log.is_empty() {
                            self.refresh_git_log_async();
                        }
                        self.refresh_git_branch_async();
                        if op == "pull" {
                            self.refresh_explorer_async();
                            self.queue_all_clean_file_reloads();
                            self.refresh_git_line_status_async();
                        }
                        let summary = stdout
                            .lines()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or(op);
                        self.notify(format!("Git {op} complete: {summary}"));
                    } else {
                        let message = stderr
                            .lines()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or("Unknown git error");
                        self.notify_error(format!("Git {op} failed: {message}"));
                    }
                }
                crate::background::BackgroundResult::GitActionDone { action, result } => {
                    self.bg_git_action_running = false;
                    match result {
                        Ok(outcome) => {
                            if let Some(request) = outcome.stage_all_commit_request {
                                self.refresh_source_control_async();
                                if self.source_control_enabled && self.current_repository_matches(&request.root) {
                                    self.sc_stage_all_commit_request = Some(request);
                                    self.modal = crate::ui_modal::ModalState::default();
                                }
                                continue;
                            }
                            if let Some(request) = &outcome.clear_commit_message {
                                self.finish_repository_commit(request, outcome.leave_amend_mode);
                            } else if outcome.leave_amend_mode {
                                self.sc_amend_mode = false;
                            }
                            if outcome.close_branch_picker {
                                self.git_branch_picker_active = false;
                                self.git_branch_picker_query.clear();
                                self.git_branch_picker_cursor = 0;
                                self.git_branch_picker_selection_anchor = None;
                                self.git_branch_picker_scroll_offset = 0;
                            }
                            self.refresh_source_control_async();
                            if outcome.refresh_line_status {
                                self.refresh_git_line_status_async();
                            }
                            if outcome.refresh_explorer {
                                self.refresh_explorer_async();
                            } else {
                                // Staging/unstaging can change whether an ignored
                                // path is tracked. Reuse directory listings.
                                self.explorer.invalidate_git_decorations();
                                self.refresh_explorer_tree_async();
                            }
                            if outcome.refresh_log && !self.git_log.is_empty() {
                                self.refresh_git_log_async();
                            }
                            if outcome.reload_all_clean_files {
                                self.queue_all_clean_file_reloads();
                            }
                            self.refresh_git_branch_async();
                            self.notify(outcome.message);
                        }
                        Err(error) => {
                            // A failed action can still have updated the index,
                            // for example staging succeeded but committing failed.
                            self.refresh_source_control_async();
                            if action == "discard" {
                                self.refresh_explorer_async();
                                self.refresh_git_line_status_async();
                                self.queue_all_clean_file_reloads();
                            } else {
                                self.explorer.invalidate_git_decorations();
                                self.refresh_explorer_tree_async();
                            }
                            self.notify_error(format!("Git {action} failed: {error}"));
                        }
                    }
                }
                crate::background::BackgroundResult::GitBranchesLoaded(result) => {
                    self.bg_git_branch_picker_running = false;
                    match result {
                        Ok(items) => {
                            self.git_branch_picker_items = items;
                            self.git_branch_picker_selected = 0;
                            self.git_branch_picker_scroll_offset = 0;
                        }
                        Err(error) => {
                            self.git_branch_picker_active = false;
                            self.notify_error(error);
                        }
                    }
                }
                crate::background::BackgroundResult::ExternalOpenDone(result) => {
                    match result {
                        Ok(message) => self.notify(message),
                        Err(error) => self.notify_error(error),
                    }
                }
                crate::background::BackgroundResult::DeletionProgress(token) => {
                    if let Some(deletion) = &self.file_deletion
                        && deletion.cancel.same_task(&token) {
                        deletion.shared.acknowledge();
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::FilesystemActionDone { action, result } => {
                    self.bg_filesystem_action_running = false;
                    if action == "Delete" && let Some(deletion) = &mut self.file_deletion {
                        if deletion.visible() || result.as_ref().map_or(true, |outcome| outcome.warning) { deletion.finished = Some(std::time::Instant::now()); deletion.shown = true; }
                        else { self.file_deletion = None; }
                    }
                    match result {
                        Ok(outcome) => {
                            if outcome.finish_rename.as_ref().is_some_and(|target| self.rename.target.as_ref() == Some(target)) {
                                self.rename.active = false; self.rename.target = None;
                                self.rename.placeholder_active = false; self.creating_new_file = false; self.creating_new_folder = false;
                            }
                            let update_tabs = |tabs: &mut Vec<crate::app::Tab>| {
                                for tab in tabs {
                                    if tab.kind == TabKind::GitRevision { continue; }
                                    tab.editor.refresh_document();
                                    let Some(path) = tab.editor.file_path.clone() else {
                                        continue;
                                    };
                                    for (from, to) in &outcome.renames {
                                        if let Ok(suffix) = path.strip_prefix(from) {
                                            tab.editor.file_path = Some(to.join(suffix));
                                            tab.editor.publish_document();
                                            break;
                                        }
                                    }
                                }
                            };
                            update_tabs(&mut self.tabs);
                            for group in &mut self.groups {
                                update_tabs(&mut group.tabs);
                            }
                            for (from, to) in &outcome.renames {
                                self.navigation.rename_path(from, to);
                                self.lsp_manager.did_rename_files(from, to);
                            }
                            for path in &outcome.deleted {
                                self.close_deleted_file_tabs(path, true);
                            }
                            // A cancelled or failed directory deletion may remove
                            // only some children. Probe open files asynchronously;
                            // never close all tabs merely because their root was selected.
                            if !outcome.check_deleted_paths.is_empty() {
                                let paths: Vec<_> = self.tabs.iter()
                                    .chain(self.groups.iter().flat_map(|group| &group.tabs))
                                    .filter_map(|tab| tab.editor.file_path.as_ref())
                                    .filter(|path| outcome.check_deleted_paths.iter().any(|root| path.ancestors().any(|ancestor| Self::same_open_path(ancestor, root))))
                                    .cloned().collect();
                                self.queue_file_disk_checks(paths);
                            }
                            if outcome.clear_explorer_clipboard {
                                self.explorer_clipboard = None;
                            }
                            self.explorer_multi_selected.retain(|path| {
                                !outcome
                                    .deleted
                                    .iter()
                                    .any(|deleted| path.ancestors().any(|ancestor| Self::same_open_path(ancestor, deleted)))
                            });
                            self.explorer_hover.invalidate(None);
                            self.explorer_hover_info_cache.clear();
                            self.explorer_line_count_cache.clear();
                            self.refresh_explorer_async();
                            if let Some(path) = &outcome.open_file { self.open_file(path); }
                            self.status_message = outcome.message.clone();
                            if outcome.warning { self.notify_warn(outcome.message); }
                            else { self.notify(outcome.message); }
                        }
                        Err(error) => self.notify_error(format!("{action} failed: {error}")),
                    }
                }
                crate::background::BackgroundResult::GlobalSearchFailed { generation, error } => {
                    if generation == self.search_generation {
                        self.bg_global_search_running = false;
                        self.global_search_error = Some(error);
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::PastePrepared { serial, result } => {
                    self.finish_paste(serial, result);
                    has_visual_changes = true;
                }
                crate::background::BackgroundResult::ExitRecoverySaved { generation, result } => {
                    self.finish_exit_recovery(generation, result);
                    has_visual_changes = true;
                }
                crate::background::BackgroundResult::GlobalSearchResults {
                    results,
                    done,
                    generation,
                    truncated,
                } => {
                    // Discard stale results from a previous search
                    if generation == self.search_generation {
                        has_visual_changes = true;
                        if truncated {
                            self.global_search_truncated = true;
                        }
                        let remaining = self
                            .max_global_search_results
                            .saturating_sub(self.global_search_results.len());
                        if remaining == 0 {
                            if !results.is_empty() {
                                self.global_search_truncated = true;
                            }
                        } else {
                            if results.len() > remaining {
                                self.global_search_truncated = true;
                            }
                            self.global_search_results
                                .extend(results.into_iter().take(remaining));
                            self.ensure_global_search_selection_visible();
                        }
                        if self.global_search_truncated {
                            if let Some(token) = self.search_cancel_token.as_ref() {
                                token.cancel();
                            }
                            self.bg_global_search_running = false;
                        }
                        if done {
                            self.bg_global_search_running = false;
                        }
                    }
                }
                crate::background::BackgroundResult::SearchMatches {
                    generation,
                    target,
                    matches,
                    truncated,
                    error,
                } => {
                    if generation == self.file_search_generation {
                        self.bg_file_search_running = false;
                        self.file_search_cancel_token = None;
                        if self.editor_content_target().as_ref() == Some(&target) {
                            self.apply_file_search_matches(target, matches, truncated, error);
                            has_visual_changes = true;
                        }
                    }
                }
                crate::background::BackgroundResult::FileReplaceDone {
                    generation,
                    target,
                    source_document_id,
                    prepared,
                    replacements,
                } => {
                    if generation == self.file_replace_generation {
                        self.bg_file_replace_running = false;
                        // Another pane may have edited the shared document
                        // since this view was last drawn. Validate live state.
                        let _ = self.editor_mut();
                        if self.editor_content_target().as_ref() == Some(&target)
                            && self.editor().is_some_and(|editor| editor.save_identity == source_document_id)
                        {
                            self.apply_file_replacement(&prepared, replacements);
                            has_visual_changes = true;
                        } else {
                            self.update_search();
                            self.notify("Replace all cancelled because the file changed");
                        }
                    }
                }
                crate::background::BackgroundResult::GlobalReplaceOpenPrepared { workspace_generation, generation, open_files, failures } => {
                    has_visual_changes |= self.apply_workspace_replace_prepared(workspace_generation, generation, open_files, failures);
                }
                crate::background::BackgroundResult::GlobalReplaceDone { workspace_generation, generation, outcome, candidates, error } => {
                    has_visual_changes |= self.finish_workspace_replace(workspace_generation, generation, outcome, candidates, error);
                }
                crate::background::BackgroundResult::SourceSymbols(reply) => {
                    has_visual_changes |= self.apply_source_symbols(reply);
                }
                crate::background::BackgroundResult::QuickOpenFiles { files, generation } => {
                    if generation == self.quick_open_generation {
                        self.quick_open_results = files.into_iter().map(|(_, p)| p).collect();
                        self.bg_quick_open_running = false;
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::ChatFileSuggestions { agent, generation, root, paths } => {
                    has_visual_changes |= self.with_agent_context(agent, agent != self.chat.active_agent,
                        |app| app.apply_chat_file_suggestions(generation, root, paths)).unwrap_or(false);
                }
                crate::background::BackgroundResult::ChatAttachmentsPrepared { agent, serial, result } => {
                    has_visual_changes |= self.with_agent_context(agent, agent != self.chat.active_agent,
                        |app| app.apply_remote_chat_attachments(serial, result)).unwrap_or(false);
                }
                crate::background::BackgroundResult::ChatPastePrepared { agent, serial, result } => {
                    has_visual_changes |= self.with_agent_context(agent, agent != self.chat.active_agent,
                        |app| app.apply_remote_chat_paste(serial, result)).unwrap_or(false);
                }
                crate::background::BackgroundResult::ChatPromptPrepared { agent, session, serial, result, draft } => {
                    has_visual_changes |= self.with_agent_context(agent, agent != self.chat.active_agent,
                        |app| app.apply_chat_prompt_context(session, serial, result, draft)).unwrap_or(false);
                }
                crate::background::BackgroundResult::FileDiskState(change) => {
                    self.pending_disk_checks.remove(&change.path);
                    // A real write schedules a fresh probe. A locally prepared
                    // remote no-op does not; retain actual concurrent hints so
                    // a deletion/change cannot disappear behind that no-op.
                    if self.pending_file_saves.contains_key(&change.path) {
                        if crate::remote::workspace::is_view_key(&change.path) {
                            self.remote_changes_deferred.insert(change.path);
                        }
                        continue;
                    }
                    if self.chat.rollback.defer_disk_change(&change) {
                        if !self.chat.rollback.file_busy(&change.path) {
                            self.queue_file_disk_checks(std::iter::once(change.path));
                        }
                        continue;
                    }
                    if self.source_control_diff_disk_changed(&change) {
                        self.refresh_source_control_diffs_for_paths(Some(&[change.path.clone()].into_iter().collect()));
                    }
                    if !change.exists {
                        let closed = self.close_deleted_file_tabs(&change.path, false);
                        if closed {
                            self.explorer_hover.invalidate(Some(&change.path));
                            self.explorer_hover_info_cache.remove(&change.path);
                            self.explorer_line_count_cache.remove(&change.path);
                            self.notify_warn(format!(
                                "File deleted externally: {}",
                                change.path.display()
                            ));
                            if self.compare_base_path.as_ref() == Some(&change.path) {
                                self.compare_diff_base_revision = None;
                                self.diff_refresh_generation =
                                    self.diff_refresh_generation.wrapping_add(1);
                                self.bg_diff_refresh_running = false;
                                self.refresh_compare_diff();
                            }
                        }
                        has_visual_changes |= closed;
                        self.chat.rollback.note_workspace_change(&change.path);
                        continue;
                    }
                    let mut disk_state_changed = false;
                    let mut should_reload = false;
                    let mut changed_unsaved = Vec::new();
                    let remote_path = self.remote_workspace.is_some() && crate::remote::workspace::is_view_key(&change.path);
                    let mut inspect_tabs = |tabs: &mut Vec<crate::app::Tab>| {
                        for tab in tabs {
                            tab.editor.refresh_document();
                            if tab.editor.file_path.as_deref() != Some(change.path.as_path())
                                || tab.editor.bg_loading
                                || matches!(tab.kind, TabKind::Diff(_) | TabKind::GitRevision | TabKind::CommitDetails)
                            {
                                continue;
                            }
                            let was_unavailable = tab.editor.disk_unavailable;
                            tab.editor.disk_unavailable = false;
                            let changed =
                                match (tab.editor.last_disk_modified, change.modified) {
                                    (Some(old), Some(new)) => new != old,
                                    (None, Some(_)) => true,
                                    _ => false,
                                } || change.len.is_some_and(|len| len != tab.editor.file_size);
                            disk_state_changed |= was_unavailable || changed;
                            if changed {
                                tab.editor.last_disk_modified = change.modified;
                                if let Some(len) = change.len {
                                    // Record the disk state that produced this
                                    // warning so the same metadata probe does
                                    // not notify and redraw every 500 ms.
                                    tab.editor.file_size = len;
                                }
                                tab.editor.publish_document();
                                if tab.editor.modified && !remote_path {
                                    changed_unsaved.push(tab.editor.file_name().to_string());
                                } else { should_reload = true; }
                            }
                        }
                    };
                    inspect_tabs(&mut self.tabs);
                    for group in &mut self.groups {
                        inspect_tabs(&mut group.tabs);
                    }

                    if disk_state_changed {
                        self.explorer_hover.invalidate(Some(&change.path));
                        self.explorer_hover_info_cache.remove(&change.path);
                        self.explorer_line_count_cache.remove(&change.path);
                    }
                    if should_reload {
                        if remote_path { self.queue_remote_file_change(change.path.clone()); }
                        else { self.queue_file_reload(change.path.clone()); }
                    }
                    changed_unsaved.sort();
                    changed_unsaved.dedup();
                    for name in changed_unsaved {
                        self.status_message = format!("{name} changed on disk; your unsaved edits are preserved.");
                        self.notify_warn(self.status_message.clone());
                    }
                    if disk_state_changed && self.compare_base_path.as_ref() == Some(&change.path) {
                        self.compare_diff_base_revision = None;
                        self.diff_refresh_generation = self.diff_refresh_generation.wrapping_add(1);
                        self.bg_diff_refresh_running = false;
                        self.refresh_compare_diff();
                    }
                    has_visual_changes |= disk_state_changed;
                    if disk_state_changed {
                        self.chat.rollback.note_workspace_change(&change.path);
                    }
                }
                crate::background::BackgroundResult::FileWatcherState { root, active } => {
                    if root == self.explorer.root {
                        self.workspace_watcher_active = active;
                        self.explorer.set_directory_watcher_active(active);
                    }
                }
                crate::background::BackgroundResult::FilesChanged {
                    root,
                    changes,
                    structure_changed,
                    rescan,
                    git_metadata_changed,
                } => {
                    if root != self.explorer.root {
                        continue;
                    }
                    self.completion_index_files_changed(&changes, rescan);
                    if rescan || (structure_changed && changes.iter().any(|change| change.path.file_name().is_some_and(|name| name == ".git"))) {
                        self.repositories.rescan = true;
                    }
                    let git_metadata_changed = self.source_control_enabled && (git_metadata_changed || rescan);
                    let active_path = self.editor().and_then(|editor| editor.file_path.as_ref());
                    let blame_inputs_changed = git_metadata_changed
                        || active_path.is_some_and(|active_path| {
                            changes.iter().any(|change| change.path == *active_path)
                        });
                    let extension_discovery_changed = rescan || changes.iter().any(|change| {
                        self.extension_manager
                            .path_may_affect_discovery(&change.path)
                    });
                    // A watcher burst can contain thousands of paths (especially while a
                    // compiler is active). Refresh workspace-wide state only once per burst.
                    let explorer_generation = self.explorer_refresh_generation;
                    // Invalidate before scheduling: path changes and Git metadata
                    // in the same burst intentionally share one tree rebuild.
                    if git_metadata_changed {
                        self.explorer.invalidate_git_decorations();
                    }
                    if rescan {
                        self.explorer_hover.invalidate(None);
                        self.explorer_hover_info_cache.clear();
                        self.explorer_line_count_cache.clear();
                        self.refresh_explorer_async();
                        let paths: rustc_hash::FxHashSet<_> = self.tabs.iter()
                            .chain(self.groups.iter().flat_map(|group| &group.tabs))
                            .filter(|tab| tab.kind == TabKind::File)
                            .filter_map(|tab| tab.editor.file_path.clone()).collect();
                        if self.remote_workspace.is_some() {
                            // Overflow means path-level events were lost. Check
                            // bytes for open models; metadata alone can miss a
                            // replacement which retained its size and mtime.
                            for path in paths { self.queue_remote_file_change(path); }
                        } else { self.queue_file_disk_checks(paths); }
                    } else if !changes.is_empty() {
                        self.refresh_explorer_changed_paths(
                            changes.iter().map(|change| change.path.as_path()),
                            structure_changed || changes.iter().any(|change| !change.exists),
                        );
                    }
                    if git_metadata_changed && explorer_generation == self.explorer_refresh_generation {
                        // Index, config and info/exclude changes also affect
                        // ignored decorations, without changing directory lists.
                        self.refresh_explorer_tree_async();
                    }
                    if git_metadata_changed {
                        if rescan { self.refresh_source_control_async(); }
                        else { self.refresh_source_control_git_changed_paths(changes.iter().map(|change| change.path.clone())); }
                    } else {
                        self.refresh_source_control_changed_paths(changes.iter().map(|change| change.path.clone()));
                    }
                    if git_metadata_changed {
                        self.refresh_git_log_async();
                        self.refresh_git_branch_async();
                    }
                    if blame_inputs_changed {
                        self.blame_requested_path = None;
                        self.blame_documents.retain(|_, document| document.revision.is_some());
                    }

                    // Process open editors for each relevant path without rebuilding the
                    // Explorer or re-running Git status for every individual event.
                    let process_tabs =
                        |tabs: &mut Vec<crate::app::Tab>,
                         reload_paths: &mut rustc_hash::FxHashSet<PathBuf>,
                         changed_unsaved: &mut rustc_hash::FxHashSet<PathBuf>,
                         change: &crate::background::WorkspacePathChange| {
                            for tab in tabs.iter_mut() {
                                tab.editor.refresh_document();
                                if tab.editor.bg_loading || matches!(tab.kind, TabKind::Diff(_) | TabKind::GitRevision | TabKind::CommitDetails) {
                                    continue;
                                }
                                let matches_exact =
                                    tab.editor.file_path.as_deref() == Some(change.path.as_path());

                                if matches_exact {
                                    tab.editor.disk_unavailable = false;
                                    let changed =
                                        match (tab.editor.last_disk_modified, change.modified) {
                                            (Some(old), Some(new)) => new != old,
                                            (None, Some(_)) => true,
                                            _ => false,
                                        } || change
                                            .len
                                            .is_some_and(|len| len != tab.editor.file_size);
                                    if changed {
                                        tab.editor.last_disk_modified = change.modified;
                                        if let Some(len) = change.len { tab.editor.file_size = len; }
                                        tab.editor.publish_document();
                                        if tab.editor.modified { changed_unsaved.insert(change.path.clone()); }
                                        else { reload_paths.insert(change.path.clone()); }
                                    }
                                }
                            }
                        };

                    let structural_descendants: rustc_hash::FxHashSet<_> = if structure_changed && !rescan {
                        let direct: rustc_hash::FxHashSet<_> = changes.iter().map(|change| change.path.as_path()).collect();
                        let parents: rustc_hash::FxHashSet<_> = changes.iter().filter(|change| change.exists).map(|change| change.path.as_path()).collect();
                        self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
                            .filter_map(|tab| {
                                if tab.kind != TabKind::File { return None; }
                                tab.editor.refresh_document();
                                let path = tab.editor.file_path.as_ref()?;
                                (!direct.contains(path.as_path()) && path.ancestors().skip(1).any(|parent| parents.contains(parent)))
                                    .then(|| path.clone())
                            }).collect()
                    } else { Default::default() };
                    let mut deleted_names: Vec<String> = Vec::new();
                    let mut changed_unsaved = rustc_hash::FxHashSet::default();
                    let mut reload_paths = rustc_hash::FxHashSet::default();
                    let mut compare_base_changed = false;
                    for change in changes {
                        if self.chat.rollback.defer_disk_change(&change) {
                            if !self.chat.rollback.file_busy(&change.path) {
                                self.queue_file_disk_checks(std::iter::once(change.path));
                            }
                            continue;
                        }
                        self.chat.rollback.note_workspace_change(&change.path);
                        self.explorer_hover.invalidate(Some(&change.path));
                        self.explorer_hover_info_cache.remove(&change.path);
                        self.explorer_line_count_cache.remove(&change.path);
                        compare_base_changed |=
                            self.compare_base_path.as_ref() == Some(&change.path);
                        self.lsp_manager
                            .notify_did_change_watched_files(&change.path);
                        if crate::remote::workspace::is_view_key(&change.path) && self.remote_workspace.is_some() {
                            if change.exists || self.pending_file_saves.contains_key(&change.path) {
                                self.queue_remote_file_change(change.path);
                            } else {
                                self.queue_file_disk_checks(std::iter::once(change.path));
                            }
                            continue;
                        }
                        if self.pending_file_saves.contains_key(&change.path) { continue; }
                        if !change.exists {
                            if self.close_deleted_file_tabs(&change.path, false) {
                                has_visual_changes = true;
                                deleted_names.push(
                                    change
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned(),
                                );
                            }
                            continue;
                        }
                        process_tabs(&mut self.tabs, &mut reload_paths, &mut changed_unsaved, &change);
                        for group in &mut self.groups {
                            process_tabs(&mut group.tabs, &mut reload_paths, &mut changed_unsaved, &change);
                        }
                    }
                    if compare_base_changed {
                        self.compare_diff_base_revision = None;
                        self.diff_refresh_generation = self.diff_refresh_generation.wrapping_add(1);
                        self.bg_diff_refresh_running = false;
                        self.refresh_compare_diff();
                    }
                    for path in reload_paths {
                        self.queue_file_reload(path);
                    }
                    for path in structural_descendants {
                        // A directory can be atomically replaced by another
                        // tree (or a regular file) without child-level events.
                        if crate::remote::workspace::is_view_key(&path) { self.queue_remote_file_change(path); }
                        else { self.queue_file_disk_checks(std::iter::once(path)); }
                    }
                    for path in changed_unsaved {
                        self.status_message = format!("{} changed on disk; your unsaved edits are preserved.", path.display());
                        self.notify_warn(self.status_message.clone());
                        has_visual_changes = true;
                    }

                    // Deduplicate notifications when a file is open in multiple groups.
                    deleted_names.sort();
                    deleted_names.dedup();
                    if deleted_names.len() == 1 {
                        self.notify_warn(format!("File deleted externally: {}", deleted_names[0]));
                    } else if !deleted_names.is_empty() {
                        self.notify_warn(format!(
                            "{} files deleted externally",
                            deleted_names.len()
                        ));
                    }
                    if extension_discovery_changed {
                        self.request_extension_scan();
                    }
                    self.refresh_git_line_status_async();
                }
                crate::background::BackgroundResult::HighlightBatch(batch) => {
                    let file_path = batch.file_path.clone();
                    let content_version = batch.content_version;
                    let provisional = batch.is_provisional();
                    let outcome = self.highlighter.apply_batch_with_outcome(batch);
                    let applied = outcome.accepted();
                    has_visual_changes |= outcome.visual_changes();
                    if !provisional
                        && applied
                        && self
                            .highlighter
                            .document_state_converged(&file_path, content_version)
                    {
                        for tab in &mut self.tabs {
                            if tab.editor.syntax_file_path() == Some(file_path.as_path())
                                && tab.editor.content_version == content_version
                            {
                                tab.editor.syntax_dirty_from = None;
                                tab.editor.syntax_dirty_through = None;
                            }
                        }
                        for group in &mut self.groups {
                            for tab in &mut group.tabs {
                                if tab.editor.syntax_file_path() == Some(file_path.as_path())
                                    && tab.editor.content_version == content_version
                                {
                                    tab.editor.syntax_dirty_from = None;
                                    tab.editor.syntax_dirty_through = None;
                                }
                            }
                        }
                    }
                    if !provisional && applied {
                        if self.visible_highlight_key.as_ref().is_some_and(|key| key.0 == file_path && key.1 == content_version) {
                            self.visible_highlight_key = None;
                        }
                        for group in &mut self.groups {
                            if group.visible_highlight_key.as_ref().is_some_and(|key| key.0 == file_path && key.1 == content_version) {
                                group.visible_highlight_key = None;
                            }
                        }
                        // Clear acknowledged edits before scheduling again;
                        // otherwise a converged cache looks like a reopened
                        // document and its completed colors are discarded.
                        self.refresh_visible_highlights_async();
                    }
                }
                crate::background::BackgroundResult::HighlightFragments {
                    registry_generation,
                    file_path,
                    theme_key,
                    language,
                    entries,
                } => {
                    if registry_generation == self.highlighter.registry_generation() {
                        let has_entries = !entries.is_empty();
                        for (line, hash, spans) in entries {
                            self.highlighter.insert_fragment_cached(
                                &file_path,
                                &theme_key,
                                language.as_deref(),
                                line,
                                hash,
                                spans,
                            );
                        }
                        // This request covered the complete diff viewport.
                        // Retain its key until the viewport, document, or theme changes.
                        has_visual_changes |= has_entries;
                    }
                }
                crate::background::BackgroundResult::SessionUntitledLoaded { restore } => {
                    let threshold = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                    for tab in self.tabs.iter_mut().chain(
                        self.groups.iter_mut().flat_map(|group| &mut group.tabs),
                    ) {
                        if tab.editor.bg_loading
                            && tab.editor.file_path.is_none()
                            && tab.editor.pending_session_restore.as_ref()
                                .is_some_and(|pending| pending.id == restore.id)
                        {
                            tab.editor.bg_loading = false;
                            tab.editor.read_only = self.default_read_only;
                            tab.editor.pending_session_restore = Some(restore.clone());
                            tab.editor.apply_pending_session_restore(threshold);
                            has_visual_changes = true;
                        }
                    }
                }
                crate::background::BackgroundResult::FileLoaded {
                    workspace_generation,
                    path,
                    rope,
                    encoding,
                    file_size,
                    last_disk_modified,
                    session_restores,
                    skip_open_prompt,
                } => {
                    if workspace_generation != self.workspace_generation
                        || !self.has_pending_file_load(&path, &session_restores)
                    {
                        continue;
                    }
                    let open_commands = if skip_open_prompt {
                        Vec::new()
                    } else {
                        let mut commands = self.extension_manager.file_open_commands_for_path(&path);
                        if encoding == "Binary" {
                            for command in self.extension_manager.binary_open_commands() {
                                if !commands.iter().any(|existing| {
                                    existing.extension_id == command.extension_id
                                        && existing.command_id == command.command_id
                                }) {
                                    commands.push(command);
                                }
                            }
                        }
                        commands
                    };
                    if !open_commands.is_empty() {
                        let mut removed_placeholder = false;
                        crate::tab_collection::retain(&mut self.tabs, &mut self.active_tab, &mut self.tab_mru_order, |_, tab| {
                            let remove = tab.editor.bg_loading
                                && tab.editor.pending_session_restore.is_none()
                                && tab.editor.file_path.as_deref() == Some(path.as_path());
                            removed_placeholder |= remove;
                            !remove
                        });
                        for group in &mut self.groups {
                            crate::tab_collection::retain(&mut group.tabs, &mut group.active_tab, &mut group.tab_mru_order, |_, tab| {
                                let remove = tab.editor.bg_loading
                                    && tab.editor.pending_session_restore.is_none()
                                    && tab.editor.file_path.as_deref() == Some(path.as_path());
                                removed_placeholder |= remove;
                                !remove
                            });
                        }
                        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
                        if removed_placeholder {
                            self.binary_open_prompt_active = true;
                            self.binary_open_prompt_path = Some(path.clone());
                            self.binary_open_prompt_selected = 1.min(open_commands.len());
                            self.binary_open_prompt_commands = open_commands;
                            self.binary_open_prompt_open_normally = encoding != "Binary";
                            self.notify(if encoding == "Binary" {
                                "Binary file detected"
                            } else {
                                "Additional open options available"
                            });
                            has_visual_changes = true;
                        }
                        continue;
                    }

                    let language_associations = self.language_associations.clone();
                    let scope_rules = self.animation_settings.scope_languages.clone();
                    let default_read_only = self.default_read_only;
                    let default_word_wrap = self.default_word_wrap;
                    let word_wrap_column = self.word_wrap_column;
                    let default_whitespace_mode = self.default_whitespace_mode;
                    let default_tab_size = self.default_tab_size;
                    let default_use_tabs = self.default_use_tabs;
                    let default_cursor_surrounding_lines = self.default_cursor_surrounding_lines;
                    let large_file_threshold_bytes =
                        self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                    let session_result = !session_restores.is_empty();
                    let session_restores: rustc_hash::FxHashMap<_, _> = session_restores
                        .into_iter()
                        .map(|restore| (restore.id, restore.tab))
                        .collect();
                    let mut applied = false;
                    let mut applied_file = false;
                    let mut loaded_large_file_mode = false;
                    let navigation = &mut self.navigation;
                    let mut apply_loaded = |tab: &mut crate::app::Tab| {
                        if tab.editor.file_path.as_deref() != Some(&path) || !tab.editor.bg_loading {
                            return;
                        }
                        let pending_restore_id = tab
                            .editor
                            .pending_session_restore
                            .as_ref()
                            .map(|restore| restore.id);
                        let matching_restore = pending_restore_id
                            .and_then(|id| session_restores.get(&id).cloned().map(|saved| (id, saved)));
                        let belongs_to_result = if session_result {
                            matching_restore.is_some()
                        } else {
                            pending_restore_id.is_none()
                        };
                        if belongs_to_result {
                            let pending_selection = crate::app_navigation::NavigationSelection::capture(&tab.editor);
                            if let Some((id, saved)) = matching_restore {
                                tab.editor.pending_session_restore =
                                    Some(crate::session::SessionRestore { id, tab: saved });
                            }
                            // Rope clones share their tree, so this remains O(1) even when
                            // the same file has a placeholder in more than one split group.
                            tab.editor.apply_bg_loaded(
                                rope.clone(),
                                encoding,
                                file_size,
                                large_file_threshold_bytes,
                            );
                            tab.editor.last_disk_modified = last_disk_modified;
                            tab.editor.set_remote_checkpoint(remote_checkpoint.clone());
                            tab.editor.language_associations = language_associations.clone();
                            tab.editor.scope_bubble_rules = scope_rules.clone();
                            tab.editor.read_only = default_read_only || encoding == "Binary"
                                || matches!(tab.kind, TabKind::Diff(_) | TabKind::CommitDetails);
                            tab.editor.word_wrap = default_word_wrap && !tab.editor.large_file_mode;
                            tab.editor.word_wrap_column = word_wrap_column;
                            tab.editor.whitespace_mode = default_whitespace_mode;
                            tab.editor.tab_size = default_tab_size;
                            tab.editor.use_tabs = default_use_tabs;
                            tab.editor.cursor_surrounding_lines = default_cursor_surrounding_lines;
                            tab.editor
                                .apply_pending_session_restore(large_file_threshold_bytes);
                            if let Some((line, col)) = tab.editor.pending_cursor_position.take() {
                                let col = if std::mem::take(&mut tab.editor.pending_cursor_utf16) {
                                    crate::lsp_positions::from_utf16(&tab.editor.rope, line, col as u32)
                                } else { col };
                                tab.editor.set_cursor(line, col);
                            }
                            if let Some(selection) = tab.editor.pending_navigation_selection.take() {
                                selection.restore(&mut tab.editor);
                            }
                            navigation.resolve_loaded_editor(&tab.editor, pending_selection);
                            loaded_large_file_mode |= tab.editor.large_file_mode;
                            applied_file |= tab.kind == TabKind::File;
                            applied = true;
                        }
                    };

                    // A tab can move to an inactive split while its file is still
                    // loading. Update every matching placeholder, not only the tabs
                    // belonging to the group that happens to be active at completion.
                    for tab in &mut self.tabs {
                        apply_loaded(tab);
                    }
                    for group in &mut self.groups {
                        for tab in &mut group.tabs {
                            apply_loaded(tab);
                        }
                    }
                    if applied {
                        has_visual_changes = true;
                        self.status_message = format!("Opened: {}", self.display_path(&path));
                        if applied_file
                            && self.editor().and_then(|editor| editor.file_path.as_ref())
                                == Some(&path)
                        {
                            self.blame_requested_path = None;
                        }
                        if session_result {
                            self.queue_file_disk_checks(std::iter::once(path.clone()));
                        }
                        if encoding == "Binary" {
                            self.notify("Binary file — editing disabled");
                        } else if !loaded_large_file_mode && applied_file {
                            self.queue_lsp_document_sync(path.clone(), 0, rope.clone());
                        } else if loaded_large_file_mode {
                            self.notify(format!(
                                "Large file mode: rich features paused for {}",
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("file")
                            ));
                        }
                        if !loaded_large_file_mode {
                            self.start_pending_git_compare_for_path(&path);
                            self.refresh_compare_diff();
                        }
                        if self.sidebar_view == crate::app::SidebarView::Outline
                            && self.editor().is_some_and(|editor| {
                                editor.file_path.as_deref() == Some(path.as_path())
                                    && !editor.large_file_mode
                            })
                        {
                            self.update_outline();
                        }
                    }
                    if self.editor().and_then(|editor| editor.file_path.as_deref())
                        == Some(path.as_path())
                        && self.pending_git_compare_path.as_ref() != Some(&path)
                        && self.editor().is_some_and(|editor| !editor.large_file_mode)
                    {
                        self.refresh_git_line_status_async();
                    }
                }
                crate::background::BackgroundResult::FileLoadFailed {
                    workspace_generation,
                    path,
                    error,
                    session_restores,
                } => {
                    if workspace_generation != self.workspace_generation
                        || !self.has_pending_file_load(&path, &session_restores)
                    {
                        continue;
                    }
                    if session_restores.is_empty() && self.skip_failed_navigation(&path) {
                        has_visual_changes = true;
                        continue;
                    }
                    let session_result = !session_restores.is_empty();
                    let session_restores: rustc_hash::FxHashMap<_, _> = session_restores
                        .into_iter()
                        .map(|restore| (restore.id, restore.tab))
                        .collect();
                    let is_missing_session_tab = |tab: &crate::app::Tab| {
                        if tab.editor.file_path.as_deref() != Some(path.as_path())
                            || !tab.editor.bg_loading
                            || !session_result
                        {
                            return false;
                        }
                        tab.editor
                            .pending_session_restore
                            .as_ref()
                            .and_then(|restore| session_restores.get(&restore.id))
                            .is_some_and(|saved| saved.pending_unsaved_content.is_none())
                    };
                    let mut removed_session_tabs = 0usize;
                    crate::tab_collection::retain(&mut self.tabs, &mut self.active_tab, &mut self.tab_mru_order, |_, tab| {
                        let remove = is_missing_session_tab(tab);
                        removed_session_tabs += usize::from(remove);
                        !remove
                    });
                    for group in &mut self.groups {
                        crate::tab_collection::retain(&mut group.tabs, &mut group.active_tab, &mut group.tab_mru_order, |_, tab| {
                            let remove = is_missing_session_tab(tab);
                            removed_session_tabs += usize::from(remove);
                            !remove
                        });
                        group.active_tab = group.active_tab.min(group.tabs.len().saturating_sub(1));
                    }
                    self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));

                    let mut applied = false;
                    let mut recovered_unsaved = false;
                    let threshold_bytes = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                    let default_read_only = self.default_read_only;
                    let navigation = &mut self.navigation;
                    let mut apply_error = |tab: &mut crate::app::Tab| {
                        if tab.editor.file_path.as_deref() != Some(&path) || !tab.editor.bg_loading {
                            return;
                        }
                        let pending_restore_id = tab
                            .editor
                            .pending_session_restore
                            .as_ref()
                            .map(|restore| restore.id);
                        let matching_restore = pending_restore_id
                            .and_then(|id| session_restores.get(&id).cloned().map(|saved| (id, saved)));
                        let belongs_to_result = if session_result {
                            matching_restore.is_some()
                        } else {
                            pending_restore_id.is_none()
                        };
                        if belongs_to_result {
                            let pending_selection = crate::app_navigation::NavigationSelection::capture(&tab.editor);
                            if let Some((id, saved)) = matching_restore {
                                tab.editor.pending_session_restore =
                                    Some(crate::session::SessionRestore { id, tab: saved });
                            }
                            let has_unsaved_restore = tab
                                .editor
                                .pending_session_restore
                                .as_ref()
                                .and_then(|restore| {
                                    restore.tab.pending_unsaved_content.as_ref()
                                })
                                .is_some();
                            if has_unsaved_restore {
                                tab.editor.rope = ropey::Rope::new();
                                tab.editor.saved_rope = None;
                                tab.editor.bg_loading = false;
                                tab.editor.load_failed = false;
                                tab.editor.read_only = default_read_only || matches!(tab.kind, TabKind::Diff(_) | TabKind::CommitDetails);
                                tab.editor.apply_pending_session_restore(threshold_bytes);
                                if let Some((line, col)) = tab.editor.pending_cursor_position.take() {
                                    let col = if std::mem::take(&mut tab.editor.pending_cursor_utf16) {
                                        crate::lsp_positions::from_utf16(&tab.editor.rope, line, col as u32)
                                    } else { col };
                                    tab.editor.set_cursor(line, col);
                                }
                                if let Some(selection) = tab.editor.pending_navigation_selection.take() {
                                    selection.restore(&mut tab.editor);
                                }
                                navigation.resolve_loaded_editor(&tab.editor, pending_selection);
                                tab.editor.disk_unavailable = true;
                                recovered_unsaved = true;
                            } else {
                                tab.editor.fold_hint_cache.get_mut().clear();
                                tab.editor.minimap_cache.get_mut().clear();
                                tab.editor.rope =
                                    ropey::Rope::from_str(&format!("Error loading file:\n{error}"));
                                tab.editor.load_failed = true;
                                tab.editor.mark_syntax_dirty_range(0, tab.editor.rope.len_lines());
                                tab.editor.bg_loading = false;
                                tab.editor.pending_cursor_position = None;
                                tab.editor.pending_navigation_selection = None;
                                tab.editor.disk_unavailable = true;
                            }
                            applied = true;
                        }
                    };
                    for tab in &mut self.tabs {
                        apply_error(tab);
                    }
                    for group in &mut self.groups {
                        for tab in &mut group.tabs {
                            apply_error(tab);
                        }
                    }
                    if applied {
                        has_visual_changes = true;
                        if self.pending_git_compare_path.as_ref() == Some(&path) {
                            self.clear_diff_state();
                        }
                        if recovered_unsaved {
                            self.queue_file_disk_checks(std::iter::once(path.clone()));
                            self.status_message =
                                format!("Restored unsaved content: {}", path.display());
                            self.notify_warn(format!(
                                "Restored unsaved content; disk file is unavailable: {}",
                                path.display()
                            ));
                        } else {
                            self.status_message = format!("Error: {error}");
                        }
                    }
                    if removed_session_tabs > 0 {
                        has_visual_changes = true;
                        if self.pending_git_compare_path.as_ref() == Some(&path) {
                            self.clear_diff_state();
                        }
                        self.status_message =
                            format!("Session file unavailable: {}", path.display());
                        self.notify_warn(format!(
                            "Session file no longer exists: {}",
                            path.display()
                        ));
                    }
                }
                crate::background::BackgroundResult::FileReloaded { workspace_generation, path, targets, result } => {
                    if workspace_generation != self.workspace_generation {
                        continue;
                    }
                    self.pending_file_reloads.remove(&path);
                    if self.chat.rollback.file_busy(&path) || self.pending_file_saves.contains_key(&path) {
                        self.pending_file_reload_again.remove(&path);
                        continue;
                    }
                    // If another watcher event arrived during the read, discard
                    // this potentially stale snapshot and immediately read once
                    // more. The editor never flashes intermediate disk state.
                    if self.pending_file_reload_again.remove(&path) {
                        if crate::remote::workspace::is_view_key(&path) { self.queue_remote_file_change(path); }
                        else { self.queue_file_reload(path); }
                        continue;
                    }
                    self.refresh_source_control_diffs_for_paths(Some(&[path.clone()].into_iter().collect()));

                    match result {
                        Ok(loaded) => {
                            self.remote_change_warnings.remove(&path);
                            let mut applied = false;
                            let default_read_only = self.default_read_only;
                            let mut apply_reload = |tab: &mut crate::app::Tab| {
                                if tab.editor.file_path.as_deref() == Some(path.as_path())
                                    && tab.reloadable_file_path() == Some(path.as_path())
                                    && targets.contains(&(tab.editor.navigation_id, tab.editor.content_version))
                                {
                                    let was_placeholder = tab.editor.save_content_error().is_some();
                                    if tab.editor.apply_reloaded_file(loaded.clone()) {
                                        file_reload_names.push(tab.editor.file_name().to_string());
                                        applied = true;
                                    }
                                    if was_placeholder || loaded.encoding == "Binary" {
                                        tab.editor.read_only = default_read_only || loaded.encoding == "Binary";
                                        tab.editor.publish_document();
                                    }
                                }
                            };
                            for tab in &mut self.tabs {
                                apply_reload(tab);
                            }
                            for group in &mut self.groups {
                                for tab in &mut group.tabs {
                                    apply_reload(tab);
                                }
                            }

                            if applied {
                                self.refresh_compare_diff();
                            }
                            if !applied && crate::remote::workspace::is_view_key(&path)
                                && self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
                                    .any(|tab| tab.kind == TabKind::File && tab.editor.file_path.as_ref() == Some(&path)
                                        && tab.editor.modified && tab.editor.saved_rope.as_ref().is_some_and(|saved| !saved.is_instance(&loaded.rope))) {
                                self.queue_remote_file_change(path.clone());
                            }
                            if applied
                                && self.editor().and_then(|editor| editor.file_path.as_deref())
                                    == Some(path.as_path())
                            {
                                self.blame_requested_path = None;
                                self.refresh_git_line_status_async();
                            }
                            has_visual_changes |= applied;
                        }
                        Err(error) => {
                            let still_open =
                                self.tabs.iter().any(|tab| {
                                    tab.editor.file_path.as_deref() == Some(path.as_path())
                                }) || self.groups.iter().any(|group| {
                                    group.tabs.iter().any(|tab| {
                                        tab.editor.file_path.as_deref() == Some(path.as_path())
                                    })
                                });
                            if still_open {
                                has_visual_changes = true;
                                if self.remote_change_warnings.insert(path.clone()) { self.notify_error(format!(
                                    "Could not reload {}: {error}",
                                    self.display_path(&path)
                                )); }
                            }
                        }
                    }
                }
                crate::background::BackgroundResult::ExtensionCommandDone {
                    extension_index,
                    extension_id,
                    result,
                    runtime,
                } => {
                    // Restore runtime and apply results
                    self.extension_manager
                        .restore_runtime(extension_index, runtime);
                    match result {
                        Ok(msg) => {
                            self.notify(msg);
                            if let Some(view_id) = self
                                .extension_manager
                                .take_requested_open_view(&extension_id)
                            {
                                self.open_extension_view(&extension_id, &view_id);
                            }
                        }
                        Err(err) => self.notify(err),
                    }
                    self.refresh_open_extension_view_tabs();
                    self.resume_pending_extension_scan();
                    self.restore_pending_custom_documents();
                }
                crate::background::BackgroundResult::ExtensionViewEventDone {
                    extension_index, extension_id, result, runtime, close_view, exit_action,
                } => {
                    has_visual_changes |= self.extension_manager.restore_runtime(extension_index, runtime)
                        || exit_action || close_view.is_some() || result.is_err();
                    if exit_action {
                        self.custom_exit_remaining = self.custom_exit_remaining.saturating_sub(1);
                        self.custom_exit_failed |= result.is_err();
                    }
                    if let Err(error) = result { self.notify_error(error); }
                    for text in self.extension_manager.take_clipboard_writes() {
                        let _ = crate::clipboard::set_text(&text);
                        self.clipboard_history.push_front(text);
                        self.clipboard_history.truncate(50);
                    }
                    if let Some(view_id) = self.extension_manager.take_requested_open_view(&extension_id) {
                        has_visual_changes = true;
                        self.open_extension_view(&extension_id, &view_id);
                    }
                    self.refresh_open_extension_view_tabs();
                    if let Some(view_id) = close_view {
                        self.close_extension_document_tabs(&extension_id, &view_id);
                    }
                    self.resume_pending_extension_scan();
                    self.restore_pending_custom_documents();
                }
                crate::background::BackgroundResult::ExtensionTickDone {
                    extension_index,
                    result,
                    runtime,
                } => {
                    has_visual_changes |= self.extension_manager
                        .restore_runtime(extension_index, runtime) || result.is_err();
                    if let Err(error) = result {
                        self.notify_error(error);
                    }
                    for text in self.extension_manager.take_clipboard_writes() {
                        let _ = crate::clipboard::set_text(&text);
                        self.clipboard_history.push_front(text);
                        self.clipboard_history.truncate(50);
                    }
                    self.refresh_open_extension_view_tabs();
                    self.resume_pending_extension_scan();
                }
                crate::background::BackgroundResult::ExtensionsDiscovered {
                    generation,
                    discovered,
                    notices,
                } => {
                    self.bg_extension_scan_running = false;
                    if generation == self.extension_scan_generation {
                        has_visual_changes |=
                            self.apply_discovered_extensions(discovered, notices);
                    }
                    self.resume_pending_extension_scan();
                }
                crate::background::BackgroundResult::ExtensionsInitialized { generation, manager, notices } => {
                    has_visual_changes |= self.apply_initialized_extensions(generation, manager, notices);
                }
                crate::background::BackgroundResult::ThemeInstalled(result) => {
                    self.finish_theme_install(result);
                }
                crate::background::BackgroundResult::SyntaxInstalled { generation, result } => {
                    self.finish_syntax_install(generation, result);
                    has_visual_changes = true;
                }
                crate::background::BackgroundResult::ExtensionVersionsDiscovered { request, result } => {
                    self.finish_extension_versions(request, result);
                }
                crate::background::BackgroundResult::ExtensionInstalled { request, result } => {
                    self.finish_extension_install(request, result);
                }
                crate::background::BackgroundResult::ExtensionRecoveries(recoveries) => {
                    self.pending_custom_recoveries = recoveries;
                    self.restore_pending_custom_documents();
                }
                crate::background::BackgroundResult::ExternalThemesLoaded(themes) => {
                    has_visual_changes |= self.install_external_themes(themes);
                }
                crate::background::BackgroundResult::SyntaxAssetsLoaded { root, result } => {
                    has_visual_changes |= self.install_syntax_assets(root, result);
                }
                crate::background::BackgroundResult::StartupSyntaxCacheLoaded { root, loaded } => {
                    if root == self.explorer.root && !self.bg_syntax_asset_refresh_pending {
                        self.apply_loaded_syntax_assets(loaded);
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::GitLineDiff {
                    file_path,
                    status,
                } => {
                    self.bg_git_diff_running = false;
                    if !self.active_diff_uses_snapshot_target() {
                        has_visual_changes |= self
                            .apply_git_line_status_to_open_editors(&file_path, &status);
                    }
                    if self.bg_git_diff_pending {
                        self.bg_git_diff_pending = false;
                        self.refresh_git_line_status_async();
                    }
                }
                crate::background::BackgroundResult::GitCompare { target, result } => {
                    if target.generation != self.git_compare_generation {
                        continue;
                    }
                    self.bg_git_compare_running = false;

                    let current_matches = self.editor().is_some_and(|editor| {
                        !editor.bg_loading
                            && editor.file_path.as_ref() == Some(&target.file_path)
                            && editor.content_version == target.content_version
                            && editor.rope.len_bytes() == target.len_bytes
                            && editor.rope.len_lines() == target.line_count
                            && editor.last_disk_modified == target.last_disk_modified
                    });
                    if !current_matches {
                        if self.pending_git_compare_path.as_ref() == Some(&target.file_path)
                            && self.editor().and_then(|editor| editor.file_path.as_ref())
                                == Some(&target.file_path)
                        {
                            self.start_pending_git_compare_for_path(&target.file_path);
                        } else {
                            self.pending_git_compare_path = None;
                        }
                        continue;
                    }

                    has_visual_changes = true;
                    match result {
                        Ok(prepared) => {
                            self.pending_git_compare_path = None;
                            self.set_source_control_diff_revisions(prepared.source_revisions);
                            if let Some(rope) = prepared.target_snapshot {
                                let threshold =
                                    self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                                let file_size = rope.len_bytes() as u64;
                                if let Some(editor) = self.editor_mut() {
                                    editor.apply_bg_loaded(rope, "UTF-8", file_size, threshold);
                                }
                            }
                            self.compare_base_path = None;
                            self.compare_base_content = Some(prepared.base_content);
                            let revisions = match self.tabs.get(self.active_tab)
                                .and_then(|tab| self.diff_tabs.staged_for_tab(tab))
                            {
                                Some(true) => ("HEAD", "Index"),
                                Some(false) => ("Index", "Working Tree"),
                                None => ("HEAD", "Working Tree"),
                            };
                            self.diff_tab_label = Some(format!(
                                "{} ({}) ↔ {} ({})",
                                prepared.file_name, revisions.0, prepared.file_name, revisions.1
                            ));
                            let source_label = |revision| match revision {
                                "HEAD" => "HEAD (last commit)",
                                "Index" => "Index (staged)",
                                other => other,
                            };
                            self.diff_labels = Some(DiffLabels::new(
                                source_label(revisions.0),
                                source_label(revisions.1),
                            ));
                            self.diff_target_path = Some(target.file_path.clone());
                            self.diff_mode = if prepared.is_new_file {
                                DiffMode::Inline
                            } else {
                                self.preferred_diff_mode
                            };
                            self.apply_prepared_compare_diff(
                                prepared.diff_lines,
                                prepared.render_cache,
                                prepared.target_line_markers,
                            );
                            self.scroll_to_first_diff_change();
                            self.status_message = format!("Opened diff: {}", prepared.file_name);
                            // Reconcile events received while the initial comparison was loading.
                            self.refresh_dirty_source_control_diffs();
                        }
                        Err(error) => {
                            self.pending_git_compare_path = None;
                            self.clear_diff_state();
                            self.status_message = format!("Error: {error}");
                            self.notify_error(error);
                        }
                    }
                }
                crate::background::BackgroundResult::SourceControlDiffs {
                    root, generation, updates,
                } => {
                    has_visual_changes |=
                        self.finish_source_control_diff_refresh(root, generation, updates);
                }
                crate::background::BackgroundResult::DiffRefresh {
                    target,
                    base_revision,
                    result,
                } => {
                    if target.generation != self.diff_refresh_generation {
                        continue;
                    }
                    self.bg_diff_refresh_running = false;
                    let target_matches = self.active_group == target.active_group
                        && self.active_tab == target.active_tab
                        && self.editor().is_some_and(|editor| {
                            !editor.bg_loading
                                && editor.file_path == target.file_path
                                && editor.content_version == target.content_version
                                && editor.rope.len_bytes() == target.len_bytes
                                && editor.rope.len_lines() == target.line_count
                                && editor.last_disk_modified == target.last_disk_modified
                        });
                    let base_matches = match base_revision {
                        crate::app::DiffBaseRevision::DiskFile { .. }
                        | crate::app::DiffBaseRevision::Missing => {
                            self.compare_base_path.as_ref().is_some_and(|path| {
                                !self
                                    .tabs
                                    .iter()
                                    .chain(self.groups.iter().flat_map(|group| group.tabs.iter()))
                                    .any(|tab| tab.editor.file_path.as_ref() == Some(path))
                            })
                        }
                        _ => self.compare_base_revision() == base_revision,
                    };

                    if !target_matches || !base_matches {
                        if self.should_render_diff_for_active_editor() {
                            self.refresh_compare_diff();
                        }
                        continue;
                    }

                    has_visual_changes = true;
                    match result {
                        Ok(prepared) => {
                            let initial_result = self.compare_diff_target_version.is_none();
                            self.compare_base_content = Some(prepared.base_content);
                            self.pending_compare_base_rope = None;
                            self.apply_prepared_compare_diff(
                                prepared.diff_lines,
                                prepared.render_cache,
                                prepared.target_line_markers,
                            );
                            self.compare_diff_base_revision = Some(base_revision);
                            if initial_result {
                                self.scroll_to_first_diff_change();
                            }
                            if let Some(label) = self.diff_tab_label.as_deref() {
                                self.status_message = format!("Diff ready: {label}");
                            }
                        }
                        Err(error) => {
                            self.clear_diff_state();
                            self.status_message = format!("Error: {error}");
                            self.notify_error(error);
                        }
                    }
                }
                crate::background::BackgroundResult::EditorWorkCancelled { kind, token } => {
                    use crate::background::EditorWorkKind;
                    match kind {
                        EditorWorkKind::Metrics if self.pending_editor_analysis.as_ref().is_some_and(|(_, pending)| pending.same_task(&token)) => {
                            self.pending_editor_analysis = None;
                            self.bg_editor_analysis_running = false;
                            self.editor_analysis_deferred = self.editor_analysis_target().is_some_and(|target| {
                                self.cached_editor_analysis_target.as_ref() != Some(&target)
                            });
                        }
                        EditorWorkKind::Brackets if self.pending_bracket_analysis.as_ref().is_some_and(|(_, pending)| pending.same_task(&token)) => {
                            self.pending_bracket_analysis = None;
                            self.bg_bracket_analysis_running = false;
                            has_visual_changes |= self.bracket_pair_colorization && self.editor_analysis_target().is_some();
                        }
                        EditorWorkKind::Cursor if self.pending_cursor_match.as_ref().is_some_and(|(_, pending)| pending.same_task(&token)) => {
                            self.pending_cursor_match = None;
                            self.bg_cursor_match_running = false;
                            has_visual_changes |= self.cursor_match_target().is_some_and(|target| {
                                self.cached_cursor_match_target.as_ref() != Some(&target)
                            });
                        }
                        _ => {}
                    }
                }
                crate::background::BackgroundResult::EditorAnalysis {
                    target,
                    errors,
                    warnings,
                    max_line_width,
                    word_count,
                    unsaved_diff,
                    sticky_scopes,
                } => {
                    self.bg_editor_analysis_running = false;
                    self.pending_editor_analysis = None;
                    if self.editor_analysis_target().as_ref() == Some(&target) {
                        has_visual_changes = true;
                        let (chars, lines) = self.editor().map_or((0, 0), |editor| {
                            (editor.rope.len_chars(), editor.line_count())
                        });
                        self.cached_error_count = errors;
                        self.cached_warning_count = warnings;
                        self.cached_max_line_width = max_line_width;
                        self.cached_editor_word_count = word_count;
                        self.cached_word_count = Some(format!("{word_count}W {chars}C {lines}L"));
                        self.cached_editor_analysis_target = Some(target.clone());
                        self.cached_sticky_scopes = sticky_scopes;
                        self.cached_sticky_key = u64::MAX;
                        if let Some(editor) = self.editor_mut() {
                            editor.apply_unsaved_diff_cache(
                                target.content_version,
                                unsaved_diff,
                            );
                        }
                    } else if self.editor_analysis_target().is_some() {
                        // A newer edit landed while this coalesced scan was in
                        // flight. Arm its quiet-time deadline directly rather
                        // than forcing an otherwise unchanged frame merely to
                        // discover that another worker pass is needed.
                        self.editor_analysis_deferred = true;
                    }
                }
                crate::background::BackgroundResult::RichFeaturesTimedOut { target, feature } => {
                    if feature == "whole-file analysis" {
                        self.bg_editor_analysis_running = false;
                        self.pending_editor_analysis = None;
                    }
                    let matches = self.active_group == target.active_group
                        && self.active_tab == target.active_tab
                        && self.editor().is_some_and(|editor| {
                            editor.file_path == target.file_path
                                && editor.content_version == target.content_version
                                && editor.rope.len_bytes() == target.len_bytes
                        });
                    if matches
                        && self
                            .editor()
                            .is_some_and(|editor| !editor.rich_features_timed_out)
                    {
                        has_visual_changes = true;
                        let threshold_bytes =
                            self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                        if let Some(editor) = self.editor_mut() {
                            editor.mark_rich_features_timed_out(threshold_bytes);
                        }
                        if let Some(path) = target.file_path.as_ref() {
                            self.pending_lsp_syncs.remove(path);
                            self.pending_lsp_did_save.remove(path);
                            self.lsp_manager.did_close(path);
                        }
                        self.visible_highlight_key = None;
                        self.cached_editor_analysis_target = None;
                        self.cached_bracket_analysis_target = None;
                        self.cached_bracket_depth_index = None;
                        self.cached_cursor_match_target = None;
                        self.cached_max_line_width = self
                            .editor()
                            .map_or(0, crate::editor::Editor::current_line_len);
                        self.lsp_inlay_hints.clear();
                        self.lsp_code_lenses.clear();
                        self.lsp_semantic_tokens.clear();
                        if self.should_render_diff_for_active_editor() {
                            self.clear_diff_state();
                        }
                        self.notify(format!(
                            "Rich features paused: {feature} exceeded {} ms",
                            self.large_file_analysis_timeout_ms
                        ));
                    }
                }
                crate::background::BackgroundResult::BracketAnalysis {
                    target,
                    depth_index,
                    starting_depth,
                    guides,
                } => {
                    self.bg_bracket_analysis_running = false;
                    self.pending_bracket_analysis = None;
                    let current = self.editor_analysis_target().map(|editor| {
                        let (first_line, visible_lines) = self.editor().map_or((0, 50), |active| {
                            (active.scroll_offset, active.viewport_height.max(50))
                        });
                        crate::background::BracketAnalysisTarget {
                            editor,
                            navigation_id: self.editor().map_or(0, |editor| editor.navigation_id),
                            first_line,
                            visible_lines,
                        }
                    });
                    let source_matches = current.is_some()
                        && self
                            .editor()
                            .is_some_and(|editor| depth_index.matches(editor));
                    if source_matches {
                        let old_source_matches = self
                            .cached_bracket_depth_index
                            .as_ref()
                            .is_some_and(|index| {
                                self.editor().is_some_and(|editor| index.matches(editor))
                            });
                        has_visual_changes |= !old_source_matches
                            && (depth_index.has_brackets || !depth_index.enabled());
                        self.cached_bracket_depth_index = Some(depth_index);
                    }
                    if source_matches && current.as_ref() == Some(&target) {
                        let old_same_viewport = self
                            .cached_bracket_analysis_target
                            .as_ref()
                            .is_some_and(|cached| {
                                cached.first_line == target.first_line
                                    && cached.navigation_id == target.navigation_id
                                    && cached.editor.same_editor(&target.editor)
                            });
                        has_visual_changes |= if old_same_viewport {
                            self.cached_bracket_starting_depth != starting_depth
                                || self.cached_bracket_guides.as_slice() != guides
                        } else {
                            starting_depth != 0 || !guides.is_empty()
                        };
                        self.cached_bracket_analysis_target = Some(target);
                        self.cached_bracket_starting_depth = starting_depth;
                        self.cached_bracket_guides = guides.into();
                    } else if current.is_some() {
                        // This worker is deliberately coalesced to one job.
                        // Schedule the newest viewport on the next frame.
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::StructuralScopes { request, index } => {
                    self.pending_structural_scope = None;
                    self.structural_scope_cancel = None;
                    let enabled = self.editor().is_some_and(|editor| {
                        self.animation_settings.scope_enabled_for(editor)
                    });
                    let source_matches = enabled
                        && self
                            .editor()
                            .is_some_and(|editor| !editor.bg_loading && request.matches(editor));
                    if let Some(index) = index.filter(|index| {
                        source_matches && self.editor().is_some_and(|editor| index.matches(editor))
                    }) {
                        has_visual_changes |= index.has_scopes();
                        self.cached_structural_scope_index = Some(index);
                    } else if enabled
                        && self.editor().is_some_and(|editor| {
                            !editor.bg_loading
                                && !self
                                    .cached_structural_scope_index
                                    .as_ref()
                                    .is_some_and(|index| index.matches(editor))
                                && crate::animation_structural_scope::StructuralScopeRequest::from_editor(editor)
                                    .is_some()
                        })
                    {
                        // A superseded job releases the only in-flight slot.
                        // Wake the scheduler for the newest source snapshot.
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::CursorMatches {
                    target,
                    bracket,
                    tag,
                    } => {
                    self.bg_cursor_match_running = false;
                    self.pending_cursor_match = None;
                    let current = self.cursor_match_target();
                    if current.as_ref() == Some(&target) {
                        let old_same_anchor = self
                            .cached_cursor_match_target
                            .as_ref()
                            .is_some_and(|cached| cached.same_render_anchor(&target));
                        has_visual_changes |= if old_same_anchor {
                            self.cached_matching_bracket != bracket
                                || self.cached_matching_tag != tag
                        } else {
                            bracket.is_some() || tag.is_some()
                        };
                        self.cached_cursor_match_target = Some(target);
                        self.cached_matching_bracket = bracket;
                        self.cached_matching_tag = tag;
                    } else if current.is_some() && current != self.cached_cursor_match_target {
                        // Matching-pair scans are coalesced; a stale completion
                        // advances the latest request without polling, unless
                        // the UI has already resolved its cheap negative case.
                        has_visual_changes = true;
                    }
                }
                crate::background::BackgroundResult::ProblemsAnalysis {
                    target,
                    items,
                    error_count,
                    warning_count,
                    info_count,
                    file_count,
                } => {
                    self.bg_problems_analysis_running = false;
                    if self.problems_analysis_is_current(&target) {
                        has_visual_changes = true;
                        self.cached_problems_target = Some(target);
                        self.cached_problems = items;
                        self.cached_problems_error_count = error_count;
                        self.cached_problems_warning_count = warning_count;
                        self.cached_problems_info_count = info_count;
                        self.cached_problems_file_count = file_count;
                    }
                }
                crate::background::BackgroundResult::WordOccurrences { target, count } => {
                    let has_newer_request = self
                        .pending_word_occurrence
                        .as_ref()
                        .is_some_and(|(pending, _)| pending != &target);
                    if !has_newer_request {
                        self.pending_word_occurrence = None;
                        self.bg_word_occ_running = false;
                    }
                    if self.word_occurrence_target().as_ref() == Some(&target) {
                        let occurrence =
                            (count > 1).then(|| format!("{count} occurrences"));
                        has_visual_changes |= self.cached_word_occ != occurrence;
                        self.cached_word_occ = occurrence;
                        self.cached_word_occ_target = Some(target);
                    }
                }
                crate::background::BackgroundResult::VtComplete {
                    generation,
                    active_group,
                    active_tab,
                    document_id,
                    cursor_line,
                    cursor_col,
                    content_version,
                    source_rope,
                    source_identity,
                    language,
                    suggestions,
                    details,
                    work_outcome,
                    deferred_candidates,
                } => {
                    debug_assert!(
                        work_outcome != crate::app_vtcomplete::VtCompleteWorkOutcome::Exact
                            || deferred_candidates == 0
                    );
                    let matches_editor = generation == self.vtcomplete_generation && self.active_group == active_group
                        && self.active_tab == active_tab
                        && self.editor().is_some_and(|editor| {
                            crate::completion_source::Source::new(editor.file_path.as_deref(), editor.language_name()) == source_identity
                                && editor.navigation_id == document_id
                                && editor.cursor_line == cursor_line
                                && editor.cursor_col == cursor_col
                                && editor.content_version == content_version
                                && editor.rope.is_instance(&source_rope)
                                && editor.language_name() == language
                        });
                    if generation == self.vtcomplete_generation && matches_editor {
                        has_visual_changes = true;
                        let selected = self.vtcomplete_navigated.then(|| self
                            .vtcomplete_suggestions
                            .get(self.vtcomplete_selected)
                            .and_then(|selected| {
                                suggestions.iter().position(|word| word == selected)
                            })).flatten();
                        if selected.is_none() {
                            self.vtcomplete_navigated = false;
                        }
                        self.vtcomplete_active = !suggestions.is_empty();
                        self.vtcomplete_details = suggestions.iter().cloned().zip(details).collect();
                        self.vtcomplete_suggestions = suggestions;
                        self.vtcomplete_selected = selected.unwrap_or(0);
                        self.vtcomplete_cancel_token = None;
                    }
                }
                crate::background::BackgroundResult::LspSemanticTokens { target, tokens } => {
                    let is_latest = self
                        .pending_lsp_semantic_decode
                        .as_ref()
                        .is_some_and(|(pending, _)| pending == &target);
                    if is_latest {
                        self.pending_lsp_semantic_decode = None;
                    }
                    if is_latest && self.editor_content_target().as_ref() == Some(&target) {
                        if self.lsp_semantic_tokens != tokens {
                            self.lsp_semantic_tokens = tokens;
                            has_visual_changes = true;
                        }
                    }
                }
                crate::background::BackgroundResult::WrapLayout { target, offsets } => {
                    has_visual_changes |= self.apply_wrap_layout_result(target, offsets);
                }
                crate::background::BackgroundResult::SaveConflictDiff(result) => {
                    has_visual_changes |= self.apply_save_conflict_diff(result);
                }
                crate::background::BackgroundResult::MergeConflicts { view, index } => {
                    for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
                        if tab.editor.navigation_id == view {
                            tab.editor.merge_conflicts.pending = false;
                            if let Some(review) = tab.editor.merge_conflicts.review.as_mut()
                                && let Some(region) = index.region_for_line(tab.editor.cursor_line)
                                    .or_else(|| index.regions.first().copied())
                            {
                                review.source = index.source.clone();
                                review.region = region;
                                review.scroll = 0;
                            }
                            tab.editor.merge_conflicts.had_conflicts |= index.has_markers();
                            tab.editor.merge_conflicts.index = Some(index.clone());
                            has_visual_changes = true;
                        }
                    }
                }
                crate::background::BackgroundResult::MergeComparison(comparison) => {
                    self.apply_merge_comparison(comparison);
                    has_visual_changes = true;
                }
                crate::background::BackgroundResult::FileSaved {
                    source_document_id,
                    workspace_generation,
                    path,
                    error,
                    last_disk_modified,
                    file_size,
                    source_content_version,
                    source_rope,
                    previous_disk,
                    saved_encoding,
                    transformed_lines,
                    disk_written,
                    saved_rope,
                } => {
                    if workspace_generation != self.workspace_generation && crate::remote::workspace::is_view_key(&path) {
                        if self.pending_file_saves.get(&path).is_some_and(|(generation, _)| *generation == workspace_generation) { self.pending_file_saves.remove(&path); }
                        if self.deferred_file_saves.get(&path).is_some_and(|(generation, _)| *generation == workspace_generation) { self.deferred_file_saves.remove(&path); }
                        continue;
                    }
                    let save_options = self.pending_file_saves.remove(&path)
                        .filter(|(generation, _)| *generation == workspace_generation)
                        .and_then(|(_, intent)| intent.completed_options(source_document_id, source_content_version, &source_rope));
                    let completed_save_as = self.pending_save_as.get(&source_document_id)
                        .is_some_and(|(generation, target)| *generation == workspace_generation && target == &path);
                    if completed_save_as { self.pending_save_as.remove(&source_document_id); }
                    self.chat.rollback.note_disk_write_finished(&path);
                    if disk_written || error.is_some() || !crate::remote::workspace::is_view_key(&path) { self.queue_file_disk_checks(std::iter::once(path.clone())); }
                    let mut deferred = self.deferred_file_saves.remove(&path);
                    if error.is_none() && let Some(saved) = saved_rope.as_ref()
                        && let Some((_, next)) = deferred.as_mut()
                    {
                        let same_baseline = match (&previous_disk, &next.expected_disk) {
                            (Some(previous), Some(next)) => previous.same_snapshot(next),
                            (None, None) => source_document_id == next.source_document_id,
                            _ => false,
                        };
                        if same_baseline {
                            next.expected_disk = Some(crate::editor::DiskSaveBaseline {
                                rope: saved.clone(), encoding: saved_encoding,
                            });
                            next.fail_if_exists = false;
                        }
                    }
                    if let Some(err) = error {
                        // One failed exit save is a failure, never permission
                        // to requeue the same write on every event-loop turn.
                        if workspace_generation == self.workspace_generation && self.exit_save_and_quit {
                            self.exit_save_and_quit = false;
                            self.custom_exit_started = false;
                            self.exit_confirm_active = true;
                        }
                        if crate::editor::save::is_conflict(&err) || err.kind() == std::io::ErrorKind::AlreadyExists {
                            deferred = None;
                        }
                        self.show_file_save_error(&path, source_document_id, workspace_generation, err);
                    } else if workspace_generation == self.workspace_generation {
                        self.remote_change_warnings.remove(&path);
                        if completed_save_as { self.finish_save_as(source_document_id, &path); }
                        self.auto_save_attempted_versions.remove(&path);
                        self.notify("File saved");
                        self.status_message = String::from("File saved.");
                        let saved_rope = saved_rope.unwrap_or_default();
                        let large_file_threshold_bytes =
                            self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                        for tab in &mut self.tabs {
                            if tab.kind == TabKind::File
                                && tab.editor.file_path.as_deref() == Some(&path)
                                && (tab.editor.saved_rope.is_some() || tab.editor.save_identity == source_document_id)
                            {
                                tab.editor.apply_background_save(
                                    source_content_version,
                                    &source_rope,
                                    saved_rope.clone(),
                                    saved_encoding,
                                    last_disk_modified,
                                    file_size,
                                    transformed_lines,
                                    large_file_threshold_bytes,
                                );
                                tab.editor.set_remote_checkpoint(remote_checkpoint.clone());
                                tab.editor.record_save_options(save_options);
                            }
                        }
                        for group in &mut self.groups {
                            for tab in &mut group.tabs {
                                if tab.kind == TabKind::File
                                    && tab.editor.file_path.as_deref() == Some(&path)
                                    && (tab.editor.saved_rope.is_some() || tab.editor.save_identity == source_document_id)
                                {
                                    tab.editor.apply_background_save(
                                        source_content_version,
                                        &source_rope,
                                        saved_rope.clone(),
                                        saved_encoding,
                                        last_disk_modified,
                                        file_size,
                                        transformed_lines,
                                        large_file_threshold_bytes,
                                    );
                                    tab.editor.set_remote_checkpoint(remote_checkpoint.clone());
                                    tab.editor.record_save_options(save_options);
                                }
                            }
                        }
                        let lsp_snapshot = self
                            .tabs
                            .iter()
                            .chain(self.groups.iter().flat_map(|group| group.tabs.iter()))
                            .find(|tab| tab.editor.file_path.as_deref() == Some(path.as_path()))
                            .map(|tab| (tab.editor.content_version, tab.editor.rope.clone()));
                        if (disk_written || !crate::remote::workspace::is_view_key(&path)) && let Some((version, rope)) = lsp_snapshot {
                            let already_synced = self
                                .lsp_manager
                                .last_notified_version
                                .get(&path)
                                .is_some_and(|notified| *notified == version);
                            if already_synced || !self.lsp_manager.has_config_for_file(&path) {
                                self.lsp_manager.notify_did_save(&path);
                            } else {
                                self.pending_lsp_did_save.insert(path.clone());
                                self.queue_lsp_document_sync(path.clone(), version, rope);
                            }
                        }
                        if disk_written {
                            self.explorer_hover.invalidate(Some(&path));
                            self.explorer_hover_info_cache.remove(&path);
                            self.explorer_line_count_cache.remove(&path);
                            self.refresh_explorer_changed_paths(std::iter::once(path.as_path()), previous_disk.is_none());
                            // The native watcher publishes the same save shortly
                            // afterward. Refresh once from that batch, including
                            // edits made externally while the save was running.
                            let watched_save = self.workspace_watcher_active && self.remote_workspace.as_ref()
                                .is_some_and(|remote| remote.save_is_watched(&path, remote_checkpoint.as_deref()));
                            if !watched_save {
                                self.refresh_source_control_changed_paths([path.clone()]);
                            }
                            self.refresh_git_line_status_async();
                        }
                    }
                    if let Some((generation, next)) = deferred {
                        self.queue_background_save_job_for_workspace(next, generation);
                    }
                    if workspace_generation == self.workspace_generation && self.remote_changes_deferred.remove(&path) {
                        self.queue_remote_file_change(path);
                    }
                }
                crate::background::BackgroundResult::BlameHistory { navigation_id, generation, result } => {
                    has_visual_changes |= crate::git_blame::finish_history(self, navigation_id, generation, result);
                }
                crate::background::BackgroundResult::BlameSnapshot { navigation_id, generation, result } => {
                    has_visual_changes |= crate::git_blame::finish_snapshot(self, navigation_id, generation, result);
                }
                crate::background::BackgroundResult::BlameCache { navigation_id, file_path, document } => {
                    self.bg_blame_running = false;
                    let editors = self.tabs.iter().chain(
                        self.groups.iter().enumerate()
                            .filter(|(index, _)| *index != self.active_group)
                            .flat_map(|(_, group)| &group.tabs));
                    let current = editors.filter(|tab| matches!(tab.kind, TabKind::File | TabKind::GitRevision)).any(|tab| {
                        tab.editor.navigation_id == navigation_id && !tab.editor.bg_loading
                            && tab.editor.file_path.as_deref() == Some(file_path.as_path())
                            && tab.editor.rope.is_instance(&document.rope)
                            && tab.editor.blame.revision == document.revision
                    });
                    if current {
                        has_visual_changes = true;
                        self.blame_documents.insert(navigation_id, document.clone());
                        if self.editor().is_some_and(|editor| editor.navigation_id == navigation_id) {
                            self.blame_cache_key = document.key.clone();
                            self.blame_cache = Some((file_path, document));
                        }
                        let open_ids: rustc_hash::FxHashSet<_> = self.tabs.iter()
                            .chain(self.groups.iter().enumerate().filter(|(index, _)| *index != self.active_group)
                                .flat_map(|(_, group)| &group.tabs))
                            .map(|tab| tab.editor.navigation_id).collect();
                        self.blame_documents.retain(|id, _| open_ids.contains(id));
                    } else if self.editor().is_some_and(|editor| editor.navigation_id == navigation_id) {
                        has_visual_changes |= self.blame_cache.take().is_some();
                        self.blame_cache_key = None;
                        self.blame_requested_path = None;
                    }
                }
            }
        }
        // Show notification if any files were auto-reloaded
        file_reload_names.sort();
        file_reload_names.dedup();
        if !file_reload_names.is_empty() {
            let msg = if file_reload_names.len() == 1 {
                format!("↻ Reloaded {}", file_reload_names[0])
            } else {
                format!("↻ Reloaded {} files", file_reload_names.len())
            };
            self.notify(msg);
        }
        // Batch reap: only clean up finished join handles every 60 frames
        self.reap_frame_counter += 1;
        if self.reap_frame_counter >= 60 {
            self.reap_frame_counter = 0;
            self.background.reap_finished();
        }
        has_visual_changes | self.flush_lsp_document_syncs()
    }

    pub(crate) fn editor(&self) -> Option<&Editor> {
        self.tabs.get(self.active_tab).map(|t| &t.editor)
    }

    pub(crate) fn editor_mut(&mut self) -> Option<&mut Editor> {
        self.tabs.get_mut(self.active_tab).map(|t| {t.editor.refresh_document(); &mut t.editor})
    }

    /// Reload a clean open file without performing disk I/O or rope building
    /// on the terminal event loop. Repeated watcher events coalesce to at most
    /// one running and one follow-up read per path.
    pub(crate) fn queue_file_reload(&mut self, path: PathBuf) {
        if self.remote_connection.checking.contains(&path) {
            self.remote_changes_deferred.insert(path);
            return;
        }
        if self.pending_file_saves.contains_key(&path) {
            if crate::remote::workspace::is_view_key(&path) { self.remote_changes_deferred.insert(path.clone()); }
            if self.pending_file_reloads.contains(&path) {
                self.pending_file_reload_again.insert(path);
            }
            return; // FileSaved probes the disk again after the write finishes.
        }
        if self.pending_file_reloads.contains(&path) {
            self.pending_file_reload_again.insert(path);
            return;
        }

        let workspace_generation = self.workspace_generation;
        let mut baseline = None;
        let targets: Vec<_> = self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .filter_map(|tab| {
                if tab.editor.file_path.as_deref() != Some(path.as_path())
                    || tab.reloadable_file_path() != Some(path.as_path()) { return None; }
                if baseline.is_none() {
                    baseline = tab.editor.remote_checkpoint.as_ref().map(|checkpoint| crate::remote::workspace::Baseline {
                        checkpoint: checkpoint.clone(), rope: tab.editor.rope.clone(), encoding: tab.editor.encoding });
                }
                Some((tab.editor.navigation_id, tab.editor.content_version))
            }).collect();
        // Agent/watcher notifications can outlive a tab or arrive while the
        // user is editing. No eligible view means the read cannot be applied;
        // do not decode, hash and transfer a file just to discard its contents.
        if targets.is_empty() { return; }
        self.pending_file_reloads.insert(path.clone());
        let sender = self.background.sender.clone();
        let activity_path = path.clone();
        let remote = self.remote_workspace.clone().filter(|_| crate::remote::workspace::is_view_key(&path));
        self.background.read_for_file("file-reload", &activity_path, false, move || {
            let result = remote.as_ref().map_or_else(|| crate::editor::load_text_file(&path), |remote| remote.reload(&path, baseline.as_ref()));
            let missing = result.as_ref().err().is_some_and(|error| matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory));
            let _ = sender.send(crate::background::BackgroundResult::FileReloaded { workspace_generation, path: path.clone(), targets, result: result.map_err(|error| error.to_string()) });
            // Only absence needs a follow-up metadata check. Re-probing every
            // read error forms an endless check/reload loop for directories or
            // unreadable files replacing an open document.
            if missing {
                let change = remote.map_or_else(|| Some(crate::background::inspect_workspace_path(path.clone())), |remote|
                    remote.inspect(std::slice::from_ref(&path), &crate::background::CancellationToken::new()).ok().and_then(|mut states| states.pop()));
                if let Some(change) = change {
                    let _ = sender.send(crate::background::BackgroundResult::WorkspaceResult { generation: workspace_generation,
                        result: Box::new(crate::background::BackgroundResult::FileDiskState(change)) });
                }
            }
        });
    }

    pub(crate) fn refresh_pending_file_reload(&mut self, path: PathBuf) {
        if self.pending_file_reloads.contains(&path) { self.queue_file_reload(path); }
    }

    pub(crate) fn queue_all_clean_file_reloads(&mut self) {
        let paths: rustc_hash::FxHashSet<_> = self.tabs.iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .filter_map(|tab| tab.reloadable_file_path().map(Path::to_path_buf)).collect();
        for path in paths {
            self.queue_file_reload(path);
        }
    }

    /// Schedule metadata checks as one background batch. This is used on focus
    /// gain and by the periodic fallback so slow/network drives cannot stall a
    /// terminal frame.
    pub(crate) fn queue_file_disk_checks(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if !self.pending_disk_checks.insert(path.clone()) { continue; }
            if self.remote_workspace.is_some() && crate::remote::workspace::is_view_key(&path) {
                self.remote_disk_checks.queued.insert(path);
                continue;
            }
            let sender = self.background.sender.clone();
            let activity_path = path.clone();
            self.background.read_for_file("file-check", &activity_path, false, move || {
                let change = crate::background::inspect_workspace_path(path);
                let _ = sender.send(crate::background::BackgroundResult::FileDiskState(change));
            });
        }
        self.start_remote_disk_checks();
    }

    fn start_remote_disk_checks(&mut self) {
        if self.remote_disk_checks.running.is_some() || self.remote_disk_checks.queued.is_empty() { return; }
        let Some(remote) = self.remote_workspace.clone() else { return; };
        let paths: Vec<_> = self.remote_disk_checks.queued.drain().collect();
        let sender = self.background.sender.clone();
        let generation = self.workspace_generation;
        // One in-flight RPC worker; focus/watcher/fallback bursts accumulate as
        // unique paths, not waiting threads or requests. Small batches bound
        // response latency without issuing one round trip per open document.
        self.remote_disk_checks.running = Some(self.background.spawn_cancellable("remote-file-check", move |cancel| {
            let send = |result| sender.send(crate::background::BackgroundResult::WorkspaceResult {
                generation, result: Box::new(result),
            });
            for (index, batch) in paths.chunks(64).enumerate() {
                match remote.inspect(batch, &cancel) {
                    Ok(changes) => for change in changes {
                        if send(crate::background::BackgroundResult::FileDiskState(change)).is_err() { return; }
                    },
                    Err(_) => {
                        let _ = send(crate::background::BackgroundResult::RemoteDiskChecksFailed {
                            paths: paths[index * 64..].to_vec(),
                        });
                        break;
                    }
                }
            }
            let _ = send(crate::background::BackgroundResult::RemoteDiskChecksDone(cancel));
        }));
    }

    pub(crate) fn cancel_remote_disk_checks(&mut self) {
        self.remote_changes_deferred.clear();
        self.remote_change_warnings.clear();
        if let Some(cancel) = self.remote_disk_checks.running.take() { cancel.cancel(); }
        self.remote_disk_checks.queued.clear();
        self.pending_disk_checks.retain(|path| !crate::remote::workspace::is_view_key(path));
    }

    /// Keep an older worker snapshot from replacing a filesystem edit's tree.
    pub(crate) fn refresh_explorer_after_edit(&mut self, target: Option<&Path>) {
        self.explorer.invalidate_directory_cache();
        self.workspace_file_index_generation = self.workspace_file_index_generation.wrapping_add(1);
        self.explorer_refresh_generation = self.explorer_refresh_generation.wrapping_add(1);
        if self.explorer.find.open {
            self.clear_explorer_selection();
            self.explorer.find.select_first = true;
            self.refresh_explorer_tree_async();
            return;
        }
        if let Some(target) = target {
            self.explorer.select_path(target);
        } else {
            self.explorer.refresh();
        }
    }

    /// Rebuild the visible Explorer tree on a worker. Filesystem traversal and
    /// size/date metadata sorting never need to occupy the event loop.
    pub(crate) fn refresh_explorer_async(&mut self) {
        if let Some(remote) = &self.remote_workspace { remote.invalidate_directories(); }
        self.explorer.invalidate_directory_cache();
        self.workspace_file_index_generation = self.workspace_file_index_generation.wrapping_add(1);
        self.refresh_explorer_tree_async();
    }

    /// Reuse unaffected directory snapshots and preserve the Quick Open index
    /// when a watcher burst only changes file contents or metadata.
    pub(crate) fn refresh_explorer_changed_paths<'a>(
        &mut self, paths: impl IntoIterator<Item = &'a Path>, structural: bool,
    ) {
        if let Some(remote) = &self.remote_workspace { remote.invalidate_directories(); }
        let mut ignore_rules_changed = false;
        self.explorer.invalidate_directory_paths(paths.into_iter().inspect(|path| {
            ignore_rules_changed |= path.file_name().is_some_and(|name| name == ".gitignore");
        }), structural);
        if structural {
            self.workspace_file_index_generation = self.workspace_file_index_generation.wrapping_add(1);
        }
        if structural || (self.source_control_enabled && ignore_rules_changed) || matches!(self.explorer.sort_mode,
            crate::explorer::ExplorerSortMode::Size | crate::explorer::ExplorerSortMode::Modified)
        {
            self.refresh_explorer_tree_async();
        }
    }

    /// Visible-tree changes do not change workspace file eligibility. Reuse the
    /// Quick Open index across expansion, collapse, sort and automatic reveal.
    pub(crate) fn refresh_explorer_tree_async(&mut self) {
        if self.remote_workspace.as_ref().is_some_and(|remote| !remote.client.connected()) {
            self.remote_explorer_error = Some("Connection lost. Showing cached files.".into());
            self.explorer.find.loading = false;
            return;
        }
        self.explorer_refresh_generation = self.explorer_refresh_generation.wrapping_add(1);
        if self.bg_explorer_refresh_running {
            self.bg_explorer_refresh_pending = true;
            if let Some(cancel) = &self.explorer.find.cancel { cancel.cancel(); }
            return;
        }
        self.bg_explorer_refresh_running = true;
        self.explorer.find.cancel = Some(crate::background::CancellationToken::new());
        let generation = self.explorer_refresh_generation;
        self.explorer_refresh_active_generation = Some(generation);
        let request = self.explorer.refresh_request();
        if let Some(remote) = self.remote_workspace.clone() {
            let expanded = self.explorer.expanded_snapshot();
            let sort = self.explorer.sort_mode;
            let find = self.explorer.find.request();
            let source_control = self.source_control_enabled;
            let cancel = self.explorer.find.cancel.clone().unwrap_or_default();
            let sender = self.background.sender.clone();
            let workspace_generation = self.workspace_generation;
            self.background.read_for_file("remote-explorer", &self.explorer.root, true, move || {
                let result = match remote.explorer_tree(&expanded,sort,find,source_control,&cancel) {
                    Ok((entries, matches)) => crate::background::BackgroundResult::ExplorerRefreshed {
                        generation, canonical_root: remote.view_root, entries, matches,
                    },
                    Err(error) => crate::background::BackgroundResult::ExplorerRefreshFailed {
                        generation, error: error.to_string(),
                    },
                };
                let _ = sender.send(crate::background::BackgroundResult::WorkspaceResult {
                    generation: workspace_generation, result: Box::new(result),
                });
            });
            return;
        }
        let sender = self.background.sender.clone();
        self.background.spawn("explorer-refresh", move || {
            let (canonical_root, entries, matches) =
                crate::explorer::FileExplorer::build_refresh_entries(request);
            let _ = sender.send(crate::background::BackgroundResult::ExplorerRefreshed {
                generation,
                canonical_root,
                entries,
                matches,
            });
        });
    }

    pub(crate) fn toggle_or_open_explorer_async(&mut self) -> Option<PathBuf> {
        if self.remote_workspace.as_ref().is_some_and(|remote| !remote.client.connected())
            && !(self.explorer.find.filtering() && self.explorer.find.results.filtered_tree.is_some())
            && self.explorer.entries.get(self.explorer.selected).is_some_and(|entry| entry.is_dir && !entry.expanded) {
            self.status_message = "Remote folders cannot be loaded while disconnected. Existing files and local edits are retained.".into();
            return None;
        }
        if !self.explorer.find.filtering()
            && let Some(entry) = self.explorer.entries.get(self.explorer.selected).filter(|entry| entry.is_dir) {
            if entry.expanded { self.explorer_expanding.remove(&entry.path); }
            else { self.explorer_expanding.insert(entry.path.clone()); }
        }
        let (path, tree_changed) = self.explorer.toggle_or_open_deferred();
        if tree_changed {
            self.refresh_explorer_expansion();
        }
        path
    }

    pub(crate) fn collapse_explorer_entry_async(&mut self) {
        if let Some(entry) = self.explorer.entries.get(self.explorer.selected) {
            self.explorer_expanding.remove(&entry.path);
        }
        if self.explorer.collapse_current_deferred() {
            self.refresh_explorer_expansion();
        }
    }

    pub(crate) fn collapse_all_explorer_async(&mut self) {
        self.explorer_expanding.clear();
        if self.explorer.collapse_all_deferred() {
            self.refresh_explorer_expansion();
        }
    }

    fn refresh_explorer_expansion(&mut self) {
        let filtered = self.explorer.rebuild_filtered_entries();
        self.hovered_explorer = None;
        self.explorer_drop_target_idx = None;
        self.explorer_scrollbar_drag = None;
        self.clear_explorer_selection();
        self.set_explorer_single_selection(self.explorer.selected);
        if !filtered {
            self.refresh_explorer_tree_async();
        }
    }

    fn finish_explorer_refresh(&mut self, generation: u64) -> bool {
        if self.explorer_refresh_active_generation.is_some_and(|active| active != generation) {
            return false;
        }
        self.bg_explorer_refresh_running = false;
        self.explorer_refresh_active_generation = None;
        if std::mem::take(&mut self.bg_explorer_refresh_pending) {
            self.refresh_explorer_tree_async();
            return false;
        }
        generation == self.explorer_refresh_generation
    }

    pub(crate) fn editor_content_target(&self) -> Option<crate::background::EditorAnalysisTarget> {
        let editor = self.editor()?;
        if editor.bg_loading {
            return None;
        }
        Some(crate::background::EditorAnalysisTarget {
            active_group: self.active_group,
            active_tab: self.active_tab,
            file_path: editor.file_path.clone(),
            content_version: editor.content_version,
            len_bytes: editor.rope.len_bytes(),
            modified: editor.modified,
            disk_modified: editor.last_disk_modified,
        })
    }

    pub(crate) fn editor_analysis_target(&self) -> Option<crate::background::EditorAnalysisTarget> {
        if self.editor()?.large_file_mode {
            return None;
        }
        self.editor_content_target()
    }

    pub(crate) fn cursor_match_target(&self) -> Option<crate::background::CursorMatchTarget> {
        let editor = self.editor()?;
        Some(crate::background::CursorMatchTarget {
            editor: self.editor_analysis_target()?,
            cursor_line: editor.cursor_line,
            cursor_col: editor.cursor_col,
            language_name: editor.language_name().to_string(),
        })
    }

    pub(crate) fn queue_lsp_semantic_decode(
        &mut self,
        target: crate::background::EditorAnalysisTarget,
        data: Vec<lsp_types::SemanticToken>,
    ) {
        if let Some((_, token)) = self.pending_lsp_semantic_decode.take() {
            token.cancel();
        }
        let Some(rope) = self.editor().map(|editor| editor.rope.clone()) else { return; };
        let request_target = target.clone();
        let sender = self.background.sender.clone();
        let token = self.background.submit_cancellable(move |cancel| {
            let Some(mut tokens) = crate::lsp::decode_semantic_tokens_cancellable(&data, || {
                cancel.is_cancelled()
            }) else {
                return;
            };
            for (index, token) in tokens.iter_mut().enumerate() {
                if index % 1024 == 0 && cancel.is_cancelled() { return; }
                let start = crate::lsp_positions::from_utf16(&rope, token.line as usize, token.start_char);
                let end = crate::lsp_positions::from_utf16(&rope, token.line as usize, token.start_char.saturating_add(token.length));
                token.start_char = start as u32;
                token.length = end.saturating_sub(start) as u32;
            }
            if !cancel.is_cancelled() {
                let _ = sender.send(crate::background::BackgroundResult::LspSemanticTokens {
                    target: request_target,
                    tokens,
                });
            }
        });
        self.pending_lsp_semantic_decode = Some((target, token));
    }

    pub(crate) fn cancel_lsp_semantic_decode(&mut self) {
        if let Some((_, token)) = self.pending_lsp_semantic_decode.take() {
            token.cancel();
        }
        self.lsp_semantic_tokens_request_target = None;
    }

    pub(crate) fn word_occurrence_target(&self) -> Option<crate::background::WordOccurrenceTarget> {
        // Check demand before even looking up the cursor word. The occurrence
        // count is hidden by default, so most navigation needs no scan or timer.
        if self.search_active || !crate::ui_status::word_occurrences_needed(self) {
            return None;
        }
        // Large-file mode disables this analysis. Check that before finding
        // and copying a potentially document-sized word on the UI thread.
        let target = self.editor_analysis_target()?;
        let editor = self.editor()?;
        if editor.has_selection() {
            return None;
        }
        let word = crate::ui_editor::cached_cursor_word(
            editor, &mut self.editor_cursor_word_cache.borrow_mut(),
        )?;
        Some(crate::background::WordOccurrenceTarget {
            editor: target,
            word,
        })
    }

    /// Like `editor_mut()` but returns `None` and notifies if the file is read-only.
    pub(crate) fn editor_mut_writable(&mut self) -> Option<&mut Editor> {
        if self.diff_selection_owns_cursor() {
            return None;
        }
        let is_read_only = self.tabs.get_mut(self.active_tab).is_some_and(|tab| {
            tab.editor.refresh_document();
            tab.editor.read_only || tab.editor.save_content_error().is_some()
        });
        if is_read_only {
            self.notify("File is read-only");
            return None;
        }
        self.tabs.get_mut(self.active_tab).map(|t| &mut t.editor)
    }

    /// Returns true if a text input context currently has focus (editor file tab,
    /// palette, search bar, rename dialog, goto-line, terminal, etc.).
    /// Used to decide whether the cursor should blink.
    pub(crate) fn has_text_input_focus(&self) -> bool {
        if self.status_details.open { return false; }
        if self.chat.visible && self.active_pane == ActivePane::Chat { return true; }
        // Overlay text inputs always have focus when active
        if self.palette.active
            || self.search_active
            || self.global_search_active
            || self.goto_line_active
            || self.rename.active
            || self.rename.symbol_active
        {
            return true;
        }
        if self.docked_terminal_has_focus() {
            return true;
        }
        // Editor pane with a file or terminal tab
        if self.active_pane == ActivePane::Editor {
            if let Some(tab) = self.tabs.get(self.active_tab) {
                return matches!(tab.kind, TabKind::File | TabKind::Terminal);
            }
        }
        false
    }

    fn apply_git_line_status_to_open_editors(
        &mut self,
        file_path: &Path,
        status: &crate::editor::PreparedGitLineStatus,
    ) -> bool {
        let mut changed = false;
        let mut stale = false;
        for tab in &mut self.tabs {
            if tab.kind == TabKind::File && tab.editor.file_path.as_deref() == Some(file_path) {
                if tab.editor.rope.len_lines() == status.len() {
                    changed |= tab.editor.set_prepared_git_line_status(status.clone());
                } else {
                    stale = true;
                }
            }
        }
        for group in &mut self.groups {
            for tab in &mut group.tabs {
                if tab.kind == TabKind::File && tab.editor.file_path.as_deref() == Some(file_path) {
                    if tab.editor.rope.len_lines() == status.len() {
                        changed |= tab.editor.set_prepared_git_line_status(status.clone());
                    } else {
                        stale = true;
                    }
                }
            }
        }
        if stale {
            self.bg_git_diff_pending = true;
        }
        changed
    }

    /// Attach document handles and refresh immutable render snapshots. The
    /// document, not whichever desk happens to be visible, owns text/history.
    pub(crate) fn sync_duplicate_file_tabs(&mut self) {
        self.sync_document_views(true);
    }
    pub(crate) fn sync_document_views(&mut self, attach_matching_paths: bool) {
        let tab_count = self.tabs.len();
        let Some(source) = self
            .tabs
            .get_mut(self.active_tab)
            .filter(|t| t.kind == TabKind::File)
        else {
            return;
        };
        source.editor.refresh_document();
        source.editor.publish_document();
        if self
            .desks
            .sync_stamp
            .as_ref()
            .is_some_and(|stamp| stamp.unchanged(&source.editor, tab_count, self.groups.len()))
        {
            return;
        }
        let (before, tail) = self.tabs.split_at_mut(self.active_tab);
        let (source, after) = tail.split_first_mut().unwrap();
        let identity = source.editor.save_identity;
        let path = source.editor.file_path.as_deref();
        for tab in before
            .iter_mut()
            .chain(after)
            .chain(self.groups.iter_mut().flat_map(|g| &mut g.tabs))
        {
            if tab.kind == TabKind::File
                && (tab.editor.save_identity == identity
                    || (attach_matching_paths
                        && path.is_some_and(|p| tab.editor.file_path.as_deref() == Some(p))))
            {
                tab.editor.copy_document_from(&source.editor);
            }
        }
        self.desks.sync_stamp = Some(crate::app_desks::DocumentStamp::capture(
            &self.tabs[self.active_tab].editor,
            self.tabs.len(),
            self.groups.len(),
        ));
    }

    /// Execute rename symbol: replace all whole-word occurrences in current file
    pub(crate) fn execute_rename_symbol(&mut self) {
        let old = self.rename.symbol_old.clone();
        let new = self.rename.symbol_new.clone();
        if old.is_empty() || new.is_empty() || old == new {
            self.rename.symbol_active = false;
            return;
        }
        if self.editor_mut_writable().is_none() {
            self.rename.symbol_active = false;
            return;
        }
        // Try LSP rename first
        if let Some(ed) = self.editor() {
            if let Some(fp) = ed.file_path.clone() {
                let line = ed.cursor_line as u32;
                let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                if self.lsp_manager.request_rename(&fp, line, col, &new) {
                    self.lsp_rename_pending = true;
                    self.rename.symbol_active = false;
                    return;
                }
            }
        }
        // Fallback: text-based rename in current file
        if let Some(editor) = self.editor_mut_writable() {
            let mut edits = Vec::new();
            let old_chars = old.chars().count();
            let mut line_start = 0;
            for line in editor.rope.lines() {
                let text = line.to_string();
                let (mut byte, mut col) = (0, 0);
                for (pos, _) in text.match_indices(&old) {
                    // Search returns byte offsets, while Rope edits use chars.
                    // Advance once through each line even with many matches.
                    col += text[byte..pos].chars().count();
                    byte = pos;
                    let end = pos + old.len();
                    let at_start = pos == 0
                        || !text[..pos].ends_with(|c: char| c.is_alphanumeric() || c == '_');
                    let at_end = end >= text.len()
                        || !text[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
                    if at_start && at_end {
                        edits.push((line_start + col, line_start + col + old_chars, new.clone()));
                    }
                }
                line_start += line.len_chars();
            }
            match crate::app_lsp_edits::apply_prepared(editor, edits) {
                Ok(count) => self.notify(format!("Renamed '{old}' → '{new}' ({count} occurrences)")),
                Err(error) => self.notify_error(error),
            }
        }
        self.rename.symbol_active = false;
    }

    pub(crate) fn open_peek_definition(&mut self) {
        let Some(word) = self.editor().and_then(Editor::word_at_cursor) else {
            return;
        };

        // Search current file first, then other open tabs
        for tab in &self.tabs {
            if let Some((line, _col)) = tab.editor.find_definition(&word) {
                let rope = &tab.editor.rope;
                let total_lines = rope.len_lines();
                let start = line.saturating_sub(2);
                let end = (line + 15).min(total_lines);
                let mut lines = Vec::new();
                for i in start..end {
                    let s = rope.line(i).to_string();
                    lines.push(s.trim_end_matches(['\n', '\r']).to_owned());
                }
                let file_path = tab.editor.file_name().to_string();
                self.peek_definition = Some(PeekDefinition {
                    lines,
                    file_path,
                    start_line: start,
                });
                return;
            }
        }
        self.notify(format!("No definition found for '{word}'"));
    }

    fn keycode_matches_binding(binding_code: KeyCode, event_code: KeyCode) -> bool {
        match (binding_code, event_code) {
            (KeyCode::Char(a), KeyCode::Char(b))
                if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() =>
            {
                a.eq_ignore_ascii_case(&b)
            }
            _ => binding_code == event_code,
        }
    }

    /// Look up which command ID is bound to a given key event
    pub(crate) fn lookup_keybinding(
        &self,
        modifiers: KeyModifiers,
        code: KeyCode,
    ) -> Option<&'static str> {
        for kb in &self.keybindings {
            if kb.code == KeyCode::Null {
                continue;
            }
            if kb.modifiers == modifiers && Self::keycode_matches_binding(kb.code, code) {
                return Some(kb.id);
            }
        }

        // Accept shifted letter variants for non-shift letter shortcuts (e.g. Ctrl+Shift+W -> Ctrl+W).
        // Save As owns Ctrl+Shift+S unless the user explicitly rebound it.
        if modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT)
            && matches!(code, KeyCode::Char('s' | 'S')) { return None; }
        if modifiers.contains(KeyModifiers::SHIFT)
            && (matches!(code, KeyCode::Char(c) if c.is_ascii_alphabetic())
                || code == KeyCode::BackTab)
        {
            // Keep Ctrl+Shift+Z and Ctrl+Shift+V distinct so they aren't normalized.
            if modifiers.contains(KeyModifiers::CONTROL)
                && matches!(code, KeyCode::Char('z' | 'Z' | 'v' | 'V'))
            {
                return None;
            }
            let mut normalized_modifiers = modifiers;
            normalized_modifiers.remove(KeyModifiers::SHIFT);
            for kb in &self.keybindings {
                if kb.code == KeyCode::Null {
                    continue;
                }
                if kb.modifiers == normalized_modifiers
                    && Self::keycode_matches_binding(kb.code, code)
                {
                    return Some(kb.id);
                }
            }
        }

        None
    }

    pub(crate) fn clear_lsp_syncs(&mut self) {
        self.pending_lsp_syncs.clear();
        self.pending_lsp_did_save.clear();
        self.pending_lsp_on_type_format = None;
        self.cancel_lsp_semantic_decode();
    }

    pub(crate) fn clear_workspace_lsp_syncs(&mut self) {
        self.clear_lsp_syncs();
        self.pending_file_reloads.clear();
        self.pending_file_reload_again.clear();
        self.auto_save_attempted_versions.clear();
    }

    fn has_pending_file_load(
        &self,
        path: &Path,
        restores: &[crate::session::SessionRestore],
    ) -> bool {
        self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs)).any(|tab| {
            tab.editor.bg_loading
                && tab.editor.file_path.as_deref() == Some(path)
                && match &tab.editor.pending_session_restore {
                    Some(pending) => restores.iter().any(|restore| restore.id == pending.id),
                    None => restores.is_empty(),
                }
        })
    }

    /// Save current session to disk
    pub(crate) fn save_session(&self) {
        if let Err(error) = self.try_save_session() {
            log_error!("failed to save workspace session: {error}");
        }
    }

    pub(crate) fn try_save_session(&self) -> std::io::Result<()> {
        let session = self.build_session();
        if self
            .session_baseline
            .as_ref()
            .is_some_and(|baseline| baseline == &session)
        {
            return Ok(());
        }
        crate::session::save_session(&self.explorer.root, &session)
    }

    pub(crate) fn capture_session_baseline(&mut self) {
        self.session_baseline = Some(self.build_session());
    }
}

#[cfg(test)]
impl App {
    /// Create an App for testing with the current directory as root.
    #[inline(always)]
    pub(crate) fn test_new() -> Self {
        App::new(std::env::current_dir().expect("test requires current directory"))
    }

    #[track_caller]
    pub(crate) fn wait_for_background(
        &mut self,
        timeout: std::time::Duration,
        ready: impl Fn(&Self) -> bool,
    ) {
        let deadline = std::time::Instant::now() + timeout;
        let wake = self.background.wake_handle();
        loop {
            let observed = wake.generation();
            // A queued reply can start another worker. Check readiness after
            // applying replies, never followed by one more unobserved poll.
            self.poll_background_results();
            if ready(self) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background operation timed out (active jobs: {}, pending results: {}, status: {:?})",
                self.background.active_job_count(),
                self.background.has_pending_results(),
                self.status_message,
            );
            // Replies and the final worker completion wake us directly. Drain
            // a bounded result burst before sleeping, and observe before the
            // poll so a completion between the check and wait cannot be lost.
            if !self.background.has_pending_results() {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                // Reconnect retries use event-loop deadlines rather than a
                // sleeping worker. Exercise the same wake policy in tests.
                let wait = self.remote_connection.next_wait().map_or(remaining, |wait| remaining.min(wait));
                wake.wait_for_change(
                    observed,
                    Some(wait),
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "app_disk_check_tests.rs"]
mod disk_check_tests;

#[cfg(test)]
#[path = "app_file_reload_tests.rs"]
mod file_reload_tests;

#[path = "app_remote_file_events.rs"]
mod remote_file_events;

#[cfg(test)]
#[path = "app_remote_external_tests.rs"]
mod remote_external_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_events_preserve_filename_indexes_and_name_sorted_trees() {
        let mut app = App::test_new();
        app.bg_explorer_refresh_running = true;
        let path = app.explorer.root.join("changed.rs");
        let generation = app.workspace_file_index_generation;
        app.refresh_explorer_changed_paths([path.as_path()], false);
        assert!(!app.bg_explorer_refresh_pending);
        assert_eq!(app.workspace_file_index_generation, generation);
        app.explorer.sort_mode = crate::explorer::ExplorerSortMode::Modified;
        app.refresh_explorer_changed_paths([path.as_path()], false);
        assert!(app.bg_explorer_refresh_pending);
        assert_eq!(app.workspace_file_index_generation, generation);
        app.refresh_explorer_changed_paths([path.as_path()], true);
        assert_eq!(app.workspace_file_index_generation, generation + 1);
    }

    #[test]
    fn ignore_rule_edits_refresh_decorations_without_invalidating_filename_indexes() {
        let mut app = App::test_new();
        app.bg_explorer_refresh_running = true;
        let generation = app.workspace_file_index_generation;
        let path = app.explorer.root.join("nested/.gitignore");
        app.refresh_explorer_changed_paths([path.as_path()], false);
        assert!(app.bg_explorer_refresh_pending);
        assert_eq!(app.workspace_file_index_generation, generation);
    }

    #[test]
    fn git_metadata_events_refresh_ignored_decorations_once_per_burst() {
        for rescan in [false, true] {
            let mut app = App::test_new();
            app.bg_explorer_refresh_running = true;
            let generation = app.explorer_refresh_generation;
            app.background.sender.send(crate::background::BackgroundResult::FilesChanged {
                root: app.explorer.root.clone(), changes: Vec::new(),
                structure_changed: false, rescan, git_metadata_changed: true,
            }).unwrap();
            app.poll_background_results();
            assert!(app.bg_explorer_refresh_pending);
            assert_eq!(app.explorer_refresh_generation, generation + 1);
        }
    }

    #[test]
    fn git_metadata_and_structural_events_in_one_burst_invalidate_cached_ignore_results() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let repo = git2::Repository::init(root).unwrap();
        std::fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::write(root.join("output.tmp"), "ignored").unwrap();
        let mut app = App::test_new();
        app.explorer = crate::explorer::FileExplorer::new_deferred(root.into());
        app.explorer.set_directory_watcher_active(true);
        let (canonical, entries, matches) = crate::explorer::FileExplorer::build_refresh_entries(app.explorer.refresh_request());
        app.explorer.apply_refresh_entries(canonical, entries, matches);
        assert!(app.explorer.entries.iter().any(|entry| entry.name == "output.tmp"
            && entry.git_status == crate::explorer::GitStatus::Ignored));
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("output.tmp")).unwrap();
        index.write().unwrap();
        let path = root.join("new.rs");
        std::fs::write(&path, "new file").unwrap();
        app.bg_explorer_refresh_running = true;
        let generation = app.explorer_refresh_generation;
        app.background.sender.send(crate::background::BackgroundResult::FilesChanged {
            root: root.into(), changes: vec![crate::background::WorkspacePathChange {
                observed_at: std::time::Instant::now(), path, exists: true, modified: None, len: None,
            }], structure_changed: true, rescan: false, git_metadata_changed: true,
        }).unwrap();
        app.poll_background_results();
        assert_eq!(app.explorer_refresh_generation, generation + 1);
        let (canonical, entries, matches) = crate::explorer::FileExplorer::build_refresh_entries(app.explorer.refresh_request());
        app.explorer.apply_refresh_entries(canonical, entries, matches);
        assert!(app.explorer.entries.iter().any(|entry| entry.name == "output.tmp"
            && !entry.git_status_summary.contains(crate::explorer::GitStatus::Ignored)));
    }

    #[test]
    fn navigation_reuses_recent_git_status_without_queuing_duplicate_scans() {
        let mut app = App::test_new();
        app.source_control_last_refresh = Some(std::time::Instant::now());
        app.refresh_source_control_for_navigation();
        assert!(!app.bg_git_status_running);
        assert!(!app.bg_git_status_pending);
        app.bg_git_status_running = true;
        app.refresh_source_control_for_navigation();
        assert!(!app.bg_git_status_pending);
        app.refresh_source_control_async();
        assert!(app.bg_git_status_pending, "explicit invalidations bypass the navigation gate");
        app.bg_git_status_pending = false;
        app.bg_git_status_running = false;
        app.source_control_last_refresh = Some(std::time::Instant::now() - std::time::Duration::from_secs(3));
        app.refresh_source_control_for_navigation();
        assert!(app.bg_git_status_running);
    }

    #[test]
    fn tooltip_reuses_an_open_disk_snapshot_and_skips_large_unloaded_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.txt");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(crate::background::HOVER_LINE_COUNT_MAX_BYTES + 1).unwrap();
        let metadata = file.metadata().unwrap();
        let mut app = App::test_new();
        // Model an already loaded snapshot: no tooltip read of this large file
        // should be needed when its metadata still matches the editor.
        let mut editor = Editor::test_with_text("first\nsecond\n");
        editor.saved_rope = Some(editor.rope.clone());
        editor.file_path = Some(path.clone());
        editor.file_size = metadata.len();
        editor.last_disk_modified = metadata.modified().ok();
        app.tabs.push(Tab::new_file(editor));
        app.request_explorer_hover_info(path.clone(), false, true);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| app.explorer_hover_info_cache.contains_key(&path));
        assert_eq!(app.explorer_hover_info_cache[&path].line_count, Some(3));
        app.tabs.clear();
        app.explorer_hover_info_cache.clear();
        app.request_explorer_hover_info(path.clone(), false, true);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| app.explorer_hover_info_cache.contains_key(&path));
        assert_eq!(app.explorer_hover_info_cache[&path].line_count, None);
        assert_eq!(app.explorer_hover_info_cache[&path].size, metadata.len());
    }

    #[test]
    fn explorer_only_refreshes_preserve_quick_open_file_index_generation() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("folder")).unwrap();
        let mut app = App::new(workspace.path().to_path_buf());
        // Coalescing avoids worker startup; the request itself determines
        // whether workspace file eligibility changed.
        app.bg_explorer_refresh_running = true;
        let initial = app.workspace_file_index_generation;
        app.refresh_explorer_tree_async();
        app.explorer.selected = 0;
        app.toggle_or_open_explorer_async();
        app.collapse_explorer_entry_async();
        app.refresh_explorer_tree_async();
        assert_eq!(app.workspace_file_index_generation, initial);
        assert!(app.bg_explorer_refresh_pending);
        app.refresh_explorer_async();
        assert_eq!(app.workspace_file_index_generation, initial.wrapping_add(1));
        app.refresh_explorer_after_edit(None);
        assert_eq!(app.workspace_file_index_generation, initial.wrapping_add(2));
    }

    #[test]
    fn background_wait_includes_work_started_by_an_already_queued_result() {
        let workspace = tempfile::tempdir().unwrap();
        let mut app = App::new(workspace.path().to_path_buf());
        app.bg_git_branch_pending = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::GitBranch(
                "initial result".into(),
            ))
            .unwrap();
        // Applying the queued result starts a coalesced branch refresh even
        // though the running flag is false before the first poll.
        assert!(!app.bg_git_branch_running);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_git_branch_running
        });
        assert!(!app.bg_git_branch_pending);
        assert_ne!(app.cached_git_branch, "initial result");
    }

    #[test]
    fn explorer_new_file_survives_a_refresh_started_before_creation() {
        let workspace = tempfile::tempdir().expect("workspace");
        let folder = workspace.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let mut app = App::new(workspace.path().to_path_buf());
        let generation = app.explorer_refresh_generation;
        let (canonical_root, entries, matches) = crate::explorer::FileExplorer::build_refresh_entries(
            app.explorer.refresh_request(),
        );
        app.bg_explorer_refresh_running = true;
        app.explorer_context_target = Some(folder.clone());
        let action = app
            .explorer_context_menu_items()
            .iter()
            .position(|item| *item == "New File")
            .unwrap();
        app.execute_explorer_context(action);
        let placeholder = folder.join("new file");

        app.background
            .sender
            .send(crate::background::BackgroundResult::ExplorerRefreshed {
                generation,
                canonical_root,
                entries,
                matches,
            })
            .unwrap();
        app.poll_background_results();

        assert!(app.rename.active);
        assert_eq!(app.explorer.entries[app.explorer.selected].path, placeholder);
        assert!(app.explorer.entries.iter().any(|entry| entry.path == folder && entry.expanded));
    }

    fn unique_temp_app_root() -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        path.push(format!("vtcode-app-test-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create test app root");
        path
    }

    fn sample_global_search_result(id: usize) -> GlobalSearchResult {
        GlobalSearchResult {
            file_path: std::path::PathBuf::from(format!("file-{id}.txt")),
            line_num: id + 1,
            line_text: format!("match {id}").into(),
            col_start: 0,
            col_end: 5,
            context_before: None,
            context_after: None,
        }
    }

    #[test]
    fn clamp_to_char_boundary_past_end() {
        assert_eq!(App::clamp_to_char_boundary("hello", 100), 5);
    }

    #[test]
    fn clamp_to_char_boundary_empty() {
        assert_eq!(App::clamp_to_char_boundary("", 0), 0);
    }

    #[test]
    fn clamp_to_char_boundary_multibyte() {
        let text = "café";
        // 'é' is 2 bytes; caf = 3 bytes, é starts at 3
        let mid_char = 4; // inside 'é' (byte 4 of 5)
        let clamped = App::clamp_to_char_boundary(text, mid_char);
        assert!(text.is_char_boundary(clamped));
    }

    #[test]
    fn byte_index_for_char_col_multibyte() {
        let text = "café";
        // c=0, a=1, f=2, é=3 (char col 3 → byte index 3)
        let idx = App::byte_index_for_char_col(text, 0, text.len(), 3);
        assert_eq!(idx, 3);
    }

    #[test]
    fn normalize_inline_cursor_basic() {
        let text = "hello";
        let mut cursor = 3;
        let mut anchor = None;
        App::normalize_inline_cursor(text, &mut cursor, &mut anchor);
        assert_eq!(cursor, 3);
        assert_eq!(anchor, None);
    }

    #[test]
    fn normalize_inline_cursor_clears_equal_anchor() {
        let text = "hello";
        let mut cursor = 3;
        let mut anchor = Some(3);
        App::normalize_inline_cursor(text, &mut cursor, &mut anchor);
        assert_eq!(anchor, None);
    }

    #[test]
    pub(crate) fn inline_selection_range_forward() {
        assert_eq!(App::inline_selection_range(5, Some(2)), Some((2, 5)));
    }

    #[test]
    pub(crate) fn inline_selection_range_reversed() {
        assert_eq!(App::inline_selection_range(2, Some(5)), Some((2, 5)));
    }

    #[test]
    fn file_load_failed_result_shows_error_and_clears_loading() {
        let mut app = App::test_new();
        let path = PathBuf::from("locked-file.txt");
        let editor = Editor::new_loading(&path, crate::editor::BG_LOAD_THRESHOLD + 1);
        app.tabs.push(Tab::new_file(editor));

        app.background
            .sender
            .send(crate::background::BackgroundResult::FileLoadFailed {
                workspace_generation: 0,
                path: path.clone(),
                error: "permission denied".to_string(),
                session_restores: Vec::new(),
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert_eq!(app.status_message, "Error: permission denied");
        assert!(!app.tabs[0].editor.bg_loading);
        assert!(app.tabs[0].editor.disk_unavailable);
        assert!(app.tabs[0].editor.load_failed);
        assert!(app.tabs[0].editor.background_save_request(false, false, false).is_none());
        assert!(app.tabs[0]
            .editor
            .rope
            .to_string()
            .contains("Error loading file"));
    }

    #[test]
    fn breadcrumb_results_ignore_superseded_directory_reads() {
        let mut app = App::test_new();
        app.breadcrumb_dropdown_generation = 2;
        app.breadcrumb_dropdown_loading = true;

        app.background
            .sender
            .send(crate::background::BackgroundResult::BreadcrumbEntries {
                generation: 1,
                segment: 1,
                entries: vec![(PathBuf::from("stale"), false)],
            })
            .unwrap();
        assert!(!app.poll_background_results());
        assert!(app.breadcrumb_dropdown_items.is_empty());
        assert!(app.breadcrumb_dropdown_loading);

        app.background
            .sender
            .send(crate::background::BackgroundResult::BreadcrumbEntries {
                generation: 2,
                segment: 3,
                entries: vec![(PathBuf::from("current"), true)],
            })
            .unwrap();
        assert!(app.poll_background_results());
        assert_eq!(app.breadcrumb_dropdown_segment, 3);
        assert_eq!(
            app.breadcrumb_dropdown_items,
            vec![(PathBuf::from("current"), true)]
        );
        assert!(!app.breadcrumb_dropdown_loading);
    }

    #[test]
    fn failed_load_is_not_reported_as_an_external_deletion() {
        let mut app = App::test_new();
        let path = PathBuf::from("missing-load-target.txt");
        app.tabs.push(Tab::new_file(Editor::new_loading(&path, 0)));

        app.background
            .sender
            .send(crate::background::BackgroundResult::FileLoadFailed {
                workspace_generation: 0,
                path: path.clone(),
                error: "path not found".to_string(),
                session_restores: Vec::new(),
            })
            .unwrap();
        app.background
            .sender
            .send(crate::background::BackgroundResult::FileDiskState(
                crate::background::WorkspacePathChange {
                    observed_at: std::time::Instant::now(),
                    path,
                    exists: false,
                    modified: None,
                    len: None,
                },
            ))
            .unwrap();

        assert!(app.poll_background_results());
        assert!(app
            .notification_history
            .iter()
            .all(|(message, _, _)| !message.contains("deleted externally")));
    }

    #[test]
    fn repeated_missing_disk_states_report_one_deletion_transition() {
        let mut app = App::test_new();
        let path = PathBuf::from("deleted-after-open.txt");
        let mut editor = Editor::test_with_text("loaded\n");
        editor.file_path = Some(path.clone());
        editor.last_disk_modified = Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1));
        app.tabs.push(Tab::new_file(editor));

        for _ in 0..2 {
            app.background
                .sender
                .send(crate::background::BackgroundResult::FileDiskState(
                    crate::background::WorkspacePathChange {
                        observed_at: std::time::Instant::now(),
                        path: path.clone(),
                        exists: false,
                        modified: None,
                        len: None,
                    },
                ))
                .unwrap();
        }

        assert!(app.poll_background_results());
        assert_eq!(
            app.notification_history
                .iter()
                .filter(|(message, _, _)| message.contains("deleted externally"))
                .count(),
            1
        );
        assert!(app.tabs.is_empty());
    }

    #[test]
    fn unchanged_disk_probe_does_not_request_a_redraw() {
        let mut app = App::test_new();
        let path = PathBuf::from("unchanged.txt");
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(7);
        let mut editor = Editor::test_with_text("stable\n");
        editor.file_path = Some(path.clone());
        editor.file_size = 7;
        editor.last_disk_modified = Some(modified);
        app.tabs.push(Tab::new_file(editor));
        app.explorer_line_count_cache.insert(path.clone(), 1);

        app.background
            .sender
            .send(crate::background::BackgroundResult::FileDiskState(
                crate::background::WorkspacePathChange {
                    observed_at: std::time::Instant::now(),
                    path: path.clone(),
                    exists: true,
                    modified: Some(modified),
                    len: Some(7),
                },
            ))
            .unwrap();

        assert!(!app.poll_background_results());
        assert_eq!(app.explorer_line_count_cache.get(&path), Some(&1));
    }

    #[test]
    fn unchanged_git_branch_probe_does_not_request_a_redraw() {
        let mut app = App::test_new();
        app.cached_git_branch = "main".to_string();
        app.bg_git_branch_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::GitBranch(
                "main".to_string(),
            ))
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(!app.bg_git_branch_running);

        app.background
            .sender
            .send(crate::background::BackgroundResult::GitBranch(
                "release".to_string(),
            ))
            .unwrap();
        assert!(app.poll_background_results());
        assert_eq!(app.cached_git_branch, "release");
    }

    #[test]
    fn unchanged_extension_scan_does_not_request_a_redraw() {
        let mut app = App::test_new();
        app.bg_extension_scan_running = true;
        app.extension_scan_generation = 7;
        app.background
            .sender
            .send(crate::background::BackgroundResult::ExtensionsDiscovered {
                generation: 7,
                discovered: Vec::new(),
                notices: Vec::new(),
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(!app.bg_extension_scan_running);
    }

    #[test]
    fn delayed_external_theme_is_installed_and_requested_setting_is_reapplied() {
        let mut app = App::test_new();
        let mut external = crate::app_theme::load_builtin_themes()
            .into_iter()
            .next()
            .expect("embedded theme");
        external.name = "Deferred Test Theme".to_string();
        external.config_key = "deferred-test-theme".to_string();
        external.syntax_theme = "deferred-test-theme".to_string();
        app.settings_effective.theme = Some(external.config_key.clone());
        app.bg_external_theme_scan_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::ExternalThemesLoaded(
                vec![external],
            ))
            .unwrap();

        assert!(app.poll_background_results());
        assert!(!app.bg_external_theme_scan_running);
        assert_eq!(app.theme_key, "deferred-test-theme");
        assert!(app
            .loaded_themes
            .iter()
            .any(|theme| theme.config_key == "deferred-test-theme"));
    }

    #[test]
    fn delayed_unselected_theme_does_not_manufacture_a_frame() {
        let mut app = App::test_new();
        let mut external = crate::app_theme::load_builtin_themes()
            .into_iter()
            .next()
            .expect("embedded theme");
        external.name = "Deferred Unselected Theme".to_string();
        external.config_key = "deferred-unselected-theme".to_string();
        external.syntax_theme = "deferred-unselected-theme".to_string();
        app.bg_external_theme_scan_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::ExternalThemesLoaded(
                vec![external],
            ))
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(!app.bg_external_theme_scan_running);
        assert_eq!(app.theme_key, "dark");
    }

    #[test]
    fn current_syntax_cache_completion_clears_the_loading_indicator() {
        let mut app = App::test_new();
        app.bg_syntax_asset_refresh_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::SyntaxAssetsLoaded {
                root: app.explorer.root.clone(), result: Ok(None),
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert!(!app.bg_syntax_asset_refresh_running);
    }

    #[test]
    fn stale_extension_scan_is_ignored_after_workspace_generation_changes() {
        let mut app = App::test_new();
        app.bg_extension_scan_running = true;
        app.extension_scan_generation = 8;
        app.background
            .sender
            .send(crate::background::BackgroundResult::ExtensionsDiscovered {
                generation: 7,
                discovered: Vec::new(),
                notices: vec!["stale scan must not be shown".to_string()],
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(!app
            .notification_history
            .iter()
            .any(|(message, _, _)| message.contains("stale scan")));
    }

    #[test]
    fn unchanged_git_line_diff_does_not_request_a_redraw() {
        let mut app = App::test_new();
        let path = PathBuf::from("stable-diff.txt");
        let mut editor = Editor::test_with_text("stable\n");
        editor.file_path = Some(path.clone());
        let line_count = editor.rope.len_lines();
        editor.set_git_line_status(vec![' '; line_count]);
        app.tabs.push(Tab::new_file(editor));
        app.bg_git_diff_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::GitLineDiff {
                file_path: path.clone(),
                status: crate::editor::PreparedGitLineStatus::from_diffs(
                    line_count,
                    &[],
                    &[],
                ),
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(!app.bg_git_diff_running);

        app.background
            .sender
            .send(crate::background::BackgroundResult::GitLineDiff {
                file_path: path,
                status: crate::editor::PreparedGitLineStatus::from_diffs(
                    line_count,
                    &[],
                    &['M'],
                ),
            })
            .unwrap();
        assert!(app.poll_background_results());
    }

    #[test]
    fn offscreen_highlight_checkpoint_progress_does_not_request_a_redraw() {
        let mut app = App::test_new();
        let path = PathBuf::from("distant-highlight.rs");
        let rope = ropey::Rope::from_str(&"let value = 1;\n".repeat(20_000));
        let mut editor = Editor::test_with_text("");
        editor.rope = rope.clone();
        editor.file_path = Some(path.clone());
        editor.scroll_offset = 15_000;
        editor.viewport_height = 50;
        app.tabs.push(Tab::new_file(editor));

        let theme = app.syntax_theme.clone();
        let job = app
            .highlighter
            .prepare_job(&rope, &path, 0, None, 15_000..15_075, None, &theme)
            .expect("distant viewport needs authoritative parsing");
        let (syntaxes, themes) = app.highlighter.shared_sets();
        let batch = job.run(&rope, &syntaxes, &themes, || false);
        assert!(!batch.has_render_entries());
        app.background
            .sender
            .send(crate::background::BackgroundResult::HighlightBatch(batch))
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(app.visible_highlight_key.is_some());
    }

    #[test]
    fn file_loaded_result_replaces_placeholder_and_refreshes_status() {
        let mut app = App::test_new();
        let path = PathBuf::from("large-file.txt");
        let editor = Editor::new_loading(&path, crate::editor::BG_LOAD_THRESHOLD + 1);
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(123);

        app.background
            .sender
            .send(crate::background::BackgroundResult::FileLoaded {
                workspace_generation: 0,
                path: path.clone(),
                rope: ropey::Rope::from_str("loaded\ncontent\n"),
                encoding: "UTF-8",
                file_size: 15,
                last_disk_modified: Some(modified),
                session_restores: Vec::new(),
                skip_open_prompt: false,
            })
            .unwrap();

        assert!(app.poll_background_results());
        let editor = &app.tabs[0].editor;
        assert!(!editor.bg_loading);
        assert_eq!(editor.rope.to_string(), "loaded\ncontent\n");
        assert_eq!(editor.last_disk_modified, Some(modified));
        assert_eq!(app.status_message, "Opened: large-file.txt");
    }

    #[test]
    fn file_loaded_result_updates_placeholder_in_inactive_group() {
        let mut app = App::test_new();
        let path = PathBuf::from("split-large-file.txt");
        let mut group = EditorGroup::new();
        group.tabs.push(Tab::new_file(Editor::new_loading(
            &path,
            crate::editor::BG_LOAD_THRESHOLD + 1,
        )));
        app.groups.push(group);

        app.background
            .sender
            .send(crate::background::BackgroundResult::FileLoaded {
                workspace_generation: 0,
                path: path.clone(),
                rope: ropey::Rope::from_str("loaded in split\n"),
                encoding: "UTF-8",
                file_size: 16,
                last_disk_modified: None,
                session_restores: Vec::new(),
                skip_open_prompt: false,
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert!(!app.groups[0].tabs[0].editor.bg_loading);
        assert_eq!(
            app.groups[0].tabs[0].editor.rope.to_string(),
            "loaded in split\n"
        );
        assert_eq!(app.status_message, "Opened: split-large-file.txt");
    }

    #[test]
    fn prepared_git_compare_result_enters_diff_without_ui_side_rebuild() {
        let mut app = App::test_new();
        let path = PathBuf::from("async-diff.txt");
        let mut editor = Editor::test_with_text("after\n");
        editor.file_path = Some(path.clone());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.git_compare_generation = 7;
        app.pending_git_compare_path = Some(path.clone());
        app.bg_git_compare_running = true;

        let diff_lines = compute_file_diff("before\n", "after\n");
        let render_cache = DiffRenderCache::new(&diff_lines);
        let markers = crate::app_git_display::target_line_markers_from_compare_diff(
            &diff_lines,
            app.tabs[0].editor.rope.len_lines(),
        );
        let target = crate::background::GitCompareTarget {
            generation: 7,
            file_path: path.clone(),
            content_version: 0,
            len_bytes: app.tabs[0].editor.rope.len_bytes(),
            line_count: app.tabs[0].editor.rope.len_lines(),
            last_disk_modified: None,
        };
        app.background
            .sender
            .send(crate::background::BackgroundResult::GitCompare {
                target,
                result: Ok(crate::background::PreparedGitCompare {
                    source_revisions: None,
                    target_snapshot: None,
                    is_new_file: false,
                    base_content: "before\n".to_string().into(),
                    file_name: "async-diff.txt".to_string(),
                    diff_lines: diff_lines.clone(),
                    render_cache,
                    target_line_markers: markers,
                }),
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert!(matches!(app.diff_mode, DiffMode::SideBySide));
        assert_eq!(app.compare_base_content.as_deref(), Some("before\n"));
        assert_eq!(app.compare_diff_lines, diff_lines);
        assert_eq!(app.diff_target_path.as_ref(), Some(&path));
        assert!(app.pending_git_compare_path.is_none());
        assert!(!app.bg_git_compare_running);
        assert_eq!(app.status_message, "Opened diff: async-diff.txt");
    }

    #[test]
    fn large_file_open_returns_placeholder_then_completes_in_background() {
        let root = unique_temp_app_root();
        let path = root.join("background-load.txt");
        let mut content = vec![b'x'; crate::editor::BG_LOAD_THRESHOLD as usize + 64];
        content.push(b'\n');
        std::fs::write(&path, &content).expect("write large test file");

        let mut app = App::new(root.clone());
        app.open_file(&path);

        assert!(app.tabs[0].editor.bg_loading);
        assert!(app.tabs[0]
            .editor
            .rope
            .to_string()
            .starts_with("Loading file"));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.tabs[0].editor.bg_loading && std::time::Instant::now() < deadline {
            app.poll_background_results();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        assert!(!app.tabs[0].editor.bg_loading);
        assert_eq!(app.tabs[0].editor.rope.len_bytes(), content.len());
        assert_eq!(app.status_message, format!("Opened: {}", path.display()));

        drop(app);
        std::fs::remove_dir_all(root).expect("remove test app root");
    }

    #[test]
    fn matching_editor_analysis_result_updates_exact_caches() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.rope = ropey::Rope::from_str("changed\n");
        editor.saved_rope = Some(ropey::Rope::from_str("before\n"));
        editor.modified = true;
        editor.content_version = 7;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;

        let target = app.editor_analysis_target().expect("analysis target");
        app.bg_editor_analysis_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::EditorAnalysis {
                target: target.clone(),
                errors: 2,
                warnings: 3,
                max_line_width: 7,
                word_count: 1,
                unsaved_diff: crate::editor::UnsavedDiff { line_count: 2, changed_ranges: vec![0..1] },
                sticky_scopes: Default::default(),
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert!(!app.bg_editor_analysis_running);
        assert_eq!(app.cached_editor_analysis_target, Some(target));
        assert_eq!(app.cached_error_count, 2);
        assert_eq!(app.cached_warning_count, 3);
        assert_eq!(app.cached_max_line_width, 7);
        assert_eq!(app.cached_word_count.as_deref(), Some("1W 8C 2L"));
        assert_eq!(app.editor().unwrap().line_diff_status(0), 'N');
    }

    #[test]
    fn large_file_keeps_content_operations_but_skips_rich_analysis() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("searchable\n");
        editor.large_file_mode = true;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;

        assert!(app.editor_content_target().is_some());
        assert!(app.editor_analysis_target().is_none());
    }

    #[test]
    fn rich_feature_timeout_pauses_optional_features_without_closing_file() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("still editable\n");
        editor.file_path = Some(PathBuf::from("slow.txt"));
        editor.content_version = 9;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        let target = app.editor_content_target().expect("content target");

        app.background
            .sender
            .send(crate::background::BackgroundResult::RichFeaturesTimedOut {
                target,
                feature: "test analysis",
            })
            .expect("send timeout");

        assert!(app.poll_background_results());
        let editor = app.editor().expect("editor remains open");
        assert!(editor.large_file_mode);
        assert!(editor.rich_features_timed_out);
        assert_eq!(editor.rope.to_string(), "still editable\n");
    }

    #[test]
    fn stale_editor_analysis_result_is_discarded() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.rope = ropey::Rope::from_str("one\n");
        editor.content_version = 1;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;

        let stale_target = app.editor_analysis_target().expect("analysis target");
        app.tabs[0].editor.content_version = 2;
        app.bg_editor_analysis_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::EditorAnalysis {
                target: stale_target,
                errors: 99,
                warnings: 99,
                max_line_width: 99,
                word_count: 99,
                unsaved_diff: crate::editor::UnsavedDiff { line_count: 1, changed_ranges: vec![0..1] },
                sticky_scopes: Default::default(),
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(!app.bg_editor_analysis_running);
        assert!(app.editor_analysis_deferred);
        assert!(app.cached_editor_analysis_target.is_none());
        assert_eq!(app.cached_error_count, 0);
    }

    #[test]
    fn matching_word_occurrence_result_updates_status_cache() {
        let mut app = App::test_new();
        app.status_bar_pins.insert("word_occ".into(), true);
        let mut editor = Editor::new();
        editor.rope = ropey::Rope::from_str("needle needle\n");
        editor.cursor_line = 0;
        editor.cursor_col = 1;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;

        let target = app.word_occurrence_target().expect("word target");
        app.bg_word_occ_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::WordOccurrences {
                target: target.clone(),
                count: 2,
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert!(!app.bg_word_occ_running);
        assert_eq!(app.cached_word_occ_target, Some(target));
        assert_eq!(app.cached_word_occ.as_deref(), Some("2 occurrences"));
    }

    #[test]
    fn empty_cursor_analysis_results_do_not_manufacture_frames() {
        let mut app = App::test_new();
        app.status_bar_pins.insert("word_occ".into(), true);
        let mut editor = Editor::test_with_text("ordinary text\n");
        editor.cursor_col = 3;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;

        let cursor_target = app.cursor_match_target().expect("cursor target");
        app.bg_cursor_match_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::CursorMatches {
                target: cursor_target.clone(),
                bracket: None,
                tag: None,
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert_eq!(app.cached_cursor_match_target, Some(cursor_target));

        let word_target = app.word_occurrence_target().expect("word target");
        app.bg_word_occ_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::WordOccurrences {
                target: word_target.clone(),
                count: 1,
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert_eq!(app.cached_word_occ_target, Some(word_target));
        assert!(app.cached_word_occ.is_none());
    }

    #[test]
    fn empty_bracket_guide_result_does_not_manufacture_a_frame() {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text("plain\n"))];
        app.active_tab = 0;
        let editor = app.editor_analysis_target().expect("editor target");
        let source = app.editor().unwrap();
        let depth_index = std::sync::Arc::new(crate::background::BracketDepthIndex::build(
            source.rope.clone(),
            source.navigation_id,
            source.content_version,
            source.wrap_revision,
        ));
        let target = crate::background::BracketAnalysisTarget {
            editor,
            navigation_id: source.navigation_id,
            first_line: 0,
            visible_lines: 50,
        };
        app.bg_bracket_analysis_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::BracketAnalysis {
                target: target.clone(),
                depth_index,
                starting_depth: 0,
                guides: Vec::new(),
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert_eq!(app.cached_bracket_analysis_target, Some(target));
        assert!(!app.bg_bracket_analysis_running);
    }

    #[test]
    fn bracket_document_index_accepts_a_result_after_scrolling_but_rejects_replacement() {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(&format!(
            "{{\n{}}}\n",
            "plain\n".repeat(400)
        )))];
        app.active_tab = 0;
        let source = app.editor().unwrap();
        let index = std::sync::Arc::new(crate::background::BracketDepthIndex::build(
            source.rope.clone(),
            source.navigation_id,
            source.content_version,
            source.wrap_revision,
        ));
        let target = crate::background::BracketAnalysisTarget {
            editor: app.editor_analysis_target().unwrap(),
            navigation_id: source.navigation_id,
            first_line: 0,
            visible_lines: 50,
        };
        app.editor_mut().unwrap().scroll_offset = 250;
        app.bg_bracket_analysis_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::BracketAnalysis {
                target: target.clone(),
                depth_index: index.clone(),
                starting_depth: 0,
                guides: vec![(0, 1, 401, 1)],
            })
            .unwrap();
        assert!(app.poll_background_results());
        assert!(std::sync::Arc::ptr_eq(
            app.cached_bracket_depth_index.as_ref().unwrap(),
            &index
        ));
        assert!(app.cached_bracket_analysis_target.is_none());
        assert!(app.cached_bracket_guides.is_empty());

        // Neither equal versions/lengths nor a reused tab location can make an
        // old snapshot valid for an independently replaced Rope or editor.
        app.cached_bracket_depth_index = None;
        app.editor_mut().unwrap().rope =
            ropey::Rope::from_str(&format!("[\n{}]\n", "plain\n".repeat(400)));
        app.background
            .sender
            .send(crate::background::BackgroundResult::BracketAnalysis {
                target: target.clone(),
                depth_index: index.clone(),
                starting_depth: 0,
                guides: Vec::new(),
            })
            .unwrap();
        app.poll_background_results();
        assert!(app.cached_bracket_depth_index.is_none());
        app.tabs[0] = Tab::new_file(Editor::test_with_text(&format!(
            "{{\n{}}}\n",
            "plain\n".repeat(400)
        )));
        app.background
            .sender
            .send(crate::background::BackgroundResult::BracketAnalysis {
                target,
                depth_index: index,
                starting_depth: 0,
                guides: Vec::new(),
            })
            .unwrap();
        app.poll_background_results();
        assert!(app.cached_bracket_depth_index.is_none());
    }

    #[test]
    fn structural_scope_results_follow_the_source_across_cursor_and_viewport_changes() {
        use crate::animation_structural_scope::StructuralScopeRequest;
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("fn sample() {\n    work();\n}\n");
        editor.file_path = Some("sample.rs".into());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.animation_settings.set(
            crate::animation_config::AnimationEffect::ScopeConnector,
            true,
        );
        let request = StructuralScopeRequest::from_editor(app.editor().unwrap()).unwrap();
        let index = std::sync::Arc::new(request.build(None, || false).unwrap());
        app.pending_structural_scope = Some(request.clone());
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 1;
        editor.cursor_col = 6;
        editor.scroll_offset = 1;
        editor.horizontal_scroll = 3;
        app.background
            .sender
            .send(crate::background::BackgroundResult::StructuralScopes {
                request,
                index: Some(index.clone()),
            })
            .unwrap();
        assert!(app.poll_background_results());
        assert!(app.pending_structural_scope.is_none());
        assert!(std::sync::Arc::ptr_eq(
            app.cached_structural_scope_index.as_ref().unwrap(),
            &index
        ));
        assert_eq!(
            index.enclosing_scope(app.editor().unwrap()),
            Some(((0, 12), (2, 0)))
        );
    }

    #[test]
    fn structural_scope_results_reject_replacements_and_syntax_changes() {
        use crate::animation_structural_scope::StructuralScopeRequest;
        for change in 0..6 {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text("fn sample() {\n    work();\n}\n");
            editor.file_path = Some("sample.rs".into());
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.animation_settings.set(
                crate::animation_config::AnimationEffect::ScopeConnector,
                true,
            );
            let request = StructuralScopeRequest::from_editor(app.editor().unwrap()).unwrap();
            let index = std::sync::Arc::new(request.build(None, || false).unwrap());
            match change {
                0 => {
                    app.editor_mut().unwrap().rope =
                        ropey::Rope::from_str("fn other_() {\n    read();\n}\n")
                }
                1 => {
                    let mut editor = Editor::test_with_text("fn sample() {\n    work();\n}\n");
                    editor.file_path = Some("sample.rs".into());
                    app.tabs[0] = Tab::new_file(editor);
                }
                2 => app.editor_mut().unwrap().language_override = Some("javascript".into()),
                3 => app.editor_mut().unwrap().file_path = Some("renamed.rs".into()),
                4 => app.editor_mut().unwrap().content_version += 1,
                5 => app.editor_mut().unwrap().wrap_revision += 1,
                _ => unreachable!(),
            }
            app.background
                .sender
                .send(crate::background::BackgroundResult::StructuralScopes {
                    request,
                    index: Some(index),
                })
                .unwrap();
            assert!(
                app.poll_background_results(),
                "reschedule changed source {change}"
            );
            assert!(
                app.cached_structural_scope_index.is_none(),
                "reject changed source {change}"
            );
        }
    }

    #[test]
    fn empty_or_disabled_structural_scope_results_do_not_request_a_frame() {
        use crate::animation_structural_scope::StructuralScopeRequest;
        for disabled in [false, true] {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text("let value = 42;\n");
            editor.file_path = Some("sample.rs".into());
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.animation_settings.set(
                crate::animation_config::AnimationEffect::ScopeConnector,
                !disabled,
            );
            let request = StructuralScopeRequest::from_editor(app.editor().unwrap()).unwrap();
            let index = std::sync::Arc::new(request.build(None, || false).unwrap());
            app.background
                .sender
                .send(crate::background::BackgroundResult::StructuralScopes {
                    request,
                    index: Some(index),
                })
                .unwrap();
            assert!(!app.poll_background_results());
            assert_eq!(app.cached_structural_scope_index.is_some(), !disabled);
        }
    }

    #[test]
    fn scope_language_disable_rejects_completed_index_without_redraw() {
        use crate::animation_structural_scope::StructuralScopeRequest;
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("fn sample() {\n    work();\n}\n");
        editor.file_path = Some("sample.rs".into());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.animation_settings.set(crate::animation_config::AnimationEffect::ScopeConnector, true);
        let request = StructuralScopeRequest::from_editor(app.editor().unwrap()).unwrap();
        let index = std::sync::Arc::new(request.build(None, || false).unwrap());
        app.pending_structural_scope = Some(request.clone());
        app.animation_settings.scope_disabled_languages = vec!["rust".into()];
        app.background.sender.send(crate::background::BackgroundResult::StructuralScopes {
            request,
            index: Some(index),
        }).unwrap();
        assert!(!app.poll_background_results());
        assert!(app.pending_structural_scope.is_none());
        assert!(app.cached_structural_scope_index.is_none());
    }

    #[test]
    fn global_search_background_results_cap_and_mark_truncation() {
        let mut app = App::test_new();
        app.max_global_search_results = 3;
        app.search_generation = 7;
        let token = crate::background::CancellationToken::new();
        app.search_cancel_token = Some(token.clone());
        app.bg_global_search_running = true;
        app.global_search_results = (0..app.max_global_search_results - 1)
            .map(sample_global_search_result)
            .collect();

        app.background
            .sender
            .send(crate::background::BackgroundResult::GlobalSearchResults {
                results: vec![
                    sample_global_search_result(999),
                    sample_global_search_result(1000),
                ],
                done: false,
                generation: 7,
                truncated: false,
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert_eq!(
            app.global_search_results.len(),
            app.max_global_search_results
        );
        assert!(app.global_search_truncated);
        assert!(!app.bg_global_search_running);
        assert!(token.is_cancelled());
    }

    #[test]
    fn clearing_global_search_rejects_already_queued_results() {
        let mut app = App::test_new();
        app.search_generation = 7;
        app.global_search_query = "needle".into();
        app.global_search_results = vec![sample_global_search_result(0)].into();

        app.clear_global_search();
        assert_eq!(app.search_generation, 8);
        app.background
            .sender
            .send(crate::background::BackgroundResult::GlobalSearchResults {
                results: vec![sample_global_search_result(1)],
                done: true,
                generation: 7,
                truncated: false,
            })
            .unwrap();

        assert!(!app.poll_background_results());
        assert!(app.global_search_results.is_empty());
        assert!(!app.bg_global_search_running);
    }

    #[test]
    fn git_line_diff_results_update_inactive_matching_tabs() {
        let root = std::env::current_dir().unwrap();
        let mut app = App::new(root.clone());
        let active_path = root.join("active.rs");
        let target_path = root.join("target.rs");

        let mut active_editor = Editor::new();
        active_editor.file_path = Some(active_path);
        active_editor.rope = ropey::Rope::from_str("active");
        active_editor.saved_rope = Some(active_editor.rope.clone());

        let mut target_editor = Editor::new();
        target_editor.file_path = Some(target_path.clone());
        target_editor.rope = ropey::Rope::from_str("one\ntwo");
        target_editor.saved_rope = Some(target_editor.rope.clone());

        app.tabs = vec![Tab::new_file(active_editor), Tab::new_file(target_editor)];
        app.active_tab = 0;
        app.bg_git_diff_running = true;

        app.background
            .sender
            .send(crate::background::BackgroundResult::GitLineDiff {
                file_path: target_path,
                status: crate::editor::PreparedGitLineStatus::from_diffs(
                    2,
                    &[' ', ' '],
                    &[' ', 'M'],
                ),
            })
            .unwrap();

        assert!(app.poll_background_results());
        assert_eq!(app.tabs[1].editor.git_line_status(), [' ', 'U']);
        assert!(app.tabs[0].editor.git_line_status().is_empty());
    }

    #[test]
    fn git_refresh_requests_are_queued_while_workers_are_running() {
        let root = std::env::current_dir().unwrap();
        let mut app = App::new(root.clone());
        let mut editor = Editor::new();
        editor.file_path = Some(root.join("queued-refresh.rs"));
        editor.rope = ropey::Rope::from_str("line\n");
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;

        app.bg_git_diff_running = true;
        app.refresh_git_line_status_async();
        assert!(app.bg_git_diff_pending);

        app.bg_git_status_running = true;
        app.refresh_source_control_async();
        assert!(app.bg_git_status_pending);
    }

    #[test]
    fn git_line_status_preserves_deletion_markers() {
        let mut editor = Editor::new();
        editor.rope = ropey::Rope::from_str("");

        let markers = crate::editor::PreparedGitLineStatus::from_diffs(
            editor.rope.len_lines(),
            &['D'],
            &[],
        );

        assert_eq!(markers.markers(), ['D']);
    }

    #[test]
    fn set_compare_diff_lines_updates_active_editor_markers() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(PathBuf::from("file.txt"));
        editor.rope = ropey::Rope::from_str("after\n");
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        let expected = compute_file_diff("before\n", "after\n");
        app.set_compare_diff_lines(expected.clone());

        assert_eq!(app.compare_diff_lines, expected);
        assert!(app.tabs[0]
            .editor
            .git_line_status()
            .iter()
            .any(|marker| *marker != ' '));
    }

    #[test]
    fn clear_diff_state_clears_target_tab_markers_even_when_inactive() {
        let mut app = App::test_new();
        let mut diff_editor = Editor::new();
        diff_editor.file_path = Some(PathBuf::from("diff.txt"));
        diff_editor.set_git_line_status(vec!['U', ' ']);
        let mut other_editor = Editor::new();
        other_editor.file_path = Some(PathBuf::from("other.txt"));
        other_editor.set_git_line_status(vec!['S']);
        app.tabs = vec![Tab::new_file(diff_editor), Tab::new_file(other_editor)];
        app.active_tab = 1;
        app.diff_mode = DiffMode::SideBySide;
        app.diff_target_path = Some(PathBuf::from("diff.txt"));
        app.compare_base_content = Some("before\n".to_string().into());
        app.compare_diff_lines = compute_file_diff("before\n", "after\n");

        app.clear_diff_state();

        assert!(matches!(app.diff_mode, DiffMode::Off));
        assert!(app.tabs[0].editor.git_line_status().is_empty());
        assert_eq!(app.tabs[1].editor.git_line_status(), ['S']);
        assert!(app.compare_base_content.is_none());
        assert!(app.compare_diff_lines.is_empty());
    }

    #[test]
    fn binary_prompt_open_normally_uses_regular_open_path() {
        let mut root = std::env::temp_dir();
        root.push(format!("vtcode-open-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file_path = root.join("trace.vcd");
        std::fs::write(&file_path, "$enddefinitions $end\n#0\n0!\n").unwrap();

        let mut app = App::new(root.clone());
        app.binary_open_prompt_active = true;
        app.binary_open_prompt_path = Some(file_path.clone());
        app.binary_open_prompt_selected = 0;
        app.binary_open_prompt_open_normally = true;

        app.execute_binary_open_prompt_choice(0);

        assert!(!app.binary_open_prompt_active);
        assert!(app.binary_open_prompt_skip_once.is_none());
        assert!(!app.tabs.is_empty());
        assert_eq!(
            app.tabs[app.active_tab].editor.file_path.as_ref(),
            Some(&file_path)
        );

        let _ = std::fs::remove_file(file_path); // Error OK: file may not exist
        let _ = std::fs::remove_dir_all(root); // Error OK: dir may not exist
    }

    #[test]
    fn save_session_skips_unchanged_startup_state() {
        let root = unique_temp_app_root();
        let mut app = App::new(root.clone());
        app.capture_session_baseline();

        app.save_session();

        assert!(!root.join(".vtcode").exists());
        assert!(!crate::session::session_path(&root).exists());

        let _ = std::fs::remove_dir_all(root); // Error OK: dir may not exist
    }

    #[test]
    fn save_session_writes_when_workspace_state_changes() {
        let root = unique_temp_app_root();
        let mut app = App::new(root.clone());
        app.capture_session_baseline();
        app.show_panel = true;

        app.save_session();

        assert!(crate::session::session_path(&root).exists());
        assert!(!root.join(".vtcode").exists());

        let _ = std::fs::remove_dir_all(root); // Error OK: dir may not exist
    }

    #[test]
    fn ctrl_click_preserves_explorer_anchor_for_followup_shift_select() {
        let root = std::env::current_dir().unwrap();
        let mut app = App::new(root.clone());
        app.explorer.entries = (0..5)
            .map(|idx| crate::explorer::FileEntry {
                path: root.join(format!("file-{idx}.txt")),
                name: format!("file-{idx}.txt"),
                is_dir: false,
                depth: 0,
                expanded: false,
                git_status: crate::explorer::GitStatus::Unmodified,
                git_status_summary: crate::explorer::GitStatusSummary::default(),
            })
            .collect();

        app.set_explorer_single_selection(1);
        app.toggle_explorer_selection(3);
        app.range_select_explorer(4, false);

        let selected_names: Vec<String> = app
            .explorer_multi_selected
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(app.explorer_selection_anchor, Some(1));
        assert_eq!(
            selected_names,
            vec![
                "file-1.txt".to_string(),
                "file-2.txt".to_string(),
                "file-3.txt".to_string(),
                "file-4.txt".to_string(),
            ]
        );
    }

    #[test]
    fn ctrl_shift_range_select_adds_from_original_anchor() {
        let root = std::env::current_dir().unwrap();
        let mut app = App::new(root.clone());
        app.explorer.entries = (0..6)
            .map(|idx| crate::explorer::FileEntry {
                path: root.join(format!("entry-{idx}.txt")),
                name: format!("entry-{idx}.txt"),
                is_dir: false,
                depth: 0,
                expanded: false,
                git_status: crate::explorer::GitStatus::Unmodified,
                git_status_summary: crate::explorer::GitStatusSummary::default(),
            })
            .collect();

        app.set_explorer_single_selection(1);
        app.toggle_explorer_selection(4);
        app.range_select_explorer(5, true);

        let selected_names: Vec<String> = app
            .explorer_multi_selected
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(app.explorer_selection_anchor, Some(1));
        assert_eq!(
            selected_names,
            vec![
                "entry-1.txt".to_string(),
                "entry-4.txt".to_string(),
                "entry-2.txt".to_string(),
                "entry-3.txt".to_string(),
                "entry-5.txt".to_string(),
            ]
        );
    }
}
