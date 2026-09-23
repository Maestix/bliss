//! `trojan://password@host:port?security=tls&sni=…&type=…#name`
//!
//! TLS unless the link says otherwise: a Trojan server is a TLS server.

use url::Url;

use super::{Query, endpoint, stream, tag, user_info};
use crate::error::LinkError;
use crate::server::{Hidden, Protocol, ServerConfig};

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let url = Url::parse(link)?;
    let password = user_info(&url).ok_or(LinkError::Missing("password"))?;
    let (address, port) = endpoint(&url, None)?;
    let q = Query::of(&url);
    // A `flow` is dropped rather than passed on: xray removed Flow for Trojan
    // and refuses a config that has one.
    let (security, transport) = stream::parse(&q, "tls")?;

    Ok(ServerConfig {
        tag: tag(url.fragment(), &address, port),
        address,
        port,
        protocol: Protocol::Trojan {
            password: Hidden(password),
        },
        security,
        transport,
        finalmask: stream::finalmask(&q)?,
    })
}
