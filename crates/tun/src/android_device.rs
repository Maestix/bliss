//! The adapter on Android: a TUN device the system made for bliss.
//!
//! An app cannot open `/dev/net/tun` there. It asks `VpnService.Builder` for
//! an interface — address, routes, DNS, MTU and the apps left out are all the
//! Builder's — and gets back a descriptor, which the Kotlin side hands to
//! Rust. What is left is the Linux device's own reading and writing: the
//! kernel's TUN, `IFF_NO_PI`, one packet per read and one per write.
//!
//! The descriptor is owned here once handed over: dropping `Packets` closes
//! it, and the system takes the interface down with the last copy.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// How much of an IP packet fits. The Builder is told the same number
/// (`setMtu` in TunnelService.kt), and the stack's own MTU is set from this.
pub const MTU: u16 = 1500;

/// The device's packets: one read, one packet; one write, one packet.
pub struct Packets(AsyncFd<OwnedFd>);

impl Packets {
    /// Take the descriptor `VpnService.Builder.establish()` gave, detached
    /// from its `ParcelFileDescriptor` so that Kotlin no longer closes it.
    pub fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        // SAFETY: plain fcntl calls on a descriptor this function owns.
        unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self(AsyncFd::new(fd)?))
    }
}

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
