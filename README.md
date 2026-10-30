# VTCode

**A code editor for your terminal, inspired by VS Code.**

Built in Rust for Windows and Linux. Under active development.

[Download](https://github.com/im-henri/vtcode/releases) · [Getting started](#getting-started) · [Animations](#animations)

![VTCode with split editors, a file explorer, and minimaps](screenshots/tiling.png)

## Features

- **Edit:** desks and collapsible tab groups, split panes, multiple cursors, snippets, folding, minimaps, bracket colors, sticky scroll, and keyboard macros.
- **Find and navigate:** quick file/symbol search, back/forward history, and find/replace across files. Explorer's **Find by Name…** filters filenames with substring, fuzzy, or regex matching; **Find in Folder…** searches file contents.
- **Delete files:** larger deletions show preparation, item counts, and a **Cancel** button while the editor stays usable. **File: Cancel Deletion** is also available in the Command Palette. Cancellation stops remaining items; it does not restore items already deleted.
- **VTComplete:** built-in suggestions learned from your code, with no language server required.
- **LSP features:** completion with documentation, hover, signature help, diagnostics, definitions, references, rename, code actions, inlay hints, and call/type hierarchies, depending on the language server.
- **Format:** choose LSP or an external command per language, with format-on-save support.
- **Git:** stage, unstage, or discard multiple files; commit, branch, stash, fetch, pull, and push. Source Control's **Show untracked files** choice is saved immediately as a User preference, follows an existing Workspace override, and also updates the change-count badge and Explorer decorations. Configure either scope in **Settings → Source Control → Show Untracked Files**. Review inline or side-by-side diffs, blame, and commit history; hover inline blame for commit details and the full message. **Disable** in Source Control pauses Git integration for the workspace; **Settings → Source Control → Enable Source Control** also supports a User-wide default.
- **Repositories:** nested Git projects appear separately in Source Control, with independent changes and commit messages. Repositories above the opened folder require **Open Repository** before their changes are included. Click a repository header to select or collapse it, use **×** to close it, and **Actions → Open Repository…** to reopen a project. Repository visibility is remembered per workspace.
- **Terminals and previews:** multiple terminals and runnable tasks; Markdown preview with tables, math, Mermaid diagrams, and Ctrl+Click links.
- **Agent chat:** connect Agent Client Protocol (ACP) agents, resume conversations, review changes, and respond to permission requests inside the editor.
- **Customize:** user/workspace settings, keybindings, VS Code color themes, optional animations, and [Wasm extensions](extension-api/README.md) for commands, views, and custom editors.

## Screenshots

<table>
  <tr>
    <td align="center">
      <strong>Agent Chat</strong><br>
      <a href="screenshots/agent_chat_ACP.png"><img src="screenshots/agent_chat_ACP.png" alt="ACP agent chat with suggested edits and inline diffs" width="480"></a>
    </td>
    <td align="center">
      <strong>Multiple Cursors</strong><br>
      <a href="screenshots/multicursor.png"><img src="screenshots/multicursor.png" alt="Editing several lines at once with multiple cursors" width="480"></a>
    </td>
  </tr>
  <tr>
    <td align="center">
      <strong>Markdown Preview</strong><br>
      <a href="screenshots/markdown_preview.png"><img src="screenshots/markdown_preview.png" alt="Markdown source and rendered preview side by side" width="480"></a>
    </td>
    <td align="center">
      <strong>Command Palette</strong><br>
      <a href="screenshots/command_palette.png"><img src="screenshots/command_palette.png" alt="Command palette searching for theme commands" width="480"></a>
    </td>
  </tr>
</table>

## Getting started

[Download a release](https://github.com/im-henri/vtcode/releases), extract it into a writable folder, and run:

**Windows**

```powershell
.\vtcode.exe path\to\project
```

**Linux**

```sh
./vtcode path/to/project
```

Run `vtcode --help` (or `.\vtcode.exe --help` on Windows) to list all command-line
arguments and examples without opening the editor. Invalid arguments print an
error and exit without opening the editor. Use `--` before a path starting with
`-`, for example `vtcode -- -notes.txt`.

**Linux:** x86_64 builds require glibc 2.28+ or 2.43+, matching the archive name.
Check with `getconf GNU_LIBC_VERSION`; stock Alpine/musl needs a different build.

Keep `settings.json` and `.vtcode/` beside the executable when updating.

| Default shortcut | Action |
| --- | --- |
| `F1` | Command palette |
| `Ctrl+P` | Open a file by name |
| `Ctrl+,` | Settings |
| `Ctrl+F` / `Ctrl+H` | Find / replace in the editor |
| `Ctrl+Backspace` / `Ctrl+Delete` | Delete the previous / next word |
| `Alt+Left` / `Alt+Right` | Navigate back / forward |
| `Ctrl+Space` | Request suggestions; toggle LSP documentation |
| `F12` / `Shift+F12` | Definition / references |
| `Shift+Alt+F` | Format document |
| `Ctrl+Shift+G` | Source Control |
| `Ctrl+Shift+I` | Agent Chat |

Reopening a closed file during the same workspace session restores its cursor
and scroll position, including when opening it from the explorer or file picker.

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

Some terminals send the same byte for `Ctrl+H` and `Ctrl+Backspace`. VTCode
prioritizes deletion when they cannot be distinguished; use **Edit → Replace**
or the command palette instead. Extended key reporting keeps both shortcuts distinct.

Rename terminal tabs by right-clicking a tab and choosing **Rename Terminal…**,
or use **Terminal → Rename Terminal…** or the command palette. Names stay with
the session when moving between the side panel and an editor tab.

**View → Auto-hide Sidebar** hides the sidebar until you move the pointer to
its edge or the Activity Bar. It stays open while you use sidebar fields or menus.
This is off by default and can also be changed in **Settings → Workbench → Layout**
with **Auto-hide Sidebar** (`sidebarAutoHide` in `settings.json`).

**File Associations** also updates file icons throughout the Explorer, editor tabs,
breadcrumbs, Quick Open, and Source Control. Explicit associations override built-in
file types; for example, associating `*.rs` with C++ uses the C++ language and icon.

Large files are sampled for binary data before text loading. In SSH mode these
checks run on the remote node, which returns the binary-file notice when detected.
Binary editor extensions, such as Hex Viewer, remain available through **Open With**.

**Using VTCode over SSH?** For editing that stays local and responsive over a
slow connection, launch
`vtcode.exe --remote user@host`. To choose a folder or use a scheduler, add
`--remote-command 'submitcmd vtcode --headless /path/to/project'` (omit `submitcmd`
for a direct connection). `--ssh-command 'ssh -e none -tt -vv'` overrides the SSH
executable and flags. No second terminal is needed; the job's input/output stays
attached to this SSH connection.
Use `F1` for the command palette if its other shortcut is intercepted.

Over SSH, copying reaches your local clipboard through OSC 52 when your terminal
allows it, including VTCode's built-in terminal. Use your terminal's **Paste**
shortcut for text copied in other applications; VTCode also remembers copied and
pasted text across editor tabs without needing a remote desktop clipboard.

## VTComplete

VTComplete is the built-in completion engine, separate from LSP suggestions.
It learns identifiers and calling patterns from your code without a language
server. Its popup is labeled **VTComplete**; language-server results are labeled
**LSP suggestions**. **Tab** inserts a suggestion, **↑/↓** selects another, and
**Esc** dismisses the popup. **Ctrl+Space** requests suggestions explicitly.

Click **VTComplete** in the status bar or run **VTComplete: Manage File Learning**
to manage learned files and resource limits. **Files you open** is the default;
**Entire workspace** opts into gradual, ignore-aware discovery for local projects.
**Pause learning** keeps learned suggestions available. **File learning CPU**
controls learning and cache loading, not prediction. Background work sleeps when idle.

Preferences live under **Settings → Text Editor → VTComplete**. The shared toggle
for automatic VTComplete and LSP popups is under **Text Editor → Suggestions**.
Automatic VTComplete suggestions start with one character and useful code
punctuation; these triggers are configurable. The default source-file limit is
**100 MiB**, adjustable up to **1 GiB**.

Saved-file learning persists between sessions for local and remote workspaces.
Remote learning uses already loaded files, adds no SSH source reads, and never
scans unopened files.

## Language servers

Install a language server separately, then open **Settings → Language Servers →
Manage Servers** or click **LSP** in the status bar. Add its language ID,
executable, file patterns, and arguments. Choose **User** or **Workspace** settings;
the workspace server list overrides the user list.

Use Pyright for Python, clangd for C/C++, or another server supporting LSP over
stdin/stdout. Diagnostics wait for a pause in typing;
adjust **Diagnostic Delay (ms)** in Language Servers settings.

**Formatting is independent of LSP.** The first format request asks for a formatter
per language and remembers the choice. Use **External Command** for tools such as
Black, or **Language Server** for servers such as clangd. External tools must be installed
separately. Change the choice through **Configure Formatter…** in the command
palette or **Settings → Files → Formatters → Configure…**.
External commands can edit a temporary file or return formatted text on stdout.
File mode starts with `{file}` as an argument, includes unsaved edits, and applies
formatting as one undoable change; it writes the original file only on Save.

## Customization and tools

- **Settings:** use **User** for defaults and **Workspace** for project overrides; edit through the UI or JSON.
- **File types:** click the status bar language → **Configure File Associations…**, or open **Settings → Files → File Associations**. Map extensions (`*.h`) or filenames (`Jenkinsfile`) to a language globally or per workspace; edit or remove rules in the same dialog. Applies to highlighting, LSP, and formatter selection.
- **Themes:** choose from 29 built-in palettes in **Settings → Workbench → Appearance → Color Theme**, including One Dark Pro, Dracula, GitHub Dark, Tokyo Night, Catppuccin, Nord, Night Owl, Ayu, Rosé Pine, and Gruvbox. All built-ins work offline; see the [full catalog and sources](themes/SOURCE.md). To add more, run **Preferences: Install Color Theme from File or Link** and paste a [VS Code Themes](https://vscodethemes.com/) link or a local `.vsix` path. Use **Delete…** beside an installed variant (or the Delete key) and confirm to remove it. Bundled themes are protected. Removals persist; reinstalling a pack restores its removed variants.
- **Syntaxes:** run **Preferences: Install Syntax from File** with a local TextMate `.tmLanguage.json` or `.tmLanguage` file. Highlighting reloads automatically; URLs are not accepted. All languages use the same native TextMate tokenizer and VS Code theme rules. Grammar sources live in the root [grammars](grammars/README.md) directory: `grammars/bundled` is compiled into the executable, and the Rust, Python, and Git folders hold the VS Code overrides. Portable releases ship these assets in `grammars/` beside the executable.
- **Agents:** install an ACP-compatible agent separately, then run **Agent: Configure Agents** from the command palette.
- **Extensions:** install through **Extensions: Install Extension from GitHub**. See the [Wasm extension guide](extension-api/README.md) to build your own.
- **Performance & activity:** pin the live monitor while you work: CPU/memory trends, UI responsiveness, slow calls, worker queues, task timings, and workspace details. Resource sampling runs at most every 10 seconds while visible and the app is focused.

## Animations

Optional and off by default; enable them in **Settings → Animations**.
Cursor Smear, Fire Hazard, Whitespace Reveal, and Scope Bubble pause during
multicursor editing. **Scope Bubble: Background Fill** adds animated shading
after the scope outline finishes drawing.
**Scope Bubble: Language Rules** lets you choose a parsing style and delimiter
pairs (`{}`, `()`, `[]`) per language, with User and Workspace overrides.
For example, use C++ scopes for a custom syntax or add parentheses to C++.
Cosmetic redraws adapt to rendering speed on slower computers, leaving time
for input; the configured FPS remains the upper limit.

<table>
  <tr>
    <td align="center">
      <strong>Cursor Smear</strong><br>
      <img src="screenshots/animations/cursor-smear.gif" alt="An elastic cursor ribbon follows quick moves and reversals through Rust code" width="360">
    </td>
    <td align="center">
      <strong>Fire Hazard</strong><br>
      <img src="screenshots/animations/fire-hazard.gif" alt="Curling flames and rising embers follow the cursor while code and selections remain readable" width="360">
    </td>
  </tr>
  <tr>
    <td align="center">
      <strong>Whitespace Reveal</strong><br>
      <img src="screenshots/animations/whitespace-reveal.gif" alt="A filled ribbon sweeps across newly inserted indentation" width="360">
    </td>
    <td align="center">
      <strong>Scope Bubble</strong><br>
      <img src="screenshots/animations/scope-connector.gif" alt="Rounded tracing lines wrap both sides of the active scope through whitespace around its code" width="360">
    </td>
  </tr>
  <tr>
    <td colspan="2" align="center">
      <strong>Sliding Tab Indicator</strong><br>
      <img src="screenshots/animations/sliding-tabs.gif" alt="The active tab underline stretches and settles during quick tab switches" width="360">
    </td>
  </tr>
</table>

## Build from source

```sh
cargo run --locked --release -- path/to/project
```

For faster development builds with incremental compilation:

```sh
cargo run --locked --profile fastrelease -- path/to/project
```

To build portable packages of the current checkout, use
`scripts/build.ps1 --target all` on Windows, or
`sh scripts/build.sh --target linux --glibc both` on Linux. Use `--output-dir PATH`
to choose the destination and `--glibc 2.28` or `--glibc 2.43` for just one Linux variant.
Add `--skip-tests` to compile and package without running tests or installing test dependencies.

## License

[MIT](LICENSE) · [Third-party notices](THIRD_PARTY_NOTICES.md)
