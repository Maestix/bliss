//! Config types shared by the bliss core and its UI.
//!
//! Everything here is pure data: parsing, validation and round-tripping of
//! server share links and of our own TOML profile. No I/O, no sockets.

mod encryption;
mod error;
mod link;
mod profile;
mod redact;
mod routing;
mod server;

pub use encryption::{AppearanceMode, Encryption, EncryptionKey, RttMode, VlessEncryption};
pub use error::{LinkError, Unsupported};
pub use link::is_server_scheme;
pub use profile::{Engine, Geo, Inbound, Profile, ProxyMode, Subscription, Tun, Usage, Vpn};
pub use routing::{Action, Preset, Routing, Rule};
pub use server::{
    Address, Fingerprint, Flow, Hidden, KcpHeader, Login, Long, PrivateKey, Protocol, PublicKey,
    Reality, Security, ServerConfig, ServerEntry, ShortId, TcpHeader, Tls, Transport, UserId,
    VmessCipher, WireGuard, link_key,
};
