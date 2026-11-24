# Folders, workspaces and navigation

How VTCode opens folders, keeps several of them live at once, and helps you find your way around them.

## Opening files and folders

Running `vtcode` without a path reopens the folders that were open when VTCode
last ran on this device, in the same order and with the same folder selected.
Folders that no longer exist are skipped. When none were open, it opens an
empty editor with no folder attached. It never loads the launch directory's
settings or session, scans its Git repository, or starts its file watcher. Use
`vtcode .` to open the current directory explicitly, or choose **Open folder…**
in the empty window.

The same applies over SSH: `vtcode --remote HOST` (or a saved remote target)
whose remote command is a bare `vtcode --headless` reopens the folders last
open on that connection. It closes the default remote folder unless that folder
was one of them or has unsaved work. Naming a folder in `--remote-command`
opens just that folder. The record is `.vtcode/last-workspaces.json` beside
vtcode.
Loose files and tools can remain open while folders are added; their window state
is stored separately from folder sessions.

Use **File → Open File…** (also available in the command palette) to browse for
an individual file anywhere. You can also enter an absolute path, a path
relative to the folder shown in the browser, or a `file://` URL. In a headless SSH
session, the browser starts on **Remote**. Choose **Use This Device** to browse
local files. Save writes back to the original file on the device it came from.

Click the directory name at the top and use **Open folders** to switch folders,
or the adjacent **+** to add a folder. **File → Open Folder…** also adds a folder
and selects it; adding an already open folder returns to its live session. Each folder keeps
its unsaved documents, undo history, desks, splits, searches, Git state, terminals,
and agent conversations while another folder is selected. User settings are
shared, and each folder keeps its own Folder settings overrides.

## Background folders and activity

Inactive folders defer Git refreshes, searches, and editor UI
work. Switching back reuses cached state and processes pending changes. Terminals, agents, pending
saves, and background extension tasks keep running locally and over headless SSH.

A badge beside the title counts unread activity from other folders. Open the
picker to see each folder's latest event and whether it is working or needs
attention. Visiting a folder clears its folder badge; terminal and chat
views retain their own unread indicators until viewed. Terminal command completion
uses shell integration (OSC 133/633); terminal process exits are also reported.

## Closing folders

Use **Folders: Close Current Folder…** in the Command Palette or **Close current
folder…** in the directory picker to close a folder. This action appears only when
more than one folder is open. Closing preserves unsaved edits for reopening and stops
that folder's terminals and agents. Quitting applies Save, Discard, or Keep
Unsaved to all open folders. Live processes are retained while switching
within the running instance; closing the application ends them. Over SSH, added
folders are on the same remote host and use the existing headless connection.
Update the local and headless binaries together; multi-folder watcher cleanup
uses SSH protocol 28. Use Quit to exit VTCode when only one folder is open.

## Saved workspaces

**Saved workspaces:** a workspace is a named collection of one or more folders.
Choose **Save workspace…** in the directory picker to save the open folders.
Use the **Saved workspaces** tab to inspect a workspace, with each folder on its
own line, then choose **Open workspace**. Opening switches to exactly those folders
and restores the starting folder. Matching open folders keep their files, undo, terminals, and chat.
Before other folders close, a confirmation lists them; unsaved edits are
preserved for next time and their terminals and agents stop. Missing folders
leave the current folders open. Each folder restores its latest saved session.
Use **Update from open folders…**, **Rename workspace…**, and **Delete workspace…**
to manage saved workspaces. They persist in the portable `.vtcode/workspace-sets.json`;
remote workspaces belong to the current SSH
connection and can be reopened after connecting with the same launch command.

Reopening a closed file during the same workspace session restores its cursor
and scroll position, including when opening it from the explorer or file picker.

## Search

File search and workspace search each remember their last 50 queries per
workspace, including after restarting VTCode. Workspace search also keeps separate
histories for replacement text and the include/exclude paths. Use **↑/↓** in the
Find field or any workspace search field to recall history; **Alt+↑/↓** navigates
workspace search results.

In-file Find and workspace search remember separate Match Case, Whole Word,
Regular Expression, and Preserve Case options. Search shortcuts apply to the
focused search view; the two sets of preferences are also available in Settings.
Each file tab keeps its own Find/Replace fields, matches, and widget width.
Split editors display their Find bars inside their respective panes.
While a workspace search is running, click **■** in the Search header or press
**Escape** with search focused to stop searching and keep the results found so far.
Click **↻** to run the search again.

Workspace search waits 300 ms after typing by default. Change **Search: Debounce
(ms)** in **Settings → Features → Search**, or set
`search.searchOnTypeDebouncePeriod` in JSON. A value of `0` searches immediately.
Enter or **↻** runs a pending search immediately. Per-file Find remains instant.

Configure **Files: Exclude** in **Settings → Files → Exclusions** and
**Search: Exclude** in **Settings → Features → Search**. Both accept glob patterns
in User or Workspace scope. `files.exclude` hides paths in Explorer;
`search.exclude` applies to workspace text search and Quick Open and inherits
every `files.exclude` pattern without listing those inherited patterns in its
control. A `false` entry disables the same inherited or default pattern.

The file defaults are `**/.git`, `**/.svn`, `**/.hg`, `**/.jj`, `**/.DS_Store`,
and `**/Thumbs.db`. Search additionally excludes `**/node_modules`,
`**/bower_components`, and `**/*.code-search`. Settings globs are relative to the
workspace root; sibling conditions such as `{ "when": "$(basename).ts" }` are
supported. To filter Quick Open's recent-file history, use a pattern matching
the absolute path, such as `**/node_modules/**`.

Build output and other hidden files remain searchable unless an explicit pattern
excludes them. `.gitignore`, `.ignore`, parent ignore files, and global Git ignore
rules never filter these searches. The search view's include and exclude fields
further narrow the results. Local and remote SSH workspaces use the same rules.

**Find and navigate:** quick file/symbol search, back/forward history, and find/replace across files. Explorer's **Find by Name…** filters filenames with substring, fuzzy, or regex matching. Right-click a folder to search only that folder and its descendants (or a file to search its containing folder). Edit the **In:** path (Alt+P) and press Enter to change folders; relative and absolute paths are supported. Clear the path or use **Workspace** (Alt+W) to search the workspace. **Find in Folder…** searches file contents.

## Explorer

**Delete files:** larger deletions show preparation, item counts, and a **Cancel** button while the editor stays usable. **File: Cancel Deletion** is also available in the Command Palette. Cancellation stops remaining items; it does not restore items already deleted.

**File Associations** also updates file icons throughout the Explorer, editor tabs,
breadcrumbs, Quick Open, and Source Control. Explicit associations override built-in
file types; for example, associating `*.rs` with C++ uses the C++ language and icon.

Large files are sampled for binary data before text loading. In SSH mode these
checks run on the remote node, which returns the binary-file notice when detected.
Binary editor extensions, such as Hex Viewer, remain available through **Open With**.

## Editor and terminal tabs

Each unpinned editor tab has a visible **×** close button beside its label.

Rename terminal tabs by right-clicking a tab and choosing **Rename Terminal…**,
or use **Terminal → Rename Terminal…** or the command palette. Names stay with
the session when moving between the side panel and an editor tab.

Some terminals send the same byte for `Ctrl+H` and `Ctrl+Backspace`. VTCode
prioritizes deletion when they cannot be distinguished; use **Edit → Replace**
or the command palette instead. Extended key reporting keeps both shortcuts distinct.

## Terminal output

In terminal output, right-click a selection to copy it and clear the selection;
right-click without a selection to paste. Mouse-aware terminal applications receive
their own mouse events; hold **Shift** to use local selection and clipboard actions.

## Ctrl+click paths

Hold **Ctrl** over a file path in terminal output to underline it, then
**Ctrl+click** to open the file. Compiler locations such as `src/main.c:12:7`
and `"src/main.c", line 12, column 7` also jump to the reported line and column.
This works in local and SSH terminals. Relative paths follow directory changes
reported by Bash, Zsh, Fish, and PowerShell shell integration.

Use **Ctrl+click** on paths in plain text, log, and Markdown source tabs too.
Relative paths start from the open file's folder. Bare relative paths also try
the workspace root when the target is missing. Line and column references work
in editor tabs as they do in the terminal.

**Ctrl+click** the path in a C or C++ include to open the header in local and SSH
workspaces. Quoted includes look in the current file's folder first. Angle-bracket
includes look in the workspace's `include/` directory and root first. Literal file
references in Rust, JavaScript, TypeScript, shell scripts, and CMake work the same
way, without requiring a language server.

## Layout

**View → Auto-hide Sidebar** hides the sidebar until you move the pointer to
its edge or the vertical Activity Bar in Left mode. It stays open while you use
sidebar fields or menus.
This is off by default and can also be changed in **Settings → Workbench → Layout**
with **Auto-hide Sidebar** (`sidebarAutoHide` in `settings.json`).

The Activity Bar uses its own row above the sidebar contents by default. The
editor tabs start alongside it directly below the menu. Hover an icon to see its
name, or use the dropdown for icons that do not fit. Switch with **View → Move
Activity Bar to Left** or use **Activity Bar Position** under **Settings → Workbench → Layout**
(`"activityBarPosition": "top"` or `"activityBarPosition": "left"` in `settings.json`).
In Top mode, the panel icon before **New Desk** or the desk picker hides and reopens
the sidebar. **View → Sidebar** (`Ctrl+B`) works in either layout.

## Detached tabs

On Windows Terminal and X11 desktops, drag an editor tab outside its terminal
window to open it in a separate terminal. Drop another tab onto that window to
add it, or drag a tab onto the main window to return it. You can open as many
detached windows as your desktop can host.

Detached windows show the tab strip and editor panes. They share the same
workspace, so unsaved edits, undo history, and terminal sessions travel with
their tabs. Closing a detached terminal returns its tabs to the main window.

Right-click a tab and choose **Move to New Window** or **Move to Window…**.
Both commands are also in the Command Palette. Use these commands on Wayland
or in terminals that report mouse coordinates only within their own viewport.

---

[← Back to README](../README.md)
