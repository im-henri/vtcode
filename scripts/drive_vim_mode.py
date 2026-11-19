#!/usr/bin/env python3
"""Drive the real editor in Vim mode from the keyboard only.

The editor binary is copied into a scratch folder with its own settings.json
(`"vimMode": true`), then started in a pseudo-terminal on scratch files.
Each scenario types keys as a person would and checks what the editor shows
and what it saves. Nothing in the repository or your settings is touched.

    uv pip install pywinpty pyte          (Windows; once)
    python scripts/drive_vim_mode.py [--exe PATH] [--show] [--only NAME]

Exit status is the number of scenarios that failed.
"""
import argparse
import json
import pathlib
import shutil
import sys
import tempfile
import threading
import time

import re

import pyte
from winpty import PtyProcess

# pyte reads only ';' between SGR parameters; the editor uses the standard
# ':' form for true colour and underline colour (38:2::r:g:b).
SGR = re.compile(r"\x1b\[([0-9:;]*)m")


def normalize(text):
    def fix(m):
        parts = []
        for param in m.group(1).split(";"):
            if ":" in param:
                sub = [p for p in param.split(":")]
                if sub[0] in ("38", "48", "58") and len(sub) >= 3 and sub[1] == "2":
                    rgb = [p for p in sub[2:] if p != ""][-3:]
                    if sub[0] == "58":
                        continue  # underline colour: ignore
                    parts.append(";".join([sub[0], "2", *rgb]))
                elif sub[0] in ("38", "48") and len(sub) >= 3 and sub[1] == "5":
                    parts.append(";".join(sub[:3]))
                else:
                    parts.append(sub[0])
            else:
                parts.append(param)
        return "\x1b[" + ";".join(parts) + "m"
    return SGR.sub(fix, text)

COLUMNS, ROWS = 120, 40
DOT = chr(0xB7)
ESC = "\x1b"
CR = "\r"


def ctrl(c):
    return chr(ord(c.lower()) & 0x1F)


class Editor:
    def __init__(self, exe, args, cwd):
        self.screen = pyte.Screen(COLUMNS, ROWS)
        self.stream = pyte.Stream(self.screen)
        self.process = PtyProcess.spawn([str(exe), *map(str, args)], dimensions=(ROWS, COLUMNS), cwd=str(cwd))
        self.pending = ""
        import queue
        self.queue = queue.Queue()
        # One reader for the whole session: output is never dropped.
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        while True:
            try:
                data = self.process.read(65536)
            except EOFError:
                return
            if data:
                self.queue.put(data)

    def settle(self, seconds):
        """Apply everything the editor draws during `seconds`."""
        import queue
        deadline = time.time() + seconds
        while True:
            timeout = deadline - time.time()
            try:
                chunk = self.queue.get(timeout=max(timeout, 0.001)) if timeout > 0 else self.queue.get_nowait()
            except queue.Empty:
                return
            data = self.pending + chunk
            # Keep an escape sequence split across reads for the next one.
            cut = data.rfind("\x1b")
            if cut != -1 and not re.search(r"[A-Za-z~]", data[cut + 2:]):
                data, self.pending = data[:cut], data[cut:]
            else:
                self.pending = ""
            self.stream.feed(normalize(data))

    def keys(self, text, pause=0.05):
        """Type text; ESC is sent on its own so it is not read as a sequence."""
        for key in text:
            self.process.write(key)
            self.settle(0.25 if key == ESC else pause)

    def lines(self):
        return [line.rstrip().replace(DOT, " ") for line in self.screen.display]

    def status(self):
        return self.lines()[-1]

    def alive(self):
        return self.process.isalive()

    def close(self):
        try:
            self.process.terminate(force=True)
        except Exception:
            pass


def wait_for(editor, predicate, seconds=6.0):
    deadline = time.time() + seconds
    while time.time() < deadline:
        editor.settle(0.2)
        if predicate():
            return True
    return False


# Each scenario: files to create, keys (a list of strings or callables taking
# the editor), and checks taking (editor, folder).
def scenario_edit_and_write(e, d):
    e.keys("dwjA!" + ESC)
    e.keys(":w" + CR)
    wait_for(e, lambda: (d / "a.txt").read_text() == "beta\ngamma!\n")
    return [
        ("buffer saved by :w", (d / "a.txt").read_text() == "beta\ngamma!\n"),
        ("status shows NORMAL", "NORMAL" in e.status()),
    ]


def scenario_modes_in_status(e, d):
    checks = []
    e.keys("i")
    checks.append(("Insert mode badge", "INSERT" in e.status()))
    e.keys(ESC + "v")
    checks.append(("Visual mode badge", "VISUAL" in e.status()))
    e.keys(ESC + "V")
    checks.append(("Visual line badge", "V-LINE" in e.status()))
    e.keys(ESC + ctrl("v"))
    checks.append(("Visual block badge (Ctrl-V)", "V-BLOCK" in e.status()))
    e.keys(ESC + "R")
    checks.append(("Replace mode badge", "REPLACE" in e.status()))
    e.keys(ESC + "qa")
    checks.append(("recording @a shown", "recording @a" in e.status()))
    e.keys("q" + "2d")
    checks.append(("pending keys shown", "2d" in e.status()))
    e.keys(ESC + ":set rnu")
    checks.append(("command line shown", ":set rnu" in e.status()))
    e.keys(CR)
    return checks


def scenario_insert_caret_is_bar(e, d):
    def caret_on_alpha():
        cur = e.screen.cursor
        return not cur.hidden and e.lines()[cur.y][cur.x:cur.x + 5] == "alpha"
    e.keys("i")
    # The terminal cursor is visible and sits on the caret cell (line 1, col 0).
    in_insert = wait_for(e, caret_on_alpha)
    e.keys(ESC)
    hidden = wait_for(e, lambda: e.screen.cursor.hidden)
    return [
        ("hardware caret placed on the text in Insert mode", in_insert),
        ("hardware caret hidden again in Normal mode", hidden),
    ]


def scenario_undo_redo_and_dot(e, d):
    e.keys("cwfoo" + ESC + "j0.")
    e.keys("u")
    e.keys(":w" + CR)
    wait_for(e, lambda: (d / "a.txt").read_text().startswith("foo\n"))
    one = (d / "a.txt").read_text()
    e.keys(ctrl("r") + ":w" + CR)
    wait_for(e, lambda: (d / "a.txt").read_text() == "foo\nfoo\n")
    two = (d / "a.txt").read_text()
    return [
        ("u undoes the repeated change", one == "foo\ngamma\n"),
        ("Ctrl-R redoes it", two == "foo\nfoo\n"),
    ]


def scenario_search_and_substitute(e, d):
    e.keys("/gam" + CR)
    e.keys(":%s/a/A/g" + CR)
    e.keys(":w" + CR)
    wait_for(e, lambda: "A" in (d / "a.txt").read_text())
    return [("search then :%s and :w", (d / "a.txt").read_text() == "AlphA\ngAmmA\n")]


def scenario_quit_guard(e, d):
    e.keys("x:q" + CR)
    e.settle(0.5)
    guarded = "E37" in e.status() and e.alive()
    e.keys(":wq" + CR)
    ok = wait_for(e, lambda: not e.alive(), 8)
    return [
        (":q with changes reports E37", guarded),
        (":wq saves and quits", ok and (d / "a.txt").read_text() == "lpha\ngamma\n"),
    ]


def scenario_explorer_keyboard(e, d):
    # Ctrl-W h to the Explorer, move down to b.txt, open it, edit, save.
    e.keys(ctrl("w") + "h")
    e.settle(0.4)
    e.keys("G")
    e.keys("l")
    e.settle(1.0)
    e.keys("Ibb " + ESC + ":w" + CR)
    wait_for(e, lambda: (d / "b.txt").read_text().startswith("bb "))
    return [("Explorer j/k/G/l opens a file, then editing it works", (d / "b.txt").read_text() == "bb second\n")]


def scenario_split_and_window_moves(e, d):
    e.keys(":vsplit" + CR)
    e.settle(0.8)
    e.keys(ctrl("w") + "l")
    e.keys("A?" + ESC)
    e.keys(ctrl("w") + "h")
    e.keys(":w" + CR)
    wait_for(e, lambda: "?" in (d / "a.txt").read_text())
    return [("split, move between windows, edit, save", (d / "a.txt").read_text() == "alpha?\ngamma\n")]


def scenario_palette_and_quick_open(e, d):
    e.keys(ctrl("p"))
    e.settle(0.8)
    shown = any("b.txt" in line for line in e.lines())
    e.keys("b.txt")
    e.settle(0.6)
    e.keys(CR)
    e.settle(1.0)
    e.keys("A!" + ESC + ":w" + CR)
    wait_for(e, lambda: (d / "b.txt").read_text().endswith("!\n"))
    return [
        ("Ctrl-P Quick Open lists files", shown),
        ("open from Quick Open and edit in Vim mode", (d / "b.txt").read_text() == "second!\n"),
    ]


def scenario_vimrc_mapping(e, d):
    e.keys("ixyz")
    e.keys("jk")
    e.settle(0.3)
    normal = "NORMAL" in e.status()
    e.keys(" w")
    wait_for(e, lambda: (d / "a.txt").read_text().startswith("xyz"))
    return [
        ("vimrc inoremap jk <Esc>", normal),
        ("vimrc <leader>w :w<CR> (leader = space)", (d / "a.txt").read_text() == "xyzalpha\ngamma\n"),
    ]


def scenario_terminal_round_trip(e, d):
    e.keys(":terminal" + CR)
    e.settle(2.5)
    e.process.write(ctrl("\\"))
    e.settle(0.1)
    e.process.write(ctrl("n"))
    e.settle(0.8)
    e.keys("A~" + ESC + ":w" + CR)
    wait_for(e, lambda: "~" in (d / "a.txt").read_text())
    return [("Ctrl-\\ Ctrl-N leaves the terminal back to the editor", (d / "a.txt").read_text() == "alpha~\ngamma\n")]


def scenario_settings_by_keyboard(e, d):
    e.keys(":VT settings" + CR)
    e.settle(1.2)
    opened = any("Settings" in line for line in e.lines())
    # Move to the Explorer and back, then close Settings like a window.
    e.keys(ctrl("w") + "h")
    e.settle(0.4)
    e.keys(ctrl("w") + "l")
    e.settle(0.4)
    e.keys(ctrl("w") + "c")
    e.settle(0.8)
    e.keys("A%" + ESC + ":w" + CR)
    wait_for(e, lambda: "%" in (d / "a.txt").read_text())
    return [
        ("Settings opens from :VT settings", opened),
        ("Ctrl-W moves out and Ctrl-W c closes Settings, editing resumes", (d / "a.txt").read_text() == "alpha%\ngamma\n"),
    ]


def scenario_problems_panel(e, d):
    e.keys(":copen" + CR)
    listed = wait_for(e, lambda: any("[TODO]" in line for line in e.lines()), 10)
    # The first problem is selected; l opens it at its line.
    e.keys("l")
    e.settle(0.6)
    e.keys("A!" + ESC + ":w" + CR)
    wait_for(e, lambda: "!" in (d / "a.txt").read_text())
    e.keys(":cclose" + CR)
    e.settle(0.4)
    closed = not any("[TODO]" in line for line in e.lines())
    return [
        (":copen lists TODO markers", listed),
        ("l opens the problem at its line; editing resumes", (d / "a.txt").read_text() == "alpha\nTODO later!\ngamma\n"),
        (":cclose hides the panel", closed),
    ]


def screen_has(e, text):
    return any(text in line for line in e.lines())


def resumes_editing(e, d, mark):
    """Back in the editor: an edit and :w reach a.txt."""
    e.keys("A" + mark + ESC + ":w" + CR)
    wait_for(e, lambda: mark in (d / "a.txt").read_text())
    return (d / "a.txt").read_text().splitlines()[0].endswith(mark)


def scenario_explorer_prompts(e, d):
    e.keys(":Explore" + CR)
    e.settle(0.6)
    e.keys("G")
    e.settle(0.3)
    e.keys("r")
    e.settle(0.5)
    # The rename field selects the name without its extension, as in VS Code.
    e.keys("c" + CR)
    renamed = wait_for(e, lambda: (d / "c.txt").exists() and not (d / "b.txt").exists())
    e.keys("a")
    e.settle(0.5)
    e.keys("new.txt" + CR)
    created = wait_for(e, lambda: (d / "new.txt").exists())
    e.settle(0.5)
    # The new file opens in Vim mode: type into it and save.
    e.keys("ifresh" + ESC + ":w" + CR)
    wait_for(e, lambda: (d / "new.txt").read_text().startswith("fresh"))
    return [
        ("Explorer r renames through the inline prompt", renamed),
        ("Explorer a creates a file through the prompt", created),
        ("the new file opens for Vim editing", (d / "new.txt").read_text().startswith("fresh")),
    ]


def scenario_save_as_and_new_buffer(e, d):
    e.keys(":sav copy.txt" + CR)
    saved_as = wait_for(e, lambda: (d / "copy.txt").exists())
    e.keys(":enew" + CR)
    e.settle(0.5)
    e.keys("ihello" + ESC + ":w fresh.txt" + CR)
    wrote = wait_for(e, lambda: (d / "fresh.txt").exists() and (d / "fresh.txt").read_text().startswith("hello"))
    return [
        (":sav writes a copy", saved_as and (d / "copy.txt").read_text() == "alpha\ngamma\n"),
        (":enew then :w {name} writes the new buffer", wrote),
    ]


def scenario_close_dialog(e, d):
    e.keys("x")
    e.keys(":VT close_tab" + CR)
    shown = wait_for(e, lambda: screen_has(e, "Don't save"))
    e.keys(ESC)
    e.settle(0.4)
    dismissed = not screen_has(e, "Don't save")
    e.keys(":VT close_tab" + CR)
    wait_for(e, lambda: screen_has(e, "Don't save"))
    # Buttons: Save & close, Don't save, Cancel. l moves right, Enter picks.
    e.keys("l")
    e.settle(0.2)
    e.keys(CR)
    closed = wait_for(e, lambda: not screen_has(e, "Don't save") and not screen_has(e, "lpha"))
    return [
        ("closing a modified tab asks first", shown),
        ("Esc dismisses the dialog", dismissed),
        ("l then Enter picks Don't save; the file is untouched", closed and (d / "a.txt").read_text() == "alpha\ngamma\n"),
    ]


def scenario_source_control(e, d):
    import subprocess
    git = lambda *args: subprocess.run(["git", "-C", str(d), *args], capture_output=True, text=True)
    git("init", "-q")
    git("-c", "user.name=t", "-c", "user.email=t@t", "add", "a.txt")
    git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init")
    e.keys("A!" + ESC + ":w" + CR)
    wait_for(e, lambda: "!" in (d / "a.txt").read_text())
    e.keys(":VT source_control" + CR)
    e.settle(0.8)
    e.keys("r")
    listed = wait_for(e, lambda: sum("a.txt" in line for line in e.lines()) >= 2, 8)
    # Select the change with j and stage it with Space.
    staged = False
    for _ in range(6):
        e.keys("j")
        e.keys(" ")
        e.settle(0.5)
        if git("diff", "--cached", "--name-only").stdout.strip() == "a.txt":
            staged = True
            break
    e.keys("q")
    e.settle(0.4)
    return [
        ("Source Control lists the change", listed),
        ("j and Space stage it", staged),
        ("q returns to the editor", resumes_editing(e, d, "#")),
    ]


def scenario_output_and_extensions(e, d):
    e.keys(":VT show_output" + CR)
    shown = wait_for(e, lambda: screen_has(e, "Output"))
    e.keys("jjG")
    e.settle(0.3)
    output_focused = "Ln 1," in e.status()
    e.keys("q")
    e.settle(0.4)
    first = resumes_editing(e, d, "@")
    e.keys(":VT extensions" + CR)
    e.settle(0.8)
    # Focus starts in the filter box; Esc leaves it for the list.
    e.keys(ESC + "jkq")
    e.settle(0.4)
    second = resumes_editing(e, d, "^")
    return [
        ("Output panel opens", shown),
        ("Output takes the keyboard (the editor cursor stays put)", output_focused),
        ("Output: j/k/G then q returns to the editor", first),
        ("Extensions: j/k then q returns to the editor", second),
    ]


def scenario_global_search(e, d):
    e.keys(":VT global_search" + CR)
    e.settle(0.8)
    e.keys("gamma")
    found = wait_for(e, lambda: screen_has(e, "2 results"))
    # Esc leaves the query for the results; j moves, l opens the match.
    e.keys(ESC + "j")
    e.settle(0.4)
    e.keys("l")
    e.settle(0.8)
    e.keys("A!" + ESC + ":w" + CR)
    wait_for(e, lambda: "!" in (d / "b.txt").read_text())
    return [
        ("Search lists matches as you type", found),
        ("Esc, j, l open the second match for editing", (d / "b.txt").read_text() == "one\ntwo gamma!\n"),
    ]


def scenario_context_menu_and_delete_dialog(e, d):
    e.keys(":Explore" + CR)
    e.settle(0.6)
    e.keys("G")
    e.settle(0.3)
    # m opens the context menu; j/k move in it, q closes it.
    e.keys("m")
    menu = wait_for(e, lambda: screen_has(e, "Copy Relative Path"))
    e.keys("jjk")
    e.keys("q")
    e.settle(0.4)
    menu_closed = not screen_has(e, "Copy Relative Path")
    # d asks before deleting. l moves to Cancel, Enter presses it.
    e.keys("d")
    asked = wait_for(e, lambda: screen_has(e, "Cancel"))
    e.keys("l")
    e.settle(0.2)
    e.keys(CR)
    e.settle(0.6)
    kept = (d / "b.txt").exists() and not screen_has(e, "Cancel")
    # d again, h back to Delete, Enter deletes.
    e.keys("d")
    wait_for(e, lambda: screen_has(e, "Cancel"))
    e.keys("lh")
    e.settle(0.2)
    e.keys(CR)
    deleted = wait_for(e, lambda: not (d / "b.txt").exists())
    e.keys("q")
    e.settle(0.4)
    return [
        ("Explorer m opens the context menu", menu),
        ("q closes the context menu", menu_closed),
        ("d asks to confirm the delete", asked),
        ("l, Enter picks Cancel and keeps the file", kept),
        ("h, Enter picks Delete", deleted),
        ("q returns to the editor", resumes_editing(e, d, "&")),
    ]


def scenario_notifications(e, d):
    e.keys(":w" + CR)
    e.settle(0.4)
    e.keys(":VT show_notifications" + CR)
    shown = wait_for(e, lambda: screen_has(e, "Notifications"))
    # Ctrl-W j moves into the panel, j/k/G scroll, q returns.
    e.keys(ctrl("w") + "j")
    e.settle(0.3)
    e.keys("jjG")
    e.settle(0.3)
    # The keys went to the panel: the editor cursor is still on line 1.
    focused = "Ln 1," in e.status()
    e.keys("gg")
    e.keys("q")
    e.settle(0.4)
    return [
        ("Notifications panel opens", shown),
        ("Ctrl-W j gives the panel the keyboard", focused),
        ("Ctrl-W j, j/k/G/gg, q returns to the editor", resumes_editing(e, d, "%")),
    ]


def scenario_extension_details(e, d):
    e.keys(":VT extensions" + CR)
    e.settle(0.8)
    e.keys(ESC)
    listed = wait_for(e, lambda: screen_has(e, "Hex Editor"), 8)
    # l opens the selected extension's details in an editor tab.
    e.keys("gg")
    e.keys("l")
    opened = wait_for(e, lambda: screen_has(e, "binary files"), 8)
    # Back to the editor area, then Ctrl-W c closes the details like a window.
    e.keys("q")
    e.settle(0.4)
    e.keys(ctrl("w") + "c")
    closed = wait_for(e, lambda: not screen_has(e, "binary files"))
    return [
        ("an installed extension is listed", listed),
        ("l opens its details", opened),
        ("q, Ctrl-W c close the details", closed),
        ("editing resumes", resumes_editing(e, d, "*")),
    ]


def scenario_explorer_cut_paste(e, d):
    # The keyboard way to drag a file into a folder: x on it, p on the folder.
    e.keys(":Explore" + CR)
    e.settle(0.6)
    e.keys("G")
    e.settle(0.3)
    e.keys("x")
    e.settle(0.3)
    e.keys("gg")
    e.settle(0.3)
    # The first row of the tree is the sub folder.
    e.keys("p")
    e.settle(0.6)
    if screen_has(e, "Cancel"):
        e.keys(CR)  # Confirm the move.
    moved = wait_for(e, lambda: (d / "sub" / "b.txt").exists() and not (d / "b.txt").exists())
    e.keys("q")
    e.settle(0.4)
    return [
        ("x then p moves a file into a folder", moved),
        ("q returns to the editor", resumes_editing(e, d, "+")),
    ]


def scenario_save_as_dialog(e, d):
    e.keys(":enew" + CR)
    e.settle(0.5)
    e.keys("ihi" + ESC + ":w" + CR)
    # An untitled buffer opens the Save As dialog; type a name and press Enter.
    asked = wait_for(e, lambda: screen_has(e, "Save As") or screen_has(e, "Save as"))
    e.keys(ctrl("u"))
    e.keys(str(d / "named.txt") + CR)
    saved = wait_for(e, lambda: (d / "named.txt").exists())
    e.keys("Aa" + ESC + ":w" + CR)
    wait_for(e, lambda: (d / "named.txt").read_text().startswith("hia"))
    return [
        (":w on a new buffer opens Save As", asked),
        ("typing a path and Enter saves it", saved),
        ("the saved buffer keeps editing in Vim mode", (d / "named.txt").read_text().startswith("hia")),
    ]


def box_count(e):
    return sum("┌" in line for line in e.lines())


def scenario_more_dialogs(e, d):
    results = []
    # Dialogs that open with a box and close with Esc (Tab moves inside).
    for command, label in [("about", "About"), ("file_associations", "File Associations"),
                           ("configure_formatter", "Formatter settings")]:
        e.keys(":VT " + command + CR)
        opened = wait_for(e, lambda: box_count(e) > 0, 5)
        e.keys("\t")
        e.settle(0.2)
        e.keys(ESC)
        closed = wait_for(e, lambda: box_count(e) == 0, 5)
        results.append((label + " opens and Esc closes it", opened and closed))
    # The theme prompt is a text field: type, clear with Ctrl-U, cancel.
    e.keys(":VT install_color_theme" + CR)
    opened = wait_for(e, lambda: box_count(e) > 0, 5)
    e.keys("https://example.invalid/theme")
    e.settle(0.3)
    typed = screen_has(e, "example.invalid")
    e.keys(ctrl("u"))
    e.settle(0.3)
    cleared = not screen_has(e, "example.invalid")
    e.keys(ESC)
    closed = wait_for(e, lambda: box_count(e) == 0, 5)
    results.append(("theme prompt: type, Ctrl-U clears, Esc cancels", opened and typed and cleared and closed))
    # Go to line, through its shortcut id.
    e.keys(":VT go_to_line" + CR)
    e.settle(0.5)
    e.keys("2" + CR)
    moved = wait_for(e, lambda: "Ln 2," in e.status(), 5)
    results.append(("go to line jumps to the typed line", moved))
    # Agent settings open a section of Settings: Esc steps out, Esc closes.
    e.keys(":VT acp_configure" + CR)
    shown = wait_for(e, lambda: screen_has(e, "Settings"), 5)
    e.keys(ESC)
    e.settle(0.3)
    e.keys(ESC)
    e.settle(0.5)
    gone = not any("⚙ Settings" in line for line in e.lines()[:3])
    results.append(("agent settings: Esc, Esc close Settings", shown and gone))
    e.keys("gg")
    results.append(("editing resumes", resumes_editing(e, d, "~")))
    return results


def mouse(e, button, x, y, release=False):
    """An SGR mouse report at a 0-based cell (ConPTY turns it into a mouse event)."""
    e.process.write(f"\x1b[<{button};{x + 1};{y + 1}{'m' if release else 'M'}")


def drag(e, start, end, steps=5):
    mouse(e, 0, *start)
    e.settle(0.1)
    for i in range(1, steps + 1):
        mouse(e, 32, start[0] + (end[0] - start[0]) * i // steps, start[1] + (end[1] - start[1]) * i // steps)
        e.settle(0.08)
    mouse(e, 0, *end, release=True)
    e.settle(0.6)


def cell_of(e, text, max_x=200, min_x=0):
    for y, line in enumerate(e.lines()):
        x = line.find(text, min_x)
        if 0 <= x < max_x:
            return x, y
    return None


def scenario_mouse_still_works(e, d):
    # Vim mode keeps the mouse: drag a file onto a folder in the Explorer.
    source, target = cell_of(e, "b.txt", 35), cell_of(e, "sub", 35)
    drag(e, source, target)
    asked = wait_for(e, lambda: screen_has(e, "Move this item"))
    e.keys(CR)
    moved = wait_for(e, lambda: (d / "sub" / "b.txt").exists() and not (d / "b.txt").exists())
    # Pressing on b.txt opened it; return to a.txt by keyboard.
    e.keys(":e " + str(d / "a.txt") + CR)
    # The editor's "gamma" is the one after line number 2 in the gutter.
    def editor_word():
        cell = cell_of(e, "2  gamma")
        return cell and (cell[0] + 3, cell[1])
    wait_for(e, lambda: editor_word() is not None)
    # A click in the text puts the cursor there; a drag selects in Visual mode.
    word = editor_word()
    mouse(e, 0, *word)
    mouse(e, 0, *word, release=True)
    e.settle(0.4)
    clicked = "Ln 2, Col 1" in e.status() and "NORMAL" in e.status()
    drag(e, word, (word[0] + 2, word[1]))
    visual = "VISUAL" in e.status()
    e.keys("d" + ESC + ":w" + CR)
    wait_for(e, lambda: "gamma" not in (d / "a.txt").read_text())
    return [
        ("dragging a file onto a folder asks to move it", asked),
        ("Enter confirms; the file moves", moved),
        ("a click places the cursor in Normal mode", clicked),
        ("a drag selects in Visual mode", visual),
        ("d deletes the mouse selection", (d / "a.txt").read_text() == "alpha\nma\n"),
    ]


def scenario_cd(e, d):
    # :cd moves where relative Ex paths start; the folder stays open.
    e.keys(":cd sub" + CR)
    e.settle(0.3)
    e.keys(":w copy.txt" + CR)
    wrote = wait_for(e, lambda: (d / "sub" / "copy.txt").exists())
    e.keys(":e inner.txt" + CR)
    opened = wait_for(e, lambda: screen_has(e, "inside"))
    e.keys(":cd -" + CR)
    e.settle(0.3)
    e.keys(":e a.txt" + CR)
    back = wait_for(e, lambda: screen_has(e, "gamma"))
    return [
        (":cd sub, then :w copy.txt writes into sub", wrote),
        (":e inner.txt opens sub/inner.txt", opened),
        (":cd - returns; :e a.txt opens the root file", back),
    ]


SCENARIOS = [
    ("edit and :w", scenario_edit_and_write, {"a.txt": "alpha beta\ngamma\n"}),
    ("mode indicators", scenario_modes_in_status, {"a.txt": "alpha\ngamma\n"}),
    ("insert caret", scenario_insert_caret_is_bar, {"a.txt": "alpha\ngamma\n"}),
    ("undo, redo, dot", scenario_undo_redo_and_dot, {"a.txt": "alpha\ngamma\n"}),
    ("search and :s", scenario_search_and_substitute, {"a.txt": "alpha\ngamma\n"}),
    ("quit guard", scenario_quit_guard, {"a.txt": "alpha\ngamma\n"}),
    ("explorer", scenario_explorer_keyboard, {"a.txt": "alpha\ngamma\n", "b.txt": "second\n"}),
    ("splits", scenario_split_and_window_moves, {"a.txt": "alpha\ngamma\n"}),
    ("quick open", scenario_palette_and_quick_open, {"a.txt": "alpha\ngamma\n", "b.txt": "second\n"}),
    ("vimrc", scenario_vimrc_mapping, {"a.txt": "alpha\ngamma\n"}),
    ("terminal", scenario_terminal_round_trip, {"a.txt": "alpha\ngamma\n"}),
    ("settings", scenario_settings_by_keyboard, {"a.txt": "alpha\ngamma\n"}),
    ("problems", scenario_problems_panel, {"a.txt": "alpha\nTODO later\ngamma\n"}),
    ("explorer prompts", scenario_explorer_prompts, {"a.txt": "alpha\ngamma\n", "b.txt": "second\n"}),
    ("save as", scenario_save_as_and_new_buffer, {"a.txt": "alpha\ngamma\n"}),
    ("close dialog", scenario_close_dialog, {"a.txt": "alpha\ngamma\n"}),
    ("source control", scenario_source_control, {"a.txt": "alpha\ngamma\n"}),
    ("output, extensions", scenario_output_and_extensions, {"a.txt": "alpha\ngamma\n"}),
    ("global search", scenario_global_search, {"a.txt": "alpha\ngamma\n", "b.txt": "one\ntwo gamma\n"}),
    ("context menu, delete dialog", scenario_context_menu_and_delete_dialog, {"a.txt": "alpha\ngamma\n", "b.txt": "second\n"}),
    ("notifications", scenario_notifications, {"a.txt": "alpha\ngamma\n"}),
    ("extension details", scenario_extension_details, {"a.txt": "alpha\ngamma\n", "@extension": "hex-editor"}),
    ("explorer cut and paste", scenario_explorer_cut_paste, {"a.txt": "alpha\ngamma\n", "b.txt": "second\n", "sub/inner.txt": "x\n"}),
    ("save as dialog", scenario_save_as_dialog, {"a.txt": "alpha\ngamma\n"}),
    ("more dialogs", scenario_more_dialogs, {"a.txt": "alpha\ngamma\n"}),
    ("mouse still works", scenario_mouse_still_works, {"a.txt": "alpha\ngamma\n", "b.txt": "second\n", "sub/inner.txt": "x\n"}),
    (":cd", scenario_cd, {"a.txt": "alpha\ngamma\n", "sub/inner.txt": "inside\n"}),
]

VIMRC = 'let mapleader = " "\ninoremap jk <Esc>\nnnoremap <leader>w :w<CR>\n'


def run(exe, name, fn, files, show):
    with tempfile.TemporaryDirectory() as tmp:
        tmp = pathlib.Path(tmp)
        home = tmp / "app"
        home.mkdir()
        local = home / pathlib.Path(exe).name
        shutil.copy2(exe, local)
        (home / "settings.json").write_text(json.dumps({"vimMode": True, "quickSuggestions": False, "showMinimap": False}), encoding="utf-8")
        (home / "vimrc").write_text(VIMRC, encoding="utf-8")
        work = tmp / "work"
        work.mkdir()
        for fname, body in files.items():
            if fname == "@extension":
                fixture = pathlib.Path(__file__).resolve().parent.parent / "tests" / "fixtures" / body
                shutil.copytree(fixture, home / ".vtcode" / "extensions" / body)
                continue
            (work / fname).parent.mkdir(parents=True, exist_ok=True)
            (work / fname).write_text(body, encoding="utf-8", newline="\n")
        editor = Editor(local, [work / "a.txt"], work)
        try:
            wait_for(editor, lambda: "NORMAL" in editor.status(), 15)
            editor.settle(0.5)
            results = fn(editor, work)
            ok = all(r for _, r in results)
            print(f"{'ok  ' if ok else 'FAIL'} {name}")
            for label, r in results:
                print(f"       {'+' if r else '-'} {label}")
            if show or not ok:
                print("\n".join("       | " + line for line in editor.lines() if line.strip()))
            return ok
        finally:
            editor.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--exe", default="target/debug/vtcode.exe")
    parser.add_argument("--show", action="store_true")
    parser.add_argument("--only", default="")
    args = parser.parse_args()
    exe = pathlib.Path(args.exe).resolve()
    failed = 0
    for name, fn, files in SCENARIOS:
        if args.only and args.only not in name:
            continue
        if not run(exe, name, fn, files, args.show):
            failed += 1
    print(f"{failed} scenario(s) failed")
    return failed


if __name__ == "__main__":
    sys.exit(main())
