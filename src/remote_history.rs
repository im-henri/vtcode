//! Demand-driven remote history retains the same revwalk as the native view.
//! Paging continues the walk; no repeated scan from HEAD or eager full history.
use super::*;
use crate::background::CancellationToken;
use std::sync::mpsc;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Page {
    offset: usize,
    result: Result<(Vec<crate::app::GitCommitInfo>, bool), String>,
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

struct Stream {
    client: client::Client,
    id: u64,
    pages: mpsc::Receiver<Page>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.client.unregister_history(self.id);
        // Stream ownership lives on the history worker, so orderly shutdown
        // cannot wait on the editor or lose Close behind a full input queue.
        let _ = self
            .client
            .notify_from_worker(Command::HistoryClose { history: self.id });
    }
}
pub(crate) fn serve_local(
    remote: workspace::Workspace,
    root: PathBuf,
    generation: u64,
    requests: mpsc::Receiver<()>,
    cancel: CancellationToken,
    sender: crate::background::BackgroundResultSender,
) {
    let mut offset = 0;
    let result = (|| -> io::Result<()> {
        let repository = remote.remote_path(&root)?;
        let (id, pages) = remote.client.register_history()?;
        let stream = Stream {
            client: remote.client.clone(),
            id,
            pages,
        };
        remote.client.call_cancellable(
            Command::HistoryStart {
                history: id,
                repository,
            },
            &cancel,
        )?;
        while requests.recv().is_ok() && !cancel.is_cancelled() {
            remote
                .client
                .call_cancellable(Command::HistoryNext { history: id }, &cancel)?;
            let page = loop {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                if !remote.client.connected() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "Remote history connection interrupted",
                    ));
                }
                match stream
                    .pages
                    .recv_timeout(std::time::Duration::from_millis(50))
                {
                    Ok(page) => break page,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(error) => return Err(io::Error::other(error)),
                }
            };
            offset = page.offset;
            let has_more = page.result.as_ref().is_ok_and(|(_, more)| *more);
            if cancel.is_cancelled()
                || sender
                    .send(crate::background::BackgroundResult::GitLogPage(
                        crate::git_history::HistoryPage {
                            root: root.clone(),
                            generation,
                            offset,
                            result: page.result,
                        },
                    ))
                    .is_err()
                || !has_more
            {
                return Ok(());
            }
        }
        Ok(())
    })();
    if let Err(error) = result
        && !cancel.is_cancelled()
    {
        let _ = sender.send(crate::background::BackgroundResult::GitLogPage(
            crate::git_history::HistoryPage {
                root,
                generation,
                offset,
                result: Err(error.to_string()),
            },
        ));
    }
}
