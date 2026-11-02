//! Running xray-core as a child process.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use crate::config::Version;
use crate::job;

#[derive(Debug, Error)]
pub enum XrayError {
    #[error("no xray executable at {0}")]
    NotFound(PathBuf),

    #[error("could not write the xray config to {path}")]
    WriteConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not start xray")]
    Spawn(#[source] std::io::Error),

    #[error("could not read the xray version: {0}")]
    Version(String),

    /// The failure this check exists to prevent looks like a broken server:
    /// an older xray accepts the config, then simply never connects — or
    /// refuses a field it has never heard of, in words about the field.
    #[error("xray {found} is too old for this server — it needs {needed} or newer")]
    TooOld { found: Version, needed: Version },

    #[error("xray exited immediately ({status}) — see the log above for its reason")]
    ExitedEarly { status: std::process::ExitStatus },

    /// Checked before spawning rather than discovered afterwards. If something
    /// already holds the bridge port, xray cannot bind it and exits — and the
    /// readiness probe would then connect to the squatter and call it success.
    #[error(
        "127.0.0.1:{port} is already taken — another client, or an engine left \
         behind by a previous bliss. Close it, or set engine.bridge_port in the profile."
    )]
    BridgePortTaken { port: u16 },

    #[error("xray did not start listening on 127.0.0.1:{port} within {secs}s")]
    NotListening { port: u16, secs: u64 },
}

/// A running xray-core.
///
/// Its lifetime is tied to ours twice over: `kill_on_drop` for the ordinary
/// paths, and the kernel's leash for the ones where `Drop` never runs — a Job
/// Object on Windows, the parent-death signal on Linux. See `job.rs`: the
/// second one is not belt-and-braces, it was a real leak.
pub struct Xray {
    child: Child,
    port: u16,
    config_path: PathBuf,
    /// Held, not used: on Windows the job dies with this field, and its
    /// members with it.
    _leash: job::Held,
}

impl Xray {
    /// Write the config, start xray, and wait until it is actually listening.
    pub async fn spawn(
        exe: &Path,
        config: &Value,
        work_dir: &Path,
        port: u16,
    ) -> Result<Self, XrayError> {
        if !exe.is_file() {
            return Err(XrayError::NotFound(exe.to_path_buf()));
        }

        // Before anything else: is the port we are about to hand xray actually
        // free? If it is not, xray fails to bind and exits, and every later
        // check in this function would pass against whatever is squatting there.
        claim_bridge_port(port)?;

        let config_path = work_dir.join("xray.json");
        std::fs::create_dir_all(work_dir)
            .and_then(|()| {
                std::fs::write(
                    &config_path,
                    serde_json::to_vec_pretty(config).expect("config is plain json"),
                )
            })
            .map_err(|source| XrayError::WriteConfig {
                path: config_path.clone(),
                source,
            })?;

        let mut command = Command::new(exe);
        command
            .arg("run")
            .arg("-c")
            .arg(&config_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true);

        // CREATE_NO_WINDOW: without it every start flashes a console window,
        // which is unacceptable from a GUI and merely ugly from the CLI.
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);

        let (mut child, leash) = job::spawn(command).await.map_err(XrayError::Spawn)?;

        // xray writes everything to stdout; stderr is drained too so a full pipe
        // can never block the child.
        if let Some(out) = child.stdout.take() {
            forward(out, "xray");
        }
        if let Some(err) = child.stderr.take() {
            forward(err, "xray!");
        }

        let mut xray = Self {
            child,
            port,
            config_path,
            _leash: leash,
        };
        // The method the config's port asks for: a login (`require_login`)
        // or none.
        let method = match config.pointer("/inbounds/0/settings/auth") {
            Some(Value::String(auth)) if auth == "password" => 2,
            _ => 0,
        };
        xray.await_listening(Duration::from_secs(10), method)
            .await?;
        Ok(xray)
    }

    /// Wait until the engine is genuinely serving, or say why it is not.
    ///
    /// Three things have to hold, and the earlier version checked only the
    /// weakest of them. Being able to open a TCP connection to the bridge port
    /// says nothing about *whose* port it is: if xray failed to bind and exited,
    /// the connection lands in whatever else is listening and the caller is told
    /// the engine is up. So:
    ///
    /// 1. the port was free before the spawn (`claim_bridge_port`, above);
    /// 2. the child is still alive — polled every round, which is what makes
    ///    `ExitedEarly` reachable rather than decorative;
    /// 3. whatever answers speaks SOCKS5 and takes the method the config
    ///    asked for — no-auth, or a login — which is the cheapest proof that
    ///    it is the inbound we asked for and not a stray program that merely
    ///    accepts connections.
    async fn await_listening(&mut self, timeout: Duration, method: u8) -> Result<(), XrayError> {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port);
        let deadline = tokio::time::Instant::now() + timeout;

        while tokio::time::Instant::now() < deadline {
            // A child that died has nothing to say about the port, and waiting
            // the rest of the timeout for it would hide the reason. Its own
            // output is already in the log, with the real explanation in it.
            if let Some(status) = self.exited() {
                return Err(XrayError::ExitedEarly { status });
            }
            if speaks_socks5(addr, method).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(XrayError::NotListening {
            port: self.port,
            secs: timeout.as_secs(),
        })
    }

    /// Has the engine exited? Non-blocking, so a caller with a heartbeat can ask
    /// on every beat.
    ///
    /// Nothing used to ask at all: `wait` existed and no code path called it, so
    /// an engine that died mid-session left the window showing a live tunnel
    /// while every connection failed at the bridge.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Where bliss dials to reach this xray.
    pub fn socks_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port)
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    // There was a `wait()` here that resolved when xray exited on its own. It
    // was never called from anywhere, so the supervision it implied did not
    // exist. `exited()` replaces it because the engine loop already has a
    // heartbeat and only needs to ask, not to await.

    pub async fn shutdown(mut self) -> Result<(), std::io::Error> {
        self.child.start_kill()?;
        self.child.wait().await.map(|_| ())
    }
}

/// Check that the bridge port is free, by taking it and letting it go.
///
/// The listener is dropped immediately, which leaves a window before xray binds
/// it. That window is not closable — only the child can hold its own socket —
/// but it turns the common failures (a previous engine still running, another
/// client on the same port, a second bliss) from "silently proxies through
/// someone else" into a message naming the port.
fn claim_bridge_port(port: u16) -> Result<(), XrayError> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    match std::net::TcpListener::bind(addr) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            Err(XrayError::BridgePortTaken { port })
        }
        // Anything else — a permission problem, an exhausted stack — is not
        // ours to interpret, and xray will report it better than a guess would.
        Err(_) => Ok(()),
    }
}

/// Does whatever is listening there answer a SOCKS5 greeting?
///
/// Sends the same three bytes the dialer does and expects `05` and that method
/// back: `05 00` for no-auth, `05 02` for a login. A
/// program that is not a SOCKS5 proxy will not, which is the difference between
/// "the port is occupied" and "the engine is up".
async fn speaks_socks5(addr: SocketAddr, method: u8) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let handshake = async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        // version 5, one method offered: 0 = no authentication, 2 = a login.
        stream.write_all(&[5, 1, method]).await.ok()?;
        let mut answer = [0u8; 2];
        stream.read_exact(&mut answer).await.ok()?;
        Some(answer == [5, method])
    };

    // A squatter that accepts and then says nothing must not hold the whole
    // readiness budget on one attempt.
    tokio::time::timeout(Duration::from_secs(1), handshake)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

fn forward<R>(reader: R, target: &'static str)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            // Prefixed, not merged: in the log pane it must be obvious which
            // lines are ours and which came from the engine. The engine's own
            // date and level are dropped — ours are already on every line, and
            // the pane is only so wide. Re-raising the level matters: a REALITY
            // rejection arrives as a Warning, and it should look like one.
            let (level, text) = split_engine_line(line);
            match level {
                "Error" => tracing::error!("{target}: {text}"),
                "Warning" => tracing::warn!("{target}: {text}"),
                "Debug" => tracing::debug!("{target}: {text}"),
                _ => tracing::info!("{target}: {text}"),
            }
        }
    });
}

/// Ask the binary what it is, and refuse early if it cannot do the job.
pub async fn check_version(exe: &Path, needs: Option<Version>) -> Result<Version, XrayError> {
    check_version_within(exe, needs, VERSION_WAIT).await
}

/// How long `xray version` has to answer. It answers at once; the first run
/// of a newly downloaded one can take seconds while an antivirus looks it
/// over. Without a bound, an engine that never answered held the engine loop
/// — and with it every click in the window — for good.
const VERSION_WAIT: Duration = Duration::from_secs(20);

async fn check_version_within(
    exe: &Path,
    needs: Option<Version>,
    wait: Duration,
) -> Result<Version, XrayError> {
    if !exe.is_file() {
        return Err(XrayError::NotFound(exe.to_path_buf()));
    }

    let mut command = Command::new(exe);
    command.arg("version");
    // A child left to run after the wait gave up on it is a process nobody owns.
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);

    let out = tokio::time::timeout(wait, command.output())
        .await
        .map_err(|_| {
            XrayError::Version(format!(
                "{} did not answer `version` within {} s",
                exe.display(),
                wait.as_secs()
            ))
        })?
        .map_err(|e| XrayError::Version(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or_default();

    // "Xray 26.7.28 (Xray, Penetrates Everything.) …"
    let word = first
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| XrayError::Version(format!("unexpected output: {first:?}")))?;
    // Parsed whether or not anything is needed: the config itself is written
    // for the version (mKCP's masks changed names in 26.6).
    let version = Version::parse(word)
        .ok_or_else(|| XrayError::Version(format!("unparsable version {word:?}")))?;

    if let Some(needed) = needs
        && version < needed
    {
        return Err(XrayError::TooOld {
            found: version,
            needed,
        });
    }

    Ok(version)
}

/// Split `2026/09/12 00:42:33.004901 [Warning] core: ...` into its level and the
/// message. Anything that does not match is passed through untouched — a line
/// we fail to recognise is still a line worth showing.
fn split_engine_line(line: &str) -> (&str, &str) {
    let rest = match line.split_once(' ') {
        // A date, not a word: digits and slashes only.
        Some((head, rest))
            if head.len() == 10 && head.bytes().all(|b| b.is_ascii_digit() || b == b'/') =>
        {
            rest.split_once(' ')
                .map(|(_time, rest)| rest)
                .unwrap_or(rest)
        }
        _ => line,
    };
    match rest.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((level, text)) => (level, text),
        None => ("Info", rest),
    }
}

#[cfg(test)]
mod forward_tests {
    use super::split_engine_line;

    #[test]
    fn strips_the_engines_own_stamp_and_level() {
        let (level, text) =
            split_engine_line("2026/09/12 00:42:33.004901 [Warning] core: Xray 26.3.27 started");
        assert_eq!(level, "Warning");
        assert_eq!(text, "core: Xray 26.3.27 started");
    }

    #[test]
    fn passes_through_anything_it_does_not_recognise() {
        let (level, text) = split_engine_line("plain line with no stamp");
        assert_eq!((level, text), ("Info", "plain line with no stamp"));
    }
}

#[cfg(test)]
mod readiness_tests {
    use std::net::{Ipv4Addr, SocketAddr, TcpListener};

    use super::*;

    fn free_port() -> u16 {
        // Port 0 asks the OS for one nobody is using; the listener is dropped
        // so the test gets a port that is free right now.
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("bind a loopback port")
            .local_addr()
            .expect("the listener has an address")
            .port()
    }

    /// The check that makes the readiness probe mean something. Without it a
    /// squatter on the bridge port answered for the engine that failed to bind.
    #[test]
    fn an_occupied_bridge_port_is_refused_before_the_engine_is_started() {
        let port = free_port();
        assert!(claim_bridge_port(port).is_ok(), "nothing is holding it");

        let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("take the port");
        assert!(matches!(
            claim_bridge_port(port),
            Err(XrayError::BridgePortTaken { .. })
        ));

        // And it is available again once the other program lets go, rather than
        // being remembered as taken.
        drop(squatter);
        assert!(claim_bridge_port(port).is_ok());
    }

    /// An engine that never answers `version` is given up on, and ended
    /// rather than left running with nobody to end it.
    #[cfg(windows)]
    #[tokio::test]
    async fn an_engine_that_does_not_answer_is_given_up_on() {
        fn waiting() -> usize {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", "IMAGENAME eq waitfor.exe", "/NH"])
                .output()
                .expect("tasklist runs");
            String::from_utf8_lossy(&out.stdout)
                .matches("waitfor.exe")
                .count()
        }
        // `waitfor version` waits for a signal called "version", for ever.
        let hangs = Path::new(r"C:\Windows\System32\waitfor.exe");
        let before = waiting();
        let started = std::time::Instant::now();
        let said = check_version_within(hangs, None, Duration::from_millis(500)).await;
        assert!(matches!(said, Err(XrayError::Version(_))), "{said:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "it waited on");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            waiting() <= before,
            "the one that did not answer is still running"
        );
    }

    /// "Something accepted the connection" is not "the engine is up". A SOCKS5
    /// greeting is the cheapest thing that tells them apart.
    #[tokio::test]
    async fn readiness_needs_a_socks5_answer_not_just_an_open_port() {
        async fn listening_with(answer: &'static [u8]) -> SocketAddr {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("address");
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut greeting = [0u8; 3];
                    if stream.read_exact(&mut greeting).await.is_ok() && !answer.is_empty() {
                        let _ = stream.write_all(answer).await;
                    }
                    // Held open deliberately: a peer that accepts and then says
                    // nothing is the case the timeout exists for.
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            });
            addr
        }

        assert!(speaks_socks5(listening_with(&[5, 0]).await, 0).await);
        // An engine that asks for the login it was given.
        assert!(speaks_socks5(listening_with(&[5, 2]).await, 2).await);
        // Accepts, answers something else — an HTTP server, say.
        assert!(!speaks_socks5(listening_with(b"HT").await, 0).await);
        // A SOCKS5 proxy that refuses every method is not one we can use.
        assert!(!speaks_socks5(listening_with(&[5, 0xFF]).await, 0).await);
        assert!(!speaks_socks5(listening_with(&[5, 0xFF]).await, 2).await);
        // Accepts and stays silent; must fail on the timeout rather than hang.
        assert!(!speaks_socks5(listening_with(&[]).await, 0).await);
    }
}
