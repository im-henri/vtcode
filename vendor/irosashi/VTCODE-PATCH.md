# Irosashi 0.3.0

This directory contains the crates.io 0.3.0 source, based on upstream commit
`51322fa8606e36250d023421a158f388756711ff` (MIT).

The tokenizer has three language-independent corrections to match
`microsoft/vscode-textmate` 9.3.2:

- Save the incoming anchor position when pushing a rule. Restore that position
  when popping it, rather than restoring the end of its own begin match.
- Apply injection rules during capture retokenization, as during normal scanning.
- Retokenize the exact captured prefix without appending a synthetic newline.

The application resolves token themes with the VS Code trie in
`src/textmate_theme.rs`; it does not use Irosashi's theme-rule ranking.

The embedded grammar catalog is owned by the application and lives in
`grammars/bundled` at the repository root. The registry's `include_dir!` path and
the corpus tests reference that directory instead of `assets/grammars`. The
grammar bytes and catalog metadata are unchanged. This vendored crate is built
as part of the VTCode repository, which supplies those assets.

Regression references cover every bundled grammar in four themes. Generate them
with `scripts/tests/generate_textmate_reference.cjs`; embedded dependencies are
registered before comparison so capture includes see the complete catalog.

Grammar hashes and upstream provenance are in `grammars/BUNDLED-GRAMMARS.json`
at the application root; `assets/provenance.lock.json` retains the upstream lock.
Additional grammar licenses and notices are distributed in
`grammars/TEXTMATE-GRAMMARS-NOTICE` at the application root.
