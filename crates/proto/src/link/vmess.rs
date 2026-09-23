//! VMess links come in two shapes:
//!
//! - v2rayN's, which nearly every provider hands out: `vmess://` and a base64
//!   JSON object — `add`, `port`, `id`, `scy`, `net`, `type`, `host`, `path`,
//!   `tls`, `sni`, `alpn`, `fp` and the rest;
//! - the URL of the AEAD share-link proposal:
//!   `vmess://uuid@host:port?encryption=auto&type=…&security=…#name`.
//!
//! The JSON is turned into the query the URL form would have carried, so both
//! go through the same reading of transports and TLS.

use serde_json::Value;
use url::Url;
use uuid::Uuid;

use super::{Query, base64, decode, endpoint, stream, tag};
use crate::error::LinkError;
use crate::server::{Address, Protocol, ServerConfig, UserId, VmessCipher};

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let body = link.split_once("://").map_or("", |(_, body)| body);
    // Base64 has no `@`, and the URL form cannot do without one.
    if body.contains('@') {
        url_form(link)
    } else {
        json_form(body)
    }
}

fn url_form(link: &str) -> Result<ServerConfig, LinkError> {
    let url = Url::parse(link)?;
    let user = match url.username() {
        "" => return Err(LinkError::MissingUuid),
        raw => UserId(Uuid::parse_str(&decode(raw))?),
    };
    let (address, port) = endpoint(&url, None)?;
    let q = Query::of(&url);
    let (security, transport) = stream::parse(&q, "none")?;
    Ok(ServerConfig {
        tag: tag(url.fragment(), &address, port),
        address,
        port,
        protocol: Protocol::Vmess {
            user,
            cipher: cipher(q.get("encryption"))?,
        },
        security,
        transport,
        finalmask: stream::finalmask(&q)?,
    })
}

fn json_form(body: &str) -> Result<ServerConfig, LinkError> {
    // Some generators put a name after the blob as well.
    let (blob, fragment) = match body.split_once('#') {
        Some((blob, fragment)) => (blob, Some(fragment)),
        None => (body, None),
    };
    // Described, never quoted: the blob is the whole credential.
    let not_json = || LinkError::BadValue {
        param: "vmess",
        expected: "base64-encoded JSON",
        got: "something else".to_owned(),
    };
    let text = base64(blob)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(not_json)?;
    let json: Value = serde_json::from_str(&text).map_err(|_| not_json())?;
    if !json.is_object() {
        return Err(not_json());
    }
    let field = |key: &str| -> Option<String> {
        match json.get(key)? {
            Value::String(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };

    let host = field("add").ok_or(LinkError::MissingHost)?;
    let address = Address::from_host(&host);
    let port = field("port").ok_or(LinkError::MissingPort)?;
    let port: u16 = port.parse().map_err(|_| LinkError::BadValue {
        param: "port",
        expected: "a port number",
        got: port,
    })?;
    let user = UserId(Uuid::parse_str(
        &field("id").ok_or(LinkError::MissingUuid)?,
    )?);

    let mut q = Query::default();
    let net = field("net").unwrap_or_else(|| "tcp".to_owned());
    let kind = field("type").filter(|kind| kind != "none");
    let (host_param, path_param) = (field("host"), field("path"));
    let mut put = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            q.set(key, value);
        }
    };
    match net.as_str() {
        "tcp" | "raw" => {
            put("headerType", kind);
            put("host", host_param);
            put("path", path_param);
        }
        // v2rayN keeps mKCP's seed where a path would be.
        "kcp" | "mkcp" => {
            put("headerType", kind);
            put("seed", path_param);
        }
        "xhttp" | "splithttp" => {
            put("mode", kind);
            put("host", host_param);
            put("path", path_param);
            put("extra", field("extra"));
        }
        // And gRPC's service name and authority where a path and a host would be.
        "grpc" | "gun" => {
            put("mode", kind);
            put("serviceName", path_param);
            put("authority", host_param);
        }
        _ => {
            put("host", host_param);
            put("path", path_param);
        }
    }
    put("type", Some(net));
    put("security", field("tls"));
    put("allowInsecure", field("insecure"));
    for key in [
        "sni", "alpn", "fp", "pcs", "vcn", "ech", "pbk", "sid", "spx", "pqv", "fm",
    ] {
        put(key, field(key));
    }

    let (security, transport) = stream::parse(&q, "none")?;
    let name = field("ps");
    Ok(ServerConfig {
        tag: match name {
            Some(name) => name,
            None => tag(fragment, &address, port),
        },
        address,
        port,
        protocol: Protocol::Vmess {
            user,
            cipher: cipher(field("scy").as_deref())?,
        },
        security,
        transport,
        finalmask: stream::finalmask(&q)?,
    })
}

fn cipher(raw: Option<&str>) -> Result<VmessCipher, LinkError> {
    Ok(match raw.unwrap_or("auto") {
        "auto" => VmessCipher::Auto,
        "aes-128-gcm" => VmessCipher::Aes128Gcm,
        "chacha20-poly1305" => VmessCipher::Chacha20Poly1305,
        "none" => VmessCipher::None,
        "zero" => VmessCipher::Zero,
        other => {
            return Err(LinkError::BadValue {
                param: "scy",
                expected: "auto, aes-128-gcm, chacha20-poly1305, none or zero",
                got: other.to_owned(),
            });
        }
    })
}
