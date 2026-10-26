# Validation record

Validated on Windows x64 with Rust 1.98.0 on 2026-10-10.
The checks below exercise the shipped release Wasm in VTCode, as well as the
native data model and actual rendered frames. Linux execution has not been
verified in this environment.

## Results

- 31 native extension tests passed.
- Native and wasm32-unknown-unknown Clippy passed with warnings denied.
- Formatting checks and the standalone release Wasm build passed.
- All 17 real-host Wasm integration workflows passed.
- Twenty-two actual-frame PNG previews were generated and inspected.
- Three release modules have identical SHA-256 hashes: the source package,
  local installation, and VTCode test fixture.

Release module: 1,925,114 bytes.

    821BE33B3177B25EA912C76AF1EA1F53604C20DB55410A8F8AD21478802236A3

Before this UI refinement, VTCode's release build and full regression suite
passed: 3,285 passed, 0 failed, 43 ignored, including the 17 tabular workflows.
A separate copy of the package also built and passed its then-current 29
native tests with only the vendored SDK and locked dependencies, using a custom
CARGO_TARGET_DIR. Those baseline checks were not repeated for the UI revision;
the current module was checked with the extension tests, host workflows, and
rendered previews listed above.

## Acceptance evidence

| Requirement | Evidence |
| --- | --- |
| Import and preserve data | Incremental CSV parsing is exercised with split sizes 1 through 29, BOM, CRLF, quoted delimiters, escaped quotes, multiline fields, and Unicode. Delimited round trips cover empty/control/Unicode values across four delimiters. JSONL tests cover sparse keys, null, nested objects, and large numbers. |
| Explore a virtual grid | Stable typed sort tests cover equal keys, missing values, large integers, and exact decimal identity. Real Wasm imports, filters, formulas, rectangular selection, clipboard commands, header sorting, and resizing run through VTCode's input and worker queues. |
| Profiles, groups, and comparison | Tests verify quantiles, histogram totals, unique counts, exact group drilldowns, sorted derived results, schema differences, duplicate-key errors, and numeric deltas. Real Wasm workflows exercise grouping and two-document comparison. |
| Edit and document lifecycle | Real-host tests cover cell edits, JSON paste, append/delete, transaction undo/redo, Save, Save As, failed saves, dirty close confirmation, recovery, and external-change conflicts. Source byte tests verify untouched records retain their original representation. |
| Export actual results | Tests verify filtered/sorted/computed export, full computed precision, named unsaved buffers, and unchanged source bytes. Budget failures never publish a partial result. Group and comparison exports use the active derived dataset. |
| Coherent interaction and layout | All modes and dialogs are rendered at widths 1, 2, 12, 24, 40, 80, 116, 160, and 400, and heights 1, 2, 6, 10, 16, 24, 40, and 160. Frame tests enforce terminal cell widths, safe characters, and cursor bounds. Interaction tests cover all four clickable tabs and Help in compact panes, a visible data row at minimum size, profile column buttons, bounded help scrolling, column-picker wheel navigation, and ignored blank choices. Twenty-two actual-frame PNG previews were generated and inspected. |
| Settings and insertion | A real Wasm workflow closes the view, creates a new App, and restores formulas, filters, column widths, visibility, and frozen columns. Insert clears filters, selects the appended record, and reveals an editable source column. Cancellation before insertion preserves the prior filter and source. |
| Cooperative work and budgets | Real Wasm tests import/sort/profile/filter 100,000 rows; paste 20,000 rows as one undo step; cancel an in-progress sparse 5,000-patch edit; save a queued 20,000-row paste; accept 2 MiB text and 1 MiB of NUL characters; reject an oversized encoded input without changing dirty state; and aggregate 20,000 groups. |
| Portable package | The standalone build uses no parent source path. The directory contains source, lockfile, vendored SDK, prebuilt Wasm, examples, tools, documentation, and dependency license notices. |

## Reproduce

From this extension directory:

    python tools/build.py --offline
    cargo clippy --locked --offline --all-targets -- -D warnings
    cargo clippy --locked --offline --target wasm32-unknown-unknown --lib -- -D warnings
    python tools/render_previews.py

To install in a workspace and refresh VTCode's real-module fixture:

    python tools/build.py --offline --install-local <workspace> --host-fixtures <vtcode-source>

From the VTCode source directory:

    cargo test --locked --offline --bin vtcode extension_tabular_tests -- --test-threads=1
    cargo test --locked --offline --bin vtcode -- --test-threads=4
    cargo build --locked --offline --release

Omit --offline when downloading dependencies for the first time. Pillow is
needed only for PNG previews. On Windows, use a normal filesystem account
and a writable TEMP/TMP directory outside a Git checkout. The restricted AppContainer used during
development refuses canonicalization and some atomic replacements, and
blocks local HTTP fixtures in unrelated host tests.

## Boundaries

The [README](README.md) documents limits and numerical behavior. Source data
is limited to 16 MiB and 250,000 rows, with additional cell, record, and memory
budgets. Formulas and statistics use finite double precision; source numeric
text, sorting, and typed key identity retain exact decimal semantics.
Clipboard text is limited to 4 MiB; export is limited to 32 MiB and available
memory. Limits produce errors rather than silently truncated results.

The release module requires the VTCode host additions for
generated text buffers, staged input, the document action barrier, and decoded
input snapshots. Older API 2 hosts without these additions cannot load it.
The extension source and prebuilt module are maintained in
[vtcode-ext-tabular-workbench](https://github.com/im-henri/vtcode-ext-tabular-workbench).
