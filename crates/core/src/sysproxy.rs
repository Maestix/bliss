//! The machine's own proxy settings.
//!
//! Three modes, the same three every client in this ecosystem offers: leave the
//! settings alone, point them at us, or turn them off. See
//! [`bliss_proto::ProxyMode`].
//!
//! This is the one module that reaches outside the process and changes how
//! *other* programs reach the network, so the whole design is about putting it
//! back. Three things make that reliable, in increasing order of how much they
//! are relied on:
//!
//! 1. Restoring when the tunnel stops, and when bliss exits.
//! 2. A panic hook, for the exits that skip the normal path.
//! 3. **A snapshot on disk and a check at startup.** This is the one that
//!    actually matters. A `TerminateProcess`, a power cut or a bug runs no
//!    code at all, and the machine is then pointed at a port nobody is
//!    listening on — which, from the user's chair, is "the internet stopped
//!    working" with no clue as to why. So the previous values are written to a
//!    file *before* ours go in, and the next start puts them back.
//!
//! The snapshot is only honoured if the settings still look like ours. If the
//! user changed the proxy themselves in between, theirs wins and we simply
//! forget ours: restoring over a deliberate choice would be the same crime in
//! the other direction.

use std::path::{Path, PathBuf};

use bliss_proto::ProxyMode;

/// What was in the registry before we touched it, plus what we put there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub enable: u32,
    pub server: String,
    pub bypass: String,
    /// The `ProxyServer` value we wrote. Restoring checks against this so a
    /// setting the user has since changed is left alone.
    pub ours: String,
    /// Which process owns these settings.
    ///
    /// Needed because the startup check would otherwise be dangerous rather
    /// than useful: a second bliss starting up would find the first one's
    /// snapshot, decide it was orphaned, and pull the proxy out from under a
    /// copy that is serving happily.
    pub pid: u32,
}

/// Hosts that must never go through the tunnel.
///
/// `<local>` is WinINet's own token for "anything without a dot", which covers
/// intranet names. The loopback entries matter more than they look: without
/// them a browser pointed at our own inbound would try to reach 127.0.0.1
/// *through* 127.0.0.1.
const BYPASS: &str = "localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;\
                      172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;\
                      172.28.*;172.29.*;172.30.*;172.31.*;192.168.*;<local>";

/// The file the pre-existing settings are parked in while ours are in force.
pub fn snapshot_path(work_dir: &Path) -> PathBuf {
    work_dir.join("system-proxy-backup.txt")
}

/// `ProxyServer` for an address: the same value v2rayN writes.
///
/// Both schemes point at the HTTP inbound. WinINet has a `socks=` slot, but
/// what it does with it differs by Windows version and by browser, and https
/// through it is not dependable — whereas `CONNECT` to an HTTP proxy is what
/// every client on this platform does properly.
pub fn proxy_value(addr: std::net::SocketAddr) -> String {
    format!("http={addr};https={addr}")
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("cannot read the machine's proxy settings")]
    Read(#[source] std::io::Error),

    #[error("cannot change the machine's proxy settings")]
    Write(#[source] std::io::Error),

    #[error("cannot write {path} — bliss will not set a proxy it might fail to remove")]
    Snapshot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("bliss does not manage the proxy settings on this platform")]
    Unsupported,
}

/// Point the machine at `addr`, remembering what was there before.
///
/// Refuses to proceed if the snapshot cannot be written. That is deliberate:
/// the snapshot is what makes this reversible after a crash, and a proxy we
/// cannot prove we can undo is not one to set.
pub fn set(work_dir: &Path, addr: std::net::SocketAddr) -> Result<Snapshot, ProxyError> {
    let ours = proxy_value(addr);
    let path = snapshot_path(work_dir);

    // Read first. If a snapshot already exists we are re-applying over our own
    // settings, and the file must keep the *original* values rather than
    // recording ours as if they had been the user's all along.
    let existing = read_snapshot(&path);
    let before = match existing {
        Some(kept) => Snapshot {
            ours: ours.clone(),
            pid: std::process::id(),
            ..kept
        },
        None => {
            let (enable, server, bypass) = imp::read()?;
            Snapshot {
                enable,
                server,
                bypass,
                ours: ours.clone(),
                pid: std::process::id(),
            }
        }
    };

    write_snapshot(&path, &before).map_err(|source| ProxyError::Snapshot {
        path: path.clone(),
        source,
    })?;

    imp::write(1, &ours, BYPASS)?;
    imp::notify();
    tracing::info!("system proxy set to {addr}");
    Ok(before)
}

/// Turn the machine's proxy off and stop tracking it.
///
/// The deliberate version of the mode: unlike [`restore`] this does not put a
/// previous value back, because the user asked for it to be off. Any snapshot
/// is dropped, since there is no longer anything of ours to undo.
pub fn clear(work_dir: &Path) -> Result<(), ProxyError> {
    imp::write(0, "", "")?;
    imp::notify();
    let _ = std::fs::remove_file(snapshot_path(work_dir));
    tracing::info!("system proxy cleared");
    Ok(())
}

/// Put back whatever was there before bliss, if it is still ours to put back.
///
/// Safe to call at any time, including when nothing was ever set: without a
/// snapshot there is nothing to undo and it does nothing. This is both the
/// shutdown path and the startup watchdog.
pub fn restore(work_dir: &Path) -> Result<bool, ProxyError> {
    let path = snapshot_path(work_dir);
    let Some(kept) = read_snapshot(&path) else {
        return Ok(false);
    };

    let current = imp::read();
    let still_ours = matches!(&current, Ok((_, server, _)) if server == &kept.ours);

    if still_ours {
        imp::write(kept.enable, &kept.server, &kept.bypass)?;
        imp::notify();
        tracing::info!("system proxy restored to what it was before bliss");
    } else {
        // Someone — the user, another client — has moved it since. Their
        // setting is the newer decision and we are not entitled to it.
        tracing::info!("system proxy was changed by something else; leaving it alone");
    }

    let _ = std::fs::remove_file(&path);
    Ok(still_ours)
}

/// Is a snapshot lying around from a run that did not get to clean up?
pub fn has_snapshot(work_dir: &Path) -> bool {
    snapshot_path(work_dir).is_file()
}

/// What the machine's settings say right now, for the window to show.
pub fn current() -> Option<(bool, String)> {
    imp::read().ok().map(|(on, server, _)| (on != 0, server))
}

// --- the snapshot file ----------------------------------------------------
//
// Deliberately not serde: four fields, written once and read by a program that
// may be in the middle of recovering from a crash. A hand-rolled format with no
// dependencies cannot fail to parse for a reason we have to go and look up, and
// the file is meant to be readable by a person who is wondering what bliss did
// to their machine.

fn write_snapshot(path: &Path, s: &Snapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = format!(
        "# bliss changed this machine's proxy settings and will put these back.\n\
         # Delete this file only if you have already restored them yourself.\n\
         enable={}\nserver={}\nbypass={}\nours={}\npid={}\n",
        s.enable, s.server, s.bypass, s.ours, s.pid
    );
    std::fs::write(path, body)
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_snapshot(&text)
}

fn parse_snapshot(text: &str) -> Option<Snapshot> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}=")))
            .map(str::to_owned)
    };
    Some(Snapshot {
        enable: field("enable")?.trim().parse().ok()?,
        server: field("server")?,
        bypass: field("bypass")?,
        ours: field("ours")?,
        // The owner is the one optional field, and it defaults to "nobody".
        // A snapshot we cannot attribute is safer treated as orphaned: the
        // worst case is restoring settings that were already correct, against
        // a worst case of leaving the machine pointed at a dead port forever.
        pid: field("pid")
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(0),
    })
}

/// The startup check: put back anything a run that never finished left behind.
///
/// Called before bliss does anything else, and it is the only part of this
/// module that survives being killed. A `TerminateProcess` runs no cleanup, so
/// without this the machine stays pointed at a port nobody is listening on
/// until someone works out what happened and fixes the registry by hand.
///
/// A snapshot whose owner is still running belongs to another bliss that is
/// serving right now, and is left strictly alone.
pub fn recover(work_dir: &Path) -> Result<bool, ProxyError> {
    let Some(kept) = read_snapshot(&snapshot_path(work_dir)) else {
        return Ok(false);
    };
    if kept.pid != std::process::id() && imp::process_alive(kept.pid) {
        tracing::debug!(
            "the system proxy is managed by bliss pid {}, which is still running",
            kept.pid
        );
        return Ok(false);
    }
    restore(work_dir)
}

/// Apply a mode. Returns whether the machine's settings now point at us.
pub fn apply(
    mode: ProxyMode,
    work_dir: &Path,
    addr: Option<std::net::SocketAddr>,
) -> Result<bool, ProxyError> {
    match (mode, addr) {
        (ProxyMode::Unchanged, _) => {
            restore(work_dir)?;
            Ok(false)
        }
        (ProxyMode::Clear, _) => {
            clear(work_dir)?;
            Ok(false)
        }
        // Asked to point at bliss while bliss is not serving. Writing a proxy
        // that refuses every connection is how a client takes the machine
        // offline, so the mode is remembered and applied when the tunnel is up.
        (ProxyMode::Set, None) => {
            restore(work_dir)?;
            Ok(false)
        }
        (ProxyMode::Set, Some(addr)) => {
            set(work_dir, addr)?;
            Ok(true)
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Networking::WinInet::{
        INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_SZ, RegCloseKey,
        RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };
    use windows::core::{HSTRING, PCWSTR};

    use super::ProxyError;

    const SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    /// A key handle that closes itself, so an early return cannot leak it.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful RegOpenKeyExW and is
            // closed exactly once, here.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn open(write: bool) -> Result<Key, ProxyError> {
        let mut key = HKEY::default();
        let access = if write {
            KEY_READ | KEY_WRITE
        } else {
            KEY_READ
        };
        let path = HSTRING::from(SETTINGS);
        // SAFETY: the path outlives the call and `key` is a valid out-pointer.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                access,
                &mut key,
            )
        };
        if status.is_err() {
            let e = std::io::Error::from_raw_os_error(status.0 as i32);
            return Err(if write {
                ProxyError::Write(e)
            } else {
                ProxyError::Read(e)
            });
        }
        Ok(Key(key))
    }

    /// (ProxyEnable, ProxyServer, ProxyOverride). Missing values read as absent
    /// rather than as an error: a machine that has never had a proxy simply has
    /// no such entries.
    pub fn read() -> Result<(u32, String, String), ProxyError> {
        let key = open(false)?;
        Ok((
            read_dword(&key, "ProxyEnable").unwrap_or(0),
            read_string(&key, "ProxyServer").unwrap_or_default(),
            read_string(&key, "ProxyOverride").unwrap_or_default(),
        ))
    }

    fn read_dword(key: &Key, name: &str) -> Option<u32> {
        let name = HSTRING::from(name);
        let mut value: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        // SAFETY: the buffer is exactly `len` bytes and outlives the call.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                None,
                Some(&mut value as *mut u32 as *mut u8),
                Some(&mut len),
            )
        };
        status.is_ok().then_some(value)
    }

    fn read_string(key: &Key, name: &str) -> Option<String> {
        let name = HSTRING::from(name);

        // Ask for the size first: REG_SZ values have no useful upper bound and
        // a guessed buffer is how this kind of code truncates a long bypass
        // list into something that silently stops matching.
        let mut len: u32 = 0;
        // SAFETY: a null data pointer with a length out-pointer is the
        // documented way to ask for the size.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut len),
            )
        };
        if status.is_err() || len == 0 {
            return None;
        }

        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is `len` bytes, which is the size the call just gave us.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
        };
        if status.is_err() {
            return None;
        }

        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        Some(String::from_utf16_lossy(&wide))
    }

    pub fn write(enable: u32, server: &str, bypass: &str) -> Result<(), ProxyError> {
        let key = open(true)?;
        set_dword(&key, "ProxyEnable", enable)?;
        set_string(&key, "ProxyServer", server)?;
        set_string(&key, "ProxyOverride", bypass)?;
        Ok(())
    }

    fn set_dword(key: &Key, name: &str, value: u32) -> Result<(), ProxyError> {
        let name = HSTRING::from(name);
        let bytes = value.to_ne_bytes();
        // SAFETY: name and bytes outlive the call; the length matches the data.
        let status =
            unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&bytes)) };
        status
            .ok()
            .map_err(|e| ProxyError::Write(std::io::Error::other(e)))
    }

    fn set_string(key: &Key, name: &str, value: &str) -> Result<(), ProxyError> {
        let name = HSTRING::from(name);
        // The trailing NUL is part of a REG_SZ value, not an artefact of C.
        // Leaving it off produces a value Windows reads back with garbage on
        // the end.
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes: Vec<u8> = wide.iter().flat_map(|c| c.to_le_bytes()).collect();
        // SAFETY: as above.
        let status =
            unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&bytes)) };
        status
            .ok()
            .map_err(|e| ProxyError::Write(std::io::Error::other(e)))
    }

    /// Is that process still there?
    ///
    /// Only ever used to decide *not* to act, so a false positive is the safe
    /// direction: at worst a reused pid makes us leave an orphaned setting for
    /// one more run, rather than pulling the proxy out from under a bliss that
    /// is serving.
    pub fn process_alive(pid: u32) -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

        // SAFETY: a failed open returns an error rather than a handle, and the
        // handle we do get is closed immediately.
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(handle) => {
                    let _ = CloseHandle(handle);
                    true
                }
                Err(_) => false,
            }
        }
    }

    /// Tell everything that caches these settings to read them again.
    ///
    /// Without this the registry is right and the machine carries on using the
    /// old setting until something restarts — which looks exactly like the
    /// write having failed.
    pub fn notify() {
        // SAFETY: both options take no buffer; a null pointer and zero length
        // is what the documentation asks for.
        unsafe {
            let _ = InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0);
            let _ = InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::ProxyError;

    pub fn read() -> Result<(u32, String, String), ProxyError> {
        Err(ProxyError::Unsupported)
    }

    pub fn write(_enable: u32, _server: &str, _bypass: &str) -> Result<(), ProxyError> {
        Err(ProxyError::Unsupported)
    }

    pub fn notify() {}

    pub fn process_alive(_pid: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_round_trips_through_the_file_format() {
        let kept = Snapshot {
            enable: 1,
            server: "proxy.corp.example:8080".into(),
            bypass: "localhost;<local>".into(),
            ours: "http=127.0.0.1:10809;https=127.0.0.1:10809".into(),
            pid: std::process::id(),
        };
        let dir = std::env::temp_dir().join("bliss-sysproxy-test");
        let path = dir.join("snapshot.txt");
        let _ = std::fs::remove_file(&path);

        write_snapshot(&path, &kept).expect("write");
        assert_eq!(read_snapshot(&path), Some(kept));

        std::fs::remove_file(&path).expect("clean up");
        assert_eq!(read_snapshot(&path), None, "a missing file is not an error");
    }

    /// An empty bypass or server has to survive, because "no proxy at all" is
    /// the most common thing we are restoring to — and it is the case where
    /// losing a field means writing a proxy back on rather than off.
    #[test]
    fn an_empty_previous_setting_round_trips() {
        let empty = Snapshot {
            enable: 0,
            server: String::new(),
            bypass: String::new(),
            ours: "http=127.0.0.1:10809;https=127.0.0.1:10809".into(),
            pid: std::process::id(),
        };
        let dir = std::env::temp_dir().join("bliss-sysproxy-test");
        let path = dir.join("empty.txt");
        write_snapshot(&path, &empty).expect("write");
        assert_eq!(read_snapshot(&path), Some(empty));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_truncated_snapshot_is_refused_rather_than_half_applied() {
        assert_eq!(parse_snapshot("enable=1\nserver=x\n"), None);
        assert_eq!(parse_snapshot(""), None);
        assert_eq!(
            parse_snapshot("enable=not-a-number\nserver=\nbypass=\nours=\n"),
            None
        );
    }

    /// Everything but the owner. Unattributable, so it belongs to nobody and
    /// is ours to clean up — the opposite of refusing to parse it and leaving
    /// the machine pointed wherever the dead run left it.
    #[test]
    fn a_snapshot_with_no_owner_is_treated_as_orphaned() {
        let kept = parse_snapshot("enable=1\nserver=old:8080\nbypass=\nours=ours:1\n")
            .expect("parses without a pid");
        assert_eq!(kept.pid, 0);
        assert!(
            !imp::process_alive(kept.pid),
            "pid 0 must never look like a live owner"
        );
    }

    #[test]
    fn both_schemes_point_at_the_http_inbound() {
        let addr = "127.0.0.1:10809".parse().unwrap();
        assert_eq!(
            proxy_value(addr),
            "http=127.0.0.1:10809;https=127.0.0.1:10809"
        );
    }

    /// Loopback must be in the bypass list. Without it a browser configured
    /// through us tries to reach our own inbound through our own inbound.
    #[test]
    fn the_bypass_list_covers_loopback_and_private_ranges() {
        for must in ["localhost", "127.*", "192.168.*", "10.*", "<local>"] {
            assert!(BYPASS.contains(must), "bypass list is missing {must}");
        }
    }
}
