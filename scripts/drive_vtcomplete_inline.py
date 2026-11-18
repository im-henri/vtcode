#!/usr/bin/env python3
"""Type into the real editor and report the inline predictions it shows.

The editor binary is started in a pseudo-terminal on a scratch copy of each
scenario file. The script moves to the end of the file, types the scenario's
text one key at a time as a person would, reads the screen, presses Tab, saves and
reads the file. Nothing in the repository is opened or changed.

    uv pip install pywinpty pyte          (Windows; once)
    python scripts/drive_vtcomplete_inline.py [--exe PATH] [--show]

Each scenario is a file body, the keys to type after it, the text that must
appear dimmed after the caret, and the line expected once Tab is pressed.
Exit status is the number of scenarios that failed.
"""
import argparse
import pathlib
import sys
import tempfile
import time

import pyte
from winpty import PtyProcess

COLUMNS, ROWS = 120, 40
# The editor draws some spaces as middle dots.
DOT = chr(0xB7)

SCENARIOS = [
    {
        "name": "a counted series continues with its next step",
        "file": "series.js",
        "body": "function setup() {\n    var myCustomOBSCURE_VAR0 = a;\n    var myCustomOBSCURE_VAR1 = b;\n",
        "type": "    v",
        "ghost": "ar myCustomOBSCURE_VAR2 = c;",
        "line": "var myCustomOBSCURE_VAR2 = c;",
    },
    {
        "name": "lower-case typing finds a capitalised name and Tab restores it",
        "file": "layout.rs",
        "body": (
            "pub struct LayoutRenderers {\n    count: usize,\n}\n\n"
            "fn first() -> LayoutRenderers {\n    LayoutRenderers { count: 1 }\n}\n\n"
            "fn second() -> LayoutRenderers {\n    LayoutRenderers { count: 2 }\n}\n\n"
        ),
        "type": "fn third() -> layoutr",
        "ghost": "enderers",
        "line": "fn third() -> LayoutRenderers",
    },
    {
        "name": "a comment is continued by the language model",
        "file": "shapes.rs",
        "body": (
            "/// A rectangle with a position and a size.\npub struct Rectangle {\n"
            "    pub width: f32,\n    pub height: f32,\n}\n\nimpl Rectangle {\n"
            "    /// Creates a rectangle from its size.\n"
            "    pub fn new(width: f32, height: f32) -> Self {\n        Self { width, height }\n    }\n\n"
        ),
        "type": "    /// Returns the area of the rect",
        "ghost": "angle",
        "line": "/// Returns the area of the rectangle",
    },
    {
        "name": "a language never seen: its own block words are picked up",
        "file": "robot.zap",
        "body": (
            "unit Robot do\n    slot battery\n\n    task charge(amount) do\n        battery <- battery + amount\n"
            "        when battery > 100 do\n            battery <- 100\n        done\n    done\n\n"
            "    task drain(amount) do\n        battery <- battery - amount\n        when battery < 0"
        ),
        "type": " ",
        "ghost": "do",
        "line": "when battery < 0 do",
    },
]


class Editor:
    def __init__(self, exe, path):
        self.screen = pyte.Screen(COLUMNS, ROWS)
        self.stream = pyte.Stream(self.screen)
        self.process = PtyProcess.spawn([str(exe), str(path)], dimensions=(ROWS, COLUMNS), cwd=str(path.parent))

    def settle(self, seconds):
        """Read what the editor draws for a while."""
        import threading

        deadline = time.time() + seconds
        while time.time() < deadline:
            chunk = []
            reader = threading.Thread(target=lambda: chunk.append(self._read()), daemon=True)
            reader.start()
            reader.join(max(0.05, deadline - time.time()))
            if reader.is_alive():
                # Nothing more to read for now; the thread ends with the process.
                self._pending = reader
                break
            if chunk and chunk[0]:
                self.stream.feed(chunk[0])

    def _read(self):
        try:
            return self.process.read(65536)
        except EOFError:
            return ""

    def keys(self, text, pause=0.06):
        for key in text:
            self.process.write(key)
            self.settle(pause)

    def lines(self):
        return [line.rstrip().replace(DOT, " ") for line in self.screen.display]

    def close(self):
        try:
            self.process.terminate(force=True)
        except Exception:
            pass


def run(exe, scenario, show):
    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / scenario["file"]
        path.write_text(scenario["body"], encoding="utf-8", newline="\n")
        editor = Editor(exe, path)
        try:
            editor.settle(2.5)
            editor.process.write("\x1b[1;5F")  # Ctrl+End, sent as one key
            editor.settle(0.4)
            editor.keys(scenario["type"])
            editor.settle(1.5)
            before = editor.lines()
            typed = scenario["type"].strip()
            # The prediction is whatever stands right of the caret on its
            # line; the terminal's cursor sits just past its first character.
            caret = editor.screen.cursor
            ghost = editor.screen.display[caret.y][max(caret.x - 1, 0) :].replace(DOT, " ").strip()
            editor.keys("\t", pause=0.6)
            after = editor.lines()
            # What Tab really inserted is read from the file the editor saves.
            editor.process.write("\x13")  # Ctrl+S
            editor.settle(1.0)
            accepted = scenario["line"] in path.read_text(encoding="utf-8")
            ok = ghost.startswith(scenario["ghost"]) and accepted
            print(f"{'ok  ' if ok else 'FAIL'} {scenario['name']}")
            print(f"       typed {typed!r}, shown after it: {ghost!r}; after Tab the line {'is' if accepted else 'is NOT'} {scenario['line']!r}")
            if show or not ok:
                print("\n".join("       | " + line for line in before if line.strip()))
                print("       --- after Tab ---")
                print("\n".join("       | " + line for line in after if line.strip()))
            return ok
        finally:
            editor.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--exe", default="target/fastrelease/vtcode.exe")
    parser.add_argument("--show", action="store_true", help="print the screens")
    arguments = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    exe = pathlib.Path(arguments.exe).resolve()
    failed = sum(not run(exe, scenario, arguments.show) for scenario in SCENARIOS)
    sys.exit(failed)


if __name__ == "__main__":
    main()
