//! `GET /releases`: the latest GitHub release, cached 10 minutes
//! (docs/CLOUD_CONTRACT.md); and `GET /plugins`, the plugin library that
//! release carries (docs/SETTINGS_CONTRACT.md section 4).

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::error::CloudError;

const CACHE_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseAsset {
    pub name: String,
    /// Inferred from the asset's file name: `"linux"`, `"windows"`, or
    /// `"other"` when neither substring appears (contract: "`os` inferred
    /// from the asset name (`linux`, `windows`)" doesn't say what a build for
    /// neither names, e.g. a checksums file, gets — `"other"` rather than
    /// dropping it keeps every asset in the response).
    pub os: String,
    pub url: String,
    pub size: u64,
    /// The package's detached Ed25519 signature (`<name>.sig` beside it in the
    /// release), which the desktop app verifies before it installs an update
    /// (docs/UPDATE_CONTRACT.md). Signature files are not listed as assets of
    /// their own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseInfo {
    /// Empty string when the repository has no releases yet (contract:
    /// "tolerate no releases yet by returning an empty assets list").
    pub version: String,
    pub published_at: Option<String>,
    pub assets: Vec<ReleaseAsset>,
}

impl ReleaseInfo {
    fn empty() -> Self {
        ReleaseInfo { version: String::new(), published_at: None, assets: Vec::new() }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    published_at: String,
    assets: Vec<GitHubAsset>,
}

fn infer_os(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.contains("linux") {
        "linux".to_string()
    } else if lower.contains("windows") || lower.contains("win64") || lower.ends_with(".exe") {
        "windows".to_string()
    } else {
        "other".to_string()
    }
}

const GITHUB_API_BASE: &str = "https://api.github.com";

pub struct ReleasesCache {
    http: reqwest::Client,
    repo: String,
    /// `https://api.github.com` in production; overridable
    /// (`PIMBLE_CLOUD_RELEASES_BASE_URL` — see `Config`) so tests can point
    /// this at a local stub instead of the real network, per
    /// docs/CLOUD_CONTRACT.md's "handles an HTTP stub (or skip if it would
    /// need the network)".
    base_url: String,
    cache: RwLock<Option<(Instant, Latest)>>,
    /// The library's index, as the latest release had it.
    library: RwLock<Option<(Instant, Library)>>,
    /// Library files already fetched, by name. A name carries the plugin's
    /// version, so a file never changes under its name.
    files: RwLock<std::collections::HashMap<String, bytes::Bytes>>,
}

/// The latest release, as the download page reads it and as GitHub listed it.
#[derive(Clone)]
struct Latest {
    info: ReleaseInfo,
    assets: Vec<GitHubAsset>,
}

/// The library's index and where each file it names is.
#[derive(Clone)]
struct Library {
    index: bytes::Bytes,
    urls: std::collections::HashMap<String, String>,
}

/// The index a release without a library answers.
const EMPTY_LIBRARY: &[u8] = br#"{"v":1,"plugins":[]}"#;
/// The library's index in a release.
const LIBRARY_INDEX: &str = "plugins.json";

/// A file of the plugin library, which the download page does not list.
fn is_library_file(name: &str) -> bool {
    name == LIBRARY_INDEX || name.ends_with(".wasm") || name.ends_with(".wasm.sig") || name.ends_with(".png")
}

impl ReleasesCache {
    pub fn new(repo: String, base_url_override: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            repo,
            base_url: base_url_override.unwrap_or_else(|| GITHUB_API_BASE.to_string()),
            cache: RwLock::new(None),
            library: RwLock::new(None),
            files: RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub async fn get(&self) -> Result<ReleaseInfo, CloudError> {
        Ok(self.latest().await?.info)
    }

    async fn latest(&self) -> Result<Latest, CloudError> {
        {
            let cache = self.cache.read().await;
            if let Some((fetched_at, latest)) = cache.as_ref() {
                if fetched_at.elapsed() < CACHE_TTL {
                    return Ok(latest.clone());
                }
            }
        }
        let latest = self.fetch().await?;
        *self.cache.write().await = Some((Instant::now(), latest.clone()));
        Ok(latest)
    }

    /// `GET /plugins`: the latest release's `plugins.json`, as it is, or an
    /// empty library when the release has none.
    pub async fn plugin_index(&self) -> Result<bytes::Bytes, CloudError> {
        Ok(self.library().await?.index)
    }

    /// `GET /plugins/<file>`: a file the library's index names, fetched once.
    /// Nothing else is served, so this is never a way into the release's other
    /// files or anywhere else.
    pub async fn plugin_file(&self, name: &str) -> Result<bytes::Bytes, CloudError> {
        let library = self.library().await?;
        let Some(url) = library.urls.get(name) else {
            return Err(CloudError::NotFound(format!("the plugin library has no {name}")));
        };
        if let Some(bytes) = self.files.read().await.get(name) {
            return Ok(bytes.clone());
        }
        let bytes = self.download(url).await?;
        self.files.write().await.insert(name.to_string(), bytes.clone());
        Ok(bytes)
    }

    async fn library(&self) -> Result<Library, CloudError> {
        {
            let cache = self.library.read().await;
            if let Some((fetched_at, library)) = cache.as_ref() {
                if fetched_at.elapsed() < CACHE_TTL {
                    return Ok(library.clone());
                }
            }
        }
        let latest = self.latest().await?;
        let library = match latest.assets.iter().find(|a| a.name == LIBRARY_INDEX) {
            None => Library { index: bytes::Bytes::from_static(EMPTY_LIBRARY), urls: Default::default() },
            Some(asset) => {
                let index = self.download(&asset.browser_download_url).await?;
                let named = library_names(&index)?;
                let urls = latest
                    .assets
                    .iter()
                    .filter(|a| named.contains(&a.name))
                    .map(|a| (a.name.clone(), a.browser_download_url.clone()))
                    .collect();
                Library { index, urls }
            }
        };
        *self.library.write().await = Some((Instant::now(), library.clone()));
        Ok(library)
    }

    async fn download(&self, url: &str) -> Result<bytes::Bytes, CloudError> {
        let resp = self
            .http
            .get(url)
            .header("User-Agent", "pimble-cloud")
            .send()
            .await
            .map_err(|e| CloudError::Internal(format!("fetching {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(CloudError::Internal(format!("{url} answered HTTP {}", resp.status())));
        }
        resp.bytes().await.map_err(|e| CloudError::Internal(format!("reading {url}: {e}")))
    }

    async fn fetch(&self) -> Result<Latest, CloudError> {
        let url = format!("{}/repos/{}/releases/latest", self.base_url, self.repo);
        let resp = self
            .http
            .get(&url)
            .header("User-Agent", "pimble-cloud")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| CloudError::Internal(format!("fetching GitHub releases: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            // No releases published yet.
            return Ok(Latest { info: ReleaseInfo::empty(), assets: Vec::new() });
        }
        if !resp.status().is_success() {
            return Err(CloudError::Internal(format!("GitHub releases API returned HTTP {}", resp.status())));
        }
        let release: GitHubRelease = resp.json().await.map_err(|e| CloudError::Internal(format!("parsing GitHub release: {e}")))?;
        let packages = release.assets.iter().filter(|a| !is_library_file(&a.name)).cloned().collect();
        Ok(Latest {
            info: ReleaseInfo { version: release.tag_name, published_at: Some(release.published_at), assets: pair_signatures(packages) },
            assets: release.assets,
        })
    }
}

/// The files `plugins.json` names (each plugin's module, signature and
/// screenshot), which are all `GET /plugins/<file>` serves.
fn library_names(index: &[u8]) -> Result<std::collections::HashSet<String>, CloudError> {
    let index: serde_json::Value =
        serde_json::from_slice(index).map_err(|e| CloudError::Internal(format!("the release's plugins.json does not read: {e}")))?;
    let mut names = std::collections::HashSet::new();
    for plugin in index["plugins"].as_array().into_iter().flatten() {
        for key in ["wasm", "sig", "shot"] {
            if let Some(name) = plugin[key].as_str() {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names)
}

/// The release's packages, each with its `<name>.sig` when the release has one.
fn pair_signatures(assets: Vec<GitHubAsset>) -> Vec<ReleaseAsset> {
    let signatures: std::collections::HashMap<String, String> = assets
        .iter()
        .filter_map(|a| a.name.strip_suffix(".sig").map(|base| (base.to_string(), a.browser_download_url.clone())))
        .collect();
    assets
        .into_iter()
        .filter(|a| !a.name.ends_with(".sig"))
        .map(|a| ReleaseAsset {
            os: infer_os(&a.name),
            signature_url: signatures.get(&a.name).cloned(),
            name: a.name,
            url: a.browser_download_url,
            size: a.size,
        })
        .collect()
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    fn asset(name: &str) -> GitHubAsset {
        GitHubAsset { name: name.into(), browser_download_url: format!("https://dl/{name}"), size: 1 }
    }

    #[test]
    fn the_library_names_only_its_own_files() {
        let names = library_names(br#"{"v":1,"plugins":[{"wasm":"kanban-viewer-0.1.0.wasm","sig":"kanban-viewer-0.1.0.wasm.sig"}]}"#).unwrap();
        assert!(names.contains("kanban-viewer-0.1.0.wasm") && names.contains("kanban-viewer-0.1.0.wasm.sig"));
        assert_eq!(names.len(), 2);
        assert!(is_library_file("plugins.json") && is_library_file("kanban-viewer-0.1.0.wasm.sig"));
        assert!(!is_library_file("pimble-1.0.0-linux-x86_64.tar.gz") && !is_library_file("pimble-1.0.0-linux-x86_64.tar.gz.sig"));
    }

    #[test]
    fn a_signature_rides_on_its_package_and_is_not_listed_itself() {
        let paired = pair_signatures(vec![
            asset("pimble-1.0.0-linux-x86_64.tar.gz"),
            asset("pimble-1.0.0-linux-x86_64.tar.gz.sig"),
            asset("pimble-1.0.0-windows-x86_64.zip"),
        ]);
        assert_eq!(paired.len(), 2);
        assert_eq!(paired[0].signature_url.as_deref(), Some("https://dl/pimble-1.0.0-linux-x86_64.tar.gz.sig"));
        assert_eq!(paired[1].signature_url, None);
    }
}

