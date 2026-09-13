//! bliss — a VLESS + REALITY client for Windows.

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
mod ui;

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;

use bliss_proto::{Encryption, Profile, Security, ServerConfig};
use clap::{Parser, Subcommand};
use color_eyre::eyre::{Context, Result, bail, eyre};
use tracing_error::ErrorLayer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser)]
#[command(name = "bliss", version, about = "VLESS + REALITY client")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and validate a profile, or a single link read from stdin.
    Check {
        /// Profile to check. Defaults to the one in the bliss config directory.
        path: Option<PathBuf>,
        /// Read one vless:// link from stdin instead of reading a profile.
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

    match Cli::parse().command {
        // No subcommand is the product: the window. The CLI is for setting it
        // up and for looking under the hood.
        None => window().inspect_err(report_to_user),
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
        } => run(path, server, &engine_log).await,
    }
}

/// Open the window.
///
/// This thread belongs to the event loop from here on — every windowing library
/// insists on the main thread — so the engine gets a runtime of its own.
fn window() -> Result<()> {
    let setup = window_setup()?;
    let work_dir = setup.work_dir.clone();
    let controller = engine::spawn(setup).wrap_err("could not start the engine thread")?;
    ui::launch(controller);

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
                Some(format!("профиль не читается: {}", e.brief())),
            )
        }
    };
    let (servers, failed) = profile.parse_servers();
    for (i, err) in &failed {
        tracing::warn!("server #{} in the profile is unusable - {err}", i + 1);
    }
    Ok(engine::Setup {
        profile,
        servers,
        selected: 0,
        work_dir: work_dir()?,
        profile_path: path,
        engine_log: bliss_xray::LogLevel::Warning,
        autostart: true,
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
                    "no server in {}. Add one in the bliss window, or paste a vless:// link into the file — the [[server]] block at the bottom shows where.",
                    path.display()
                ),
            }
        }
        None => 0,
    };

    Ok(engine::Setup {
        work_dir: work_dir()?,
        engine_log: parse_log_level(engine_log)?,
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
        },
        decision.rule
    );

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

async fn run(path: Option<PathBuf>, want: Option<String>, engine_log: &str) -> Result<()> {
    let setup = prepare(path, want, engine_log)?;
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

// The console stays. Detaching it with FreeConsole made the window look tidier
// when launched from a shell, and broke it entirely when launched from Explorer
// — the process kept running and serving traffic, but created no window at all.
// A shell inherits its parent's console, so detaching there is harmless; a
// double-click owns its console, and destroying it takes the window with it.
// Tested the first way, shipped, and broke the second.
//
// The proper fix is the windows subsystem plus AttachConsole for the CLI paths,
// which needs testing from Explorer before it goes anywhere near a release.

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
        bail!("nothing on stdin — paste a vless:// link, then Ctrl-Z Enter");
    }

    let config: ServerConfig = link
        .trim()
        .parse()
        .wrap_err("this is not a vless:// link bliss can use")?;

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
    let encryption = match &c.encryption {
        Encryption::None => String::new(),
        Encryption::MlKem768X25519Plus(e) => format!("  enc={}/{}", e.mode, e.rtt),
    };
    let flow = match c.flow {
        bliss_proto::Flow::XtlsRprxVision => "  vision",
        bliss_proto::Flow::None => "",
    };
    format!(
        "{:<20} {}:{}  {security}{flow}{encryption}",
        c.tag, c.address, c.port
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
