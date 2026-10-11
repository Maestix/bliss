//! The local listener: what a browser or a shell is pointed at.
//!
//! One port serves both SOCKS5 and HTTP CONNECT, decided by peeking at the first
//! byte — 0x05 can only be a SOCKS version, and every HTTP method starts with a
//! letter. xray-core calls this a `mixed` inbound and it is the right default:
//! two ports for two protocols is a setting nobody wants to think about.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};

use crate::http;
use crate::outbound::Outbound;
use crate::relay::relay;
use crate::routing::Router;
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

/// Everything a connection needs that is the same for all of them.
///
/// One struct because it travels together everywhere: the dialer, the table
/// that records what happened, and the rules that decide. Passing them
/// separately grew the signatures past the point where the order of the
/// arguments was the only thing telling them apart.
pub struct Serving {
    pub outbound: Arc<dyn Outbound>,
    pub tracker: Arc<Tracker>,
    /// Behind a lock so the rules can change under live traffic; see
    /// `set_router`.
    router: RwLock<Arc<Router>>,
    /// Where `Route::Vpn` goes: set while the VPN is connected and cleared
    /// when it is not, so a connection meant for it is refused rather than
    /// sent some other way.
    vpn: RwLock<Option<Arc<dyn Outbound>>>,
}

impl Serving {
    pub fn new(outbound: Arc<dyn Outbound>, tracker: Arc<Tracker>, router: Arc<Router>) -> Self {
        Self {
            outbound,
            tracker,
            router: RwLock::new(router),
            vpn: RwLock::new(None),
        }
    }

    /// The way through the VPN, while there is one.
    pub fn vpn(&self) -> Option<Arc<dyn Outbound>> {
        self.vpn.read().expect("vpn lock").clone()
    }

    /// Send `Route::Vpn` connections this way from now on, or refuse them.
    /// The VPN comes and goes on its own, beside the tunnel, so this changes
    /// under live traffic the way the rules do.
    pub fn set_vpn(&self, vpn: Option<Arc<dyn Outbound>>) {
        *self.vpn.write().expect("vpn lock") = vpn;
    }

    /// Dial `target` the way `route` says, for a route that dials at all:
    /// through the tunnel, or through the VPN while it is up. `Direct` is
    /// dialled by each inbound itself, since the TUN one has to name the
    /// interface it leaves by.
    pub async fn dial_tunnelled(
        &self,
        route: Route,
        target: &Target,
    ) -> std::io::Result<crate::outbound::BoxedStream> {
        match route {
            Route::Vpn => match self.vpn() {
                Some(vpn) => vpn.connect_tcp(target).await,
                None => Err(crate::interface::no_vpn()),
            },
            _ => self.outbound.connect_tcp(target).await,
        }
    }

    /// `dial_tunnelled` for a flow of datagrams.
    pub async fn bind_tunnelled(
        &self,
        route: Route,
        target: &Target,
    ) -> std::io::Result<crate::outbound::BoxedDatagram> {
        match route {
            Route::Vpn => match self.vpn() {
                Some(vpn) => vpn.bind_udp(target).await,
                None => Err(crate::interface::no_vpn()),
            },
            _ => self.outbound.bind_udp(target).await,
        }
    }

    /// The table the next connection is decided by.
    pub fn router(&self) -> Arc<Router> {
        Arc::clone(&self.router.read().expect("router lock"))
    }

    /// Decide new connections by `router` from now on.
    ///
    /// Connections already open keep the route they were given. Until this
    /// existed every inbound held the table it started with, so changing one
    /// rule restarted the whole tunnel and dropped every connection through it.
    /// A lock rather than a lock-free swap: it is taken once per connection,
    /// not per packet, for as long as cloning an `Arc` takes.
    pub fn set_router(&self, router: Arc<Router>) {
        *self.router.write().expect("router lock") = router;
    }
}

/// Accept connections until the future is dropped.
pub async fn serve(bound: Bound, serving: Arc<Serving>) -> std::io::Result<()> {
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

        let serving = Arc::clone(&serving);
        tokio::spawn(async move {
            if let Err(e) = handle(stream, peer, serving).await {
                tracing::debug!("conn: {peer} — {e}");
            }
        });
    }
}

async fn handle(stream: TcpStream, peer: SocketAddr, serving: Arc<Serving>) -> std::io::Result<()> {
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

    // The head a forwarded request has to replay upstream. Empty for SOCKS and
    // for CONNECT, where the client's next byte is already payload.
    let mut replay = Vec::new();

    let target = if socks {
        socks5::accept(&mut stream).await?
    } else {
        match http::accept(&mut stream).await {
            Ok(http::Request::Connect(target)) => target,
            Ok(http::Request::Forward { target, head }) => {
                replay = head;
                target
            }
            Err(e @ (http::HttpError::NotProxied(..) | http::HttpError::Scheme(_))) => {
                // Nothing is wrong with the connection, so answer on it rather
                // than dropping it: a browser shows this, a dropped socket
                // shows a spinner.
                http::refuse(&mut stream, "501 Not Implemented", &e.to_string()).await?;
                return Ok(());
            }
            Err(e) => return Err(std::io::Error::other(e)),
        }
    };

    dial_and_relay(stream, socks, replay, target, peer, serving).await
}

async fn dial_and_relay(
    mut client: BufReader<TcpStream>,
    socks: bool,
    replay: Vec<u8>,
    target: Target,
    peer: SocketAddr,
    serving: Arc<Serving>,
) -> std::io::Result<()> {
    let tracker = &serving.tracker;
    // Read once: this connection is decided by one table, whatever changes
    // while it is open.
    let router = serving.router();
    // The process is looked up before the decision rather than after, because a
    // rule may be about the program rather than the destination. By both ends
    // of the connection: Linux gives one local port to connections to
    // different places, and the port alone could name another program.
    let process = match client.get_ref().local_addr() {
        Ok(inbound) => crate::process::name_for_connection(peer, inbound),
        Err(_) => crate::process::name_for_port(peer.port()),
    };
    let decision = router.decide(&target, process.as_deref());

    // Blocked is answered, not dropped. A refusal a program can see becomes an
    // error page or a failed request; a silently dropped connection becomes a
    // spinner, and then a support question.
    if decision.route == Route::Blocked {
        tracing::debug!("conn: {target} blocked by {}", decision.rule);
        if socks {
            // 2 is "connection not allowed by ruleset", which is exactly this.
            socks5::reply(&mut client, 2).await?;
        } else {
            http::refuse(
                &mut client,
                "403 Forbidden",
                &format!("заблокировано правилом «{}»", decision.rule),
            )
            .await?;
        }
        // Recorded so the connections screen can show what was refused and why:
        // a block nobody can see is indistinguishable from a bug.
        let conn = tracker.open(target, decision.route, decision.rule, process);
        tracker.close(conn.id);
        return Ok(());
    }

    let dialled = match decision.route {
        Route::Direct => dial_direct(&target).await,
        // Blocked returned above: the tunnel, or the VPN.
        route => serving.dial_tunnelled(route, &target).await,
    };
    let mut upstream = match dialled {
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
    } else if replay.is_empty() {
        // A tunnel: tell the client it is open and then get out of the way.
        http::established(&mut client).await?;
    } else {
        // A forwarded request: the client is not told anything, because
        // everything it sees from here is the origin server's own answer. Its
        // head goes upstream first, then the relay carries the body and the
        // response.
        use tokio::io::AsyncWriteExt;
        upstream.write_all(&replay).await?;
        upstream.flush().await?;
    }

    let route = decision.route;
    let conn = tracker.open(target, route, decision.rule, process);
    let id = conn.id;
    tracing::debug!(
        "conn #{id} {} via {route} ({}) — {}",
        conn.target,
        conn.process.as_deref().unwrap_or("?"),
        conn.rule
    );

    let result = relay(client, upstream, Arc::clone(&conn)).await;
    tracker.close(id);
    result
}

/// Dial without the tunnel.
///
/// The name is resolved here, on this machine, which is the one place bliss
/// does that — and it is consistent rather than careless: a connection routed
/// direct is going out over the local network anyway, so looking its name up
/// locally reveals nothing the connection itself does not. For a proxied
/// connection the name travels inside the tunnel and is resolved at the far
/// end, which is why `Target` keeps names unresolved everywhere else.
async fn dial_direct(target: &Target) -> std::io::Result<crate::outbound::BoxedStream> {
    let dial = async {
        let stream = match target {
            Target::Socket(addr) => TcpStream::connect(addr).await?,
            Target::Domain { host, port } => TcpStream::connect((host.as_str(), *port)).await?,
        };
        stream.set_nodelay(true)?;
        Ok::<_, std::io::Error>(stream)
    };

    match tokio::time::timeout(crate::outbound::DIAL_TIMEOUT, dial).await {
        Ok(Ok(stream)) => Ok(Box::new(stream) as crate::outbound::BoxedStream),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "{target} did not answer directly within {:?}",
                crate::outbound::DIAL_TIMEOUT
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use bliss_proto::{Action, Routing, Rule};

    use super::*;
    use crate::outbound::{EngineInfo, SocksOutbound};

    /// A rule changed in the window decides the next connection, with the
    /// tunnel left up. Before `set_router` the inbounds kept the table they
    /// started with, and the only way to a new one was a restart.
    #[test]
    fn the_next_connection_is_decided_by_the_newest_rules() {
        let outbound = Arc::new(SocksOutbound::new(
            "127.0.0.1:1".parse().expect("an address"),
            EngineInfo {
                kind: "test",
                version: String::new(),
                external: true,
            },
        ));
        let serving = Serving::new(
            outbound,
            Arc::new(Tracker::new()),
            Arc::new(Router::proxy_everything()),
        );
        let target = Target::domain("a.example", 443);
        let before = serving.router();
        assert_eq!(before.decide(&target, None).route, Route::Proxy);

        let block = Rule {
            name: Some("no".to_owned()),
            action: Action::Block,
            domain: vec!["a.example".to_owned()],
            ..Rule::default()
        };
        let routing = Routing {
            fallback: Action::Proxy,
            geo_dir: None,
            rules: vec![block],
            ..Routing::default()
        };
        serving.set_router(Arc::new(Router::build(
            &routing,
            std::path::Path::new("nonexistent"),
        )));

        assert_eq!(serving.router().decide(&target, None).route, Route::Blocked);
        // A connection that read the table before the change keeps its answer.
        assert_eq!(before.decide(&target, None).route, Route::Proxy);
    }

    /// Answers every dial with its own name, so a test can see which way a
    /// connection went without a network.
    struct Named(&'static str);

    #[async_trait::async_trait]
    impl Outbound for Named {
        async fn connect_tcp(&self, _target: &Target) -> std::io::Result<crate::BoxedStream> {
            Err(std::io::Error::other(self.0))
        }
        async fn bind_udp(&self, _target: &Target) -> std::io::Result<crate::BoxedDatagram> {
            Err(std::io::Error::other(self.0))
        }
        fn describe(&self) -> EngineInfo {
            EngineInfo {
                kind: self.0,
                version: String::new(),
                external: false,
            }
        }
    }

    /// A connection a rule sends to the VPN goes to the VPN while there is
    /// one, and is refused while there is not — never through the tunnel,
    /// which is where it would otherwise have gone.
    #[tokio::test]
    async fn a_vpn_route_goes_to_the_vpn_or_nowhere() {
        let serving = Serving::new(
            Arc::new(Named("tunnel")),
            Arc::new(Tracker::new()),
            Arc::new(Router::proxy_everything()),
        );
        let target = Target::domain("tools.example", 443);
        let way = |e: std::io::Error| e.to_string();

        let refused = serving.dial_tunnelled(Route::Vpn, &target).await.err();
        let refused = refused.expect("no VPN, no connection");
        assert_eq!(refused.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(socks5::reply_code_for(&refused), 3);
        assert!(serving.bind_tunnelled(Route::Vpn, &target).await.is_err());

        serving.set_vpn(Some(Arc::new(Named("vpn"))));
        let went = serving.dial_tunnelled(Route::Vpn, &target).await.err();
        assert_eq!(went.map(way).as_deref(), Some("vpn"));
        let went = serving.dial_tunnelled(Route::Proxy, &target).await.err();
        assert_eq!(went.map(way).as_deref(), Some("tunnel"));

        serving.set_vpn(None);
        let refused = serving.dial_tunnelled(Route::Vpn, &target).await.err();
        assert_eq!(
            refused.map(|e| e.kind()),
            Some(std::io::ErrorKind::NotConnected)
        );
    }
}
