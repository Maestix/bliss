//! A server as described by a share link: what it speaks, how it hides, and
//! what carries it. The links themselves are read in `link/`.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
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
    pub protocol: Protocol,
    pub security: Security,
    pub transport: Transport,
    /// `fm`: xray's own `finalmask` object, carried as written. It replaces
    /// the one bliss would build, the way v2rayN treats it.
    pub finalmask: Option<Hidden>,
}

impl ServerConfig {
    /// Whether the server is reached over UDP, where a TCP handshake — the
    /// server list's ping — measures nothing.
    pub fn over_udp(&self) -> bool {
        matches!(
            self.protocol,
            Protocol::Hysteria2 { .. } | Protocol::WireGuard(_)
        ) || matches!(self.transport, Transport::Kcp { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Ip(IpAddr),
    Domain(String),
}

/// What the server speaks, and the credentials it takes.
#[derive(Debug, Clone)]
pub enum Protocol {
    Vless {
        user: UserId,
        encryption: Encryption,
        flow: Flow,
    },
    /// AEAD VMess only: xray dropped the legacy handshake, so a link's
    /// `alterId` is read and ignored.
    Vmess {
        user: UserId,
        cipher: VmessCipher,
    },
    Trojan {
        password: Hidden,
    },
    /// `method` is one xray knows, checked when the link is read.
    Shadowsocks {
        method: String,
        password: Hidden,
    },
    Socks {
        login: Option<Login>,
    },
    Hysteria2 {
        auth: Hidden,
    },
    WireGuard(WireGuard),
}

impl Protocol {
    /// The protocol's name as people write it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Vless { .. } => "VLESS",
            Self::Vmess { .. } => "VMess",
            Self::Trojan { .. } => "Trojan",
            Self::Shadowsocks { .. } => "Shadowsocks",
            Self::Socks { .. } => "SOCKS",
            Self::Hysteria2 { .. } => "Hysteria2",
            Self::WireGuard(_) => "WireGuard",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flow {
    #[default]
    None,
    XtlsRprxVision,
    /// Vision that lets UDP to port 443 through, where plain Vision blocks it
    /// to push browsers from QUIC back to TCP.
    XtlsRprxVisionUdp443,
}

impl Flow {
    pub fn as_param(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::XtlsRprxVision => Some("xtls-rprx-vision"),
            Self::XtlsRprxVisionUdp443 => Some("xtls-rprx-vision-udp443"),
        }
    }
}

/// VMess's own cipher, `scy` in a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VmessCipher {
    #[default]
    Auto,
    Aes128Gcm,
    Chacha20Poly1305,
    None,
    Zero,
}

impl VmessCipher {
    pub fn as_param(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Aes128Gcm => "aes-128-gcm",
            Self::Chacha20Poly1305 => "chacha20-poly1305",
            Self::None => "none",
            Self::Zero => "zero",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Login {
    pub user: Hidden,
    pub pass: Hidden,
}

#[derive(Debug, Clone)]
pub struct WireGuard {
    /// This side's key: the link's user part.
    pub secret_key: PrivateKey,
    /// The server's key.
    pub peer_key: PublicKey,
    pub pre_shared_key: Option<PrivateKey>,
    /// This side's addresses inside the tunnel, with their prefixes.
    pub addresses: Vec<String>,
    /// Three bytes some servers (Cloudflare's WARP) expect in every packet.
    pub reserved: Option<[u8; 3]>,
    pub mtu: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum Security {
    None,
    Tls(Tls),
    Reality(Reality),
}

impl Security {
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Tls(_) => "tls",
            Self::Reality(_) => "reality",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tls {
    pub sni: Option<String>,
    pub alpn: Vec<String>,
    pub fingerprint: Fingerprint,
    /// Asked for by the link and never passed on: xray refuses the option
    /// since June 2026 and wants the certificate pinned (`pcs`) instead.
    pub allow_insecure: bool,
    /// `pcs`: SHA-256 of the certificates to accept, in hex.
    pub pinned: Vec<String>,
    /// `vcn`: names to check the certificate against instead of the SNI.
    pub verify_names: Vec<String>,
    /// `ech`: an ECHConfigList, base64.
    pub ech: Option<String>,
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
    /// `pqv`: the server's ML-DSA-65 key, to verify its post-quantum signature.
    pub mldsa65_verify: Option<Long>,
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

/// What carries the protocol's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// Plain TCP, which xray now calls `raw`.
    Tcp {
        header: TcpHeader,
    },
    Ws {
        path: String,
        host: Option<String>,
    },
    HttpUpgrade {
        path: String,
        host: Option<String>,
    },
    Xhttp {
        path: String,
        host: Option<String>,
        /// `auto`, `packet-up`, `stream-up` or `stream-one`.
        mode: String,
        /// The link's `extra`: the rest of xray's XHTTP settings, as JSON.
        extra: Option<Hidden>,
    },
    Grpc {
        service_name: String,
        authority: Option<String>,
        multi: bool,
    },
    Kcp {
        header: KcpHeader,
        seed: Option<Hidden>,
        mtu: Option<u32>,
    },
    /// Hysteria 2's own QUIC; only a `hysteria2://` link has it.
    Hysteria {
        /// Salamander's password, when the packets are obfuscated.
        obfs: Option<Hidden>,
        /// The ports to hop between, as xray writes them: `20000-30000,443`.
        hop: Option<String>,
    },
    /// No stream at all: WireGuard carries itself.
    None,
}

impl Transport {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Tcp { .. } => "tcp",
            Self::Ws { .. } => "ws",
            Self::HttpUpgrade { .. } => "httpupgrade",
            Self::Xhttp { .. } => "xhttp",
            Self::Grpc { .. } => "grpc",
            Self::Kcp { .. } => "kcp",
            Self::Hysteria { .. } => "quic",
            Self::None => "udp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TcpHeader {
    #[default]
    None,
    /// The first bytes dressed as an HTTP request to one of these hosts.
    Http {
        host: Vec<String>,
        path: Vec<String>,
    },
}

/// What mKCP's packets are dressed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KcpHeader {
    #[default]
    None,
    Srtp,
    Utp,
    /// `wechat-video` in a link.
    Wechat,
    Dtls,
    WireGuard,
}

impl KcpHeader {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Srtp => "srtp",
            Self::Utp => "utp",
            Self::Wechat => "wechat",
            Self::Dtls => "dtls",
            Self::WireGuard => "wireguard",
        }
    }
}

// --- credential-bearing newtypes ------------------------------------------
//
// These exist for one reason: `Debug`. A ServerConfig is printed into the log
// pane, into error reports and into bug reports, and a plain derive would
// spill a live UUID, password or key every time. Redacting at the type means
// we cannot forget at a call site.

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UserId(pub Uuid);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(pub [u8; 32]);

/// A key that must never be shown, not even in part: a WireGuard private key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PrivateKey(pub [u8; 32]);

/// A value `Debug` never shows: passwords, and anything that may hold one.
///
/// Unlike a UUID, a password has no shape that makes part of it safe to
/// print — eight characters of a sixteen-character password are most of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Hidden(pub String);

/// A long public value, shown by its two ends: enough to tell two apart.
#[derive(Clone, PartialEq, Eq)]
pub struct Long(pub String);

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

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("‹key›")
    }
}

impl fmt::Debug for Hidden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("‹hidden›")
    }
}

impl fmt::Debug for Long {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::redact::head_tail(&self.0))
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
        crate::link::parse(link.trim())
    }
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

impl Address {
    /// The host part of a URL, with an IPv6 literal's brackets taken off.
    pub(crate) fn from_host(host: &str) -> Self {
        match host.trim_start_matches('[').trim_end_matches(']').parse() {
            Ok(ip) => Self::Ip(ip),
            Err(_) => Self::Domain(host.to_owned()),
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
