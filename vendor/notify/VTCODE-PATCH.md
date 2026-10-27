# Local notify 7.0.0 patch

Source: the published notify 7.0.0 crate, CC0-1.0 (see LICENSE-CC0).
The original source, manifest, and README are retained.

`Config::with_open_events(false)` lets the Linux inotify backend omit IN_OPEN
when installing watches. Its default remains true, preserving upstream behavior
for consumers that use open-access notifications. The option is construction-only
and has no effect on backends that do not subscribe to file opens.

VTCode's headless watcher discards read-only access events. Subscribing to them
first still wakes the native backend and allocates events, causing CPU usage when
another process reads source files or ignored caches. The headless watcher opts
out at registration instead. IN_CLOSE_WRITE, modifications, creates, removals,
renames, recursive directory tracking, and overflow handling are unchanged.

The newer upstream EventKindMask API provides broader event filtering in notify
9 release candidates. This patch keeps the existing stable backend and adds only
the narrow switch needed here, without changing the other platforms' backends.

`remote::watch` has Linux-native regression coverage for quiet reads followed by
writes and renames. The default open-event behavior is tested separately.

Upstream references:
- https://github.com/notify-rs/notify/blob/main/notify/src/config.rs
- https://github.com/notify-rs/notify/blob/main/notify/CHANGELOG.md
