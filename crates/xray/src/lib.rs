//! Driving an external xray-core.
//!
//! Everything that knows xray exists lives in this crate. The rest of bliss
//! reaches the network through a plain SOCKS dialer, so replacing this with our
//! own engine in stage 3 changes one implementation and nothing else.

mod config;
mod download;
mod job;
mod process;

pub use config::{LogLevel, Version, build, minimum_xray_version, require_login};
pub use download::{Channel, DownloadError, ENGINE_FILE, Release, client, install, list, select};
pub use process::{Xray, XrayError, check_version};
