//! `socks://BASE64(user:pass)@host:port#name`, as v2rayN writes it, or plainly
//! `socks5://user:pass@host:port`. No user part is no login.

use url::Url;

use super::{base64, endpoint, tag, user_info};
use crate::error::LinkError;
use crate::server::{Hidden, Login, Protocol, Security, ServerConfig, TcpHeader, Transport};

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let url = Url::parse(link)?;
    let (address, port) = endpoint(&url, None)?;
    let login = user_info(&url).map(|info| {
        // Written out when it has its colon; otherwise v2rayN's base64 of it.
        let info = if url.password().is_some() {
            info
        } else {
            base64(&info)
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .filter(|decoded| decoded.contains(':'))
                .unwrap_or(info)
        };
        let (user, pass) = info.split_once(':').unwrap_or((&info, ""));
        Login {
            user: Hidden(user.to_owned()),
            pass: Hidden(pass.to_owned()),
        }
    });

    Ok(ServerConfig {
        tag: tag(url.fragment(), &address, port),
        address,
        port,
        protocol: Protocol::Socks { login },
        security: Security::None,
        transport: Transport::Tcp {
            header: TcpHeader::None,
        },
        finalmask: None,
    })
}
