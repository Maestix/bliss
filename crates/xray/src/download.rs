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

/// The engine's file name, in the archive and in the work dir.
pub const ENGINE_FILE: &str = if cfg!(windows) { "xray.exe" } else { "xray" };

/// Files worth keeping out of the archive. The geo databases are the routing
/// rule sets for stage 1.5; taking them now costs nothing and saves a second
/// download later.
const WANTED: &[&str] = &[ENGINE_FILE, "geoip.dat", "geosite.dat"];

/// The most an archive may weigh. Xray-windows-64.zip is about 15 MB; the
/// length the server announces is checked before anything is read or
/// allocated for it, and the bytes as they arrive, for a server that
/// announces none.
const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;

/// The most one file inside it may unpack to: xray.exe is about 35 MB and
/// geoip.dat 20. A size in a zip header is the archive's word, not a fact,
/// so the read stops here whatever the header says.
const MAX_ENTRY: u64 = 256 * 1024 * 1024;

/// The most a checksum file may be: one line, really.
const MAX_CHECKSUM: u64 = 64 * 1024;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("could not reach GitHub")]
    Http(#[from] reqwest::Error),

    #[error("no release found with an asset named {ASSET}")]
    NoRelease,

    /// Every release looked at is a prerelease. Not the same as there being
    /// no build: xray-core flags nearly all of its releases so.
    #[error("none of the last {0} releases is stable; --newest takes a prerelease")]
    NoStable(usize),

    #[error("{what} is larger than {limit} bytes, which no xray-core release is")]
    TooLarge { what: String, limit: u64 },

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

impl DownloadError {
    /// The engine could not be replaced because it is running. Windows alone
    /// refuses that (a sharing violation); on Linux the rename goes through
    /// and the running engine keeps the file it had open.
    pub fn engine_in_use(&self) -> bool {
        // ERROR_SHARING_VIOLATION. 32 is EPIPE elsewhere, which means nothing
        // of the kind.
        cfg!(windows)
            && matches!(self, Self::Write { source, .. } if source.raw_os_error() == Some(32))
    }
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
///
/// Timed by silence, not by length: a request with nothing arriving for a
/// minute is given up, one that is slow but moving is not. A limit on the
/// whole request, 300 s, cut off a 15 MB archive on any link slower than
/// 50 KB/s — the very links that make people want a tunnel.
pub fn client(via: Option<std::net::SocketAddr>) -> Result<reqwest::Client, DownloadError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(20))
        .read_timeout(std::time::Duration::from_secs(60));
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

    if releases.is_empty() {
        return Err(DownloadError::NoRelease);
    }
    // The API returns newest first, so the first match on the channel wins.
    releases
        .into_iter()
        .find(|r| match channel {
            Channel::Newest => true,
            Channel::Stable => !r.prerelease,
        })
        .ok_or(DownloadError::NoStable(LOOKBACK))
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
    let (bytes, actual) =
        download(client, &release.zip_url, ASSET, MAX_ARCHIVE, &mut progress).await?;
    if actual != expected {
        return Err(DownloadError::Corrupt {
            tag: release.tag.clone(),
            expected,
            actual,
        });
    }
    unpack(&bytes, dest_dir, MAX_ENTRY)
}

/// The body at `url` and its SHA-256, reading no more than `limit` bytes:
/// not allocating more on the server's word, and not reading on past it.
///
/// Hashed while it arrives rather than re-read after: one pass.
async fn download<F>(
    client: &reqwest::Client,
    url: &str,
    what: &str,
    limit: u64,
    progress: &mut F,
) -> Result<(Vec<u8>, String), DownloadError>
where
    F: FnMut(u64, u64),
{
    let response = client.get(url).send().await?.error_for_status()?;
    let total = response.content_length().unwrap_or(0);
    let too_large = || DownloadError::TooLarge {
        what: what.to_owned(),
        limit,
    };
    if total > limit {
        return Err(too_large());
    }
    let mut hasher = Sha256::new();
    let mut bytes = Vec::with_capacity(total as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Err(too_large());
        }
        hasher.update(&chunk);
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }
    Ok((bytes, hex(&hasher.finalize())))
}

async fn fetch_checksum(
    client: &reqwest::Client,
    release: &Release,
) -> Result<String, DownloadError> {
    let (bytes, _) = download(
        client,
        &release.dgst_url,
        "the checksum file",
        MAX_CHECKSUM,
        &mut |_, _| {},
    )
    .await?;
    let text = String::from_utf8_lossy(&bytes);

    // "SHA2-256= d004c392…" — note the spelling; SHA256= finds nothing.
    text.lines()
        .find_map(|line| line.strip_prefix("SHA2-256="))
        .map(|v| v.trim().to_ascii_lowercase())
        .ok_or_else(|| DownloadError::NoChecksum {
            tag: release.tag.clone(),
        })
}

fn unpack(archive: &[u8], dest_dir: &Path, limit: u64) -> Result<PathBuf, DownloadError> {
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

        let too_large = || DownloadError::TooLarge {
            what: file_name.to_owned(),
            limit,
        };
        if entry.size() > limit {
            return Err(too_large());
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        (&mut entry)
            .take(limit + 1)
            .read_to_end(&mut buf)
            .map_err(|source| DownloadError::Write {
                path: dest_dir.join(file_name),
                source,
            })?;
        if buf.len() as u64 > limit {
            return Err(too_large());
        }

        let path = dest_dir.join(file_name);
        let is_executable = file_name == ENGINE_FILE;
        write_replacing(&path, &buf, is_executable)?;

        if is_executable {
            executable = Some(path);
        }
    }

    executable.ok_or(DownloadError::NoExecutable)
}

/// Write via a temporary file and rename over the target.
///
/// A half-written engine is worse than no engine: it passes the "is there a
/// file" check and then fails to start with something unhelpful. Rename is
/// atomic, so the file at the destination is always whole — and on Linux an
/// `executable` one is executable before it gets there, since a zip carries no
/// mode bits worth trusting and `xray` written as a plain file starts as
/// "permission denied".
fn write_replacing(path: &Path, bytes: &[u8], executable: bool) -> Result<(), DownloadError> {
    let tmp = path.with_extension("part");
    let fail = |source| DownloadError::Write {
        path: path.to_path_buf(),
        source,
    };

    std::fs::write(&tmp, bytes).map_err(fail)?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        if let Err(source) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(fail(source));
        }
    }
    #[cfg(not(unix))]
    let _ = executable;

    // Straight over the top. There used to be a `remove_file` here, on the
    // premise that Windows will not rename onto an existing file — it will:
    // `std::fs::rename` is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`. So
    // the removal bought nothing and cost the guarantee this function exists
    // for, by leaving a moment with no engine at all if the rename then failed.
    //
    // A running engine still cannot be replaced on Windows — the rename fails
    // with a sharing violation, which is the right answer and leaves the old
    // one working. Only the temporary needs clearing up in that case. On Linux
    // the rename succeeds: the running engine keeps the file it opened, and
    // the next start takes the new one.
    if let Err(source) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail(source));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::Ipv4Addr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// A server on loopback that answers one request with `head`, then `body`.
    async fn serve_once(head: String, body: Vec<u8>) -> String {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("a request");
            let mut request = vec![0u8; 4096];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        });
        format!("http://{at}/asset.zip")
    }

    fn loopback_client() -> reqwest::Client {
        // Past HTTP_PROXY, which on the machine this was written on points at
        // a proxy that is not this test's.
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a client")
    }

    /// An archive no bigger than the limit arrives whole; one the server says
    /// is bigger is refused before a byte is read, and one that only turns
    /// out bigger as it arrives is refused at the limit.
    #[tokio::test]
    async fn an_archive_larger_than_the_limit_is_refused() {
        let client = loopback_client();
        let body = vec![7u8; 1000];

        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".to_owned(),
            body.clone(),
        )
        .await;
        let (bytes, sha) = download(&client, &url, "test", 1000, &mut |_, _| {})
            .await
            .expect("within the limit");
        assert_eq!(bytes, body);
        assert_eq!(sha, hex(&Sha256::digest(&body)));

        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".to_owned(),
            body.clone(),
        )
        .await;
        let said = download(&client, &url, "test", 100, &mut |_, _| {}).await;
        assert!(
            matches!(said, Err(DownloadError::TooLarge { .. })),
            "{said:?}"
        );

        // No length announced: the bytes themselves are counted.
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_owned(),
            body,
        )
        .await;
        let mut seen = 0;
        let arrived = download(&client, &url, "test", 100, &mut |done, _| seen = done).await;
        assert!(
            matches!(arrived, Err(DownloadError::TooLarge { .. })),
            "{arrived:?}"
        );
        assert!(seen <= 100, "read past the limit: {seen}");
    }

    /// A file in the archive that unpacks to more than the limit is refused,
    /// whatever size the archive gives for it.
    #[test]
    fn a_file_in_the_archive_larger_than_the_limit_is_refused() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file(ENGINE_FILE, zip::write::SimpleFileOptions::default())
            .expect("an entry");
        archive.write_all(&[0u8; 1000]).expect("its bytes");
        let archive = archive.finish().expect("an archive").into_inner();

        let dir = std::env::temp_dir().join(format!("bliss-unpack-{}", std::process::id()));
        let refused = unpack(&archive, &dir, 100);
        assert!(
            matches!(refused, Err(DownloadError::TooLarge { .. })),
            "{refused:?}"
        );
        assert!(!dir.join(ENGINE_FILE).exists(), "nothing written for it");

        let unpacked = unpack(&archive, &dir, 1000).expect("within the limit");
        assert_eq!(std::fs::read(&unpacked).expect("written").len(), 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Linux engine comes out of the zip runnable, and the rule sets
    /// beside it do not. Written as a plain file, `xray` was refused with
    /// "permission denied" at its first start.
    #[cfg(unix)]
    #[test]
    fn the_engine_is_unpacked_executable() {
        use std::os::unix::fs::PermissionsExt;

        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["xray", "geoip.dat"] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .expect("an entry");
            archive.write_all(b"#!/bin/sh\n").expect("its bytes");
        }
        let archive = archive.finish().expect("an archive").into_inner();

        let dir = std::env::temp_dir().join(format!("bliss-unpack-mode-{}", std::process::id()));
        let engine = unpack(&archive, &dir, 1000).expect("unpacked");
        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("written")
                .permissions()
                .mode()
        };
        assert_eq!(mode(&engine) & 0o777, 0o755, "the engine runs");
        assert_eq!(
            mode(&dir.join("geoip.dat")) & 0o111,
            0,
            "a rule set does not"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
