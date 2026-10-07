//! A VPN beside the tunnel: OpenVPN, for a work network's own profile.
//!
//! bliss does not speak OpenVPN. It drives the official client, which a
//! workplace's profile is written for and which every option in it works
//! with: OpenVPN's interactive service starts `openvpn.exe` without
//! administrator rights (`service`), bliss rewrites the profile so the server
//! gets neither the machine's routes nor its DNS (`profile`), and it follows
//! the connection over OpenVPN's management interface (`management`,
//! `session`). What comes out is an address on the work network and the DNS
//! servers there; `bliss_core::Through` dials through that address for the
//! connections a rule sends to the VPN.
//!
//! The seam holds as it does for xray: nothing outside the `bliss` binary
//! knows OpenVPN exists.

pub mod management;
pub mod profile;
pub mod service;
mod session;

pub use profile::{MAX_PROFILE, ProfileError, looks_like, rewrite};
pub use service::{Availability, availability};
pub use session::{Event, Leaving, Session};
