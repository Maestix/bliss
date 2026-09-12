//! Fetching an xray-core release from GitHub.
//!
//! Two things about Xray-core's releases shape this module, and both were found
//! by looking rather than assuming:
//!
//! 1. **Nearly every release is flagged `prerelease`.** At the time of writing,
//!    the last six in a row were, so `/releases/latest` answers with a build
//!    from six months earlier. The default is still [`Channel::Stable`] — for
//!    something a user depends on, proven beats recent — but the gap is large
//!    enough that the CLI says so out loud, and [`Channel::Newest`] is there
//!    for anyone who wants the edge.
//! 2. **The checksum file spells it `SHA2-256=`**, not `SHA256=`.
//!
//! There is also a bootstrapping problem worth stating plainly: the user who
//! most needs to download an engine is the one for whom GitHub is blocked, and
//! they have no working tunnel yet because they have no engine. So the download
//! can be routed through a proxy we already have running, and pointing
//! `engine.xray` at a file fetched by hand stays supported forever.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// GitHub wants a user agent and refuses the request without one.
const USER_AGENT: &str = concat!("bliss/", env!("CARGO_PKG_VERSION"));
const RELEASES: &str = "https://api.github.com/repos/XTLS/Xray-core/releases";

/// The release zip for the machine we are running on.
#[cfg(all(windows, target_arch = "x86_64"))]
const ASSET: &str = "Xray-windows-64.zip";
#[cfg(all(windows, target_arch = "aarch64"))]
const ASSET: &str = "Xray-windows-arm64-v8a.zip";
#[cfg(all(windows, target_arch = "x86"))]
const ASSET: &str = "Xray-windows-32.zip";
#[cfg(all(unix, target_arch = "x86_64"))]
const ASSET: &str = "Xray-linux-64.zip";
#[cfg(all(unix, target_arch = "aarch64"))]
const ASSET: &str = "Xray-linux-arm64-v8a.zip";

/// Files worth keeping out of the archive. The geo databases are the routing
/// rule sets for stage 1.5; taking them now costs nothing and saves a second
/// download later.
const WANTED: &[&str] = &["xray.exe", "xray", "geoip.dat", "geosite.dat"];

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("could not reach GitHub")]
    Http(#[from] reqwest::Error),

    #[error("no release found with an asset named {ASSET}")]
    NoRelease,

    #[error("release {tag} has no {ASSET}")]
    NoAsset { tag: String },

    #[error("no version {0} in the last {1} releases")]
    NoSuchVersion(String, usize),

    #[error("checksum file for {tag} has no SHA2-256 line")]
    NoChecksum { tag: String },

    /// Never continue past this. A mismatch means the bytes are not the bytes
    /// the publisher signed off on, whatever the reason.
    #[error("checksum mismatch for {tag}: expected {expected}, got {actual}")]
    Corrupt {
        tag: String,
        expected: String,
        actual: String,
    },

    #[error("{ASSET} does not contain an xray executable")]
    NoExecutable,

    #[error("could not unpack the archive")]
    Zip(#[from] zip::result::ZipError),

    #[error("could not write to {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Which releases to consider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    /// The newest release not flagged as a prerelease.
    ///
    /// The default, and the right one for something a user relies on: an engine
    /// that quietly stops working is worse than an engine that is a few months
    /// behind. Note what this means for xray-core specifically — almost every
    /// release there is flagged prerelease, so "stable" can be far back.
    #[default]
    Stable,
    /// The newest release of any kind, prereleases included. Opt-in.
    Newest,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub published: String,
    pub prerelease: bool,
    zip_url: String,
    dgst_url: String,
}

impl Release {
    /// The version as xray reports it: tags carry a leading `v`, the binary
    /// does not.
    pub fn version(&self) -> &str {
        self.tag.strip_prefix('v').unwrap_or(&self.tag)
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    published_at: String,
    prerelease: bool,
    draft: bool,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// Build the HTTP client, optionally sending everything through a proxy.
///
/// `via` is a SOCKS5 address — in practice the bridge of an engine that is
/// already running, which is how a user behind a block updates at all.
pub fn client(via: Option<std::net::SocketAddr>) -> Result<reqwest::Client, DownloadError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(300));
    if let Some(addr) = via {
        builder = builder.proxy(reqwest::Proxy::all(format!("socks5h://{addr}"))?);
    }
    Ok(builder.build()?)
}

/// The most recent releases that carry a build for this platform.
pub async fn list(client: &reqwest::Client, limit: usize) -> Result<Vec<Release>, DownloadError> {
    let raw: Vec<ApiRelease> = client
        .get(RELEASES)
        .query(&[("per_page", limit.to_string())])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(raw
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let zip = r.assets.iter().find(|a| a.name == ASSET)?;
            let dgst = r
                .assets
                .iter()
                .find(|a| a.name == format!("{ASSET}.dgst"))?;
            Some(Release {
                tag: r.tag_name,
                published: r.published_at,
                prerelease: r.prerelease,
                zip_url: zip.browser_download_url.clone(),
                dgst_url: dgst.browser_download_url.clone(),
            })
        })
        .collect())
}

/// Pick a release: the newest on the channel, or a named version.
pub async fn select(
    client: &reqwest::Client,
    channel: Channel,
    want: Option<&str>,
) -> Result<Release, DownloadError> {
    const LOOKBACK: usize = 30;
    let releases = list(client, LOOKBACK).await?;

    if let Some(want) = want {
        let want = want.strip_prefix('v').unwrap_or(want);
        return releases
            .into_iter()
            .find(|r| r.version() == want)
            .ok_or_else(|| DownloadError::NoSuchVersion(want.to_owned(), LOOKBACK));
    }

    // The API returns newest first, so the first match on the channel wins.
    releases
        .into_iter()
        .find(|r| match channel {
            Channel::Newest => true,
            Channel::Stable => !r.prerelease,
        })
        .ok_or(DownloadError::NoRelease)
}

/// Download, verify, unpack. Returns the path of the installed executable.
///
/// `progress` is called with (bytes so far, total) — total is 0 when the server
/// does not say.
pub async fn install<F>(
    client: &reqwest::Client,
    release: &Release,
    dest_dir: &Path,
    mut progress: F,
) -> Result<PathBuf, DownloadError>
where
    F: FnMut(u64, u64),
{
    let expected = fetch_checksum(client, release).await?;

    let response = client
        .get(&release.zip_url)
        .send()
        .await?
        .error_for_status()?;
    let total = response.content_length().unwrap_or(0);

    // Hash while downloading rather than re-reading afterwards: the archive is
    // ~20 MB and this keeps it to one pass.
    let mut hasher = Sha256::new();
    let mut bytes = Vec::with_capacity(total as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }

    let actual = hex(&hasher.finalize());
    if actual != expected {
        return Err(DownloadError::Corrupt {
            tag: release.tag.clone(),
            expected,
            actual,
        });
    }

    unpack(&bytes, dest_dir)
}

async fn fetch_checksum(
    client: &reqwest::Client,
    release: &Release,
) -> Result<String, DownloadError> {
    let text = client
        .get(&release.dgst_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    // "SHA2-256= d004c392…" — note the spelling; SHA256= finds nothing.
    text.lines()
        .find_map(|line| line.strip_prefix("SHA2-256="))
        .map(|v| v.trim().to_ascii_lowercase())
        .ok_or_else(|| DownloadError::NoChecksum {
            tag: release.tag.clone(),
        })
}

fn unpack(archive: &[u8], dest_dir: &Path) -> Result<PathBuf, DownloadError> {
    std::fs::create_dir_all(dest_dir).map_err(|source| DownloadError::Write {
        path: dest_dir.to_path_buf(),
        source,
    })?;

    let mut zip = zip::ZipArchive::new(Cursor::new(archive))?;
    let mut executable = None;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // enclosed_name refuses paths that escape the destination — an archive
        // is untrusted input even when it comes from a project we trust.
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let Some(file_name) = name.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !WANTED.contains(&file_name) {
            continue;
        }

        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buf)
            .map_err(|source| DownloadError::Write {
                path: dest_dir.join(file_name),
                source,
            })?;

        let path = dest_dir.join(file_name);
        write_replacing(&path, &buf)?;

        if file_name == "xray.exe" || file_name == "xray" {
            executable = Some(path);
        }
    }

    executable.ok_or(DownloadError::NoExecutable)
}

/// Write via a temporary file and rename over the target.
///
/// A half-written engine is worse than no engine: it passes the "is there a
/// file" check and then fails to start with something unhelpful. Rename is
/// atomic, so the file at the destination is always whole.
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<(), DownloadError> {
    let tmp = path.with_extension("part");
    let fail = |source| DownloadError::Write {
        path: path.to_path_buf(),
        source,
    };

    std::fs::write(&tmp, bytes).map_err(fail)?;
    // Windows will not rename onto an existing file; the old one may also be a
    // running engine, in which case removal fails and the update must wait.
    if path.exists() {
        std::fs::remove_file(path).map_err(fail)?;
    }
    std::fs::rename(&tmp, path).map_err(fail)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
