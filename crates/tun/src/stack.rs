//! Packets in one side, connections out the other.
//!
//! The adapter hands over raw IP packets; a userspace stack turns them into the
//! same thing the SOCKS and HTTP inbounds hand over — a stream with a
//! destination — and from there the path is the one bliss already has: decide,
//! dial, relay, count. A TUN connection differs from a proxied one in exactly
//! two ways, and both are here: the destination is always an address, never a
//! name, and a connection bliss makes for itself has to be told to leave by
//! the real interface.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use bliss_core::{Decision, Route, Serving, Target, relay};
use ipstack::{
    IpStack, IpStackConfig, IpStackStream, IpStackTcpStream, IpStackUdpStream, TcpConfig,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpSocket;
use tokio_util::compat::FuturesAsyncReadCompatExt;
use wintun_bindings::AsyncSession;

use crate::device::MTU;
use crate::net::{self, Gateway};

/// How long a connection may sit idle before the stack forgets it.
///
/// ipstack's own default is a minute, which is shorter than a person is away
/// from an open terminal or a chat window; a forgotten connection then answers
/// nothing and the program holding it waits forever. Ten minutes is long
/// enough for that and short enough that a machine that slept does not keep a
/// table full of connections nobody is coming back to.
const IDLE: Duration = Duration::from_secs(600);

/// Everything a TUN connection needs on top of what every inbound needs.
pub struct Carrying {
    /// The dialer, the connection table and the rules: shared with the SOCKS
    /// and HTTP inbounds, so one table and one set of rules cover both ways in.
    pub serving: Arc<Serving>,
    /// Where a connection bliss dials itself has to leave by.
    pub gateway: Gateway,
}

/// Carry what the adapter gives us until the session ends.
pub async fn serve(session: AsyncSession, carrying: Arc<Carrying>) -> io::Result<()> {
    let mut config = IpStackConfig::default();
    config
        .mtu(MTU)
        .map_err(|e| io::Error::other(format!("the stack refused an MTU of {MTU}: {e}")))?;
    let mut tcp = TcpConfig::default();
    tcp.timeout = IDLE;
    config.with_tcp_config(tcp);

    // wintun's session speaks the futures traits and the stack wants tokio's:
    // the same bytes, two spellings of the same two traits.
    let mut stack = IpStack::new(config, session.compat());
    loop {
        let stream = match stack.accept().await {
            Ok(stream) => stream,
            // The stack only stops when the session does, which is bliss
            // shutting the adapter down.
            Err(e) => return Err(io::Error::other(format!("the stack stopped: {e}"))),
        };

        match stream {
            IpStackStream::Tcp(tcp) => {
                let carrying = Arc::clone(&carrying);
                tokio::spawn(async move {
                    let target = tcp.peer_addr();
                    if let Err(e) = carry(tcp, carrying).await {
                        tracing::debug!("tun: {target} — {e}");
                    }
                });
            }
            IpStackStream::Udp(udp) => {
                let carrying = Arc::clone(&carrying);
                tokio::spawn(async move {
                    let target = udp.peer_addr();
                    if let Err(e) = carry_datagrams(udp, carrying).await {
                        tracing::debug!("tun: udp {target} — {e}");
                    }
                });
            }
            IpStackStream::UnknownTransport(packet) => {
                tracing::trace!("tun: ip protocol {:?} dropped", packet.ip_protocol());
            }
            IpStackStream::UnknownNetwork(packet) => {
                tracing::trace!("tun: {} bytes of something else dropped", packet.len());
            }
        }
    }
}

/// One connection, from the packet that opened it to the last byte.
async fn carry(mut client: IpStackTcpStream, carrying: Arc<Carrying>) -> io::Result<()> {
    let Carrying { serving, gateway } = &*carrying;
    // Always an address here: a TUN sees what the program put in the packet,
    // and the name it looked up first is gone by then. Domain rules therefore
    // do not match a TUN connection until bliss answers the DNS query itself
    // and hands out an address it can recognise again.
    let target = Target::Socket(client.peer_addr());
    let process = bliss_core::name_for_port(client.local_addr().port());
    // One table decides this connection, whatever changes while it is open.
    let decision = decide(serving, &target, process.as_deref());

    if decision.route == Route::Blocked {
        tracing::debug!("tun: {target} blocked by {}", decision.rule);
        // There is no protocol here to refuse in, so the answer is the
        // connection closing at once: the program sees a closed connection
        // rather than the endless wait a dropped packet would give it.
        let _ = client.shutdown().await;
        let conn = serving
            .tracker
            .open(target, decision.route, decision.rule, process);
        serving.tracker.close(conn.id);
        return Ok(());
    }

    let upstream = match decision.route {
        Route::Direct => dial_direct(client.peer_addr(), *gateway).await,
        route => serving.dial_tunnelled(route, &target).await,
    };
    let upstream = match upstream {
        Ok(upstream) => upstream,
        Err(e) => {
            tracing::info!("tun: {target} refused — {e}");
            let _ = client.shutdown().await;
            return Err(e);
        }
    };

    let route = decision.route;
    let conn = serving
        .tracker
        .open(target, route, decision.rule, process.clone());
    let id = conn.id;
    tracing::debug!(
        "tun #{id} {} via {route} ({}) — {}",
        conn.target,
        process.as_deref().unwrap_or("?"),
        conn.rule
    );

    let result = relay(client, upstream, Arc::clone(&conn)).await;
    serving.tracker.close(id);
    result
}

/// One flow of datagrams: a call, a QUIC request, a name being looked up.
///
/// The shape is `carry`'s, with three differences that are all UDP's: the
/// program is found in the datagram table rather than the connection one, a
/// blocked flow is answered by dropping it because there is nothing to close,
/// and a direct flow is bound to the real interface for the same reason a
/// direct connection is.
async fn carry_datagrams(client: IpStackUdpStream, carrying: Arc<Carrying>) -> io::Result<()> {
    let Carrying { serving, gateway } = &*carrying;
    let target = Target::Socket(client.peer_addr());
    let process = bliss_core::name_for_datagram_port(client.local_addr().port());
    let decision = decide(serving, &target, process.as_deref());

    if decision.route == Route::Blocked {
        tracing::debug!("tun: udp {target} blocked by {}", decision.rule);
        // Nothing to refuse in: a datagram has no reply that means "no". The
        // flow is recorded so the connections screen can show what was
        // refused, and then dropped.
        let conn = serving
            .tracker
            .open(target, decision.route, decision.rule, process);
        serving.tracker.close(conn.id);
        return Ok(());
    }

    let upstream = match decision.route {
        Route::Direct => bind_direct(client.peer_addr(), *gateway).await,
        route => serving.bind_tunnelled(route, &target).await,
    };
    let upstream = match upstream {
        Ok(upstream) => upstream,
        Err(e) => {
            tracing::info!("tun: udp {target} refused — {e}");
            return Err(e);
        }
    };

    let route = decision.route;
    let conn = serving
        .tracker
        .open(target, route, decision.rule, process.clone());
    let id = conn.id;
    tracing::debug!(
        "tun #{id} udp {} via {route} ({}) — {}",
        conn.target,
        process.as_deref().unwrap_or("?"),
        conn.rule
    );

    let result = bliss_core::relay_datagrams(client, upstream, Arc::clone(&conn)).await;
    serving.tracker.close(id);
    result
}

/// Programs whose own connections carry a VPN beside the tunnel.
///
/// OpenVPN's connection to its server is another program's socket, so the
/// adapter catches it like any other. Decided by the rules, it would usually
/// go through the proxy server — a work network reached by way of a server
/// abroad, if the work server takes connections from there at all.
const VPN_PROGRAMS: [&str; 1] = ["openvpn.exe"];

/// The rules' answer, except for the VPN's own connections, which leave the
/// ordinary way whatever the rules say.
fn decide(serving: &Serving, target: &Target, process: Option<&str>) -> Decision {
    if process.is_some_and(|p| VPN_PROGRAMS.iter().any(|v| p.eq_ignore_ascii_case(v))) {
        return Decision {
            route: Route::Direct,
            rule: "OpenVPN, мимо туннеля".to_owned(),
        };
    }
    serving.router().decide(target, process)
}

/// A flow that goes out the ordinary way, past the tunnel.
struct Direct {
    socket: tokio::net::UdpSocket,
}

#[async_trait::async_trait]
impl bliss_core::Datagram for Direct {
    async fn send(&self, payload: &[u8]) -> io::Result<()> {
        self.socket.send(payload).await?;
        Ok(())
    }

    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.socket.recv(buf).await
    }
}

/// Bind one, leaving by the interface the machine used before the tunnel.
async fn bind_direct(
    address: std::net::SocketAddr,
    gateway: Gateway,
) -> io::Result<bliss_core::BoxedDatagram> {
    let socket = std::net::UdpSocket::bind(std::net::SocketAddr::from(([0, 0, 0, 0], 0)))?;
    socket.set_nonblocking(true)?;
    net::outside(&socket, gateway)?;
    let socket = tokio::net::UdpSocket::from_std(socket)?;
    // Connected, so the two halves need no address and anything from
    // elsewhere is dropped before bliss sees it.
    socket.connect(address).await?;
    Ok(Box::new(Direct { socket }))
}

/// Dial past the tunnel, for a connection the rules sent direct.
///
/// The same dial the SOCKS inbound makes, with one addition: the socket is
/// told which interface to leave by before it is connected, because the
/// routing table now sends everything into the adapter — including this.
async fn dial_direct(
    address: std::net::SocketAddr,
    gateway: Gateway,
) -> io::Result<bliss_core::BoxedStream> {
    let dial = async {
        let socket = match address {
            std::net::SocketAddr::V4(_) => TcpSocket::new_v4()?,
            std::net::SocketAddr::V6(_) => TcpSocket::new_v6()?,
        };
        net::outside(&socket, gateway)?;
        let stream = socket.connect(address).await?;
        stream.set_nodelay(true)?;
        Ok::<_, io::Error>(stream)
    };

    match tokio::time::timeout(bliss_core::DIAL_TIMEOUT, dial).await {
        Ok(Ok(stream)) => Ok(Box::new(stream) as bliss_core::BoxedStream),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{address} did not answer directly within {:?}",
                bliss_core::DIAL_TIMEOUT
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use bliss_core::{EngineInfo, Router, SocksOutbound, Tracker};

    use super::*;

    /// OpenVPN's own connection to its server leaves the ordinary way, whatever
    /// the rules say: through the tunnel it would reach the work network by way
    /// of the proxy server, if at all.
    #[test]
    fn the_vpns_own_connection_goes_around_the_tunnel() {
        let serving = Serving::new(
            Arc::new(SocksOutbound::new(
                "127.0.0.1:1".parse().expect("an address"),
                EngineInfo {
                    kind: "test",
                    version: String::new(),
                    external: true,
                },
            )),
            Arc::new(Tracker::new()),
            Arc::new(Router::proxy_everything()),
        );
        let target = Target::Socket("203.0.113.9:1194".parse().expect("an address"));
        assert_eq!(
            decide(&serving, &target, Some("OpenVPN.exe")).route,
            Route::Direct
        );
        assert_eq!(
            decide(&serving, &target, Some("chrome.exe")).route,
            Route::Proxy
        );
        assert_eq!(decide(&serving, &target, None).route, Route::Proxy);
    }
}
