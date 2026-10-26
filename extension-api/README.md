# VTCode extension API

VTCode runs `wasm32-unknown-unknown` extensions with the `vtcode` host imports.
API version `2` adds custom file editors to the existing commands, views,
overlays, settings, diagnostics, and text-edit APIs. API version `1` remains
accepted by the host. This SDK exposes `API_VERSION = "2"`.

Host calls are available when compiling for `wasm32`. Shared editor data types
also compile natively, so extension logic can be unit-tested without linking
the editor's Wasm host imports into a native test executable.

## Tagged runtime packages

Each extension has a GitHub repository and versioned GitHub releases.
Users choose **Extensions: Install Extension from GitHub**, paste the repository
link, then select a published version. `#tag=v1.2.0` or a GitHub release URL can
select a particular tag. Branches, source-tree links, bare tags without a release
package, and repositories without eligible releases cannot be installed.

The release must have a semantic version tag (such as `v1.2.0`) and an uploaded
asset named **`vtcode-extension.zip`**. GitHub supplies the asset's SHA-256 digest.
Its ZIP root contains `extension.json`, the prebuilt Wasm module, required runtime
assets, and license notices. Source code, Git history, build tools, and vendored
development dependencies stay in the repository. Git is not required to install.

```json
{
  "id": "example.custom-editor",
  "name": "Example Custom Editor",
  "version": "1.2.0",
  "repository": "https://github.com/owner/custom-editor",
  "apiVersion": "2",
  "main": "custom-editor.wasm"
}
```

The tag and manifest version must match; `apiVersion` is independent. Publish a
new version for corrections. An already installed tag cannot be replaced by a
different package. The picker recommends the highest stable semantic version,
labels prereleases, identifies the installed package, and supports deliberate
upgrades and downgrades. There are no automatic updates.

Public releases work without sign-in. For private repositories or authenticated
API access, VTCode uses `GH_TOKEN` / `GITHUB_TOKEN`, or your existing GitHub CLI
sign-in (`gh auth login`). Credential lookup happens only during an explicit
release lookup or download, with a five-second limit for the CLI. Tokens stay
out of URLs, logs, and installation metadata; redirects to asset-storage hosts
do not receive the authorization header.

Downloads stream to temporary disk storage in bounded chunks and are verified
against the selected asset's exact size and SHA-256 before extraction. The host
checks the manifest, repository identity, version, and Wasm host API before an
atomic replacement. Failures preserve the existing installation. Provenance in
`.vtcode-source.json` records the repository, tag, asset ID, and SHA-256.

Packages contain one root manifest. Absolute paths, traversal, links, special
files, case-colliding paths, nested extension manifests, Git metadata, and forged
installation metadata are rejected. Installed build scripts are never executed.
Compressed and unpacked packages are bounded at 256 MiB, with at most 4096 ZIP
entries and 32 MiB per Wasm module, to keep installation resource usage bounded.

The three example extension projects include `package.py`, an explicit
`package-files.json` allowlist, and a Release workflow. Their build commands also
accept `--package <zip-path>`. To publish, update the manifest/Cargo versions,
build and test, then push the matching tag. The workflow creates the GitHub
release and uploads only the listed runtime files. Existing published assets
are left unchanged. Locally built packages can be uploaded to that release too:

```sh
python package.py --tag v1.2.0 --output dist/vtcode-extension.zip
gh release create v1.2.0 dist/vtcode-extension.zip dist/vtcode-extension.zip.sha256 --verify-tag
```

## File-backed editors

`register_file_open_command(id, title, "vcd,fst")` offers a command for the
specified extensions in the file-open chooser, for both text and binary files.
Binary files also offer generic `register_binary_open_command` handlers;
duplicate extension/command pairs appear once.

Use `editor::open_document()` during a command invoked for a file. The workbench
authorizes the command's context path or the active file; a guest cannot request
an arbitrary path. An unsaved text buffer for that file must be saved or
discarded first. IDs are positive, scoped to the extension, and valid while its
runtime remains loaded. Opening the same path reuses its document.

`document_info` reports the path, 64-bit size, dirty/read-only state, and history
availability. `read_document(id, offset, count)` returns up to 1 MiB;
`read_document_into(id, offset, &mut buffer)` fills reusable storage and returns
the byte count without allocating a temporary read buffer. Only that prefix is
valid; buffer contents are unspecified on error. The host validates the whole
requested memory range before reading directly into it. Sequential reads reuse
the file position while retaining external-change checks.
`edit_document(id, offset, deleted, bytes)` replaces a range, preserving undo
history. An empty replacement deletes bytes; inserting at the end is supported.
Offsets must fit a nonnegative signed 64-bit integer. `changed_bytes` returns a
0/1 flag per returned byte, compared with the saved bytes at the same offset.

Use the constants in `editor::action` with `document_action`:

| Action | Behavior |
| --- | --- |
| `UNDO`, `REDO` | Return whether history changed the document |
| `SAVE` | Stream and atomically replace after external-change checks |
| `RELOAD` | Reopen disk content, refusing unsaved edits or an active transaction |
| `AMEND_NEXT_EDIT` | Fold the next edit into the preceding undo step |
| `BEGIN_TRANSACTION` | Start a group of edits without adding intermediate undo steps |
| `COMMIT_TRANSACTION` | Publish the group as one undo step |
| `ROLLBACK_TRANSACTION` | Restore the bytes and history before the group |

Do not nest transactions. Save, Save As, and Keep Unsaved refuse an unfinished
transaction; finish or roll it back first. Undo/redo are unavailable during a
transaction. History retains up to 1000 edit steps. File reads remain lazy, and
file handles referenced by undo snapshots survive atomic saves. If an external
writer mutates one of those files in place, the host reports an error rather
than reading changed bytes into an old snapshot.

Associate the document with a view using `editor::Frame { document: Some(id),
.. }`. The workbench then routes Save As, Save All, close confirmation, and exit
recovery to that document. `dirty` and `path` are host-owned; a guest cannot
forge them. Save failures preserve the open document. Recovery stores the
current bytes and the command which opened it, then invokes that command at the
next startup. Recovery does not persist guest state or undo history.

`editor::document_action(id, editor::action::RELOAD)` reopens a file after an
external writer replaces it. It refuses unsaved edits, active transactions,
and conflicts with an unsaved text editor. Save and reload return `false` on
success; undo/redo return whether the document changed.

## Terminal surfaces and input

The editor::open_text_document(name, text) function opens generated UTF-8 content
in a new unsaved text buffer. The name is a leaf filename, content is limited to
32 MiB, and normal Save As, close confirmation, and text recovery apply. This
does not grant filesystem access. Pending generated buffers share a 32 MiB limit.

The editor::clipboard_limited(max_bytes) function checks the clipboard snapshot
size before allocating. Capture a clipboard command in its input callback:
background task invocations have no input clipboard context.
Use current_view_event_limited(max_bytes) to reject oversized encoded events
before allocating them. Input callbacks receive proportional decoding fuel,
capped at the background task budget.
For paste events, editor::event_text_limited(max_bytes) captures the decoded
text directly. This avoids unescaping a large JSON string under Wasm task fuel.
The snapshot is scoped to the current input callback.

An extension which stages document edits in tasks may export
vtcode_prepare_document_action() -> i32. Before a workbench Save, Save As,
recovery, or discard operation, the host calls this optional barrier with task
fuel. Flush previously accepted input and pending edits, then return zero.
Nonzero results or traps refuse the workbench operation. Ordinary extensions
need not implement this export.

Use editor::set_input_pending(view_id, true) when accepting input into a guest
queue. Clear it when the queue and any source transactions settle or cancel.
The workbench waits before closing a view with staged input, allowing its final
dirty state to participate in close confirmation. This flag does not alter
document bytes or dirty state.

Register a normal view and its key/mouse/scroll handlers, then call
`editor::set_frame(view_id, &frame)`. Frames are structured lines of styled
spans, with a viewport size, optional title, cursor, and document ID. They use
theme roles rather than embedded ANSI escape sequences.

Foreground roles: `text`, `muted`, `accent`, `offset`, `changed`, `warning`,
`error`, `success`, `selection`, `cursor`, and eight distinguishable plot colors
`chart1` through `chart8`. Background roles: `panel`, `header`,
`selection`, `cursor`, `match`. Unknown roles use the editor defaults. Bold and
underline are supported. The host enforces readable contrast and clips drawing
to the viewport. Control characters in span text are rejected. Use Unicode
terminal column widths when computing layout.

Register `editor::register_resize_handler` and read JSON through
`current_view_event()`. All rich-view events include `viewId`:

- Keys: `code`, optional `text`, `shift`, `control`, `alt`. Codes include `char`,
  `left`, `right`, `enter`, `escape`, `function`, and `paste`.
- Workbench commands: `code: "command"`, with `text` such as `save`, `undo`,
  `redo`, `copy`, `cut`, `paste`, `find`, `find_replace`, `goto_line`,
  `select_all`, `find_next`, or `find_prev`.
- Bracketed paste: `code: "paste"`, `text` containing the pasted content.
- Pointer: `kind: "down" | "up" | "drag"`, absolute `column`/`row`, relative
  `viewColumn`/`viewRow`, `width`, `height`, and modifier flags.
- Passive pointer: opt in with `editor::register_hover_handler(view_id, export)`.
  That handler receives `kind: "move"` inside the view and `kind: "leave"` when
  the pointer exits, with the same coordinates, size, and modifiers. Relative
  coordinates on leave may be outside the viewport and saturate at zero for
  positions above or to its left. Ordinary mouse handlers keep down/up/drag only.
- Context menus: opt in with `editor::register_context_menu_handler(view_id, export)`.
  A right press inside a rich view sends `kind: "contextMenu"`, `button: "right"`,
  absolute and relative coordinates, viewport size, and modifier flags. The view
  owns rendering and dismissing its menu; this event does not begin pointer
  capture. Without this registration, right-click keeps its workbench behavior.
- Pointer capture: opt in with `editor::enable_pointer_capture(view_id, true)`
  for a rich view. A left press inside the view captures subsequent drags and
  release, including outside its bounds. Mouse payloads include `pointerInside`;
  outside coordinates saturate at zero above/left. A `kind: "cancel"` mouse
  event ends a gesture interrupted by a key, fresh press, view switch, or resize.
  Capture prevents a signal drag from selecting text or activating workbench
  controls when the pointer leaves the view. Views that do not opt in keep
  their existing routing.
  Consecutive captured drags with the same modifiers and inside/outside state
  coalesce to the latest position. Presses, releases, keys, wheel input, and
  view changes remain ordering barriers. Cancel events have zero width/height
  so they cannot overwrite a newer resize.
- Scroll: `direction` (-1 up/left, 1 down/right), `axis` (`vertical` or
  `horizontal`), `control`, `shift`, `alt`, coordinates and viewport size. `ctrl`
  remains an alias for `control` for compatibility with existing extensions.
- Resize: `kind: "resize"`, `width`, `height`.
- Workbench document actions: `code: "documentChanged"`; re-read document info
  and render the resulting state.
- Last tab closed: register `editor::register_close_handler`; `code: "closed"`
  is sent to that handler after the final
  copy of a view closes across editor groups. Save guest session settings and
  release parsed data here; the same view ID may be opened again later.

Tab, explorer-file, and docked-terminal drags remain workbench gestures,
including their release over a rich view. These drops create splits or transfer
tabs without invoking the view's mouse handler.

Input callbacks run on workers. Keystrokes preserve order while a callback is
busy; pending resize events and consecutive hover events are coalesced. Hover
coalescing never crosses a key, click, drag, scroll, or view-change boundary.
An outstanding resize is delivered once per viewport size until the view is
reopened; repainting an older or size-limited frame does not enqueue it again.
Input and due tasks share
the runtime fairly. Worker completion wakes the workbench; callbacks that only
queue work or advance an internal decoder do not redraw an unchanged surface.
Keep callbacks bounded:
the host supplies 10 million Wasm fuel units per UI call and limits guest memory
to 128 MiB. Frames are limited to 2 MiB, 512 lines, and 32768 total spans.
Clipboard reads are supplied for paste events, and writes return through the
UI thread. The guest has no WASI filesystem, process, or network capabilities.

For incremental work, call `editor::register_task_handler(export, interval_ms)`.
These handlers continue while the terminal is idle or unfocused, unlike
cosmetic tick handlers. Registered background task exports receive 250 million
fuel units per call for bounded decoding/indexing; ordinary callbacks and
cosmetic ticks retain the 10 million limit. A positive interval is clamped to at least 10 ms; zero
unregisters it. Unregister promptly when work finishes or is cancelled. Task
handlers continue until unregistered, including after the last view closes, so
extensions can finish cleanup. Cosmetic tick handlers still require an open
view (or an extension without views).
Task exports return `0` (or have no return value) on success. Input handlers may
also return `1` to indicate that they handled an event; tasks must use `0`.

Check `editor::task_should_yield()` between small units of incremental work.
It returns true after a 6 ms work slice or when fewer than 20 million fuel
units remain, and always returns true outside a registered background task.
Return normally, preserving progress for the next callback. This is cooperative:
the host still enforces the hard fuel limit, and each unit of work must be
bounded. The clock and fuel checks are host-owned; guests need no WASI clock.

[Hex Editor](https://github.com/im-henri/vtcode-ext-hex-editor) is a complete working
example with native behavior tests. VTCode keeps real-Wasm workbench integration
tests in its main repository.
