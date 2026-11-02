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
/// The rates kept for the chart: twenty seconds at one a second, and one past
/// each edge so the line can glide (AGENTS.md §6 rule 22).
const HISTORY: usize = 22;
/// Servers in a profile however they arrive: the desktop's cap
/// (`MAX_PROFILE_SERVERS` in crates/app/src/profile_file.rs).
const MAX_PROFILE_SERVERS: usize = 5_000;
/// A server list older than this is fetched again when the engine starts.
const STALE: i64 = 12 * 3600;

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
                        let Ok(mut text) = serde_json::to_string(&frame) else { continue };
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
    login: bliss_core::Login,
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
            login: fresh_login(),
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
        };
        engine.bridge = free_port().unwrap_or(engine.profile.engine.bridge_port);
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
                changed = watching.changed() => {
                    if changed.is_err() { break; }
                    let now = *watching.borrow_and_update();
                    if drawn == 0 && now > 0 {
                        // Someone opened a screen: fresh numbers, and pings if
                        // none were taken yet.
                        self.last = self.measure_now();
                        if self.pings.is_empty() { self.ping_all(); }
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
                    Ok(running) => {
                        tracing::info!("tunnel: up, {} ms through the server", running.latency);
                        self.version = Some(running.version);
                        let link = running.link.clone();
                        self.running = Some(running);
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
                        self.state = State::On;
                        self.error = None;
                        self.suggest = None;
                        self.follow_choice(&link);
                    }
                    (Ok(_), None) => self.port.release(),
                    (Err(why), _) => self.fail(why),
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
                for (link, ms) in results {
                    self.pings.insert(link, ms);
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
            UiCommand::Select { index } => self.select(index),
            UiCommand::Delete { index } => self.delete(index),
            UiCommand::DeleteGroup { id } => self.delete_group(&id),
            UiCommand::Restore => self.restore(),
            UiCommand::Ping => self.ping_all(),
            UiCommand::Check { index } => self.check_servers(index),
            UiCommand::Disconnect => self.disconnect(),
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
        let port = self.bridge;
        let login = self.login.clone();
        let version = self.version;
        let names = Arc::clone(&self.names);
        let link = self.current_link().unwrap_or_default();
        tracing::info!("tunnel: starting through «{}»", server.tag);
        // The engine takes the port in its turn; the stack and the check
        // through the server go on after it, so a stop asked for meanwhile
        // need not wait for them.
        self.port.queue(move |slot| async move {
            let started = spawn_engine(&server, &paths, port, version, &login, &slot).await;
            tokio::spawn(async move {
                let result = match started {
                    Ok((socks, version)) => {
                        let up = Up {
                            link,
                            socks,
                            login,
                            version,
                            tracker,
                            router,
                            packets,
                            names,
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
        let port = self.bridge;
        let login = self.login.clone();
        let version = self.version;
        tracing::info!("tunnel: switching to «{}»", server.tag);
        self.port.queue(move |slot| async move {
            let started = spawn_engine(&server, &paths, port, version, &login, &slot).await;
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

    fn select(&mut self, index: usize) {
        let Some(entry) = self.profile.servers.get(index) else {
            return;
        };
        if self.servers[index].is_none() {
            self.toast("Эта ссылка не читается", true, false);
            return;
        }
        self.selected = Some(entry.link.clone());
        let _ = std::fs::write(self.paths.selected(), &entry.link);
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
        for link in found.links {
            if MAX_PROFILE_SERVERS <= self.profile.servers.len() {
                break;
            }
            if self.profile.servers.iter().any(|entry| entry.link == link) {
                continue;
            }
            self.profile.servers.push(ServerEntry {
                link,
                name: None,
                group: None,
            });
            added += 1;
        }
        if added == 0 {
            let why = if found.foreign > 0 {
                "Такой протокол движок xray не знает"
            } else if found.broken > 0 {
                "Ссылка не читается"
            } else {
                "Здесь нет ссылки на сервер или подписку"
            };
            self.toast(why, true, false);
            return;
        }
        if self.selected.is_none() {
            self.selected = self.profile.servers.last().map(|e| e.link.clone());
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
                            }),
                    );
                if self.selected.is_none() {
                    self.selected = self.profile.servers.first().map(|e| e.link.clone());
                }
                self.save();
                self.reparse();
                self.toast(
                    &format!(
                        "«{name}»: {}",
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
        let via = self.running.as_ref().map(|r| (r.socks, self.login.clone()));
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
        }
    }

    fn publish(&mut self) {
        let current = self.current().map(|(index, _)| index);
        let (up_total, down_total, opened) = self.tracker.totals();
        let servers = self
            .profile
            .servers
            .iter()
            .zip(&self.servers)
            .map(|(entry, server)| {
                let ping = self.pings.get(&entry.link).copied();
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
    port: u16,
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
    link: String,
    socks: SocketAddr,
    login: bliss_core::Login,
    version: bliss_xray::Version,
    tracker: Arc<Tracker>,
    router: Arc<Router>,
    packets: bliss_tun::Packets,
    names: Arc<bliss_tun::names::FakeNames>,
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
        link,
        socks,
        login,
        version,
        tracker,
        router,
        packets,
        names,
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
        .with_login(login),
    );
    let serving = Arc::new(Serving::new(outbound, tracker, router));
    let mut carrying = bliss_tun::Carrying::new(Arc::clone(&serving), bliss_tun::Gateway);
    // The service names DNS_ADDRESS as the phone's DNS server, and answers
    // there are what lets domain and geosite rules see TUN connections.
    carrying.names = Some((DNS_ADDRESS, names));
    carrying.owner = Some(Arc::new(crate::android::owner_of));
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
    let latency = match check(&serving).await {
        Ok(latency) => latency,
        Err(e) => {
            stack.abort();
            return Err(silent(server, &e));
        }
    };
    Ok(Running {
        link,
        socks,
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
        let (socks, version) = spawn_engine(&server, &paths, port, None, &login, &slot)
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
}
