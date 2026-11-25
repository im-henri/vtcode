//! `g-` and `g+`: move through text states in the order they were made,
//! as in Vim's undo tree. The editor's undo is linear, so a change made
//! after `u` would lose the undone branch; here every state stays
//! reachable in time order.

use ropey::Rope;

use super::exec::{Tx, set_cursor};
use super::text::{Pos, Text};
use crate::editor::Editor;

/// States are capped; the oldest go first.
const MAX_STATES: usize = 2000;

#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    /// (undo change id, text after it); the first entry is the text
    /// before the first change (id `u64::MAX`).
    states: Vec<(u64, Rope)>,
    /// The undo step made by the last `g-`/`g+` itself: not a new state.
    travel_id: Option<u64>,
}

/// Note the current text after each key (cheap: ropes share their nodes).
pub(crate) fn record(ed: &mut Editor) {
    let Some((id, before)) = ed.undo_stack.back().map(|s| (s.change_id, s.rope.clone())) else {
        return;
    };
    let h = &mut ed.vim_buf.history;
    if h.travel_id == Some(id) {
        return;
    }
    if h.states.is_empty() {
        h.states.push((u64::MAX, before));
    }
    match h.states.last_mut() {
        // Still the newest change (typing goes on): keep its latest text.
        Some((last, rope)) if *last == id => *rope = ed.rope.clone(),
        _ => {
            if h.states.iter().any(|(i, _)| *i == id) {
                // An older change on top after `u`: not a new state.
                return;
            }
            h.states.push((id, ed.rope.clone()));
            if h.states.len() > MAX_STATES {
                h.states.remove(0);
            }
        }
    }
}

/// `g-` (`newer` false) or `g+`, `count` times. Returns false at the end.
pub(crate) fn travel(ed: &mut Editor, newer: bool, count: usize) -> bool {
    record(ed);
    let h = &ed.vim_buf.history;
    if h.states.len() < 2 {
        return false;
    }
    let len = ed.rope.len_chars();
    let current = h
        .states
        .iter()
        .rposition(|(_, r)| r.len_chars() == len && *r == ed.rope)
        .unwrap_or(h.states.len() - 1);
    let target = if newer {
        (current + count).min(h.states.len() - 1)
    } else {
        current.saturating_sub(count)
    };
    if target == current {
        return false;
    }
    let text = h.states[target].1.clone();
    let first_diff = first_changed_line(&ed.rope, &text);
    let mut tx = Tx::default();
    let end = ed.rope.len_chars();
    tx.edit(ed, 0, end, &text.to_string());
    tx.finish(ed);
    ed.vim_buf.history.travel_id = ed.undo_stack.back().map(|s| s.change_id);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let line = first_diff.min(t.last_line());
    set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
    true
}

fn first_changed_line(a: &Rope, b: &Rope) -> usize {
    let n = a.len_lines().min(b.len_lines());
    (0..n)
        .find(|&i| a.line(i) != b.line(i))
        .unwrap_or(n.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use crate::vim::tests::run;

    #[test]
    fn g_minus_reaches_the_branch_a_new_change_replaced() {
        // one -> two, undo, -> three: g- goes back to two, as in Vim.
        let o = run("one", (0, 0), "ciwtwo<Esc>uciwthree<Esc>g-");
        assert_eq!(o.lines, ["two"]);
        let o = run("one", (0, 0), "ciwtwo<Esc>uciwthree<Esc>g-g-");
        assert_eq!(o.lines, ["one"]);
        let o = run("one", (0, 0), "ciwtwo<Esc>uciwthree<Esc>g-g-g+g+");
        assert_eq!(o.lines, ["three"]);
        let o = run("one", (0, 0), "ciwtwo<Esc>uciwthree<Esc>2g-");
        assert_eq!(o.lines, ["one"]);
    }
}
