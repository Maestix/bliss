//! The TUN adapter and what bliss puts through it.
//!
//! The machine's proxy settings only ever carried TCP, so a call in Discord, a
//! game or anything else on UDP went straight out and was cut. A TUN adapter
//! takes the packets themselves; they come back out of a userspace stack as
//! connections and datagrams, which is the seam the relay already has.
//!
//! Everything here needs rights an ordinary program lacks: creating an adapter
//! and moving routes does — administrator on Windows, `CAP_NET_ADMIN` on
//! Linux. See §7 of AGENTS.md for how it is probed.
//!
//! Windows' adapter is wintun's, driven through IP Helper (`device.rs`,
//! `net.rs`); Linux's is the kernel's own TUN device, driven over ioctls and
//! netlink (`linux_device.rs`, `linux_net.rs`). Both answer to the same names,
//! so the tunnel itself (`Tun::up`) and the stack it carries are one.

use std::io;
use std::net::Ipv4Addr;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(windows)]
mod device;
#[cfg(target_os = "linux")]
#[path = "linux_device.rs"]
mod device;
#[cfg(target_os = "android")]
#[path = "android_device.rs"]
mod device;
#[cfg(windows)]
mod net;
#[cfg(target_os = "linux")]
#[path = "linux_net.rs"]
mod net;
#[cfg(target_os = "android")]
#[path = "android_net.rs"]
mod net;
#[cfg(any(windows, target_os = "linux", target_os = "android"))]
mod stack;

#[cfg(any(windows, target_os = "linux", target_os = "android"))]
pub use device::MTU;
/// On Android the system makes the device and bliss is handed its
/// descriptor, so the stack is driven from outside rather than by `Tun::up`.
#[cfg(target_os = "android")]
pub use device::Packets;
#[cfg(target_os = "android")]
pub use net::Gateway;
#[cfg(target_os = "android")]
pub use stack::{Carrying, serve};

/// The signed library, carried inside bliss.exe.
///
/// Wintun installs a kernel driver out of this file and Windows checks the
/// signature of what it installs, so it has to be a file on disk: bliss writes
/// it out beside the engine and loads it from there. Loading a library straight
/// from memory would leave nothing to check and is what malware does — bliss.exe
/// is unsigned and has already collected one false positive without any help.
#[cfg(windows)]
const LIBRARY: &[u8] = include_bytes!("../assets/wintun.dll");

/// The terms it travels under. Clause 3d is the one that lets bliss ship it:
/// alongside software that uses it only through the documented API.
#[cfg(windows)]
const LICENCE: &str = include_str!("../assets/LICENSE-wintun.txt");

/// Write the library and its licence into the work dir; answer where it went.
///
/// Written only when what is already there is not this, byte for byte: a copy
/// another bliss has loaded cannot be replaced anyway, and one that is already
/// right does not need to be.
#[cfg(windows)]
pub fn place(work_dir: &Path) -> io::Result<PathBuf> {
    let library = work_dir.join("wintun.dll");
    if std::fs::read(&library).is_ok_and(|found| found == LIBRARY) {
        return Ok(library);
    }
    std::fs::create_dir_all(work_dir)?;
    // Beside it, so that whoever finds the file finds its terms.
    std::fs::write(work_dir.join("LICENSE-wintun.txt"), LICENCE)?;
    // Through a temporary name: half a library is worse than none, because it
    // would pass the "is it there?" check and fail at load.
    let beside = work_dir.join("wintun.dll.new");
    std::fs::write(&beside, LIBRARY)?;
    std::fs::rename(&beside, &library)?;
    Ok(library)
}

/// Whether this copy of bliss could create an adapter at all.
///
/// Asked before trying, so the settings screen can say why the switch has no
/// effect rather than leaving a failed start to explain it. A window started
/// from the Run key at sign-in is never elevated, which is exactly the case
/// this exists to describe.
#[cfg(windows)]
pub fn elevated() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = windows::Win32::Foundation::HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    let asked = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    asked.is_ok() && elevation.TokenIsElevated != 0
}

/// On Linux: `CAP_NET_ADMIN` in the effective set — bliss run as root, or
/// given the one capability with `setcap cap_net_admin+ep` on its executable.
#[cfg(target_os = "linux")]
pub fn elevated() -> bool {
    device::capable()
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn elevated() -> bool {
    false
}

/// What to do about not being `elevated`, in the window's words.
pub fn rights_hint() -> &'static str {
    if cfg!(windows) {
        "перезапустите bliss от имени администратора"
    } else {
        "выдайте bliss право на сетевые настройки: sudo setcap cap_net_admin+ep <путь к bliss> \
         — и заново после каждого обновления"
    }
}

/// The tunnel, for as long as this is held.
///
/// Dropping it puts the machine back: the routes go first, so nothing is being
/// steered at an adapter that is about to vanish, and then the adapter itself
/// goes and the system forgets everything that pointed at it. There is nothing
/// to undo after a crash, which is the one thing this has over the system
/// proxy — the adapter cannot outlive the process that made it. The one
/// exception is the routes around it on Linux, which the next tunnel takes
/// away (`net::forget_stale`).
#[cfg(any(windows, target_os = "linux"))]
pub struct Tun {
    /// Declared before the device so it is dropped before the device.
    _routes: net::Routes,
    _device: device::Device,
    carrying: tokio::task::JoinHandle<()>,
}

#[cfg(any(windows, target_os = "linux"))]
impl Tun {
    /// Bring the adapter up and start carrying traffic through `serving`.
    ///
    /// `around` is every address the engine itself talks to — the server, and
    /// each address its name resolves to. Those get a route out the ordinary
    /// way, because the tunnel cannot carry the connection that is the tunnel.
    ///
    /// Needs administrator rights, and says so when it does not have them.
    /// Must be called from a multi-threaded tokio runtime: the stack ends a
    /// connection with a blocking wait, which a current-thread runtime panics
    /// on.
    pub fn up(
        work_dir: &Path,
        serving: Arc<bliss_core::Serving>,
        around: &[Ipv4Addr],
    ) -> io::Result<Self> {
        #[cfg(windows)]
        let device = device::Device::open(&place(work_dir)?)?;
        #[cfg(target_os = "linux")]
        let mut device = {
            let _ = work_dir;
            device::Device::open()?
        };
        let interface = device.interface();
        net::set_address(interface, device::ADDRESS, device::PREFIX)?;

        // Read before the routes are added, or the best default route is the
        // one bliss is about to install.
        let Some(gateway) = net::gateway(interface)? else {
            return Err(io::Error::other(
                "this machine has no default route, so there is nothing for the tunnel itself to go out by",
            ));
        };

        #[cfg(target_os = "linux")]
        match net::forget_stale() {
            Ok(0) => {}
            Ok(stale) => tracing::info!("tun: took away {stale} route(s) a previous run left"),
            Err(e) => tracing::warn!("tun: could not look for routes a previous run left: {e}"),
        }

        let mut routes = net::Routes::new();
        // Around first: on Linux the way to each address is asked of the
        // kernel, which would answer "the tunnel" once its routes exist.
        for address in around {
            routes.around_tunnel(gateway, *address)?;
        }
        // The two halves of the address space, which beat the machine's own
        // default route by being longer without replacing it.
        routes.through_tunnel(interface, Ipv4Addr::UNSPECIFIED, 1)?;
        routes.through_tunnel(interface, Ipv4Addr::new(128, 0, 0, 0), 1)?;
        tracing::info!(
            "tun: up on {} via {}, {} address(es) kept outside",
            device::ADDRESS,
            gateway,
            around.len()
        );

        // wintun's session speaks the futures traits and the stack wants
        // tokio's: the same bytes, two spellings of the same two traits.
        #[cfg(windows)]
        let packets = {
            use tokio_util::compat::FuturesAsyncReadCompatExt;
            device.session().compat()
        };
        #[cfg(target_os = "linux")]
        let packets = device
            .packets()
            .ok_or_else(|| io::Error::other("the device's packets were taken already"))?;
        let carrying = tokio::spawn(async move {
            let carrying = Arc::new(stack::Carrying { serving, gateway });
            if let Err(e) = stack::serve(packets, carrying).await {
                tracing::error!("tun: {e}");
            }
        });

        Ok(Self {
            _routes: routes,
            _device: device,
            carrying,
        })
    }
}

#[cfg(any(windows, target_os = "linux"))]
impl Drop for Tun {
    fn drop(&mut self) {
        // The task is parked on a read from a session that is about to be shut
        // down, and would otherwise sit there until the process ends.
        self.carrying.abort();
    }
}

/// Everywhere else, so that the code around it needs no `cfg`.
#[cfg(not(any(windows, target_os = "linux")))]
pub struct Tun;

#[cfg(not(any(windows, target_os = "linux")))]
impl Tun {
    pub fn up(
        _work_dir: &Path,
        _serving: Arc<bliss_core::Serving>,
        _around: &[Ipv4Addr],
    ) -> io::Result<Self> {
        Err(io::Error::other(
            "bliss carries a TUN adapter on Windows and Linux only",
        ))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// What `LIBRARY` hashes to: the amd64 library out of wintun.net's
    /// `wintun-0.14.1.zip`, whose own checksum that site publishes.
    const LIBRARY_SHA256: &str = "e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce";

    /// The library has to be the signed one, byte for byte: it is what installs
    /// the driver, and anything else would not be signed. Its licence travels
    /// with it, and `place` writes that out too.
    #[test]
    fn the_library_is_the_signed_one_and_licensed() {
        assert_eq!(format!("{:x}", Sha256::digest(LIBRARY)), LIBRARY_SHA256);
        assert_eq!(LIBRARY.len(), 427_552);
        assert!(
            LICENCE.starts_with("Prebuilt Binaries License"),
            "the wrong licence"
        );
        assert!(
            LICENCE.contains("distributed alongside"),
            "clause 3d, which is why bliss may ship it at all"
        );
    }

    #[test]
    fn a_library_already_in_place_is_left_alone() {
        let dir = std::env::temp_dir().join("bliss-tun-places-the-library");
        let _ = std::fs::remove_dir_all(&dir);
        let first = place(&dir).expect("written");
        assert_eq!(std::fs::read(&first).expect("readable"), LIBRARY);
        assert!(dir.join("LICENSE-wintun.txt").exists(), "the terms too");
        let written_at = std::fs::metadata(&first)
            .expect("there")
            .modified()
            .expect("a time");

        let again = place(&dir).expect("asked again");
        assert_eq!(first, again);
        assert_eq!(
            std::fs::metadata(&again)
                .expect("there")
                .modified()
                .expect("a time"),
            written_at,
            "written again for nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
