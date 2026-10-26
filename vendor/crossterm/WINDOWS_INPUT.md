# Local Windows input patch

The Windows reader accepts bracketed paste and Win32 input packets from
`ReadConsoleInputW`. vtcode enables VT input and requests Win32 input mode to
retain modifiers such as Ctrl+Shift+P. OpenConsole can mix raw text with encoded
Enter/Tab records in one paste, so the transport is decoded before paste data.

Incomplete sequences survive empty reads. A bare Escape in legacy VT input has
a 100 ms grace period; vtcode informs the reader when it requests Win32 mode,
so even its first packet survives a longer gap. In Win32 mode, physical Escape
uses its complete key packet. Incomplete packets and pastes are discarded after two
seconds without input. Physical Escape also cancels a paste immediately. These
limits apply to idle time, not the total duration of a large paste.

Paste accumulation is limited to 10 Mi UTF-16 code units. Oversized pastes are
discarded through their end marker, retaining only enough input to recognize
that marker. The editor separately enforces its 10 MiB UTF-8 insertion limit.

## Literal terminal control codes

An actual ESC followed by a valid Win32 input packet in **raw** pasted text is
indistinguishable from a packet inserted by OpenConsole. Interpreting neither
would reintroduce the Windows Terminal newline/tab corruption. Heuristics cannot
provide a lossless fix for both cases. Encoded clipboard payloads are decoded
only once, preserving packet-looking text inside them.

For clipboard text containing actual escape/control codes, use vtcode's
**Edit: Paste** command from its command palette or Edit menu. It reads the system
clipboard directly, bypassing terminal encoding (normal editor newline/indent
handling still applies). Ctrl+V also uses this path when the terminal forwards
the shortcut rather than intercepting it. Text containing the printed characters
`\x1b` or `ESC` does not have this ambiguity.

## Validation

`cargo test --offline --locked --bin vtcode paste` exercises the editor and
isolated Windows console/ConPTY integration tests. `input_keyboard::tests`
checks shortcut routing. Decoder unit tests are in `windows_paste.rs`; run
`cargo test --manifest-path vendor/crossterm/Cargo.toml --lib windows_paste::tests`
when upstream Crossterm's development dependencies are available.
