//! Detect path links only in the source line under Ctrl-hover or Ctrl-click.
use crate::{app::{App, TabKind}, editor::Editor, path_links::{self, Location}};
use std::{borrow::Cow, ops::Range, path::{Path, PathBuf}};

const LINE_WINDOW: usize = 4096;

#[derive(PartialEq, Eq)]
struct LineKey {
    editor: u64,
    revision: u64,
    version: u64,
    line: usize,
    band: usize,
    markdown: bool,
}

pub(crate) struct Link {
    pub columns: Range<usize>,
    location: Location,
}

#[derive(Default)]
pub(crate) struct Cache {
    key: Option<LineKey>,
    links: Vec<Link>,
}

impl Cache {
    fn hit(&mut self, editor: &Editor, line: usize, column: usize, markdown: bool) -> Option<&Link> {
        let key = LineKey {
            editor: editor.navigation_id,
            revision: editor.document_revision(),
            version: editor.content_version,
            line,
            band: column / LINE_WINDOW,
            markdown,
        };
        if self.key.as_ref() != Some(&key) {
            let source = editor.rope.get_line(line)?;
            let start = key.band.saturating_sub(1) * LINE_WINDOW;
            let end = ((key.band + 2) * LINE_WINDOW).min(source.len_chars());
            let slice = source.get_slice(start..end)?;
            let text = slice.as_str().map_or_else(|| Cow::Owned(slice.to_string()), Cow::Borrowed);
            let paths = if markdown { path_links::parse_markdown(&text) } else { path_links::parse(&text) };
            self.links.clear();
            for path in paths {
                // A token truncated by the scan window is not a complete path.
                if (start > 0 && path.range.start == 0)
                    || (end < source.len_chars() && path.range.end == text.len())
                {
                    continue;
                }
                self.links.push(Link {
                    columns: start + slice.byte_to_char(path.range.start)..start + slice.byte_to_char(path.range.end),
                    location: path.location,
                });
            }
            self.key = Some(key);
        }
        self.links.iter().find(|link| link.columns.contains(&column))
    }
}

impl App {
    pub(crate) fn editor_path_link_at(&mut self, line: usize, column: usize) -> Option<&Link> {
        let tab = self.tabs.get(self.active_tab)?;
        if tab.kind != TabKind::File || tab.extension_view.is_some()
            || tab.editor.bg_loading || tab.editor.load_failed || self.should_render_diff_for_active_editor()
        {
            return None;
        }
        let editor = &tab.editor;
        let markdown = editor.is_markdown();
        let language = editor.language_name();
        if !markdown && !["Plain Text", "Log", "Git Log", "reStructuredText", "AsciiDoc"]
            .iter().any(|candidate| language.eq_ignore_ascii_case(candidate))
        {
            return None;
        }
        self.lsp_link_hover.path_links.hit(editor, line, column, markdown)
    }

    pub(crate) fn follow_editor_path_link(&mut self, line: usize, column: usize) -> bool {
        let Some(location) = self.editor_path_link_at(line, column).map(|link| link.location.clone()) else {
            return false;
        };
        match self.resolve_editor_path(&location.path) {
            Ok((path, fallback)) => self.open_editor_location_at(path, fallback, location.line, location.column),
            Err(error) => self.notify_warn(format!("Cannot open {}: {error}", location.path)),
        }
        true
    }

    fn resolve_editor_path(&self, value: &str) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
        let file = self.editor().and_then(|editor| editor.file_path.as_deref());
        let explicit_relative = matches!(value, "." | "..")
            || ["./", "../", ".\\", "..\\"].iter().any(|&prefix| value.starts_with(prefix));
        if let Some(workspace) = self.remote_workspace.as_ref().filter(|_| self.editor_uses_remote_files()) {
            let root = &workspace.view_root;
            let folder = file.map(|key| workspace.parent_key(key)).transpose()?.flatten();
            let value = if workspace.windows_paths() && !value.starts_with("file://") {
                let native = value.replace('\\', "/");
                Cow::Owned(if native.starts_with("//") { format!("file:{native}") }
                    else if native.as_bytes().get(1) == Some(&b':') { format!("/{native}") }
                    else { native })
            } else { Cow::Borrowed(value) };
            let path = workspace.resolve_from_key(folder.as_deref().unwrap_or(root), &value)?;
            let fallback = (!explicit_relative).then(|| workspace.resolve_from_key(root, &value).ok()).flatten();
            return Ok(with_distinct_fallback(path, fallback));
        }
        if value.starts_with("file://") {
            let path = url::Url::parse(value).ok().and_then(|url| url.to_file_path().ok())
                .ok_or_else(|| std::io::Error::other("Invalid file URL"))?;
            return Ok((path, None));
        }
        let root = self.local_file_base();
        let folder = file.and_then(Path::parent).unwrap_or(&root);
        let path = folder.join(value);
        Ok(with_distinct_fallback(path, (!explicit_relative).then(|| root.join(value))))
    }
}

fn with_distinct_fallback(path: PathBuf, fallback: Option<PathBuf>) -> (PathBuf, Option<PathBuf>) {
    let fallback = fallback.filter(|candidate| candidate != &path);
    (path, fallback)
}

#[cfg(test)]
#[path = "app_editor_path_links_tests.rs"]
mod tests;
