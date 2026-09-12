//! Starting and stopping the tunnel, and publishing what it is doing.
//!
//! Both the CLI and the window drive this. The window never touches tokio: the
//! engine lives on its own runtime thread and pushes a [`Frame`] into a watch
//! channel a few times a second, which the UI awaits. A watch notification
//! needs no timer and no reactor, so the UI works whatever executor renders it
//! — which also means swapping WebView2 for Blitz changes nothing here.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bliss_core::{EngineInfo, Inbound, SocksOutbound, Tracker};
use bliss_proto::{Flow, Profile, Security, ServerConfig};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// How often the UI gets a new frame. Fast enough that a speed readout looks
/// live, slow enough that it costs nothing.
const TICK: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Stopped,
    Starting,
    Running,
    Failed,
}

impl State {
    /// The state as a word. The main screen says it in colour instead, but the
    /// tray tooltip has no room for a ring.
    #[allow(dead_code, reason = "the tray menu lands in 1.8")]
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Отключено",
            Self::Starting => "Подключение",
            Self::Running => "Подключено",
            Self::Failed => "Ошибка",
        }
    }

    pub fn is_on(self) -> bool {
        matches!(self, Self::Running | Self::Starting)
    }
}

/// Everything the window shows, recomputed each tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub state: State,
    pub engine: String,
    pub error: Option<String>,
    pub server_tag: String,
    pub server_addr: String,
    pub sni: String,
    pub flow: String,
    pub listen: String,
    pub uptime: String,
    pub up_total: u64,
    pub down_total: u64,
    pub opened: u64,
    /// Connections open right now.
    pub active: u64,
    /// Round trip to open a connection through the tunnel, in milliseconds.
    ///
    /// Not a ping to the server: it is the time to negotiate an outbound and
    /// have the far end answer, which is what a page load actually waits for.
    /// `None` until the first probe lands.
    pub latency_ms: Option<u64>,
    /// Which browser the ClientHello is dressed as, straight from the link.
    pub fingerprint: String,
    /// How many servers the profile holds, for the nav counter.
    pub servers: usize,
    /// Bytes per second over the last tick.
    pub up_rate: u64,
    pub down_rate: u64,
    /// Newest first, live connections before closed ones.
    pub connections: Vec<bliss_core::Snapshot>,
    /// Recent download rates, oldest first, for the bar chart.
    pub history: Vec<u64>,
    /// What the chart's full height means, in bytes per second. Computed here
    /// rather than in the UI so it can be smoothed across ticks.
    pub scale: u64,
}

#[derive(Debug)]
pub enum Command {
    // Start and Stop exist for the tray menu, which wants the explicit verbs
    // rather than a toggle whose meaning depends on state the menu cannot see.
    #[allow(dead_code, reason = "the tray menu lands in 1.8")]
    Start,
    #[allow(dead_code, reason = "the tray menu lands in 1.8")]
    Stop,
    Toggle,
}

/// The window's handle on the engine.
#[derive(Clone)]
pub struct Controller {
    frames: watch::Receiver<Frame>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Controller {
    pub fn frame(&self) -> Frame {
        self.frames.borrow().clone()
    }

    /// Resolves when the engine publishes a new frame.
    pub async fn changed(&mut self) {
        let _ = self.frames.changed().await;
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

pub struct Setup {
    pub profile: Profile,
    pub server: ServerConfig,
    pub exe: PathBuf,
    pub work_dir: PathBuf,
    pub engine_log: bliss_xray::LogLevel,
    pub autostart: bool,
}

/// Put the engine on its own runtime thread and hand back a handle.
pub fn spawn(setup: Setup) -> std::io::Result<Controller> {
    let (frame_tx, frame_rx) = watch::channel(initial_frame(&setup));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    std::thread::Builder::new()
        .name("bliss-engine".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    frame_tx.send_modify(|f| {
                        f.state = State::Failed;
                        f.error = Some(format!("could not start a runtime: {e}"));
                    });
                    return;
                }
            };
            runtime.block_on(drive(setup, frame_tx, cmd_rx));
        })?;

    Ok(Controller {
        frames: frame_rx,
        commands: cmd_tx,
    })
}

fn initial_frame(setup: &Setup) -> Frame {
    let server = &setup.server;
    let (sni, flow) = match &server.security {
        Security::Reality(r) => (r.sni.clone(), "reality · vision".to_owned()),
        Security::Tls(t) => (t.sni.clone().unwrap_or_default(), "tls".to_owned()),
        Security::None => (String::new(), "plain".to_owned()),
    };
    Frame {
        state: State::Stopped,
        server_tag: server.tag.clone(),
        server_addr: format!("{}:{}", server.address, server.port),
        sni,
        flow: match server.flow {
            Flow::XtlsRprxVision => flow,
            Flow::None => flow.replace(" · vision", ""),
        },
        listen: format!(
            "socks {} · http {}",
            setup.profile.inbound.socks, setup.profile.inbound.http
        ),
        history: vec![0; 40],
        fingerprint: match &server.security {
            Security::Reality(r) => r.fingerprint.to_string(),
            Security::Tls(tls) => tls.fingerprint.to_string(),
            Security::None => "-".to_owned(),
        },
        servers: setup.profile.servers.len(),
        ..Frame::default()
    }
}

/// The engine's whole life: start, publish, obey, stop.
async fn drive(
    setup: Setup,
    frames: watch::Sender<Frame>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let tracker = Arc::new(Tracker::new());
    let mut running: Option<Running> = None;
    let mut ticker = tokio::time::interval(TICK);
    let mut last = Meter::default();
    // Probing on every tick would put a connection a second through the tunnel
    // just to draw one number.
    let mut probe = tokio::time::interval(Duration::from_secs(8));

    if setup.autostart {
        running = start(&setup, &tracker, &frames).await;
    }

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                let want_on = match command {
                    Command::Start => true,
                    Command::Stop => false,
                    Command::Toggle => running.is_none(),
                };
                if want_on && running.is_none() {
                    running = start(&setup, &tracker, &frames).await;
                } else if !want_on && running.is_some() {
                    if let Some(parts) = running.take() {
                        parts.stop().await;
                    }
                    frames.send_modify(|f| {
                        f.state = State::Stopped;
                        f.error = None;
                        f.uptime = String::new();
                    });
                }
            }
            _ = ticker.tick() => {
                publish(&frames, &tracker, &mut last, running.as_ref());
            }
            _ = probe.tick() => {
                if let Some(parts) = running.as_ref() {
                    let outbound = Arc::clone(&parts.outbound);
                    let frames = frames.clone();
                    tokio::spawn(async move {
                        let measured = measure_latency(outbound.as_ref()).await;
                        frames.send_modify(|f| f.latency_ms = measured);
                    });
                }
            }
        }
    }

    if let Some(parts) = running.take() {
        parts.stop().await;
    }
}

struct Running {
    xray: bliss_xray::Xray,
    inbounds: Vec<JoinHandle<()>>,
    since: Instant,
    /// Kept so the latency probe can dial the same way traffic does.
    outbound: Arc<dyn bliss_core::Outbound>,
}

impl Running {
    async fn stop(self) {
        for task in self.inbounds {
            task.abort();
        }
        let _ = self.xray.shutdown().await;
    }
}

async fn start(
    setup: &Setup,
    tracker: &Arc<Tracker>,
    frames: &watch::Sender<Frame>,
) -> Option<Running> {
    frames.send_modify(|f| {
        f.state = State::Starting;
        f.error = None;
    });

    let needs = bliss_xray::minimum_xray_version(&setup.server).map(|(major, _)| major);
    let version = match bliss_xray::check_version(&setup.exe, needs).await {
        Ok(v) => v,
        Err(e) => return fail(frames, &e.to_string()),
    };

    let port = setup.profile.engine.bridge_port;
    let config = bliss_xray::build(&setup.server, port, setup.engine_log);
    let xray = match bliss_xray::Xray::spawn(&setup.exe, &config, &setup.work_dir, port).await {
        Ok(x) => x,
        Err(e) => return fail(frames, &e.to_string()),
    };

    let outbound = Arc::new(SocksOutbound::new(
        xray.socks_addr(),
        EngineInfo {
            kind: "xray-core",
            version: version.clone(),
            external: true,
        },
    )) as Arc<dyn bliss_core::Outbound>;

    // Take the ports before claiming success. A busy port is the usual way a
    // start fails, and the window must not show a green ring over a proxy that
    // never came up.
    let mut bound = Vec::new();
    for (addr, tag) in [
        (
            SocketAddr::new(setup.profile.inbound.listen, setup.profile.inbound.socks),
            "socks",
        ),
        (
            SocketAddr::new(setup.profile.inbound.listen, setup.profile.inbound.http),
            "http",
        ),
    ] {
        match (Inbound { addr, tag }).bind().await {
            Ok(listener) => bound.push(listener),
            Err(e) => {
                let _ = xray.shutdown().await;
                let hint = if e.kind() == std::io::ErrorKind::AddrInUse {
                    format!(
                        "порт {addr} уже занят другой программой — другим клиентом или прошлой копией bliss"
                    )
                } else {
                    format!("не удалось занять {addr}: {e}")
                };
                return fail(frames, &hint);
            }
        }
    }

    let mut inbounds = Vec::new();
    for listener in bound {
        let outbound = Arc::clone(&outbound);
        let tracker = Arc::clone(tracker);
        inbounds.push(tokio::spawn(async move {
            if let Err(e) = bliss_core::serve(listener, outbound, tracker).await {
                tracing::error!("inbound stopped: {e}");
            }
        }));
    }

    frames.send_modify(|f| {
        f.state = State::Running;
        f.engine = format!("xray-core {version}");
        f.error = None;
    });

    Some(Running {
        xray,
        inbounds,
        since: Instant::now(),
        outbound,
    })
}

fn fail(frames: &watch::Sender<Frame>, message: &str) -> Option<Running> {
    // Also to the log: the window shows this, but a window nobody is looking at
    // is not a record, and a failed start is exactly what gets asked about later.
    tracing::error!("engine did not start: {message}");
    frames.send_modify(|f| {
        f.state = State::Failed;
        f.error = Some(message.to_owned());
    });
    None
}

#[derive(Default)]
struct Meter {
    up: u64,
    down: u64,
    /// The chart's current full-scale value. See `snap_scale`.
    scale: u64,
}

/// The chart never means less than this. Without a floor, a few hundred bytes
/// of background chatter draws a full-height bar and the chart looks alarming
/// when nothing is happening.
const SCALE_FLOOR: u64 = 64 * 1024;

/// Round the chart's full-scale value up to the next doubling.
///
/// Two earlier attempts were wrong in opposite directions. Using the window's
/// plain maximum made every bar jump the moment a new peak arrived. Letting the
/// scale decay smoothly was worse: a bar already on screen kept growing as the
/// scale slid out from under it, so one sample was 100 pixels tall, then 200,
/// then 100 again.
///
/// Buckets fix both. A sample's height is settled when it is recorded and stays
/// put; the scale only moves when the peak crosses a doubling, which is rare
/// and reads as a deliberate change of range rather than as drift.
fn snap_scale(peak: u64) -> u64 {
    let mut bucket = SCALE_FLOOR;
    while bucket < peak {
        bucket = bucket.saturating_mul(2);
    }
    bucket
}

fn publish(
    frames: &watch::Sender<Frame>,
    tracker: &Tracker,
    last: &mut Meter,
    running: Option<&Running>,
) {
    let (up, down, opened) = tracker.totals();
    let per_second =
        |now: u64, before: u64| now.saturating_sub(before) * 1000 / TICK.as_millis().max(1) as u64;
    let up_rate = per_second(up, last.up);
    let down_rate = per_second(down, last.down);
    let mut connections = tracker.active();
    connections.sort_by_key(|c| std::cmp::Reverse(c.id));
    connections.extend(tracker.recent());

    let active = tracker.active_count() as u64;
    let mut scale = last.scale.max(SCALE_FLOOR);

    frames.send_modify(|f| {
        f.up_total = up;
        f.down_total = down;
        f.opened = opened;
        f.active = active;
        f.up_rate = up_rate;
        f.down_rate = down_rate;
        f.connections = connections;
        f.uptime = running.map(|r| hms(r.since.elapsed())).unwrap_or_default();

        // A fixed-length ring: the chart shows the recent past and the vector
        // never grows, which is the rule for anything that ticks forever.
        f.history.remove(0);
        f.history.push(down_rate);

        let peak = f.history.iter().copied().max().unwrap_or(0);
        scale = snap_scale(peak);
        f.scale = scale;
    });

    // Written back after the closure has run, so the smoothing carries into the
    // next tick instead of being recomputed from nothing every time.
    *last = Meter { up, down, scale };
}

fn hms(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// Bytes as the mockups write them: a comma for the decimal mark.
pub fn bytes(n: u64) -> String {
    const UNITS: [(u64, &str); 4] = [(1 << 30, "ГБ"), (1 << 20, "МБ"), (1 << 10, "КБ"), (1, "Б")];
    for (scale, unit) in UNITS {
        if n >= scale {
            let value = n as f64 / scale as f64;
            let text = if value >= 100.0 {
                format!("{value:.0}")
            } else {
                format!("{value:.2}")
            };
            return format!("{} {unit}", text.replace('.', ","));
        }
    }
    "0 Б".to_owned()
}

pub fn rate(n: u64) -> String {
    format!("{}/с", bytes(n))
}

/// Round trip through the tunnel, with the handshake excluded.
///
/// Two earlier attempts at this were wrong in instructive ways. Timing
/// `connect_tcp` measured nothing: xray answers the SOCKS request as soon as it
/// accepts it, not when the far end is up, so the figure came back as zero.
/// Timing the first request instead gave ~750 ms, because a fresh connection
/// pays for the TLS and REALITY handshakes — true, but not what "latency"
/// means to anyone reading it.
///
/// So the probe sends two requests on one kept-alive connection and times the
/// second. The first pays for the handshake; the second is the round trip.
async fn measure_latency(outbound: &dyn bliss_core::Outbound) -> Option<u64> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const REQUEST: &[u8] = b"HEAD / HTTP/1.1\r\nHost: one.one.one.one\r\n\r\n";

    /// Read to the end of a response head. HEAD replies carry no body, so the
    /// blank line is the whole of it — and the socket must be left empty, or
    /// the next read returns leftovers instantly and times nothing.
    async fn drain_head<S: AsyncReadExt + Unpin>(stream: &mut S) -> Option<()> {
        let mut tail = [0u8; 4];
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte).await.ok()?;
            tail = [tail[1], tail[2], tail[3], byte[0]];
            if &tail == b"\r\n\r\n" {
                return Some(());
            }
        }
    }

    // Anycast, so the figure reflects the tunnel rather than the distance to
    // one particular datacentre.
    let target = bliss_core::Target::domain("one.one.one.one", 80);
    let probe = async {
        let mut stream = outbound.connect_tcp(&target).await.ok()?;

        stream.write_all(REQUEST).await.ok()?;
        drain_head(&mut stream).await?;

        let started = Instant::now();
        stream.write_all(REQUEST).await.ok()?;
        drain_head(&mut stream).await?;
        Some(started.elapsed().as_millis() as u64)
    };

    // A failed probe says nothing about latency, and a timeout drawn as a
    // measurement would be a lie with a unit attached — so it collapses to None
    // either way.
    tokio::time::timeout(Duration::from_secs(8), probe)
        .await
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This calculation was wrong twice, in opposite directions, and both times
    /// the symptom was a chart that would not hold still. The property that
    /// matters is not the exact number: it is that a given sample keeps the
    /// same height while its neighbours change.
    #[test]
    fn a_sample_keeps_its_height_as_the_window_moves() {
        let heights = |window: &[u64], sample: u64| {
            let peak = window.iter().copied().max().unwrap_or(0);
            sample * 100 / snap_scale(peak)
        };

        // A 40 KB/s sample, as traffic around it comes and goes but stays
        // inside the same bucket.
        let quiet = heights(&[40_000, 10_000], 40_000);
        let busier = heights(&[40_000, 50_000, 10_000], 40_000);
        let busiest = heights(&[40_000, 60_000], 40_000);
        assert_eq!(quiet, busier);
        assert_eq!(quiet, busiest);
    }

    #[test]
    fn the_scale_only_moves_in_doublings() {
        assert_eq!(snap_scale(0), SCALE_FLOOR);
        assert_eq!(snap_scale(SCALE_FLOOR), SCALE_FLOOR);
        assert_eq!(snap_scale(SCALE_FLOOR + 1), SCALE_FLOOR * 2);
        assert_eq!(snap_scale(SCALE_FLOOR * 2), SCALE_FLOOR * 2);
        assert_eq!(snap_scale(SCALE_FLOOR * 3), SCALE_FLOOR * 4);
        // Nothing real reaches this, but a loop that multiplies must terminate.
        assert!(snap_scale(u64::MAX) > 0);
    }

    #[test]
    fn bytes_are_written_the_way_the_mockups_write_them() {
        assert_eq!(bytes(0), "0 Б");
        assert_eq!(bytes(512), "512 Б");
        assert_eq!(bytes(1536), "1,50 КБ");
        assert_eq!(bytes(5 * 1024 * 1024), "5,00 МБ");
        // Three digits drop the fraction: "104 МБ" not "104,32 МБ".
        assert_eq!(bytes(109 * 1024 * 1024), "109 МБ");
        assert_eq!(rate(1024), "1,00 КБ/с");
    }
}
