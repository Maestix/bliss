//! Shadowsocks links, in both of their shapes:
//!
//! - SIP002: `ss://userinfo@host:port/?plugin=…#name`, where the user info is
//!   `method:password` in base64 — or percent-encoded as it is, which is how
//!   the 2022 ciphers are written;
//! - the older one, the whole `method:password@host:port` in base64.
//!
//! Two plugins can be kept, because xray can do what they do: obfs-local's
//! HTTP disguise is a TCP header, and v2ray-plugin is WebSocket, with or
//! without TLS. Anything else would need the plugin itself.

use url::Url;

use super::{Query, base64, decode, endpoint, stream, tag};
use crate::error::{LinkError, Unsupported};
use crate::server::{
    Address, Fingerprint, Hidden, Protocol, Security, ServerConfig, TcpHeader, Tls, Transport,
};

/// The methods xray-core 26 builds. The stream ciphers (`aes-256-cfb`,
/// `rc4-md5`, …) went with Shadowsocks' own deprecation of them.
const METHODS: &[&str] = &[
    "aes-128-gcm",
    "aes-256-gcm",
    "chacha20-poly1305",
    "chacha20-ietf-poly1305",
    "xchacha20-poly1305",
    "xchacha20-ietf-poly1305",
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "none",
    "plain",
];

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let body = link.split_once("://").map_or("", |(_, body)| body);
    let before_name = body.split(['#', '?', '/']).next().unwrap_or_default();
    if !before_name.contains('@') {
        return legacy(body);
    }

    let url = Url::parse(link)?;
    let info = super::user_info(&url).ok_or(LinkError::Missing("method and password"))?;
    // Written out, or in base64 when there is no colon to split on.
    let info = if info.contains(':') {
        info
    } else {
        base64(&info)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or(LinkError::Missing("method and password"))?
    };
    let (method, password) = credentials(&info)?;
    let (address, port) = endpoint(&url, None)?;
    let q = Query::of(&url);
    let (security, transport) = match q.get("plugin") {
        Some(plugin) => self::plugin(plugin)?,
        None => stream::parse(&q, "none")?,
    };

    Ok(ServerConfig {
        tag: tag(url.fragment(), &address, port),
        address,
        port,
        protocol: Protocol::Shadowsocks { method, password },
        security,
        transport,
        finalmask: stream::finalmask(&q)?,
    })
}

/// `ss://BASE64(method:password@host:port)#name`
fn legacy(body: &str) -> Result<ServerConfig, LinkError> {
    let (blob, fragment) = match body.split_once('#') {
        Some((blob, fragment)) => (blob, Some(fragment)),
        None => (body, None),
    };
    let text = base64(&decode(blob))
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or(LinkError::Missing("method and password"))?;
    let (info, place) = text.rsplit_once('@').ok_or(LinkError::MissingHost)?;
    let (method, password) = credentials(info)?;
    let (host, port) = place.rsplit_once(':').ok_or(LinkError::MissingPort)?;
    if host.is_empty() {
        return Err(LinkError::MissingHost);
    }
    let port: u16 = port.trim().parse().map_err(|_| LinkError::BadValue {
        param: "port",
        expected: "a port number",
        got: port.to_owned(),
    })?;
    let address = Address::from_host(host)?;

    Ok(ServerConfig {
        tag: tag(fragment, &address, port),
        address,
        port,
        protocol: Protocol::Shadowsocks { method, password },
        security: Security::None,
        transport: Transport::Tcp {
            header: TcpHeader::None,
        },
        finalmask: None,
    })
}

fn credentials(info: &str) -> Result<(String, Hidden), LinkError> {
    let (method, password) = info.split_once(':').ok_or(LinkError::Missing("password"))?;
    let method = method.trim().to_ascii_lowercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(LinkError::Unsupported(Unsupported::Cipher(method)));
    }
    Ok((method, Hidden(password.to_owned())))
}

/// `obfs-local;obfs=http;obfs-host=…` or `v2ray-plugin;mode=websocket;host=…;path=…;tls`
fn plugin(spec: &str) -> Result<(Security, Transport), LinkError> {
    let mut parts = spec.split(';').map(str::trim).filter(|p| !p.is_empty());
    let name = parts.next().unwrap_or_default();
    let options: Vec<(&str, &str)> = parts
        .map(|part| part.split_once('=').unwrap_or((part, "")))
        .collect();
    let option = |key: &str| {
        options
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .filter(|v| !v.is_empty())
    };

    match name {
        "obfs-local" | "simple-obfs" => match option("obfs") {
            Some("http") => Ok((
                Security::None,
                Transport::Tcp {
                    header: TcpHeader::Http {
                        host: option("obfs-host").map(str::to_owned).into_iter().collect(),
                        path: vec![option("obfs-uri").unwrap_or("/").to_owned()],
                    },
                },
            )),
            other => Err(LinkError::Unsupported(Unsupported::Plugin(format!(
                "obfs-local obfs={}",
                other.unwrap_or("tls")
            )))),
        },
        "v2ray-plugin" => {
            if option("mode").is_some_and(|mode| mode != "websocket") {
                return Err(LinkError::Unsupported(Unsupported::Plugin(format!(
                    "v2ray-plugin mode={}",
                    option("mode").unwrap_or_default()
                ))));
            }
            let host = option("host").map(str::to_owned);
            let security = if options.iter().any(|(k, _)| *k == "tls") {
                Security::Tls(Tls {
                    sni: host.clone(),
                    alpn: Vec::new(),
                    fingerprint: Fingerprint::default(),
                    allow_insecure: false,
                    pinned: Vec::new(),
                    verify_names: Vec::new(),
                    ech: None,
                })
            } else {
                Security::None
            };
            Ok((
                security,
                Transport::Ws {
                    path: option("path").unwrap_or("/").to_owned(),
                    host,
                },
            ))
        }
        other => Err(LinkError::Unsupported(Unsupported::Plugin(
            other.to_owned(),
        ))),
    }
}
