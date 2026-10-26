# VTCode extension API

VTCode runs `wasm32-unknown-unknown` extensions with the `vtcode` host imports.
API version `2` adds custom file editors to the existing commands, views,
overlays, settings, diagnostics, and text-edit APIs. API version `1` remains
accepted by the host. This SDK exposes `API_VERSION = "2"`.

## Source-backed packages

Every `extension.json` must declare a hosted Git `repository`, as well as its
`id`, `name`, `version`, `apiVersion`, and Wasm `main` (or LSP configuration).
Repository metadata is required for locally discovered extensions too. Existing
manifests must add it before they can load.

```json
{
  "id": "example.custom-editor",
  "name": "Example Custom Editor",
  "version": "0.1.0",
  "repository": "https://git.example/team/custom-editor",
  "apiVersion": "2",
  "main": "custom-editor.wasm"
}
```

Commit the extension's source and prebuilt module in the same package directory.
Users install through **Extensions: Install Extension from Git Link**. The host
clones HTTPS or SSH links using Git's existing credentials, copies tracked files,
validates every package, and records the commit in `.vtcode-source.json`.
Repositories may contain a single package or a collection. `#path=folder&ref=tag`
selects a package and preselects a branch/tag; GitHub, GitLab and Codeberg browser tree links
are also recognized. Use fragments for branch names containing `/`.

The installer lists remote Git tags and branches before downloading a package.
Semantic release tags such as `v1.2.0` are ordered by version with the latest
stable release recommended; prereleases, other tags, and development branches
are labeled separately. The installed version and selected commit are shown.
Choosing an older tag deliberately downgrades the package. There are no
automatic updates. A repository without tags offers development branches.

Each selection is pinned to the advertised commit. Annotated tags are resolved
to their underlying commits, and full refs distinguish a tag from a branch
with the same name. If a ref changes between selection and download, the host
refuses the installation and asks the user to refresh. `.vtcode-source.json`
records `version`, `reference`, `referenceKind`, and `commit`; older provenance
files without `referenceKind` remain readable.

For a single package, a semantic version tag must match `extension.json`'s
version. Commit source and prebuilt Wasm before tagging, then push the tag to
publish that version. `apiVersion` remains independent of the package version.
Do not force-update published version tags: replacing an already installed tag
with a different commit is refused. Publish a new version for corrections.

Pasting the same source link lets users change installed versions. A different repository
cannot replace the same extension ID. Installation is staged, preserves the
previous version on validation failure, and rolls back a failed publication.
Downloaded build scripts and Git hooks are never run. Packages cannot contain
symlinks, submodules, absolute paths, or parent-directory traversal. Installation
requires Git on PATH and noninteractive access to the repository. Each package
is limited to 256 MiB and each Wasm module to 32 MiB.

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
