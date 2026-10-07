//! Connections that leave by one adapter of the machine: the VPN's.
//!
//! The VPN puts an adapter on the machine and bliss keeps it out of everyone's
//! way: the server's routes and DNS are refused, and the one route through it
//! bliss asks for has a metric no other connection would pick. So a connection
//! a rule sends to the VPN is told which adapter to leave by and from which
//! address, and its name is looked up by the VPN's own DNS server, through the
//! VPN. Nothing else on the machine takes this way, and nothing sent this way
//! can take another: with the VPN gone, the dial fails.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use async_trait::async_trait;
use tokio::net::{TcpSocket, TcpStream};

use crate::dns::Resolver;
use crate::interface::{leave_by, only_ipv4};
use crate::outbound::{BoxedDatagram, BoxedStream, DIAL_TIMEOUT, Datagram, EngineInfo, Outbound};
use crate::target::Target;

/// Dials through one interface, from its address.
pub struct Through {
    index: u32,
    local: Ipv4Addr,
    resolver: Resolver,
    info: EngineInfo,
}

impl Through {
    /// `index` and `local` are the adapter's number and its address on the
    /// VPN; `dns` the servers the VPN offered, which may be none.
    pub fn new(index: u32, local: Ipv4Addr, dns: Vec<SocketAddr>, info: EngineInfo) -> Self {
        Self {
            index,
            local,
            resolver: Resolver::new(dns, local, index),
            info,
        }
    }

    /// Where `target` is, in the addresses this way can reach.
    async fn addresses(&self, target: &Target) -> io::Result<Vec<SocketAddr>> {
        let (host, port) = match target {
            Target::Socket(address) => {
                return Ok(vec![SocketAddr::new(
                    only_ipv4(address.ip())?.into(),
                    address.port(),
                )]);
            }
            Target::Domain { host, port } => (host.as_str(), *port),
        };
        // An address spelled as a name is an address.
        if let Ok(address) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(only_ipv4(address)?.into(), port)]);
        }
        let found: Vec<Ipv4Addr> = if self.resolver.has_servers() {
            self.resolver.lookup(host).await?
        } else {
            // The VPN offered no DNS server: the machine's own resolver, which
            // knows public names. A work network's own names need its server.
            tokio::net::lookup_host((host, port))
                .await?
                .filter_map(|address| match address.ip() {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                })
                .collect()
        };
        Ok(found
            .into_iter()
            .map(|address| SocketAddr::new(address.into(), port))
            .collect())
    }

    async fn dial(&self, address: SocketAddr) -> io::Result<TcpStream> {
        let socket = TcpSocket::new_v4()?;
        socket.bind(SocketAddr::from((self.local, 0)))?;
        leave_by(&socket, self.index)?;
        let stream = socket.connect(address).await?;
        stream.set_nodelay(true)?;
        Ok(stream)
    }
}

#[async_trait]
impl Outbound for Through {
    async fn connect_tcp(&self, target: &Target) -> io::Result<BoxedStream> {
        let attempt = async {
            let mut last = None;
            for address in self.addresses(target).await? {
                match self.dial(address).await {
                    Ok(stream) => return Ok(Box::new(stream) as BoxedStream),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("у {target} нет адреса IPv4"),
                )
            }))
        };
        match tokio::time::timeout(DIAL_TIMEOUT, attempt).await {
            Ok(result) => result,
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{target} не ответил через VPN за {} с",
                    DIAL_TIMEOUT.as_secs()
                ),
            )),
        }
    }

    async fn bind_udp(&self, target: &Target) -> io::Result<BoxedDatagram> {
        let address = self
            .addresses(target)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("у {target} нет адреса IPv4"),
                )
            })?;
        let socket = std::net::UdpSocket::bind(SocketAddr::from((self.local, 0)))?;
        socket.set_nonblocking(true)?;
        leave_by(&socket, self.index)?;
        let socket = tokio::net::UdpSocket::from_std(socket)?;
        // Connected, so anything from elsewhere is dropped before bliss sees it.
        socket.connect(address).await?;
        Ok(Box::new(Flow { socket }))
    }

    fn describe(&self) -> EngineInfo {
        self.info.clone()
    }
}

/// One flow of datagrams through the VPN.
struct Flow {
    socket: tokio::net::UdpSocket,
}

#[async_trait]
impl Datagram for Flow {
    async fn send(&self, payload: &[u8]) -> io::Result<()> {
        self.socket.send(payload).await?;
        Ok(())
    }

    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.socket.recv(buf).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn through() -> Through {
        Through::new(
            1,
            Ipv4Addr::new(10, 8, 0, 2),
            Vec::new(),
            EngineInfo {
                kind: "test",
                version: String::new(),
                external: true,
            },
        )
    }

    /// An address, however it was spelled, is used as it is: no lookup.
    #[tokio::test]
    async fn an_address_is_not_looked_up() {
        let way = through();
        let spelled = Target::Domain {
            host: "10.1.2.3".to_owned(),
            port: 443,
        };
        assert_eq!(
            way.addresses(&spelled).await.expect("an address"),
            [SocketAddr::from(([10, 1, 2, 3], 443))]
        );
        let written = Target::Socket(SocketAddr::from(([10, 1, 2, 4], 80)));
        assert_eq!(
            way.addresses(&written).await.expect("an address"),
            [SocketAddr::from(([10, 1, 2, 4], 80))]
        );
    }

    /// IPv6 is refused in words rather than sent some other way.
    #[tokio::test]
    async fn ipv6_is_refused() {
        let way = through();
        let target = Target::Socket("[2001:db8::1]:443".parse().expect("an address"));
        let refused = way.addresses(&target).await.expect_err("not carried");
        assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
    }
}
