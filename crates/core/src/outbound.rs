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

#[async_trait]
pub trait Outbound: Send + Sync {
    async fn connect_tcp(&self, target: &Target) -> std::io::Result<BoxedStream>;
    fn describe(&self) -> EngineInfo;
}

/// How long to wait for the proxy to say the far end is up.
///
/// The dial itself is to loopback and is instant; what this bounds is the
/// engine's own connection to the server, which can hang indefinitely when a
/// REALITY handshake gets no answer. Measured against a real server that
/// accepted roughly one connection in four, an unbounded wait is the difference
/// between a page that loads a second late and one that never loads.
const DIAL_TIMEOUT: Duration = Duration::from_secs(6);

/// Total attempts, including the first.
const DIAL_ATTEMPTS: usize = 3;

/// An outbound that reaches the world through a local SOCKS5 proxy.
pub struct SocksOutbound {
    addr: SocketAddr,
    info: EngineInfo,
}

impl SocksOutbound {
    pub fn new(addr: SocketAddr, info: EngineInfo) -> Self {
        Self { addr, info }
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

        match tokio::time::timeout(DIAL_TIMEOUT, socks5::connect_through(&mut stream, target)).await
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
