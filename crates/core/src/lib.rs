//! The half of bliss that does not care which engine carries the bytes.
//!
//! Inbounds, the relay, the connection table and (later) routing all sit above
//! a single trait, [`Outbound`]. Stage 1 implements it by dialing a local
//! xray-core; stage 3 implements it with our own VLESS stack. Nothing else in
//! this crate changes when that happens, which is the whole point.

mod dns;
mod inbound;
mod outbound;
mod process;
mod relay;
mod socks5;
mod target;
mod through;
mod tracker;

pub mod geo;
pub mod http;
pub mod interface;
pub mod routing;
pub mod sysproxy;

pub use inbound::{Bound, Inbound, Serving, serve};
pub use outbound::{
    BoxedDatagram, BoxedStream, DIAL_TIMEOUT, Datagram, EngineInfo, Outbound, SocksOutbound,
};
pub use process::{name_for_datagram_port, name_for_port, process_of};
pub use relay::{relay, relay_datagrams};
pub use routing::{Decision, Router};
pub use target::{Target, TargetError};
pub use through::Through;
pub use tracker::{Connection, ConnectionId, Route, Snapshot, Tracker};
