//! Leaving past the tunnel on Android: nothing to do.
//!
//! On Windows and Linux a connection bliss dials for a `direct` rule has to be
//! bound to the real interface, or the routing table sends it back into the
//! adapter. On Android the service leaves its own app out of the VPN
//! (`addDisallowedApplication(packageName)` in TunnelService.kt), so every
//! socket bliss opens — its direct dials and the engine's connection to the
//! server alike — already uses the network as if there were no VPN.

use std::io;

/// Where a direct connection leaves by: the system's choice, which is already
/// the right one for an app outside its own VPN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gateway;

impl std::fmt::Display for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the system's network")
    }
}

/// Nothing to bind: see the module's note.
pub fn outside<S>(_socket: &S, _gateway: Gateway) -> io::Result<()> {
    Ok(())
}
