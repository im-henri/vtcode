//! Discover packaged, versioned GitHub releases, without fetching a repository.
use crate::extension_install::{ExtensionSource, InstalledExtension};
use serde::Deserialize;
use std::{io::Read, path::Path};

pub(crate) const PACKAGE_ASSET: &str = "vtcode-extension.zip";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseVersion {
    pub name: String,
    pub release_id: u64,
    pub asset_id: u64,
    pub size: u64,
    pub sha256: String,
    pub prerelease: bool,
}

impl ReleaseVersion {
    pub(crate) fn semantic_version(&self) -> Option<semver::Version> {
        crate::extension_release_source::semantic_version(&self.name)
    }
    pub(crate) fn is_stable(&self) -> bool {
        !self.prerelease && self.semantic_version().is_some_and(|v| v.pre.is_empty())
    }
    pub(crate) fn category(&self) -> &'static str {
        if self.is_stable() {
            "release"
        } else {
            "prerelease"
        }
    }
    pub(crate) fn is_installed(&self, installed: &[InstalledExtension]) -> bool {
        installed
            .iter()
            .any(|info| info.tag == self.name && info.sha256 == self.sha256)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ExtensionVersions {
    pub input: String,
    pub source: ExtensionSource,
    pub versions: Vec<ReleaseVersion>,
    pub installed: Vec<InstalledExtension>,
}

impl ExtensionVersions {
    pub(crate) fn preferred_index(&self) -> usize {
        self.source
            .reference
            .as_ref()
            .and_then(|tag| self.versions.iter().position(|v| &v.name == tag))
            .unwrap_or(0)
    }
    pub(crate) fn latest_stable(&self) -> Option<&ReleaseVersion> {
        self.versions.iter().find(|v| v.is_stable())
    }
}

#[derive(Deserialize)]
struct Release {
    id: u64,
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    state: String,
    digest: Option<String>,
}

fn json<T: serde::de::DeserializeOwned>(
    agent: &crate::extension_release_source::GitHubClient,
    url: &str,
) -> Result<T, String> {
    let mut response = agent.get(url, "application/vnd.github+json")?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(crate::extension_release_source::request_error)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("GitHub release metadata exceeded 4 MiB. Use a specific release link.".into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid GitHub release metadata: {error}"))
}

pub(crate) fn discover(input: &str, installed_root: &Path) -> Result<ExtensionVersions, String> {
    let source = ExtensionSource::parse(input)?;
    let agent = crate::extension_release_source::GitHubClient::new();
    let releases = if let Some(tag) = &source.reference {
        let mut url = url::Url::parse(&source.api("releases/tags/")).unwrap();
        url.path_segments_mut().unwrap().pop_if_empty().push(tag);
        vec![json::<Release>(&agent, url.as_str())?]
    } else {
        let mut releases = Vec::new();
        for page in 1.. {
            let batch: Vec<Release> = json(
                &agent,
                &source.api(&format!("releases?per_page=100&page={page}")),
            )?;
            let last = batch.len() < 100;
            releases.extend(batch);
            if last {
                break;
            }
        }
        releases
    };
    let mut catalog = catalog(input, source, releases)?;
    catalog.installed = installed_for_source(installed_root, &catalog.source);
    Ok(catalog)
}

fn catalog(
    input: &str,
    source: ExtensionSource,
    releases: Vec<Release>,
) -> Result<ExtensionVersions, String> {
    let mut versions = Vec::new();
    let mut names = rustc_hash::FxHashSet::default();
    for release in releases {
        if release.draft
            || crate::extension_release_source::semantic_version(&release.tag_name).is_none()
        {
            continue;
        }
        let mut packages = release
            .assets
            .into_iter()
            .filter(|asset| asset.name == PACKAGE_ASSET && asset.state == "uploaded");
        let Some(asset) = packages.next() else {
            continue;
        };
        if packages.next().is_some() {
            return Err("A release contains duplicate VTCode packages.".into());
        }
        let Some(hash) = asset
            .digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
        else {
            continue;
        };
        if hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            || asset.id == 0
            || release.id == 0
            || asset.size == 0
            || asset.size > crate::extension_install::MAX_PACKAGE_BYTES
        {
            continue;
        }
        if !names.insert(release.tag_name.clone()) {
            return Err("GitHub returned duplicate release tags. Refresh the release list.".into());
        }
        versions.push(ReleaseVersion {
            name: release.tag_name,
            release_id: release.id,
            asset_id: asset.id,
            size: asset.size,
            sha256: hash.to_ascii_lowercase(),
            prerelease: release.prerelease,
        });
    }
    versions.sort_by(|a, b| {
        (!a.is_stable())
            .cmp(&(!b.is_stable()))
            .then_with(|| {
                b.semantic_version()
                    .unwrap()
                    .cmp_precedence(&a.semantic_version().unwrap())
            })
            .then_with(|| a.name.cmp(&b.name))
    });
    if versions.is_empty() {
        return Err(format!(
            "This repository has no installable VTCode releases. The author must publish a semantic version tag (for example v1.0.0) with a GitHub release containing {PACKAGE_ASSET} and its SHA-256 digest. Branches and source archives are not installable."
        ));
    }
    if source
        .reference
        .as_ref()
        .is_some_and(|tag| !versions.iter().any(|v| &v.name == tag))
    {
        return Err("The selected tag has no valid VTCode release package. Choose a published release containing vtcode-extension.zip.".into());
    }
    Ok(ExtensionVersions {
        input: input.into(),
        source,
        versions,
        installed: Vec::new(),
    })
}

#[cfg(test)]
pub(crate) fn parse_releases(input: &str, bytes: &[u8]) -> Result<ExtensionVersions, String> {
    let releases = serde_json::from_slice(bytes)
        .map_err(|error| format!("Invalid release metadata: {error}"))?;
    catalog(input, ExtensionSource::parse(input)?, releases)
}

fn installed_for_source(root: &Path, source: &ExtensionSource) -> Vec<InstalledExtension> {
    let mut installed = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return installed;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path().join(".vtcode-source.json");
        if !std::fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.is_file() && meta.len() <= 1024 * 1024)
        {
            continue;
        }
        if let Some(info) = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<InstalledExtension>(&bytes).ok())
            && ExtensionSource::parse(&info.repository)
                .is_ok_and(|previous| previous.repository_key() == source.repository_key())
        {
            installed.push(info);
        }
    }
    installed.sort_by(|a, b| a.id.cmp(&b.id));
    installed
}
