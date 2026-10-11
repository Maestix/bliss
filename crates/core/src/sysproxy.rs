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
//!    actually matters. A `TerminateProcess`, a `kill -9`, a power cut or a
//!    bug runs no code at all, and the machine is then pointed at a port
//!    nobody is listening on — which, from the user's chair, is "the internet
//!    stopped working" with no clue as to why. So the previous values are
//!    written to a file *before* ours go in, and the next start puts them back.
//!
//! The snapshot is only honoured if the settings still look like ours. If the
//! user changed the proxy themselves in between, theirs wins and we simply
//! forget ours: restoring over a deliberate choice would be the same crime in
//! the other direction.
//!
//! Changes are made one at a time (`CHANGING`). The engine makes its own off
//! its loop, since on Linux each is a chain of programs to run, and the
//! window puts the settings back on its own thread as bliss ends: a restore
//! that read them halfway through a `set` would find them neither bliss's
//! nor the user's, forget the snapshot, and leave the machine on bliss's
//! port once the `set` finished. After the restore on the way out (`leave`)
//! nothing is set again.
//!
//! Where the settings live is the platform's business (`imp`): on Windows
//! three values in the registry that WinINet reads; on Linux the desktop's
//! own — GNOME's `org.gnome.system.proxy`, which the GNOME family and Firefox
//! read, or KDE's `kioslaverc`. A desktop with neither has no system proxy to
//! set, and says so rather than pretending.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

use bliss_proto::ProxyMode;

/// One change to the machine's settings at a time in this process; `true`
/// once bliss has put them back on its way out (see the module's head).
static CHANGING: Mutex<bool> = Mutex::new(false);

fn one_at_a_time() -> MutexGuard<'static, bool> {
    CHANGING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The machine's proxy settings as `key=value` pairs, in the platform's own
/// words: on Windows the three registry values, on Linux the desktop's keys.
/// Text, so that one snapshot format holds whichever it was and a person
/// wondering what bliss did to their machine can read it.
pub type Settings = Vec<(String, String)>;

/// What was there before we touched it, plus what we put there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The settings as they were before bliss.
    pub previous: Settings,
    /// What bliss wrote, as `imp::signature` reads it back. Restoring checks
    /// against this so a setting the user has since changed is left alone.
    pub ours: String,
    /// Which process owns these settings.
    ///
    /// Needed because the startup check would otherwise be dangerous rather
    /// than useful: a second bliss starting up would find the first one's
    /// snapshot, decide it was orphaned, and pull the proxy out from under a
    /// copy that is serving happily.
    pub pid: u32,
}

/// The file the pre-existing settings are parked in while ours are in force.
pub fn snapshot_path(work_dir: &Path) -> PathBuf {
    work_dir.join("system-proxy-backup.txt")
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

    /// A Linux desktop without a system proxy of its own: programs there take
    /// a proxy from their own settings or from the environment.
    #[error(
        "this desktop has no system proxy setting bliss can change (GNOME and KDE have one) — \
         point programs at the HTTP inbound in their own settings"
    )]
    NoDesktopProxy,

    #[error("another bliss (pid {0}) is managing the proxy settings right now")]
    OwnedElsewhere(u32),
}

/// One value out of a set of settings.
fn value<'a>(settings: &'a Settings, key: &str) -> Option<&'a str> {
    settings
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Is this snapshot another bliss's, and is that bliss still running?
///
/// A snapshot with no owner recorded (pid 0) counts as nobody's: see
/// `parse_snapshot` for why an unattributed one is treated as orphaned.
fn owned_elsewhere(kept: &Snapshot) -> bool {
    kept.pid != 0 && kept.pid != std::process::id() && imp::process_alive(kept.pid)
}

/// Point the machine at `addr`, remembering what was there before.
///
/// Refuses to proceed if the snapshot cannot be written. That is deliberate:
/// the snapshot is what makes this reversible after a crash, and a proxy we
/// cannot prove we can undo is not one to set.
fn set(work_dir: &Path, addr: std::net::SocketAddr) -> Result<Snapshot, ProxyError> {
    let path = snapshot_path(work_dir);

    // Read first. If a snapshot already exists we are re-applying over our own
    // settings, and the file must keep the *original* values rather than
    // recording ours as if they had been the user's all along.
    let existing = read_snapshot(&path);
    // Another running bliss set these; writing over them would take its proxy
    // and, on our way out, restore settings that are not ours to restore.
    // Asked before the settings are so much as read.
    if let Some(kept) = &existing
        && owned_elsewhere(kept)
    {
        return Err(ProxyError::OwnedElsewhere(kept.pid));
    }
    // One reading: what is there now, and on Linux which stores there are
    // to write to. Each store is a program to run there, and read twice it
    // was twice as slow.
    let now = imp::read()?;
    let wanted = imp::pointing_at(addr, &now)?;
    let ours = imp::signature(&wanted);
    let before = match existing {
        Some(kept) => Snapshot {
            ours: ours.clone(),
            pid: std::process::id(),
            ..kept
        },
        None => Snapshot {
            previous: now,
            ours: ours.clone(),
            pid: std::process::id(),
        },
    };

    write_snapshot(&path, &before).map_err(|source| ProxyError::Snapshot {
        path: path.clone(),
        source,
    })?;

    imp::write(&wanted)?;
    imp::notify();
    tracing::info!("system proxy set to {addr}");
    Ok(before)
}

/// Turn the machine's proxy off and stop tracking it.
///
/// The deliberate version of the mode: unlike [`restore`] this does not put a
/// previous value back, because the user asked for it to be off. Any snapshot
/// is dropped, since there is no longer anything of ours to undo.
fn clear(work_dir: &Path) -> Result<(), ProxyError> {
    imp::write(&imp::off()?)?;
    imp::notify();
    let _ = std::fs::remove_file(snapshot_path(work_dir));
    tracing::info!("system proxy cleared");
    Ok(())
}

/// Put back whatever was there before bliss, if it is still ours to put back.
///
/// Safe to call at any time, including when nothing was ever set: without a
/// snapshot there is nothing to undo and it does nothing. This is the startup
/// watchdog (`recover`) and what a mode that no longer points at bliss does;
/// the way out is [`leave`].
pub fn restore(work_dir: &Path) -> Result<bool, ProxyError> {
    let _one = one_at_a_time();
    restore_now(work_dir)
}

/// [`restore`], for the last time: bliss is on its way out, and nothing it
/// asks for after this changes the settings again — a change still on its
/// way to being made would point the machine at a port about to close.
pub fn leave(work_dir: &Path) -> Result<bool, ProxyError> {
    let mut left = one_at_a_time();
    *left = true;
    restore_now(work_dir)
}

/// [`restore`], for a panic hook: skipped while a change is being made,
/// which may be on the very thread that panicked, where waiting for it would
/// hang. The startup check covers what this skips.
pub fn restore_unless_busy(work_dir: &Path) -> Result<bool, ProxyError> {
    let _one = match CHANGING.try_lock() {
        Ok(one) => one,
        Err(TryLockError::Poisoned(one)) => one.into_inner(),
        Err(TryLockError::WouldBlock) => return Ok(false),
    };
    restore_now(work_dir)
}

fn restore_now(work_dir: &Path) -> Result<bool, ProxyError> {
    let path = snapshot_path(work_dir);
    let Some(kept) = read_snapshot(&path) else {
        return Ok(false);
    };
    // A copy of bliss that is still running owns these settings. The check is
    // here rather than only in `recover` because every path leads here — the
    // tunnel stopping, the window closing, a mode applied at startup. It used
    // to live in `recover` alone, so a second window opened next to a copy that
    // was serving pulled that copy's proxy out from under it as it started.
    if owned_elsewhere(&kept) {
        tracing::debug!(
            "the system proxy is managed by bliss pid {}, which is still running",
            kept.pid
        );
        return Ok(false);
    }

    // Read from the stores the snapshot came from, which on Linux need not be
    // the desktop this session runs. Settings that could not be read are not
    // settings someone changed: the snapshot stays, for the next start to try
    // again, rather than being forgotten with the machine still pointed at a
    // port nobody listens on.
    let current = imp::read_like(&kept.previous)?;
    // Store by store: on Plasma GNOME's settings (Firefox's) and KDE's are
    // two, and one changed by hand says nothing about the other.
    let back = imp::still_ours(&kept.previous, &current, &kept.ours);

    if !back.is_empty() {
        imp::write(&back)?;
        imp::notify();
        tracing::info!("system proxy restored to what it was before bliss");
    }
    if back.len() < kept.previous.len() {
        // Someone — the user, another client — has moved it since. Their
        // setting is the newer decision and we are not entitled to it.
        tracing::info!("system proxy was changed by something else; leaving it alone");
    }

    let _ = std::fs::remove_file(&path);
    Ok(!back.is_empty())
}

/// Is a snapshot lying around from a run that did not get to clean up?
pub fn has_snapshot(work_dir: &Path) -> bool {
    snapshot_path(work_dir).is_file()
}

/// What the machine's settings say right now, for the window to show.
pub fn current() -> Option<(bool, String)> {
    imp::read().ok().map(|settings| imp::describe(&settings))
}

// --- the snapshot file ----------------------------------------------------
//
// Deliberately not serde: a few `key=value` lines, written once and read by a
// program that may be in the middle of recovering from a crash. A hand-rolled
// format with no dependencies cannot fail to parse for a reason we have to go
// and look up, and the file is meant to be readable by a person who is
// wondering what bliss did to their machine. Windows' lines are the ones
// every earlier bliss wrote — enable, server, bypass, ours, pid — so a
// snapshot left by an older build still puts the machine back.

fn write_snapshot(path: &Path, s: &Snapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = String::from(
        "# bliss changed this machine's proxy settings and will put these back.\n\
         # Delete this file only if you have already restored them yourself.\n",
    );
    for (key, value) in &s.previous {
        // One line per setting is the whole format: a setting that spans two
        // would come back as something else, so it is refused here instead.
        if [key, value].iter().any(|text| text.contains(['\n', '\r'])) || key.contains('=') {
            return Err(std::io::Error::other(format!(
                "the setting {key:?} cannot be written on one line"
            )));
        }
        body.push_str(&format!("{key}={value}\n"));
    }
    body.push_str(&format!("ours={}\npid={}\n", s.ours, s.pid));
    std::fs::write(path, body)
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_snapshot(&text)
}

fn parse_snapshot(text: &str) -> Option<Snapshot> {
    let mut previous = Settings::new();
    let mut ours = None;
    let mut pid = None;
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "ours" => ours = Some(value.to_owned()),
            "pid" => pid = Some(value),
            _ => previous.push((key.to_owned(), value.to_owned())),
        }
    }
    // Half a snapshot is refused rather than half applied: settings this
    // platform could not write back whole would put the machine somewhere
    // that is neither ours nor the user's.
    if !imp::valid(&previous) {
        return None;
    }
    Some(Snapshot {
        previous,
        ours: ours?,
        // The owner is the one optional field, and it defaults to "nobody".
        // A snapshot we cannot attribute is safer treated as orphaned: the
        // worst case is restoring settings that were already correct, against
        // a worst case of leaving the machine pointed at a dead port forever.
        pid: pid.and_then(|p| p.trim().parse().ok()).unwrap_or(0),
    })
}

/// The startup check: put back anything a run that never finished left behind.
///
/// Called before bliss does anything else, and it is the only part of this
/// module that survives being killed. A `TerminateProcess` runs no cleanup, so
/// without this the machine stays pointed at a port nobody is listening on
/// until someone works out what happened and fixes the settings by hand.
///
/// A snapshot whose owner is still running belongs to another bliss that is
/// serving right now, and is left strictly alone — by `restore` itself, which
/// every path goes through, so this is only the name the startup call reads by.
pub fn recover(work_dir: &Path) -> Result<bool, ProxyError> {
    restore(work_dir)
}

/// Apply a mode. Returns whether the machine's settings now point at us.
///
/// Does nothing once bliss has left (`leave`).
pub fn apply(
    mode: ProxyMode,
    work_dir: &Path,
    addr: Option<std::net::SocketAddr>,
) -> Result<bool, ProxyError> {
    let left = one_at_a_time();
    if *left {
        return Ok(false);
    }
    match (mode, addr) {
        (ProxyMode::Unchanged, _) => {
            restore_now(work_dir)?;
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
            restore_now(work_dir)?;
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

    use super::{ProxyError, Settings, value};

    const SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    /// Hosts that must never go through the tunnel.
    ///
    /// `<local>` is WinINet's own token for "anything without a dot", which
    /// covers intranet names. The loopback entries matter more than they look:
    /// without them a browser pointed at our own inbound would try to reach
    /// 127.0.0.1 *through* 127.0.0.1.
    pub(super) const BYPASS: &str = "localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;\
                      172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;\
                      172.28.*;172.29.*;172.30.*;172.31.*;192.168.*;<local>";

    /// `ProxyServer` for an address: the same value v2rayN writes.
    ///
    /// Both schemes point at the HTTP inbound. WinINet has a `socks=` slot,
    /// but what it does with it differs by Windows version and by browser,
    /// and https through it is not dependable — whereas `CONNECT` to an HTTP
    /// proxy is what every client on this platform does properly.
    pub(super) fn proxy_value(addr: std::net::SocketAddr) -> String {
        format!("http={addr};https={addr}")
    }

    /// The three values, under the names every snapshot has used.
    fn settings(enable: u32, server: &str, bypass: &str) -> Settings {
        vec![
            ("enable".to_owned(), enable.to_string()),
            ("server".to_owned(), server.to_owned()),
            ("bypass".to_owned(), bypass.to_owned()),
        ]
    }

    pub fn pointing_at(
        addr: std::net::SocketAddr,
        _now: &Settings,
    ) -> Result<Settings, ProxyError> {
        Ok(settings(1, &proxy_value(addr), BYPASS))
    }

    /// One store, so all of it or nothing: `ProxyServer` still bliss's.
    pub fn still_ours(previous: &Settings, current: &Settings, ours: &str) -> Settings {
        if signature(current) == ours {
            previous.clone()
        } else {
            Settings::new()
        }
    }

    pub fn off() -> Result<Settings, ProxyError> {
        Ok(settings(0, "", ""))
    }

    /// `ProxyServer`: what makes the setting ours.
    pub fn signature(settings: &Settings) -> String {
        value(settings, "server").unwrap_or_default().to_owned()
    }

    pub fn describe(settings: &Settings) -> (bool, String) {
        (
            value(settings, "enable").is_some_and(|on| on != "0"),
            signature(settings),
        )
    }

    pub fn valid(settings: &Settings) -> bool {
        value(settings, "enable").is_some_and(|on| on.trim().parse::<u32>().is_ok())
            && value(settings, "server").is_some()
            && value(settings, "bypass").is_some()
    }

    /// One registry, whatever the snapshot says.
    pub fn read_like(_previous: &Settings) -> Result<Settings, ProxyError> {
        read()
    }

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

    /// ProxyEnable, ProxyServer and ProxyOverride. Missing values read as
    /// absent rather than as an error: a machine that has never had a proxy
    /// simply has no such entries.
    pub fn read() -> Result<Settings, ProxyError> {
        let key = open(false)?;
        Ok(settings(
            read_dword(&key, "ProxyEnable").unwrap_or(0),
            &read_string(&key, "ProxyServer").unwrap_or_default(),
            &read_string(&key, "ProxyOverride").unwrap_or_default(),
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
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|&c| c != 0)
            .collect();
        Some(String::from_utf16_lossy(&wide))
    }

    pub fn write(settings: &Settings) -> Result<(), ProxyError> {
        let invalid = || {
            ProxyError::Write(std::io::Error::other(
                "the settings to write are not the three Windows has",
            ))
        };
        let enable = value(settings, "enable")
            .and_then(|on| on.trim().parse().ok())
            .ok_or_else(invalid)?;
        let server = value(settings, "server").ok_or_else(invalid)?;
        let bypass = value(settings, "bypass").ok_or_else(invalid)?;

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

/// Linux has no one system proxy: each desktop keeps its own, and programs
/// read whichever their makers chose. Chromium (and every Electron program)
/// reads GNOME's settings on the GNOME family and KDE's on Plasma; Firefox
/// reads GNOME's on every desktop; GNOME's own programs read them through
/// GIO, KDE's through KIO. So bliss writes:
///
/// - **GNOME's** — `org.gnome.system.proxy`, through `gsettings` — wherever
///   the schema is kept by dconf, which is every desktop with GTK programs on
///   it, Plasma included. GSettings without dconf keeps settings in the
///   process that wrote them, so there is nothing to write to.
/// - **KDE's** — `[Proxy Settings]` in `kioslaverc`, through `kwriteconfig5`
///   or `6` — on Plasma, followed by the D-Bus signal that has KIO read it
///   again.
///
/// A desktop with neither has no system proxy, and `set` says so
/// (`NoDesktopProxy`). Only the keys bliss writes are kept to be put back.
/// Two of them it writes only to keep the user's own setting from being
/// misread around bliss's: GNOME's `use-authentication`, switched off so that
/// a program does not send the old proxy's login to bliss — the login itself
/// is never read, and stays where it is — and KDE's `ReversedException`,
/// which turns the bypass list into the only hosts the proxy is for.
///
/// Each key in `Settings` names its store (`gnome:…`, `kde:…`), so a snapshot
/// is written back into the stores it came from, even if the next session
/// runs another desktop.
#[cfg(target_os = "linux")]
mod imp {
    use std::ffi::OsString;
    use std::io;
    use std::net::SocketAddr;
    use std::process::{Command, Output, Stdio};
    use std::time::Duration;

    use super::{ProxyError, Settings};

    /// Hosts that must never go through the tunnel, in a form both desktops
    /// read: loopback above all, or a browser pointed at our inbound would
    /// reach it through itself — and the window's own page talks to its
    /// renderer over 127.0.0.1.
    pub(super) const BYPASS: [&str; 6] = [
        "localhost",
        "127.0.0.0/8",
        "::1",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
    ];

    /// How long a settings program has to answer. They answer in
    /// milliseconds; one stuck on a session bus that went away would
    /// otherwise hold the engine loop for good.
    const PATIENCE: Duration = Duration::from_secs(5);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Store {
        Gnome,
        Kde,
    }

    impl Store {
        fn prefix(self) -> &'static str {
            match self {
                Self::Gnome => gnome::PREFIX,
                Self::Kde => kde::PREFIX,
            }
        }

        fn of(key: &str) -> Option<Self> {
            [Self::Gnome, Self::Kde]
                .into_iter()
                .find(|store| key.starts_with(store.prefix()))
        }

        fn read(self) -> Result<Settings, ProxyError> {
            match self {
                Self::Gnome => gnome::Gsettings::installed().read(),
                Self::Kde => kde::Kconfig::installed().read(),
            }
        }

        fn write(self, settings: &Settings) -> Result<(), ProxyError> {
            match self {
                Self::Gnome => gnome::Gsettings::installed().write(settings),
                Self::Kde => kde::Kconfig::installed().write(settings),
            }
        }
    }

    /// Plasma, by `XDG_CURRENT_DESKTOP` — a list, such as `KDE` or
    /// `ubuntu:GNOME`.
    pub(super) fn plasma(names: &str) -> bool {
        names
            .split(':')
            .any(|name| name.trim().eq_ignore_ascii_case("kde"))
    }

    /// The stores this session's programs read, with what each holds now:
    /// KDE's on Plasma, and GNOME's wherever dconf keeps it.
    fn session() -> Result<Vec<(Store, Settings)>, ProxyError> {
        let mut stores = Vec::new();
        if plasma(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()) {
            stores.push((Store::Kde, Store::Kde.read()?));
        }
        match Store::Gnome.read() {
            Ok(settings) => stores.push((Store::Gnome, settings)),
            // Not there, or nothing keeps it: GNOME's settings are simply not
            // a store on this desktop.
            Err(ProxyError::NoDesktopProxy) => {}
            Err(e) if stores.is_empty() => return Err(e),
            Err(e) => tracing::debug!("system proxy: GNOME's settings not used: {e}"),
        }
        if stores.is_empty() {
            return Err(ProxyError::NoDesktopProxy);
        }
        Ok(stores)
    }

    /// The stores a set of settings names, in the order they first appear.
    fn stores_in(settings: &Settings) -> Vec<Store> {
        let mut stores: Vec<Store> = Vec::new();
        for (key, _) in settings {
            if let Some(store) = Store::of(key)
                && !stores.contains(&store)
            {
                stores.push(store);
            }
        }
        stores
    }

    fn only(settings: &Settings, store: Store) -> Settings {
        settings
            .iter()
            .filter(|(key, _)| key.starts_with(store.prefix()))
            .cloned()
            .collect()
    }

    pub fn read() -> Result<Settings, ProxyError> {
        Ok(session()?.into_iter().flat_map(|(_, s)| s).collect())
    }

    pub fn read_like(previous: &Settings) -> Result<Settings, ProxyError> {
        let mut now = Settings::new();
        for store in stores_in(previous) {
            now.extend(store.read()?);
        }
        Ok(now)
    }

    pub fn write(settings: &Settings) -> Result<(), ProxyError> {
        if !valid(settings) {
            return Err(ProxyError::Write(io::Error::other(
                "the settings to write belong to no desktop bliss knows",
            )));
        }
        for store in stores_in(settings) {
            store.write(&only(settings, store))?;
        }
        Ok(())
    }

    /// bliss's settings for each store `now` was read from.
    pub fn pointing_at(addr: SocketAddr, now: &Settings) -> Result<Settings, ProxyError> {
        let mut wanted = Settings::new();
        for store in stores_in(now) {
            wanted.extend(match store {
                Store::Gnome => gnome::pointing_at(addr),
                Store::Kde => kde::pointing_at(addr),
            });
        }
        if wanted.is_empty() {
            return Err(ProxyError::NoDesktopProxy);
        }
        Ok(wanted)
    }

    /// What of `previous` goes back: each store whose settings still look as
    /// bliss wrote them. `ours` is every store's signature in one line, and a
    /// store's own is bracketed by its name, so it is found there as it was.
    pub fn still_ours(previous: &Settings, current: &Settings, ours: &str) -> Settings {
        let mut back = Settings::new();
        for store in stores_in(previous) {
            let now = signature(&only(current, store));
            if !now.is_empty() && ours.contains(&now) {
                back.extend(only(previous, store));
            }
        }
        back
    }

    pub fn off() -> Result<Settings, ProxyError> {
        let mut wanted = Settings::new();
        for (store, _) in session()? {
            wanted.extend(match store {
                Store::Gnome => gnome::off(),
                Store::Kde => kde::off(),
            });
        }
        Ok(wanted)
    }

    /// What makes the settings ours, store by store, in a fixed order so
    /// that what was written and what is read back compare.
    pub fn signature(settings: &Settings) -> String {
        let mut stores = stores_in(settings);
        stores.sort_by_key(|store| store.prefix());
        stores
            .into_iter()
            .map(|store| {
                let mine = only(settings, store);
                match store {
                    Store::Gnome => format!("gnome[{}]", gnome::signature(&mine)),
                    Store::Kde => format!("kde[{}]", kde::signature(&mine)),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// As the window shows it: KDE's when there are both, since on Plasma
    /// that is the one its own settings show.
    pub fn describe(settings: &Settings) -> (bool, String) {
        let stores = stores_in(settings);
        if stores.contains(&Store::Kde) {
            kde::describe(&only(settings, Store::Kde))
        } else if stores.contains(&Store::Gnome) {
            gnome::describe(&only(settings, Store::Gnome))
        } else {
            (false, String::new())
        }
    }

    pub fn valid(settings: &Settings) -> bool {
        !settings.is_empty() && settings.iter().all(|(key, _)| Store::of(key).is_some())
    }

    /// Each store tells its readers itself, as part of the write.
    pub fn notify() {}

    /// Is that process a bliss that is still running?
    ///
    /// Not merely a process: pids here are handed out in order from boot,
    /// so after a restart the number of the bliss that wrote a snapshot
    /// belongs to whatever this boot started at about that point — and a
    /// live stranger kept the snapshot from ever going back, and had bliss
    /// refuse to set the proxy over a copy that was not there. A process of
    /// another user's cannot be read, and is not the user's bliss. pid 0 is
    /// nobody.
    pub fn process_alive(pid: u32) -> bool {
        pid != 0
            && std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| {
                exe.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("bliss"))
            })
    }

    /// Run a settings program to its end, or give up on it after `PATIENCE`.
    fn run(program: &OsString, args: &[&str]) -> io::Result<Output> {
        let child = Command::new(program)
            .args(args)
            // Their messages are read for what they say ("No such schema"),
            // and in the user's language they would say it in other words.
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("could not run {}: {e}", program.to_string_lossy()),
                )
            })?;
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        match rx.recv_timeout(PATIENCE) {
            Ok(output) => output,
            Err(_) => {
                // SAFETY: a plain kill(2) of a child not yet reaped — its
                // waiter is still waiting — so the pid is still its own.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{} did not answer within {} s",
                        program.to_string_lossy(),
                        PATIENCE.as_secs()
                    ),
                ))
            }
        }
    }

    /// `run`, and its standard output if it succeeded.
    fn answer(program: &OsString, args: &[&str]) -> io::Result<String> {
        let output = run(program, args)?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr);
            return Err(io::Error::other(format!(
                "{} {} failed ({}): {}",
                program.to_string_lossy(),
                args.first().copied().unwrap_or_default(),
                output.status,
                said.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The first program by these names that is on the PATH.
    fn on_path(names: &[&str]) -> Option<OsString> {
        let path = std::env::var_os("PATH")?;
        names.iter().find_map(|name| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|candidate| candidate.is_file())
                .map(|_| OsString::from(*name))
        })
    }

    pub(super) mod gnome {
        use std::ffi::OsString;
        use std::io;
        use std::net::SocketAddr;

        use super::super::{ProxyError, Settings, value};
        use super::{BYPASS, answer};

        pub const PREFIX: &str = "gnome:";
        const SCHEMA: &str = "org.gnome.system.proxy";

        /// The keys bliss writes, in the order it writes them: the mode last,
        /// so nothing reads a manual proxy before its address is in place.
        const KEYS: [(&str, &str); 9] = [
            ("org.gnome.system.proxy", "ignore-hosts"),
            ("org.gnome.system.proxy.http", "host"),
            ("org.gnome.system.proxy.http", "port"),
            ("org.gnome.system.proxy.http", "use-authentication"),
            ("org.gnome.system.proxy.https", "host"),
            ("org.gnome.system.proxy.https", "port"),
            ("org.gnome.system.proxy.socks", "host"),
            ("org.gnome.system.proxy.socks", "port"),
            ("org.gnome.system.proxy", "mode"),
        ];

        fn key(schema: &str, name: &str) -> String {
            format!("{PREFIX}{schema} {name}")
        }

        /// A string as GVariant text writes one. The values bliss puts in are
        /// addresses and fixed words, none of which holds a quote.
        fn quoted(text: &str) -> String {
            format!("'{text}'")
        }

        fn unquoted(text: &str) -> &str {
            text.trim().trim_matches('\'')
        }

        pub fn pointing_at(addr: SocketAddr) -> Settings {
            let host = quoted(&addr.ip().to_string());
            let port = addr.port().to_string();
            let hosts: Vec<String> = BYPASS.iter().map(|h| quoted(h)).collect();
            let values = [
                format!("[{}]", hosts.join(", ")),
                host.clone(),
                port.clone(),
                // bliss asks for no login, and the old proxy's is not ours.
                "false".to_owned(),
                host,
                port,
                // Nothing else of the old proxy stays in force beside ours:
                // a SOCKS server left in place would carry what HTTP does not.
                quoted(""),
                "0".to_owned(),
                quoted("manual"),
            ];
            KEYS.iter()
                .zip(values)
                .map(|((schema, name), value)| (key(schema, name), value))
                .collect()
        }

        pub fn off() -> Settings {
            vec![(key(SCHEMA, "mode"), quoted("none"))]
        }

        pub fn signature(settings: &Settings) -> String {
            let get = |schema: &str, name: &str| {
                value(settings, &key(schema, name))
                    .map(unquoted)
                    .unwrap_or_default()
                    .to_owned()
            };
            format!(
                "mode={} http={}:{} https={}:{}",
                get(SCHEMA, "mode"),
                get("org.gnome.system.proxy.http", "host"),
                get("org.gnome.system.proxy.http", "port"),
                get("org.gnome.system.proxy.https", "host"),
                get("org.gnome.system.proxy.https", "port"),
            )
        }

        pub fn describe(settings: &Settings) -> (bool, String) {
            let get = |schema: &str, name: &str| {
                value(settings, &key(schema, name))
                    .map(unquoted)
                    .unwrap_or_default()
            };
            (
                get(SCHEMA, "mode") == "manual",
                format!(
                    "{}:{}",
                    get("org.gnome.system.proxy.http", "host"),
                    get("org.gnome.system.proxy.http", "port")
                ),
            )
        }

        /// `gsettings`, by name or, for tests, by path.
        pub struct Gsettings(pub OsString);

        impl Gsettings {
            pub fn installed() -> Self {
                Self(OsString::from("gsettings"))
            }

            /// `NoDesktopProxy` when there is no such store here: no
            /// `gsettings`, no schema, or no dconf behind it.
            pub fn read(&self) -> Result<Settings, ProxyError> {
                let output = match super::run(&self.0, &["list-recursively", SCHEMA]) {
                    Ok(output) => output,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        return Err(ProxyError::NoDesktopProxy);
                    }
                    Err(e) => return Err(ProxyError::Read(e)),
                };
                let said = String::from_utf8_lossy(&output.stderr);
                // Without dconf, GLib keeps settings in this process's memory
                // and forgets them as it exits: nothing written would reach
                // any other program.
                if said.contains("'memory' GSettings backend") || said.contains("No such schema") {
                    return Err(ProxyError::NoDesktopProxy);
                }
                if !output.status.success() {
                    return Err(ProxyError::Read(io::Error::other(format!(
                        "gsettings could not list {SCHEMA}: {}",
                        said.trim()
                    ))));
                }
                let text = String::from_utf8_lossy(&output.stdout);
                // "org.gnome.system.proxy.http host '127.0.0.1'"
                let listed: Vec<(&str, &str, &str)> = text
                    .lines()
                    .filter_map(|line| {
                        let (schema, rest) = line.split_once(' ')?;
                        let (name, value) = rest.split_once(' ')?;
                        Some((schema, name, value))
                    })
                    .collect();
                KEYS.iter()
                    .map(|(schema, name)| {
                        listed
                            .iter()
                            .find(|(s, n, _)| s == schema && n == name)
                            .map(|(_, _, v)| (key(schema, name), (*v).to_owned()))
                            .ok_or_else(|| {
                                ProxyError::Read(io::Error::other(format!(
                                    "gsettings has no {schema} {name}"
                                )))
                            })
                    })
                    .collect()
            }

            pub fn write(&self, settings: &Settings) -> Result<(), ProxyError> {
                for (k, v) in settings {
                    let (schema, name) = k
                        .strip_prefix(PREFIX)
                        .and_then(|rest| rest.split_once(' '))
                        .ok_or_else(|| {
                            ProxyError::Write(io::Error::other(format!("not a GNOME key: {k}")))
                        })?;
                    answer(&self.0, &["set", schema, name, v]).map_err(ProxyError::Write)?;
                }
                Ok(())
            }
        }
    }

    pub(super) mod kde {
        use std::ffi::OsString;
        use std::io;
        use std::net::SocketAddr;

        use super::super::{ProxyError, Settings, value};
        use super::{BYPASS, answer, on_path};

        pub const PREFIX: &str = "kde:";
        const FILE: &str = "kioslaverc";
        const GROUP: &str = "Proxy Settings";

        /// The keys bliss writes, the type last.
        const KEYS: [&str; 6] = [
            "NoProxyFor",
            "ReversedException",
            "httpProxy",
            "httpsProxy",
            "socksProxy",
            "ProxyType",
        ];

        /// What a key that was not in the file reads as, so that putting it
        /// back deletes it rather than writing an empty value in its place.
        pub const ABSENT: &str = "bliss:absent";

        fn key(name: &str) -> String {
            format!("{PREFIX}{name}")
        }

        /// KIO's own spelling: the scheme, the host, a space, the port.
        fn proxy(addr: SocketAddr) -> String {
            let host = match addr.ip() {
                std::net::IpAddr::V6(v6) => format!("[{v6}]"),
                v4 => v4.to_string(),
            };
            format!("http://{host} {}", addr.port())
        }

        pub fn pointing_at(addr: SocketAddr) -> Settings {
            let values = [
                BYPASS.join(","),
                "false".to_owned(),
                proxy(addr),
                proxy(addr),
                String::new(),
                // 1: "use manually specified proxy configuration".
                "1".to_owned(),
            ];
            KEYS.iter()
                .zip(values)
                .map(|(name, value)| (key(name), value))
                .collect()
        }

        pub fn off() -> Settings {
            vec![(key("ProxyType"), "0".to_owned())]
        }

        pub fn signature(settings: &Settings) -> String {
            let get = |name: &str| value(settings, &key(name)).unwrap_or_default();
            format!(
                "type={} http={} https={}",
                get("ProxyType"),
                get("httpProxy"),
                get("httpsProxy")
            )
        }

        pub fn describe(settings: &Settings) -> (bool, String) {
            let get = |name: &str| value(settings, &key(name)).unwrap_or_default();
            (get("ProxyType") == "1", get("httpProxy").to_owned())
        }

        /// `kreadconfig` and `kwriteconfig` of the session's Plasma, and
        /// whether to tell KIO when done — by path, for tests.
        pub struct Kconfig {
            pub read: OsString,
            pub write: OsString,
            pub announce: bool,
        }

        impl Kconfig {
            pub fn installed() -> Self {
                // Plasma 6 ships the 6 tools, Plasma 5 the 5; either may have
                // the other's beside it.
                let six = std::env::var("KDE_SESSION_VERSION").is_ok_and(|v| v.trim() == "6");
                let pick = |base: &str| {
                    let (first, second) = if six {
                        (format!("{base}6"), format!("{base}5"))
                    } else {
                        (format!("{base}5"), format!("{base}6"))
                    };
                    on_path(&[first.as_str(), second.as_str()]).unwrap_or_else(|| first.into())
                };
                Self {
                    read: pick("kreadconfig"),
                    write: pick("kwriteconfig"),
                    announce: true,
                }
            }

            pub fn read(&self) -> Result<Settings, ProxyError> {
                KEYS.iter()
                    .map(|name| {
                        let read = answer(
                            &self.read,
                            &[
                                "--file",
                                FILE,
                                "--group",
                                GROUP,
                                "--key",
                                name,
                                "--default",
                                ABSENT,
                            ],
                        )
                        .map_err(ProxyError::Read)?;
                        Ok((key(name), read.trim_end_matches(['\n', '\r']).to_owned()))
                    })
                    .collect()
            }

            pub fn write(&self, settings: &Settings) -> Result<(), ProxyError> {
                for (k, v) in settings {
                    let name = k.strip_prefix(PREFIX).ok_or_else(|| {
                        ProxyError::Write(io::Error::other(format!("not a KDE key: {k}")))
                    })?;
                    let mut args = vec!["--file", FILE, "--group", GROUP, "--key", name];
                    if v == ABSENT {
                        args.push("--delete");
                    } else {
                        args.push(v);
                    }
                    answer(&self.write, &args).map_err(ProxyError::Write)?;
                }
                if self.announce {
                    announce();
                }
                Ok(())
            }
        }

        /// Have KIO read the file again: the signal KDE's own settings send,
        /// under its Plasma 5 name, which Plasma 6 kept.
        fn announce() {
            let sent = answer(
                &OsString::from("dbus-send"),
                &[
                    "--session",
                    "--type=signal",
                    "/KIO/Scheduler",
                    "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
                    "string:",
                ],
            );
            if let Err(e) = sent {
                tracing::debug!("system proxy: could not tell KIO: {e}");
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::{ProxyError, Settings};

    pub fn read() -> Result<Settings, ProxyError> {
        Err(ProxyError::Unsupported)
    }

    pub fn read_like(_previous: &Settings) -> Result<Settings, ProxyError> {
        Err(ProxyError::Unsupported)
    }

    pub fn write(_settings: &Settings) -> Result<(), ProxyError> {
        Err(ProxyError::Unsupported)
    }

    pub fn pointing_at(
        _addr: std::net::SocketAddr,
        _now: &Settings,
    ) -> Result<Settings, ProxyError> {
        Err(ProxyError::Unsupported)
    }

    pub fn still_ours(_previous: &Settings, _current: &Settings, _ours: &str) -> Settings {
        Settings::new()
    }

    pub fn off() -> Result<Settings, ProxyError> {
        Err(ProxyError::Unsupported)
    }

    pub fn signature(_settings: &Settings) -> String {
        String::new()
    }

    pub fn describe(_settings: &Settings) -> (bool, String) {
        (false, String::new())
    }

    pub fn valid(settings: &Settings) -> bool {
        !settings.is_empty()
    }

    pub fn notify() {}

    pub fn process_alive(_pid: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings this platform's snapshot can hold.
    fn previous(server: &str) -> Settings {
        let pairs: Vec<(&str, String)> = if cfg!(target_os = "linux") {
            vec![
                (
                    "gnome:org.gnome.system.proxy.http host",
                    format!("'{server}'"),
                ),
                ("gnome:org.gnome.system.proxy mode", "'manual'".to_owned()),
            ]
        } else {
            vec![
                ("enable", "1".to_owned()),
                ("server", server.to_owned()),
                ("bypass", "localhost;<local>".to_owned()),
            ]
        };
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    }

    #[test]
    fn a_snapshot_round_trips_through_the_file_format() {
        let kept = Snapshot {
            previous: previous("proxy.corp.example:8080"),
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

    /// The lines every earlier bliss wrote on Windows still read, so a
    /// snapshot a dead older build left behind still puts the machine back.
    #[cfg(windows)]
    #[test]
    fn a_snapshot_from_an_older_bliss_still_reads() {
        let kept = parse_snapshot(
            "# bliss changed this machine's proxy settings and will put these back.\n\
             enable=0\nserver=\nbypass=\nours=http=127.0.0.1:10809;https=127.0.0.1:10809\n\
             pid=4242\n",
        )
        .expect("parses");
        assert_eq!(kept.pid, 4242);
        assert_eq!(value(&kept.previous, "enable"), Some("0"));
        assert_eq!(value(&kept.previous, "server"), Some(""));
        assert_eq!(kept.ours, "http=127.0.0.1:10809;https=127.0.0.1:10809");
    }

    /// A setting that would take two lines cannot be put back from one, so
    /// the snapshot is refused — and with it the proxy, by `set`.
    #[test]
    fn a_setting_that_spans_lines_is_refused() {
        let mut settings = previous("x");
        settings[0].1.push('\n');
        let kept = Snapshot {
            previous: settings,
            ours: "ours".into(),
            pid: 1,
        };
        let path = std::env::temp_dir()
            .join("bliss-sysproxy-test")
            .join("two-lines.txt");
        assert!(write_snapshot(&path, &kept).is_err());
        let _ = std::fs::remove_file(&path);
    }

    /// A second bliss must leave a running copy's settings alone on every
    /// path, not only at startup. `restore` used to skip the owner check that
    /// `recover` made, so a window opened next to a serving copy pulled that
    /// copy's proxy out as soon as it applied its own mode.
    ///
    /// Touches no settings: every call below has to return before reading or
    /// writing them. The snapshot is checked before `set` is tried, so code
    /// that lost the check fails here rather than going on to write a proxy.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_running_copys_settings_are_left_alone() {
        /// Ends the stand-in and removes the snapshot, and the stand-in's
        /// file if it has one, even when an assertion fails.
        struct Cleanup(std::process::Child, PathBuf, Option<PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
                let _ = std::fs::remove_file(&self.1);
                if let Some(file) = &self.2 {
                    let _ = std::fs::remove_file(file);
                }
            }
        }

        // A process that is certainly alive for as long as the test runs,
        // standing in for the other copy of bliss. On Linux only a process
        // whose executable is bliss counts as one (`imp::process_alive`), so
        // there it is a copy of `sleep` under a name of bliss's.
        let (other, file) = if cfg!(windows) {
            let other = std::process::Command::new("cmd")
                .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
                .spawn();
            (other, None)
        } else {
            let file = std::env::temp_dir().join(format!("bliss-stand-in-{}", std::process::id()));
            std::fs::copy("/bin/sleep", &file).expect("a copy of sleep");
            // A file just written can be "busy" for a moment: another test's
            // child, forked while it was open, holds it until that child execs.
            let mut tries = 0;
            let other = loop {
                match std::process::Command::new(&file).arg("30").spawn() {
                    Err(e) if e.raw_os_error() == Some(26) && tries < 20 => {
                        tries += 1;
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    other => break other,
                }
            };
            (other, Some(file))
        };
        let other = other.expect("a stand-in process");
        let dir = std::env::temp_dir().join("bliss-sysproxy-owner-test");
        let path = snapshot_path(&dir);
        let kept = Snapshot {
            previous: previous(""),
            ours: "http=127.0.0.1:1;https=127.0.0.1:1".into(),
            pid: other.id(),
        };
        let owner = kept.pid;
        let _cleanup = Cleanup(other, path.clone(), file);
        // Before anything reads a setting: a stand-in that did not count
        // would have the calls below go on to the machine's own.
        assert!(
            imp::process_alive(owner),
            "the stand-in counts as a running bliss"
        );
        write_snapshot(&path, &kept).expect("write");

        let restored = restore(&dir);
        assert!(matches!(restored, Ok(false)), "{restored:?}");
        let recovered = recover(&dir);
        assert!(matches!(recovered, Ok(false)), "{recovered:?}");
        assert_eq!(
            read_snapshot(&path),
            Some(kept),
            "the running copy's snapshot must still be there"
        );

        let taken = set(&dir, "127.0.0.1:1".parse().expect("an address"));
        assert!(
            matches!(taken, Err(ProxyError::OwnedElsewhere(pid)) if pid == owner),
            "{taken:?}"
        );
    }

    /// After a restart the number of the bliss that wrote a snapshot belongs
    /// to whatever this boot started at that point — on Linux pids are
    /// handed out in order from boot. A process that is not bliss is not a
    /// running copy; this one, the test, is called bliss.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_reused_number_is_not_a_running_copy() {
        let mut stranger = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        let counted = imp::process_alive(stranger.id());
        let _ = stranger.kill();
        let _ = stranger.wait();
        assert!(!counted, "sleep is not bliss");
        assert!(imp::process_alive(std::process::id()), "the test is");
        assert!(!imp::process_alive(0));
    }

    /// Once bliss has put the settings back on its way out, nothing it asks
    /// for after changes them. The work dir here cannot hold a snapshot, so
    /// even with the guard broken the machine's settings are never reached:
    /// `set` stops when its snapshot cannot be written.
    #[test]
    fn nothing_is_set_once_bliss_has_left() {
        let file = std::env::temp_dir().join(format!("bliss-sysproxy-left-{}", std::process::id()));
        std::fs::write(&file, b"a file, where the work dir's folder would be").expect("write");
        let dir = file.join("work");

        let left = leave(&dir);
        let applied = apply(
            ProxyMode::Set,
            &dir,
            Some("127.0.0.1:1".parse().expect("an address")),
        );
        *one_at_a_time() = false;
        let _ = std::fs::remove_file(&file);

        assert!(matches!(left, Ok(false)), "nothing to put back: {left:?}");
        assert!(matches!(applied, Ok(false)), "{applied:?}");
    }

    /// An empty bypass or server has to survive, because "no proxy at all" is
    /// the most common thing we are restoring to — and it is the case where
    /// losing a field means writing a proxy back on rather than off.
    #[test]
    fn an_empty_previous_setting_round_trips() {
        let empty = Snapshot {
            previous: previous(""),
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
            parse_snapshot("ours=x\npid=1\n"),
            None,
            "nothing to put back"
        );
        #[cfg(windows)]
        assert_eq!(
            parse_snapshot("enable=not-a-number\nserver=\nbypass=\nours=\n"),
            None
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            parse_snapshot("enable=1\nserver=\nbypass=\nours=\n"),
            None,
            "Windows' settings are no Linux desktop's"
        );
    }

    /// Everything but the owner. Unattributable, so it belongs to nobody and
    /// is ours to clean up — the opposite of refusing to parse it and leaving
    /// the machine pointed wherever the dead run left it.
    #[test]
    fn a_snapshot_with_no_owner_is_treated_as_orphaned() {
        let mut text: String = previous("old:8080")
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect();
        text.push_str("ours=ours:1\n");
        let kept = parse_snapshot(&text).expect("parses without a pid");
        assert_eq!(kept.pid, 0);
        assert!(
            !imp::process_alive(kept.pid),
            "pid 0 must never look like a live owner"
        );
    }

    #[cfg(windows)]
    #[test]
    fn both_schemes_point_at_the_http_inbound() {
        let addr = "127.0.0.1:10809".parse().unwrap();
        assert_eq!(
            imp::proxy_value(addr),
            "http=127.0.0.1:10809;https=127.0.0.1:10809"
        );
    }

    /// Loopback must be in the bypass list. Without it a browser configured
    /// through us tries to reach our own inbound through our own inbound.
    #[cfg(windows)]
    #[test]
    fn the_bypass_list_covers_loopback_and_private_ranges() {
        for must in ["localhost", "127.*", "192.168.*", "10.*", "<local>"] {
            assert!(imp::BYPASS.contains(must), "bypass list is missing {must}");
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use std::ffi::OsString;
        use std::path::{Path, PathBuf};

        use super::super::imp::{self, gnome, kde, plasma};
        use super::super::{Settings, value};

        #[test]
        fn plasma_is_read_from_the_list_of_names() {
            assert!(plasma("KDE"));
            assert!(plasma("foo:kde"));
            assert!(!plasma("ubuntu:GNOME"));
            assert!(!plasma("XFCE"));
            assert!(!plasma(""));
        }

        #[test]
        fn loopback_is_never_sent_to_the_proxy() {
            for must in ["localhost", "127.0.0.0/8", "::1", "192.168.0.0/16"] {
                assert!(imp::BYPASS.contains(&must), "bypass list is missing {must}");
            }
        }

        /// A folder of its own with a stand-in program in it, removed after.
        struct Bench(PathBuf);

        impl Bench {
            fn new(name: &str) -> Self {
                let dir = std::env::temp_dir()
                    .join(format!("bliss-sysproxy-{name}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).expect("a folder");
                Self(dir)
            }

            fn program(&self, name: &str, script: &str) -> OsString {
                use std::os::unix::fs::PermissionsExt;
                let path = self.0.join(name);
                std::fs::write(&path, script).expect("the stand-in");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("runnable");
                path.into_os_string()
            }

            fn file(&self, name: &str) -> PathBuf {
                self.0.join(name)
            }
        }

        impl Drop for Bench {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// `gsettings` with its two verbs, over a text file of
        /// `schema key value` lines.
        const GSETTINGS: &str = r#"#!/bin/sh
state="$(dirname "$0")/dconf"
case "$1" in
  list-recursively) cat "$state" ;;
  set) grep -v "^$2 $3 " "$state" > "$state.new"; echo "$2 $3 $4" >> "$state.new"; mv "$state.new" "$state" ;;
  *) exit 1 ;;
esac
"#;

        /// A machine with a corporate proxy found by WPAD, and a password
        /// nobody should copy anywhere.
        const DCONF: &str = "org.gnome.system.proxy autoconfig-url 'http://wpad.example/wpad.dat'
org.gnome.system.proxy ignore-hosts ['localhost', '127.0.0.0/8', '::1']
org.gnome.system.proxy mode 'auto'
org.gnome.system.proxy use-same-proxy true
org.gnome.system.proxy.http authentication-password 'PPPPPPPP'
org.gnome.system.proxy.http authentication-user 'user'
org.gnome.system.proxy.http enabled false
org.gnome.system.proxy.http host 'proxy.corp.example'
org.gnome.system.proxy.http port 8080
org.gnome.system.proxy.http use-authentication true
org.gnome.system.proxy.https host ''
org.gnome.system.proxy.https port 0
org.gnome.system.proxy.socks host 'socks.corp.example'
org.gnome.system.proxy.socks port 1080
";

        fn sorted(path: &Path) -> Vec<String> {
            let mut lines: Vec<String> = std::fs::read_to_string(path)
                .expect("the state")
                .lines()
                .map(str::to_owned)
                .collect();
            lines.sort();
            lines
        }

        /// The whole round through GNOME's store: read, point at bliss, see it
        /// as ours, put the old settings back — and every line ends as it
        /// began, the WPAD address bliss never touched included. The password
        /// beside them is not among what is kept.
        #[test]
        fn gnome_settings_go_back_as_they_were() {
            let bench = Bench::new("gnome");
            std::fs::write(bench.file("dconf"), DCONF).expect("the state");
            let store = gnome::Gsettings(bench.program("gsettings", GSETTINGS));

            let before = store.read().expect("reads");
            assert!(
                before.iter().all(|(_, v)| !v.contains("PPPP")),
                "a password was kept: {before:?}"
            );
            assert!(!imp::describe(&before).0, "auto is not manual");

            let ours = gnome::pointing_at("127.0.0.1:10809".parse().unwrap());
            store.write(&ours).expect("writes");
            let now = store.read().expect("reads again");
            assert_eq!(imp::describe(&now), (true, "127.0.0.1:10809".to_owned()));
            assert_eq!(
                imp::signature(&now),
                imp::signature(&ours),
                "ours by its look"
            );
            assert_eq!(
                value(&now, "gnome:org.gnome.system.proxy.socks host"),
                Some("''"),
                "no SOCKS server is left beside ours"
            );
            assert_eq!(
                value(&now, "gnome:org.gnome.system.proxy.http use-authentication"),
                Some("false"),
                "the old proxy's login is not offered to bliss"
            );

            store.write(&before).expect("puts back");
            let mut original: Vec<String> = DCONF.lines().map(str::to_owned).collect();
            original.sort();
            assert_eq!(sorted(&bench.file("dconf")), original);
        }

        /// `kreadconfig` and `kwriteconfig` in one, over `key=value` lines.
        const KCONFIG: &str = r#"#!/bin/sh
state="$(dirname "$0")/kioslaverc"
key=; default=; delete=; value=; have=
while [ $# -gt 0 ]; do
  case "$1" in
    --file|--group) shift 2 ;;
    --key) key="$2"; shift 2 ;;
    --default) default="$2"; shift 2 ;;
    --delete) delete=1; shift ;;
    *) value="$1"; have=1; shift ;;
  esac
done
case "$(basename "$0")" in
  kread*) line=$(grep "^$key=" "$state"); if [ -n "$line" ]; then echo "${line#*=}"; else echo "$default"; fi ;;
  kwrite*) grep -v "^$key=" "$state" > "$state.new"; if [ -z "$delete" ]; then echo "$key=$value" >> "$state.new"; fi; mv "$state.new" "$state" ;;
esac
"#;

        /// The same round through KDE's file, where one key was never there:
        /// it is deleted on the way back, not written empty.
        #[test]
        fn kde_settings_go_back_as_they_were() {
            let bench = Bench::new("kde");
            let original = "ProxyType=0\nhttpProxy=http://proxy.corp.example 8080\n\
                            NoProxyFor=localhost\nReversedException=true\n";
            std::fs::write(bench.file("kioslaverc"), original).expect("the state");
            let store = kde::Kconfig {
                read: bench.program("kreadconfig5", KCONFIG),
                write: bench.program("kwriteconfig5", KCONFIG),
                announce: false,
            };

            let before: Settings = store.read().expect("reads");
            assert_eq!(value(&before, "kde:socksProxy"), Some(kde::ABSENT));

            store
                .write(&kde::pointing_at("127.0.0.1:10809".parse().unwrap()))
                .expect("writes");
            let now = store.read().expect("reads again");
            assert_eq!(
                imp::describe(&now),
                (true, "http://127.0.0.1 10809".to_owned())
            );
            assert_eq!(value(&now, "kde:socksProxy"), Some(""));
            assert_eq!(
                value(&now, "kde:ReversedException"),
                Some("false"),
                "the bypass list is a bypass list while bliss has the proxy"
            );

            store.write(&before).expect("puts back");
            let mut after = sorted(&bench.file("kioslaverc"));
            let mut wanted: Vec<String> = original.lines().map(str::to_owned).collect();
            after.retain(|line| !line.is_empty());
            wanted.sort();
            assert_eq!(after, wanted, "socksProxy is gone again, not empty");
        }

        /// On Plasma the two stores are put back one by one: KDE's changed by
        /// hand stays as the person left it, while GNOME's — Firefox's —
        /// still pointed at bliss goes back, rather than both being left on a
        /// port nobody listens on.
        #[test]
        fn each_store_goes_back_on_its_own() {
            let addr = "127.0.0.1:10809".parse().unwrap();
            let mut ours = gnome::pointing_at(addr);
            ours.extend(kde::pointing_at(addr));
            let signature = imp::signature(&ours);

            let previous: Settings = vec![
                (
                    "gnome:org.gnome.system.proxy mode".to_owned(),
                    "'none'".to_owned(),
                ),
                ("kde:ProxyType".to_owned(), "0".to_owned()),
            ];
            let mut current = gnome::pointing_at(addr);
            let mut by_hand = kde::pointing_at(addr);
            by_hand.retain(|(k, _)| k != "kde:ProxyType");
            by_hand.push(("kde:ProxyType".to_owned(), "0".to_owned()));
            current.extend(by_hand);

            let back = imp::still_ours(&previous, &current, &signature);
            assert_eq!(
                back,
                vec![(
                    "gnome:org.gnome.system.proxy mode".to_owned(),
                    "'none'".to_owned()
                )],
                "GNOME's goes back, KDE's is left as the person set it"
            );
            assert_eq!(imp::still_ours(&previous, &ours, &signature), previous);
        }

        /// A snapshot whose settings cannot be read is kept, not forgotten:
        /// here a KDE snapshot where no KDE tool is installed. Nothing is
        /// written, since nothing could be read.
        #[test]
        fn a_snapshot_that_cannot_be_read_back_is_kept() {
            let tools = ["kreadconfig5", "kreadconfig6"];
            let installed = std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path)
                    .any(|dir| tools.iter().any(|tool| dir.join(tool).is_file()))
            });
            if installed {
                return;
            }
            let dir =
                std::env::temp_dir().join(format!("bliss-sysproxy-unread-{}", std::process::id()));
            let path = super::super::snapshot_path(&dir);
            let kept = super::super::Snapshot {
                previous: vec![("kde:ProxyType".to_owned(), "0".to_owned())],
                ours: "kde[type=1 http=http://127.0.0.1 1 https=http://127.0.0.1 1]".to_owned(),
                pid: 0,
            };
            super::super::write_snapshot(&path, &kept).expect("written");
            assert!(
                super::super::restore(&dir).is_err(),
                "nothing could be read"
            );
            assert!(path.exists(), "the snapshot stays for the next try");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Every key names its store, so a snapshot of both goes back into
        /// both — GNOME's keys are never handed to KDE's tools — and what makes
        /// it ours does not depend on which store was read first.
        #[test]
        fn a_snapshot_keeps_each_key_with_its_store() {
            let gnome = gnome::pointing_at("127.0.0.1:1".parse().unwrap());
            let kde = kde::pointing_at("127.0.0.1:1".parse().unwrap());
            let mut both = kde.clone();
            both.extend(gnome.clone());
            let mut other_way = gnome.clone();
            other_way.extend(kde.clone());
            assert!(imp::valid(&both));
            assert_eq!(imp::signature(&both), imp::signature(&other_way));
            assert_ne!(imp::signature(&both), imp::signature(&gnome));
            assert_eq!(
                imp::describe(&both).1,
                "http://127.0.0.1 1",
                "KDE's, on Plasma"
            );

            let mut stray = both.clone();
            stray.push(("enable".to_owned(), "1".to_owned()));
            assert!(!imp::valid(&stray), "a key of no store");
            assert!(!imp::valid(&Vec::new()));
        }
    }
}
