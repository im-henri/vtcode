# Remote development over SSH

VTCode can run its UI locally while a headless VTCode on the remote host does the file, Git, search and terminal work. Editing stays responsive over slow connections.

## Connecting

**Using VTCode over SSH?** For editing that stays local and responsive over a
slow connection, launch
`vtcode.exe --remote user@host`. To choose a folder or use a scheduler, add
`--remote-command 'submitcmd vtcode --headless /path/to/project'` (omit `submitcmd`
for a direct connection). `--ssh-command 'ssh -e none -tt -vv'` overrides the SSH
executable and flags. No second terminal is needed; the job's input/output stays
attached to this SSH connection.

## Remote Explorer

Open **Remote** in the Activity Bar (or run **View: Open Remote** from `F1`)
to manage saved targets. Choose **+ Add**, enter a name and SSH host or config
alias, and optionally enter a remote folder. **Save & Connect** opens it;
**Save** keeps it for later. SSH authentication appears inside VTCode. Both
machines need matching VTCode builds, with `vtcode` on the remote host's PATH
unless you supply a custom command.

Targets open as live workspaces. Your previous folder, open buffers and tools
remain available through the directory title's workspace switcher. Connecting
to an already open target switches to that workspace. The current connection
appears above the target list; **Save Target** also saves a connection opened
using `--remote`.
Use **Close** in that section to close the remote workspace. When no other
workspace is open, VTCode returns to a local editor window.

Search by name, host or folder. Press `Enter` to connect, `F2` to edit, `N` to
add, `Delete` to remove, and `F5` to refresh. Right-click a target or choose
**⋯** for Connect, Edit, Duplicate and Remove. Removing a saved target keeps
its open workspaces connected. Forms support `Tab` / `Shift+Tab`, clipboard
editing, `Ctrl+S` to save, and `Esc` to cancel.

## Signing in

SSH sign-in opens in a full editor tab. Type at the prompt, then press `Enter`;
SSH deliberately hides password and passphrase characters. `Ctrl+V` pastes
into the prompt. Switch tabs to keep working while it connects, or close the
sign-in tab to cancel. Reconnect sign-in also uses an editor tab.

## Advanced connection options

**Advanced connection options** accepts the same remote and SSH commands as
the command line. A custom remote command replaces the default command and
the Folder field; include the folder in that command, for example
`submitcmd vtcode --headless /path/to/project`. The SSH command is the local
executable and flags, such as `ssh -e none -tt -p 2222`. SSH keys, config and
the SSH agent work as usual.

## Saved targets

Targets are saved in `.vtcode/remote-targets.json` beside the local executable,
shared by all of its workspaces. Keep this file when moving or updating VTCode.
Manual edits take effect after **Refresh**. Invalid files show an error while
keeping the last loaded targets visible. Changes to different targets merge;
if the same target changes elsewhere, reload it or save your draft as a copy.

```sh
vtcode --remote-target development
vtcode --remote-target "My server"
```

The flag uses the same SSH command, folder settings, and saved workspaces as
connecting to that target from the Remote sidebar. Use `--remote HOST` for an
ad hoc connection with `--remote-command` or `--ssh-command` overrides. Saved
state for ad hoc connections is keyed by the SSH invocation, including the
remote command.
Use `F1` for the command palette if its other shortcut is intercepted.

## Clipboard

Over SSH, copying reaches your local clipboard through OSC 52 when your terminal
allows it, including VTCode's built-in terminal. Use your terminal's **Paste**
shortcut for text copied in other applications; VTCode also remembers copied and
pasted text across editor tabs without needing a remote desktop clipboard.

## Terminal latency

When remote terminal input waits more than 200 ms for acknowledgement, the tab
shows a spinner and the cursor becomes the same spinner. Hover over the tab for
an explanation. Both spinners clear on acknowledgement, including at silent
password prompts.

---

[← Back to README](../README.md)
