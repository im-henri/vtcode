use super::{Process, TerminalSnapshot, Work, Workspace, failure, p, trim_front};
use crate::acp::terminal_history::TerminalHistory;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const MAX_OUTPUT: usize = 1024 * 1024;

pub(super) struct Terminal {
    process: Process,
    input: mpsc::SyncSender<String>,
    pub snapshot: TerminalSnapshot,
    pub waiters: Vec<p::RequestId>,
    limit: usize,
    eof: usize,
    changed: bool,
    last_emit: Instant,
}

impl Terminal {
    pub fn spawn(
        id: String,
        request: p::CreateTerminalRequest,
        workspace: &Workspace,
        sender: mpsc::SyncSender<Work>,
        storage: &Path,
        lifetime: &super::Lifetime,
    ) -> Result<Self, p::Error> {
        let cwd = workspace.resolve(request.cwd.as_deref().unwrap_or(workspace.root()))?;
        if !cwd.is_dir() {
            return Err(failure("Terminal cwd must be a workspace directory"));
        }
        let display = std::iter::once(&request.command)
            .chain(&request.args)
            .map(|arg| {
                if arg.is_empty()
                    || arg
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\\'))
                {
                    serde_json::to_string(arg).unwrap_or_else(|_| arg.clone())
                } else {
                    arg.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let history = Arc::new(
            TerminalHistory::new(storage)
                .map_err(|e| failure(format!("Cannot record agent terminal output: {e}")))?,
        );
        let version = history
            .append(format!("[agent command] {display}\n[cwd] {}\n", cwd.display()).as_bytes())
            .map_err(|e| failure(format!("Cannot record agent terminal command: {e}")))?;
        let mut command = crate::acp::process::command(
            &request.command,
            &cwd,
            request
                .env
                .iter()
                .map(|v| (v.name.as_str(), v.value.as_str())),
        );
        command
            .args(&request.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = Process::spawn(&mut command).map_err(|e| failure(e.to_string()))?;
        lifetime.register(&process);
        let stdout = process.child.stdout.take().expect("piped stdout");
        let stderr = process.child.stderr.take().expect("piped stderr");
        let mut stdin = process.child.stdin.take().expect("piped stdin");
        let (input, writes) = mpsc::sync_channel::<String>(16);
        let input_sender = sender.clone();
        let input_id = id.clone();
        thread::Builder::new()
            .name("acp-terminal-input".into())
            .spawn(move || {
                while let Ok(text) = writes.recv() {
                    if let Err(error) = stdin.write_all(text.as_bytes()).and_then(|_| stdin.flush())
                    {
                        let _ = input_sender.send(Work::TerminalInputError(
                            input_id.clone(),
                            format!("Terminal input was not delivered: {error}"),
                        ));
                        break;
                    }
                }
            })
            .map_err(|e| failure(e.to_string()))?;
        start_reader(stdout, id.clone(), sender.clone())?;
        start_reader(stderr, id.clone(), sender)?;
        let limit = request
            .output_byte_limit
            .unwrap_or(MAX_OUTPUT as u64)
            .min(MAX_OUTPUT as u64) as usize;
        Ok(Self {
            process,
            input,
            snapshot: TerminalSnapshot {
                session_id: request.session_id,
                id,
                command: display,
                output: String::new(),
                truncated: false,
                exit_status: None,
                released: false,
                cwd,
                history: Some(history),
                history_version: version,
                killed: false,
                error: None,
            },
            waiters: Vec::new(),
            limit,
            eof: 0,
            changed: false,
            last_emit: Instant::now(),
        })
    }
    pub fn append(&mut self, bytes: Vec<u8>, eof: bool) {
        if eof {
            self.eof += 1;
        }
        if let Some(history) = &self.snapshot.history {
            match history.append(&bytes) {
                Ok(version) => self.snapshot.history_version = version,
                Err(error) => {
                    self.snapshot.error =
                        Some(format!("Terminal output recording failed: {error}"));
                    self.kill();
                }
            }
        }
        // Each reader preserves partial UTF-8 across OS reads, so chunk
        // boundaries cannot turn split multibyte characters into replacements.
        self.snapshot
            .output
            .push_str(&String::from_utf8_lossy(&bytes));
        self.snapshot.truncated |= trim_front(&mut self.snapshot.output, self.limit);
        self.changed = true;
    }
    pub fn complete(&self) -> bool {
        self.snapshot.exit_status.is_some() && self.eof >= 2
    }
    pub fn poll(&mut self) -> bool {
        if self.snapshot.exit_status.is_none() {
            match self.process.try_wait() {
                Ok(Some(status)) => {
                    let mut exit =
                        p::TerminalExitStatus::new().exit_code(status.code().map(|n| n as u32));
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        exit.signal = status.signal().map(|n| n.to_string());
                    }
                    #[cfg(not(unix))]
                    if !status.success() && status.code().is_none() {
                        exit.signal = Some("terminated".into());
                    }
                    self.snapshot.exit_status = Some(exit);
                    // A detached descendant can inherit the pipes even after
                    // the command exits. End the owned tree so wait_for_exit
                    // cannot hang indefinitely waiting for those pipe handles.
                    self.process.kill();
                    self.changed = true;
                }
                Err(error) => {
                    self.snapshot.exit_status =
                        Some(p::TerminalExitStatus::new().signal(error.to_string()));
                    self.process.kill();
                    self.changed = true;
                }
                _ => {}
            }
        }
        if self.changed
            && (self.complete() || self.last_emit.elapsed() >= Duration::from_millis(40))
        {
            self.last_emit = Instant::now();
            std::mem::take(&mut self.changed)
        } else {
            false
        }
    }
    pub fn kill(&mut self) {
        if self.snapshot.exit_status.is_none() {
            self.snapshot.killed = true;
            self.changed = true;
        }
        self.process.kill();
    }
    pub fn input_error(&mut self, error: String) {
        self.snapshot.error = Some(error);
        self.changed = true;
    }
    pub fn input(&mut self, text: &str) -> Result<(), p::Error> {
        if self.snapshot.exit_status.is_some() || self.snapshot.killed {
            return Err(failure("The terminal has already exited"));
        }
        if text.len() > 64 * 1024 {
            return Err(failure("Terminal input is limited to 64 KiB"));
        }
        self.input
            .try_send(text.into())
            .map_err(|_| failure("Terminal input is blocked or closed"))?;
        if let Some(history) = &self.snapshot.history {
            match history.input(text) {
                Ok(version) => self.snapshot.history_version = version,
                Err(error) => {
                    let message = format!("Terminal input recording failed: {error}");
                    self.snapshot.error = Some(message.clone());
                    self.kill();
                    return Err(failure(message));
                }
            }
        }
        self.changed = true;
        Ok(())
    }
}

fn start_reader(
    reader: impl Read + Send + 'static,
    id: String,
    sender: mpsc::SyncSender<Work>,
) -> Result<(), p::Error> {
    thread::Builder::new()
        .name("acp-terminal-output".into())
        .spawn(move || {
            let mut reader = reader;
            let mut buffer = [0; 8192];
            let mut pending = Vec::new();
            loop {
                let n = match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                pending.extend_from_slice(&buffer[..n]);
                let count = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(error) if error.error_len().is_none() => error.valid_up_to(),
                    Err(_) => pending.len(), // Invalid bytes are visibly replaced.
                };
                if count > 0
                    && sender
                        .send(Work::TerminalBytes(
                            id.clone(),
                            pending.drain(..count).collect(),
                            false,
                        ))
                        .is_err()
                {
                    return;
                }
            }
            let _ = sender.send(Work::TerminalBytes(id, pending, true));
        })
        .map_err(|e| failure(e.to_string()))?;
    Ok(())
}
