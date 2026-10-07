//! One OpenVPN connection, from the profile written to disk to OpenVPN gone.
//!
//! bliss listens on a loopback port and OpenVPN, started by its service,
//! connects to it (`--management-client`). The connection is the leash:
//! OpenVPN leaves when it closes (`management-exit`), so a bliss that dies
//! takes its VPN with it rather than leaving it up with nobody to switch it
//! off. It starts held (`--management-hold`) so nothing is missed before bliss
//! is listening to it, and `hold off` keeps a reconnect from waiting for bliss.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::management::{self, Line};
use crate::profile;
use crate::service;

/// How long OpenVPN has to call bliss once the service has started it.
const CALL: Duration = Duration::from_secs(20);

/// How long OpenVPN has to leave once asked to.
const LEAVE: Duration = Duration::from_secs(8);

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
struct Written(PathBuf);

impl Drop for Written {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

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
    let _written = Written(path.clone());

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| format!("не удалось занять порт для связи с OpenVPN: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let options = options(&path, port);

    let dir = directory.clone();
    let started = tokio::task::spawn_blocking(move || service::start(&dir, &options))
        .await
        .map_err(|e| format!("поток службы OpenVPN не дожил до конца: {e}"))?
        .map_err(|e| e.to_string())?;
    let pid = started.pid;
    tracing::info!("openvpn: started by its service, process {pid}");
    // Waits on the service for as long as OpenVPN runs; it speaks only when
    // OpenVPN exits with an error.
    let last_word = tokio::task::spawn_blocking(move || started.pipe.last_word());

    let stream = tokio::select! {
        called = answer(&listener) => match called {
            Ok(stream) => stream,
            Err(e) => {
                service::terminate(pid);
                return Err(e);
            }
        },
        _ = &mut stop => {
            service::terminate(pid);
            return Ok(());
        }
    };
    drop(listener);

    match converse(stream, &mut stop, &report).await {
        Ok(Ending::Asked) => Ok(()),
        Ok(Ending::Overstayed) => {
            // Asked and still there: made to. The service takes the routes
            // away as the process ends.
            service::terminate(pid);
            Ok(())
        }
        // Gone by itself: what the service saw is the best account of why.
        Ok(Ending::Left) => match tokio::time::timeout(Duration::from_secs(2), last_word).await {
            Ok(Ok(Some(said))) => Err(exit_note(&said)),
            _ => Err("OpenVPN завершился".to_owned()),
        },
        Err(why) => Err(why),
    }
}

/// The service's word on an OpenVPN that exited with an error: its second
/// line, `OpenVPN exited with error: exit code = 1`.
fn exit_note(said: &str) -> String {
    match said.split('\n').nth(1).map(str::trim) {
        Some(line) if !line.is_empty() => format!("OpenVPN завершился с ошибкой: {line}"),
        _ => "OpenVPN завершился с ошибкой".to_owned(),
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

/// OpenVPN's options: everything else is in the profile.
fn options(profile: &Path, port: u16) -> String {
    format!(
        "--config {} --verb 3 --management 127.0.0.1 {port} --management-client \
         --management-hold --management-query-passwords",
        service::quoted(profile)
    )
}

/// Wait for OpenVPN to call. A caller that is not `openvpn.exe` is turned
/// away: the port is on loopback, where any program can knock.
async fn answer(listener: &TcpListener) -> Result<TcpStream, String> {
    let wait = async {
        loop {
            let (stream, peer) = listener
                .accept()
                .await
                .map_err(|e| format!("OpenVPN не смог связаться с bliss: {e}"))?;
            let caller = bliss_core::name_for_port(peer.port());
            if caller
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("openvpn.exe"))
            {
                return Ok(stream);
            }
            tracing::warn!(
                "openvpn: turned away a call from {}",
                caller.as_deref().unwrap_or("an unknown program")
            );
        }
    };
    match tokio::time::timeout(CALL, wait).await {
        Ok(result) => result,
        Err(_) => Err(format!("OpenVPN не вышел на связь за {} с", CALL.as_secs())),
    }
}

/// Talk to OpenVPN until it leaves.
async fn converse(
    stream: TcpStream,
    stop: &mut oneshot::Receiver<()>,
    report: &Arc<dyn Fn(Event) + Send + Sync>,
) -> Result<Ending, String> {
    let (read, mut write) = stream.into_split();
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
                } else if management::worth_saying(&flags) {
                    if flags.contains(['F', 'N']) {
                        last_error = Some(text.clone());
                    }
                    report(Event::Said(text));
                }
            }
            Line::Hold => {
                let _ = write.write_all(b"hold release\r\n").await;
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

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The options are the ones that make the leash: OpenVPN calls bliss,
    /// waits to be released, and asks bliss for any password.
    #[test]
    fn openvpn_is_told_to_call_and_wait() {
        let options = options(Path::new(r"C:\Users\A B\profile.ovpn"), 50123);
        for part in [
            r#"--config "C:\Users\A B\profile.ovpn""#,
            "--management 127.0.0.1 50123",
            "--management-client",
            "--management-hold",
            "--management-query-passwords",
            "--verb 3",
        ] {
            assert!(options.contains(part), "{part} in {options}");
        }
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
                      >STATE:1,CONNECTED,SUCCESS,10.8.0.2,203.0.113.9,1194,,,\r\n",
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
                    local: Ipv4Addr::new(10, 8, 0, 2),
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
