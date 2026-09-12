//! Routing rules, as the user writes them.
//!
//! Three things can happen to a connection, and the whole feature is those
//! three words: send it through the proxy, let it go straight out, or refuse
//! it. Everything else here is about deciding which.
//!
//! Rules are read top to bottom and the first match wins. That is the one
//! ordering rule worth having, because it is the only one a person can hold in
//! their head — "put the exceptions above the general case" needs no further
//! explanation.

use serde::{Deserialize, Serialize};

/// What to do with a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Through the tunnel.
    #[default]
    Proxy,
    /// Straight out, not through the tunnel. Faster, and visible to anyone
    /// watching the local network — which is the point for a site that is
    /// local anyway.
    Direct,
    /// Refused. Nothing is sent anywhere; the program asking is told no.
    Block,
}

impl Action {
    /// The word the window uses.
    pub fn label(self) -> &'static str {
        match self {
            Self::Proxy => "через прокси",
            Self::Direct => "напрямую",
            Self::Block => "блокировать",
        }
    }
}

/// Everything routing needs from the profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Routing {
    /// What happens to a connection no rule matched.
    ///
    /// `proxy` by default, which is both the safe answer — nothing leaks by
    /// omission — and what a client with no rules at all does today.
    #[serde(default)]
    pub fallback: Action,

    /// Where the geo databases live. Unset means the copies that came with the
    /// engine, in bliss's own folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_dir: Option<std::path::PathBuf>,

    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            fallback: Action::Proxy,
            geo_dir: None,
            rules: Vec::new(),
        }
    }
}

/// One rule: what to match, and what to do about it.
///
/// Every list is optional and a rule matches if **any** entry in **any** of its
/// lists matches. Written the other way round — all lists having to agree — the
/// common case ("these twenty domains go direct") would need twenty rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// What to call this rule in the log and on screen.
    ///
    /// Worth filling in: every connection bliss handles records the rule that
    /// decided it, and "Реклама" tells the user something that
    /// `geosite:category-ads-all` does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    pub action: Action,

    /// Domain patterns. Bare text means "this name and everything under it";
    /// `full:` means exactly this name; `keyword:` means the text appears
    /// anywhere in the name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain: Vec<String>,

    /// Addresses and blocks, as `10.0.0.0/8` or a single `1.2.3.4`.
    ///
    /// Only ever matches a connection that named an address rather than a name
    /// — see the note on [`Rule::geoip`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ip: Vec<String>,

    /// Destination ports: `443`, or `1000-2000`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub port: Vec<String>,

    /// Programs, by executable name: `qbittorrent.exe`. Case-insensitive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process: Vec<String>,

    /// Categories from `geosite.dat`: `category-ads-all`, `ru`, `google`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geosite: Vec<String>,

    /// Countries from `geoip.dat`: `ru`, `private`.
    ///
    /// **These match addresses, not names.** A browser asks for
    /// `example.ru` and bliss deliberately never resolves it — that is what
    /// keeps the DNS lookup off the local network — so there is no address to
    /// compare. Use `geosite` to route sites by country; `geoip` catches the
    /// programs that connect to a bare address. Both become useful together in
    /// TUN mode, where bliss sees packets rather than requests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geoip: Vec<String>,
}

impl Rule {
    /// True if the rule would never match anything.
    ///
    /// A rule with an action and no conditions reads as "everything", which is
    /// almost certainly not what someone meant to write — so it is reported
    /// rather than applied.
    pub fn is_empty(&self) -> bool {
        self.domain.is_empty()
            && self.ip.is_empty()
            && self.port.is_empty()
            && self.process.is_empty()
            && self.geosite.is_empty()
            && self.geoip.is_empty()
    }

    /// The name to show, invented from the contents if none was given.
    pub fn display_name(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let mut parts = Vec::new();
        let mut describe = |label: &str, list: &[String]| {
            if let Some(first) = list.first() {
                parts.push(match list.len() {
                    1 => format!("{label} {first}"),
                    n => format!("{label} {first} +{}", n - 1),
                });
            }
        };
        describe("домен", &self.domain);
        describe("набор", &self.geosite);
        describe("адрес", &self.ip);
        describe("страна", &self.geoip);
        describe("порт", &self.port);
        describe("процесс", &self.process);
        if parts.is_empty() {
            return "пустое правило".to_owned();
        }
        parts.join(", ")
    }
}
