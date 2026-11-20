#!/usr/bin/env python3
"""Record real Vim behaviour for the Vim-mode parity corpus.

Each case is (buffer lines, cursor, keys in Vim notation). The script runs
every case through `vim -Es` with options matching VTCode's Vim defaults and
writes the resulting buffer, cursor and unnamed register to
src/vim/parity_expected.json.gz. `cargo test vim::parity` replays the corpus
through VTCode's engine and reports every difference.

Usage: python scripts/vim_parity.py [--vim PATH] [--jobs N] [--filter TEXT]
"""

import argparse
import concurrent.futures
import gzip
import re
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src" / "vim" / "parity_expected.json.gz"

# Options shared with the engine's defaults (see VimState/Options::default and
# the parity test host). The host types plain text with 'autoindent'.
SETUP = [
    "set nocompatible",
    "set tabstop=4 shiftwidth=4 softtabstop=0 expandtab",
    "set autoindent nosmartindent nocindent",
    "set nojoinspaces",
    "set nrformats=bin,hex",
    "set whichwrap=b,s",
    "set startofline",
    "set noignorecase nosmartcase",
    "set wrapscan magic",
    "set textwidth=0",
    "set backspace=indent,eol,start",
    "set selection=inclusive virtualedit=",
    "set formatoptions=",
    "set nofoldenable",
    "set shortmess+=s",
]

TEXTS = {
    "words": (["foo bar.baz  qux", "  indented line here", "", "last-line end"], True),
    "code": (["if (a && (b || c)) {", "    x = \"he said \\\"hi\\\"\";", "    y = 'q';", "}"], True),
    "prose": (["The quick brown fox. Jumps over!  The lazy dog?", "", "Second para line one.", "line two.", "", "Third."], True),
    "tags": (["<div><p>hello <b>world</b></p></div>", "<ul>", "  <li>one</li>", "</ul>"], True),
    "nums": (["a1 b22 c333", "0x1f 0b101 -5 007 x9y", "val=41;"], True),
    "tabs": (["\tfoo\tbar", "  baz  ", "\t\tdeep"], True),
    "lines": (["one", "two", "three", "four", "five", "six"], False),
    "single": (["x"], True),
    "empty": ([""], True),
    "blank": (["alpha", "", "", "beta", "gamma"], True),
}

CURSORS = {
    "words": [(0, 0), (0, 5), (0, 13), (1, 3), (3, 12)],
    "code": [(0, 0), (0, 8), (0, 12), (1, 10), (1, 16), (2, 9), (3, 0)],
    "prose": [(0, 0), (0, 10), (0, 22), (2, 3), (3, 8)],
    "tags": [(0, 16), (0, 10), (0, 1), (2, 7)],
    "nums": [(0, 0), (0, 4), (1, 0), (1, 6), (1, 13), (1, 15), (2, 0)],
    "tabs": [(0, 0), (0, 2), (1, 3), (2, 2)],
    "lines": [(0, 0), (2, 1), (5, 0)],
    "single": [(0, 0)],
    "empty": [(0, 0)],
    "blank": [(0, 2), (1, 0), (3, 1)],
}

MOTIONS = [
    "w", "W", "b", "B", "e", "E", "ge", "gE", "0", "^", "$", "g_", "3|", "h", "l", "j", "k",
    "2w", "3e", "2b", "fa", "Fa", "ta", "Ta", "f.", "fz", "2fo", ";", ",", "fo;", "to;", "to,",
    "%", "(", ")", "{", "}", "2}", "gg", "G", "2G", "-", "+", "_", "2_", "<BS>",
    "1<Space>", "3<Space>", "3<BS>", "<CR>", "[(", "])", "[{", "]}", "gm", "gM", "go", "4go", "`a", "'a",
]

OPERATORS = ["d", "c", "y", "g~", "gu", "gU", ">", "<"]

OBJECTS = [
    "iw", "aw", "iW", "aW", "2iw", "2aw", "is", "as", "ip", "ap", "i(", "a(", "ib", "ab", "i{", "a{",
    "i[", "a[", "i<", "a<", "i\"", "a\"", "i'", "a'", "it", "at", "2i(", "2a(",
]

COMMANDS = [
    "x", "X", "3x", "s", "S", "rZ", "3rZ", "r<CR>", "~", "3~", "J", "3J", "gJ", "D", "C", "Y", "dd", "2dd", "cc", "yy",
    ">>", "<<", "3>>", "g~~", "guu", "gUU", "gUgU", "p", "P", "yyp", "yyP", "ddp", "ddP", "xp", "dwP", "yiwP", "yiwp",
    "ywP", "3p", "2yyp", "dd.", "x..", "dw..", "cwnew<Esc>w.", "A;<Esc>j.", "I# <Esc>j.", "o<Esc>", "Onew<Esc>",
    "ofoo<Esc>j.", "3ia<Esc>", "3Ab<Esc>", "ix<Esc>u", "dwu", "dwu<C-r>", "ddjdduu", "xxxu", "ihello<Esc>u",
    "cwabc<Esc>u", "<C-a>", "<C-x>", "5<C-a>", "10<C-x>", "<C-a><C-a>", "ma$`a", "majd'a", "mak`ad`a",
    "qaxjq@a", "qa0x$xq@a", "qaA!<Esc>q2@a", "qbddq@b@@", "\"ayy\"ap", "\"ayyj\"Ayy\"ap",
    "\"_dd", "\"add\"bdd\"ap\"bp", "dd\"1p", "ddddu\"2p", "dw\"-p", "yyjdd\"0p",
    "Rxy<Esc>", "Rxy<BS><Esc>", "Rabcdef<Esc>", "gIx<Esc>", "aY<Esc>", "iA<CR>B<Esc>", "A<CR>c<Esc>",
    "i<C-w><Esc>", "A<C-w><Esc>", "A<C-u><Esc>", "A<C-h><Esc>", "i<C-t><Esc>", "i<C-d><Esc>", "A<C-r>\"<Esc>",
    "yiwA<C-r>0<Esc>", "ix<Esc>A<C-a><Esc>",
    # <C-o>: one Normal-mode command, then Insert mode resumes where Vim does.
    "i<C-o>$!<Esc>", "A<C-o>h!<Esc>", "A<C-o>x!<Esc>", "A<C-o>j!<Esc>", "A<C-o>k!<Esc>", "i<C-o>w!<Esc>",
    "A<C-o>0!<Esc>", "a<C-o>l!<Esc>", "i<C-o>dw!<Esc>", "A<C-o>b!<Esc>", "i<C-o>j!<Esc>",
    "ve", "vey", "ved", "vjd", "vjy", "v$d", "vlld", "vllc!<Esc>", "v3ld", "vawd", "viwd", "vi(d", "va\"d",
    "viwy", "vjo<Esc>", "vjoy", "Vd", "Vjd", "Vjy", "VjJ", "Vj>", "Vj<", "Vj~", "VjU", "Vju", "vlU", "vlu", "vl~",
    "Vjrx", "vllrx", "vjrx", "V>.", "Vjd.", "vlld.", "<C-v>jd", "<C-v>jld", "<C-v>jjlld", "<C-v>jy", "<C-v>j$d",
    "<C-v>jIX<Esc>", "<C-v>jlAX<Esc>", "<C-v>j$AX<Esc>", "<C-v>jcZ<Esc>", "<C-v>jlrx", "<C-v>j>", "<C-v>jU",
    "<C-v>jlyP", "<C-v>jlyp", "yiwviwp", "yiwvep", "yyVp", "yyjVp", "ddVp", "yiwVp", "vlyP", "vy$p",
    "vjJ", "vjgJ", "Vjj:s/o/0/g<CR>", "gvd", "vly\\gvd", "v<Esc>gvd",
    ":s/o/0/<CR>", ":s/o/0/g<CR>", ":%s/e/E/g<CR>", ":%s/\\(\\w\\+\\) \\(\\w\\+\\)/\\2 \\1/<CR>",
    ":%s/\\<\\w/\\u&/g<CR>", ":s/$/!/<CR>", ":%s/^/> /<CR>", ":%s/o\\zsn/N/g<CR>", ":%s/x/y/ge<CR>",
    ":s/a/b<CR>j:&<CR>", ":%s/o/O/<CR>u", ":%s/i/I/gc<CR>yny", ":g/e/d<CR>", ":v/e/d<CR>", ":g/o/s/o/0/g<CR>",
    ":g/./m0<CR>", ":g/^/m0<CR>", ":2,3d<CR>", ":2,3y<CR>P", ":1t$<CR>", ":1,2t0<CR>", ":2m$<CR>", ":$m0<CR>",
    ":%j<CR>", ":1,2j!<CR>", ":2,3><CR>", ":%norm A;<CR>", ":%norm! I- <CR>", ":2,3normal dw<CR>",
    ":sort<CR>", ":sort!<CR>", ":sort u<CR>", ":%sort n<CR>", ":1,2sort<CR>", ":2<CR>", ":$<CR>", ":.+1<CR>",
    ":/two/<CR>", ":?one?<CR>", ":2;+1d<CR>", ":.,$d<CR>", ":%d<CR>", "dd:pu<CR>", ":2pu!<CR>", ":0pu<CR>",
    "/o<CR>", "/o<CR>n", "/o<CR>N", "?o<CR>", "/e<CR>dn", "d/e<CR>", "y/e<CR>P", "/e/e<CR>", "/e/e+1<CR>",
    "/o/s-1<CR>", "/e/+1<CR>", "*", "#", "2*", "g*", "g#", "*N", "#n", "/xyz<CR>", "dgn", "cgnX<Esc>", "/o<CR>cgnX<Esc>..",
    "zfj", "100x", "100dd", "100J", "50p", "dj", "dk", "2dj", "dG", "dgg", "yG", "d}", "d{", "d)", "d(", "c$X<Esc>",
    "d0", "d^", "dfo", "dto", "dFo", "dTo", "d;", "d,", "fod;", "dvj", "dVw", "d<C-v>j", "dvb", "dve", "c<C-v>jX<Esc>",
    "di(", "da(", "ci(X<Esc>", "ca(X<Esc>", "dit", "dat", "cit<Esc>", "yitP", "di\"", "ci\"X<Esc>", "da'", "ci'Z<Esc>",
    "dip", "yap", "dap", "cipX<Esc>", "dis", "das", "cisX<Esc>", "d2aw", "c2iwX<Esc>", "yiwviwy", "v3iwd",
    "vipd", "vapd", "vipJ", "v2apd", "vasd", "visd",
    # Line breaks and "not" classes in patterns (CRLF buffers keep "\r").
    # (Vim's quirks with the line break after the last line, and the cursor
    # after a :s that joins lines, are left out: see docs/vim-mode.md.)
    ":s/\\n/,/<CR>", ":%s/e\\n/E/<CR>", ":%s/\\S\\+/X/g<CR>", ":1s/\\n/-/<CR>u", ":1,2s/\\n/-/<CR>dd",
    ":s/ [^ ]*/_/g<CR>", ":%s/[^a-z]/_/g<CR>", ":%s/\\w\\+$/X/<CR>", ":%s/.*/[&]/<CR>", ":1,2s/\\_s\\+/ /g<CR>",
    ":%s/\\s*$/;/<CR>", ":%s/\\W/_/g<CR>", ":%s/\\D\\+/#/g<CR>", "/e\\n<CR>", ":s/\\n\\n/\\r/<CR>dd",
    "/\\S\\+$<CR>", "/[^ ]$<CR>", ":%s/\\_.//<CR>", ":%s/$\\n^//<CR>", ":%s/.$//<CR>", ":%s/\\n\\(.\\)/|\\1/<CR>",
    ":s/ /\\r/<CR>x", ":s/ /\\r/g<CR>x", ":%s/e/\\r/<CR>x", ":%s/ /\\r\\r/g<CR>x", ":s/o/<\\r>/<CR>x",
    "qaxjq@a.", "qaA!<Esc>jq@aj.", "qadwq@aj.", "xqajq@a.", "qaqqaxj@aq@a", "qaqqa~l@aq@a", "qaxq2@a.u",
    ":s/o\\|a/X/g<CR>", ":%s/e\\|o/[&]/g|1<CR>", ":s/ /|/g|s/|/+/<CR>", ":s#o#|#g<CR>", ":s/x/y/|norm! x<CR>",
]


def build_cases(filter_text):
    cases = []
    for tname, (lines, eol) in TEXTS.items():
        for (cl, cc) in CURSORS[tname]:
            cl = min(cl, len(lines) - 1)
            cc = min(cc, max(len(lines[cl]) - 1, 0))
            seqs = []
            seqs += ["ma" + m if m in ("`a", "'a") else m for m in MOTIONS]
            for op in OPERATORS:
                for m in ["w", "e", "b", "$", "0", "j", "k", "G", "gg", "}", ")", "%", "fa", "ta", "iw", "aw", "i(", "a\"", "ip", "l", "h", "2w", "W", "B"]:
                    seqs.append(op + m)
                for o in OBJECTS:
                    if op in ("d", "c", "y"):
                        seqs.append(op + o)
            seqs += ["v" + o + "d" for o in OBJECTS]
            seqs += COMMANDS
            for keys in seqs:
                # Change commands end in Insert mode: return to Normal.
                if keys.startswith("c") and "<Esc>" not in keys and not keys.startswith(("cc", "c$")):
                    keys = keys + "X<Esc>"
                elif keys in ("cc", "s", "S", "C") or (keys.startswith("c") and "<Esc>" not in keys):
                    keys = keys + "X<Esc>"
                if keys in ("s", "S", "C"):
                    pass
                if keys in (";", ",", "d;", "d,") or "gc<CR>" in keys:
                    continue
                # `*` on an empty line falls back to the previous search in
                # an undocumented way; covered by unit tests instead.
                if tname == "blank" and keys in ("*N", "#n"):
                    continue
                # Vim matches `\n` after the last line even when the file has
                # no final line break; VTCode's buffer has nothing there.
                if not eol and "\\n" in keys and cl == len(lines) - 1:
                    continue
                name = f"{tname}@{cl},{cc}:{keys}"
                if filter_text and filter_text not in name:
                    continue
                cases.append({"name": name, "lines": lines, "eol": eol, "cursor": [cl, cc], "keys": fix_insert(keys)})
    # Deduplicate names.
    seen = set()
    out = []
    for c in cases:
        if c["name"] in seen:
            continue
        seen.add(c["name"])
        out.append(c)
    return out


def fix_insert(keys):
    """Commands that leave Insert mode open get an <Esc>."""
    opens = ("s", "S", "C", "cc", "c$", "o", "O", "A", "I", "i", "a", "R")
    if keys in opens:
        return keys + "X<Esc>"
    return keys


def vim_string(keys):
    """Vim notation to a double-quoted Vim string with <> escapes."""
    out = []
    i = 0
    while i < len(keys):
        c = keys[i]
        if c == "<":
            m = re.match(r"<(?:[CSAMD]-)*(?:[A-Za-z][A-Za-z0-9]+|[^<>\s])>", keys[i:])
            if m:
                out.append("\\" + m.group(0))
                i += len(m.group(0))
                continue
        if c in "\\\"":
            out.append("\\" + c)
        else:
            out.append(c)
        i += 1
    return '"' + "".join(out) + '"'


SCRIPT = r"""
{setup}
let s:cases = json_decode(join(readfile('{cases}'), "\n"))
let s:out = {{}}
for s:c in s:cases
  enew!
  call setreg('"', '')
  for s:r in split('abcdefghijklmnopqrstuvwxyz0123456789-', '\zs')
    call setreg(s:r, '')
  endfor
  delmarks!
  let @/ = ''
  " The initial text is not an undoable change.
  set undolevels=-1
  call setline(1, s:c.lines)
  set undolevels=1000
  call setcursorcharpos(s:c.cursor[0] + 1, s:c.cursor[1] + 1)
  try
    call feedkeys(eval(s:c.keyexpr), 'xt')
  catch
  endtry
  let s:pos = getcursorcharpos()
  let s:out[s:c.name] = {{'lines': getline(1, '$'), 'cursor': [s:pos[1] - 1, s:pos[2] - 1], 'reg': getreg('"'), 'regtype': getregtype('"')}}
endfor
call writefile([json_encode(s:out)], '{out}')
qa!
"""


def run_batch(vim, batch):
    with tempfile.TemporaryDirectory() as d:
        cases_path = Path(d) / "cases.json"
        out_path = Path(d) / "out.json"
        script_path = Path(d) / "run.vim"
        payload = [dict(c, keyexpr=vim_string(c["keys"])) for c in batch]
        cases_path.write_text(json.dumps(payload), encoding="utf-8")
        script_path.write_text(SCRIPT.format(
            setup="\n".join(SETUP),
            cases=cases_path.as_posix(),
            out=out_path.as_posix(),
        ), encoding="utf-8")
        subprocess.run([vim, "-N", "-u", "NONE", "-i", "NONE", "-n", "-Es", "-S", script_path.as_posix()],
                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=600)
        if not out_path.exists():
            raise RuntimeError("vim produced no output for batch starting " + batch[0]["name"])
        return json.loads(out_path.read_text(encoding="utf-8"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--vim", default=shutil.which("vim") or "vim")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--filter", default="")
    args = ap.parse_args()
    cases = build_cases(args.filter)
    batches = [cases[i:i + 200] for i in range(0, len(cases), 200)]
    results = {}
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        for r in pool.map(lambda b: run_batch(args.vim, b), batches):
            results.update(r)
    corpus = []
    for c in cases:
        exp = results.get(c["name"])
        if exp is None:
            continue
        corpus.append({**c, "expect": exp})
    data = json.dumps(corpus, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    # mtime=0 keeps the archive byte-identical across regenerations.
    with gzip.GzipFile(OUT, "wb", compresslevel=9, mtime=0) as f:
        f.write(data)
    print(f"{len(corpus)} cases written to {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    sys.exit(main())
