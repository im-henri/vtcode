# TextMate grammars

All shipped grammar sources live in this directory.

- `bundled/`: the catalog of 257 TextMate grammars and its `index.json`, embedded
  into the executable by the native tokenizer. Edit these sources and rebuild
  VTCode to update the base catalog.
- `Rust/`, `Python/`, and `Git/`: the VS Code grammar packages that override
  entries in the base catalog. These are also embedded for startup and can be
  reloaded from the shipped files with **Developer: Reload Syntaxes**.
- `SOURCES.json`, `BUNDLED-GRAMMARS.json`, and the license/notice files: pinned
  provenance, hashes, and redistribution terms.

The tokenizer implementation remains in `vendor/irosashi`; it reads its bundled
grammar sources from this directory at build time. Runtime registry caches store
only the overrides, keeping the base catalog shared and avoiding duplicate data.
Portable releases include this directory beside the executable.

Color themes are separate assets in the repository's root `themes/` directory.
