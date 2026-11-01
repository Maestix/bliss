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

use crate::protocol::{
    ConnRow, Frame, GroupRow, Numbers, RuleRow, ServerRow, Suggest, Toast, UiCommand, action_key,
    conditions, how_it_hides, kind,
};
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

/// What reaches the loop.
pub enum Cmd {
    /// The service established the TUN: its descriptor, and the ticket the
    /// service was given for it (see `Frame::ticket`).
    Connect(OwnedFd, u64),
    Ui(UiCommand),
    Step(u64, u8),
    Started(u64, Result<Running, String>),
    Switched(u64, Result<(bliss_xray::Xray, u32), String>),
    Pinged(Vec<(usize, Option<u32>)>),
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
}

/// A tunnel that is up: the engine, and the stack reading the TUN.
pub struct Running {
    xray: Option<bliss_xray::Xray>,
    serving: Arc<Serving>,
    stack: JoinHandle<()>,
    since: i64,
    latency: u32,
    version: bliss_xray::Version,
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
            continue;
        };
        let commands = commands.clone();
        let frames = frames.clone();
        let watchers = Arc::clone(&watchers);
        let watching = watching.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut frames = frames;
            let mut full = false;
            // The first frame goes out at once; then one per change.
            frames.mark_changed();
            loop {
                tokio::select! {
                    line = lines.next_line() => {
                        let Ok(Some(line)) = line else { break };
                        match serde_json::from_str::<UiCommand>(&line) {
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
    /// The descriptor of a TUN whose start is waiting for a server switch to
    /// end, or that arrived while busy.
    tracker: Arc<Tracker>,
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
                tracing::warn!(
                    "engine: the profile did not read, starting afresh: {}",
                    e.message()
                );
                first_profile()
            }),
            Err(_) => first_profile(),
        };
        let selected = std::fs::read_to_string(paths.selected()).ok();
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
            tracker: Arc::new(Tracker::new()),
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
        };
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
                    // A start nobody waits for any more: let it go.
                    if let Ok(running) = result {
                        stop_running(running);
                    }
                    return;
                }
                match result {
                    Ok(running) => {
                        tracing::info!("tunnel: up, {} ms through the server", running.latency);
                        self.version = Some(running.version);
                        self.running = Some(running);
                        self.state = State::On;
                        self.error = None;
                        self.suggest = None;
                    }
                    Err(why) => self.fail(why),
                }
            }
            Cmd::Switched(epoch, result) => {
                if epoch != self.epoch {
                    if let Ok((xray, _)) = result {
                        tokio::spawn(xray.shutdown());
                    }
                    return;
                }
                match (result, self.running.as_mut()) {
                    (Ok((xray, latency)), Some(running)) => {
                        running.xray = Some(xray);
                        running.latency = latency;
                        running.since = now_ms();
                        self.state = State::On;
                        self.error = None;
                        self.suggest = None;
                    }
                    (Ok((xray, _)), None) => {
                        tokio::spawn(xray.shutdown());
                    }
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
                for (index, ms) in results {
                    if let Some(entry) = self.profile.servers.get(index) {
                        self.pings.insert(entry.link.clone(), ms);
                    }
                }
            }
            Cmd::Fetched { id, url, result } => self.fetched(id, url, result),
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
            stop_running(running);
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
        let port = self.profile.engine.bridge_port;
        let version = self.version;
        tracing::info!("tunnel: starting through «{}»", server.tag);
        tokio::spawn(async move {
            let result = bring_up(
                server,
                paths,
                port,
                version,
                tracker,
                router,
                packets,
                epoch,
                own.clone(),
            )
            .await;
            if let Some(own) = own.upgrade() {
                let _ = own.send(Cmd::Started(epoch, result));
            }
        });
    }

    /// Change the server under a tunnel that is up: the TUN and the stack
    /// stay, the engine is replaced on the same port.
    fn switch(&mut self) {
        let Some((_, server)) = self.current() else {
            return;
        };
        let Some(running) = self.running.as_mut() else {
            return;
        };
        let old = running.xray.take();
        let serving = Arc::clone(&running.serving);
        self.epoch += 1;
        self.state = State::Busy;
        self.step = 0;
        let epoch = self.epoch;
        let own = self.own.clone();
        let paths = self.paths.clone();
        let port = self.profile.engine.bridge_port;
        let version = self.version;
        tracing::info!("tunnel: switching to «{}»", server.tag);
        tokio::spawn(async move {
            if let Some(old) = old {
                let _ = old.shutdown().await;
            }
            let result = async {
                let (xray, _) = spawn_engine(&server, &paths, port, version).await?;
                step(&own, epoch, 2);
                let latency = check(&serving).await.map_err(|e| silent(&server, &e))?;
                Ok((xray, latency))
            }
            .await;
            if let Some(own) = own.upgrade() {
                let _ = own.send(Cmd::Switched(epoch, result));
            }
        });
    }

    fn disconnect(&mut self) {
        self.epoch += 1;
        if let Some(running) = self.running.take() {
            stop_running(running);
            tracing::info!("tunnel: down");
        }
        self.state = State::Off;
        self.error = None;
        self.suggest = None;
    }

    fn fail(&mut self, why: String) {
        tracing::warn!("tunnel: {why}");
        if let Some(running) = self.running.take() {
            stop_running(running);
        }
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
        let targets: Vec<(usize, Option<(String, u16)>)> = self
            .servers
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let target = server.as_ref().filter(|s| !s.over_udp()).map(|s| {
                    (
                        s.address.to_string().trim_matches(['[', ']']).to_owned(),
                        s.port,
                    )
                });
                (index, target)
            })
            .collect();
        let own = self.own.clone();
        tokio::spawn(async move {
            let results = futures_util::stream::iter(targets)
                .map(|(index, target)| async move {
                    let ms = match target {
                        Some((host, port)) => handshake(&host, port).await,
                        None => None,
                    };
                    (index, ms)
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
        Router::build(&routing, &self.paths.files)
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
                std::fs::write(&tmp, text)
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
                conditions: conditions(rule),
                action: action_key(rule.action),
                enabled: rule.enabled,
            })
            .collect();
        let table = self.table_open.then(|| {
            self.tracker
                .table()
                .into_iter()
                .rev()
                .take(200)
                .map(|row| ConnRow {
                    target: row.target,
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
        };
        let _ = self.frames.send(frame);
    }
}

/// A new profile: no servers, and the one rule every phone wants — its own
/// network straight, so a printer or a router's page is not sent abroad.
fn first_profile() -> Profile {
    let mut profile = Profile::default();
    profile.routing.rules.push(Rule {
        name: Some("Локальная сеть".to_owned()),
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

fn stop_running(running: Running) {
    running.stack.abort();
    if let Some(xray) = running.xray {
        tokio::spawn(xray.shutdown());
    }
}

/// Start xray for one server; answer it with its version.
async fn spawn_engine(
    server: &ServerConfig,
    paths: &Paths,
    port: u16,
    version: Option<bliss_xray::Version>,
) -> Result<(bliss_xray::Xray, bliss_xray::Version), String> {
    let exe = paths.engine();
    let version = match version {
        Some(version) => version,
        None => bliss_xray::check_version(&exe, bliss_xray::minimum_xray_version(server))
            .await
            .map_err(|e| format!("движок не запустился: {e}"))?,
    };
    let config = bliss_xray::build(server, port, bliss_xray::LogLevel::Warning, version);
    let xray = bliss_xray::Xray::spawn(&exe, &config, &paths.work(), port)
        .await
        .map_err(|e| format!("движок не запустился: {e}"))?;
    Ok((xray, version))
}

#[cfg(target_os = "android")]
#[allow(clippy::too_many_arguments)]
async fn bring_up(
    server: ServerConfig,
    paths: Paths,
    port: u16,
    version: Option<bliss_xray::Version>,
    tracker: Arc<Tracker>,
    router: Arc<Router>,
    packets: bliss_tun::Packets,
    epoch: u64,
    own: mpsc::WeakUnboundedSender<Cmd>,
) -> Result<Running, String> {
    let (xray, version) = spawn_engine(&server, &paths, port, version).await?;
    step(&own, epoch, 1);
    let outbound = Arc::new(bliss_core::SocksOutbound::new(
        xray.socks_addr(),
        bliss_core::EngineInfo {
            kind: "xray",
            version: version.to_string(),
            external: true,
        },
    ));
    let serving = Arc::new(Serving::new(outbound, tracker, router));
    let carrying = Arc::new(bliss_tun::Carrying {
        serving: Arc::clone(&serving),
        gateway: bliss_tun::Gateway,
    });
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
    step(&own, epoch, 2);
    let latency = match check(&serving).await {
        Ok(latency) => latency,
        Err(e) => {
            stack.abort();
            let _ = xray.shutdown().await;
            return Err(silent(&server, &e));
        }
    };
    Ok(Running {
        xray: Some(xray),
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
    let config = bliss_xray::build(&server, port, bliss_xray::LogLevel::None, version);
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
    );
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
