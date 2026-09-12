//! The local listener: what a browser or a shell is pointed at.
//!
//! One port serves both SOCKS5 and HTTP CONNECT, decided by peeking at the first
//! byte — 0x05 can only be a SOCKS version, and every HTTP method starts with a
//! letter. xray-core calls this a `mixed` inbound and it is the right default:
//! two ports for two protocols is a setting nobody wants to think about.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};

use crate::http;
use crate::outbound::Outbound;
use crate::relay::relay;
use crate::socks5;
use crate::target::Target;
use crate::tracker::{Route, Tracker};

/// A listening address and what it is called in the logs.
#[derive(Debug, Clone)]
pub struct Inbound {
    pub addr: SocketAddr,
    pub tag: &'static str,
}

/// A port we already hold.
///
/// Binding is separate from serving on purpose. A port that is already taken —
/// by another client, or by a previous copy of us — is the most common way a
/// start fails, and it has to fail *before* anything reports success. Bound
/// inside the accept loop, the error arrived asynchronously and the window went
/// on showing a green ring over a proxy that was not listening.
pub struct Bound {
    listener: TcpListener,
    inbound: Inbound,
}

impl Inbound {
    pub async fn bind(self) -> std::io::Result<Bound> {
        let listener = TcpListener::bind(self.addr).await?;
        Ok(Bound {
            listener,
            inbound: self,
        })
    }
}

/// Accept connections until the future is dropped.
pub async fn serve(
    bound: Bound,
    outbound: Arc<dyn Outbound>,
    tracker: Arc<Tracker>,
) -> std::io::Result<()> {
    let Bound { listener, inbound } = bound;
    tracing::info!("{} listening on {}", inbound.tag, inbound.addr);

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            // One failed accept is not a reason to stop serving: a client can
            // vanish between the SYN and our accept, and that is routine.
            Err(e) => {
                tracing::warn!("{}: accept failed: {e}", inbound.tag);
                continue;
            }
        };

        let outbound = Arc::clone(&outbound);
        let tracker = Arc::clone(&tracker);
        tokio::spawn(async move {
            if let Err(e) = handle(stream, peer, outbound, tracker).await {
                tracing::debug!("conn: {peer} — {e}");
            }
        });
    }
}

async fn handle(
    stream: TcpStream,
    peer: SocketAddr,
    outbound: Arc<dyn Outbound>,
    tracker: Arc<Tracker>,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;

    let mut first = [0u8; 1];
    // peek, not read: the protocol handlers want the byte too.
    if stream.peek(&mut first).await? == 0 {
        return Ok(());
    }
    let socks = first[0] == 0x05;

    // Both paths get the same wrapper so the relay sees one type, and so that
    // anything the HTTP head reader buffered travels with the stream.
    let mut stream = BufReader::new(stream);

    let target = if socks {
        socks5::accept(&mut stream).await?
    } else {
        match http::accept(&mut stream).await {
            Ok(target) => target,
            Err(http::HttpError::Method(m)) => {
                http::refuse(
                    &mut stream,
                    "501 Not Implemented",
                    &format!("{m} is not served; bliss proxies CONNECT only"),
                )
                .await?;
                return Ok(());
            }
            Err(e) => return Err(std::io::Error::other(e)),
        }
    };

    dial_and_relay(stream, socks, target, peer, outbound, tracker).await
}

async fn dial_and_relay(
    mut client: BufReader<TcpStream>,
    socks: bool,
    target: Target,
    peer: SocketAddr,
    outbound: Arc<dyn Outbound>,
    tracker: Arc<Tracker>,
) -> std::io::Result<()> {
    // Routing lands in 1.5. Until then everything is proxied, and the rule is
    // named honestly rather than left blank — the UI promises to always say
    // which rule chose the route.
    let route = Route::Proxy;
    let rule = "default";

    let upstream = match outbound.connect_tcp(&target).await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::info!("conn: {target} refused — {e}");
            if socks {
                socks5::reply(&mut client, socks5::reply_code_for(&e)).await?;
            } else {
                http::refuse(&mut client, "502 Bad Gateway", &e.to_string()).await?;
            }
            return Err(e);
        }
    };

    if socks {
        socks5::reply(&mut client, 0).await?;
    } else {
        http::established(&mut client).await?;
    }

    let process = crate::process::name_for_port(peer.port());
    let conn = tracker.open(target, route, rule, process);
    let id = conn.id;
    tracing::debug!(
        "conn #{id} {} via {route} ({})",
        conn.target,
        conn.process.as_deref().unwrap_or("?")
    );

    let result = relay(client, upstream, Arc::clone(&conn)).await;
    tracker.close(id);
    result
}
