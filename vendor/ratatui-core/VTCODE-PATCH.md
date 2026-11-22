# Local ratatui-core 0.1.0 patch

Source: the published ratatui-core 0.1.0 crate, MIT (Copyright (c) 2016-2022
Florian Dehau, Copyright (c) 2023-2025 The Ratatui Developers).
The original source, manifest, and README are retained.

Every interactive frame diffs the whole terminal buffer and repaints styled
regions, so these per-cell paths dominate the cost of scrolling:

- `Buffer::diff` measured the Unicode width of each cell's symbol three times
  (twice for the new cell, once for the old). It now measures each once, and
  answers a single printable-ASCII byte directly without the Unicode tables.
  Emitted updates, including wide-character and VS16 handling, are unchanged.
- `Buffer::set_style` indexed every cell through `self[(x, y)]`, recomputing
  and bounds-checking its position. It now styles each row as one slice.
- `Cell::eq` compared symbol strings before the colors and modifier. It now
  checks the plain fields first; the result is the same (`None` still equals
  `" "`).
