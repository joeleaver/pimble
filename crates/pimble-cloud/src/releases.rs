//! `GET /releases`: the latest GitHub release, cached 10 minutes
//! (docs/CLOUD_CONTRACT.md).

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

#[derive(Deserialize)]
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
    cache: RwLock<Option<(Instant, ReleaseInfo)>>,
}

impl ReleasesCache {
    pub fn new(repo: String, base_url_override: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            repo,
            base_url: base_url_override.unwrap_or_else(|| GITHUB_API_BASE.to_string()),
            cache: RwLock::new(None),
        }
    }

    pub async fn get(&self) -> Result<ReleaseInfo, CloudError> {
        {
            let cache = self.cache.read().await;
            if let Some((fetched_at, info)) = cache.as_ref() {
                if fetched_at.elapsed() < CACHE_TTL {
                    return Ok(clone_info(info));
                }
            }
        }
        let info = self.fetch().await?;
        *self.cache.write().await = Some((Instant::now(), clone_info(&info)));
        Ok(info)
    }

    async fn fetch(&self) -> Result<ReleaseInfo, CloudError> {
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
            return Ok(ReleaseInfo::empty());
        }
        if !resp.status().is_success() {
            return Err(CloudError::Internal(format!("GitHub releases API returned HTTP {}", resp.status())));
        }
        let release: GitHubRelease = resp.json().await.map_err(|e| CloudError::Internal(format!("parsing GitHub release: {e}")))?;
        Ok(ReleaseInfo {
            version: release.tag_name,
            published_at: Some(release.published_at),
            assets: pair_signatures(release.assets),
        })
    }
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

fn clone_info(info: &ReleaseInfo) -> ReleaseInfo {
    ReleaseInfo { version: info.version.clone(), published_at: info.published_at.clone(), assets: info.assets.clone() }
}
