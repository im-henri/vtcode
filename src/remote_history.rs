//! Demand-driven remote history retains the same revwalk as the native view.
//! Paging continues the walk; no repeated scan from HEAD or eager full history.
use super::*;
use crate::{
    app::GitCommitInfo,
    background::{BackgroundResultSender, CancellationToken},
    git_history::HistoryPage,
};
use std::sync::mpsc;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Page {
    offset: usize,
    result: Result<(Vec<crate::app::GitCommitInfo>, bool), String>,
}

impl Page {
    pub(super) fn fail(&mut self, error: String) {
        self.result = Err(error);
    }
}
struct Control {
    requests: mpsc::SyncSender<()>,
    cancel: CancellationToken,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub(crate) struct Manager {
    streams: std::collections::HashMap<u64, Control>,
    output: mpsc::SyncSender<Response>,
}
impl Manager {
    pub(crate) fn new(output: mpsc::SyncSender<Response>) -> Self {
        Self {
            streams: Default::default(),
            output,
        }
    }
    pub(crate) fn start(&mut self, history: u64, root: PathBuf) -> io::Result<()> {
        if self.streams.len() >= 4 || self.streams.contains_key(&history) {
            return Err(io::Error::other("Remote history stream limit reached"));
        }
        let (requests, receiver) = mpsc::sync_channel(1);
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let output = self.output.clone();
        std::thread::Builder::new()
            .name("headless-history".into())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                crate::git_history::stream_history(
                    root,
                    receiver,
                    worker_cancel,
                    |offset, result| {
                        output
                            .send(Response {
                                id: 0,
                                result: Ok(Reply::HistoryPage {
                                    history,
                                    page: Page { offset, result },
                                }),
                            })
                            .is_ok()
                    },
                );
            })?;
        self.streams.insert(history, Control { requests, cancel });
        Ok(())
    }
    pub(crate) fn next(&self, history: u64) -> io::Result<()> {
        self.streams
            .get(&history)
            .ok_or_else(|| io::Error::other("Remote history is closed"))?
            .requests
            .try_send(())
            .map_err(io::Error::other)
    }
    pub(crate) fn close(&mut self, history: u64) {
        self.streams.remove(&history);
    }
}

#[derive(Clone)]
pub(crate) struct Handle {
    client: client::Client,
    id: u64,
}

impl Handle {
    pub(crate) fn close(&self) {
        self.client.close_history(self.id);
    }
}

pub(crate) struct Stream {
    pub(crate) handle: Handle,
    repository: RemotePath,
    pages: mpsc::Receiver<Page>,
}

pub(crate) struct CachedHistory {
    count: usize,
    head: String,
    tail: String,
}

impl CachedHistory {
    pub(crate) fn from_commits(commits: &[GitCommitInfo]) -> Option<Self> {
        Some(Self {
            count: commits.len(),
            head: commits.first()?.full_hash.clone(),
            tail: commits.last()?.full_hash.clone(),
        })
    }
}

pub(crate) struct Read {
    pub root: PathBuf,
    pub generation: u64,
    pub cached: Option<CachedHistory>,
    pub requests: mpsc::Receiver<()>,
    pub cancel: CancellationToken,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.handle.close();
    }
}

impl Stream {
    pub(crate) fn register(remote: &workspace::Workspace, root: &Path) -> io::Result<Self> {
        let repository = remote.remote_path(root)?;
        let (id, pages) = remote.client.register_history()?;
        Ok(Self {
            handle: Handle {
                client: remote.client.clone(),
                id,
            },
            repository,
            pages,
        })
    }

    fn next(&self, cancel: &CancellationToken) -> io::Result<Page> {
        let client = &self.handle.client;
        client.call_cancellable(
            Command::HistoryNext {
                history: self.handle.id,
            },
            cancel,
        )?;
        loop {
            if cancel.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if !client.connected() {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "SSH connection lost while reading Git history",
                ));
            }
            match self
                .pages
                .recv_timeout(std::time::Duration::from_millis(50))
            {
                Ok(page) => return Ok(page),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }

    pub(crate) fn serve(self, read: Read, sender: BackgroundResultSender, log: diagnostics::Log) {
        let Read {
            root,
            generation,
            cached,
            requests,
            cancel,
        } = read;
        if requests.recv().is_err() || cancel.is_cancelled() {
            return;
        }
        let mut offset = 0;
        let mut cached = cached.as_ref();
        let result = (|| -> io::Result<()> {
            self.handle.client.call_cancellable(
                Command::HistoryStart {
                    history: self.handle.id,
                    repository: self.repository.clone(),
                },
                &cancel,
            )?;
            loop {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let page = self.next(&cancel)?;
                if page.offset != offset {
                    return Err(io::Error::other(
                        "Remote Git history returned an unexpected page",
                    ));
                }
                let (mut commits, has_more) = page.result.map_err(io::Error::other)?;
                let end = offset + commits.len();
                if has_more && commits.is_empty() {
                    return Err(io::Error::other(
                        "Remote Git history returned an empty page before the end",
                    ));
                }
                if let Some(prefix) = cached {
                    if offset == 0
                        && commits
                            .first()
                            .is_none_or(|commit| commit.full_hash != prefix.head)
                    {
                        cached = None;
                    } else if end < prefix.count && has_more {
                        offset = end;
                        continue;
                    } else {
                        // Replay the prefix on the worker to rebuild merge lanes.
                        // Append only new rows when the branch still has the same tip.
                        let skip = prefix.count - offset;
                        if commits
                            .get(skip - 1)
                            .is_none_or(|commit| commit.full_hash != prefix.tail)
                        {
                            return Err(io::Error::other(
                                "Git history changed while this folder was inactive",
                            ));
                        }
                        commits.drain(..skip);
                        offset = prefix.count;
                        cached = None;
                        if commits.is_empty() && has_more {
                            continue;
                        }
                    }
                }
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let page = HistoryPage {
                    root: root.clone(),
                    generation,
                    offset,
                    result: Ok((commits, has_more)),
                };
                if !page.publish(&sender) || !has_more || requests.recv().is_err() {
                    return Ok(());
                }
                offset = end;
            }
        })();
        if let Err(error) = result
            && !cancel.is_cancelled()
        {
            let target = if self.repository.0.is_empty() && !self.repository.is_absolute() {
                self.handle.client.hello().root.clone()
            } else {
                format!(
                    "{} · {}",
                    self.handle.client.hello().root,
                    self.repository.display()
                )
            };
            log.operation("remote Git", "Read history page", &target)
                .error(&error);
            let offset = cached.map_or(offset, |prefix| prefix.count);
            HistoryPage {
                root,
                generation,
                offset,
                result: Err(error.to_string()),
            }
            .publish(&sender);
        }
    }
}
