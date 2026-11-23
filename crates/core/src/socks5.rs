//! SOCKS5, both ends of it.
//!
//! The client half dials the engine; the server half serves whatever the user
//! pointed at bliss. Same protocol, opposite directions, so they live together.
//!
//! One trap worth naming: SOCKS5 address types are **1 = IPv4, 3 = domain,
//! 4 = IPv6**. VLESS uses 1, 2, 3 for the same three things. Mixing them up
//! produces a connection that opens and then talks to the wrong place.

use std::net::SocketAddr;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::target::Target;

const VERSION: u8 = 5;
const NO_AUTH: u8 = 0;
/// RFC 1929: a user name and a password.
const USER_PASS: u8 = 2;
/// The version of RFC 1929's own little exchange.
const USER_PASS_VERSION: u8 = 1;
const CMD_CONNECT: u8 = 1;
const CMD_UDP: u8 = 3;
const RESERVED: u8 = 0;

/// The header every datagram carries: two reserved bytes, a fragment number,
/// then the address it is going to or came from.
const DATAGRAM_HEAD: usize = 3;

const ATYP_IPV4: u8 = 1;
const ATYP_DOMAIN: u8 = 3;
const ATYP_IPV6: u8 = 4;

/// Longest domain the address field can carry: one length byte.
const MAX_DOMAIN: usize = 255;

#[derive(Debug, Error)]
pub enum SocksError {
    #[error("io")]
    Io(#[from] std::io::Error),

    #[error("not SOCKS5: version byte {0:#04x}")]
    Version(u8),

    #[error("the proxy wants an authentication method bliss does not offer ({0:#04x})")]
    Auth(u8),

    #[error("the proxy refused the login")]
    LoginRefused,

    #[error("unsupported command {0:#04x} — bliss serves CONNECT only")]
    Command(u8),

    #[error("unsupported address type {0:#04x}")]
    AddressType(u8),

    #[error("domain name is not utf-8")]
    DomainEncoding,

    #[error("the proxy refused: {}", reply_reason(*.0))]
    Refused(u8),
}

fn reply_reason(code: u8) -> &'static str {
    match code {
        1 => "general failure",
        2 => "not allowed by ruleset",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        _ => "unknown reason",
    }
}

// --- client: dial through a SOCKS5 proxy ----------------------------------

/// A user name and a password for the engine's port (RFC 1929). Each at most
/// 255 bytes, which is all the exchange can carry.
#[derive(Clone)]
pub struct Login {
    pub user: String,
    pub pass: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// The greeting: no authentication, or the login when there is one — and
/// then the login itself.
async fn greet<S>(stream: &mut S, login: Option<&Login>) -> Result<(), SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let method = if login.is_some() { USER_PASS } else { NO_AUTH };
    stream.write_all(&[VERSION, 1, method]).await?;
    stream.flush().await?;

    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting[0] != VERSION {
        return Err(SocksError::Version(greeting[0]));
    }
    if greeting[1] != method {
        return Err(SocksError::Auth(greeting[1]));
    }
    if let Some(login) = login {
        let (user, pass) = (login.user.as_bytes(), login.pass.as_bytes());
        if user.len() > 255 || pass.len() > 255 {
            return Err(SocksError::Auth(USER_PASS));
        }
        let mut hello = vec![USER_PASS_VERSION, user.len() as u8];
        hello.extend_from_slice(user);
        hello.push(pass.len() as u8);
        hello.extend_from_slice(pass);
        stream.write_all(&hello).await?;
        stream.flush().await?;
        let mut answer = [0u8; 2];
        stream.read_exact(&mut answer).await?;
        if answer[1] != 0 {
            return Err(SocksError::LoginRefused);
        }
    }
    Ok(())
}

/// Greet the proxy — with `login` when there is one — and ask it to connect
/// to `target`.
///
/// On return the stream carries the tunnelled connection and nothing else.
pub async fn connect_through<S>(
    stream: &mut S,
    target: &Target,
    login: Option<&Login>,
) -> Result<(), SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    greet(stream, login).await?;

    let mut request = vec![VERSION, CMD_CONNECT, RESERVED];
    write_target(&mut request, target)?;
    stream.write_all(&request).await?;
    stream.flush().await?;

    let mut head = [0u8; 3];
    stream.read_exact(&mut head).await?;
    if head[0] != VERSION {
        return Err(SocksError::Version(head[0]));
    }
    if head[1] != 0 {
        return Err(SocksError::Refused(head[1]));
    }
    // The bound address is of no use to us, but it has to be consumed before
    // the tunnel proper begins.
    read_target(stream).await?;
    Ok(())
}

/// Ask the proxy where to send datagrams, and answer with that address.
///
/// `from` is where they will be sent from. Nothing is bound yet when this is
/// asked, so it is the unspecified address: the proxy is told "expect them
/// from wherever this connection came from", which is what every client does
/// and what xray accepts.
///
/// The association lives exactly as long as `stream`. SOCKS5 says so, and a
/// proxy drops it the moment that connection closes — so whoever asks has to
/// keep the stream, not just the address.
pub async fn associate_udp<S>(
    stream: &mut S,
    from: SocketAddr,
    login: Option<&Login>,
) -> Result<Target, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    greet(stream, login).await?;

    let mut request = vec![VERSION, CMD_UDP, RESERVED];
    write_target(&mut request, &Target::Socket(from))?;
    stream.write_all(&request).await?;
    stream.flush().await?;

    let mut head = [0u8; 3];
    stream.read_exact(&mut head).await?;
    if head[0] != VERSION {
        return Err(SocksError::Version(head[0]));
    }
    if head[1] != 0 {
        return Err(SocksError::Refused(head[1]));
    }
    // Here the bound address is the answer, not something to be consumed: it
    // is where the datagrams go.
    read_target(stream).await
}

/// Put the header in front of a datagram on its way out.
pub fn wrap_datagram(out: &mut Vec<u8>, to: &Target, payload: &[u8]) -> Result<(), SocksError> {
    out.clear();
    // Two reserved bytes, then the fragment number. bliss never fragments: a
    // datagram that does not fit is the sender's problem, as it is on any
    // other path, and reassembly is a buffer that grows.
    out.extend_from_slice(&[RESERVED, RESERVED, 0]);
    write_target(out, to)?;
    out.extend_from_slice(payload);
    Ok(())
}

/// Take one apart: who it came from, and where its payload starts.
///
/// Parsed with the same reader as every other address on this wire, over a
/// cursor that is never going to pend. Two parsers for one format is how the
/// two ends of it drift apart.
pub async fn unwrap_datagram(packet: &[u8]) -> Result<(Target, usize), SocksError> {
    if packet.len() < DATAGRAM_HEAD {
        return Err(SocksError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a datagram too short to hold its own header",
        )));
    }
    if packet[2] != 0 {
        // A fragment. Dropped rather than held: keeping them means a buffer
        // per sender with a timer on it, and nothing bliss talks to sends any.
        return Err(SocksError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a fragmented datagram, which bliss does not reassemble",
        )));
    }
    let mut cursor = std::io::Cursor::new(&packet[DATAGRAM_HEAD..]);
    let from = read_target(&mut cursor).await?;
    Ok((from, DATAGRAM_HEAD + cursor.position() as usize))
}

// --- server: accept a SOCKS5 client ---------------------------------------

/// Read the greeting and the CONNECT request. The reply is the caller's to
/// send, once it knows whether the outbound leg came up.
pub async fn accept<S>(stream: &mut S) -> Result<Target, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let version = stream.read_u8().await?;
    if version != VERSION {
        return Err(SocksError::Version(version));
    }
    let count = stream.read_u8().await? as usize;
    let mut methods = vec![0u8; count];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&NO_AUTH) {
        stream.write_all(&[VERSION, 0xFF]).await?;
        stream.flush().await?;
        return Err(SocksError::Auth(methods.first().copied().unwrap_or(0xFF)));
    }
    stream.write_all(&[VERSION, NO_AUTH]).await?;
    stream.flush().await?;

    let mut head = [0u8; 3];
    stream.read_exact(&mut head).await?;
    if head[0] != VERSION {
        return Err(SocksError::Version(head[0]));
    }
    if head[1] != CMD_CONNECT {
        // BIND and UDP ASSOCIATE are not served yet; the client is told so
        // rather than left waiting.
        reply(stream, 7).await?;
        return Err(SocksError::Command(head[1]));
    }
    read_target(stream).await
}

/// Tell the client how the outbound leg went. 0 is success.
pub async fn reply<S>(stream: &mut S, code: u8) -> Result<(), SocksError>
where
    S: AsyncWrite + Unpin,
{
    // The bound address is meaningless for CONNECT and every client ignores it.
    stream
        .write_all(&[VERSION, code, RESERVED, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await?;
    stream.flush().await?;
    Ok(())
}

/// Map a dial failure onto the closest SOCKS5 reply code.
pub fn reply_code_for(err: &std::io::Error) -> u8 {
    use std::io::ErrorKind::*;
    match err.kind() {
        ConnectionRefused => 5,
        HostUnreachable => 4,
        // A rule's VPN that is not connected: the network is not there.
        NetworkUnreachable | NotConnected => 3,
        TimedOut => 6,
        _ => 1,
    }
}

// --- shared wire format ---------------------------------------------------

pub(crate) fn write_target(out: &mut Vec<u8>, target: &Target) -> Result<(), SocksError> {
    match target {
        Target::Domain { host, port } => {
            if host.len() > MAX_DOMAIN {
                return Err(SocksError::DomainEncoding);
            }
            out.push(ATYP_DOMAIN);
            out.push(host.len() as u8);
            out.extend_from_slice(host.as_bytes());
            out.extend_from_slice(&port.to_be_bytes());
        }
        Target::Socket(SocketAddr::V4(a)) => {
            out.push(ATYP_IPV4);
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&a.port().to_be_bytes());
        }
        Target::Socket(SocketAddr::V6(a)) => {
            out.push(ATYP_IPV6);
            out.extend_from_slice(&a.ip().octets());
            out.extend_from_slice(&a.port().to_be_bytes());
        }
    }
    Ok(())
}

pub(crate) async fn read_target<S>(stream: &mut S) -> Result<Target, SocksError>
where
    S: AsyncRead + Unpin,
{
    let atyp = stream.read_u8().await?;
    let target = match atyp {
        ATYP_IPV4 => {
            let mut octets = [0u8; 4];
            stream.read_exact(&mut octets).await?;
            let port = stream.read_u16().await?;
            Target::Socket(SocketAddr::from((octets, port)))
        }
        ATYP_IPV6 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets).await?;
            let port = stream.read_u16().await?;
            Target::Socket(SocketAddr::from((octets, port)))
        }
        ATYP_DOMAIN => {
            let len = stream.read_u8().await? as usize;
            let mut host = vec![0u8; len];
            stream.read_exact(&mut host).await?;
            let port = stream.read_u16().await?;
            let host = String::from_utf8(host).map_err(|_| SocksError::DomainEncoding)?;
            Target::domain(host, port)
        }
        other => return Err(SocksError::AddressType(other)),
    };
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_requests_use_type_3_not_type_2() {
        let mut out = Vec::new();
        write_target(&mut out, &Target::domain("example.com", 443)).unwrap();
        // 3, then the length byte, then the name, then the port big-endian.
        assert_eq!(out[0], ATYP_DOMAIN);
        assert_eq!(out[1] as usize, "example.com".len());
        assert_eq!(&out[2..13], b"example.com");
        assert_eq!(&out[13..], &443u16.to_be_bytes());
    }

    #[test]
    fn ipv4_requests_are_packed_flat() {
        let mut out = Vec::new();
        write_target(&mut out, &Target::Socket("1.2.3.4:80".parse().unwrap())).unwrap();
        assert_eq!(out, vec![ATYP_IPV4, 1, 2, 3, 4, 0, 80]);
    }

    /// A datagram says where it is going in front of what it carries, and the
    /// header is three bytes plus an address — which is what `recv` moves the
    /// payload down over.
    #[tokio::test]
    async fn a_datagram_carries_its_address_in_front_of_its_payload() {
        let to = Target::Socket("203.0.113.9:53".parse().unwrap());
        let mut wire = Vec::new();
        wrap_datagram(&mut wire, &to, b"a query").unwrap();
        assert_eq!(&wire[..3], &[RESERVED, RESERVED, 0], "reserved, then FRAG");

        let (from, start) = unwrap_datagram(&wire).await.unwrap();
        assert_eq!(from, to);
        assert_eq!(&wire[start..], b"a query");
        // Three header bytes, the address type, four octets and a port.
        assert_eq!(start, 3 + 1 + 4 + 2);
    }

    /// A fragment is refused rather than passed on as if it were whole.
    /// Carried through, its payload would be half a packet with a header that
    /// looked fine.
    /// With a login, the greeting offers user and password alone and sends
    /// them as RFC 1929 lays out; a refusal is said as one.
    #[tokio::test]
    async fn a_login_is_offered_and_sent() {
        let login = Login {
            user: "u1".to_owned(),
            pass: "PPPP".to_owned(),
        };
        for accept in [true, false] {
            let (mut client, mut server) = tokio::io::duplex(1024);
            let proxy = tokio::spawn(async move {
                let mut greeting = [0u8; 3];
                server.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [VERSION, 1, USER_PASS]);
                server.write_all(&[VERSION, USER_PASS]).await.unwrap();
                let mut hello = [0u8; 9];
                server.read_exact(&mut hello).await.unwrap();
                assert_eq!(&hello, b"\x01\x02u1\x04PPPP");
                server
                    .write_all(&[1, if accept { 0 } else { 1 }])
                    .await
                    .unwrap();
                if accept {
                    let mut request = [0u8; 10];
                    server.read_exact(&mut request).await.unwrap();
                    server
                        .write_all(&[VERSION, 0, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
                        .await
                        .unwrap();
                }
            });
            let target = Target::Socket("203.0.113.7:443".parse().unwrap());
            let result = connect_through(&mut client, &target, Some(&login)).await;
            if accept {
                result.unwrap();
            } else {
                assert!(matches!(result, Err(SocksError::LoginRefused)));
            }
            proxy.await.unwrap();
        }
        assert!(!format!("{login:?}").contains("PPPP"));
    }

    #[tokio::test]
    async fn a_fragment_is_refused() {
        let mut wire = Vec::new();
        wrap_datagram(
            &mut wire,
            &Target::Socket("203.0.113.9:53".parse().unwrap()),
            b"half",
        )
        .unwrap();
        wire[2] = 1;
        assert!(unwrap_datagram(&wire).await.is_err());
    }

    #[tokio::test]
    async fn round_trips_a_domain_through_the_wire_format() {
        let mut wire = Vec::new();
        write_target(&mut wire, &Target::domain("a.example", 8443)).unwrap();
        let mut cursor = std::io::Cursor::new(wire);
        let back = read_target(&mut cursor).await.unwrap();
        assert_eq!(back, Target::domain("a.example", 8443));
    }
}
