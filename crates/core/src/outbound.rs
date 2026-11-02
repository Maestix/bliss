//! The seam between bliss and whatever actually carries the bytes.
//!
//! This trait is the entire contract. Stage 1 satisfies it by dialing a local
//! xray-core over SOCKS5; stage 3 will satisfy it with our own VLESS + REALITY
//! stack. Everything else in bliss — inbounds, routing, the connection table,
//! the UI — sits above this line and does not change when the engine does.

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::socks5::{self, SocksError};
use crate::target::Target;

/// Anything that can carry a tunnelled connection.
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

pub type BoxedStream = Box<dyn AsyncStream>;

/// What the UI should say about the engine in use.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub kind: &'static str,
    pub version: String,
    /// True while the engine is a separate process we do not control the source
    /// of. The point of stage 3 is to make this false.
    pub external: bool,
}

/// A channel for datagrams to one destination.
///
/// One of these is one flow: the pair of ports a program is talking through.
/// Both halves take `&self`, because the two directions run at once and
/// neither waits for the other — which is most of the point of UDP.
#[async_trait]
pub trait Datagram: Send + Sync {
    /// Send one datagram to the destination this was bound for.
    async fn send(&self, payload: &[u8]) -> std::io::Result<()>;
    /// Wait for the next one back, into `buf`, and answer how much it holds.
    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize>;
}

pub type BoxedDatagram = Box<dyn Datagram>;

#[async_trait]
pub trait Outbound: Send + Sync {
    async fn connect_tcp(&self, target: &Target) -> std::io::Result<BoxedStream>;
    /// A channel for datagrams to `target`.
    ///
    /// The machine's proxy settings never carried UDP at all, which is why a
    /// call in Discord went straight out and was cut; this is what the TUN
    /// adapter hands its datagrams to.
    async fn bind_udp(&self, target: &Target) -> std::io::Result<BoxedDatagram>;
    fn describe(&self) -> EngineInfo;
}

/// How long to wait for the proxy to say the far end is up.
///
/// The dial itself is to loopback and is instant; what this bounds is the
/// engine's own connection to the server, which can hang indefinitely when a
/// REALITY handshake gets no answer. Measured against a real server that
/// accepted roughly one connection in four, an unbounded wait is the difference
/// between a page that loads a second late and one that never loads.
///
/// Public because the TUN inbound dials direct connections itself — it has to
/// name the interface they leave by — and two copies of this number would
/// drift.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(6);

/// Total attempts, including the first.
const DIAL_ATTEMPTS: usize = 3;

/// An outbound that reaches the world through a local SOCKS5 proxy.
pub struct SocksOutbound {
    addr: SocketAddr,
    info: EngineInfo,
    login: Option<socks5::Login>,
}

impl SocksOutbound {
    pub fn new(addr: SocketAddr, info: EngineInfo) -> Self {
        Self {
            addr,
            info,
            login: None,
        }
    }

    /// Dial with a user name and a password: an engine whose port others on
    /// the machine can reach — every app on a phone can reach loopback —
    /// serves only who has them.
    pub fn with_login(mut self, login: socks5::Login) -> Self {
        self.login = Some(login);
        self
    }
}

#[async_trait]
impl Outbound for SocksOutbound {
    async fn connect_tcp(&self, target: &Target) -> std::io::Result<BoxedStream> {
        let mut last: Option<std::io::Error> = None;

        for attempt in 1..=DIAL_ATTEMPTS {
            match self.dial_once(target).await {
                Ok(stream) => {
                    if attempt > 1 {
                        tracing::debug!("conn: {target} came up on attempt {attempt}");
                    }
                    return Ok(stream);
                }
                Err(e) => {
                    // A refusal is an answer; only silence is worth retrying.
                    // Retrying a "connection refused" just delays the error the
                    // client already needs to see.
                    let retryable = e.kind() == std::io::ErrorKind::TimedOut;
                    tracing::debug!("conn: {target} attempt {attempt} failed: {e}");
                    last = Some(e);
                    if !retryable {
                        break;
                    }
                }
            }
        }

        Err(last.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "no attempt was made")
        }))
    }

    async fn bind_udp(&self, target: &Target) -> std::io::Result<BoxedDatagram> {
        // The association is asked for over a TCP connection and lasts exactly
        // as long as that connection: the proxy drops it when the connection
        // closes, so the channel keeps it open for its whole life.
        let mut control = TcpStream::connect(self.addr).await?;
        control.set_nodelay(true)?;

        // Nothing is bound yet, so there is no address to promise; every
        // client says this and the engine takes datagrams from wherever they
        // arrive.
        let unspecified = SocketAddr::from(([0, 0, 0, 0], 0));
        let bound = match tokio::time::timeout(
            DIAL_TIMEOUT,
            socks5::associate_udp(&mut control, unspecified, self.login.as_ref()),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("the engine did not answer within {DIAL_TIMEOUT:?}"),
                ));
            }
        };

        let relay = self.datagrams_go_to(bound)?;
        let socket = tokio::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))).await?;
        // Connected, so the kernel drops anything that is not from the engine
        // before bliss ever sees it.
        socket.connect(relay).await?;

        Ok(Box::new(SocksDatagram {
            _control: control,
            socket,
            to: target.clone(),
        }))
    }

    fn describe(&self) -> EngineInfo {
        self.info.clone()
    }
}

impl SocksOutbound {
    async fn dial_once(&self, target: &Target) -> std::io::Result<BoxedStream> {
        let mut stream = TcpStream::connect(self.addr).await?;
        // Interactive traffic dominates a proxy: latency matters more than the
        // few bytes Nagle would save.
        stream.set_nodelay(true)?;

        match tokio::time::timeout(
            DIAL_TIMEOUT,
            socks5::connect_through(&mut stream, target, self.login.as_ref()),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("the engine did not answer within {DIAL_TIMEOUT:?}"),
                ));
            }
        }
        Ok(Box::new(stream))
    }
}

impl SocksOutbound {
    /// Where the engine wants this flow's datagrams.
    ///
    /// A proxy commonly answers with the unspecified address, meaning "the one
    /// you are already talking to me on": sending to 0.0.0.0 would go nowhere,
    /// so its port is paired with the address the association was asked over.
    fn datagrams_go_to(&self, bound: Target) -> std::io::Result<SocketAddr> {
        match bound {
            Target::Socket(addr) if addr.ip().is_unspecified() => {
                Ok(SocketAddr::new(self.addr.ip(), addr.port()))
            }
            Target::Socket(addr) => Ok(addr),
            // Never seen from xray, which answers with an address. Resolving a
            // name here would be the one place bliss looks one up for
            // something it is about to tunnel.
            Target::Domain { host: _, port: _ } => Err(std::io::Error::other(
                "the engine answered the association with a name rather than an address",
            )),
        }
    }
}

/// One UDP flow through the engine.
struct SocksDatagram {
    /// Held, not used: the association dies with it.
    _control: TcpStream,
    socket: tokio::net::UdpSocket,
    to: Target,
}

#[async_trait]
impl Datagram for SocksDatagram {
    async fn send(&self, payload: &[u8]) -> std::io::Result<()> {
        // One allocation per datagram. The relay's buffers have the same
        // shape of problem and the same answer — a pool — and neither has
        // shown up in a measurement yet.
        let mut framed = Vec::with_capacity(payload.len() + 32);
        socks5::wrap_datagram(&mut framed, &self.to, payload)?;
        self.socket.send(&framed).await?;
        Ok(())
    }

    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let filled = self.socket.recv(buf).await?;
            // The header is in front of the payload, so the payload is moved
            // down over it rather than copied into a second buffer.
            match socks5::unwrap_datagram(&buf[..filled]).await {
                Ok((_from, start)) => {
                    buf.copy_within(start..filled, 0);
                    return Ok(filled - start);
                }
                // One malformed datagram is not the end of the flow: a
                // fragment, or something else answering on the port.
                Err(e) => tracing::debug!("udp: a datagram was dropped — {e}"),
            }
        }
    }
}

/// Keep the kind of failure, not just the text: the inbound turns it back into
/// a SOCKS reply code, and "refused" must not become "general failure".
impl From<SocksError> for std::io::Error {
    fn from(err: SocksError) -> Self {
        use std::io::ErrorKind;
        let kind = match &err {
            SocksError::Io(e) => e.kind(),
            SocksError::Refused(3) => ErrorKind::NetworkUnreachable,
            SocksError::Refused(4) => ErrorKind::HostUnreachable,
            SocksError::Refused(5) => ErrorKind::ConnectionRefused,
            SocksError::Refused(6) => ErrorKind::TimedOut,
            _ => ErrorKind::Other,
        };
        std::io::Error::new(kind, err)
    }
}
