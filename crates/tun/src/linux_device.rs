//! The adapter on Linux: a TUN device of the kernel's own, read and written
//! as a file.
//!
//! Nothing to carry: the driver is in the kernel. `/dev/net/tun` is opened,
//! and one `TUNSETIFF` names the device and makes it a TUN — bare IP packets,
//! no Ethernet frames — without the four-byte header the kernel would put in
//! front of each (`IFF_NO_PI`). Every read is then one packet and every write
//! sends one, which is what the stack in `stack.rs` reads and writes.
//!
//! Creating it takes `CAP_NET_ADMIN`: bliss run as root, or, better, given the
//! one capability with `setcap cap_net_admin+ep` on its executable. And like
//! Windows' adapter, the device lives exactly as long as its descriptor:
//! however the process ends, the kernel closes it, unregisters the device and
//! drops every route through it.

use std::ffi::CStr;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::net::Interface;

/// What the device is called in `ip link`: the name a person sees when they
/// wonder what this is. The same as Windows' adapter.
const NAME: &str = "bliss";

/// How much of an IP packet fits; the stack's own MTU is set from this, so
/// the two agree.
pub const MTU: u16 = 1500;

/// The device's own address, and how much of the space around it is its own:
/// the same as Windows' (see `device.rs` there for why this range).
pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 27, 0, 2);
pub const PREFIX: u8 = 24;

/// A device that exists for as long as its packets are held.
pub struct Device {
    index: u32,
    packets: Option<Packets>,
}

impl Device {
    /// Make the device, size it and give it its address, and bring it up.
    pub fn open() -> io::Result<Self> {
        // SAFETY: a NUL-terminated path; the descriptor, when there is one,
        // is owned below.
        let raw = unsafe {
            libc::open(
                c"/dev/net/tun".as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if raw < 0 {
            let error = io::Error::last_os_error();
            return Err(io::Error::new(
                error.kind(),
                format!("could not open /dev/net/tun: {error}"),
            ));
        }
        // SAFETY: just opened, and owned by nothing else.
        let file = unsafe { OwnedFd::from_raw_fd(raw) };

        let mut request = request(NAME);
        request.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
        // SAFETY: TUNSETIFF takes an ifreq, which outlives the call.
        if unsafe { libc::ioctl(file.as_raw_fd(), libc::TUNSETIFF, &mut request) } < 0 {
            return Err(denied(
                io::Error::last_os_error(),
                "could not create the bliss device",
            ));
        }
        // The kernel may have named it otherwise; it writes back the name.
        let name = unsafe { CStr::from_ptr(request.ifr_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();

        let control = control_socket()?;
        let mut sized = self::request(&name);
        sized.ifr_ifru.ifru_mtu = libc::c_int::from(MTU);
        ioctl(
            &control,
            libc::SIOCSIFMTU,
            &mut sized,
            "could not set the device's MTU",
        )?;

        // SAFETY: a NUL-terminated name in a buffer of the size asked for.
        let index = unsafe { libc::if_nametoindex(request.ifr_name.as_ptr()) };
        if index == 0 {
            return Err(io::Error::other(format!(
                "the device {name} has no interface index"
            )));
        }

        // SAFETY: an `OwnedFd` stays open as long as it lives and always
        // names the same descriptor, and the `AsyncFd` owns it from here.
        // (tokio 1.54 made the constructor `unsafe` to say so: RUSTSEC-2026-0386.)
        let packets = unsafe { AsyncFd::register(file) }.map_err(|e| e.into_parts().1)?;
        Ok(Self {
            index,
            packets: Some(Packets(packets)),
        })
    }

    /// Which interface it is, for addresses and routes.
    pub fn interface(&self) -> Interface {
        Interface(self.index)
    }

    /// The packets themselves, as something to read and write. Once: the
    /// stack owns them, and the device goes when the stack lets them go.
    pub fn packets(&mut self) -> Option<Packets> {
        self.packets.take()
    }
}

/// Give the device its address and bring it up, which is what makes it
/// routable at all.
pub fn set_address(interface: Interface, address: Ipv4Addr, prefix: u8) -> io::Result<()> {
    let mut name = [0 as libc::c_char; libc::IF_NAMESIZE];
    // SAFETY: a buffer of IF_NAMESIZE, as if_indextoname asks.
    if unsafe { libc::if_indextoname(interface.0, name.as_mut_ptr()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    let name = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let control = control_socket()?;

    let mut at = request(&name);
    at.ifr_ifru.ifru_addr = socket_address(address);
    ioctl(
        &control,
        libc::SIOCSIFADDR,
        &mut at,
        "could not give the device its address",
    )?;

    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    };
    let mut netmask = request(&name);
    netmask.ifr_ifru.ifru_netmask = socket_address(Ipv4Addr::from(mask));
    ioctl(
        &control,
        libc::SIOCSIFNETMASK,
        &mut netmask,
        "could not set the device's netmask",
    )?;

    let mut flags = request(&name);
    ioctl(
        &control,
        libc::SIOCGIFFLAGS,
        &mut flags,
        "could not read the device's flags",
    )?;
    // SAFETY: SIOCGIFFLAGS filled the flags member of the union.
    unsafe { flags.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short };
    ioctl(
        &control,
        libc::SIOCSIFFLAGS,
        &mut flags,
        "could not bring the device up",
    )
}

/// An `ifreq` naming the device, the rest zeroed.
fn request(name: &str) -> libc::ifreq {
    // SAFETY: ifreq is plain data, and all zeroes is a valid one.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request
        .ifr_name
        .iter_mut()
        .zip(name.bytes().take(libc::IF_NAMESIZE - 1))
    {
        *slot = byte as libc::c_char;
    }
    request
}

fn socket_address(address: Ipv4Addr) -> libc::sockaddr {
    let v4 = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        // Network order, which is the order the octets are written in.
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(address.octets()),
        },
        sin_zero: [0; 8],
    };
    // SAFETY: sockaddr_in and sockaddr are the same size, and the kernel
    // reads this one as the family it says.
    unsafe { std::mem::transmute::<libc::sockaddr_in, libc::sockaddr>(v4) }
}

/// The socket interface ioctls are asked through.
fn control_socket() -> io::Result<OwnedFd> {
    // SAFETY: a plain socket(2); the descriptor is owned at once.
    let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn ioctl(
    socket: &OwnedFd,
    request: libc::c_ulong,
    data: &mut libc::ifreq,
    what: &str,
) -> io::Result<()> {
    // SAFETY: every request here takes an ifreq, which outlives the call.
    if unsafe { libc::ioctl(socket.as_raw_fd(), request as _, data) } < 0 {
        return Err(denied(io::Error::last_os_error(), what));
    }
    Ok(())
}

/// An error, with what was being done, and what to do about a refusal: the
/// one capability the device needs.
fn denied(error: io::Error, what: &str) -> io::Error {
    let hint = if error.raw_os_error() == Some(libc::EPERM) {
        " — this needs CAP_NET_ADMIN: run `sudo setcap cap_net_admin+ep` on bliss's executable"
    } else {
        ""
    };
    io::Error::new(error.kind(), format!("{what}: {error}{hint}"))
}

/// The device's packets: one read, one packet; one write, one packet.
pub struct Packets(AsyncFd<OwnedFd>);

impl AsyncRead for Packets {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut ready = ready!(self.0.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            let read = ready.try_io(|fd| {
                // SAFETY: reads into the unfilled part of the buffer, at most
                // its length.
                let n = unsafe {
                    libc::read(fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match read {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for Packets {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        packet: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut ready = ready!(self.0.poll_write_ready(cx))?;
            let written = ready.try_io(|fd| {
                // SAFETY: writes the packet, which outlives the call.
                let n =
                    unsafe { libc::write(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match written {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Whether this process may create the device: `CAP_NET_ADMIN` in its
/// effective set, read from `/proc/self/status` — bit 12 of `CapEff`. A
/// capability held over a user namespace of its own passes this and still
/// cannot make a device the machine routes through; the creation's own
/// refusal is the last word.
pub fn capable() -> bool {
    std::fs::read_to_string("/proc/self/status").is_ok_and(|status| net_admin(&status))
}

/// CAP_NET_ADMIN's bit in `CapEff`.
fn net_admin(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|bits| u64::from_str_radix(bits.trim(), 16).ok())
        .is_some_and(|bits| bits & (1 << 12) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capability_is_read_from_the_effective_set() {
        let none = "Name:\tbliss\nCapInh:\t0000000000000000\nCapPrm:\t0000000000001000\n\
                    CapEff:\t0000000000000000\n";
        assert!(!net_admin(none), "permitted is not effective");
        let setcap = "CapPrm:\t0000000000001000\nCapEff:\t0000000000001000\n";
        assert!(net_admin(setcap));
        let root = "CapEff:\t000001ffffffffff\n";
        assert!(net_admin(root));
        assert!(!net_admin(""));
    }

    /// The address has to sit inside the prefix it is given, or the kernel
    /// routes nothing through the device.
    #[test]
    fn the_address_is_inside_its_own_prefix() {
        let mask = u32::MAX << (32 - u32::from(PREFIX));
        assert_eq!(
            u32::from(ADDRESS) & mask,
            u32::from(Ipv4Addr::new(10, 27, 0, 0))
        );
        assert!(ADDRESS.is_private());
    }

    #[test]
    fn a_name_fits_its_slot() {
        let request = request("a-very-long-device-name-indeed");
        assert_eq!(request.ifr_name[libc::IF_NAMESIZE - 1], 0, "NUL-terminated");
        let request = super::request(NAME);
        let written = unsafe { CStr::from_ptr(request.ifr_name.as_ptr()) };
        assert_eq!(written.to_str(), Ok(NAME));
    }
}
