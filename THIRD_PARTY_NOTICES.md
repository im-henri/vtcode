# Third-party assets

vtcode ships the following upstream material. Retain these notices when
redistributing source or portable packages.

| Material | Source and notice |
| --- | --- |
| Native TextMate engine and embedded grammars | Irosashi 0.3.0 and Shiki TextMate grammars; pinned in [grammars/SOURCES.json](grammars/SOURCES.json) and [grammars/BUNDLED-GRAMMARS.json](grammars/BUNDLED-GRAMMARS.json). [Engine license](grammars/IROSASHI-LICENSE), [grammar catalog license](grammars/TEXTMATE-GRAMMARS-LICENSE), [individual grammar notices](grammars/TEXTMATE-GRAMMARS-NOTICE). Local tokenizer corrections are described in `vendor/irosashi/VTCODE-PATCH.md`. |
| Rust TextMate grammar and semantic token contributions | Pinned VS Code and rust-analyzer revisions in [grammars/SOURCES.json](grammars/SOURCES.json); [Microsoft license](grammars/Rust/VSCode-LICENSE.txt), [grammar license](grammars/Rust/Rust-Syntax-LICENSE.txt), [rust-analyzer license](grammars/Rust/Rust-Analyzer-LICENSE-MIT). |
| Semantic theme selector port and tokenizer reference fixtures | Microsoft VS Code and vscode-textmate, MIT; [Microsoft license](#vs-code-color-data-license). `src/scope_selector.rs` and reference generators in `scripts/tests/`. |
| Python TextMate grammars | VS Code 1.103.0 and MagicPython; [Microsoft notice](grammars/Python/LICENSE.txt) and [MagicPython notice](grammars/Python/MagicPython-LICENSE). The Python language manifest is adapted for vtcode's asset layout. |
| Bundled color themes | [Theme sources and per-project MIT licenses](themes/SOURCE.md); exact revisions, published archive hashes, and definition hashes are recorded in [themes/SOURCES.json](themes/SOURCES.json). Microsoft theme assets retain [themes/LICENSE](themes/LICENSE). |
| VS Code color defaults and test references | Microsoft VS Code commit `04d61b08def5469aaa7af86fba5a9415ebc2495f`; [MIT notice](#vs-code-color-data-license). `src/vscode_color_defaults.json` records the upstream source modules. |
| VS Code search behavior ports and regression references | Microsoft VS Code commit `959031245ebb1fe077e0d512e1397c0ae82006e4`; [MIT notice](#vs-code-color-data-license). Regression fixture generator: [tests/support/generate_vscode_search_fixtures.mjs](tests/support/generate_vscode_search_fixtures.mjs). |
| File ordering and workspace search ordering references | Microsoft VS Code commit `eaa5f41042f412308642339e8874c6bf13b3f01b`; [MIT notice](#vs-code-color-data-license). Comparers are in `src/file_order.rs`; reference generator: [tests/support/generate_vscode_ordering_fixtures.mjs](tests/support/generate_vscode_ordering_fixtures.mjs). |
| Patched Crossterm 0.29.0 | [vendor/crossterm/LICENSE](vendor/crossterm/LICENSE); local changes are documented in the source repository's `vendor/crossterm/VTCODE-PATCH.md`. |
| ACP Python examples used in tests | The source repository retains `tests/fixtures/acp-python-sdk/README.md`, its Apache-2.0 `LICENSE`, and the pinned archive metadata in `REVISION`. Test peers are not included in portable downloads. |

Rust dependencies retain their own upstream licenses and are pinned by
`Cargo.lock`. vtcode's original code is available under the [MIT license](LICENSE).

## VS Code color data license

MIT License

Copyright (c) 2015 - present Microsoft Corporation

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
