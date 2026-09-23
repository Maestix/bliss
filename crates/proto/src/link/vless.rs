//! `vless://uuid@host:port?encryption=…&flow=…&security=…&type=…#name`

use url::Url;
use uuid::Uuid;

use super::{Query, decode, endpoint, stream, tag};
use crate::encryption::Encryption;
use crate::error::LinkError;
use crate::server::{Flow, Protocol, ServerConfig, UserId};

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let url = Url::parse(link)?;
    let user = match url.username() {
        "" => return Err(LinkError::MissingUuid),
        raw => UserId(Uuid::parse_str(&decode(raw))?),
    };
    let (address, port) = endpoint(&url, None)?;
    let q = Query::of(&url);

    let encryption = Encryption::parse(q.get("encryption").unwrap_or("none"))?;
    let flow = match q.get("flow") {
        None => Flow::None,
        Some("xtls-rprx-vision") => Flow::XtlsRprxVision,
        Some("xtls-rprx-vision-udp443") => Flow::XtlsRprxVisionUdp443,
        Some(other) => {
            return Err(LinkError::BadValue {
                param: "flow",
                expected: "xtls-rprx-vision, xtls-rprx-vision-udp443 or empty",
                got: other.to_owned(),
            });
        }
    };
    let (security, transport) = stream::parse(&q, "none")?;

    Ok(ServerConfig {
        tag: tag(url.fragment(), &address, port),
        address,
        port,
        protocol: Protocol::Vless {
            user,
            encryption,
            flow,
        },
        security,
        transport,
        finalmask: stream::finalmask(&q)?,
    })
}
