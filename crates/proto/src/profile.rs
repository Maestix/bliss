//! The TOML profile: what bliss keeps on disk between runs.
//!
//! Deliberately small. Servers are stored as their original links (see
//! [`ServerEntry`]), so a profile survives link parameters this build has never
//! heard of, and `deny_unknown_fields` means a typo in a hand-edited file is an
//! error rather than a setting that silently does nothing.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::LinkError;
use crate::server::{ServerConfig, ServerEntry};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub engine: Engine,
    #[serde(default)]
    pub inbound: Inbound,
    #[serde(default, rename = "server")]
    pub servers: Vec<ServerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inbound {
    /// Loopback by default. Anything else exposes an open proxy to the network,
    /// so it stays something the user has to type on purpose.
    #[serde(default = "Inbound::default_listen")]
    pub listen: IpAddr,
    #[serde(default = "Inbound::default_socks")]
    pub socks: u16,
    #[serde(default = "Inbound::default_http")]
    pub http: u16,
    /// What bliss does with the machine's own proxy settings.
    #[serde(default)]
    pub system_proxy: ProxyMode,
}

/// What to do with the machine-wide proxy settings, the three choices every
/// client in this ecosystem offers.
///
/// The default is [`ProxyMode::Unchanged`], and deliberately so: writing to
/// `HKCU\...\Internet Settings` changes how every program on the machine
/// reaches the network, and a proxy client has no business doing that until it
/// is asked. It is also the mode that cannot strand anyone — see
/// `bliss_core::sysproxy` for what the other two have to clean up after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyMode {
    /// Touch nothing.
    #[default]
    Unchanged,
    /// Point the machine at bliss for as long as bliss is serving.
    Set,
    /// Turn the machine's proxy off and leave it off.
    Clear,
}

impl ProxyMode {
    /// The label the window shows. Kept here rather than in the UI because the
    /// tray menu in 1.8 needs the same three words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "Не изменять",
            Self::Set => "Установить системный прокси",
            Self::Clear => "Очистить системный прокси",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Set => "set",
            Self::Clear => "clear",
        }
    }
}

impl Inbound {
    fn default_listen() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }
    // The ports every Windows client in this ecosystem uses; keeping them means
    // an existing browser or shell config keeps working after a switch.
    fn default_socks() -> u16 {
        10808
    }
    fn default_http() -> u16 {
        10809
    }
}

impl Default for Inbound {
    fn default() -> Self {
        Self {
            listen: Self::default_listen(),
            socks: Self::default_socks(),
            http: Self::default_http(),
            system_proxy: ProxyMode::default(),
        }
    }
}

/// Which engine carries the traffic, and how bliss reaches it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    /// Path to `xray.exe`. Unset means the copy bliss manages itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xray: Option<PathBuf>,
    /// The loopback port bliss dials the engine through.
    ///
    /// An internal detail between the two processes, not the port a browser is
    /// pointed at — that one is served by our own inbound. It is configurable
    /// only so a clash with something else on the machine can be worked around.
    #[serde(default = "Engine::default_bridge_port")]
    pub bridge_port: u16,
}

impl Engine {
    fn default_bridge_port() -> u16 {
        11080
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            xray: None,
            bridge_port: Self::default_bridge_port(),
        }
    }
}

impl Profile {
    /// Parse every server, keeping going past failures.
    ///
    /// `bliss check` exists to tell the user everything that is wrong with a
    /// profile in one run, so a bad link must not stop the ones after it.
    /// Errors carry the index they came from.
    pub fn parse_servers(&self) -> (Vec<ServerConfig>, Vec<(usize, LinkError)>) {
        let mut parsed = Vec::with_capacity(self.servers.len());
        let mut failed = Vec::new();
        for (i, entry) in self.servers.iter().enumerate() {
            match entry.parse() {
                Ok(cfg) => parsed.push(cfg),
                Err(e) => failed.push((i, e)),
            }
        }
        (parsed, failed)
    }
}
