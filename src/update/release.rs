//! GitHub release list and public-release selection.

use semver::Version;
use serde::Deserialize;

use crate::VivariumError;

/// Public release host for `ianzepp/vivi`.
pub(crate) const DOWNLOAD_BASE: &str = "https://github.com/ianzepp/vivi/releases/download";
const RELEASES_API: &str = "https://api.github.com/repos/ianzepp/vivi/releases";
const TAG_PREFIX: &str = "v";
const MAX_RELEASE_PAGES: usize = 5;

/// One release row from the GitHub release list.
#[derive(Debug, Deserialize)]
pub(crate) struct ReleaseJson {
    pub(crate) tag_name: String,
    #[serde(default)]
    pub(crate) draft: bool,
    #[serde(default)]
    pub(crate) prerelease: bool,
    #[serde(default)]
    pub(crate) html_url: String,
}

/// The newest public stable `v*` release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublicRelease {
    pub(crate) version: Version,
    pub(crate) tag: String,
    pub(crate) url: String,
}

/// Resolve the newest public stable `v*` release by semver, not list order.
pub(crate) fn latest_release() -> Result<PublicRelease, VivariumError> {
    let mut best = None;
    for page in 1..=MAX_RELEASE_PAGES {
        let url = format!("{RELEASES_API}?per_page=100&page={page}");
        let body = http_get(&url)?;
        let releases: Vec<ReleaseJson> = serde_json::from_slice(&body).map_err(|error| {
            VivariumError::Other(format!("cannot parse the release list from {url}: {error}"))
        })?;
        if releases.is_empty() {
            break;
        }
        for release in releases {
            consider_release(&mut best, &release);
        }
    }
    best.ok_or_else(|| VivariumError::Other("no public vivi release found".into()))
}

/// Fold one release row into the current best stable `v*` release.
pub(crate) fn consider_release(best: &mut Option<PublicRelease>, release: &ReleaseJson) {
    if release.draft || release.prerelease {
        return;
    }
    let Some(raw) = release.tag_name.strip_prefix(TAG_PREFIX) else {
        return;
    };
    let Ok(version) = Version::parse(raw) else {
        return;
    };
    if !version.pre.is_empty() {
        return;
    }
    if best
        .as_ref()
        .is_none_or(|current| version > current.version)
    {
        *best = Some(PublicRelease {
            version,
            tag: release.tag_name.clone(),
            url: release.html_url.clone(),
        });
    }
}

pub(crate) fn http_get(url: &str) -> Result<Vec<u8>, VivariumError> {
    let url = url.to_string();
    std::thread::spawn(move || http_get_blocking(&url))
        .join()
        .map_err(|_| VivariumError::Other("update HTTP thread panicked".into()))?
}

fn http_get_blocking(url: &str) -> Result<Vec<u8>, VivariumError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|error| VivariumError::Other(format!("http client: {error}")))?;
    let mut request = client
        .get(url)
        .header("User-Agent", concat!("vivi/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = github_token() {
        request = request.bearer_auth(token);
    }
    read_response(url, request.send())
}

fn read_response(
    url: &str,
    sent: Result<reqwest::blocking::Response, reqwest::Error>,
) -> Result<Vec<u8>, VivariumError> {
    let response =
        sent.map_err(|error| VivariumError::Other(format!("cannot reach {url}: {error}")))?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .map_err(|error| VivariumError::Other(format!("cannot read {url}: {error}")))?;
    if (200..300).contains(&status) {
        return Ok(bytes.to_vec());
    }
    Err(http_status_error(url, status, &bytes))
}

fn http_status_error(url: &str, status: u16, body: &[u8]) -> VivariumError {
    let detail = String::from_utf8_lossy(body);
    if status == 403 || status == 429 {
        VivariumError::Other(format!(
            "GitHub rate limit or authorization failure ({status}) for {url}; \
             set GITHUB_TOKEN or GH_TOKEN and retry. {detail}"
        ))
    } else {
        VivariumError::Other(format!("GET {url} failed with HTTP {status}: {detail}"))
    }
}

fn github_token() -> Option<String> {
    for name in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}
