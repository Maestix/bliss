//! `wireguard://PRIVATEKEY@host:port?publickey=…&address=…&reserved=…&mtu=…#name`
//!
//! v2rayN's shape: this side's private key where a user would be, and the rest
//! in the query. `presharedkey` is optional; `reserved` is three numbers, or
//! the same three bytes in base64 as Cloudflare's WARP tools print them.

use url::Url;

use super::{Query, base64, endpoint, key, list, number, stream, tag, user_info};
use crate::error::LinkError;
use crate::server::{
    PrivateKey, Protocol, PublicKey, Security, ServerConfig, Transport, WireGuard,
};

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let url = Url::parse(link)?;
    let secret = user_info(&url).ok_or(LinkError::Missing("private key"))?;
    let (address, port) = endpoint(&url, None)?;
    let q = Query::of(&url);

    let peer = q
        .get("publickey")
        .or_else(|| q.get("publicKey"))
        .ok_or(LinkError::Missing("publickey"))?;
    let wireguard = WireGuard {
        secret_key: PrivateKey(key("private key", &secret)?),
        peer_key: PublicKey(key("publickey", peer)?),
        pre_shared_key: q
            .get("presharedkey")
            .map(|psk| key("presharedkey", psk).map(PrivateKey))
            .transpose()?,
        addresses: list(q.get("address").or_else(|| q.get("ip"))),
        reserved: q.get("reserved").map(reserved).transpose()?,
        mtu: number("mtu", "a number of bytes", q.get("mtu"))?,
    };

    Ok(ServerConfig {
        tag: tag(url.fragment(), &address, port),
        address,
        port,
        protocol: Protocol::WireGuard(wireguard),
        security: Security::None,
        transport: Transport::None,
        finalmask: stream::finalmask(&q)?,
    })
}

fn reserved(raw: &str) -> Result<[u8; 3], LinkError> {
    let bad = || LinkError::BadValue {
        param: "reserved",
        expected: "three numbers from 0 to 255, or three bytes in base64",
        got: raw.to_owned(),
    };
    let bytes: Vec<u8> = if raw.contains(',') {
        raw.split(',')
            .map(|n| n.trim().parse::<u8>())
            .collect::<Result<_, _>>()
            .map_err(|_| bad())?
    } else {
        base64(raw).ok_or_else(bad)?
    };
    bytes.try_into().map_err(|_| bad())
}
