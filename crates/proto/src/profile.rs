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
    #[serde(default)]
    pub tun: Tun,
    #[serde(default)]
    pub routing: crate::Routing,
    /// Server lists a provider publishes at an address. Their servers are the
    /// `[[server]]` entries whose `group` is the subscription's `id`.
    #[serde(
        default,
        rename = "subscription",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub subscriptions: Vec<Subscription>,
    #[serde(default, rename = "server")]
    pub servers: Vec<ServerEntry>,
}

/// A provider's server list, fetched from an address and kept up to date.
///
/// The servers it brought are ordinary `[[server]]` entries tagged with this
/// subscription's `id` in their `group`, so everything that reads servers keeps
/// working unchanged, and an update replaces exactly the servers that came
/// from here and nothing a user added by hand.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    /// Ties its servers to it. Never shown, never changed.
    pub id: String,
    /// What the window calls it: the provider's title, or the address's host.
    pub name: String,
    /// Where the list is fetched from. It nearly always carries a personal
    /// token, so it is a credential like a link: `Debug` shows the host only.
    pub url: String,
    /// When the list last arrived, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<i64>,
    /// Traffic used and the expiry date, as the provider last reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl Subscription {
    /// The address with everything after the host left out, which is where
    /// the token lives: `https://sub.example/…`.
    pub fn shown_url(&self) -> String {
        match url::Url::parse(&self.url) {
            Ok(url) => match (url.host_str(), url.port()) {
                (Some(host), Some(port)) => format!("{}://{host}:{port}/…", url.scheme()),
                (Some(host), None) => format!("{}://{host}/…", url.scheme()),
                (None, _) => "…".to_owned(),
            },
            Err(_) => "…".to_owned(),
        }
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.shown_url())
            .field("updated", &self.updated)
            .field("usage", &self.usage)
            .finish()
    }
}

/// The `subscription-userinfo` a provider sends with the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Bytes.
    #[serde(default)]
    pub upload: u64,
    /// Bytes.
    #[serde(default)]
    pub download: u64,
    /// Bytes allowed in all; 0 when there is no limit.
    #[serde(default)]
    pub total: u64,
    /// When it runs out, in Unix seconds; absent when it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire: Option<i64>,
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

/// Whether bliss carries the machine's traffic through a TUN adapter.
///
/// Off by default, like every setting that reaches outside bliss. It is also
/// the one setting bliss cannot act on by itself: creating the adapter and
/// writing routes need administrator rights, and neither the window nor a
/// start from the Run key has them. Asked for here and honoured whenever the
/// copy that reads it does have them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tun {
    #[serde(default)]
    pub enabled: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A subscription address is a token with a host in front of it, and
    /// `Profile` is `Debug`.
    #[test]
    fn debug_does_not_show_a_subscription_token() {
        let subscription = Subscription {
            id: "a1".to_owned(),
            name: "provider".to_owned(),
            url: "https://sub.example:2096/api/sub/SECRETTOKEN?x=SECRETQUERY".to_owned(),
            updated: None,
            usage: None,
        };
        let shown = format!("{subscription:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(shown.contains("https://sub.example:2096/…"), "{shown}");

        let profile = Profile {
            subscriptions: vec![subscription],
            ..Profile::default()
        };
        assert!(!format!("{profile:?}").contains("SECRET"));
    }
}
