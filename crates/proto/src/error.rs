use thiserror::Error;

/// Why a `vless://` link could not be turned into a [`ServerConfig`].
///
/// Messages are English and developer-facing: they end up in the log pane and
/// in `bliss check`. The UI localises by matching on the variant, not by
/// translating these strings.
///
/// [`ServerConfig`]: crate::ServerConfig
#[derive(Debug, Error)]
pub enum LinkError {
    #[error("not a URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("expected a vless:// link, got {0}://")]
    Scheme(String),

    #[error("link carries no user UUID before the '@'")]
    MissingUuid,

    #[error("bad UUID: {0}")]
    Uuid(#[from] uuid::Error),

    #[error("link carries no server address")]
    MissingHost,

    #[error("link carries no port")]
    MissingPort,

    #[error("security=reality requires the {0} parameter")]
    RealityMissing(&'static str),

    #[error("{param}: expected {expected}, got {got:?}")]
    BadValue {
        param: &'static str,
        expected: &'static str,
        got: String,
    },

    #[error("transport {0:?} is not supported yet — bliss speaks raw TCP only")]
    UnsupportedTransport(String),

    #[error("encryption: {0}")]
    Encryption(String),
}
