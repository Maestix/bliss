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
use std::net::{IpAddr, SocketAddr};
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
use crate::names::{Addresses, Answer, FakeNames};
use crate::net::{self, Gateway};
use crate::sniff::{self, Sniffed};

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
    /// Where bliss is the system's DNS server (Android's VPN service names
    /// one inside the tunnel): that address, and the names it handed out.
    /// Connections to those addresses are then decided by name.
    pub names: Option<(SocketAddr, Arc<FakeNames>)>,
    /// Who holds a connection, where the system has to be asked rather than
    /// a table read (Android: `getConnectionOwnerUid`). Without it, the
    /// machine's own tables (`bliss_core::name_for_connection`).
    pub owner: Option<Owner>,
    /// Sites' real addresses, learnt through the tunnel where `names` hands
    /// out the pool's: what `geoip:` and subnet rules decide a site by.
    pub addresses: Arc<Addresses>,
}

/// How long a connection waits to learn its program from the system.
const OWNER_WAIT: Duration = Duration::from_millis(500);

/// Which program holds a connection: TCP or not, its local end and its far one.
pub type Owner = Arc<dyn Fn(bool, SocketAddr, SocketAddr) -> Option<String> + Send + Sync>;

impl Carrying {
    pub fn new(serving: Arc<Serving>, gateway: Gateway) -> Self {
        Self {
            serving,
            gateway,
            names: None,
            owner: None,
            addresses: Arc::new(Addresses::new()),
        }
    }

    /// The program behind a connection, asked off the runtime's threads,
    /// since the system's answer is a call into it — and not waited for past
    /// `OWNER_WAIT`: a connection without its program is decided by the
    /// other rules, one that waits for a call that never returns is no
    /// connection at all (0.2.0 on Android, where every one hung).
    async fn program(&self, tcp: bool, local: SocketAddr, remote: SocketAddr) -> Option<String> {
        match &self.owner {
            Some(owner) => ask_owner(owner, tcp, local, remote, OWNER_WAIT).await,
            None if tcp => bliss_core::name_for_connection(local, remote),
            None => bliss_core::name_for_datagram_port(local.port()),
        }
    }

    /// The destination as the rules see it: by name when the address is one
    /// bliss handed out for a name. `Err` for such an address whose name has
    /// been forgotten: nothing can be dialled for it.
    fn target(&self, peer: SocketAddr) -> Result<Target, ()> {
        let Some((_, names)) = &self.names else {
            return Ok(Target::Socket(peer));
        };
        match peer.ip() {
            IpAddr::V4(ip) if FakeNames::is_fake(ip) => match names.name_of(ip) {
                Some(host) => Ok(Target::Domain {
                    host,
                    port: peer.port(),
                }),
                None => Err(()),
            },
            _ => Ok(Target::Socket(peer)),
        }
    }
}

async fn ask_owner(
    owner: &Owner,
    tcp: bool,
    local: SocketAddr,
    remote: SocketAddr,
    wait: Duration,
) -> Option<String> {
    let owner = Arc::clone(owner);
    let asked = tokio::task::spawn_blocking(move || owner(tcp, local, remote));
    match tokio::time::timeout(wait, asked).await {
        Ok(Ok(name)) => name,
        Ok(Err(_)) => None,
        Err(_) => {
            tracing::debug!("tun: nobody said whose {local} → {remote} is; deciding without");
            None
        }
    }
}

impl Carrying {
    /// The rules' answer, by the site's real address too where a rule that
    /// looks at addresses comes before the decision.
    async fn decide(
        &self,
        target: &Target,
        known: Option<IpAddr>,
        process: Option<&str>,
        udp: bool,
    ) -> Decision {
        let router = self.serving.router();
        if is_vpn_program(process) {
            return decide(&self.serving, target, process);
        }
        let known = match known {
            Some(ip) => Some(ip),
            None => match target {
                Target::Domain { host, .. }
                    if self.names.is_some() && router.wants_address(target, process, udp) =>
                {
                    self.real_address(host).await.map(IpAddr::V4)
                }
                _ => None,
            },
        };
        router.decide_flow(target, known, process, udp)
    }

    /// Carry a connection until it ends — or, where bliss names sites (the
    /// phone), until the rules change so that it would go another way: then
    /// it is ended, and the program's next connection takes the new way. An
    /// app keeps its connections for minutes, and a rule that did nothing
    /// until then was taken for one that does not work (the user's Reddit
    /// rule, 1 November).
    async fn until_rerouted(
        &self,
        relayed: impl std::future::Future<Output = io::Result<()>>,
        conn: &bliss_core::Connection,
        seen: Option<IpAddr>,
        udp: bool,
    ) -> Carried {
        let (target, id) = (&conn.target, conn.id);
        if self.names.is_none() {
            return Carried::Ended(relayed.await);
        }
        let mut changes = self.serving.router_changes();
        tokio::pin!(relayed);
        loop {
            tokio::select! {
                result = &mut relayed => return Carried::Ended(result),
                changed = changes.changed() => {
                    if changed.is_err() {
                        return Carried::Ended(relayed.await);
                    }
                    if self.reconsidered(target, seen, conn.process.as_deref(), udp) != conn.route {
                        tracing::info!("tun #{id} {target}: the rules send it elsewhere now; closed");
                        return Carried::Rerouted;
                    }
                }
            }
        }
    }
}

/// How a connection's carrying ended.
enum Carried {
    Ended(io::Result<()>),
    /// The rules changed under it: the program is to be told it is over.
    Rerouted,
}

impl Carrying {
    /// The route the table in force now gives a connection already open,
    /// without asking the network anything.
    fn reconsidered(
        &self,
        target: &Target,
        seen: Option<IpAddr>,
        process: Option<&str>,
        udp: bool,
    ) -> Route {
        if is_vpn_program(process) {
            return Route::Direct;
        }
        let known = seen.or_else(|| self.known_address(target));
        self.serving
            .router()
            .decide_flow(target, known, process, udp)
            .route
    }

    /// A site's address as already learnt, without asking anything.
    fn known_address(&self, target: &Target) -> Option<IpAddr> {
        match target {
            Target::Domain { host, .. } => self.addresses.get(host).flatten().map(IpAddr::V4),
            Target::Socket(_) => None,
        }
    }

    /// A site's IPv4 address, asked of 1.1.1.1 through the tunnel — never of
    /// the phone's network, which would tell it every site (AGENTS.md §5) —
    /// and kept for ten minutes.
    async fn real_address(&self, name: &str) -> Option<std::net::Ipv4Addr> {
        if let Some(known) = self.addresses.get(name) {
            return known;
        }
        static IDS: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0x5a17);
        let id = IDS.fetch_add(0x9e37, std::sync::atomic::Ordering::Relaxed);
        let query = crate::names::query_a(id, name);
        // Over UDP first, the cheap way; over TCP when the server carries no
        // UDP or loses it, which a server with Vision does for some ports.
        let over_udp = async {
            let upstream = self
                .serving
                .bind_tunnelled(Route::Proxy, &Target::Socket(UPSTREAM_DNS))
                .await?;
            upstream.send(&query).await?;
            let mut back = vec![0u8; 1500];
            loop {
                let got = upstream.recv(&mut back).await?;
                // Only the answer to this question: an id that differs is
                // something else, and the wait goes on.
                if got >= 2 && back[..2] == id.to_be_bytes() {
                    return io::Result::Ok(crate::names::first_a(&back[..got], id));
                }
            }
        };
        let over_tcp = async {
            use tokio::io::AsyncReadExt;
            let mut upstream = self
                .serving
                .dial_tunnelled(Route::Proxy, &Target::Socket(UPSTREAM_DNS))
                .await?;
            let mut framed = (query.len() as u16).to_be_bytes().to_vec();
            framed.extend_from_slice(&query);
            upstream.write_all(&framed).await?;
            let mut len = [0u8; 2];
            upstream.read_exact(&mut len).await?;
            let mut back = vec![0u8; usize::from(u16::from_be_bytes(len))];
            upstream.read_exact(&mut back).await?;
            io::Result::Ok(crate::names::first_a(&back, id))
        };
        let found = match tokio::time::timeout(ADDRESS_WAIT, over_udp).await {
            Ok(Ok(ip)) => Some(ip),
            _ => tokio::time::timeout(ADDRESS_WAIT, over_tcp)
                .await
                .ok()
                .and_then(Result::ok),
        };
        match found {
            Some(ip) => {
                self.addresses.put(name, ip);
                ip
            }
            None => {
                tracing::debug!("tun: no address learnt for {name}; deciding by its name");
                self.addresses.failed(name);
                None
            }
        }
    }
}

/// A name's address on the real network, for a direct route: resolved here,
/// as direct routes always are (AGENTS.md §5).
async fn resolve(target: &Target) -> io::Result<SocketAddr> {
    match target {
        Target::Socket(addr) => Ok(*addr),
        Target::Domain { host, port } => tokio::net::lookup_host((host.as_str(), *port))
            .await?
            .find(SocketAddr::is_ipv4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{host}: no address"))),
    }
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
    let Carrying {
        serving, gateway, ..
    } = &*carrying;
    // An address, unless bliss answered the lookup itself (`names`): a TUN
    // sees what the program put in the packet, and the name it looked up
    // first is gone by then.
    let Ok(mut target) = carrying.target(client.peer_addr()) else {
        tracing::debug!("tun: {} is a name no longer known", client.peer_addr());
        let _ = client.shutdown().await;
        return Ok(());
    };
    // A bare address where bliss names sites: the program found it some
    // other way (a «Частный DNS», its own DNS over HTTPS, an address it
    // kept), and its first bytes are asked for the name instead.
    let mut first = Vec::new();
    let mut seen = None;
    if carrying.names.is_some()
        && let Target::Socket(addr) = target
        && SNIFFED_PORTS.contains(&addr.port())
    {
        first = read_first(&mut client).await;
        if let Sniffed::Name(host) = sniff::name(&first) {
            seen = Some(addr.ip());
            target = Target::Domain {
                host,
                port: addr.port(),
            };
        }
    }
    let process = carrying
        .program(true, client.local_addr(), client.peer_addr())
        .await;
    // One table decides this connection, whatever changes while it is open.
    let decision = carrying
        .decide(&target, seen, process.as_deref(), false)
        .await;

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

    let upstream = match (decision.route, seen) {
        // Straight to the address the program chose, which a name read out
        // of its connection only explains.
        (Route::Direct, Some(ip)) => {
            dial_direct(SocketAddr::new(ip, target.port()), *gateway).await
        }
        (Route::Direct, None) => match resolve(&target).await {
            Ok(addr) => dial_direct(addr, *gateway).await,
            Err(e) => Err(e),
        },
        // Through the tunnel to the very address too: the name only explains
        // it, and a name read out of a ClientHello can be an outer one (ECH)
        // that resolves elsewhere.
        (route, Some(ip)) => {
            let at = Target::Socket(SocketAddr::new(ip, target.port()));
            serving.dial_tunnelled(route, &at).await
        }
        (route, None) => serving.dial_tunnelled(route, &target).await,
    };
    let mut upstream = match upstream {
        Ok(upstream) => upstream,
        Err(e) => {
            tracing::info!("tun: {target} refused — {e}");
            let _ = client.shutdown().await;
            return Err(e);
        }
    };
    // What was read to find the name goes first, as it came.
    if !first.is_empty()
        && let Err(e) = upstream.write_all(&first).await
    {
        let _ = client.shutdown().await;
        return Err(e);
    }

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

    conn.add_up(first.len() as u64);

    let relayed = relay(&mut client, upstream, Arc::clone(&conn));
    let carried = carrying.until_rerouted(relayed, &conn, seen, false).await;
    serving.tracker.close(id);
    match carried {
        Carried::Ended(result) => result,
        // Said to the program: a stream merely dropped tells it nothing, and
        // it would wait on the old way for good.
        Carried::Rerouted => {
            let _ = client.shutdown().await;
            Ok(())
        }
    }
}

/// Ports whose clients speak first, with a name in what they say: HTTPS and
/// HTTP and their usual others. A server that speaks first (SSH, mail) is
/// never waited on.
const SNIFFED_PORTS: [u16; 4] = [443, 80, 8443, 8080];
/// How long a connection's first bytes are waited for.
const SNIFF_WAIT: Duration = Duration::from_millis(400);

/// The first bytes of a connection, until they name a site, say they will
/// not, reach `MAX_SNIFF`, or `SNIFF_WAIT` passes.
async fn read_first(client: &mut IpStackTcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut first = Vec::new();
    let deadline = tokio::time::Instant::now() + SNIFF_WAIT;
    let mut buf = vec![0u8; 4096];
    while first.len() < sniff::MAX_SNIFF {
        let n = match tokio::time::timeout_at(deadline, client.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => break,
        };
        first.extend_from_slice(&buf[..n]);
        if sniff::name(&first) != Sniffed::More {
            break;
        }
    }
    first
}

/// One flow of datagrams: a call, a QUIC request, a name being looked up.
///
/// The shape is `carry`'s, with three differences that are all UDP's: the
/// program is found in the datagram table rather than the connection one, a
/// blocked flow is answered by dropping it because there is nothing to close,
/// and a direct flow is bound to the real interface for the same reason a
/// direct connection is.
async fn carry_datagrams(client: IpStackUdpStream, carrying: Arc<Carrying>) -> io::Result<()> {
    if let Some((dns, names)) = &carrying.names
        && client.peer_addr() == *dns
    {
        return answer_lookups(client, Arc::clone(names), carrying).await;
    }
    let Carrying {
        serving, gateway, ..
    } = &*carrying;
    let Ok(target) = carrying.target(client.peer_addr()) else {
        tracing::debug!("tun: udp {} is a name no longer known", client.peer_addr());
        return Ok(());
    };
    let mut client = client;
    let mut first = Vec::new();
    if carrying.names.is_some()
        && let Target::Socket(_) = target
        && target.port() == 443
    {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 2048];
        if let Ok(n) = client.read(&mut buf).await {
            first.extend_from_slice(&buf[..n]);
        }
        // QUIC to a bare address: its name is sealed, so the rules could see
        // only the address. Refused, the program falls back to TCP, whose
        // name `carry` reads.
        if sniff::looks_like_quic(&first)
            && let Target::Socket(addr) = target
        {
            tracing::debug!("tun: QUIC to bare {addr} refused, for TCP to name the site");
            return Ok(());
        }
    }
    let process = carrying
        .program(false, client.local_addr(), client.peer_addr())
        .await;
    let decision = carrying
        .decide(&target, None, process.as_deref(), true)
        .await;

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
        Route::Direct => match resolve(&target).await {
            Ok(addr) => bind_direct(addr, *gateway).await,
            Err(e) => Err(e),
        },
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

    if !first.is_empty() {
        upstream.send(&first).await?;
        conn.add_up(first.len() as u64);
    }
    let relayed = bliss_core::relay_datagrams(client, upstream, Arc::clone(&conn));
    let carried = carrying.until_rerouted(relayed, &conn, None, true).await;
    serving.tracker.close(id);
    match carried {
        Carried::Ended(result) => result,
        // A flow has nothing to close: its next datagram starts a new one.
        Carried::Rerouted => Ok(()),
    }
}

/// Where the lookups `names` does not answer itself go: through the tunnel.
const UPSTREAM_DNS: SocketAddr =
    SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::new(1, 1, 1, 1)), 53);
/// How long each way of learning a site's address is waited on.
const ADDRESS_WAIT: Duration = Duration::from_millis(1500);
/// How long a flow of lookups may stay quiet, and one lookup may take.
const LOOKUP_IDLE: Duration = Duration::from_secs(30);
const LOOKUP_WAIT: Duration = Duration::from_secs(5);

/// The lookups sent to bliss's own DNS address, one datagram at a time.
async fn answer_lookups(
    mut client: IpStackUdpStream,
    names: Arc<FakeNames>,
    carrying: Arc<Carrying>,
) -> io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 1500];
    loop {
        let n = match tokio::time::timeout(LOOKUP_IDLE, client.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => return Ok(()),
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
        };
        let reply = match names.answer(&buf[..n]) {
            None => continue,
            Some(Answer::Ready(reply)) => reply,
            Some(Answer::Faked { reply, name }) => {
                // The connection follows in a moment: learn the site's real
                // address now, if any rule looks at addresses, so it is
                // decided without waiting.
                if carrying.serving.router().has_address_rules()
                    && carrying.addresses.get(&name).is_none()
                {
                    let carrying = Arc::clone(&carrying);
                    tokio::spawn(async move {
                        let _ = carrying.real_address(&name).await;
                    });
                }
                reply
            }
            Some(Answer::Local(query)) => {
                let found = tokio::time::timeout(
                    LOOKUP_WAIT,
                    tokio::net::lookup_host((query.name.as_str(), 0)),
                )
                .await;
                match found {
                    Ok(Ok(addrs)) => {
                        let ips: Vec<_> = addrs
                            .filter_map(|a| match a.ip() {
                                IpAddr::V4(ip) => Some(ip),
                                IpAddr::V6(_) => None,
                            })
                            .take(8)
                            .collect();
                        query.reply(&ips)
                    }
                    _ => query.failed(),
                }
            }
            Some(Answer::Forward(query)) => {
                let asked = &buf[..n];
                let forwarded = async {
                    let upstream = carrying
                        .serving
                        .bind_tunnelled(Route::Proxy, &Target::Socket(UPSTREAM_DNS))
                        .await?;
                    upstream.send(asked).await?;
                    let mut back = vec![0u8; 1500];
                    let got = upstream.recv(&mut back).await?;
                    back.truncate(got);
                    io::Result::Ok(back)
                };
                match tokio::time::timeout(LOOKUP_WAIT, forwarded).await {
                    Ok(Ok(back)) => back,
                    _ => query.failed(),
                }
            }
        };
        client.write_all(&reply).await?;
    }
}

/// Programs whose own connections carry a VPN beside the tunnel.
///
/// OpenVPN's connection to its server is another program's socket, so the
/// adapter catches it like any other. Decided by the rules, it would usually
/// go through the proxy server — a work network reached by way of a server
/// abroad, if the work server takes connections from there at all.
const VPN_PROGRAMS: [&str; 2] = ["openvpn.exe", "openvpn"];

fn is_vpn_program(process: Option<&str>) -> bool {
    process.is_some_and(|p| VPN_PROGRAMS.iter().any(|v| p.eq_ignore_ascii_case(v)))
}

/// The rules' answer, except for the VPN's own connections, which leave the
/// ordinary way whatever the rules say.
fn decide(serving: &Serving, target: &Target, process: Option<&str>) -> Decision {
    if is_vpn_program(process) {
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

    /// A system that never answers whose a connection is must not hold the
    /// connection: on Android 0.2.0 every one waited for good.
    #[tokio::test]
    async fn an_owner_that_never_answers_is_not_waited_for() {
        let owner: Owner = Arc::new(|_, _, _| {
            std::thread::sleep(Duration::from_secs(2));
            Some("never.seen".to_owned())
        });
        let local: SocketAddr = "172.19.0.1:40000".parse().unwrap();
        let remote: SocketAddr = "198.18.0.1:443".parse().unwrap();
        let started = std::time::Instant::now();
        let name = ask_owner(&owner, true, local, remote, Duration::from_millis(100)).await;
        assert_eq!(name, None);
        assert!(started.elapsed() < Duration::from_secs(1));

        let quick: Owner = Arc::new(|tcp, _, _| tcp.then(|| "org.example.app".to_owned()));
        assert_eq!(
            ask_owner(&quick, true, local, remote, Duration::from_millis(500))
                .await
                .as_deref(),
            Some("org.example.app")
        );
    }

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
    /// An IPv4 packet carrying one TCP segment, both checksums right.
    #[cfg(target_os = "linux")]
    fn tcp_packet(
        src: SocketAddr,
        dst: SocketAddr,
        seq: u32,
        ack: u32,
        flags: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let (SocketAddr::V4(src), SocketAddr::V4(dst)) = (src, dst) else {
            panic!("IPv4 only");
        };
        let fold = |mut sum: u32| {
            while sum >> 16 != 0 {
                sum = (sum & 0xffff) + (sum >> 16);
            }
            !(sum as u16)
        };
        let words = |bytes: &[u8]| -> u32 {
            bytes
                .chunks(2)
                .map(|c| u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])))
                .sum()
        };
        let mut tcp = Vec::new();
        tcp.extend_from_slice(&src.port().to_be_bytes());
        tcp.extend_from_slice(&dst.port().to_be_bytes());
        tcp.extend_from_slice(&seq.to_be_bytes());
        tcp.extend_from_slice(&ack.to_be_bytes());
        tcp.extend_from_slice(&[5 << 4, flags, 0xff, 0xff, 0, 0, 0, 0]);
        tcp.extend_from_slice(payload);
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&src.ip().octets());
        pseudo.extend_from_slice(&dst.ip().octets());
        pseudo.extend_from_slice(&[0, 6]);
        pseudo.extend_from_slice(&(tcp.len() as u16).to_be_bytes());
        let sum = fold(words(&pseudo) + words(&tcp));
        tcp[16..18].copy_from_slice(&sum.to_be_bytes());
        let total = 20 + tcp.len();
        let mut p = vec![
            0x45,
            0,
            (total >> 8) as u8,
            total as u8,
            0,
            1,
            0x40,
            0,
            64,
            6,
            0,
            0,
        ];
        p.extend_from_slice(&src.ip().octets());
        p.extend_from_slice(&dst.ip().octets());
        let sum = fold(words(&p));
        p[10..12].copy_from_slice(&sum.to_be_bytes());
        p.extend_from_slice(&tcp);
        p
    }

    /// The packets the stack writes to the phone, one at a time, however
    /// the pipe joins them.
    #[cfg(target_os = "linux")]
    struct Packets {
        pipe: tokio::io::DuplexStream,
        held: Vec<u8>,
    }

    #[cfg(target_os = "linux")]
    impl Packets {
        async fn next(&mut self) -> Vec<u8> {
            use tokio::io::AsyncReadExt;
            loop {
                if self.held.len() >= 4 {
                    let len = usize::from(u16::from_be_bytes([self.held[2], self.held[3]]));
                    if self.held.len() >= len {
                        return self.held.drain(..len).collect();
                    }
                }
                let mut buf = vec![0u8; 65536];
                let n = tokio::time::timeout(Duration::from_secs(3), self.pipe.read(&mut buf))
                    .await
                    .expect("the stack wrote a packet")
                    .unwrap();
                assert!(n > 0, "the stack ended");
                self.held.extend_from_slice(&buf[..n]);
            }
        }

        /// One packet to the stack, alone: a TUN hands over one packet a
        /// read, and the pipe would join two written back to back.
        async fn send(&mut self, packet: &[u8]) {
            use tokio::io::AsyncWriteExt;
            self.pipe.write_all(packet).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        /// The next TCP segment: its flags, sequence number and payload.
        async fn segment(&mut self) -> (u8, u32, Vec<u8>) {
            let p = self.next().await;
            assert_eq!(p[9], 6, "TCP");
            let tcp = &p[20..];
            let header = usize::from(tcp[12] >> 4) * 4;
            let seq = u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]);
            (tcp[13], seq, tcp[header..].to_vec())
        }
    }

    /// A connection to a bare address — a program that found it without
    /// asking bliss — whose ClientHello names `www.reddit.com`: a rule for
    /// that name sends it straight to the address the program chose, with
    /// every byte it sent. And when the rules change to send it elsewhere,
    /// the open connection is ended rather than carried on the old way. Both
    /// were the user's Reddit rule that "did not work", 1 November.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_bare_address_is_decided_by_the_name_it_says() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // The far end: an echo on loopback, at a port whose clients speak first.
        let Ok(echo) = tokio::net::TcpListener::bind("127.0.0.1:8443").await else {
            eprintln!("127.0.0.1:8443 is taken here; not run");
            return;
        };
        let echoed = tokio::spawn(async move {
            let (mut socket, _) = echo.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let mut got = Vec::new();
            while let Ok(n) = socket.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                socket.write_all(&buf[..n]).await.unwrap();
            }
            got
        });

        let mut reddit = bliss_proto::Rule {
            name: Some("Reddit".to_owned()),
            action: bliss_proto::Action::Direct,
            ..bliss_proto::Rule::default()
        };
        reddit.domain = vec!["reddit.com".to_owned()];
        let routing = bliss_proto::Routing {
            fallback: bliss_proto::Action::Proxy,
            rules: vec![reddit],
            ..bliss_proto::Routing::default()
        };
        let serving = Arc::new(Serving::new(
            Arc::new(SocksOutbound::new(
                "127.0.0.1:1".parse().expect("an address"),
                EngineInfo {
                    kind: "test",
                    version: String::new(),
                    external: true,
                },
            )),
            Arc::new(Tracker::new()),
            Arc::new(Router::build_resolving(
                &routing,
                std::path::Path::new("/nonexistent"),
            )),
        ));
        let mut carrying = Carrying::new(Arc::clone(&serving), Gateway::for_tests());
        carrying.names = Some(("172.19.0.2:53".parse().unwrap(), Arc::new(FakeNames::new())));
        let (phone, device) = tokio::io::duplex(256 * 1024);
        let stack = tokio::spawn(serve(device, Arc::new(carrying)));
        let mut phone = Packets {
            pipe: phone,
            held: Vec::new(),
        };

        let app: SocketAddr = "172.19.0.1:40001".parse().unwrap();
        let far: SocketAddr = "127.0.0.1:8443".parse().unwrap();
        const SYN: u8 = 0x02;
        const ACK: u8 = 0x10;
        const PSH: u8 = 0x08;
        const FIN: u8 = 0x01;
        const RST: u8 = 0x04;
        phone.send(&tcp_packet(app, far, 1000, 0, SYN, &[])).await;
        let (flags, theirs, _) = phone.segment().await;
        assert_eq!(flags & (SYN | ACK), SYN | ACK);
        let mut ours = 1001;
        let mut acked = theirs.wrapping_add(1);
        phone
            .send(&tcp_packet(app, far, ours, acked, ACK, &[]))
            .await;

        let hello = crate::sniff::tests::client_hello("www.reddit.com");
        phone
            .send(&tcp_packet(app, far, ours, acked, PSH | ACK, &hello))
            .await;
        ours += hello.len() as u32;

        // The echo comes back through the stack: the bytes went straight.
        let mut back = Vec::new();
        while back.len() < hello.len() {
            let (flags, seq, payload) = phone.segment().await;
            assert_eq!(
                flags & (FIN | RST),
                0,
                "closed before the echo: {:?}",
                serving.tracker.table()
            );
            if !payload.is_empty() {
                acked = seq.wrapping_add(payload.len() as u32);
                back.extend_from_slice(&payload);
                phone
                    .send(&tcp_packet(app, far, ours, acked, ACK, &[]))
                    .await;
            }
        }
        assert_eq!(back, hello);
        let rows = serving.tracker.table();
        assert_eq!(rows[0].target, "www.reddit.com:8443");
        assert_eq!(rows[0].route, Route::Direct);
        assert_eq!(rows[0].rule, "Reddit");

        // The rule goes: the open connection is ended, not left on its way.
        serving.set_router(Arc::new(Router::proxy_everything()));
        loop {
            let (flags, _, _) = phone.segment().await;
            if flags & (FIN | RST) != 0 {
                break;
            }
        }
        assert_eq!(echoed.await.unwrap(), hello);
        stack.abort();
    }

    /// An IPv4 packet carrying one UDP datagram, checksum left out (allowed
    /// for UDP over IPv4).
    #[cfg(target_os = "linux")]
    fn udp_packet(src: SocketAddr, dst: SocketAddr, payload: &[u8]) -> Vec<u8> {
        let (SocketAddr::V4(src), SocketAddr::V4(dst)) = (src, dst) else {
            panic!("IPv4 only");
        };
        let total = 20 + 8 + payload.len();
        let mut p = vec![
            0x45,
            0,
            (total >> 8) as u8,
            total as u8,
            0,
            1,
            0,
            0,
            64,
            17,
            0,
            0,
        ];
        p.extend_from_slice(&src.ip().octets());
        p.extend_from_slice(&dst.ip().octets());
        let sum = (0..20)
            .step_by(2)
            .map(|i| u32::from(u16::from_be_bytes([p[i], p[i + 1]])))
            .sum::<u32>();
        let sum = !((sum & 0xffff) + (sum >> 16)) as u16;
        p[10..12].copy_from_slice(&sum.to_be_bytes());
        p.extend_from_slice(&src.port().to_be_bytes());
        p.extend_from_slice(&dst.port().to_be_bytes());
        p.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        p.extend_from_slice(&[0, 0]);
        p.extend_from_slice(payload);
        p
    }

    /// A lookup sent into the TUN, to the address the stack answers at,
    /// comes back from that address with an address from the pool — and a
    /// second one for the same name with the same address. This is the path
    /// every name on the phone takes; 0.2.0 shipped without it run anywhere.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_site_by_name_meets_a_rule_by_address() {
        let mut lan = bliss_proto::Rule {
            name: Some("Сеть".to_owned()),
            action: bliss_proto::Action::Direct,
            ..bliss_proto::Rule::default()
        };
        lan.ip = vec!["203.0.113.0/24".to_owned()];
        let routing = bliss_proto::Routing {
            fallback: bliss_proto::Action::Proxy,
            rules: vec![lan],
            ..bliss_proto::Routing::default()
        };
        let router = Router::build_resolving(&routing, std::path::Path::new("/nonexistent"));
        let serving = Arc::new(Serving::new(
            Arc::new(SocksOutbound::new(
                "127.0.0.1:1".parse().expect("an address"),
                EngineInfo {
                    kind: "test",
                    version: String::new(),
                    external: true,
                },
            )),
            Arc::new(Tracker::new()),
            Arc::new(router),
        ));
        let mut carrying = Carrying::new(serving, Gateway::for_tests());
        carrying.names = Some(("172.19.0.2:53".parse().unwrap(), Arc::new(FakeNames::new())));
        // As if learnt through the tunnel already.
        carrying
            .addresses
            .put("nas.example", Some(std::net::Ipv4Addr::new(203, 0, 113, 7)));
        carrying.addresses.put(
            "far.example",
            Some(std::net::Ipv4Addr::new(198, 51, 100, 5)),
        );
        let site = |host: &str| Target::Domain {
            host: host.to_owned(),
            port: 443,
        };
        assert_eq!(
            carrying
                .decide(&site("nas.example"), None, None, false)
                .await
                .route,
            Route::Direct
        );
        assert_eq!(
            carrying
                .decide(&site("far.example"), None, None, false)
                .await
                .route,
            Route::Proxy
        );
        // Without names (the desktop), a name is never resolved for a rule.
        carrying.names = None;
        assert_eq!(
            carrying
                .decide(&site("nas.example"), None, None, false)
                .await
                .route,
            Route::Proxy
        );
    }

    /// The phone's built-in rule, UDP and port 443 → block, as other clients
    /// write it: a flow to a site's port 443 is refused — shown so in the
    /// table — so the page opens over TCP; UDP to another port, or with the
    /// rule switched off, is carried. The user found parts of GitHub not
    /// loading, and asked for the rule in this shape.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_to_443_meets_the_rule() {
        use tokio::io::AsyncWriteExt;
        let routing = |on: bool| {
            let mut quic = bliss_proto::Rule {
                name: Some("QUIC".to_owned()),
                action: bliss_proto::Action::Block,
                enabled: on,
                ..bliss_proto::Rule::default()
            };
            quic.network = vec!["udp".to_owned()];
            quic.port = vec!["443".to_owned()];
            let routing = bliss_proto::Routing {
                fallback: bliss_proto::Action::Proxy,
                rules: vec![quic],
                ..bliss_proto::Routing::default()
            };
            Arc::new(Router::build_resolving(
                &routing,
                std::path::Path::new("/nonexistent"),
            ))
        };
        let serving = Arc::new(Serving::new(
            Arc::new(SocksOutbound::new(
                "127.0.0.1:1".parse().expect("an address"),
                EngineInfo {
                    kind: "test",
                    version: String::new(),
                    external: true,
                },
            )),
            Arc::new(Tracker::new()),
            routing(true),
        ));
        let names = Arc::new(FakeNames::new());
        let mut carrying = Carrying::new(Arc::clone(&serving), Gateway::for_tests());
        carrying.names = Some(("172.19.0.2:53".parse().unwrap(), Arc::clone(&names)));
        let (mut phone, device) = tokio::io::duplex(256 * 1024);
        let stack = tokio::spawn(serve(device, Arc::new(carrying)));

        // An address from the pool, as the phone's lookup would get it.
        let site = SocketAddr::new(
            IpAddr::V4(names.address_for("github.githubassets.com")),
            443,
        );
        let mut initial = vec![0u8; 1200];
        initial[0] = 0xc3;
        initial[1..5].copy_from_slice(&1u32.to_be_bytes());
        let mut send = async |port: u16, to: SocketAddr, payload: &[u8]| {
            let app = SocketAddr::new("172.19.0.1".parse().unwrap(), port);
            phone
                .write_all(&udp_packet(app, to, payload))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
        };

        send(41000, site, &initial).await;
        let rows = serving.tracker.table();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].target, "github.githubassets.com:443");
        assert_eq!(rows[0].route, Route::Blocked);
        assert_eq!(rows[0].rule, "QUIC");

        // A later packet of the same QUIC connection — a short header, as
        // the flow after a refused Initial starts with — meets the rule too:
        // refusing the Initial alone let these through, and Google answered
        // a handshake it had not seen (ERR_QUIC_PROTOCOL_ERROR).
        let mut short = vec![0u8; 300];
        short[0] = 0x43;
        send(41003, site, &short).await;
        assert_eq!(serving.tracker.table().len(), 2);
        assert_eq!(serving.tracker.table()[0].route, Route::Blocked);

        // UDP to another port: carried towards the server, which here
        // refuses the association, so nothing is recorded.
        let call = SocketAddr::new(site.ip(), 3478);
        send(41001, call, &[0x17; 200]).await;
        assert_eq!(serving.tracker.table().len(), 2);

        // The rule off: carried like any flow.
        serving.set_router(routing(false));
        send(41002, site, &initial).await;
        assert_eq!(serving.tracker.table().len(), 2);
        stack.abort();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lookup_through_the_stack_is_answered_from_the_pool() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let serving = Arc::new(Serving::new(
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
        ));
        let dns: SocketAddr = "172.19.0.2:53".parse().expect("an address");
        let names = Arc::new(FakeNames::new());
        let mut carrying = Carrying::new(serving, Gateway::for_tests());
        carrying.names = Some((dns, Arc::clone(&names)));
        let (mut phone, device) = tokio::io::duplex(64 * 1024);
        let stack = tokio::spawn(serve(device, Arc::new(carrying)));

        let mut query = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in ["www", "example", "net"] {
            query.push(label.len() as u8);
            query.extend_from_slice(label.as_bytes());
        }
        query.extend_from_slice(&[0, 0, 1, 0, 1]);
        let app: SocketAddr = "172.19.0.1:40000".parse().expect("an address");

        let mut seen = Vec::new();
        for _ in 0..2 {
            phone
                .write_all(&udp_packet(app, dns, &query))
                .await
                .unwrap();
            let mut buf = vec![0u8; 2048];
            let n = tokio::time::timeout(Duration::from_secs(3), phone.read(&mut buf))
                .await
                .expect("the stack answered")
                .unwrap();
            let packet = &buf[..n];
            // From the DNS address back to the app, UDP.
            assert_eq!(packet[9], 17);
            assert_eq!(&packet[12..16], &[172, 19, 0, 2]);
            assert_eq!(&packet[16..20], &[172, 19, 0, 1]);
            let answer = &packet[28..];
            assert_eq!(&answer[..2], &[0xab, 0xcd]);
            assert_eq!(&answer[6..8], &[0, 1], "one answer");
            let ip =
                std::net::Ipv4Addr::from(<[u8; 4]>::try_from(&answer[answer.len() - 4..]).unwrap());
            assert!(FakeNames::is_fake(ip));
            assert_eq!(names.name_of(ip).as_deref(), Some("www.example.net"));
            seen.push(ip);
        }
        assert_eq!(seen[0], seen[1]);
        stack.abort();
    }

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
