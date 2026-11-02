//! The rule sets on the phone: `geoip.dat` and `geosite.dat`.
//!
//! The desktop takes them from xray's release archive, and the engine there
//! is installed with them. On Android xray ships inside the APK without them —
//! some 25 MB nobody without a `geosite:` rule needs — so they are fetched
//! when asked for, from one of three lists, and kept up to date by the week
//! once they are here. Each file is checked to read as a rule set before it
//! replaces the one in use.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// A file bigger than this is not a rule set: the largest of the three
/// sources' files, runetfreedom's geosite, was 86 MB on 1 November 2026.
/// Read up to it and no further.
pub const MAX_FILE: u64 = 160 * 1024 * 1024;
/// Category names sent to the editor at most; the largest geosite holds
/// some 1 500.
pub const MAX_NAMES: usize = 4_000;
/// Files older than this are fetched again when the tunnel comes up.
pub const STALE: i64 = 7 * 24 * 3600;

/// Where the files come from.
pub struct Source {
    pub key: &'static str,
    pub name: &'static str,
    pub about: &'static str,
    ip: &'static str,
    site: &'static str,
}

/// Sizes as of 1 November 2026, for the words on screen.
pub const SOURCES: [Source; 3] = [
    Source {
        key: "loyalsoldier",
        name: "Loyalsoldier",
        about: "категории v2fly с дополнениями: category-ru, category-ads-all, страны · ~28 МБ",
        ip: "https://github.com/Loyalsoldier/v2ray-rules-dat/releases/latest/download/geoip.dat",
        site: "https://github.com/Loyalsoldier/v2ray-rules-dat/releases/latest/download/geosite.dat",
    },
    Source {
        key: "runetfreedom",
        name: "Россия",
        about: "runetfreedom: списки заблокированного в России — ru-blocked, ru-available-only-inside · ~100 МБ",
        ip: "https://github.com/runetfreedom/russia-v2ray-rules-dat/releases/latest/download/geoip.dat",
        site: "https://github.com/runetfreedom/russia-v2ray-rules-dat/releases/latest/download/geosite.dat",
    },
    Source {
        key: "v2fly",
        name: "v2fly",
        about: "исходные списки v2fly, самые лёгкие · ~25 МБ",
        ip: "https://github.com/v2fly/geoip/releases/latest/download/geoip.dat",
        site: "https://github.com/v2fly/domain-list-community/releases/latest/download/dlc.dat",
    },
];

pub fn source(key: &str) -> &'static Source {
    SOURCES.iter().find(|s| s.key == key).unwrap_or(&SOURCES[0])
}

/// The phone's own settings, beside the profile: the profile is the
/// desktop's format, which refuses keys it does not know.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub geo_source: String,
    /// Fetch again by the week, once the files are here.
    pub geo_auto: bool,
    /// When the files were last fetched, in seconds since 1970.
    pub geo_updated: Option<i64>,
    /// The switch 0.2.5 had for refusing QUIC, read once to give the
    /// built-in rule that replaced it the same position (`quic_rule`).
    pub block_quic: bool,
    /// Whether the built-in «QUIC» rule has been offered to this profile:
    /// once, so a person who deletes it is not given it back.
    pub quic_rule: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            geo_source: SOURCES[0].key.to_owned(),
            geo_auto: true,
            geo_updated: None,
            block_quic: true,
            quic_rule: false,
        }
    }
}

impl Settings {
    pub fn load(files: &Path) -> Self {
        std::fs::read_to_string(files.join("android.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, files: &Path) {
        let path = files.join("android.json");
        let tmp = path.with_extension("json.new");
        let written = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|text| std::fs::write(&tmp, text))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            tracing::error!("geo: the settings were not saved: {e}");
        }
    }
}

pub fn ip_path(files: &Path) -> PathBuf {
    files.join("geoip.dat")
}

pub fn site_path(files: &Path) -> PathBuf {
    files.join("geosite.dat")
}

/// A file's size, if it is there.
pub fn size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// Every category in both files, `geosite:` and `geoip:` before each, sorted,
/// at most `MAX_NAMES`.
pub fn names(files: &Path) -> Vec<String> {
    let mut sites: Vec<String> = bliss_core::geo::site_names(&site_path(files))
        .unwrap_or_default()
        .into_iter()
        .map(|n| format!("geosite:{}", n.to_ascii_lowercase()))
        .collect();
    let mut ips: Vec<String> = bliss_core::geo::site_names(&ip_path(files))
        .unwrap_or_default()
        .into_iter()
        .map(|n| format!("geoip:{}", n.to_ascii_lowercase()))
        .collect();
    sites.sort();
    ips.sort();
    sites.extend(ips);
    sites.truncate(MAX_NAMES);
    sites
}

/// Fetch both files from `source` into `files`, through a SOCKS5 address when
/// `via` is given; `progress` hears the bytes so far. Nothing is replaced
/// until both have arrived and read as rule sets.
pub async fn update(
    source: &Source,
    files: &Path,
    via: Option<(SocketAddr, bliss_core::Login)>,
    progress: impl Fn(u64),
) -> Result<(), String> {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // Timed by silence, not length: a 20 MB file on a slow line is fine.
        .read_timeout(Duration::from_secs(60));
    let builder = match via {
        Some((addr, login)) => builder.proxy(
            reqwest::Proxy::all(format!("socks5h://{addr}"))
                .map_err(|_| "не удалось направить загрузку через туннель".to_owned())?
                .basic_auth(&login.user, &login.pass),
        ),
        // An app on Android has no proxy variables; a test on a machine with
        // them goes the way that machine goes out.
        None => builder,
    };
    let client = builder
        .build()
        .map_err(|_| "не удалось подготовить загрузку".to_owned())?;

    let ip_new = files.join("geoip.dat.new");
    let site_new = files.join("geosite.dat.new");
    let result = async {
        let ip_bytes = fetch(&client, source.ip, &ip_new, 0, &progress).await?;
        fetch(&client, source.site, &site_new, ip_bytes, &progress).await?;
        for (path, what) in [(&ip_new, "geoip.dat"), (&site_new, "geosite.dat")] {
            let names = bliss_core::geo::site_names(path)
                .map_err(|e| format!("{what} не прочитался: {e}"))?;
            if names.is_empty() {
                return Err(format!("{what} пришёл пустым — это не список правил"));
            }
        }
        std::fs::rename(&ip_new, ip_path(files)).map_err(|e| e.to_string())?;
        std::fs::rename(&site_new, site_path(files)).map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&ip_new);
    let _ = std::fs::remove_file(&site_new);
    result
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    to: &Path,
    before: u64,
    progress: &impl Fn(u64),
) -> Result<u64, String> {
    let response = client.get(url).send().await.map_err(describe)?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GitHub ответил кодом {}", status.as_u16()));
    }
    if response.content_length().is_some_and(|n| n > MAX_FILE) {
        return Err(too_large());
    }
    let mut file = tokio::fs::File::create(to)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();
    let mut got = 0u64;
    let mut told = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(describe)?;
        got += chunk.len() as u64;
        if got > MAX_FILE {
            return Err(too_large());
        }
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        if got - told > 512 * 1024 {
            told = got;
            progress(before + got);
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    progress(before + got);
    Ok(got)
}

fn too_large() -> String {
    format!(
        "файл больше {} МБ — это не список правил",
        MAX_FILE / 1024 / 1024
    )
}

/// reqwest's own words name the URL; these are by kind.
fn describe(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "загрузка замолчала больше чем на минуту".to_owned()
    } else if e.is_connect() {
        "не удалось соединиться с GitHub".to_owned()
    } else {
        "загрузка оборвалась".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip_and_an_unknown_source_is_the_first() {
        let dir = std::env::temp_dir().join(format!("bliss-geo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Settings::load(&dir).geo_source, "loyalsoldier");
        let settings = Settings {
            geo_source: "v2fly".to_owned(),
            geo_auto: false,
            geo_updated: Some(42),
            block_quic: false,
            quic_rule: true,
        };
        settings.save(&dir);
        let back = Settings::load(&dir);
        assert_eq!(back.geo_source, "v2fly");
        assert!(!back.block_quic);
        assert!(!back.geo_auto);
        assert_eq!(back.geo_updated, Some(42));
        assert_eq!(source("nonsense").key, "loyalsoldier");
        // A file from before the QUIC switch reads with it on.
        std::fs::write(dir.join("android.json"), r#"{"geo_source":"v2fly"}"#).unwrap();
        assert!(Settings::load(&dir).block_quic);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fetches the lightest source for real and reads its names
    /// (`cargo test -p bliss-android -- --ignored geo`): about 25 MB.
    #[tokio::test]
    #[ignore]
    async fn a_source_arrives_and_reads() {
        let dir = std::env::temp_dir().join(format!("bliss-geo-fetch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let seen = std::sync::atomic::AtomicU64::new(0);
        update(source("v2fly"), &dir, None, |n| {
            seen.store(n, std::sync::atomic::Ordering::Relaxed)
        })
        .await
        .unwrap();
        assert!(seen.load(std::sync::atomic::Ordering::Relaxed) > 1_000_000);
        let names = names(&dir);
        assert!(
            names.iter().any(|n| n == "geosite:google"),
            "{:?}",
            &names[..5]
        );
        assert!(names.iter().any(|n| n == "geoip:ru"));
        assert!(!dir.join("geoip.dat.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads rule sets fetched some other way (`BLISS_GEO_DIR`, a folder
    /// with both files), and decides a name by them.
    #[test]
    #[ignore]
    fn fetched_rule_sets_read_and_decide() {
        let dir = PathBuf::from(std::env::var("BLISS_GEO_DIR").expect("BLISS_GEO_DIR"));
        let names = names(&dir);
        eprintln!(
            "{} categories, {} of them geoip",
            names.len(),
            names.iter().filter(|n| n.starts_with("geoip:")).count()
        );
        assert!(names.iter().any(|n| n == "geosite:google"));
        assert!(names.iter().any(|n| n == "geoip:ru"));
        let mut routing = bliss_proto::Routing::default();
        routing.rules.push(bliss_proto::Rule {
            action: bliss_proto::Action::Direct,
            geosite: vec!["google".to_owned()],
            ..bliss_proto::Rule::default()
        });
        let router = bliss_core::Router::build(&routing, &dir);
        let target = bliss_core::Target::Domain {
            host: "mail.google.com".to_owned(),
            port: 443,
        };
        assert_eq!(
            router.decide(&target, None).route,
            bliss_core::Route::Direct
        );

        // A site by name meets a country rule by the address learnt for it.
        routing.rules.push(bliss_proto::Rule {
            action: bliss_proto::Action::Block,
            geoip: vec!["ru".to_owned()],
            ..bliss_proto::Rule::default()
        });
        let router = bliss_core::Router::build_resolving(&routing, &dir);
        let site = bliss_core::Target::Domain {
            host: "shop.example".to_owned(),
            port: 443,
        };
        assert!(router.wants_address(&site, None, false));
        // 77.88.8.8, Yandex's public DNS, in Russia; 203.0.113.5 nowhere.
        let ru: std::net::IpAddr = "77.88.8.8".parse().unwrap();
        let elsewhere: std::net::IpAddr = "203.0.113.5".parse().unwrap();
        assert_eq!(
            router.decide_with(&site, Some(ru), None).route,
            bliss_core::Route::Blocked
        );
        assert_eq!(
            router.decide_with(&site, Some(elsewhere), None).route,
            bliss_core::Route::Proxy
        );
    }

    #[test]
    fn no_files_no_names() {
        assert!(names(Path::new("/nonexistent")).is_empty());
    }
}
