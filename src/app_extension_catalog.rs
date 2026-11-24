//! Marketplace listings, update checks and one-click installs for the Extensions sidebar.
use crate::app::App;
use crate::extension_catalog::{Catalog, Refresh};
use crate::extension_install::{ExtensionSource, InstalledExtension};
use std::time::Duration;

/// A startup check waits this long so it never competes with opening the window.
const STARTUP_DELAY: Duration = Duration::from_secs(30);

/// Sidebar state for marketplaces and updates.
#[derive(Default)]
pub(crate) struct CatalogState {
    pub catalog: Catalog,
    generation: u64,
    pub refreshing: bool,
    /// A startup check (cache now, network later) has been queued.
    started: bool,
    /// Marketplace indexes were asked for in this session.
    fetched: bool,
    /// Repository keys with a one-click install or update in progress.
    pub installing: Vec<String>,
}

impl App {
    /// Installed extensions that came from a GitHub release, by repository link.
    fn release_installed_repositories(&self) -> Vec<String> {
        self.extension_views()
            .into_iter()
            .filter(|view| view.removable && !view.repository.is_empty())
            .map(|view| view.repository)
            .collect()
    }

    pub(crate) fn request_extension_catalog(&mut self, mode: Refresh) {
        self.request_extension_catalog_after(mode, None);
    }

    fn request_extension_catalog_after(&mut self, mode: Refresh, delay: Option<Duration>) {
        if cfg!(test) {
            return;
        }
        let state = &mut self.ext_catalog;
        state.generation = state.generation.wrapping_add(1);
        state.fetched |= mode != Refresh::Cached;
        state.refreshing = mode != Refresh::Cached && delay.is_none();
        let generation = state.generation;
        let configured = self
            .settings_effective
            .extension_marketplaces
            .clone()
            .unwrap_or_default();
        let installed = self.release_installed_repositories();
        let sender = self.background.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("extension-catalog".into())
            .spawn(move || {
                // Network checks are optional: yield to every editor worker.
                let _priority = crate::worker_priority::CompletionPriority::new();
                let client = crate::extension_catalog::LazyGitHub::default();
                let path = crate::extension_catalog::cache_path();
                if let Some(delay) = delay {
                    // Show what the cache already knows, then check later.
                    let catalog = crate::extension_catalog::refresh(
                        Refresh::Cached,
                        &configured,
                        &installed,
                        &path,
                        &client,
                    );
                    if sender
                        .send(crate::background::BackgroundResult::ExtensionCatalog {
                            generation,
                            catalog,
                        })
                        .is_err()
                    {
                        return;
                    }
                    std::thread::sleep(delay);
                }
                let catalog = crate::extension_catalog::refresh(
                    mode,
                    &configured,
                    &installed,
                    &path,
                    &client,
                );
                let _ = sender.send(crate::background::BackgroundResult::ExtensionCatalog {
                    generation,
                    catalog,
                });
            });
        if spawned.is_err() {
            self.ext_catalog.refreshing = false;
        }
    }

    /// After extensions are discovered: read the cache, and check for updates
    /// once the window has settled (at most every 12 hours, see `UPDATE_INTERVAL`).
    pub(crate) fn start_extension_catalog(&mut self) {
        if self.ext_catalog.started {
            return;
        }
        self.ext_catalog.started = true;
        self.request_extension_catalog_after(Refresh::Due, Some(STARTUP_DELAY));
    }

    /// Opening the Extensions sidebar asks each marketplace whether its index
    /// changed, once per session.
    pub(crate) fn ensure_extension_catalog(&mut self) {
        if !self.ext_catalog.fetched {
            self.ext_catalog.started = true;
            self.request_extension_catalog(Refresh::Due);
        }
    }

    pub(crate) fn finish_extension_catalog(&mut self, generation: u64, catalog: Catalog) {
        if generation != self.ext_catalog.generation {
            return;
        }
        self.ext_catalog.refreshing = false;
        self.ext_catalog.catalog = catalog;
        self.clamp_extensions_selection();
    }

    /// Marketplace entries that are not installed, in index order.
    pub(crate) fn marketplace_listings(&self) -> Vec<crate::extension_catalog::Listing> {
        let views = self.extension_views();
        self.ext_catalog
            .catalog
            .listings
            .iter()
            .filter(|listing| {
                let key = listing.key();
                !views.iter().any(|view| {
                    view.id == listing.id
                        || crate::extension_catalog::repository_key(&view.repository).as_ref()
                            == Some(&key)
                })
            })
            .cloned()
            .collect()
    }

    /// The newer release an installed extension can update to.
    pub(crate) fn extension_update(
        &self,
        view: &crate::extension_manifest::ExtensionRecordView,
    ) -> Option<crate::extension_versions::ReleaseVersion> {
        if !view.removable {
            return None;
        }
        self.ext_catalog
            .catalog
            .update_for(&view.repository, &view.version)
            .cloned()
    }

    pub(crate) fn extension_installing(&self, repository: &str) -> bool {
        crate::extension_catalog::repository_key(repository)
            .is_some_and(|key| self.ext_catalog.installing.contains(&key))
    }

    /// Install (or update to) the newest stable release, without a picker.
    pub(crate) fn install_latest_extension(&mut self, repository: &str) {
        let Ok(source) = ExtensionSource::parse(repository) else {
            self.notify_error(format!("Not a GitHub repository link: {repository}"));
            return;
        };
        let key = source.repository_key();
        if self.ext_catalog.installing.contains(&key) {
            return;
        }
        if self.extension_manager.has_unsaved_documents() {
            self.notify_error(
                "Save or close modified extension documents before installing or updating extensions.",
            );
            return;
        }
        self.ext_catalog.installing.push(key.clone());
        let root = crate::portable::data_dir().join("extensions");
        let sender = self.background.sender.clone();
        let job_key = key.clone();
        if self
            .background
            .maintenance_sender()
            .send(Box::new(move || {
                let result =
                    crate::extension_catalog::latest_release(&source).and_then(|version| {
                        crate::extension_install::install_version(&source, &version, &root)
                    });
                let _ = sender.send(
                    crate::background::BackgroundResult::ExtensionQuickInstalled {
                        key: job_key,
                        result,
                    },
                );
            }))
            .is_err()
        {
            self.ext_catalog.installing.retain(|item| item != &key);
            self.notify_error("Extension installer worker is unavailable.");
        }
    }

    pub(crate) fn finish_extension_quick_install(
        &mut self,
        key: String,
        result: Result<Vec<InstalledExtension>, String>,
    ) {
        self.ext_catalog.installing.retain(|item| item != &key);
        match result {
            Ok(installed) => {
                let names = installed
                    .iter()
                    .map(|extension| format!("{} {}", extension.name, extension.version))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.request_extension_scan();
                self.notify(format!("Installed {names}. Loading extensions…"));
            }
            Err(error) => self.notify_error(format!("Extension installation failed: {error}")),
        }
    }

    /// The install dropdown: pick any published version of this repository.
    pub(crate) fn choose_extension_version(&mut self, repository: &str) {
        self.open_extension_install_prompt();
        self.palette.query = repository.to_owned();
        self.palette.cursor = self.palette.query.chars().count();
        self.palette.selection_anchor = None;
        self.refresh_extension_versions();
    }

    pub(crate) fn update_all_extensions(&mut self) {
        let repositories: Vec<String> = self
            .extension_views()
            .iter()
            .filter(|view| self.extension_update(view).is_some())
            .map(|view| view.repository.clone())
            .collect();
        if repositories.is_empty() {
            self.notify("All extensions are up to date");
            return;
        }
        for repository in repositories {
            self.install_latest_extension(&repository);
        }
    }

    pub(crate) fn check_extension_updates(&mut self) {
        self.request_extension_catalog(Refresh::Now);
        self.notify("Checking extension marketplaces and updates…");
    }
}
