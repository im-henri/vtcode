These fixtures run the compiled waveform extension through VTCode's document API,
worker scheduler, input routing, settings persistence, and ratatui rendering.
Tests do not require the extension source directory or a Wasm compiler.

Refresh them from the standalone extension source with:

```text
python tools/build.py --host-fixtures PATH_TO_VTCODE_SOURCE
```

The extension source is in the separate
[Waveform Viewer repository](https://github.com/im-henri/vtcode-ext-waveform-viewer).
Run the build command from that checkout. These fixtures were built from
commit `4ca4ff58a324d62595e58de9ca469321257c26c3` for extension version 0.1.0.
The build also copies dependency licenses and fixture attribution so these
tests remain self-contained.
