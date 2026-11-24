//! Marketplaces and update checks.
//!
//! A marketplace is a GitHub repository whose `marketplace.json` lists extension
//! repositories. The official one is always included; more can be added with the
//! `extensions.marketplaces` setting. Versions are never stored in an index: they
//! come from each extension's GitHub releases, exactly as for a pasted link.
//!
//! Everything fetched is kept in one cache file with its ETag, so a refresh asks
//! GitHub "has this changed?" and an unchanged answer (304) costs no rate limit.
//! Update checks run on a low-priority thread, at most once per
//! [`UPDATE_INTERVAL`] unless the user asks, one release request per installed
//! extension that came from GitHub.
use crate::extension_release_source::{ExtensionSource, GitHubClient};
use crate::extension_versions::ReleaseVersion;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const OFFICIAL_MARKETPLACE: &str = "https://github.com/im-henri/vtcode-extensions";
const INDEX_FILE: &str = "marketplace.json";
const SCHEMA_VERSION: u32 = 1;
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
const MAX_RELEASE_BYTES: u64 = 1024 * 1024;
pub(crate) const UPDATE_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
/// Spacing between release requests, so a check never bursts.
const REQUEST_SPACING: Duration = Duration::from_millis(300);

/// An extension a marketplace offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listing {
    pub id: String,
    pub name: String,
    pub description: String,
    pub repository: String,
    pub marketplace: String,
    pub official: bool,
}

impl Listing {
    pub(crate) fn key(&self) -> String {
        repository_key(&self.repository).unwrap_or_default()
    }
}

/// What the sidebar shows: marketplace listings and the newest release of
/// each installed extension, by repository key.
#[derive(Clone, Debug, Default)]
pub(crate) struct Catalog {
    pub listings: Vec<Listing>,
    pub latest: BTreeMap<String, ReleaseVersion>,
    pub errors: Vec<String>,
}

impl Catalog {
    /// The newer release for an installed extension, if one is published.
    pub(crate) fn update_for(&self, repository: &str, installed: &str) -> Option<&ReleaseVersion> {
        let latest = self.latest.get(&repository_key(repository)?)?;
        let installed = crate::extension_release_source::semantic_version(installed)?;
        (latest.semantic_version()? > installed).then_some(latest)
    }
}

pub(crate) fn repository_key(repository: &str) -> Option<String> {
    ExtensionSource::parse(repository)
        .ok()
        .map(|source| source.repository_key())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    schema_version: u32,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    extensions: Vec<IndexEntry>,
}

#[derive(Deserialize)]
struct IndexEntry {
    id: String,
    repository: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.contains('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn display(text: Option<String>, limit: usize) -> String {
    text.unwrap_or_default()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Read one marketplace index. Entries that are not a plain repository link
/// or repeat an earlier id are skipped, so one bad line hides only itself.
pub(crate) fn parse_index(
    bytes: &[u8],
    source: &ExtensionSource,
    official: bool,
) -> Result<Vec<Listing>, String> {
    let index: IndexFile = serde_json::from_slice(bytes)
        .map_err(|error| format!("{}: invalid {INDEX_FILE}: {error}", source.repository))?;
    if index.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "{}: {INDEX_FILE} uses schema version {}, which this VTCode does not read. Update VTCode.",
            source.repository, index.schema_version
        ));
    }
    let marketplace = match display(index.name, 60) {
        name if name.is_empty() => format!("{}/{}", source.owner, source.project),
        name => name,
    };
    let mut seen = rustc_hash::FxHashSet::default();
    let mut listings = Vec::new();
    for entry in index.extensions {
        let Ok(repo) = ExtensionSource::parse(&entry.repository) else {
            continue;
        };
        if repo.reference.is_some() || !valid_id(&entry.id) || !seen.insert(entry.id.clone()) {
            continue;
        }
        let name = match display(entry.name, 80) {
            name if name.is_empty() => entry.id.clone(),
            name => name,
        };
        listings.push(Listing {
            id: entry.id,
            name,
            description: display(entry.description, 300),
            repository: repo.repository,
            marketplace: marketplace.clone(),
            official,
        });
    }
    Ok(listings)
}

/// The official marketplace first, then configured ones; repeats and invalid
/// links are dropped (a configured copy of the official one adds nothing).
pub(crate) fn marketplaces(configured: &[String]) -> Vec<(ExtensionSource, bool)> {
    let official = ExtensionSource::parse(OFFICIAL_MARKETPLACE).expect("official marketplace");
    let mut keys = vec![official.repository_key()];
    let mut sources = vec![(official, true)];
    for link in configured {
        if let Ok(source) = ExtensionSource::parse(link)
            && source.reference.is_none()
            && !keys.contains(&source.repository_key())
        {
            keys.push(source.repository_key());
            sources.push((source, false));
        }
    }
    sources
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CacheFile {
    /// Unix seconds of the last completed update check.
    updates_checked: u64,
    marketplaces: BTreeMap<String, CachedIndex>,
    releases: BTreeMap<String, CachedRelease>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CachedIndex {
    etag: Option<String>,
    body: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CachedRelease {
    etag: Option<String>,
    latest: Option<ReleaseVersion>,
}

pub(crate) fn cache_path() -> PathBuf {
    crate::portable::data_dir().join("extension-catalog.json")
}

fn load(path: &Path) -> CacheFile {
    std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() <= 8 * 1024 * 1024)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(path: &Path, cache: &CacheFile) {
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// What a refresh may contact GitHub for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refresh {
    /// Only the disk cache.
    Cached,
    /// Marketplace indexes, plus updates when the last check is older than [`UPDATE_INTERVAL`].
    Due,
    /// Marketplace indexes and updates now (the user asked).
    Now,
}

/// Abstracts GitHub so the cache logic can be tested offline.
pub(crate) trait Fetch {
    fn fetch(
        &self,
        url: &str,
        accept: &str,
        etag: Option<&str>,
        limit: u64,
    ) -> Result<Option<crate::extension_release_source::Fetched>, String>;
}

impl Fetch for GitHubClient {
    fn fetch(
        &self,
        url: &str,
        accept: &str,
        etag: Option<&str>,
        limit: u64,
    ) -> Result<Option<crate::extension_release_source::Fetched>, String> {
        self.get_if_changed(url, accept, etag, limit)
    }
}

/// Creates the GitHub client (which may ask `gh` for a token) only when a
/// request is actually made, so a cache-only refresh starts no process.
#[derive(Default)]
pub(crate) struct LazyGitHub(std::cell::OnceCell<GitHubClient>);

impl Fetch for LazyGitHub {
    fn fetch(
        &self,
        url: &str,
        accept: &str,
        etag: Option<&str>,
        limit: u64,
    ) -> Result<Option<crate::extension_release_source::Fetched>, String> {
        self.0
            .get_or_init(GitHubClient::new)
            .get_if_changed(url, accept, etag, limit)
    }
}

/// Builds the catalog. Runs on a background thread; blocking is expected.
pub(crate) fn refresh(
    mode: Refresh,
    configured: &[String],
    installed: &[String],
    path: &Path,
    client: &dyn Fetch,
) -> Catalog {
    let mut cache = load(path);
    let mut catalog = Catalog::default();
    let mut changed = false;
    for (source, official) in marketplaces(configured) {
        let key = source.repository_key();
        if mode != Refresh::Cached {
            let entry = cache.marketplaces.get(&key);
            match client.fetch(
                &source.api(&format!("contents/{INDEX_FILE}")),
                "application/vnd.github.raw+json",
                entry.and_then(|entry| entry.etag.as_deref()),
                MAX_INDEX_BYTES,
            ) {
                Ok(Some(fetched)) => match String::from_utf8(fetched.body) {
                    Ok(body) => {
                        cache.marketplaces.insert(
                            key.clone(),
                            CachedIndex {
                                etag: fetched.etag,
                                body,
                            },
                        );
                        changed = true;
                    }
                    Err(_) => catalog
                        .errors
                        .push(format!("{}: {INDEX_FILE} is not UTF-8", source.repository)),
                },
                Ok(None) => {}
                Err(error) => catalog
                    .errors
                    .push(format!("{}/{}: {error}", source.owner, source.project)),
            }
        }
        if let Some(entry) = cache.marketplaces.get(&key) {
            match parse_index(entry.body.as_bytes(), &source, official) {
                Ok(listings) => catalog.listings.extend(listings),
                Err(error) => catalog.errors.push(error),
            }
        }
    }
    // One listing per extension id, the first marketplace (the official one) winning.
    let mut seen = rustc_hash::FxHashSet::default();
    catalog
        .listings
        .retain(|listing| seen.insert(listing.id.clone()));

    let mut repositories: Vec<ExtensionSource> = installed
        .iter()
        .filter_map(|link| ExtensionSource::parse(link).ok())
        .collect();
    repositories.sort_by_key(ExtensionSource::repository_key);
    repositories.dedup_by_key(|source| source.repository_key());
    let due = match mode {
        Refresh::Cached => false,
        Refresh::Due => {
            now().saturating_sub(cache.updates_checked) >= UPDATE_INTERVAL.as_secs()
                || repositories
                    .iter()
                    .any(|source| !cache.releases.contains_key(&source.repository_key()))
        }
        Refresh::Now => true,
    };
    if due && !repositories.is_empty() {
        let mut complete = true;
        for (position, source) in repositories.iter().enumerate() {
            if position > 0 {
                std::thread::sleep(REQUEST_SPACING);
            }
            let key = source.repository_key();
            let entry = cache.releases.get(&key);
            match client.fetch(
                &source.api("releases/latest"),
                "application/vnd.github+json",
                entry.and_then(|entry| entry.etag.as_deref()),
                MAX_RELEASE_BYTES,
            ) {
                Ok(Some(fetched)) => {
                    cache.releases.insert(
                        key,
                        CachedRelease {
                            etag: fetched.etag,
                            latest: crate::extension_versions::release_version(
                                source,
                                &fetched.body,
                            ),
                        },
                    );
                    changed = true;
                }
                Ok(None) => {}
                // No published release yet: nothing to update to.
                Err(error) if error.contains("404") => {
                    cache.releases.insert(key, CachedRelease::default());
                    changed = true;
                }
                Err(_) => complete = false,
            }
        }
        if complete {
            cache.updates_checked = now();
            changed = true;
        }
    }
    for source in &repositories {
        if let Some(latest) = cache
            .releases
            .get(&source.repository_key())
            .and_then(|entry| entry.latest.clone())
        {
            catalog.latest.insert(source.repository_key(), latest);
        }
    }
    if changed {
        save(path, &cache);
    }
    catalog
}

/// The newest stable release of one repository, fetched fresh for an install.
pub(crate) fn latest_release(source: &ExtensionSource) -> Result<ReleaseVersion, String> {
    let client = GitHubClient::new();
    let fetched = client
        .get_if_changed(
            &source.api("releases/latest"),
            "application/vnd.github+json",
            None,
            MAX_RELEASE_BYTES,
        )?
        .ok_or("GitHub returned no release")?;
    crate::extension_versions::release_version(source, &fetched.body).ok_or_else(|| {
        format!(
            "The latest release of {} has no installable {} package. Choose another version.",
            source.repository,
            crate::extension_versions::PACKAGE_ASSET
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_release_source::Fetched;
    use std::cell::RefCell;

    fn source(link: &str) -> ExtensionSource {
        ExtensionSource::parse(link).unwrap()
    }

    #[test]
    fn index_keeps_valid_repository_entries_and_skips_the_rest() {
        let index = br#"{"schemaVersion": 1, "name": "Official", "extensions": [
            {"id": "vtcode.hex-editor", "repository": "https://github.com/im-henri/vtcode-ext-hex-editor", "name": "Hex Editor", "description": "Bytes\nand more"},
            {"id": "vtcode.bitcalc", "repository": "https://github.com/im-henri/vtcode-ext-bitcalc"},
            {"id": "vtcode.bitcalc", "repository": "https://github.com/someone/else"},
            {"id": "bad", "repository": "https://github.com/a/b"},
            {"id": "x.pinned", "repository": "https://github.com/a/b/releases/tag/v1.0.0"},
            {"id": "x.elsewhere", "repository": "https://gitlab.com/a/b"}
        ]}"#;
        let listings = parse_index(index, &source(OFFICIAL_MARKETPLACE), true).unwrap();
        assert_eq!(listings.len(), 2);
        assert_eq!(listings[0].name, "Hex Editor");
        assert_eq!(listings[0].description, "Bytes and more");
        assert_eq!(listings[0].marketplace, "Official");
        assert_eq!(listings[1].name, "vtcode.bitcalc");
        assert!(listings.iter().all(|listing| listing.official));
        assert!(
            parse_index(
                br#"{"schemaVersion": 2, "extensions": []}"#,
                &source(OFFICIAL_MARKETPLACE),
                true
            )
            .unwrap_err()
            .contains("schema version 2")
        );
    }

    #[test]
    fn the_official_marketplace_is_always_first_and_never_repeated() {
        let sources = marketplaces(&[
            "https://github.com/IM-HENRI/vtcode-extensions".into(),
            "not a link".into(),
            "https://github.com/someone/market".into(),
            "https://github.com/someone/market.git".into(),
        ]);
        let keys: Vec<_> = sources
            .iter()
            .map(|(source, official)| (source.repository_key(), *official))
            .collect();
        assert_eq!(
            keys,
            [
                ("im-henri/vtcode-extensions".into(), true),
                ("someone/market".into(), false)
            ]
        );
    }

    /// Serves fixed bodies; answers 304 when the request carries the served ETag.
    struct Fake {
        responses: BTreeMap<String, (String, Vec<u8>)>,
        requests: RefCell<Vec<(String, Option<String>)>>,
    }

    impl Fetch for Fake {
        fn fetch(
            &self,
            url: &str,
            _accept: &str,
            etag: Option<&str>,
            _limit: u64,
        ) -> Result<Option<Fetched>, String> {
            self.requests
                .borrow_mut()
                .push((url.into(), etag.map(str::to_owned)));
            let Some((tag, body)) = self.responses.get(url) else {
                return Err("GitHub release request failed: http status: 404".into());
            };
            if etag == Some(tag.as_str()) {
                return Ok(None);
            }
            Ok(Some(Fetched {
                etag: Some(tag.clone()),
                body: body.clone(),
            }))
        }
    }

    fn release(tag: &str) -> Vec<u8> {
        serde_json::to_vec(
            &serde_json::json!({"id": 7, "tag_name": tag, "draft": false, "prerelease": false,
            "assets": [{"id": 9, "name": "vtcode-extension.zip", "size": 10, "state": "uploaded",
                "digest": format!("sha256:{}", "a".repeat(64))}]}),
        )
        .unwrap()
    }

    #[test]
    fn refresh_uses_etags_throttles_update_checks_and_reports_newer_releases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let index = br#"{"schemaVersion": 1, "extensions": [
            {"id": "vtcode.hex-editor", "repository": "https://github.com/im-henri/vtcode-ext-hex-editor"}]}"#;
        let index_url =
            "https://api.github.com/repos/im-henri/vtcode-extensions/contents/marketplace.json";
        let release_url =
            "https://api.github.com/repos/im-henri/vtcode-ext-hex-editor/releases/latest";
        let fake = Fake {
            responses: BTreeMap::from([
                (index_url.into(), ("\"i1\"".into(), index.to_vec())),
                (release_url.into(), ("\"r1\"".into(), release("v0.3.0"))),
            ]),
            requests: RefCell::default(),
        };
        let installed = ["https://github.com/im-henri/vtcode-ext-hex-editor".to_string()];

        let catalog = refresh(Refresh::Due, &[], &installed, &path, &fake);
        assert_eq!(catalog.listings.len(), 1);
        let hex = "https://github.com/im-henri/vtcode-ext-hex-editor";
        assert_eq!(catalog.update_for(hex, "0.2.0").unwrap().name, "v0.3.0");
        assert!(catalog.update_for(hex, "0.3.0").is_none());
        assert_eq!(fake.requests.borrow().len(), 2);

        // Within the interval only the index is asked about, conditionally.
        fake.requests.borrow_mut().clear();
        let catalog = refresh(Refresh::Due, &[], &installed, &path, &fake);
        assert_eq!(
            *fake.requests.borrow(),
            [(index_url.into(), Some("\"i1\"".into()))]
        );
        assert_eq!(catalog.listings.len(), 1);
        assert!(catalog.update_for(hex, "0.2.0").is_some());

        // Asking now re-checks releases, also conditionally.
        fake.requests.borrow_mut().clear();
        refresh(Refresh::Now, &[], &installed, &path, &fake);
        assert_eq!(
            fake.requests.borrow()[1],
            (release_url.into(), Some("\"r1\"".into()))
        );

        // The cache alone still answers without any request.
        fake.requests.borrow_mut().clear();
        let cached = refresh(Refresh::Cached, &[], &installed, &path, &fake);
        assert!(fake.requests.borrow().is_empty());
        assert_eq!(cached.listings.len(), 1);
        assert!(cached.update_for(hex, "0.2.0").is_some());
    }

    #[test]
    #[ignore = "Authenticated GitHub access to the private official marketplace"]
    fn official_marketplace_lists_the_official_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let client = GitHubClient::new();
        let path = dir.path().join("catalog.json");
        let installed = ["https://github.com/im-henri/vtcode-ext-hex-editor".to_string()];
        let catalog = refresh(Refresh::Now, &[], &installed, &path, &client);
        assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
        let ids: Vec<_> = catalog.listings.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "vtcode.bitcalc",
                "vtcode.hex-editor",
                "vtcode.waveform-viewer"
            ]
        );
        assert!(catalog.update_for(installed[0].as_str(), "0.0.1").is_some());
        // A second refresh is answered by 304s and keeps the same result.
        let again = refresh(Refresh::Now, &[], &installed, &path, &client);
        assert_eq!(again.listings, catalog.listings);
        assert!(again.update_for(installed[0].as_str(), "0.0.1").is_some());
        for listing in &catalog.listings {
            let latest = latest_release(&source(&listing.repository)).unwrap();
            assert!(latest.is_stable(), "{}", listing.repository);
        }
    }
}
