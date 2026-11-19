//! Engine tests driven through a minimal host that types like plain Vim
//! (no auto-pairs or completion).

use super::exec::Tx;
use super::keys::{self, Key, K};
use super::text::{Pos, Text};
use super::{Act, Host, Mode, VimState};
use crate::editor::Editor;

pub(crate) struct TestHost {
    pub(crate) ed: Editor,
    pub(crate) clip: Option<String>,
    pub(crate) acts: Vec<Act>,
}

impl TestHost {
    pub(crate) fn new(text: &str) -> Self {
        let mut ed = Editor::test_with_text(text);
        // The parity harness runs Vim with an 80x24 screen.
        ed.viewport_width = 80;
        ed.viewport_height = 24;
        Self { ed, clip: None, acts: Vec::new() }
    }
}

impl Host for TestHost {
    fn ed(&mut self) -> Option<&mut Editor> { Some(&mut self.ed) }
    fn ed_ref(&self) -> Option<&Editor> { Some(&self.ed) }

    fn native_key(&mut self, key: Key) {
        let ed = &mut self.ed;
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let p = t.clamp_insert(Pos::new(ed.cursor_line, ed.cursor_col));
        let at = t.idx(p);
        let mut tx = Tx::default();
        match (key.k, key.ctrl || key.alt) {
            (K::Char(c), false) => {
                tx.edit(ed, at, at, &c.to_string());
                tx.finish(ed);
                ed.cursor_col = p.col + 1;
                ed.cursor_line = p.line;
            }
            (K::Tab, false) => {
                tx.edit(ed, at, at, "\t");
                tx.finish(ed);
                ed.cursor_col = p.col + 1;
            }
            (K::Enter, _) => {
                // Plain Vim 'autoindent'.
                let ind = super::indent_like(ed, &t, p.line);
                let chars = t.line_chars(p.line);
                let blank_line = chars.iter().all(|&c| c == ' ' || c == '\t');
                let lead = chars[p.col.min(chars.len())..].iter().take_while(|&&c| c == ' ' || c == '\t').count();
                // A line left with only its indent loses it.
                let start = if blank_line && p.col == t.len(p.line) && !chars.is_empty() { t.line_start(p.line) } else { at };
                tx.edit(ed, start, at + lead, &format!("\n{ind}"));
                tx.finish(ed);
                ed.cursor_line = p.line + 1;
                ed.cursor_col = ind.chars().count();
            }
            (K::Backspace, false) => {
                if at > 0 {
                    let prev = t.pos(at - 1);
                    let start = if p.col == 0 { t.line_end(p.line - 1) } else { at - 1 };
                    let np = if p.col == 0 { Pos::new(p.line - 1, t.len(p.line - 1)) } else { prev };
                    tx.edit(ed, start, at, "");
                    tx.finish(ed);
                    ed.cursor_line = np.line;
                    ed.cursor_col = np.col;
                }
            }
            (K::Delete, false) => {
                if at < ed.rope.len_chars() {
                    let end = if t.char_at(p).is_some() { at + 1 } else { t.line_next_start(p.line) };
                    tx.edit(ed, at, end, "");
                    tx.finish(ed);
                }
            }
            (K::Left, false) => ed.cursor_col = p.col.saturating_sub(1),
            (K::Right, false) => ed.cursor_col = (p.col + 1).min(t.len(p.line)),
            (K::Up, false) if p.line > 0 => { ed.cursor_line -= 1; ed.cursor_col = p.col.min(t.len(p.line - 1)); }
            (K::Down, false) if p.line + 1 < t.lines() => { ed.cursor_line += 1; ed.cursor_col = p.col.min(t.len(p.line + 1)); }
            (K::Home, false) => ed.cursor_col = 0,
            (K::End, false) => ed.cursor_col = t.len(p.line),
            _ => {}
        }
    }
    fn clip_get(&mut self) -> Option<String> { self.clip.clone() }
    fn clip_set(&mut self, text: &str, _linewise: bool) { self.clip = Some(text.to_string()); }
    fn act(&mut self, act: Act) -> Result<(), String> {
        self.acts.push(act);
        Ok(())
    }
    fn file_name(&self) -> Option<String> { Some("test.txt".into()) }
}

/// Vim options matching the parity harness (`scripts/vim_parity.py`).
pub(crate) fn parity_vim() -> VimState {
    let mut vim = VimState::default();
    vim.enabled = true;
    vim.opts.highlight_yank = false;
    vim
}

/// Run keys (Vim notation) on `text` with the cursor at `cursor`.
pub(crate) fn run_keys(vim: &mut VimState, host: &mut TestHost, keys_text: &str) {
    for key in keys::parse(keys_text, &vim.leader.clone()) {
        vim.feed(host, key);
    }
}

pub(crate) struct Outcome {
    pub(crate) lines: Vec<String>,
    pub(crate) cursor: (usize, usize),
    pub(crate) mode: Mode,
    pub(crate) host: TestHost,
    pub(crate) vim: VimState,
}

pub(crate) fn vim_lines(ed: &Editor) -> Vec<String> {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    (0..t.lines()).map(|l| t.line_string(l)).collect()
}

pub(crate) fn run(text: &str, cursor: (usize, usize), keys_text: &str) -> Outcome {
    let mut host = TestHost::new(text);
    host.ed.cursor_line = cursor.0;
    host.ed.cursor_col = cursor.1;
    let mut vim = parity_vim();
    run_keys(&mut vim, &mut host, keys_text);
    let lines = vim_lines(&host.ed);
    let cursor = (host.ed.cursor_line, host.ed.cursor_col);
    let mode = vim.mode;
    Outcome { lines, cursor, mode, host, vim }
}

fn check(text: &str, cursor: (usize, usize), keys_text: &str, want: &str, want_cursor: (usize, usize)) {
    let o = run(text, cursor, keys_text);
    let want_lines: Vec<String> = want.split('\n').map(str::to_string).collect();
    assert_eq!(o.lines, want_lines, "text after {keys_text:?} on {text:?}");
    assert_eq!(o.cursor, want_cursor, "cursor after {keys_text:?} on {text:?}");
}

#[test]
fn basic_motions_and_deletes() {
    check("foo bar baz", (0, 0), "dw", "bar baz", (0, 0));
    check("foo bar baz", (0, 0), "2dw", "baz", (0, 0));
    check("foo bar baz", (0, 0), "d2w", "baz", (0, 0));
    check("foo bar baz", (0, 4), "D", "foo ", (0, 3));
    check("foo bar baz", (0, 4), "x", "foo ar baz", (0, 4));
    check("foo bar baz", (0, 10), "x", "foo bar ba", (0, 9));
    check("a\nb\nc", (1, 0), "dd", "a\nc", (1, 0));
    check("a\nb\nc", (2, 0), "dd", "a\nb", (1, 0));
    check("a\nb\nc\n", (2, 0), "dd", "a\nb", (1, 0));
    check("  a\nb", (1, 0), "kdd", "b", (0, 0));
}

#[test]
fn change_word_acts_like_change_end() {
    let o = run("foo bar", (0, 0), "cwxy<Esc>");
    assert_eq!(o.lines, ["xy bar"]);
    assert_eq!(o.cursor, (0, 1));
    assert_eq!(o.mode, Mode::Normal);
}

#[test]
fn dot_repeats_changes_with_inserted_text() {
    let o = run("foo foo foo", (0, 0), "cwbar<Esc>w.w.");
    assert_eq!(o.lines, ["bar bar bar"]);
    let o = run("a\nb\nc", (0, 0), "Ax<Esc>j.j.");
    assert_eq!(o.lines, ["ax", "bx", "cx"]);
    let o = run("x", (0, 0), "3ia<Esc>");
    assert_eq!(o.lines, ["aaax"]);
}

#[test]
fn undo_reverts_a_whole_insert_session() {
    let o = run("foo", (0, 0), "ihello world<Esc>u");
    assert_eq!(o.lines, ["foo"]);
    let o = run("foo bar", (0, 0), "cwxyz<Esc>u");
    assert_eq!(o.lines, ["foo bar"]);
    let o = run("foo bar", (0, 0), "cwxyz<Esc>u<C-r>");
    assert_eq!(o.lines, ["xyz bar"]);
}

#[test]
fn visual_modes_and_operators() {
    check("foo bar baz", (0, 4), "vey", "foo bar baz", (0, 4));
    check("foo bar baz", (0, 4), "ved", "foo  baz", (0, 4));
    check("a\nb\nc\nd", (1, 0), "Vjd", "a\nd", (1, 0));
    // Vim clamps the cursor onto the shortened line.
    check("abc\ndef\nghi", (0, 1), "<C-v>jld", "a\nd\nghi", (0, 0));
    check("abc\nabc", (0, 0), "<C-v>jIX<Esc>", "Xabc\nXabc", (0, 0));
}

#[test]
fn registers_and_put() {
    let o = run("one\ntwo", (0, 0), "\"ayyj\"ap");
    assert_eq!(o.lines, ["one", "two", "one"]);
    let o = run("one two", (0, 0), "dwP");
    assert_eq!(o.lines, ["one two"]);
    let o = run("a\nb", (0, 0), "ddp");
    assert_eq!(o.lines, ["b", "a"]);
}

#[test]
fn macros_record_and_replay() {
    let o = run("1\n2\n3\n4", (0, 0), "qaA!<Esc>jq2@a");
    assert_eq!(o.lines, ["1!", "2!", "3!", "4"]);
    let o = run("x\nx\nx\nx", (0, 0), "qqrYjq@q@@");
    assert_eq!(o.lines, ["Y", "Y", "Y", "x"]);
}

#[test]
fn ex_commands() {
    let o = run("a1\nb2\na3", (0, 0), ":%s/a/X/<CR>");
    assert_eq!(o.lines, ["X1", "b2", "X3"]);
    let o = run("a1\nb2\na3", (0, 0), ":g/a/d<CR>");
    assert_eq!(o.lines, ["b2"]);
    let o = run("c\nb\na", (0, 0), ":sort<CR>");
    assert_eq!(o.lines, ["a", "b", "c"]);
    let o = run("a\nb\nc", (0, 0), ":2,3m0<CR>");
    assert_eq!(o.lines, ["b", "c", "a"]);
    let o = run("a\nb", (0, 0), ":%norm Ax<CR>");
    assert_eq!(o.lines, ["ax", "bx"]);
    let o = run("abc", (0, 0), ":w<CR>");
    assert_eq!(o.host.acts, vec![Act::Write { path: None, force: false }]);
}

#[test]
fn mappings_with_timeout_prefixes() {
    let mut host = TestHost::new("abc");
    let mut vim = parity_vim();
    run_keys(&mut vim, &mut host, ":inoremap jk <lt>Esc><CR>");
    run_keys(&mut vim, &mut host, "ixjk");
    assert_eq!(vim.mode, Mode::Normal);
    assert_eq!(vim_lines(&host.ed), ["xabc"]);
    run_keys(&mut vim, &mut host, ":let mapleader = \" \"<CR>:nnoremap <lt>leader>d dd<CR>");
    run_keys(&mut vim, &mut host, "<Space>d");
    assert_eq!(vim_lines(&host.ed), [""]);
    // An ambiguous prefix waits, then resolves on timeout.
    let mut host = TestHost::new("abc");
    let mut vim = parity_vim();
    run_keys(&mut vim, &mut host, ":nnoremap x l<CR>:nnoremap xy dd<CR>");
    run_keys(&mut vim, &mut host, "x");
    assert_eq!(host.ed.cursor_col, 0, "waiting for a longer mapping");
    vim.tick(&mut host, std::time::Instant::now() + std::time::Duration::from_secs(5));
    assert_eq!(host.ed.cursor_col, 1);
}

#[test]
fn search_and_star() {
    check("foo bar foo", (0, 0), "/foo<CR>", "foo bar foo", (0, 8));
    check("foo bar foo", (0, 0), "*", "foo bar foo", (0, 8));
    check("foo bar foo", (0, 8), "#", "foo bar foo", (0, 0));
    check("foo bar foo", (0, 0), "d/bar<CR>", "bar foo", (0, 0));
    check("a foo b foo", (0, 0), "/foo<CR>n", "a foo b foo", (0, 8));
}

#[test]
fn text_objects_with_operators() {
    check("f(a, b)", (0, 3), "di(", "f()", (0, 2));
    check("x = \"hi there\";", (0, 6), "ci\"yo<Esc>", "x = \"yo\";", (0, 6));
    check("<a><b>x</b></a>", (0, 6), "dit", "<a><b></b></a>", (0, 6));
    check("foo bar baz", (0, 5), "daw", "foo baz", (0, 4));
    check("a\n\nb\nc\n\nd", (2, 0), "dap", "a\n\nd", (2, 0));
}

