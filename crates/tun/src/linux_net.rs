//! The routes that steer traffic into the device on Linux, over netlink.
//!
//! In the process rather than through `ip`: the capability that lets bliss
//! touch routes is the executable's own (`setcap`), and a program it starts
//! does not inherit it, so `ip route add` from a capable bliss is refused.
//! Four messages are all it takes — add, delete, look one up, list — written
//! here by hand over a `NETLINK_ROUTE` socket, as dns.rs writes DNS.
//!
//! Every route bliss adds carries a protocol number of its own
//! (`rtm_protocol`, shown by `ip route` as `proto 98`). The routes into the
//! device die with it; the ones around it, through the old gateway, do not,
//! and a bliss that was killed leaves them behind. They point where traffic
//! would go anyway, until the machine moves to another network: so the next
//! start of the tunnel finds them by that number and takes them away first.
//!
//! A route is added, never replaced. One of the same prefix and metric that
//! is already there is another program's — bliss's own were taken away
//! first — and replaced, it would carry bliss's number and be deleted with
//! bliss's routes when the tunnel stops: another VPN's halves of the address
//! space, and its traffic out around it until it reconnected.
//!
//! IPv4 only, as on Windows: a machine with working IPv6 reaches an IPv6 site
//! around the tunnel.

use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub use crate::device::set_address;

/// Which interface: the kernel's index, which is what routes and sockets
/// both name it by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interface(pub u32);

/// How traffic left before the tunnel existed: the interface, and the router
/// on it, if the way out has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gateway {
    index: u32,
    next_hop: Option<Ipv4Addr>,
}

impl Gateway {
    /// A way out for tests that never dial directly.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            index: 1,
            next_hop: None,
        }
    }
}

impl std::fmt::Display for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.next_hop {
            Some(router) => write!(f, "{router}"),
            None => write!(f, "interface {}", self.index),
        }
    }
}

/// The protocol number bliss's routes carry. Not one of iproute2's own
/// (`/etc/iproute2/rt_protos`: the kernel's 0–4, routing daemons' 8–18, 42,
/// 99, 186–192).
pub const PROTOCOL: u8 = 98;

/// An address any machine can route to, to ask the kernel which way out it
/// would take — the question, not a packet.
const SOMEWHERE: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

/// The way out the machine had before bliss, ignoring the device bliss just
/// made: the kernel's own answer for a public address, policy rules and all.
pub fn gateway(except: Interface) -> io::Result<Option<Gateway>> {
    let found = Netlink::open()?.way_to(SOMEWHERE)?;
    Ok(found.filter(|gateway| gateway.index != except.0))
}

/// Take away the routes a bliss that died left around the tunnel. Asked
/// before new ones are made, and answered with how many there were.
pub fn forget_stale() -> io::Result<usize> {
    let mut netlink = Netlink::open()?;
    let stale: Vec<Route> = netlink
        .routes()?
        .into_iter()
        .filter(|route| route.protocol == PROTOCOL)
        .collect();
    for route in &stale {
        netlink.change(RTM_DELROUTE, route)?;
    }
    Ok(stale.len())
}

/// The routes bliss added, and takes away again.
#[derive(Default)]
pub struct Routes(Vec<Route>);

impl Routes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send a whole prefix into the device: the two halves `0.0.0.0/1` and
    /// `128.0.0.0/1`, which beat the machine's default route by being longer
    /// without replacing it.
    ///
    /// Refused, in words, when another program holds the same prefix: most
    /// likely another VPN taking the whole machine, and two tunnels cannot
    /// both have it.
    pub fn through_tunnel(
        &mut self,
        interface: Interface,
        destination: Ipv4Addr,
        prefix: u8,
    ) -> io::Result<()> {
        let route = Route {
            destination,
            prefix,
            index: interface.0,
            next_hop: None,
            protocol: PROTOCOL,
            table: RT_TABLE_MAIN,
        };
        match self.add(&route) {
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{destination}/{prefix} already goes through {} — another VPN? Two tunnels \
                     cannot both take the whole machine: switch that one off first",
                    holder(&route).unwrap_or_else(|| "another interface".to_owned())
                ),
            )),
            Err(e) => Err(could_not(&route, e)),
            Ok(()) => Ok(()),
        }
    }

    /// Send one address out the way the kernel would have sent it before the
    /// tunnel: asked of the kernel for that address, which a machine with
    /// several ways out may answer differently from `gateway`. Must be asked
    /// before the routes through the tunnel exist, or the answer is the
    /// tunnel.
    pub fn around_tunnel(&mut self, gateway: Gateway, host: Ipv4Addr) -> io::Result<()> {
        let way = Netlink::open()?
            .way_to(host)
            .ok()
            .flatten()
            .unwrap_or(gateway);
        let route = Route {
            destination: host,
            prefix: 32,
            index: way.index,
            next_hop: way.next_hop,
            protocol: PROTOCOL,
            table: RT_TABLE_MAIN,
        };
        match self.add(&route) {
            // A route of its own already keeps the address out of the
            // tunnel's halves, being longer. It is someone else's, and stays
            // theirs: not taken over, and not taken away after.
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {
                tracing::info!("tun: {host} has a route of its own already; left as it is");
                Ok(())
            }
            Err(e) => Err(could_not(&route, e)),
            Ok(()) => Ok(()),
        }
    }

    fn add(&mut self, route: &Route) -> io::Result<()> {
        Netlink::open()?.change(RTM_NEWROUTE, route)?;
        self.0.push(route.clone());
        Ok(())
    }
}

/// A refused route, said with what was refused.
fn could_not(route: &Route, e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!(
            "could not route {}/{}: {e}",
            route.destination, route.prefix
        ),
    )
}

/// The interface another program's route goes out by, where bliss's of the
/// same prefix was refused, named as `ip route` names it.
fn holder(wanted: &Route) -> Option<String> {
    let theirs = Netlink::open()
        .ok()?
        .routes()
        .ok()?
        .into_iter()
        .find(|route| {
            route.table == wanted.table
                && route.destination == wanted.destination
                && route.prefix == wanted.prefix
                && route.protocol != PROTOCOL
        })?;
    let mut name = [0 as libc::c_char; libc::IF_NAMESIZE];
    // SAFETY: a buffer of IF_NAMESIZE, as if_indextoname asks; on success it
    // holds a NUL-terminated name.
    if unsafe { libc::if_indextoname(theirs.index, name.as_mut_ptr()) }.is_null() {
        return Some(format!("interface {}", theirs.index));
    }
    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
    Some(name.to_string_lossy().into_owned())
}

impl Drop for Routes {
    fn drop(&mut self) {
        let Ok(mut netlink) = Netlink::open() else {
            return;
        };
        for route in &self.0 {
            // A route into a device that has already gone is gone with it.
            let _ = netlink.change(RTM_DELROUTE, route);
        }
    }
}

/// Send this socket out the way traffic left before the tunnel existed: the
/// same option the VPN's connections use (`bliss_core::interface`). Bound to
/// the interface, a socket is routed among that interface's routes alone, so
/// the tunnel's halves of the address space never see it.
pub fn outside(socket: &impl AsRawFd, gateway: Gateway) -> io::Result<()> {
    bliss_core::interface::leave_by(socket, gateway.index)
}

// --- netlink ---------------------------------------------------------------

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const RTM_NEWROUTE: u16 = 24;
const RTM_DELROUTE: u16 = 25;
const RTM_GETROUTE: u16 = 26;

const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_DUMP: u16 = 0x300;
const NLM_F_EXCL: u16 = 0x200;
const NLM_F_CREATE: u16 = 0x400;

const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_TABLE: u16 = 15;

const RT_TABLE_MAIN: u8 = 254;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_LINK: u8 = 253;
const RTN_UNICAST: u8 = 1;

/// The header of every message, and of a route message after it.
const HEADER: usize = 16;
const RTMSG: usize = 12;

/// One IPv4 route, as bliss adds it and as the kernel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    destination: Ipv4Addr,
    prefix: u8,
    index: u32,
    next_hop: Option<Ipv4Addr>,
    protocol: u8,
    table: u8,
}

/// The four bytes of a netlink alignment.
fn aligned(len: usize) -> usize {
    (len + 3) & !3
}

/// One attribute: its length, its type, its bytes, padded.
fn attribute(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
    let len = 4 + data.len();
    out.extend_from_slice(&(len as u16).to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    out.extend_from_slice(data);
    out.resize(out.len() + aligned(len) - len, 0);
}

/// A whole message: the header, then `body`.
fn message(kind: u16, flags: u16, sequence: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + body.len());
    out.extend_from_slice(&((HEADER + body.len()) as u32).to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    out.extend_from_slice(&flags.to_ne_bytes());
    out.extend_from_slice(&sequence.to_ne_bytes());
    out.extend_from_slice(&0u32.to_ne_bytes());
    out.extend_from_slice(body);
    out
}

/// The `rtmsg` and attributes of a route, as a change to it says it.
fn route_body(route: &Route) -> Vec<u8> {
    let scope = if route.next_hop.is_some() {
        RT_SCOPE_UNIVERSE
    } else {
        RT_SCOPE_LINK
    };
    let mut body = vec![
        libc::AF_INET as u8,
        route.prefix,
        0, // source prefix
        0, // tos
        route.table,
        route.protocol,
        scope,
        RTN_UNICAST,
    ];
    body.extend_from_slice(&0u32.to_ne_bytes()); // flags
    attribute(&mut body, RTA_DST, &route.destination.octets());
    attribute(&mut body, RTA_OIF, &route.index.to_ne_bytes());
    if let Some(router) = route.next_hop {
        attribute(&mut body, RTA_GATEWAY, &router.octets());
    }
    body
}

/// A route message the kernel sent, read back. Anything but IPv4 is none.
fn parse_route(body: &[u8]) -> Option<Route> {
    if body.len() < RTMSG || body[0] != libc::AF_INET as u8 {
        return None;
    }
    let mut route = Route {
        destination: Ipv4Addr::UNSPECIFIED,
        prefix: body[1],
        index: 0,
        next_hop: None,
        protocol: body[5],
        table: body[4],
    };
    for (kind, data) in attributes(&body[RTMSG..]) {
        match (kind, data.len()) {
            (RTA_DST, 4) => route.destination = Ipv4Addr::new(data[0], data[1], data[2], data[3]),
            (RTA_GATEWAY, 4) => {
                route.next_hop = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
            }
            (RTA_OIF, 4) => route.index = u32::from_ne_bytes(data.try_into().ok()?),
            // The table's number in full, where it does not fit the header's byte.
            (RTA_TABLE, 4) => {
                let table = u32::from_ne_bytes(data.try_into().ok()?);
                route.table = u8::try_from(table).unwrap_or(0);
            }
            _ => {}
        }
    }
    Some(route)
}

/// The attributes in a run of them, as (type, bytes).
fn attributes(mut rest: &[u8]) -> Vec<(u16, &[u8])> {
    let mut found = Vec::new();
    while rest.len() >= 4 {
        let len = usize::from(u16::from_ne_bytes([rest[0], rest[1]]));
        let kind = u16::from_ne_bytes([rest[2], rest[3]]) & 0x3fff;
        if len < 4 || len > rest.len() {
            break;
        }
        found.push((kind, &rest[4..len]));
        rest = &rest[aligned(len).min(rest.len())..];
    }
    found
}

/// The messages in one datagram from the kernel, as (type, body).
fn messages(mut rest: &[u8]) -> Vec<(u16, &[u8])> {
    let mut found = Vec::new();
    while rest.len() >= HEADER {
        let len = u32::from_ne_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let kind = u16::from_ne_bytes([rest[4], rest[5]]);
        if len < HEADER || len > rest.len() {
            break;
        }
        found.push((kind, &rest[HEADER..len]));
        rest = &rest[aligned(len).min(rest.len())..];
    }
    found
}

/// A `NETLINK_ROUTE` socket, which a request at a time is sent over.
struct Netlink {
    socket: OwnedFd,
    sequence: u32,
}

/// The most a reply datagram may be: a dump of a full table comes in many.
const DATAGRAM: usize = 64 * 1024;

impl Netlink {
    fn open() -> io::Result<Self> {
        // SAFETY: a plain socket(2); the descriptor is owned at once.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            socket: unsafe { OwnedFd::from_raw_fd(raw) },
            sequence: 0,
        })
    }

    /// Send one request and gather every message of its answer.
    fn ask(&mut self, kind: u16, flags: u16, body: &[u8]) -> io::Result<Vec<(u16, Vec<u8>)>> {
        self.sequence = self.sequence.wrapping_add(1);
        let request = message(kind, NLM_F_REQUEST | flags, self.sequence, body);
        // SAFETY: a zeroed sockaddr_nl names the kernel (pid 0).
        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let sent = unsafe {
            libc::sendto(
                self.socket.as_raw_fd(),
                request.as_ptr().cast(),
                request.len(),
                0,
                (&raw const kernel).cast(),
                size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }

        let dump = flags & NLM_F_DUMP == NLM_F_DUMP;
        let mut answer = Vec::new();
        let mut buffer = vec![0u8; DATAGRAM];
        loop {
            // SAFETY: reads into the buffer, at most its length.
            let n = unsafe {
                libc::recv(
                    self.socket.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            for (kind, body) in messages(&buffer[..n as usize]) {
                match kind {
                    NLMSG_DONE => return Ok(answer),
                    NLMSG_ERROR => {
                        let code = body
                            .get(..4)
                            .map(|b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
                            .unwrap_or(-libc::EIO);
                        // 0 is the acknowledgement of a change.
                        return if code == 0 {
                            Ok(answer)
                        } else {
                            Err(io::Error::from_raw_os_error(-code))
                        };
                    }
                    _ => answer.push((kind, body.to_vec())),
                }
            }
            // A lookup is one message and no more.
            if !dump && !answer.is_empty() {
                return Ok(answer);
            }
        }
    }

    /// Add or take away one route. An add that meets a route of the same
    /// prefix and metric is refused (`EEXIST`), as `ip route add` is: that
    /// route is another program's, and is not bliss's to replace.
    fn change(&mut self, kind: u16, route: &Route) -> io::Result<()> {
        let flags = if kind == RTM_NEWROUTE {
            NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL
        } else {
            NLM_F_ACK
        };
        self.ask(kind, flags, &route_body(route)).map(|_| ())
    }

    /// Which way the kernel would send a packet to `address` right now.
    fn way_to(&mut self, address: Ipv4Addr) -> io::Result<Option<Gateway>> {
        let mut body = vec![libc::AF_INET as u8, 32, 0, 0, 0, 0, 0, 0];
        body.extend_from_slice(&0u32.to_ne_bytes());
        attribute(&mut body, RTA_DST, &address.octets());
        let answer = match self.ask(RTM_GETROUTE, 0, &body) {
            Ok(answer) => answer,
            // No way there at all is an answer, not a failure.
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::ENETUNREACH | libc::EHOSTUNREACH)
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        Ok(answer
            .iter()
            .filter(|(kind, _)| *kind == RTM_NEWROUTE)
            .find_map(|(_, body)| parse_route(body))
            .filter(|route| route.index != 0)
            .map(|route| Gateway {
                index: route.index,
                next_hop: route.next_hop,
            }))
    }

    /// Every IPv4 route in every table.
    fn routes(&mut self) -> io::Result<Vec<Route>> {
        let mut body = vec![libc::AF_INET as u8, 0, 0, 0, 0, 0, 0, 0];
        body.extend_from_slice(&0u32.to_ne_bytes());
        let answer = self.ask(RTM_GETROUTE, NLM_F_DUMP, &body)?;
        Ok(answer
            .iter()
            .filter(|(kind, _)| *kind == RTM_NEWROUTE)
            .filter_map(|(_, body)| parse_route(body))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn around() -> Route {
        Route {
            destination: Ipv4Addr::new(203, 0, 113, 7),
            prefix: 32,
            index: 3,
            next_hop: Some(Ipv4Addr::new(192, 168, 1, 1)),
            protocol: PROTOCOL,
            table: RT_TABLE_MAIN,
        }
    }

    /// A route reads back as it was written: the kernel answers a dump in the
    /// same shape a change is asked in.
    #[test]
    fn a_route_reads_back_as_written() {
        let route = around();
        assert_eq!(parse_route(&route_body(&route)), Some(route));
        let through = Route {
            destination: Ipv4Addr::new(128, 0, 0, 0),
            prefix: 1,
            index: 9,
            next_hop: None,
            protocol: PROTOCOL,
            table: RT_TABLE_MAIN,
        };
        let body = route_body(&through);
        assert_eq!(
            body[6], RT_SCOPE_LINK,
            "a route into the device is on the link"
        );
        assert_eq!(parse_route(&body), Some(through));
    }

    /// The octets land in the order they are written: a byte-swapped address
    /// is a route to somewhere else entirely.
    #[test]
    fn an_address_keeps_its_order() {
        let body = route_body(&around());
        let attrs = attributes(&body[RTMSG..]);
        assert_eq!(attrs[0], (RTA_DST, &[203, 0, 113, 7][..]));
        assert_eq!(attrs[2], (RTA_GATEWAY, &[192, 168, 1, 1][..]));
    }

    /// Messages are walked by their own lengths, padded to four bytes; a
    /// length that runs past the datagram ends the walk instead of reading
    /// beyond it.
    #[test]
    fn messages_are_walked_by_their_lengths() {
        let mut datagram = message(RTM_NEWROUTE, 0, 1, &route_body(&around()));
        datagram.extend(message(NLMSG_DONE, 0, 1, &[0, 0, 0, 0]));
        let found = messages(&datagram);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, RTM_NEWROUTE);
        assert_eq!(parse_route(found[0].1), Some(around()));
        assert_eq!(found[1].0, NLMSG_DONE);

        let mut broken = datagram.clone();
        broken[0] = 0xff;
        assert!(messages(&broken).is_empty(), "a length past the end");
        assert!(messages(&datagram[..10]).is_empty(), "half a header");
    }

    /// The kernel's answer to a lookup, unprivileged: loopback is reached by
    /// its own interface and no router.
    #[test]
    fn the_kernel_says_which_way_loopback_is() {
        let way = Netlink::open()
            .expect("a netlink socket")
            .way_to(Ipv4Addr::LOCALHOST)
            .expect("the kernel answers");
        let lo = bliss_core::interface::index_of(Ipv4Addr::LOCALHOST)
            .expect("the addresses read")
            .expect("loopback has one");
        assert_eq!(
            way,
            Some(Gateway {
                index: lo,
                next_hop: None
            })
        );
    }

    /// And a dump of the machine's routes reads, unprivileged too.
    #[test]
    fn the_routes_can_be_listed() {
        let routes = Netlink::open()
            .expect("a netlink socket")
            .routes()
            .expect("the kernel lists them");
        assert!(
            routes
                .iter()
                .any(|route| route.destination == Ipv4Addr::new(127, 0, 0, 0)
                    || route.destination == Ipv4Addr::new(127, 0, 0, 1)),
            "loopback's own routes are on every machine: {routes:?}"
        );
    }
}
