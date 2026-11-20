//! Native Vim emulation for the code editor.
//!
//! The engine is split into a pure command grammar ([`parse`]), Vim-exact
//! motions and text objects ([`motion`], [`textobj`]), an executor that edits
//! an [`Editor`] through one bookkeeping-aware primitive ([`exec`]), Insert
//! mode ([`insert`]), the command line and Ex commands ([`cmdline`], [`ex`]),
//! and key mappings ([`map`]). The workbench talks to it through [`Host`].

pub(crate) mod keys;
pub(crate) mod text;
pub(crate) mod motion;
pub(crate) mod textobj;
pub(crate) mod parse;
pub(crate) mod register;
pub(crate) mod search;
pub(crate) mod map;
pub(crate) mod exec;
pub(crate) mod ops;
pub(crate) mod insert;
pub(crate) mod cmdline;
pub(crate) mod ex;
pub(crate) mod app_host;
pub(crate) mod ui;
pub(crate) mod feedback;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod parity_tests;
#[cfg(test)]
mod app_tests;

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use crate::editor::Editor;
pub(crate) use keys::Key;
use keys::K;
use parse::{Ctx, Parsed};
pub(crate) use text::Pos;

/// Visual-block area for rendering and block operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockArea {
    pub(crate) top: usize,
    pub(crate) bottom: usize,
    /// Display columns, inclusive start and exclusive end. `usize::MAX`
    /// end means "to the end of every line" (`$`).
    pub(crate) left: usize,
    pub(crate) right: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisualKind { Char, Line, Block }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastVisual {
    pub(crate) kind: VisualKind,
    pub(crate) start: Pos,
    pub(crate) end: Pos,
    pub(crate) curswant_eol: bool,
}

/// Per-buffer state stored on each editor view.
#[derive(Clone, Debug, Default)]
pub(crate) struct BufState {
    /// `a`-`z` plus `[ ] < > . ^ "` marks.
    pub(crate) marks: BTreeMap<char, Pos>,
    pub(crate) last_visual: Option<LastVisual>,
    pub(crate) changes: Vec<Pos>,
    /// Position in `changes` for `g;`/`g,` (len = past the newest).
    pub(crate) change_idx: usize,
    /// `U`: the line and its text before the latest run of changes on it.
    pub(crate) line_undo: Option<(usize, String)>,
    /// Lines still to visit for `:global` and `:normal` with a range.
    pub(crate) global_lines: Vec<usize>,
    /// Incremented whenever line shifts are applied.
    pub(crate) shift_rev: u64,
    /// `:e newfile` for a file that does not exist yet.
    pub(crate) intended_path: Option<std::path::PathBuf>,
    /// The buffer ends with an empty line Vim keeps (see [`text::Text::of`]).
    pub(crate) trailing_empty: bool,
    /// All lines were deleted (Vim's ML_EMPTY): `dd` has nothing to delete.
    pub(crate) ml_empty: bool,
}

/// Keep [`BufState::trailing_empty`] in step with an edit: a buffer whose
/// last line had no line break and now ends with one has an empty last line.
pub(crate) fn update_trailing(ed: &mut Editor, had_eol: bool) {
    let has_eol = ed.rope.len_chars() > 0 && matches!(ed.rope.char(ed.rope.len_chars() - 1), '\n' | '\r');
    if !has_eol {
        ed.vim_buf.trailing_empty = false;
    } else if !had_eol {
        ed.vim_buf.trailing_empty = true;
    }
}

pub(crate) fn ends_with_eol(rope: &ropey::Rope) -> bool {
    rope.len_chars() > 0 && matches!(rope.char(rope.len_chars() - 1), '\n' | '\r')
}

impl BufState {
    /// Keep marks on the same text across a line insertion/deletion.
    pub(crate) fn shift_lines(&mut self, from_line: usize, delta: isize) {
        let fix = |p: &mut Pos| -> bool {
            if p.line < from_line { return true; }
            if delta < 0 {
                let removed = (-delta) as usize;
                if p.line < from_line + removed { return false; }
                p.line -= removed;
            } else {
                p.line += delta as usize;
            }
            true
        };
        self.shift_rev += 1;
        let mut gone = Vec::new();
        for (c, p) in self.marks.iter_mut() {
            if !fix(p) {
                // Lowercase marks on deleted lines are deleted, as in Vim.
                if c.is_ascii_lowercase() { gone.push(*c); } else { p.line = from_line.saturating_sub(1); }
            }
        }
        for c in gone { self.marks.remove(&c); }
        let mut kept = Vec::with_capacity(self.global_lines.len());
        for &l in &self.global_lines {
            let mut p = Pos::new(l, 0);
            if fix(&mut p) { kept.push(p.line); }
        }
        self.global_lines = kept;
        for p in &mut self.changes {
            if !fix(p) { p.line = from_line; }
        }
        if let Some(v) = &mut self.last_visual {
            if !fix(&mut v.start) { v.start.line = from_line; }
            if !fix(&mut v.end) { v.end.line = from_line; }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Insert,
    Replace,
    Visual(VisualKind),
    Cmdline,
}

#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub(crate) ignorecase: bool,
    pub(crate) smartcase: bool,
    pub(crate) hlsearch: bool,
    pub(crate) incsearch: bool,
    pub(crate) wrapscan: bool,
    pub(crate) magic: bool,
    pub(crate) gdefault: bool,
    pub(crate) timeout: bool,
    pub(crate) timeoutlen: u64,
    pub(crate) ttimeoutlen: u64,
    pub(crate) clipboard_unnamed: bool,
    pub(crate) startofline: bool,
    pub(crate) joinspaces: bool,
    pub(crate) tildeop: bool,
    pub(crate) whichwrap: String,
    pub(crate) iskeyword: text::Keyword,
    pub(crate) shiftround: bool,
    pub(crate) autoindent: bool,
    pub(crate) textwidth: usize,
    pub(crate) nrformats_hex: bool,
    pub(crate) nrformats_bin: bool,
    pub(crate) nrformats_octal: bool,
    pub(crate) nrformats_alpha: bool,
    pub(crate) showcmd: bool,
    /// Show what can follow a prefix after a short pause (`:set nowhichkey`).
    pub(crate) which_key: bool,
    pub(crate) showmode: bool,
    pub(crate) scrolloff: Option<usize>,
    pub(crate) sidescroll: usize,
    pub(crate) report: usize,
    pub(crate) undolevels: usize,
    pub(crate) splitright: bool,
    pub(crate) splitbelow: bool,
    pub(crate) wildignorecase: bool,
    /// Vim owns Ctrl key combinations (`<C-d>`, `<C-v>`, `<C-w>`...).
    pub(crate) ctrl_keys: bool,
    /// Insert-mode `<C-o>`-style leaders are not configured here.
    pub(crate) highlight_yank: bool,
}

impl Default for Options {
    fn default() -> Self {
        // Neovim's defaults: they are what most Vim users configure anyway.
        Self {
            ignorecase: false,
            smartcase: false,
            hlsearch: true,
            incsearch: true,
            wrapscan: true,
            magic: true,
            gdefault: false,
            timeout: true,
            timeoutlen: 1000,
            ttimeoutlen: 50,
            clipboard_unnamed: false,
            startofline: true,
            joinspaces: false,
            tildeop: false,
            whichwrap: "b,s".into(),
            iskeyword: text::Keyword::default(),
            shiftround: false,
            autoindent: true,
            textwidth: 0,
            nrformats_hex: true,
            nrformats_bin: true,
            nrformats_octal: false,
            nrformats_alpha: false,
            showcmd: true,
            which_key: true,
            showmode: true,
            scrolloff: None,
            sidescroll: 0,
            report: 2,
            undolevels: 1000,
            splitright: false,
            splitbelow: false,
            wildignorecase: false,
            ctrl_keys: true,
            highlight_yank: true,
        }
    }
}

/// Quickfix-style commands, which act on the Problems panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quickfix { Open, Close, Next, Prev }

/// App-level actions requested by the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// A workbench command id, as used by the Command Palette.
    Command(String),
    Write { path: Option<String>, force: bool },
    WriteAll,
    Quit { force: bool },
    QuitAll { force: bool },
    WriteQuitAll,
    /// `:wq`/`:x`: save the current buffer and close its window.
    WriteQuit { force: bool },
    Edit { path: Option<String>, force: bool },
    NewBuffer,
    Window(parse::Window, usize),
    TabNext(Option<usize>),
    TabPrev(usize),
    TabClose { force: bool },
    TabOnly,
    BufferNext(usize),
    BufferPrev(usize),
    Buffer(String),
    BufferDelete { force: bool },
    Alternate,
    JumpTo { nav_id: u64, path: Option<std::path::PathBuf>, pos: Pos },
    Explore,
    /// `:copen`, `:cclose`, `:cnext`, `:cprevious` on the Problems panel.
    Quickfix(Quickfix),
    Terminal(Option<String>),
    SetOption { name: String, value: String },
    Cd(Option<String>),
    /// Highlight a yanked range briefly.
    YankFlash { start: Pos, end: Pos, linewise: bool },
}

/// What the engine needs from the workbench.
pub(crate) trait Host {
    fn ed(&mut self) -> Option<&mut Editor>;
    fn ed_ref(&self) -> Option<&Editor>;
    /// Run the workbench's own handling for a key: Insert-mode typing with
    /// completion, snippets and auto-pairs, or an unclaimed shortcut.
    fn native_key(&mut self, key: Key);
    /// `o`/`O`: open a line below/above the cursor and put the cursor on
    /// it. The default copies the current line's indent ('autoindent').
    fn open_line(&mut self, above: bool, autoindent: bool) {
        let Some(ed) = self.ed() else { return; };
        let t = text::Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let line = ed.cursor_line.min(t.last_line());
        let indent = if autoindent { indent_like(ed, &t, line) } else { String::new() };
        let mut tx = exec::Tx::default();
        if above {
            let at = t.line_start(line);
            tx.edit(ed, at, at, &format!("{indent}\n"));
            tx.finish(ed);
            ed.cursor_line = line;
        } else {
            let at = t.line_end(line);
            tx.edit(ed, at, at, &format!("\n{indent}"));
            tx.finish(ed);
            ed.cursor_line = line + 1;
        }
        ed.cursor_col = indent.chars().count();
    }
    fn clip_get(&mut self) -> Option<String>;
    fn clip_set(&mut self, text: &str, linewise: bool);
    fn act(&mut self, act: Act) -> Result<(), String>;
    fn file_name(&self) -> Option<String>;
    fn cwd(&self) -> Option<std::path::PathBuf> { None }
    /// Visible popups (completion) that should receive Insert-mode keys.
    fn completion_visible(&self) -> bool { false }
    /// Names for `:b` completion and listing: (index, name, modified).
    fn buffers(&self) -> Vec<(usize, String, bool)> { Vec::new() }
    fn commands(&self) -> Vec<String> { Vec::new() }
}

/// 'autoindent': the indent of `line` as tabs/spaces per 'expandtab'.
pub(crate) fn indent_like(ed: &Editor, t: &text::Text<'_>, line: usize) -> String {
    let chars = t.line_chars(line);
    let ws = chars.iter().take_while(|&&c| c == ' ' || c == '\t').count();
    let ts = ed.tab_size.max(1);
    let width = t.vcol(line, ws, ts);
    if ed.use_tabs { format!("{}{}", "\t".repeat(width / ts), " ".repeat(width % ts)) } else { " ".repeat(width) }
}

pub(crate) struct HostClip<'a>(pub(crate) &'a mut dyn Host);

impl register::Clipboard for HostClip<'_> {
    fn get(&mut self) -> Option<String> { self.0.clip_get() }
    fn set(&mut self, text: &str, linewise: bool) { self.0.clip_set(text, linewise) }
}

#[derive(Clone, Copy, Debug)]
struct Typed {
    key: Key,
    remap: bool,
    /// Keys from a macro or mapping that must be dropped on an error.
    from_macro: bool,
}

/// A recorded change for `.`.
#[derive(Clone, Debug)]
pub(crate) struct Change {
    pub(crate) cmd: parse::Cmd,
    /// Visual operators replay on an area of the same size.
    pub(crate) visual: Option<(VisualKind, usize, usize, bool)>,
    /// The search pattern for `d/pat<CR>`.
    pub(crate) search: Option<(String, bool)>,
    pub(crate) insert: Option<insert::Inserted>,
}

#[derive(Clone, Debug)]
pub(crate) struct Jump {
    pub(crate) nav_id: u64,
    pub(crate) path: Option<std::path::PathBuf>,
    pub(crate) pos: Pos,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Message {
    pub(crate) text: String,
    pub(crate) error: bool,
    /// Multi-line output (`:reg`, `:marks`, `:ls`) waits for a key.
    pub(crate) lines: Vec<String>,
}

pub(crate) struct VimState {
    pub(crate) enabled: bool,
    pub(crate) mode: Mode,
    pub(crate) opts: Options,
    pub(crate) pending: Vec<Key>,
    typeahead: VecDeque<Typed>,
    map_wait_since: Option<Instant>,
    /// When a key that is not a command was typed (the badge flashes).
    pub(crate) bell: Option<Instant>,
    pub(crate) maps: map::Maps,
    pub(crate) leader: Vec<Key>,
    pub(crate) regs: register::Registers,
    pub(crate) visual_anchor: Pos,
    pub(crate) visual_kind: VisualKind,
    /// Block selections extended with `$`.
    pub(crate) curswant_eol: bool,
    /// Desired display column for vertical motions.
    pub(crate) curswant: Option<usize>,
    pub(crate) last_find: Option<(char, bool, bool)>,
    pub(crate) last_search: Option<(String, bool)>,
    /// `:nohlsearch` hides the highlight until the next search.
    pub(crate) hl_hidden: bool,
    pub(crate) last_change: Option<Change>,
    pub(crate) recording: Option<(char, Vec<Key>)>,
    pub(crate) last_macro: Option<char>,
    pub(crate) insert: Option<insert::Session>,
    pub(crate) replace_stack: Vec<Option<char>>,
    /// `<C-o>` from Insert mode: run one command, then resume.
    pub(crate) insert_oneshot: Option<insert::Resume>,
    pub(crate) cmdline: Option<cmdline::CmdLine>,
    pub(crate) history_cmd: Vec<String>,
    pub(crate) history_search: Vec<String>,
    pub(crate) last_sub: Option<ex::Substitute>,
    pub(crate) last_ex: Option<String>,
    pub(crate) jumps: Vec<Jump>,
    pub(crate) jump_idx: usize,
    pub(crate) global_marks: BTreeMap<char, Jump>,
    pub(crate) message: Option<Message>,
    /// Running `.`, a macro or `:normal`: do not record changes.
    pub(crate) replaying: u32,
    pub(crate) key_budget: usize,
    /// Nesting of `:normal` (Vim's E192 limit).
    pub(crate) normal_depth: u32,
    /// Buffer text for searching, per (view, content version).
    pub(crate) hay_cache: std::cell::RefCell<Option<(u64, u64, std::sync::Arc<String>)>>,
    /// Compiled patterns by (pattern, ignorecase, smartcase, magic).
    pub(crate) re_cache: std::cell::RefCell<Option<((String, bool, bool, bool), search::VimRegex)>>,
    /// Pending confirmation for `:s///c`.
    pub(crate) confirm: Option<ex::Confirm>,
    pub(crate) last_nav_id: u64,
    pub(crate) ex_abort: bool,
    pub(crate) yank_flash: Option<(Instant, u64, (usize, usize, usize, usize), bool)>,
    pub(crate) error_in_replay: bool,
    /// Mode to return to after a command line opened from Visual mode.
    pub(crate) cmdline_visual: Option<Mode>,
    /// The pattern being typed for 'incsearch' highlighting.
    pub(crate) incsearch: Option<String>,
    pub(crate) last_offset: cmdline::Offset,
    /// The 'scroll' value set by a count to `<C-d>`/`<C-u>`.
    pub(crate) scroll_lines: usize,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: Mode::Normal,
            opts: Options::default(),
            pending: Vec::new(),
            typeahead: VecDeque::new(),
            map_wait_since: None,
            bell: None,
            maps: map::Maps::default(),
            leader: vec![Key::ch('\\')],
            regs: register::Registers::default(),
            visual_anchor: Pos::default(),
            visual_kind: VisualKind::Char,
            curswant_eol: false,
            curswant: None,
            last_find: None,
            last_search: None,
            hl_hidden: false,
            last_change: None,
            recording: None,
            last_macro: None,
            insert: None,
            replace_stack: Vec::new(),
            insert_oneshot: None,
            cmdline: None,
            history_cmd: Vec::new(),
            history_search: Vec::new(),
            last_sub: None,
            last_ex: None,
            jumps: Vec::new(),
            jump_idx: 0,
            global_marks: BTreeMap::new(),
            message: None,
            replaying: 0,
            key_budget: 0,
            normal_depth: 0,
            hay_cache: Default::default(),
            re_cache: Default::default(),
            confirm: None,
            last_nav_id: 0,
            ex_abort: false,
            yank_flash: None,
            error_in_replay: false,
            cmdline_visual: None,
            incsearch: None,
            last_offset: cmdline::Offset::None,
            scroll_lines: 0,
        }
    }
}

/// Keys that one user keystroke may expand to before Vim gives up
/// (recursive mappings or macros).
const KEY_BUDGET: usize = 200_000;

impl VimState {
    pub(crate) fn is_insert(&self) -> bool { matches!(self.mode, Mode::Insert | Mode::Replace) }
    pub(crate) fn is_visual(&self) -> bool { matches!(self.mode, Mode::Visual(_)) }

    pub(crate) fn set_message(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), error: false, lines: Vec::new() });
    }
    pub(crate) fn set_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        // Hosts report informational answers (`:set nu?`) through errors
        // prefixed with U+0001.
        if let Some(info) = text.strip_prefix('\u{1}') {
            self.set_message(info.to_string());
            return;
        }
        self.message = Some(Message { text, error: true, lines: Vec::new() });
        if self.replaying > 0 { self.error_in_replay = true; }
        self.abort_macro();
    }
    pub(crate) fn set_lines(&mut self, lines: Vec<String>) {
        self.message = Some(Message { text: String::new(), error: false, lines });
    }

    /// A failed command: Vim beeps and flushes typeahead that was not
    /// typed by the user (macro, mapping and `:normal` keys).
    pub(crate) fn beep(&mut self) {
        self.pending.clear();
        self.abort_macro();
    }

    /// Drop keys queued by a macro after an error, as Vim does.
    fn abort_macro(&mut self) {
        self.typeahead.retain(|t| !t.from_macro);
    }

    /// Mapping mode letter for the current state.
    fn map_mode(&self) -> map::MapMode {
        match self.mode {
            Mode::Insert | Mode::Replace => map::MapMode::Insert,
            Mode::Cmdline => map::MapMode::Cmdline,
            Mode::Visual(_) => map::MapMode::Visual,
            Mode::Normal => {
                let ctx = Ctx::Normal;
                if !self.pending.is_empty() && op_pending(&self.pending, ctx) {
                    map::MapMode::OpPending
                } else {
                    map::MapMode::Normal
                }
            }
        }
    }

    /// Whether the next key is the argument of `f`, `r`, `m`, `"`... which
    /// Vim never remaps.
    fn expects_char_arg(&self) -> bool {
        match self.mode {
            Mode::Normal | Mode::Visual(_) => {
                if self.pending.is_empty() { return false; }
                let ctx = if self.is_visual() { Ctx::Visual } else { Ctx::Normal };
                let mut probe = self.pending.clone();
                probe.push(Key::ch('\u{E000}'));
                parse::parse(&probe, ctx) != Parsed::Bad
            }
            Mode::Insert | Mode::Replace => self.insert.as_ref().is_some_and(|s| s.awaiting.is_some()),
            Mode::Cmdline => self.cmdline.as_ref().is_some_and(|c| c.awaiting_register),
        }
    }

    /// Feed one key typed by the user.
    pub(crate) fn feed(&mut self, host: &mut dyn Host, key: Key) {
        self.key_budget = KEY_BUDGET;
        if self.message.as_ref().is_some_and(|m| !m.lines.is_empty()) {
            // "Press ENTER" after multi-line output: the key is consumed
            // unless it starts a command (Vim's `:` behaviour).
            self.message = None;
            if key == Key::ENTER || key == Key::ch(' ') || key.is_escape() {
                return;
            }
        }
        if let Some((_, keys)) = &mut self.recording {
            keys.push(key);
        }
        self.typeahead.push_back(Typed { key, remap: true, from_macro: false });
        self.drain(host, false);
    }

    /// Queue keys to run next, ahead of anything typed (a macro). The loop
    /// already draining runs them, so a macro that plays itself (`qa...@aq`)
    /// loops in that one loop instead of nesting a call per round, which
    /// would overflow the stack. A runaway one stops at the key budget.
    pub(crate) fn queue_keys(&mut self, keys: &[Key], remap: bool, from_macro: bool) {
        if self.key_budget == 0 { self.key_budget = KEY_BUDGET; }
        for (i, &key) in keys.iter().enumerate() {
            self.typeahead.insert(i, Typed { key, remap, from_macro });
        }
    }

    /// Run keys as if typed, synchronously (`:normal`, macros, tests).
    pub(crate) fn feed_keys(&mut self, host: &mut dyn Host, keys: &[Key], remap: bool, from_macro: bool) {
        // Nested calls (`:normal` inside a macro) share the outer budget.
        if self.key_budget == 0 { self.key_budget = KEY_BUDGET; }
        for (i, &key) in keys.iter().enumerate() {
            self.typeahead.insert(i, Typed { key, remap, from_macro });
        }
        self.drain(host, false);
    }

    /// Mapping timeout: resolve an ambiguous prefix after `timeoutlen`.
    pub(crate) fn tick(&mut self, host: &mut dyn Host, now: Instant) -> bool {
        if let Some(since) = self.map_wait_since
            && self.opts.timeout
            && now.duration_since(since) >= Duration::from_millis(self.opts.timeoutlen)
        {
            self.key_budget = KEY_BUDGET;
            self.drain(host, true);
            return true;
        }
        false
    }

    /// The deadline for a pending mapping, for the event loop's poll timeout.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let since = self.map_wait_since?;
        self.opts.timeout.then(|| since + Duration::from_millis(self.opts.timeoutlen))
    }

    fn drain(&mut self, host: &mut dyn Host, timed_out: bool) {
        let mut timed_out = timed_out;
        while !self.typeahead.is_empty() {
            if self.key_budget == 0 {
                let in_macro = self.typeahead.iter().any(|t| t.from_macro);
                self.typeahead.clear();
                self.set_error(if in_macro {
                    "E223: macro stopped after 200000 keys (does it call itself with nothing to stop it?)"
                } else {
                    "E223: recursive mapping"
                });
                break;
            }
            self.key_budget -= 1;
            let first = self.typeahead[0];
            if first.remap && !self.expects_char_arg() {
                let mode = self.map_mode();
                let seq: Vec<Key> = self.typeahead.iter().take_while(|t| t.remap).map(|t| t.key).collect();
                match self.maps.lookup(mode, &seq) {
                    map::Lookup::Prefix if !timed_out => {
                        self.map_wait_since.get_or_insert_with(Instant::now);
                        return;
                    }
                    map::Lookup::Prefix => {
                        // Timed out: use the longest complete mapping, if any.
                        timed_out = false;
                        self.map_wait_since = None;
                        if let Some((len, rhs, noremap)) = self.maps.longest(mode, &seq) {
                            self.apply_mapping(len, rhs, noremap);
                            continue;
                        }
                    }
                    map::Lookup::Match { len, rhs, noremap } => {
                        self.map_wait_since = None;
                        self.apply_mapping(len, rhs, noremap);
                        continue;
                    }
                    map::Lookup::None => {}
                }
            }
            self.map_wait_since = None;
            let t = self.typeahead.pop_front().unwrap();
            // Keys a macro replays do not record changes for `.`.
            if t.from_macro { self.replaying += 1; }
            self.process(host, t.key);
            if t.from_macro { self.replaying -= 1; }
            if self.error_in_replay {
                self.error_in_replay = false;
                self.abort_macro();
            }
        }
    }

    fn apply_mapping(&mut self, len: usize, rhs: Vec<Key>, noremap: bool) {
        let from_macro = self.typeahead.iter().take(len).any(|t| t.from_macro);
        for _ in 0..len { self.typeahead.pop_front(); }
        for (i, key) in rhs.into_iter().enumerate() {
            self.typeahead.insert(i, Typed { key, remap: !noremap, from_macro });
        }
    }

    /// Process one key after mappings.
    pub(crate) fn process(&mut self, host: &mut dyn Host, key: Key) {
        if self.confirm.is_some() {
            ex::confirm_key(self, host, key);
            return;
        }
        // A `<C-o>` one-shot waiting before this key (also through `:`).
        let armed = self.insert_oneshot.is_some()
            || self.cmdline.as_ref().is_some_and(|c| c.resume_insert.is_some());
        let before = host.ed_ref().map(|ed| (ed.cursor_line, ed.cursor_col));
        match self.mode {
            Mode::Insert | Mode::Replace => insert::key(self, host, key),
            Mode::Cmdline => cmdline::key(self, host, key),
            Mode::Normal | Mode::Visual(_) => self.normal_key(host, key),
        }
        // `<C-o>`: once the one Normal-mode command is complete, resume Insert mode.
        if armed && self.mode == Mode::Normal && self.pending.is_empty() && self.cmdline.is_none()
            && let Some(r) = self.insert_oneshot.take()
        {
            insert::resume(self, host, r, before);
        }
    }

    fn normal_key(&mut self, host: &mut dyn Host, key: Key) {
        if self.message.as_ref().is_some_and(|m| !m.error) && self.pending.is_empty() {
            self.message = None;
        }
        let ctx = if self.is_visual() { Ctx::Visual } else { Ctx::Normal };
        if self.pending.is_empty() && key == Key::ch('q') && self.recording.is_some() {
            // Stop recording: the register gets the typed keys without `q`.
            let (r, mut keys) = self.recording.take().unwrap();
            keys.pop();
            let text = ops::keys_to_text(&keys);
            let mut clip = HostClip(host);
            self.regs.set(r, register::Reg::new(text, register::RegKind::Char), &mut clip);
            return;
        }
        self.pending.push(key);
        match parse::parse(&self.pending, ctx) {
            Parsed::Need => {}
            Parsed::Bad => {
                let keys = std::mem::take(&mut self.pending);
                // A lone unknown key: let the workbench handle it (F-keys,
                // Alt shortcuts, Ctrl keys Vim does not use).
                if keys.len() == 1 && unclaimed(keys[0], &self.opts) {
                    host.native_key(keys[0]);
                    exec::after_native(self, host);
                } else if !(keys.len() == 1 && keys[0].is_escape()) {
                    self.beep();
                    // Say so: silently dropping keys leaves the user guessing.
                    self.bell = Some(Instant::now());
                    let typed = keys::showcmd(&keys);
                    self.set_message(if keys[0].is_ctrl('w') {
                        format!("{typed}: not a window command (^W then h j k l, s, v, c, o...)")
                    } else {
                        format!("{typed}: not a command")
                    });
                }
            }
            Parsed::Done(cmd) => {
                self.pending.clear();
                exec::run(self, host, cmd);
            }
        }
    }

    /// Short mode name for the status bar.
    pub(crate) fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::Normal if self.op_pending() => "O-PENDING",
            Mode::Normal if self.insert_oneshot.is_some() => "(INSERT)",
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Replace => "REPLACE",
            Mode::Visual(VisualKind::Char) => "VISUAL",
            Mode::Visual(VisualKind::Line) => "V-LINE",
            Mode::Visual(VisualKind::Block) => "V-BLOCK",
            Mode::Cmdline => "COMMAND",
        }
    }

    pub(crate) fn op_pending(&self) -> bool {
        self.mode == Mode::Normal && !self.pending.is_empty() && op_pending(&self.pending, Ctx::Normal)
    }

    pub(crate) fn showcmd(&self) -> String {
        let mut s = keys::showcmd(&self.pending);
        if self.map_wait_since.is_some() {
            let waiting: Vec<Key> = self.typeahead.iter().map(|t| t.key).collect();
            s.push_str(&keys::showcmd(&waiting));
        }
        if self.is_visual() && self.pending.is_empty() {
            return String::new();
        }
        s
    }

    pub(crate) fn recording_label(&self) -> Option<String> {
        self.recording.as_ref().map(|(r, _)| format!("recording @{r}"))
    }

    /// Leave any mode for Normal mode without side effects on the buffer
    /// (focus changes, file switches).
    pub(crate) fn reset_to_normal(&mut self, host: &mut dyn Host) {
        if self.is_insert() {
            insert::finish(self, host, false);
        }
        if self.is_visual() {
            exec::exit_visual(self, host);
        }
        if self.mode == Mode::Cmdline {
            cmdline::cancel(self, host);
        }
        self.pending.clear();
        self.insert_oneshot = None;
        self.mode = Mode::Normal;
        if let Some(ed) = host.ed() {
            exec::clamp_normal(ed);
        }
    }

    /// The buffer as one string, cached until the text changes.
    pub(crate) fn haystack(&self, ed: &Editor) -> std::sync::Arc<String> {
        let key = (ed.navigation_id, ed.content_version);
        if let Some((id, v, s)) = self.hay_cache.borrow().as_ref()
            && (*id, *v) == key
            && s.len() == ed.rope.len_bytes()
        {
            return s.clone();
        }
        let s = std::sync::Arc::new(ed.rope.to_string());
        *self.hay_cache.borrow_mut() = Some((key.0, key.1, s.clone()));
        s
    }

    pub(crate) fn compile(&self, pat: &str) -> Result<search::VimRegex, String> {
        let o = self.search_options();
        let key = (pat.to_string(), o.ignorecase, o.smartcase, o.magic);
        if let Some((k, re)) = self.re_cache.borrow().as_ref()
            && *k == key
            && !pat.contains('~')
        {
            return Ok(re.clone());
        }
        let re = search::compile(pat, &o)?;
        *self.re_cache.borrow_mut() = Some((key, re.clone()));
        Ok(re)
    }

    pub(crate) fn search_options(&self) -> search::Options<'_> {
        search::Options {
            ignorecase: self.opts.ignorecase,
            smartcase: self.opts.smartcase,
            magic: self.opts.magic,
            last_sub: self.last_sub.as_ref().map(|s| s.replacement.as_str()),
        }
    }
}

/// Keys Vim leaves to the workbench in Normal mode.
pub(crate) fn unclaimed(key: Key, opts: &Options) -> bool {
    if key.alt { return true; }
    if matches!(key.k, K::F(_)) { return true; }
    if key.ctrl {
        if !opts.ctrl_keys || key.shift { return true; }
        // Ctrl keys that Vim does not define in Normal mode.
        return match key.k {
            K::Char(c) => matches!(c, 'k' | 's' | 'z' | 'p' | 'n' | 'q' | '\\' | '/' | '.' | '`' | ',' | '=' | '-' | '0'..='9' | ' ' | '\''),
            _ => true,
        };
    }
    false
}

fn op_pending(pending: &[Key], ctx: Ctx) -> bool {
    // An operator is pending when the keys parse as an incomplete command
    // whose first non-count/register part is an operator.
    let mut i = 0;
    while i < pending.len() {
        match pending[i].char() {
            Some('"') => i += 2,
            Some(c) if c.is_ascii_digit() && c != '0' => {
                i += 1;
                while pending.get(i).and_then(|k| k.char()).is_some_and(|c| c.is_ascii_digit()) { i += 1; }
            }
            _ => break,
        }
    }
    if ctx == Ctx::Visual { return false; }
    let rest = &pending[i.min(pending.len())..];
    let first = rest.first().and_then(|k| k.char());
    let second = rest.get(1).and_then(|k| k.char());
    match first {
        Some('d' | 'c' | 'y' | '>' | '<' | '=' | '!') => true,
        Some('g') => matches!(second, Some('~' | 'u' | 'U' | '?' | 'q' | 'w' | 'c')),
        Some('z') => second == Some('f'),
        _ => false,
    }
}
