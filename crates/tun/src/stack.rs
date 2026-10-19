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
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bliss_core::{Decision, Route, Serving, Target, relay};
use ipstack::{
    IpStack, IpStackConfig, IpStackStream, IpStackTcpStream, IpStackUdpStream, TcpConfig,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpSocket;

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

/// The device as the stack reads it, with an end the stack cannot miss.
///
/// A wintun session that has been shut down answers every read at once, with
/// nothing — `Ok(0)`, for ever. ipstack takes each answer for a packet, cannot
/// hand it to anyone once bliss has stopped taking streams, logs a warning
/// and reads again, without once yielding: so the abort meant to end its task
/// never lands, and the loop runs until the process ends. Switching TUN off
/// shuts the session down before the task that takes the streams has gone,
/// and a friend's bliss wrote 305 GB of "process_device_read error: Send Error
/// channel closed" into bliss.log that way, at some 600 MB/s, until the disk
/// was nearly full. Here the first empty read or failure is the end: said
/// once, and every read after it waits for good, which gives the stack's task
/// the yield its abort needs (`an_ended_device_is_read_once`).
struct Ended<D> {
    device: D,
    ended: bool,
}

impl<D> Ended<D> {
    fn new(device: D) -> Self {
        Self {
            device,
            ended: false,
        }
    }
}

impl<D: AsyncRead + Unpin> AsyncRead for Ended<D> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.ended {
            return Poll::Pending;
        }
        let before = buf.filled().len();
        let room = buf.remaining();
        match Pin::new(&mut self.device).poll_read(cx, buf) {
            Poll::Ready(Ok(())) if room > 0 && buf.filled().len() == before => {
                self.ended = true;
                tracing::info!("tun: the adapter has no more packets");
                Poll::Pending
            }
            Poll::Ready(Err(e)) => {
                self.ended = true;
                tracing::warn!("tun: the adapter could not be read: {e}");
                Poll::Pending
            }
            other => other,
        }
    }
}

impl<D: AsyncWrite + Unpin> AsyncWrite for Ended<D> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.device).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.device).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.device).poll_shutdown(cx)
    }
}

/// Carry what the adapter gives us until its packets end: wintun's session on
/// Windows, the device itself on Linux — a read is a packet either way.
pub async fn serve<D>(packets: D, carrying: Arc<Carrying>) -> io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut config = IpStackConfig::default();
    config
        .mtu(MTU)
        .map_err(|e| io::Error::other(format!("the stack refused an MTU of {MTU}: {e}")))?;
    let mut tcp = TcpConfig::default();
    tcp.timeout = IDLE;
    config.with_tcp_config(tcp);

    let mut stack = IpStack::new(config, Ended::new(packets));
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
    let process = bliss_core::name_for_connection(client.local_addr(), client.peer_addr());
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
const VPN_PROGRAMS: [&str; 2] = ["openvpn.exe", "openvpn"];

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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bliss_core::{EngineInfo, Router, SocksOutbound, Tracker};

    use super::*;

    /// A device that has ended as a shut-down wintun session has: every read
    /// answered at once with nothing.
    struct ShutDown {
        reads: Arc<AtomicUsize>,
    }

    impl AsyncRead for ShutDown {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for ShutDown {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// The stack reads an ended device once, not for ever: given the
    /// device bare, it read it millions of times a second and never let go
    /// of its thread. `shutdown_background`, because a runtime with that
    /// loop on a thread would never finish shutting down.
    #[test]
    fn an_ended_device_is_read_once() {
        let reads = Arc::new(AtomicUsize::new(0));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let device = ShutDown {
            reads: Arc::clone(&reads),
        };
        runtime.block_on(async {
            let stack = IpStack::new(IpStackConfig::default(), Ended::new(device));
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(stack);
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let read = reads.load(Ordering::Relaxed);
        runtime.shutdown_background();
        assert_eq!(read, 1, "read {read} times");
    }

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
