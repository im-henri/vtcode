# Source Control

Built-in Git support: staging, commits, branches, diffs, blame, history, rebases and cherry-picks, for local and SSH workspaces.

## Overview

**Git:** stage, unstage, or discard multiple files; commit, branch, stash, fetch, pull, and push. Source Control's **Show untracked files** choice is saved immediately as a User preference, follows an existing Workspace override, and also updates the change-count badge and Explorer decorations. Configure either scope in **Settings → Source Control → Show Untracked Files**. Review inline or side-by-side diffs, blame, and commit history; hover inline blame for commit details and the full message. **Disable** in Source Control pauses Git integration for the workspace; **Settings → Source Control → Enable Source Control** also supports a User-wide default.

## Repositories

**Repositories:** nested Git projects appear separately in Source Control, with independent changes and commit messages. Repositories above the opened folder require **Open Repository** before their changes are included. Click a repository header to select or collapse it, use **×** to close it, and **Actions → Open Repository…** to reopen a project. Repository visibility is remembered per workspace.

## Smart diff

The default **Smart** diff uses distinctive lines to align revisions, pairs related edits even when lines were inserted nearby, and highlights changes within words. The two ends of a moved block share a number, such as **→1** and **←1**. Hover over either badge to see both line ranges, or click it to jump to the other end. With the cursor on a moved line, the command palette also offers **Go to Other End of Move**. Choose **Classic** in **Settings → Source Control → Diff Algorithm** to use the original Myers diff, or switch from the command palette with **View: Toggle Diff Algorithm (Smart / Classic)**.

## Gutter change previews

Click a colored gutter marker to preview a change. The copy icon beside each
block's **+ / −** controls copies its old lines. Right-click the preview for
copy, select-all, and change actions, including copying a single old line.
**Shift+F10** opens this menu from the keyboard; **Esc** dismisses the menu first.

## Rebase and cherry-pick

In **Source Control → Actions**, choose **Rebase Current Branch…** or
**Cherry-pick Commit…** to start an operation. Enter the base branch or commit;
cherry-picking also uses the selected Graph commit when the Graph is open.
Operations started in the terminal appear in Source Control too, including over
SSH. Resolve each conflict, save the file, and stage its resolution. The operation
card explains any remaining blockers and enables **Continue Rebase** or
**Continue Cherry-pick** when the repository is ready. **Skip Commit…** and
**Abort…** ask for confirmation before discarding resolution changes.

## Tabs and windows

In Source Control, choose **Actions → Open in Editor** to use it as a tab, or
**Open in New Window** to detach it directly. The tab’s toolbar and tab context
menu can move it to another window. Use **Actions → Move to Sidebar** from the
main window to put it back.

## Performance

Filesystem-driven Git updates wait for 600 ms of quiet, with updates about every two seconds during sustained builds. Batches containing only ignored files or hidden untracked files skip worktree status scans. Open files reload promptly, and saves, Git actions, and manual refreshes bypass this delay.

Large-repository status refreshes automatically use the installed Git executable for faster scans, with bundled libgit2 as a fallback when Git is unavailable. Startup shows the repository and current branch first, then its changes while remaining repositories are discovered, including over SSH. Remote status scans use a separate queue so discovery cannot hold up staging or diff requests.

Navigating back to Source Control reuses watched status, with complete revalidation after 30 seconds. Index-only changes update staged files without reloading commit history or branch counts. Repository discovery caches inherited ignore rules using Explorer's matcher and reuses directory listings to avoid redundant Git metadata probes.

---

[← Back to README](../README.md)
