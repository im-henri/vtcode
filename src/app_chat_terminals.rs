//! Independent, read-only terminal viewports inside each conversation.
use crate::{app::App, app_acp::ChatAction};
use std::{
    collections::{HashMap, HashSet},
    sync::mpsc,
};
use vtcode::acp::{
    ChatEntry,
    terminal_history::{TERMINAL_ROWS, TerminalPage},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct PageKey {
    first: Option<usize>,
    column: usize,
    width: usize,
    height: usize,
    version: u64,
}
impl PageKey {
    fn same_position(&self, other: &Self) -> bool {
        self.first == other.first
            && self.column == other.column
            && self.width == other.width
            && self.height == other.height
    }
}

#[derive(Default)]
pub(crate) struct TerminalView {
    pub first: Option<usize>,
    pub column: usize,
    pub page: Option<TerminalPage>,
    pub error: Option<String>,
    pub width: usize,
    pub height: usize,
    loaded: Option<PageKey>,
    pending: Option<(PageKey, mpsc::Receiver<Result<TerminalPage, String>>)>,
}
pub(crate) type TerminalViews = HashMap<String, TerminalView>;

// Poll saved views too: a page finishing in another visible chat pane must
// request a repaint even when the selected conversation has no new output.
pub(crate) fn poll_views(views: &mut TerminalViews) -> bool {
    let mut changed = false;
    for view in views.values_mut() {
        let result = view
            .pending
            .as_ref()
            .and_then(|(_, rx)| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Terminal output reader stopped".into()))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            });
        if let Some(result) = result {
            changed = true;
            let (mut key, _) = view.pending.take().unwrap();
            if key.first != view.first
                || key.column != view.column
                || key.width != view.width
                || key.height != view.height
            {
                continue;
            }
            match result {
                Ok(page) => {
                    key.version = page.version;
                    view.page = Some(page);
                    view.error = None;
                }
                Err(error) => view.error = Some(error),
            }
            view.loaded = Some(key);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode::acp::protocol::SessionId;

    #[test]
    fn completed_terminal_page_in_another_conversation_requests_repaint() {
        let mut app = App::test_new();
        let first = SessionId::new("first");
        app.chat.selected = Some(first.clone());
        let (tx, rx) = mpsc::channel();
        app.chat.terminal_views.insert(
            "term".into(),
            TerminalView {
                width: 80,
                height: 10,
                pending: Some((
                    PageKey {
                        first: None,
                        column: 0,
                        width: 80,
                        height: 10,
                        version: 1,
                    },
                    rx,
                )),
                ..Default::default()
            },
        );
        app.chat.select_view(Some(SessionId::new("other")));
        tx.send(Ok(TerminalPage {
            version: 1,
            lines: vec!["completed".into()],
            ..Default::default()
        }))
        .unwrap();
        assert!(app.poll_chat_terminals());
        assert!(!app.poll_chat_terminals());
        app.chat.select_view(Some(first));
        assert_eq!(
            app.chat.terminal_views["term"].page.as_ref().unwrap().lines,
            ["completed"]
        );
    }
}

impl App {
    pub(crate) fn poll_chat_terminals(&mut self) -> bool {
        if !self.chat.connection.terminal_views.is_empty()
            && let Some(session) = self
            .chat
            .selected
            .as_ref()
            .and_then(|id| self.chat.connection.sessions.get(id))
        {
            // Most conversations have no terminal view. Even when one is
            // open, native terminal IDs do not need a transcript-wide scan.
            let tool_ids = std::cell::OnceCell::<HashSet<&str>>::new();
            self.chat.connection.terminal_views.retain(|id, _| {
                session.terminals.contains_key(id)
                    || id
                        .strip_prefix("tool:")
                        .is_some_and(|id| tool_ids.get_or_init(|| {
                            session.entries.iter().filter_map(|entry| match entry {
                                ChatEntry::Tool(tool) => Some(tool.tool_call_id.0.as_ref()),
                                _ => None,
                            }).collect()
                        }).contains(id))
            });
        }
        let changed = poll_views(&mut self.chat.terminal_views);
        self.chat.poll_saved_terminal_views() || changed
    }

    pub(crate) fn request_chat_terminal_page(&mut self, id: &str, width: usize, height: usize) {
        let Some(snapshot) = self
            .chat
            .selected
            .as_ref()
            .and_then(|s| self.chat.connection.sessions.get(s))
            .and_then(|s| s.terminals.get(id))
        else {
            return;
        };
        let view = self.chat.connection.terminal_views.entry(id.into()).or_default();
        view.width = width;
        view.height = height;
        let key = PageKey {
            first: view.first,
            column: view.column,
            width,
            height,
            version: snapshot.history_version,
        };
        if view.pending.is_some()
            || view
                .loaded
                .as_ref()
                .is_some_and(|k| k.same_position(&key) && k.version >= key.version)
        {
            return;
        }
        if let Some(history) = snapshot.history.clone() {
            let (tx, rx) = mpsc::channel();
            view.pending = Some((key.clone(), rx));
            let wake = self.background.wake_handle();
            let _ = self.background.pool_sender().send(Box::new(move || {
                let result = history
                    .page(key.first, key.column, key.width, key.height)
                    .map_err(|e| e.to_string());
                let _ = tx.send(result);
                wake.notify();
            }));
        } else {
            // In-memory/test snapshots have no recorder. Live hosted commands
            // always use the indexed transcript, including after agent release.
            let output = vtcode::acp::terminal_style::parse(&snapshot.output);
            let lines: Vec<_> = output.iter().map(|line| line.text.as_str()).collect();
            let total = lines.len();
            let first = view
                .first
                .unwrap_or(total.saturating_sub(height))
                .min(total.saturating_sub(height));
            view.page = Some(TerminalPage {
                styled: output
                    .iter()
                    .skip(first)
                    .take(height)
                    .map(|line| line.window(view.column, width))
                    .collect(),
                version: snapshot.history_version,
                first,
                total,
                max_columns: lines
                    .iter()
                    .map(|s| unicode_width::UnicodeWidthStr::width(*s))
                    .max()
                    .unwrap_or(0),
                lines: lines
                    .iter()
                    .skip(first)
                    .take(height)
                    .map(|s| vtcode::acp::terminal_history::window(s, view.column, width))
                    .collect(),
            });
            view.loaded = Some(key);
        }
    }

    pub(crate) fn request_chat_tool_terminal_page(
        &mut self,
        id: &str,
        revision: u64,
        lines: &[String],
        styled: &[vtcode::acp::terminal_style::TerminalLine],
        columns: usize,
        width: usize,
        height: usize,
    ) {
        let view = self
            .chat
            .terminal_views
            .entry(id.into())
            .or_insert_with(|| TerminalView {
                first: Some(0),
                ..Default::default()
            });
        view.width = width;
        view.height = height;
        let key = PageKey {
            first: view.first,
            column: view.column,
            width,
            height,
            version: revision,
        };
        if view.loaded.as_ref() == Some(&key) {
            return;
        }
        let first = view
            .first
            .unwrap_or(lines.len().saturating_sub(height))
            .min(lines.len().saturating_sub(height));
        view.page = Some(TerminalPage {
            styled: styled
                .iter()
                .skip(first)
                .take(height)
                .map(|line| line.window(view.column, width))
                .collect(),
            version: revision,
            first,
            total: lines.len(),
            max_columns: columns,
            lines: lines
                .iter()
                .skip(first)
                .take(height)
                .map(|line| vtcode::acp::terminal_history::window(line, view.column, width))
                .collect(),
        });
        view.loaded = Some(key);
    }

    pub(crate) fn scroll_chat_terminal(&mut self, id: &str, amount: isize) {
        let Some(view) = self.chat.terminal_views.get_mut(id) else {
            return;
        };
        let Some(page) = &view.page else {
            return;
        };
        let max = page.total.saturating_sub(view.height.max(1));
        let start = view.first.unwrap_or(page.first);
        let next = start.saturating_add_signed(amount).min(max);
        view.first = if next == max { None } else { Some(next) };
        self.chat.follow = false;
        self.chat.focus = Some(ChatAction::FocusTerminal(id.into()));
    }

    pub(crate) fn pan_chat_terminal(&mut self, id: &str, amount: isize) {
        if let Some(view) = self.chat.terminal_views.get_mut(id) {
            let max = view
                .page
                .as_ref()
                .map_or(0, |p| p.max_columns.saturating_sub(view.width));
            view.column = view.column.saturating_add_signed(amount).min(max);
            self.chat.follow = false;
            self.chat.focus = Some(ChatAction::FocusTerminal(id.into()));
        }
    }

    pub(crate) fn reveal_chat_terminal(&mut self, id: String) {
        if let Some(index) = self.chat.session().and_then(|s| {
            s.entries
                .iter()
                .position(|e| matches!(e, ChatEntry::Terminal(key) if key == &id))
        }) {
            self.chat.expanded.insert(index);
            self.chat.reveal_terminal = Some(id);
            self.chat.follow = false;
        } else {
            self.chat.error = Some(
                "This terminal is owned by the agent and has no hosted output in this chat.".into(),
            );
        }
    }

    pub(crate) fn chat_terminal_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let Some(ChatAction::FocusTerminal(id)) = self.chat.focus.clone() else {
            return false;
        };
        match key.code {
            KeyCode::Up => self.chat_action(ChatAction::ScrollTerminal(id, -1)),
            KeyCode::Down => self.chat_action(ChatAction::ScrollTerminal(id, 1)),
            KeyCode::PageUp => self.scroll_chat_terminal(&id, -(TERMINAL_ROWS as isize)),
            KeyCode::PageDown => self.scroll_chat_terminal(&id, TERMINAL_ROWS as isize),
            KeyCode::Left => self.chat_action(ChatAction::PanTerminal(id, -4)),
            KeyCode::Right => self.chat_action(ChatAction::PanTerminal(id, 4)),
            KeyCode::Home => self.chat_action(ChatAction::TerminalTop(id)),
            KeyCode::End => self.chat_action(ChatAction::FollowTerminal(id)),
            KeyCode::Esc => self.chat.focus = None,
            KeyCode::Tab | KeyCode::BackTab => return false,
            _ if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                return false;
            }
            _ => {} // Read-only viewport: typing never enters the process or draft.
        }
        true
    }
}
