//! Generating xray-core's config.
//!
//! Deliberately the smallest config that can carry traffic: one inbound, one
//! outbound, no `routing` section at all. Routing, DNS and the connection table
//! are ours — xray is only a dialer here, and every feature we let it own is one
//! we would have to take back in stage 3.
//!
//! Shapes checked against Xray-core's own config code (`infra/conf`, release
//! 26.3.27) and against configs xray accepts, not written from memory. Things
//! memory would have got wrong:
//!
//! - `streamSettings.network` is `raw` (Xray renamed it from `tcp` in 25.x),
//!   and an inbound can be `mixed`, serving SOCKS and HTTP on one port;
//! - `allowInsecure` is refused outright after 2026-06-01, by a date check in
//!   the release itself; a certificate is pinned (`pinnedPeerCertSha256`)
//!   instead;
//! - mKCP's `header` and `seed` are gone from `kcpSettings`: they are masks in
//!   `finalmask` now, under names that changed again in 26.6 (`build` takes
//!   the engine's version for this);
//! - Hysteria 2 is the `hysteria` protocol over the `hysteria` transport, with
//!   Salamander and port hopping in `finalmask` rather than in its settings;
//! - REALITY goes over RAW, XHTTP and gRPC only (checked when a link is read).

use std::fmt;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use bliss_proto::{
    Encryption, KcpHeader, Protocol, Security, ServerConfig, TcpHeader, Tls, Transport,
};
use serde_json::{Value, json};

/// How talkative xray should be in our log pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogLevel {
    Debug,
    Info,
    #[default]
    Warning,
    Error,
    None,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::None => "none",
        }
    }
}

/// An xray-core release, as `xray version` names it: `26.3.27`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// `26.3.27`, or `v26.3.27`; a missing patch reads as 0.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let mut next = || -> Option<u32> {
            let part = parts.next()?;
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        };
        let major = next()?;
        let minor = next()?;
        Some(Self::new(major, minor, next().unwrap_or(0)))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The first release with finalmask, where mKCP's disguises moved.
const FINALMASK: Version = Version::new(26, 2, 0);
/// The release where those masks became one, `mkcp-legacy`.
const MKCP_LEGACY: Version = Version::new(26, 6, 0);

/// Seconds between Hysteria 2's port hops.
const HOP_INTERVAL: u32 = 30;

/// Build the config for one server.
///
/// `port` is the local SOCKS port bliss will dial through. It is an internal
/// detail between us and the child process, not the port the user configured
/// their browser with — that one is served by our own inbound. `engine` is the
/// version of the xray that will read it.
pub fn build(server: &ServerConfig, port: u16, log: LogLevel, engine: Version) -> Value {
    json!({
        "log": { "loglevel": log.as_str() },
        "inbounds": [{
            "tag": "bliss-in",
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "mixed",
            "settings": { "auth": "noauth", "udp": true }
        }],
        "outbounds": [outbound(server, engine)]
    })
}

fn outbound(server: &ServerConfig, engine: Version) -> Value {
    let address = server.address.to_string();
    let port = server.port;
    let (protocol, settings) = match &server.protocol {
        Protocol::Vless {
            user,
            encryption,
            flow,
        } => {
            let mut account = json!({
                "id": user.0.to_string(),
                "encryption": encryption.as_param(),
            });
            if let Some(flow) = flow.as_param() {
                account["flow"] = json!(flow);
            }
            (
                "vless",
                json!({ "vnext": [{ "address": address, "port": port, "users": [account] }] }),
            )
        }
        Protocol::Vmess { user, cipher } => (
            "vmess",
            json!({ "vnext": [{
                "address": address,
                "port": port,
                "users": [{ "id": user.0.to_string(), "security": cipher.as_param() }]
            }] }),
        ),
        Protocol::Trojan { password } => (
            "trojan",
            json!({ "servers": [{ "address": address, "port": port, "password": password.0 }] }),
        ),
        Protocol::Shadowsocks { method, password } => (
            "shadowsocks",
            json!({ "servers": [{
                "address": address,
                "port": port,
                "method": method,
                "password": password.0
            }] }),
        ),
        Protocol::Socks { login } => {
            let mut target = json!({ "address": address, "port": port });
            if let Some(login) = login {
                target["users"] = json!([{ "user": login.user.0, "pass": login.pass.0 }]);
            }
            ("socks", json!({ "servers": [target] }))
        }
        Protocol::Hysteria2 { .. } => (
            "hysteria",
            json!({ "version": 2, "address": address, "port": port }),
        ),
        Protocol::WireGuard(wg) => {
            let mut peer = json!({
                "publicKey": STANDARD.encode(wg.peer_key.0),
                "endpoint": format!("{address}:{port}"),
            });
            if let Some(psk) = &wg.pre_shared_key {
                peer["preSharedKey"] = json!(STANDARD.encode(psk.0));
            }
            let mut settings = json!({
                "secretKey": STANDARD.encode(wg.secret_key.0),
                "peers": [peer],
            });
            if !wg.addresses.is_empty() {
                settings["address"] = json!(wg.addresses);
            }
            if let Some(mtu) = wg.mtu {
                settings["mtu"] = json!(mtu);
            }
            if let Some(reserved) = wg.reserved {
                settings["reserved"] = json!(reserved);
            }
            ("wireguard", settings)
        }
    };

    let mut outbound = json!({
        "tag": "proxy",
        "protocol": protocol,
        "settings": settings,
        // Mux multiplexes many streams onto one connection, which is exactly
        // what Vision is designed to avoid: it would hide the inner TLS shape
        // that Vision pads around. Off for everything, so no server behaves
        // differently for a reason that is not on screen.
        "mux": { "enabled": false }
    });
    // WireGuard carries itself, and a stream section would only be refused.
    if !matches!(server.transport, Transport::None) {
        outbound["streamSettings"] = stream_settings(server, engine);
    }
    outbound
}

fn stream_settings(server: &ServerConfig, engine: Version) -> Value {
    let mut s = json!({});
    let mut finalmask = None;

    match &server.transport {
        // "tcp" still parses, but "raw" is the current spelling.
        Transport::Tcp { header } => {
            s["network"] = json!("raw");
            if let TcpHeader::Http { host, path } = header {
                s["rawSettings"] = json!({ "header": http_header(host, path) });
            }
        }
        Transport::Ws { path, host } => {
            s["network"] = json!("ws");
            s["wsSettings"] = with_host(json!({ "path": path }), host);
        }
        Transport::HttpUpgrade { path, host } => {
            s["network"] = json!("httpupgrade");
            s["httpupgradeSettings"] = with_host(json!({ "path": path }), host);
        }
        Transport::Xhttp {
            path,
            host,
            mode,
            extra,
        } => {
            s["network"] = json!("xhttp");
            let mut xhttp = with_host(json!({ "path": path, "mode": mode }), host);
            // Checked to be a JSON object when the link was read. xray merges
            // it under `path`, `host` and `mode` itself.
            if let Some(extra) = extra
                .as_ref()
                .and_then(|e| serde_json::from_str::<Value>(&e.0).ok())
            {
                xhttp["extra"] = extra;
            }
            s["xhttpSettings"] = xhttp;
        }
        Transport::Grpc {
            service_name,
            authority,
            multi,
        } => {
            s["network"] = json!("grpc");
            let mut grpc = json!({ "serviceName": service_name, "multiMode": multi });
            if let Some(authority) = authority {
                grpc["authority"] = json!(authority);
            }
            s["grpcSettings"] = grpc;
        }
        Transport::Kcp { header, seed, mtu } => {
            s["network"] = json!("kcp");
            let mut kcp = json!({});
            if let Some(mtu) = mtu {
                kcp["mtu"] = json!(mtu);
            }
            s["kcpSettings"] = kcp;
            finalmask = Some(kcp_masks(
                *header,
                seed.as_ref().map(|s| s.0.as_str()),
                engine,
            ));
        }
        Transport::Hysteria { obfs, hop } => {
            s["network"] = json!("hysteria");
            if let Protocol::Hysteria2 { auth } = &server.protocol {
                s["hysteriaSettings"] = json!({ "version": 2, "auth": auth.0 });
            }
            let mut mask = json!({});
            if let Some(password) = obfs {
                mask["udp"] =
                    json!([{ "type": "salamander", "settings": { "password": password.0 } }]);
            }
            if let Some(ports) = hop {
                // The interval is not optional, whatever its type says: xray
                // 26.3.27 without one panics on the first connection ("non-
                // positive interval for NewTicker"), taking the whole engine
                // with it. Found by the loopback test. No link carries one;
                // 30 s is Hysteria's own default and v2rayN's.
                mask["quicParams"] =
                    json!({ "udpHop": { "ports": ports, "interval": HOP_INTERVAL } });
            }
            if mask != json!({}) {
                finalmask = Some(mask);
            }
        }
        Transport::None => {}
    }

    match &server.security {
        Security::None => {
            s["security"] = json!("none");
        }
        Security::Tls(t) => {
            s["security"] = json!("tls");
            s["tlsSettings"] =
                tls_settings(t, matches!(server.transport, Transport::Hysteria { .. }));
        }
        Security::Reality(r) => {
            s["security"] = json!("reality");
            let mut reality = json!({
                "serverName": r.sni,
                "fingerprint": r.fingerprint.to_string(),
                "publicKey": URL_SAFE_NO_PAD.encode(r.public_key.0),
                "show": false,
            });
            if !r.short_id.is_empty() {
                reality["shortId"] = json!(r.short_id.to_string());
            }
            if let Some(spx) = &r.spider_x {
                reality["spiderX"] = json!(spx);
            }
            if let Some(pqv) = &r.mldsa65_verify {
                reality["mldsa65Verify"] = json!(pqv.0);
            }
            s["realitySettings"] = reality;
        }
    }

    // A link's own `fm` replaces bliss's masks, as it does in v2rayN: it was
    // written for the server, and bliss's guess was not.
    if let Some(own) = server
        .finalmask
        .as_ref()
        .and_then(|fm| serde_json::from_str::<Value>(&fm.0).ok())
    {
        finalmask = Some(own);
    }
    if let Some(mask) = finalmask {
        s["finalmask"] = mask;
    }
    s
}

/// TLS as the link asks for it. Hysteria 2's QUIC does its own handshake, so
/// there is no browser ClientHello to parrot. Its ALPN is not ours to set
/// either: xray hands the TLS config to quic-go's HTTP/3 transport, which
/// writes `h3` over whatever is there.
fn tls_settings(t: &Tls, hysteria: bool) -> Value {
    let mut tls = json!({});
    if !hysteria {
        tls["fingerprint"] = json!(t.fingerprint.to_string());
    }
    if let Some(sni) = &t.sni {
        tls["serverName"] = json!(sni);
    }
    if !t.alpn.is_empty() {
        tls["alpn"] = json!(t.alpn);
    }
    // `allowInsecure` is deliberately never written: this xray refuses the
    // whole config over it. A pinned certificate is what it takes instead.
    if !t.pinned.is_empty() {
        tls["pinnedPeerCertSha256"] = json!(t.pinned.join(","));
    }
    if !t.verify_names.is_empty() {
        tls["verifyPeerCertByName"] = json!(t.verify_names.join(","));
    }
    if let Some(ech) = &t.ech {
        tls["echConfigList"] = json!(ech);
    }
    tls
}

fn with_host(mut settings: Value, host: &Option<String>) -> Value {
    if let Some(host) = host {
        settings["host"] = json!(host);
    }
    settings
}

/// The first bytes of a connection dressed as a browser's GET.
fn http_header(host: &[String], path: &[String]) -> Value {
    let mut headers = json!({
        "User-Agent": [
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
        ],
        "Accept-Encoding": ["gzip, deflate"],
        "Connection": ["keep-alive"],
        "Pragma": "no-cache"
    });
    if !host.is_empty() {
        headers["Host"] = json!(host);
    }
    json!({
        "type": "http",
        "request": { "version": "1.1", "method": "GET", "path": path, "headers": headers }
    })
}

/// mKCP's disguise and obfuscation as masks, innermost first.
///
/// From 26.2 each has a mask of its own; from 26.6 both are `mkcp-legacy`.
/// Without a seed the packets get mKCP's original light obfuscation, as they
/// always did.
fn kcp_masks(header: KcpHeader, seed: Option<&str>, engine: Version) -> Value {
    let mut masks = Vec::new();
    if engine >= MKCP_LEGACY {
        masks.push(match seed {
            Some(seed) => json!({ "type": "mkcp-legacy", "settings": { "value": seed } }),
            None => json!({ "type": "mkcp-legacy" }),
        });
        if header != KcpHeader::None {
            masks.push(json!({ "type": "mkcp-legacy", "settings": { "header": header.name() } }));
        }
    } else {
        masks.push(match seed {
            Some(seed) => json!({ "type": "mkcp-aes128gcm", "settings": { "password": seed } }),
            None => json!({ "type": "mkcp-original" }),
        });
        if header != KcpHeader::None {
            masks.push(json!({ "type": format!("header-{}", header.name()) }));
        }
    }
    json!({ "udp": masks })
}

/// What this config needs from the child process, for the "is your xray new
/// enough" check. An older build either refuses a field it has never heard of
/// or, worse, parses it and fails to connect, which looks like a server
/// problem.
pub fn minimum_xray_version(server: &ServerConfig) -> Option<Version> {
    let mut needs: Option<Version> = None;
    let mut need = |version: Version| {
        needs = Some(needs.map_or(version, |n| n.max(version)));
    };
    if let Protocol::Vless {
        encryption: Encryption::MlKem768X25519Plus(_),
        ..
    } = &server.protocol
    {
        // VLESS Encryption landed in 25.x.
        need(Version::new(25, 0, 0));
    }
    match &server.transport {
        // Hysteria arrived in 26.1, and its Salamander and hopping live in
        // finalmask, which arrived in 26.2.
        Transport::Hysteria { .. } | Transport::Kcp { .. } => need(FINALMASK),
        _ => {}
    }
    if server.finalmask.is_some() {
        need(FINALMASK);
    }
    if let Security::Tls(t) = &server.security {
        if !t.verify_names.is_empty() {
            need(FINALMASK);
        }
        if !t.pinned.is_empty() {
            need(Version::new(26, 1, 0));
        }
    }
    needs
}

#[cfg(test)]
mod tests {
    use super::*;

    const STABLE: Version = Version::new(26, 3, 27);

    fn outbound_of(link: &str, engine: Version) -> Value {
        let server: ServerConfig = link.parse().expect("the link parses");
        build(&server, 10_000, LogLevel::Warning, engine)["outbounds"][0].clone()
    }

    #[test]
    fn versions_read_and_compare_as_numbers() {
        assert_eq!(Version::parse("26.3.27"), Some(STABLE));
        assert_eq!(Version::parse("v26.10"), Some(Version::new(26, 10, 0)));
        assert_eq!(Version::parse("Xray"), None);
        // 26.10 is after 26.9, which a comparison of text would get wrong.
        assert!(Version::new(26, 10, 0) > Version::new(26, 9, 9));
        assert_eq!(STABLE.to_string(), "26.3.27");
    }

    /// xray 26.3.27 panics on the first connection when a hop has no
    /// interval, and the whole engine goes with it.
    #[test]
    fn a_port_hop_always_has_an_interval() {
        let out = outbound_of("hy2://PPPP@example.net:443/?mport=20000-30000", STABLE);
        let hop = &out["streamSettings"]["finalmask"]["quicParams"]["udpHop"];
        assert_eq!(hop["ports"], "20000-30000");
        assert!(
            hop["interval"].as_u64().is_some_and(|s| s >= 5),
            "got {hop}"
        );
    }

    /// Hysteria 2 is the `hysteria` protocol over the `hysteria` transport,
    /// its password in the transport and no browser fingerprint on QUIC.
    #[test]
    fn hysteria2_is_written_the_way_xray_reads_it() {
        let out = outbound_of(
            "hy2://PPPP@example.net/?sni=a.example&obfs=salamander&obfs-password=OOOO",
            STABLE,
        );
        assert_eq!(out["protocol"], "hysteria");
        assert_eq!(
            out["settings"],
            json!({ "version": 2, "address": "example.net", "port": 443 })
        );
        let stream = &out["streamSettings"];
        assert_eq!(stream["network"], "hysteria");
        assert_eq!(
            stream["hysteriaSettings"],
            json!({ "version": 2, "auth": "PPPP" })
        );
        assert_eq!(
            stream["finalmask"]["udp"],
            json!([{ "type": "salamander", "settings": { "password": "OOOO" } }])
        );
        assert!(stream["tlsSettings"].get("fingerprint").is_none());
        assert_eq!(stream["tlsSettings"]["serverName"], "a.example");
    }

    /// Refused outright by xray after 2026-06-01, whole config and all.
    #[test]
    fn allow_insecure_is_never_written() {
        let pin = "ab".repeat(32);
        let out = outbound_of(
            &format!("trojan://PPPP@example.net:443?allowInsecure=1&pcs={pin}&vcn=a.example"),
            STABLE,
        );
        let tls = &out["streamSettings"]["tlsSettings"];
        assert!(tls.get("allowInsecure").is_none(), "got {tls}");
        assert_eq!(tls["pinnedPeerCertSha256"], pin);
        assert_eq!(tls["verifyPeerCertByName"], "a.example");
    }

    /// mKCP's disguise and seed are masks, innermost first, under names that
    /// changed in 26.6.
    #[test]
    fn mkcp_masks_follow_the_engine_version() {
        const LINK: &str = "vless://11111111-2222-3333-4444-555555555555@example.net:443\
                            ?type=kcp&headerType=wechat-video&seed=SSSS";
        let masks =
            |engine| outbound_of(LINK, engine)["streamSettings"]["finalmask"]["udp"].clone();
        assert_eq!(
            masks(STABLE),
            json!([
                { "type": "mkcp-aes128gcm", "settings": { "password": "SSSS" } },
                { "type": "header-wechat" }
            ])
        );
        assert_eq!(
            masks(Version::new(26, 9, 9)),
            json!([
                { "type": "mkcp-legacy", "settings": { "value": "SSSS" } },
                { "type": "mkcp-legacy", "settings": { "header": "wechat" } }
            ])
        );
        let plain = "vless://11111111-2222-3333-4444-555555555555@example.net:443?type=kcp";
        assert_eq!(
            outbound_of(plain, STABLE)["streamSettings"]["finalmask"]["udp"],
            json!([{ "type": "mkcp-original" }])
        );
    }

    /// A link's `fm` was written for its server; bliss's guess gives way.
    #[test]
    fn the_links_own_finalmask_wins() {
        let out = outbound_of(
            "vless://11111111-2222-3333-4444-555555555555@example.net:443\
             ?type=kcp&fm=%7B%22udp%22%3A%5B%7B%22type%22%3A%22header-dns%22%7D%5D%7D",
            STABLE,
        );
        assert_eq!(
            out["streamSettings"]["finalmask"],
            json!({ "udp": [{ "type": "header-dns" }] })
        );
    }

    #[test]
    fn wireguard_carries_itself() {
        let key = ("A".repeat(43) + "=").replace('=', "%3D");
        let peer = "B".repeat(42) + "Q%3D";
        let out = outbound_of(
            &format!(
                "wireguard://{key}@203.0.113.7:51820?publickey={peer}&address=10.7.0.2%2F32&reserved=1,2,3"
            ),
            STABLE,
        );
        assert_eq!(out["protocol"], "wireguard");
        assert!(out.get("streamSettings").is_none(), "got {out}");
        let settings = &out["settings"];
        assert_eq!(settings["secretKey"], "A".repeat(43) + "=");
        assert_eq!(settings["peers"][0]["endpoint"], "203.0.113.7:51820");
        assert_eq!(settings["address"], json!(["10.7.0.2/32"]));
        assert_eq!(settings["reserved"], json!([1, 2, 3]));
    }

    #[test]
    fn the_engine_it_needs_is_named() {
        let needs = |link: &str| minimum_xray_version(&link.parse().unwrap());
        assert_eq!(
            needs("vless://11111111-2222-3333-4444-555555555555@example.net:443"),
            None
        );
        assert_eq!(needs("hy2://PPPP@example.net"), Some(FINALMASK));
        assert_eq!(
            needs("vless://11111111-2222-3333-4444-555555555555@example.net:443?type=kcp"),
            Some(FINALMASK)
        );
        assert_eq!(
            needs(
                "vless://11111111-2222-3333-4444-555555555555@example.net:443\
                 ?encryption=mlkem768x25519plus.native.0rtt.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            ),
            Some(Version::new(25, 0, 0))
        );
    }
}
