use thiserror::Error;

/// Why a share link could not be turned into a [`ServerConfig`].
///
/// Messages are English and developer-facing: they end up in the log pane and
/// in `bliss check`. The UI localises by matching on the variant, not by
/// translating these strings. None of them may carry a password or a key:
/// `got` holds only what is safe to show, or a description of it.
///
/// [`ServerConfig`]: crate::ServerConfig
#[derive(Debug, Error)]
pub enum LinkError {
    #[error("not a URL: {0}")]
    Url(#[from] url::ParseError),

    #[error(
        "{0}:// is not a server link; bliss reads vless, vmess, trojan, ss, socks, hysteria2 and wireguard"
    )]
    Scheme(String),

    #[error("{0}:// is a server link, but for a protocol xray-core does not speak")]
    NotInXray(String),

    #[error("link carries no user UUID before the '@'")]
    MissingUuid,

    #[error("bad UUID: {0}")]
    Uuid(#[from] uuid::Error),

    #[error("link carries no server address")]
    MissingHost,

    #[error("link carries no port")]
    MissingPort,

    #[error("link carries no {0}")]
    Missing(&'static str),

    #[error("security=reality requires the {0} parameter")]
    RealityMissing(&'static str),

    #[error("{param}: expected {expected}, got {got:?}")]
    BadValue {
        param: &'static str,
        expected: &'static str,
        got: String,
    },

    #[error("transport {0:?} is not one xray-core still has")]
    UnsupportedTransport(String),

    #[error("{0}, which xray-core cannot do")]
    Unsupported(Unsupported),

    #[error("encryption: {0}")]
    Encryption(String),
}

/// What a link asks for that xray-core has no way to do. Separate variants,
/// so the window can say it in its own words.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Unsupported {
    /// A Shadowsocks stream cipher: `aes-256-cfb`, `rc4-md5`, …
    #[error("the {0} cipher")]
    Cipher(String),
    /// A Shadowsocks plugin xray cannot stand in for, or a mode of one.
    #[error("the {0} plugin")]
    Plugin(String),
    /// A Hysteria 2 obfuscation other than Salamander.
    #[error("{0} obfuscation")]
    Obfs(String),
    /// `security=xtls`, removed from Xray years ago.
    #[error("legacy XTLS")]
    LegacyXtls,
}
