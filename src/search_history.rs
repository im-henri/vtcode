//! Bounded query history shared by file and workspace search inputs.
use std::collections::VecDeque;

const MAX_QUERIES: usize = 50;

#[cfg(test)]
#[path = "search_history_tests.rs"]
mod tests;

#[derive(Default)]
pub(crate) struct History {
    entries: VecDeque<String>,
    index: Option<usize>,
    draft: String,
}

impl History {
    pub(crate) fn restore(entries: &[String]) -> Self {
        let mut history = Self::default();
        for query in entries {
            history.record(query);
        }
        history
    }

    pub(crate) fn record(&mut self, query: &str) {
        if !query.is_empty() {
            self.entries.retain(|entry| entry != query);
            self.entries.push_back(query.to_owned());
            if self.entries.len() > MAX_QUERIES {
                self.entries.pop_front();
            }
        }
        self.edited();
    }

    pub(crate) fn edited(&mut self) {
        self.index = None;
        self.draft.clear();
    }

    pub(crate) fn previous(&mut self, query: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let index = match self.index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = query.to_owned();
                self.entries.len() - 1
            }
        };
        self.index = Some(index);
        Some(self.entries[index].clone())
    }

    pub(crate) fn next(&mut self) -> Option<String> {
        let index = self.index? + 1;
        if index < self.entries.len() {
            self.index = Some(index);
            Some(self.entries[index].clone())
        } else {
            self.index = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    /// Include the current query on shutdown without promoting a history preview.
    pub(crate) fn saved(&self, query: &str) -> Vec<String> {
        let mut saved = Self {
            entries: self.entries.clone(),
            ..Self::default()
        };
        saved.record(if self.index.is_some() {
            &self.draft
        } else {
            query
        });
        saved.entries.into()
    }
}
