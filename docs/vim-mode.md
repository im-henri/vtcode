# Vim mode tutorial

VTCode has a native Vim mode. It is a Vim engine built into the editor, not an
emulation layer on top of VS Code-style keys. With it on, you can do
everything without the mouse: edit, search, split windows, browse files, use
Git, read problems, run terminals and talk to the agent. The mouse keeps
working if you want it.

This tutorial has three parts:

1. [Getting started](#1-getting-started): turning Vim mode on and the first five minutes.
2. [Learning Vim in VTCode](#2-learning-vim-in-vtcode): for people new to Vim, one lesson at a time.
3. [Vim mode for Vim users](#3-vim-mode-for-vim-users): what carries over from Vim or Neovim, what VTCode adds, and how to set it up.

A [quick reference](#quick-reference) closes the page.

Key notation follows Vim's: `<Esc>` is Escape, `<CR>` is Enter, `<C-w>` is
Ctrl+W, and `<C-w>h` means press Ctrl+W, let go, then press `h`.

---

## 1. Getting started

### Turn it on

Vim mode is off by default. Turn it on in either of two ways:

- **Settings → Editor → Vim Mode**, or
- the Command Palette (`Ctrl+Shift+P`): **Vim: Toggle Vim Mode**.

The status bar now shows `NORMAL` in the bottom-left corner. That badge always
tells you which mode you are in.

Turning it off again restores VTCode's usual keys immediately; nothing else
about your files or settings changes.

### Modes in one minute

Vim is *modal*: the same key does different things depending on the mode.

| Badge | Mode | What keys do |
|---|---|---|
| `NORMAL` | Normal | Keys are commands: move, delete, copy, paste. Typing does **not** insert text. |
| `INSERT` | Insert | Keys type text, as in any editor. The cursor becomes a thin bar. |
| `VISUAL`, `V-LINE`, `V-BLOCK` | Visual | Keys extend a selection; then an operator acts on it. |
| `REPLACE` | Replace | Typing overwrites characters. The cursor becomes an underline. |

`<Esc>` always brings you back to Normal mode. If you are ever unsure, press
`<Esc>`.

### Knowing where you are

The badge at the left of the status bar always answers "where am I, and did
that key do anything?":

- **The mode or the pane.** In the editor it shows the mode (`NORMAL`,
  `INSERT`, `VISUAL`...). Anywhere else it names the pane that has the
  keyboard: `EXPLORER`, `SEARCH`, `SOURCE CONTROL`, `PROBLEMS`, `OUTPUT`,
  `TERMINAL`, `CHAT`, `SETTINGS`, `PALETTE`, `MENU`, `DIALOG`...
- **Keys waiting for more.** A prefix such as `<C-w>`, `g`, `z`, `"a` or an
  operator like `d2` shows next to the badge as soon as you type it (`^W`,
  `g`, `d2`). `O-PENDING` means an operator waits for a motion.
- **What can follow.** Pause for a moment after a prefix and a list of the
  keys that can come next opens above the status bar. It closes when the
  command completes or you press `<Esc>`. `:set nowhichkey` turns it off.
- **Keys that are not commands.** The badge flashes red and the status bar
  says so, for example `gX: not a command`, or lists the keys a list
  accepts.
- **Window moves.** After `<C-w>h/j/k/l` the pane you landed in is outlined
  briefly.
- **Searches** show which match you are on (`/foo  [3/12]`), and Visual mode
  shows the size of the selection (characters, lines, or rows x columns).
- **`(INSERT)`** means `<C-o>` is running one Normal-mode command from
  Insert mode; `^R`, `^K` or `^V` mean Insert mode waits for a register, a
  digraph or a literal key.

### Getting around

The screen has three areas you move between: the **sidebar** on the left
(Explorer, Search, Source Control, Extensions), the **editor** with its tabs
in the middle, and the **panel** at the bottom (Problems, Output, Terminal).
The docked Agent Chat, when shown, sits on the right.

**Two keys get you almost everywhere:**

- `<C-w>` then `h`, `j`, `k` or `l` moves to the area on the left, below,
  above or right, from wherever you are. `<C-w>h` from the editor goes to the
  sidebar; `<C-w>l` from the sidebar comes back.
- `:` opens the command line from any area, the sidebar and panels included,
  and even with no file open. Type a command and press `<CR>`.

**Switching what the sidebar shows.** The sidebar shows one view at a time,
and `<C-w>h` goes to whichever one it shows. To change the view, type one of
these from anywhere (or use the shortcut):

| To get to | Type | Shortcut |
|---|---|---|
| Explorer (files) | `:Explore` | `Ctrl+Shift+E` |
| Search in files | `:VT global_search` | |
| Source Control (Git) | `:VT source_control` | `Ctrl+Shift+G` |
| Extensions | `:VT extensions` | |

For example, **from Source Control to the Explorer**: type `:Explore` and
press `<CR>`. The badge changes from `SOURCE CONTROL` to `EXPLORER`. If a
text box has the cursor (the commit message, a search box), press `<Esc>`
first, or `:` is typed into it. `:VT ` followed by Tab completes command names, so you don't
have to remember them.

**Tabs.** Back in the editor (`q` or `<Esc>` leaves any sidebar or list),
`gt` and `gT` go to the next and previous tab, and `3gt` to the third. `<C-p>`
finds a file by name; `:e path` opens one.

**The bottom panel.** `:copen` opens Problems, `:terminal` opens a terminal,
and `<C-w>j` from the editor moves into whatever the panel shows. In a
terminal, keys go to the shell; `<C-\><C-n>` returns to the editor.

**Lost?** Look at the badge: it names the area that has the keyboard. Press
`<Esc>` (or `q` in a list) to get back to the editor, then start again from
there.

### Your first edit

Open any file and try this:

1. Press `i`. The badge says `INSERT`. Type `hello`.
2. Press `<Esc>`. You are back in `NORMAL`.
3. Press `u`. The word disappears: the whole insert was one undo step.
4. Press `<C-r>` to redo it.
5. Type `:w` and press `<CR>` to save. `:q` closes the tab; `:wq` saves and closes.

If a file has unsaved changes, `:q` refuses with
`E37: No write since last change`. Use `:w` first, or `:q!` to discard them.

---

## 2. Learning Vim in VTCode

Each lesson builds on the last one. Practise each one for a few minutes in a
scratch file (`:enew` opens an empty buffer).

### Lesson 1: moving

In Normal mode:

| Keys | Moves |
|---|---|
| `h` `j` `k` `l` | left, down, up, right |
| `w` / `b` / `e` | next word start / previous word start / word end |
| `0` / `^` / `$` | line start / first non-blank / line end |
| `gg` / `G` | first line / last line |
| `{` / `}` | previous / next blank line (paragraph) |
| `<C-d>` / `<C-u>` | half a page down / up |
| `%` | the matching bracket |
| `f{char}` / `t{char}` | onto / just before the next `{char}` on the line; `;` repeats, `,` goes back |

Most commands take a **count**: `5j` moves five lines down, `3w` three words
on. The pending count and keys appear in the status bar as you type them.

Tip: the **Relative Line Numbers** setting (or `:set rnu`) shows the
distance to each line, so counts like `12j` become easy.

### Lesson 2: entering Insert mode

| Keys | Starts inserting |
|---|---|
| `i` / `a` | before / after the cursor |
| `I` / `A` | at the start / end of the line |
| `o` / `O` | on a new line below / above |
| `s` / `S` | after deleting the character / the whole line |

Everything you know from VTCode still works while you type: completions,
snippets, auto-closing brackets and quotes. Insert mode also has some Vim
keys: `<C-w>` deletes the word before the cursor, `<C-u>` the line, `<C-r>a`
pastes register `a`, and `<C-o>` runs one Normal-mode command and returns.

### Lesson 3: operators and motions

This is the heart of Vim. An **operator** says *what* to do and a **motion**
says *where*:

| Operator | Does |
|---|---|
| `d` | delete |
| `c` | change (delete, then Insert mode) |
| `y` | yank (copy) |
| `>` / `<` | indent / outdent |
| `gu` / `gU` / `g~` | lowercase / uppercase / toggle case |
| `gc` | comment / uncomment |
| `=` | re-indent |

Combine them: `dw` deletes a word, `d$` to the end of the line, `c3w` changes
three words, `y}` yanks to the end of the paragraph, `gUiw` uppercases the
word under the cursor. Doubling an operator acts on whole lines: `dd`, `yy`,
`cc`, `>>`, `gcc`.

Some shortcuts: `x` deletes a character, `D` deletes to the line end, `C`
changes to the line end, `J` joins the next line, `r{char}` replaces one
character.

### Lesson 4: text objects

After an operator, `i` ("inner") or `a` ("around") plus an object selects a
whole thing, wherever the cursor is inside it:

| Object | Meaning |
|---|---|
| `iw` / `aw` | word |
| `i"` / `a"`, `i'`, `` i` `` | quoted string |
| `i(` / `a(` (also `ib`) | parentheses |
| `i{` / `a{` (also `iB`), `i[`, `i<` | braces, brackets, angle brackets |
| `it` / `at` | HTML/XML tag |
| `ip` / `ap` | paragraph |
| `is` / `as` | sentence |

`ci"` changes the text inside quotes, `da(` deletes a parenthesised group
with its parentheses, `yi{` copies a function body, `dit` empties a tag.

### Lesson 5: undo, redo and the dot

- `u` undoes, `<C-r>` redoes. Each change, including a whole insert, is one step.
- `.` repeats the last change. For example, `cwfoo<Esc>` changes a word to
  `foo`; move to another word and press `.` to do it again.

The dot is what makes Vim fast: make a change once, then repeat it with one
key.

### Lesson 6: copy and paste

- `yy` yanks a line, `yiw` a word; `p` / `P` paste after / before the cursor.
- Deleted text is also pasted by `p`, so `ddp` swaps two lines.
- `"ayy` yanks into register `a`; `"ap` pastes it. `:reg` lists all registers.
- `"+y` and `"+p` use the system clipboard. To make the clipboard the
  default, turn on **Settings → Vim: Use System Clipboard** (or
  `:set clipboard=unnamedplus`).

### Lesson 7: searching and replacing

- `/text<CR>` searches forward, `?text<CR>` backward; `n` / `N` go to the next
  / previous match. Matches highlight as you type. `:noh` clears the highlight.
- `*` / `#` search for the word under the cursor.
- `:s/old/new/` replaces on the current line; `:%s/old/new/g` in the whole file;
  add `c` to confirm each one (`:%s/old/new/gc`).
- Search patterns use Vim regular expressions: `\<word\>` matches a whole word,
  `\v` turns on "very magic" mode, `\c` ignores case.

To search the whole project, see [Lesson 10](#lesson-10-the-rest-of-the-workbench).

### Lesson 8: Visual mode

- `v` selects characters, `V` whole lines, `<C-v>` a rectangular block.
- Move to extend the selection, then use an operator: `d`, `y`, `c`, `>`, `gU`, `gc`...
- `o` jumps to the other end of the selection; `gv` reselects the last one.
- In block mode, `I` or `A` then text then `<Esc>` types on every line at once.
- Dragging with the mouse also starts Visual mode, and includes the character
  under the pointer, as in Vim; a click returns to Normal mode.

### Lesson 9: windows, tabs and files

| Keys / command | Does |
|---|---|
| `:e path` | open a file or browse a directory in the current pane |
| `<C-p>` | Quick Open (fuzzy file finder) |
| `gt` / `gT` or `:bn` / `:bp` | next / previous tab |
| `:sp` / `:vs` (or `<C-w>s` / `<C-w>v`) | split horizontally / vertically |
| `:sp path` / `:vsp path` | open a file or directory in a horizontal / vertical split |
| `:pwd` / `:cd path` | show or change the working directory for Ex commands |
| `<C-w>h` `j` `k` `l` | move to the window left / below / above / right |
| `<C-w>w` / `<C-w>W` | cycle forward / backward through visible panes |
| `<C-w>c` / `<C-w>o` | close this window / close all others |

The cycle includes editor splits, the sidebar, the bottom panel, the terminal
dock and docked Agent Chat when they are visible. Use `<C-w>h/j/k/l` to move
by direction. From a terminal, use `<C-\>w` to continue the cycle.

Press Tab or Shift+Tab to cycle through path completions. For example,
`:vsp .<Tab>` offers `./` and `../`, and `:vsp ./<Tab>` completes entries
inside the working directory.

`:vsp .` opens the working directory alongside your file. Use `j`/`k` to
move through the directory, Enter to open the selected entry in that pane,
and `-` to go up one level. `:Sexplore` and `:Vexplore` browse the current
file's directory in a horizontal or vertical split.

### Lesson 10: the rest of the workbench

All of VTCode works from the keyboard in Vim mode. Inside lists, `j`/`k` move,
`l` or Enter opens, `gg`/`G` jump to the first / last item, and `q` or
`<Esc>` returns to the editor. `:` opens the command line from any of them
(the Explorer, the panels, a sidebar), as from any window in Vim.

**Explorer.** `:Explore` focuses the file tree from anywhere, including another
sidebar view; `<C-w>h` reaches it when the sidebar already shows it (see
[Getting around](#getting-around)).

| Key | Does |
|---|---|
| `j` `k` | move |
| `l` / `o` / Enter | open a file or expand a folder |
| `h` | collapse a folder |
| `a` / `A` | new file / new folder |
| `r` | rename (the name is selected without its extension, so type just the new name) |
| `d` | delete |
| `y` `x` `p` | copy, cut, paste |
| `Y` | copy the relative path |
| `/` | find in the tree |
| `m` | context menu |
| `R` | refresh Git status |

**Project search.** `:VT global_search` (or `Ctrl+Shift+F`) puts you in the
search box. Type the query, press `<Esc>` to move to the results, then `j`/`k`
to preview matches, `h` to collapse a file, and `l` to open a match.

**Source Control.** `:VT source_control` opens and focuses it. Move with `j`/`k`,
press Space to stage or unstage, `l` to open a diff, Tab to move between
sections. The commit message box is a normal text field.

**Problems (quickfix).** `:copen` opens the Problems panel with the cursor in
it. `j`/`k` move, `l` jumps to the problem. `:cnext` / `:cprev` (or `]d` /
`[d`) step through diagnostics without opening the panel. `:cclose` hides it.

**Terminal.** `:terminal` opens one. Keys go to the shell, `<C-w>` included
(shells use it). Press `<C-\><C-n>` to return to the editor, or `<C-\>`
then `h/j/k/l` to move to a neighbouring window directly.

**Agent Chat.** Reach the docked chat with `<C-w>l`. Leave it with
`<C-\><C-n>` or `<C-w>h`; `<C-w>c` hides it.

**Command Palette, menus and dialogs.**

- The palette (`Ctrl+Shift+P`) and Quick Open move with `<C-n>` / `<C-p>`.
- In any text field (search boxes, Quick Open, rename and Save As prompts),
  `<C-w>` deletes the word before the cursor and `<C-u>` everything before it,
  as on Vim's command line.
- Context menus, the menu bar and status-bar menus take `j`/`k`, `l` to choose
  and `q` to close. In the menu bar, `h`/`l` switch menus.
- In dialogs, `h`/`j`/`k`/`l` move between buttons, Enter presses one and
  `<Esc>` cancels.
- Settings opens with `:VT settings`; `<C-w>c` closes it like a window.

**Language features.**

| Keys | Does |
|---|---|
| `gd` / `gD` | go to definition / declaration |
| `gy` | go to type definition |
| `gr` | find references |
| `K` | hover documentation |
| `g.` | code actions |
| `]d` / `[d` | next / previous diagnostic |
| `]c` / `[c` | next / previous Git change |
| `<C-o>` / `<C-i>` | jump back / forward |

### Lesson 11: macros

Record a sequence once, replay it many times:

1. `qa` starts recording into register `a` (the status bar shows `recording @a`).
2. Make your edits, for example `I- <Esc>j` to turn a line into a list item and move down.
3. `q` stops recording.
4. `@a` replays it; `10@a` replays it ten times; `@@` repeats the last macro.

---

## 3. Vim mode for Vim users

### What to expect

The engine follows Vim 9's behaviour, not an approximation of it. Its test
suite replays about 24,000 recorded sessions of real Vim and compares the text,
cursor and registers after each one. Counts, registers, the dot, marks, the
jump and change lists, text objects, `:s` with its flags and `\1`/`\u`/`\U`,
`:g`/`:v`, `:normal`, ranges and Vim regular expressions all behave as you
know them.

Supported beyond the basics include:

- `gq`/`gw` formatting, `g?`, `!` filters, `zf` and the fold commands.
- `<C-a>` / `<C-x>` on decimal, hex and binary numbers.
- `gn`/`gN`, `g;`/`g,`, `]p`, forced motions (`dv`, `dV`, `d<C-v>`).
- Digraphs with `<C-k>`.
- Command-line history (Up/Down filter by prefix) and Tab completion.

### Differences from Vim

- **Insert mode is VTCode's editor.** Completion, snippets and auto-pairs work
  while you type; each insert is still one undo step and repeats with `.`.
- **Buffers are tabs and windows are editor splits.** `:ls`, `:b`, `:bn` and
  `:bd` work on tabs, and `:sp`/`:vs` create VTCode splits.
- **`<C-n>` and `<C-p>` in Normal mode stay with VTCode** (Quick Open and
  friends), since Vim's line motions have `j`/`k`. In Insert mode they complete.
- **Ctrl keys.** **Vim: Use Ctrl Keys** (on by default) gives Vim the Ctrl
  keys it defines: `<C-d>`, `<C-u>`, `<C-v>`, `<C-w>`, `<C-r>` and so on. Turn
  it off to keep VTCode's Ctrl shortcuts for those. Ctrl keys Vim does not use
  (`Ctrl+S`, `Ctrl+Shift+P`...), Alt keys and function keys always reach VTCode.
- **No Vimscript or plugins.** Option settings, mappings and `:` commands work;
  functions, `autocmd` and plugin managers are skipped quietly.
- **`:cd` changes where relative paths start, not the open folder.** As in
  Vim, `:cd dir` (also `:lcd`, `:tcd`) affects `:e`, `:w`, `:r`, `:!` and path
  completion; `:cd -` goes back and `:pwd` shows it. Opening another folder
  (`:VT open_folder`) starts from that folder again.
- **The quickfix list is the Problems panel** (TODO markers and diagnostics).
- **`\n` after the last line.** Vim lets a pattern's `\n` match the end of
  the last line, even in a file without a final line break; VTCode has no
  line break there to match. Elsewhere `\n` works as in Vim, and in files
  with Windows (CRLF) line endings it matches the whole `\r\n`.

### Run any VTCode command

`:VT {command}` (or `:vtcode {command}`) runs any Command Palette or menu
command by its id, with Tab completion. For example: `:VT toggle_word_wrap`,
`:VT format_document`, `:VT rename_symbol`, `:VT git_stage_all`. This is also
how you map VTCode features to keys.

### The vimrc

VTCode reads a `vimrc` file kept beside `settings.json`. **Vim: Open vimrc**
in the Command Palette opens it (creating a commented template), and **Vim:
Reload vimrc** applies changes without restarting. The `vimrcPath` setting
points it to another file, such as your existing `~/.vimrc`; lines VTCode does
not understand are skipped.

It supports:

- `set` options;
- `let mapleader` and `let maplocalleader`;
- the whole `map` / `noremap` family for every mode, with `<silent>` and `<nowait>`;
- `unmap`, abbreviations, and any `:` command.

A starting point:

```vim
let mapleader = " "

set relativenumber
set ignorecase smartcase
set scrolloff=8
set clipboard=unnamedplus

" Leave Insert mode without reaching for Esc.
inoremap jk <Esc>

" Move by display line when lines wrap.
nnoremap j gj
nnoremap k gk

" VTCode features on the leader key.
nnoremap <leader>w :w<CR>
nnoremap <leader>ff :VT quick_open<CR>
nnoremap <leader>fg :VT global_search<CR>
nnoremap <leader>e :Explore<CR>
nnoremap <leader>g :VT source_control<CR>
nnoremap <leader>rn :VT rename_symbol<CR>
nnoremap <leader>ca :VT code_action<CR>
nnoremap <leader>q :copen<CR>
nnoremap <leader>t :terminal<CR>

" Keep the selection when indenting.
vnoremap > >gv
vnoremap < <gv
```

Useful options include `number`, `relativenumber`, `ignorecase`, `smartcase`,
`hlsearch`, `incsearch`, `wrapscan`, `scrolloff`, `wrap`, `linebreak`,
`tabstop`, `shiftwidth`, `expandtab`, `textwidth`, `timeoutlen`, `clipboard`,
`iskeyword`, `gdefault`, `joinspaces`, `nrformats`, `whichwrap`,
`splitbelow`, `splitright` and `highlightyank`. `:set` with no argument shows
the current values.

---

## Quick reference

| Area | Keys |
|---|---|
| Modes | `i a I A o O` insert · `v V <C-v>` visual · `R` replace · `<Esc>` normal |
| Move | `h j k l` · `w b e` · `0 ^ $` · `gg G` · `{ }` · `%` · `f t ; ,` · `<C-d> <C-u>` |
| Edit | `d c y` + motion · `dd yy cc` · `x D C J r` · `p P` · `u <C-r>` · `.` |
| Objects | `iw aw` · `i" a"` · `i( a(` · `i{ a{` · `it at` · `ip ap` |
| Search | `/ ?` · `n N` · `* #` · `:%s/a/b/g` · `:noh` |
| Files | `:w :q :wq :q!` · `:e path` · `<C-p>` · `gt gT` · `:Explore` |
| Windows | `<C-w>h j k l` · `<C-w>s v` · `<C-w>c o` · `<C-w>w` |
| Lists and sidebars | `j k` · `l` open · `gg G` · `q` back to the editor |
| Sidebar views | `:Explore` · `:VT global_search` · `:VT source_control` · `:VT extensions` |
| Code | `gd gy gr K` · `g.` · `]d [d` · `]c [c` · `gc` comment |
| Problems | `:copen :cclose` · `:cnext :cprev` |
| Terminal and chat | `<C-\><C-n>` back to the editor |
| Macros | `qa` … `q` · `@a` · `@@` |
| VTCode commands | `:VT {command}` |
