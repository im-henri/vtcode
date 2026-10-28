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

`INotifyWatcher::new_with_event_batching` adds an opt-in batch window after a
filesystem event arrives. The headless watcher uses 10 ms. No timer or thread
wakes while the filesystem is quiet, and watch/unwatch/shutdown commands remain
responsive during the window. Repeated data, metadata, and close-after-write
records are coalesced before allocating paths or calling application code.
Structural events reset coalescing to preserve renames and path replacements;
recursive subscriptions, all directory names, and queue-overflow reporting are
retained. The coalescing cache is bounded at 1,024 paths, and the read buffer is
64 KiB. Default constructors retain immediate event delivery.

The newer upstream EventKindMask API provides broader event filtering in notify
9 release candidates. This patch keeps the existing stable backend and adds only
the registration switch and event batching needed here, without changing other
platforms' backends.

`remote::watch` has Linux-native regression coverage for quiet reads followed by
writes and renames, updates inside common project directory names, hot-file
coalescing, path replacements, deletion, and new recursive directories. The
default open-event behavior is tested separately.

Upstream references:
- https://github.com/notify-rs/notify/blob/main/notify/src/config.rs
- https://github.com/notify-rs/notify/blob/main/notify/CHANGELOG.md
