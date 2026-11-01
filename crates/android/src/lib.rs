//! bliss on Android: the tunnel's half (docs/android-stack.md).
//!
//! This library is loaded by the `:tunnel` process alone, the one that holds
//! the VPN service. It owns the profile, starts the engine (xray, shipped in
//! the APK as an executable), carries the TUN's packets through bliss-tun's
//! stack and bliss-core's rules, and talks to the screens over a unix socket
//! in the app's own folder: one `Frame` per line out, one `UiCommand` per
//! line in, both JSON (`protocol.rs`).
//!
//! The desktop's engine loop (crates/app/src/engine.rs) is not used: it lives
//! inside the desktop binary and carries the window, the tray, the system
//! proxy and OpenVPN with it. This one is smaller and Android's own; moving
//! what the two share into one crate is the refactor docs/android-stack.md
//! names, still ahead.

pub mod engine;
pub mod logs;
pub mod protocol;
pub mod ui;

/// The desktop's subscription fetch and list reader, used as they are rather
/// than copied: they need nothing of the desktop's but `plural` (ui.rs).
#[path = "../../app/src/subscription.rs"]
#[allow(dead_code)]
pub mod subscription;

#[cfg(target_os = "android")]
mod android;
