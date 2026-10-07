//! bliss — a VLESS + REALITY client for Windows.

// No console window beside the real one. A console-subsystem program gets a
// console from Windows before `main` runs, so a double-click opened a black
// window next to bliss that did nothing but echo a log already written to
// bliss.log. The commands that print join the terminal they were started from
// instead: see `attach_console`.
#![windows_subsystem = "windows"]

// Binary crates only. A `#[global_allocator]` in a library would be imposed on
// everything that links it, which is not a library's call to make.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod clipboard;
mod engine;
mod icons;
mod logs;
mod profile_file;
mod rule_text;
mod startup;
mod subscription;
mod ui;

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;

use bliss_proto::{Encryption, Profile, Protocol, Security, ServerConfig};
use clap::{Parser, Subcommand};
use color_eyre::eyre::{Context, Result, bail, eyre};
use tracing_error::ErrorLayer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser)]
#[command(name = "bliss", version, about = "VLESS + REALITY client")]
struct Cli {
    /// Open the window minimized. Windows passes this when it starts bliss at
    /// sign-in and the settings ask for a minimized window.
    #[arg(long)]
    minimized: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and validate a profile, or a single link read from stdin.
    Check {
        /// Profile to check. Defaults to the one in the bliss config directory.
        path: Option<PathBuf>,
        /// Read one server link (vless://, vmess://, trojan://, ss://, …) from
        /// stdin instead of reading a profile.
        ///
        /// stdin rather than an argument: a link carries a live UUID and Reality
        /// keys, and an argument lands in shell history and the process table.
        #[arg(long, conflicts_with = "path")]
        link: bool,
    },
    /// Manage the xray-core engine bliss runs.
    Engine {
        #[command(subcommand)]
        what: EngineCmd,
    },
    /// Ask which rule decides a destination, and what it decides.
    ///
    /// The routing screen's "check a domain", before there is a screen. Answers
    /// without connecting to anything.
    Route {
        /// A name or an address: `example.com`, `youtube.com:443`, `1.2.3.4`.
        target: String,
        /// Pretend the connection came from this program, for process rules.
        #[arg(long, value_name = "NAME")]
        process: Option<String>,
        /// Profile to read the rules from. Defaults to %APPDATA%\bliss\bliss.toml.
        path: Option<PathBuf>,
    },
    /// Start the engine and serve traffic.
    Run {
        #[arg(value_name = "PROFILE")]
        path: Option<PathBuf>,
        /// Which server to use. Defaults to the first in the profile.
        #[arg(long, value_name = "TAG")]
        server: Option<String>,
        /// How talkative the engine should be: warning, info, debug.
        #[arg(long, default_value = "warning")]
        engine_log: String,
        /// Also carry the machine's traffic through a TUN adapter, which is
        /// the only way UDP goes through the tunnel — calls, games, QUIC.
        ///
        /// Needs administrator rights: the adapter and its routes are not
        /// something a normal process may create. Run it from an elevated
        /// console.
        #[arg(long)]
        tun: bool,
    },
}

#[derive(Subcommand)]
enum EngineCmd {
    /// Show recent xray-core releases.
    List {
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Download an engine into %LOCALAPPDATA%\bliss\data and verify its checksum.
    Install {
        /// A specific version, e.g. 26.7.28. Without it: the newest stable
        /// release, or the newest of any kind with --newest.
        #[arg(long)]
        version: Option<String>,
        /// Take the newest release even if it is flagged prerelease. Most
        /// xray-core releases are, so this is often much newer than the
        /// default — and correspondingly less proven.
        #[arg(long)]
        newest: bool,
        /// Download through a SOCKS5 proxy — typically a bliss bridge that is
        /// already up, for when GitHub itself is what cannot be reached.
        #[arg(long, value_name = "ADDR")]
        via: Option<SocketAddr>,
    },
}

fn main() -> Result<()> {
    // Before anything can print, clap's own --help and usage errors included.
    // The window prints nowhere; see `wants_console`.
    if wants_console(std::env::args_os().skip(1)) {
        attach_console();
    }
    color_eyre::install()?;
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_env("BLISS_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_target(false).without_time())
        .with(file_layer())
        .with(logs::Recorder)
        .with(ErrorLayer::default())
        .init();

    tracing::info!(
        "--- bliss {} started, {} ---",
        env!("CARGO_PKG_VERSION"),
        std::env::args()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end()
    );

    // Before anything else. A run that was killed rather than closed leaves the
    // machine pointed at a port nobody is listening on, and from the user's
    // chair that is "the internet stopped working" with nothing to go on. The
    // snapshot on disk is the only part of that cleanup which survives being
    // killed, so honouring it is the first thing bliss does.
    if let Ok(dir) = work_dir() {
        match bliss_core::sysproxy::recover(&dir) {
            Ok(true) => tracing::warn!(
                "a previous run left the system proxy set; put the old settings back"
            ),
            Ok(false) => {}
            Err(e) => tracing::error!("could not undo a previous run's system proxy: {e}"),
        }
        guard_system_proxy(dir);
    }

    let parsed = Cli::parse();
    match parsed.command {
        // No subcommand is the product: the window. The CLI is for setting it
        // up and for looking under the hood.
        None => window(parsed.minimized).inspect_err(report_to_user),
        Some(command) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .wrap_err("could not start a runtime")?
            .block_on(cli(command)),
    }
}

async fn cli(command: Command) -> Result<()> {
    match command {
        Command::Check { path, link: true } => {
            debug_assert!(path.is_none(), "clap enforces this");
            check_one_link()
        }
        Command::Check { path, .. } => check_profile(path),
        Command::Engine { what } => engine_cmd(what).await,
        Command::Route {
            target,
            process,
            path,
        } => explain_route(path, &target, process.as_deref()),
        Command::Run {
            path,
            server,
            engine_log,
            tun,
        } => run(path, server, &engine_log, tun).await,
    }
}

/// The claim on being *the* bliss window, held for as long as the process runs.
///
/// A second copy is worse than useless: it shares the work dir, the profile,
/// the ports and — through dioxus-desktop, which names WebView2's data folder
/// after the executable — that folder too. WebView2 will not give a second
/// process an environment over the same folder, so the second copy **never
/// gets a webview and therefore never shows a window**. Measured on 20
/// September: the second and third starts each sat there with no child process
/// and no page while the first went on working, and from the outside that is a
/// program that started and did nothing. The user spent an evening on it,
/// clicking a window that belonged to the copy already running.
#[cfg(windows)]
struct OnlyWindow(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for OnlyWindow {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateMutexW and is closed once, here.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Take the claim, or answer `None` when another window already has it.
///
/// Session-local (`Local\`), not machine-wide: another user signed in at the
/// same time has their own profile and their own ports, and has every right to
/// their own window.
///
/// Named after the executable, because that is what the thing being protected
/// is named after: dioxus-desktop calls WebView2's data folder
/// `%LOCALAPPDATA%\<exe stem>`, and it is one browser process per folder. Two
/// copies of `bliss.exe` anywhere on the disk share it and must not both run;
/// a copy renamed `bliss-probe.exe` has a folder of its own and may (§7).
#[cfg(windows)]
fn only_window() -> Option<OnlyWindow> {
    claim(&claim_name())
}

/// `bliss-window-<exe stem>`, with anything a mutex name cannot hold replaced.
///
/// A name may not contain a backslash — that separates the namespace — and
/// the stem comes from the file system, so it is filtered rather than trusted.
#[cfg(windows)]
fn claim_name() -> String {
    let stem = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "bliss".to_owned());
    let kept: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    format!(
        "Local\\bliss-window-{}",
        if kept.is_empty() { "bliss" } else { &kept }
    )
}

/// The claim itself, by name, so a test can take one of its own instead of
/// fighting whichever bliss the person has open — the same trick as
/// `BLISS_STARTUP_KEY` for the Run key.
#[cfg(windows)]
fn claim(name: &str) -> Option<OnlyWindow> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::HSTRING;

    let name = HSTRING::from(name);
    // SAFETY: a named mutex with no security attributes; the name is ours, and
    // lives until the call returns.
    let claim = unsafe { CreateMutexW(None, true, windows::core::PCWSTR(name.as_ptr())) }.ok()?;
    // The handle is valid either way: what says whether it is ours is the last
    // error, which has to be read before anything else can overwrite it.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // Closed here rather than held: this copy is leaving. What to do about
        // it is the caller's, so that taking the claim is all this does.
        drop(OnlyWindow(claim));
        return None;
    }
    Some(OnlyWindow(claim))
}

/// Bring the window that is already running to the front, if it is on screen.
///
/// Best effort, and only that: a copy hiding in the tray cannot be shown from
/// out here. Its window belongs to tao, which keeps its own idea of whether it
/// is visible and undoes anything done behind its back (rule 20 in AGENTS.md),
/// so this raises a window that is already up and never tries to show one.
#[cfg(windows)]
fn raise_the_running_window() {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsWindowVisible, SetForegroundWindow,
    };
    use windows::core::w;

    // tao's class for every window it makes, and the title `launch` gives ours.
    // SAFETY: both arguments are literals; a window that has gone since is
    // reported as not found rather than used.
    let Ok(window) = (unsafe { FindWindowW(w!("Window Class"), w!("bliss")) }) else {
        tracing::info!("the running bliss is in the tray; its icon is by the clock");
        return;
    };
    if unsafe { IsWindowVisible(window) }.as_bool() {
        let _ = unsafe { SetForegroundWindow(window) };
    } else {
        tracing::info!("the running bliss is in the tray; its icon is by the clock");
    }
}

#[cfg(not(windows))]
struct OnlyWindow;

#[cfg(not(windows))]
fn only_window() -> Option<OnlyWindow> {
    Some(OnlyWindow)
}

/// Open the window.
///
/// This thread belongs to the event loop from here on — every windowing library
/// insists on the main thread — so the engine gets a runtime of its own.
fn window(minimized: bool) -> Result<()> {
    // Only one window at a time, and this is where a second copy stops.
    let Some(_only) = only_window() else {
        tracing::info!("another bliss already has the window; leaving it to that one");
        raise_the_running_window();
        return Ok(());
    };
    let setup = window_setup()?;
    let work_dir = setup.work_dir.clone();
    let controller = engine::spawn(setup).wrap_err("could not start the engine thread")?;
    ui::launch(controller, minimized);

    // The engine thread is detached, so it may not get to run its own cleanup
    // when the event loop ends and the process winds down. This is the ordinary
    // close path and it happens here, on the thread that is definitely alive.
    if let Err(e) = bliss_core::sysproxy::restore(&work_dir) {
        tracing::error!("could not restore the system proxy on the way out: {e}");
    }
    Ok(())
}

/// Put the machine's proxy settings back if we panic.
///
/// Not the safety net — a panic in a worker thread does not end the process,
/// and a kill runs no hooks at all. The startup check in `main` is what this
/// actually relies on; this just shortens the window in the one case where
/// some of our code still gets to run.
fn guard_system_proxy(work_dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = bliss_core::sysproxy::restore(&work_dir);
        previous(info);
    }));
}

/// Everything the window needs, without insisting on anything.
///
/// A first run has no profile and no server, and a hand-edited profile can
/// stop parsing. Neither is a reason for a message box instead of a window:
/// the window is where a server gets added and where the problem is shown.
fn window_setup() -> Result<engine::Setup> {
    let path = default_profile_path()?;
    let (profile, problem) = match profile_file::read(&path) {
        Ok(profile) => (profile.unwrap_or_default(), None),
        Err(e) => {
            // The brief form in the log too: the full one quotes the offending
            // line, and in a profile that line can be a link.
            tracing::error!("profile: {}", e.brief());
            (
                Profile::default(),
                // Not a dead end: the first change saves a new profile, and
                // this one is kept beside it (`profile_file::Store::save`).
                Some(format!(
                    "профиль не читается: {}. Чинить не обязательно: при первом изменении \
                     в окне bliss сохранит новый, а этот оставит рядом копией",
                    e.brief()
                )),
            )
        }
    };
    let (servers, failed) = profile.parse_servers();
    for (i, err) in &failed {
        tracing::warn!("server #{} in the profile is unusable - {err}", i + 1);
    }
    Ok(engine::Setup {
        servers,
        selected: 0,
        work_dir: work_dir()?,
        profile_path: path,
        engine_log: bliss_xray::LogLevel::Warning,
        autostart: true,
        // Honoured only by a copy that has the rights for it; the settings
        // screen says so when this one does not.
        tun: profile.tun.enabled,
        profile,
        problem,
    })
}

/// Everything `bliss run` needs, gathered from the profile once.
///
/// Stricter than the window: a terminal has nowhere to ask for a server, so a
/// missing profile, or one without a usable server, is an error here.
fn prepare(path: Option<PathBuf>, want: Option<String>, engine_log: &str) -> Result<engine::Setup> {
    let (profile, path) = load_profile(path)?;
    let Some(profile) = profile else {
        bail!(
            "no profile yet at {}. Add a server in the bliss window, which writes it, or pass the path of a profile.",
            path.display()
        );
    };
    let (servers, failed) = profile.parse_servers();
    for (i, err) in &failed {
        tracing::warn!("server #{} in the profile is unusable - {err}", i + 1);
    }

    // An index, not a copy: the window can switch between all of them, so the
    // list has to travel intact and the choice is just a number into it.
    let selected = match &want {
        Some(tag) => servers
            .iter()
            .position(|s| &s.tag == tag)
            .ok_or_else(|| eyre!("no server tagged {tag:?} in {}", path.display()))?,
        None if servers.is_empty() => {
            // "No server" and "the server did not parse" are different
            // problems with different fixes, and telling the user the wrong
            // one sends them looking in the wrong place.
            match failed.first() {
                Some((i, why)) => bail!(
                    "server #{} in {} could not be read: {why}",
                    i + 1,
                    path.display()
                ),
                None => bail!(
                    "no server in {}. Add one in the bliss window, or paste a server link into the file — the [[server]] block at the bottom shows where.",
                    path.display()
                ),
            }
        }
        None => 0,
    };

    Ok(engine::Setup {
        work_dir: work_dir()?,
        engine_log: parse_log_level(engine_log)?,
        // The profile's choice; `run --tun` turns it on for this run alone.
        tun: profile.tun.enabled,
        autostart: true,
        profile_path: path,
        profile,
        servers,
        selected,
        problem: None,
    })
}

// --- routing --------------------------------------------------------------

/// Say which rule claims a destination, and what it does with it.
///
/// The whole point of recording a rule name on every connection is that "why
/// did this go direct?" has an answer; this is that question asked ahead of
/// time, so a rule can be checked before it is trusted with real traffic.
fn explain_route(path: Option<PathBuf>, target: &str, process: Option<&str>) -> Result<()> {
    let (profile, path) = load_profile(path)?;
    let profile = profile.unwrap_or_else(|| {
        println!(
            "(профиля {} пока нет — действуют значения по умолчанию)\n",
            path.display()
        );
        Profile::default()
    });
    let dir = profile.routing.geo_dir.clone().unwrap_or(work_dir()?);

    let router = bliss_core::Router::build(&profile.routing, &dir);
    for warning in &router.warnings {
        println!("  ! {warning}");
    }
    if !router.warnings.is_empty() {
        println!();
    }

    // Accepted in the three shapes someone would type: `example.com`,
    // `example.com:8080`, and a bare address. A port is assumed only when there
    // is none to read, and it is 443 because that is what nearly everything is.
    let parsed = if let Ok(ip) = target.parse::<std::net::IpAddr>() {
        bliss_core::Target::Socket(SocketAddr::new(ip, 443))
    } else if target.contains(':') && !target.starts_with('[') {
        bliss_core::Target::parse_authority(target)
            .wrap_err_with(|| format!("cannot read {target:?} as a destination"))?
    } else {
        bliss_core::Target::parse_authority(target)
            .unwrap_or_else(|_| bliss_core::Target::domain(target, 443))
    };

    let decision = router.decide(&parsed, process);
    println!("{parsed}");
    if let Some(process) = process {
        println!("  как будто из {process}");
    }
    println!(
        "  {} — правило «{}»",
        match decision.route {
            bliss_core::Route::Proxy => "через прокси",
            bliss_core::Route::Direct => "напрямую, мимо туннеля",
            bliss_core::Route::Blocked => "заблокировано",
            bliss_core::Route::Vpn => "через рабочий VPN, когда он подключён в окне",
        },
        decision.rule
    );
    if let Some(note) = router.geoip_note(&parsed, process) {
        println!("  {note}");
    }

    if router.rule_count() == 0 {
        println!();
        println!("В профиле нет правил, поэтому решает только [routing] fallback.");
    }
    Ok(())
}

// --- engine ---------------------------------------------------------------

async fn engine_cmd(what: EngineCmd) -> Result<()> {
    match what {
        EngineCmd::List { limit } => {
            let client = bliss_xray::client(None)?;
            let releases = bliss_xray::list(&client, limit)
                .await
                .wrap_err("could not list xray-core releases")?;
            println!("{:<12} {:<12} channel", "version", "published");
            for r in &releases {
                println!(
                    "{:<12} {:<12} {}",
                    r.version(),
                    &r.published[..10],
                    if r.prerelease { "prerelease" } else { "stable" }
                );
            }
            if releases.iter().all(|r| r.prerelease) {
                println!();
                println!(
                    "Every one of these is flagged prerelease, which is normal for xray-core."
                );
                println!("bliss installs the newest non-prerelease by default, which can be");
                println!("months behind. Use --newest to take the newest build of any kind.");
            }
            Ok(())
        }
        EngineCmd::Install {
            version,
            newest,
            via,
        } => install_engine(version.as_deref(), newest, via).await,
    }
}

async fn install_engine(
    version: Option<&str>,
    newest: bool,
    via: Option<SocketAddr>,
) -> Result<()> {
    let channel = if newest {
        bliss_xray::Channel::Newest
    } else {
        bliss_xray::Channel::Stable
    };

    let client = bliss_xray::client(via)?;
    if let Some(addr) = via {
        println!("downloading through {addr}");
    }

    let release = bliss_xray::select(&client, channel, version)
        .await
        .wrap_err("could not pick a release to install")?;
    println!(
        "xray-core {} ({}, {})",
        release.version(),
        &release.published[..10],
        if release.prerelease {
            "prerelease"
        } else {
            "stable"
        }
    );

    let dir = work_dir()?;
    let mut last_shown = 0u64;
    let path = bliss_xray::install(&client, &release, &dir, |done, total| {
        // Redraw at most every 512 KB: a progress line that repaints on every
        // chunk costs more than the download.
        if done - last_shown < 512 * 1024 && done != total {
            return;
        }
        last_shown = done;
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        if total > 0 {
            print!(
                "\r  {:.1} / {:.1} MB ({:.0}%)",
                mb(done),
                mb(total),
                done as f64 / total as f64 * 100.0
            );
        } else {
            print!("\r  {:.1} MB", mb(done));
        }
        let _ = std::io::stdout().flush();
    })
    .await
    .wrap_err("could not install the engine")?;
    println!();

    let installed = bliss_xray::check_version(&path, None)
        .await
        .wrap_err("the downloaded engine does not run")?;
    println!(
        "
installed xray-core {installed}
  {}",
        path.display()
    );
    println!(
        "
bliss uses it automatically — no engine.xray needed in the profile."
    );
    Ok(())
}

// --- run ------------------------------------------------------------------

async fn run(
    path: Option<PathBuf>,
    want: Option<String>,
    engine_log: &str,
    tun: bool,
) -> Result<()> {
    let mut setup = prepare(path, want, engine_log)?;
    // The flag turns it on for this run; it never turns off what the profile
    // asks for, because leaving it out is not the same as saying no.
    setup.tun |= tun;
    // Before the engine thread, so a missing engine is reported here rather
    // than as a failed start a moment later.
    engine::engine_path(&setup).map_err(|e| eyre!(e))?;
    let listen = format!(
        "{}:{}",
        setup.profile.inbound.listen, setup.profile.inbound.socks
    );
    let described = describe(&setup.servers[setup.selected]);
    let bridge = setup.profile.engine.bridge_port;
    let work_dir = setup.work_dir.clone();

    let mut controller = engine::spawn(setup).wrap_err("could not start the engine thread")?;

    // Wait for it to settle, so a failure is reported here rather than left in
    // a log line while the terminal claims to be ready.
    loop {
        let frame = controller.frame();
        match frame.state {
            engine::State::Running => break,
            engine::State::Failed => {
                bail!("{}", frame.error.unwrap_or_else(|| "engine failed".into()))
            }
            _ => controller.changed().await,
        }
    }

    println!(
        "
bliss - {}",
        controller.frame().engine
    );
    println!("  server   {described}");
    println!("  listen   {listen}  (socks5 + http on either)");
    println!("  bridge   127.0.0.1:{bridge} -> engine");
    println!(
        "
ready. Try it:"
    );
    println!("  curl -x socks5h://{listen} https://ifconfig.me");
    println!(
        "
Ctrl-C to stop.
"
    );

    tokio::signal::ctrl_c().await?;
    report(&controller.frame());
    if let Err(e) = bliss_core::sysproxy::restore(&work_dir) {
        tracing::error!("could not restore the system proxy on the way out: {e}");
    }
    // The engine is a child in a Job Object, so it goes when we do.
    Ok(())
}

/// What went through us, printed on the way out.
///
/// This is the data behind the connections screen, and precisely what xray-core
/// cannot supply: it never sees the local socket, so it cannot name a process.
fn report(frame: &engine::Frame) {
    println!(
        "
{} connection(s), {} up, {} down",
        frame.opened,
        engine::bytes(frame.up_total),
        engine::bytes(frame.down_total)
    );
    if frame.connections.is_empty() {
        return;
    }
    println!(
        "
  {:<34} {:<8} {:<18} {:>11} {:>11}",
        "target", "route", "process", "up", "down"
    );
    for row in frame.connections.iter().take(20) {
        println!(
            "  {:<34} {:<8} {:<18} {:>11} {:>11}",
            elide(&row.target, 34),
            row.route.to_string(),
            row.process.as_deref().unwrap_or("-"),
            engine::bytes(row.up),
            engine::bytes(row.down)
        );
    }
}

/// Trim to a column width. Neither a terminal nor Blitz does text-overflow, so
/// the truncation is ours either way.
fn elide(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_owned();
    }
    let keep: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{keep}…")
}

/// A copy of the log on disk.
///
/// The window has no console to print to, and a desktop app that fails at
/// startup otherwise leaves the user with nothing to look at.
///
/// Appended to, not truncated: truncating meant that a second bliss — a test
/// run, a stray copy — erased the record of the first, which is exactly when
/// the log is wanted. Trimmed when it gets large rather than rotated, since one
/// file someone can open beats a directory of numbered ones.
fn file_layer<S>() -> Option<impl tracing_subscriber::Layer<S>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    const LIMIT: u64 = 2 * 1024 * 1024;

    // The directory has to exist before a file can be opened in it. On a first
    // run it does not: nothing has written there yet, and the first thing that
    // would is the engine installer. So the log silently failed to open on
    // exactly the run most likely to need it — and `report_to_user` then handed
    // the user the path of a file that was never created.
    let dir = work_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;

    let path = dir.join("bliss.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LIMIT) && held_by_no_one_else(&path) {
        let _ = std::fs::remove_file(&path);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    Some(
        fmt::layer()
            .with_ansi(false)
            .with_target(false)
            // A handle per event is a syscall we can afford at a few lines a
            // second, and it keeps this to one function with no new dependency.
            .with_writer(move || file.try_clone().expect("clone the log handle")),
    )
}

/// Is this log ours alone to trim?
///
/// Rust opens files with `FILE_SHARE_DELETE`, so deleting one another bliss is
/// appending to succeeds — and that process goes on writing into a file nobody
/// can find any more. Which destroys exactly the record the comment above
/// promises to keep. Asking for the file with no sharing at all answers the
/// question: the open fails if anyone else has it.
///
/// There is a hair of a race between this and the delete. Losing it costs one
/// oversized log, which is where we started.
#[cfg(windows)]
fn held_by_no_one_else(path: &std::path::Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .is_ok()
}

#[cfg(not(windows))]
fn held_by_no_one_else(_path: &std::path::Path) -> bool {
    true
}

/// Put a startup failure where someone who double-clicked can read it.
fn report_to_user(error: &color_eyre::Report) {
    let text = format!(
        "{error}

Подробности в журнале:
{}",
        work_dir()
            .map(|d| d.join("bliss.log").display().to_string())
            .unwrap_or_else(|_| "—".to_owned())
    );
    show_message(&text);
}

#[cfg(windows)]
fn show_message(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;
    // SAFETY: both strings outlive the call, and a message box has no other
    // preconditions.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("bliss"),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn show_message(text: &str) {
    eprintln!("{text}");
}

/// Give a command somewhere to print.
///
/// bliss is a windows-subsystem program, so nothing gives it a console. A shell
/// that hands over pipes — Git Bash, or anything capturing the output — needs
/// nothing more, and is left alone. cmd and PowerShell hand over nothing, so
/// their console is joined here; neither waits for a windows-subsystem program,
/// which means the prompt comes back before the output does. Started with
/// arguments and no terminal at all, from a shortcut say, it gets a console of
/// its own, as it always had.
///
/// The first attempt at losing the console went the other way: a console
/// program calling FreeConsole. From a shell that was harmless; from Explorer
/// the process went on serving traffic and never showed a window. So the window
/// path does not touch consoles at all.
#[cfg(windows)]
fn attach_console() {
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
        STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows::core::w;

    fn usable(id: STD_HANDLE) -> bool {
        // SAFETY: reads the process's standard handle; no preconditions.
        unsafe { GetStdHandle(id) }.is_ok_and(|handle| !handle.is_invalid())
    }

    if usable(STD_OUTPUT_HANDLE) {
        return;
    }
    // SAFETY: plain Win32 calls with no pointers of ours beyond the constant
    // device names; a handle that fails to open is simply not installed.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() && AllocConsole().is_err() {
            return;
        }
        for (id, device) in [
            (STD_INPUT_HANDLE, w!("CONIN$")),
            (STD_OUTPUT_HANDLE, w!("CONOUT$")),
            (STD_ERROR_HANDLE, w!("CONOUT$")),
        ] {
            if usable(id) {
                continue;
            }
            if let Ok(handle) = CreateFileW(
                device,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            ) {
                let _ = SetStdHandle(id, handle);
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_console() {}

/// Whether this start is a command, which prints and so wants a console.
///
/// Any argument used to mean yes. The window's one, `--minimized`, is what
/// Windows passes when it starts bliss at sign-in with no terminal around, and
/// a console of its own would have opened beside the minimized window every
/// time the user signed in.
fn wants_console(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    args.any(|arg| arg.to_str() != Some(startup::MINIMIZED))
}

fn parse_log_level(s: &str) -> Result<bliss_xray::LogLevel> {
    use bliss_xray::LogLevel::*;
    Ok(match s {
        "debug" => Debug,
        "info" => Info,
        "warning" | "warn" => Warning,
        "error" => Error,
        "none" => None,
        other => {
            bail!("unknown engine log level {other:?}: use debug, info, warning, error or none")
        }
    })
}

// --- check ----------------------------------------------------------------

fn check_one_link() -> Result<()> {
    let mut link = String::new();
    std::io::stdin()
        .read_to_string(&mut link)
        .wrap_err("failed to read a link from stdin")?;
    if link.trim().is_empty() {
        bail!("nothing on stdin — paste a server link, then Ctrl-Z Enter");
    }

    let config: ServerConfig = link
        .trim()
        .parse()
        .wrap_err("this is not a server link bliss can use")?;

    println!("{}", describe(&config));
    Ok(())
}

fn check_profile(path: Option<PathBuf>) -> Result<()> {
    let (profile, path) = load_profile(path)?;
    let Some(profile) = profile else {
        bail!(
            "no profile yet at {} — bliss writes it when a server is added in its window",
            path.display()
        );
    };

    let (ok, failed) = profile.parse_servers();
    println!("{} — {} server(s)\n", path.display(), profile.servers.len());
    for config in &ok {
        println!("  ✓ {}", describe(config));
    }
    for (i, err) in &failed {
        // The link itself is not echoed back: it is exactly the thing that must
        // not end up in a pasted terminal log.
        println!("  ✗ server #{} — {err}", i + 1);
    }

    let inbound = &profile.inbound;
    println!(
        "\n  inbound   socks {}:{}   http {}:{}",
        inbound.listen, inbound.socks, inbound.listen, inbound.http
    );
    println!("  bridge    127.0.0.1:{}", profile.engine.bridge_port);
    if inbound.socks == inbound.http {
        bail!(
            "socks and http are both on port {} — they cannot share one",
            inbound.socks
        );
    }
    if !failed.is_empty() {
        bail!(
            "{} of {} servers did not parse",
            failed.len(),
            profile.servers.len()
        );
    }
    Ok(())
}

// --- shared ---------------------------------------------------------------

/// The profile a CLI command reads: the path given, which has to exist, or the
/// default one, which may not yet. Nothing is created here — the window writes
/// the profile once there is something to put in it.
fn load_profile(path: Option<PathBuf>) -> Result<(Option<Profile>, PathBuf)> {
    let explicit = path.is_some();
    let path = match path {
        Some(p) => p,
        None => default_profile_path()?,
    };
    let profile = profile_file::read(&path)?;
    if explicit && profile.is_none() {
        bail!("there is no profile at {}", path.display());
    }
    Ok((profile, path))
}

/// One line per server: what it is, where it goes, how it hides.
fn describe(c: &ServerConfig) -> String {
    let security = match &c.security {
        Security::Reality(r) => {
            format!("reality/{} sni={} sid={}", r.fingerprint, r.sni, r.short_id)
        }
        Security::Tls(t) => format!("tls sni={}", t.sni.as_deref().unwrap_or("-")),
        Security::None => "plain".to_owned(),
    };
    let own = match &c.protocol {
        Protocol::Vless {
            encryption, flow, ..
        } => {
            let flow = flow
                .as_param()
                .map(|f| format!("  {f}"))
                .unwrap_or_default();
            let encryption = match encryption {
                Encryption::None => String::new(),
                Encryption::MlKem768X25519Plus(e) => format!("  enc={}/{}", e.mode, e.rtt),
            };
            format!("{flow}{encryption}")
        }
        Protocol::Vmess { cipher, .. } => format!("  scy={}", cipher.as_param()),
        Protocol::Shadowsocks { method, .. } => format!("  {method}"),
        _ => String::new(),
    };
    format!(
        "{:<20} {:<11} {}:{}  {} {security}{own}",
        c.tag,
        c.protocol.name(),
        c.address,
        c.port,
        c.transport.name()
    )
}

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "bliss").ok_or_else(|| eyre!("cannot locate %APPDATA%"))
}

fn default_profile_path() -> Result<PathBuf> {
    // A different profile altogether, for the window and the CLI alike: for
    // checking a change against a scratch profile rather than a real one, and
    // for a portable setup. The work dir does not move with it.
    if let Some(path) = std::env::var_os("BLISS_PROFILE").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    // %APPDATA%\bliss\bliss.toml, not the crate's config_dir(): that appends a
    // "config" folder, which is a unix habit. On Windows an app's settings live
    // directly under its own folder in Roaming, and this is a file users edit.
    let base = directories::BaseDirs::new().ok_or_else(|| eyre!("cannot locate %APPDATA%"))?;
    Ok(base.config_dir().join("bliss").join("bliss.toml"))
}

/// Where generated configs and the managed engine live.
fn work_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_local_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One window, and the claim comes back when that one lets go.
    ///
    /// The second copy is not merely redundant: WebView2 refuses it an
    /// environment over the data folder the first one holds, so it never gets
    /// a page and never shows a window, while still taking a turn at the
    /// profile and the ports. Measured on 20 September, three copies deep.
    #[cfg(windows)]
    #[test]
    fn only_one_copy_holds_the_window() {
        let name = r"Local\bliss-window-test";
        let first = claim(name).expect("the first copy takes the claim");
        assert!(
            claim(name).is_none(),
            "a second copy is refused while the first holds it"
        );
        drop(first);
        let again = claim(name);
        assert!(
            again.is_some(),
            "and the claim is free again once that copy lets go"
        );
        drop(again);

        // A copy under another name has a WebView2 folder of its own, so it is
        // not the same claim and must not be refused — that is how a probe
        // runs beside the user's bliss (§7 of AGENTS.md).
        let ours = claim(name).expect("taken again");
        let beside = claim(r"Local\bliss-window-test-probe");
        assert!(beside.is_some(), "another name is another claim");
        drop(beside);
        drop(ours);

        // And the name really does follow the executable, or every copy would
        // share one claim however it was named.
        let mine = claim_name();
        assert!(mine.starts_with(r"Local\bliss-window-"), "{mine}");
        assert!(!mine[r"Local\bliss-window-".len()..].is_empty(), "{mine}");
    }

    /// A console follows a command, not the presence of an argument: the
    /// startup entry's `--minimized` on its own is still the window.
    #[test]
    fn only_a_command_gets_a_console() {
        let args = |list: &[&str]| {
            list.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
                .into_iter()
        };
        assert!(!wants_console(args(&[])), "the window");
        assert!(
            !wants_console(args(&["--minimized"])),
            "the window, as Windows starts it"
        );
        assert!(wants_console(args(&["check"])));
        assert!(
            wants_console(args(&["--minimized", "--help"])),
            "clap has help to print"
        );
    }

    #[test]
    fn the_minimized_flag_reaches_the_window() {
        let cli = Cli::try_parse_from(["bliss", "--minimized"]).expect("parses");
        assert!(cli.minimized && cli.command.is_none());
    }
}
