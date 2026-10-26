//! Diagnostic display has its own quiet period: completion and navigation may
//! synchronize each keystroke without exposing errors in unfinished text.
use super::{Diagnostic, DiagnosticSeverity, LspDiagnostic, LspManager};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "lsp_diagnostics_tests.rs"]
mod tests;

pub(crate) const DEFAULT_DELAY_MS: u64 = 1_000;
pub(crate) const MAX_DELAY_MS: u64 = 10_000;

/// Small ordering metadata only: diagnostic messages remain in the published
/// reports and paths are stored once per file. Invalidated by the same revision
/// as editor diagnostic decorations, including document close and server reset.
#[derive(Default)]
pub(super) struct DiagnosticRows {
    revision: Option<u64>,
    paths: Vec<PathBuf>,
    // Severity, sorted file index, line, diagnostic index. The final index keeps
    // server order for multiple diagnostics at the same location.
    rows: Vec<(DiagnosticSeverity, usize, usize, usize)>,
}

#[derive(Clone)]
pub(crate) struct DiagnosticReport {
    pub(crate) version: i32,
    pub(crate) diagnostics: Arc<Vec<Diagnostic>>,
}

pub(crate) struct DiagnosticPublication {
    delay: Duration,
    last_edits: HashMap<PathBuf, Instant>,
    pending: HashMap<PathBuf, DiagnosticReport>,
    displayed_versions: HashMap<PathBuf, i32>,
}

impl Default for DiagnosticPublication {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(DEFAULT_DELAY_MS),
            last_edits: HashMap::new(),
            pending: HashMap::new(),
            displayed_versions: HashMap::new(),
        }
    }
}

impl DiagnosticPublication {
    pub(crate) fn delay_ms(&self) -> u64 {
        self.delay.as_millis() as u64
    }

    pub(crate) fn set_delay_ms(&mut self, milliseconds: u64) {
        self.delay = Duration::from_millis(milliseconds.min(MAX_DELAY_MS));
    }

    pub(super) fn edited(&mut self, path: &Path, now: Instant) {
        self.last_edits.insert(path.to_path_buf(), now);
        // Even an unversioned reply queued before this edit is now obsolete.
        self.pending.remove(path);
    }

    pub(super) fn forget(&mut self, path: &Path) {
        self.last_edits.remove(path);
        self.pending.remove(path);
        self.displayed_versions.remove(path);
    }

    fn remaining(&self, path: &Path, now: Instant) -> Duration {
        self.last_edits.get(path).map_or(Duration::ZERO, |edited| {
            self.delay
                .saturating_sub(now.saturating_duration_since(*edited))
        })
    }

    /// Only retained reports need a wakeup; there is no periodic polling timer.
    pub(crate) fn next_wait(&self, now: Instant) -> Option<Duration> {
        self.pending
            .keys()
            .map(|path| self.remaining(path, now))
            .min()
    }
}

impl LspManager {
    pub(crate) fn problem_counts(&mut self) -> (usize, usize, usize) {
        let _ = self.problems_diagnostics();
        let rows = &self.diagnostic_rows.rows;
        let errors = rows.partition_point(|row| row.0 <= lsp_types::DiagnosticSeverity::ERROR);
        let warnings = rows.partition_point(|row| row.0 <= lsp_types::DiagnosticSeverity::WARNING);
        (errors, warnings - errors, rows.len() - warnings)
    }

    pub(crate) fn problems_diagnostics(&mut self) -> impl ExactSizeIterator<Item = (&Path, &LspDiagnostic)> {
        let cache = &mut self.diagnostic_rows;
        if cache.revision != Some(self.diagnostics_version) {
            cache.paths.clear();
            cache.paths.extend(self.diagnostics.iter().filter(|(_, items)| !items.is_empty())
                .map(|(path, _)| path.clone()));
            cache.paths.sort_unstable();
            cache.rows.clear();
            for (file, path) in cache.paths.iter().enumerate() {
                cache.rows.extend(self.diagnostics[path].iter().enumerate()
                    .map(|(index, diagnostic)| (diagnostic.severity, file, diagnostic.line, index)));
            }
            cache.rows.sort_unstable();
            cache.revision = Some(self.diagnostics_version);
        }
        cache.rows.iter().map(|&(_, file, _, index)| {
            let path = &cache.paths[file];
            (path.as_path(), &self.diagnostics[path][index])
        })
    }

    pub(crate) fn receive_diagnostics(
        &mut self,
        path: PathBuf,
        version: Option<i32>,
        diagnostics: Vec<Diagnostic>,
        now: Instant,
    ) {
        let open = self.clients.iter().any(|client| client.is_file_open(&path));
        if !open || self.diagnostic_dirty_documents.contains(&path) {
            return;
        }
        let current = self.doc_versions.get(&path).copied().unwrap_or(0);
        if version.is_some_and(|version| version != current) {
            return;
        }

        // Code actions and delayed publication share one immutable report,
        // including potentially large related-information and fix payloads.
        let report = DiagnosticReport { version: current, diagnostics: Arc::new(diagnostics) };
        self.diagnostic_context.insert(path.clone(), report.clone());
        if !self.diagnostic_publication.remaining(&path, now).is_zero() {
            self.diagnostic_publication.pending.insert(path, report);
        } else {
            self.diagnostic_publication.pending.remove(&path);
            self.publish_diagnostics(path, report.diagnostics);
        }
    }

    fn publish_diagnostics(&mut self, path: PathBuf, diagnostics: Arc<Vec<Diagnostic>>) {
        // Servers often publish identical reports after saves or analysis of
        // dependencies. Keep the render caches intact, but always refresh the
        // full code-action context above (its data can change independently).
        // An identical UTF-16 range can map to different scalar columns after
        // an edit (for example replacing an emoji with two ASCII characters).
        // Nonempty reports must refresh that mapping for a new snapshot.
        let same_snapshot = self.diagnostic_publication.displayed_versions.get(&path)
            == self.doc_versions.get(&path);
        if (diagnostics.is_empty() || same_snapshot) && self.diagnostics.get(&path).is_some_and(|displayed| {
            displayed.len() == diagnostics.len() && displayed.iter().zip(diagnostics.iter()).all(|(a, b)| {
                a.line == b.range.start.line as usize && a.col == b.range.start.character as usize
                    && a.end_line == b.range.end.line as usize && a.end_col == b.range.end.character as usize
                    && a.message == b.message
                    && a.severity == b.severity.unwrap_or(DiagnosticSeverity::INFORMATION)
            })
        }) { return; }
        let diagnostics = diagnostics
            .iter()
            .map(|diagnostic| LspDiagnostic {
                line: diagnostic.range.start.line as usize,
                col: diagnostic.range.start.character as usize,
                end_line: diagnostic.range.end.line as usize,
                end_col: diagnostic.range.end.character as usize,
                message: diagnostic.message.clone(),
                severity: diagnostic
                    .severity
                    .unwrap_or(DiagnosticSeverity::INFORMATION),
            })
            .collect();
        if let Some(&version) = self.doc_versions.get(&path) {
            self.diagnostic_publication.displayed_versions.insert(path.clone(), version);
        }
        self.diagnostics.insert(path, diagnostics);
        self.diagnostics_version = self.diagnostics_version.wrapping_add(1);
    }

    pub(crate) fn current_diagnostic_context(&self, path: &Path) -> Option<&[Diagnostic]> {
        let report = self.diagnostic_context.get(path)?;
        (!self.diagnostic_dirty_documents.contains(path)
            && self.doc_versions.get(path) == Some(&report.version))
            .then_some(report.diagnostics.as_slice())
    }

    pub(crate) fn publish_pending_diagnostics(&mut self, now: Instant) {
        if self.diagnostic_publication.next_wait(now) != Some(Duration::ZERO) {
            return;
        }
        let ready: Vec<_> = self
            .diagnostic_publication
            .pending
            .keys()
            .filter(|path| self.diagnostic_publication.remaining(path, now).is_zero())
            .cloned()
            .collect();
        for path in ready {
            let report = self.diagnostic_publication.pending.remove(&path).unwrap();
            if !self.diagnostic_dirty_documents.contains(&path)
                && self.doc_versions.get(&path) == Some(&report.version)
                && self.clients.iter().any(|client| client.is_file_open(&path))
            {
                self.publish_diagnostics(path, report.diagnostics);
            }
        }
    }
}
