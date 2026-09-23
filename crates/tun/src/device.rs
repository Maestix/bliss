//! The adapter: wintun's library, one adapter, and a session read as a stream.

use std::io;
use std::path::Path;

use wintun_bindings::{Adapter, AsyncSession};

/// What the adapter is called in `ipconfig` and in the network connections
/// window. It is the name a person sees when they wonder what this is.
const NAME: &str = "bliss";

/// A GUID of bliss's own, so the adapter keeps its identity between runs.
/// Windows files the network profile, the firewall category and the per-
/// interface settings under it; a fresh GUID every start would ask the person
/// about a new network each time, and leave a trail of dead profiles behind.
const GUID: u128 = 0xb1155000_00000000_00000000_00000001;

/// The ring the driver and bliss pass packets through, in bytes. A power of
/// two between 128 KB and 64 MB; it is allocated for real and counts toward
/// the RSS the budget is about, so this is the middle rather than the top.
const RING: u32 = 2 * 1024 * 1024;

/// How much of an IP packet fits. 1500 minus nothing — wintun carries bare IP
/// frames — and the stack's own MTU is set from this, so the two agree.
pub const MTU: u16 = 1500;

/// An adapter that exists for as long as this is held.
///
/// Wintun removes the adapter when the last handle to it goes, and that
/// includes the process being killed: the routes into it die with it, which is
/// why none of them are written down anywhere (see `net::Routes`).
pub struct Device {
    session: AsyncSession,
    interface: crate::net::Interface,
}

impl Device {
    /// Load the library from `library`, make the adapter, start its session.
    ///
    /// Needs administrator rights: this is where a run without them fails, with
    /// "access denied" and nothing else, which is why the error says so.
    pub fn open(library: &Path) -> io::Result<Self> {
        // SAFETY: loading a DLL runs its entry point, so the file has to be the
        // one bliss means. `place` writes it out of the binary and rewrites it
        // whenever the bytes on disk differ, and the `verify_binary_signature`
        // feature checks WireGuard's signature on it before the first call.
        let wintun = unsafe { wintun_bindings::load_from_path(library) }
            .map_err(|e| io::Error::other(format!("wintun.dll did not load: {e}")))?;

        let adapter = Adapter::create(&wintun, NAME, NAME, Some(GUID)).map_err(|e| {
            io::Error::other(format!(
                "could not create the bliss adapter — this needs administrator rights: {e}"
            ))
        })?;
        adapter
            .set_mtu(MTU as usize)
            .map_err(|e| io::Error::other(format!("could not set the adapter's MTU: {e}")))?;

        // The two libraries declare the same LUID union; only the number
        // crosses between them.
        let interface = crate::net::Interface(unsafe { adapter.get_luid().Value });
        let session = adapter
            .start_session(RING)
            .map_err(|e| io::Error::other(format!("could not start the wintun session: {e}")))?;

        Ok(Self {
            // The session holds the adapter, so both live as long as this does.
            session: AsyncSession::from(session),
            interface,
        })
    }

    /// Which interface it is, for addresses and routes.
    pub fn interface(&self) -> crate::net::Interface {
        self.interface
    }

    /// The packets themselves, as something to read and write.
    pub fn session(&self) -> AsyncSession {
        self.session.clone()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // Ends the session's waiting reader, which is parked on an event and
        // would otherwise hold a thread from the blocking pool until the
        // process exits.
        if let Err(e) = self.session.shutdown() {
            tracing::debug!("tun: the session did not shut down cleanly: {e}");
        }
    }
}

/// The adapter's own address, and how much of the space around it is its own.
///
/// A range out of RFC 1918 that home networks rarely use: 10.0.0.0/8 is wide
/// enough that something had to be picked, and 192.168 and 172.16 are what a
/// router hands out. Nothing answers on this address — the stack answers for
/// everything the adapter is given — but Windows will not route through an
/// interface that has none.
pub const ADDRESS: std::net::Ipv4Addr = std::net::Ipv4Addr::new(10, 27, 0, 2);
pub const PREFIX: u8 = 24;

#[cfg(test)]
mod tests {
    use super::*;

    /// Wintun refuses a ring that is not a power of two inside its range, and
    /// the refusal arrives as a failed start with the tunnel already half up.
    #[test]
    fn the_ring_is_one_wintun_accepts() {
        assert!(RING.is_power_of_two());
        assert!(
            (wintun_bindings::MIN_RING_CAPACITY..=wintun_bindings::MAX_RING_CAPACITY)
                .contains(&RING)
        );
    }

    /// The adapter's own address has to sit inside the prefix it is given, or
    /// Windows routes the adapter's subnet somewhere else entirely.
    #[test]
    fn the_adapter_owns_its_own_subnet() {
        assert_eq!(PREFIX, 24);
        assert!(ADDRESS.is_private());
    }
}
