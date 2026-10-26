//! External formatters work on a buffer snapshot, never the user's on-disk file.
use serde::{Deserialize, Serialize};

#[path = "formatter_output.rs"]
mod output;
pub(crate) use output::{Prepared, prepare};
use std::{
    io::{BufWriter, Read, Write},
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Provider {
    #[default]
    Lsp,
    Command,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CommandMode {
    #[default]
    File,
    Stdin,
    FileStdout,
}

impl CommandMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::File => "Edit file in place",
            Self::Stdin => "Standard input → standard output",
            Self::FileStdout => "File → standard output",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Config {
    pub provider: Provider,
    pub mode: CommandMode,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

impl Provider {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Lsp => "Language Server",
            Self::Command => "External Command",
            Self::Disabled => "Disabled",
        }
    }
}

pub(crate) fn language(editor: &crate::editor::Editor) -> String {
    crate::file_associations::language_id(editor.language_name())
}

pub(crate) const TEMP_DIR_PREFIX: &str = ".vtcode-format-";

pub(crate) fn is_temporary_path(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str().to_str().is_some_and(|name| name.starts_with(TEMP_DIR_PREFIX))
    })
}

fn has_file_argument(config: &Config) -> bool {
    config.args.iter().any(|arg| arg.contains("{file}"))
}

pub(crate) fn validate(language: &str, config: &Config) -> Result<(), String> {
    if language.is_empty()
        || language
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("Enter a language ID, such as python, c, cpp or rust.".into());
    }
    if config.provider == Provider::Command && config.command.trim().is_empty() {
        return Err("Enter the formatter executable or interpreter path.".into());
    }
    if config.command.contains('\0') || config.args.iter().any(|arg| arg.contains('\0')) {
        return Err("Executable and arguments cannot contain NUL.".into());
    }
    if config.provider == Provider::Command
        && config.mode != CommandMode::Stdin
        && !has_file_argument(config)
    {
        return Err("Add {file} to Arguments so the formatter receives a file. No arguments are added automatically. For a tool that reads stdin, choose Standard input → standard output.".into());
    }
    Ok(())
}

fn read_output(
    mut stream: impl Read,
    limit: usize,
    overflow: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let keep = count.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..keep]);
        if keep != count {
            overflow.store(true, Ordering::Release);
        }
    }
    Ok(bytes)
}

fn write_source(source: &ropey::Rope, writer: impl Write) -> std::io::Result<()> {
    // Rope chunks are small. Coalesce them before writing to a working copy
    // (possibly on shared storage) or the formatter's stdin pipe.
    let mut writer = BufWriter::with_capacity(64 * 1024, writer);
    source.write_to(&mut writer)?;
    // BufWriter's destructor cannot report errors: finish before starting the
    // formatter, and propagate pipe failures just like file-write failures.
    writer.flush()
}

pub(crate) fn run(
    config: &Config,
    path: &Path,
    root: &Path,
    source: &ropey::Rope,
    lines: Option<(usize, usize)>,
    cancelled: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<Prepared, String> {
    let output = run_output(config, path, root, source, lines, cancelled, timeout)?;
    Ok(prepare(source, &output))
}

/// Running a tool and preparing editor edits are separate: a headless tool
/// returns text, while the local worker retains shared Rope chunks and undo.
pub(crate) fn run_output(
    config: &Config,
    path: &Path,
    root: &Path,
    source: &ropey::Rope,
    lines: Option<(usize, usize)>,
    cancelled: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<String, String> {
    run_output_tracked(config, path, root, source, lines, cancelled, timeout, None)
}

/// A service can retain a weak kill control without owning the worker or its
/// pipes. Shutdown can then stop an in-flight formatter before worker teardown.
pub(crate) fn run_output_tracked(
    config: &Config,
    path: &Path,
    root: &Path,
    source: &ropey::Rope,
    lines: Option<(usize, usize)>,
    cancelled: Arc<AtomicBool>,
    timeout: Duration,
    started: Option<&dyn Fn(vtcode::acp::ProcessKiller)>,
) -> Result<String, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("Formatting cancelled because the document changed or was closed.".into());
    }
    if lines.is_some() {
        return Err("This external command supports Format Document only. Choose a language server with range formatting support for Format Selection.".into());
    }
    if config.mode != CommandMode::Stdin && !has_file_argument(config) {
        return Err("Add {file} to Arguments, or choose Standard input → standard output in Formatter settings.".into());
    }
    // Keep the original basename and extension for language detection, and stay
    // beneath its parent so formatters can discover project configuration.
    // No open handle is retained: tools may replace their input atomically.
    let temporary = if has_file_argument(config) {
        let parent = std::path::absolute(path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(root))
            .map_err(|error| format!("Could not resolve the document folder: {error}"))?;
        let directory = tempfile::Builder::new().prefix(TEMP_DIR_PREFIX).tempdir_in(&parent)
            .map_err(|error| format!("Could not create a formatter working copy beside {}: {error}", path.display()))?;
        let copy = directory.path().join(path.file_name().ok_or("The document has no filename.")?);
        let file = std::fs::File::create(&copy)
            .map_err(|error| format!("Could not create the formatter working copy: {error}"))?;
        write_source(source, file)
            .map_err(|error| format!("Could not write the current document for formatting: {error}"))?;
        Some((directory, copy))
    } else {
        None
    };
    let program = &config.command;
    let mut command = vtcode::acp::local_command(program, root, std::iter::empty());
    let file_argument = temporary.as_ref().map(|(_, copy)| copy.to_string_lossy());
    let workspace_argument = root.to_string_lossy();
    for arg in &config.args {
        // Expand once: braces in a real directory name are literal path text.
        let mut expanded = String::new();
        let mut rest = arg.as_str();
        while let Some(start) = rest.find('{') {
            expanded.push_str(&rest[..start]);
            rest = &rest[start..];
            if let Some(tail) = rest.strip_prefix("{file}") {
                expanded.push_str(file_argument.as_deref().unwrap_or_default());
                rest = tail;
            } else if let Some(tail) = rest.strip_prefix("{workspace}") {
                expanded.push_str(&workspace_argument);
                rest = tail;
            } else {
                expanded.push('{');
                rest = &rest[1..];
            }
        }
        expanded.push_str(rest);
        command.arg(expanded);
    }
    command
        .stdin(if config.mode == CommandMode::Stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    vtcode::acp::without_controlling_terminal(&mut command);
    let mut process = vtcode::acp::LocalProcess::spawn(&mut command)
        .map_err(|error| format!("Could not start {program}: {error}. Install it or change its executable in Formatter settings."))?;
    if let Some(started) = started { started(process.killer()); }
    let stdin = process.child.stdin.take();
    let stdout = process.child.stdout.take().unwrap();
    let stderr = process.child.stderr.take().unwrap();
    let overflow = AtomicBool::new(false);
    let limit = source
        .len_bytes()
        .saturating_mul(4)
        .saturating_add(1024 * 1024);
    let (status, output, errors) = std::thread::scope(|scope| {
        let writer = stdin.map(|stdin| scope.spawn(move || write_source(source, stdin)));
        let output = scope.spawn(|| read_output(stdout, limit, &overflow));
        let errors = scope.spawn(|| read_output(stderr, 64 * 1024, &AtomicBool::new(false)));
        let start = Instant::now();
        let status = loop {
            if cancelled.load(Ordering::Acquire) {
                break Err(
                    "Formatting cancelled because the document changed or was closed.".into(),
                );
            }
            if overflow.load(Ordering::Acquire) {
                break Err("Formatter output exceeded the size limit.".into());
            }
            if start.elapsed() >= timeout {
                break Err(format!("The formatter did not finish within {} seconds.", timeout.as_secs_f64()));
            }
            match process.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Err(error) => break Err(format!("Could not wait for the formatter: {error}")),
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        // Also close pipes held by descendants before joining the reader threads.
        process.kill();
        let written = writer.map(|writer| writer.join()
            .map_err(|_| "Formatter input writer failed".to_string()))
            .transpose()?;
        let output = output
            .join()
            .map_err(|_| "Formatter output reader failed".to_string())?
            .map_err(|error| error.to_string())?;
        let errors = errors
            .join()
            .map_err(|_| "Formatter error reader failed".to_string())?
            .map_err(|error| error.to_string())?;
        let status = status?;
        if status.success() && let Some(written) = written {
            written.map_err(|error| {
                format!("Could not send the document to the formatter: {error}")
            })?;
        }
        Ok::<_, String>((status, output, errors))
    })?;
    if !status.success() {
        return Err(format!(
            "{program} exited with {status}: {}",
            String::from_utf8_lossy(&errors).trim()
        ));
    }
    if overflow.load(Ordering::Acquire) {
        return Err("Formatter output exceeded the size limit.".into());
    }
    let output = if config.mode == CommandMode::File {
        let copy = &temporary.as_ref().unwrap().1;
        let file = std::fs::File::open(copy)
            .map_err(|error| format!("Could not read the formatted working copy: {error}"))?;
        // Apply the same bound to in-place files, including files that grow
        // after opening. Stdout in this mode is logging, never document text.
        let output = read_output(file.take(limit.saturating_add(1) as u64), limit, &overflow)
            .map_err(|error| format!("Could not read the formatted working copy: {error}"))?;
        if overflow.load(Ordering::Acquire) {
            return Err("Formatter output exceeded the size limit.".into());
        }
        output
    } else {
        output
    };
    let output = String::from_utf8(output)
        .map_err(|_| "Formatter output is not valid UTF-8.".to_string())?;
    if output.is_empty() && source.chars().any(|ch| !ch.is_whitespace()) {
        return Err(if config.mode == CommandMode::File {
            "The formatter emptied the working copy. The document was left unchanged."
        } else {
            "The formatter returned no text on stdout. If it rewrites a file, choose Edit file in place in Formatter settings."
        }.into());
    }
    if cancelled.load(Ordering::Acquire) {
        return Err("Formatting cancelled because the document changed or was closed.".into());
    }
    Ok(output)
}

#[cfg(test)]
#[path = "formatters_tests.rs"]
mod tests;
