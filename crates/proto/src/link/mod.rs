//! Share links, one parser per scheme, and the parameters they share.
//!
//! The formats are the ones v2rayN reads and writes, checked against its
//! parsers (`ServiceLib/Handler/Fmt`) and against what Xray-core 26.3.27
//! accepts, not written from memory. Most schemes put the transport and the
//! TLS or REALITY settings in the same query parameters, which `stream` reads
//! for all of them.

mod hysteria2;
mod shadowsocks;
mod socks;
mod stream;
mod trojan;
mod vless;
mod vmess;
mod wireguard;

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use percent_encoding::percent_decode_str;
use url::Url;

use crate::error::LinkError;
use crate::server::{Address, ServerConfig};

/// Schemes of server links for protocols xray-core does not speak. They are
/// recognised so the refusal can say that, rather than "not a link".
const ELSEWHERE: &[&str] = &[
    "ssr",
    "tuic",
    "anytls",
    "hysteria",
    "hy",
    "naive+https",
    "naive+quic",
    "juicity",
    "shadowtls",
    "snell",
    "mieru",
    "brook",
];

/// Whether bliss reads links of this scheme.
pub fn is_server_scheme(scheme: &str) -> bool {
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "vless"
            | "vmess"
            | "trojan"
            | "ss"
            | "socks"
            | "socks5"
            | "hysteria2"
            | "hy2"
            | "wireguard"
            | "wg"
    )
}

pub(crate) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let Some((scheme, _)) = link.split_once("://") else {
        return Err(match Url::parse(link) {
            Err(e) => LinkError::Url(e),
            Ok(url) => LinkError::Scheme(url.scheme().to_owned()),
        });
    };
    match scheme.to_ascii_lowercase().as_str() {
        "vless" => vless::parse(link),
        "vmess" => vmess::parse(link),
        "trojan" => trojan::parse(link),
        "ss" => shadowsocks::parse(link),
        "socks" | "socks5" => socks::parse(link),
        "hysteria2" | "hy2" => hysteria2::parse(link),
        "wireguard" | "wg" => wireguard::parse(link),
        other if ELSEWHERE.contains(&other) => Err(LinkError::NotInXray(other.to_owned())),
        other => Err(LinkError::Scheme(other.to_owned())),
    }
}

/// A link's query, decoded, with empty values treated as absent.
#[derive(Debug, Default)]
pub(crate) struct Query(HashMap<String, String>);

impl Query {
    fn of(url: &Url) -> Self {
        Self(
            url.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn set(&mut self, key: &str, value: impl Into<String>) {
        self.0.insert(key.to_owned(), value.into());
    }
}

fn decode(raw: &str) -> String {
    percent_decode_str(raw).decode_utf8_lossy().into_owned()
}

/// Everything before the `@`, percent-decoded: a password may hold a `:`,
/// which the URL parser splits off as a password of its own.
fn user_info(url: &Url) -> Option<String> {
    let user = url.username();
    let info = match url.password() {
        Some(pass) => format!("{user}:{pass}"),
        None if user.is_empty() => return None,
        None => user.to_owned(),
    };
    Some(decode(&info))
}

fn endpoint(url: &Url, default_port: Option<u16>) -> Result<(Address, u16), LinkError> {
    let host = url.host_str().ok_or(LinkError::MissingHost)?;
    let port = url.port().or(default_port).ok_or(LinkError::MissingPort)?;
    Ok((Address::from_host(host), port))
}

/// The server's name: the link's `#fragment`, or where it points.
fn tag(fragment: Option<&str>, address: &Address, port: u16) -> String {
    fragment
        .map(decode)
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| format!("{address}:{port}"))
}

/// Base64 in whichever of its four spellings a link uses.
fn base64(text: &str) -> Option<Vec<u8>> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    [STANDARD, URL_SAFE, STANDARD_NO_PAD, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(&compact).ok())
}

/// A 32-byte key in base64, as WireGuard writes them.
fn key(param: &'static str, raw: &str) -> Result<[u8; 32], LinkError> {
    // The value is a key, so the refusal describes it rather than repeating it.
    let bytes = base64(raw).ok_or_else(|| LinkError::BadValue {
        param,
        expected: "a base64 key",
        got: "text that is not base64".to_owned(),
    })?;
    let len = bytes.len();
    bytes.try_into().map_err(|_| LinkError::BadValue {
        param,
        expected: "32 bytes",
        got: format!("{len} bytes"),
    })
}

fn number<T: std::str::FromStr>(
    param: &'static str,
    expected: &'static str,
    raw: Option<&str>,
) -> Result<Option<T>, LinkError> {
    raw.map(|raw| {
        raw.trim().parse().map_err(|_| LinkError::BadValue {
            param,
            expected,
            got: raw.to_owned(),
        })
    })
    .transpose()
}

/// `a,b, c` as a list, leaving out the empty ones.
fn list(raw: Option<&str>) -> Vec<String> {
    raw.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}
