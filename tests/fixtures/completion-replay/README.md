# Completion editing replay fixtures

## Advanced designed mechanism probes

`advanced-predictions.json` contains 46 fixed development probes for
bidirectional identifier binding, retained-suffix insertion, no-op suppression,
and numeric continuations. The fixture was written and frozen before its first
paired baseline/candidate run. Its SHA-256 is
`480654b05da266bf16df6b9e481357a73af41c9fe2d4cc3c7a95092bb76b8d2a`.

These are designed mechanism checks, not independent held-out data, telemetry,
or a population accuracy study. The `split: holdout` value only selects the
existing transformation harness's dispatch path. No temporal-history examples
are included because that harness resets its direct cache for each case.
Correctness is evaluated on actual popup rank, no-op/quiet behavior, and
unexpected novel suggestions. Lexical candidate-admission recall is omitted:
numeric predictions enter after the existing admission instrumentation.

Run these separately from the established regression inventories using
`scripts/benchmark_completion_advanced.py --mechanism-inventory
tests/fixtures/completion-replay/advanced-predictions.json` with the other
required baseline, baseline provenance, candidate, source, inventory, and output
arguments.

## Earlier sequence prediction probes

`next-predictions.json` contains 45 designed development cases for the current
two-example policy: non-unit identifier counters, one-letter prefixes, bare
numbers, inline numeric lists, explicit-input relationships, and core context
fallback/specificity. It has 21 positive cases and 24 contradiction/context
controls.

The earlier `advanced-predictions.json` remains a historical fixture. Two of its
abstention expectations deliberately change in the new inventory: two numeric
items may now establish a progression, and an established input/output rule may
predict a different supplied input. Fourteen other old controls are preserved.

The new inventory was frozen before paired outputs were inspected. A subsequent
core specificity correction was developed from one of its misses, so the final
results remain development evidence. Numeric admission recall is omitted for
the same tracing reason as the earlier mechanism inventory.

## Recorded source fixtures

These are unmodified source snapshots from VTCode commit
`8bcdb5f78b6311c3217db3ab41e5adfa6bb34961`, retained so later production edits do
not silently change the evaluation corpus. They inherit the repository license.

| Fixture | Original path | SHA-256 |
| --- | --- | --- |
| `history.rs` | `src/completion_history.rs` | `2F50EDE4D4EFA58BCE7DA8DED88F43117C2E07435A9A9631BF99C233C48D482A` |
| `peer.py` | `tests/fixtures/remote_agent_peer.py` | `F8071B25B9EC7ACA22B58EEB668321C542ABF17B7E62AB81AEF7908CDA7D8C41` |
| `bridge.c` | `scripts/tests/fixtures/bridge_slow_disk.c` | `83A669A086B969701C35AB514571F891B0E939607D2030039E286CB3939B477F` |
| `diagnostic_bench.py` | `scripts/bridge_metadata_bench.py` | `50648AA02984ED08D3453DE69447063CE9C3A1749FFBB9CA6AB694228468AEA7` |

The fourth file is reported separately as diagnostic source replay. Its initial
results were observed during development, so it must not be called a holdout.
An additional final holdout must be selected after configuration freeze and gated
behind `VTCODE_LEARNING_HOLDOUT=1` until that final comparison. Supply its frozen
path in `VTCODE_LEARNING_HOLDOUT_FIXTURE`; fixture I/O occurs only in test setup,
outside prediction and latency measurement. Development and ablation runs do not
load it.

`holdout-selection.json` locks the fresh final selection using filenames and
sizes only. The selected independent hex-editor source and its license are
copied only after configuration/regression freeze; the manifest then records
the SHA-256. It is not part of development reports.

The deterministic replay scans left to right, samples every seventh eligible
repeated code identifier (5–80 ASCII bytes; existing streaming lexer excludes
comment/string targets), and alternates two/three typed prefix
characters, up to 64 events per file. At every event, the worker receives only
text already typed plus that prefix. The hidden identifier suffix and all future
source are absent from both the active Rope and index. Completed source is
revealed only as the chronological replay advances. No acceptance history is
fabricated from the answer. The `recorded` group sets all languages to `Unknown`
to check language-agnostic learning. The same chronological queries also run
with Rust, Python, and C in the separate `recorded-native` group, exercising
existing keywords and snippets. Its difficult subset is `recorded-native-hard`.
These paired language variants are the same source events, not independent new
examples. The final holdout's predeclared `Unknown` protocol is unchanged.

This is a reproducible simulation of appending code, not captured user telemetry.
It does not represent arbitrary cursor movement or all real editing habits.
The `recorded-hard` subset requires at least three distinct already-observed
identifiers sharing the typed prefix; membership uses only the visible prefix
and preceding source, and is fixed independently of either model's answer.
Designed insertion probes separately supply an already-visible bounded suffix
after the caret, with the held-out identifier removed. They cover relationships,
renaming, conflicting mappings, custom syntax and mixed indexed source files.

The benchmark reports retrieval admission recall, visible recall at ten, top
choice accuracy, reciprocal rank, unwanted automatic popups, cold/warm worker
compute time and estimated index memory separately. Worker byte accounting is
explicitly partial; it is not a process RSS measurement.

## Source-derived copy/rename replay

`vtcomplete_learning_rename_replay_tests.rs` defines the fixed
`source-copy-single-rename-v3` development protocol. It selects contiguous windows
of at most 32 lexical tokens from the three development fixtures, ending at a
code identifier that already occurs earlier in the window. The target must be
an argument between `(`/`,` and `,`/`)`, or a receiver before `.`/`::`, with
preceding receiver or simple-assignment evidence in source. It excludes declared
or type-position targets, callees, and a spelling also occurring in a string or
comment within the window. Another literal callee provides an anchor. These are
language-independent lexical selection rules; the predictor receives no labels.

Selection takes every third eligible event, at most 24 per source, interleaved
in fixed Rust/Python/C source order to a maximum of 64. Freeze the metadata-only
`source_rename_selection_manifest` output in `rename-selection-v3.json` before
running predictors. `VTCODE_LEARNING_RENAME_MANIFEST` selects that path; prediction
runs verify it has not changed. `VTCODE_LEARNING_RENAME_REPORT` selects results.

Each v3 replay supplies the full original source prefix through the target's
original line end, a newline, and an unfinished copied fragment beginning on the
whole line containing the selected witness start. Only code occurrences of the
target spelling consistently change to `targetNext`.
The new spelling already appears earlier in that copied context; only its first
two or three characters appear at the final caret. Other identifiers and callees
stay unchanged. Cold development runs start with an empty index and acceptance
history, and the future copy suffix is absent. Original and renamed spellings
share the typed prefix.

This simulates copying code and renaming a variable while continuing to type.
It is a synthetic transformation of actual source, not independently captured
user editing or another repository. Windows can overlap, and lexical selection
does not prove semantic symbol identity or compilation. Report it separately
from unmodified source replays and final held-out editing. Unknown/native
language runs are paired versions of the same transformed events.

Version 1 selected 32 events: 23 Rust and nine Python, with no C event meeting
the conservative rules. Its original example stopped at the target word, which
could leave its delimiter stack unfinished before starting the copy. Its frozen
manifest is `rename-selection-v1.json` (SHA-256
`A4DECCBC7F4FE7626701C5B2F40B37287E655D393E73062BA62BBA56134767A0`).
The diagnostic result was 5/32 baseline versus 0/32 learner in both paired
language groups. It is preserved rather than discarded.

Version 2 repairs only rendering of the already-written original. It asserts
that every selected source offset, target, prefix and window is identical to
version 1; no failed case is removed or replaced after seeing model outputs.
The new manifest is frozen before another prediction run. This corpus therefore
provides no C-renaming coverage and remains development data.

The frozen v2 manifest SHA-256 is
`6CF5A414FBEA28317DB9D00F7E383FBC3E311F703958F08B89B3A18C1EF11C48`.
Full-mode v2 also measured 5/32 baseline versus 0/32 learner, so the rendering
repair did not explain the failure. Later experiments can select a test-only
mode with `VTCODE_LEARNING_RENAME_MODE`; the selected mode is recorded in results
without changing this manifest or any case.

Version 3 keeps the same selected witnesses/case IDs and restores the baseline's
actual file-prefix lexical/receiver context. Version 2 still used a cropped
teacher, so its results remain diagnostic. The v3 manifest SHA-256 is
`95B88E74BFE6C29340430D73BF652E31F81BA6C955D405A8BE31B6B5F17A7E92`.
This rendering decision preceded v3 outputs. The selected production-candidate
mechanism measured 28/32 versus baseline 1/32 on v3, in both paired settings;
these remain development transformations with overlapping windows.

Fresh validation uses the frozen source copies described below.
`VTCODE_LEARNING_RENAME_FIXTURES` loads its two frozen source copies only during
test setup. The selector/renderer parameters remain fixed; empty coverage is
reported without replacing a selected file. The ignored ordinary append and
primary-summary tests use the same runtime fixture manifest and measured report
paths. No fresh source is selected or read by ordinary development runs.

`VTCODE_LEARNING_REPLAY_WARM_INDEX=1` tests the default opened-buffer learning
state: before each query, the exact visible Rope is submitted through
`observe_buffer_as` using the active source kind. A later index revision, Idle,
zero pending work and exactly one indexed file are required. Both engines then
query that same state. Every row records the warm flag, publication revisions
and visible-content SHA-256; no sentinel or hidden answer is indexed. Indexes
are isolated per program and paired language run. Warm case manifests use a
separate path/state from the unchanged cold manifest. Fresh primary validation
uses the warm native cohorts; cold/native/Unknown replications are secondary.

## Frozen final validation

`final-validation/fixtures.json` records exact copied bytes, SHA-256, licenses,
and provenance for the two new sources. The installed ACP Python SDK 0.12.1
contributed `__init__.py` (6,703 bytes); the waveform extension checkout at
`9dfa8c71aa1211ecad3d00a4fc44af838eabf15a` contributed `controls.rs`
(18,883 bytes). Selection used metadata only after the production/regression
freeze: first case-sensitive ordinal non-test source in the inclusive
5–30 KiB interval. No coverage or prediction-quality filter was applied.

The frozen candidate's warm primary cohort has 23 ordinary difficult events
and 16 copy/rename events. Errors fall from 21 to 7. All 66 ordinary native
append events improve from 16 errors to 13; ordinary difficult errors alone
fall from 8 to 7. Python contributes only two primary events, so the evidence
is weighted toward the independent Rust program. Neither paired language runs
nor cold states add primary observations. Fresh negative-popup coverage is zero.

Reproduce from the copied fixtures using
[`benchmark_completion_learning_validation.ps1`](../../../scripts/benchmark_completion_learning_validation.ps1)
and a newly built optimized test binary; choose a new output directory.
The verifier checks exact event sets, visible-buffer hashes, source membership,
and recomputed metrics.
