//! Leaving by one interface of the machine, whatever the routing table says.
//!
//! Two kinds of connection need it. With the TUN adapter up, the routing table
//! sends everything into bliss — including the connections bliss dials itself
//! for a `direct` rule, which have to leave by the interface the machine used
//! before (`bliss_tun`'s `net::outside`). And a connection a rule sends through
//! the VPN has to leave by the VPN's adapter, which the routing table keeps out
//! of every other program's way on purpose (`Through`).

use std::io;
use std::net::Ipv4Addr;

pub use imp::{index_of, leave_by};

#[cfg(windows)]
mod imp {
    use std::io;
    use std::net::Ipv4Addr;

    use windows::Win32::Foundation::NO_ERROR;
    use windows::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetUnicastIpAddressTable, MIB_UNICASTIPADDRESS_TABLE,
    };
    use windows::Win32::Networking::WinSock::{
        AF_INET, IP_UNICAST_IF, IPPROTO_IP, SOCKET, SOCKET_ERROR, WSAGetLastError, setsockopt,
    };

    /// Send this socket's traffic out by interface `index`.
    ///
    /// `IP_UNICAST_IF` is the one socket option that overrides the routing
    /// table's choice of interface. Windows still looks for a route, but only
    /// among the routes through that interface — so the interface needs one
    /// that covers the destination, if only a route nobody else would pick.
    pub fn leave_by(socket: &impl std::os::windows::io::AsRawSocket, index: u32) -> io::Result<()> {
        let option = super::interface_option(index);
        let handle = SOCKET(socket.as_raw_socket() as usize);
        if unsafe { setsockopt(handle, IPPROTO_IP.0, IP_UNICAST_IF, Some(&option)) } == SOCKET_ERROR
        {
            let code = unsafe { WSAGetLastError() }.0;
            let error = io::Error::from_raw_os_error(code);
            return Err(io::Error::new(
                error.kind(),
                format!("could not send the connection out by interface {index}: {error}"),
            ));
        }
        Ok(())
    }

    /// The interface that holds `address`, by the number a socket is told to
    /// leave by.
    ///
    /// How the VPN's adapter is found: OpenVPN says which address it was
    /// given, not which adapter it put it on, and an adapter's number can
    /// change every time it is created.
    pub fn index_of(address: Ipv4Addr) -> io::Result<Option<u32>> {
        let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
        let status = unsafe { GetUnicastIpAddressTable(AF_INET, &mut table) };
        if status != NO_ERROR {
            let error = io::Error::from_raw_os_error(status.0 as i32);
            return Err(io::Error::new(
                error.kind(),
                format!("could not read the machine's addresses: {error}"),
            ));
        }
        if table.is_null() {
            return Ok(None);
        }
        // One allocation: a count, then that many rows past it.
        let rows = unsafe {
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
        };
        let found = rows
            .iter()
            .find(|row| {
                let family = unsafe { row.Address.si_family };
                // Network order, which is the order the octets are written in.
                family == AF_INET
                    && Ipv4Addr::from(
                        unsafe { row.Address.Ipv4.sin_addr.S_un.S_addr }.to_ne_bytes(),
                    ) == address
            })
            .map(|row| row.InterfaceIndex);
        unsafe { FreeMibTable(table as *const _) };
        Ok(found)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::net::Ipv4Addr;

    pub fn leave_by<S>(_socket: &S, _index: u32) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "choosing an interface is written for Windows only",
        ))
    }

    pub fn index_of(_address: Ipv4Addr) -> io::Result<Option<u32>> {
        Ok(None)
    }
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

/// The error for a VPN connection with no VPN under it.
pub(crate) fn no_vpn() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "рабочий VPN не подключён")
}

/// Nothing but IPv4 leaves by the VPN yet.
pub(crate) fn only_ipv4(address: std::net::IpAddr) -> io::Result<Ipv4Addr> {
    match address {
        std::net::IpAddr::V4(v4) => Ok(v4),
        std::net::IpAddr::V6(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "через VPN пока ходит только IPv4",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index goes out in network byte order. Written the usual way round
    /// it names another interface entirely, and the connection either leaves
    /// by the wrong one or fails for a reason that mentions addresses.
    #[test]
    fn the_interface_index_travels_backwards() {
        assert_eq!(interface_option(7), [0, 0, 0, 7]);
        assert_eq!(interface_option(0x0102_0304), [1, 2, 3, 4]);
    }

    /// Loopback is on every machine, and on its own interface.
    #[cfg(windows)]
    #[test]
    fn loopback_has_an_interface() {
        let index = index_of(Ipv4Addr::LOCALHOST).expect("the table reads");
        assert!(index.is_some_and(|i| i > 0), "{index:?}");
        assert_eq!(
            index_of(Ipv4Addr::new(203, 0, 113, 250)).expect("the table reads"),
            None,
            "a TEST-NET address is on no interface here"
        );
    }
}
