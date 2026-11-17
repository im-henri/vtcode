//! Detect path links only in the source line under Ctrl-hover or Ctrl-click.
use crate::{
    app::{App, TabKind},
    code_path_links::{self, Search, Syntax, explicit_relative},
    editor::Editor,
    path_links::{self, Location},
};
use std::{
    borrow::Cow,
    collections::VecDeque,
    io,
    ops::Range,
    path::{Path, PathBuf},
};

const LINE_WINDOW: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Text,
    Markdown,
    Code(Syntax),
}

#[derive(PartialEq, Eq)]
struct LineKey {
    editor: u64,
    revision: u64,
    version: u64,
    line: usize,
    band: usize,
    mode: Mode,
}

pub(crate) struct Link {
    pub columns: Range<usize>,
    location: Location,
    search: Option<Search>,
}

#[derive(Default)]
pub(crate) struct Cache {
    key: Option<LineKey>,
    links: Vec<Link>,
}

impl Cache {
    fn hit(&mut self, editor: &Editor, line: usize, column: usize, mode: Mode) -> Option<&Link> {
        let key = LineKey {
            editor: editor.navigation_id,
            revision: editor.document_revision(),
            version: editor.content_version,
            line,
            band: column / LINE_WINDOW,
            mode,
        };
        if self.key.as_ref() != Some(&key) {
            let source = editor.rope.get_line(line)?;
            let start = key.band.saturating_sub(1) * LINE_WINDOW;
            let end = ((key.band + 2) * LINE_WINDOW).min(source.len_chars());
            let slice = source.get_slice(start..end)?;
            let text = slice
                .as_str()
                .map_or_else(|| Cow::Owned(slice.to_string()), Cow::Borrowed);
            self.links.clear();
            let mut add = |range: Range<usize>, location, search| {
                // A token truncated by the scan window is not a complete path.
                if (start > 0 && range.start == 0)
                    || (end < source.len_chars() && range.end == text.len())
                {
                    return;
                }
                self.links.push(Link {
                    columns: start + slice.byte_to_char(range.start)
                        ..start + slice.byte_to_char(range.end),
                    location,
                    search,
                });
            };
            match mode {
                Mode::Code(syntax) if start == 0 => {
                    for reference in code_path_links::parse(&text, syntax) {
                        let location = Location {
                            path: reference.path,
                            line: None,
                            column: 0,
                        };
                        add(reference.range, location, Some(reference.search));
                    }
                }
                Mode::Code(_) => {} // An interior slice is not the start of a directive.
                Mode::Text | Mode::Markdown => {
                    let paths = if mode == Mode::Markdown {
                        path_links::parse_markdown(&text)
                    } else {
                        path_links::parse(&text)
                    };
                    for path in paths {
                        add(path.range, path.location, None);
                    }
                }
            }
            self.key = Some(key);
        }
        self.links
            .iter()
            .find(|link| link.columns.contains(&column))
    }
}

impl App {
    pub(crate) fn editor_path_link_at(&mut self, line: usize, column: usize) -> Option<&Link> {
        let tab = self.tabs.get(self.active_tab)?;
        if tab.kind != TabKind::File
            || tab.extension_view.is_some()
            || tab.editor.bg_loading
            || tab.editor.load_failed
            || self.should_render_diff_for_active_editor()
        {
            return None;
        }
        let editor = &tab.editor;
        let mode = if editor.is_markdown() {
            Mode::Markdown
        } else if let Some(syntax) = Syntax::for_editor(editor) {
            Mode::Code(syntax)
        } else if [
            "Plain Text",
            "Log",
            "Git Log",
            "reStructuredText",
            "AsciiDoc",
        ]
        .iter()
        .any(|candidate| editor.language_name().eq_ignore_ascii_case(candidate))
        {
            Mode::Text
        } else {
            return None;
        };
        self.lsp_link_hover
            .path_links
            .hit(editor, line, column, mode)
    }

    pub(crate) fn follow_editor_path_link(&mut self, line: usize, column: usize) -> bool {
        let Some((location, search)) = self
            .editor_path_link_at(line, column)
            .map(|link| (link.location.clone(), link.search))
        else {
            return false;
        };
        match self.resolve_editor_paths(&location.path, search) {
            Ok(paths) => {
                self.open_editor_paths(paths, location.line, location.column, search.is_some())
            }
            Err(error) => self.notify_warn(format!("Cannot open {}: {error}", location.path)),
        }
        true
    }

    fn resolve_editor_paths(
        &self,
        value: &str,
        search: Option<Search>,
    ) -> io::Result<VecDeque<PathBuf>> {
        let file = self.editor().and_then(|editor| editor.file_path.as_deref());
        let relative = explicit_relative(value);
        if let Some(workspace) = self
            .remote_workspace
            .as_ref()
            .filter(|_| self.editor_uses_remote_files())
        {
            let root = &workspace.view_root;
            let folder = file
                .map(|key| workspace.parent_key(key))
                .transpose()?
                .flatten();
            let value = if workspace.windows_paths() && !value.starts_with("file://") {
                let native = value.replace('\\', "/");
                Cow::Owned(if native.starts_with("//") {
                    format!("file:{native}")
                } else if native.as_bytes().get(1) == Some(&b':') {
                    format!("/{native}")
                } else {
                    native
                })
            } else {
                Cow::Borrowed(value)
            };
            return resolve_paths(
                &value,
                folder.as_deref().unwrap_or(root),
                root,
                search,
                relative,
                |base, value| workspace.resolve_from_key(base, value),
            );
        }
        if value.starts_with("file://") {
            let path = url::Url::parse(value)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .ok_or_else(|| io::Error::other("Invalid file URL"))?;
            return Ok(VecDeque::from([path]));
        }
        let root = self.local_file_base();
        let folder = file.and_then(Path::parent).unwrap_or(&root);
        resolve_paths(value, folder, &root, search, relative, |base, value| {
            Ok(base.join(value))
        })
    }
}

fn resolve_paths(
    value: &str,
    folder: &Path,
    root: &Path,
    search: Option<Search>,
    relative: bool,
    join: impl Fn(&Path, &str) -> io::Result<PathBuf>,
) -> io::Result<VecDeque<PathBuf>> {
    let mut paths = VecDeque::new();
    let mut push = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push_back(path);
        }
    };
    match search {
        Some(Search::Include { angled }) if !relative => {
            if !angled {
                push(join(folder, value)?);
            }
            push(join(&root.join("include"), value)?);
            push(join(root, value)?);
            if angled {
                push(join(folder, value)?);
            }
        }
        Some(Search::Module { typescript }) => {
            let path = join(folder, value)?;
            push(path.clone());
            if !value.starts_with("file://") {
                let extensions: &[&str] = if typescript {
                    &["ts", "tsx", "js", "jsx", "mts", "cts"]
                } else {
                    &["js", "jsx", "mjs", "cjs"]
                };
                let directory = matches!(value, "." | "..") || value.ends_with(['/', '\\']);
                if !directory && path.extension().is_none() {
                    for extension in extensions {
                        push(path.with_extension(extension));
                    }
                }
                if directory || path.extension().is_none() {
                    for extension in extensions {
                        push(path.join("index").with_extension(extension));
                    }
                }
            }
        }
        Some(Search::CMake) => {
            let path = join(folder, value)?;
            push(path.clone());
            if path.extension().is_none() {
                push(path.with_extension("cmake"));
                if !relative {
                    push(join(&root.join("cmake"), value)?.with_extension("cmake"));
                }
            }
            if !relative {
                let workspace_path = join(root, value)?;
                if workspace_path.extension().is_none() {
                    push(workspace_path.with_extension("cmake"));
                }
                push(workspace_path);
            }
        }
        _ => {
            push(join(folder, value)?);
            if !relative && matches!(search, None | Some(Search::Shell)) {
                push(join(root, value)?);
            }
        }
    }
    Ok(paths)
}

#[cfg(test)]
#[path = "app_editor_path_links_tests.rs"]
mod tests;
