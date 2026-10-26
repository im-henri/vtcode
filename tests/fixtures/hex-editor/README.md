# Hex Editor workbench fixture

`extension.json` and `hex-editor.wasm` are test data built from the MIT-licensed
[Hex Editor extension](https://github.com/im-henri/vtcode-ext-hex-editor).
The implementation and its native behavior tests live in that repository;
VTCode uses this fixed Wasm module for host, installer, and workbench tests
without requiring another checkout or network access.

Source commit: `c7ae50a6d3da9d4cba9a28b5b15d156bf3d8b1a9`.
Wasm SHA-256: `693ae9bbcd03af3cc705faac38c812bac66b371b1bbbd09479c3558d2b047b5d`.

To update the fixture, run `python build.py` in the extension repository, copy
its manifest and rebuilt Wasm here, update the provenance above, and run:

```sh
cargo test --locked --bin vtcode extension_
```

Keep the fixture's `LICENSE` with the module. This directory is test data and is
not an installable extension package; install the extension from its Git link.
