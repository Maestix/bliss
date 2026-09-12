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

use crate::job::Job;

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
    /// an older xray accepts the config, then simply never connects.
    #[error(
        "xray {found} is too old for this server — it needs {needed}.x or newer \
         for VLESS Encryption (encryption=mlkem768x25519plus…)"
    )]
    TooOld { found: String, needed: u32 },

    #[error("xray exited immediately ({status}) — see the log above for its reason")]
    ExitedEarly { status: std::process::ExitStatus },

    #[error("xray did not start listening on 127.0.0.1:{port} within {secs}s")]
    NotListening { port: u16, secs: u64 },
}

/// A running xray-core.
///
/// Its lifetime is tied to ours twice over: `kill_on_drop` for the ordinary
/// paths, and a Job Object for the ones where `Drop` never runs. See `job.rs` —
/// the second one is not belt-and-braces, it was a real leak.
pub struct Xray {
    child: Child,
    port: u16,
    config_path: PathBuf,
    /// Held, not used: the job dies with this field, and its members with it.
    _job: Option<Job>,
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

        let job = match Job::kill_on_close() {
            Ok(job) => Some(job),
            Err(e) => {
                tracing::warn!("no job object ({e}) — a force-killed bliss may leak the engine");
                None
            }
        };

        let mut child = command.spawn().map_err(XrayError::Spawn)?;

        // Assigned immediately after spawn. There is a hair of a window before
        // this in which the child could spawn its own children outside the job;
        // closing it needs CREATE_SUSPENDED, which tokio does not expose, and
        // xray spawns nothing, so the window is not worth the machinery.
        #[cfg(windows)]
        if let (Some(job), Some(handle)) = (job.as_ref(), child.raw_handle()) {
            // SAFETY: the handle comes from the child we just created, which
            // carries the rights AssignProcessToJobObject needs.
            if let Err(e) = unsafe { job.adopt(handle as isize) } {
                tracing::warn!("could not put the engine in the job object: {e}");
            }
        }

        // xray writes everything to stdout; stderr is drained too so a full pipe
        // can never block the child.
        if let Some(out) = child.stdout.take() {
            forward(out, "xray");
        }
        if let Some(err) = child.stderr.take() {
            forward(err, "xray!");
        }

        let xray = Self {
            child,
            port,
            config_path,
            _job: job,
        };
        xray.await_listening(Duration::from_secs(10)).await?;
        Ok(xray)
    }

    /// Poll the port rather than sleeping: a connection that is accepted is
    /// proof the inbound is up, and a child that died is noticed at once
    /// instead of after the timeout.
    async fn await_listening(&self, timeout: Duration) -> Result<(), XrayError> {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port);
        let deadline = tokio::time::Instant::now() + timeout;

        while tokio::time::Instant::now() < deadline {
            if TcpStream::connect(addr).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(XrayError::NotListening {
            port: self.port,
            secs: timeout.as_secs(),
        })
    }

    /// Where bliss dials to reach this xray.
    pub fn socks_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port)
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Resolves if xray exits on its own, which always means something is wrong.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus, std::io::Error> {
        self.child.wait().await
    }

    pub async fn shutdown(mut self) -> Result<(), std::io::Error> {
        self.child.start_kill()?;
        self.child.wait().await.map(|_| ())
    }
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
pub async fn check_version(exe: &Path, needs_major: Option<u32>) -> Result<String, XrayError> {
    if !exe.is_file() {
        return Err(XrayError::NotFound(exe.to_path_buf()));
    }

    let mut command = Command::new(exe);
    command.arg("version");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);

    let out = command
        .output()
        .await
        .map_err(|e| XrayError::Version(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or_default();

    // "Xray 26.7.28 (Xray, Penetrates Everything.) …"
    let version = first
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| XrayError::Version(format!("unexpected output: {first:?}")))?
        .to_owned();

    if let Some(needs) = needs_major {
        let major: u32 = version
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .ok_or_else(|| XrayError::Version(format!("unparsable version {version:?}")))?;
        if major < needs {
            return Err(XrayError::TooOld {
                found: version,
                needed: needs,
            });
        }
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
