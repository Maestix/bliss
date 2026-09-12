//! The half of bliss that does not care which engine carries the bytes.
//!
//! Inbounds, the relay, the connection table and (later) routing all sit above
//! a single trait, [`Outbound`]. Stage 1 implements it by dialing a local
//! xray-core; stage 3 implements it with our own VLESS stack. Nothing else in
//! this crate changes when that happens, which is the whole point.

mod inbound;
mod outbound;
mod process;
mod relay;
mod socks5;
mod target;
mod tracker;

pub mod http;

pub use inbound::{Bound, Inbound, serve};
pub use outbound::{BoxedStream, EngineInfo, Outbound, SocksOutbound};
pub use process::name_for_port;
pub use relay::relay;
pub use target::{Target, TargetError};
pub use tracker::{Connection, ConnectionId, Route, Snapshot, Tracker};
