//! One OpenVPN connection, from the profile written to disk to OpenVPN gone.
//!
//! bliss listens on a loopback port and OpenVPN, started by its service,
//! connects to it (`--management-client`). The connection is the leash:
//! OpenVPN leaves when it closes (`management-exit`), so a bliss that dies
//! takes its VPN with it rather than leaving it up with nobody to switch it
//! off. It starts held (`--management-hold`) so nothing is missed before bliss
//! is listening to it, and `hold off` keeps a reconnect from waiting for bliss.
//!
//! Linux has no service: OpenVPN is started with root's rights through
//! polkit (`service::linux`), reads the profile from its standard input — the
//! key never touches the disk — and calls bliss over a unix socket in the
//! session's own folder, where the kernel names the caller's process and
//! user (`SO_PEERCRED`). What it says before it calls comes from its output,
//! as there is no log file of its own to read: a root OpenVPN's would be
//! root's.

use std::net::{Ipv4Addr, SocketAddr};
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(windows)]
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::management::{self, Line};
use crate::profile;
#[cfg(any(windows, target_os = "linux"))]
use crate::service;

/// How long OpenVPN has to call bliss once the service has started it.
#[cfg(windows)]
const CALL: Duration = Duration::from_secs(20);

/// On Linux the start waits on a person typing a password into polkit's
/// dialog, which is not a thing to hurry.
#[cfg(not(windows))]
const CALL: Duration = Duration::from_secs(180);

/// How long OpenVPN has to leave once asked to.
const LEAVE: Duration = Duration::from_secs(8);

/// How long the VPN's address has, once OpenVPN is connected, to take a
/// socket before bliss says it is up anyway.
const USABLE: Duration = Duration::from_secs(5);

/// How much of the end of OpenVPN's own log is read for why it stopped: a
/// log of a connection that ran for days is long, and only its last words
/// matter here.
#[cfg(windows)]
const LOG_TAIL: u64 = 64 * 1024;

/// What happens to the connection, as it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A step on the way up, in OpenVPN's word for it: `RESOLVE`, `WAIT`,
    /// `AUTH`, `GET_CONFIG`, `ASSIGN_IP`, `ADD_ROUTES`…
    Step(String),
    /// Up: its address on the work network and the DNS servers the server
    /// offered, which bliss asks itself.
    Up {
        local: Ipv4Addr,
        dns: Vec<SocketAddr>,
    },
    /// Down for a moment while OpenVPN reconnects, and why.
    Lost(String),
    /// A line of OpenVPN's worth a line in bliss's log: an error or warning.
    Said(String),
    /// Gone: `None` when it was asked to go, otherwise why it went.
    Ended(Option<String>),
}

/// A connection being made or held; dropping it without `stop` ends it too,
/// as the leash breaks.
pub struct Session {
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<()>,
}

impl Session {
    /// Start OpenVPN with `profile`, in `directory`, once `after` — the one it
    /// replaces — has gone, telling `report` what happens. Must be called from
    /// within a tokio runtime.
    pub fn start(
        profile: String,
        directory: PathBuf,
        after: Option<Leaving>,
        report: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        let (stop, stopped) = oneshot::channel();
        let report: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(report);
        let done = tokio::spawn(async move {
            // One OpenVPN at a time: two would fight over the adapter, and the
            // one leaving takes routes away as it goes.
            if let Some(before) = after {
                before.wait().await;
            }
            let end = run(profile, directory, stopped, Arc::clone(&report)).await;
            report(Event::Ended(end.err()));
        });
        Self {
            stop: Some(stop),
            done,
        }
    }

    /// Ask OpenVPN to leave, and wait until it has.
    pub async fn stop(self) {
        self.leave().wait().await;
    }

    /// Ask OpenVPN to leave without waiting for it: what comes back is
    /// something to wait on, which the next connection does.
    pub fn leave(mut self) -> Leaving {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        Leaving(self.done)
    }
}

/// A connection on its way out.
pub struct Leaving(tokio::task::JoinHandle<()>);

impl Leaving {
    /// Until it has gone, or it plainly is not going to.
    pub async fn wait(self) {
        let _ = tokio::time::timeout(CALL + LEAVE + Duration::from_secs(4), self.0).await;
    }
}

/// The profile bliss writes, with its key, and deletes when it is done.
#[cfg(windows)]
struct Written(PathBuf);

#[cfg(windows)]
impl Drop for Written {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(windows)]
async fn run(
    profile: String,
    directory: PathBuf,
    mut stop: oneshot::Receiver<()>,
    report: Arc<dyn Fn(Event) + Send + Sync>,
) -> Result<(), String> {
    // Told to go while waiting for the one before it: nothing to start.
    if !matches!(stop.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
        return Ok(());
    }
    let written = profile::rewrite(&profile).map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|e| format!("не удалось создать папку для OpenVPN: {e}"))?;
    let path = directory.join("profile.ovpn");
    tokio::fs::write(&path, written)
        .await
        .map_err(|e| format!("не удалось записать профиль OpenVPN: {e}"))?;
    let written = Written(path.clone());

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| format!("не удалось занять порт для связи с OpenVPN: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // OpenVPN's own log, written over at every start: what it says before
    // it calls — an option it will not take, a file it cannot read — is
    // said nowhere else.
    let log = directory.join("openvpn.log");
    let options = options(&path, &log, port);

    let dir = directory.clone();
    let started = tokio::task::spawn_blocking(move || service::start(&dir, &options))
        .await
        .map_err(|e| format!("поток службы OpenVPN не дожил до конца: {e}"))?
        .map_err(|e| e.to_string())?;
    let pid = started.pid;
    tracing::info!("openvpn: started by its service, process {pid}");
    // Waits on the service for as long as OpenVPN runs: it writes again when
    // OpenVPN exits with an error, and hangs up when it has gone.
    let mut last_word = tokio::task::spawn_blocking(move || started.pipe.last_word());

    let stream = tokio::select! {
        called = answer(&listener, pid, CALL) => match called {
            Ok(stream) => stream,
            Err(e) => {
                service::terminate(pid);
                return Err(explained(e, &log, &path).await);
            }
        },
        said = &mut last_word => {
            // Gone before it called, which is no reason to wait out the call.
            let note = said.ok().flatten().map_or_else(String::new, |said| exit_note(&said));
            let what = format!("OpenVPN завершился{note}, не связавшись с bliss");
            return Err(explained(what, &log, &path).await);
        }
        _ = &mut stop => {
            service::terminate(pid);
            return Ok(());
        }
    };
    drop(listener);
    // OpenVPN reads its profile before it calls, keys and all, and keeps it
    // in memory across reconnects; only a SIGHUP, which nothing here sends,
    // would read the file again. So the key leaves the disk now rather than
    // when the VPN goes — or never, if bliss is killed before then.
    drop(written);

    match converse(stream, &mut stop, &report).await {
        Ok(Ending::Asked) => Ok(()),
        Ok(Ending::Overstayed) => {
            // Asked and still there: made to. The service takes the routes
            // away as the process ends.
            service::terminate(pid);
            Ok(())
        }
        // Gone by itself: its own log, then the service, say why.
        Ok(Ending::Left) => {
            let note = match tokio::time::timeout(Duration::from_secs(2), last_word).await {
                Ok(Ok(Some(said))) => exit_note(&said),
                _ => String::new(),
            };
            Err(explained(format!("OpenVPN завершился{note}"), &log, &path).await)
        }
        Err(why) => Err(why),
    }
}

/// What went wrong, with the line of OpenVPN's own log that says why, when
/// one does. That line goes to bliss's log too.
#[cfg(windows)]
async fn explained(what: String, log: &Path, profile: &Path) -> String {
    let log = log.to_owned();
    let reason = tokio::task::spawn_blocking(move || reason_in_log(&log))
        .await
        .ok()
        .flatten();
    match reason {
        Some(reason) => {
            let reason = their_line(&reason, &profile.display().to_string());
            tracing::warn!("openvpn: its log says: {reason}");
            format!("{what}: {reason}")
        }
        None => what,
    }
}

/// OpenVPN names a line of the profile by the path bliss wrote it to — or
/// `stdin`, where it read it from on Linux — and its number there, neither of
/// which the person reading it has seen: what they have is their own file,
/// where the same line comes `PREAMBLE` lines earlier.
fn their_line(reason: &str, written: &str) -> String {
    let named = format!("{written}:");
    let Some((before, after)) = reason.split_once(&named) else {
        return reason.to_owned();
    };
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    match after[..digits].parse::<usize>() {
        Ok(line) if line > profile::PREAMBLE => format!(
            "{before}line {}{}",
            line - profile::PREAMBLE,
            &after[digits..]
        ),
        _ => reason.to_owned(),
    }
}

/// The line at the end of OpenVPN's log that says why it stopped.
#[cfg(windows)]
fn reason_in_log(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(LOG_TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(LOG_TAIL).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // Started mid-line: the first one is a piece.
    let skip = usize::from(start > 0);
    reason(text.lines().skip(skip))
}

/// Why OpenVPN stopped, by its log's last words: an options error, a failed
/// call to bliss, or the line before "Exiting due to fatal error", which is
/// what OpenVPN prints after the fatal one.
fn reason<'a>(lines: impl Iterator<Item = &'a str>) -> Option<String> {
    let lines: Vec<&str> = lines
        .map(|line| without_time(line.trim()))
        .filter(|line| !line.is_empty())
        .collect();
    for (at, line) in lines.iter().enumerate().rev() {
        if line.starts_with("Options error:")
            || (line.starts_with("MANAGEMENT: connect to") && line.contains("failed"))
        {
            return Some((*line).to_owned());
        }
        if line.contains("Exiting due to fatal error") {
            return at.checked_sub(1).map(|before| lines[before].to_owned());
        }
    }
    None
}

/// A log line without the time OpenVPN writes before it, `2026-10-04 02:17:19`.
fn without_time(line: &str) -> &str {
    let stamped = line.len() > 20
        && line.as_bytes()[..19]
            .iter()
            .enumerate()
            .all(|(at, byte)| match at {
                4 | 7 => *byte == b'-',
                10 => *byte == b' ',
                13 | 16 => *byte == b':',
                _ => byte.is_ascii_digit(),
            })
        && line.as_bytes()[19] == b' ';
    if stamped {
        line[20..].trim_start()
    } else {
        line
    }
}

/// The service's word on an OpenVPN that exited with an error — its second
/// line, `OpenVPN exited with error: exit code = 1` — as the end of a
/// sentence that starts "OpenVPN завершился".
#[cfg(windows)]
fn exit_note(said: &str) -> String {
    let code = said
        .split('\n')
        .nth(1)
        .and_then(|line| line.trim().rsplit_once(": "))
        .map(|(_, code)| code.trim())
        .filter(|code| !code.is_empty());
    match code {
        Some(code) => format!(" с ошибкой ({code})"),
        None => " с ошибкой".to_owned(),
    }
}

/// How a conversation ended, when it ended without an error.
#[derive(Debug, PartialEq, Eq)]
enum Ending {
    /// Asked to go, and gone.
    Asked,
    /// Asked to go, and still there when the time was up.
    Overstayed,
    /// Gone without being asked.
    Left,
}

/// OpenVPN's options: everything else is in the profile. The log comes
/// first, since OpenVPN writes there from the moment it reads `--log`, and a
/// line of the profile it will not take is refused while the profile is
/// read; after it, that refusal went nowhere. The rest come after the
/// profile, so they win over anything it says.
#[cfg(windows)]
fn options(profile: &Path, log: &Path, port: u16) -> String {
    format!(
        "--log {} --config {} --verb 3 --management 127.0.0.1 {port} --management-client \
         --management-hold --management-query-passwords",
        service::quoted(log),
        service::quoted(profile),
    )
}

/// Wait for OpenVPN to call: the process the service started, and no other,
/// since the port is on loopback, where any program can knock.
///
/// The caller is known by the connection's row in the machine's table. It
/// was known by its name, and the name never came: the service's access
/// list lets the user read OpenVPN's name only by a right bliss did not ask
/// for, so every call was turned away as a stranger's, OpenVPN left as its
/// leash broke, and bliss waited out the rest of the 20 seconds for a call
/// that could not come again — the first thing a real OpenVPN showed.
#[cfg(windows)]
async fn answer(listener: &TcpListener, pid: u32, within: Duration) -> Result<TcpStream, String> {
    let at = listener.local_addr().map_err(|e| e.to_string())?;
    let wait = async {
        loop {
            let (stream, peer) = listener
                .accept()
                .await
                .map_err(|e| format!("OpenVPN не смог связаться с bliss: {e}"))?;
            let caller = bliss_core::process_of(peer, at);
            if caller == Some(pid) {
                return Ok(stream);
            }
            tracing::warn!(
                "openvpn: turned away a call from {}, waiting for process {pid}",
                caller.map_or_else(
                    || "an unknown program".to_owned(),
                    |c| format!("process {c}")
                )
            );
        }
    };
    match tokio::time::timeout(within, wait).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "OpenVPN не вышел на связь за {} с",
            within.as_secs()
        )),
    }
}

/// Talk to OpenVPN until it leaves, over whatever it called on: a loopback
/// connection on Windows, a unix socket on Linux.
async fn converse<S>(
    stream: S,
    stop: &mut oneshot::Receiver<()>,
    report: &Arc<dyn Fn(Event) + Send + Sync>,
) -> Result<Ending, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    // Notifications first, then the history of what was said before bliss
    // listened, then off it goes.
    write
        .write_all(b"state on\r\nlog on all\r\nhold off\r\nhold release\r\n")
        .await
        .map_err(|e| format!("OpenVPN не принял команду: {e}"))?;

    let mut dns: Vec<SocketAddr> = Vec::new();
    let mut fatal: Option<String> = None;
    let mut last_error: Option<String> = None;
    let mut asked = false;
    let mut deadline: Option<tokio::time::Instant> = None;
    let mut holds = 0u32;

    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            _ = &mut *stop, if !asked => {
                asked = true;
                deadline = Some(tokio::time::Instant::now() + LEAVE);
                let _ = write.write_all(b"signal SIGTERM\r\n").await;
                continue;
            }
            _ = sleep_until(deadline), if deadline.is_some() => return Ok(Ending::Overstayed),
        };
        let Ok(Some(line)) = line else {
            // The connection closed: OpenVPN has gone.
            break;
        };
        match management::parse(&line) {
            Line::State(state) => match state.name.as_str() {
                "CONNECTED" => {
                    let Some(local) = state.local else {
                        fatal = Some("OpenVPN подключился, но не сказал своего адреса".to_owned());
                        let _ = write.write_all(b"signal SIGTERM\r\n").await;
                        continue;
                    };
                    if state.detail == "ERROR" {
                        report(Event::Said(
                            "connected with errors on the way — a route may be missing".to_owned(),
                        ));
                    }
                    if !usable(local, USABLE).await {
                        report(Event::Said(format!(
                            "the VPN's address {local} took no socket within {} s",
                            USABLE.as_secs()
                        )));
                    }
                    report(Event::Up {
                        local,
                        dns: dns.clone(),
                    });
                }
                "RECONNECTING" => {
                    dns.clear();
                    report(Event::Lost(state.detail));
                }
                "EXITING" => {}
                step => report(Event::Step(step.to_owned())),
            },
            Line::Log { flags, text } => {
                if let Some(found) = management::pushed_dns(&text) {
                    for server in found {
                        if !dns.contains(&server) {
                            dns.push(server);
                        }
                    }
                } else if management::worth_saying(&flags, &text) {
                    if flags.contains(['F', 'N']) {
                        last_error = Some(text.clone());
                    }
                    report(Event::Said(text));
                }
            }
            // The first is the hold OpenVPN starts in, which it announces as
            // it calls and the commands above already release. Released a
            // second time, the command arrived while OpenVPN was still
            // printing its history, and OpenVPN 2.7 ran `log on all` again
            // inside it: every line of the history came twice, and so did
            // every warning in bliss's log.
            Line::Hold => {
                holds += 1;
                if holds > 1 {
                    let _ = write.write_all(b"hold release\r\n").await;
                }
            }
            Line::Password(what) => {
                fatal = Some(format!(
                    "OpenVPN просит пароль ({what}), а bliss пока так не умеет"
                ));
                let _ = write.write_all(b"signal SIGTERM\r\n").await;
            }
            Line::Fatal(why) => fatal = Some(why),
            Line::Other => {}
        }
    }

    // Gone without being asked and without a `>FATAL`: the last error it
    // logged is the best account of why — a refused key says so there, and
    // the service only knows the exit code.
    let why = match fatal {
        Some(why) => Some(why),
        None if asked => None,
        None => last_error,
    };
    match why {
        Some(why) => Err(why),
        None if asked => Ok(Ending::Asked),
        None => Ok(Ending::Left),
    }
}

/// Until the adapter's new address takes a socket, or `within` has passed.
///
/// Windows makes sure nobody else has an address before it lets anything
/// use it, and OpenVPN says it is connected before that is done: for about a
/// second after, measured with the window, binding to the address was
/// refused, and a connection a rule sent through the VPN in that second
/// failed for nothing.
async fn usable(address: Ipv4Addr, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if std::net::UdpSocket::bind((address, 0)).is_ok() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

// --- Linux -------------------------------------------------------------------

/// The most of OpenVPN's output kept for why it stopped: its last words are
/// what matter, and a connection that ran for days says a great deal.
#[cfg(target_os = "linux")]
const TAIL: usize = 64 * 1024;

/// The longest line of it kept whole; the rest of a longer one is dropped as
/// it arrives rather than gathered first.
#[cfg(target_os = "linux")]
const LINE: usize = 4096;

/// The end of what OpenVPN printed, which without `--log` is its log.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct Tail {
    lines: std::collections::VecDeque<String>,
    bytes: usize,
}

#[cfg(target_os = "linux")]
impl Tail {
    fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.bytes > TAIL {
            match self.lines.pop_front() {
                Some(gone) => self.bytes -= gone.len(),
                None => break,
            }
        }
    }

    fn reason(&self) -> Option<String> {
        reason(self.lines.iter().map(String::as_str))
    }
}

/// Read OpenVPN's output into the tail until it closes.
#[cfg(target_os = "linux")]
async fn drain(mut from: impl AsyncRead + Unpin, tail: Arc<std::sync::Mutex<Tail>>) {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 4096];
    let mut line = Vec::new();
    loop {
        let n = match from.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &byte in &chunk[..n] {
            if byte == b'\n' {
                let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                if let Ok(mut tail) = tail.lock() {
                    tail.push(text);
                }
                line.clear();
            } else if line.len() < LINE {
                line.push(byte);
            }
        }
    }
    if !line.is_empty()
        && let Ok(mut tail) = tail.lock()
    {
        tail.push(String::from_utf8_lossy(&line).trim_end().to_owned());
    }
}

/// What went wrong, with the line of OpenVPN's output that says why.
#[cfg(target_os = "linux")]
fn explained_by(what: String, tail: &std::sync::Mutex<Tail>) -> String {
    match tail.lock().ok().and_then(|tail| tail.reason()) {
        Some(reason) => {
            let reason = their_line(&reason, "stdin");
            tracing::warn!("openvpn: it said: {reason}");
            format!("{what}: {reason}")
        }
        None => what,
    }
}

/// The socket OpenVPN calls bliss on, in the session's own folder, which no
/// other user may enter; taken away with this.
#[cfg(target_os = "linux")]
struct Socket {
    path: PathBuf,
    listener: tokio::net::UnixListener,
}

#[cfg(target_os = "linux")]
impl Socket {
    fn bind(fallback: &std::path::Path) -> Result<Self, String> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| fallback.to_path_buf());
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("не удалось создать папку для связи с OpenVPN: {e}"))?;
        let path = dir.join(format!("bliss-openvpn-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path)
            .map_err(|e| format!("не удалось открыть сокет для связи с OpenVPN: {e}"))?;
        // Nobody but bliss's user, and root, which OpenVPN runs as.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok(Self { path, listener })
    }
}

#[cfg(target_os = "linux")]
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// OpenVPN's options on Linux: the profile from its standard input, and the
/// management interface as a unix socket it connects to.
#[cfg(target_os = "linux")]
fn linux_options(socket: &std::path::Path) -> Vec<String> {
    [
        "--config",
        "stdin",
        "--verb",
        "3",
        "--management",
        &socket.to_string_lossy(),
        "unix",
        "--management-client",
        "--management-hold",
        "--management-query-passwords",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// Wait for OpenVPN to call: the process bliss started, as root, and no
/// other. The kernel names the caller of a unix socket — its process and its
/// user, as they were when it connected — so nothing has to be looked up.
#[cfg(target_os = "linux")]
async fn answer_unix(
    listener: &tokio::net::UnixListener,
    pid: u32,
    uid: u32,
    within: Duration,
) -> Result<tokio::net::UnixStream, String> {
    let wait = async {
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|e| format!("OpenVPN не смог связаться с bliss: {e}"))?;
            match stream.peer_cred() {
                Ok(caller) if caller.pid() == Some(pid as i32) && caller.uid() == uid => {
                    return Ok(stream);
                }
                Ok(caller) => tracing::warn!(
                    "openvpn: turned away a call from process {:?} of user {}, waiting for \
                     process {pid}",
                    caller.pid(),
                    caller.uid()
                ),
                Err(e) => tracing::warn!("openvpn: turned away a call that named nobody: {e}"),
            }
        }
    };
    match tokio::time::timeout(within, wait).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "OpenVPN не вышел на связь за {} с",
            within.as_secs()
        )),
    }
}

#[cfg(target_os = "linux")]
async fn run(
    profile: String,
    directory: PathBuf,
    mut stop: oneshot::Receiver<()>,
    report: Arc<dyn Fn(Event) + Send + Sync>,
) -> Result<(), String> {
    // Told to go while waiting for the one before it: nothing to start.
    if !matches!(stop.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
        return Ok(());
    }
    let written = profile::rewrite(&profile).map_err(|e| e.to_string())?;
    let openvpn = service::linux::openvpn().ok_or_else(|| {
        "OpenVPN не установлен — поставьте пакет openvpn (например, sudo apt install openvpn)"
            .to_owned()
    })?;
    let socket = Socket::bind(&directory)?;
    let through_pkexec = !service::linux::root();
    let command = service::linux::command(&openvpn, &linux_options(&socket.path));
    let mut child = service::linux::launch(command)
        .await
        .map_err(|e| format!("не удалось запустить OpenVPN: {e}"))?;
    let pid = child
        .id()
        .ok_or_else(|| "OpenVPN завершился, едва запустившись".to_owned())?;
    tracing::info!(
        "openvpn: started{}, process {pid}",
        if through_pkexec {
            " through pkexec"
        } else {
            ""
        }
    );

    // The profile goes in on its standard input, key and all, and the input
    // closes behind it: nothing of it is written anywhere. A task of its own,
    // since a profile larger than the pipe waits on OpenVPN reading it, and
    // OpenVPN on the password.
    if let Some(mut input) = child.stdin.take() {
        tokio::spawn(async move {
            let _ = input.write_all(written.as_bytes()).await;
            let _ = input.shutdown().await;
        });
    }
    let tail = Arc::new(std::sync::Mutex::new(Tail::default()));
    if let Some(out) = child.stdout.take() {
        tokio::spawn(drain(out, Arc::clone(&tail)));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(drain(err, Arc::clone(&tail)));
    }

    let stream = tokio::select! {
        called = answer_unix(&socket.listener, pid, 0, CALL) => match called {
            Ok(stream) => stream,
            Err(e) => {
                // A pkexec still waiting on the password is bliss's user's
                // and goes; an OpenVPN running as root leaves when it finds
                // nobody on the socket.
                let _ = child.start_kill();
                return Err(explained_by(e, &tail));
            }
        },
        status = child.wait() => {
            // Gone before it called: the password refused or the dialog
            // closed, or OpenVPN would not take the profile.
            let code = status.ok().and_then(|status| status.code());
            return Err(explained_by(service::linux::exit_words(code, through_pkexec), &tail));
        }
        _ = &mut stop => {
            let _ = child.start_kill();
            return Ok(());
        }
    };
    drop(socket);

    match converse(stream, &mut stop, &report).await {
        Ok(Ending::Asked) => Ok(()),
        // Asked and still there. Running as root, it cannot be ended from
        // here: the conversation has ended, and with it the leash, which
        // OpenVPN answers by leaving.
        Ok(Ending::Overstayed) => {
            tracing::warn!("openvpn: asked to leave and still there; its leash is let go");
            Ok(())
        }
        Ok(Ending::Left) => {
            let code = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => status.code(),
                _ => None,
            };
            Err(explained_by(service::linux::exit_words(code, false), &tail))
        }
        Err(why) => Err(why),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    /// The options make the leash, as on Windows, over a unix socket, with
    /// the profile read from standard input and no log of OpenVPN's own.
    #[test]
    fn openvpn_is_told_to_read_its_input_and_call() {
        let options = linux_options(std::path::Path::new("/run/user/1000/bliss-openvpn-7.sock"));
        let joined = options.join(" ");
        for part in [
            "--config stdin",
            "--management /run/user/1000/bliss-openvpn-7.sock unix",
            "--management-client",
            "--management-hold",
            "--management-query-passwords",
            "--verb 3",
        ] {
            assert!(joined.contains(part), "{part} in {joined}");
        }
        assert!(
            !joined.contains("--log"),
            "a root OpenVPN's log would be root's"
        );
    }

    /// The call is taken from the process and user waited for and from no
    /// other: here the caller is this test, so it is taken when that is who
    /// is waited for, and turned away for another process or another user.
    #[tokio::test]
    async fn the_call_is_taken_only_from_the_process_started() {
        let dir = std::env::temp_dir().join(format!("bliss-openvpn-call-{}", std::process::id()));
        let socket = Socket::bind(&dir).expect("a socket");
        // SAFETY: getuid has no preconditions.
        let me = unsafe { libc::getuid() };
        let pid = std::process::id();

        let _caller = tokio::net::UnixStream::connect(&socket.path)
            .await
            .expect("calls");
        let answered = answer_unix(&socket.listener, pid, me, Duration::from_secs(5)).await;
        assert!(answered.is_ok(), "{:?}", answered.err());

        let _stranger = tokio::net::UnixStream::connect(&socket.path)
            .await
            .expect("calls");
        let turned = answer_unix(&socket.listener, pid + 4, me, Duration::from_millis(300)).await;
        assert!(turned.is_err(), "another process is not taken");

        let _not_root = tokio::net::UnixStream::connect(&socket.path)
            .await
            .expect("calls");
        if me != 0 {
            let turned = answer_unix(&socket.listener, pid, 0, Duration::from_millis(300)).await;
            assert!(turned.is_err(), "a caller not running as root is not taken");
        }
        let path = socket.path.clone();
        drop(socket);
        assert!(!path.exists(), "the socket goes with it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tail keeps OpenVPN's last words and no more: a reason further back
    /// than it is gone, and so is the part of a line past its length.
    #[tokio::test]
    async fn only_the_end_of_what_it_said_is_kept() {
        let tail = Arc::new(std::sync::Mutex::new(Tail::default()));
        let noise = "2026-10-09 17:00:00 MANAGEMENT: >STATE:1759999999,WAIT,,,,,,\n".repeat(4096);
        let said = format!(
            "Options error: something early\n{noise}{}\nOptions error: something late\n",
            "x".repeat(10 * LINE)
        );
        drain(said.as_bytes(), Arc::clone(&tail)).await;
        let tail = tail.lock().expect("the tail");
        assert!(tail.bytes <= TAIL, "{} kept", tail.bytes);
        assert!(tail.lines.iter().all(|line| line.len() <= LINE));
        assert_eq!(
            tail.reason().as_deref(),
            Some("Options error: something late")
        );
        assert!(
            !tail
                .lines
                .iter()
                .any(|line| line.contains("something early")),
            "older than the tail"
        );
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
async fn run(
    _profile: String,
    _directory: PathBuf,
    _stop: oneshot::Receiver<()>,
    _report: Arc<dyn Fn(Event) + Send + Sync>,
) -> Result<(), String> {
    Err("рабочий VPN на этой системе не поддерживается".to_owned())
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::path::Path;

    #[cfg(not(windows))]
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    /// The options are the ones that make the leash: OpenVPN calls bliss,
    /// waits to be released, and asks bliss for any password.
    #[cfg(windows)]
    #[test]
    fn openvpn_is_told_to_call_and_wait() {
        let options = options(
            Path::new(r"C:\Users\A B\profile.ovpn"),
            Path::new(r"C:\Users\A B\openvpn.log"),
            50123,
        );
        for part in [
            r#"--config "C:\Users\A B\profile.ovpn""#,
            r#"--log "C:\Users\A B\openvpn.log""#,
            "--management 127.0.0.1 50123",
            "--management-client",
            "--management-hold",
            "--management-query-passwords",
            "--verb 3",
        ] {
            assert!(options.contains(part), "{part} in {options}");
        }
        assert!(
            options.starts_with("--log "),
            "the log is opened before the profile is read: {options}"
        );
    }

    /// The call is taken from the process the service started and from no
    /// other: here the caller is this test, so it is taken when that is the
    /// process waited for, and turned away when another is.
    #[cfg(windows)]
    #[tokio::test]
    async fn the_call_is_taken_only_from_the_process_the_service_started() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a port");
        let at = listener.local_addr().expect("an address");

        let caller = TcpStream::connect(at).await.expect("calls");
        let answered = answer(&listener, std::process::id(), Duration::from_secs(5)).await;
        let answered = answered.expect("the call is taken");
        assert_eq!(
            answered.peer_addr().expect("its peer"),
            caller.local_addr().expect("the caller's address")
        );

        let _stranger = TcpStream::connect(at).await.expect("calls");
        let turned = answer(
            &listener,
            std::process::id() + 4,
            Duration::from_millis(500),
        )
        .await;
        assert!(turned.is_err(), "a call from another process is not taken");
    }

    /// OpenVPN's last words, as `--log` writes them: the reason it stopped,
    /// without the time before it.
    #[test]
    fn the_log_says_why() {
        let options = "2026-10-04 02:17:19 WARNING: Using --management on a TCP port WITHOUT passwords\n\
                       Options error: On Windows, --ifconfig is required when --dev tun is used\n\
                       Use --help for more information.\n";
        assert_eq!(
            reason(options.lines()).as_deref(),
            Some("Options error: On Windows, --ifconfig is required when --dev tun is used")
        );
        let fatal = "2026-10-04 02:18:01 MANAGEMENT: Connected to management server at [AF_INET]127.0.0.1:50123\n\
                     2026-10-04 02:18:01 All ovpn-dco adapters on this system are currently in use or disabled.\n\
                     2026-10-04 02:18:01 Exiting due to fatal error\n";
        assert_eq!(
            reason(fatal.lines()).as_deref(),
            Some("All ovpn-dco adapters on this system are currently in use or disabled.")
        );
        let call = "2026-10-04 02:18:01 MANAGEMENT: connect to [AF_INET]127.0.0.1:50123 failed: Connection refused (WSAECONNREFUSED) (errno=10061)\n\
                    2026-10-04 02:18:01 SIGTERM[soft,management-connect-failed] received, process exiting\n";
        assert!(
            reason(call.lines()).is_some_and(|why| why.starts_with("MANAGEMENT: connect to")),
            "{:?}",
            reason(call.lines())
        );
        let fine = "2026-10-04 02:18:01 Initialization Sequence Completed\n\
                    2026-10-04 02:19:30 SIGTERM[hard,] received, process exiting\n";
        assert_eq!(reason(fine.lines()), None);
    }

    /// Only the end of the log is read, however long it grew: a reason
    /// further back than that is not found.
    #[cfg(windows)]
    #[test]
    fn only_the_end_of_the_log_is_read() {
        let dir = std::env::temp_dir().join(format!("bliss-openvpn-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let path = dir.join("openvpn.log");
        let noise = "2026-10-04 02:18:01 MANAGEMENT: >STATE:1759533481,WAIT,,,,,,\n".repeat(4096);
        let early = format!("Options error: something early\n{noise}");
        std::fs::write(&path, &early).expect("written");
        assert!(
            early.len() as u64 > 2 * LOG_TAIL,
            "the log is longer than its tail"
        );
        assert_eq!(reason_in_log(&path), None, "a reason older than the tail");
        std::fs::write(&path, format!("{noise}Options error: something late\n")).expect("written");
        assert_eq!(
            reason_in_log(&path).as_deref(),
            Some("Options error: something late")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A line OpenVPN names is named by its place in the person's own file.
    #[test]
    fn a_line_is_named_as_it_is_in_their_file() {
        let written = r"C:\Users\A B\bliss\openvpn\profile.ovpn";
        let line = profile::PREAMBLE + 74;
        let reason = format!(
            r"Options error: Unrecognized option or missing or extra parameter(s) in C:\Users\A B\bliss\openvpn\profile.ovpn:{line}: frobnicate (2.7.7)"
        );
        assert_eq!(
            their_line(&reason, written),
            "Options error: Unrecognized option or missing or extra parameter(s) in line 74: \
             frobnicate (2.7.7)"
        );
        assert_eq!(
            their_line("Cannot open TUN/TAP dev", written),
            "Cannot open TUN/TAP dev"
        );
        // Linux's OpenVPN read it from its standard input.
        let reason = format!(
            "Options error: Unrecognized option or missing or extra parameter(s) in stdin:{line}: \
             frobnicate (2.6.19)"
        );
        assert_eq!(
            their_line(&reason, "stdin"),
            "Options error: Unrecognized option or missing or extra parameter(s) in line 74: \
             frobnicate (2.6.19)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_services_word_ends_a_sentence() {
        assert_eq!(
            exit_note("0x20000000\nOpenVPN exited with error: exit code = 1\n"),
            " с ошибкой (exit code = 1)"
        );
        assert_eq!(exit_note("0x20000000\n"), " с ошибкой");
    }

    /// A conversation with a stand-in for OpenVPN: the commands bliss sends,
    /// the DNS servers it keeps from the push line, and what it reports.
    #[tokio::test]
    async fn a_conversation_reports_up_with_its_dns() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a port");
        let at = listener.local_addr().expect("an address");
        let openvpn = tokio::spawn(async move {
            let mut stream = TcpStream::connect(at).await.expect("connects");
            let mut said = vec![0u8; 256];
            let mut heard = String::new();
            // What bliss says first, all of it.
            while !heard.contains("hold release") {
                let n = tokio::io::AsyncReadExt::read(&mut stream, &mut said)
                    .await
                    .expect("reads");
                heard.push_str(&String::from_utf8_lossy(&said[..n]));
            }
            stream
                .write_all(
                    b">INFO:OpenVPN Management Interface Version 5\r\n\
                      >STATE:1,WAIT,,,,,,\r\n\
                      >LOG:1,,PUSH: Received control message: 'PUSH_REPLY,dhcp-option DNS 10.8.0.1,ping 10'\r\n\
                      >STATE:1,CONNECTED,SUCCESS,127.0.0.2,203.0.113.9,1194,,,\r\n",
                )
                .await
                .expect("writes");
            heard
        });

        let (stream, _) = listener.accept().await.expect("accepts");
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let report: Arc<dyn Fn(Event) + Send + Sync> =
            Arc::new(move |event| seen.lock().expect("events").push(event));
        let (_keep, mut stop) = oneshot::channel();
        let ended = converse(stream, &mut stop, &report).await;

        let heard = openvpn.await.expect("the stand-in ran");
        assert!(heard.starts_with("state on\r\nlog on all\r\nhold off\r\nhold release"));
        assert_eq!(ended, Ok(Ending::Left), "it left without being asked");
        assert_eq!(
            *events.lock().expect("events"),
            [
                Event::Step("WAIT".to_owned()),
                Event::Up {
                    local: Ipv4Addr::new(127, 0, 0, 2),
                    dns: vec![SocketAddr::from(([10, 8, 0, 1], 53))],
                },
            ]
        );
    }

    /// Gone by itself after an error in its log: that error is why, not the
    /// exit code the service would report.
    #[tokio::test]
    async fn an_error_in_the_log_is_the_reason() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a port");
        let at = listener.local_addr().expect("an address");
        let openvpn = tokio::spawn(async move {
            let mut stream = TcpStream::connect(at).await.expect("connects");
            let mut said = vec![0u8; 256];
            let mut heard = String::new();
            while !heard.contains("hold release") {
                let n = tokio::io::AsyncReadExt::read(&mut stream, &mut said)
                    .await
                    .expect("reads");
                heard.push_str(&String::from_utf8_lossy(&said[..n]));
            }
            stream
                .write_all(
                    b">LOG:1,W,WARNING: something harmless\r\n\
                      >LOG:1,N,AUTH: Received control message: AUTH_FAILED\r\n\
                      >STATE:1,EXITING,auth-failure,,,,,\r\n",
                )
                .await
                .expect("writes");
        });
        let (stream, _) = listener.accept().await.expect("accepts");
        let report: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(|_| {});
        let (_keep, mut stop) = oneshot::channel();
        let ended = converse(stream, &mut stop, &report).await;
        openvpn.await.expect("the stand-in ran");
        assert_eq!(
            ended,
            Err("AUTH: Received control message: AUTH_FAILED".to_owned())
        );
    }

    /// An address on this machine is usable at once; one that is not never
    /// becomes so, and the wait for it ends.
    #[tokio::test]
    async fn the_address_is_waited_for_until_it_takes_a_socket() {
        assert!(usable(Ipv4Addr::LOCALHOST, Duration::from_millis(300)).await);
        let started = tokio::time::Instant::now();
        assert!(!usable(Ipv4Addr::new(203, 0, 113, 77), Duration::from_millis(300)).await);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the wait has an end"
        );
    }

    /// The hold OpenVPN starts in is released once, by the first commands;
    /// a hold after that is released when it comes.
    #[tokio::test]
    async fn the_hold_it_starts_in_is_released_once() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a port");
        let at = listener.local_addr().expect("an address");
        let openvpn = tokio::spawn(async move {
            let mut stream = TcpStream::connect(at).await.expect("connects");
            stream
                .write_all(b">INFO:OpenVPN Management Interface Version 6\r\n>HOLD:Waiting for hold release:0\r\n")
                .await
                .expect("writes");
            let mut buf = vec![0u8; 256];
            let mut heard = String::new();
            let mut more = async |heard: &mut String, stream: &mut TcpStream| {
                while let Ok(Ok(n)) = tokio::time::timeout(
                    Duration::from_millis(300),
                    tokio::io::AsyncReadExt::read(stream, &mut buf),
                )
                .await
                {
                    if n == 0 {
                        break;
                    }
                    heard.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            };
            more(&mut heard, &mut stream).await;
            let first = heard.matches("hold release").count();
            stream
                .write_all(b">HOLD:Waiting for hold release:10\r\n")
                .await
                .expect("writes");
            more(&mut heard, &mut stream).await;
            (first, heard.matches("hold release").count())
        });
        let (stream, _) = listener.accept().await.expect("accepts");
        let report: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(|_| {});
        let (_keep, mut stop) = oneshot::channel();
        let _ = converse(stream, &mut stop, &report).await;
        assert_eq!(openvpn.await.expect("the stand-in ran"), (1, 2));
    }

    /// Asked to stop, bliss tells OpenVPN so and waits for it to go.
    #[tokio::test]
    async fn a_stop_is_passed_on() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a port");
        let at = listener.local_addr().expect("an address");
        let openvpn = tokio::spawn(async move {
            let stream = TcpStream::connect(at).await.expect("connects");
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line == "signal SIGTERM" {
                    return true;
                }
            }
            false
        });
        let (stream, _) = listener.accept().await.expect("accepts");
        let report: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(|_| {});
        let (stop_now, mut stop) = oneshot::channel();
        stop_now.send(()).expect("sent");
        let ended = converse(stream, &mut stop, &report).await;
        assert_eq!(ended, Ok(Ending::Asked));
        assert!(openvpn.await.expect("the stand-in ran"), "SIGTERM was sent");
    }
}
