//! The query parameters every URL-shaped link shares: the transport, and the
//! TLS or REALITY around it.
//!
//! Names follow the VLESS share-link proposal (XTLS/Xray-core discussion
//! #716) as v2rayN reads it, plus what Xray added since: `pcs`, `vcn`, `ech`,
//! `pqv`, XHTTP's `extra` and the `fm` finalmask.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use super::{Query, list, number};
use crate::error::{LinkError, Unsupported};
use crate::server::{
    Fingerprint, Hidden, KcpHeader, Long, PublicKey, Reality, Security, ShortId, TcpHeader, Tls,
    Transport,
};

/// The security and the transport, checked against each other.
///
/// `default_security` is what the scheme means without a `security`
/// parameter: nothing for VLESS, TLS for Trojan.
pub(super) fn parse(q: &Query, default_security: &str) -> Result<(Security, Transport), LinkError> {
    let security = security(q, default_security)?;
    let transport = transport(q)?;
    // Xray refuses this itself, at start, with a message the window cannot
    // show; said here, it is said when the link is added.
    if matches!(security, Security::Reality(_))
        && !matches!(
            transport,
            Transport::Tcp { .. } | Transport::Xhttp { .. } | Transport::Grpc { .. }
        )
    {
        return Err(LinkError::BadValue {
            param: "type",
            expected: "tcp, xhttp or grpc under REALITY",
            got: transport.name().to_owned(),
        });
    }
    Ok((security, transport))
}

fn security(q: &Query, default: &str) -> Result<Security, LinkError> {
    match q.get("security").unwrap_or(default) {
        "none" => Ok(Security::None),
        "tls" => tls(q).map(Security::Tls),
        "reality" => reality(q).map(Security::Reality),
        "xtls" => Err(LinkError::Unsupported(Unsupported::LegacyXtls)),
        other => Err(LinkError::BadValue {
            param: "security",
            expected: "none, tls or reality",
            got: other.to_owned(),
        }),
    }
}

pub(super) fn tls(q: &Query) -> Result<Tls, LinkError> {
    let pinned = list(q.get("pcs"));
    for hash in &pinned {
        let hex: String = hash.chars().filter(|c| *c != ':').collect();
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(LinkError::BadValue {
                param: "pcs",
                expected: "SHA-256 in hex, 64 digits",
                got: hash.clone(),
            });
        }
    }
    Ok(Tls {
        // `peer` is trojan-go's name for it.
        sni: q.get("sni").or_else(|| q.get("peer")).map(str::to_owned),
        alpn: list(q.get("alpn")),
        fingerprint: q.get("fp").map(Fingerprint::from).unwrap_or_default(),
        allow_insecure: matches!(
            q.get("allowInsecure").or_else(|| q.get("insecure")),
            Some("1" | "true")
        ),
        pinned,
        verify_names: list(q.get("vcn")),
        ech: q.get("ech").map(str::to_owned),
    })
}

fn reality(q: &Query) -> Result<Reality, LinkError> {
    Ok(Reality {
        sni: q
            .get("sni")
            .ok_or(LinkError::RealityMissing("sni"))?
            .to_owned(),
        fingerprint: q.get("fp").map(Fingerprint::from).unwrap_or_default(),
        public_key: public_key(q.get("pbk").ok_or(LinkError::RealityMissing("pbk"))?)?,
        short_id: match q.get("sid") {
            Some(hex) => ShortId::parse_hex(hex)?,
            None => ShortId::EMPTY,
        },
        spider_x: q.get("spx").map(str::to_owned),
        mldsa65_verify: q.get("pqv").map(|v| Long(v.to_owned())),
    })
}

fn public_key(raw: &str) -> Result<PublicKey, LinkError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| LinkError::BadValue {
            param: "pbk",
            expected: "base64url without padding",
            got: raw.to_owned(),
        })?;
    let len = bytes.len();
    Ok(PublicKey(bytes.try_into().map_err(|_| {
        LinkError::BadValue {
            param: "pbk",
            expected: "32 bytes",
            got: format!("{len} bytes"),
        }
    })?))
}

fn transport(q: &Query) -> Result<Transport, LinkError> {
    let path = |default: &str| q.get("path").unwrap_or(default).to_owned();
    let host = || q.get("host").map(str::to_owned);
    Ok(match q.get("type").unwrap_or("tcp") {
        // Xray renamed "tcp" to "raw" in 25.x; links in the wild use both.
        "tcp" | "raw" => Transport::Tcp {
            header: match q.get("headerType") {
                None | Some("none") => TcpHeader::None,
                Some("http") => TcpHeader::Http {
                    host: list(q.get("host")),
                    path: match list(q.get("path")) {
                        paths if paths.is_empty() => vec!["/".to_owned()],
                        paths => paths,
                    },
                },
                Some(other) => {
                    return Err(LinkError::BadValue {
                        param: "headerType",
                        expected: "none or http",
                        got: other.to_owned(),
                    });
                }
            },
        },
        "ws" | "websocket" => Transport::Ws {
            path: path("/"),
            host: host(),
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: path("/"),
            host: host(),
        },
        "xhttp" | "splithttp" => Transport::Xhttp {
            path: path("/"),
            host: host(),
            mode: match q.get("mode").unwrap_or("auto") {
                mode @ ("auto" | "packet-up" | "stream-up" | "stream-one") => mode.to_owned(),
                other => {
                    return Err(LinkError::BadValue {
                        param: "mode",
                        expected: "auto, packet-up, stream-up or stream-one",
                        got: other.to_owned(),
                    });
                }
            },
            extra: json(q, "extra")?,
        },
        "grpc" | "gun" => Transport::Grpc {
            service_name: q.get("serviceName").unwrap_or_default().to_owned(),
            authority: q.get("authority").map(str::to_owned),
            multi: match q.get("mode").unwrap_or("gun") {
                "gun" => false,
                "multi" => true,
                other => {
                    return Err(LinkError::BadValue {
                        param: "mode",
                        expected: "gun or multi",
                        got: other.to_owned(),
                    });
                }
            },
        },
        "kcp" | "mkcp" => Transport::Kcp {
            header: match q.get("headerType").unwrap_or("none") {
                "none" => KcpHeader::None,
                "srtp" => KcpHeader::Srtp,
                "utp" => KcpHeader::Utp,
                "wechat-video" | "wechat" => KcpHeader::Wechat,
                "dtls" => KcpHeader::Dtls,
                "wireguard" => KcpHeader::WireGuard,
                other => {
                    return Err(LinkError::BadValue {
                        param: "headerType",
                        expected: "none, srtp, utp, wechat-video, dtls or wireguard",
                        got: other.to_owned(),
                    });
                }
            },
            seed: q.get("seed").map(|seed| Hidden(seed.to_owned())),
            mtu: number("mtu", "a number of bytes", q.get("mtu"))?,
        },
        other => return Err(LinkError::UnsupportedTransport(other.to_owned())),
    })
}

/// `fm`: a finalmask object, carried to xray as written.
pub(super) fn finalmask(q: &Query) -> Result<Option<Hidden>, LinkError> {
    json(q, "fm")
}

/// A parameter that holds a JSON object. Checked here so a broken one is
/// refused when the link is added, not when xray starts; kept hidden, since
/// such an object can hold a password.
fn json(q: &Query, param: &'static str) -> Result<Option<Hidden>, LinkError> {
    let Some(raw) = q.get(param) else {
        return Ok(None);
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(_)) => Ok(Some(Hidden(raw.to_owned()))),
        _ => Err(LinkError::BadValue {
            param,
            expected: "a JSON object",
            got: "something else".to_owned(),
        }),
    }
}
