//! One window per executable on Linux, and a second copy that brings the
//! first one up instead of starting.
//!
//! Windows' reason for the rule — WebView2 gives one browser process to one
//! data folder — does not hold here: WebKitGTK starts processes of its own for
//! every copy. The rest of it does: a second copy shares the work dir, the
//! profile and the ports. And here the second copy has a job Windows' cannot
//! do. A window in the tray is hidden, and a desktop without a tray host
//! (GNOME as it ships, before an extension) shows no icon to bring it back
//! with: starting bliss again from the application menu is how it comes back.
//!
//! The claim is a `flock` on a file in `$XDG_RUNTIME_DIR` — the kernel lets go
//! of it however the holder ends — and beside it a socket the holder listens
//! on. A second copy finds the lock taken, connects, says `show` with the
//! activation token its launcher gave it, and leaves; the first shows its
//! window with that token, which is what lets a Wayland desktop raise a window
//! the user asked for rather than flash "bliss is ready". Both are named after
//! the executable, as on Windows, so a renamed probe copy is a claim of its own
//! and may run beside the user's bliss.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// An ask to show the window, with the activation token, if the copy that
/// asked had one.
pub type Raise = Option<String>;

/// The claim, held for as long as the process runs, and the asks that arrive
/// while it is held.
pub struct OnlyWindow {
    /// Held, not read: the lock lives as long as this descriptor.
    _lock: Option<std::fs::File>,
    raises: Option<tokio::sync::mpsc::UnboundedReceiver<Raise>>,
}

impl OnlyWindow {
    /// The asks to show the window, for the window to answer. Once.
    pub fn take_raises(&mut self) -> Option<tokio::sync::mpsc::UnboundedReceiver<Raise>> {
        self.raises.take()
    }
}

/// Where the claim and its socket live: the session's runtime folder, which
/// only this user can enter, or the user's cache when there is none.
fn place() -> (PathBuf, PathBuf) {
    let dir = directories::BaseDirs::new()
        .and_then(|base| {
            base.runtime_dir()
                .map(Path::to_path_buf)
                .or_else(|| Some(base.cache_dir().join("bliss")))
        })
        .unwrap_or_else(std::env::temp_dir);
    let name = format!("bliss-window-{}", stem());
    (
        dir.join(format!("{name}.lock")),
        dir.join(format!("{name}.sock")),
    )
}

/// The executable's name, with anything a file name should not hold dropped.
fn stem() -> String {
    let stem = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let kept: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if kept.is_empty() {
        "bliss".to_owned()
    } else {
        kept
    }
}

/// Take the claim, or answer `None` when another window already has it.
pub fn only_window() -> Option<OnlyWindow> {
    let (lock, socket) = place();
    claim(&lock, &socket)
}

/// The claim itself, by path, so a test can take one of its own instead of
/// fighting whichever bliss the person has open.
fn claim(lock: &Path, socket: &Path) -> Option<OnlyWindow> {
    if let Some(dir) = lock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock)
    {
        Ok(file) => file,
        Err(e) => {
            // A window without the claim is better than none: the person asked
            // for bliss, and nothing here is theirs to fix.
            tracing::warn!("no single-window claim ({}): {e}", lock.display());
            return Some(OnlyWindow {
                _lock: None,
                raises: None,
            });
        }
    };
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return None;
        }
        tracing::warn!("no single-window claim ({}): {error}", lock.display());
        return Some(OnlyWindow {
            _lock: Some(file),
            raises: None,
        });
    }

    // Whatever socket is there was left by a copy that has gone: the lock
    // says no live one holds it.
    let _ = std::fs::remove_file(socket);
    let raises = match UnixListener::bind(socket) {
        Ok(listener) => Some(listen(listener)),
        Err(e) => {
            tracing::warn!("a second bliss will not be able to show this window: {e}");
            None
        }
    };
    Some(OnlyWindow {
        _lock: Some(file),
        raises,
    })
}

/// The most an ask may be: a word and a token.
const ASK: u64 = 4096;

/// Answer every copy that connects, on a thread of its own.
fn listen(listener: UnixListener) -> tokio::sync::mpsc::UnboundedReceiver<Raise> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let spawned = std::thread::Builder::new()
        .name("bliss-raise".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                // A copy that connects and says nothing must not hold the
                // next one up.
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut asked = String::new();
                if stream.take(ASK).read_to_string(&mut asked).is_err() {
                    continue;
                }
                let mut lines = asked.lines();
                if lines.next() != Some("show") {
                    continue;
                }
                let token = lines
                    .next()
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned);
                if tx.send(token).is_err() {
                    return;
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("a second bliss will not be able to show this window: {e}");
    }
    rx
}

/// Ask the window that is already running to show itself.
pub fn raise_the_running_window() {
    let (_, socket) = place();
    if let Err(e) = ask(&socket, launcher_token()) {
        tracing::info!("could not reach the running bliss ({e}); its icon is in the tray");
    }
}

/// The activation token this copy's launcher gave it, to hand on: Wayland's,
/// or X11's startup notification.
fn launcher_token() -> Option<String> {
    ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
}

fn ask(socket: &Path, token: Option<String>) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(format!("show\n{}\n", token.unwrap_or_default()).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One window, an ask from a second copy reaches it with its token, and
    /// the claim comes back when the first lets go.
    #[test]
    fn only_one_copy_holds_the_window_and_the_next_one_raises_it() {
        let dir = std::env::temp_dir().join(format!("bliss-instance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lock = dir.join("w.lock");
        let socket = dir.join("w.sock");

        let mut first = claim(&lock, &socket).expect("the first copy takes the claim");
        assert!(
            claim(&lock, &socket).is_none(),
            "a second copy is refused while the first holds it"
        );

        let mut raises = first.take_raises().expect("the first listens");
        ask(&socket, Some("token-1".to_owned())).expect("the second asks");
        ask(&socket, None).expect("and asks again, with no token");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let wait = std::time::Duration::from_secs(5);
            let one = tokio::time::timeout(wait, raises.recv()).await;
            assert_eq!(one.expect("in time"), Some(Some("token-1".to_owned())));
            let two = tokio::time::timeout(wait, raises.recv()).await;
            assert_eq!(two.expect("in time"), Some(None));
        });

        drop(first);
        let again = claim(&lock, &socket);
        assert!(
            again.is_some(),
            "the claim is free again once that copy lets go"
        );
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
