//! What crosses the socket between the tunnel's process and the screens.
//!
//! One JSON object per line each way. The screens never see a link or a
//! subscription's address — those are credentials (AGENTS.md §5) — only
//! names, kinds, hosts and numbers; deleting and restoring is done by index
//! on this side, so an undo needs no link either.

use bliss_proto::{Flow, Protocol, Security, ServerConfig, TcpHeader, Transport};
use serde::{Deserialize, Serialize};

/// Everything the screens draw, as of one moment.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Frame {
    /// "off", "busy", "on" or "failed".
    pub state: &'static str,
    /// While busy: 0 the engine, 1 the stack, 2 the check through the server.
    pub step: u8,
    /// Why the last start failed, in words.
    pub error: Option<String>,
    /// After a failure: the server that answered fastest, to offer instead.
    pub suggest: Option<Suggest>,
    /// When the tunnel came up, in milliseconds since 1970; the screens tick
    /// the clock themselves, so nothing here has to wake every second.
    pub since: Option<i64>,
    /// The server a connection uses: the chosen one.
    pub current: Option<usize>,
    /// «Автовыбор» is on: `current` is the server it chose.
    pub auto: bool,
    pub servers: Vec<ServerRow>,
    pub groups: Vec<GroupRow>,
    pub rules: Vec<RuleRow>,
    pub fallback: &'static str,
    pub presets: Vec<String>,
    pub preset: Option<String>,
    pub drifted: bool,
    pub warnings: Vec<String>,
    pub numbers: Numbers,
    /// Download rates, oldest first, one a second; empty when nobody looks.
    pub history: Vec<u64>,
    /// The connections, only while their screen is open.
    pub table: Option<Vec<ConnRow>>,
    /// The journal, only while its tab is open.
    pub log: Option<Vec<String>>,
    pub toast: Option<Toast>,
    /// "xray 26.3.27", once it has been asked.
    pub engine: String,
    /// Whether a deleted server or subscription can still be given back.
    pub can_restore: bool,
    /// The newest TUN descriptor the engine has taken, by the ticket `connect`
    /// answered the service with. A frame with this ticket or a later one and
    /// `holding` false means the engine has let that TUN go — failed, or
    /// switched off — and the service steps out of the foreground. A frame
    /// still in the socket from before the hand-over carries an older ticket,
    /// so it cannot be read as the answer.
    pub ticket: u64,
    /// Whether the engine holds a TUN: starting, switching or up.
    pub holding: bool,
    pub geo: GeoView,
    /// Every category in the rule sets, `geosite:…` then `geoip:…`, only
    /// while the rule editor asks (`UiCommand::GeoNames`).
    pub categories: Option<Vec<String>>,
    /// Counts the journals written for sharing: a new number means
    /// `journal.txt` is ready to be sent.
    pub journal: u64,
    /// The network the phone is on, and what bliss does there.
    pub network: Option<NetView>,
    /// Every network with a rule.
    pub networks: Vec<NetView>,
    /// What the network asks of the tunnel's service — "on", "off", or
    /// "back" from a network that asks nothing — with
    /// a number that moves at each network change that asks something, so
    /// the service acts once per change and a heart pressed after is the
    /// person's.
    pub wish: Option<&'static str>,
    pub wish_id: u64,
}

/// A network as the screens show it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NetView {
    /// "mobile", "wifi", "wifi:<name>".
    pub id: String,
    pub name: String,
    /// "on", "off" or "": this network's own rule, or the one it falls under.
    pub connect: String,
    pub preset: Option<String>,
    /// The server taken here: a key, "auto", or none.
    pub server: Option<String>,
    /// That server's name, as the list shows it; none when it is gone.
    pub server_name: Option<String>,
    /// Whether the rule shown is this network's own (else «любой Wi-Fi»'s, or none).
    pub own: bool,
}

/// The rule sets: where from, what is here, and a fetch under way.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GeoView {
    pub source: String,
    pub sources: Vec<GeoSource>,
    pub auto: bool,
    /// Sizes in bytes of the files on the phone.
    pub ip: Option<u64>,
    pub site: Option<u64>,
    pub updated: Option<i64>,
    /// Bytes so far, while fetching.
    pub fetching: Option<u64>,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeoSource {
    pub key: &'static str,
    pub name: &'static str,
    pub about: &'static str,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Numbers {
    pub down_rate: u64,
    pub up_rate: u64,
    pub down_total: u64,
    pub up_total: u64,
    pub active: usize,
    pub opened: u64,
    pub latency: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggest {
    pub index: usize,
    pub name: String,
    pub ping: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerRow {
    /// Which server this is, as commands name it back: a hash of its link,
    /// which a refresh does not move as it moves the server's place.
    pub key: String,
    pub name: String,
    /// "VLESS · REALITY": the fewest words.
    pub kind: String,
    /// "VLESS · xhttp · reality · vision": how it hides, in full.
    pub detail: String,
    /// host:port — not a secret, and what a person recognises a server by.
    pub address: String,
    /// The subscription it came from, by id.
    pub group: Option<String>,
    /// "unknown", "probing", "ok", "down" or "udp".
    pub ping: &'static str,
    pub ms: Option<u32>,
    /// The real check: a page loaded through the server ("ok", with the
    /// time of the second request), nothing did ("down"), "probing" or
    /// "unknown".
    pub check: &'static str,
    pub check_ms: Option<u32>,
    /// Marked with a star: the list puts it above the rest.
    pub favorite: bool,
}

/// A server's key in the frame (`ServerRow::key`): FNV-1a over its link, so
/// the same in every run and nothing of the link read back from it.
pub fn server_key(link: &str) -> String {
    bliss_proto::link_key(link)
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupRow {
    pub id: String,
    pub name: String,
    /// scheme://host — never the path, where the token is.
    pub host: String,
    pub updated: Option<i64>,
    pub used: Option<u64>,
    pub total: Option<u64>,
    pub expire: Option<i64>,
    pub fetching: bool,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuleRow {
    pub name: String,
    /// Whether `name` was given, rather than made up from the conditions.
    pub named: bool,
    pub conditions: String,
    /// The conditions one to a line, as the editor shows them (rule_text.rs).
    pub text: String,
    /// The package, for a rule whose one condition is an app: what the apps
    /// view shows as that app's choice.
    pub app: Option<String>,
    /// "proxy", "direct", "block" or "vpn".
    pub action: &'static str,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnRow {
    pub target: String,
    /// The package that opened it, when Android said.
    pub app: Option<String>,
    pub route: &'static str,
    pub rule: String,
    pub up: u64,
    pub down: u64,
    pub closed: bool,
}

/// A line about what just happened. `id` changes with every new one, so a
/// screen shows each once.
#[derive(Debug, Clone, Serialize)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub bad: bool,
    /// Offer «Вернуть»: a deletion that can be undone.
    pub undo: bool,
}

/// What the screens ask for.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum UiCommand {
    /// Sent first: whether this client draws numbers (the screens) or only
    /// needs the state (the service, which closes the TUN when the tunnel is
    /// off). Nothing ticks while no client draws.
    Hello {
        full: bool,
    },
    /// Links, a list of them, or a subscription's address.
    Import {
        text: String,
    },
    /// By place, and by `key` when the screen sends one: a list refreshed
    /// in the background moves servers, and the place alone named another.
    Select {
        index: usize,
        #[serde(default)]
        key: Option<String>,
    },
    Delete {
        index: usize,
        #[serde(default)]
        key: Option<String>,
    },
    DeleteGroup {
        id: String,
    },
    Restore,
    Ping,
    /// Real checks through the servers: one by index, or all.
    Check {
        index: Option<usize>,
        #[serde(default)]
        key: Option<String>,
    },
    /// `by_hand` false when the tunnel's service takes it down for a
    /// network's rule, a pause or another VPN; the heart's is by hand.
    Disconnect {
        #[serde(default = "yes")]
        by_hand: bool,
    },
    Rule {
        index: usize,
        on: bool,
    },
    Fallback {
        action: String,
    },
    Preset {
        name: String,
    },
    SavePreset,
    Refresh {
        id: Option<String>,
    },
    Table {
        open: bool,
    },
    Log {
        open: bool,
    },
    /// A rule from the editor: a new one (`index` none) or one replaced.
    /// The conditions are the editor's text, one to a line.
    SaveRule {
        index: Option<usize>,
        name: String,
        action: String,
        text: String,
    },
    DeleteRule {
        index: usize,
    },
    MoveRule {
        from: usize,
        to: usize,
    },
    /// An app's own way: `action` none hands it back to the other rules.
    AppRoute {
        package: String,
        label: String,
        action: Option<String>,
    },
    /// Fetch the rule sets, from another source when one is named.
    GeoUpdate {
        source: Option<String>,
    },
    GeoAuto {
        on: bool,
    },
    /// The editor is open and wants the categories, or is closed.
    GeoNames {
        open: bool,
    },
    /// «Автовыбор» on or off. Picking a server by hand turns it off.
    Auto {
        on: bool,
    },
    /// The phone's network changed, from the tunnel's service: its kind
    /// ("mobile", "wifi", …, none for no network at all), a Wi-Fi's name
    /// when bliss may read it, and whether it wants a sign-in first.
    Network {
        kind: Option<String>,
        #[serde(default)]
        ssid: Option<String>,
        #[serde(default)]
        captive: bool,
    },
    /// What bliss does on a network, from the screens: `connect` "on",
    /// "off" or "" (the heart decides), and a preset or none.
    SetNetwork {
        id: String,
        #[serde(default)]
        connect: String,
        #[serde(default)]
        preset: Option<String>,
        /// A server's key, "auto", or none to leave the choice alone.
        #[serde(default)]
        server: Option<String>,
    },
    /// A server's star, by its key.
    Favorite {
        key: String,
        on: bool,
    },
    /// «Поделиться журналом»: the journal with every secret taken out,
    /// written to `journal.txt` beside the profile (`Frame::journal`).
    ShareLog,
}

fn yes() -> bool {
    true
}

/// The action a word names; anything unknown goes through the tunnel.
pub fn action_of(word: &str) -> bliss_proto::Action {
    match word {
        "direct" => bliss_proto::Action::Direct,
        "block" => bliss_proto::Action::Block,
        _ => bliss_proto::Action::Proxy,
    }
}

/// The package a rule is about, when that is all it is about.
pub fn app_of(rule: &bliss_proto::Rule) -> Option<String> {
    let only_app = rule.process.len() == 1
        && rule.domain.is_empty()
        && rule.geosite.is_empty()
        && rule.ip.is_empty()
        && rule.geoip.is_empty()
        && rule.port.is_empty()
        && rule.network.is_empty();
    only_app.then(|| rule.process[0].clone())
}

/// What a server is, in the fewest words the list has room for. The same
/// words as the desktop's `kind` (crates/app/src/engine.rs).
pub fn kind(server: &ServerConfig) -> String {
    let name = server.protocol.name();
    match (&server.security, &server.transport) {
        (Security::None, _) | (_, Transport::Hysteria { .. }) => name.to_owned(),
        (Security::Tls(_), _) => format!("{name} · TLS"),
        (Security::Reality(_), _) => format!("{name} · REALITY"),
    }
}

/// The same in full, as the desktop's `how_it_hides`.
pub fn how_it_hides(server: &ServerConfig) -> String {
    let mut parts = vec![server.protocol.name()];
    match &server.transport {
        Transport::Tcp {
            header: TcpHeader::None,
        }
        | Transport::None => {}
        Transport::Tcp {
            header: TcpHeader::Http { .. },
        } => parts.push("tcp-http"),
        Transport::Hysteria { obfs, .. } => {
            if obfs.is_some() {
                parts.push("salamander");
            }
        }
        other => parts.push(other.name()),
    }
    match (&server.security, &server.transport) {
        (Security::None, _) | (_, Transport::Hysteria { .. }) => {}
        (security, _) => parts.push(security.name()),
    }
    if let Protocol::Vless { flow, .. } = &server.protocol
        && *flow != Flow::None
    {
        parts.push("vision");
    }
    parts.join(" · ")
}

/// The action's key, as the screens colour it.
pub fn action_key(action: bliss_proto::Action) -> &'static str {
    match action {
        bliss_proto::Action::Proxy => "proxy",
        bliss_proto::Action::Direct => "direct",
        bliss_proto::Action::Block => "block",
        bliss_proto::Action::Vpn => "vpn",
    }
}

/// A rule's conditions in one line, for the list.
pub fn conditions(rule: &bliss_proto::Rule) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(rule.domain.iter().cloned());
    parts.extend(rule.geosite.iter().map(|g| format!("geosite:{g}")));
    parts.extend(rule.ip.iter().cloned());
    parts.extend(rule.geoip.iter().map(|g| format!("geoip:{g}")));
    parts.extend(rule.port.iter().map(|p| format!("port:{p}")));
    parts.extend(rule.process.iter().map(|p| format!("app:{p}")));
    // The limit last, as «только UDP».
    parts.extend(
        rule.network
            .iter()
            .map(|n| format!("только {}", n.to_ascii_uppercase())),
    );
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_read_as_the_screens_write_them() {
        let c: UiCommand = serde_json::from_str(r#"{"cmd":"select","index":3}"#).unwrap();
        assert!(matches!(
            c,
            UiCommand::Select {
                index: 3,
                key: None
            }
        ));
        let c: UiCommand = serde_json::from_str(r#"{"cmd":"refresh","id":null}"#).unwrap();
        assert!(matches!(c, UiCommand::Refresh { id: None }));
        let c: UiCommand = serde_json::from_str(r#"{"cmd":"hello","full":true}"#).unwrap();
        assert!(matches!(c, UiCommand::Hello { full: true }));
    }

    #[test]
    fn a_frame_carries_no_link() {
        let server: ServerConfig = "vless://11111111-2222-3333-4444-555555555555@203.0.113.7:443?security=reality&sni=www.example.net&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&type=tcp#Test"
            .parse()
            .unwrap();
        let link = "vless://11111111-2222-3333-4444-555555555555@203.0.113.7:443?security=reality&sni=www.example.net&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&type=tcp#Test";
        let row = ServerRow {
            key: server_key(link),
            name: server.tag.clone(),
            kind: kind(&server),
            detail: how_it_hides(&server),
            address: format!("{}:{}", server.address, server.port),
            group: None,
            ping: "unknown",
            ms: None,
            check: "unknown",
            check_ms: None,
            favorite: false,
        };
        let json = serde_json::to_string(&row).unwrap();
        assert!(!json.contains("11111111"), "{json}");
        assert!(!json.contains("AAAAAAAA"), "{json}");
        assert!(json.contains("REALITY"), "{json}");
    }
}
