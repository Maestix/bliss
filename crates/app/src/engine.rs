//! Starting and stopping the tunnel, and publishing what it is doing.
//!
//! Both the CLI and the window drive this. The window never touches tokio: the
//! engine lives on its own runtime thread and pushes a [`Frame`] into a watch
//! channel a few times a second, which the UI awaits. A watch notification
//! needs no timer and no reactor, so the UI works whatever executor renders it
//! — which also means swapping WebView2 for Blitz changes nothing here.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bliss_core::{EngineInfo, Inbound, Router, Serving, SocksOutbound, Tracker};
use bliss_proto::{
    Action, Flow, Profile, Protocol, ProxyMode, Rule, Security, ServerConfig, TcpHeader, Transport,
    Usage,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::profile_file::{Store, Subject};
use crate::ui::plural;

/// How often the UI gets a new frame. Fast enough that a speed readout looks
/// live, slow enough that it costs nothing.
pub(crate) const TICK: Duration = Duration::from_millis(500);

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

/// What a server's response column is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ping {
    /// Never measured.
    #[default]
    Unknown,
    /// A probe is in flight.
    Probing,
    /// TCP handshake, in milliseconds.
    Ok(u32),
    /// Refused, or nothing inside the timeout.
    Down,
    /// Reached over UDP — Hysteria 2, WireGuard, mKCP — where a TCP handshake
    /// measures nothing: such a server would read as down when it is not.
    Udp,
}

/// One row of the server list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerRow {
    pub tag: String,
    pub addr: String,
    /// What it is, briefly: `VLESS · REALITY`, `Trojan · TLS`, `WireGuard`.
    pub kind: String,
    /// And in full, for the pointer: `VLESS · xhttp · reality · vision`.
    pub detail: String,
    pub ping: Ping,
    /// True for the one the engine is set to use.
    pub current: bool,
    /// The subscription it came from, by name; `None` for one added by hand.
    pub group: Option<String>,
    /// The same subscription by id, which two subscriptions cannot share.
    pub group_id: Option<String>,
}

/// One rule, as the routing screen shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleView {
    /// Its place in the order, from 1. The order is the rule: first match wins,
    /// so a screen that hid it would hide how the thing works.
    pub position: usize,
    pub name: String,
    pub action: Action,
    /// What it matches, in the words the profile used.
    pub conditions: Vec<String>,
    /// Off, it keeps its row and decides nothing.
    pub enabled: bool,
}

/// The rule table, for the screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingView {
    pub fallback: Action,
    pub rules: Vec<RuleView>,
    /// The same rules as the profile holds them, for the editor to start from.
    pub source: Vec<Rule>,
    /// Rules that cannot work, said out loud. See `Router::warnings`.
    pub warnings: Vec<String>,
    /// The presets, by name, in the order they were made.
    pub presets: Vec<String>,
    /// The one applied or saved last.
    pub current_preset: Option<String>,
    /// A switch, or where everything else goes, has moved since.
    pub drifted: bool,
}

/// The answer to "what happens to this destination?"
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    pub target: String,
    pub action: Action,
    pub rule: String,
    /// Why a rule with a country did not decide a site, when one did not:
    /// `Router::geoip_note`.
    pub note: Option<String>,
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
    /// Every server in the profile, in the order it is written there.
    pub servers: Vec<ServerRow>,
    /// What bliss is set to do with the machine's proxy settings.
    pub proxy_mode: ProxyMode,
    /// Whether those settings currently point at us. Distinct from the mode:
    /// "set" with the tunnel down deliberately writes nothing, because a proxy
    /// aimed at a port nobody is listening on takes the machine offline.
    pub proxy_applied: bool,
    /// The rule table as the routing screen shows it.
    pub routing: RoutingView,
    /// The last destination someone checked, and what the rules do with it.
    pub check: Option<CheckResult>,
    /// Bytes per second over the last tick.
    pub up_rate: u64,
    pub down_rate: u64,
    /// Newest first, live connections before closed ones.
    pub connections: Vec<bliss_core::Snapshot>,
    /// Recent download rates, oldest first, for the chart: `CHART_SAMPLES` of
    /// them, the first and the last outside it for most of a tick.
    pub history: Vec<u64>,
    /// How many times `history` has moved on by a sample. The chart glides one
    /// sample left over a tick, and starts the glide again when this changes.
    pub samples: u64,
    /// What the chart's full height means, in bytes per second. Computed here
    /// rather than in the UI so it can be smoothed across ticks.
    pub scale: u64,
    /// What became of the last pasted link.
    pub added: Option<Added>,
    /// Every subscription, in profile order.
    pub subscriptions: Vec<SubscriptionRow>,
    /// A line about what the last action did, on any screen, for a few seconds.
    pub toast: Option<Toast>,
    /// Whether Windows starts bliss at sign-in; the settings screen's first card.
    pub startup: crate::startup::Startup,
    /// The TUN adapter: asked for, allowed, and whether it is carrying.
    pub tun: TunView,
}

/// What the settings screen says about carrying the machine's traffic.
///
/// Three separate facts, because they come apart: the profile can ask for the
/// adapter, the copy of bliss reading it can be unable to create one, and a
/// created one can still have failed. The screen shows a different line for
/// each, and a switch that silently did nothing would be the worst of the
/// three.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TunView {
    /// What the profile asks for.
    pub on: bool,
    /// Whether this copy has the rights to create an adapter.
    pub elevated: bool,
    /// Whether one is up and carrying traffic right now.
    pub carrying: bool,
    /// Why it is not, when it is asked for and is not carrying.
    pub problem: Option<String>,
}

/// One subscription, as its screen shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionRow {
    pub id: String,
    pub name: String,
    /// The address with the token left out.
    pub shown_url: String,
    pub servers: usize,
    /// Unix seconds.
    pub updated: Option<i64>,
    pub usage: Option<Usage>,
    pub fetching: bool,
    /// Why the last fetch failed, until one works.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    /// Counts up, so the window replays its fade for a new line.
    pub id: u64,
    pub text: String,
    pub bad: bool,
}

/// A finished fetch, on its way back to the engine loop.
///
/// Its links are credentials, and `Command` is `Debug`: this shows the id alone.
pub struct FetchDone {
    id: String,
    result: Result<crate::subscription::Fetched, String>,
    /// Asked for in the window, rather than a background refresh: only then is
    /// the outcome worth interrupting anyone with.
    asked: bool,
}

impl std::fmt::Debug for FetchDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchDone")
            .field("id", &self.id)
            .field("ok", &self.result.is_ok())
            .finish()
    }
}

/// The answer to "add this server".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Added {
    /// Counts up with every attempt, so a form can tell a fresh answer from one
    /// it has already acted on — two identical refusals in a row are two
    /// answers, and would otherwise look like one.
    pub attempt: u64,
    /// The new server's name, or why it was not added.
    pub outcome: Result<String, String>,
}

/// A pasted server link on its way to the profile.
///
/// It carries the user's UUID and keys, so its `Debug` shows none of it:
/// `Command` is `Debug`, and one `{command:?}` in a log line would otherwise put
/// a credential in bliss.log.
pub struct Link(pub String);

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Link(..)")
    }
}

#[derive(Debug)]
pub enum Command {
    // Start and Stop exist for the tray menu, which wants the explicit verbs
    // rather than a toggle whose meaning depends on state the menu cannot see.
    #[allow(dead_code, reason = "the tray menu lands in 1.8")]
    Start,
    #[allow(dead_code, reason = "the tray menu lands in 1.8")]
    Stop,
    /// The switch was clicked, carrying what the window was showing at the time.
    ///
    /// The state travels with the click because the engine may be busy starting
    /// when it arrives, and starting is not interruptible: the command waits in
    /// the queue until the start finishes, by which point the state it would
    /// have been read against is gone. Deciding from `running.is_none()` at
    /// that moment turned "cancel this" into "start another one", and a second
    /// impatient click into a third attempt.
    Toggle {
        was_on: bool,
    },
    /// Tear the tunnel down and bring it back on the same server.
    Restart,
    /// Use the server at this index from now on, restarting if the tunnel is up.
    Select(usize),
    /// Measure every server's handshake time.
    PingAll,
    /// Change what bliss does with the machine's proxy settings.
    Proxy(ProxyMode),
    /// Ask the rules about one destination. The answer lands in the frame.
    Check(String),
    /// Put the starting set of rules in a profile that has none, and save it.
    SeedRules,
    /// Pasted text: a server link, several, a base64 list, or the address of a
    /// subscription. `from_form` answers in the form's `added`; the hotkey,
    /// which has no form, gets a toast.
    Import {
        text: Link,
        from_form: bool,
    },
    /// Put a rule in place of the one at `at`, or after the others.
    SaveRule {
        at: Option<usize>,
        rule: Rule,
    },
    DeleteRule(usize),
    MoveRule {
        from: usize,
        to: usize,
    },
    /// What happens to a connection no rule matched.
    Fallback(Action),
    /// Switch the rule at `at` on or off.
    SwitchRule {
        at: usize,
        on: bool,
    },
    /// Put every switch where a preset has it.
    ApplyPreset(String),
    /// Keep the switches as they stand under a name, new or known.
    SavePreset(String),
    RenamePreset {
        from: String,
        to: String,
    },
    DeletePreset(String),
    /// Send one program, site or address somewhere, from a row of the
    /// connections table, which is `target` from `process`.
    RouteOne {
        subject: Subject,
        action: Action,
        target: String,
        process: Option<String>,
    },
    /// Fetch one subscription again, or every one.
    Refresh(Option<String>),
    DeleteSubscription(String),
    /// Remove the server at this place in the list. Its tag travels with the
    /// index, so a list that changed in the meantime is noticed rather than
    /// the server that moved into that place deleted.
    DeleteServer {
        index: usize,
        tag: String,
    },
    /// A fetch finished. Only the engine sends this, to itself.
    Fetched(FetchDone),
    /// Show one of bliss's own folders to the user.
    Reveal(Reveal),
    /// Have Windows start this bliss at sign-in, minimized or not, or stop.
    Startup {
        on: bool,
        minimized: bool,
    },
    /// Read the startup entry again: Task Manager, or another copy of bliss,
    /// may have changed it since.
    ReadStartup,
    /// Carry the machine's whole traffic through a TUN adapter, or stop.
    ///
    /// Remembered in the profile, and applied to a tunnel that is already up
    /// without restarting it: the adapter is another way in, and the
    /// connections going through the two ports have no reason to be dropped
    /// for it.
    Tun(bool),
    /// An adapter finished coming up. Only the engine sends this, to itself.
    TunUp(Box<TunRaised>),
}

/// An adapter on its way back into the loop, and the tunnel it was built for.
pub struct TunRaised {
    /// A restart between the click and the adapter coming up leaves this
    /// pointing at a `Serving` nobody serves any more; an adapter carrying
    /// traffic into one would be a hole with no bottom, so it is compared
    /// before the adapter is kept.
    serving: Arc<Serving>,
    result: Result<bliss_tun::Tun, String>,
}

/// `Command` is `Debug` because a refused link must not be printable; an
/// adapter has nothing to say here beyond whether it arrived.
impl std::fmt::Debug for TunRaised {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.result {
            Ok(_) => f.write_str("TunRaised(up)"),
            Err(e) => write!(f, "TunRaised({e})"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Reveal {
    /// Where the log, the engine and its generated config live.
    WorkDir,
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
    /// Every server that parsed, in profile order. Empty in a window that has
    /// none yet, which is why every look into it goes through `server`.
    pub servers: Vec<ServerConfig>,
    /// Which of them to use. An index rather than a clone, so switching is a
    /// number and the list stays the single copy of the truth.
    pub selected: usize,
    pub work_dir: PathBuf,
    /// Where the profile is saved. There is no file until the first change
    /// made in the window.
    pub profile_path: PathBuf,
    pub engine_log: bliss_xray::LogLevel,
    pub autostart: bool,
    /// Carry the machine's traffic through a TUN adapter as well as the two
    /// local ports. Needs administrator rights, so it is asked for on the
    /// command line while it is being proved out.
    pub tun: bool,
    /// Why the profile on disk could not be used, for the window to say.
    pub problem: Option<String>,
}

impl Setup {
    fn server(&self, index: usize) -> Option<&ServerConfig> {
        self.servers.get(index)
    }
}

/// Where the engine binary is: the profile's choice, then the copy bliss manages.
///
/// Looked up at every start rather than once at launch, so the window opens
/// without an engine, and one installed while it is open is simply found.
pub fn engine_path(setup: &Setup) -> Result<PathBuf, String> {
    if let Some(path) = &setup.profile.engine.xray {
        return if path.is_file() {
            Ok(path.clone())
        } else {
            Err(format!(
                "в профиле [engine] xray указывает на {}, но такого файла нет",
                path.display()
            ))
        };
    }
    let managed = setup.work_dir.join("xray.exe");
    if managed.is_file() {
        Ok(managed)
    } else {
        Err(
            "движок не установлен. Скачайте его командой «bliss engine install» \
             — bliss найдёт его при следующем подключении"
                .to_owned(),
        )
    }
}

/// Put the engine on its own runtime thread and hand back a handle.
pub fn spawn(setup: Setup) -> std::io::Result<Controller> {
    let (frame_tx, frame_rx) = watch::channel(initial_frame(&setup));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    // Weak, so the loop still ends when the window lets go of its controller.
    let own = cmd_tx.downgrade();

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
            runtime.block_on(drive(setup, frame_tx, cmd_rx, own));
        })?;

    Ok(Controller {
        frames: frame_rx,
        commands: cmd_tx,
    })
}

fn initial_frame(setup: &Setup) -> Frame {
    let mut frame = Frame {
        state: State::Stopped,
        error: setup.problem.clone(),
        listen: format!(
            "socks {} · http {}",
            setup.profile.inbound.socks, setup.profile.inbound.http
        ),
        history: vec![0; CHART_SAMPLES],
        proxy_mode: setup.profile.inbound.system_proxy,
        servers: server_rows(&setup.profile, &setup.servers, setup.selected),
        subscriptions: subscription_rows(&setup.profile, &HashSet::new(), &HashMap::new()),
        tun: TunView {
            on: setup.tun,
            elevated: bliss_tun::elevated(),
            carrying: false,
            problem: None,
        },
        ..Frame::default()
    };
    if let Some(server) = setup.server(setup.selected) {
        describe_selection(&mut frame, server);
    }
    frame
}

/// The server list's rows: every server that parsed, marked with the
/// subscription it came from.
fn server_rows(profile: &Profile, servers: &[ServerConfig], selected: usize) -> Vec<ServerRow> {
    // In step with `servers`, which holds the entries that parse, in order.
    let groups: Vec<Option<(String, String)>> = profile
        .servers
        .iter()
        .filter(|entry| entry.parse().is_ok())
        .map(|entry| {
            let id = entry.group.as_deref()?;
            profile
                .subscriptions
                .iter()
                .find(|subscription| subscription.id == id)
                .map(|subscription| (subscription.id.clone(), subscription.name.clone()))
        })
        .collect();
    servers
        .iter()
        .enumerate()
        .map(|(i, server)| {
            let group = groups.get(i).cloned().flatten();
            ServerRow {
                tag: server.tag.clone(),
                addr: format!("{}:{}", server.address, server.port),
                kind: kind(server),
                detail: how_it_hides(server),
                ping: Ping::Unknown,
                current: i == selected,
                group_id: group.as_ref().map(|(id, _)| id.clone()),
                group: group.map(|(_, name)| name),
            }
        })
        .collect()
}

/// The links of the servers that parse, in the order `parse_servers` keeps.
fn links_of(profile: &Profile) -> Vec<String> {
    profile
        .servers
        .iter()
        .filter(|entry| entry.parse().is_ok())
        .map(|entry| entry.link.clone())
        .collect()
}

fn subscription_rows(
    profile: &Profile,
    fetching: &HashSet<String>,
    problems: &HashMap<String, String>,
) -> Vec<SubscriptionRow> {
    profile
        .subscriptions
        .iter()
        .map(|subscription| SubscriptionRow {
            id: subscription.id.clone(),
            name: subscription.name.clone(),
            shown_url: subscription.shown_url(),
            servers: profile
                .servers
                .iter()
                .filter(|entry| entry.group.as_deref() == Some(subscription.id.as_str()))
                .count(),
            updated: subscription.updated,
            usage: subscription.usage,
            fetching: fetching.contains(&subscription.id),
            problem: problems.get(&subscription.id).cloned(),
        })
        .collect()
}

/// The fields that describe whichever server is in use.
///
/// Its own function because switching servers has to rewrite exactly these and
/// nothing else — leaving one behind is how a window ends up showing the old
/// server's SNI next to the new server's address.
fn describe_selection(frame: &mut Frame, server: &ServerConfig) {
    frame.server_tag = server.tag.clone();
    frame.server_addr = format!("{}:{}", server.address, server.port);
    frame.sni = match &server.security {
        Security::Reality(r) => r.sni.clone(),
        Security::Tls(t) => t.sni.clone().unwrap_or_default(),
        Security::None => String::new(),
    };
    frame.flow = how_it_hides(server);
    frame.fingerprint = match &server.security {
        Security::Reality(r) => r.fingerprint.to_string(),
        // Hysteria's QUIC does its own handshake and parrots no browser.
        Security::Tls(_) if matches!(server.transport, Transport::Hysteria { .. }) => {
            "-".to_owned()
        }
        Security::Tls(tls) => tls.fingerprint.to_string(),
        Security::None => "-".to_owned(),
    };
}

/// What a server is, in the fewest words the list has room for: the protocol,
/// and what hides it. Hysteria 2 is TLS by nature, so it is not said.
fn kind(server: &ServerConfig) -> String {
    let name = server.protocol.name();
    match (&server.security, &server.transport) {
        (Security::None, _) | (_, Transport::Hysteria { .. }) => name.to_owned(),
        (Security::Tls(_), _) => format!("{name} · TLS"),
        (Security::Reality(_), _) => format!("{name} · REALITY"),
    }
}

/// The same in full: the transport when it is not plain TCP, and Vision.
fn how_it_hides(server: &ServerConfig) -> String {
    let mut parts = vec![server.protocol.name()];
    match &server.transport {
        Transport::Tcp {
            header: TcpHeader::None,
        }
        | Transport::None => {}
        Transport::Tcp {
            header: TcpHeader::Http { .. },
        } => parts.push("tcp-http"),
        Transport::Hysteria { obfs, .. } => {
            if obfs.is_some() {
                parts.push("salamander");
            }
        }
        other => parts.push(other.name()),
    }
    match (&server.security, &server.transport) {
        (Security::None, _) | (_, Transport::Hysteria { .. }) => {}
        (security, _) => parts.push(security.name()),
    }
    if let Protocol::Vless { flow, .. } = &server.protocol
        && *flow != Flow::None
    {
        parts.push("vision");
    }
    parts.join(" · ")
}

/// The engine's whole life: start, publish, obey, stop.
async fn drive(
    mut setup: Setup,
    frames: watch::Sender<Frame>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    // The loop's own address, for work finished elsewhere — a subscription
    // fetched — to come back through, in order with everything else.
    own: mpsc::WeakUnboundedSender<Command>,
) {
    let tracker = Arc::new(Tracker::new());
    let mut toaster = Toaster::default();
    // Subscriptions with a fetch on its way, and why the last one failed.
    let mut fetching: HashSet<String> = HashSet::new();
    let mut problems: HashMap<String, String> = HashMap::new();
    // Its first tick is immediate, and comes after the tunnel has had its
    // chance to start: lists gone stale while bliss was closed are fetched
    // through it. Then once an hour.
    let mut refresher = tokio::time::interval(Duration::from_secs(3600));
    refresher.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut running: Option<Running> = None;
    let mut selected = setup.selected;
    let mut ticker = tokio::time::interval(TICK);
    let mut last = Meter::new();
    // Probing on every tick would put a connection a second through the tunnel
    // just to draw one number.
    let mut probe = tokio::time::interval(Duration::from_secs(8));
    // Numbers the answers to "add a server"; see `Added::attempt`.
    let mut attempts = 0u64;
    // Every change made in the window is saved through this, and taken only
    // once the save worked; see `save_profile`.
    let mut store = crate::profile_file::Store::new(setup.profile_path.clone());

    // Starting the engine blocks this loop for as long as it takes — up to ten
    // seconds if the engine comes up but never listens. The default behaviour
    // for a missed tick is Burst: every tick the block swallowed then fires
    // back to back, which flushed the chart's forty slots with zeros in one
    // frame. Delay simply resumes the beat.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // One permit set for the whole program, not one per sweep: a fresh
    // semaphore per call means the cap is per sweep, and N sweeps in flight is
    // 12N sockets — which is exactly what two clicks on the button produced.
    let probes = Arc::new(Sweep::new());

    let mut proxy = setup.profile.inbound.system_proxy;

    // The entry Windows starts bliss from at sign-in, and the executable it
    // should name: this one.
    let startup_keys = crate::startup::Keys::from_env();
    let exe = std::env::current_exe();
    publish_startup(&frames, &startup_keys, exe.as_deref().ok());

    // The rule table lives here rather than inside `start`, so it survives a
    // stop and can be asked questions while the tunnel is down — which is when
    // someone is most likely to be editing rules.
    let mut rules = Rules::from_setup(&setup);
    publish_routing(&frames, &rules, setup.tun);

    // One sweep at the start, so the list arrives with numbers in it rather
    // than with a column of dashes waiting for someone to discover the button.
    ping_all(&setup.servers, &frames, &probes);

    if setup.autostart {
        running = start(
            &setup,
            selected,
            wiring(&tracker, &rules.router, &frames, &own),
        )
        .await;
    }
    apply_proxy(proxy, &setup, running.is_some(), &frames);

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    Command::Start => {
                        if running.is_none() {
                            running = start(&setup, selected, wiring(&tracker, &rules.router, &frames, &own)).await;
                            apply_proxy(proxy, &setup, running.is_some(), &frames);
                        }
                    }
                    Command::Stop => {
                        stop_now(&mut running, &frames).await;
                        apply_proxy(proxy, &setup, false, &frames);
                    }
                    Command::Toggle { was_on } => {
                        // Read against what the user saw, not against what has
                        // become true since. A click on a switch that showed
                        // "on" means off even if the start it was interrupting
                        // has finished — or failed — while it queued.
                        if was_on {
                            stop_now(&mut running, &frames).await;
                            apply_proxy(proxy, &setup, false, &frames);
                        } else if running.is_none() {
                            running = start(&setup, selected, wiring(&tracker, &rules.router, &frames, &own)).await;
                            apply_proxy(proxy, &setup, running.is_some(), &frames);
                        }
                    }
                    Command::Restart => {
                        if let Some(parts) = running.take() {
                            parts.stop().await;
                        }
                        running = start(&setup, selected, wiring(&tracker, &rules.router, &frames, &own)).await;
                        apply_proxy(proxy, &setup, running.is_some(), &frames);
                    }
                    // Switching server means rebuilding the engine: its config
                    // names exactly one, so there is nothing to reload. The old
                    // process must be gone before the new one starts, or the
                    // second one finds the bridge port taken and fails for a
                    // reason that has nothing to do with the server.
                    Command::Select(index) => {
                        if index != selected && index < setup.servers.len() {
                            // A failed start counts as wanting to be connected.
                            // Picking a different server is the obvious thing
                            // to try after one fails, and it used to do nothing
                            // at all except quietly turn the error into "off".
                            let resume = running.is_some()
                                || frames.borrow().state == State::Failed;
                            if let Some(parts) = running.take() {
                                parts.stop().await;
                            }
                            selected = index;
                            let server = &setup.servers[selected];
                            tracing::info!("server is now {}", server.tag);
                            frames.send_modify(|f| {
                                describe_selection(f, server);
                                for (i, row) in f.servers.iter_mut().enumerate() {
                                    row.current = i == index;
                                }
                                // The figure on screen was measured through the
                                // server we just left.
                                f.latency_ms = None;
                                f.error = None;
                                if !resume {
                                    f.state = State::Stopped;
                                    f.uptime = String::new();
                                }
                            });
                            if resume {
                                running = start(&setup, selected, wiring(&tracker, &rules.router, &frames, &own)).await;
                            }
                            apply_proxy(proxy, &setup, running.is_some(), &frames);
                        }
                    }
                    Command::PingAll => ping_all(&setup.servers, &frames, &probes),
                    Command::Check(text) => {
                        let answer = check_target(&rules.router, &text);
                        frames.send_modify(|f| f.check = answer);
                    }
                    Command::SeedRules => {
                        let mut next = setup.profile.clone();
                        let saved = crate::profile_file::add_starting_rules(&mut next).and_then(|()| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                        });
                        match saved {
                            Ok(()) => tracing::info!("routing: the starting rules are in the profile"),
                            Err(e) => toaster.show(&frames, format!("не удалось сохранить правила: {e}"), true),
                        }
                    }
                    Command::SaveRule { at, rule } => {
                        let mut next = setup.profile.clone();
                        let saved = crate::profile_file::save_rule(&mut next, at, rule).and_then(|()| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                        });
                        match saved {
                            Ok(()) => toaster.show(
                                &frames,
                                if at.is_some() { "Правило сохранено" } else { "Правило добавлено" },
                                false,
                            ),
                            Err(e) => toaster.show(&frames, format!("правило не сохранилось: {e}"), true),
                        }
                    }
                    Command::DeleteRule(at) => {
                        let mut next = setup.profile.clone();
                        let deleted = crate::profile_file::delete_rule(&mut next, at).and_then(|rule| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                                .map(|()| rule)
                        });
                        match deleted {
                            Ok(rule) => toaster.show(
                                &frames,
                                format!("Правило «{}» удалено", rule.display_name()),
                                false,
                            ),
                            Err(e) => toaster.show(&frames, format!("правило не удалилось: {e}"), true),
                        }
                    }
                    Command::MoveRule { from, to } => {
                        let mut next = setup.profile.clone();
                        let moved = crate::profile_file::move_rule(&mut next, from, to).and_then(|()| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                        });
                        if let Err(e) = moved {
                            toaster.show(&frames, format!("порядок не сохранился: {e}"), true);
                        }
                    }
                    Command::Fallback(action) => {
                        if setup.profile.routing.fallback != action {
                            let mut next = setup.profile.clone();
                            next.routing.fallback = action;
                            if let Err(e) =
                                commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                            {
                                toaster.show(&frames, format!("выбор не сохранился: {e}"), true);
                            }
                        }
                    }
                    Command::SwitchRule { at, on } => {
                        let mut next = setup.profile.clone();
                        let switched = crate::profile_file::switch_rule(&mut next, at, on).and_then(|()| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                        });
                        if let Err(e) = switched {
                            toaster.show(&frames, format!("правило не переключилось: {e}"), true);
                        }
                    }
                    Command::ApplyPreset(name) => {
                        let mut next = setup.profile.clone();
                        let applied = crate::profile_file::apply_preset(&mut next, &name).and_then(|()| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                        });
                        match applied {
                            Ok(()) => {
                                tracing::info!("routing: preset {name:?} applied");
                                toaster.show(&frames, format!("Пресет «{name}»"), false);
                            }
                            Err(e) => toaster.show(&frames, format!("пресет не применился: {e}"), true),
                        }
                    }
                    Command::SavePreset(name) => {
                        let mut next = setup.profile.clone();
                        let saved = crate::profile_file::save_preset(&mut next, &name).and_then(|created| {
                            let name = next.routing.current_preset.clone().unwrap_or_default();
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                                .map(|()| (created, name))
                        });
                        match saved {
                            Ok((true, name)) => toaster.show(&frames, format!("Пресет «{name}» сохранён"), false),
                            Ok((false, name)) => toaster.show(&frames, format!("Пресет «{name}» обновлён"), false),
                            Err(e) => toaster.show(&frames, format!("пресет не сохранился: {e}"), true),
                        }
                    }
                    Command::RenamePreset { from, to } => {
                        let mut next = setup.profile.clone();
                        let renamed = crate::profile_file::rename_preset(&mut next, &from, &to).and_then(|to| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                                .map(|()| to)
                        });
                        match renamed {
                            Ok(to) if to != from => {
                                toaster.show(&frames, format!("Пресет «{from}» теперь «{to}»"), false)
                            }
                            Ok(_) => {}
                            Err(e) => toaster.show(&frames, format!("пресет не переименовался: {e}"), true),
                        }
                    }
                    Command::DeletePreset(name) => {
                        let mut next = setup.profile.clone();
                        let deleted = crate::profile_file::delete_preset(&mut next, &name).and_then(|()| {
                            commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames)
                        });
                        match deleted {
                            Ok(()) => toaster.show(&frames, format!("Пресет «{name}» удалён"), false),
                            Err(e) => toaster.show(&frames, format!("пресет не удалился: {e}"), true),
                        }
                    }
                    Command::RouteOne { subject, action, target, process } => {
                        let mut next = setup.profile.clone();
                        crate::profile_file::route_one(&mut next, &subject, action);
                        match commit_routing(next, &mut store, &mut setup, &mut rules, running.as_ref(), &frames) {
                            Ok(()) => {
                                tracing::info!("routing: {} now {}", subject.value(), action.label());
                                let mut text = format!("{} — {}", subject.value(), action.label());
                                // The new rule went last, so a rule above it can
                                // still claim this connection. Said out loud, or
                                // the choice looks ignored.
                                if let Ok(parsed) = bliss_core::Target::parse_authority(&target) {
                                    let decision = rules.router.decide(&parsed, process.as_deref());
                                    if decision.route != bliss_core::Route::from(action) {
                                        text.push_str(&format!(
                                            ". Это соединение по-прежнему решает «{}» — это правило выше в списке",
                                            decision.rule
                                        ));
                                    }
                                }
                                toaster.show(&frames, text, false);
                            }
                            Err(e) => toaster.show(&frames, format!("правило не сохранилось: {e}"), true),
                        }
                    }
                    Command::Import { text, from_form } => {
                        attempts += 1;
                        let attempt = attempts;
                        let first = setup.servers.is_empty();
                        let was = links_of(&setup.profile).get(selected).cloned();
                        let mut next = setup.profile.clone();
                        let mut servers_came = false;

                        let outcome = if let Some(url) = crate::profile_file::subscription_address(&text.0) {
                            let seed = jiff::Timestamp::now().as_millisecond().unsigned_abs();
                            crate::profile_file::add_subscription(&mut next, &url, seed)
                                .and_then(|added| save_profile(&mut store, &next).map(|()| added))
                                .map(|added| {
                                    // The host alone: the rest of the address is a token.
                                    tracing::info!("subscription added: {}", added.name);
                                    setup.profile = next;
                                    forget_problem(&mut setup, &frames);
                                    fetching.insert(added.id.clone());
                                    fetch_subscription(added.id, added.url, via(running.as_ref()), true, own.clone());
                                    publish_subscriptions(&frames, &setup.profile, &fetching, &problems);
                                    format!("Подписка «{}» добавлена, загружаю список…", added.name)
                                })
                        } else {
                            crate::profile_file::import_servers(&mut next, &text.0)
                                .and_then(|imported| save_profile(&mut store, &next).map(|()| imported))
                                .map(|imported| {
                                    for server in &imported.added {
                                        // Names alone: a link is a credential.
                                        tracing::info!("server added: {}", server.tag);
                                    }
                                    setup.profile = next;
                                    forget_problem(&mut setup, &frames);
                                    reload_servers(&mut setup, &mut selected, was, &frames);
                                    servers_came = true;
                                    imported.summary()
                                })
                        };

                        // Before any start, which holds this loop for seconds:
                        // whoever pasted should hear back the moment it is saved.
                        if outcome.is_err() {
                            tracing::info!("pasted text added nothing");
                        }
                        // Ctrl+V takes whatever the clipboard holds, which is
                        // mostly not a link at all: say that, rather than what
                        // is wrong with the text as a link.
                        let outcome = outcome.map_err(|why| {
                            if from_form || text.0.contains("://") {
                                why
                            } else {
                                "в буфере обмена нет ссылки на сервер или адреса подписки".to_owned()
                            }
                        });
                        if from_form {
                            frames.send_modify(|f| f.added = Some(Added { attempt, outcome }));
                        } else {
                            match outcome {
                                Ok(text) => toaster.show(&frames, text, false),
                                Err(why) => toaster.show(&frames, why, true),
                            }
                        }
                        if servers_came {
                            ping_all(&setup.servers, &frames, &probes);
                            // The first server is what the window was waiting
                            // for. Connect, as opening it with one already would.
                            if first && setup.autostart && running.is_none() {
                                running = start(&setup, selected, wiring(&tracker, &rules.router, &frames, &own)).await;
                                apply_proxy(proxy, &setup, running.is_some(), &frames);
                            }
                        }
                    }
                    Command::Refresh(only) => {
                        let started = refresh(
                            &setup.profile,
                            only.as_deref(),
                            false,
                            true,
                            &mut fetching,
                            via(running.as_ref()),
                            &own,
                        );
                        if started > 0 {
                            publish_subscriptions(&frames, &setup.profile, &fetching, &problems);
                        }
                    }
                    Command::DeleteSubscription(id) => {
                        let was = links_of(&setup.profile).get(selected).cloned();
                        let servers = setup
                            .profile
                            .servers
                            .iter()
                            .filter(|entry| entry.group.as_deref() == Some(id.as_str()))
                            .count();
                        let mut next = setup.profile.clone();
                        let deleted = crate::profile_file::delete_subscription(&mut next, &id)
                            .and_then(|gone| save_profile(&mut store, &next).map(|()| gone));
                        match deleted {
                            Ok(gone) => {
                                tracing::info!("subscription removed: {}", gone.name);
                                setup.profile = next;
                                forget_problem(&mut setup, &frames);
                                fetching.remove(&id);
                                problems.remove(&id);
                                let lost = reload_servers(&mut setup, &mut selected, was, &frames);
                                publish_subscriptions(&frames, &setup.profile, &fetching, &problems);
                                toaster.show(
                                    &frames,
                                    format!(
                                        "Подписка «{}» удалена вместе с {}",
                                        gone.name,
                                        plural(servers as u64, "сервером", "серверами", "серверами")
                                    ),
                                    false,
                                );
                                if lost {
                                    follow_selection(&mut running, &setup, selected, wiring(&tracker, &rules.router, &frames, &own), proxy)
                                        .await;
                                }
                            }
                            Err(e) => toaster.show(&frames, e, true),
                        }
                    }
                    Command::DeleteServer { index, tag } => {
                        let links = links_of(&setup.profile);
                        let was = links.get(selected).cloned();
                        let Some(link) = setup
                            .servers
                            .get(index)
                            .filter(|server| server.tag == tag)
                            .and(links.get(index))
                            .cloned()
                        else {
                            toaster.show(&frames, "этого сервера уже нет — список успел измениться", true);
                            continue;
                        };
                        let mut next = setup.profile.clone();
                        let deleted = crate::profile_file::delete_server(&mut next, &link)
                            .and_then(|entry| save_profile(&mut store, &next).map(|()| entry));
                        match deleted {
                            Ok(entry) => {
                                tracing::info!("server removed: {tag}");
                                let from = entry
                                    .group
                                    .as_deref()
                                    .and_then(|id| setup.profile.subscriptions.iter().find(|s| s.id == id))
                                    .map(|subscription| subscription.name.clone());
                                setup.profile = next;
                                forget_problem(&mut setup, &frames);
                                let lost = reload_servers(&mut setup, &mut selected, was, &frames);
                                publish_subscriptions(&frames, &setup.profile, &fetching, &problems);
                                // Said plainly: one from a subscription is back
                                // with its next update, as in v2rayN.
                                let text = match from {
                                    Some(name) => format!(
                                        "Сервер «{tag}» удалён. Он вернётся при следующем обновлении подписки «{name}»"
                                    ),
                                    None => format!("Сервер «{tag}» удалён"),
                                };
                                toaster.show(&frames, text, false);
                                // The rows came back without their numbers.
                                ping_all(&setup.servers, &frames, &probes);
                                if lost {
                                    follow_selection(&mut running, &setup, selected, wiring(&tracker, &rules.router, &frames, &own), proxy)
                                        .await;
                                }
                            }
                            Err(e) => toaster.show(&frames, e, true),
                        }
                    }
                    Command::Fetched(done) => {
                        fetching.remove(&done.id);
                        let Some(name) = setup
                            .profile
                            .subscriptions
                            .iter()
                            .find(|subscription| subscription.id == done.id)
                            .map(|subscription| subscription.name.clone())
                        else {
                            // Deleted while the fetch was on its way.
                            continue;
                        };
                        let outcome = done.result.and_then(|fetched| {
                            let mut next = setup.profile.clone();
                            let now = jiff::Timestamp::now().as_second();
                            crate::profile_file::apply_subscription(&mut next, &done.id, &fetched, now)
                                .and_then(|refreshed| save_profile(&mut store, &next).map(|()| (refreshed, next)))
                        });
                        match outcome {
                            Ok((refreshed, next)) => {
                                problems.remove(&done.id);
                                tracing::info!(
                                    "subscription {}: {} server(s), {} new, {} gone",
                                    refreshed.name, refreshed.servers, refreshed.added, refreshed.removed
                                );
                                let first = setup.servers.is_empty();
                                let was = links_of(&setup.profile).get(selected).cloned();
                                setup.profile = next;
                                forget_problem(&mut setup, &frames);
                                let lost = reload_servers(&mut setup, &mut selected, was, &frames);
                                if done.asked {
                                    toaster.show(&frames, refreshed_text(&refreshed), false);
                                }
                                // The rows came back without their numbers.
                                ping_all(&setup.servers, &frames, &probes);
                                if lost {
                                    follow_selection(&mut running, &setup, selected, wiring(&tracker, &rules.router, &frames, &own), proxy)
                                        .await;
                                } else if first && setup.autostart && running.is_none() && !setup.servers.is_empty() {
                                    running = start(&setup, selected, wiring(&tracker, &rules.router, &frames, &own)).await;
                                    apply_proxy(proxy, &setup, running.is_some(), &frames);
                                }
                            }
                            Err(why) => {
                                tracing::info!("subscription {name}: not updated");
                                if done.asked {
                                    toaster.show(&frames, format!("«{name}»: {why}"), true);
                                }
                                problems.insert(done.id.clone(), why);
                            }
                        }
                        publish_subscriptions(&frames, &setup.profile, &fetching, &problems);
                    }
                    Command::Proxy(mode) => {
                        proxy = mode;
                        apply_proxy(proxy, &setup, running.is_some(), &frames);
                        // Remembered, like every other choice made in the window.
                        // It used to last only until the window closed.
                        if setup.profile.inbound.system_proxy != mode {
                            let mut next = setup.profile.clone();
                            next.inbound.system_proxy = mode;
                            match save_profile(&mut store, &next) {
                                Ok(()) => {
                                    setup.profile = next;
                                    forget_problem(&mut setup, &frames);
                                }
                                Err(e) => frames.send_modify(|f| {
                                    f.error = Some(format!("выбор не сохранился: {e}"));
                                }),
                            }
                        }
                    }
                    Command::Reveal(Reveal::WorkDir) => reveal(&setup.work_dir),
                    Command::Startup { on, minimized } => {
                        match exe.as_deref() {
                            Ok(path) => match crate::startup::set(&startup_keys, path, on, minimized) {
                                Ok(()) => tracing::info!(
                                    "startup: {}",
                                    match (on, minimized) {
                                        (false, _) => "Windows no longer starts bliss",
                                        (true, false) => "Windows starts bliss at sign-in",
                                        (true, true) => "Windows starts bliss at sign-in, minimized",
                                    }
                                ),
                                Err(e) => toaster.show(&frames, format!("автозапуск не изменился: {e}"), true),
                            },
                            Err(_) => toaster.show(&frames, "не удалось узнать, где лежит bliss.exe", true),
                        }
                        publish_startup(&frames, &startup_keys, exe.as_deref().ok());
                    }
                    Command::TunUp(raised) => {
                        let TunRaised { serving, result } = *raised;
                        // The tunnel it was built for, and the one running now,
                        // have to be the same one: a restart in between leaves
                        // an adapter pointed at a `Serving` nobody serves.
                        let mine = running
                            .as_ref()
                            .is_some_and(|parts| Arc::ptr_eq(&parts.serving, &serving));
                        if let Some(parts) = running.as_mut().filter(|_| mine) {
                            parts.tun_rising = false;
                        }
                        match result {
                            Ok(tun) if mine && setup.tun => {
                                if let Some(parts) = running.as_mut() {
                                    parts.tun = Some(tun);
                                }
                                tracing::info!("tun: carrying the machine's traffic");
                                publish_tun(&frames, true, true, None);
                            }
                            // It arrived after the switch went back, or after a
                            // restart. Dropped, which removes the adapter and
                            // its routes — off the loop, like every other
                            // driver call.
                            Ok(tun) => {
                                let _ = tokio::task::spawn_blocking(move || drop(tun)).await;
                                publish_tun(&frames, setup.tun, false, None);
                            }
                            Err(e) => {
                                tracing::error!("tun: {e}");
                                publish_tun(&frames, setup.tun, false, Some(e.clone()));
                                toaster.show(&frames, format!("TUN не поднялся: {e}"), true);
                            }
                        }
                    }
                    Command::ReadStartup => publish_startup(&frames, &startup_keys, exe.as_deref().ok()),
                    Command::Tun(on) => {
                        if setup.profile.tun.enabled != on {
                            let mut next = setup.profile.clone();
                            next.tun.enabled = on;
                            match save_profile(&mut store, &next) {
                                Ok(()) => {
                                    setup.profile = next;
                                    forget_problem(&mut setup, &frames);
                                }
                                Err(e) => {
                                    toaster.show(&frames, format!("выбор не сохранился: {e}"), true);
                                }
                            }
                        }
                        setup.tun = on;
                        match (running.as_mut(), setup.server(selected)) {
                            (Some(parts), Some(server)) if on => {
                                if parts.tun.is_none() && !parts.tun_rising {
                                    let serving = Arc::clone(&parts.serving);
                                    parts.tun_rising =
                                        begin_tun(&setup, server, &serving, &own, &frames);
                                }
                            }
                            (Some(parts), _) => {
                                if let Some(tun) = parts.tun.take() {
                                    // Off the loop, as raising it is: taking an
                                    // adapter away is driver work too.
                                    let _ = tokio::task::spawn_blocking(move || drop(tun)).await;
                                    tracing::info!(
                                        "tun: the adapter is down; the machine's own routes are back"
                                    );
                                }
                                // `setup.tun`, not `false`: this arm also
                                // catches "asked for, but there is no server
                                // to carry it", and the switch must go on
                                // saying what the profile asks for.
                                publish_tun(&frames, setup.tun, false, None);
                            }
                            // Nothing to apply it to yet: the next start reads
                            // the same flag and brings the adapter up with it.
                            _ => publish_tun(&frames, on, false, None),
                        }
                        // The one refusal that is known at the moment of the
                        // click says so at the moment of the click. Everything
                        // else is answered when the adapter reports back.
                        if on && !bliss_tun::elevated() {
                            // The window runs elevated as well as not (rule 25),
                            // so the answer is the plain one. The choice is
                            // saved: an elevated start raises the adapter.
                            let note = "TUN не поднялся: адаптеру нужны права администратора. \
                                        Выбор сохранён — перезапустите bliss от имени \
                                        администратора, и он включится сам";
                            toaster.show(&frames, note.to_owned(), true);
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                // The heartbeat is also the supervision. Nothing watched the
                // engine before this: an xray that died mid-session left a
                // green ring over a bridge that refused every connection, and
                // the only clue was that the internet had stopped working.
                if let Some(status) = running.as_mut().and_then(|r| r.xray.exited()) {
                    if let Some(parts) = running.take() {
                        // Takes the inbounds down with it. A listener still
                        // accepting over a dead engine turns every request into
                        // a hang instead of a refusal.
                        parts.stop().await;
                    }
                    running = fail(&frames, &format!("движок неожиданно завершился ({status})"));
                    // The machine must not be left pointed at a port that has
                    // just stopped listening.
                    apply_proxy(proxy, &setup, false, &frames);
                }
                publish(&frames, &tracker, &mut last, running.as_ref());
                toaster.expire(&frames);
            }
            _ = refresher.tick() => {
                let route = via(running.as_ref());
                if refresh(&setup.profile, None, true, false, &mut fetching, route, &own) > 0 {
                    publish_subscriptions(&frames, &setup.profile, &fetching, &problems);
                }
            }
            _ = probe.tick() => {
                if let Some(parts) = running.as_ref() {
                    let outbound = Arc::clone(&parts.serving.outbound);
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
    // Belt: the window also restores on its way out, and the startup check is
    // the braces. This covers the ordinary case of the channel closing.
    let _ = bliss_core::sysproxy::restore(&setup.work_dir);
}

/// Point the machine's proxy settings at us, or put them back.
///
/// `serving` is what makes this safe: mode `Set` with nothing listening writes
/// nothing at all. A proxy aimed at a dead port is indistinguishable, from the
/// user's chair, from the internet being broken — so the mode is remembered
/// and takes effect when the tunnel comes up.
fn apply_proxy(mode: ProxyMode, setup: &Setup, serving: bool, frames: &watch::Sender<Frame>) {
    let addr = serving.then(|| {
        let inbound = &setup.profile.inbound;
        // A proxy value has to name an address a client can dial. If we are
        // listening on every interface, the one to tell the machine about is
        // loopback.
        let ip = if inbound.listen.is_unspecified() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            inbound.listen
        };
        SocketAddr::new(ip, inbound.http)
    });

    match bliss_core::sysproxy::apply(mode, &setup.work_dir, addr) {
        Ok(applied) => frames.send_modify(|f| {
            f.proxy_mode = mode;
            f.proxy_applied = applied;
        }),
        Err(e) => {
            tracing::error!("system proxy: {e}");
            frames.send_modify(|f| {
                f.proxy_mode = mode;
                f.proxy_applied = false;
                f.error = Some(format!("не удалось изменить системный прокси: {e}"));
            });
        }
    }
}

struct Running {
    xray: bliss_xray::Xray,
    inbounds: Vec<JoinHandle<()>>,
    /// The adapter, while there is one. Dropping it takes the routes away and
    /// removes the adapter, which is the whole of putting the machine back.
    tun: Option<bliss_tun::Tun>,
    /// One is on its way up. Without this a second click while the driver is
    /// still installing starts a second adapter, and the first one is orphaned
    /// the moment the second arrives.
    tun_rising: bool,
    since: Instant,
    /// What the inbounds share: the dialer, which the latency probe uses too,
    /// and the rule table a routing change swaps under live traffic.
    serving: Arc<Serving>,
    /// The engine's own SOCKS port, for fetching a subscription through the
    /// tunnel without the rules getting a say.
    bridge: SocketAddr,
}

impl Running {
    async fn stop(self) {
        for task in self.inbounds {
            task.abort();
        }
        // Explicitly, and before the engine: while the adapter is up the
        // machine keeps handing it traffic, and the engine is what carries
        // that traffic anywhere. Dropping it takes the routes away and the
        // adapter with them — driver calls, so off this thread, as raising it
        // is.
        if let Some(tun) = self.tun {
            let _ = tokio::task::spawn_blocking(move || drop(tun)).await;
        }
        let _ = self.xray.shutdown().await;
    }
}

// --- routing ---------------------------------------------------------------

/// The rules as written, next to the rules as compiled.
///
/// Both, because they answer different questions. The compiled table decides
/// connections; the written config is what the screen shows — a compiled rule
/// has lost the words the user typed, and those are exactly what makes a rule
/// list readable.
struct Rules {
    config: bliss_proto::Routing,
    router: Arc<Router>,
}

impl Rules {
    fn compile(config: bliss_proto::Routing, work_dir: &std::path::Path) -> Self {
        let dir = config
            .geo_dir
            .clone()
            .unwrap_or_else(|| work_dir.to_owned());
        let router = Router::build(&config, &dir);
        for warning in &router.warnings {
            tracing::warn!("routing: {warning}");
        }
        if router.rule_count() > 0 {
            tracing::info!(
                "routing: {} rule(s), everything else {}",
                router.rule_count(),
                router.fallback()
            );
        }
        Self {
            config,
            router: Arc::new(router),
        }
    }

    fn from_setup(setup: &Setup) -> Self {
        Self::compile(setup.profile.routing.clone(), &setup.work_dir)
    }

    /// The table as the screen shows it, told whether the adapter is carrying.
    ///
    /// The warning depends on both: the rules come from the profile and the
    /// adapter comes and goes under them, so it cannot live in `Router`, which
    /// is compiled once and knows nothing about how traffic arrives.
    fn view(&self, tun: bool) -> RoutingView {
        let mut warnings = self.router.warnings.clone();
        if tun && let Some(note) = blind_under_tun(&self.config.rules) {
            warnings.push(note);
        }
        RoutingView {
            fallback: self.config.fallback,
            rules: self
                .config
                .rules
                .iter()
                .enumerate()
                .map(|(i, rule)| RuleView {
                    position: i + 1,
                    name: rule.display_name(),
                    action: rule.action,
                    conditions: describe_conditions(rule),
                    enabled: rule.enabled,
                })
                .collect(),
            source: self.config.rules.clone(),
            warnings,
            presets: self.config.presets.iter().map(|p| p.name.clone()).collect(),
            current_preset: self.config.current_preset.clone(),
            drifted: self.config.drifted(),
        }
    }
}

/// Which rules a TUN adapter cannot decide, said the way the other warnings
/// are said.
///
/// A packet holds an address; the name the program looked up is gone by then.
/// So a rule made of domains and geosite sets alone never matches anything
/// that arrives through the adapter — while it goes on matching everything
/// that arrives at the SOCKS and HTTP ports, which is why the note says both.
/// A rule that also has an address, a port or a program is left out: half of
/// it still decides, and naming it would bury the ones that decide nothing.
fn blind_under_tun(rules: &[Rule]) -> Option<String> {
    let blind: Vec<String> = rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            rule.enabled
                && !(rule.domain.is_empty() && rule.geosite.is_empty())
                && rule.ip.is_empty()
                && rule.geoip.is_empty()
                && rule.port.is_empty()
                && rule.process.is_empty()
        })
        .map(|(i, _)| format!("#{}", i + 1))
        .collect();
    if blind.is_empty() {
        return None;
    }
    let (many, list) = (blind.len() > 1, blind.join(", "));
    let head = if many {
        format!("правила {list} не сработают")
    } else {
        format!("правило {list} не сработает")
    };
    let tail = if many {
        "они работают"
    } else {
        "оно работает"
    };
    Some(format!(
        "Включён режим TUN: в пакете виден только адрес, а не имя сайта, поэтому {head} \
         для трафика через адаптер. Для программ, которые ходят через порты SOCKS и HTTP, \
         {tail} как раньше."
    ))
}

/// Save a changed profile, saying so in the log.
///
/// Callers change a clone and take it only when this succeeds, so what the
/// window shows and what is on disk cannot drift apart.
fn save_profile(store: &mut crate::profile_file::Store, next: &Profile) -> Result<(), String> {
    match store.save(next) {
        Ok(Some(backup)) => {
            tracing::warn!(
                "profile: the file bliss replaced was not its own; kept as {}",
                backup.display()
            );
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => {
            tracing::error!("profile: {e}");
            Err(e)
        }
    }
}

/// After a successful save, the complaint about the profile bliss started
/// with is out of date: that file has been kept and replaced.
fn forget_problem(setup: &mut Setup, frames: &watch::Sender<Frame>) {
    if let Some(problem) = setup.problem.take() {
        frames.send_modify(|f| {
            if f.error.as_deref() == Some(problem.as_str()) {
                f.error = None;
            }
        });
    }
}

/// Save a routing change made on a copy of the profile, and put it to work: on
/// screen, and under the traffic already flowing.
///
/// Nothing restarts. The inbounds read the table for every new connection
/// (`Serving::set_router`), so connections already open keep their route and
/// the next one gets the new rules. A change used to stop and start the whole
/// tunnel, which dropped every connection for the sake of one rule.
fn commit_routing(
    next: Profile,
    store: &mut Store,
    setup: &mut Setup,
    rules: &mut Rules,
    running: Option<&Running>,
    frames: &watch::Sender<Frame>,
) -> Result<(), String> {
    let mut next = next;
    // Every rule saved has an id, so a preset saved later can name it.
    next.routing.ensure_ids();
    save_profile(store, &next)?;
    setup.profile = next;
    forget_problem(setup, frames);
    *rules = Rules::compile(setup.profile.routing.clone(), &setup.work_dir);
    publish_routing(frames, rules, setup.tun);
    if let Some(parts) = running {
        parts.serving.set_router(Arc::clone(&rules.router));
    }
    Ok(())
}

/// Take a changed server list from the profile: parse it again, keep the
/// server in use selected while it is still listed, and redraw the rows.
///
/// `was` is the link of the server in use before the change. Returns true when
/// that server has gone, so the tunnel can follow the new selection.
fn reload_servers(
    setup: &mut Setup,
    selected: &mut usize,
    was: Option<String>,
    frames: &watch::Sender<Frame>,
) -> bool {
    let (servers, _) = setup.profile.parse_servers();
    let found = was
        .as_ref()
        .and_then(|link| links_of(&setup.profile).iter().position(|l| l == link));
    let lost = was.is_some() && found.is_none();
    *selected = found.unwrap_or(0);
    setup.servers = servers;
    let rows = server_rows(&setup.profile, &setup.servers, *selected);
    let server = setup.server(*selected);
    frames.send_modify(|f| {
        f.servers = rows;
        match server {
            Some(server) => describe_selection(f, server),
            None => {
                f.server_tag.clear();
                f.server_addr.clear();
                f.sni.clear();
                f.flow.clear();
                f.fingerprint.clear();
            }
        }
    });
    lost
}

/// After the server in use left the list: carry the tunnel over to what is
/// selected now, or stop it when nothing is left to carry it to.
async fn follow_selection(
    running: &mut Option<Running>,
    setup: &Setup,
    selected: usize,
    wiring: Wiring<'_>,
    proxy: ProxyMode,
) {
    let frames = wiring.frames;
    let Some(parts) = running.take() else {
        return;
    };
    parts.stop().await;
    if setup.servers.is_empty() {
        stop_now(running, frames).await;
    } else {
        *running = start(setup, selected, wiring).await;
    }
    apply_proxy(proxy, setup, running.is_some(), frames);
}

fn publish_subscriptions(
    frames: &watch::Sender<Frame>,
    profile: &Profile,
    fetching: &HashSet<String>,
    problems: &HashMap<String, String>,
) {
    let rows = subscription_rows(profile, fetching, problems);
    frames.send_modify(|f| f.subscriptions = rows);
}

/// The engine's SOCKS port while the tunnel is up.
fn via(running: Option<&Running>) -> Option<SocketAddr> {
    running.map(|parts| parts.bridge)
}

/// Start fetching subscriptions: `only` the one named, or all of them; with
/// `stale_only`, just those not fetched for `REFRESH_AFTER`. One already on
/// its way is not fetched twice. Returns how many were started.
fn refresh(
    profile: &Profile,
    only: Option<&str>,
    stale_only: bool,
    asked: bool,
    fetching: &mut HashSet<String>,
    via: Option<SocketAddr>,
    own: &mpsc::WeakUnboundedSender<Command>,
) -> usize {
    let now = jiff::Timestamp::now().as_second();
    let stale_after = crate::subscription::REFRESH_AFTER.as_secs() as i64;
    let mut started = 0;
    for subscription in &profile.subscriptions {
        if only.is_some_and(|id| id != subscription.id) {
            continue;
        }
        let stale = subscription
            .updated
            .is_none_or(|at| now.saturating_sub(at) >= stale_after);
        if (stale_only && !stale) || !fetching.insert(subscription.id.clone()) {
            continue;
        }
        fetch_subscription(
            subscription.id.clone(),
            subscription.url.clone(),
            via,
            asked,
            own.clone(),
        );
        started += 1;
    }
    started
}

/// Fetch a list off the engine loop and hand the result back to it as a
/// command, so a slow provider holds nothing else up.
///
/// Through the tunnel first when it is up, then straight: a provider's address
/// can be blocked on this network, or unreachable from the server's.
fn fetch_subscription(
    id: String,
    url: String,
    via: Option<SocketAddr>,
    asked: bool,
    own: mpsc::WeakUnboundedSender<Command>,
) {
    tokio::spawn(async move {
        let mut result = crate::subscription::fetch(&url, via).await;
        if result.is_err() && via.is_some() {
            result = crate::subscription::fetch(&url, None).await;
        }
        if let Some(own) = own.upgrade() {
            let _ = own.send(Command::Fetched(FetchDone { id, result, asked }));
        }
    });
}

fn refreshed_text(refreshed: &crate::profile_file::Refreshed) -> String {
    let mut text = format!(
        "«{}»: {}",
        refreshed.name,
        plural(refreshed.servers as u64, "сервер", "сервера", "серверов")
    );
    if refreshed.added > 0 || refreshed.removed > 0 {
        text.push_str(&format!(
            " — новых {}, убрано {}",
            refreshed.added, refreshed.removed
        ));
    }
    text
}

/// Short lines about what the last action did, taken down after a while.
#[derive(Default)]
struct Toaster {
    shown: u64,
    until: Option<Instant>,
}

impl Toaster {
    fn show(&mut self, frames: &watch::Sender<Frame>, text: impl Into<String>, bad: bool) {
        // Long enough to read a sentence twice; a failure stays a little longer.
        let lasts = Duration::from_secs(if bad { 9 } else { 5 });
        self.shown += 1;
        self.until = Some(Instant::now() + lasts);
        let toast = Toast {
            id: self.shown,
            text: text.into(),
            bad,
        };
        frames.send_modify(|f| f.toast = Some(toast));
    }

    /// On the tick: take the line down once its time is up.
    fn expire(&mut self, frames: &watch::Sender<Frame>) {
        if self.until.is_some_and(|until| Instant::now() >= until) {
            self.until = None;
            frames.send_modify(|f| f.toast = None);
        }
    }
}

/// One line per condition, in the words the profile used.
fn describe_conditions(rule: &bliss_proto::Rule) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |label: &str, list: &[String]| {
        if !list.is_empty() {
            out.push(format!("{label}: {}", list.join(", ")));
        }
    };
    add("домены", &rule.domain);
    add("наборы", &rule.geosite);
    add("адреса", &rule.ip);
    add("страны", &rule.geoip);
    add("порты", &rule.port);
    add("программы", &rule.process);
    out
}

fn publish_routing(frames: &watch::Sender<Frame>, rules: &Rules, tun: bool) {
    let view = rules.view(tun);
    frames.send_modify(|f| f.routing = view);
}

/// Answer the screen's "what happens to this?" box.
///
/// Accepts what someone would type: a name, a name with a port, or a bare
/// address. 443 is assumed only when there is no port to read.
fn check_target(router: &Router, text: &str) -> Option<CheckResult> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // The address of a page, pasted whole: the site in it is the question.
    let from_url = reqwest::Url::parse(text)
        .ok()
        .filter(|_| text.contains("://"))
        .and_then(|url| {
            let host = url.host_str()?.to_owned();
            Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host,
            })
        });
    let text = from_url.as_deref().unwrap_or(text);
    let (target, typed_port) = if let Ok(ip) = text.parse::<std::net::IpAddr>() {
        (bliss_core::Target::Socket(SocketAddr::new(ip, 443)), false)
    } else {
        match bliss_core::Target::parse_authority(text) {
            Ok(target) => (target, true),
            Err(_) => (bliss_core::Target::domain(text, 443), false),
        }
    };

    let decision = router.decide(&target, None);
    let note = router.geoip_note(&target, None);
    Some(CheckResult {
        // The port only when it was typed. Assumed, it read as part of the
        // answer: `geoip:ru` came back as `geoip:ru:443`.
        target: if typed_port {
            target.to_string()
        } else {
            target.host()
        },
        action: match decision.route {
            bliss_core::Route::Proxy => Action::Proxy,
            bliss_core::Route::Direct => Action::Direct,
            bliss_core::Route::Blocked => Action::Block,
        },
        rule: decision.rule,
        note,
    })
}

/// Read the startup entry into the frame, for the settings screen.
fn publish_startup(
    frames: &watch::Sender<Frame>,
    keys: &crate::startup::Keys,
    exe: Option<&std::path::Path>,
) {
    let startup = match exe {
        Some(exe) => crate::startup::read(keys, exe),
        None => crate::startup::Startup {
            problem: Some("не удалось узнать, где лежит bliss.exe".to_owned()),
            ..Default::default()
        },
    };
    frames.send_modify(|f| f.startup = startup);
}

/// Tear the tunnel down and say so, whether or not there was one.
///
/// Called even when nothing is running: after a failed start there is no
/// `Running` to take, but the window is still showing an error the user has
/// just asked to be rid of.
async fn stop_now(running: &mut Option<Running>, frames: &watch::Sender<Frame>) {
    if let Some(parts) = running.take() {
        parts.stop().await;
    }
    frames.send_modify(|f| {
        f.state = State::Stopped;
        f.error = None;
        f.uptime = String::new();
        f.latency_ms = None;
    });
}

/// What a start needs besides the profile, and what it reports to.
///
/// One argument because these four travel together everywhere and none of
/// them tells the others apart by type: two `Arc`s, a sender and a sender.
/// The same reason `Serving` exists on the other side of the seam.
#[derive(Clone, Copy)]
struct Wiring<'a> {
    /// Where every connection is recorded.
    tracker: &'a Arc<Tracker>,
    /// Which rules decide them, as compiled right now.
    router: &'a Arc<Router>,
    /// What the window reads.
    frames: &'a watch::Sender<Frame>,
    /// The loop's own address, for work that finishes elsewhere — a
    /// subscription fetched, an adapter raised — to come back through.
    own: &'a mpsc::WeakUnboundedSender<Command>,
}

fn wiring<'a>(
    tracker: &'a Arc<Tracker>,
    router: &'a Arc<Router>,
    frames: &'a watch::Sender<Frame>,
    own: &'a mpsc::WeakUnboundedSender<Command>,
) -> Wiring<'a> {
    Wiring {
        tracker,
        router,
        frames,
        own,
    }
}

async fn start(setup: &Setup, selected: usize, wiring: Wiring<'_>) -> Option<Running> {
    let Wiring {
        tracker,
        router,
        frames,
        own,
    } = wiring;
    // Nothing to start before a server is added. The window offers to add one,
    // which says more than a failure would.
    let server = setup.server(selected)?;
    frames.send_modify(|f| {
        f.state = State::Starting;
        f.error = None;
        // Both belong to the session that just ended. The uptime in particular
        // is published from here through the whole start — the loop is blocked,
        // so `publish` cannot correct it — and it read as time already spent
        // online on a server that had not connected yet.
        f.uptime = String::new();
        f.latency_ms = None;
    });

    let exe = match engine_path(setup) {
        Ok(exe) => exe,
        Err(e) => return fail(frames, &e),
    };
    let needs = bliss_xray::minimum_xray_version(server);
    let version = match bliss_xray::check_version(&exe, needs).await {
        Ok(v) => v,
        Err(e) => return fail(frames, &e.to_string()),
    };
    if let bliss_proto::Security::Tls(tls) = &server.security
        && tls.allow_insecure
        && tls.pinned.is_empty()
    {
        tracing::warn!(
            "{}: the link asks not to check the server's certificate, which xray no longer \
             allows; without a pinned certificate (pcs) a self-signed server will be refused",
            server.tag
        );
    }

    let port = setup.profile.engine.bridge_port;
    let config = bliss_xray::build(server, port, setup.engine_log, version);
    let xray = match bliss_xray::Xray::spawn(&exe, &config, &setup.work_dir, port).await {
        Ok(x) => x,
        Err(e) => return fail(frames, &e.to_string()),
    };

    let outbound = Arc::new(SocksOutbound::new(
        xray.socks_addr(),
        EngineInfo {
            kind: "xray-core",
            version: version.to_string(),
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
                // Name the program holding it. This is the most common way a
                // start fails — another client of the same family uses the same
                // two ports — and "занят другой программой" sent the user
                // looking at bliss itself for an evening, while v2rayN sat on
                // 10808. The table that answers this is the one the connections
                // screen already uses.
                let hint = if e.kind() == std::io::ErrorKind::AddrInUse {
                    match bliss_core::name_for_port(addr.port()) {
                        Some(who) => format!(
                            "порт {addr} занят программой {who} — закройте её или задайте bliss другие порты в профиле"
                        ),
                        None => format!(
                            "порт {addr} уже занят другой программой — другим клиентом или прошлой копией bliss"
                        ),
                    }
                } else {
                    format!("не удалось занять {addr}: {e}")
                };
                return fail(frames, &hint);
            }
        }
    }

    let serving = Arc::new(Serving::new(
        Arc::clone(&outbound),
        Arc::clone(tracker),
        Arc::clone(router),
    ));

    let mut inbounds = Vec::new();
    for listener in bound {
        let serving = Arc::clone(&serving);
        inbounds.push(tokio::spawn(async move {
            if let Err(e) = bliss_core::serve(listener, serving).await {
                tracing::error!("inbound stopped: {e}");
            }
        }));
    }

    // After the inbounds, because the adapter starts carrying traffic the
    // moment its routes go in, and it carries it into `serving`. Not waited
    // for: the tunnel is up either way, and the adapter joins it when it is
    // ready.
    let tun_rising = begin_tun(setup, server, &serving, own, frames);

    frames.send_modify(|f| {
        f.state = State::Running;
        f.engine = format!("xray-core {version}");
        f.error = None;
    });

    let bridge = xray.socks_addr();
    Some(Running {
        xray,
        inbounds,
        tun: None,
        tun_rising,
        since: Instant::now(),
        serving,
        bridge,
    })
}

/// Bring the adapter up when the profile asks for one, and put what happened
/// in the frame either way.
///
/// It never fails the tunnel. The two ports carry traffic perfectly well
/// without an adapter, and a bliss that refused to start because it could not
/// create one would take away more than it explains — especially at sign-in,
/// where the Run key starts it without administrator rights and there is no
/// way for it to ask for them.
fn begin_tun(
    setup: &Setup,
    server: &ServerConfig,
    serving: &Arc<Serving>,
    own: &mpsc::WeakUnboundedSender<Command>,
    frames: &watch::Sender<Frame>,
) -> bool {
    if !setup.tun {
        publish_tun(frames, false, false, None);
        return false;
    }
    if !bliss_tun::elevated() {
        tracing::warn!("tun: asked for, but this bliss has no administrator rights");
        publish_tun(frames, true, false, None);
        return false;
    }
    // Asked for, not there yet: the switch shows this as its own state rather
    // than pretending either of the other two.
    publish_tun(frames, true, false, None);
    raise_tun(setup, server, serving, own);
    true
}

/// What the settings screen shows about the adapter.
///
/// Whether the rights are there is asked at every publish rather than once at
/// startup: it cannot change inside one process, but reading it here keeps the
/// three facts in one place and costs a token query.
fn publish_tun(frames: &watch::Sender<Frame>, on: bool, carrying: bool, problem: Option<String>) {
    let elevated = bliss_tun::elevated();
    frames.send_modify(|f| {
        f.tun = TunView {
            on,
            elevated,
            carrying,
            problem,
        };
    });
}

/// Start bringing the adapter up; the answer comes back through the loop.
///
/// The same path a finished subscription takes, and for the same reason: this
/// takes seconds — the first adapter on a machine installs a driver — and
/// every click the window makes in the meantime would otherwise wait behind
/// it. The engine loop answering nothing is a program that has hung, which is
/// exactly how this was found.
///
/// The server's name is resolved on this machine, which is the one other place
/// bliss does that, alongside the latency probe. It has to be: the route that
/// keeps the tunnel's own traffic out of the tunnel names an address, and the
/// engine will be dialling whatever that name resolves to.
fn raise_tun(
    setup: &Setup,
    server: &ServerConfig,
    serving: &Arc<Serving>,
    own: &mpsc::WeakUnboundedSender<Command>,
) {
    let host = server.address.to_string();
    let port = server.port;
    let work_dir = setup.work_dir.clone();
    let serving = Arc::clone(serving);
    let own = own.clone();

    tokio::spawn(async move {
        let result = match around_the_tunnel(&host, port).await {
            Ok(around) => {
                let dir = work_dir;
                let carrying = Arc::clone(&serving);
                // Creating the adapter is blocking work, so it goes to a
                // thread that is allowed to block.
                let raising = tokio::task::spawn_blocking(move || {
                    bliss_tun::Tun::up(&dir, carrying, &around)
                });
                match tokio::time::timeout(TUN_TIMEOUT, raising).await {
                    Ok(Ok(Ok(tun))) => Ok(tun),
                    Ok(Ok(Err(e))) => Err(format!("не удалось поднять адаптер: {e}")),
                    Ok(Err(e)) => Err(format!("поток адаптера не дожил до конца: {e}")),
                    // A blocking call cannot be cancelled: it runs to its own
                    // end on its own thread, and whatever it built is dropped
                    // there — the adapter with it, since that is all `Tun` is.
                    Err(_) => Err(format!(
                        "адаптер не поднялся за {} с",
                        TUN_TIMEOUT.as_secs()
                    )),
                }
            }
            Err(e) => Err(e),
        };
        if let Some(own) = own.upgrade() {
            let _ = own.send(Command::TunUp(Box::new(TunRaised { serving, result })));
        }
    });
}

/// Every address of the server, for the route that keeps the tunnel's own
/// traffic outside it.
async fn around_the_tunnel(host: &str, port: u16) -> Result<Vec<std::net::Ipv4Addr>, String> {
    let found = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("не удалось узнать адрес сервера {host}: {e}"))?;
    let around: Vec<std::net::Ipv4Addr> = found
        .filter_map(|addr| match addr {
            SocketAddr::V4(v4) => Some(*v4.ip()),
            // A server reached over IPv6 would need a route of its own, and
            // IPv6 is not carried yet either way.
            SocketAddr::V6(_) => None,
        })
        .collect();
    if around.is_empty() {
        return Err(format!(
            "у сервера {host} нет адреса IPv4, а TUN пока несёт только IPv4"
        ));
    }
    Ok(around)
}

/// How long the adapter gets before bliss stops waiting for it.
///
/// Half a minute, because the first adapter on a machine installs a driver.
/// Nothing else waits on this — the loop went back to work the moment it was
/// asked for — so the only thing the timeout buys is a line that says what
/// happened instead of a switch that stays violet forever.
const TUN_TIMEOUT: Duration = Duration::from_secs(30);

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

// --- probing the server list ----------------------------------------------

/// How long a probe waits before calling a server unreachable. Short on
/// purpose: this runs against a whole list, and a dead entry should not hold
/// the sweep up for the live ones behind it.
const PING_TIMEOUT: Duration = Duration::from_secs(4);

/// Probes in flight at once. A subscription can hold a few hundred servers,
/// and opening that many sockets in one breath is both rude and the kind of
/// thing a firewall takes an interest in.
const PING_CONCURRENCY: usize = 12;

/// The program's one sweep: a permit set and a flag saying whether a sweep is
/// already under way.
///
/// Both used to be per call. The permit set being per call made the cap
/// per sweep rather than absolute, and nothing stopped a second sweep starting,
/// so a button pressed N times opened `12 * N` sockets at once and let an older
/// probe land on top of a newer one.
struct Sweep {
    permits: tokio::sync::Semaphore,
    running: std::sync::atomic::AtomicBool,
}

impl Sweep {
    fn new() -> Self {
        Self {
            permits: tokio::sync::Semaphore::new(PING_CONCURRENCY),
            running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Claim the right to sweep, or report that someone else holds it.
    fn begin(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn end(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Measure every server and write the results into the frame as they land.
///
/// Returns immediately: results arrive one at a time over the following
/// seconds, which is also how they should appear on screen.
fn ping_all(servers: &[ServerConfig], frames: &watch::Sender<Frame>, sweep: &Arc<Sweep>) {
    if !sweep.begin() {
        tracing::debug!("a server sweep is already running");
        return;
    }

    let targets: Vec<(usize, String, u16)> = servers
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.over_udp())
        .map(|(i, s)| (i, s.address.to_string(), s.port))
        .collect();

    frames.send_modify(|f| {
        for (row, server) in f.servers.iter_mut().zip(servers) {
            row.ping = if server.over_udp() {
                Ping::Udp
            } else {
                Ping::Probing
            };
        }
    });

    let frames = frames.clone();
    let sweep = Arc::clone(sweep);
    tokio::spawn(async move {
        let mut probes = Vec::with_capacity(targets.len());
        for (index, host, port) in targets {
            let sweep = Arc::clone(&sweep);
            let frames = frames.clone();
            probes.push(tokio::spawn(async move {
                let _permit = sweep.permits.acquire().await;
                let measured = tcp_ping(&host, port).await;
                frames.send_modify(|f| {
                    if let Some(row) = f.servers.get_mut(index) {
                        row.ping = match measured {
                            Some(ms) => Ping::Ok(ms),
                            None => Ping::Down,
                        };
                    }
                });
            }));
        }
        for probe in probes {
            let _ = probe.await;
        }
        sweep.end();
    });
}

/// Time to complete a TCP handshake with a server's edge.
///
/// Not a round trip through the tunnel. Nothing is negotiated here — no TLS,
/// no REALITY — so a server can answer this and still refuse to carry a byte.
/// It is what every client in this ecosystem calls a server's ping, and it is
/// cheap enough to run across a whole subscription, but the column is labelled
/// for what it measures rather than for what one would like it to mean.
///
/// DNS is resolved before the clock starts: a cold lookup can cost more than
/// the connection that follows it, and it says nothing about the server.
async fn tcp_ping(host: &str, port: u16) -> Option<u32> {
    let addr = tokio::time::timeout(PING_TIMEOUT, tokio::net::lookup_host((host, port)))
        .await
        .ok()?
        .ok()?
        .next()?;

    let started = Instant::now();
    tokio::time::timeout(PING_TIMEOUT, tokio::net::TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;
    Some(started.elapsed().as_millis() as u32)
}

/// Open one of bliss's own files or folders the way a double click would.
///
/// Through `explorer.exe` rather than `ShellExecuteW`: it performs the same
/// default action, needs no unsafe block and no COM on this thread, and is the
/// one program certain to be present. A missing association shows Windows' own
/// "open with" picker, which is the correct answer to a `.toml` nobody has
/// claimed yet.
#[cfg(windows)]
fn reveal(path: &std::path::Path) {
    tracing::info!("opening {}", path.display());
    if let Err(e) = std::process::Command::new("explorer.exe").arg(path).spawn() {
        tracing::warn!("could not open {}: {e}", path.display());
    }
}

#[cfg(not(windows))]
fn reveal(path: &std::path::Path) {
    tracing::warn!("cannot open {} on this platform", path.display());
}

struct Meter {
    up: u64,
    down: u64,
    /// The chart's current full-scale value. See `snap_scale`.
    scale: u64,
    /// When these counters were read.
    ///
    /// Rates used to be computed by dividing by the tick interval, on the
    /// assumption that a tick is when the last one was taken. Starting the
    /// engine blocks the loop for seconds, so the first reading afterwards
    /// covered all of that time and was still divided by 500 ms — a twenty-fold
    /// overstatement, which then set the chart's scale and flattened every real
    /// bar after it.
    at: Instant,
}

impl Meter {
    fn new() -> Self {
        Self {
            up: 0,
            down: 0,
            scale: 0,
            at: Instant::now(),
        }
    }
}

/// Samples in the chart's history: forty intervals across the chart, twenty
/// seconds at a tick each, and one more past each edge, where a sample waits to
/// glide in or finishes gliding out between ticks (`TrafficTab` in ui.rs).
pub(crate) const CHART_SAMPLES: usize = 42;

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
/// and reads as a deliberate change of range rather than as drift. The window
/// eases into a new range rather than snapping to it (`chart_lift` in ui.rs).
fn snap_scale(peak: u64) -> u64 {
    let mut bucket = SCALE_FLOOR;
    while bucket < peak {
        bucket = bucket.saturating_mul(2);
    }
    bucket
}

/// Bytes per second, over however long it has actually been.
///
/// The span is a parameter rather than the tick constant because the loop that
/// calls this is not always ticking: starting the engine blocks it for as long
/// as the engine takes to come up, and dividing that whole stretch of traffic
/// by half a second drew a spike that had never happened — which then set the
/// chart's scale and squashed everything real that followed.
fn per_second(now: u64, before: u64, span_ms: u64) -> u64 {
    now.saturating_sub(before) * 1000 / span_ms.max(1)
}

fn publish(
    frames: &watch::Sender<Frame>,
    tracker: &Tracker,
    last: &mut Meter,
    running: Option<&Running>,
) {
    let (up, down, opened) = tracker.totals();
    let now = Instant::now();
    // Real elapsed time, not the nominal tick: they are the same while the loop
    // is idle and wildly different across a start.
    let span_ms = now.duration_since(last.at).as_millis() as u64;
    let up_rate = per_second(up, last.up, span_ms);
    let down_rate = per_second(down, last.down, span_ms);
    let connections = tracker.table();
    // Counted from the rows themselves rather than asked for separately, so the
    // number in the metrics card cannot disagree with the list beside it.
    let active = connections.iter().filter(|c| c.closed_at.is_none()).count() as u64;
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
        // The adapter goes down with the tunnel, and the tunnel goes down from
        // half a dozen places — a failed engine among them, which is nobody's
        // click. Read from what is actually held rather than set at each of
        // those places, so the screen cannot say "carrying" over nothing.
        f.tun.carrying = running.is_some_and(|r| r.tun.is_some());

        // A fixed-length ring: the chart shows the recent past and the vector
        // never grows, which is the rule for anything that ticks forever.
        f.history.remove(0);
        f.history.push(down_rate);
        f.samples = f.samples.wrapping_add(1);

        let peak = f.history.iter().copied().max().unwrap_or(0);
        scale = snap_scale(peak);
        f.scale = scale;
    });

    // Written back after the closure has run, so the smoothing carries into the
    // next tick instead of being recomputed from nothing every time.
    *last = Meter {
        up,
        down,
        scale,
        at: now,
    };
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

    /// The list's column is 136px: the short form has to fit it, and the full
    /// one, under the pointer and on the status card, says the rest.
    #[test]
    fn a_server_is_named_briefly_and_in_full() {
        let server = |link: &str| link.parse::<ServerConfig>().expect("the link parses");
        let reality = server(
            "vless://11111111-2222-3333-4444-555555555555@a.example:443?type=xhttp\
             &security=reality&sni=b.example&pbk=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ\
             &flow=xtls-rprx-vision",
        );
        assert_eq!(kind(&reality), "VLESS · REALITY");
        assert_eq!(how_it_hides(&reality), "VLESS · xhttp · reality · vision");

        let trojan = server("trojan://PPPP@a.example:443");
        assert_eq!(kind(&trojan), "Trojan · TLS");
        assert_eq!(how_it_hides(&trojan), "Trojan · tls");

        // TLS is Hysteria's nature, not a choice worth a word.
        let hysteria = server("hy2://PPPP@a.example/?obfs=salamander&obfs-password=OOOO");
        assert_eq!(kind(&hysteria), "Hysteria2");
        assert_eq!(how_it_hides(&hysteria), "Hysteria2 · salamander");

        let plain = server("vless://11111111-2222-3333-4444-555555555555@a.example:443");
        assert_eq!(
            (kind(&plain), how_it_hides(&plain)),
            ("VLESS".to_owned(), "VLESS".to_owned())
        );
        for name in [&reality, &trojan, &hysteria, &plain].map(kind) {
            assert!(
                name.chars().count() <= 16,
                "{name} is too long for its column"
            );
        }
    }

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

    /// The rate has to be per second of real time, not per nominal tick.
    ///
    /// Starting the engine blocks the loop that publishes these, so the first
    /// reading afterwards spans seconds. Dividing it by the tick reported a
    /// burst of traffic that never happened, at whatever multiple the stall
    /// happened to be.
    #[test]
    fn a_rate_is_measured_against_the_time_that_actually_passed() {
        // A quiet tick: 32 KB in half a second is 64 KB/s.
        assert_eq!(per_second(32_768, 0, 500), 65_536);

        // The same 32 KB, but the loop was stuck in a start for twelve seconds.
        // The old code divided by 500 regardless and drew 64 KB/s.
        assert_eq!(per_second(32_768, 0, 12_500), 2_621);

        // Counters only ever climb, but a restart must not be able to produce a
        // negative delta if one ever did.
        assert_eq!(per_second(5, 10, 500), 0);
        // A span of zero is possible between two publishes in the same
        // millisecond and must not divide by zero.
        assert_eq!(per_second(1_000, 0, 0), 1_000_000);
    }

    /// Two sweeps at once was how one button could open twelve sockets per
    /// click instead of twelve in total.
    #[test]
    fn only_one_server_sweep_runs_at_a_time() {
        let sweep = Sweep::new();
        assert!(sweep.begin(), "the first caller gets it");
        assert!(!sweep.begin(), "the second is turned away");
        assert!(!sweep.begin());
        sweep.end();
        assert!(sweep.begin(), "and it is available again afterwards");
    }

    /// The window opens with no profile and no server, so nothing about its
    /// first frame may assume there is a server to describe.
    #[test]
    fn a_window_without_servers_gets_a_frame() {
        // Never created: no profile in it, and no engine either.
        let dir = std::env::temp_dir()
            .join("bliss-engine-tests")
            .join("no-servers-nothing-here");
        let setup = Setup {
            profile: Profile::default(),
            servers: Vec::new(),
            selected: 0,
            work_dir: dir.clone(),
            profile_path: dir.join("bliss.toml"),
            engine_log: bliss_xray::LogLevel::Warning,
            autostart: true,
            tun: false,
            problem: Some("профиль не читается".to_owned()),
        };
        assert!(setup.server(setup.selected).is_none());

        let frame = initial_frame(&setup);
        assert!(frame.servers.is_empty());
        assert!(frame.server_tag.is_empty());
        assert_eq!(frame.error.as_deref(), Some("профиль не читается"));
        assert!(
            engine_path(&setup).is_err(),
            "no engine in a work dir that does not exist"
        );
    }

    /// A link is a credential, `Command` is `Debug`, and a `{command:?}` in a
    /// log line is one careless edit away.
    #[test]
    fn a_pasted_link_does_not_show_in_debug_output() {
        let command = Command::Import {
            text: Link(
                "vless://11111111-2222-3333-4444-555555555555@a.example:443?encryption=none"
                    .to_owned(),
            ),
            from_form: true,
        };
        let shown = format!("{command:?}");
        assert!(
            !shown.contains("11111111") && !shown.contains("a.example"),
            "{shown}"
        );
    }

    /// A rule made of names alone decides nothing that comes through the
    /// adapter, because a packet holds an address. The user was told this once
    /// already about `geoip:ru` from the other side, and found it by watching
    /// traffic go the wrong way for days; the same mistake in the other
    /// direction gets said out loud rather than discovered.
    #[test]
    fn rules_of_names_alone_are_named_under_tun() {
        let rule = |name: &str, domain: Vec<&str>, ip: Vec<&str>| Rule {
            name: Some(name.to_owned()),
            action: Action::Proxy,
            domain: domain.into_iter().map(str::to_owned).collect(),
            ip: ip.into_iter().map(str::to_owned).collect(),
            ..Rule::default()
        };
        let names = rule("сайты", vec!["a.example"], vec![]);
        let both = rule("и то и то", vec!["b.example"], vec!["203.0.113.0/24"]);
        let addresses = rule("адреса", vec![], vec!["203.0.113.7"]);

        let note = blind_under_tun(std::slice::from_ref(&names)).expect("a rule of names is named");
        assert!(note.contains("правило #1"), "{note}");
        assert!(note.contains("не сработает"), "{note}");

        // Half of this one still decides, so naming it would bury the ones
        // that decide nothing at all.
        assert_eq!(blind_under_tun(&[both.clone(), addresses.clone()]), None);

        let two = blind_under_tun(&[names.clone(), addresses, names])
            .expect("two of them are named together");
        assert!(two.contains("правила #1, #3"), "{two}");
        assert!(two.contains("не сработают"), "{two}");
        assert_eq!(blind_under_tun(&[]), None, "nothing to warn about");

        // A switched-off rule decides nothing anywhere, so it is not named.
        let off = Rule {
            enabled: false,
            ..rule("выключено", vec!["c.example"], vec![])
        };
        assert_eq!(blind_under_tun(&[off]), None);
    }

    /// And the note is there only while the adapter is: the same rules are
    /// perfectly good for everything arriving at the two ports.
    #[test]
    fn the_note_about_names_comes_and_goes_with_the_adapter() {
        let routing = bliss_proto::Routing {
            fallback: Action::Proxy,
            rules: vec![Rule {
                name: Some("сайты".to_owned()),
                action: Action::Proxy,
                domain: vec!["a.example".to_owned()],
                ..Rule::default()
            }],
            ..bliss_proto::Routing::default()
        };
        let rules = Rules::compile(routing, std::path::Path::new("nonexistent"));
        assert!(rules.view(false).warnings.is_empty());
        assert_eq!(rules.view(true).warnings.len(), 1);
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
