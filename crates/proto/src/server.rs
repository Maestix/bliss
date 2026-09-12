//! A server as described by a `vless://` share link.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::encryption::Encryption;
use crate::error::LinkError;

/// A server as stored in the profile.
///
/// The link is kept verbatim rather than exploded into fields: it round-trips
/// perfectly, survives parameters this build does not understand yet, and is
/// what the user pastes and copies. Everything else here is local metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub link: String,
    /// Overrides the link's `#fragment` when the user renames the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl ServerEntry {
    pub fn parse(&self) -> Result<ServerConfig, LinkError> {
        let mut cfg: ServerConfig = self.link.parse()?;
        if let Some(name) = &self.name {
            cfg.tag = name.clone();
        }
        Ok(cfg)
    }
}

/// A parsed, validated link.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub tag: String,
    pub address: Address,
    pub port: u16,
    pub user: UserId,
    pub encryption: Encryption,
    pub flow: Flow,
    pub security: Security,
    pub transport: Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Ip(IpAddr),
    Domain(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flow {
    #[default]
    None,
    XtlsRprxVision,
}

#[derive(Debug, Clone)]
pub enum Security {
    None,
    Tls(Tls),
    Reality(Reality),
}

#[derive(Debug, Clone)]
pub struct Tls {
    pub sni: Option<String>,
    pub alpn: Vec<String>,
    pub fingerprint: Fingerprint,
    pub allow_insecure: bool,
}

#[derive(Debug, Clone)]
pub struct Reality {
    /// The borrowed name we present to the network, and the site the server
    /// falls back to proxying when our auth does not check out.
    pub sni: String,
    pub fingerprint: Fingerprint,
    pub public_key: PublicKey,
    pub short_id: ShortId,
    /// `spx` — the path the client crawls on the real site when auth fails.
    /// Camouflage only; it never carries our traffic.
    pub spider_x: Option<String>,
}

/// Which browser's ClientHello to parrot.
///
/// Camouflage only — the REALITY server never checks it, so a mismatch with
/// the link costs nothing but a different shape on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Fingerprint {
    #[default]
    Chrome,
    Firefox,
    Safari,
    Ios,
    Android,
    Edge,
    Random,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Tcp { header: TcpHeader },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TcpHeader {
    #[default]
    None,
    Http,
}

// --- credential-bearing newtypes ------------------------------------------
//
// These exist for one reason: `Debug`. A ServerConfig is printed into the log
// pane, into error reports and into bug reports, and a plain derive would
// spill a live UUID and Reality key every time. Redacting at the type means we
// cannot forget at a call site.

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UserId(pub Uuid);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(pub [u8; 32]);

/// REALITY's `sid`: up to 8 bytes of hex, zero-padded on the right.
///
/// Deliberately not redacted. A rejected handshake is indistinguishable from a
/// good one on the client side, and the short id is half of what makes the
/// failure diagnosable — the server logs it too.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ShortId {
    bytes: [u8; 8],
    len: u8,
}

impl fmt::Debug for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::redact::head_tail(
            &self.0.as_hyphenated().to_string(),
        ))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::redact::b64(&self.0))
    }
}

impl ShortId {
    pub const EMPTY: Self = Self {
        bytes: [0; 8],
        len: 0,
    };

    pub fn parse_hex(hex: &str) -> Result<Self, LinkError> {
        let bad = || LinkError::BadValue {
            param: "sid",
            expected: "up to 16 hex digits, even length",
            got: hex.to_owned(),
        };
        if hex.len() > 16 || !hex.len().is_multiple_of(2) {
            return Err(bad());
        }
        let mut bytes = [0u8; 8];
        for (i, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            let s = std::str::from_utf8(pair).map_err(|_| bad())?;
            bytes[i] = u8::from_str_radix(s, 16).map_err(|_| bad())?;
        }
        Ok(Self {
            bytes,
            len: (hex.len() / 2) as u8,
        })
    }

    /// The 8 bytes that go into the ClientHello session id, zero-padded.
    pub fn padded(&self) -> [u8; 8] {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl fmt::Display for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.bytes[..self.len as usize] {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

// --- parsing --------------------------------------------------------------

impl FromStr for ServerConfig {
    type Err = LinkError;

    fn from_str(link: &str) -> Result<Self, Self::Err> {
        let url = Url::parse(link.trim())?;
        if url.scheme() != "vless" {
            return Err(LinkError::Scheme(url.scheme().to_owned()));
        }

        let user = match url.username() {
            "" => return Err(LinkError::MissingUuid),
            raw => UserId(Uuid::parse_str(
                &percent_decode_str(raw).decode_utf8_lossy(),
            )?),
        };

        let host = url.host_str().ok_or(LinkError::MissingHost)?;
        let address = match host.trim_start_matches('[').trim_end_matches(']').parse() {
            Ok(ip) => Address::Ip(ip),
            Err(_) => Address::Domain(host.to_owned()),
        };
        let port = url.port().ok_or(LinkError::MissingPort)?;

        let q: std::collections::HashMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let get = |k: &str| q.get(k).map(String::as_str).filter(|v| !v.is_empty());

        let encryption = Encryption::parse(get("encryption").unwrap_or("none"))?;

        let flow = match get("flow") {
            None => Flow::None,
            Some("xtls-rprx-vision") => Flow::XtlsRprxVision,
            Some(other) => {
                return Err(LinkError::BadValue {
                    param: "flow",
                    expected: "xtls-rprx-vision or empty",
                    got: other.to_owned(),
                });
            }
        };

        let fingerprint = get("fp").map(Fingerprint::from).unwrap_or_default();
        let security = match get("security") {
            None | Some("none") => Security::None,
            Some("tls") => Security::Tls(Tls {
                sni: get("sni").map(str::to_owned),
                alpn: get("alpn")
                    .map(|a| a.split(',').map(str::to_owned).collect())
                    .unwrap_or_default(),
                fingerprint,
                allow_insecure: get("allowInsecure") == Some("1"),
            }),
            Some("reality") => Security::Reality(Reality {
                sni: get("sni")
                    .ok_or(LinkError::RealityMissing("sni"))?
                    .to_owned(),
                fingerprint,
                public_key: parse_public_key(get("pbk").ok_or(LinkError::RealityMissing("pbk"))?)?,
                short_id: match get("sid") {
                    Some(hex) => ShortId::parse_hex(hex)?,
                    None => ShortId::EMPTY,
                },
                spider_x: get("spx").map(str::to_owned),
            }),
            Some(other) => {
                return Err(LinkError::BadValue {
                    param: "security",
                    expected: "none, tls or reality",
                    got: other.to_owned(),
                });
            }
        };

        let transport = match get("type").unwrap_or("tcp") {
            // Xray renamed "tcp" to "raw" in 25.x; links in the wild use both.
            "tcp" | "raw" => Transport::Tcp {
                header: match get("headerType") {
                    None | Some("none") => TcpHeader::None,
                    Some("http") => TcpHeader::Http,
                    Some(other) => {
                        return Err(LinkError::BadValue {
                            param: "headerType",
                            expected: "none or http",
                            got: other.to_owned(),
                        });
                    }
                },
            },
            other => return Err(LinkError::UnsupportedTransport(other.to_owned())),
        };

        let tag = url
            .fragment()
            .map(|f| percent_decode_str(f).decode_utf8_lossy().into_owned())
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| format!("{address}:{port}"));

        Ok(Self {
            tag,
            address,
            port,
            user,
            encryption,
            flow,
            security,
            transport,
        })
    }
}

fn parse_public_key(raw: &str) -> Result<PublicKey, LinkError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| LinkError::BadValue {
            param: "pbk",
            expected: "base64url without padding",
            got: raw.to_owned(),
        })?;
    let len = bytes.len();
    Ok(PublicKey(bytes.try_into().map_err(|_| {
        LinkError::BadValue {
            param: "pbk",
            expected: "32 bytes",
            got: format!("{len} bytes"),
        }
    })?))
}

impl From<&str> for Fingerprint {
    fn from(s: &str) -> Self {
        match s {
            "chrome" => Self::Chrome,
            "firefox" => Self::Firefox,
            "safari" => Self::Safari,
            "ios" => Self::Ios,
            "android" => Self::Android,
            "edge" => Self::Edge,
            "random" | "randomized" => Self::Random,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]"),
            Self::Ip(ip) => write!(f, "{ip}"),
            Self::Domain(d) => f.write_str(d),
        }
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Chrome => "chrome",
            Self::Firefox => "firefox",
            Self::Safari => "safari",
            Self::Ios => "ios",
            Self::Android => "android",
            Self::Edge => "edge",
            Self::Random => "random",
            Self::Other(s) => s,
        })
    }
}
