//! Generating xray-core's config.
//!
//! Deliberately the smallest config that can carry traffic: one inbound, one
//! outbound, no `routing` section at all. Routing, DNS and the connection table
//! are ours — xray is only a dialer here, and every feature we let it own is one
//! we would have to take back in stage 3.
//!
//! Shapes verified against what v2rayN generates for a live server on Xray
//! 26.7.28, not from memory. Two things that memory would have got wrong:
//! `streamSettings.network` is `raw` (Xray renamed it from `tcp` in 25.x), and
//! an inbound can be `mixed`, serving SOCKS and HTTP on one port.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bliss_proto::{Encryption, Security, ServerConfig, Transport};
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

/// Build the config for one server.
///
/// `port` is the local SOCKS port bliss will dial through. It is an internal
/// detail between us and the child process, not the port the user configured
/// their browser with — that one is served by our own inbound.
pub fn build(server: &ServerConfig, port: u16, log: LogLevel) -> Value {
    json!({
        "log": { "loglevel": log.as_str() },
        "inbounds": [{
            "tag": "bliss-in",
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "mixed",
            "settings": { "auth": "noauth", "udp": true }
        }],
        "outbounds": [outbound(server)]
    })
}

fn outbound(server: &ServerConfig) -> Value {
    let mut user = json!({
        "id": server.user.0.to_string(),
        "encryption": server.encryption.as_param(),
    });
    if let bliss_proto::Flow::XtlsRprxVision = server.flow {
        user["flow"] = json!("xtls-rprx-vision");
    }

    json!({
        "tag": "proxy",
        "protocol": "vless",
        "settings": {
            "vnext": [{
                "address": server.address.to_string(),
                "port": server.port,
                "users": [user]
            }]
        },
        "streamSettings": stream_settings(server),
        // Mux multiplexes many streams onto one connection, which is exactly
        // what Vision is designed to avoid: it would hide the inner TLS shape
        // that Vision pads around.
        "mux": { "enabled": false }
    })
}

fn stream_settings(server: &ServerConfig) -> Value {
    let network = match server.transport {
        // "tcp" still parses, but "raw" is the current spelling and the one
        // v2rayN emits for this Xray.
        Transport::Tcp { .. } => "raw",
    };

    let mut s = json!({ "network": network });

    match &server.security {
        Security::None => {
            s["security"] = json!("none");
        }
        Security::Tls(t) => {
            s["security"] = json!("tls");
            let mut tls = json!({ "fingerprint": t.fingerprint.to_string() });
            if let Some(sni) = &t.sni {
                tls["serverName"] = json!(sni);
            }
            if !t.alpn.is_empty() {
                tls["alpn"] = json!(t.alpn);
            }
            if t.allow_insecure {
                tls["allowInsecure"] = json!(true);
            }
            s["tlsSettings"] = tls;
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
            s["realitySettings"] = reality;
        }
    }

    s
}

/// What this config needs from the child process, for the "is your xray new
/// enough" check. VLESS Encryption landed in Xray 25.x; an older build parses
/// the field and then fails to connect, which looks like a server problem.
pub fn minimum_xray_version(server: &ServerConfig) -> Option<(u32, u32)> {
    match server.encryption {
        Encryption::MlKem768X25519Plus(_) => Some((25, 0)),
        Encryption::None => None,
    }
}
