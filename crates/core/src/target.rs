//! Where a connection is going.

use std::fmt;
use std::net::SocketAddr;

use thiserror::Error;

/// A destination as the client asked for it.
///
/// The domain is kept rather than resolved on our side, for three reasons that
/// all matter later: routing rules match on names, the remote end resolves
/// closer to the destination than we can, and resolving locally would leak the
/// name to whoever is watching our DNS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Domain { host: String, port: u16 },
    Socket(SocketAddr),
}

#[derive(Debug, Error)]
pub enum TargetError {
    #[error("empty host")]
    EmptyHost,
    #[error("missing port in {0:?}")]
    MissingPort(String),
    #[error("bad port in {0:?}")]
    BadPort(String),
}

impl Target {
    pub fn domain(host: impl Into<String>, port: u16) -> Self {
        Self::Domain {
            host: host.into(),
            port,
        }
    }

    /// Parse a `host:port` authority, the form both SOCKS and HTTP hand us.
    pub fn parse_authority(s: &str) -> Result<Self, TargetError> {
        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(Self::Socket(addr));
        }
        let (host, port) = s
            .rsplit_once(':')
            .ok_or_else(|| TargetError::MissingPort(s.to_owned()))?;
        // An IPv6 literal arrives bracketed; the brackets are syntax, not name.
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Err(TargetError::EmptyHost);
        }
        let port: u16 = port
            .parse()
            .map_err(|_| TargetError::BadPort(s.to_owned()))?;
        Ok(Self::domain(host, port))
    }

    pub fn port(&self) -> u16 {
        match self {
            Self::Domain { port, .. } => *port,
            Self::Socket(a) => a.port(),
        }
    }

    /// The name a routing rule or the UI should match on.
    pub fn host(&self) -> String {
        match self {
            Self::Domain { host, .. } => host.clone(),
            Self::Socket(a) => a.ip().to_string(),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain { host, port } => write!(f, "{host}:{port}"),
            Self::Socket(a) => write!(f, "{a}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_forms_that_turn_up() {
        assert_eq!(
            Target::parse_authority("example.com:443").unwrap(),
            Target::domain("example.com", 443)
        );
        assert!(matches!(
            Target::parse_authority("1.2.3.4:80").unwrap(),
            Target::Socket(_)
        ));
        // Bracketed IPv6 without a port is ambiguous with the address itself,
        // so only the socket form is accepted for it.
        assert!(matches!(
            Target::parse_authority("[::1]:443").unwrap(),
            Target::Socket(_)
        ));
        assert!(Target::parse_authority("example.com").is_err());
        assert!(Target::parse_authority("example.com:not-a-port").is_err());
    }
}
