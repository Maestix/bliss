//! The adapter's address, and the routes that steer traffic into it.
//!
//! Through IP Helper rather than `netsh` or `route.exe`. The wintun bindings
//! shell out for their own address helpers, which means a spawned process, a
//! localised error message and no way to tell "already there" from "denied";
//! these calls answer with a Win32 code and nothing has to be parsed.
//!
//! IPv4 only for now. IPv6 is either carried the same way or blocked outright,
//! and blocking it belongs with the firewall work rather than here — until
//! then a machine with working IPv6 reaches an IPv6 site around the tunnel.

use std::io;
use std::net::Ipv4Addr;

use windows::Win32::Foundation::{ERROR_OBJECT_ALREADY_EXISTS, NO_ERROR, WIN32_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    CreateIpForwardEntry2, CreateUnicastIpAddressEntry, DeleteIpForwardEntry2, FreeMibTable,
    GetIpForwardTable2, InitializeIpForwardEntry, InitializeUnicastIpAddressEntry,
    MIB_IPFORWARD_ROW2, MIB_IPFORWARD_TABLE2, MIB_UNICASTIPADDRESS_ROW,
};
use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows::Win32::Networking::WinSock::{
    AF_INET, IN_ADDR, IN_ADDR_0, IP_UNICAST_IF, IPPROTO_IP, IpDadStatePreferred,
    IpPrefixOriginManual, IpSuffixOriginManual, SOCKADDR_IN, SOCKADDR_INET, SOCKET, SOCKET_ERROR,
    WSAGetLastError, setsockopt,
};

/// Which interface, in the one form both libraries agree on.
///
/// wintun-bindings hands out `windows_sys`' LUID and IP Helper here is the
/// `windows` crate's: the same union over the same eight bytes, declared twice,
/// so the number travels between them and the type does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interface(pub u64);

impl Interface {
    fn luid(self) -> NET_LUID_LH {
        NET_LUID_LH { Value: self.0 }
    }
}

/// Give the adapter an address, which is what makes it routable at all.
///
/// The address itself is barely used: nothing on the far side answers, and the
/// stack in `stack.rs` replies for whatever bliss carries. Windows still needs
/// one before it will install a route through the interface.
pub fn set_address(interface: Interface, address: Ipv4Addr, prefix: u8) -> io::Result<()> {
    let mut row = MIB_UNICASTIPADDRESS_ROW::default();
    // Fills in the lifetimes and the flags; the documented way to build one.
    unsafe { InitializeUnicastIpAddressEntry(&mut row) };
    row.InterfaceLuid = interface.luid();
    row.Address = socket_address(address);
    row.OnLinkPrefixLength = prefix;
    row.PrefixOrigin = IpPrefixOriginManual;
    row.SuffixOrigin = IpSuffixOriginManual;
    // Without this the address spends a few seconds in duplicate-address
    // detection, during which a route through it cannot be added.
    row.DadState = IpDadStatePreferred;

    match unsafe { CreateUnicastIpAddressEntry(&row) } {
        NO_ERROR | ERROR_OBJECT_ALREADY_EXISTS => Ok(()),
        e => Err(failed(e, "could not give the adapter its address")),
    }
}

/// The default route the machine had before bliss touched anything.
///
/// The one thing that must *not* go through the tunnel is the tunnel itself:
/// the engine's connection to the server has to leave by the ordinary route, or
/// its own packets come back through the adapter and the tunnel carries itself.
#[derive(Debug, Clone, Copy)]
pub struct Gateway {
    interface: Interface,
    /// The same interface as a number, which is what a socket is told to leave
    /// by; a LUID will not do there.
    index: u32,
    next_hop: Ipv4Addr,
}

impl std::fmt::Display for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.next_hop)
    }
}

/// Find it, ignoring the adapter bliss just made.
///
/// Lowest metric wins, which is how Windows itself picks between several: a
/// machine on Wi-Fi and Ethernet at once has a default route on each.
pub fn gateway(except: Interface) -> io::Result<Option<Gateway>> {
    let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    match unsafe { GetIpForwardTable2(AF_INET, &mut table) } {
        NO_ERROR => {}
        e => return Err(failed(e, "could not read the routing table")),
    }
    if table.is_null() {
        return Ok(None);
    }

    // The table is one allocation: a count and then that many rows past it.
    let rows = unsafe {
        std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
    };
    let mut best: Option<(u32, Gateway)> = None;
    for row in rows {
        if row.DestinationPrefix.PrefixLength != 0 {
            continue;
        }
        let interface = Interface(unsafe { row.InterfaceLuid.Value });
        if interface == except {
            continue;
        }
        let Some(next_hop) = ipv4_of(&row.NextHop) else {
            continue;
        };
        if best.is_none_or(|(metric, _)| row.Metric < metric) {
            best = Some((
                row.Metric,
                Gateway {
                    interface,
                    index: row.InterfaceIndex,
                    next_hop,
                },
            ));
        }
    }
    unsafe { FreeMibTable(table as *const _) };

    Ok(best.map(|(_, gateway)| gateway))
}

/// The routes bliss added, and takes away again.
///
/// Held rather than remembered on disk, which the system proxy needs and this
/// does not: the routes into the adapter die with the adapter, and the adapter
/// dies with the process, however it dies. The one route on another interface
/// is the one around the tunnel, and a stale one of those points at the
/// gateway traffic would have used anyway.
#[derive(Default)]
pub struct Routes(Vec<MIB_IPFORWARD_ROW2>);

impl Routes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send a whole prefix into the adapter.
    ///
    /// The two halves `0.0.0.0/1` and `128.0.0.0/1` are how this is done rather
    /// than replacing the default route: a longer prefix wins on its own, so
    /// nothing has to be taken away and put back, and a bliss that dies leaves
    /// the machine's own default route where it was.
    pub fn through_tunnel(
        &mut self,
        interface: Interface,
        destination: Ipv4Addr,
        prefix: u8,
    ) -> io::Result<()> {
        // On-link: the adapter is the far end, and there is nothing to send it
        // to first.
        self.add(interface, destination, prefix, Ipv4Addr::UNSPECIFIED)
    }

    /// Send one address out the way everything used to go.
    pub fn around_tunnel(&mut self, gateway: Gateway, host: Ipv4Addr) -> io::Result<()> {
        self.add(gateway.interface, host, 32, gateway.next_hop)
    }

    fn add(
        &mut self,
        interface: Interface,
        destination: Ipv4Addr,
        prefix: u8,
        next_hop: Ipv4Addr,
    ) -> io::Result<()> {
        let mut row = MIB_IPFORWARD_ROW2::default();
        unsafe { InitializeIpForwardEntry(&mut row) };
        row.InterfaceLuid = interface.luid();
        row.DestinationPrefix.Prefix = socket_address(destination);
        row.DestinationPrefix.PrefixLength = prefix;
        row.NextHop = socket_address(next_hop);
        // Added to the interface's own metric, so 0 means "as good as this
        // interface gets" rather than "better than everything".
        row.Metric = 0;

        match unsafe { CreateIpForwardEntry2(&row) } {
            // Already there: a copy of bliss that died without tidying up, and
            // the row is ours either way, so it goes on the list to be removed.
            NO_ERROR | ERROR_OBJECT_ALREADY_EXISTS => {
                self.0.push(row);
                Ok(())
            }
            e => Err(failed(
                e,
                &format!("could not route {destination}/{prefix}"),
            )),
        }
    }
}

impl Drop for Routes {
    fn drop(&mut self) {
        for row in &self.0 {
            // A route into an adapter that has already gone is gone with it,
            // which is not a failure worth a line in the log.
            let _ = unsafe { DeleteIpForwardEntry2(row) };
        }
    }
}

/// Send this socket out the way traffic left before the tunnel existed.
///
/// A connection bliss makes for itself is decided by the same routing table as
/// everyone else's, and bliss has just pointed all of it at the adapter: a
/// route decided `direct` and then dialled normally would come straight back
/// in through the TUN, be decided `direct` again, and dial again, for as long
/// as the machine had sockets. `IP_UNICAST_IF` is the one setting that
/// overrides the lookup and names the interface to leave by.
///
/// The engine's own connection to the server is kept out by a route instead
/// (`around_tunnel`), because the engine is another process and this is a
/// setting on a socket.
pub fn outside(
    socket: &impl std::os::windows::io::AsRawSocket,
    gateway: Gateway,
) -> io::Result<()> {
    let index = interface_option(gateway.index);
    let handle = SOCKET(socket.as_raw_socket() as usize);
    if unsafe { setsockopt(handle, IPPROTO_IP.0, IP_UNICAST_IF, Some(&index)) } == SOCKET_ERROR {
        let code = unsafe { WSAGetLastError() }.0;
        return Err(io::Error::new(
            io::Error::from_raw_os_error(code).kind(),
            format!(
                "could not send the connection out by interface {}: {}",
                gateway.index,
                io::Error::from_raw_os_error(code)
            ),
        ));
    }
    Ok(())
}

/// The interface index as `IP_UNICAST_IF` wants it.
///
/// In network byte order, which is true of this option and of almost nothing
/// else on a socket; the IPv6 spelling of the same option takes the index the
/// usual way round. Swapped, it names an interface that does not exist, and
/// the call fails with "the requested address is not valid in its context" —
/// an error that says nothing about byte order.
fn interface_option(index: u32) -> [u8; 4] {
    index.to_be_bytes()
}

/// An address in the shape the IP Helper rows want.
fn socket_address(address: Ipv4Addr) -> SOCKADDR_INET {
    let mut socket = SOCKADDR_INET::default();
    socket.Ipv4 = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: 0,
        sin_addr: IN_ADDR {
            // Network order, which is the order the octets are written in.
            S_un: IN_ADDR_0 {
                S_addr: u32::from_ne_bytes(address.octets()),
            },
        },
        sin_zero: [0; 8],
    };
    socket
}

/// The IPv4 address in one of those rows, if that is what it holds.
fn ipv4_of(socket: &SOCKADDR_INET) -> Option<Ipv4Addr> {
    if unsafe { socket.si_family } != AF_INET {
        return None;
    }
    Some(Ipv4Addr::from(unsafe {
        socket.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()
    }))
}

/// A Win32 error with the call that produced it, since the code alone reads as
/// "access denied" with no hint of what was denied.
fn failed(error: WIN32_ERROR, what: &str) -> io::Error {
    io::Error::new(
        io::Error::from_raw_os_error(error.0 as i32).kind(),
        format!("{what}: {}", io::Error::from_raw_os_error(error.0 as i32)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The octets land in the order they are written. A byte-swapped address
    /// is a route to somewhere else entirely, and both calls that take one are
    /// too slow and too privileged to find that out by trying.
    #[test]
    fn an_address_keeps_its_order() {
        let socket = socket_address(Ipv4Addr::new(203, 0, 113, 7));
        assert_eq!(ipv4_of(&socket), Some(Ipv4Addr::new(203, 0, 113, 7)));
        assert_eq!(
            unsafe { socket.Ipv4.sin_addr.S_un.S_addr }.to_ne_bytes(),
            [203, 0, 113, 7]
        );
    }

    /// The index goes out in network byte order. Written the usual way round
    /// it names another interface entirely, and the connection either leaves
    /// by the wrong one or fails for a reason that mentions addresses.
    #[test]
    fn the_interface_index_travels_backwards() {
        assert_eq!(interface_option(7), [0, 0, 0, 7]);
        assert_eq!(interface_option(0x0102_0304), [1, 2, 3, 4]);
    }

    /// An address of another family is not read as an IPv4 one: the routing
    /// table holds both, and the IPv6 rows are skipped by this alone.
    #[test]
    fn an_address_of_another_family_is_not_ipv4() {
        let mut socket = SOCKADDR_INET::default();
        socket.si_family = windows::Win32::Networking::WinSock::AF_INET6;
        assert_eq!(ipv4_of(&socket), None);
    }
}
