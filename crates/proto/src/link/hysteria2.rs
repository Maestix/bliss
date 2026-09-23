//! `hysteria2://auth@host:port/?sni=…&obfs=salamander&obfs-password=…#name`
//! (or `hy2://`), from Hysteria's own URI scheme:
//!
//! - the port may be left out, and is then 443;
//! - it may also be a list to hop between, `host:443,20000-30000`, which a
//!   URL parser refuses, so it is taken out before parsing; v2rayN writes the
//!   same list as `mport`;
//! - `insecure=1` and `pinSHA256` are Hysteria's names for what Xray calls
//!   `allowInsecure` and `pcs`.
//!
//! Always TLS: Hysteria 2 is QUIC.

use std::borrow::Cow;

use url::Url;

use super::{Query, endpoint, stream, tag, user_info};
use crate::error::{LinkError, Unsupported};
use crate::server::{Hidden, Protocol, Security, ServerConfig, Transport};

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let (link, ports) = take_ports(link);
    let url = Url::parse(&link)?;
    let auth = user_info(&url).ok_or(LinkError::Missing("password"))?;
    let (address, port) = endpoint(&url, Some(443))?;
    let mut q = Query::of(&url);

    let hop = match q.get("mport").map(str::to_owned).or(ports) {
        Some(hop)
            if hop
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == '-') =>
        {
            Some(hop)
        }
        Some(hop) => {
            return Err(LinkError::BadValue {
                param: "mport",
                expected: "ports and ranges, as in 443,20000-30000",
                got: hop,
            });
        }
        None => None,
    };
    let obfs = match q.get("obfs") {
        None | Some("none") => None,
        Some("salamander") => Some(Hidden(
            q.get("obfs-password")
                .ok_or(LinkError::Missing("obfs-password"))?
                .to_owned(),
        )),
        Some(other) => return Err(LinkError::Unsupported(Unsupported::Obfs(other.to_owned()))),
    };
    if q.get("pcs").is_none()
        && let Some(pin) = q.get("pinSHA256").map(str::to_owned)
    {
        q.set("pcs", pin);
    }

    Ok(ServerConfig {
        tag: tag(url.fragment(), &address, port),
        address,
        port,
        protocol: Protocol::Hysteria2 { auth: Hidden(auth) },
        security: Security::Tls(stream::tls(&q)?),
        transport: Transport::Hysteria { obfs, hop },
        finalmask: stream::finalmask(&q)?,
    })
}

/// The link with a port list replaced by its first port, and the list.
fn take_ports(link: &str) -> (Cow<'_, str>, Option<String>) {
    let Some((scheme, rest)) = link.split_once("://") else {
        return (Cow::Borrowed(link), None);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_at = authority.rfind('@').map_or(0, |at| at + 1);
    let place = &authority[host_at..];
    // An IPv6 literal carries colons of its own.
    let colon = if place.starts_with('[') {
        place.find("]:").map(|at| at + 1)
    } else {
        place.rfind(':')
    };
    let Some(colon) = colon else {
        return (Cow::Borrowed(link), None);
    };
    let ports = &place[colon + 1..];
    if !ports.contains([',', '-']) {
        return (Cow::Borrowed(link), None);
    }
    let first: String = ports.chars().take_while(char::is_ascii_digit).collect();
    let rebuilt = format!(
        "{scheme}://{}{}:{first}{}",
        &authority[..host_at],
        &place[..colon],
        &rest[end..]
    );
    (Cow::Owned(rebuilt), Some(ports.to_owned()))
}
