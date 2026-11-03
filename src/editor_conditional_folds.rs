//! Conditional folding has two explicit scopes: one branch, or the complete
//! #if / #elif / #else tree. Retain branch boundaries for labels and actions
//! without rescanning hidden source on every repaint.
use crate::{
    editor::Editor,
    editor_transform::{PreprocessorDirective, preprocessor_continues, preprocessor_directive},
};
use ropey::Rope;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct ConditionalFold {
    pub start: usize,
    pub end: usize,
    pub branches: Vec<usize>,
}

impl ConditionalFold {
    pub(crate) fn branch_at(&self, line: usize) -> (usize, usize) {
        let index = self
            .branches
            .partition_point(|&header| header <= line)
            .saturating_sub(1);
        (
            self.branches[index],
            self.branches.get(index + 1).copied().unwrap_or(self.end),
        )
    }
}

#[derive(Default)]
pub(crate) struct ConditionalFoldCache {
    source: Option<Rope>,
    entries: rustc_hash::FxHashMap<usize, Option<Arc<ConditionalFold>>>,
    #[cfg(test)]
    pub builds: usize,
}

impl Editor {
    pub(crate) fn conditional_fold(&self, start: usize) -> Option<Arc<ConditionalFold>> {
        if !self.has_preprocessor_folds()
            || start >= self.line_count()
            || preprocessor_directive(self.rope.line(start)) != Some(PreprocessorDirective::If)
        {
            return None;
        }
        let mut cache = self.conditional_fold_cache.borrow_mut();
        if cache
            .source
            .as_ref()
            .is_none_or(|source| !source.is_instance(&self.rope))
        {
            cache.entries.clear();
            cache.source = Some(self.rope.clone());
        }
        if let Some(scope) = cache.entries.get(&start) {
            return scope.clone();
        }
        if cache.entries.len() >= 512 {
            cache.entries.clear();
        }
        #[cfg(test)]
        {
            cache.builds += 1;
        }
        let scope = self.scan_conditional_fold(start).map(Arc::new);
        cache.entries.insert(start, scope.clone());
        scope
    }

    fn scan_conditional_fold(&self, start: usize) -> Option<ConditionalFold> {
        // A '#' inside a continued macro is source text, not a conditional.
        if start > 0 && preprocessor_continues(self.rope.line(start - 1)) {
            return None;
        }
        let mut continued = preprocessor_continues(self.rope.line(start));
        let mut depth = 0usize;
        let mut branches = vec![start];
        for (offset, line) in self.rope.lines_at(start + 1).enumerate() {
            if continued {
                continued = preprocessor_continues(line);
                continue;
            }
            let Some(directive) = preprocessor_directive(line) else {
                continue;
            };
            continued = preprocessor_continues(line);
            let row = start + offset + 1;
            match directive {
                PreprocessorDirective::If => depth += 1,
                PreprocessorDirective::End if depth > 0 => depth -= 1,
                PreprocessorDirective::End => {
                    return Some(ConditionalFold {
                        start,
                        end: row,
                        branches,
                    });
                }
                PreprocessorDirective::Branch if depth == 0 => branches.push(row),
                _ => {}
            }
        }
        None
    }

    pub(crate) fn conditional_at(&self, line: usize) -> Option<Arc<ConditionalFold>> {
        if !self.has_preprocessor_folds() || line >= self.line_count() {
            return None;
        }
        let mut depth = 0usize;
        for (offset, source) in self.rope.lines_at(line + 1).reversed().enumerate() {
            let row = line - offset;
            if row > 0 && preprocessor_continues(self.rope.line(row - 1)) {
                continue;
            }
            match preprocessor_directive(source) {
                Some(PreprocessorDirective::End) if row != line => depth += 1,
                Some(PreprocessorDirective::If) if depth > 0 => depth -= 1,
                Some(PreprocessorDirective::If) => {
                    return self.conditional_fold(row).filter(|scope| line <= scope.end);
                }
                _ => {}
            }
        }
        None
    }

    pub(crate) fn collapse_conditional(&mut self, line: usize) -> bool {
        let Some(scope) = self.conditional_at(line) else {
            return false;
        };
        if scope.end <= scope.start + 1 {
            return false;
        }
        self.collapse_range(scope.start, scope.end);
        true
    }

    pub(crate) fn collapse_conditional_branch(&mut self, line: usize) -> bool {
        let Some(scope) = self.conditional_at(line) else {
            return false;
        };
        let (start, end) = scope.branch_at(line);
        // Opening an ancestor is necessary when changing its collapsed scope.
        self.folded_ranges.remove(&scope.start);
        self.collapse_range(start, end);
        true
    }

    pub(crate) fn conditional_directive(&self, line: usize) -> Option<PreprocessorDirective> {
        (self.has_preprocessor_folds() && line < self.line_count())
            .then(|| preprocessor_directive(self.rope.line(line)))
            .flatten()
    }
}

#[cfg(test)]
#[path = "editor_conditional_folds_tests.rs"]
mod tests;
