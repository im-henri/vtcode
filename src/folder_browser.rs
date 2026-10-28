//! In-app directory picker state and navigation, with background directory listings.
use std::collections::HashMap;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::App;
use crate::text_input::TextInputId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Focus {
    Address,
    Search,
    Locations,
    #[default]
    List,
    Folder,
    Open,
    Cancel,
}

impl Focus {
    pub(crate) fn input(self) -> Option<TextInputId> {
        match self {
            Self::Address => Some(TextInputId::FolderAddress),
            Self::Search => Some(TextInputId::FolderSearch),
            Self::Folder => Some(TextInputId::FolderName),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Back,
    Forward,
    Up,
    Refresh,
    Address,
    Navigate(PathBuf),
    Location(usize),
    NewFolder,
    Hidden,
    SortName,
    SortModified,
    ClearSearch,
    Close,
}

#[derive(Default)]
pub(crate) struct FolderBrowser {
    pub save_as_return: bool,
    pub focus: Focus,
    pub address_cursor: usize,
    pub address_anchor: Option<usize>,
    pub search: String,
    pub search_cursor: usize,
    pub search_anchor: Option<usize>,
    pub folder: String,
    pub folder_cursor: usize,
    pub folder_anchor: Option<usize>,
    pub new_folder: bool,
    pub creating: bool,
    pub error: Option<String>,
    pub show_hidden: bool,
    pub sort_modified: bool,
    pub descending: bool,
    pub generation: u64,
    pub back: Vec<PathBuf>,
    pub forward: Vec<PathBuf>,
    pub locations: Vec<(String, PathBuf)>,
    pub location_selected: usize,
    pub location_scroll: usize,
    pub location_area: Rect,
    pub modified: HashMap<PathBuf, SystemTime>,
    pub controls: Vec<(Rect, Action)>,
    pub last_click: Option<(usize, Instant)>,
    listing_cache: RefCell<Option<FolderBrowserListing>>,
}

struct FolderBrowserListing {
    query: String,
    generation: u64,
    entry_count: usize,
    sort_modified: bool,
    descending: bool,
    indices: Arc<[usize]>,
}

pub(crate) fn normalized_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn locations(workspace: PathBuf) -> Vec<(String, PathBuf)> {
    let mut result = vec![("Computer".into(), PathBuf::new())];
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home = PathBuf::from(home);
        result.push(("Home".into(), home.clone()));
        for name in [
            "Desktop",
            "Downloads",
            "Documents",
            "Pictures",
            "Music",
            "Videos",
        ] {
            let path = known_location(name).unwrap_or_else(|| home.join(name));
            if path.is_dir() {
                result.push((name.into(), path));
            }
        }
    }
    result.push(("Workspace".into(), workspace));
    #[cfg(windows)]
    result.extend(crate::filesystem_roots::list().unwrap_or_default());
    #[cfg(not(windows))]
    result.push(("File system (/)".into(), PathBuf::from("/")));
    result
}

#[cfg(windows)]
fn known_location(name: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{System::Com::CoTaskMemFree, UI::Shell::*};
    let id = match name {
        "Desktop" => FOLDERID_Desktop,
        "Downloads" => FOLDERID_Downloads,
        "Documents" => FOLDERID_Documents,
        "Pictures" => FOLDERID_Pictures,
        "Music" => FOLDERID_Music,
        "Videos" => FOLDERID_Videos,
        _ => return None,
    };
    let mut pointer = std::ptr::null_mut();
    // SAFETY: Shell allocates a NUL-terminated UTF-16 string. We copy it before
    // freeing the allocation with the matching COM allocator on every path.
    unsafe {
        let result = SHGetKnownFolderPath(
            &id,
            KF_FLAG_DONT_VERIFY as u32,
            std::ptr::null_mut(),
            &mut pointer,
        );
        if result < 0 || pointer.is_null() {
            CoTaskMemFree(pointer.cast());
            return None;
        }
        let mut length = 0;
        while *pointer.add(length) != 0 {
            length += 1;
        }
        let path = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
            pointer, length,
        )));
        CoTaskMemFree(pointer.cast());
        Some(path)
    }
}

#[cfg(not(windows))]
fn known_location(_name: &str) -> Option<PathBuf> {
    None
}

impl App {
    /// An empty location is the virtual Computer page, never a filesystem path.
    pub(crate) fn folder_browser_is_computer(&self) -> bool {
        self.folder_browser_path.as_os_str().is_empty()
    }

    pub(crate) fn folder_browser_entry_name<'a>(&'a self, path: &'a Path) -> std::borrow::Cow<'a, str> {
        if self.folder_browser_is_computer() {
            if let Some((name, _)) = self.folder_browser.locations.iter().rev().find(|(_, root)| root == path) {
                return std::borrow::Cow::Borrowed(name);
            }
            return std::borrow::Cow::Owned(self.display_path(path));
        }
        crate::remote::workspace::display_name(path).map(std::borrow::Cow::Owned)
            .unwrap_or_else(|| path.file_name().unwrap_or_default().to_string_lossy())
    }

    pub(crate) fn open_folder_browser(&mut self) {
        let generation = self.folder_browser.generation.wrapping_add(1);
        self.folder_browser = FolderBrowser {
            generation,
            ..FolderBrowser::default()
        };
        self.change_dir_active = true;
        self.folder_browser_path = self.explorer.root.clone();
        self.text_input_drag = None;
        self.text_input_regions.clear();
        for id in [
            TextInputId::FolderAddress,
            TextInputId::FolderSearch,
            TextInputId::FolderName,
        ] {
            self.text_input_histories.remove(&id);
        }
        self.refresh_folder_browser();
    }

    pub(crate) fn refresh_folder_browser(&mut self) {
        let computer = self.folder_browser_is_computer();
        if !computer && self.remote_workspace.is_none() { self.folder_browser_path = normalized_path(&self.folder_browser_path); }
        self.change_dir_query = self.display_path(&self.folder_browser_path);
        self.folder_browser.address_cursor = self.change_dir_query.len();
        self.folder_browser.address_anchor = None;
        self.folder_browser.folder = self.change_dir_query.clone();
        self.folder_browser.folder_cursor = self.folder_browser.folder.len();
        self.folder_browser.folder_anchor = None;
        self.folder_browser.error = None;
        self.folder_browser.last_click = None;
        self.folder_browser.modified.clear();
        self.folder_browser_entries.clear();
        self.folder_browser_selected = 0;
        self.folder_browser_scroll = 0;
        self.folder_browser_loading = true;
        self.folder_browser.generation = self.folder_browser.generation.wrapping_add(1);
        let generation = self.folder_browser.generation;
        let hidden = self.folder_browser.show_hidden;
        let path = self.folder_browser_path.clone();
        let workspace = self.explorer.root.clone();
        let cached_locations = self.folder_browser.locations.clone();
        let sender = self.background.sender.clone();
        let read_path = path.clone();
        if let Some(remote) = self.remote_workspace.clone() {
            self.background.read_for_file("remote-folder-browser", &read_path, true, move || {
                let mut entries = Vec::new();
                let mut locations = if cached_locations.is_empty() || computer {
                    vec![("Computer".into(), PathBuf::new()), ("Remote workspace".into(), workspace)]
                } else { cached_locations };
                let result = if computer {
                    remote.filesystem_roots().map(|roots| {
                        entries = roots.iter().map(|(_, path)| path.clone()).collect();
                        locations.extend(roots);
                    })
                } else {
                    remote.remote_path(&path).and_then(|remote_path| remote.list(&remote_path)).map(|items| {
                        entries = items.iter().filter(|entry| entry.is_dir && (hidden || !entry.path.name().starts_with('.'))).map(|entry| remote.view_key(&entry.path)).collect();
                    })
                };
                let _ = sender.send(crate::background::BackgroundResult::FolderBrowserEntries {
                    path, generation, entries, modified: HashMap::new(),
                    locations, error: result.err().map(|error| error.to_string()),
                });
            });
            return;
        }
        self.background.read_for_file("folder-browser", &read_path, true, move || {
            if computer {
                let roots = crate::filesystem_roots::list();
                let mut locations = locations(workspace);
                let mut entries = Vec::new();
                if let Ok(roots) = &roots {
                    entries = roots.iter().map(|(_, path)| path.clone()).collect();
                    for root in roots {
                        if !locations.contains(root) { locations.push(root.clone()); }
                    }
                }
                let _ = sender.send(crate::background::BackgroundResult::FolderBrowserEntries {
                    path, generation, entries, modified: HashMap::new(), locations,
                    error: roots.err().map(|error| format!("Cannot list file systems: {error}")),
                });
                return;
            }
            let mut dirs = Vec::new();
            let mut modified = HashMap::new();
            let mut error = None;
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = match entry {
                            Ok(entry) => entry,
                            Err(err) => {
                                error = Some(format!("Some folders could not be read: {err}"));
                                continue;
                            }
                        };
                        if !hidden && entry.file_name().to_string_lossy().starts_with('.') {
                            continue;
                        }
                        // Most workspace entries are files. read_dir already
                        // supplies their type; only stat directories (or a
                        // symlink that may resolve to one) for modification time.
                        if let Ok(kind) = entry.file_type() {
                            if !kind.is_dir() && !kind.is_symlink() {
                                continue;
                            }
                        }
                        let entry_path = entry.path();
                        let metadata = match std::fs::metadata(&entry_path) {
                            Ok(metadata) if metadata.is_dir() => metadata,
                            _ => continue,
                        };
                        #[cfg(windows)]
                        {
                            use std::os::windows::fs::MetadataExt;
                            if !hidden && metadata.file_attributes() & 2 != 0 {
                                continue;
                            }
                        }
                        if let Ok(time) = metadata.modified() {
                            modified.insert(entry_path.clone(), time);
                        }
                        dirs.push(entry_path);
                    }
                }
                Err(err) => error = Some(format!("Cannot read this folder: {err}")),
            }
            dirs.sort_by_cached_key(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase()
            });
            let _ = sender.send(crate::background::BackgroundResult::FolderBrowserEntries {
                path,
                generation,
                entries: dirs,
                modified,
                locations: if cached_locations.is_empty() {
                    locations(workspace)
                } else {
                    cached_locations
                },
                error,
            });
        });
    }

    pub(crate) fn folder_browser_indices(&self) -> Arc<[usize]> {
        let browser = &self.folder_browser;
        let mut cached = browser.listing_cache.borrow_mut();
        if let Some(view) = cached.as_ref().filter(|view| {
            view.query == browser.search && view.generation == browser.generation
                && view.entry_count == self.folder_browser_entries.len()
                && view.sort_modified == browser.sort_modified
                && view.descending == browser.descending
        }) {
            return Arc::clone(&view.indices);
        }
        let query = self.folder_browser.search.to_lowercase();
        let mut indices: Vec<_> = self
            .folder_browser_entries
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                query.is_empty() || self.folder_browser_entry_name(p)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        if self.folder_browser.sort_modified {
            indices.sort_by_cached_key(|&i| {
                self.folder_browser
                    .modified
                    .get(&self.folder_browser_entries[i])
                    .copied()
            });
        }
        if self.folder_browser.descending {
            indices.reverse();
        }
        let indices: Arc<[usize]> = indices.into();
        *cached = Some(FolderBrowserListing {
            query: browser.search.clone(),
            generation: browser.generation,
            entry_count: self.folder_browser_entries.len(),
            sort_modified: browser.sort_modified,
            descending: browser.descending,
            indices: Arc::clone(&indices),
        });
        indices
    }

    pub(crate) fn folder_browser_count(&self) -> usize {
        self.folder_browser_indices().len()
            + usize::from(self.folder_browser_parent().is_some())
    }

    pub(crate) fn filter_folder_browser(&mut self) {
        *self.folder_browser.listing_cache.get_mut() = None;
        self.folder_browser_selected = 0;
        self.folder_browser_scroll = 0;
        self.folder_browser.last_click = None;
        if !self.folder_browser.new_folder {
            self.folder_browser.folder = self.display_path(&self.folder_browser_path);
            self.folder_browser.folder_cursor = self.folder_browser.folder.len();
            self.folder_browser.folder_anchor = None;
        }
    }

    fn selected_browser_folder(&self) -> Option<PathBuf> {
        let offset = usize::from(self.folder_browser_parent().is_some());
        let index = self.folder_browser_selected.checked_sub(offset)?;
        self.folder_browser_indices()
            .get(index)
            .map(|&i| self.folder_browser_entries[i].clone())
    }

    pub(crate) fn select_browser_row(&mut self, index: usize) {
        self.folder_browser_selected = index.min(self.folder_browser_count().saturating_sub(1));
        self.folder_browser.focus = Focus::List;
        self.folder_browser.new_folder = false;
        self.folder_browser.folder = self.selected_browser_folder()
            .map(|path| self.display_path(&path)).unwrap_or_else(|| self.display_path(&self.folder_browser_path));
        self.folder_browser.folder_cursor = self.folder_browser.folder.len();
        self.folder_browser.folder_anchor = None;
    }

    pub(crate) fn navigate_folder_browser(&mut self, path: PathBuf) {
        if self.folder_browser.creating {
            return;
        }
        let path = if path.as_os_str().is_empty() || self.remote_workspace.is_some() { path } else { normalized_path(&path) };
        if path != self.folder_browser_path {
            self.folder_browser
                .back
                .push(self.folder_browser_path.clone());
            self.folder_browser.forward.clear();
        }
        self.folder_browser_path = path;
        self.folder_browser.search.clear();
        self.folder_browser.search_cursor = 0;
        self.folder_browser.search_anchor = None;
        self.folder_browser.new_folder = false;
        self.folder_browser.focus = Focus::List;
        self.refresh_folder_browser();
    }

    pub(crate) fn folder_browser_enter(&mut self) {
        if let Some(path) = self.selected_browser_folder() {
            self.navigate_folder_browser(path);
        } else if self.folder_browser_parent().is_some() {
            self.folder_browser_up();
        }
    }

    pub(crate) fn folder_browser_up(&mut self) {
        if let Some(parent) = self.folder_browser_parent() {
            self.navigate_folder_browser(parent);
        }
    }

    pub(crate) fn folder_browser_parent(&self) -> Option<PathBuf> {
        if self.folder_browser_is_computer() { return None; }
        if let Some(remote) = &self.remote_workspace {
            return remote.parent_key(&self.folder_browser_path).ok().map(|parent| parent.unwrap_or_default());
        }
        Some(normalized_path(&self.folder_browser_path).parent().map(Path::to_path_buf).unwrap_or_default())
    }

    pub(crate) fn folder_browser_action(&mut self, action: Action) {
        if self.folder_browser.creating && !matches!(action, Action::Close) {
            return;
        }
        match action {
            Action::Back | Action::Forward => {
                let back = matches!(action, Action::Back);
                let target = if back {
                    self.folder_browser.back.pop()
                } else {
                    self.folder_browser.forward.pop()
                };
                if let Some(target) = target {
                    if back {
                        self.folder_browser
                            .forward
                            .push(self.folder_browser_path.clone());
                    } else {
                        self.folder_browser
                            .back
                            .push(self.folder_browser_path.clone());
                    }
                    self.folder_browser_path = target;
                    self.folder_browser.search.clear();
                    self.folder_browser.search_cursor = 0;
                    self.folder_browser.search_anchor = None;
                    self.folder_browser.new_folder = false;
                    self.folder_browser.focus = Focus::List;
                    self.refresh_folder_browser();
                }
            }
            Action::Up => self.folder_browser_up(),
            Action::Refresh => {
                self.folder_browser.new_folder = false;
                self.refresh_folder_browser();
            }
            Action::Navigate(path) => self.navigate_folder_browser(path),
            Action::Location(i) => {
                if let Some((_, path)) = self.folder_browser.locations.get(i).cloned() {
                    self.folder_browser.location_selected = i;
                    self.navigate_folder_browser(path);
                }
            }
            Action::Address => {
                self.folder_browser.focus = Focus::Address;
                self.change_dir_query = self.display_path(&self.folder_browser_path);
                self.folder_browser.address_cursor = self.change_dir_query.len();
                self.folder_browser.address_anchor = Some(0);
            }
            Action::NewFolder => {
                if self.folder_browser_is_computer() { return; }
                self.folder_browser.new_folder = true;
                self.folder_browser.focus = Focus::Folder;
                self.folder_browser.folder = "New folder".into();
                self.folder_browser.folder_cursor = self.folder_browser.folder.len();
                self.folder_browser.folder_anchor = Some(0);
                self.folder_browser.error = None;
            }
            Action::Hidden => {
                self.folder_browser.show_hidden = !self.folder_browser.show_hidden;
                self.folder_browser.new_folder = false;
                self.refresh_folder_browser();
            }
            Action::SortName | Action::SortModified => {
                let modified = matches!(action, Action::SortModified);
                self.folder_browser.descending = if self.folder_browser.sort_modified == modified {
                    !self.folder_browser.descending
                } else {
                    modified
                };
                self.folder_browser.sort_modified = modified;
                self.filter_folder_browser();
            }
            Action::ClearSearch => {
                self.folder_browser.search.clear();
                self.folder_browser.search_cursor = 0;
                self.folder_browser.search_anchor = None;
                self.filter_folder_browser();
            }
            Action::Close => {
                self.change_dir_active = false;
                if self.folder_browser.save_as_return {
                    self.save_as_active = true;
                    self.modal.kind = None;
                }
                self.folder_browser.generation = self.folder_browser.generation.wrapping_add(1);
            }
        }
    }

    fn browser_typed_path(&self, value: &str) -> std::io::Result<PathBuf> {
        let value = value.trim().trim_matches('"');
        if value.is_empty() { return Err(std::io::Error::other("Enter a folder path.")); }
        if let Some(remote) = &self.remote_workspace {
            return if self.folder_browser_is_computer() {
                if !value.starts_with('/') && !value.starts_with("file://") {
                    return Err(std::io::Error::other("Enter an absolute remote folder path."));
                }
                remote.target_key(value)
            } else { remote.resolve_from_key(&self.folder_browser_path, value) };
        }
        if value == "~" || value.starts_with("~/") || value.starts_with("~\\") {
            if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
            {
                return Ok(PathBuf::from(home).join(value.get(2..).unwrap_or("")));
            }
        }
        if self.folder_browser_is_computer() && !Path::new(value).is_absolute() {
            return Err(std::io::Error::other("Enter an absolute folder path."));
        }
        Ok(self.folder_browser_path.join(value))
    }

    pub(crate) fn confirm_folder_browser(&mut self) {
        if self.folder_browser.creating {
            return;
        }
        if self.folder_browser_is_computer()
            && (self.folder_browser.new_folder || self.folder_browser.folder.trim().is_empty()) {
            return;
        }
        if self.folder_browser.new_folder {
            let name = self.folder_browser.folder.trim();
            if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
                self.folder_browser.error =
                    Some("Enter a folder name without path separators.".into());
                return;
            }
            let path = self.folder_browser_path.join(name);
            let generation = self.folder_browser.generation;
            self.folder_browser.creating = true;
            self.folder_browser.error = None;
            let sender = self.background.sender.clone();
            let request_path = path.clone();
            let remote = self.remote_workspace.clone();
            self.background.read_for_file("folder-create", &request_path, true, move || {
                let result = match remote {
                    Some(remote) => remote.remote_path(&path).and_then(|path| remote.client.call(crate::remote::Command::CreateDirectory { path })).map(|_| ()),
                    None => std::fs::create_dir(&path),
                };
                let error = result
                    .err()
                    .map(|e| format!("Cannot create folder: {e}"));
                let _ = sender.send(crate::background::BackgroundResult::FolderBrowserCreated {
                    path,
                    generation,
                    error,
                });
            });
            return;
        }
        let path = match self.browser_typed_path(&self.folder_browser.folder) {
            Ok(path) => path,
            Err(error) => { self.folder_browser.error = Some(error.to_string()); return; }
        };
        let remote_folder = self.remote_workspace.as_ref().is_some_and(|remote| remote.remote_path(&path).is_ok());
        if self.folder_browser.folder.trim().is_empty() || (!remote_folder && !path.is_dir()) {
            self.folder_browser.error =
                Some("Choose an existing folder, or use New folder to create one.".into());
            return;
        }
        if self.folder_browser.save_as_return {
            self.save_as_query = if let Some(remote) = &self.remote_workspace {
                let name = remote.target_key(&self.save_as_query).ok()
                    .and_then(|key| remote.remote_path(&key).ok())
                    .and_then(|path| path.file_name_bytes().map(<[u8]>::to_vec))
                    .unwrap_or_else(|| b"untitled.txt".to_vec());
                let destination = remote.remote_path(&path).and_then(|folder| folder.child(&name)).map(|path| remote.view_key(&path))
                    .and_then(|key| remote.display_path(&key));
                match destination {
                    Ok(path) => path,
                    Err(error) => { self.folder_browser.error = Some(error.to_string()); return; }
                }
            } else {
                let name = Path::new(self.save_as_query.trim().trim_matches('"'))
                    .file_name().unwrap_or_else(|| std::ffi::OsStr::new("untitled.txt"));
                path.join(name).display().to_string()
            };
            self.save_as_cursor = self.save_as_query.len();
            self.save_as_selection_anchor = None;
            crate::ui_modal::field_changed(self);
        } else {
            self.change_workspace(&path);
        }
        self.folder_browser_action(Action::Close);
    }

    pub(crate) fn folder_browser_key(&mut self, key: KeyEvent) {
        if self.folder_browser.creating {
            self.folder_browser.focus = Focus::Cancel;
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ')) {
                self.folder_browser_action(Action::Close);
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                if self.folder_browser.new_folder && !self.folder_browser.creating {
                    self.folder_browser.new_folder = false;
                    self.folder_browser.error = None;
                    self.filter_folder_browser();
                    self.folder_browser.focus = Focus::List;
                } else if self.folder_browser.focus == Focus::Address {
                    self.folder_browser.focus = Focus::List;
                } else {
                    self.folder_browser_action(Action::Close);
                }
                return;
            }
            KeyCode::Char('l' | 'L') if ctrl => {
                self.folder_browser_action(Action::Address);
                return;
            }
            KeyCode::Char('f' | 'F') if ctrl => {
                self.folder_browser.focus = Focus::Search;
                self.folder_browser.search_cursor = self.folder_browser.search.len();
                self.folder_browser.search_anchor = Some(0);
                return;
            }
            KeyCode::Char('n' | 'N') if ctrl && key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.folder_browser_action(Action::NewFolder);
                return;
            }
            KeyCode::Char('h' | 'H') if ctrl => {
                self.folder_browser_action(Action::Hidden);
                return;
            }
            KeyCode::Left if alt => {
                self.folder_browser_action(Action::Back);
                return;
            }
            KeyCode::Right if alt => {
                self.folder_browser_action(Action::Forward);
                return;
            }
            KeyCode::Up if alt => {
                self.folder_browser_up();
                return;
            }
            KeyCode::F(5) => {
                self.folder_browser_action(Action::Refresh);
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let order = [
                    Focus::Address,
                    Focus::Search,
                    Focus::Locations,
                    Focus::List,
                    Focus::Folder,
                    Focus::Open,
                    Focus::Cancel,
                ];
                let i = order
                    .iter()
                    .position(|&f| f == self.folder_browser.focus)
                    .unwrap_or(0);
                let reverse =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                let mut next = if reverse {
                    (i + order.len() - 1) % order.len()
                } else {
                    (i + 1) % order.len()
                };
                // Compact layouts hide the locations pane; do not strand focus there.
                if order[next] == Focus::Locations && self.folder_browser.location_area.is_empty() {
                    next = if reverse {
                        (next + order.len() - 1) % order.len()
                    } else {
                        (next + 1) % order.len()
                    };
                }
                self.folder_browser.focus = order[next];
                return;
            }
            KeyCode::Enter if ctrl => {
                self.confirm_folder_browser();
                return;
            }
            _ => {}
        }
        match self.folder_browser.focus {
            Focus::Address | Focus::Search | Focus::Folder => {
                if key.code == KeyCode::Enter {
                    match self.folder_browser.focus {
                        Focus::Address => {
                            match self.browser_typed_path(&self.change_dir_query) {
                                Ok(path) => self.navigate_folder_browser(path),
                                Err(error) => self.folder_browser.error = Some(error.to_string()),
                            }
                        }
                        Focus::Folder => self.confirm_folder_browser(),
                        _ => {
                            self.folder_browser.focus = Focus::List;
                        }
                    }
                } else if let Some(id) = self.folder_browser.focus.input() {
                    if self
                        .edit_text_input(id, key.code, key.modifiers, None)
                        .changed
                    {
                        self.text_input_changed(id);
                    }
                }
            }
            Focus::Locations => {
                let count = self.folder_browser.locations.len();
                match key.code {
                    KeyCode::Up => {
                        self.folder_browser.location_selected =
                            self.folder_browser.location_selected.saturating_sub(1)
                    }
                    KeyCode::Down => {
                        self.folder_browser.location_selected =
                            (self.folder_browser.location_selected + 1).min(count.saturating_sub(1))
                    }
                    KeyCode::Enter | KeyCode::Right => self.folder_browser_action(
                        Action::Location(self.folder_browser.location_selected),
                    ),
                    _ => {}
                }
                let visible = self.folder_browser.location_area.height.max(1) as usize;
                self.folder_browser.location_scroll = self
                    .folder_browser
                    .location_scroll
                    .min(self.folder_browser.location_selected);
                if self.folder_browser.location_selected
                    >= self.folder_browser.location_scroll + visible
                {
                    self.folder_browser.location_scroll =
                        self.folder_browser.location_selected + 1 - visible;
                }
            }
            Focus::Open | Focus::Cancel => {
                if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down) {
                    self.folder_browser.focus = if self.folder_browser.focus == Focus::Open {
                        Focus::Cancel
                    } else {
                        Focus::Open
                    };
                } else if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                    if self.folder_browser.focus == Focus::Open {
                        self.confirm_folder_browser();
                    } else {
                        self.folder_browser_action(Action::Close);
                    }
                }
            }
            Focus::List => {
                let selected = self.folder_browser_selected;
                let page = self
                    .folder_browser_area
                    .map_or(10, |r| r.height.max(1) as usize);
                match key.code {
                    KeyCode::Up => self.select_browser_row(selected.saturating_sub(1)),
                    KeyCode::Down => self.select_browser_row(selected.saturating_add(1)),
                    KeyCode::PageUp => self.select_browser_row(selected.saturating_sub(page)),
                    KeyCode::PageDown => self.select_browser_row(selected.saturating_add(page)),
                    KeyCode::Home => self.select_browser_row(0),
                    KeyCode::End => {
                        self.select_browser_row(self.folder_browser_count().saturating_sub(1))
                    }
                    KeyCode::Enter | KeyCode::Right => self.folder_browser_enter(),
                    KeyCode::Left | KeyCode::Backspace => self.folder_browser_up(),
                    KeyCode::Char(c) if !ctrl && !alt => {
                        self.folder_browser.focus = Focus::Search;
                        self.edit_text_input(
                            TextInputId::FolderSearch,
                            KeyCode::Char(c),
                            key.modifiers,
                            None,
                        );
                        self.filter_folder_browser();
                    }
                    _ => {}
                }
                self.folder_browser_scroll =
                    self.folder_browser_scroll.min(self.folder_browser_selected);
                if self.folder_browser_selected >= self.folder_browser_scroll + page {
                    self.folder_browser_scroll = self.folder_browser_selected + 1 - page;
                }
            }
        }
    }

    pub(crate) fn folder_browser_click(&mut self, col: u16, row: u16) {
        if let Some((_, action)) = self
            .folder_browser
            .controls
            .iter()
            .rev()
            .find(|(area, _)| area.contains((col, row).into()))
        {
            self.folder_browser_action(action.clone());
            return;
        }
        if let Some(index) = self
            .folder_browser_buttons
            .iter()
            .position(|&(x, y, w)| row == y && col >= x && col < x + w)
        {
            if index == 0 {
                self.confirm_folder_browser();
            } else {
                self.folder_browser_action(Action::Close);
            }
            return;
        }
        if self.folder_browser.creating {
            return;
        }
        if let Some(area) = self
            .folder_browser_area
            .filter(|a| a.contains((col, row).into()))
        {
            let index = self.folder_browser_scroll + (row - area.y) as usize;
            if index < self.folder_browser_count() {
                let double = self
                    .folder_browser
                    .last_click
                    .is_some_and(|(i, time)| i == index && time.elapsed().as_millis() < 400);
                self.select_browser_row(index);
                self.folder_browser.last_click = Some((index, Instant::now()));
                if double {
                    self.folder_browser_enter();
                }
            } else {
                self.filter_folder_browser();
                self.folder_browser.focus = Focus::List;
            }
        }
    }

    pub(crate) fn hover_folder_browser_button(&mut self, col: u16, row: u16) {
        if let Some(index) = self.folder_browser_buttons.iter()
            .position(|&(x, y, width)| row == y && col >= x && col < x + width)
            && (index != 0 || (!self.folder_browser.creating
                && (!self.folder_browser_is_computer() || !self.folder_browser.folder.is_empty())))
        {
            self.folder_browser.focus = if index == 0 { Focus::Open } else { Focus::Cancel };
        }
    }

    pub(crate) fn scroll_folder_browser(&mut self, col: u16, row: u16, up: bool) {
        self.folder_browser.last_click = None;
        if self
            .folder_browser
            .location_area
            .contains((col, row).into())
        {
            let max = self
                .folder_browser
                .locations
                .len()
                .saturating_sub(self.folder_browser.location_area.height as usize);
            self.folder_browser.location_scroll = if up {
                self.folder_browser.location_scroll.saturating_sub(1)
            } else {
                self.folder_browser
                    .location_scroll
                    .saturating_add(1)
                    .min(max)
            };
        } else if let Some(area) = self
            .folder_browser_area
            .filter(|r| r.contains((col, row).into()))
        {
            let max = self
                .folder_browser_count()
                .saturating_sub(area.height.max(1) as usize);
            self.folder_browser_scroll = if up {
                self.folder_browser_scroll.saturating_sub(3)
            } else {
                self.folder_browser_scroll.saturating_add(3).min(max)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ready(app: &mut App) {
        app.wait_for_background(Duration::from_secs(10), |a| {
            !a.folder_browser_loading && !a.folder_browser.creating
        });
    }

    #[test]
    fn folder_browser_can_go_above_the_filesystem_root_and_navigate_disk_history() {
        let root = tempfile::tempdir().unwrap();
        let mut app = App::new_deferred(root.path().to_path_buf());
        app.open_folder_browser();
        ready(&mut app);
        let mut depth = 0;
        while !app.folder_browser_is_computer() {
            assert!(depth < 64);
            app.folder_browser_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            ready(&mut app);
            depth += 1;
        }
        assert!(app.folder_browser_parent().is_none());
        assert!(!app.folder_browser_entries.is_empty());
        assert!(app.folder_browser.locations.iter().any(|(label, path)| label == "Computer" && path.as_os_str().is_empty()));
        let computer_entries = app.folder_browser_entries.clone();
        for (name, path) in crate::filesystem_roots::list().unwrap() {
            assert!(computer_entries.contains(&path));
            assert_eq!(app.folder_browser_entry_name(&path), name);
        }
        app.folder_browser_action(Action::NewFolder);
        assert!(!app.folder_browser.new_folder);
        app.confirm_folder_browser();
        assert!(app.change_dir_active);
        assert_eq!(app.explorer.root, root.path());
        assert!(app.browser_typed_path("relative-folder").is_err());

        let disk_row = computer_entries.iter().position(|path| root.path().starts_with(path)).unwrap();
        app.select_browser_row(disk_row);
        let disk = computer_entries[disk_row].clone();
        assert_eq!(app.folder_browser.folder, app.display_path(&disk));
        app.folder_browser_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ready(&mut app);
        assert_eq!(app.folder_browser_path, disk);
        app.folder_browser_action(Action::Back);
        ready(&mut app);
        assert!(app.folder_browser_is_computer());
        app.folder_browser_action(Action::Forward);
        ready(&mut app);
        assert_eq!(app.folder_browser_path, disk);

        let computer = app.folder_browser.locations.iter().position(|(label, _)| label == "Computer").unwrap();
        app.folder_browser_action(Action::Location(computer));
        ready(&mut app);
        assert!(app.folder_browser_is_computer());
        app.folder_browser_action(Action::Back);
        ready(&mut app);
        assert_eq!(app.folder_browser_path, disk);
    }

    #[test]
    fn folder_browser_computer_rejects_stale_directory_results_and_invalid_addresses() {
        let root = tempfile::tempdir().unwrap();
        let mut app = App::new_deferred(root.path().to_path_buf());
        app.open_folder_browser();
        ready(&mut app);
        let generation = app.folder_browser.generation;
        app.navigate_folder_browser(PathBuf::new());
        ready(&mut app);
        let roots = app.folder_browser_entries.clone();
        app.background.sender.send(crate::background::BackgroundResult::FolderBrowserEntries {
            path: root.path().to_path_buf(), generation, entries: vec![root.path().join("stale")],
            modified: HashMap::new(), locations: Vec::new(), error: None,
        }).unwrap();
        app.poll_background_results();
        assert_eq!(app.folder_browser_entries, roots);
        app.folder_browser_action(Action::Address);
        app.change_dir_query = "relative-folder".into();
        app.folder_browser_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.folder_browser_is_computer());
        assert!(app.folder_browser.error.as_ref().unwrap().contains("absolute"));
        app.change_dir_query = root.path().display().to_string();
        app.folder_browser_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ready(&mut app);
        assert_eq!(app.folder_browser_path, root.path());
    }

    #[test]
    fn listing_cache_reuses_rows_and_invalidates_filter_sort_and_refresh() {
        let mut app = App::test_new();
        app.folder_browser_entries = ["alpha", "beta", "Éclair"]
            .map(PathBuf::from).to_vec();
        let rows = app.folder_browser_indices();
        assert_eq!(&*rows, &[0, 1, 2]);
        assert!(Arc::ptr_eq(&rows, &app.folder_browser_indices()));

        app.folder_browser.search = "ÉC".into();
        assert_eq!(&*app.folder_browser_indices(), &[2]);
        app.folder_browser.search.clear();
        app.folder_browser.descending = true;
        assert_eq!(&*app.folder_browser_indices(), &[2, 1, 0]);
        app.folder_browser.sort_modified = true;
        app.folder_browser.modified.insert(PathBuf::from("alpha"), SystemTime::UNIX_EPOCH + Duration::from_secs(20));
        app.folder_browser.modified.insert(PathBuf::from("beta"), SystemTime::UNIX_EPOCH + Duration::from_secs(30));
        assert_eq!(&*app.folder_browser_indices(), &[1, 0, 2]);

        // Background replacement may keep exactly the same number of rows.
        app.folder_browser_entries = ["changed", "next", "last"].map(PathBuf::from).to_vec();
        app.filter_folder_browser();
        app.folder_browser.search = "changed".into();
        assert_eq!(&*app.folder_browser_indices(), &[0]);
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_folder_browser_cached_listing() {
        let mut app = App::test_new();
        app.folder_browser_entries = (0..20_000)
            .map(|index| PathBuf::from(format!("workspace-folder-{index}"))).collect();
        app.folder_browser.search = "workspace".into();
        let mut rebuilt = Vec::new();
        let mut cached = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            for _ in 0..100 {
                // Reference: the original method lowercased and filtered every
                // directory on redraw, row selection and scrollbar movement.
                let query = app.folder_browser.search.to_lowercase();
                let indices: Vec<_> = app.folder_browser_entries.iter().enumerate()
                    .filter(|(_, path)| path.file_name().unwrap_or_default()
                        .to_string_lossy().to_lowercase().contains(&query))
                    .map(|(index, _)| index).collect();
                std::hint::black_box(indices);
            }
            rebuilt.push(started.elapsed());
            std::hint::black_box(app.folder_browser_indices());
            let started = Instant::now();
            for _ in 0..100 {
                std::hint::black_box(app.folder_browser_indices());
            }
            cached.push(started.elapsed());
        }
        rebuilt.sort();
        cached.sort();
        eprintln!("folder browser 20,000 directories, 100 reads: rebuilt={:?}, cached={:?}", rebuilt[3], cached[3]);
    }

    #[test]
    fn folder_browser_history_branches_and_hidden_filter_refreshes() {
        let root = tempfile::tempdir().unwrap();
        let alpha = root.path().join("alpha");
        let beta = root.path().join("beta");
        std::fs::create_dir(&alpha).unwrap();
        std::fs::create_dir(&beta).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();
        let mut app = App::new(root.path().to_path_buf());
        app.open_folder_browser();
        ready(&mut app);
        assert_eq!(app.folder_browser_entries.len(), 2);
        app.navigate_folder_browser(alpha.clone());
        ready(&mut app);
        app.folder_browser_action(Action::Back);
        ready(&mut app);
        assert_eq!(app.folder_browser_path, root.path());
        app.folder_browser_action(Action::Forward);
        ready(&mut app);
        assert_eq!(app.folder_browser_path, alpha);
        app.folder_browser_action(Action::Back);
        ready(&mut app);
        app.navigate_folder_browser(beta);
        ready(&mut app);
        assert!(app.folder_browser.forward.is_empty());
        app.folder_browser_up();
        ready(&mut app);
        app.folder_browser_action(Action::Hidden);
        ready(&mut app);
        assert_eq!(app.folder_browser_entries.len(), 3);
    }

    #[test]
    fn folder_browser_filter_selection_double_click_and_workspace_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let alpha = root.path().join("alpha");
        let beta = root.path().join("beta");
        std::fs::create_dir(&alpha).unwrap();
        std::fs::create_dir(&beta).unwrap();
        let mut app = App::new(root.path().to_path_buf());
        app.open_folder_browser();
        ready(&mut app);
        app.folder_browser.search = "BET".into();
        app.filter_folder_browser();
        app.folder_browser_area = Some(Rect::new(10, 5, 50, 10));
        // Hover must not change selection or make a first click navigate.
        crate::mouse::handle_mouse(
            &mut app,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Moved,
                column: 12,
                row: 6,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.folder_browser_selected, 0);
        app.folder_browser_click(12, 6);
        assert_eq!(app.folder_browser_path, root.path());
        assert_eq!(app.folder_browser.folder, beta.display().to_string());
        app.folder_browser_click(12, 6);
        ready(&mut app);
        assert_eq!(app.folder_browser_path, beta);
        assert_eq!(app.explorer.root, root.path());
        app.confirm_folder_browser();
        assert!(!app.change_dir_active);
        assert_eq!(app.explorer.root, beta);
    }

    #[test]
    fn folder_browser_path_input_paste_and_cancel_do_not_edit_workbench() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("unicode-你好");
        std::fs::create_dir(&child).unwrap();
        let mut app = App::new(root.path().to_path_buf());
        app.open_folder_browser();
        ready(&mut app);
        app.folder_browser_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(app.paste_into_focused_text_input("unicode-你好"));
        app.folder_browser_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ready(&mut app);
        assert_eq!(app.folder_browser_path, child);
        assert!(app.paste_into_focused_text_input("ignore this paste while browsing"));
        app.folder_browser_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.change_dir_active);
        assert_eq!(app.explorer.root, root.path());
    }

    #[test]
    fn folder_browser_reports_read_errors_and_rejects_stale_same_path_results() {
        let root = tempfile::tempdir().unwrap();
        let mut app = App::new(root.path().to_path_buf());
        app.open_folder_browser();
        ready(&mut app);
        let generation = app.folder_browser.generation;
        app.folder_browser_action(Action::Hidden);
        ready(&mut app);
        app.background
            .sender
            .send(crate::background::BackgroundResult::FolderBrowserEntries {
                path: root.path().to_path_buf(),
                generation,
                entries: vec![root.path().join("stale")],
                modified: HashMap::new(),
                locations: Vec::new(),
                error: None,
            })
            .unwrap();
        app.poll_background_results();
        assert!(app.folder_browser_entries.is_empty());
        app.navigate_folder_browser(root.path().join("missing"));
        ready(&mut app);
        assert!(
            app.folder_browser
                .error
                .as_ref()
                .unwrap()
                .contains("Cannot read")
        );
        app.confirm_folder_browser();
        assert!(app.change_dir_active);
        assert_eq!(app.explorer.root, root.path());
        app.folder_browser_action(Action::Back);
        ready(&mut app);
        assert!(app.folder_browser.error.is_none());
    }

    #[test]
    fn folder_browser_new_folder_success_collision_and_cancel() {
        let root = tempfile::tempdir().unwrap();
        let mut app = App::new(root.path().to_path_buf());
        app.open_folder_browser();
        ready(&mut app);
        app.folder_browser_action(Action::NewFolder);
        app.folder_browser.folder = "../outside".into();
        app.confirm_folder_browser();
        assert!(app.folder_browser.error.is_some());
        app.folder_browser.folder = "Project".into();
        app.confirm_folder_browser();
        ready(&mut app);
        assert_eq!(app.folder_browser_path, root.path().join("Project"));
        assert!(app.folder_browser_path.is_dir());
        app.folder_browser_up();
        ready(&mut app);
        app.folder_browser_action(Action::NewFolder);
        app.folder_browser.folder = "Project".into();
        app.confirm_folder_browser();
        ready(&mut app);
        assert!(
            app.folder_browser
                .error
                .as_ref()
                .unwrap()
                .contains("Cannot create")
        );
        app.folder_browser_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.folder_browser.new_folder);
        assert!(app.change_dir_active);
    }
}
