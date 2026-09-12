//! Config types shared by the bliss core and its UI.
//!
//! Everything here is pure data: parsing, validation and round-tripping of
//! `vless://` share links and of our own TOML profile. No I/O, no sockets.

mod encryption;
mod error;
mod profile;
mod redact;
mod server;

pub use encryption::{AppearanceMode, Encryption, EncryptionKey, RttMode, VlessEncryption};
pub use error::LinkError;
pub use profile::{Engine, Inbound, Profile, ProxyMode};
pub use server::{
    Address, Fingerprint, Flow, PublicKey, Reality, Security, ServerConfig, ServerEntry, ShortId,
    TcpHeader, Tls, Transport, UserId,
};
