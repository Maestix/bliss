//! The tunnel's process: one loop that owns the profile and the tunnel, and a
//! socket the screens and the service talk to it through.
//!
//! The order of a start is the one the home screen shows (docs/android-design.md):
//! the engine, then the stack on the TUN, then one request through the server
//! — and a server that does not answer is a failure, not a tunnel that looks
//! up and carries nothing. A failed start drops the TUN's descriptor, and the
//! service, watching the state over the same socket, takes the VPN down, so a
//! phone is never left with a VPN that leads nowhere.
//!
//! Nothing here ticks while no screen is open: the rates are sampled only
//! while a client that draws them is connected (docs/android-stack.md, rule 2).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bliss_core::{Route, Router, Serving, Target, Tracker};
use bliss_proto::{Action, Profile, Rule, ServerConfig, ServerEntry, Subscription};
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::geo;
use crate::protocol::{
    ConnRow, Frame, GeoSource, GeoView, GroupRow, Numbers, RuleRow, ServerRow, Suggest, Toast,
    UiCommand, action_key, conditions, how_it_hides, kind,
};
use crate::rule_text;
use crate::subscription;
use crate::ui::plural;

/// Where things are: the app's own folder, and the folder Android unpacked
/// the APK's native libraries into — xray among them, as `libxray.so`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub files: PathBuf,
    pub libs: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    fn profile(&self) -> PathBuf {
        self.files.join("bliss.toml")
    }
    fn selected(&self) -> PathBuf {
        self.files.join("selected")
    }
    fn engine(&self) -> PathBuf {
        self.libs.join("libxray.so")
    }
    fn work(&self) -> PathBuf {
        self.files.join("engine")
    }
}

/// How the start's check waits for the server before calling it silent.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// How long «Автовыбор» trusts a sweep of handshakes before a start takes
/// it again.
const AUTO_FRESH: Duration = Duration::from_secs(600);
/// The rates kept for the chart: twenty seconds at one a second, and one past
/// each edge so the line can glide (AGENTS.md §6 rule 22).
const HISTORY: usize = 22;
/// Servers in a profile however they arrive: the desktop's cap
/// (`MAX_PROFILE_SERVERS` in crates/app/src/profile_file.rs).
const MAX_PROFILE_SERVERS: usize = 5_000;
/// Servers with a star, at most: anything that grows has a cap (AGENTS.md §5).
const MAX_FAVORITES: usize = 256;
/// A server list older than this is fetched again: when the engine starts,
/// and at the next `REFRESH_CHECK` while it runs.
const STALE: i64 = 12 * 3600;
const REFRESH_CHECK: Duration = Duration::from_secs(30 * 60);

/// The phone's DNS server as the service names it (`TunnelService.DNS`):
/// inside the TUN's /30, answered by the stack itself.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const DNS_ADDRESS: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(172, 19, 0, 2)),
    53,
);

/// What reaches the loop.
pub enum Cmd {
    /// The service established the TUN: its descriptor, and the ticket the
    /// service was given for it (see `Frame::ticket`).
    Connect(OwnedFd, u64),
    Ui(UiCommand),
    Step(u64, u8),
    Started(u64, Result<Running, String>),
    /// A switch ended: the link it switched to, and the time through it.
    Switched(u64, String, Result<u32, String>),
    /// Handshake times by link: a list refreshed or a server deleted while
    /// they were measured moves the indices.
    Pinged(Vec<(String, Option<u32>)>),
    /// One server's real check, by its link: the timed request, or none.
    Checked(String, Option<u32>),
    /// The engine's version, learnt by a check before any tunnel.
    Version(bliss_xray::Version),
    Fetched {
        id: String,
        url: String,
        result: Result<subscription::Fetched, String>,
    },
    /// A check through the running tunnel, asked when a screen came on: whether
    /// the server still answers (`health`).
    Health(u64, bool),
    /// The stack ended by itself: the system took the TUN away.
    StackEnded(u64, String),
    /// The rule sets' fetch: bytes so far, then how it ended.
    GeoProgress(u64),
    GeoDone(Result<(), String>),
}

/// A tunnel that is up: the stack reading the TUN, and which server's
/// engine it goes through. The engine itself lives on the bridge port
/// (`Port`), not here.
pub struct Running {
    /// The server's link, which can differ from the one chosen since.
    link: String,
    /// The engine's SOCKS address.
    socks: SocketAddr,
    serving: Arc<Serving>,
    stack: JoinHandle<()>,
    /// Up, but nothing loaded through the server: kept so «Автовыбор» can
    /// try the next one over the same TUN (`bring_up`'s `keep`).
    doubt: Option<String>,
    since: i64,
    latency: u32,
    version: bliss_xray::Version,
}

/// The engine on the bridge port, and the queue of work on that port.
///
/// One xray at a time can hold the port, so every start, switch and stop
/// takes its turn here, in the order the loop asked for them: each waits for
/// the one before it, takes whatever engine is on the port down, and leaves
/// its own there. Before, a stop only began the old engine's shutdown and a
/// start spawned the new one at once, and the new one found the port taken:
/// «Полностью мимо VPN» on an app, «Продолжить» after «Пауза», and a second
/// server picked during a switch all failed the tunnel with "already taken".
#[derive(Default)]
struct Port {
    engine: Arc<std::sync::Mutex<Option<bliss_xray::Xray>>>,
    turn: Option<JoinHandle<()>>,
}

type Slot = Arc<std::sync::Mutex<Option<bliss_xray::Xray>>>;

impl Port {
    /// Queue `work` behind every turn already queued.
    fn queue<F>(&mut self, work: impl FnOnce(Slot) -> F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let before = self.turn.take();
        let work = work(Arc::clone(&self.engine));
        self.turn = Some(tokio::spawn(async move {
            if let Some(before) = before {
                let _ = before.await;
            }
            work.await;
        }));
    }

    /// Take the engine off the port, once the turns before this one are done.
    fn release(&mut self) {
        self.queue(|slot| async move { clear(&slot).await });
    }
}

/// Take down whatever engine is on the port, and wait until it is gone.
async fn clear(slot: &Slot) {
    let old = slot.lock().expect("port lock").take();
    if let Some(old) = old {
        let _ = old.shutdown().await;
    }
}

/// The loop's handle, kept by the JNI side.
pub struct Handle {
    pub commands: mpsc::UnboundedSender<Cmd>,
    _runtime: tokio::runtime::Runtime,
}

static HANDLE: OnceLock<Handle> = OnceLock::new();

/// Start the engine, once per process. Later calls find it running.
pub fn start(paths: Paths) -> std::io::Result<&'static Handle> {
    if let Some(handle) = HANDLE.get() {
        return Ok(handle);
    }
    crate::logs::init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("bliss")
        .build()?;
    let (tx, rx) = mpsc::unbounded_channel();
    let (frames_tx, frames_rx) = watch::channel(Frame::default());
    let watchers = Arc::new(AtomicUsize::new(0));
    let (watching_tx, watching_rx) = watch::channel(0usize);

    let engine = Engine::load(paths.clone(), tx.downgrade(), frames_tx);
    runtime.spawn(engine.run(rx, watching_rx));
    runtime.spawn(listen(
        paths.socket.clone(),
        tx.clone(),
        frames_rx,
        watchers,
        watching_tx,
    ));
    tracing::info!("engine: started in {}", paths.files.display());
    let _ = HANDLE.set(Handle {
        commands: tx,
        _runtime: runtime,
    });
    Ok(HANDLE.get().expect("just set"))
}

/// The running engine, if `start` was called.
pub fn handle() -> Option<&'static Handle> {
    HANDLE.get()
}

// --- the socket -------------------------------------------------------------

/// A network rule's id as a person reads it.
fn net_name(id: &str) -> String {
    match id {
        "wifi" => "Любой Wi-Fi".to_owned(),
        "mobile" => "Мобильная сеть".to_owned(),
        "ethernet" => "Кабель".to_owned(),
        other => other.strip_prefix("wifi:").unwrap_or(other).to_owned(),
    }
}

/// The state in the journal's words.
fn state_word(state: State) -> &'static str {
    match state {
        State::Off => "выключен",
        State::Busy => "подключается",
        State::On => "подключён",
        State::Failed => "ошибка",
    }
}

/// The parts of the frame a screen was last sent, each as its JSON text.
///
/// A frame goes out every second while a screen looks, and it carried the
/// whole server list — up to 5 000 rows — the rules and every subscription
/// each time, for the numbers' sake; the screen parsed and recomposed all
/// of it. Now a line holds only the top-level parts that changed since the
/// last one this screen got, and the screen merges it into what it has. The
/// first line after a connection or a hello is every part.
#[derive(Default)]
struct Sent {
    parts: HashMap<String, String>,
}

impl Sent {
    /// The line to send for `frame`, or none when nothing changed.
    fn patch(&mut self, frame: &Frame) -> Option<String> {
        let serde_json::Value::Object(whole) = serde_json::to_value(frame).ok()? else {
            return None;
        };
        let mut changed = serde_json::Map::new();
        for (key, value) in whole {
            let text = value.to_string();
            if self.parts.get(&key) != Some(&text) {
                self.parts.insert(key.clone(), text);
                changed.insert(key, value);
            }
        }
        (!changed.is_empty()).then(|| serde_json::Value::Object(changed).to_string())
    }
}

/// The longest command a screen may send: an import of a pasted list is the
/// longest there is, and a subscription's body is held to the same 4 MB.
const MAX_LINE: usize = 4 << 20;

/// Serve the screens and the service: frames out, commands in.
async fn listen(
    path: PathBuf,
    commands: mpsc::UnboundedSender<Cmd>,
    frames: watch::Receiver<Frame>,
    watchers: Arc<AtomicUsize>,
    watching: watch::Sender<usize>,
) {
    let _ = std::fs::remove_file(&path);
    let listener = match tokio::net::UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("engine: cannot listen at {}: {e}", path.display());
            return;
        }
    };
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // Out of descriptors, it fails again at once: a pause, not a spin.
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let commands = commands.clone();
        let frames = frames.clone();
        let watchers = Arc::clone(&watchers);
        let watching = watching.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            let mut line = Vec::new();
            let mut frames = frames;
            let mut full = false;
            // What this screen was last sent, part by part: only parts that
            // changed go out again (`patch`).
            let mut sent = Sent::default();
            // The first frame goes out at once; then one per change.
            frames.mark_changed();
            loop {
                // Cancel-safe: what a frame interrupts stays in `line`, and
                // the limit counts it.
                let mut limited = (&mut read).take((MAX_LINE - line.len()) as u64);
                tokio::select! {
                    got = limited.read_until(b'\n', &mut line) => {
                        match got {
                            Ok(0) | Err(_) => break,
                            Ok(_) if line.last() != Some(&b'\n') => {
                                if line.len() >= MAX_LINE {
                                    tracing::warn!("engine: a screen sent a line past {MAX_LINE} bytes; closed");
                                    break;
                                }
                                continue;
                            }
                            Ok(_) => {}
                        }
                        let text = std::mem::take(&mut line);
                        match serde_json::from_slice::<UiCommand>(&text) {
                            Ok(UiCommand::Hello { full: wants }) => {
                                if wants != full {
                                    full = wants;
                                    let n = if full {
                                        watchers.fetch_add(1, Ordering::SeqCst) + 1
                                    } else {
                                        watchers.fetch_sub(1, Ordering::SeqCst) - 1
                                    };
                                    let _ = watching.send(n);
                                }
                                // A hello is a screen starting over: everything again.
                                sent = Sent::default();
                                frames.mark_changed();
                            }
                            Ok(command) => {
                                let _ = commands.send(Cmd::Ui(command));
                            }
                            Err(e) => tracing::warn!("engine: a line from a screen did not read: {e}"),
                        }
                    }
                    changed = frames.changed() => {
                        if changed.is_err() { break; }
                        let frame = frames.borrow_and_update().clone();
                        let Some(mut text) = sent.patch(&frame) else { continue };
                        text.push('\n');
                        if write.write_all(text.as_bytes()).await.is_err() { break; }
                    }
                }
            }
            if full {
                let n = watchers.fetch_sub(1, Ordering::SeqCst) - 1;
                let _ = watching.send(n);
            }
        });
    }
}

/// A login for the engine's port, made of random bytes: every app on the
/// phone can reach loopback, and without one any of them could use the
/// server around bliss's rules, or find the proxy by knocking on its port.
fn fresh_login() -> bliss_core::Login {
    let word = |n: usize| {
        let mut bytes = vec![0u8; n];
        let read = std::fs::File::open("/dev/urandom").and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut bytes)
        });
        if read.is_err() {
            // Not seen on Android; the clock and the process's hasher keys
            // still make it nobody else's.
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            );
            let seed = h.finish().to_le_bytes();
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = seed[i % 8] ^ (i as u8).wrapping_mul(0x9d);
            }
        }
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    bliss_core::Login {
        user: word(8),
        pass: word(16),
    }
}

/// What a connection's program is called when it belongs to another user of
/// the phone (a work profile, Shelter's): `ownerOf` in Native.kt says it so,
/// with the uid after the colon.
const FOREIGN: &str = "foreign:";

/// The programs the tunnel does not carry for: every one of another user of
/// the phone, and the apps the person put «мимо VPN» (`bypass.txt`, beside
/// this process's files), whose sockets can reach the tunnel's interface by
/// name all the same (`bliss_tun::ShutOut`).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn shut_out_of(files: &std::path::Path) -> bliss_tun::ShutOut {
    let bypassed: std::collections::HashSet<String> =
        std::fs::read_to_string(files.join("bypass.txt"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect();
    Arc::new(move |program| program.starts_with(FOREIGN) || bypassed.contains(program))
}

/// Another such port, not `taken`.
fn free_port_besides(taken: u16) -> u16 {
    (0..8)
        .filter_map(|_| free_port())
        .find(|port| *port != taken)
        .unwrap_or_else(|| taken.wrapping_add(1))
}

/// A port nobody holds now, from the system's own choice: the engine's
/// port differs from run to run, so it is not a fixed place to knock.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    Some(listener.local_addr().ok()?.port())
}

/// Write a file and wait until it is on the disk, so that a rename over the
/// old one never leaves an empty file after the phone loses power.
fn write_synced(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Copy a profile that does not read to `bliss.toml.bak` (`.1.bak` and on
/// if taken), as the desktop does; the copy's path, if it was made.
fn keep_aside(path: &std::path::Path) -> Option<PathBuf> {
    let free = (0..100).map(|n| match n {
        0 => path.with_extension("toml.bak"),
        n => path.with_extension(format!("toml.{n}.bak")),
    });
    let target = free.into_iter().find(|p| !p.exists())?;
    std::fs::copy(path, &target).ok()?;
    Some(target)
}

// --- the loop ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Off,
    Busy,
    On,
    Failed,
}

/// Something deleted, kept until the next deletion so «Вернуть» can put it
/// back where it was.
enum Trashed {
    Server(usize, ServerEntry),
    Group(Subscription, Vec<(usize, ServerEntry)>),
    Rule(usize, Rule),
}

struct Engine {
    paths: Paths,
    own: mpsc::WeakUnboundedSender<Cmd>,
    frames: watch::Sender<Frame>,
    profile: Profile,
    servers: Vec<Option<ServerConfig>>,
    /// The chosen server, by its link: indices move when a list is refreshed.
    selected: Option<String>,
    state: State,
    step: u8,
    error: Option<String>,
    suggest: Option<Suggest>,
    epoch: u64,
    running: Option<Running>,
    port: Port,
    /// The engine's SOCKS port and the login it asks for, made afresh for
    /// each run of the tunnel's process (`fresh_login`, `free_port`).
    bridge: u16,
    /// The Trojan inbound the datagrams go through, beside the bridge
    /// (`bliss_xray::datagrams_over_trojan`).
    door: u16,
    login: bliss_core::Login,
    /// The tracker's totals as the tunnel last came up: «за сессию» counts
    /// from there. The tracker lives as long as the process, which the
    /// bound screens can keep for days, across any number of connections.
    session: (u64, u64, u64),
    /// «Автовыбор»: a TUN waiting for the ping sweep a start asked for.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pending: Option<OwnedFd>,
    /// When the handshakes were last measured.
    pinged_at: Option<Instant>,
    /// Servers «Автовыбор» tried in this start and saw fail, by link.
    tried: std::collections::HashSet<String>,
    /// The descriptor of a TUN whose start is waiting for a server switch to
    /// end, or that arrived while busy.
    tracker: Arc<Tracker>,
    /// The names handed out as the phone's DNS: kept across reconnects, as
    /// programs keep the answers for a minute.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    names: Arc<bliss_tun::names::FakeNames>,
    version: Option<bliss_xray::Version>,
    pings: HashMap<String, Option<u32>>,
    probing: bool,
    /// Real checks by link: a time through the server, or none for a server
    /// nothing loaded through.
    checks: HashMap<String, Option<u32>>,
    checking: std::collections::HashSet<String>,
    fetching: HashMap<String, Option<String>>,
    trash: Option<Trashed>,
    toast: Option<Toast>,
    toasts: u64,
    /// The newest TUN handed over; see `Frame::ticket`.
    ticket: u64,
    history: VecDeque<u64>,
    last: (Instant, u64, u64),
    rates: (u64, u64),
    table_open: bool,
    log_open: bool,
    warnings: Vec<String>,
    /// The phone's own settings (geo.rs).
    settings: geo::Settings,
    /// Bytes so far while the rule sets are fetched.
    geo_fetch: Option<u64>,
    geo_problem: Option<String>,
    /// The categories, read once the editor asks, until it closes.
    categories: Option<Vec<String>>,
    /// Journals written for sharing so far (`Frame::journal`).
    journals: u64,
    /// The card the screens have open (`Frame::card`).
    card: Option<crate::protocol::CardView>,
    /// The answer to the last save of the server form (`Frame::saved`).
    saved: Option<crate::protocol::Saved>,
    /// The phone's network, as the service last said.
    network: Option<crate::networks::Seen>,
    /// What the network asks of the service (`Frame::wish`).
    wish: Option<&'static str>,
    wishes: u64,
    /// Switched off by the person: no network's rule switches the tunnel
    /// on until they do (`UiCommand::Disconnect`).
    held_off: bool,
}

impl Engine {
    fn load(
        paths: Paths,
        own: mpsc::WeakUnboundedSender<Cmd>,
        frames: watch::Sender<Frame>,
    ) -> Self {
        let _ = std::fs::create_dir_all(&paths.files);
        let profile = match std::fs::read_to_string(paths.profile()) {
            Ok(text) => toml::from_str::<Profile>(&text).unwrap_or_else(|e| {
                // Kept aside before anything writes over it: the next save
                // would otherwise take every server and subscription with it.
                let kept = keep_aside(&paths.profile());
                tracing::warn!(
                    "engine: the profile did not read, starting afresh{}: {}",
                    kept.map(|p| format!(" (the old one is {})", p.display()))
                        .unwrap_or_default(),
                    e.message()
                );
                first_profile()
            }),
            Err(_) => first_profile(),
        };
        let selected = std::fs::read_to_string(paths.selected()).ok();
        let settings = geo::Settings::load(&paths.files);
        let mut engine = Self {
            paths,
            own,
            frames,
            servers: Vec::new(),
            profile,
            selected,
            state: State::Off,
            step: 0,
            error: None,
            suggest: None,
            epoch: 0,
            running: None,
            port: Port::default(),
            bridge: 0,
            door: 0,
            login: fresh_login(),
            session: (0, 0, 0),
            pending: None,
            pinged_at: None,
            tried: std::collections::HashSet::new(),
            tracker: Arc::new(Tracker::new()),
            names: Arc::new(bliss_tun::names::FakeNames::new()),
            version: None,
            pings: HashMap::new(),
            probing: false,
            checks: HashMap::new(),
            checking: std::collections::HashSet::new(),
            fetching: HashMap::new(),
            trash: None,
            toast: None,
            toasts: 0,
            ticket: 0,
            history: VecDeque::new(),
            last: (Instant::now(), 0, 0),
            rates: (0, 0),
            table_open: false,
            log_open: false,
            warnings: Vec::new(),
            settings,
            geo_fetch: None,
            geo_problem: None,
            categories: None,
            journals: 0,
            card: None,
            saved: None,
            network: None,
            wish: None,
            wishes: 0,
            held_off: false,
        };
        engine.bridge = free_port().unwrap_or(engine.profile.engine.bridge_port);
        engine.door = free_port_besides(engine.bridge);
        engine.settle_protocols();
        engine.offer_quic_rule();
        engine.reparse();
        engine
    }

    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Cmd>,
        mut watching: watch::Receiver<usize>,
    ) {
        self.publish();
        self.refresh_stale();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Lists were fetched when the tunnel's process started and never
        // after, so a tunnel left on for days kept its first list. Due ones
        // are fetched while it runs too; whether one is due is the wall
        // clock's to say, since this one stands still while the phone sleeps.
        let mut lists =
            tokio::time::interval_at(tokio::time::Instant::now() + REFRESH_CHECK, REFRESH_CHECK);
        lists.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut drawn = 0usize;
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    self.obey(command);
                    self.publish();
                }
                // Disabled while nobody draws the numbers: the interval is
                // then not polled at all, and the process sleeps.
                _ = ticker.tick(), if drawn > 0 => {
                    self.sample();
                    self.publish();
                }
                _ = lists.tick() => {
                    self.refresh_stale();
                    self.publish();
                }
                changed = watching.changed() => {
                    if changed.is_err() { break; }
                    let now = *watching.borrow_and_update();
                    if drawn == 0 && now > 0 {
                        // Someone opened a screen: fresh numbers, and pings if
                        // none were taken yet.
                        self.last = self.measure_now();
                        if self.pings.is_empty() { self.ping_all(); }
                        self.health();
                    }
                    drawn = now;
                    if drawn == 0 { self.history.clear(); self.table_open = false; self.log_open = false; }
                    self.publish();
                }
            }
        }
    }

    fn obey(&mut self, command: Cmd) {
        match command {
            Cmd::Connect(fd, ticket) => {
                self.ticket = self.ticket.max(ticket);
                self.held_off = false;
                self.connect(fd);
            }
            Cmd::Ui(command) => self.ui(command),
            Cmd::Step(epoch, step) => {
                if epoch == self.epoch && self.state == State::Busy {
                    self.step = step;
                }
            }
            Cmd::Started(epoch, result) => {
                if epoch != self.epoch {
                    // A start nobody waits for any more: its stack goes, and
                    // its engine with the next turn on the port, which
                    // whatever moved the epoch has queued.
                    if let Ok(running) = result {
                        running.stack.abort();
                    }
                    return;
                }
                match result {
                    Ok(running) if running.doubt.is_some() => {
                        // «Автовыбор» kept the TUN over a server nothing
                        // loaded through: the next one, the stack staying.
                        let why = running.doubt.clone().unwrap_or_default();
                        let link = running.link.clone();
                        self.version = Some(running.version);
                        self.running = Some(running);
                        self.session = self.tracker.totals();
                        self.fail_over(&link, why);
                    }
                    Ok(running) => {
                        tracing::info!("tunnel: up, {} ms through the server", running.latency);
                        self.version = Some(running.version);
                        let link = running.link.clone();
                        self.checks.insert(link.clone(), Some(running.latency));
                        self.tried.clear();
                        self.running = Some(running);
                        self.session = self.tracker.totals();
                        self.state = State::On;
                        self.error = None;
                        self.suggest = None;
                        self.refresh_geo_if_stale();
                        self.follow_choice(&link);
                    }
                    Err(why) => self.fail(why),
                }
            }
            Cmd::Switched(epoch, link, result) => {
                if epoch != self.epoch {
                    return;
                }
                match (result, self.running.as_mut()) {
                    (Ok(latency), Some(running)) => {
                        running.link = link.clone();
                        running.latency = latency;
                        running.since = now_ms();
                        self.checks.insert(link.clone(), Some(latency));
                        self.tried.clear();
                        self.state = State::On;
                        self.error = None;
                        self.suggest = None;
                        self.follow_choice(&link);
                    }
                    (Ok(_), None) => self.port.release(),
                    (Err(why), Some(_)) if self.settings.auto_server => self.fail_over(&link, why),
                    (Err(why), _) => self.fail(why),
                }
            }
            Cmd::Health(epoch, fine) => {
                if epoch == self.epoch
                    && !fine
                    && self.state == State::On
                    && self.settings.auto_server
                {
                    let link = self
                        .running
                        .as_ref()
                        .map(|r| r.link.clone())
                        .unwrap_or_default();
                    self.fail_over(&link, "сервер перестал отвечать".to_owned());
                }
            }
            Cmd::StackEnded(epoch, why) => {
                if epoch == self.epoch && self.running.is_some() {
                    tracing::info!("tunnel: the device went away — {why}");
                    self.disconnect();
                }
            }
            Cmd::Checked(link, ms) => {
                self.checking.remove(&link);
                self.checks.insert(link, ms);
            }
            Cmd::Version(version) => self.version = Some(version),
            Cmd::Pinged(results) => {
                self.probing = false;
                self.pinged_at = Some(Instant::now());
                for (link, ms) in results {
                    self.pings.insert(link, ms);
                }
                if let Some(fd) = self.pending.take() {
                    self.connect(fd);
                }
            }
            Cmd::Fetched { id, url, result } => self.fetched(id, url, result),
            Cmd::GeoProgress(bytes) => {
                if self.geo_fetch.is_some() {
                    self.geo_fetch = Some(bytes);
                }
            }
            Cmd::GeoDone(result) => {
                self.geo_fetch = None;
                match result {
                    Ok(()) => {
                        tracing::info!("geo: the rule sets are new");
                        self.settings.geo_updated = Some(jiff::Timestamp::now().as_second());
                        self.settings.save(&self.paths.files);
                        self.geo_problem = None;
                        if self.categories.is_some() {
                            self.categories = Some(geo::names(&self.paths.files));
                        }
                        self.commit_routing();
                        self.toast("Гео-файлы обновлены", false, false);
                    }
                    Err(why) => {
                        tracing::warn!("geo: {why}");
                        self.toast(&format!("Гео-файлы не обновились: {why}"), true, false);
                        self.geo_problem = Some(why);
                    }
                }
            }
        }
    }

    fn ui(&mut self, command: UiCommand) {
        match command {
            UiCommand::Hello { .. } => {}
            UiCommand::Import { text } => self.import(&text),
            UiCommand::Select { index, key } => {
                if let Some(index) = self.place_of(index, key.as_deref()) {
                    // A server picked by hand is the person's choice.
                    self.set_auto(false);
                    self.select(index);
                }
            }
            UiCommand::Delete { index, key } => {
                if let Some(index) = self.place_of(index, key.as_deref()) {
                    self.delete(index);
                }
            }
            UiCommand::DeleteGroup { id } => self.delete_group(&id),
            UiCommand::Restore => self.restore(),
            UiCommand::Ping => self.ping_all(),
            UiCommand::Check { index: None, .. } => self.check_servers(None),
            UiCommand::Check {
                index: Some(index),
                key,
            } => {
                if let Some(index) = self.place_of(index, key.as_deref()) {
                    self.check_servers(Some(index));
                }
            }
            UiCommand::Disconnect { by_hand } => {
                // Switched off by the person: a network's «Включать bliss»
                // waits until they switch it on again. It used to win at the
                // next network report, and a heart pressed while the phone
                // moved between networks was undone at once.
                self.held_off |= by_hand;
                self.disconnect();
            }
            UiCommand::Rule { index, on } => {
                if let Some(rule) = self.profile.routing.rules.get_mut(index) {
                    rule.enabled = on;
                    self.commit_routing();
                }
            }
            UiCommand::Fallback { action } => {
                self.profile.routing.fallback = match action.as_str() {
                    "direct" => Action::Direct,
                    "block" => Action::Block,
                    _ => Action::Proxy,
                };
                self.commit_routing();
            }
            UiCommand::Preset { name } => self.apply_preset(&name),
            UiCommand::SavePreset => self.save_preset(),
            UiCommand::Refresh { id } => match id {
                Some(id) => self.refresh(&id),
                None => {
                    let ids: Vec<String> = self
                        .profile
                        .subscriptions
                        .iter()
                        .map(|s| s.id.clone())
                        .collect();
                    for id in ids {
                        self.refresh(&id);
                    }
                }
            },
            UiCommand::Table { open } => self.table_open = open,
            UiCommand::Log { open } => self.log_open = open,
            UiCommand::ShareLog => self.share_log(),
            UiCommand::Auto { on } => self.set_auto(on),
            UiCommand::Network {
                kind,
                ssid,
                captive,
            } => self.network_changed(kind.map(|kind| crate::networks::Seen {
                kind,
                ssid,
                captive,
            })),
            UiCommand::SetNetwork {
                id,
                connect,
                preset,
                server,
            } => {
                let connect = match &*connect {
                    "on" | "off" => connect,
                    _ => String::new(),
                };
                crate::networks::put(
                    &mut self.settings.networks,
                    crate::networks::NetworkRule {
                        id,
                        connect,
                        preset,
                        server: server.filter(|s| !s.is_empty()),
                    },
                );
                self.settings.save(&self.paths.files);
            }
            UiCommand::Favorite { key, on } => self.favorite(key, on),
            UiCommand::ServerCard { index, key } => {
                if let Some(index) = self.place_of(index, key.as_deref()) {
                    self.server_card(index);
                }
            }
            UiCommand::GroupCard { id } => self.group_card(&id),
            UiCommand::CloseCard => self.card = None,
            UiCommand::SaveServer { key, form, attempt } => {
                let (ok, text) = match self.save_server(key.as_deref(), &form) {
                    Ok(text) => (true, text),
                    Err(why) => (false, why),
                };
                if ok {
                    self.card = None;
                    self.toast(&text, false, false);
                }
                self.saved = Some(crate::protocol::Saved { attempt, ok, text });
            }
            UiCommand::SaveRule {
                index,
                name,
                action,
                text,
            } => self.save_rule(index, &name, &action, &text),
            UiCommand::DeleteRule { index } => self.delete_rule(index),
            UiCommand::MoveRule { from, to } => {
                let rules = &mut self.profile.routing.rules;
                if from < rules.len() && to < rules.len() && from != to {
                    let rule = rules.remove(from);
                    rules.insert(to, rule);
                    self.commit_routing();
                }
            }
            UiCommand::AppRoute {
                package,
                label,
                action,
            } => self.app_route(&package, &label, action.as_deref()),
            UiCommand::GeoUpdate { source } => {
                if let Some(source) = source {
                    self.settings.geo_source = geo::source(&source).key.to_owned();
                    self.settings.save(&self.paths.files);
                }
                self.update_geo();
            }
            UiCommand::GeoAuto { on } => {
                self.settings.geo_auto = on;
                self.settings.save(&self.paths.files);
            }
            UiCommand::GeoNames { open } => {
                self.categories = open.then(|| geo::names(&self.paths.files));
            }
        }
    }

    // --- the tunnel ---------------------------------------------------------

    fn current(&self) -> Option<(usize, ServerConfig)> {
        let wanted = self.selected.as_deref();
        let by_link = wanted.and_then(|link| {
            self.profile
                .servers
                .iter()
                .position(|entry| entry.link == link)
        });
        let index = by_link.or_else(|| self.servers.iter().position(Option::is_some))?;
        self.servers[index].clone().map(|server| (index, server))
    }

    fn connect(&mut self, fd: OwnedFd) {
        if self.settings.auto_server && !self.profile.servers.is_empty() {
            // Measured on a start, never on a timer: a sweep older than ten
            // minutes, or none, is taken again first, the TUN waiting.
            let fresh = self.pinged_at.is_some_and(|at| at.elapsed() < AUTO_FRESH);
            if !fresh {
                self.pending = Some(fd);
                self.epoch += 1;
                self.state = State::Busy;
                self.step = 0;
                self.error = None;
                self.suggest = None;
                self.ping_all();
                return;
            }
            self.tried.clear();
            if let Some(index) = self.auto_pick() {
                self.remember(self.profile.servers[index].link.clone());
            }
        }
        let Some((_, server)) = self.current() else {
            self.toast("Сначала добавьте сервер", true, false);
            self.state = State::Off;
            // The descriptor is dropped here: the service sees «off» and
            // takes the VPN down.
            drop(fd);
            return;
        };
        if let Some(running) = self.running.take() {
            running.stack.abort();
        }
        #[cfg(not(target_os = "android"))]
        {
            drop((fd, server));
            self.fail("TUN бывает только на Android".to_owned());
        }
        #[cfg(target_os = "android")]
        self.bring_up(fd, server);
    }

    #[cfg(target_os = "android")]
    fn bring_up(&mut self, fd: OwnedFd, server: ServerConfig) {
        let packets = match bliss_tun::Packets::from_fd(fd) {
            Ok(packets) => packets,
            Err(e) => {
                self.fail(format!("Android дал устройство, которое не читается: {e}"));
                return;
            }
        };
        self.epoch += 1;
        self.state = State::Busy;
        self.step = 0;
        self.error = None;
        self.suggest = None;
        let epoch = self.epoch;
        let own = self.own.clone();
        let paths = self.paths.clone();
        let tracker = Arc::clone(&self.tracker);
        let router = Arc::new(self.router());
        let (port, door) = (self.bridge, self.door);
        let login = self.login.clone();
        let version = self.version;
        let names = Arc::clone(&self.names);
        let shut_out = shut_out_of(&self.paths.files);
        let link = self.current_link().unwrap_or_default();
        let keep = self.settings.auto_server;
        tracing::info!("tunnel: starting through «{}»", server.tag);
        // The engine takes the port in its turn; the stack and the check
        // through the server go on after it, so a stop asked for meanwhile
        // need not wait for them.
        self.port.queue(move |slot| async move {
            let started = spawn_engine(&server, &paths, (port, door), version, &login, &slot).await;
            tokio::spawn(async move {
                let result = match started {
                    Ok((socks, version)) => {
                        let up = Up {
                            keep,
                            link,
                            socks,
                            door,
                            login,
                            version,
                            tracker,
                            router,
                            packets,
                            names,
                            shut_out,
                        };
                        bring_up(&server, up, epoch, &own).await
                    }
                    Err(why) => Err(why),
                };
                if let Some(own) = own.upgrade() {
                    let _ = own.send(Cmd::Started(epoch, result));
                }
            });
        });
    }

    /// The link of the server a start or switch would go through now.
    fn current_link(&self) -> Option<String> {
        self.current()
            .map(|(index, _)| self.profile.servers[index].link.clone())
    }

    /// A server chosen while the tunnel was starting or switching: switch
    /// to it now. Before, the start went on with the old server while the
    /// screen and the notification named the new one.
    fn follow_choice(&mut self, running: &str) {
        if self.current_link().is_some_and(|link| link != running) {
            self.switch();
        }
    }

    /// Change the server under a tunnel that is up: the TUN and the stack
    /// stay, the engine is replaced on the same port.
    fn switch(&mut self) {
        let Some((_, server)) = self.current() else {
            return;
        };
        let Some(running) = self.running.as_ref() else {
            return;
        };
        let serving = Arc::clone(&running.serving);
        let link = self.current_link().unwrap_or_default();
        self.epoch += 1;
        self.state = State::Busy;
        self.step = 0;
        let epoch = self.epoch;
        let own = self.own.clone();
        let paths = self.paths.clone();
        let (port, door) = (self.bridge, self.door);
        let login = self.login.clone();
        let version = self.version;
        tracing::info!("tunnel: switching to «{}»", server.tag);
        self.port.queue(move |slot| async move {
            let started = spawn_engine(&server, &paths, (port, door), version, &login, &slot).await;
            tokio::spawn(async move {
                let result = match started {
                    Ok(_) => {
                        step(&own, epoch, 2);
                        check(&serving).await.map_err(|e| silent(&server, &e))
                    }
                    Err(why) => Err(why),
                };
                if let Some(own) = own.upgrade() {
                    let _ = own.send(Cmd::Switched(epoch, link, result));
                }
            });
        });
    }

    fn disconnect(&mut self) {
        self.epoch += 1;
        self.pending = None;
        if let Some(running) = self.running.take() {
            running.stack.abort();
            tracing::info!("tunnel: down");
        }
        // Always, not only with a tunnel up: a start under way leaves its
        // engine on the port.
        self.port.release();
        self.state = State::Off;
        self.error = None;
        self.suggest = None;
    }

    fn fail(&mut self, why: String) {
        tracing::warn!("tunnel: {why}");
        if let Some(running) = self.running.take() {
            running.stack.abort();
        }
        self.port.release();
        self.epoch += 1;
        self.state = State::Failed;
        self.error = Some(why);
        let current = self.current().map(|(index, _)| index);
        self.suggest = self
            .profile
            .servers
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != current && self.servers[*index].is_some())
            // A server a page loaded through first, by that time; then the
            // fastest handshake among servers never checked. One that failed
            // its check is not offered.
            .filter_map(|(index, entry)| match self.checks.get(&entry.link) {
                Some(Some(ms)) => Some(((0, *ms), index, *ms)),
                Some(None) => None,
                None => {
                    let ms = (*self.pings.get(&entry.link)?)?;
                    Some(((1, ms), index, ms))
                }
            })
            .min_by_key(|(rank, _, _)| *rank)
            .map(|(_, index, ms)| (index, ms))
            .map(|(index, ping)| Suggest {
                index,
                name: self.servers[index]
                    .as_ref()
                    .map(|s| s.tag.clone())
                    .unwrap_or_default(),
                ping,
            });
    }

    /// The place of the server a screen meant: the one at `index` when its
    /// key agrees (or none was sent), else wherever that key is now. A
    /// subscription refreshed between the frame the screen drew and the tap
    /// had moved the servers, and «Удалить» took the one that slid into the
    /// place. None, and a word, when the server is gone.
    fn place_of(&mut self, index: usize, key: Option<&str>) -> Option<usize> {
        let Some(key) = key else {
            return Some(index);
        };
        let at = |i: usize| {
            self.profile
                .servers
                .get(i)
                .map(|e| crate::protocol::server_key(&e.link))
        };
        if at(index).as_deref() == Some(key) {
            return Some(index);
        }
        let found = (0..self.profile.servers.len()).find(|&i| at(i).as_deref() == Some(key));
        if found.is_none() {
            self.toast("Этого сервера уже нет в списке", true, false);
        }
        found
    }

    /// «Автовыбор» on or off. On, with the tunnel up, it chooses at once.
    fn set_auto(&mut self, on: bool) {
        if self.settings.auto_server == on {
            return;
        }
        self.settings.auto_server = on;
        self.settings.save(&self.paths.files);
        if on && self.running.is_some() {
            self.tried.clear();
            if let Some(index) = self.auto_pick() {
                let link = self.profile.servers[index].link.clone();
                if self.selected.as_deref() != Some(link.as_str()) {
                    self.remember(link);
                    self.switch();
                }
            }
        }
    }

    /// The server «Автовыбор» would take now: one a page loaded through
    /// first, fastest first; then the fastest handshake; then one reached
    /// over UDP, which has no handshake to time; then one never measured.
    /// A server whose check failed, whose handshake found nobody, or that
    /// failed in this start (`tried`), only when nothing else is left —
    /// and then none, so a dead list ends in a failure said in words.
    fn auto_pick(&self) -> Option<usize> {
        let rank = |index: usize| -> Option<(u8, u32)> {
            let entry = &self.profile.servers[index];
            let server = self.servers[index].as_ref()?;
            if self.tried.contains(&entry.link) {
                return None;
            }
            match (self.checks.get(&entry.link), self.pings.get(&entry.link)) {
                (Some(Some(ms)), _) => Some((0, *ms)),
                (Some(None), _) => None,
                (None, Some(Some(ms))) => Some((1, *ms)),
                (None, _) if server.over_udp() => Some((2, 0)),
                (None, Some(None)) => None,
                (None, None) => Some((3, 0)),
            }
        };
        (0..self.profile.servers.len())
            .filter_map(|index| Some((rank(index)?, index)))
            .min()
            .map(|(_, index)| index)
    }

    /// The server under «Автовыбор» failed — at start, at a switch, or when
    /// a screen came on and nothing loaded through it: mark it, and go to the
    /// next one over the same TUN. With none left, the tunnel fails as it
    /// would by hand, and says why.
    fn fail_over(&mut self, link: &str, why: String) {
        tracing::info!("auto: a server failed ({why}); trying the next");
        self.tried.insert(link.to_owned());
        self.checks.insert(link.to_owned(), None);
        match self.auto_pick() {
            Some(index) if self.running.is_some() => {
                self.remember(self.profile.servers[index].link.clone());
                self.switch();
            }
            _ => {
                self.tried.clear();
                self.fail(format!("ни один сервер не ответил — последний: {why}"));
            }
        }
    }

    /// When a screen comes on with the tunnel up under «Автовыбор»: one small
    /// request through the server, and the next server if it does not answer.
    fn health(&mut self) {
        if !self.settings.auto_server || self.state != State::On {
            return;
        }
        let Some(running) = self.running.as_ref() else {
            return;
        };
        let serving = Arc::clone(&running.serving);
        let epoch = self.epoch;
        let own = self.own.clone();
        tokio::spawn(async move {
            let fine = check(&serving).await.is_ok();
            if let Some(own) = own.upgrade() {
                let _ = own.send(Cmd::Health(epoch, fine));
            }
        });
    }

    /// The server in use, kept on disk the moment it changes, however it
    /// changed — a tap, «Автовыбор», a failover, a network's rule — so the
    /// next start of the tunnel's process shows and takes the same one. It
    /// was written on a tap alone, and after «Автовыбор» had moved on, a
    /// restart came back on the server picked by hand days before.
    fn remember(&mut self, link: String) {
        let _ = std::fs::write(self.paths.selected(), &link);
        self.selected = Some(link);
    }

    fn select(&mut self, index: usize) {
        let Some(entry) = self.profile.servers.get(index) else {
            return;
        };
        if self.servers[index].is_none() {
            self.toast("Эта ссылка не читается", true, false);
            return;
        }
        self.remember(entry.link.clone());
        if self.running.is_some() {
            self.switch();
        } else if self.state == State::Failed {
            self.state = State::Off;
            self.error = None;
            self.suggest = None;
        }
    }

    // --- servers and subscriptions ------------------------------------------

    fn reparse(&mut self) {
        self.servers = self
            .profile
            .servers
            .iter()
            .map(|entry| entry.parse().ok())
            .collect();
        self.warnings = self.router().warnings;
    }

    fn import(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if text.starts_with("https://") || text.starts_with("http://") {
            let Ok(url) = reqwest::Url::parse(text.lines().next().unwrap_or(text).trim()) else {
                self.toast("Адрес подписки не читается", true, false);
                return;
            };
            if self
                .profile
                .subscriptions
                .iter()
                .any(|s| s.url == url.as_str())
            {
                self.toast("Эта подписка уже есть", false, false);
                return;
            }
            let id = format!("s{}", now_ms());
            let name = url.host_str().unwrap_or("подписка").to_owned();
            self.profile.subscriptions.push(Subscription {
                id: id.clone(),
                name,
                url: url.to_string(),
                updated: None,
                usage: None,
                renamed: false,
            });
            self.save();
            self.refresh(&id);
            return;
        }
        let found = subscription::links_in(text);
        let mut added = 0;
        let (mut known, mut full) = (0, false);
        let first = self.profile.servers.len();
        for link in found.links {
            if self.profile.servers.iter().any(|entry| entry.link == link) {
                known += 1;
                continue;
            }
            if MAX_PROFILE_SERVERS <= self.profile.servers.len() {
                full = true;
                break;
            }
            self.profile.servers.push(ServerEntry {
                link,
                name: None,
                group: None,
                favorite: false,
            });
            added += 1;
        }
        if added == 0 {
            // Said by what was found: a link already in the list used to
            // read as «no link here», which sent the person looking for one.
            let why = if full {
                format!("В списке уже {MAX_PROFILE_SERVERS} серверов — больше не поместится")
            } else if known == 1 {
                "Этот сервер уже есть в списке".to_owned()
            } else if known > 1 {
                "Все эти серверы уже есть в списке".to_owned()
            } else if found.foreign > 0 {
                "Такой протокол движок xray не знает".to_owned()
            } else if found.broken > 0 {
                "Ссылка не читается".to_owned()
            } else {
                "Здесь нет ссылки на сервер или подписку".to_owned()
            };
            self.toast(&why, known == 0 || full, false);
            return;
        }
        if self.selected.is_none() {
            // The first of them: it is the one on top of what was added.
            self.selected = self.profile.servers.get(first).map(|e| e.link.clone());
        }
        self.save();
        self.reparse();
        let text = if added == 1 {
            let name = self
                .servers
                .last()
                .and_then(Option::as_ref)
                .map(|s| s.tag.clone())
                .unwrap_or_default();
            format!("Добавлен «{name}»")
        } else {
            format!(
                "Добавлено {}",
                plural(added, "сервер", "сервера", "серверов")
            )
        };
        self.toast(&text, false, false);
        self.ping_all();
    }

    fn refresh(&mut self, id: &str) {
        let Some(sub) = self.profile.subscriptions.iter().find(|s| s.id == id) else {
            return;
        };
        if self.fetching.get(id).is_some_and(Option::is_none) {
            return;
        }
        self.fetching.insert(id.to_owned(), None);
        let (id, url) = (sub.id.clone(), sub.url.clone());
        let own = self.own.clone();
        tokio::spawn(async move {
            // Straight out: bliss's own app is left out of its VPN, so this is
            // the phone's own network whether the tunnel is up or not.
            let result = subscription::fetch(&url, None).await;
            if let Some(own) = own.upgrade() {
                let _ = own.send(Cmd::Fetched { id, url, result });
            }
        });
    }

    fn refresh_stale(&mut self) {
        let now = jiff::Timestamp::now().as_second();
        let stale: Vec<String> = self
            .profile
            .subscriptions
            .iter()
            .filter(|s| s.updated.is_none_or(|t| now - t > STALE))
            .map(|s| s.id.clone())
            .collect();
        for id in stale {
            self.refresh(&id);
        }
    }

    fn fetched(&mut self, id: String, url: String, result: Result<subscription::Fetched, String>) {
        let Some(position) = self.profile.subscriptions.iter().position(|s| s.id == id) else {
            self.fetching.remove(&id);
            return;
        };
        // A list from an address the subscription no longer has is dropped
        // (AGENTS.md §5).
        if self.profile.subscriptions[position].url != url {
            return;
        }
        match result {
            Err(why) => {
                self.fetching.insert(id, Some(why.clone()));
                self.toast(&format!("Подписка не обновилась: {why}"), true, false);
            }
            Ok(fetched) => {
                self.fetching.remove(&id);
                let sub = &mut self.profile.subscriptions[position];
                sub.updated = Some(jiff::Timestamp::now().as_second());
                sub.usage = fetched.usage;
                if !sub.renamed
                    && let Some(title) = fetched.title
                {
                    sub.name = title;
                }
                let name = sub.name.clone();
                self.profile
                    .servers
                    .retain(|entry| entry.group.as_deref() != Some(id.as_str()));
                let room = MAX_PROFILE_SERVERS.saturating_sub(self.profile.servers.len());
                let count = fetched.links.links.len().min(room);
                // Said, not dropped in silence: a provider's new servers in a
                // form bliss cannot read would otherwise never be missed.
                let skipped = fetched.links.foreign + fetched.links.broken;
                tracing::info!("subscription {name}: {count} server(s), {skipped} unreadable");
                self.profile
                    .servers
                    .extend(
                        fetched
                            .links
                            .links
                            .into_iter()
                            .take(room)
                            .map(|link| ServerEntry {
                                link,
                                name: None,
                                group: Some(id.clone()),
                                favorite: false,
                            }),
                    );
                if self.selected.is_none() {
                    self.selected = self.profile.servers.first().map(|e| e.link.clone());
                }
                self.save();
                self.reparse();
                let unread = if skipped > 0 {
                    format!(
                        "; ещё {} bliss прочитать не может",
                        plural(skipped as u64, "ссылку", "ссылки", "ссылок")
                    )
                } else {
                    String::new()
                };
                self.toast(
                    &format!(
                        "«{name}»: {}{unread}",
                        plural(count as u64, "сервер", "сервера", "серверов")
                    ),
                    false,
                    false,
                );
                self.ping_all();
            }
        }
    }

    fn delete(&mut self, index: usize) {
        if index >= self.profile.servers.len() {
            return;
        }
        let entry = self.profile.servers.remove(index);
        let name = self.servers[index]
            .as_ref()
            .map(|s| s.tag.clone())
            .unwrap_or_default();
        let was_current = self.selected.as_deref() == Some(entry.link.as_str());
        self.trash = Some(Trashed::Server(index, entry));
        if was_current {
            self.selected = None;
        }
        self.save();
        self.reparse();
        self.toast(&format!("«{name}» удалён"), false, true);
    }

    fn delete_group(&mut self, id: &str) {
        let Some(position) = self.profile.subscriptions.iter().position(|s| s.id == id) else {
            return;
        };
        let sub = self.profile.subscriptions.remove(position);
        let mut taken = Vec::new();
        let mut index = 0;
        self.profile.servers.retain(|entry| {
            let keep = entry.group.as_deref() != Some(id);
            if !keep {
                taken.push((index, entry.clone()));
            }
            index += 1;
            keep
        });
        let name = sub.name.clone();
        self.trash = Some(Trashed::Group(sub, taken));
        self.save();
        self.reparse();
        self.toast(&format!("Подписка «{name}» удалена"), false, true);
    }

    fn restore(&mut self) {
        match self.trash.take() {
            Some(Trashed::Server(index, entry)) => {
                let index = index.min(self.profile.servers.len());
                self.profile.servers.insert(index, entry);
            }
            Some(Trashed::Group(sub, entries)) => {
                self.profile.subscriptions.push(sub);
                for (index, entry) in entries {
                    let index = index.min(self.profile.servers.len());
                    self.profile.servers.insert(index, entry);
                }
            }
            Some(Trashed::Rule(index, rule)) => {
                let index = index.min(self.profile.routing.rules.len());
                self.profile.routing.rules.insert(index, rule);
                self.toast = None;
                self.commit_routing();
                return;
            }
            None => return,
        }
        self.toast = None;
        self.save();
        self.reparse();
    }

    fn ping_all(&mut self) {
        if self.probing || self.profile.servers.is_empty() {
            return;
        }
        self.probing = true;
        let targets: Vec<(String, Option<(String, u16)>)> = self
            .servers
            .iter()
            .zip(&self.profile.servers)
            .map(|(server, entry)| {
                let target = server.as_ref().filter(|s| !s.over_udp()).map(|s| {
                    (
                        s.address.to_string().trim_matches(['[', ']']).to_owned(),
                        s.port,
                    )
                });
                (entry.link.clone(), target)
            })
            .collect();
        let own = self.own.clone();
        tokio::spawn(async move {
            let results = futures_util::stream::iter(targets)
                .map(|(link, target)| async move {
                    let ms = match target {
                        Some((host, port)) => handshake(&host, port).await,
                        None => None,
                    };
                    (link, ms)
                })
                .buffer_unordered(16)
                .collect::<Vec<_>>()
                .await;
            if let Some(own) = own.upgrade() {
                let _ = own.send(Cmd::Pinged(results));
            }
        });
    }

    /// Real checks: one server, or all of them. Four engines at most at once —
    /// each is a process of its own — and only on a tap, never on a timer.
    fn check_servers(&mut self, index: Option<usize>) {
        let wanted: Vec<(usize, String, ServerConfig)> = self
            .profile
            .servers
            .iter()
            .zip(&self.servers)
            .enumerate()
            .filter(|(i, _)| index.is_none_or(|only| only == *i))
            .filter_map(|(i, (entry, server))| Some((i, entry.link.clone(), server.clone()?)))
            .filter(|(_, link, _)| !self.checking.contains(link))
            .collect();
        if wanted.is_empty() {
            return;
        }
        for (_, link, _) in &wanted {
            self.checking.insert(link.clone());
            self.checks.remove(link);
        }
        let own = self.own.clone();
        let paths = self.paths.clone();
        let known = self.version;
        tokio::spawn(async move {
            let version = match known {
                Some(version) => version,
                None => match bliss_xray::check_version(&paths.engine(), None).await {
                    Ok(version) => {
                        if let Some(own) = own.upgrade() {
                            let _ = own.send(Cmd::Version(version));
                        }
                        version
                    }
                    Err(e) => {
                        tracing::warn!("check: the engine did not answer — {e}");
                        for (_, link, _) in wanted {
                            if let Some(own) = own.upgrade() {
                                let _ = own.send(Cmd::Checked(link, None));
                            }
                        }
                        return;
                    }
                },
            };
            futures_util::stream::iter(wanted)
                .map(|(slot, link, server)| {
                    let paths = paths.clone();
                    let own = own.clone();
                    async move {
                        let ms = real_check(server, paths, version, slot).await;
                        if let Some(own) = own.upgrade() {
                            let _ = own.send(Cmd::Checked(link, ms));
                        }
                    }
                })
                .buffer_unordered(4)
                .collect::<Vec<()>>()
                .await;
        });
    }

    // --- rules ---------------------------------------------------------------

    fn router(&self) -> Router {
        let mut routing = self.profile.routing.clone();
        // IPv6 is not carried yet, and leaking it around the tunnel would be
        // worse than refusing it: refused at once, a program falls back to
        // IPv4 (docs/android-stack.md). Not saved: the profile stays the
        // person's own.
        routing.rules.push(Rule {
            name: Some("IPv6 — пока не через туннель".to_owned()),
            action: Action::Block,
            ip: vec!["::/0".to_owned()],
            ..Rule::default()
        });
        // The stack learns a site's address through the tunnel for rules that
        // look at addresses (`Carrying::decide`), so `geoip:` sees sites here.
        Router::build_resolving(&routing, &self.paths.files)
    }

    /// A rule from the editor. One condition that does not read refuses the
    /// whole rule, in the words rule_text.rs gives.
    fn save_rule(&mut self, index: Option<usize>, name: &str, action: &str, text: &str) {
        let rule = match rule_text::parse(name, crate::protocol::action_of(action), text) {
            Ok(rule) if rule.is_empty() => {
                self.toast("У правила нет ни одного условия", true, false);
                return;
            }
            Ok(rule) => rule,
            Err(why) => {
                self.toast(&why, true, false);
                return;
            }
        };
        let rules = &mut self.profile.routing.rules;
        match index {
            // The editor knows the rule's words, not its id or its switch.
            Some(at) => {
                let Some(old) = rules.get_mut(at) else {
                    self.toast("Этого правила уже нет", true, false);
                    return;
                };
                *old = Rule {
                    id: std::mem::take(&mut old.id),
                    enabled: old.enabled,
                    ..rule
                };
            }
            None => rules.push(rule),
        }
        self.commit_routing();
        self.toast("Правило сохранено", false, false);
    }

    fn delete_rule(&mut self, index: usize) {
        if index >= self.profile.routing.rules.len() {
            return;
        }
        let rule = self.profile.routing.rules.remove(index);
        self.profile.routing.forget(&rule.id);
        let name = rule.name.clone().unwrap_or_else(|| conditions(&rule));
        self.trash = Some(Trashed::Rule(index, rule));
        self.commit_routing();
        self.toast(&format!("«{name}» удалено"), false, true);
    }

    /// An app's own way, as the apps view sets it: its rule changed, made
    /// (first in the list, so it wins over the rules about sites), or, for
    /// none, taken away.
    fn app_route(&mut self, package: &str, label: &str, action: Option<&str>) {
        let package = package.trim();
        if package.is_empty() {
            return;
        }
        let rules = &mut self.profile.routing.rules;
        let at = rules.iter().position(|r| {
            crate::protocol::app_of(r).is_some_and(|p| p.eq_ignore_ascii_case(package))
        });
        match (at, action) {
            (Some(at), None) => {
                let rule = rules.remove(at);
                self.profile.routing.forget(&rule.id);
            }
            (Some(at), Some(action)) => {
                let rule = &mut rules[at];
                rule.action = crate::protocol::action_of(action);
                rule.enabled = true;
            }
            (None, Some(action)) => rules.insert(
                0,
                Rule {
                    name: Some(if label.trim().is_empty() {
                        package.to_owned()
                    } else {
                        label.trim().to_owned()
                    }),
                    action: crate::protocol::action_of(action),
                    process: vec![package.to_owned()],
                    ..Rule::default()
                },
            ),
            (None, None) => return,
        }
        self.commit_routing();
    }

    // --- rule sets -------------------------------------------------------------

    fn update_geo(&mut self) {
        if self.geo_fetch.is_some() {
            return;
        }
        self.geo_fetch = Some(0);
        self.geo_problem = None;
        let source = geo::source(&self.settings.geo_source);
        let files = self.paths.files.clone();
        // Through the tunnel when it is up: GitHub is slow from some networks.
        let via = self
            .running
            .as_ref()
            .map(|r| (r.socks, Some(self.login.clone())));
        let own = self.own.clone();
        tracing::info!("geo: fetching the rule sets from {}", source.key);
        tokio::spawn(async move {
            let progress = |bytes| {
                if let Some(own) = own.upgrade() {
                    let _ = own.send(Cmd::GeoProgress(bytes));
                }
            };
            let result = geo::update(source, &files, via, progress).await;
            if let Some(own) = own.upgrade() {
                let _ = own.send(Cmd::GeoDone(result));
            }
        });
    }

    /// Once they are here, the rule sets are kept a week old at most.
    fn refresh_geo_if_stale(&mut self) {
        let here = geo::size(&geo::site_path(&self.paths.files)).is_some();
        let now = jiff::Timestamp::now().as_second();
        let stale = self
            .settings
            .geo_updated
            .is_none_or(|t| now - t > geo::STALE);
        if here && stale && self.settings.geo_auto {
            self.update_geo();
        }
    }

    /// Rewrite 0.2.6's `protocol = ["quic"]` as UDP to port 443, and save,
    /// so the profile says what it means.
    fn settle_protocols(&mut self) {
        let rules = &mut self.profile.routing.rules;
        if rules.iter().any(|r| !r.protocol.is_empty()) {
            for rule in rules.iter_mut() {
                rule.settle_protocol();
            }
            self.save();
        }
    }

    /// Give a profile made before it the built-in «QUIC» rule, once: under
    /// «Локальная сеть» when that leads, else first, and switched as 0.2.5's
    /// switch stood. A rule of the person's own about QUIC is left alone.
    fn offer_quic_rule(&mut self) {
        if self.settings.quic_rule {
            return;
        }
        let rules = &mut self.profile.routing.rules;
        if !rules
            .iter()
            .any(|r| !r.network.is_empty() || !r.protocol.is_empty())
        {
            let at = usize::from(
                rules
                    .first()
                    .is_some_and(|r| r.name.as_deref() == Some(LOCAL_RULE)),
            );
            let mut rule = quic_rule();
            rule.enabled = self.settings.block_quic;
            rules.insert(at, rule);
            self.profile.routing.ensure_ids();
            self.save();
        }
        self.settings.quic_rule = true;
        self.settings.save(&self.paths.files);
    }

    fn commit_routing(&mut self) {
        self.profile.routing.ensure_ids();
        self.save();
        let router = Arc::new(self.router());
        self.warnings = router.warnings.clone();
        if let Some(running) = &self.running {
            running.serving.set_router(router);
        }
    }

    fn apply_preset(&mut self, name: &str) {
        let Some(preset) = self
            .profile
            .routing
            .presets
            .iter()
            .find(|p| p.name == name)
            .cloned()
        else {
            return;
        };
        for rule in &mut self.profile.routing.rules {
            if preset.on.contains(&rule.id) {
                rule.enabled = true;
            } else if preset.off.contains(&rule.id) {
                rule.enabled = false;
            }
        }
        self.profile.routing.fallback = preset.fallback;
        self.profile.routing.current_preset = Some(preset.name);
        self.commit_routing();
    }

    fn save_preset(&mut self) {
        self.profile.routing.ensure_ids();
        let routing = &mut self.profile.routing;
        let Some(name) = routing.current_preset.clone() else {
            return;
        };
        let (on, off): (Vec<&Rule>, Vec<&Rule>) = routing.rules.iter().partition(|r| r.enabled);
        let on = on.into_iter().map(|r| r.id.clone()).collect();
        let off = off.into_iter().map(|r| r.id.clone()).collect();
        let fallback = routing.fallback;
        if let Some(preset) = routing.presets.iter_mut().find(|p| p.name == name) {
            preset.on = on;
            preset.off = off;
            preset.fallback = fallback;
        }
        self.commit_routing();
    }

    fn drifted(&self) -> bool {
        let routing = &self.profile.routing;
        let Some(preset) = routing
            .current_preset
            .as_ref()
            .and_then(|name| routing.presets.iter().find(|p| &p.name == name))
        else {
            return false;
        };
        preset.fallback != routing.fallback
            || routing.rules.iter().any(|rule| {
                (rule.enabled && preset.off.contains(&rule.id))
                    || (!rule.enabled && preset.on.contains(&rule.id))
            })
    }

    // --- networks ------------------------------------------------------------

    /// The phone moved to another network, or the one it is on changed
    /// (a captive portal signed into). Its preset applies at once, said in
    /// a line; whether to connect is asked of the service through the frame.
    /// The same network reported again — Android calls back on every change
    /// of its capabilities — changes nothing, so a preset or the heart
    /// chosen by hand lasts until the network does.
    fn network_changed(&mut self, seen: Option<crate::networks::Seen>) {
        if seen == self.network {
            return;
        }
        let moved = seen.as_ref().map(|s| s.id()) != self.network.as_ref().map(|s| s.id());
        self.network = seen.clone();
        let Some(seen) = seen else {
            return;
        };
        let rule = crate::networks::rule_for(&self.settings.networks, &seen)
            .cloned()
            .unwrap_or_else(|| crate::networks::NetworkRule {
                id: seen.id(),
                connect: String::new(),
                preset: None,
                server: None,
            });
        if moved && let Some(server) = &rule.server {
            self.network_server(&seen, server);
        }
        if moved
            && let Some(preset) = &rule.preset
            && self.profile.routing.current_preset.as_ref() != Some(preset)
            && self
                .profile
                .routing
                .presets
                .iter()
                .any(|p| &p.name == preset)
        {
            self.apply_preset(preset);
            self.toast(
                &format!("{} — пресет «{preset}»", seen.name()),
                false,
                false,
            );
        }
        let wish = match &*rule.connect {
            "on" if seen.captive => {
                // A network that leads nowhere until signed into: wait for
                // it to be validated, which comes as another change.
                self.toast(
                    &format!("{} просит войти — подключусь после", seen.name()),
                    false,
                    false,
                );
                None
            }
            "on" if self.held_off => None,
            "on" => Some("on"),
            "off" => Some("off"),
            // A network that asks nothing, moved onto: a tunnel that waited
            // out the last one comes back (the service's `waitingOn`).
            _ if moved => Some("back"),
            _ => None,
        };
        if wish.is_some() {
            self.wish = wish;
            self.wishes += 1;
        }
    }

    /// The server a network asks for, taken as the phone moves onto it:
    /// «Автовыбор», or one server by its key, the way a tap on it in the
    /// list takes it — switching a tunnel that is up, said in a line. A
    /// server deleted since is passed over in silence: the network's rule
    /// still names it, and the sheet says it is gone.
    fn network_server(&mut self, seen: &crate::networks::Seen, server: &str) {
        if server == "auto" {
            if !self.settings.auto_server && self.profile.servers.len() > 1 {
                self.set_auto(true);
                self.toast(
                    &format!("{} — автовыбор сервера", seen.name()),
                    false,
                    false,
                );
            }
            return;
        }
        let Some(index) = self.index_of_key(server) else {
            return;
        };
        let link = &self.profile.servers[index].link;
        if !self.settings.auto_server && self.selected.as_deref() == Some(link.as_str()) {
            return;
        }
        let name = self.server_name(index);
        self.set_auto(false);
        self.select(index);
        self.toast(&format!("{} — сервер {name}", seen.name()), false, false);
    }

    /// Where the server with this key is in the list now.
    fn index_of_key(&self, key: &str) -> Option<usize> {
        self.profile
            .servers
            .iter()
            .position(|e| crate::protocol::server_key(&e.link) == key)
    }

    /// A server's name as the list shows it.
    fn server_name(&self, index: usize) -> String {
        self.servers
            .get(index)
            .and_then(|s| s.as_ref())
            .map(|s| s.tag.clone())
            .unwrap_or_else(|| "без имени".to_owned())
    }

    /// The words for the server a network rule names.
    fn rule_server_name(&self, server: Option<&str>) -> Option<String> {
        match server? {
            "auto" => Some("Автовыбор".to_owned()),
            key => self.index_of_key(key).map(|i| self.server_name(i)),
        }
    }

    /// A server's card: its fields, read back into the form, and its link.
    fn server_card(&mut self, index: usize) {
        let (Some(entry), Some(Some(server))) =
            (self.profile.servers.get(index), self.servers.get(index))
        else {
            self.toast("Эта ссылка не читается", true, false);
            return;
        };
        let form = bliss_proto::ServerForm::from_config(server);
        self.card = Some(crate::protocol::CardView {
            title: server.tag.clone(),
            rows: form.rows(),
            link: entry.link.clone(),
            key: Some(crate::protocol::server_key(&entry.link)),
            manual: entry.group.is_none(),
            form: Some(form),
        });
    }

    /// A subscription's card: its whole address, hidden until asked for.
    fn group_card(&mut self, id: &str) {
        let Some(sub) = self.profile.subscriptions.iter().find(|s| s.id == id) else {
            self.toast("Этой подписки уже нет", true, false);
            return;
        };
        let servers = self
            .profile
            .servers
            .iter()
            .filter(|e| e.group.as_deref() == Some(id))
            .count();
        let row = |label: &str, value: String, secret: bool| bliss_proto::DetailRow {
            label: label.to_owned(),
            value,
            secret,
        };
        self.card = Some(crate::protocol::CardView {
            title: sub.name.clone(),
            rows: vec![
                row("Сервер подписки", sub.shown_url(), false),
                row("Адрес целиком", sub.url.clone(), true),
                row("Серверов", servers.to_string(), false),
            ],
            link: sub.url.clone(),
            key: None,
            manual: false,
            form: None,
        });
    }

    /// A server from the form: added, or put in place of the one with `key`,
    /// which keeps its place, its star, the networks that name it and its
    /// being the server in use. One from a subscription is not changed: its
    /// next update would put it back.
    fn save_server(
        &mut self,
        key: Option<&str>,
        form: &bliss_proto::ServerForm,
    ) -> Result<String, String> {
        let link = form.to_link()?;
        let tag = link
            .parse::<ServerConfig>()
            .map(|c| c.tag)
            .unwrap_or_default();
        let at = match key {
            None => None,
            Some(key) => Some(
                self.profile
                    .servers
                    .iter()
                    .position(|e| crate::protocol::server_key(&e.link) == key)
                    .ok_or_else(|| "Этого сервера уже нет в списке".to_owned())?,
            ),
        };
        if self
            .profile
            .servers
            .iter()
            .enumerate()
            .any(|(i, e)| Some(i) != at && e.link == link)
        {
            return Err(format!("Такой сервер уже есть: «{tag}»"));
        }
        let Some(at) = at else {
            if MAX_PROFILE_SERVERS <= self.profile.servers.len() {
                return Err("Серверов уже слишком много".to_owned());
            }
            self.profile.servers.push(ServerEntry {
                link,
                name: None,
                group: None,
                favorite: false,
            });
            self.save();
            self.reparse();
            tracing::info!("server added by hand: {tag}");
            return Ok(format!("Сервер «{tag}» добавлен"));
        };
        if self.profile.servers[at].group.is_some() {
            return Err(
                "Сервер из подписки не изменить: обновление вернёт его. Сохраните копию".to_owned(),
            );
        }
        let old = std::mem::replace(&mut self.profile.servers[at].link, link.clone());
        self.profile.servers[at].name = None;
        let (old_key, new_key) = (
            crate::protocol::server_key(&old),
            crate::protocol::server_key(&link),
        );
        for favorite in &mut self.settings.favorites {
            if *favorite == old_key {
                favorite.clone_from(&new_key);
            }
        }
        for rule in &mut self.settings.networks {
            if rule.server.as_deref() == Some(old_key.as_str()) {
                rule.server = Some(new_key.clone());
            }
        }
        self.settings.save(&self.paths.files);
        let in_use = self.selected.as_deref() == Some(old.as_str());
        self.save();
        self.reparse();
        tracing::info!("server changed: {tag}");
        if in_use {
            // The server in use, changed: the tunnel takes it up again.
            self.select(at);
        }
        Ok(format!("Сервер «{tag}» сохранён"))
    }

    /// A star on a server or off it. The stars are kept by key, so a list
    /// refreshed under them keeps them; a key whose server is gone is
    /// dropped as the next star is set, and past `MAX_FAVORITES` the oldest.
    fn favorite(&mut self, key: String, on: bool) {
        let keys: std::collections::HashSet<String> = self
            .profile
            .servers
            .iter()
            .map(|e| crate::protocol::server_key(&e.link))
            .collect();
        let favorites = &mut self.settings.favorites;
        favorites.retain(|k| k != &key && keys.contains(k));
        if on && keys.contains(&key) {
            favorites.push(key);
        }
        while favorites.len() > MAX_FAVORITES {
            favorites.remove(0);
        }
        self.settings.save(&self.paths.files);
    }

    fn net_view(&self, seen: &crate::networks::Seen) -> crate::protocol::NetView {
        let rule = crate::networks::rule_for(&self.settings.networks, seen);
        let server = rule.and_then(|r| r.server.clone());
        crate::protocol::NetView {
            id: seen.id(),
            name: seen.name(),
            connect: rule.map(|r| r.connect.clone()).unwrap_or_default(),
            preset: rule.and_then(|r| r.preset.clone()),
            server_name: self.rule_server_name(server.as_deref()),
            server,
            own: rule.is_some_and(|r| r.id == seen.id()),
        }
    }

    // --- the journal, shared ----------------------------------------------------

    /// Write the journal for «Поделиться журналом»: a few lines on the state
    /// of things, then the log, with servers, subscriptions, links and keys
    /// taken out (report.rs). Into `journal.txt`, which the screens read and
    /// hand to Android's share sheet; the frame only says it is ready.
    fn share_log(&mut self) {
        let mut servers = Vec::new();
        for server in self.servers.iter().flatten() {
            servers.push(
                server
                    .address
                    .to_string()
                    .trim_matches(['[', ']'])
                    .to_owned(),
            );
            match &server.security {
                bliss_proto::Security::Tls(tls) => servers.extend(tls.sni.clone()),
                bliss_proto::Security::Reality(reality) => servers.push(reality.sni.clone()),
                bliss_proto::Security::None => {}
            }
        }
        let subscriptions: Vec<String> = self
            .profile
            .subscriptions
            .iter()
            .filter_map(|s| {
                reqwest::Url::parse(&s.url)
                    .ok()?
                    .host_str()
                    .map(str::to_owned)
            })
            .collect();
        let current = self
            .current()
            .map(|(_, s)| crate::protocol::kind(&s))
            .unwrap_or_default();
        let mut text = format!(
            "Состояние: {} · {}\nДвижок: {}\nСерверов: {}, подписок: {}, правил: {} (включено {}), \
             по умолчанию: {}\nНаборы правил: {}\n\n",
            state_word(self.state),
            if current.is_empty() {
                "сервер не выбран"
            } else {
                &current
            },
            self.version
                .map(|v| format!("xray {v}"))
                .unwrap_or_else(|| "ещё не спрошен".to_owned()),
            self.profile.servers.len(),
            self.profile.subscriptions.len(),
            self.profile.routing.rules.len(),
            self.profile
                .routing
                .rules
                .iter()
                .filter(|r| r.enabled)
                .count(),
            crate::protocol::action_key(self.profile.routing.fallback),
            geo::source(&self.settings.geo_source).key,
        );
        for line in crate::logs::lines() {
            text.push_str(&crate::report::redact(&line, &servers, &subscriptions));
            text.push('\n');
        }
        match std::fs::write(self.paths.files.join("journal.txt"), text) {
            Ok(()) => self.journals += 1,
            Err(e) => {
                tracing::warn!("engine: the journal was not written: {e}");
                self.toast("Журнал не записался", true, false);
            }
        }
    }

    // --- numbers -------------------------------------------------------------

    fn measure_now(&self) -> (Instant, u64, u64) {
        let (up, down, _) = self.tracker.totals();
        (Instant::now(), up, down)
    }

    fn sample(&mut self) {
        let now = self.measure_now();
        let elapsed = now.0.duration_since(self.last.0).as_secs_f64().max(0.001);
        let up = (now.1.saturating_sub(self.last.1) as f64 / elapsed) as u64;
        let down = (now.2.saturating_sub(self.last.2) as f64 / elapsed) as u64;
        self.last = now;
        self.rates = (down, up);
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(down);
    }

    // --- out -----------------------------------------------------------------

    fn toast(&mut self, text: &str, bad: bool, undo: bool) {
        self.toasts += 1;
        self.toast = Some(Toast {
            id: self.toasts,
            text: text.to_owned(),
            bad,
            undo,
        });
    }

    fn save(&mut self) {
        let path = self.paths.profile();
        let result = toml::to_string(&self.profile)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                let tmp = path.with_extension("toml.new");
                write_synced(&tmp, text.as_bytes())
                    .and_then(|()| std::fs::rename(&tmp, &path))
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = result {
            tracing::error!("engine: the profile was not saved: {e}");
        }
        if let Some(link) = &self.selected {
            let _ = std::fs::write(self.paths.selected(), link);
        } else {
            let _ = std::fs::remove_file(self.paths.selected());
        }
    }

    fn publish(&mut self) {
        let current = self.current().map(|(index, _)| index);
        let (up_total, down_total, opened) = {
            let (up, down, opened) = self.tracker.totals();
            let (up0, down0, opened0) = self.session;
            (
                up.saturating_sub(up0),
                down.saturating_sub(down0),
                opened.saturating_sub(opened0),
            )
        };
        let servers = self
            .profile
            .servers
            .iter()
            .zip(&self.servers)
            .map(|(entry, server)| {
                let ping = self.pings.get(&entry.link).copied();
                let key = crate::protocol::server_key(&entry.link);
                let (name, kind_, detail, address, udp) = match server {
                    Some(s) => (
                        s.tag.clone(),
                        kind(s),
                        how_it_hides(s),
                        format!("{}:{}", s.address, s.port),
                        s.over_udp(),
                    ),
                    None => (
                        "ссылка не читается".to_owned(),
                        String::new(),
                        String::new(),
                        String::new(),
                        false,
                    ),
                };
                let (ping, ms) = match (udp, ping, self.probing) {
                    (true, _, _) => ("udp", None),
                    (false, Some(Some(ms)), _) => ("ok", Some(ms)),
                    (false, Some(None), _) => ("down", None),
                    (false, None, true) => ("probing", None),
                    (false, None, false) => ("unknown", None),
                };
                ServerRow {
                    key: key.clone(),
                    name,
                    kind: kind_,
                    detail,
                    address,
                    group: entry.group.clone(),
                    ping,
                    ms,
                    check: if self.checking.contains(&entry.link) {
                        "probing"
                    } else {
                        match self.checks.get(&entry.link) {
                            Some(Some(_)) => "ok",
                            Some(None) => "down",
                            None => "unknown",
                        }
                    },
                    check_ms: self.checks.get(&entry.link).copied().flatten(),
                    favorite: self.settings.favorites.contains(&key),
                }
            })
            .collect();
        let groups = self
            .profile
            .subscriptions
            .iter()
            .map(|sub| GroupRow {
                id: sub.id.clone(),
                name: sub.name.clone(),
                host: shown_url(&sub.url),
                updated: sub.updated,
                used: sub.usage.as_ref().map(|u| u.upload + u.download),
                total: sub.usage.as_ref().map(|u| u.total).filter(|t| *t > 0),
                expire: sub.usage.as_ref().and_then(|u| u.expire),
                fetching: self.fetching.get(&sub.id).is_some_and(Option::is_none),
                problem: self.fetching.get(&sub.id).cloned().flatten(),
            })
            .collect();
        let rules = self
            .profile
            .routing
            .rules
            .iter()
            .map(|rule| RuleRow {
                name: rule.name.clone().unwrap_or_else(|| conditions(rule)),
                named: rule.name.is_some(),
                conditions: conditions(rule),
                text: rule_text::to_text(rule),
                app: crate::protocol::app_of(rule),
                action: action_key(rule.action),
                enabled: rule.enabled,
            })
            .collect();
        let table = self.table_open.then(|| {
            // Live connections first, then the closed ones, newest first: the
            // table's own order. Reversed, the screen showed the 200 oldest
            // of the closed ones and never a live connection.
            self.tracker
                .table()
                .into_iter()
                .take(200)
                .map(|row| ConnRow {
                    target: row.target,
                    app: row.process,
                    route: match row.route {
                        Route::Proxy => "proxy",
                        Route::Direct => "direct",
                        Route::Blocked => "block",
                        Route::Vpn => "vpn",
                    },
                    rule: row.rule,
                    up: row.up,
                    down: row.down,
                    closed: row.closed_at.is_some(),
                })
                .collect()
        });
        let frame = Frame {
            state: match self.state {
                State::Off => "off",
                State::Busy => "busy",
                State::On => "on",
                State::Failed => "failed",
            },
            step: self.step,
            error: self.error.clone(),
            suggest: self.suggest.clone(),
            since: self
                .running
                .as_ref()
                .filter(|_| self.state == State::On)
                .map(|r| r.since),
            current,
            auto: self.settings.auto_server,
            servers,
            groups,
            rules,
            fallback: action_key(self.profile.routing.fallback),
            presets: self
                .profile
                .routing
                .presets
                .iter()
                .map(|p| p.name.clone())
                .collect(),
            preset: self.profile.routing.current_preset.clone(),
            drifted: self.drifted(),
            warnings: self.warnings.clone(),
            numbers: Numbers {
                down_rate: self.rates.0,
                up_rate: self.rates.1,
                down_total,
                up_total,
                active: self.tracker.active_count(),
                opened,
                latency: self.running.as_ref().map(|r| r.latency),
            },
            history: self.history.iter().copied().collect(),
            table,
            log: self.log_open.then(crate::logs::lines),
            toast: self.toast.clone(),
            engine: self
                .version
                .map(|v| format!("xray {v}"))
                .unwrap_or_default(),
            can_restore: self.trash.is_some(),
            ticket: self.ticket,
            holding: self.running.is_some() || self.state == State::Busy,
            geo: GeoView {
                source: geo::source(&self.settings.geo_source).key.to_owned(),
                sources: geo::SOURCES
                    .iter()
                    .map(|s| GeoSource {
                        key: s.key,
                        name: s.name,
                        about: s.about,
                    })
                    .collect(),
                auto: self.settings.geo_auto,
                ip: geo::size(&geo::ip_path(&self.paths.files)),
                site: geo::size(&geo::site_path(&self.paths.files)),
                updated: self.settings.geo_updated,
                fetching: self.geo_fetch,
                problem: self.geo_problem.clone(),
            },
            categories: self.categories.clone(),
            journal: self.journals,
            network: self.network.as_ref().map(|seen| self.net_view(seen)),
            networks: self
                .settings
                .networks
                .iter()
                .map(|rule| crate::protocol::NetView {
                    id: rule.id.clone(),
                    name: net_name(&rule.id),
                    connect: rule.connect.clone(),
                    preset: rule.preset.clone(),
                    server: rule.server.clone(),
                    server_name: self.rule_server_name(rule.server.as_deref()),
                    own: true,
                })
                .collect(),
            wish: self.wish,
            wish_id: self.wishes,
            card: self.card.clone(),
            saved: self.saved.clone(),
        };
        let _ = self.frames.send(frame);
    }
}

/// A new profile: no servers, and the one rule every phone wants — its own
/// network straight, so a printer or a router's page is not sent abroad.
/// The first rule of a new profile.
const LOCAL_RULE: &str = "Локальная сеть";

/// The built-in rule against QUIC, the second of a new profile: QUIC is UDP
/// inside the tunnel, which Vision drops on 443 and other servers carry
/// badly, and a page waits on it — the user found parts of GitHub not
/// loading. Refused, the browser opens the page over TCP at once. A rule
/// above it decides first, so a site sent direct can keep its QUIC.
fn quic_rule() -> Rule {
    Rule {
        name: Some("QUIC — открывать по TCP".to_owned()),
        action: Action::Block,
        network: vec!["udp".to_owned()],
        port: vec!["443".to_owned()],
        ..Rule::default()
    }
}

fn first_profile() -> Profile {
    let mut profile = Profile::default();
    profile.routing.rules.push(Rule {
        name: Some(LOCAL_RULE.to_owned()),
        action: Action::Direct,
        ip: [
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "169.254.0.0/16",
            "fc00::/7",
            "fe80::/10",
        ]
        .map(str::to_owned)
        .to_vec(),
        ..Rule::default()
    });
    profile.routing.rules.push(quic_rule());
    profile.routing.ensure_ids();
    profile
}

/// scheme://host[:port] — never the path, where a subscription's token is.
fn shown_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(url) => match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => format!("{}://{host}:{port}", url.scheme()),
            (Some(host), None) => format!("{}://{host}", url.scheme()),
            _ => url.scheme().to_owned(),
        },
        Err(_) => String::new(),
    }
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn step(own: &mpsc::WeakUnboundedSender<Cmd>, epoch: u64, step: u8) {
    if let Some(own) = own.upgrade() {
        let _ = own.send(Cmd::Step(epoch, step));
    }
}

fn silent(server: &ServerConfig, e: &str) -> String {
    tracing::info!("tunnel: the check through «{}» failed — {e}", server.tag);
    format!("«{}» не ответил", server.tag)
}

/// Start xray for one server on the bridge port, once the engine before it
/// there is gone; answer its SOCKS address and its version. Called in the
/// port's turn (`Port::queue`).
async fn spawn_engine(
    server: &ServerConfig,
    paths: &Paths,
    (port, door): (u16, u16),
    version: Option<bliss_xray::Version>,
    login: &bliss_core::Login,
    slot: &Slot,
) -> Result<(SocketAddr, bliss_xray::Version), String> {
    clear(slot).await;
    let exe = paths.engine();
    let version = match version {
        Some(version) => version,
        None => bliss_xray::check_version(&exe, bliss_xray::minimum_xray_version(server))
            .await
            .map_err(|e| format!("движок не запустился: {e}"))?,
    };
    let mut config = bliss_xray::build(server, port, bliss_xray::LogLevel::Warning, version);
    bliss_xray::require_login(&mut config, &login.user, &login.pass);
    bliss_xray::datagrams_over_trojan(&mut config, door, &login.pass);
    let xray = bliss_xray::Xray::spawn(&exe, &config, &paths.work(), port)
        .await
        .map_err(|e| format!("движок не запустился: {e}"))?;
    let socks = xray.socks_addr();
    *slot.lock().expect("port lock") = Some(xray);
    Ok((socks, version))
}

/// What a start hands the stack once its engine is up.
#[cfg(target_os = "android")]
struct Up {
    /// «Автовыбор»: a failed check keeps the TUN (`Running::doubt`).
    keep: bool,
    link: String,
    socks: SocketAddr,
    /// The port of the Trojan inbound, on the same address.
    door: u16,
    login: bliss_core::Login,
    version: bliss_xray::Version,
    tracker: Arc<Tracker>,
    router: Arc<Router>,
    packets: bliss_tun::Packets,
    names: Arc<bliss_tun::names::FakeNames>,
    shut_out: bliss_tun::ShutOut,
}

/// The stack over an engine that is up, and the check through the server.
/// A check that fails leaves the engine on the port: the failure moves the
/// epoch, and the turn it queues takes the engine down.
#[cfg(target_os = "android")]
async fn bring_up(
    server: &ServerConfig,
    up: Up,
    epoch: u64,
    own: &mpsc::WeakUnboundedSender<Cmd>,
) -> Result<Running, String> {
    let Up {
        keep,
        link,
        socks,
        door,
        login,
        version,
        tracker,
        router,
        packets,
        names,
        shut_out,
    } = up;
    step(own, epoch, 1);
    let outbound = Arc::new(
        bliss_core::SocksOutbound::new(
            socks,
            bliss_core::EngineInfo {
                kind: "xray",
                version: version.to_string(),
                external: true,
            },
        )
        .with_login(login.clone())
        .with_datagrams(SocketAddr::new(socks.ip(), door), login.pass),
    );
    let serving = Arc::new(Serving::new(outbound, tracker, router));
    let mut carrying = bliss_tun::Carrying::new(Arc::clone(&serving), bliss_tun::Gateway);
    // The service names DNS_ADDRESS as the phone's DNS server, and answers
    // there are what lets domain and geosite rules see TUN connections.
    carrying.names = Some((DNS_ADDRESS, names));
    carrying.owner = Some(Arc::new(crate::android::owner_of));
    carrying.shut_out = Some(shut_out);
    let carrying = Arc::new(carrying);
    let ended = own.clone();
    let stack = tokio::spawn(async move {
        let why = match bliss_tun::serve(packets, carrying).await {
            Ok(()) => "the stack ended".to_owned(),
            Err(e) => e.to_string(),
        };
        if let Some(own) = ended.upgrade() {
            let _ = own.send(Cmd::StackEnded(epoch, why));
        }
    });
    step(own, epoch, 2);
    let (latency, doubt) = match check(&serving).await {
        Ok(latency) => (latency, None),
        Err(e) if keep => (0, Some(silent(server, &e))),
        Err(e) => {
            stack.abort();
            return Err(silent(server, &e));
        }
    };
    Ok(Running {
        link,
        socks,
        doubt,
        serving,
        stack,
        since: now_ms(),
        latency,
        version,
    })
}

/// One small request through the server, timed: the proof the tunnel leads
/// somewhere. xray answers a SOCKS CONNECT before it has reached the server
/// (AGENTS.md §10), so only an answer from the far side counts.
async fn check(serving: &Serving) -> Result<u32, String> {
    let target = Target::Domain {
        host: "www.gstatic.com".to_owned(),
        port: 80,
    };
    let attempt = async {
        let started = Instant::now();
        let mut stream = serving
            .dial_tunnelled(Route::Proxy, &target)
            .await
            .map_err(|e| e.to_string())?;
        stream
            .write_all(
                b"HEAD /generate_204 HTTP/1.1\r\nHost: www.gstatic.com\r\nConnection: close\r\n\r\n",
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut head = [0u8; 12];
        stream
            .read_exact(&mut head)
            .await
            .map_err(|e| e.to_string())?;
        if !head.starts_with(b"HTTP/") {
            return Err("the answer was not HTTP".to_owned());
        }
        Ok(started.elapsed().as_millis() as u32)
    };
    match tokio::time::timeout(CHECK_TIMEOUT, attempt).await {
        Ok(result) => result,
        Err(_) => Err(format!("no answer in {} s", CHECK_TIMEOUT.as_secs())),
    }
}

/// The real check of one server: its own xray, for this check alone, on a
/// free loopback port, and two requests through it on one connection — the
/// second timed, since the first pays for every handshake. A TCP ping says
/// the server's port answers; this says a page loads through it.
async fn real_check(
    server: ServerConfig,
    paths: Paths,
    version: bliss_xray::Version,
    slot: usize,
) -> Option<u32> {
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()?
        .local_addr()
        .ok()?
        .port();
    let work = paths.files.join("checks").join(slot.to_string());
    let login = fresh_login();
    let mut config = bliss_xray::build(&server, port, bliss_xray::LogLevel::None, version);
    bliss_xray::require_login(&mut config, &login.user, &login.pass);
    let xray = bliss_xray::Xray::spawn(&paths.engine(), &config, &work, port)
        .await
        .ok()?;
    let outbound = bliss_core::SocksOutbound::new(
        xray.socks_addr(),
        bliss_core::EngineInfo {
            kind: "xray",
            version: version.to_string(),
            external: true,
        },
    )
    .with_login(login);
    let timed = async {
        use bliss_core::Outbound;
        let mut stream = outbound.connect_tcp(&probe_target()).await.ok()?;
        http_head(&mut stream, true).await.ok()?;
        let started = Instant::now();
        http_head(&mut stream, false).await.ok()?;
        Some(started.elapsed().as_millis() as u32)
    };
    let result = tokio::time::timeout(CHECK_TIMEOUT, timed)
        .await
        .ok()
        .flatten();
    let _ = xray.shutdown().await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn probe_target() -> Target {
    Target::Domain {
        host: "www.gstatic.com".to_owned(),
        port: 80,
    }
}

/// One `HEAD /generate_204` and its answer's head, read to the blank line.
async fn http_head(stream: &mut bliss_core::BoxedStream, keep: bool) -> Result<(), String> {
    let request: &[u8] = if keep {
        b"HEAD /generate_204 HTTP/1.1\r\nHost: www.gstatic.com\r\n\r\n"
    } else {
        b"HEAD /generate_204 HTTP/1.1\r\nHost: www.gstatic.com\r\nConnection: close\r\n\r\n"
    };
    stream.write_all(request).await.map_err(|e| e.to_string())?;
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 4096 {
            return Err("an answer head past 4 KB".to_owned());
        }
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|e| e.to_string())?;
        head.push(byte[0]);
    }
    if head.starts_with(b"HTTP/") {
        Ok(())
    } else {
        Err("the answer was not HTTP".to_owned())
    }
}

/// A TCP handshake to the server, past the tunnel, timed: the list's ping.
async fn handshake(host: &str, port: u16) -> Option<u32> {
    let attempt = async {
        let addr: SocketAddr = tokio::net::lookup_host((host, port)).await.ok()?.next()?;
        let started = Instant::now();
        tokio::net::TcpStream::connect(addr).await.ok()?;
        Some(started.elapsed().as_millis() as u32)
    };
    tokio::time::timeout(Duration::from_secs(4), attempt)
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A screen is sent the whole frame once, then only the parts that
    /// changed: a tick of the numbers does not carry the server list again.
    #[test]
    fn a_screen_gets_only_what_changed() {
        let mut sent = Sent::default();
        let mut frame = Frame {
            state: "on",
            ..Frame::default()
        };
        let first: serde_json::Value = serde_json::from_str(&sent.patch(&frame).unwrap()).unwrap();
        assert!(first.get("servers").is_some() && first.get("numbers").is_some());
        assert_eq!(sent.patch(&frame), None, "nothing changed, nothing sent");
        frame.numbers.down_rate = 1234;
        let tick: serde_json::Value = serde_json::from_str(&sent.patch(&frame).unwrap()).unwrap();
        let keys: Vec<_> = tick.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["numbers".to_owned()]);
    }

    /// Work on the bridge port runs in the order the loop queued it, each
    /// turn after the one before has finished — however long that took. A
    /// stop's shutdown used to run beside the next start, which then found
    /// the port taken.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn turns_on_the_port_run_in_order() {
        let mut port = Port::default();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        for (turn, wait) in [(0, 60), (1, 0), (2, 30), (3, 0)] {
            let seen = Arc::clone(&seen);
            port.queue(move |_| async move {
                seen.lock().unwrap().push(("begin", turn));
                tokio::time::sleep(Duration::from_millis(wait)).await;
                seen.lock().unwrap().push(("end", turn));
            });
        }
        port.turn.take().unwrap().await.unwrap();
        let seen = seen.lock().unwrap().clone();
        let expected: Vec<_> = (0..4).flat_map(|t| [("begin", t), ("end", t)]).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_subscription_shows_its_host_and_not_its_token() {
        assert_eq!(
            shown_url("https://sub.example.net:2096/api/SECRETTOKEN?x=1"),
            "https://sub.example.net:2096"
        );
        assert_eq!(
            shown_url("https://sub.example.net/SECRET"),
            "https://sub.example.net"
        );
    }

    /// The real check through a server that is not there answers "no access",
    /// inside its time, and leaves no engine or folder behind. Needs an xray
    /// for this machine: `BLISS_XRAY=<path> cargo test -p bliss-android --
    /// --ignored`.
    #[tokio::test]
    #[ignore]
    async fn a_server_nobody_answers_has_no_access() {
        let xray = PathBuf::from(std::env::var("BLISS_XRAY").expect("BLISS_XRAY names an xray"));
        let dir = std::env::temp_dir().join(format!("bliss-android-check-{}", std::process::id()));
        let libs = dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        std::fs::copy(&xray, libs.join("libxray.so")).unwrap();
        let paths = Paths {
            files: dir.join("files"),
            libs,
            socket: dir.join("engine.sock"),
        };
        let version = bliss_xray::check_version(&paths.engine(), None)
            .await
            .unwrap();
        // TEST-NET-3: nothing answers there.
        let server: ServerConfig = "vless://11111111-2222-3333-4444-555555555555@203.0.113.7:443?security=reality&sni=www.example.net&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&type=tcp#Test"
            .parse()
            .unwrap();
        let started = Instant::now();
        let result = real_check(server, paths.clone(), version, 0).await;
        assert_eq!(result, None);
        assert!(started.elapsed() < CHECK_TIMEOUT + Duration::from_secs(5));
        assert!(!paths.files.join("checks").join("0").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_profile_keeps_the_local_network_direct() {
        let profile = first_profile();
        let router = Router::build(&profile.routing, Path::new("/nonexistent"));
        let decision = router.decide(&Target::Socket("192.168.1.1:80".parse().unwrap()), None);
        assert_eq!(decision.route, Route::Direct);
        let decision = router.decide(&Target::Socket("203.0.113.7:443".parse().unwrap()), None);
        assert_eq!(decision.route, Route::Proxy);
    }

    /// The engine's port answers only with the run's login: bliss's dial
    /// with it is taken, one without it — any other app on the phone — is
    /// turned away at the greeting. Needs an xray, like the check above.
    #[tokio::test]
    #[ignore]
    async fn the_engine_serves_only_who_has_the_login() {
        use bliss_core::Outbound;
        let xray = PathBuf::from(std::env::var("BLISS_XRAY").expect("BLISS_XRAY names an xray"));
        let dir = std::env::temp_dir().join(format!("bliss-android-login-{}", std::process::id()));
        let libs = dir.join("libs");
        std::fs::create_dir_all(&libs).unwrap();
        std::fs::copy(&xray, libs.join("libxray.so")).unwrap();
        let paths = Paths {
            files: dir.join("files"),
            libs,
            socket: dir.join("engine.sock"),
        };
        std::fs::create_dir_all(&paths.files).unwrap();
        let server: ServerConfig =
            "trojan://PPPP@203.0.113.7:443?security=tls&sni=www.example.net#t"
                .parse()
                .unwrap();
        let login = fresh_login();
        let slot = Slot::default();
        let port = free_port().unwrap();
        let door = free_port_besides(port);
        let (socks, version) = spawn_engine(&server, &paths, (port, door), None, &login, &slot)
            .await
            .expect("the engine starts with a login");
        let info = || bliss_core::EngineInfo {
            kind: "xray",
            version: version.to_string(),
            external: true,
        };
        let target = Target::Socket("203.0.113.9:80".parse().unwrap());
        // xray answers CONNECT before it reaches the server (AGENTS.md §10),
        // so a dial that gets past the greeting is a login taken.
        let with = bliss_core::SocksOutbound::new(socks, info()).with_login(login.clone());
        assert!(
            with.connect_tcp(&target).await.is_ok(),
            "the login was refused"
        );
        let wrong = bliss_core::Login {
            user: login.user.clone(),
            pass: "QQQQ".to_owned(),
        };
        let guessed = bliss_core::SocksOutbound::new(socks, info()).with_login(wrong);
        assert!(
            guessed.connect_tcp(&target).await.is_err(),
            "a wrong password was taken"
        );
        let without = bliss_core::SocksOutbound::new(socks, info());
        assert!(
            without.connect_tcp(&target).await.is_err(),
            "the port served a stranger"
        );
        clear(&slot).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rule made in the editor, as the user made it, against real rule
    /// sets (`BLISS_GEO_DIR`): `geosite:reddit` sends Reddit's names
    /// straight, through the phone's own table with its first rules.
    #[test]
    #[ignore]
    fn an_editor_rule_decides_by_real_rule_sets() {
        let geo = PathBuf::from(std::env::var("BLISS_GEO_DIR").expect("BLISS_GEO_DIR"));
        let dir = std::env::temp_dir().join(format!("bliss-android-geo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["geoip.dat", "geosite.dat"] {
            std::fs::copy(geo.join(name), dir.join(name)).unwrap();
        }
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        engine.ui(UiCommand::SaveRule {
            index: None,
            name: "Reddit".to_owned(),
            action: "direct".to_owned(),
            text: "geosite:reddit".to_owned(),
        });
        eprintln!(
            "rules: {:?}",
            engine
                .profile
                .routing
                .rules
                .iter()
                .map(|r| (&r.name, r.action, &r.geosite))
                .collect::<Vec<_>>()
        );
        eprintln!("warnings: {:?}", engine.warnings);
        let router = engine.router();
        for host in [
            "www.reddit.com",
            "reddit.com",
            "i.redd.it",
            "www.redditstatic.com",
            "gql.reddit.com",
            "v.redd.it",
            "preview.redd.it",
            "external-preview.redd.it",
            "styles.redditmedia.com",
        ] {
            let target = Target::Domain {
                host: host.to_owned(),
                port: 443,
            };
            let d = router.decide_with(&target, Some("151.101.1.140".parse().unwrap()), None);
            eprintln!("{host}: {:?} by {}", d.route, d.rule);
            assert_eq!(d.route, Route::Direct, "{host}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A profile from before the built-in «QUIC» rule gets it once, under
    /// «Локальная сеть», switched as 0.2.5's switch stood; deleted, it is not
    /// given back. A new profile starts with it on. And the editor's words
    /// for it read back into the same rule.
    #[test]
    fn the_quic_rule_is_offered_once() {
        let dir = std::env::temp_dir().join(format!("bliss-android-quic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            files: dir.clone(),
            libs: dir.clone(),
            socket: dir.join("s"),
        };
        // 0.2.5's profile — the local network rule and one of the user's —
        // with the switch turned off.
        let mut old = Profile::default();
        old.routing
            .rules
            .push(first_profile().routing.rules[0].clone());
        old.routing.rules.push(Rule {
            name: Some("Reddit".to_owned()),
            action: Action::Direct,
            geosite: vec!["reddit".to_owned()],
            ..Rule::default()
        });
        std::fs::write(paths.profile(), toml::to_string(&old).unwrap()).unwrap();
        std::fs::write(dir.join("android.json"), r#"{"block_quic":false}"#).unwrap();
        let load = || {
            let (tx, _rx) = mpsc::unbounded_channel();
            let (frames, _) = watch::channel(Frame::default());
            Engine::load(paths.clone(), tx.downgrade(), frames)
        };

        let mut engine = load();
        let rules = &engine.profile.routing.rules;
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].name.as_deref(), Some(LOCAL_RULE));
        assert_eq!(rules[1].network, ["udp"]);
        assert_eq!(rules[1].port, ["443"]);
        assert!(!rules[1].enabled, "as the switch stood");
        assert_eq!(rules[2].name.as_deref(), Some("Reddit"));

        // Deleted, and not given back on the next start.
        engine.ui(UiCommand::DeleteRule { index: 1 });
        drop(engine);
        let engine = load();
        assert!(
            engine
                .profile
                .routing
                .rules
                .iter()
                .all(|r| r.network.is_empty())
        );

        // A new profile has it, on.
        let new = first_profile();
        assert_eq!(new.routing.rules[1].network, ["udp"]);
        assert!(new.routing.rules[1].enabled);

        let typed = rule_text::parse("QUIC", Action::Block, "network:UDP\nport:443").unwrap();
        assert_eq!(
            (typed.network.as_slice(), typed.port.as_slice()),
            (&["udp".to_owned()][..], &["443".to_owned()][..])
        );
        assert_eq!(rule_text::to_text(&typed), "port:443\nnetwork:udp");
        assert!(rule_text::parse("x", Action::Block, "network:icmp").is_err());

        // 0.2.6 wrote protocol = ["quic"]: read, rewritten, saved.
        let dir2 = dir.join("026");
        std::fs::create_dir_all(&dir2).unwrap();
        let paths2 = Paths {
            files: dir2.clone(),
            libs: dir2.clone(),
            socket: dir2.join("s"),
        };
        std::fs::write(
            paths2.profile(),
            "[[routing.rule]]\nname = \"QUIC\"\naction = \"block\"\nprotocol = [\"quic\"]\n",
        )
        .unwrap();
        std::fs::write(dir2.join("android.json"), r#"{"quic_rule":true}"#).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let engine = Engine::load(paths2.clone(), tx.downgrade(), frames);
        let rule = &engine.profile.routing.rules[0];
        assert_eq!(
            (rule.network.as_slice(), rule.port.as_slice()),
            (&["udp".to_owned()][..], &["443".to_owned()][..])
        );
        let saved = std::fs::read_to_string(paths2.profile()).unwrap();
        assert!(!saved.contains("protocol"), "{saved}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A network's preset applies as the phone moves onto it, and what it
    /// asks of the service — connect, stay off — is asked once per move; the
    /// same network reported again changes nothing, and a Wi-Fi that wants
    /// a sign-in is connected only once it is signed into.
    #[tokio::test]
    async fn a_network_applies_its_preset_and_asks_once() {
        let dir = std::env::temp_dir().join(format!("bliss-android-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        let mut home = engine.profile.routing.snapshot("Дома");
        home.fallback = Action::Direct;
        engine.profile.routing.presets.push(home);
        engine.ui(UiCommand::SetNetwork {
            id: "wifi:Дом".to_owned(),
            connect: "off".to_owned(),
            preset: Some("Дома".to_owned()),
            server: None,
        });
        engine.ui(UiCommand::SetNetwork {
            id: "mobile".to_owned(),
            connect: "on".to_owned(),
            preset: None,
            server: None,
        });
        let seen = |kind: &str, ssid: Option<&str>, captive: bool| UiCommand::Network {
            kind: Some(kind.to_owned()),
            ssid: ssid.map(str::to_owned),
            captive,
        };
        engine.ui(seen("wifi", Some("Дом"), false));
        assert_eq!(
            engine.profile.routing.current_preset.as_deref(),
            Some("Дома")
        );
        assert_eq!(engine.profile.routing.fallback, Action::Direct);
        assert_eq!((engine.wish, engine.wishes), (Some("off"), 1));
        engine.ui(seen("wifi", Some("Дом"), false));
        assert_eq!(engine.wishes, 1, "the same network again asks nothing");
        engine.ui(seen("mobile", None, false));
        assert_eq!((engine.wish, engine.wishes), (Some("on"), 2));
        // A Wi-Fi of «любой Wi-Fi»'s rule that wants a sign-in first.
        engine.ui(UiCommand::SetNetwork {
            id: "wifi".to_owned(),
            connect: "on".to_owned(),
            preset: None,
            server: None,
        });
        engine.ui(seen("wifi", Some("Кафе"), true));
        assert_eq!(engine.wishes, 2, "not into a network that leads nowhere");
        engine.ui(seen("wifi", Some("Кафе"), false));
        assert_eq!((engine.wish, engine.wishes), (Some("on"), 3));
        // And the rules outlive the process.
        let saved = geo::Settings::load(&dir);
        assert_eq!(saved.networks.len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The server in use is on disk as soon as it changes, and the next
    /// start of the tunnel's process takes it up; deleted, it is forgotten.
    #[tokio::test]
    async fn the_server_in_use_outlives_a_restart() {
        let dir = std::env::temp_dir().join(format!("bliss-android-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = || Paths {
            files: dir.clone(),
            libs: dir.clone(),
            socket: dir.join("s"),
        };
        let load = || {
            let (tx, _rx) = mpsc::unbounded_channel();
            let (frames, _) = watch::channel(Frame::default());
            Engine::load(paths(), tx.downgrade(), frames)
        };
        let link = |n: u8| {
            format!("trojan://PPPP@203.0.113.{n}:443?security=tls&sni=www.example.net#s{n}")
        };
        let mut engine = load();
        engine.import(&[link(1), link(2), link(3)].join("\n"));
        engine.remember(link(3));
        drop(engine);
        let mut engine = load();
        assert_eq!(engine.current().map(|(i, _)| i), Some(2), "taken up again");
        engine.delete(2);
        drop(engine);
        let engine = load();
        assert_eq!(engine.current().map(|(i, _)| i), Some(0), "gone: the first");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Switched off by hand, bliss stays off whatever a network's rule says,
    /// until the person switches it on again; the service's own stops — a
    /// network's «Выключать», a pause — do not hold it off.
    #[tokio::test]
    async fn the_heart_switched_off_outweighs_a_networks_rule() {
        let dir = std::env::temp_dir().join(format!("bliss-android-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        for id in ["mobile", "wifi"] {
            engine.ui(UiCommand::SetNetwork {
                id: id.to_owned(),
                connect: "on".to_owned(),
                preset: None,
                server: None,
            });
        }
        let seen = |kind: &str| UiCommand::Network {
            kind: Some(kind.to_owned()),
            ssid: None,
            captive: false,
        };
        engine.ui(UiCommand::Disconnect { by_hand: true });
        engine.ui(seen("wifi"));
        engine.ui(seen("mobile"));
        assert_eq!(engine.wishes, 0, "off by hand: no rule switches it on");
        // The person switches it on: the rules speak again.
        let fd = std::os::fd::OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
        engine.obey(Cmd::Connect(fd, 1));
        engine.ui(seen("wifi"));
        assert_eq!((engine.wish, engine.wishes), (Some("on"), 1));
        // A stop of the service's own holds nothing off.
        engine.ui(UiCommand::Disconnect { by_hand: false });
        engine.ui(seen("mobile"));
        assert_eq!((engine.wish, engine.wishes), (Some("on"), 2));
        // And the screens' heart is by hand without saying so.
        let c: UiCommand = serde_json::from_str(r#"{"cmd":"disconnect"}"#).unwrap();
        assert!(matches!(c, UiCommand::Disconnect { by_hand: true }));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A paste says what it found: a server already listed is named as
    /// such, not as «no link here», and the first of new ones is chosen.
    #[tokio::test]
    async fn a_paste_says_what_it_found() {
        let dir = std::env::temp_dir().join(format!("bliss-android-paste-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        let link = |n: u8| {
            format!("trojan://PPPP@203.0.113.{n}:443?security=tls&sni=www.example.net#s{n}")
        };
        engine.import(&[link(1), link(2)].join("\n"));
        assert_eq!(
            engine.selected.as_deref(),
            Some(link(1).as_str()),
            "the first added"
        );
        let said = |e: &Engine| e.toast.as_ref().map(|t| (t.text.clone(), t.bad));
        engine.import(&link(1));
        assert_eq!(
            said(&engine),
            Some(("Этот сервер уже есть в списке".to_owned(), false))
        );
        engine.import(&[link(1), link(2)].join("\n"));
        assert_eq!(
            said(&engine).unwrap().0,
            "Все эти серверы уже есть в списке"
        );
        engine.import("просто текст");
        assert_eq!(
            said(&engine),
            Some(("Здесь нет ссылки на сервер или подписку".to_owned(), true))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A network can name a server: moving onto it takes that server (or
    /// «Автовыбор»), once per move, and a server deleted since is passed
    /// over. Stars follow a server by its key, outlive a refresh, are
    /// dropped with their server, and have a cap.
    #[tokio::test]
    async fn a_network_takes_its_server_and_stars_have_a_cap() {
        let dir = std::env::temp_dir().join(format!("bliss-android-star-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        // Held, so that a frame sent is a frame kept.
        let (frames, _seen) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        let link = |n: u8| {
            format!("trojan://PPPP@203.0.113.{n}:443?security=tls&sni=www.example.net#s{n}")
        };
        engine.import(&[link(1), link(2), link(3)].join("\n"));
        let key = |n: u8| crate::protocol::server_key(&link(n));
        engine.ui(UiCommand::Select {
            index: 0,
            key: Some(key(1)),
        });
        engine.ui(UiCommand::SetNetwork {
            id: "wifi:Дом".to_owned(),
            connect: String::new(),
            preset: None,
            server: Some(key(3)),
        });
        engine.ui(UiCommand::SetNetwork {
            id: "mobile".to_owned(),
            connect: String::new(),
            preset: None,
            server: Some("auto".to_owned()),
        });
        let seen = |kind: &str, ssid: Option<&str>| UiCommand::Network {
            kind: Some(kind.to_owned()),
            ssid: ssid.map(str::to_owned),
            captive: false,
        };
        engine.ui(seen("wifi", Some("Дом")));
        assert_eq!(engine.selected.as_deref(), Some(link(3).as_str()));
        assert!(!engine.settings.auto_server);
        // Picked by hand afterwards: the same network reported again keeps it.
        engine.ui(UiCommand::Select {
            index: 1,
            key: Some(key(2)),
        });
        engine.ui(seen("wifi", Some("Дом")));
        assert_eq!(engine.selected.as_deref(), Some(link(2).as_str()));
        engine.ui(seen("mobile", None));
        assert!(engine.settings.auto_server, "«Автовыбор» on mobile data");
        // A server deleted since: the rule still names it, and nothing moves.
        engine.ui(UiCommand::Auto { on: false });
        engine.delete(2);
        engine.ui(seen("wifi", Some("Дом")));
        assert_eq!(engine.selected.as_deref(), Some(link(2).as_str()));
        engine.publish();
        let net = engine
            .frames
            .borrow()
            .networks
            .iter()
            .find(|n| n.id == "wifi:Дом")
            .cloned();
        assert_eq!(
            net.and_then(|n| n.server_name),
            None,
            "the sheet says it is gone"
        );

        // Stars: by key, shown in the row, and gone with their server.
        engine.ui(UiCommand::Favorite {
            key: key(2),
            on: true,
        });
        engine.ui(UiCommand::Favorite {
            key: key(9),
            on: true,
        });
        assert_eq!(
            engine.settings.favorites,
            [key(2)],
            "only a server that is there"
        );
        engine.publish();
        let rows = engine.frames.borrow().servers.clone();
        assert!(
            rows.iter()
                .find(|r| r.key == key(2))
                .is_some_and(|r| r.favorite)
        );
        assert!(
            rows.iter()
                .find(|r| r.key == key(1))
                .is_some_and(|r| !r.favorite)
        );
        engine.ui(UiCommand::Favorite {
            key: key(2),
            on: false,
        });
        assert!(engine.settings.favorites.is_empty());
        let many: Vec<String> = (0..MAX_FAVORITES + 10)
            .map(|n| {
                format!(
                    "trojan://PPPP@10.{}.{}.1:443?security=tls&sni=a.example#m{n}",
                    n / 250,
                    n % 250
                )
            })
            .collect();
        engine.import(&many.join("\n"));
        for l in &many {
            engine.ui(UiCommand::Favorite {
                key: crate::protocol::server_key(l),
                on: true,
            });
        }
        assert_eq!(engine.settings.favorites.len(), MAX_FAVORITES);
        assert_eq!(
            engine.settings.favorites[0],
            crate::protocol::server_key(&many[10]),
            "the oldest go first"
        );
        assert_eq!(geo::Settings::load(&dir).favorites.len(), MAX_FAVORITES);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// «Автовыбор» takes a server a page loaded through first, then the
    /// fastest handshake, then one over UDP, then one never measured; never
    /// one whose check failed, and none it saw fail in this start. A
    /// server picked by hand turns it off.
    #[tokio::test]
    async fn auto_takes_the_best_that_answers_and_then_the_next() {
        let dir = std::env::temp_dir().join(format!("bliss-android-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        let link = |n: u8| {
            format!("trojan://PPPP@203.0.113.{n}:443?security=tls&sni=www.example.net#s{n}")
        };
        let udp = "hy2://PPPP@203.0.113.9:443/?sni=a.example#u".to_owned();
        engine.import(&[link(1), link(2), link(3), link(4), udp.clone()].join("\n"));
        assert_eq!(engine.profile.servers.len(), 5);
        engine.ui(UiCommand::Auto { on: true });
        assert!(engine.settings.auto_server);
        engine.checks.insert(link(1), Some(300));
        engine.pings.insert(link(2), Some(50));
        engine.checks.insert(link(3), None);
        engine.pings.insert(link(4), None);
        let picked = |e: &Engine| e.auto_pick().map(|i| e.profile.servers[i].link.clone());
        assert_eq!(picked(&engine), Some(link(1)), "a page loaded through it");
        engine.tried.insert(link(1));
        assert_eq!(picked(&engine), Some(link(2)), "then the fastest handshake");
        engine.tried.insert(link(2));
        assert_eq!(picked(&engine), Some(udp.clone()), "then one over UDP");
        engine.tried.insert(udp);
        assert_eq!(
            picked(&engine),
            None,
            "a failed check and a dead handshake are not offered"
        );
        // With nothing left, the tunnel fails in words.
        engine.fail_over(&link(2), "нет ответа".to_owned());
        assert_eq!(engine.state, State::Failed);
        assert!(engine.tried.is_empty());
        // A server picked by hand is the person's choice.
        engine.ui(UiCommand::Select {
            index: 1,
            key: None,
        });
        assert!(!engine.settings.auto_server);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The shared journal names no server: its address and the name it shows
    /// the network are taken out of every line, the engine's own included.
    #[tokio::test]
    async fn the_shared_journal_names_no_server() {
        let dir =
            std::env::temp_dir().join(format!("bliss-android-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        engine.import("vless://11111111-2222-3333-4444-555555555555@203.0.113.7:443?security=reality&sni=www.example.net&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&type=tcp#Test");
        crate::logs::push(
            "12:00:00 WARN xray: dial 203.0.113.7:443 for www.example.net failed".to_owned(),
        );
        crate::logs::push("12:00:01 INFO tun #3 www.google.com:443: closed".to_owned());
        engine.share_log();
        assert_eq!(engine.journals, 1);
        let text = std::fs::read_to_string(dir.join("journal.txt")).unwrap();
        assert!(
            !text.contains("203.0.113.7") && !text.contains("www.example.net"),
            "{text}"
        );
        assert!(!text.contains("11111111"), "{text}");
        assert!(text.contains("www.google.com:443"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A tap names the server the screen drew: after the list moved under
    /// it, by its key, never whatever slid into its place.
    #[tokio::test]
    async fn a_command_finds_its_server_after_the_list_moved() {
        let dir = std::env::temp_dir().join(format!("bliss-android-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        let link = |n: u8| {
            format!("trojan://PPPP@203.0.113.{n}:443?security=tls&sni=www.example.net#s{n}")
        };
        engine.import(&format!("{}\n{}\n{}", link(1), link(2), link(3)));
        assert_eq!(engine.profile.servers.len(), 3);
        let second = crate::protocol::server_key(&link(2));
        // The screen drew s2 at place 1; s1 went away in between.
        engine.profile.servers.remove(0);
        engine.reparse();
        engine.ui(UiCommand::Delete {
            index: 1,
            key: Some(second.clone()),
        });
        let left: Vec<_> = engine
            .profile
            .servers
            .iter()
            .map(|e| e.link.clone())
            .collect();
        assert_eq!(left, vec![link(3)]);
        // Gone altogether: nothing is deleted in its stead.
        engine.ui(UiCommand::Delete {
            index: 0,
            key: Some(second),
        });
        assert_eq!(engine.profile.servers.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A profile that does not read is copied aside before anything can
    /// write over it, and a second such start keeps the first copy.
    #[test]
    fn a_profile_that_does_not_read_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("bliss-android-bak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            files: dir.clone(),
            libs: dir.clone(),
            socket: dir.join("s"),
        };
        let broken = "[[servers]]\nlink = \"vless://unterminated\n";
        for _ in 0..2 {
            std::fs::write(paths.profile(), broken).unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let (frames, _) = watch::channel(Frame::default());
            let mut engine = Engine::load(paths.clone(), tx.downgrade(), frames);
            engine.save();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("bliss.toml.bak")).unwrap(),
            broken
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("bliss.toml.1.bak")).unwrap(),
            broken
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ipv6_is_refused_rather_than_leaked() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _) = watch::channel(Frame::default());
        let dir = std::env::temp_dir().join(format!("bliss-android-test-{}", std::process::id()));
        let engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        let router = engine.router();
        let v6 = Target::Socket("[2001:db8::1]:443".parse().unwrap());
        assert_eq!(router.decide(&v6, None).route, Route::Blocked);
        let local = Target::Socket("[fe80::1]:80".parse().unwrap());
        assert_eq!(router.decide(&local, None).route, Route::Direct);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A server written in the form is added; changed, it keeps its star,
    /// the network that names it and its being the server in use; one from a
    /// subscription is not changed; and its card holds its link.
    #[tokio::test]
    async fn a_server_by_hand_is_added_changed_and_shown() {
        let dir = std::env::temp_dir().join(format!("bliss-android-form-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (frames, _seen) = watch::channel(Frame::default());
        let mut engine = Engine::load(
            Paths {
                files: dir.clone(),
                libs: dir.clone(),
                socket: dir.join("s"),
            },
            tx.downgrade(),
            frames,
        );
        let form = bliss_proto::ServerForm {
            protocol: "trojan".to_owned(),
            address: "a.example".to_owned(),
            port: "443".to_owned(),
            password: "PPPP".to_owned(),
            security: "tls".to_owned(),
            ..bliss_proto::ServerForm::default()
        };
        engine.ui(UiCommand::SaveServer {
            key: None,
            form: Box::new(form.clone()),
            attempt: 1,
        });
        let saved = engine.saved.clone().unwrap();
        assert!(saved.ok, "{}", saved.text);
        assert_eq!(saved.attempt, 1);
        let link = engine.profile.servers[0].link.clone();
        assert!(link.starts_with("trojan://PPPP@a.example:443"), "{link}");
        // The same again is refused.
        engine.ui(UiCommand::SaveServer {
            key: None,
            form: Box::new(form.clone()),
            attempt: 2,
        });
        assert!(!engine.saved.clone().unwrap().ok);

        let key = crate::protocol::server_key(&link);
        engine.ui(UiCommand::Select {
            index: 0,
            key: Some(key.clone()),
        });
        engine.ui(UiCommand::Favorite {
            key: key.clone(),
            on: true,
        });
        engine.ui(UiCommand::SetNetwork {
            id: "wifi:Дом".to_owned(),
            connect: String::new(),
            preset: None,
            server: Some(key.clone()),
        });
        engine.ui(UiCommand::ServerCard {
            index: 0,
            key: Some(key.clone()),
        });
        let card = engine.card.clone().unwrap();
        assert_eq!(card.link, link);
        assert!(card.manual);
        let mut changed = card.form.unwrap();
        changed.port = "8443".to_owned();
        engine.ui(UiCommand::SaveServer {
            key: Some(key.clone()),
            form: Box::new(changed),
            attempt: 3,
        });
        assert!(engine.saved.clone().unwrap().ok);
        assert!(engine.card.is_none(), "the card closes on a save");
        let new_link = engine.profile.servers[0].link.clone();
        assert!(new_link.contains("a.example:8443"), "{new_link}");
        let new_key = crate::protocol::server_key(&new_link);
        assert_eq!(engine.settings.favorites, std::slice::from_ref(&new_key));
        assert_eq!(
            engine.settings.networks[0].server.as_deref(),
            Some(new_key.as_str())
        );
        assert_eq!(engine.selected.as_deref(), Some(new_link.as_str()));

        // One a subscription brought.
        engine.profile.servers[0].group = Some("s1".to_owned());
        let mut again = form.clone();
        again.port = "9443".to_owned();
        engine.ui(UiCommand::SaveServer {
            key: Some(new_key),
            form: Box::new(again),
            attempt: 4,
        });
        let refused = engine.saved.clone().unwrap();
        assert!(
            !refused.ok && refused.text.contains("подписки"),
            "{}",
            refused.text
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another user's app and an app put «мимо VPN» are shut out of the
    /// tunnel; everything else, and a list that is not there, is not.
    #[test]
    fn the_tunnel_carries_for_this_user_and_not_for_the_bypassed() {
        let dir = std::env::temp_dir().join(format!("bliss-shut-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let none = shut_out_of(&dir);
        assert!(!none("org.example.app"));
        assert!(
            none("foreign:1010123"),
            "another user's app, list or no list"
        );
        std::fs::write(
            dir.join("bypass.txt"),
            "org.example.bank\n\n  org.example.shop \n",
        )
        .unwrap();
        let shut_out = shut_out_of(&dir);
        assert!(shut_out("org.example.bank"));
        assert!(shut_out("org.example.shop"));
        assert!(!shut_out("org.example.app"));
        assert!(!shut_out(""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
