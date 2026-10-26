# Local Crossterm 0.29.0 patch

`MouseButton::Back` and `Forward` preserve XBUTTON1/XBUTTON2 in native console
records and X11 buttons 8/9 in SGR, X10 and rxvt reports. Press, release, drag and
modifier information remain distinct. The Win32 decoder reads individual raw
button bits because `crossterm_winapi::ButtonState::right_button()` also treats
both side-button bits as right-clicks. The input reader retains their pressed
state to recognize releases. `src/windows_input.rs` exercises native and VT
events through the real reader; the shared VT parser has protocol regressions.
Windows Terminal's omission of side-button reports is handled separately by
vtcode's `src/windows_mouse.rs`, not by changing terminal settings.

`SetUnderlineColor` emits colon-delimited SGR subparameters (`58:2::R:G:B`
and `58:5:N`). Windows Terminal treats semicolon-delimited underline RGB
components as separate attributes, causing dim text and spurious backgrounds.
Foreground/background encodings and underline reset (`59`) remain unchanged.
The regression in `src/terminal_output.rs` exercises actual Crossterm output
across syntax-color transitions within a search match.
Parser reference: https://github.com/microsoft/terminal/blob/main/src/terminal/adapter/adaptDispatchGraphics.cpp

Source: the published crossterm 0.29.0 crate (MIT; see LICENSE). Only the
manifest's unused example declarations are omitted from this source copy.

The Unix window-size probe also omits redundant closure parentheses so the
vendored library builds without an `unused_parens` warning.

The shared VT decoder distinguishes Backspace (normally DEL) from
Ctrl+Backspace (normally BS), instead of turning BS into the Ctrl+H Replace
shortcut. Unix uses the saved tty erase character to support terminals with
the opposite convention, without querying termios for every key. Native
Windows virtual keys and explicit CSI-u Ctrl+H reports remain distinct.
VTCode requests disambiguated key reporting on Unix and restores it on exit.
Terminals without that protocol cannot distinguish raw Ctrl+H from BS; the
byte follows the Backspace convention, and Replace remains in the Edit menu
and Command Palette. Tests use actual Unix PTYs (both erase conventions),
Windows console records, and CSI-u packets through the real input readers.

The VT parser also accepts xterm `modifyOtherKeys` (`CSI 27;mod;codepoint ~`)
and modified BackTab (`CSI 1;mod Z`). CSI-u alternate characters retain their
explicit Shift modifier so Ctrl+Shift+P is never routed as Ctrl+P. Raw ASCII
control punctuation uses `\\`, `]`, `^`, `_` rather than unrelated number
keys. Extended key packets preserve modifiers independently of tty VERASE;
invalid codepoints and malformed modifyOtherKeys packets are rejected.
Protocol reference: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Alt-and-Meta-Keys

Both Unix input backends retain escape sequences across short reads and polls.
Previously, an SSH read ending at the first ESC of Shift+Enter emitted Escape
and closed Find; the remaining bytes became text. The shared stream parser
allows 100 ms to disambiguate a bare Escape, using the existing poll timeout
rather than sleeping or blocking a shorter UI poll. Complete keys (including
encoded Escape) remain immediate. Regressions split CSI-u/modifyOtherKeys input
at every byte boundary through Linux PTYs and the Windows console reader,
including Find navigation, repeats/releases, and standalone Escape delivery.

`src/event/source/windows.rs` and `windows_paste.rs` recognize bracketed paste
in raw Windows KEY_EVENT_RECORDs before ordinary key translation. This keeps
UTF-16, CR/LF, tabs and repeated characters intact and emits one Event::Paste.
Text-bearing Alt key releases are Unicode composition events; older ConPTY
uses these for unmapped characters, including each half of an emoji. The paste
reader retains that text while ignoring ordinary key releases and empty
modifier/numpad events. The console regression replays both native and encoded
composition records even on newer Windows versions.
Ordinary keyboard, mouse, resize and focus handling use the upstream reader.
The decoder is enabled only with the existing bracketed-paste feature.

VT input alone loses modifiers on shortcuts such as Ctrl+Shift+P. vtcode also
requests Win32 Input Mode (`CSI ? 9001 h`), and the reader unwraps
`CSI Vk;Sc;Uc;Kd;Cs;Rc _` packets before applying the original native key parser.
This preserves virtual keys, modifier flags, releases and paste repeat counts.
Ctrl press/release records are also exposed as modifier-only key events so
Ctrl-hover links update without requiring pointer movement. VTCode consumes
these events without dispatching editor commands.
The transport and paste decoders are separate so an encoded paste payload is
never mistaken for shortcut input. Windows Terminal/OpenConsole can mix raw
paste text and delimiters with encoded Enter/Tab records in the same paste.
Every record passes through transport decoding regardless of how the paste
started, and a decoded payload is never recursively decoded. Console reads use
bounded batches to keep the more verbose transport responsive.

The Windows reader shares one persistent `CONIN$` handle between waiting and
reading. A signaled handle provides readiness without extra
`GetNumberOfConsoleInputEvents` calls. A reusable 4,096-record buffer reduces
console-host handoffs for verbose Win32/VT input backlogs; sparse reads return
as soon as any records are available. Paste/escape timeouts are checked when
waiting expires, without polling an otherwise idle console. Native console and
ConPTY regressions cover keyboard, mouse, paste, release ordering, and timeouts.

VTCode also supplies a fixed Windows mouse viewport origin while it owns its
alternate screen. Native mouse decoding then avoids opening `CONOUT$` and
querying screen-buffer information for each event. The console guard clears
the override on exit; applications that do not opt in retain the live viewport
lookup. Regressions check nonzero origins, primary-console scrollback,
alternate-screen entry, and restoration without changing native coordinates.

Legacy VT input can also encode navigation keys and shortcuts as sequences.
Zero-time polling drains the current bounded native read batch before reporting
no complete event. Returning after one UTF-16 record falsely hid queued complete
VT mouse/key packets and prevented the application from batching ready input.
Partial packets still return immediately once their buffered records run out.
The existing `src/event/sys/unix/parse.rs` parser is shared with this reader;
its platform imports and raw-mode query are adapted for Windows. Paste payloads
are accumulated separately so parsing large pastes stays linear in their size.

vtcode enables ENABLE_VIRTUAL_TERMINAL_INPUT after mouse capture (Crossterm's
mouse setup replaces the input mode). Its console guard restores the original
input/output modes on shutdown, setup errors and panic unwinding, and disables
the Win32 Input Mode it requested.

Upstream tracking: https://github.com/crossterm-rs/crossterm/issues/737
Windows behavior: https://github.com/microsoft/terminal/issues/18094
Win32 Input Mode: https://github.com/microsoft/terminal/blob/main/doc/specs/%234999%20-%20Improved%20keyboard%20handling%20in%20Conpty.md

Incoming bracketed pastes no longer have the old Windows ten-million-UTF-16-unit
limit. A shared collector stores UTF-8 once and moves that allocation into
`Event::Paste`, including on Unix. `Event::PasteStatus` reports progress every
512 KiB and explicit cancellation/allocation failures; no clipboard text is
logged. Collection uses fallible reservation. A failed paste frees its payload
and drains through the closing delimiter instead of replaying its remaining
control characters as shortcuts. `event::cancel_paste()` lets the workbench's
Cancel action request this safely from the input thread. Cancellation is checked
per read batch and, only while actively receiving, at most every 200 ms while
waiting. Discarding input and ordinary idle operation add no periodic wakeups.
Windows physical Escape also cancels the incoming paste. A slow paste never
expires into ordinary key handling after a two-second pause. Windows timestamps
native read batches once rather than reading the clock per pasted code unit.
The Win32 packet decoder parses bounded decimal fields directly from records,
without allocating a UTF-16 vector or String per key, and checks each prefix
unit only once. Native reads grow geometrically from 4,096 to at most 1,048,576
records only when an active paste fills its previous batch. This bounds native
staging at 20 MiB; it is not a paste-size limit. Records are decoded in place
instead of copied into a second converted queue. Extra staging is released
before returning to idle, including after cancellation, once buffered records
have been consumed. Failure to grow staging reports allocation failure and
drains through the paste delimiter using the existing allocation. Million-line Unicode
regressions cover the collector, Unix tty-sized reads, and the real Windows
console reader. ConPTY tests cover modified shortcuts, paste, and their ordering.

Large clipboard operations initiated through VTCode's Edit > Paste use the native
clipboard directly on Windows. Pasting through the terminal's own Paste action
instead travels through ConPTY. An unpaced, 21 MB Win32-input stress run with a
debug reader reproduced roughly 18 GB of OpenConsole working memory before
timing out, independently of the editor's Rope insertion. This is not evidence
that the editor allocated 18 GB, nor confirmation of the user's particular crash.
The million-line native-console regression bounds its producer's queue so CI
does not repeat that console-host exhaustion. Keep the Win32 reporting needed
for modified shortcuts; do not disable it as a bulk-paste workaround that loses
Ctrl+Shift+letter distinctions.

The follow-up memory investigation identified a second problem: small native
reads let older console hosts build up their expanded input queue much faster
than VTCode drains it. The 65,536-record reader crossed the 512 MiB test cutoff
on a 1.05 MB unpaced paste through the system console. Adaptive larger reads
complete the full 21 MB input without that runaway in quiet and loaded tests.
Timed polling also read the clock for every expanded native record, although
it could only return a timeout at a batch boundary. Check the boundary first
so `poll(timeout)` does not add millions of redundant timer queries; a direct
`read()`-only probe did not expose that extra bottleneck.
The input collector still stores only one UTF-8 payload, and the reader adds no
idle polling, per-character allocations, runtime memory sampling or mode toggles.
Regression tests in `src/windows_paste_memory_tests.rs` measure the host as well
as the reader, exercise cancellation and following modified shortcuts, and
terminate only their private processes if memory runs away. Their default
backend is explicitly the system console: portable-pty would otherwise find
unrelated `conpty.dll` versions on PATH. See
`docs/large-paste-memory-2026-10-24.md` for measurements and the opt-in stress run.

Remove the Cargo patch and this directory when an upstream release provides
equivalent Windows support. Regression tests live in src/windows_input.rs in
vtcode and exercise the actual console input reader in hidden console and
ConPTY children, including modified navigation/letter keys, command palette routing,
key releases, Unicode/multiline/bulk paste, and cleanup after panic unwinding.
The console tests also reproduce Windows Terminal's leaked Enter press/release
packets and cover all eight raw/encoded start/body/end combinations.
