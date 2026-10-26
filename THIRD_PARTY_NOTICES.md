# Third-party assets

vtcode ships the following upstream material. Retain these notices when
redistributing source or portable packages.

| Material | Source and notice |
| --- | --- |
| Sublime syntax definitions | `sublimehq/Packages` at `ceda1630fa4990371fad48def6ebef28993c3d22`; [syntaxes/LICENSE](syntaxes/LICENSE) and [Rust notice](syntaxes/Rust/LICENSE.txt). |
| Additional syntax definitions | Pinned repositories and revisions in [syntaxes/SOURCES.json](syntaxes/SOURCES.json); each affected language folder retains its upstream license and notices. |
| Python TextMate grammars | VS Code 1.103.0 and MagicPython; [Microsoft notice](syntaxes/Python/LICENSE.txt) and [MagicPython notice](syntaxes/Python/MagicPython-LICENSE). The Python language manifest is adapted for vtcode's asset layout. |
| Bundled VS Code themes | [themes/SOURCE.md](themes/SOURCE.md) and [themes/LICENSE](themes/LICENSE). |
| VS Code color defaults and test references | Microsoft VS Code commit `04d61b08def5469aaa7af86fba5a9415ebc2495f`; [MIT notice](#vs-code-color-data-license). `src/vscode_color_defaults.json` records the upstream source modules. |
| VS Code search behavior ports and regression references | Microsoft VS Code commit `959031245ebb1fe077e0d512e1397c0ae82006e4`; [MIT notice](#vs-code-color-data-license). Regression fixture generator: [tests/support/generate_vscode_search_fixtures.mjs](tests/support/generate_vscode_search_fixtures.mjs). |
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
