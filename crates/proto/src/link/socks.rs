//! `socks://BASE64(user:pass)@host:port#name`, as v2rayN writes it, or plainly
//! `socks5://user:pass@host:port`. No user part is no login.

use url::Url;

use super::{Query, base64, decode, endpoint, stream, tag, user_info};
use crate::error::LinkError;
use crate::server::{Hidden, Login, Protocol, Security, ServerConfig, TcpHeader, Transport};

pub(super) fn parse(link: &str) -> Result<ServerConfig, LinkError> {
    let url = Url::parse(link)?;
    let (address, port) = endpoint(&url, None)?;
    let login = user_info(&url).map(|info| {
        // Written out when it has its colon, each half decoded on its own: a
        // user with a colon in it, escaped, was split at that colon once the
        // two were decoded together. Otherwise v2rayN's base64 of the pair.
        let (user, pass) = match url.password() {
            Some(pass) => (decode(url.username()), decode(pass)),
            None => {
                let info = base64(&info)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .filter(|decoded| decoded.contains(':'))
                    .unwrap_or(info);
                match info.split_once(':') {
                    Some((user, pass)) => (user.to_owned(), pass.to_owned()),
                    None => (info, String::new()),
                }
            }
        };
        Login {
            user: Hidden(user),
            pass: Hidden(pass),
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
        // The form writes `fm` for every protocol, and xray reads it under any
        // outbound: dropped here, it was lost the first time the server was
        // saved (found by fuzzing the form).
        finalmask: stream::finalmask(&Query::of(&url))?,
    })
}
