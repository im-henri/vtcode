//! Parse shortcut conditions once and evaluate them against live keyboard focus.
use crate::{
    app::{ActivePane, App, KeyBinding, SidebarView, TabKind},
    text_input::TextInputId,
};
use std::sync::{Arc, LazyLock};

#[derive(Clone)]
pub(crate) struct When {
    text: Arc<str>,
    expression: Arc<Expr>,
}

impl When {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.len() > 512 {
            return Err("A When clause can have up to 512 bytes.".into());
        }
        let mut parser = Parser(text);
        let expression = if text.is_empty() {
            Expr::Bool(true)
        } else {
            parser.any(0)?
        };
        if !parser.0.trim().is_empty() {
            return Err(format!(
                "Unexpected input in When clause: {}",
                parser.0.trim()
            ));
        }
        Ok(Self {
            text: text.into(),
            expression: Arc::new(expression),
        })
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn matches(&self, context: &Context<'_>) -> bool {
        self.expression.matches(context)
    }
}

enum Expr {
    Bool(bool),
    Key(Key),
    Not(Box<Expr>),
    All(Vec<Expr>),
    Any(Vec<Expr>),
    Equal(Key, String, bool),
    Number(Key, f64, NumberOp),
    Regex(Key, regex::Regex),
}

enum NumberOp {
    Less,
    AtMost,
    Greater,
    AtLeast,
}

impl Expr {
    fn matches(&self, context: &Context<'_>) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Key(key) => match context.value(*key) {
                Value::Bool(value) => value,
                Value::Text(value) => !value.is_empty(),
                Value::Number(value) => value != 0.0,
            },
            Self::Not(expr) => !expr.matches(context),
            Self::All(items) => items.iter().all(|expr| expr.matches(context)),
            Self::Any(items) => items.iter().any(|expr| expr.matches(context)),
            Self::Equal(key, expected, equal) => {
                let matched = match context.value(*key) {
                    Value::Bool(value) => expected == if value { "true" } else { "false" },
                    Value::Text(value) => expected == value.as_ref(),
                    Value::Number(value) => expected.parse::<f64>() == Ok(value),
                };
                matched == *equal
            }
            Self::Number(key, expected, operator) => {
                let Value::Number(value) = context.value(*key) else {
                    return false;
                };
                match operator {
                    NumberOp::Less => value < *expected,
                    NumberOp::AtMost => value <= *expected,
                    NumberOp::Greater => value > *expected,
                    NumberOp::AtLeast => value >= *expected,
                }
            }
            Self::Regex(key, regex) => match context.value(*key) {
                Value::Text(value) => regex.is_match(&value),
                Value::Bool(_) | Value::Number(_) => false,
            },
        }
    }
}

struct Parser<'a>(&'a str);

impl Parser<'_> {
    fn eat(&mut self, token: &str) -> bool {
        self.0 = self.0.trim_start();
        if let Some(rest) = self.0.strip_prefix(token) {
            self.0 = rest;
            true
        } else {
            false
        }
    }

    fn any(&mut self, depth: usize) -> Result<Expr, String> {
        let first = self.all(depth)?;
        if !self.eat("||") {
            return Ok(first);
        }
        let mut items = vec![first];
        loop {
            items.push(self.all(depth)?);
            if !self.eat("||") {
                return Ok(Expr::Any(items));
            }
        }
    }

    fn all(&mut self, depth: usize) -> Result<Expr, String> {
        let first = self.atom(depth)?;
        if !self.eat("&&") {
            return Ok(first);
        }
        let mut items = vec![first];
        loop {
            items.push(self.atom(depth)?);
            if !self.eat("&&") {
                return Ok(Expr::All(items));
            }
        }
    }

    fn atom(&mut self, depth: usize) -> Result<Expr, String> {
        if depth >= 32 {
            return Err("This When clause is nested too deeply.".into());
        }
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.atom(depth + 1)?)));
        }
        if self.eat("(") {
            let expression = self.any(depth + 1)?;
            return if self.eat(")") {
                Ok(expression)
            } else {
                Err("Missing ')' in When clause.".into())
            };
        }
        let name = self.word();
        if name.is_empty() {
            return Err("Expected a context key in the When clause.".into());
        }
        if name == "true" || name == "false" {
            return Ok(Expr::Bool(name == "true"));
        }
        let key =
            Key::parse(name).ok_or_else(|| format!("Unknown shortcut context key '{name}'."))?;
        if self.eat("!==") || self.eat("!=") {
            return Ok(Expr::Equal(key, self.value()?, false));
        }
        if self.eat("===") || self.eat("==") {
            return Ok(Expr::Equal(key, self.value()?, true));
        }
        if self.eat("=~") {
            return Ok(Expr::Regex(key, self.regex()?));
        }
        let operator = if self.eat("<=") {
            NumberOp::AtMost
        } else if self.eat(">=") {
            NumberOp::AtLeast
        } else if self.eat("<") {
            NumberOp::Less
        } else if self.eat(">") {
            NumberOp::Greater
        } else {
            return Ok(Expr::Key(key));
        };
        let number = self
            .word()
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .ok_or_else(|| "Expected a number after the comparison operator.".to_owned())?;
        Ok(Expr::Number(key, number, operator))
    }

    fn word(&mut self) -> &str {
        self.0 = self.0.trim_start();
        let end = self
            .0
            .find(|ch: char| ch.is_whitespace() || "()!&|=<>".contains(ch))
            .unwrap_or(self.0.len());
        let (word, rest) = self.0.split_at(end);
        self.0 = rest;
        word
    }

    fn value(&mut self) -> Result<String, String> {
        self.0 = self.0.trim_start();
        if let Some(quote @ ('\'' | '"')) = self.0.chars().next() {
            self.0 = &self.0[1..];
            let mut value = String::new();
            let mut chars = self.0.char_indices();
            while let Some((index, ch)) = chars.next() {
                match ch {
                    ch if ch == quote => {
                        self.0 = &self.0[index + 1..];
                        return Ok(value);
                    }
                    '\\' => {
                        let Some((_, escaped)) = chars.next() else {
                            break;
                        };
                        if escaped != quote && escaped != '\\' {
                            value.push('\\');
                        }
                        value.push(escaped);
                    }
                    ch => value.push(ch),
                }
            }
            return Err("Unclosed quoted value in When clause.".into());
        }
        let word = self.word();
        if word.is_empty() {
            Err("Expected a value after the equality operator.".into())
        } else {
            Ok(word.to_owned())
        }
    }

    fn regex(&mut self) -> Result<regex::Regex, String> {
        if !self.eat("/") {
            return Err("Write the regular expression between slashes.".into());
        }
        let mut escaped = false;
        let end = self
            .0
            .char_indices()
            .find_map(|(index, ch)| {
                if ch == '/' && !escaped {
                    return Some(index);
                }
                escaped = ch == '\\' && !escaped;
                None
            })
            .ok_or_else(|| "Unclosed regular expression in When clause.".to_owned())?;
        let pattern = &self.0[..end];
        self.0 = &self.0[end + 1..];
        let end_flags = self
            .0
            .find(|ch: char| !ch.is_ascii_alphabetic())
            .unwrap_or(self.0.len());
        let (flags, rest) = self.0.split_at(end_flags);
        self.0 = rest;
        if flags
            .chars()
            .any(|flag| !matches!(flag, 'i' | 'm' | 's' | 'u'))
        {
            return Err("Supported shortcut regex flags are i, m, s, and u.".into());
        }
        regex::RegexBuilder::new(pattern)
            .case_insensitive(flags.contains('i'))
            .multi_line(flags.contains('m'))
            .dot_matches_new_line(flags.contains('s'))
            .size_limit(64 * 1024)
            .build()
            .map_err(|error| format!("Invalid regular expression in When clause: {error}"))
    }
}

#[derive(Clone, Copy)]
enum Key {
    Editor,
    EditorText,
    EditorReadonly,
    Selection,
    Language,
    Extension,
    Terminal,
    TerminalAltScreen,
    TerminalOpen,
    TerminalSelected,
    RemoteConnection,
    Sidebar,
    Explorer,
    Search,
    SearchInput,
    SearchResults,
    FindInput,
    FindVisible,
    SourceControl,
    Remote,
    Settings,
    Keybindings,
    Chat,
    Panel,
    Input,
    Suggest,
    Zen,
    Recording,
    RemoteWorkspace,
    RemoteName,
    Workbench,
    FolderCount,
    TabCount,
    ActiveView,
    Windows,
    Linux,
    Mac,
}

impl Key {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "editorFocus" => Self::Editor,
            "editorTextFocus" => Self::EditorText,
            "editorReadonly" => Self::EditorReadonly,
            "editorHasSelection" => Self::Selection,
            "editorLangId" => Self::Language,
            "resourceExtname" => Self::Extension,
            "terminalFocus" => Self::Terminal,
            "terminalAltScreen" => Self::TerminalAltScreen,
            "terminalIsOpen" => Self::TerminalOpen,
            "terminalTextSelected" => Self::TerminalSelected,
            "remoteConnectionFocus" => Self::RemoteConnection,
            "sideBarFocus" => Self::Sidebar,
            "filesExplorerFocus" | "explorerFocus" => Self::Explorer,
            "searchViewletFocus" | "searchFocus" => Self::Search,
            "searchInputFocus" => Self::SearchInput,
            "searchResultsFocus" => Self::SearchResults,
            "findInputFocus" => Self::FindInput,
            "findWidgetVisible" => Self::FindVisible,
            "sourceControlFocus" => Self::SourceControl,
            "remoteViewFocus" => Self::Remote,
            "settingsFocus" => Self::Settings,
            "keybindingsFocus" => Self::Keybindings,
            "chatFocus" => Self::Chat,
            "panelFocus" => Self::Panel,
            "inputFocus" | "textInputFocus" => Self::Input,
            "suggestWidgetVisible" => Self::Suggest,
            "inZenMode" => Self::Zen,
            "macroRecording" => Self::Recording,
            "remoteWorkspace" => Self::RemoteWorkspace,
            "remoteName" => Self::RemoteName,
            "workbenchState" => Self::Workbench,
            "workspaceFolderCount" => Self::FolderCount,
            "editorTabCount" => Self::TabCount,
            "activeView" => Self::ActiveView,
            "isWindows" => Self::Windows,
            "isLinux" => Self::Linux,
            "isMac" => Self::Mac,
            _ => return None,
        })
    }
}

enum Value<'a> {
    Bool(bool),
    Text(std::borrow::Cow<'a, str>),
    Number(f64),
}

pub(crate) struct Context<'a> {
    app: &'a App,
    terminal: bool,
    remote_connection: bool,
    input: std::cell::OnceCell<Option<TextInputId>>,
}

impl<'a> Context<'a> {
    pub(crate) fn new(app: &'a App) -> Self {
        let terminal = app.focused_terminal_index().is_some();
        Self {
            app,
            terminal,
            remote_connection: !terminal && crate::ui_remote_targets::connection_has_focus(app),
            input: Default::default(),
        }
    }

    pub(crate) fn shell_focus(&self) -> bool {
        self.terminal || self.remote_connection
    }

    pub(crate) fn shell_input_has_focus(&self) -> bool {
        self.shell_focus() && self.focused_input().is_none()
    }

    fn focused_input(&self) -> Option<TextInputId> {
        *self.input.get_or_init(|| self.app.focused_text_input_id())
    }

    fn value(&self, key: Key) -> Value<'_> {
        use std::borrow::Cow;
        let app = self.app;
        let input = self.focused_input();
        let tab = app.tabs.get(app.active_tab);
        let search_results =
            !self.shell_focus() && app.global_search_active && app.global_search_results_focused;
        let editor_area = app.active_pane == ActivePane::Editor
            && !self.shell_focus()
            && input.is_none()
            && !search_results;
        let file = tab.filter(|tab| {
            matches!(
                tab.kind,
                TabKind::File | TabKind::GitRevision | TabKind::Diff(_)
            )
        });
        let text_editor = editor_area && file.is_some();
        let editor = text_editor
            || (editor_area && tab.is_some_and(|tab| tab.kind == TabKind::ExtensionView));
        let terminal = self.terminal && input.is_none();
        let sidebar = app.active_pane == ActivePane::Explorer
            && !self.shell_focus()
            && app.sidebar_is_visible();
        let search = sidebar && app.sidebar_view == SidebarView::Search
            || matches!(input, Some(TextInputId::GlobalSearch(_)))
            || search_results;
        let find = matches!(
            input,
            Some(TextInputId::SearchFind | TextInputId::SearchReplace)
        );
        let settings = !self.shell_focus()
            && (app.settings_editor_has_focus()
                || app.settings_active
                || (sidebar && app.sidebar_view == SidebarView::Settings));
        let chat = !self.shell_focus() && app.active_pane == ActivePane::Chat && app.chat.visible;
        Value::Bool(match key {
            Key::Editor => editor,
            Key::EditorText => text_editor,
            Key::EditorReadonly => {
                file.is_some_and(|tab| tab.editor.read_only) || app.diff_selection_owns_cursor()
            }
            Key::Selection => {
                text_editor && file.is_some_and(|tab| tab.editor.selection_range().is_some())
            }
            Key::Language => {
                return Value::Text(file.map_or(Cow::Borrowed(""), |tab| {
                    Cow::Owned(crate::file_associations::language_id(
                        tab.editor.language_name(),
                    ))
                }));
            }
            Key::Extension => {
                return Value::Text(
                    file.and_then(|tab| tab.editor.file_path.as_ref()?.extension()?.to_str())
                        .map_or(Cow::Borrowed(""), |extension| {
                            Cow::Owned(format!(".{extension}"))
                        }),
                );
            }
            Key::Terminal => terminal,
            Key::TerminalAltScreen => terminal && app.focused_terminal_is_fullscreen(),
            Key::TerminalOpen => (0..app.terminals.len()).any(|index| app.terminal_is_open(index)),
            Key::TerminalSelected => terminal && app.focused_terminal_has_selection(),
            Key::RemoteConnection => self.remote_connection && input.is_none(),
            Key::Sidebar => sidebar,
            Key::Explorer => {
                sidebar && app.sidebar_view == SidebarView::Explorer && input.is_none()
            }
            Key::Search => search,
            Key::SearchInput => matches!(input, Some(TextInputId::GlobalSearch(_))),
            Key::SearchResults => search_results && input.is_none(),
            Key::FindInput => find,
            Key::FindVisible => app.file_search_visible(),
            Key::SourceControl => !self.shell_focus() && app.source_control_has_focus(),
            Key::Remote => sidebar && app.sidebar_view == SidebarView::Remote,
            Key::Settings => settings,
            Key::Keybindings => !self.shell_focus() && crate::ui_keybindings::focused(app),
            Key::Chat => chat,
            Key::Panel => app.active_pane == ActivePane::Panel || app.docked_terminal_has_focus(),
            Key::Input => text_editor || input.is_some(),
            Key::Suggest => text_editor && (app.lsp_completion_active || app.vtcomplete_active),
            Key::Zen => app.zen_mode,
            Key::Recording => app.macro_recording,
            Key::RemoteWorkspace => app.remote_workspace.is_some(),
            Key::RemoteName => {
                return Value::Text(
                    if app.remote_workspace.is_some() {
                        "ssh"
                    } else {
                        ""
                    }
                    .into(),
                );
            }
            Key::Workbench => {
                return Value::Text(
                    if app.has_open_folder() {
                        "folder"
                    } else {
                        "empty"
                    }
                    .into(),
                );
            }
            Key::FolderCount => return Value::Number(app.open_folder_count() as f64),
            Key::TabCount => return Value::Number(app.tabs.len() as f64),
            Key::ActiveView => {
                return Value::Text(
                    (if terminal {
                        "terminal"
                    } else if self.remote_connection {
                        "remoteConnection"
                    } else if search {
                        "search"
                    } else if settings {
                        "settings"
                    } else if chat {
                        "chat"
                    } else if sidebar {
                        match app.sidebar_view {
                            SidebarView::Explorer => "explorer",
                            SidebarView::Search => "search",
                            SidebarView::SourceControl => "sourceControl",
                            SidebarView::Settings => "settings",
                            SidebarView::Extensions => "extensions",
                            SidebarView::Remote => "remote",
                        }
                    } else if editor {
                        "editor"
                    } else {
                        ""
                    })
                    .into(),
                );
            }
            Key::Windows => cfg!(windows),
            Key::Linux => cfg!(target_os = "linux"),
            Key::Mac => cfg!(target_os = "macos"),
        })
    }
}

pub(crate) fn default_when(binding: &KeyBinding) -> &'static When {
    static ALWAYS: LazyLock<When> = LazyLock::new(|| When::parse("").unwrap());
    static WORKBENCH: LazyLock<When> = LazyLock::new(|| When::parse("!terminalAltScreen").unwrap());
    static EDITOR: LazyLock<When> = LazyLock::new(|| When::parse("editorTextFocus").unwrap());
    static FIND: LazyLock<When> =
        LazyLock::new(|| When::parse("editorFocus || findInputFocus").unwrap());
    static ZEN: LazyLock<When> = LazyLock::new(|| {
        When::parse("!terminalFocus && !remoteConnectionFocus || inZenMode && !terminalAltScreen")
            .unwrap()
    });
    static NON_TERMINAL: LazyLock<When> = LazyLock::new(|| When::parse("!terminalFocus").unwrap());
    static NON_SHELL: LazyLock<When> =
        LazyLock::new(|| When::parse("!terminalFocus && !remoteConnectionFocus").unwrap());
    static START_MACRO: LazyLock<When> =
        LazyLock::new(|| When::parse("editorTextFocus && !macroRecording").unwrap());
    static STOP_MACRO: LazyLock<When> =
        LazyLock::new(|| When::parse("editorTextFocus && macroRecording").unwrap());
    match binding.id {
        "toggle_terminal" => &ALWAYS,
        // Alt+F is also the shell's forward-word shortcut.
        "global_search" if binding.default_modifiers == crossterm::event::KeyModifiers::ALT => {
            &NON_SHELL
        }
        // Editor-only Alt aliases; shells and other views keep these keys.
        "show_explorer" | "toggle_problems"
            if binding.default_modifiers == crossterm::event::KeyModifiers::ALT =>
        {
            &EDITOR
        }
        "new_terminal" | "palette" | "source_control" | "extensions" | "global_search"
        | "toggle_explorer" | "show_explorer" | "toggle_panel" | "toggle_problems" | "acp_chat"
        | "next_tab" | "prev_tab" | "next_tab_page" | "prev_tab_page" | "navigate_back"
        | "navigate_forward" | "workspace_switch" | "workspace_add" | "workspace_next"
        | "workspace_previous" | "workspace_sets" | "workspace_set_save" | "reopen_closed" => {
            &WORKBENCH
        }
        "toggle_zen" => &ZEN,
        "close_tab" | "quit" | "quick_open" => &NON_TERMINAL,
        "macro_start" => &START_MACRO,
        "macro_stop" => &STOP_MACRO,
        "find" | "find_replace" | "find_next" | "find_prev" => &FIND,
        "rename_symbol"
        | "go_to_definition"
        | "go_to_type_definition"
        | "go_to_implementation"
        | "go_to_declaration"
        | "find_references" => &EDITOR,
        _ if binding.editor_only => &EDITOR,
        _ => &NON_SHELL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Tab, editor::Editor};

    #[test]
    fn when_expressions_evaluate_precedence_comparisons_and_regex() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("example");
        editor.file_path = Some("example.LOG".into());
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        let context = Context::new(&app);

        for (source, expected) in [
            ("", true),
            ("true || false && false", true),
            ("(true || false) && false", false),
            ("!!editorTextFocus && !terminalFocus", true),
            ("activeView == 'editor' && activeView != terminal", true),
            ("editorTabCount >= 1 && editorTabCount < 2", true),
            (r"resourceExtname =~ /^\.(log|txt)$/i", true),
            (r"resourceExtname =~ /^\.log$/", false),
        ] {
            assert_eq!(
                When::parse(source).unwrap().matches(&context),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn malformed_when_expressions_are_rejected() {
        for source in [
            "editorFoucs",
            "terminalFocus &&",
            "(terminalFocus || editorFocus",
            "activeView == 'terminal",
            "editorTabCount > many",
            "resourceExtname =~ /[a-/",
            "resourceExtname =~ /abc/g",
            "editorFocus terminalFocus",
        ] {
            assert!(When::parse(source).is_err(), "{source}");
        }
    }
}
